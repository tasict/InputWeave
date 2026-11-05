//! Clipboards too large to send as the cursor moves are offered instead,
//! and pulled only when an app pastes (docs/architecture.md §8). This is
//! what the engine keeps for that: the offers it made, the pulls it waits
//! on or passes on, and the offer this device's clipboard promises. The
//! engine does the sending.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Condvar, Mutex, TryLockError};
use std::time::Instant;

use iw_core::layout::DeviceId;
use iw_platform::{
    ClipboardContent, ClipboardFormat, ClipboardSource, FilesDone, FilesFetch, PROMISE_WAIT,
    PromisedFile,
};
use iw_proto::peer::v1::{ClipboardPart, ClipboardPull, DragItem};
use tokio::sync::mpsc;

use super::engine::EngineEvent;
use super::files::kept_name;
use super::listing::{MAX_BYTES, MAX_FILES, MAX_ITEMS};

/// The most an offered clipboard may hold, all formats together.
pub(crate) const LAZY_MAX: usize = 256 * 1024 * 1024;

/// The peer protocol version that knows offers and pulls; a session at an
/// earlier one gets clipboards sent whole, or not at all.
pub(crate) const OFFERS_SINCE: u32 = 2;

/// The peer protocol version that knows files in offers, offers of files
/// alone, and `CLIPBOARD_SKIP_BUSY`; a session at an earlier one gets
/// offers without files.
pub(crate) const FILES_SINCE: u32 = 3;

/// How long a pull answered busy waits before it asks again the first
/// time; each time after, twice as long, up to `BUSY_RETRY_MAX`.
pub(crate) const BUSY_RETRY: std::time::Duration = std::time::Duration::from_secs(1);
pub(crate) const BUSY_RETRY_MAX: std::time::Duration = std::time::Duration::from_secs(15);

/// How long to wait after a pull was answered busy following a wait of
/// `last`, if any.
pub(crate) fn busy_backoff(last: Option<std::time::Duration>) -> std::time::Duration {
    last.map_or(BUSY_RETRY, |last| (last * 2).min(BUSY_RETRY_MAX))
}

/// Set in every pull id, so pulls and their answers never meet the ids of
/// clipboard requests.
pub(crate) const PULL_BIT: u64 = 1 << 63;

/// How long a pull is waited on, here or passed on: as long as an app
/// waits for promised content.
pub(crate) const PULL_TIMEOUT: std::time::Duration = PROMISE_WAIT;

/// Offers of its own clipboard this device answers pulls for; older ones
/// are forgotten.
const OWN_OFFERS: usize = 16;

/// Pulls of this device's clipboard read for one device at once; more are
/// answered as unavailable.
const SERVES_PER_PEER: usize = 2;

/// Pulls passed on for one device at once, and for all together; more are
/// answered as unavailable.
const RELAYS_PER_PEER: usize = 4;
const RELAYS: usize = 64;

pub(crate) fn is_pull(id: u64) -> bool {
    id & PULL_BIT != 0
}

/// The most a clipboard may hold to be offered, when the eager limit set
/// is not already larger.
pub(crate) fn lazy_max(eager_max: usize) -> usize {
    LAZY_MAX.max(eager_max)
}

/// How a clipboard of `len` bytes moves on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Delivery {
    /// Whole, as the cursor moves.
    Send,
    /// Offered, to be pulled when an app pastes.
    Offer,
    TooLarge,
}

/// Up to `eager_max` bytes a clipboard is sent at once; beyond, up to the
/// lazy limit, it is offered where `offers` (every device on the way speaks
/// protocol 2), and otherwise it does not move.
pub(crate) fn delivery(len: usize, eager_max: usize, offers: bool) -> Delivery {
    if len <= eager_max {
        Delivery::Send
    } else if offers && len <= lazy_max(eager_max) {
        Delivery::Offer
    } else {
        Delivery::TooLarge
    }
}

/// The formats an offer announces with their sizes, if it is sound: each
/// a known format at most once, together more than nothing and at most
/// `max` bytes. No formats at all is sound only beside files
/// (`with_files`).
pub(crate) fn offered_formats(
    parts: &[ClipboardPart],
    max: usize,
    with_files: bool,
) -> Option<Vec<(ClipboardFormat, u64)>> {
    if parts.is_empty() && with_files {
        return Some(Vec::new());
    }
    if parts.is_empty() || parts.len() > ClipboardFormat::ALL.len() {
        return None;
    }
    let mut formats: Vec<(ClipboardFormat, u64)> = Vec::with_capacity(parts.len());
    let mut total = 0u64;
    for part in parts {
        let format = ClipboardFormat::from_mime(&part.mime)?;
        if formats.iter().any(|(f, _)| *f == format) {
            return None;
        }
        total = total.checked_add(part.size)?;
        formats.push((format, part.size));
    }
    (total > 0 && total <= max as u64).then_some(formats)
}

/// A file or folder an offer carries, copied on the origin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OfferedFile {
    /// Its place in the offer, which the origin names it by.
    pub(crate) index: u32,
    pub(crate) name: String,
    pub(crate) folder: bool,
    pub(crate) size: u64,
    /// Files and empty folders in it; 1 for a file.
    pub(crate) files: u32,
}

/// The files an offer carries that can be promised here, if the offer is
/// sound: at most a batch's items, files and bytes. An item whose name
/// would not be written as it is ([`kept_name`]) is left out, as the file
/// would come under another name than the one promised; the others are
/// still promised.
pub(crate) fn offered_files(items: &[DragItem]) -> Option<Vec<OfferedFile>> {
    if items.len() > MAX_ITEMS {
        return None;
    }
    let (mut files, mut bytes) = (0u64, 0u64);
    let mut offered = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        // A folder is counted once its files are listed, when pasted, and
        // an item the origin could not tell says nothing until then.
        files += u64::from(item.files.max(1));
        let size = if item.unknown { 0 } else { item.size };
        bytes = bytes.checked_add(size)?;
        if !kept_name(&item.name) {
            tracing::debug!(
                index,
                "leaving out an offered file whose name would not be kept"
            );
            continue;
        }
        offered.push(OfferedFile {
            index: index as u32,
            name: item.name.clone(),
            folder: item.folder && !item.unknown,
            size,
            files: item.files.max(1),
        });
    }
    (files <= MAX_FILES as u64 && bytes <= MAX_BYTES).then_some(offered)
}

impl OfferedFile {
    pub(crate) fn promised(&self) -> PromisedFile {
        PromisedFile {
            name: self.name.clone(),
            folder: self.folder,
        }
    }
}

/// The parts announcing `content`.
pub(crate) fn parts_of(content: &ClipboardContent) -> Vec<ClipboardPart> {
    content
        .items
        .iter()
        .map(|(format, data)| ClipboardPart {
            mime: format.mime().to_owned(),
            size: data.len() as u64,
        })
        .collect()
}

/// Offers this device made of its own clipboard, with its change count
/// then and the files it offered. Only pulls for one of them are
/// answered, and only with those very files, so no device can read this
/// clipboard, nor any file, without having been offered it.
#[derive(Debug, Default)]
pub(crate) struct OwnOffers {
    recent: VecDeque<OwnOffer>,
}

#[derive(Debug)]
struct OwnOffer {
    id: u64,
    change: u64,
    files: Arc<Vec<PathBuf>>,
}

impl OwnOffers {
    /// A new offer of the clipboard at change count `change`, with the
    /// files and folders at `files` copied there; its id, from the
    /// system's random numbers so no other device can guess it. `None` if
    /// there are none to be had.
    pub(crate) fn make(&mut self, change: u64, files: Vec<PathBuf>) -> Option<u64> {
        let id = getrandom::u64().ok()?;
        self.recent.push_back(OwnOffer {
            id,
            change,
            files: Arc::new(files),
        });
        if self.recent.len() > OWN_OFFERS {
            self.recent.pop_front();
        }
        Some(id)
    }

    /// The change count `offer_id` was offered at, if this device made it.
    pub(crate) fn change_of(&self, offer_id: u64) -> Option<u64> {
        self.get(offer_id).map(|o| o.change)
    }

    /// The change count `offer_id` was offered at and the files it offered,
    /// if this device made it with files.
    pub(crate) fn files_of(&self, offer_id: u64) -> Option<(u64, Arc<Vec<PathBuf>>)> {
        self.get(offer_id)
            .filter(|o| !o.files.is_empty())
            .map(|o| (o.change, o.files.clone()))
    }

    fn get(&self, offer_id: u64) -> Option<&OwnOffer> {
        self.recent.iter().find(|o| o.id == offer_id)
    }
}

/// What of an offer a pull asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Part {
    /// Its formats, read from the clipboard.
    Content,
    /// Its files, listed and sent.
    Files,
}

/// Pulls of this device's clipboard being served, by the device pulling,
/// the offer and what of it: a burst of pulls never queues a burst of
/// reads, and pulling an offer's content never holds up its files.
#[derive(Debug, Default)]
pub(crate) struct Serving {
    going: Vec<(DeviceId, u64, Part)>,
}

impl Serving {
    /// Whether to serve `from`'s pull of `part` of `offer_id`: not while
    /// the same is being served, nor beyond a few of that part at once
    /// for it.
    pub(crate) fn admit(&mut self, from: &DeviceId, offer_id: u64, part: Part) -> bool {
        let same = self
            .going
            .iter()
            .any(|(d, o, p)| d == from && *o == offer_id && *p == part);
        let busy = self
            .going
            .iter()
            .filter(|(d, _, p)| d == from && *p == part)
            .count()
            >= SERVES_PER_PEER;
        if same || busy {
            return false;
        }
        self.going.push((from.clone(), offer_id, part));
        true
    }

    pub(crate) fn done(&mut self, from: &DeviceId, offer_id: u64, part: Part) {
        if let Some(i) = self
            .going
            .iter()
            .position(|(d, o, p)| d == from && *o == offer_id && *p == part)
        {
            self.going.swap_remove(i);
        }
    }
}

/// What waits for a pull's answer.
#[derive(Debug)]
pub(crate) enum Waiter {
    /// An app pastes here, through the platform's promise.
    App(std_mpsc::Sender<Option<ClipboardContent>>),
    /// This device's clipboard cannot promise, so the content is written
    /// as soon as it arrives, unless the clipboard changed since its change
    /// count was `base`: then something was copied here meanwhile.
    Write { held: Held, base: u64 },
    /// A pull passed on for `to`, which asked under `id`.
    Relay { to: DeviceId, id: u64 },
}

#[derive(Debug)]
struct Pending {
    waiter: Waiter,
    /// The device the pull went to; only its answer counts.
    sent_to: DeviceId,
    /// The pull, to send again when it is answered busy.
    pull: ClipboardPull,
    /// How long it waited the last time it was answered busy.
    backoff: Option<std::time::Duration>,
    deadline: Instant,
}

/// The pulls sent and not yet answered, by id, with the device each went
/// to: the transfer streams that bring their answers read up to the lazy
/// limit only for these.
#[derive(Clone, Debug, Default)]
pub(crate) struct PendingPulls(Arc<Mutex<HashMap<u64, DeviceId>>>);

impl PendingPulls {
    /// Whether pull `id` went to `from`.
    pub(crate) fn expects(&self, id: u64, from: &DeviceId) -> bool {
        self.lock().get(&id) == Some(from)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<u64, DeviceId>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Pulls sent and not yet answered.
#[derive(Debug, Default)]
pub(crate) struct Pulls {
    next: u64,
    pending: HashMap<u64, Pending>,
    shared: PendingPulls,
}

impl Pulls {
    /// Pulls listed in `shared` as they are sent and answered.
    pub(crate) fn new(shared: PendingPulls) -> Self {
        Self {
            shared,
            ..Self::default()
        }
    }

    /// A pull, `pull` with an id of its own, sent to `sent_to` for
    /// `waiter`; that pull.
    pub(crate) fn start(
        &mut self,
        waiter: Waiter,
        sent_to: DeviceId,
        pull: ClipboardPull,
        now: Instant,
    ) -> ClipboardPull {
        self.next = (self.next + 1) & !PULL_BIT;
        let id = self.next | PULL_BIT;
        let pull = ClipboardPull {
            pull_id: id,
            ..pull
        };
        self.shared.lock().insert(id, sent_to.clone());
        self.pending.insert(
            id,
            Pending {
                waiter,
                sent_to,
                pull: pull.clone(),
                backoff: None,
                deadline: now + PULL_TIMEOUT,
            },
        );
        pull
    }

    /// When pull `id`, answered busy by `from`, the device it went to, is
    /// asked again, a little later each time: if it would still be waited
    /// for by then.
    pub(crate) fn retry_in(
        &mut self,
        id: u64,
        from: &DeviceId,
        now: Instant,
    ) -> Option<std::time::Duration> {
        let pending = self.pending.get_mut(&id).filter(|p| p.sent_to == *from)?;
        let wait = busy_backoff(pending.backoff);
        pending.backoff = Some(wait);
        (now + wait < pending.deadline).then_some(wait)
    }

    /// Pull `id` again, if still waited for: where it goes, and itself.
    pub(crate) fn again(&self, id: u64) -> Option<(DeviceId, ClipboardPull)> {
        self.pending
            .get(&id)
            .map(|p| (p.sent_to.clone(), p.pull.clone()))
    }

    /// The waiter of pull `id`, if `from` is the device it went to.
    pub(crate) fn finish(&mut self, id: u64, from: &DeviceId) -> Option<Waiter> {
        if self.pending.get(&id)?.sent_to != *from {
            return None;
        }
        self.shared.lock().remove(&id);
        self.pending.remove(&id).map(|p| p.waiter)
    }

    /// Pulls nobody answered in time.
    pub(crate) fn expire(&mut self, now: Instant) -> Vec<Waiter> {
        self.take(|p| p.deadline <= now)
    }

    /// Pulls that went to `device`, which is gone, and pulls passed on
    /// for it, whose answers nobody would take.
    pub(crate) fn lost(&mut self, device: &DeviceId) -> Vec<Waiter> {
        self.take(|p| {
            p.sent_to == *device || matches!(&p.waiter, Waiter::Relay { to, .. } if to == device)
        })
    }

    pub(crate) fn pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Whether another pull may be passed on for `to`: a few for each
    /// device, a few dozen in all.
    pub(crate) fn may_relay(&self, to: &DeviceId) -> bool {
        let relays = || {
            self.pending.values().filter_map(|p| match &p.waiter {
                Waiter::Relay { to, .. } => Some(to),
                _ => None,
            })
        };
        relays().count() < RELAYS && relays().filter(|t| *t == to).count() < RELAYS_PER_PEER
    }

    fn take(&mut self, mut gone: impl FnMut(&Pending) -> bool) -> Vec<Waiter> {
        let ids: Vec<u64> = self
            .pending
            .iter()
            .filter(|(_, p)| gone(p))
            .map(|(id, _)| *id)
            .collect();
        let mut shared = self.shared.lock();
        ids.into_iter()
            .filter_map(|id| {
                shared.remove(&id);
                self.pending.remove(&id).map(|p| p.waiter)
            })
            .collect()
    }
}

/// The offer this device's clipboard holds, promised to its apps or
/// waiting to be written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Held {
    pub(crate) origin: DeviceId,
    pub(crate) offer_id: u64,
    /// The device the offer came through, which pulls go to.
    pub(crate) via: DeviceId,
    pub(crate) formats: Vec<ClipboardFormat>,
    /// All formats together, in bytes.
    pub(crate) bytes: u64,
    /// Files and folders, fetched straight from the origin when pasted.
    pub(crate) files: Vec<OfferedFile>,
    /// Which promise of the offer the clipboard holds: one made anew once
    /// its files arrived late replaces the first.
    pub(crate) serial: u64,
}

impl Held {
    pub(crate) fn is(&self, origin: &DeviceId, offer_id: u64) -> bool {
        self.origin == *origin && self.offer_id == offer_id
    }
}

/// What a promise has of its content.
#[derive(Debug)]
enum Fetched {
    Not,
    /// Shared with a promise of the same offer made anew.
    Content(Arc<ClipboardContent>),
    /// Pulling it failed, so it is not asked for again.
    Failed,
    /// The promise left the clipboard.
    Gone,
}

/// What a promise has of its files, which the engine fills in as they
/// arrive: an app asking for them waits on it.
#[derive(Debug, Default)]
pub(crate) struct FilesWait {
    inner: Mutex<FilesInner>,
    changed: Condvar,
}

#[derive(Debug, Default)]
struct FilesInner {
    state: FilesState,
    /// An app gave up waiting for them. macOS never asks a promise for
    /// what it once did not get, so once they arrive the clipboard
    /// promises them anew (`EngineEvent::ClipboardFilesEnded`). Read and
    /// set with the state, so an app giving up just as they arrive always
    /// leads to that.
    late: bool,
    /// Those that hear once they ended (`fetch_files_into`), without a
    /// thread waiting for them.
    waiters: Waiters,
}

#[derive(Default)]
struct Waiters(Vec<FilesDone>);

impl std::fmt::Debug for Waiters {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} waiting", self.0.len())
    }
}

impl FilesInner {
    /// What a call asking for them gets now, unless they are still coming.
    fn ended(&self) -> Option<FilesFetch> {
        match &self.state {
            FilesState::Not | FilesState::Coming => None,
            FilesState::Ready(files) => Some(FilesFetch::Ready(files.clone())),
            FilesState::Failed | FilesState::Gone => Some(FilesFetch::Unavailable),
        }
    }

    /// Those waiting, with what they hear, once they ended.
    fn tell(&mut self) -> Option<(Vec<FilesDone>, FilesFetch)> {
        let ended = self.ended()?;
        Some((std::mem::take(&mut self.waiters.0), ended))
    }
}

/// Tells those waiting, outside the lock: they may take other locks.
fn tell(waiting: Option<(Vec<FilesDone>, FilesFetch)>) {
    if let Some((waiters, ended)) = waiting {
        for done in waiters {
            done(ended.clone());
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum FilesState {
    #[default]
    Not,
    Coming,
    /// Where each promised file is, `None` for one that did not come.
    Ready(Vec<Option<PathBuf>>),
    /// They could not be had, so they are not asked for again.
    Failed,
    /// The promise left the clipboard.
    Gone,
}

impl FilesWait {
    /// Files that already arrived, for a promise made anew.
    pub(crate) fn ready(files: Vec<Option<PathBuf>>) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(FilesInner {
                state: FilesState::Ready(files),
                ..FilesInner::default()
            }),
            ..Self::default()
        })
    }

    /// Whether an app gave up waiting for them.
    #[cfg(test)]
    pub(crate) fn late(&self) -> bool {
        self.lock().late
    }

    /// The files arrived, as far as they did: `None` when none could be
    /// had. Returns whether some did after an app gave up waiting, when the
    /// clipboard promises them anew.
    pub(crate) fn arrived(&self, files: Option<Vec<Option<PathBuf>>>) -> bool {
        let mut inner = self.lock();
        let mut late = false;
        if matches!(inner.state, FilesState::Not | FilesState::Coming) {
            inner.state = match files {
                Some(files) if files.iter().any(Option::is_some) => {
                    late = inner.late;
                    FilesState::Ready(files)
                }
                _ => FilesState::Failed,
            };
        }
        let waiting = inner.tell();
        drop(inner);
        self.changed.notify_all();
        tell(waiting);
        late
    }

    fn gone(&self) {
        let mut inner = self.lock();
        inner.state = FilesState::Gone;
        let waiting = inner.tell();
        drop(inner);
        self.changed.notify_all();
        tell(waiting);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FilesInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The promise this device's clipboard makes its apps: content is pulled
/// through the engine the first time an app asks, and kept for the formats
/// asked for next until the promise is gone; files are fetched from the
/// origin into a folder of the agent's, the first time an app asks.
pub(crate) struct PromiseSource {
    engine: mpsc::WeakUnboundedSender<EngineEvent>,
    origin: DeviceId,
    offer_id: u64,
    /// Which promise of the offer this is: one made anew replaces it.
    serial: u64,
    /// How long an app waits for the files.
    files_wait: std::time::Duration,
    fetched: Mutex<Fetched>,
    /// The promise is gone. Set without waiting for a pull that holds
    /// `fetched`, which lets go of what it pulled when it sees this.
    gone: AtomicBool,
    files: Arc<FilesWait>,
}

impl PromiseSource {
    /// How long an app pasting waits for the files, at most: shorter in
    /// tests (`Options::paste_wait`).
    pub(crate) fn waiting(mut self, wait: std::time::Duration) -> Self {
        self.files_wait = wait;
        self
    }

    #[cfg(test)]
    pub(crate) fn new(
        engine: mpsc::WeakUnboundedSender<EngineEvent>,
        origin: DeviceId,
        offer_id: u64,
        serial: u64,
    ) -> Self {
        Self::with_files(engine, origin, offer_id, serial, Arc::default())
    }

    /// A promise whose files are `files`, as when it is made anew once
    /// they arrived.
    pub(crate) fn with_files(
        engine: mpsc::WeakUnboundedSender<EngineEvent>,
        origin: DeviceId,
        offer_id: u64,
        serial: u64,
        files: Arc<FilesWait>,
    ) -> Self {
        Self {
            engine,
            origin,
            offer_id,
            serial,
            files_wait: PROMISE_WAIT,
            fetched: Mutex::new(Fetched::Not),
            gone: AtomicBool::new(false),
            files,
        }
    }

    /// A promise made anew that has what this one pulled already, so
    /// pasting it does not pull that again.
    pub(crate) fn keeping(self, pulled: Option<Arc<ClipboardContent>>) -> Self {
        if let Some(content) = pulled {
            *self.fetched.lock().unwrap_or_else(|e| e.into_inner()) = Fetched::Content(content);
        }
        self
    }

    /// The content it pulled, if it did and still has it. Never waits on
    /// a pull under way.
    pub(crate) fn pulled(&self) -> Option<Arc<ClipboardContent>> {
        match &*self.fetched.try_lock().ok()? {
            Fetched::Content(content) => Some(content.clone()),
            _ => None,
        }
    }

    /// Asks the engine to pull the content, and waits for it.
    fn pull(&self) -> Option<ClipboardContent> {
        let (reply, answer) = std_mpsc::channel();
        let asked = self.engine.upgrade()?.send(EngineEvent::PullClipboard {
            origin: self.origin.clone(),
            offer_id: self.offer_id,
            reply,
        });
        asked.ok()?;
        answer.recv_timeout(PULL_TIMEOUT).ok().flatten()
    }
}

impl PromiseSource {
    /// Has the engine fetch the files, the first time: into `folder`, or,
    /// without one, into a folder of the agent's.
    fn ask_files(&self, inner: &mut FilesInner, folder: Option<PathBuf>) {
        if inner.state != FilesState::Not {
            return;
        }
        let asked = self.engine.upgrade().is_some_and(|engine| {
            engine
                .send(EngineEvent::PullClipboardFiles {
                    origin: self.origin.clone(),
                    offer_id: self.offer_id,
                    wait: self.files.clone(),
                    folder,
                })
                .is_ok()
        });
        inner.state = if asked {
            FilesState::Coming
        } else {
            FilesState::Failed
        };
    }
}

impl PromiseSource {
    /// Pulls the content, the first time, unless the promise is gone;
    /// `keep_failure` marks a failed pull so it is not tried again.
    fn fetch_kept(&self, format: ClipboardFormat, keep_failure: bool) -> Option<Vec<u8>> {
        if self.gone.load(Ordering::SeqCst) {
            return None;
        }
        let mut fetched = self.fetched.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(*fetched, Fetched::Not) {
            match self.pull() {
                Some(content) => *fetched = Fetched::Content(Arc::new(content)),
                None if keep_failure => *fetched = Fetched::Failed,
                None => {}
            }
        }
        // Gone while it was pulled: what came is let go here.
        if self.gone.load(Ordering::SeqCst) {
            *fetched = Fetched::Gone;
        }
        match &*fetched {
            Fetched::Content(content) => content.get(format).map(<[u8]>::to_vec),
            _ => None,
        }
    }
}

impl ClipboardSource for PromiseSource {
    fn fetch(&self, format: ClipboardFormat) -> Option<Vec<u8>> {
        self.fetch_kept(format, true)
    }

    /// A failed pull is not kept: a paste pulls again.
    fn fetch_ahead(&self, format: ClipboardFormat) -> Option<Vec<u8>> {
        self.fetch_kept(format, false)
    }

    /// The first call has the engine fetch the files and waits for them,
    /// at most as long as an app waits; a call while they come waits too,
    /// and past that says they are still coming, as every call does from
    /// then on: the paste that got nothing is over, and the clipboard
    /// promises them anew once they arrived.
    fn fetch_files(&self) -> FilesFetch {
        if self.gone.load(Ordering::SeqCst) {
            return FilesFetch::Unavailable;
        }
        let mut inner = self.files.lock();
        self.ask_files(&mut inner, None);
        let deadline = Instant::now() + self.files_wait;
        while inner.state == FilesState::Coming {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() || inner.late {
                inner.late = true;
                return FilesFetch::Busy;
            }
            inner = self
                .files
                .changed
                .wait_timeout(inner, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        match &inner.state {
            FilesState::Ready(files) => FilesFetch::Ready(files.clone()),
            _ => FilesFetch::Unavailable,
        }
    }

    /// The first call has the engine fetch the files into `folder`; every
    /// call hears once they all ended, which the engine bounds by giving
    /// up on files that stall, or once the promise is gone.
    fn fetch_files_into(&self, folder: PathBuf, done: FilesDone) {
        if self.gone.load(Ordering::SeqCst) {
            done(FilesFetch::Unavailable);
            return;
        }
        let mut inner = self.files.lock();
        self.ask_files(&mut inner, Some(folder));
        match inner.ended() {
            None => inner.waiters.0.push(done),
            Some(ended) => {
                drop(inner);
                done(ended);
            }
        }
    }

    fn origin(&self) -> Option<String> {
        Some(self.origin.to_string())
    }

    /// Never waits: while a fetch pulls, it lets go of the content itself.
    fn finished(&self) {
        self.files.gone();
        self.gone.store(true, Ordering::SeqCst);
        match self.fetched.try_lock() {
            Ok(mut fetched) => *fetched = Fetched::Gone,
            Err(TryLockError::Poisoned(e)) => *e.into_inner() = Fetched::Gone,
            Err(TryLockError::WouldBlock) => {}
        }
        if let Some(engine) = self.engine.upgrade() {
            let _ = engine.send(EngineEvent::PromiseGone {
                origin: self.origin.clone(),
                offer_id: self.offer_id,
                serial: self.serial,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> DeviceId {
        DeviceId::new(s)
    }

    fn part(mime: &str, size: u64) -> ClipboardPart {
        ClipboardPart {
            mime: mime.to_owned(),
            size,
        }
    }

    #[test]
    fn small_clipboards_move_whole_and_large_ones_are_offered() {
        const MB: usize = 1024 * 1024;
        assert_eq!(delivery(8 * MB, 8 * MB, true), Delivery::Send);
        assert_eq!(delivery(8 * MB + 1, 8 * MB, true), Delivery::Offer);
        assert_eq!(delivery(LAZY_MAX, 8 * MB, true), Delivery::Offer);
        assert_eq!(delivery(LAZY_MAX + 1, 8 * MB, true), Delivery::TooLarge);
        // A device on the way that knows no offers.
        assert_eq!(delivery(9 * MB, 8 * MB, false), Delivery::TooLarge);
        assert_eq!(delivery(MB, 8 * MB, false), Delivery::Send);
        // An eager limit set above the lazy one wins.
        assert_eq!(delivery(LAZY_MAX + 1, LAZY_MAX + 2, false), Delivery::Send);
    }

    #[test]
    fn offers_announce_known_formats_within_the_limit() {
        let text = "text/plain;charset=utf-8";
        assert_eq!(
            offered_formats(&[part(text, 10), part("image/png", 20)], 100, false),
            Some(vec![
                (ClipboardFormat::Text, 10),
                (ClipboardFormat::Png, 20)
            ])
        );
        assert_eq!(offered_formats(&[], 100, false), None);
        // Files may come with no formats at all.
        assert_eq!(offered_formats(&[], 100, true), Some(Vec::new()));
        assert_eq!(offered_formats(&[part(text, 0)], 100, false), None);
        assert_eq!(offered_formats(&[part(text, 101)], 100, false), None);
        assert_eq!(
            offered_formats(&[part("text/x-unknown", 1)], 100, false),
            None
        );
        assert_eq!(
            offered_formats(&[part(text, 1), part(text, 1)], 100, false),
            None
        );
        assert_eq!(
            offered_formats(
                &[part(text, u64::MAX), part("image/png", 2)],
                usize::MAX,
                false
            ),
            None
        );
    }

    #[test]
    fn only_offers_made_are_known_and_old_ones_are_forgotten() {
        let mut own = OwnOffers::default();
        let first = own.make(7, Vec::new()).unwrap();
        assert_eq!(own.change_of(first), Some(7));
        assert_eq!(own.change_of(first ^ 1), None);
        for change in 0..OWN_OFFERS as u64 {
            own.make(change, Vec::new());
        }
        assert_eq!(own.change_of(first), None);
        assert_ne!(own.make(1, Vec::new()), own.make(1, Vec::new()));
    }

    fn item(name: &str, files: u32, size: u64) -> DragItem {
        DragItem {
            name: name.to_owned(),
            folder: files > 1,
            size,
            files,
            unknown: false,
        }
    }

    #[test]
    fn an_item_the_origin_could_not_tell_is_promised_as_a_file_of_no_size() {
        let unknown = DragItem {
            unknown: true,
            folder: true,
            ..item("locked", 0, u64::MAX)
        };
        assert_eq!(
            offered_files(&[unknown, item("report.txt", 1, 10)]),
            Some(vec![
                OfferedFile {
                    index: 0,
                    name: "locked".to_owned(),
                    folder: false,
                    size: 0,
                    files: 1,
                },
                OfferedFile {
                    index: 1,
                    name: "report.txt".to_owned(),
                    folder: false,
                    size: 10,
                    files: 1,
                },
            ])
        );
    }

    #[test]
    fn offers_carry_plain_names_within_a_batch() {
        let long = format!("{}.txt", "長".repeat(100));
        assert_eq!(
            offered_files(&[
                item("report.txt", 1, 10),
                item("a\\b.txt", 1, 5),
                item(&long, 1, 5),
                item("photos", 3, 20),
            ]),
            // Names that would not be kept are left out, each alone; the
            // others keep their places in the offer.
            Some(vec![
                OfferedFile {
                    index: 0,
                    name: "report.txt".to_owned(),
                    folder: false,
                    size: 10,
                    files: 1,
                },
                OfferedFile {
                    index: 3,
                    name: "photos".to_owned(),
                    folder: true,
                    size: 20,
                    files: 3,
                },
            ])
        );
        assert_eq!(offered_files(&[]), Some(Vec::new()));
        for name in ["", ".", "..", "a/b", "a\\b", "a\0b", " a"] {
            assert_eq!(
                offered_files(&[item(name, 1, 1)]),
                Some(Vec::new()),
                "{name:?}"
            );
        }
        let many: Vec<DragItem> = (0..=MAX_ITEMS)
            .map(|i| item(&i.to_string(), 1, 1))
            .collect();
        assert_eq!(offered_files(&many), None);
        assert_eq!(offered_files(&[item("big", 1, MAX_BYTES + 1)]), None);
        assert_eq!(
            offered_files(&[item("full", MAX_FILES as u32 + 1, 1)]),
            None
        );
        assert_eq!(
            offered_files(&[item("a", 1, u64::MAX), item("b", 1, 1)]),
            None
        );
    }

    #[test]
    fn only_the_files_an_offer_made_are_its_own() {
        let mut own = OwnOffers::default();
        let paths = vec![PathBuf::from("/Users/me/report.txt")];
        let with = own.make(3, paths.clone()).unwrap();
        let without = own.make(4, Vec::new()).unwrap();
        assert_eq!(
            own.files_of(with).map(|(c, p)| (c, p.to_vec())),
            Some((3, paths))
        );
        // An offer without files, and one never made, have none.
        assert!(own.files_of(without).is_none());
        assert!(own.files_of(with ^ without ^ 1).is_none());
        assert_eq!(own.change_of(without), Some(4));
    }

    fn held(origin: &str, offer_id: u64) -> Held {
        Held {
            origin: id(origin),
            offer_id,
            via: id(origin),
            formats: vec![ClipboardFormat::Text],
            bytes: 1,
            files: Vec::new(),
            serial: 1,
        }
    }

    #[test]
    fn pull_ids_are_marked_and_answered_only_by_their_device() {
        let now = Instant::now();
        let shared = PendingPulls::default();
        let mut pulls = Pulls::new(shared.clone());
        let sent = pulls.start(
            Waiter::Write {
                held: held("pc", 3),
                base: 1,
            },
            id("pc"),
            ClipboardPull {
                pull_id: 0,
                offer_id: 3,
                origin_device_id: "pc".to_owned(),
                mimes: Vec::new(),
            },
            now,
        );
        let pull = sent.pull_id;
        assert_eq!(sent.offer_id, 3);
        assert!(is_pull(pull));
        assert!(!is_pull(pull & !PULL_BIT));
        // Transfer streams know to expect its answer, from the PC only.
        assert!(shared.expects(pull, &id("pc")));
        assert!(!shared.expects(pull, &id("mini")));
        assert!(!shared.expects(pull ^ 1, &id("pc")));
        assert!(pulls.finish(pull, &id("mini")).is_none());
        assert!(matches!(
            pulls.finish(pull, &id("pc")),
            Some(Waiter::Write { base: 1, .. })
        ));
        assert!(!shared.expects(pull, &id("pc")));
        assert!(pulls.finish(pull, &id("pc")).is_none());
        assert!(!pulls.pending());
    }

    #[test]
    fn a_pull_answered_busy_is_asked_again_while_it_is_waited_for() {
        let now = Instant::now();
        let mut pulls = Pulls::default();
        let (reply, _answer) = std_mpsc::channel();
        let sent = pulls.start(
            Waiter::App(reply),
            id("pc"),
            ClipboardPull {
                pull_id: 0,
                offer_id: 3,
                origin_device_id: "pc".to_owned(),
                mimes: vec!["text/html".to_owned()],
            },
            now,
        );
        let id_ = sent.pull_id;
        // Asked again a little later each time, up to a limit.
        let waits: Vec<_> = (0..6)
            .map(|_| pulls.retry_in(id_, &id("pc"), now))
            .collect();
        let secs = |s| Some(std::time::Duration::from_secs(s));
        assert_eq!(
            waits,
            [secs(1), secs(2), secs(4), secs(8), secs(15), secs(15)]
        );
        // Only the device it went to is heard, and not past its deadline.
        assert_eq!(pulls.retry_in(id_, &id("mini"), now), None);
        assert_eq!(pulls.retry_in(id_, &id("pc"), now + PULL_TIMEOUT), None);
        assert_eq!(pulls.again(id_), Some((id("pc"), sent)));
        pulls.finish(id_, &id("pc"));
        assert_eq!(pulls.again(id_), None);
    }

    /// The engine's side of a promise's pull: answers the first pull
    /// with `answer`.
    fn answer_pull(
        events: &mut mpsc::UnboundedReceiver<EngineEvent>,
        answer: Option<ClipboardContent>,
    ) {
        match events.blocking_recv() {
            Some(EngineEvent::PullClipboard {
                offer_id: 4, reply, ..
            }) => reply.send(answer).unwrap(),
            _ => panic!("no pull"),
        }
    }

    #[test]
    fn a_promise_pulls_once_and_lets_go_when_gone() {
        let (engine, mut events) = mpsc::unbounded_channel();
        let source = Arc::new(PromiseSource::new(engine.downgrade(), id("pc"), 4, 1));
        let pasting = source.clone();
        let paste = std::thread::spawn(move || pasting.fetch(ClipboardFormat::Html));
        let content = ClipboardContent {
            items: vec![
                (ClipboardFormat::Text, b"t".to_vec()),
                (ClipboardFormat::Html, b"<b>t</b>".to_vec()),
            ],
        };
        answer_pull(&mut events, Some(content));
        assert_eq!(paste.join().unwrap(), Some(b"<b>t</b>".to_vec()));
        // The other format comes from what was pulled.
        assert_eq!(source.fetch(ClipboardFormat::Text), Some(b"t".to_vec()));
        assert_eq!(source.fetch(ClipboardFormat::Png), None);
        assert!(events.try_recv().is_err());
        // Copied over: what was pulled is let go, and the engine told.
        source.finished();
        assert!(matches!(
            events.try_recv(),
            Ok(EngineEvent::PromiseGone { offer_id: 4, .. })
        ));
        assert_eq!(source.fetch(ClipboardFormat::Text), None);
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn a_promise_made_anew_keeps_what_the_first_pulled() {
        let (engine, mut events) = mpsc::unbounded_channel();
        let first = Arc::new(PromiseSource::new(engine.downgrade(), id("pc"), 4, 1));
        assert!(first.pulled().is_none());
        let pasting = first.clone();
        let paste = std::thread::spawn(move || pasting.fetch(ClipboardFormat::Text));
        let names = ClipboardContent {
            items: vec![(ClipboardFormat::Text, b"names".to_vec())],
        };
        answer_pull(&mut events, Some(names));
        assert_eq!(paste.join().unwrap(), Some(b"names".to_vec()));
        let anew = PromiseSource::new(engine.downgrade(), id("pc"), 4, 2).keeping(first.pulled());
        // The first goes as the one made anew replaces it.
        first.finished();
        assert!(matches!(
            events.try_recv(),
            Ok(EngineEvent::PromiseGone { serial: 1, .. })
        ));
        assert_eq!(anew.fetch(ClipboardFormat::Text), Some(b"names".to_vec()));
        assert!(events.try_recv().is_err(), "pulled again");
    }

    #[test]
    fn files_late_for_a_paste_come_with_the_promise_made_anew() {
        let (engine, mut events) = mpsc::unbounded_channel();
        let mut source = PromiseSource::new(engine.downgrade(), id("pc"), 4, 1);
        source.files_wait = std::time::Duration::from_millis(50);
        // The paste has the engine fetch them, and gives up waiting.
        assert_eq!(source.fetch_files(), FilesFetch::Busy);
        let Ok(EngineEvent::PullClipboardFiles {
            offer_id: 4, wait, ..
        }) = events.try_recv()
        else {
            panic!("the files were not asked for");
        };
        assert!(wait.late());
        // Asked again while they come: at once, and not fetched again.
        let start = Instant::now();
        assert_eq!(source.fetch_files(), FilesFetch::Busy);
        assert!(start.elapsed() < std::time::Duration::from_millis(40));
        assert!(events.try_recv().is_err());
        // They arrive: the promise made anew has them from the start.
        let files = vec![Some(PathBuf::from("/staging/report.txt")), None];
        wait.arrived(Some(files.clone()));
        assert_eq!(source.fetch_files(), FilesFetch::Ready(files.clone()));
        let anew = PromiseSource::with_files(
            engine.downgrade(),
            id("pc"),
            4,
            2,
            FilesWait::ready(files.clone()),
        );
        assert_eq!(anew.fetch_files(), FilesFetch::Ready(files));
        assert!(events.try_recv().is_err());
        // The first promise going says which it was.
        source.finished();
        assert!(matches!(
            events.try_recv(),
            Ok(EngineEvent::PromiseGone { serial: 1, .. })
        ));
        assert_eq!(source.fetch_files(), FilesFetch::Unavailable);
        // None arriving at all, they are not asked for again.
        let failed = PromiseSource::new(engine.downgrade(), id("pc"), 5, 3);
        let pasting = std::thread::spawn(move || failed.fetch_files());
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        let wait = loop {
            if let Ok(EngineEvent::PullClipboardFiles { wait, .. }) = events.try_recv() {
                break wait;
            }
            assert!(Instant::now() < deadline, "the files were not asked for");
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        wait.arrived(Some(vec![None]));
        assert_eq!(pasting.join().unwrap(), FilesFetch::Unavailable);
    }

    #[test]
    fn giving_up_and_arriving_together_always_leads_to_a_promise_made_anew() {
        let path = vec![Some(PathBuf::from("/staging/a"))];
        // The app gave up first: the files arriving say so.
        let (engine, mut events) = mpsc::unbounded_channel();
        let mut source = PromiseSource::new(engine.downgrade(), id("pc"), 4, 1);
        source.files_wait = std::time::Duration::ZERO;
        assert_eq!(source.fetch_files(), FilesFetch::Busy);
        let Ok(EngineEvent::PullClipboardFiles { wait, .. }) = events.try_recv() else {
            panic!("the files were not asked for");
        };
        assert!(wait.arrived(Some(path.clone())));
        // They arrived first: the app gets them, and nothing is made anew.
        let source = PromiseSource::new(engine.downgrade(), id("pc"), 5, 2);
        let asked = std::thread::spawn(move || source.fetch_files());
        let wait = loop {
            if let Ok(EngineEvent::PullClipboardFiles { wait, .. }) = events.try_recv() {
                break wait;
            }
            std::thread::yield_now();
        };
        assert!(!wait.arrived(Some(path.clone())));
        assert_eq!(asked.join().unwrap(), FilesFetch::Ready(path.clone()));
        // None arriving is never late, and arriving twice counts once.
        let wait = FilesWait::default();
        wait.lock().late = true;
        assert!(!wait.arrived(Some(vec![None])));
        assert!(!wait.arrived(Some(path)));
    }

    #[test]
    fn a_promise_gone_mid_pull_does_not_wait_for_it_and_keeps_nothing() {
        let (engine, mut events) = mpsc::unbounded_channel();
        let source = Arc::new(PromiseSource::new(engine.downgrade(), id("pc"), 4, 1));
        let pasting = source.clone();
        let paste = std::thread::spawn(move || pasting.fetch(ClipboardFormat::Text));
        // The pull is under way, holding what the promise has.
        let Some(EngineEvent::PullClipboard { reply, .. }) = events.blocking_recv() else {
            panic!("no pull");
        };
        let started = std::time::Instant::now();
        source.finished();
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert!(matches!(
            events.try_recv(),
            Ok(EngineEvent::PromiseGone { offer_id: 4, .. })
        ));
        // What comes after is let go.
        let content = ClipboardContent {
            items: vec![(ClipboardFormat::Text, b"t".to_vec())],
        };
        reply.send(Some(content)).unwrap();
        assert_eq!(paste.join().unwrap(), None);
        assert!(matches!(*source.fetched.lock().unwrap(), Fetched::Gone));
        assert_eq!(source.fetch(ClipboardFormat::Text), None);
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn a_failed_pull_is_not_tried_again() {
        let (engine, mut events) = mpsc::unbounded_channel();
        let source = Arc::new(PromiseSource::new(engine.downgrade(), id("pc"), 4, 1));
        let pasting = source.clone();
        let paste = std::thread::spawn(move || pasting.fetch(ClipboardFormat::Text));
        answer_pull(&mut events, None);
        assert_eq!(paste.join().unwrap(), None);
        // The next format asked for gets nothing at once, without a pull.
        assert_eq!(source.fetch(ClipboardFormat::Html), None);
        assert!(events.try_recv().is_err());
    }

    /// Content fetched ahead of a paste that cannot be had is not taken to
    /// be gone: the paste pulls again, and gets it.
    #[test]
    fn a_failed_pull_ahead_is_tried_again_at_paste() {
        let (engine, mut events) = mpsc::unbounded_channel();
        let source = Arc::new(PromiseSource::new(engine.downgrade(), id("pc"), 4, 1));
        let ahead = source.clone();
        let early = std::thread::spawn(move || ahead.fetch_ahead(ClipboardFormat::Text));
        answer_pull(&mut events, None);
        assert_eq!(early.join().unwrap(), None);
        let pasting = source.clone();
        let paste = std::thread::spawn(move || pasting.fetch(ClipboardFormat::Text));
        answer_pull(
            &mut events,
            Some(ClipboardContent {
                items: vec![(ClipboardFormat::Text, b"names".to_vec())],
            }),
        );
        assert_eq!(paste.join().unwrap(), Some(b"names".to_vec()));
        // Pulled once it came: ahead or not, nothing more is asked for.
        assert_eq!(
            source.fetch_ahead(ClipboardFormat::Text),
            Some(b"names".to_vec())
        );
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn a_burst_of_pulls_reads_the_clipboard_only_a_few_times() {
        let mut serving = Serving::default();
        let content = Part::Content;
        // The same pull again and again: one read.
        let admitted = (0..10)
            .filter(|_| serving.admit(&id("mini"), 7, content))
            .count();
        assert_eq!(admitted, 1);
        // Other offers for the same device: up to the cap.
        assert!(serving.admit(&id("mini"), 8, content));
        assert!(!serving.admit(&id("mini"), 9, content));
        // Another device is not held up by it, nor the offer's files.
        assert!(serving.admit(&id("pc"), 7, content));
        assert!(serving.admit(&id("mini"), 7, Part::Files));
        assert!(!serving.admit(&id("mini"), 7, Part::Files));
        serving.done(&id("mini"), 7, content);
        assert!(serving.admit(&id("mini"), 9, content));
    }

    #[test]
    fn pulls_passed_on_are_capped() {
        let now = Instant::now();
        let mut pulls = Pulls::default();
        let relay = |pulls: &mut Pulls, to: &str, n: u64| {
            pulls.start(
                Waiter::Relay {
                    to: id(to),
                    id: n | PULL_BIT,
                },
                id("pc"),
                ClipboardPull::default(),
                now,
            )
        };
        for n in 0..RELAYS_PER_PEER as u64 {
            assert!(pulls.may_relay(&id("mini")));
            relay(&mut pulls, "mini", n);
        }
        assert!(!pulls.may_relay(&id("mini")));
        // Others still may, up to the cap for all.
        let mut n = 0;
        while pulls.may_relay(&id(&format!("d{}", n / RELAYS_PER_PEER))) {
            relay(&mut pulls, &format!("d{}", n / RELAYS_PER_PEER), n as u64);
            n += 1;
        }
        assert_eq!(n + RELAYS_PER_PEER, RELAYS);
        // A device's own pulls do not count.
        let (reply, _answer) = std_mpsc::channel();
        pulls.start(Waiter::App(reply), id("pc"), ClipboardPull::default(), now);
        assert!(!pulls.may_relay(&id("other")));
    }

    #[test]
    fn pulls_lapse_and_go_with_their_devices() {
        let now = Instant::now();
        let shared = PendingPulls::default();
        let mut pulls = Pulls::new(shared.clone());
        let (reply, _answer) = std_mpsc::channel();
        pulls.start(
            Waiter::App(reply),
            id("mini"),
            ClipboardPull::default(),
            now,
        );
        pulls.start(
            Waiter::Relay {
                to: id("pc"),
                id: 9 | PULL_BIT,
            },
            id("mac"),
            ClipboardPull::default(),
            now,
        );
        let late = now + PULL_TIMEOUT / 2;
        pulls.start(
            Waiter::Write {
                held: held("mac", 1),
                base: 0,
            },
            id("mac"),
            ClipboardPull::default(),
            late,
        );
        assert!(pulls.expire(now + PULL_TIMEOUT / 4).is_empty());
        let lapsed = pulls.expire(now + PULL_TIMEOUT);
        assert_eq!(lapsed.len(), 2);
        // The pull still waiting went to the Mac, which goes away.
        assert!(pulls.lost(&id("pc")).is_empty());
        assert!(matches!(pulls.lost(&id("mac"))[..], [Waiter::Write { .. }]));
        assert!(!pulls.pending());
        assert!(shared.lock().is_empty());

        // A pull passed on for a device that goes away is dropped.
        pulls.start(
            Waiter::Relay {
                to: id("pc"),
                id: 1 | PULL_BIT,
            },
            id("mac"),
            ClipboardPull::default(),
            now,
        );
        assert_eq!(pulls.lost(&id("pc")).len(), 1);
    }
}
