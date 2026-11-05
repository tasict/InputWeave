//! File transfer (docs/architecture.md §9): sending files and folders
//! to a device and receiving them, one transfer stream per file.
//!
//! What a user sends together is a batch, and each file or folder in it an
//! item (`listing`). The transfer list has a row per item: a file's row is
//! the file's transfer; a folder's sums up its files, which go one by one
//! like any other file, each with its path in the folder, and are kept
//! here only while they go. One item that cannot be sent, or one file of a
//! folder, fails on its own; the rest goes on. Only the receiver turning a
//! batch away ends all of it.
//!
//! Every transfer is a task. A sender waits for its turn (see `queue`),
//! hashes the file, waits for the device to be connected, offers the file,
//! and sends what the receiver does not have yet, at the lowest stream
//! priority and as fast as the device's pacer lets it (see `throttle`);
//! when the connection drops it waits and resumes. A receiver decides
//! about a batch the first time one of its files is offered (its limits,
//! and its user's answer when asked), writes each file into a hidden part
//! file named after the transfer, so a resumed transfer continues where it
//! stopped, checks the whole file's BLAKE3 at the end, and only then gives
//! it its name: a lone file in the folder, numbered if the name is taken;
//! a file of a folder in the folder received for it, created once for the
//! whole folder under a name not taken yet.
//!
//! Files another device drags here go where this device's user dropped
//! them, and nowhere else: the batch names the drag, and the folder is the
//! one accepted for it here (`expect_drag`). Files copied on another
//! device and pasted here likewise go only into the folder kept here for
//! that clipboard offer (`expect_clipboard`).
//!
//! On Windows the agent runs as SYSTEM and never reads or writes the
//! user's files (`Store::Broker`, see `broker`). Receiving, the broker
//! writes the part file as the user, reports what it already holds, hashes
//! it at the end, and names it when the agent, which keeps the transfer's
//! state here, says the hash matches. Without a broker, files are declined
//! as on a device that does not receive them. Sending, the broker lists
//! the folders and opens the files as the user, hashes them and reads the
//! parts the transfer sends, so a process of the user that asks to send a
//! file the user cannot read gets nowhere; without a broker nothing is
//! sent.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use iw_core::layout::DeviceId;
use iw_net::{FramedRecv, FramedSend, NetError, SessionInfo, priority, stream_code};
use iw_proto::peer::v1::{
    Chunk, DragItem, FileAnswer, FileBatch, FileDecline, FileDone, FileOffer, Transfer, transfer,
};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::{oneshot, watch};
use tracing::{debug, info, warn};

use super::app::{AppOpener, Need};
use super::directory::Directory;
use super::history::{self, HistoryStore, HistoryWriter};
use super::listing::{
    self, ItemKind, LocalLister, MAX_BYTES, MAX_FILES, MAX_ITEMS, MAX_LISTED_FAILURES, Manifest,
};
use super::queue::SendQueue;
use super::throttle::{MAX_CHUNK, Pacer, PacingStatus};
use super::transfer::FRAME_LIMIT;

/// The stream code either side resets or stops a transfer with when its
/// user cancels.
const CANCELLED: u32 = 1;
/// The stream code a receiver stops a transfer with when it cannot go on
/// for now, as when the Windows broker restarts; the sender tries again.
const INTERRUPTED: u32 = 2;
/// Files received at once, from all devices; more are turned away as
/// busy, and their senders try again. Each sender sends at most
/// `queue::MAX_ACTIVE`.
const MAX_INCOMING: usize = 4;
/// How long a sender waits for the other side's user to accept.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// How long a receiver waits for its own user to decide.
const ASK_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// How long a sender waits for the receiver to hash what it received.
const DONE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const RETRY: Duration = Duration::from_secs(2);
/// How often progress is published.
const PROGRESS_EVERY: Duration = Duration::from_millis(200);
/// Part files left from transfers that never resumed are removed after
/// this long.
const STALE_PART: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const PART_PREFIX: &str = ".inputweave-";
const PART_SUFFIX: &str = ".part";
/// Batches a receiver remembers its decisions about, so that files of a
/// batch resuming later are not asked about again; beyond this, those
/// heard from last longest ago are forgotten.
const MAX_BATCHES: usize = 256;
/// How long a drag dropped here waits for its files to start coming.
pub(crate) const DRAG_WAIT: Duration = Duration::from_secs(10 * 60);
/// Folders numbered " (1)" and on, at most, before giving up.
const MAX_NUMBERED: u32 = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Send,
    Receive,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransferState {
    /// Sender: waiting for its turn behind the files sent to the device
    /// before it.
    Queued,
    /// Sender: hashing the file.
    Preparing,
    /// The device is not connected; the transfer goes on when it is.
    WaitingForConnection,
    /// Waiting for a user to accept: the other one when sending, this one
    /// when receiving.
    WaitingForAnswer,
    Transferring,
    /// Receiver: checking the file's hash.
    Verifying,
    Done,
    Declined(FileDecline),
    Failed(&'static str),
    Cancelled,
}

/// Every reason a transfer fails with (`TransferState::Failed`).
pub(crate) const FAILURES: [&str; 8] = [
    "unreadable",
    "file_changed",
    "corrupt",
    "write_failed",
    "symlink",
    "not_a_file",
    "invalid_path",
    "unknown",
];

/// The reason in [`FAILURES`] that `reason` names, or "unknown".
pub(crate) fn failure(reason: &str) -> &'static str {
    FAILURES
        .into_iter()
        .find(|r| *r == reason)
        .unwrap_or("unknown")
}

impl TransferState {
    pub fn finished(&self) -> bool {
        matches!(
            self,
            Self::Done | Self::Declined(_) | Self::Failed(_) | Self::Cancelled
        )
    }

    /// Why the transfer was declined or failed, as clients are told; empty
    /// otherwise.
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Declined(FileDecline::Rejected) => "rejected",
            Self::Declined(FileDecline::NoSpace) => "no_space",
            Self::Declined(FileDecline::Busy) => "busy",
            Self::Declined(FileDecline::TooLarge) => "too_large",
            Self::Declined(FileDecline::InvalidPath) => "invalid_path",
            Self::Declined(_) => "unavailable",
            Self::Failed(reason) => reason,
            _ => "",
        }
    }

    /// How much is going on: a folder shows what the busiest of its files
    /// does.
    fn rank(&self) -> u8 {
        match self {
            Self::Transferring => 6,
            Self::Verifying => 5,
            Self::WaitingForAnswer => 4,
            Self::Preparing => 3,
            Self::WaitingForConnection => 2,
            Self::Queued => 1,
            _ => 0,
        }
    }
}

/// Whether a decline stands for the rest of its batch.
fn ends_batch(decline: FileDecline) -> bool {
    !matches!(decline, FileDecline::Busy | FileDecline::InvalidPath)
}

/// A row of the transfer list: an item of a batch, a file or a folder.
#[derive(Clone, Debug, PartialEq)]
pub struct TransferStatus {
    /// A file's transfer id; a folder's item id. The same on both devices.
    pub id: String,
    pub direction: Direction,
    pub device: String,
    pub name: String,
    /// A folder's files together.
    pub size: u64,
    pub done: u64,
    pub bytes_per_second: u64,
    pub state: TransferState,
    /// With FileDecline::NoSpace: bytes missing on the receiver.
    pub missing_bytes: u64,
    /// Received: where the file or folder was saved. Sent: the file or
    /// folder.
    pub path: Option<PathBuf>,
    pub started: SystemTime,
    /// When it finished; none while it is going.
    pub finished: Option<SystemTime>,
    /// The batch it came in; none from devices before batches.
    pub batch: Option<BatchInfo>,
    /// A folder's files, counted.
    pub folder: Option<FolderProgress>,
}

/// The batch an item belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchInfo {
    pub id: String,
    /// The item's place in the batch, from 0.
    pub index: u32,
    pub items: u32,
    /// Files and empty folders in the batch, and its bytes, as offered.
    pub files: u32,
    pub bytes: u64,
    /// The drag that carried it.
    pub drag: Option<String>,
    /// The clipboard offer whose files it carries, pasted on the receiver.
    pub clipboard: Option<u64>,
}

/// What arrived of the files of a clipboard offer pasted here, kept apart
/// from the transfer list, which forgets old rows and which the user may
/// clear meanwhile.
#[derive(Clone, Debug)]
pub(crate) struct Arrival {
    /// The items that ended, by their place in the offer: where each is,
    /// if it arrived.
    pub(crate) ended: BTreeMap<u32, Option<PathBuf>>,
    /// Items with files listed here, which end with their rows.
    pub(crate) listed: BTreeSet<u32>,
    /// A file was declined for a reason that ends the batch: the origin
    /// sends nothing more of it.
    pub(crate) refused: bool,
    /// A file of them was offered here.
    pub(crate) started: bool,
    /// When anything of them last moved, or the origin said they still
    /// come.
    pub(crate) progressed: Instant,
    /// How many of their files this device checks and names now, once
    /// all of each came: the origin waits on this device meanwhile, for
    /// as long as writing out and hashing a large file takes
    /// ([`Patience::checking`]).
    pub(crate) checking: u32,
}

impl Arrival {
    pub(crate) fn new() -> Self {
        Self {
            ended: BTreeMap::new(),
            listed: BTreeSet::new(),
            refused: false,
            started: false,
            progressed: Instant::now(),
            checking: 0,
        }
    }
}

/// Files of a clipboard offer pasted here: where they go, what of them
/// arrived, and how far each item is.
struct Receipt {
    /// Which receipt this is: another may be made under the same key once
    /// this one is forgotten.
    number: u64,
    folder: PathBuf,
    arrival: watch::Sender<Arrival>,
    tallies: HashMap<u32, ItemTally>,
}

/// The files of an item of a clipboard offer that ended, kept apart from
/// its row: a file declined before it was listed is never counted there,
/// so a folder with one would never end.
#[derive(Default)]
struct ItemTally {
    /// Files and empty folders in it, as offered.
    files: u32,
    /// Declined before they were listed.
    unlisted: u32,
    /// Listed, and ended since.
    settled: u32,
    /// Any of them arrived.
    arrived: bool,
}

impl ItemTally {
    /// Whether the item ended only by what its row does not count.
    fn ended_unlisted(&self) -> bool {
        self.unlisted > 0 && self.unlisted.saturating_add(self.settled) >= self.files
    }
}

/// The offer a row of pasted files belongs to, by origin and offer id, and
/// its item's place in it.
type ClipboardItem = ((DeviceId, u64), u32);

/// What a batch is sent for, when not just sent: a drag, or a paste of
/// files a clipboard offer carried.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Carrier<'a> {
    Sent,
    Drag(&'a str),
    Clipboard(u64),
}

/// How a folder's files went.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FolderProgress {
    /// Files and empty folders in it. Sending, those that could not be
    /// read count too; receiving, those offered.
    pub entries: u32,
    pub done: u32,
    /// Those that failed or were declined on their own.
    pub failed: u32,
    /// The first [`MAX_LISTED_FAILURES`] of them.
    pub failures: Vec<FailedEntry>,
}

impl FolderProgress {
    fn fail(&mut self, path: &str, reason: &str) {
        self.failed += 1;
        if self.failures.len() < MAX_LISTED_FAILURES {
            self.failures.push(FailedEntry {
                path: path.to_owned(),
                reason: reason.to_owned(),
            });
        }
    }

    /// Whether every file and folder in it arrived or failed.
    fn accounted(&self) -> bool {
        self.done + self.failed >= self.entries
    }
}

/// A file or folder in a folder that failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailedEntry {
    /// Its path in the batch: the folder's name, the folders down to it,
    /// its name, separated by "/".
    pub path: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileSettings {
    /// Where received files go; none means this device does not receive.
    /// Where the Windows broker writes them, none means the folder the
    /// broker names, and they are received only while it is connected.
    pub folder: Option<PathBuf>,
    /// Ask the user about every batch, instead of accepting files from
    /// paired devices.
    pub ask: bool,
}

/// Who reads the files sent and writes the files received.
pub(crate) enum Store {
    /// This process.
    Local,
    /// The Windows broker, as the user.
    #[cfg(any(unix, windows))]
    Broker(Arc<super::broker::Broker>),
}

impl Store {
    /// Whether names are made ones Windows allows.
    fn windows_names(&self) -> bool {
        match self {
            Self::Local => cfg!(windows),
            #[cfg(any(unix, windows))]
            Self::Broker(_) => true,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error("{0} is not a trusted device")]
    UnknownDevice(String),
    /// This device's user turned files with that device off.
    #[error("files with {0} are turned off on this device")]
    FilesOff(String),
    #[error("{0} is not a file or a folder")]
    NotAFile(PathBuf),
    #[error("{0} is a symbolic link")]
    Symlink(PathBuf),
    /// Files are read as the logged-on user, through the broker, and none
    /// is connected.
    #[error("files are read as the logged-on user, and no user's broker is connected to read them")]
    NoBroker,
    #[error("cannot read {path}: {reason}")]
    Unreadable { path: PathBuf, reason: String },
    /// More items than may go at once: in the batch, or with those still
    /// going.
    #[error("{0} items is more than {MAX_ITEMS} going at once")]
    TooManyItems(usize),
    #[error("more than {MAX_FILES} files and folders, or {MAX_BYTES} bytes, at once")]
    TooLarge,
}

impl SendError {
    /// The error code clients are told.
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownDevice(_) => "unknown_device",
            Self::FilesOff(_) => "files_off",
            Self::NotAFile(_) => "not_a_file",
            Self::Symlink(_) => "symlink",
            Self::NoBroker => "no_broker",
            Self::Unreadable { .. } => "unreadable",
            Self::TooManyItems(_) => "too_many_items",
            Self::TooLarge => "too_large",
        }
    }
}

impl From<listing::ListError> for SendError {
    fn from(e: listing::ListError) -> Self {
        match e {
            listing::ListError::NoBroker => Self::NoBroker,
            listing::ListError::TooManyItems(n) => Self::TooManyItems(n),
            listing::ListError::TooLarge => Self::TooLarge,
        }
    }
}

/// Why a transfer cannot be taken off the list.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum RemoveError {
    #[error("no transfer {0:?} is listed")]
    NotFound(String),
    #[error("transfer {0:?} is still going; cancel it first")]
    NotFinished(String),
}

struct Control {
    cancel: watch::Sender<bool>,
    /// Set while a file to send waits for its turn.
    turn: Option<oneshot::Sender<()>>,
}

/// The files of folders while they go, and the folders' sums.
#[derive(Default)]
struct Children {
    /// By transfer id.
    files: HashMap<String, Child>,
    /// Folders going, by row id.
    folders: HashMap<String, Tally>,
}

struct Child {
    /// The folder's row.
    row: String,
    /// Its path in the batch.
    relative: String,
    status: TransferStatus,
}

#[derive(Default)]
struct Tally {
    /// Files not finished yet, queued ones too.
    going: BTreeSet<String>,
    /// Bytes of the files done.
    done_bytes: u64,
    /// Cancelled as a whole.
    cancelled: bool,
}

/// What a receiver decided about a batch, and keeps for its files.
struct IncomingBatch {
    /// Where its items go: the receive folder, or where its drag was
    /// dropped.
    folder: PathBuf,
    /// Accepted, or declined why; none while its user is asked.
    decision: watch::Sender<Option<Result<(), FileDecline>>>,
    /// Its user was asked already, so the app is opened for it once.
    asked: AtomicBool,
    /// The names its folders were received under, by item id, decided
    /// once each.
    folders: Mutex<HashMap<String, Arc<tokio::sync::Mutex<Option<String>>>>>,
    /// Items cancelled here.
    cancelled: Mutex<HashSet<String>>,
    /// Files offered so far, by transfer id.
    offered: Mutex<HashSet<String>>,
    /// When a file of it was last offered.
    last: Mutex<Instant>,
}

impl IncomingBatch {
    fn decide(&self, accept: bool) -> bool {
        self.decision.send_if_modified(|decision| {
            if decision.is_some() {
                return false;
            }
            *decision = Some(if accept {
                Ok(())
            } else {
                Err(FileDecline::Rejected)
            });
            true
        })
    }

    /// Counts a file offered; false past [`MAX_FILES`], whatever the
    /// batch said it holds.
    fn count(&self, id: &str) -> bool {
        let mut offered = self.offered.lock().unwrap();
        offered.contains(id) || (offered.len() < MAX_FILES && offered.insert(id.to_owned()))
    }

    /// The name folder item `item`, called `name`, is received under in
    /// `self.folder`: created the first time, never one already there.
    async fn claim(&self, store: &Store, item: &str, name: &str) -> std::io::Result<String> {
        let slot = self
            .folders
            .lock()
            .unwrap()
            .entry(item.to_owned())
            .or_default()
            .clone();
        let mut claimed = slot.lock().await;
        if let Some(claimed) = claimed.as_ref() {
            return Ok(claimed.clone());
        }
        let path = match store {
            Store::Local => {
                let (folder, name) = (self.folder.clone(), name.to_owned());
                tokio::task::spawn_blocking(move || claim_local(&folder, &name))
                    .await
                    .map_err(std::io::Error::other)??
            }
            #[cfg(any(unix, windows))]
            Store::Broker(broker) => broker.claim_folder(&self.folder, name).await?,
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| std::io::Error::other("the folder has no name"))?;
        *claimed = Some(name.clone());
        Ok(name)
    }
}

pub(crate) struct Files {
    /// The rows, newest first.
    transfers: watch::Sender<Vec<TransferStatus>>,
    controls: Mutex<HashMap<String, Control>>,
    /// Locked after `controls` and before the list, never the other way.
    children: Mutex<Children>,
    /// Batches being received, by sender and batch id.
    incoming_batches: Mutex<HashMap<(DeviceId, String), Arc<IncomingBatch>>>,
    /// Held while deciding about a batch seen for the first time.
    deciding: tokio::sync::Mutex<()>,
    /// Batches the receiver turned away, while their files end.
    declined: Mutex<HashMap<String, FileDecline>>,
    /// Drags dropped here, by sender and drag id: where their files go and
    /// when.
    drags: Mutex<HashMap<(DeviceId, String), (PathBuf, Instant)>>,
    /// Files pasted here from a clipboard offer, by origin and offer id:
    /// the folder they are fetched into, and what of them arrived.
    clipboards: Mutex<HashMap<(DeviceId, u64), Receipt>>,
    /// How many receipts were made, numbering them.
    receipts: AtomicU64,
    /// The rows of those files, by row id.
    clipboard_rows: Mutex<HashMap<String, ClipboardItem>>,
    settings: Mutex<FileSettings>,
    /// Folders whose leftovers are being or were removed (`tidy`), each
    /// with a signal that turns true once that is done.
    tidied: Mutex<HashMap<PathBuf, watch::Receiver<bool>>>,
    store: Store,
    /// The live session with each device, for opening transfer streams.
    sessions: watch::Sender<HashMap<DeviceId, SessionInfo>>,
    directory: Arc<Directory>,
    /// Opens the app to ask about files when none is connected.
    app: Arc<AppOpener>,
    incoming: AtomicUsize,
    /// Which files to send go now. Locked before `controls` and the
    /// transfer list, never after.
    queue: Mutex<SendQueue>,
    /// How fast files may go to each device.
    pacers: Mutex<HashMap<DeviceId, Arc<Pacer>>>,
    next_id: Mutex<u64>,
    /// Keeps the finished transfers across restarts, when there is
    /// somewhere to keep them.
    history: Option<Arc<HistoryWriter>>,
}

/// One file or empty folder to send, and how it is offered.
struct Job {
    id: String,
    source: Source,
    batch: FileBatch,
    relative: String,
    folder: bool,
    cancel: watch::Receiver<bool>,
    turn: oneshot::Receiver<()>,
}

impl Files {
    /// Files sent and received, listing at first the finished transfers
    /// kept in a file of `history`, which then keeps them. The broker's
    /// history is listed once a broker loads it (`history::follow_broker`).
    pub(crate) fn new(
        settings: FileSettings,
        directory: Arc<Directory>,
        app: Arc<AppOpener>,
        store: Store,
        history: Option<HistoryStore>,
        tidy_now: bool,
    ) -> Arc<Self> {
        // The broker cleans the folders it writes to itself. Where macOS
        // protects the receive folder (in Downloads), listing it now could
        // have macOS ask the user out of nowhere, so that waits until files
        // are written there (`tidy`); before any transfer has begun, here.
        let mut tidied = HashMap::new();
        if let (true, Store::Local, Some(folder)) = (tidy_now, &store, &settings.folder) {
            remove_stale_parts(folder);
            tidied.insert(folder.clone(), watch::channel(true).1);
        }
        let kept = match &history {
            Some(HistoryStore::File(path)) => history::load(path),
            _ => Vec::new(),
        };
        let transfers = watch::Sender::new(kept);
        let history = history.map(|store| HistoryWriter::new(store, transfers.subscribe()));
        Arc::new(Self {
            transfers,
            controls: Mutex::new(HashMap::new()),
            children: Mutex::new(Children::default()),
            incoming_batches: Mutex::new(HashMap::new()),
            deciding: tokio::sync::Mutex::new(()),
            declined: Mutex::new(HashMap::new()),
            drags: Mutex::new(HashMap::new()),
            clipboards: Mutex::new(HashMap::new()),
            receipts: AtomicU64::new(0),
            clipboard_rows: Mutex::new(HashMap::new()),
            settings: Mutex::new(settings),
            tidied: Mutex::new(tidied),
            store,
            sessions: watch::Sender::new(HashMap::new()),
            directory,
            app,
            incoming: AtomicUsize::new(0),
            queue: Mutex::new(SendQueue::default()),
            pacers: Mutex::new(HashMap::new()),
            next_id: Mutex::new(0),
            history,
        })
    }

    /// Removes what transfers into `folder` left behind long ago, once a
    /// run, before the first file is written there: no part file is open
    /// there yet, so none still in use goes. Files for `folder` that come
    /// meanwhile wait for it; files for other folders do not. Off the
    /// runtime: macOS may keep the listing waiting while it asks the user
    /// about Downloads. The removal goes on by itself, so a file that stops
    /// waiting neither stops it nor lets the next file in before it is done.
    async fn tidy(&self, folder: &Path) {
        if !matches!(self.store, Store::Local) {
            return;
        }
        let mut done = {
            let mut tidied = self.tidied.lock().unwrap();
            if let Some(done) = tidied.get(folder) {
                done.clone()
            } else {
                let (finished, done) = watch::channel(false);
                tidied.insert(folder.to_owned(), done.clone());
                let folder = folder.to_owned();
                tokio::spawn(async move {
                    let _ = tokio::task::spawn_blocking(move || remove_stale_parts(&folder)).await;
                    let _ = finished.send(true);
                });
                done
            }
        };
        // Done, or the runtime is going away.
        let _ = done.wait_for(|done| *done).await;
    }

    pub(crate) fn session_up(&self, device: &DeviceId, info: &SessionInfo) {
        self.pacer(device).connected();
        self.sessions.send_modify(|sessions| {
            sessions.insert(device.clone(), info.clone());
        });
    }

    /// The round trip of the session with `device`, sampled every
    /// heartbeat; it decides how fast files may go there.
    pub(crate) fn sample_rtt(&self, device: &DeviceId, rtt: Duration) {
        if let Some(change) = self.pacer(device).sample(rtt, Instant::now()) {
            debug!(%device, ?rtt, ?change, "file pacing changed");
        }
    }

    /// How fast files may go to each device, for diagnostics.
    pub(crate) fn pacing(&self) -> Vec<PacingStatus> {
        let mut all: Vec<PacingStatus> = self
            .pacers
            .lock()
            .unwrap()
            .iter()
            .map(|(device, pacer)| pacer.status(device.as_str()))
            .collect();
        all.sort_by(|a, b| a.device.cmp(&b.device));
        all
    }

    fn pacer(&self, device: &DeviceId) -> Arc<Pacer> {
        self.pacers
            .lock()
            .unwrap()
            .entry(device.clone())
            .or_insert_with(|| Arc::new(Pacer::new()))
            .clone()
    }

    pub(crate) fn session_down(&self, device: &DeviceId, info: &SessionInfo) {
        self.sessions.send_if_modified(|sessions| {
            // A newer session may have replaced this one already.
            let ours = sessions
                .get(device)
                .is_some_and(|s| s.connection.stable_id() == info.connection.stable_id());
            ours && sessions.remove(device).is_some()
        });
    }

    pub(crate) fn watch(&self) -> watch::Receiver<Vec<TransferStatus>> {
        self.transfers.subscribe()
    }

    pub(crate) fn transfers(&self) -> Vec<TransferStatus> {
        self.transfers.borrow().clone()
    }

    /// The settings in effect: where the broker writes files, the folder
    /// is the one the user chose or else the broker's, and none while no
    /// broker is connected.
    pub(crate) fn settings(&self) -> FileSettings {
        let mut settings = self.settings.lock().unwrap().clone();
        match &self.store {
            Store::Local => {}
            #[cfg(any(unix, windows))]
            Store::Broker(broker) => {
                settings.folder = broker
                    .receive_folder()
                    .map(|default| settings.folder.unwrap_or(default));
            }
        }
        settings
    }

    pub(crate) fn set_settings(&self, settings: FileSettings) {
        *self.settings.lock().unwrap() = settings;
        self.settings_changed();
    }

    /// Tells clients the settings in effect changed, as when the broker
    /// comes or goes.
    pub(crate) fn settings_changed(&self) {
        // Clients get settings with the transfer list.
        self.transfers.send_modify(|_| {});
    }

    /// The files and folders at `paths` as a clipboard offers them, only
    /// the top of each (`clipboard_files::top_level`): looked at by this
    /// process, or, as the user, through the broker, which reads what the
    /// user copied on Windows.
    pub(crate) async fn clipboard_items(&self, paths: Vec<PathBuf>) -> Vec<(PathBuf, DragItem)> {
        match &self.store {
            Store::Local => {
                tokio::task::spawn_blocking(move || super::clipboard_files::top_level(&paths))
                    .await
                    .unwrap_or_default()
            }
            #[cfg(any(unix, windows))]
            Store::Broker(broker) => {
                let lister = super::broker::BrokerLister(broker.clone());
                super::clipboard_files::top_level_listed(
                    lister,
                    paths,
                    super::clipboard_files::LIST_BUDGET,
                )
                .await
            }
        }
    }

    /// Lists the files and folders at `paths` to send them: as the user,
    /// through the broker, where it reads them. Refused when none can be
    /// sent, with the first one's reason.
    pub(crate) async fn manifest(&self, paths: Vec<PathBuf>) -> Result<Manifest, SendError> {
        let manifest = match &self.store {
            Store::Local => listing::list_items(&LocalLister, paths).await?,
            #[cfg(any(unix, windows))]
            Store::Broker(broker) => {
                let lister = super::broker::BrokerLister(broker.clone());
                listing::list_items(&lister, paths).await?
            }
        };
        // Every item going is listed in every message to the apps, so the
        // limit on a batch's items holds for all those going together.
        let going = self.sending_items();
        if going + manifest.items.len() > MAX_ITEMS {
            return Err(SendError::TooManyItems(going + manifest.items.len()));
        }
        if !manifest.items.iter().any(listing::Item::sendable)
            && let Some(first) = manifest.items.first()
        {
            let path = first.path.clone();
            return Err(match &first.kind {
                ItemKind::Failed("symlink") => SendError::Symlink(path),
                ItemKind::Failed("unreadable") => SendError::Unreadable {
                    path,
                    reason: if first.detail.is_empty() {
                        "it cannot be read".to_owned()
                    } else {
                        first.detail.clone()
                    },
                },
                _ => SendError::NotAFile(path),
            });
        }
        Ok(manifest)
    }

    /// Sends the files and folders at `paths` to `device` as a batch,
    /// after the files already queued for it, and returns its items' ids,
    /// in order. Items that cannot be sent are listed as failed.
    pub(crate) async fn send(
        self: &Arc<Self>,
        device: &str,
        paths: Vec<PathBuf>,
    ) -> Result<Vec<String>, SendError> {
        match self.directory.peer(device) {
            None => return Err(SendError::UnknownDevice(device.to_owned())),
            Some(peer) if peer.files_off => return Err(SendError::FilesOff(device.to_owned())),
            Some(_) => {}
        }
        let manifest = self.manifest(paths).await?;
        Ok(self.send_manifest(device, &manifest, Carrier::Sent))
    }

    /// Sends what `manifest` lists to `device` as a batch, for `carrier`;
    /// returns its items' ids, in order.
    pub(crate) fn send_manifest(
        self: &Arc<Self>,
        device: &str,
        manifest: &Manifest,
        carrier: Carrier<'_>,
    ) -> Vec<String> {
        let drag = match carrier {
            Carrier::Drag(drag) => Some(drag),
            _ => None,
        };
        let clipboard = match carrier {
            Carrier::Clipboard(offer_id) => Some(offer_id),
            _ => None,
        };
        let batch_id = self.new_id();
        let (files, bytes) = manifest.totals();
        let items = manifest.items.len() as u32;
        let now = SystemTime::now();
        let mut rows = Vec::new();
        let mut jobs = Vec::new();
        let mut any_finished = false;
        {
            let mut queue = self.queue.lock().unwrap();
            let mut controls = self.controls.lock().unwrap();
            let mut children = self.children.lock().unwrap();
            // Joins the queue and gets the way to give it its turn, in one
            // go, so `leave_queue` always finds the transfer it lets go.
            let mut enqueue = |id: &str| {
                let goes = queue.push(device, id);
                let (cancel_tx, cancel) = watch::channel(false);
                let (give_turn, turn) = oneshot::channel();
                let give_turn = if goes {
                    let _ = give_turn.send(());
                    None
                } else {
                    Some(give_turn)
                };
                controls.insert(
                    id.to_owned(),
                    Control {
                        cancel: cancel_tx,
                        turn: give_turn,
                    },
                );
                let state = if goes {
                    TransferState::Preparing
                } else {
                    TransferState::Queued
                };
                (state, cancel, turn)
            };
            for (index, item) in manifest.items.iter().enumerate() {
                let (item_files, item_bytes) = item.totals();
                let info = BatchInfo {
                    id: batch_id.clone(),
                    index: index as u32,
                    items,
                    files: files as u32,
                    bytes,
                    drag: drag.map(str::to_owned),
                    clipboard,
                };
                let wire = |item_id: &str| FileBatch {
                    batch_id: batch_id.clone(),
                    item_id: item_id.to_owned(),
                    item_index: index as u32,
                    items,
                    files: files as u32,
                    bytes,
                    item_files: item_files as u32,
                    item_bytes,
                    drag_id: drag.unwrap_or_default().to_owned(),
                    clipboard_offer_id: clipboard.unwrap_or_default(),
                };
                let row = TransferStatus {
                    id: String::new(),
                    direction: Direction::Send,
                    device: device.to_owned(),
                    name: item.name.clone(),
                    size: item_bytes,
                    done: 0,
                    bytes_per_second: 0,
                    state: TransferState::Queued,
                    missing_bytes: 0,
                    path: Some(item.path.clone()),
                    started: now,
                    finished: None,
                    batch: Some(info),
                    folder: None,
                };
                match &item.kind {
                    ItemKind::Failed(reason) => {
                        any_finished = true;
                        rows.push(TransferStatus {
                            id: self.new_id(),
                            state: TransferState::Failed(reason),
                            finished: Some(now),
                            ..row
                        });
                    }
                    ItemKind::File(_) => {
                        let id = self.new_id();
                        let (state, cancel, turn) = enqueue(&id);
                        jobs.push(Job {
                            id: id.clone(),
                            source: Source::new(&self.store, &id, item.path.clone(), false),
                            batch: wire(&id),
                            relative: item.name.clone(),
                            folder: false,
                            cancel,
                            turn,
                        });
                        rows.push(TransferStatus { id, state, ..row });
                    }
                    ItemKind::Folder { entries, failures } => {
                        let row_id = self.new_id();
                        let mut progress = FolderProgress {
                            entries: (entries.len() + failures.len()) as u32,
                            ..FolderProgress::default()
                        };
                        for failed in failures {
                            progress.fail(&failed.relative, failed.reason);
                        }
                        let mut tally = Tally::default();
                        let mut state = TransferState::Queued;
                        for entry in entries {
                            let id = self.new_id();
                            let (file_state, cancel, turn) = enqueue(&id);
                            if file_state.rank() > state.rank() {
                                state = file_state.clone();
                            }
                            let name = entry
                                .relative
                                .rsplit('/')
                                .next()
                                .unwrap_or_default()
                                .to_owned();
                            tally.going.insert(id.clone());
                            children.files.insert(
                                id.clone(),
                                Child {
                                    row: row_id.clone(),
                                    relative: entry.relative.clone(),
                                    status: TransferStatus {
                                        id: id.clone(),
                                        name,
                                        size: entry.size,
                                        state: file_state,
                                        path: Some(entry.path.clone()),
                                        batch: None,
                                        ..row.clone()
                                    },
                                },
                            );
                            jobs.push(Job {
                                id: id.clone(),
                                source: Source::new(
                                    &self.store,
                                    &id,
                                    entry.path.clone(),
                                    entry.folder,
                                ),
                                batch: wire(&row_id),
                                relative: entry.relative.clone(),
                                folder: entry.folder,
                                cancel,
                                turn,
                            });
                        }
                        if entries.is_empty() {
                            // Nothing in it could be read.
                            any_finished = true;
                            let reason = failure(
                                progress
                                    .failures
                                    .first()
                                    .map_or("unreadable", |f| f.reason.as_str()),
                            );
                            state = TransferState::Failed(reason);
                        } else {
                            children.folders.insert(row_id.clone(), tally);
                        }
                        let finished = state.finished().then_some(now);
                        rows.push(TransferStatus {
                            id: row_id,
                            state,
                            finished,
                            folder: Some(progress),
                            ..row
                        });
                    }
                }
            }
            // All listed at once, the batch's last item on top.
            let ids: Vec<String> = rows.iter().map(|r| r.id.clone()).collect();
            self.transfers.send_modify(|list| {
                list.splice(0..0, rows.into_iter().rev());
                if any_finished {
                    history::trim(list);
                }
            });
            drop(children);
            drop(controls);
            drop(queue);
            if any_finished {
                self.history_changed();
            }
            info!(
                batch = %batch_id,
                items,
                files,
                bytes,
                drag = drag.is_some(),
                clipboard = clipboard.is_some(),
                "sending a batch"
            );
            let device = DeviceId::new(device);
            for job in jobs {
                let place = Place {
                    files: self.clone(),
                    device: device.clone(),
                    id: job.id.clone(),
                };
                tokio::spawn({
                    let (files, device) = (self.clone(), device.clone());
                    async move { send_file(files, job, device, place).await }
                });
            }
            ids
        }
    }

    /// Takes a file to send out of its device's queue once its transfer
    /// ended, and gives the next ones their turn.
    fn leave_queue(&self, device: &DeviceId, id: &str) {
        let mut queue = self.queue.lock().unwrap();
        for next in queue.remove(device.as_str(), id) {
            let turn = self
                .controls
                .lock()
                .unwrap()
                .get_mut(&next)
                .and_then(|c| c.turn.take());
            // Listed as going before its task even wakes, so the list never
            // shows a file going ahead of one sent before it. One that was
            // cancelled meanwhile is about to leave the queue too.
            self.update(&next, |t| {
                if t.state == TransferState::Queued {
                    t.state = TransferState::Preparing;
                }
            });
            if let Some(turn) = turn {
                let _ = turn.send(());
            }
        }
    }

    /// Handles a file offered on a transfer stream from `from`.
    pub(crate) fn receive(
        self: &Arc<Self>,
        from: DeviceId,
        offer: FileOffer,
        tx: FramedSend,
        rx: FramedRecv,
    ) {
        let files = self.clone();
        tokio::spawn(async move { receive_file(files, from, offer, tx, rx).await });
    }

    /// Cancels what is still going of a transfer: a file, a folder with
    /// its files, or every item of a batch, by batch id. Returns whether
    /// there was any.
    pub(crate) fn cancel(&self, id: &str) -> bool {
        let rows: Vec<String> = {
            let list = self.transfers.borrow();
            let named = list.iter().any(|t| t.id == id);
            if named {
                vec![id.to_owned()]
            } else {
                list.iter()
                    .filter(|t| !t.state.finished() && t.batch.as_ref().is_some_and(|b| b.id == id))
                    .map(|t| t.id.clone())
                    .collect()
            }
        };
        let mut cancelled = false;
        let mut targets = Vec::new();
        let mut ended = Vec::new();
        for row in rows.iter().map(String::as_str).chain(
            // A file of a folder, by its own id.
            rows.is_empty().then_some(id),
        ) {
            let mut children = self.children.lock().unwrap();
            if let Some(tally) = children.folders.get_mut(row) {
                tally.cancelled = true;
                cancelled = true;
                if tally.going.is_empty() {
                    // Between files: nothing is left to end it.
                    ended.push(row.to_owned());
                } else {
                    targets.extend(tally.going.iter().cloned());
                }
            } else {
                targets.push(row.to_owned());
            }
        }
        for row in &ended {
            self.children.lock().unwrap().folders.remove(row);
            self.end_row(row, TransferState::Cancelled);
        }
        self.note_cancelled_items(&rows);
        let mut waiting = Vec::new();
        {
            let controls = self.controls.lock().unwrap();
            for target in targets {
                match controls.get(&target) {
                    Some(control) => {
                        let _ = control.cancel.send(true);
                        cancelled = true;
                    }
                    None => waiting.push(target),
                }
            }
        }
        // Files of a folder cut off while being received wait for their
        // sender to come back; nothing else would end them.
        for id in waiting {
            let child = self.children.lock().unwrap().files.contains_key(&id);
            if child {
                self.finish(&id, TransferState::Cancelled);
                cancelled = true;
            }
        }
        cancelled
    }

    /// Remembers incoming folders cancelled here, so their files still to
    /// come are turned away.
    fn note_cancelled_items(&self, rows: &[String]) {
        let list = self.transfers.borrow();
        for t in list.iter().filter(|t| rows.contains(&t.id)) {
            if t.direction != Direction::Receive {
                continue;
            }
            if let Some(batch) = self.incoming_batch_of(t) {
                batch.cancelled.lock().unwrap().insert(t.id.clone());
            }
        }
    }

    fn cancel_ids(&self, ids: &[String]) {
        let controls = self.controls.lock().unwrap();
        for id in ids {
            if let Some(control) = controls.get(id) {
                let _ = control.cancel.send(true);
            }
        }
    }

    /// The batch an incoming row belongs to, as kept here.
    fn incoming_batch_of(&self, row: &TransferStatus) -> Option<Arc<IncomingBatch>> {
        let id = row
            .batch
            .as_ref()
            .map_or(row.id.as_str(), |b| b.id.as_str());
        let key = (DeviceId::new(&row.device), id.to_owned());
        self.incoming_batches.lock().unwrap().get(&key).cloned()
    }

    /// This device's user's answer about the batch of the incoming
    /// transfer `id`. Returns whether it was waiting for one. Only a batch
    /// its user is asked about waits for a decision, and its row is listed
    /// as waiting before its offer gets round to asking, so an answer that
    /// comes that early counts.
    pub(crate) fn answer(&self, id: &str, accept: bool) -> bool {
        let row = self
            .transfers
            .borrow()
            .iter()
            .find(|t| t.id == id && t.direction == Direction::Receive)
            .cloned();
        let Some(batch) = row.and_then(|row| self.incoming_batch_of(&row)) else {
            return false;
        };
        batch.decide(accept)
    }

    /// Takes a finished transfer off the list, and out of the history. Its
    /// file stays.
    pub(crate) fn remove(&self, id: &str) -> Result<(), RemoveError> {
        let mut result = Err(RemoveError::NotFound(id.to_owned()));
        self.transfers.send_if_modified(|list| {
            let Some(at) = list.iter().position(|t| t.id == id) else {
                return false;
            };
            if !list[at].state.finished() {
                result = Err(RemoveError::NotFinished(id.to_owned()));
                return false;
            }
            list.remove(at);
            result = Ok(());
            true
        });
        if result.is_ok() {
            self.history_changed();
        }
        result
    }

    /// Takes every finished transfer off the list, and out of the history.
    /// Their files stay; transfers still going stay listed.
    pub(crate) fn clear(&self) {
        if self.transfers.send_if_modified(history::forget_finished) {
            self.history_changed();
        }
    }

    /// Takes the finished transfers off the list, but not out of the
    /// history: they are not this list's to keep any more, as when the
    /// broker that keeps them disconnects.
    pub(crate) fn forget_finished(&self) {
        self.transfers.send_if_modified(history::forget_finished);
    }

    /// Lists the finished transfers a history kept, after those listed.
    pub(crate) fn take_in_history(&self, kept: Vec<TransferStatus>) {
        self.transfers
            .send_if_modified(|list| history::take_in(list, kept));
    }

    pub(crate) fn history_writer(&self) -> Option<Arc<HistoryWriter>> {
        self.history.clone()
    }

    /// Writes the history once what changed so far is written, and stops
    /// writing it: the agent is stopping.
    pub(crate) async fn close_history(&self) {
        if let Some(history) = &self.history {
            history.close().await;
        }
    }

    fn history_changed(&self) {
        if let Some(history) = &self.history {
            history.changed();
        }
    }

    /// A new id for a transfer, a batch, a folder or a drag: unique across
    /// restarts and devices, so a receiver never mixes two transfers' part
    /// files.
    pub(crate) fn new_id(&self) -> String {
        let mut next = self.next_id.lock().unwrap();
        *next += 1;
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        format!("{nanos:x}-{:x}-{next}", std::process::id())
    }

    fn control(&self, id: &str) -> watch::Receiver<bool> {
        let (cancel, cancelled) = watch::channel(false);
        self.controls
            .lock()
            .unwrap()
            .insert(id.to_owned(), Control { cancel, turn: None });
        cancelled
    }

    /// Lists a transfer that just began.
    fn add(&self, status: TransferStatus) {
        self.transfers.send_modify(|list| list.insert(0, status));
    }

    /// Changes a transfer as it goes: a row, or a file of a folder, which
    /// the folder's row then sums up. One that ends goes through `finish`;
    /// a row that was finished and goes on again, as when a sender resumes
    /// a transfer this device's user was asked about, leaves the history.
    fn update(&self, id: &str, change: impl FnOnce(&mut TransferStatus)) {
        {
            let mut children = self.children.lock().unwrap();
            if let Some(child) = children.files.get_mut(id) {
                change(&mut child.status);
                let row = child.row.clone();
                self.refresh(&children, &row);
                drop(children);
                self.clipboard_moved(&row);
                return;
            }
        }
        self.clipboard_moved(id);
        let mut resumed = false;
        self.transfers.send_modify(|list| {
            if let Some(t) = list.iter_mut().find(|t| t.id == id) {
                let was_finished = t.state.finished();
                change(t);
                if was_finished && !t.state.finished() {
                    t.finished = None;
                    resumed = true;
                }
            }
        });
        if resumed {
            self.history_changed();
        }
    }

    /// Sums up a folder's row from its files.
    fn refresh(&self, children: &Children, row: &str) {
        let Some(tally) = children.folders.get(row) else {
            return;
        };
        let mut done = tally.done_bytes;
        let mut rate = 0;
        let mut busiest: Option<&TransferState> = None;
        for child in tally.going.iter().filter_map(|id| children.files.get(id)) {
            done += child.status.done;
            rate += child.status.bytes_per_second;
            if busiest.is_none_or(|b| child.status.state.rank() > b.rank()) {
                busiest = Some(&child.status.state);
            }
        }
        let state = busiest.filter(|s| s.rank() > 0).cloned();
        self.transfers.send_if_modified(|list| {
            let Some(t) = list.iter_mut().find(|t| t.id == row) else {
                return false;
            };
            if t.state.finished() {
                return false;
            }
            let before = (t.done, t.bytes_per_second, t.state.clone());
            t.done = done;
            t.bytes_per_second = rate;
            if let Some(state) = state {
                t.state = state;
            }
            before != (t.done, t.bytes_per_second, t.state.clone())
        });
    }

    /// The direction and batch of a transfer, a row or a file of one.
    fn batch_of(&self, id: &str) -> Option<(Direction, String)> {
        let row = {
            let children = self.children.lock().unwrap();
            children
                .files
                .get(id)
                .map_or_else(|| id.to_owned(), |c| c.row.clone())
        };
        let list = self.transfers.borrow();
        let t = list.iter().find(|t| t.id == row)?;
        Some((t.direction, t.batch.as_ref()?.id.clone()))
    }

    /// Lists how a transfer ended: a row, kept in the history, letting the
    /// transfers that finished first go beyond the limit; or a file of a
    /// folder, counted in the folder's row, which ends with its last file.
    fn finish(&self, id: &str, state: TransferState) {
        let batch = self.batch_of(id);
        // The rest of a batch the receiver turned away ends as declined.
        let state = match (&state, &batch) {
            (TransferState::Cancelled, Some((Direction::Send, batch))) => self
                .declined
                .lock()
                .unwrap()
                .get(batch)
                .map_or(state, |reason| TransferState::Declined(*reason)),
            _ => state,
        };
        info!(id, ?state, "file transfer ended");
        self.controls.lock().unwrap().remove(id);
        match self.finish_child(id, &state) {
            Some(siblings) => self.cancel_ids(&siblings),
            None => self.end_row(id, state.clone()),
        }
        if let (TransferState::Declined(reason), Some((Direction::Send, batch))) = (&state, batch)
            && ends_batch(*reason)
        {
            self.decline_batch(&batch, *reason);
        }
    }

    /// Ends a row as `state`.
    fn end_row(&self, id: &str, state: TransferState) {
        let mut listed = false;
        let mut arrived = None;
        self.transfers.send_modify(|list| {
            if let Some(t) = list.iter_mut().find(|t| t.id == id) {
                t.state = state.clone();
                t.bytes_per_second = 0;
                t.finished = Some(SystemTime::now());
                listed = true;
                arrived = t.path.clone();
            }
            history::trim(list);
        });
        self.clipboard_ended(id, arrived.filter(|_| state == TransferState::Done));
        if listed {
            self.history_changed();
        }
    }

    /// Counts a file of a folder that ended in the folder's row, and ends
    /// the row with its last one. Returns the other files to cancel when
    /// it was cancelled, which cancels the folder; none if `id` is no file
    /// of a folder.
    fn finish_child(&self, id: &str, state: &TransferState) -> Option<Vec<String>> {
        let mut children = self.children.lock().unwrap();
        let child = children.files.remove(id)?;
        let row = child.row.clone();
        let mut siblings = Vec::new();
        let tally = children.folders.entry(row.clone()).or_default();
        tally.going.remove(id);
        match state {
            TransferState::Done => tally.done_bytes += child.status.size,
            TransferState::Cancelled if !tally.cancelled => {
                tally.cancelled = true;
                siblings = tally.going.iter().cloned().collect();
            }
            _ => {}
        }
        let (cancelled, going, done_bytes) =
            (tally.cancelled, !tally.going.is_empty(), tally.done_bytes);
        let failure = match state {
            TransferState::Failed(reason) => Some(*reason),
            TransferState::Declined(FileDecline::InvalidPath) => Some("invalid_path"),
            _ => None,
        };
        let declined = match state {
            TransferState::Declined(reason) if ends_batch(*reason) => Some(*reason),
            _ => None,
        };
        let mut ended = false;
        let mut arrived = None;
        self.transfers.send_modify(|list| {
            let Some(t) = list.iter_mut().find(|t| t.id == row) else {
                return;
            };
            let Some(progress) = t.folder.as_mut() else {
                return;
            };
            if *state == TransferState::Done {
                progress.done += 1;
            } else if let Some(reason) = failure {
                progress.fail(&child.relative, reason);
            }
            if t.state.finished()
                || going
                || !(progress.accounted() || cancelled || declined.is_some())
            {
                return;
            }
            t.state = if let Some(reason) = declined {
                TransferState::Declined(reason)
            } else if cancelled {
                TransferState::Cancelled
            } else if progress.done > 0 {
                TransferState::Done
            } else {
                TransferState::Failed(failure_of(progress))
            };
            t.done = done_bytes;
            t.bytes_per_second = 0;
            t.finished = Some(SystemTime::now());
            ended = true;
            arrived = t.path.clone().filter(|_| t.state == TransferState::Done);
            history::trim(list);
        });
        if ended {
            children.folders.remove(&row);
            drop(children);
            self.clipboard_ended(&row, arrived);
            self.history_changed();
        } else {
            self.refresh(&children, &row);
            drop(children);
            self.clipboard_moved(&row);
            self.clipboard_child_ended(&row, *state == TransferState::Done);
        }
        Some(siblings)
    }

    /// The receiver turned batch `id` away: what is still to go of it ends
    /// as declined.
    fn decline_batch(&self, id: &str, reason: FileDecline) {
        {
            let mut declined = self.declined.lock().unwrap();
            if declined.contains_key(id) {
                return;
            }
            if declined.len() >= MAX_BATCHES {
                declined.clear();
            }
            declined.insert(id.to_owned(), reason);
        }
        info!(
            batch = id,
            ?reason,
            "the batch was declined; the rest of it ends"
        );
        let rows: Vec<String> = self
            .transfers
            .borrow()
            .iter()
            .filter(|t| !t.state.finished() && t.batch.as_ref().is_some_and(|b| b.id == id))
            .map(|t| t.id.clone())
            .collect();
        let mut targets = Vec::new();
        {
            let children = self.children.lock().unwrap();
            for row in rows {
                match children.folders.get(&row) {
                    Some(tally) => targets.extend(tally.going.iter().cloned()),
                    None => targets.push(row),
                }
            }
        }
        self.cancel_ids(&targets);
    }

    /// The session with `device`, once there is one.
    async fn session(&self, device: &DeviceId) -> SessionInfo {
        let mut sessions = self.sessions.subscribe();
        loop {
            if let Some(info) = sessions.borrow_and_update().get(device) {
                return info.clone();
            }
            if sessions.changed().await.is_err() {
                // The agent is stopping; wait to be dropped.
                std::future::pending::<()>().await;
            }
        }
    }

    /// Checks that `folder` is an absolute folder this device's user can
    /// create files in: where a drag from another device was dropped.
    pub(crate) async fn check_destination(&self, folder: &Path) -> std::io::Result<PathBuf> {
        if !folder.is_absolute() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "the folder is not an absolute path",
            ));
        }
        match &self.store {
            Store::Local => {
                let folder = folder.to_owned();
                tokio::task::spawn_blocking(move || check_folder_local(&folder))
                    .await
                    .map_err(std::io::Error::other)?
            }
            #[cfg(any(unix, windows))]
            Store::Broker(broker) => broker.check_folder(folder).await,
        }
    }

    /// Files from `from` for drag `drag` go into `folder`, which this
    /// device's user dropped them in, once they start coming within
    /// [`DRAG_WAIT`].
    pub(crate) fn expect_drag(&self, from: &DeviceId, drag: &str, folder: PathBuf) {
        let now = Instant::now();
        let mut drags = self.drags.lock().unwrap();
        drags.retain(|_, (_, at)| now.saturating_duration_since(*at) < DRAG_WAIT);
        drags.insert((from.clone(), drag.to_owned()), (folder, now));
    }

    /// Whether files from `from` for drag `drag` are still awaited: it was
    /// accepted here, and none has come yet.
    pub(crate) fn expects_drag(&self, from: &DeviceId, drag: &str) -> bool {
        self.drags
            .lock()
            .unwrap()
            .get(&(from.clone(), drag.to_owned()))
            .is_some_and(|(_, at)| at.elapsed() < DRAG_WAIT)
    }

    /// Drag `drag` from `from` will not bring files after all.
    pub(crate) fn forget_drag(&self, from: &DeviceId, drag: &str) {
        self.drags
            .lock()
            .unwrap()
            .remove(&(from.clone(), drag.to_owned()));
    }

    /// Files of clipboard offer `offer_id` from `from`, pasted here, go
    /// into `folder`, which the engine made for them. What of them arrives
    /// is told on the channel returned.
    pub(crate) fn expect_clipboard(
        &self,
        from: &DeviceId,
        offer_id: u64,
        folder: PathBuf,
    ) -> watch::Receiver<Arrival> {
        let (arrival, arriving) = watch::channel(Arrival::new());
        self.clipboards.lock().unwrap().insert(
            (from.clone(), offer_id),
            Receipt {
                number: self.receipts.fetch_add(1, Ordering::Relaxed),
                folder,
                arrival,
                tallies: HashMap::new(),
            },
        );
        arriving
    }

    /// The origin of clipboard offer `offer_id` says its files still come,
    /// waiting their turn there.
    pub(crate) fn clipboard_alive(&self, from: &DeviceId, offer_id: u64) {
        if let Some(receipt) = self
            .clipboards
            .lock()
            .unwrap()
            .get(&(from.clone(), offer_id))
        {
            receipt
                .arrival
                .send_modify(|arrival| arrival.progressed = Instant::now());
        }
    }

    /// Files of clipboard offer `offer_id` from `from` are no longer
    /// wanted here.
    pub(crate) fn forget_clipboard(&self, from: &DeviceId, offer_id: u64) {
        let key = (from.clone(), offer_id);
        self.clipboards.lock().unwrap().remove(&key);
        self.clipboard_rows
            .lock()
            .unwrap()
            .retain(|_, (k, _)| *k != key);
    }

    /// Cancels what still comes of the files of clipboard offer `offer_id`
    /// from `from`.
    pub(crate) fn cancel_clipboard(&self, from: &DeviceId, offer_id: u64) {
        let key = (from.clone(), offer_id);
        let rows: Vec<String> = self
            .clipboard_rows
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, (k, _))| *k == key)
            .map(|(row, _)| row.clone())
            .collect();
        for row in rows {
            self.cancel(&row);
        }
    }

    /// Notes that something of row `row` moved, if it is one of pasted
    /// files.
    fn clipboard_moved(&self, row: &str) {
        self.clipboard_note(row, |_, arrival| arrival.progressed = Instant::now());
    }

    /// Notes how row `row` ended, if it is one of pasted files: where its
    /// item is, if it arrived.
    fn clipboard_ended(&self, row: &str, path: Option<PathBuf>) {
        self.clipboard_note(row, |index, arrival| {
            arrival.ended.insert(index, path);
            arrival.progressed = Instant::now();
        });
    }

    /// Notes that this device checks a file of row `row`, if it is one of
    /// pasted files, until the returned guard is dropped.
    fn clipboard_checking<'a>(&'a self, row: &str) -> Checking<'a> {
        let key = self
            .clipboard_rows
            .lock()
            .unwrap()
            .get(row)
            .map(|(key, _)| key.clone());
        let receipt = key.and_then(|key| {
            let clipboards = self.clipboards.lock().unwrap();
            let receipt = clipboards.get(&key)?;
            receipt.arrival.send_modify(|arrival| {
                arrival.checking += 1;
                arrival.progressed = Instant::now();
            });
            Some((key, receipt.number))
        });
        Checking {
            files: self,
            receipt,
        }
    }

    fn clipboard_note(&self, row: &str, note: impl FnOnce(u32, &mut Arrival)) {
        let Some((key, index)) = self.clipboard_rows.lock().unwrap().get(row).cloned() else {
            return;
        };
        if let Some(receipt) = self.clipboards.lock().unwrap().get(&key) {
            receipt.arrival.send_modify(|arrival| note(index, arrival));
        }
    }

    /// A file of item `index` of clipboard offer `key`, which has `files`
    /// files, was listed.
    fn clipboard_listed(&self, key: &(DeviceId, u64), index: u32, files: u32) {
        let mut clipboards = self.clipboards.lock().unwrap();
        let Some(receipt) = clipboards.get_mut(key) else {
            return;
        };
        receipt.tallies.entry(index).or_insert_with(|| ItemTally {
            files,
            ..ItemTally::default()
        });
        receipt.arrival.send_modify(|arrival| {
            arrival.listed.insert(index);
            arrival.started = true;
            arrival.progressed = Instant::now();
        });
    }

    /// A file of the folder of row `row` ended, `arrived` or not: if it is
    /// one of pasted files, its item ends once every file of it did,
    /// those declined before they were listed too.
    fn clipboard_child_ended(&self, row: &str, arrived: bool) {
        let Some((key, index)) = self.clipboard_rows.lock().unwrap().get(row).cloned() else {
            return;
        };
        let path = self.row_path(row);
        let mut clipboards = self.clipboards.lock().unwrap();
        let Some(receipt) = clipboards.get_mut(&key) else {
            return;
        };
        let Some(tally) = receipt.tallies.get_mut(&index) else {
            return;
        };
        tally.settled += 1;
        tally.arrived |= arrived;
        if tally.ended_unlisted() {
            let path = path.filter(|_| tally.arrived);
            receipt.arrival.send_modify(|arrival| {
                arrival.ended.entry(index).or_insert(path);
            });
        }
    }

    /// A file of a clipboard offer pasted here was declined as `reason`
    /// before it was listed, or after: the item it belongs to, or the
    /// whole batch, counts it as ended, as its row may never do.
    fn clipboard_declined(&self, from: &DeviceId, offer: &FileOffer, reason: FileDecline) {
        let Some(b) = offer.batch.as_ref().filter(|b| b.clipboard_offer_id != 0) else {
            return;
        };
        let path = self.row_path(&b.item_id);
        let mut clipboards = self.clipboards.lock().unwrap();
        let Some(receipt) = clipboards.get_mut(&(from.clone(), b.clipboard_offer_id)) else {
            return;
        };
        let ended = match reason {
            // Tried again a moment later.
            FileDecline::Busy => None,
            // Only this file: its item ends with the rest of it.
            FileDecline::InvalidPath => {
                let tally = receipt
                    .tallies
                    .entry(b.item_index)
                    .or_insert_with(|| ItemTally {
                        files: b.item_files.max(1),
                        ..ItemTally::default()
                    });
                tally.unlisted += 1;
                tally
                    .ended_unlisted()
                    .then(|| path.filter(|_| tally.arrived))
            }
            _ => None,
        };
        receipt.arrival.send_modify(|arrival| {
            arrival.started = true;
            arrival.progressed = Instant::now();
            arrival.refused |= ends_batch(reason);
            if let Some(path) = ended {
                arrival.ended.entry(b.item_index).or_insert(path);
            }
        });
    }

    /// Where row `id` went, once known.
    fn row_path(&self, id: &str) -> Option<PathBuf> {
        self.transfers
            .borrow()
            .iter()
            .find(|t| t.id == id)
            .and_then(|t| t.path.clone())
    }

    /// Whether any of rows `rows`, sent, has not reached the receiver yet:
    /// it, or a file of it if it is a folder, waits its turn behind other
    /// files sent there, or is hashed. None once all of them ended.
    pub(crate) fn waiting(&self, rows: &[String]) -> Option<bool> {
        let held = |state: &TransferState| {
            matches!(state, TransferState::Queued | TransferState::Preparing)
        };
        let children = self.children.lock().unwrap();
        let list = self.transfers.borrow();
        let mut going = list
            .iter()
            .filter(|t| rows.contains(&t.id) && !t.state.finished())
            .peekable();
        going.peek()?;
        Some(going.any(|t| {
            held(&t.state)
                || children.folders.get(&t.id).is_some_and(|folder| {
                    folder
                        .going
                        .iter()
                        .filter_map(|id| children.files.get(id))
                        .any(|child| held(&child.status.state))
                })
        }))
    }

    /// Items sent and not finished, which every message to the apps lists.
    pub(crate) fn sending_items(&self) -> usize {
        self.transfers
            .borrow()
            .iter()
            .filter(|t| t.direction == Direction::Send && !t.state.finished())
            .count()
    }

    /// The batch `offer` belongs to, deciding about it the first time one
    /// of its files is offered: its limits, and where it goes. Declined
    /// with why otherwise.
    async fn incoming_batch(
        &self,
        from: &DeviceId,
        offer: &FileOffer,
    ) -> Result<Arc<IncomingBatch>, FileDecline> {
        let id = offer
            .batch
            .as_ref()
            .map_or(offer.transfer_id.as_str(), |b| b.batch_id.as_str());
        // Turned off with that device, its files are taken as by a device
        // that does not receive files, also those of a batch under way.
        if self
            .directory
            .peer(from.as_str())
            .is_some_and(|peer| peer.files_off)
        {
            debug!(device = %from, batch = id, "declining files from a device files are turned off with");
            return Err(FileDecline::Unavailable);
        }
        let key = (from.clone(), id.to_owned());
        let _deciding = self.deciding.lock().await;
        if let Some(batch) = self.incoming_batches.lock().unwrap().get(&key).cloned() {
            *batch.last.lock().unwrap() = Instant::now();
            return match *batch.decision.borrow() {
                Some(Err(reason)) => Err(reason),
                _ => Ok(batch.clone()),
            };
        }
        if let Some(b) = &offer.batch
            && (b.items as usize > MAX_ITEMS || b.files as usize > MAX_FILES || b.bytes > MAX_BYTES)
        {
            info!(device = %from, batch = id, files = b.files, bytes = b.bytes, "declining a batch over the limits");
            return Err(FileDecline::TooLarge);
        }
        let drag = offer
            .batch
            .as_ref()
            .map(|b| b.drag_id.as_str())
            .filter(|d| !d.is_empty());
        let clipboard = offer
            .batch
            .as_ref()
            .map(|b| b.clipboard_offer_id)
            .filter(|c| *c != 0);
        let settings = self.settings();
        let (folder, decision) = match (drag, clipboard) {
            (Some(_), Some(_)) => {
                info!(device = %from, batch = id, "declining a batch for both a drag and a paste");
                return Err(FileDecline::Rejected);
            }
            // Pasted here: into the folder made for the offer, without
            // asking, as the user just pasted them.
            (None, Some(offer_id)) => {
                let folder = self
                    .clipboards
                    .lock()
                    .unwrap()
                    .get(&(from.clone(), offer_id))
                    .map(|r| r.folder.clone());
                let Some(folder) = folder else {
                    info!(device = %from, batch = id, "declining files of a clipboard offer not pasted here");
                    return Err(FileDecline::Rejected);
                };
                // Checked as it was pasted, as the Windows app chose it;
                // checked again all the same, as it may have changed.
                if let Err(e) = self.check_destination(&folder).await {
                    warn!(error = %e, "the folder pasted files come into cannot take them");
                    return Err(FileDecline::Unavailable);
                }
                (folder, Some(Ok(())))
            }
            (None, None) => {
                let folder = settings.folder.ok_or(FileDecline::Unavailable)?;
                (folder, (!settings.ask).then_some(Ok(())))
            }
            (Some(drag), None) => {
                let accepted = self
                    .drags
                    .lock()
                    .unwrap()
                    .remove(&(from.clone(), drag.to_owned()))
                    .filter(|(_, at)| at.elapsed() < DRAG_WAIT);
                let Some((folder, _)) = accepted else {
                    info!(device = %from, drag, "declining files for a drag not dropped here");
                    return Err(FileDecline::Rejected);
                };
                // Dropped there a moment ago; checked again all the same.
                if let Err(e) = self.check_destination(&folder).await {
                    warn!(error = %e, "the folder a drag was dropped in cannot take it");
                    return Err(FileDecline::Unavailable);
                }
                (folder, Some(Ok(())))
            }
        };
        let batch = Arc::new(IncomingBatch {
            folder,
            decision: watch::Sender::new(decision),
            asked: AtomicBool::new(false),
            folders: Mutex::new(HashMap::new()),
            cancelled: Mutex::new(HashSet::new()),
            offered: Mutex::new(HashSet::new()),
            last: Mutex::new(Instant::now()),
        });
        let mut batches = self.incoming_batches.lock().unwrap();
        if batches.len() >= MAX_BATCHES {
            let oldest = batches
                .iter()
                .min_by_key(|(_, b)| *b.last.lock().unwrap())
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                batches.remove(&oldest);
            }
        }
        batches.insert(key, batch.clone());
        Ok(batch)
    }

    /// Lists a file offered by `from` in batch `batch`, as a row of its
    /// own or as a file of its folder's row. Returns whether it was listed
    /// already, as when it resumes; `None` if its folder ended already.
    fn list_incoming(
        &self,
        from: &DeviceId,
        offer: &FileOffer,
        place: &Destination,
    ) -> Option<bool> {
        let now = SystemTime::now();
        let info = offer.batch.as_ref().map(|b| BatchInfo {
            id: b.batch_id.clone(),
            index: b.item_index,
            items: b.items,
            files: b.files,
            bytes: b.bytes,
            drag: (!b.drag_id.is_empty()).then(|| b.drag_id.clone()),
            clipboard: (b.clipboard_offer_id != 0).then_some(b.clipboard_offer_id),
        });
        let row = |id: &str, name: &str, size: u64| TransferStatus {
            id: id.to_owned(),
            direction: Direction::Receive,
            device: from.to_string(),
            name: name.to_owned(),
            size,
            done: 0,
            bytes_per_second: 0,
            state: TransferState::WaitingForAnswer,
            missing_bytes: 0,
            path: None,
            started: now,
            finished: None,
            batch: info.clone(),
            folder: None,
        };
        let id = &offer.transfer_id;
        if let Some(info) = &info
            && let Some(offer_id) = info.clipboard
        {
            let row = if place.in_folder { &place.item } else { id };
            let key = (from.clone(), offer_id);
            self.clipboard_rows
                .lock()
                .unwrap()
                .insert(row.clone(), (key.clone(), info.index));
            let files = offer.batch.as_ref().map_or(1, |b| b.item_files.max(1));
            self.clipboard_listed(&key, info.index, files);
        }
        if !place.in_folder {
            let resumed = self.transfers.borrow().iter().any(|t| t.id == *id);
            if resumed {
                self.update(id, |t| t.state = TransferState::WaitingForAnswer);
            } else {
                self.add(row(id, &place.name, offer.size));
            }
            return Some(resumed);
        }
        let mut children = self.children.lock().unwrap();
        if let Some(child) = children.files.get_mut(id) {
            child.status.state = TransferState::WaitingForAnswer;
            let row = child.row.clone();
            self.refresh(&children, &row);
            return Some(true);
        }
        let (item_files, item_bytes) = offer
            .batch
            .as_ref()
            .map_or((1, offer.size), |b| (b.item_files, b.item_bytes));
        let existing = self
            .transfers
            .borrow()
            .iter()
            .find(|t| t.id == place.item)
            .map(|t| t.state.finished());
        match existing {
            Some(true) => return None,
            Some(false) => {}
            None => {
                children
                    .folders
                    .insert(place.item.clone(), Tally::default());
                self.add(TransferStatus {
                    folder: Some(FolderProgress {
                        entries: item_files.max(1),
                        ..FolderProgress::default()
                    }),
                    ..row(&place.item, &place.top, item_bytes)
                });
            }
        }
        let name = if place.folder {
            place.sub.last().unwrap_or(&place.top).clone()
        } else {
            place.name.clone()
        };
        children
            .folders
            .entry(place.item.clone())
            .or_default()
            .going
            .insert(id.clone());
        children.files.insert(
            id.clone(),
            Child {
                row: place.item.clone(),
                relative: offer.relative_path.clone(),
                status: row(id, &name, offer.size),
            },
        );
        self.refresh(&children, &place.item);
        Some(false)
    }
}

/// Throttled progress with a smoothed rate.
struct Progress {
    last_at: Instant,
    last_done: u64,
    rate: f64,
}

impl Progress {
    fn new(done: u64) -> Self {
        Self {
            last_at: Instant::now(),
            last_done: done,
            rate: 0.0,
        }
    }

    /// Returns the rate to publish, at most every PROGRESS_EVERY.
    fn tick(&mut self, done: u64) -> Option<u64> {
        let elapsed = self.last_at.elapsed();
        if elapsed < PROGRESS_EVERY {
            return None;
        }
        let rate = (done - self.last_done) as f64 / elapsed.as_secs_f64();
        self.rate = if self.rate == 0.0 {
            rate
        } else {
            0.7 * self.rate + 0.3 * rate
        };
        self.last_at = Instant::now();
        self.last_done = done;
        Some(self.rate as u64)
    }
}

/// Why a folder none of whose files arrived failed: its first failure's.
fn failure_of(progress: &FolderProgress) -> &'static str {
    failure(
        progress
            .failures
            .first()
            .map_or("unknown", |f| f.reason.as_str()),
    )
}

async fn cancelled(cancel: &mut watch::Receiver<bool>) {
    let _ = cancel.wait_for(|c| *c).await;
}

/// A file's place in its device's queue, which it gives up when its
/// transfer ends, however it ends, letting the next one go.
struct Place {
    files: Arc<Files>,
    device: DeviceId,
    id: String,
}

impl Drop for Place {
    fn drop(&mut self) {
        self.files.leave_queue(&self.device, &self.id);
    }
}

async fn send_file(
    files: Arc<Files>,
    job: Job,
    device: DeviceId,
    // Dropped last, once the transfer's end is listed.
    _place: Place,
) {
    let Job {
        id,
        mut source,
        batch,
        relative,
        folder,
        mut cancel,
        turn,
    } = job;
    tokio::select! {
        given = turn => if given.is_err() {
            // Nothing is left to give it its turn.
            return files.finish(&id, TransferState::Cancelled);
        },
        _ = cancelled(&mut cancel) => return files.finish(&id, TransferState::Cancelled),
    }
    let pacer = files.pacer(&device);
    // A file of a folder that cannot be read is still offered, as failed,
    // so the receiver knows the folder is complete without it.
    let in_folder = relative.contains('/');
    let mut failed = None;
    let (size, hash) = loop {
        let hashed = tokio::select! {
            hashed = source.hash() => hashed,
            _ = cancelled(&mut cancel) => return files.finish(&id, TransferState::Cancelled),
        };
        match hashed {
            Ok(hashed) => break hashed,
            // The broker went away, as when it restarts; hash once it is
            // back.
            Err(e) if e.kind() == std::io::ErrorKind::NotConnected => {
                debug!(id, error = %e, "cannot hash a file to send yet");
                tokio::select! {
                    _ = tokio::time::sleep(RETRY) => {}
                    _ = cancelled(&mut cancel) => return files.finish(&id, TransferState::Cancelled),
                }
            }
            Err(e) => {
                warn!(id, error = %e, "cannot read a file to send");
                if !in_folder {
                    return files.finish(&id, TransferState::Failed("unreadable"));
                }
                failed = Some("unreadable");
                source = Source::Folder;
                break (0, blake3::hash(&[]));
            }
        }
    };
    files.update(&id, |t| {
        t.size = size;
        t.state = TransferState::WaitingForConnection;
    });
    let name = relative.rsplit('/').next().unwrap_or_default().to_owned();
    let offer = FileOffer {
        transfer_id: id.clone(),
        name,
        size,
        blake3: hash.as_bytes().to_vec(),
        batch: Some(batch),
        relative_path: relative,
        folder,
        failed: failed.unwrap_or_default().to_owned(),
    };
    loop {
        let info = tokio::select! {
            info = files.session(&device) => info,
            _ = cancelled(&mut cancel) => return files.finish(&id, TransferState::Cancelled),
        };
        match offer_file(&files, &info, &pacer, &mut source, &offer, &mut cancel).await {
            Ok(end) => {
                // The receiver heard of the failure, unless the batch was
                // turned away.
                let end = match (failed, end) {
                    (Some(reason), TransferState::Done | TransferState::Failed(_)) => {
                        TransferState::Failed(reason)
                    }
                    (_, end) => end,
                };
                return files.finish(&id, end);
            }
            Err(e) => {
                debug!(id, error = %e, "file transfer interrupted; resuming when connected");
                files.update(&id, |t| {
                    t.state = TransferState::WaitingForConnection;
                    t.bytes_per_second = 0;
                });
                tokio::select! {
                    _ = tokio::time::sleep(RETRY) => {}
                    _ = cancelled(&mut cancel) => return files.finish(&id, TransferState::Cancelled),
                }
            }
        }
    }
}

/// One attempt at sending a file as `offer` says. Returns how the transfer
/// ended, or an error after which it is worth trying again.
async fn offer_file(
    files: &Files,
    info: &SessionInfo,
    pacer: &Pacer,
    source: &mut Source,
    offer: &FileOffer,
    cancel: &mut watch::Receiver<bool>,
) -> Result<TransferState, NetError> {
    let (id, size) = (offer.transfer_id.as_str(), offer.size);
    source.ready().await?;
    let (mut tx, mut rx) = info.open_transfer(FRAME_LIMIT).await?;
    tx.set_priority(priority::FILE);
    tx.send(&Transfer {
        kind: Some(transfer::Kind::File(offer.clone())),
    })
    .await?;
    files.update(id, |t| t.state = TransferState::WaitingForAnswer);
    let answer = tokio::select! {
        answer = tokio::time::timeout(ANSWER_TIMEOUT, rx.recv::<FileAnswer>()) => match answer {
            Err(_) => return Err(NetError::Protocol("no answer to a file offer")),
            // The receiver cancelled what this file belongs to.
            Ok(Err(e)) if stream_code(&e) == Some(CANCELLED) => return Ok(TransferState::Cancelled),
            Ok(answer) => answer?.ok_or(NetError::Protocol("the receiver closed the transfer"))?,
        },
        _ = cancelled(cancel) => {
            tx.reset(CANCELLED);
            return Ok(TransferState::Cancelled);
        }
    };
    if !answer.accept {
        if answer.decline() == FileDecline::Busy {
            return Err(NetError::Protocol("the receiver is busy"));
        }
        let missing = answer.missing_bytes;
        files.update(id, |t| t.missing_bytes = missing);
        return Ok(TransferState::Declined(answer.decline()));
    }
    if answer.offset > size {
        return Err(NetError::Protocol("resume offset past the end"));
    }
    source.seek(answer.offset).await?;
    files.update(id, |t| {
        t.done = answer.offset;
        t.state = TransferState::Transferring;
    });
    let mut progress = Progress::new(answer.offset);
    let mut buf = vec![0u8; MAX_CHUNK];
    let mut done = answer.offset;
    // Round trips sampled while data goes tell the pacer how files affect
    // the connection.
    let sending = pacer.sending();
    while done < size {
        if *cancel.borrow() {
            tx.reset(CANCELLED);
            return Ok(TransferState::Cancelled);
        }
        let want = usize::try_from(size - done)
            .unwrap_or(usize::MAX)
            .min(pacer.chunk());
        let n = source.read(done, &mut buf[..want]).await?;
        if n == 0 {
            return Ok(TransferState::Failed("file_changed"));
        }
        tokio::select! {
            _ = pacer.take(n) => {}
            _ = cancelled(cancel) => {
                tx.reset(CANCELLED);
                return Ok(TransferState::Cancelled);
            }
        }
        if let Err(e) = tx
            .send(&Chunk {
                data: buf[..n].to_vec(),
            })
            .await
        {
            if stream_code(&e) == Some(CANCELLED) {
                return Ok(TransferState::Cancelled);
            }
            return Err(e.into());
        }
        done += n as u64;
        if let Some(rate) = progress.tick(done) {
            files.update(id, |t| {
                t.done = done;
                t.bytes_per_second = rate;
            });
        }
    }
    drop(sending);
    tx.finish();
    files.update(id, |t| {
        t.done = size;
        t.state = TransferState::Verifying;
    });
    let verdict = tokio::time::timeout(DONE_TIMEOUT, rx.recv::<FileDone>())
        .await
        .map_err(|_| NetError::Protocol("the receiver never confirmed the file"))??
        .ok_or(NetError::Protocol("the receiver closed the transfer"))?;
    Ok(if verdict.verified {
        TransferState::Done
    } else {
        TransferState::Failed("corrupt")
    })
}

/// A received file this device checks, which pasted files it belongs to
/// wait on: see [`Arrival::checking`].
struct Checking<'a> {
    files: &'a Files,
    /// The receipt counted on, by its key and number; none if the file is
    /// none of pasted files.
    receipt: Option<((DeviceId, u64), u64)>,
}

impl Drop for Checking<'_> {
    fn drop(&mut self) {
        let Some((key, number)) = &self.receipt else {
            return;
        };
        let clipboards = self.files.clipboards.lock().unwrap();
        if let Some(receipt) = clipboards.get(key).filter(|r| r.number == *number) {
            receipt.arrival.send_modify(|arrival| {
                arrival.checking = arrival.checking.saturating_sub(1);
                arrival.progressed = Instant::now();
            });
        }
    }
}

/// Counts an incoming transfer for as long as it lives.
struct IncomingSlot<'a>(&'a AtomicUsize);

impl Drop for IncomingSlot<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Where an offered file goes in its batch, its names made ones this
/// device writes.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Destination {
    /// Its item: a lone file's transfer id, or the folder's id.
    item: String,
    /// The item's name.
    top: String,
    /// In a folder: the folders inside it down to the file, or for an
    /// empty folder down to and including it.
    sub: Vec<String>,
    /// A file's name.
    name: String,
    /// It belongs to a folder item.
    in_folder: bool,
    /// An empty folder.
    folder: bool,
}

impl Destination {
    /// Where `offer` goes; none for a path no receiver writes.
    fn of(offer: &FileOffer, windows: bool) -> Option<Self> {
        let item = offer
            .batch
            .as_ref()
            .map_or(offer.transfer_id.clone(), |b| b.item_id.clone());
        let lone = |name: String| Self {
            item: item.clone(),
            top: name.clone(),
            sub: Vec::new(),
            name,
            in_folder: false,
            folder: false,
        };
        if offer.relative_path.is_empty() {
            if offer.folder {
                return None;
            }
            return Some(lone(part_name(&sanitize_name(&offer.name), windows)));
        }
        let parts = listing::split_relative(&offer.relative_path)?;
        let mut names: Vec<String> = parts.iter().map(|p| part_name(p, windows)).collect();
        if names.len() == 1 && !offer.folder {
            return names.pop().map(lone);
        }
        let top = names.remove(0);
        let name = if offer.folder {
            String::new()
        } else {
            names.pop().unwrap_or_default()
        };
        Some(Self {
            item,
            top,
            sub: names,
            name,
            in_folder: true,
            folder: offer.folder,
        })
    }
}

/// A part of a relative path made a name this device writes: as
/// [`sanitize_name`] makes it, with a backslash kept as `_` rather than
/// read as a separator, and one Windows allows where Windows names are
/// written.
fn part_name(part: &str, windows: bool) -> String {
    let name = sanitize_name(&part.replace('\\', "_"));
    if windows { windows_name(&name) } else { name }
}

async fn receive_file(
    files: Arc<Files>,
    from: DeviceId,
    offer: FileOffer,
    mut tx: FramedSend,
    mut rx: FramedRecv,
) {
    tx.set_priority(priority::FILE);
    let Ok(hash) = <[u8; 32]>::try_from(offer.blake3.as_slice()) else {
        rx.stop(CANCELLED);
        return;
    };
    let ids_valid = offer.batch.as_ref().is_none_or(|b| {
        valid_transfer_id(&b.batch_id)
            && valid_transfer_id(&b.item_id)
            && (b.drag_id.is_empty() || valid_transfer_id(&b.drag_id))
    });
    if !valid_transfer_id(&offer.transfer_id) || !ids_valid {
        rx.stop(CANCELLED);
        return;
    }
    let id = offer.transfer_id.clone();
    let size = offer.size;
    let decline = async |tx: &mut FramedSend, reason: FileDecline, missing: u64| {
        let answer = FileAnswer {
            accept: false,
            offset: 0,
            decline: reason.into(),
            missing_bytes: missing,
        };
        let _ = tx.send(&answer).await;
        tx.finish();
    };
    // A failed file of a folder: its folder learns it will not come.
    let failed = (!offer.failed.is_empty()).then(|| failure(&offer.failed));
    let place = Destination::of(&offer, files.store.windows_names()).filter(|place| {
        let empty = place.folder || failed.is_some();
        (!empty || size == 0) && (failed.is_none() || (place.in_folder && !place.folder))
    });
    let Some(place) = place else {
        warn!(device = %from, id, "declining a file whose path is not one to write");
        files.clipboard_declined(&from, &offer, FileDecline::InvalidPath);
        return decline(&mut tx, FileDecline::InvalidPath, 0).await;
    };
    let batch = match files.incoming_batch(&from, &offer).await {
        Ok(batch) => batch,
        Err(reason) => {
            files.clipboard_declined(&from, &offer, reason);
            return decline(&mut tx, reason, 0).await;
        }
    };
    let refuse = |tx: &mut FramedSend, rx: &mut FramedRecv| {
        tx.reset(CANCELLED);
        rx.stop(CANCELLED);
    };
    if batch.cancelled.lock().unwrap().contains(&place.item) {
        return refuse(&mut tx, &mut rx);
    }
    if !batch.count(&id) {
        files.clipboard_declined(&from, &offer, FileDecline::TooLarge);
        return decline(&mut tx, FileDecline::TooLarge, 0).await;
    }
    if files.incoming.fetch_add(1, Ordering::SeqCst) >= MAX_INCOMING {
        files.incoming.fetch_sub(1, Ordering::SeqCst);
        files.clipboard_declined(&from, &offer, FileDecline::Busy);
        return decline(&mut tx, FileDecline::Busy, 0).await;
    }
    let _slot = IncomingSlot(&files.incoming);

    let mut cancel = files.control(&id);
    if files.list_incoming(&from, &offer, &place).is_none() {
        files.controls.lock().unwrap().remove(&id);
        return refuse(&mut tx, &mut rx);
    }

    // The batch's answer: given already, or asked of the user once for
    // all its files.
    let mut decision = batch.decision.subscribe();
    let decided = *decision.borrow_and_update();
    let decided = match decided {
        Some(decided) => decided,
        None => {
            if !batch.asked.swap(true, Ordering::SeqCst) {
                files.app.needed(Need::FileOffer);
            }
            tokio::select! {
                answer = tokio::time::timeout(ASK_TIMEOUT, decision.wait_for(Option::is_some)) => {
                    if !matches!(answer, Ok(Ok(_))) {
                        batch.decide(false);
                    }
                }
                _ = cancelled(&mut cancel) => {
                    batch.decide(false);
                }
                // Nothing is sent before the answer, so anything read here
                // means the sender gave up.
                _ = rx.recv::<Chunk>() => {
                    return files.finish(&id, TransferState::Cancelled);
                }
            }
            let decided = *batch.decision.borrow();
            decided.unwrap_or(Err(FileDecline::Rejected))
        }
    };
    if let Err(reason) = decided {
        files.clipboard_declined(&from, &offer, reason);
        decline(&mut tx, reason, 0).await;
        return files.finish(&id, TransferState::Declined(reason));
    }

    if place.folder || failed.is_some() {
        return receive_empty(&files, &batch, &place, &id, failed, tx, rx).await;
    }
    // Accepted, by the user or by the settings: now the folder may be
    // listed.
    files.tidy(&batch.folder).await;
    let (mut target, have, free) = match Target::open(&files.store, &batch.folder, &id, size).await
    {
        Ok(opened) => opened,
        Err(e) => {
            warn!(folder = %batch.folder.display(), error = %e, "cannot prepare the folder for received files");
            files.clipboard_declined(&from, &offer, FileDecline::Unavailable);
            decline(&mut tx, FileDecline::Unavailable, 0).await;
            return files.finish(&id, TransferState::Declined(FileDecline::Unavailable));
        }
    };
    let needed = size - have;
    if let Some(free) = free
        && free < needed
    {
        let missing = needed - free;
        files.update(&id, |t| t.missing_bytes = missing);
        files.clipboard_declined(&from, &offer, FileDecline::NoSpace);
        decline(&mut tx, FileDecline::NoSpace, missing).await;
        if have == 0 {
            target.discard().await;
        } else {
            target.close().await;
        }
        return files.finish(&id, TransferState::Declined(FileDecline::NoSpace));
    }

    let answer = FileAnswer {
        accept: true,
        offset: have,
        decline: FileDecline::Unspecified.into(),
        missing_bytes: 0,
    };
    if tx.send(&answer).await.is_err() {
        target.close().await;
        return files.update(&id, |t| t.state = TransferState::WaitingForConnection);
    }
    if let Err(e) = target.start_writing().await {
        warn!(error = %e, "cannot write a received file");
        rx.stop(CANCELLED);
        target.close().await;
        return files.finish(&id, TransferState::Failed("write_failed"));
    }
    files.update(&id, |t| {
        t.done = have;
        t.state = TransferState::Transferring;
    });
    let mut done = have;
    let mut progress = Progress::new(have);
    // Cut off: keep what arrived for the sender to resume.
    let interrupted = |done: u64| {
        files.controls.lock().unwrap().remove(&id);
        files.update(&id, |t| {
            t.done = done;
            t.state = TransferState::WaitingForConnection;
            t.bytes_per_second = 0;
        });
    };
    loop {
        let chunk = tokio::select! {
            chunk = rx.recv::<Chunk>() => chunk,
            _ = cancelled(&mut cancel) => {
                rx.stop(CANCELLED);
                target.discard().await;
                return files.finish(&id, TransferState::Cancelled);
            }
        };
        match chunk {
            Ok(Some(chunk)) => {
                if done + chunk.data.len() as u64 > size {
                    rx.stop(CANCELLED);
                    target.discard().await;
                    return files.finish(&id, TransferState::Failed("corrupt"));
                }
                if let Err(e) = target.write(&chunk.data).await {
                    if e.kind() == std::io::ErrorKind::NotConnected {
                        // The broker went away; the sender tries again.
                        debug!(id, error = %e, "file transfer interrupted");
                        rx.stop(INTERRUPTED);
                        return interrupted(done);
                    }
                    warn!(error = %e, "cannot write a received file");
                    rx.stop(CANCELLED);
                    target.close().await;
                    return files.finish(&id, TransferState::Failed("write_failed"));
                }
                done += chunk.data.len() as u64;
                if let Some(rate) = progress.tick(done) {
                    files.update(&id, |t| {
                        t.done = done;
                        t.bytes_per_second = rate;
                    });
                }
            }
            Ok(None) => break,
            Err(e) => {
                if stream_code(&e) == Some(CANCELLED) {
                    target.discard().await;
                    return files.finish(&id, TransferState::Cancelled);
                }
                debug!(id, error = %e, "file transfer interrupted");
                target.close().await;
                return interrupted(done);
            }
        }
    }
    // Until it ends, the sender waits on this device.
    let _checking = files.clipboard_checking(if place.in_folder { &place.item } else { &id });
    files.update(&id, |t| {
        t.done = done;
        t.state = TransferState::Verifying;
    });
    let (written, hashed) = match target.finish().await {
        Ok(finished) => finished,
        Err(e) => {
            warn!(error = %e, "cannot write a received file");
            target.close().await;
            return files.finish(&id, TransferState::Failed("write_failed"));
        }
    };
    let verified = done == size && written == size && *hashed.as_bytes() == hash;
    let saved = if !verified {
        target.discard().await;
        None
    } else if place.in_folder {
        match batch.claim(&files.store, &place.item, &place.top).await {
            Ok(top) => {
                files.update(&place.item, |t| t.path = Some(batch.folder.join(&top)));
                let mut sub = vec![top];
                sub.extend(place.sub.iter().cloned());
                target
                    .keep(&sub, &place.name)
                    .await
                    .inspect_err(|e| warn!(error = %e, "cannot name a received file"))
                    .ok()
            }
            Err(e) => {
                warn!(error = %e, "cannot make the folder for received files");
                target.discard().await;
                None
            }
        }
    } else {
        target
            .keep(&[], &place.name)
            .await
            .inspect_err(|e| warn!(error = %e, "cannot name a received file"))
            .ok()
    };
    let _ = tx
        .send(&FileDone {
            verified: saved.is_some(),
        })
        .await;
    tx.finish();
    match saved {
        Some(path) => {
            files.update(&id, |t| t.path = Some(path));
            files.finish(&id, TransferState::Done);
        }
        None if verified => files.finish(&id, TransferState::Failed("write_failed")),
        None => files.finish(&id, TransferState::Failed("corrupt")),
    }
}

/// Receives what has no data: an empty folder, made once the sender is
/// done with it, or a file of a folder the sender could not read, `failed`
/// with its reason, which is only counted.
async fn receive_empty(
    files: &Files,
    batch: &IncomingBatch,
    place: &Destination,
    id: &str,
    failed: Option<&'static str>,
    mut tx: FramedSend,
    mut rx: FramedRecv,
) {
    let answer = FileAnswer {
        accept: true,
        offset: 0,
        decline: FileDecline::Unspecified.into(),
        missing_bytes: 0,
    };
    if tx.send(&answer).await.is_err() {
        files.controls.lock().unwrap().remove(id);
        return files.update(id, |t| t.state = TransferState::WaitingForConnection);
    }
    files.update(id, |t| t.state = TransferState::Transferring);
    match rx.recv::<Chunk>().await {
        Ok(None) => {}
        Ok(Some(_)) => {
            rx.stop(CANCELLED);
            return files.finish(id, TransferState::Failed("corrupt"));
        }
        Err(e) if stream_code(&e) == Some(CANCELLED) => {
            return files.finish(id, TransferState::Cancelled);
        }
        Err(_) => {
            files.controls.lock().unwrap().remove(id);
            return files.update(id, |t| t.state = TransferState::WaitingForConnection);
        }
    }
    if let Some(reason) = failed {
        let _ = tx.send(&FileDone { verified: false }).await;
        tx.finish();
        return files.finish(id, TransferState::Failed(reason));
    }
    let made = match batch.claim(&files.store, &place.item, &place.top).await {
        Ok(top) => {
            files.update(&place.item, |t| t.path = Some(batch.folder.join(&top)));
            let mut sub = vec![top];
            sub.extend(place.sub.iter().cloned());
            make_folder(&files.store, &batch.folder, &sub).await
        }
        Err(e) => Err(e),
    };
    let made = made
        .inspect_err(|e| warn!(error = %e, "cannot make a received folder"))
        .ok();
    let _ = tx
        .send(&FileDone {
            verified: made.is_some(),
        })
        .await;
    tx.finish();
    match made {
        Some(path) => {
            files.update(id, |t| t.path = Some(path));
            files.finish(id, TransferState::Done);
        }
        None => files.finish(id, TransferState::Failed("write_failed")),
    }
}

/// Creates the folders `sub` in `folder`.
async fn make_folder(store: &Store, folder: &Path, sub: &[String]) -> std::io::Result<PathBuf> {
    match store {
        Store::Local => {
            let path = sub.iter().fold(folder.to_owned(), |p, s| p.join(s));
            tokio::fs::create_dir_all(&path).await?;
            Ok(path)
        }
        #[cfg(any(unix, windows))]
        Store::Broker(broker) => broker.make_folder(folder, &sub.join("/")).await,
    }
}

/// Where a file to send is read from.
enum Source {
    /// This process reads it; `file` is open while it is sent.
    Local {
        path: PathBuf,
        file: Option<tokio::fs::File>,
    },
    /// The Windows broker reads it, as the user. It is opened again when
    /// the broker that opened it went away.
    #[cfg(any(unix, windows))]
    Broker {
        broker: Arc<super::broker::Broker>,
        /// The transfer's id, for the broker's logs.
        id: String,
        path: PathBuf,
        file: Option<super::broker::BrokerSource>,
    },
    /// An empty folder: nothing to read.
    Folder,
}

impl Source {
    /// Where to read the file at `path` for transfer `id`, or `folder`, an
    /// empty folder.
    fn new(store: &Store, id: &str, path: PathBuf, folder: bool) -> Self {
        if folder {
            return Self::Folder;
        }
        match store {
            Store::Local => Self::Local { path, file: None },
            #[cfg(any(unix, windows))]
            Store::Broker(broker) => Self::Broker {
                broker: broker.clone(),
                id: id.to_owned(),
                path,
                file: None,
            },
        }
    }

    /// Gets ready to read: where the broker reads, has it open the file
    /// again if the broker that opened it went away. Fails with
    /// `NotConnected` while there is no broker.
    async fn ready(&mut self) -> std::io::Result<()> {
        match self {
            Self::Local { .. } | Self::Folder => Ok(()),
            #[cfg(any(unix, windows))]
            Self::Broker {
                broker,
                id,
                path,
                file,
            } => {
                if file.as_ref().is_some_and(|f| !f.is_lost()) {
                    return Ok(());
                }
                *file = None;
                use super::broker::OpenError;
                match broker.open_for_sending(id, path).await {
                    Ok((opened, _)) => {
                        *file = Some(opened);
                        Ok(())
                    }
                    Err(OpenError::NoBroker) => Err(std::io::Error::new(
                        std::io::ErrorKind::NotConnected,
                        OpenError::NoBroker,
                    )),
                    Err(e) => Err(std::io::Error::other(e)),
                }
            }
        }
    }

    /// Reads the whole file and returns its size and hash.
    async fn hash(&mut self) -> std::io::Result<(u64, blake3::Hash)> {
        match self {
            Self::Local { path, .. } => {
                let path = path.clone();
                tokio::task::spawn_blocking(move || hash_file(&path))
                    .await
                    .map_err(std::io::Error::other)?
            }
            #[cfg(any(unix, windows))]
            Self::Broker { .. } => {
                self.ready().await?;
                let Self::Broker {
                    file: Some(file), ..
                } = self
                else {
                    unreachable!("ready opens the file");
                };
                let (size, hash) = file.hash().await?;
                Ok((size, blake3::Hash::from_bytes(hash)))
            }
            Self::Folder => Ok((0, blake3::hash(&[]))),
        }
    }

    /// Starts reading the file at `offset`, for one attempt at sending it.
    async fn seek(&mut self, offset: u64) -> std::io::Result<()> {
        match self {
            Self::Local { path, file } => {
                let mut opened = tokio::fs::File::open(&path).await?;
                opened.seek(SeekFrom::Start(offset)).await?;
                *file = Some(opened);
                Ok(())
            }
            #[cfg(any(unix, windows))]
            Self::Broker { file, .. } => match file {
                Some(_) => Ok(()),
                None => Err(std::io::Error::other("the file is not open")),
            },
            Self::Folder => Ok(()),
        }
    }

    /// Reads into `buf` what follows `offset`, where the last read ended;
    /// returns how much, 0 at the end of the file.
    async fn read(&mut self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        let not_open = || std::io::Error::other("the file is not open");
        match self {
            Self::Local { file, .. } => file.as_mut().ok_or_else(not_open)?.read(buf).await,
            #[cfg(any(unix, windows))]
            Self::Broker { file, .. } => {
                let data = file
                    .as_mut()
                    .ok_or_else(not_open)?
                    .read(offset, buf.len())
                    .await?;
                buf[..data.len()].copy_from_slice(&data);
                Ok(data.len())
            }
            Self::Folder => Ok(0),
        }
    }
}

/// Where an incoming file is written while it arrives: a part file this
/// process writes, or one the Windows broker writes for it.
enum Target {
    Local {
        folder: PathBuf,
        part: PathBuf,
        /// Open once the transfer is accepted.
        file: Option<tokio::fs::File>,
    },
    #[cfg(any(unix, windows))]
    Broker(super::broker::BrokerFile),
}

impl Target {
    /// Gets the part file for transfer `id` ready in `folder`. Returns it
    /// with the bytes it already holds and the free space there, if known.
    async fn open(
        store: &Store,
        folder: &Path,
        id: &str,
        size: u64,
    ) -> std::io::Result<(Self, u64, Option<u64>)> {
        match store {
            Store::Local => {
                tokio::fs::create_dir_all(folder).await?;
                let part = part_path(folder, id);
                let mut have = tokio::fs::metadata(&part).await.map_or(0, |m| m.len());
                if have > size {
                    let _ = tokio::fs::remove_file(&part).await;
                    have = 0;
                }
                let free = iw_platform::system::free_space(folder);
                let target = Self::Local {
                    folder: folder.to_owned(),
                    part,
                    file: None,
                };
                Ok((target, have, free))
            }
            #[cfg(any(unix, windows))]
            Store::Broker(broker) => {
                let (file, have, free) = broker.open_file(id, size, folder).await?;
                Ok((Self::Broker(file), have, free))
            }
        }
    }

    /// Opens the part file to append to it.
    async fn start_writing(&mut self) -> std::io::Result<()> {
        match self {
            Self::Local { part, file, .. } => {
                let opened = tokio::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&part)
                    .await?;
                *file = Some(opened);
                Ok(())
            }
            #[cfg(any(unix, windows))]
            Self::Broker(_) => Ok(()),
        }
    }

    async fn write(&mut self, data: &[u8]) -> std::io::Result<()> {
        match self {
            Self::Local { file, .. } => match file {
                Some(file) => file.write_all(data).await,
                None => Err(std::io::Error::other("the part file is not open")),
            },
            #[cfg(any(unix, windows))]
            Self::Broker(file) => file.write(data).await,
        }
    }

    /// Writes the part file out and closes it; returns its size and hash.
    async fn finish(&mut self) -> std::io::Result<(u64, blake3::Hash)> {
        match self {
            Self::Local { part, file, .. } => {
                if let Some(file) = file.take() {
                    file.sync_all().await?;
                }
                let part = part.clone();
                tokio::task::spawn_blocking(move || hash_file(&part))
                    .await
                    .map_err(std::io::Error::other)?
            }
            #[cfg(any(unix, windows))]
            Self::Broker(file) => {
                let (size, hash) = file.finish().await?;
                Ok((size, blake3::Hash::from_bytes(hash)))
            }
        }
    }

    /// Gives the finished file `name` in the folders `sub` of its folder,
    /// numbered if taken; returns where it is.
    async fn keep(self, sub: &[String], name: &str) -> std::io::Result<PathBuf> {
        match self {
            Self::Local { folder, part, .. } => {
                let folder = sub.iter().fold(folder, |p, s| p.join(s));
                tokio::fs::create_dir_all(&folder).await?;
                let target = unique_path(&folder, name, |p| p.exists());
                tokio::fs::rename(&part, &target).await?;
                Ok(target)
            }
            #[cfg(any(unix, windows))]
            Self::Broker(file) => file.keep(&windows_name(name), &sub.join("/")).await,
        }
    }

    /// Deletes the part file.
    async fn discard(self) {
        match self {
            Self::Local { part, file, .. } => {
                drop(file);
                let _ = tokio::fs::remove_file(&part).await;
            }
            #[cfg(any(unix, windows))]
            Self::Broker(file) => file.discard().await,
        }
    }

    /// Closes the part file and keeps it, for the transfer to resume.
    async fn close(self) {
        match self {
            Self::Local { file, .. } => {
                if let Some(mut file) = file {
                    let _ = file.flush().await;
                }
            }
            #[cfg(any(unix, windows))]
            Self::Broker(file) => file.close().await,
        }
    }
}

fn hash_file(path: &Path) -> std::io::Result<(u64, blake3::Hash)> {
    let mut hasher = blake3::Hasher::new();
    let mut file = std::fs::File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("not a file"));
    }
    let size = std::io::copy(&mut file, &mut hasher)?;
    Ok((size, hasher.finalize()))
}

/// Creates the folder `name` in `folder`, or `name (1)`, `(2)`… when that
/// is taken: never one already there.
fn claim_local(folder: &Path, name: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(folder)?;
    for candidate in folder_candidates(folder, name) {
        match std::fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("every numbered name is taken"))
}

/// `folder/name`, then `folder/name (1)`, `(2)`…: a folder's name is
/// numbered at its end, extension or not.
pub(crate) fn folder_candidates<'a>(
    folder: &'a Path,
    name: &'a str,
) -> impl Iterator<Item = PathBuf> + 'a {
    std::iter::once(folder.join(name))
        .chain((1..=MAX_NUMBERED).map(move |n| folder.join(format!("{name} ({n})"))))
}

/// Whether `folder` is a folder files can be created in, by creating one
/// and deleting it again.
fn check_folder_local(folder: &Path) -> std::io::Result<PathBuf> {
    if !std::fs::metadata(folder)?.is_dir() {
        return Err(std::io::Error::other("not a folder"));
    }
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let probe = folder.join(format!(
        "{PART_PREFIX}check-{:x}-{nanos:x}{PART_SUFFIX}",
        std::process::id()
    ));
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)?;
    std::fs::remove_file(&probe)?;
    Ok(folder.to_owned())
}

/// Transfer ids name part files, so they are kept plain.
pub(crate) fn valid_transfer_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn part_path(folder: &Path, id: &str) -> PathBuf {
    folder.join(format!("{PART_PREFIX}{id}{PART_SUFFIX}"))
}

/// A name a received file can safely get: the last path component only,
/// without control characters, not empty, `.` or `..`, and at most 255
/// bytes.
pub(crate) fn sanitize_name(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let cleaned: String = last.chars().filter(|c| !c.is_control()).collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        return "file".to_owned();
    }
    let mut end = trimmed.len().min(255);
    while !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    trimmed[..end].to_owned()
}

/// Whether a file or folder named `name` is written under that very name
/// by any receiver, but for the rules of Windows a receiver there applies
/// ([`windows_name`]): one part of a path [`sanitize_name`] leaves as it is,
/// so without a backslash or control characters, not starting or ending
/// with a space, and at most 255 bytes.
pub(crate) fn kept_name(name: &str) -> bool {
    listing::valid_part(name) && sanitize_name(name) == name
}

/// Names Windows keeps for devices, with or without an extension.
const WINDOWS_RESERVED: [&str; 32] = [
    "CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$", "COM0", "COM1", "COM2", "COM3", "COM4",
    "COM5", "COM6", "COM7", "COM8", "COM9", "COM¹", "COM²", "COM³", "LPT0", "LPT1", "LPT2", "LPT3",
    "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9", "LPT¹", "LPT²", "LPT³",
];

/// A name Windows allows, for a name [`sanitize_name`] made: characters
/// Windows refuses become `_`, dots and spaces at the end go, and device
/// names such as `CON` get a `_` in front. At most 240 UTF-16 units, so a
/// number still fits when the name is taken. The broker refuses any name
/// Windows does not allow.
pub(crate) fn windows_name(name: &str) -> String {
    const REFUSED: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];
    let mut units = 0;
    let shortened: String = name
        .chars()
        .map(|c| {
            if c.is_control() || REFUSED.contains(&c) {
                '_'
            } else {
                c
            }
        })
        .take_while(|c| {
            units += c.len_utf16();
            units <= 240
        })
        .collect();
    let trimmed = shortened.trim_end_matches(['.', ' ']);
    let stem = trimmed.split('.').next().unwrap_or_default().trim_end();
    if trimmed.is_empty() {
        "file".to_owned()
    } else if WINDOWS_RESERVED
        .iter()
        .any(|r| r.eq_ignore_ascii_case(stem))
    {
        format!("_{trimmed}")
    } else {
        trimmed.to_owned()
    }
}

/// `folder/name`, or `folder/name (1).ext`, `(2)`… if taken.
pub(crate) fn unique_path(folder: &Path, name: &str, taken: impl Fn(&Path) -> bool) -> PathBuf {
    let first = folder.join(name);
    if !taken(&first) {
        return first;
    }
    let (stem, extension) = match name.rfind('.') {
        Some(dot) if dot > 0 => (&name[..dot], &name[dot..]),
        _ => (name, ""),
    };
    (1..)
        .map(|n| folder.join(format!("{stem} ({n}){extension}")))
        .find(|p| !taken(p))
        .expect("some number is free")
}

fn remove_stale_parts(folder: &Path) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with(PART_PREFIX) && name.ends_with(PART_SUFFIX)) {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|modified| modified.elapsed().is_ok_and(|age| age > STALE_PART));
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_lose_directories_and_control_characters() {
        assert_eq!(sanitize_name("報告.pdf"), "報告.pdf");
        assert_eq!(sanitize_name("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_name("C:\\\\Users\\\\a\\\\b.txt"), "b.txt");
        assert_eq!(sanitize_name("evil\n.txt"), "evil.txt");
        assert_eq!(sanitize_name(".."), "file");
        assert_eq!(sanitize_name("dir/"), "file");
        assert_eq!(sanitize_name("  "), "file");
        assert!(sanitize_name(&"長".repeat(200)).len() <= 255);
    }

    #[test]
    fn only_names_written_as_they_are_are_kept() {
        assert!(kept_name("報告.pdf"));
        assert!(kept_name(&format!("{}.txt", "長".repeat(83))));
        // 100 characters, but 304 bytes.
        assert!(!kept_name(&format!("{}.txt", "長".repeat(100))));
        assert!(!kept_name("a\\b.txt"));
        assert!(!kept_name("a/b.txt"));
        assert!(!kept_name(" padded.txt"));
        assert!(!kept_name("tab\t.txt"));
        assert!(!kept_name(".."));
        assert!(!kept_name(""));
    }

    #[test]
    fn taken_names_get_a_number() {
        let folder = Path::new("/d");
        let taken = |p: &Path| {
            [
                Path::new("/d/報告.pdf"),
                Path::new("/d/報告 (1).pdf"),
                Path::new("/d/notes"),
            ]
            .contains(&p)
        };
        assert_eq!(
            unique_path(folder, "new.txt", taken),
            Path::new("/d/new.txt")
        );
        assert_eq!(
            unique_path(folder, "報告.pdf", taken),
            Path::new("/d/報告 (2).pdf")
        );
        assert_eq!(
            unique_path(folder, "notes", taken),
            Path::new("/d/notes (1)")
        );
        let folders: Vec<PathBuf> = folder_candidates(folder, "v1.2").take(2).collect();
        assert_eq!(folders, [Path::new("/d/v1.2"), Path::new("/d/v1.2 (1)")]);
    }

    #[test]
    fn names_for_windows_avoid_what_it_refuses() {
        assert_eq!(windows_name("報告.pdf"), "報告.pdf");
        assert_eq!(windows_name("a:b*c?.txt"), "a_b_c_.txt");
        assert_eq!(windows_name("notes. . "), "notes");
        assert_eq!(windows_name("con.txt"), "_con.txt");
        assert_eq!(windows_name("LPT1"), "_LPT1");
        assert_eq!(windows_name("console.log"), "console.log");
        assert_eq!(windows_name("..."), "file");
        let long = windows_name(&"長".repeat(255));
        assert_eq!(long.encode_utf16().count(), 240);
    }

    #[test]
    fn transfer_ids_are_plain() {
        assert!(valid_transfer_id("18f0a-1c2-3"));
        assert!(!valid_transfer_id("../x"));
        assert!(!valid_transfer_id(""));
    }

    fn offer(relative: &str, folder: bool) -> FileOffer {
        FileOffer {
            transfer_id: "t-1".to_owned(),
            name: relative.rsplit('/').next().unwrap().to_owned(),
            size: 0,
            blake3: vec![0; 32],
            batch: Some(FileBatch {
                batch_id: "b-1".to_owned(),
                item_id: "i-1".to_owned(),
                ..FileBatch::default()
            }),
            relative_path: relative.to_owned(),
            folder,
            failed: String::new(),
        }
    }

    fn names(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|p| (*p).to_owned()).collect()
    }

    #[test]
    fn offered_paths_land_inside_the_folder_with_names_this_device_writes() {
        let lone = Destination::of(&offer("報告.pdf", false), false).unwrap();
        assert_eq!(
            (lone.top.as_str(), lone.name.as_str(), lone.in_folder),
            ("報告.pdf", "報告.pdf", false)
        );
        let deep = Destination::of(&offer("Photos/2024/a.jpg", false), false).unwrap();
        assert_eq!(deep.item, "i-1");
        assert_eq!(deep.top, "Photos");
        assert_eq!(deep.sub, names(&["2024"]));
        assert_eq!(deep.name, "a.jpg");
        assert!(deep.in_folder && !deep.folder);
        let empty = Destination::of(&offer("Photos/empty", true), false).unwrap();
        assert_eq!(empty.sub, names(&["empty"]));
        assert!(empty.folder);
        let top_empty = Destination::of(&offer("Empty", true), false).unwrap();
        assert!(top_empty.in_folder && top_empty.sub.is_empty());
        // Windows names, part by part; a backslash is no separator.
        let windows = Destination::of(&offer("CON/a:b/x\\y.txt", false), true).unwrap();
        assert_eq!(windows.top, "_CON");
        assert_eq!(windows.sub, names(&["a_b"]));
        assert_eq!(windows.name, "x_y.txt");
        for bad in ["/etc/passwd", "a/../../b", "a//b", ""] {
            let mut bad_offer = offer(bad, false);
            if bad.is_empty() {
                bad_offer.folder = true;
            }
            assert_eq!(Destination::of(&bad_offer, false), None, "{bad:?}");
        }
        // Older senders: the name alone.
        let mut old = offer("", false);
        old.batch = None;
        old.name = "../../etc/passwd".to_owned();
        let old = Destination::of(&old, false).unwrap();
        assert_eq!((old.item.as_str(), old.name.as_str()), ("t-1", "passwd"));
    }

    #[test]
    fn folders_are_claimed_under_names_not_taken() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("Photos")).unwrap();
        std::fs::write(dir.path().join("Photos").join("old.jpg"), "old").unwrap();
        let claimed = claim_local(dir.path(), "Photos").unwrap();
        assert_eq!(claimed, dir.path().join("Photos (1)"));
        assert_eq!(
            claim_local(dir.path(), "Photos").unwrap(),
            dir.path().join("Photos (2)")
        );
        assert!(std::fs::read_dir(&claimed).unwrap().next().is_none());
        assert!(check_folder_local(dir.path()).is_ok());
        assert!(check_folder_local(&dir.path().join("gone")).is_err());
        assert!(check_folder_local(&dir.path().join("Photos").join("old.jpg")).is_err());
    }

    fn status(id: &str, state: TransferState) -> TransferStatus {
        TransferStatus {
            id: id.to_owned(),
            direction: Direction::Send,
            device: "win".to_owned(),
            name: id.to_owned(),
            size: 10,
            done: 0,
            bytes_per_second: 0,
            state,
            missing_bytes: 0,
            path: None,
            started: SystemTime::UNIX_EPOCH,
            finished: None,
            batch: None,
            folder: None,
        }
    }

    #[test]
    fn a_folder_sums_up_its_files_and_ends_with_its_last_one() {
        let files = test_files();
        let row = "row";
        files.add(TransferStatus {
            size: 30,
            folder: Some(FolderProgress {
                entries: 4,
                failed: 1,
                failures: vec![FailedEntry {
                    path: "F/link".to_owned(),
                    reason: "symlink".to_owned(),
                }],
                ..FolderProgress::default()
            }),
            ..status(row, TransferState::Queued)
        });
        {
            let mut children = files.children.lock().unwrap();
            let mut tally = Tally::default();
            for (id, relative) in [("a", "F/a"), ("b", "F/b"), ("c", "F/sub/c")] {
                tally.going.insert(id.to_owned());
                children.files.insert(
                    id.to_owned(),
                    Child {
                        row: row.to_owned(),
                        relative: relative.to_owned(),
                        status: status(id, TransferState::Queued),
                    },
                );
            }
            children.folders.insert(row.to_owned(), tally);
        }
        let row_of = || files.transfers().into_iter().find(|t| t.id == row).unwrap();
        files.update("a", |t| {
            t.state = TransferState::Transferring;
            t.done = 4;
            t.bytes_per_second = 100;
        });
        files.update("b", |t| t.state = TransferState::WaitingForConnection);
        let going = row_of();
        assert_eq!(
            (going.state, going.done, going.bytes_per_second),
            (TransferState::Transferring, 4, 100)
        );
        files.update("a", |t| t.done = 10);
        files.finish("a", TransferState::Done);
        files.finish("c", TransferState::Failed("unreadable"));
        let one_left = row_of();
        assert_eq!(one_left.state, TransferState::WaitingForConnection);
        assert_eq!(one_left.done, 10);
        assert!(one_left.finished.is_none());
        files.finish("b", TransferState::Done);
        let ended = row_of();
        assert_eq!(ended.state, TransferState::Done);
        assert_eq!(ended.done, 20);
        let progress = ended.folder.unwrap();
        assert_eq!(
            (progress.entries, progress.done, progress.failed),
            (4, 2, 2)
        );
        assert_eq!(progress.failures[1].path, "F/sub/c");
        assert!(ended.finished.is_some());
        assert!(files.children.lock().unwrap().folders.is_empty());
    }

    #[test]
    fn sent_rows_wait_while_a_file_of_their_folder_does() {
        let files = test_files();
        files.add(status("lone", TransferState::Transferring));
        files.add(TransferStatus {
            folder: Some(FolderProgress {
                entries: 2,
                ..FolderProgress::default()
            }),
            ..status("folder", TransferState::Queued)
        });
        {
            let mut children = files.children.lock().unwrap();
            let mut tally = Tally::default();
            for id in ["a", "b"] {
                tally.going.insert(id.to_owned());
                children.files.insert(
                    id.to_owned(),
                    Child {
                        row: "folder".to_owned(),
                        relative: format!("F/{id}"),
                        status: status(id, TransferState::Queued),
                    },
                );
            }
            children.folders.insert("folder".to_owned(), tally);
        }
        let rows = ["lone".to_owned(), "folder".to_owned()];
        assert_eq!(files.waiting(&rows), Some(true));
        // One of the folder's files goes, which the folder's row then
        // says; the other still waits its turn.
        files.update("a", |t| t.state = TransferState::Transferring);
        assert_eq!(
            files
                .transfers()
                .iter()
                .find(|t| t.id == "folder")
                .unwrap()
                .state,
            TransferState::Transferring
        );
        assert_eq!(files.waiting(&rows), Some(true));
        // Its turn came once the first one ended: it is hashed, then goes.
        files.finish("a", TransferState::Done);
        files.update("b", |t| t.state = TransferState::Preparing);
        assert_eq!(files.waiting(&rows), Some(true));
        files.update("b", |t| t.state = TransferState::Transferring);
        assert_eq!(files.waiting(&rows), Some(false));
        files.finish("b", TransferState::Done);
        assert_eq!(files.waiting(&rows), Some(false));
        files.finish("lone", TransferState::Done);
        assert_eq!(files.waiting(&rows), None);
    }

    #[test]
    fn a_folder_none_of_whose_files_arrived_failed_and_one_cancelled_cancels_all() {
        let files = test_files();
        for (row, kids) in [("f1", ["a1", "b1"]), ("f2", ["a2", "b2"])] {
            files.add(TransferStatus {
                folder: Some(FolderProgress {
                    entries: 2,
                    ..FolderProgress::default()
                }),
                ..status(row, TransferState::Queued)
            });
            let mut children = files.children.lock().unwrap();
            let mut tally = Tally::default();
            for id in kids {
                tally.going.insert(id.to_owned());
                children.files.insert(
                    id.to_owned(),
                    Child {
                        row: row.to_owned(),
                        relative: format!("F/{id}"),
                        status: status(id, TransferState::Queued),
                    },
                );
            }
            children.folders.insert(row.to_owned(), tally);
        }
        let state = |row: &str| {
            files
                .transfers()
                .into_iter()
                .find(|t| t.id == row)
                .unwrap()
                .state
        };
        files.finish("a1", TransferState::Failed("corrupt"));
        files.finish("b1", TransferState::Declined(FileDecline::InvalidPath));
        assert_eq!(state("f1"), TransferState::Failed("corrupt"));
        // Cancelling one file cancels the folder: the others are told to
        // stop, and it ends as cancelled with its last one.
        let (cancel, mut stopped) = watch::channel(false);
        files
            .controls
            .lock()
            .unwrap()
            .insert("b2".to_owned(), Control { cancel, turn: None });
        files.finish("a2", TransferState::Cancelled);
        assert!(*stopped.borrow_and_update());
        assert!(!state("f2").finished());
        files.finish("b2", TransferState::Cancelled);
        assert_eq!(state("f2"), TransferState::Cancelled);
    }

    #[test]
    fn cancelling_a_folder_stops_its_files_and_ends_those_cut_off() {
        let files = test_files();
        files.add(TransferStatus {
            direction: Direction::Receive,
            folder: Some(FolderProgress {
                entries: 3,
                ..FolderProgress::default()
            }),
            ..status("row", TransferState::Transferring)
        });
        {
            let mut children = files.children.lock().unwrap();
            let mut tally = Tally::default();
            for id in ["going", "cut off"] {
                tally.going.insert(id.to_owned());
                children.files.insert(
                    id.to_owned(),
                    Child {
                        row: "row".to_owned(),
                        relative: format!("F/{id}"),
                        status: status(id, TransferState::Transferring),
                    },
                );
            }
            children.folders.insert("row".to_owned(), tally);
        }
        // Only the one still receiving has a control; the one cut off
        // waits for its sender.
        let mut stopped = files.control("going");
        let state = || {
            files
                .transfers()
                .into_iter()
                .find(|t| t.id == "row")
                .unwrap()
                .state
        };
        assert!(files.cancel("row"));
        assert!(*stopped.borrow_and_update());
        assert!(!files.children.lock().unwrap().files.contains_key("cut off"));
        assert!(!state().finished());
        files.finish("going", TransferState::Cancelled);
        assert_eq!(state(), TransferState::Cancelled);
        assert!(!files.cancel("nothing"));
    }

    /// The folder pasted files come into is checked again as their batch
    /// starts: gone since the paste, the batch is refused cleanly.
    #[tokio::test]
    async fn a_batch_for_a_paste_is_refused_once_its_folder_is_gone() {
        use iw_proto::peer::v1::FileBatch;

        let files = test_files();
        let mac = DeviceId::new("mac");
        let dir = tempfile::tempdir().unwrap();
        let offer = |batch: &str, offer_id: u64| FileOffer {
            transfer_id: format!("{batch}-a"),
            name: "a".to_owned(),
            size: 1,
            batch: Some(FileBatch {
                batch_id: batch.to_owned(),
                item_id: "item0".to_owned(),
                items: 1,
                files: 1,
                bytes: 1,
                item_files: 1,
                item_bytes: 1,
                clipboard_offer_id: offer_id,
                ..FileBatch::default()
            }),
            ..FileOffer::default()
        };
        let there = dir.path().join("there");
        std::fs::create_dir(&there).unwrap();
        let _ = files.expect_clipboard(&mac, 7, there.clone());
        let batch = files.incoming_batch(&mac, &offer("one", 7)).await.unwrap();
        assert_eq!(batch.folder, there);

        let gone = dir.path().join("gone");
        std::fs::create_dir(&gone).unwrap();
        let _ = files.expect_clipboard(&mac, 8, gone.clone());
        std::fs::remove_dir(&gone).unwrap();
        assert!(matches!(
            files.incoming_batch(&mac, &offer("two", 8)).await,
            Err(FileDecline::Unavailable)
        ));
    }

    #[test]
    fn pasted_files_declined_before_they_were_listed_still_end_their_items() {
        use iw_proto::peer::v1::FileBatch;

        let files = test_files();
        let mac = DeviceId::new("mac");
        let arrival = files.expect_clipboard(&mac, 7, PathBuf::from("/staging"));
        let offer = |id: &str, index: u32, relative: &str, item_files: u32| FileOffer {
            transfer_id: id.to_owned(),
            name: relative.rsplit('/').next().unwrap_or(id).to_owned(),
            size: 1,
            relative_path: relative.to_owned(),
            batch: Some(FileBatch {
                batch_id: "batch".to_owned(),
                item_id: format!("item{index}"),
                item_index: index,
                items: 3,
                files: 4,
                bytes: 4,
                item_files,
                item_bytes: u64::from(item_files),
                clipboard_offer_id: 7,
                ..FileBatch::default()
            }),
            ..FileOffer::default()
        };
        // Item 0, a file, declined before it was listed: it ended.
        files.clipboard_declined(&mac, &offer("x", 0, "", 1), FileDecline::InvalidPath);
        assert_eq!(arrival.borrow().ended.get(&0), Some(&None));
        assert!(arrival.borrow().started);

        // Item 1, a folder of two files: one listed and arrived, the other
        // declined before it was listed. Its row never counts that one;
        // the item ends all the same, where its folder went.
        let a = offer("a", 1, "F/a", 2);
        let place = Destination::of(&a, false).unwrap();
        assert_eq!(files.list_incoming(&mac, &a, &place), Some(false));
        assert!(arrival.borrow().listed.contains(&1));
        files.update("item1", |t| t.path = Some(PathBuf::from("/staging/F")));
        files.finish("a", TransferState::Done);
        assert!(!arrival.borrow().ended.contains_key(&1));
        // Busy is tried again: nothing ends.
        files.clipboard_declined(&mac, &offer("b", 1, "F/b", 2), FileDecline::Busy);
        assert!(!arrival.borrow().ended.contains_key(&1));
        files.clipboard_declined(&mac, &offer("b", 1, "F/b", 2), FileDecline::InvalidPath);
        assert_eq!(
            arrival.borrow().ended.get(&1),
            Some(&Some(PathBuf::from("/staging/F")))
        );

        // Item 2 is never sent: a file declined for the whole batch.
        files.clipboard_declined(&mac, &offer("c", 2, "", 1), FileDecline::TooLarge);
        let arrival = arrival.borrow();
        assert!(arrival.refused);
        assert_eq!(
            super::super::clipboard_files::arrived(&arrival, &[0, 1, 2], &[0, 1, 2]),
            Some(vec![None, Some(PathBuf::from("/staging/F")), None])
        );
    }

    #[tokio::test]
    async fn pasted_files_this_device_checks_do_not_count_against_their_origin() {
        use super::super::clipboard_files::{Patience, await_files};
        use super::super::lazy_clipboard::FilesWait;
        use iw_proto::peer::v1::FileBatch;

        let files = test_files();
        let mac = DeviceId::new("mac");
        let arrival = files.expect_clipboard(&mac, 7, PathBuf::from("/staging"));
        let offer = FileOffer {
            transfer_id: "a".to_owned(),
            name: "a".to_owned(),
            size: 1,
            batch: Some(FileBatch {
                batch_id: "batch".to_owned(),
                item_id: "a".to_owned(),
                items: 1,
                files: 1,
                bytes: 1,
                item_files: 1,
                item_bytes: 1,
                clipboard_offer_id: 7,
                ..FileBatch::default()
            }),
            ..FileOffer::default()
        };
        let place = Destination::of(&offer, false).unwrap();
        assert_eq!(files.list_incoming(&mac, &offer, &place), Some(false));
        let (_expected, items) = watch::channel(Some(vec![0]));
        let patience = Patience {
            stall: Duration::from_millis(200),
            start: Duration::from_secs(10),
            checking: Duration::from_millis(1500),
        };
        let wait = |arrival| {
            tokio::spawn(await_files(
                files.clone(),
                (mac.clone(), 7),
                vec![0],
                (arrival, items.clone()),
                Arc::new(FilesWait::default()),
                patience,
            ))
        };
        let checking = files.clipboard_checking("a");
        let waiting = wait(arrival);

        // All of it came; writing it out and hashing it takes long here.
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert!(!waiting.is_finished());
        // Then nothing more comes from the origin.
        drop(checking);
        let given_up = tokio::time::timeout(Duration::from_secs(5), waiting).await;
        assert_eq!(given_up.unwrap().unwrap(), None);

        // Checking that never ends is given up too, only later.
        let _checking = files.clipboard_checking("a");
        let arrival = files.clipboards.lock().unwrap()[&(mac.clone(), 7)]
            .arrival
            .subscribe();
        let started = Instant::now();
        let given_up = tokio::time::timeout(Duration::from_secs(5), wait(arrival)).await;
        assert_eq!(given_up.unwrap().unwrap(), None);
        assert!(started.elapsed() >= Duration::from_millis(1400));
    }

    #[test]
    fn a_check_counts_back_on_the_receipt_it_counted_on() {
        let files = test_files();
        let mac = DeviceId::new("mac");
        let first = files.expect_clipboard(&mac, 7, PathBuf::from("/staging"));
        files
            .clipboard_rows
            .lock()
            .unwrap()
            .insert("a".to_owned(), ((mac.clone(), 7), 0));
        let checking = files.clipboard_checking("a");
        assert_eq!(first.borrow().checking, 1);
        // The paste is forgotten and made again under the same key, its
        // file listed again and checked anew.
        files.forget_clipboard(&mac, 7);
        let again = files.expect_clipboard(&mac, 7, PathBuf::from("/staging"));
        files
            .clipboard_rows
            .lock()
            .unwrap()
            .insert("a".to_owned(), ((mac.clone(), 7), 0));
        let anew = files.clipboard_checking("a");
        assert_eq!(again.borrow().checking, 1);
        // The first check ending leaves the new one counted.
        drop(checking);
        assert_eq!(again.borrow().checking, 1);
        drop(anew);
        assert_eq!(again.borrow().checking, 0);
    }

    fn test_files() -> Arc<Files> {
        use crate::config::{BackendConfig, Config, DeviceConfig, NetworkConfig, PathsConfig};
        let config = Config {
            device: DeviceConfig {
                id: "mac".to_owned(),
                name: "MacBook Pro".to_owned(),
            },
            network: NetworkConfig::default(),
            paths: PathsConfig::default(),
            backend: BackendConfig::Mock {
                displays: vec![],
                folders: None,
            },
            peers: vec![],
            layout: vec![],
            clipboard: Default::default(),
        };
        let directory = Arc::new(Directory::new(
            &config,
            None,
            crate::store::DevicesFile::default(),
        ));
        let app = Arc::new(AppOpener::new(None, Arc::new(AtomicUsize::new(0))));
        Files::new(
            FileSettings {
                folder: None,
                ask: false,
            },
            directory,
            app,
            Store::Local,
            None,
            false,
        )
    }

    fn stale_parts(dir: &Path, count: usize) -> Vec<PathBuf> {
        let weeks_ago = std::time::SystemTime::now() - STALE_PART * 2;
        (0..count)
            .map(|n| {
                let path = part_path(dir, &format!("old{n}"));
                std::fs::write(&path, "half a file").unwrap();
                std::fs::File::options()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_modified(weeks_ago)
                    .unwrap();
                path
            })
            .collect()
    }

    /// What transfers left long ago is removed before the first file is
    /// written to the folder, and every file waits for that: none can open
    /// its part while parts are being removed, nor find it gone afterwards.
    #[tokio::test(flavor = "multi_thread")]
    async fn files_wait_for_the_folder_to_be_tidied() {
        let dir = tempfile::tempdir().unwrap();
        let stale = stale_parts(dir.path(), 200);
        let going = part_path(dir.path(), "going");
        std::fs::write(&going, "a file still coming").unwrap();

        let files = test_files();
        let tidying: Vec<_> = (0..4)
            .map(|_| {
                let (files, folder, stale) = (files.clone(), dir.path().to_owned(), stale.clone());
                tokio::spawn(async move {
                    files.tidy(&folder).await;
                    assert!(stale.iter().all(|p| !p.exists()));
                })
            })
            .collect();
        for task in tidying {
            task.await.unwrap();
        }
        assert!(going.exists());

        // Once a run.
        let again = stale_parts(dir.path(), 1);
        files.tidy(dir.path()).await;
        assert!(again[0].exists());
    }

    /// A file that stops waiting, as when its sender goes away, does not
    /// let the next one in before the folder is tidied.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_file_that_stops_waiting_does_not_cut_the_tidying_short() {
        let dir = tempfile::tempdir().unwrap();
        let stale = stale_parts(dir.path(), 2000);
        let files = test_files();
        let first = {
            let (files, folder) = (files.clone(), dir.path().to_owned());
            tokio::spawn(async move { files.tidy(&folder).await })
        };
        while !files.tidied.lock().unwrap().contains_key(dir.path()) {
            tokio::task::yield_now().await;
        }
        first.abort();
        files.tidy(dir.path()).await;
        assert!(stale.iter().all(|p| !p.exists()));
    }

    /// A folder being tidied, perhaps while macOS asks the user about
    /// Downloads, does not hold up files for other folders.
    #[tokio::test]
    async fn other_folders_do_not_wait() {
        let dir = tempfile::tempdir().unwrap();
        let files = test_files();
        let (_never, held) = watch::channel(false);
        files
            .tidied
            .lock()
            .unwrap()
            .insert(dir.path().join("Downloads"), held);
        let elsewhere = dir.path().join("Caches");
        std::fs::create_dir(&elsewhere).unwrap();
        let stale = stale_parts(&elsewhere, 1);
        tokio::time::timeout(Duration::from_secs(10), files.tidy(&elsewhere))
            .await
            .expect("waited for another folder");
        assert!(!stale[0].exists());
    }
}
