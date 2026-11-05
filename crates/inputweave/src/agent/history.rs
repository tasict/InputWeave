//! The history of file transfers: the finished ones (done, declined,
//! failed, cancelled) the list keeps, and where they are kept across
//! restarts of the agent ([`HistoryStore`]), as a TOML file:
//!
//! ```toml
//! [[transfers]]
//! id = "18f0a2b3c4d5e6f7-1c2-3"
//! direction = "send"
//! device = "win"
//! name = "Report.pdf"
//! size = 3000000
//! state = "declined"
//! reason = "no_space"
//! missing_bytes = 1200000
//! path = "/Users/me/Report.pdf"
//! started_unix_ms = 1761100000000
//! finished_unix_ms = 1761100002000
//! ```
//!
//! On macOS and Linux the agent runs as the user and keeps the file
//! itself, `transfers.toml` next to the devices file
//! (`HistoryStore::File`). On Windows it runs as SYSTEM, and the history
//! holds the names of the user's files, so the broker keeps the file as
//! the user, in the user's own `%LOCALAPPDATA%\InputWeave`
//! (`HistoryStore::Broker`, see `broker`), and the history is that user's:
//!
//! - It is empty until a broker connects. The agent then asks the broker
//!   for it, and what the broker loads takes the place of the finished
//!   transfers listed: those that finished while nobody was logged on, or
//!   for someone else. Transfers still going stay listed.
//! - From then on it is saved through that broker, and no other.
//! - Once the broker disconnects, as when the user logs off, the finished
//!   transfers leave the list and nothing is saved, until the next broker
//!   has loaded its user's history. A transfer that finishes in between is
//!   listed until then and never saved: the next user may not be the one
//!   it was for.
//!
//! A file holds at most [`MAX_BYTES`]; when the finished transfers take
//! more, as folders whose many files failed with long paths can, the
//! newest that fit are kept.
//!
//! Transfers still going are never written: a sender's ends with the
//! agent, and a receiver keeps its part file for the sender to resume
//! with, which lists the transfer again. Removing a transfer from the
//! history never touches its file.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use iw_proto::peer::v1::FileDecline;
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, watch};
#[cfg(any(unix, windows))]
use tracing::debug;
use tracing::{info, warn};

#[cfg(any(unix, windows))]
use super::broker::{Broker, UserHistory};
#[cfg(any(unix, windows))]
use super::files::Files;
use super::files::{
    BatchInfo, Direction, FailedEntry, FolderProgress, TransferState, TransferStatus, failure,
    valid_transfer_id,
};
use super::listing::MAX_LISTED_FAILURES;

/// Finished transfers kept in the list; beyond this, those that finished
/// first go. Transfers still going do not count.
pub(crate) const KEEP_FINISHED: usize = 100;

/// The most a history file holds, and the broker loads or saves.
pub(crate) const MAX_BYTES: usize = 1024 * 1024;

const HEADER: &str = "# File transfers that finished, kept by InputWeave.\n\
                      # Edit only while InputWeave is stopped.\n\n";

/// Drops the finished transfers beyond [`KEEP_FINISHED`], those that
/// finished first; of two that finished at the same time, the one that
/// started first. Returns whether any went.
pub(crate) fn trim(list: &mut Vec<TransferStatus>) -> bool {
    let mut finished: Vec<(Option<SystemTime>, usize)> = list
        .iter()
        .enumerate()
        .filter(|(_, t)| t.state.finished())
        .map(|(at, t)| (t.finished, at))
        .collect();
    let Some(excess) = finished.len().checked_sub(KEEP_FINISHED).filter(|n| *n > 0) else {
        return false;
    };
    // The list is newest first, so further down started earlier.
    finished.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    let mut dropped: Vec<usize> = finished[..excess].iter().map(|(_, at)| *at).collect();
    dropped.sort_unstable();
    let mut at = 0;
    list.retain(|_| {
        let keep = dropped.binary_search(&at).is_err();
        at += 1;
        keep
    });
    true
}

/// Takes the finished transfers off `list`; those still going stay.
/// Returns whether any went.
pub(crate) fn forget_finished(list: &mut Vec<TransferStatus>) -> bool {
    let before = list.len();
    list.retain(|t| !t.state.finished());
    list.len() != before
}

/// Lists the finished transfers a history kept after those listed, but
/// none listed already, as a transfer that resumed is; then drops those
/// beyond the limit. Returns whether the list changed.
pub(crate) fn take_in(list: &mut Vec<TransferStatus>, kept: Vec<TransferStatus>) -> bool {
    let mut changed = false;
    for t in kept {
        if !list.iter().any(|listed| listed.id == t.id) {
            list.push(t);
            changed = true;
        }
    }
    trim(list) || changed
}

#[derive(Debug, Default, Deserialize)]
struct HistoryFile {
    #[serde(default)]
    transfers: Vec<SavedTransfer>,
}

/// What a history file is written from.
#[derive(Serialize)]
struct HistoryOut<'a> {
    transfers: &'a [SavedTransfer],
}

/// A finished transfer as the file keeps it. Fields a later version adds
/// are ignored.
#[derive(Debug, Deserialize, Serialize)]
struct SavedTransfer {
    id: String,
    direction: SavedDirection,
    device: String,
    name: String,
    size: u64,
    /// "done", "declined", "failed" or "cancelled".
    state: String,
    /// Why it was declined or failed, as clients are told.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    reason: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    missing_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(default)]
    started_unix_ms: u64,
    #[serde(default)]
    finished_unix_ms: u64,
    /// The batch it came in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    batch: Option<SavedBatch>,
    /// For a folder, how its files went.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    folder: Option<SavedFolder>,
}

#[derive(Debug, Deserialize, Serialize)]
struct SavedBatch {
    id: String,
    #[serde(default)]
    index: u32,
    #[serde(default)]
    items: u32,
    #[serde(default)]
    files: u32,
    #[serde(default)]
    bytes: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    drag: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    clipboard: u64,
}

#[derive(Debug, Deserialize, Serialize)]
struct SavedFolder {
    entries: u32,
    #[serde(default)]
    done: u32,
    #[serde(default)]
    failed: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    failures: Vec<SavedFailure>,
}

#[derive(Debug, Deserialize, Serialize)]
struct SavedFailure {
    path: String,
    reason: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum SavedDirection {
    Send,
    Receive,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// TOML integers are signed; a size offered by another device could be
/// anything.
fn integer(n: u64) -> u64 {
    n.min(i64::MAX as u64)
}

fn unix_ms(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH)
        .map_or(0, |d| integer(d.as_millis().try_into().unwrap_or(u64::MAX)))
}

fn from_unix_ms(ms: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(ms)
}

impl SavedTransfer {
    /// The transfer as the file keeps it, if it finished.
    fn of(t: &TransferStatus) -> Option<Self> {
        let state = match &t.state {
            TransferState::Done => "done",
            TransferState::Declined(_) => "declined",
            TransferState::Failed(_) => "failed",
            TransferState::Cancelled => "cancelled",
            _ => return None,
        };
        Some(Self {
            id: t.id.clone(),
            direction: match t.direction {
                Direction::Send => SavedDirection::Send,
                Direction::Receive => SavedDirection::Receive,
            },
            device: t.device.clone(),
            name: t.name.clone(),
            size: integer(t.size),
            state: state.to_owned(),
            reason: t.state.reason().to_owned(),
            missing_bytes: integer(t.missing_bytes),
            path: t.path.as_ref().map(|p| p.display().to_string()),
            started_unix_ms: unix_ms(t.started),
            finished_unix_ms: unix_ms(t.finished.unwrap_or(t.started)),
            batch: t.batch.as_ref().map(|b| SavedBatch {
                id: b.id.clone(),
                index: b.index,
                items: b.items,
                files: b.files,
                bytes: integer(b.bytes),
                drag: b.drag.clone().unwrap_or_default(),
                clipboard: b.clipboard.unwrap_or_default(),
            }),
            folder: t.folder.as_ref().map(|f| SavedFolder {
                entries: f.entries,
                done: f.done,
                failed: f.failed,
                failures: f
                    .failures
                    .iter()
                    .map(|f| SavedFailure {
                        path: f.path.clone(),
                        reason: f.reason.clone(),
                    })
                    .collect(),
            }),
        })
    }

    /// The transfer to list, unless the entry is not one this agent wrote.
    fn restore(self) -> Option<TransferStatus> {
        if !valid_transfer_id(&self.id) {
            return None;
        }
        let state = match self.state.as_str() {
            "done" => TransferState::Done,
            "cancelled" => TransferState::Cancelled,
            "declined" => TransferState::Declined(match self.reason.as_str() {
                "rejected" => FileDecline::Rejected,
                "no_space" => FileDecline::NoSpace,
                "busy" => FileDecline::Busy,
                "too_large" => FileDecline::TooLarge,
                "invalid_path" => FileDecline::InvalidPath,
                _ => FileDecline::Unavailable,
            }),
            "failed" => TransferState::Failed(failure(&self.reason)),
            _ => return None,
        };
        let batch = self
            .batch
            .filter(|b| valid_transfer_id(&b.id))
            .map(|b| BatchInfo {
                id: b.id,
                index: b.index,
                items: b.items,
                files: b.files,
                bytes: b.bytes,
                drag: (!b.drag.is_empty()).then_some(b.drag),
                clipboard: (b.clipboard != 0).then_some(b.clipboard),
            });
        let folder = self.folder.map(|f| FolderProgress {
            entries: f.entries,
            done: f.done,
            failed: f.failed,
            failures: f
                .failures
                .into_iter()
                .take(MAX_LISTED_FAILURES)
                .map(|f| FailedEntry {
                    path: f.path,
                    reason: f.reason,
                })
                .collect(),
        });
        Some(TransferStatus {
            done: if state == TransferState::Done {
                self.size
            } else {
                0
            },
            id: self.id,
            direction: match self.direction {
                SavedDirection::Send => Direction::Send,
                SavedDirection::Receive => Direction::Receive,
            },
            device: self.device,
            name: self.name,
            size: self.size,
            bytes_per_second: 0,
            state,
            missing_bytes: self.missing_bytes,
            path: self.path.map(PathBuf::from),
            started: from_unix_ms(self.started_unix_ms),
            finished: Some(from_unix_ms(self.finished_unix_ms)),
            batch,
            folder,
        })
    }
}

/// The finished transfers a history file holds, newest first; entries
/// this agent did not write are skipped. Fails, saying why, for more than
/// [`MAX_BYTES`] or anything but such a file.
pub(crate) fn parse(bytes: &[u8]) -> Result<Vec<TransferStatus>, String> {
    if bytes.len() > MAX_BYTES {
        return Err(format!("it is larger than {MAX_BYTES} bytes"));
    }
    let text = std::str::from_utf8(bytes).map_err(|e| e.to_string())?;
    let file: HistoryFile = toml::from_str(text).map_err(|e| e.to_string())?;
    let mut list: Vec<TransferStatus> = Vec::new();
    for t in file
        .transfers
        .into_iter()
        .filter_map(SavedTransfer::restore)
    {
        if !list.iter().any(|kept| kept.id == t.id) {
            list.push(t);
        }
    }
    trim(&mut list);
    Ok(list)
}

fn render(transfers: &[SavedTransfer]) -> io::Result<String> {
    let body = toml::to_string(&HistoryOut { transfers }).map_err(io::Error::other)?;
    Ok(format!("{HEADER}{body}"))
}

/// The finished transfers in `list` as a history file holds them, in at
/// most [`MAX_BYTES`]: when they take more, the newest that fit, which is
/// logged.
pub(crate) fn encode(list: &[TransferStatus]) -> io::Result<String> {
    let transfers: Vec<SavedTransfer> = list.iter().filter_map(SavedTransfer::of).collect();
    let text = render(&transfers)?;
    if text.len() <= MAX_BYTES {
        return Ok(text);
    }
    // Each measured on its own, newest first.
    let total = transfers.len();
    let mut room = MAX_BYTES - HEADER.len();
    let mut kept = Vec::new();
    for t in transfers {
        let size = render(std::slice::from_ref(&t))?.len() - HEADER.len();
        if size <= room {
            room -= size;
            kept.push(t);
        }
    }
    let mut text = render(&kept)?;
    while text.len() > MAX_BYTES && kept.pop().is_some() {
        text = render(&kept)?;
    }
    warn!(
        kept = kept.len(),
        left_out = total - kept.len(),
        "the transfer history is too large to keep whole; keeping the newest transfers that fit"
    );
    Ok(text)
}

/// The file at `path`, read up to a byte past [`MAX_BYTES`] so that one
/// larger is told apart.
fn read_limited(path: &Path) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// The finished transfers kept in `path`, newest first. A missing file is
/// an empty history, and so is one that cannot be read or is larger than
/// [`MAX_BYTES`], which is logged: the history is not worth refusing to
/// start over.
pub(crate) fn load(path: &Path) -> Vec<TransferStatus> {
    let parsed = match read_limited(path) {
        Ok(bytes) => parse(&bytes),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => Err(e.to_string()),
    };
    parsed.unwrap_or_else(|e| {
        warn!(path = %path.display(), error = %e, "cannot read the transfer history");
        Vec::new()
    })
}

/// Where a history file is written before it replaces the file.
fn temporary(path: &Path) -> PathBuf {
    path.with_extension("toml.tmp")
}

/// Replaces the file in one step with the finished transfers in `list`,
/// readable only by its owner, as it holds names of files.
fn save(path: &Path, list: &[TransferStatus]) -> io::Result<()> {
    let text = encode(list)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = temporary(path);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut out = options.open(&tmp)?;
    out.write_all(text.as_bytes())?;
    out.sync_all()?;
    drop(out);
    std::fs::rename(&tmp, path)
}

/// Removes the history file an earlier agent kept in `path`, now that the
/// broker keeps the history in the user's own folder: on Windows it was in
/// `%ProgramData%\InputWeave`, written as SYSTEM. Once it is gone nothing
/// writes it again.
pub(crate) fn remove_left_behind(path: &Path) {
    for path in [path.to_owned(), temporary(path)] {
        match std::fs::remove_file(&path) {
            Ok(()) => info!(
                path = %path.display(),
                "removed the transfer history the agent kept; the broker keeps it for the user now"
            ),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => {
                warn!(path = %path.display(), error = %e, "cannot remove the transfer history the agent kept");
            }
        }
    }
}

/// Where the finished transfers are kept across restarts of the agent.
pub(crate) enum HistoryStore {
    /// A file the agent reads and writes itself, as it runs as the user.
    File(PathBuf),
    /// The broker's file in the user's folder, which the broker reads and
    /// writes as the user; the history is the connected broker's user's.
    #[cfg(any(unix, windows))]
    Broker(Arc<Broker>),
}

/// Where a write goes.
enum Target {
    File(PathBuf),
    /// The broker on the connection the history was loaded from.
    #[cfg(any(unix, windows))]
    Broker(Arc<Broker>, u64),
}

impl Target {
    async fn write(self, list: Vec<TransferStatus>) {
        match self {
            Target::File(path) => {
                let at = path.clone();
                let saved = tokio::task::spawn_blocking(move || save(&path, &list))
                    .await
                    .unwrap_or_else(|e| Err(io::Error::other(e)));
                if let Err(e) = saved {
                    warn!(path = %at.display(), error = %e, "cannot save the transfer history");
                }
            }
            #[cfg(any(unix, windows))]
            Target::Broker(broker, link) => {
                let encoded = tokio::task::spawn_blocking(move || encode(&list))
                    .await
                    .unwrap_or_else(|e| Err(io::Error::other(e)));
                let saved = match encoded {
                    Ok(text) => broker.save_history(link, text.into_bytes()).await,
                    Err(e) => Err(e),
                };
                match saved {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::NotConnected => {
                        debug!("the broker went away; the transfer history is not saved");
                    }
                    Err(e) => warn!(error = %e, "cannot save the transfer history"),
                }
            }
        }
    }
}

/// Writes the history whenever it changes, off the async threads: a
/// change only marks it to be written, and one task at a time writes the
/// list as it is then, so changes that come together are written once and
/// in order.
pub(crate) struct HistoryWriter {
    store: HistoryStore,
    list: watch::Receiver<Vec<TransferStatus>>,
    state: Mutex<Writing>,
    idle: Notify,
}

#[derive(Default)]
struct Writing {
    /// The list changed since it was last written.
    dirty: bool,
    /// A task is writing.
    busy: bool,
    /// Nothing more is written.
    closed: bool,
    /// For the broker's history: the connection whose user's history the
    /// list holds, loaded from it and saved through it. None while the
    /// list holds nobody's, which is not saved.
    link: Option<u64>,
}

impl HistoryWriter {
    pub(crate) fn new(
        store: HistoryStore,
        list: watch::Receiver<Vec<TransferStatus>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            list,
            state: Mutex::new(Writing::default()),
            idle: Notify::new(),
        })
    }

    /// Has the history written, as the list is by then.
    pub(crate) fn changed(self: &Arc<Self>) {
        let mut state = self.state.lock().unwrap();
        if state.closed {
            return;
        }
        state.dirty = true;
        self.start(&mut state);
    }

    /// Where the list is written now, if anywhere.
    fn target(&self, state: &Writing) -> Option<Target> {
        match &self.store {
            HistoryStore::File(path) => Some(Target::File(path.clone())),
            #[cfg(any(unix, windows))]
            HistoryStore::Broker(broker) => {
                state.link.map(|link| Target::Broker(broker.clone(), link))
            }
        }
    }

    /// Starts a task writing what changed, unless one is writing already
    /// or there is nowhere to write it.
    fn start(self: &Arc<Self>, state: &mut Writing) {
        if state.busy || !state.dirty || self.target(state).is_none() {
            return;
        }
        state.busy = true;
        let writer = self.clone();
        tokio::spawn(async move { writer.write_while_dirty().await });
    }

    /// The list holds the history of the user of broker connection `link`
    /// now, which it is saved through from now on.
    #[cfg(any(unix, windows))]
    fn link(self: &Arc<Self>, link: u64) {
        let mut state = self.state.lock().unwrap();
        state.link = Some(link);
        if !state.closed {
            self.start(&mut state);
        }
    }

    /// The list holds nobody's history: nothing is saved until linked.
    #[cfg(any(unix, windows))]
    fn unlink(&self) {
        self.state.lock().unwrap().link = None;
    }

    async fn write_while_dirty(&self) {
        loop {
            let target = {
                let mut state = self.state.lock().unwrap();
                match self.target(&state).filter(|_| state.dirty) {
                    Some(target) => {
                        state.dirty = false;
                        target
                    }
                    None => {
                        state.busy = false;
                        drop(state);
                        self.idle.notify_waiters();
                        return;
                    }
                }
            };
            let list = self.list.borrow().clone();
            target.write(list).await;
        }
    }

    /// Waits until every change so far is written.
    pub(crate) async fn flush(&self) {
        loop {
            let idle = self.idle.notified();
            if !self.state.lock().unwrap().busy {
                return;
            }
            idle.await;
        }
    }

    /// Writes what changed so far, and nothing after: the agent is
    /// stopping, and another may soon use the file.
    pub(crate) async fn close(&self) {
        self.state.lock().unwrap().closed = true;
        self.flush().await;
    }
}

/// Keeps the history the broker keeps (`HistoryStore::Broker`) with the
/// brokers as they come and go, as the module's documentation says, until
/// the agent stops.
#[cfg(any(unix, windows))]
pub(crate) async fn follow_broker(broker: Arc<Broker>, files: Arc<Files>) {
    let Some(writer) = files.history_writer() else {
        return;
    };
    let mut history = broker.watch_history();
    // The connection whose user the finished transfers listed are for:
    // those listed before it connected went then.
    let mut cleared_for = None;
    loop {
        let now = history.borrow_and_update().clone();
        writer.unlink();
        let link = match &now {
            UserHistory::None => None,
            UserHistory::Loading(link) | UserHistory::Loaded { link, .. } => Some(*link),
        };
        if link.is_none() || cleared_for != link {
            files.forget_finished();
            cleared_for = link;
        }
        // A load that failed links nothing: saving would write this logon's
        // few transfers over the file the broker could not read.
        match now {
            UserHistory::Loaded { link, toml, error } if error.is_empty() => {
                let parsed = tokio::task::spawn_blocking(move || parse(&toml))
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()));
                match history.has_changed() {
                    Ok(false) => {}
                    // It went, or another came, meanwhile.
                    Ok(true) => continue,
                    Err(_) => return,
                }
                let kept = parsed.unwrap_or_else(|e| {
                    warn!(error = %e, "cannot read the transfer history the broker loaded");
                    Vec::new()
                });
                debug!(count = kept.len(), "loaded the user's transfer history");
                files.take_in_history(kept);
                writer.link(link);
            }
            UserHistory::Loaded { error, .. } => {
                warn!(error = %error, "the broker cannot load the transfer history; transfers finishing now are not saved");
            }
            UserHistory::None | UserHistory::Loading(_) => {}
        }
        if history.changed().await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transfer(id: &str, state: TransferState, finished_secs: Option<u64>) -> TransferStatus {
        TransferStatus {
            id: id.to_owned(),
            direction: Direction::Receive,
            device: "win".to_owned(),
            name: format!("{id}.bin"),
            size: 1000,
            done: if state == TransferState::Done {
                1000
            } else {
                0
            },
            bytes_per_second: 0,
            state,
            missing_bytes: 0,
            path: None,
            started: UNIX_EPOCH + Duration::from_secs(1_000),
            finished: finished_secs.map(|s| UNIX_EPOCH + Duration::from_secs(s)),
            batch: None,
            folder: None,
        }
    }

    fn ids(list: &[TransferStatus]) -> Vec<&str> {
        list.iter().map(|t| t.id.as_str()).collect()
    }

    #[test]
    fn the_transfers_that_finished_first_go_and_those_going_stay() {
        // Newest first: f0 started last; f104 started first but finished
        // last, after a long wait in the queue.
        let mut list: Vec<TransferStatus> = (0..105u64)
            .map(|n| {
                let finished = if n == 104 { 10_000 } else { 2_000 - n };
                transfer(&format!("f{n}"), TransferState::Done, Some(finished))
            })
            .collect();
        list.insert(3, transfer("going", TransferState::Transferring, None));
        list.push(transfer("waiting", TransferState::Queued, None));
        assert!(trim(&mut list));

        assert_eq!(list.len(), KEEP_FINISHED + 2);
        let kept = ids(&list);
        for gone in ["f99", "f100", "f101", "f102", "f103"] {
            assert!(!kept.contains(&gone), "{gone} is still listed");
        }
        assert_eq!(kept[3], "going");
        assert_eq!(kept[kept.len() - 2..], ["f104", "waiting"]);
        assert!(!trim(&mut list));
    }

    #[test]
    fn of_two_that_finished_together_the_one_that_started_first_goes() {
        let mut list: Vec<TransferStatus> = (0..=KEEP_FINISHED)
            .map(|n| transfer(&format!("f{n}"), TransferState::Cancelled, Some(5)))
            .collect();
        trim(&mut list);
        assert_eq!(list.len(), KEEP_FINISHED);
        assert_eq!(list.last().unwrap().id, format!("f{}", KEEP_FINISHED - 1));
    }

    #[test]
    fn finished_transfers_are_saved_and_come_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("transfers.toml");
        assert!(load(&path).is_empty());

        let mut sent = transfer("a-1", TransferState::Done, Some(2_000));
        sent.direction = Direction::Send;
        sent.path = Some("/Users/me/報告.pdf".into());
        let mut full = transfer(
            "b-2",
            TransferState::Declined(FileDecline::NoSpace),
            Some(1_900),
        );
        full.missing_bytes = 1_200_000;
        let list = vec![
            transfer("going-0", TransferState::Transferring, None),
            sent,
            full,
            transfer("c-3", TransferState::Failed("file_changed"), Some(1_800)),
            transfer("d-4", TransferState::Failed("unreadable"), Some(1_700)),
            transfer("e-5", TransferState::Cancelled, Some(1_600)),
            transfer(
                "f-6",
                TransferState::Declined(FileDecline::Rejected),
                Some(1_500),
            ),
        ];
        save(&path, &list).unwrap();

        // Only the finished ones, in the same order and with every detail.
        assert_eq!(load(&path), list[1..]);
        assert!(!path.with_extension("toml.tmp").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn sizes_too_large_for_the_file_are_saved_as_the_largest_it_holds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transfers.toml");
        let mut offered = transfer(
            "a-1",
            TransferState::Declined(FileDecline::NoSpace),
            Some(5),
        );
        offered.size = u64::MAX;
        offered.missing_bytes = u64::MAX - 1;
        save(&path, &[offered]).unwrap();
        let loaded = load(&path);
        assert_eq!(
            (loaded[0].size, loaded[0].missing_bytes),
            (i64::MAX as u64, i64::MAX as u64)
        );
    }

    #[test]
    fn a_damaged_history_is_an_empty_one_and_strange_entries_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transfers.toml");
        std::fs::write(&path, "not toml [").unwrap();
        assert!(load(&path).is_empty());

        let entry = |id: &str, state: &str, reason: &str| {
            format!(
                "[[transfers]]\nid = \"{id}\"\ndirection = \"receive\"\ndevice = \"win\"\n\
                 name = \"x\"\nsize = 1\nstate = \"{state}\"\nreason = \"{reason}\"\n\
                 later_field = true\n"
            )
        };
        let text = [
            entry("ok-1", "failed", "a reason from a later version"),
            entry("../x", "done", ""),
            entry("going-2", "transferring", ""),
            entry("ok-1", "done", ""),
            entry("ok-3", "declined", "busy"),
        ]
        .concat();
        std::fs::write(&path, text).unwrap();
        let loaded = load(&path);
        assert_eq!(ids(&loaded), ["ok-1", "ok-3"]);
        assert_eq!(loaded[0].state, TransferState::Failed("unknown"));
        assert_eq!(loaded[1].state, TransferState::Declined(FileDecline::Busy));
    }

    #[test]
    fn a_file_with_more_than_the_limit_keeps_the_newest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transfers.toml");
        let list: Vec<TransferStatus> = (0..KEEP_FINISHED as u64 + 3)
            .map(|n| transfer(&format!("f-{n}"), TransferState::Done, Some(1_000 - n)))
            .collect();
        save(&path, &list).unwrap();
        assert_eq!(load(&path), list[..KEEP_FINISHED]);
    }

    /// A folder whose many files failed with long paths.
    fn failed_folder(id: &str, finished_secs: u64) -> TransferStatus {
        let failures: Vec<FailedEntry> = (0..MAX_LISTED_FAILURES)
            .map(|n| FailedEntry {
                path: format!("{id}/{}/{n}.bin", "deep".repeat(120)),
                reason: "unreadable".to_owned(),
            })
            .collect();
        TransferStatus {
            folder: Some(FolderProgress {
                entries: 200,
                done: 100,
                failed: 100,
                failures,
            }),
            ..transfer(id, TransferState::Done, Some(finished_secs))
        }
    }

    #[test]
    fn a_history_too_large_to_keep_whole_keeps_the_newest_that_fit() {
        let list: Vec<TransferStatus> = (0..40u64)
            .map(|n| failed_folder(&format!("f-{n}"), 1_000 - n))
            .collect();
        let text = encode(&list).unwrap();
        assert!(text.len() <= MAX_BYTES, "{} bytes", text.len());
        let kept = parse(text.as_bytes()).unwrap();
        assert!(kept.len() > 10 && kept.len() < list.len(), "{}", kept.len());
        assert_eq!(kept, list[..kept.len()]);

        // A file larger than that is no history.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transfers.toml");
        let mut huge = text.into_bytes();
        huge.extend(std::iter::repeat_n(b'#', MAX_BYTES));
        std::fs::write(&path, &huge).unwrap();
        assert!(load(&path).is_empty());
        assert!(parse(&huge).is_err());
        assert!(parse(b"\xff\xfe not text").is_err());
    }

    #[test]
    fn loaded_transfers_go_after_those_listed_and_finished_ones_can_be_forgotten() {
        let mut list = vec![
            transfer("going-1", TransferState::Transferring, None),
            transfer("new-2", TransferState::Done, Some(3_000)),
        ];
        let kept = vec![
            transfer("old-3", TransferState::Cancelled, Some(2_000)),
            // A transfer that resumed is listed as it goes.
            transfer("going-1", TransferState::Done, Some(1_500)),
            transfer("old-4", TransferState::Done, Some(1_000)),
        ];
        assert!(take_in(&mut list, kept.clone()));
        assert_eq!(ids(&list), ["going-1", "new-2", "old-3", "old-4"]);
        assert_eq!(list[0].state, TransferState::Transferring);
        assert!(!take_in(&mut list, kept));

        // The limit holds for what is taken in too.
        let many: Vec<TransferStatus> = (0..KEEP_FINISHED as u64)
            .map(|n| transfer(&format!("k-{n}"), TransferState::Done, Some(500 - n)))
            .collect();
        assert!(take_in(&mut list, many));
        assert_eq!(list.len(), KEEP_FINISHED + 1);
        assert_eq!(ids(&list)[..4], ["going-1", "new-2", "old-3", "old-4"]);

        assert!(forget_finished(&mut list));
        assert_eq!(ids(&list), ["going-1"]);
        assert!(!forget_finished(&mut list));
    }

    #[test]
    fn the_history_an_earlier_agent_kept_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transfers.toml");
        save(&path, &[transfer("a-1", TransferState::Done, Some(5))]).unwrap();
        std::fs::write(temporary(&path), "left from a write cut short").unwrap();
        remove_left_behind(&path);
        assert!(!path.exists());
        assert!(!temporary(&path).exists());
        // Nothing to remove.
        remove_left_behind(&path);
    }

    #[tokio::test]
    async fn the_writer_writes_the_latest_list_until_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transfers.toml");
        let (list, rx) = watch::channel(Vec::new());
        let writer = HistoryWriter::new(HistoryStore::File(path.clone()), rx);
        for n in 0..5 {
            list.send_modify(|l| {
                l.insert(0, transfer(&format!("f-{n}"), TransferState::Done, Some(n)))
            });
            writer.changed();
        }
        writer.flush().await;
        assert_eq!(load(&path), *list.borrow());

        writer.close().await;
        list.send_modify(Vec::clear);
        writer.changed();
        writer.flush().await;
        assert_eq!(load(&path).len(), 5);
    }
}
