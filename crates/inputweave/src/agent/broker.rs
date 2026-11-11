//! The agent's side of the Windows broker (`iw_winsvc::broker`).
//!
//! On Windows the agent runs as SYSTEM and never touches the user's
//! files or clipboard, so the broker, running as the logged-on user, does
//! that work over the pipe the agent serves it on, in
//! `inputweave.service.v1`:
//!
//! - Desktop pictures: [`Broker`] keeps the previews the broker sent and
//!   is this device's [`DesktopPictures`]: the platform poller reads it
//!   like any other backend, and each read asks a connected broker to look
//!   again (one that connects sends them by itself). Until a broker sent
//!   them, they are not known, and other devices keep theirs.
//! - Clipboard: [`BrokerClipboard`] is this device's [`Clipboard`], so the
//!   engine moves clipboards as it does on macOS. The engine reads the
//!   clipboard on its own task, in the middle of moving input, so nothing
//!   here waits for the broker: the broker reports each change other apps
//!   make (`ClipboardChanged`), the agent then asks for the content at
//!   once, while clipboard sync is on, and keeps it; a read returns what
//!   was kept for the latest change. Writes are sent and not waited for;
//!   the broker marks them as InputWeave's and does not report them, so
//!   the change count the engine sees stays where it was. A clipboard
//!   larger than read ahead is read when the engine asks for more, with
//!   the engine's thread waiting for it ([`BrokerClipboard`]'s `read`).
//!   Content another device offers is promised (`PromiseClipboard`, which
//!   the broker acknowledges): the broker asks for a format when an app
//!   pastes it (`RenderClipboard`), the promise's source pulls it, and the
//!   agent answers (`ClipboardRendered`), unless the broker stopped waiting
//!   (`RenderCancelled`); the broker says when the promise is gone
//!   (`PromiseGone`).
//! - Received files: [`Broker::open_file`] has the broker write a part
//!   file in the receive folder, as the user; `files` keeps the
//!   transfer's state and decides, from the hash the broker reports,
//!   whether to keep the file, and where in the folder received for a
//!   folder ([`Broker::claim_folder`], [`Broker::make_folder`]). Where the
//!   user drops files dragged from another device is checked as the user
//!   too ([`Broker::check_folder`]).
//! - Sent files: [`BrokerLister`] has the broker list the files and
//!   folders the user sends, and [`Broker::open_for_sending`] open each
//!   file, as the user, then hash it and read the parts the transfer needs
//!   ([`BrokerSource`]). Any process of the user can ask the agent to send
//!   a path, so the agent, which runs as SYSTEM, never looks at it: that
//!   would let them send files the user cannot read.
//! - Starting the app: [`Broker::launch_app`] asks the broker to start it as
//!   the user, when the user has to see or decide something and no app is
//!   connected (`app`).
//! - Transfer history: the broker keeps the user's, in the user's folder,
//!   since it holds the names of the user's files (`history`). Each
//!   connection is asked for it once (`LoadHistory`); [`UserHistory`] says
//!   whose the history is and what the broker loaded, and
//!   [`Broker::save_history`] saves it through the connection it was
//!   loaded from, and no other, so that one user's history never lands in
//!   another's file.
//!
//! The newest connection does the clipboard and files work: a broker that
//! restarts connects again before its old connection is noticed gone.
//!
//! The broker is less trusted than the agent (any process of the user can
//! open the pipe), so what it sends is held to limits: pictures to those of
//! other devices' (`previews::accept_pictures`), clipboards to the size
//! the agent asked for, histories to `history::MAX_BYTES`, and a broker
//! that breaks the protocol is dropped.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use iw_core::layout::Display;
use iw_ipc::{IpcConnection, IpcListener, IpcReader};
use iw_platform::{
    Clipboard, ClipboardContent, ClipboardFormat, ClipboardPromise, ClipboardRead, ClipboardSource,
    Current, DesktopPictures, HandOver, HandedOver, PlatformError,
};
use iw_proto::service::v1::{
    AgentToBroker, BrokerMessage, CheckFolder, ClaimFolder, ClipboardContents, ClipboardData,
    ClipboardPart, ClipboardRendered, ClipboardState, CloseFile, CloseSending, DiscardFile,
    FileData, FinishFile, HashFile, HistoryLoaded, KeepFile, LaunchApp, LaunchReason, ListPath,
    LoadHistory, MakeFolder, OpenFailure, OpenFile, OpenForSending, PathKind, PromiseClipboard,
    ReadChunk, ReadClipboard, RenderClipboard, RequestDesktopPictures, SaveHistory,
    WithdrawPromise, WriteClipboard, agent_to_broker, broker_message,
};
use iw_proto::service::{MAX_CLIPBOARD, RENDER_WAIT, copied_path, within_copied_limits};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinSet;
use tracing::{debug, info, warn};

use super::app::Need;
use super::app_clipboard::{AppClipboard, Unchanged};
use super::history;
use super::listing::{Kind, Lister, Listing};
use super::previews::{self, Pictures, Refused};

const ACCEPT_RETRY: Duration = Duration::from_millis(100);
/// Clipboard content goes to the broker in pieces of this size.
const PIECE: usize = 1024 * 1024;
/// File messages waiting for the pipe; beyond this, receiving waits for
/// the broker to keep up.
const FILE_QUEUE: usize = 16;
/// How long the broker has to answer about a file.
const FILE_ANSWER: Duration = Duration::from_secs(5 * 60);
/// How long the broker has to open a file to send: the user who asked to
/// send it waits for the answer.
const OPEN_ANSWER: Duration = Duration::from_secs(30);
/// How long the broker has to save the transfer history; the agent waits
/// for it when it stops.
const HISTORY_ANSWER: Duration = Duration::from_secs(10);
/// Promises remembered at most. Only the newest is on the clipboard; the
/// broker says when one is gone, and those beyond are let go.
const MAX_PROMISES: usize = 8;
/// How long the broker has to put a promise on the clipboard: its
/// clipboard thread may be answering an app's paste for up to
/// [`RENDER_WAIT`] first. A promise not answered by then is withdrawn
/// (`WithdrawPromise`), so it never lands once forgotten.
const PROMISE_ANSWER: Duration = RENDER_WAIT.saturating_add(Duration::from_secs(5));

/// [`PROMISE_ANSWER`], or a moment in tests, which see the agent give up.
fn promise_answer() -> Duration {
    if cfg!(test) {
        Duration::from_millis(300)
    } else {
        PROMISE_ANSWER
    }
}
/// How long the broker has to read a clipboard larger than read ahead.
const LARGE_READ_ANSWER: Duration = Duration::from_secs(10);

/// The broker's work for this device. One lives as long as the agent;
/// brokers come and go.
pub(crate) struct Broker {
    state: Mutex<State>,
    /// Bumped to have the connected brokers read the pictures again.
    asks: watch::Sender<u64>,
    /// Where received files go unless the user chose elsewhere, while a
    /// broker is connected to write them.
    folder: watch::Sender<Option<PathBuf>>,
    /// Whose transfer history it is, and what the broker loaded.
    history: watch::Sender<UserHistory>,
    /// The largest clipboard asked for.
    clipboard_max: usize,
    /// The app that holds promises with files, which the broker cannot
    /// (`app_clipboard`); set once as the agent starts.
    app_clipboard: std::sync::OnceLock<Arc<AppClipboard>>,
}

/// The transfer history of the user the current broker runs as, as far as
/// the agent knows it.
#[derive(Clone, Debug, Default)]
pub(crate) enum UserHistory {
    /// No broker is connected: the history is nobody's.
    #[default]
    None,
    /// The current connection, by its id, was asked for its user's
    /// history.
    Loading(u64),
    /// What the current connection, `link`, loaded: the bytes last saved,
    /// or none and why not.
    Loaded {
        link: u64,
        toml: Arc<Vec<u8>>,
        error: String,
    },
}

#[derive(Default)]
struct State {
    /// Brokers connected now; normally one.
    brokers: usize,
    pictures: Pictures,
    /// Whether a broker sent the pictures since the agent started, even
    /// none; until then `pictures` are only empty, not known.
    pictures_sent: bool,
    /// Bumped whenever `pictures` change.
    generation: u64,
    /// The newest connection, which does the clipboard and files work.
    link: Option<Link>,
    next_link: u64,
    next_handle: u64,
    clipboard: ClipboardCache,
    promises: Promises,
    /// Promises waiting for the broker to say it put them on the
    /// clipboard, by number.
    acks: HashMap<u64, std::sync::mpsc::SyncSender<bool>>,
}

/// Content promised on the user's clipboard, by the number the broker
/// knows each promise by, with the connection it went to and where its
/// content comes from.
#[derive(Default)]
struct Promises {
    next: u64,
    held: VecDeque<(u64, u64, Arc<dyn ClipboardSource>)>,
}

impl Promises {
    /// Remembers a promise sent on connection `link`, and returns its
    /// number and those let go to make room.
    fn add(
        &mut self,
        link: u64,
        source: Arc<dyn ClipboardSource>,
    ) -> (u64, Vec<Arc<dyn ClipboardSource>>) {
        self.next += 1;
        self.held.push_back((self.next, link, source));
        let mut gone = Vec::new();
        while self.held.len() > MAX_PROMISES {
            gone.extend(self.held.pop_front().map(|(_, _, source)| source));
        }
        (self.next, gone)
    }

    fn get(&self, link: u64, id: u64) -> Option<Arc<dyn ClipboardSource>> {
        self.held
            .iter()
            .find(|(i, l, _)| *i == id && *l == link)
            .map(|(_, _, source)| source.clone())
    }

    /// Forgets the promises on connection `link` that `which` picks, and
    /// returns them by number.
    fn remove(
        &mut self,
        link: u64,
        which: impl Fn(u64) -> bool,
    ) -> Vec<(u64, Arc<dyn ClipboardSource>)> {
        let mut gone = Vec::new();
        self.held.retain(|(id, l, source)| {
            let goes = *l == link && which(*id);
            if goes {
                gone.push((*id, source.clone()));
            }
            !goes
        });
        gone
    }
}

/// The clipboard as the broker last reported it.
struct ClipboardCache {
    sync: bool,
    /// Changes other apps made, counted: the engine's change count.
    count: u64,
    /// The broker's sequence number for the last change counted.
    sequence: Option<u64>,
    /// What was read, with the files copied in File Explorer, and for
    /// which count.
    snapshot: (u64, Snapshot),
    /// Reads asked of the current link and not answered yet; the broker
    /// answers in order.
    pending: VecDeque<PendingRead>,
    /// What a read larger than read ahead found, for which count, and the
    /// most it took, so the same clipboard is not read again.
    large: Option<(u64, ClipboardRead, usize)>,
}

/// A read the broker has not answered yet.
enum PendingRead {
    /// Read ahead, for the count it is for, at most `clipboard_max`.
    Ahead(u64),
    /// Larger than read ahead, at most `max` bytes, for a caller waiting
    /// on `answer`.
    Waited {
        max: usize,
        answer: std::sync::mpsc::SyncSender<Snapshot>,
    },
}

impl ClipboardCache {
    /// Whether the clipboard was asked to be read ahead for `count`.
    fn reading_ahead(&self, count: u64) -> bool {
        self.pending
            .iter()
            .any(|p| matches!(p, PendingRead::Ahead(c) if *c == count))
    }
}

impl Default for ClipboardCache {
    fn default() -> Self {
        Self {
            sync: true,
            count: 0,
            sequence: None,
            // Until another app copies, nothing is new.
            snapshot: (0, Snapshot::Read(ClipboardRead::Empty, Vec::new())),
            pending: VecDeque::new(),
            large: None,
        }
    }
}

#[derive(Clone, Debug)]
enum Snapshot {
    /// What it holds, and the files copied in File Explorer it names:
    /// paths only the broker opens.
    Read(ClipboardRead, Vec<PathBuf>),
    /// The broker could not read it.
    Unavailable,
}

/// A connection to a broker, as the agent sends on it.
#[derive(Clone)]
struct Link {
    id: u64,
    /// Clipboard messages, each group sent whole.
    control: mpsc::UnboundedSender<Vec<AgentToBroker>>,
    /// File messages, in order, bounded.
    files: mpsc::Sender<AgentToBroker>,
    replies: Arc<Mutex<Replies>>,
    /// The promises whose content is being fetched for the broker, with
    /// what stops waiting for it once the broker gave up; one at a time
    /// for each promise.
    rendering: Arc<Mutex<HashMap<u64, Option<oneshot::Sender<()>>>>>,
}

#[derive(Default)]
struct Replies {
    /// Files waiting for the broker's answer, by handle.
    waiting: HashMap<u64, oneshot::Sender<Result<broker_message::Msg, String>>>,
    /// Files whose writes failed.
    failed: HashSet<u64>,
    /// Saves of the transfer history waiting for the broker's answer, in
    /// the order asked, which is the order answered: why each failed, or
    /// nothing.
    saves: VecDeque<oneshot::Sender<String>>,
    /// The connection ended; nothing more is answered.
    closed: bool,
}

impl Link {
    /// Sends `msg` about file `handle`, on the file queue or else ahead of
    /// it, and waits for the broker's answer about that file, for at most
    /// `wait`.
    async fn ask(
        &self,
        handle: u64,
        msg: AgentToBroker,
        queued: bool,
        wait: Option<Duration>,
    ) -> io::Result<broker_message::Msg> {
        let (tx, rx) = oneshot::channel();
        {
            let mut replies = self.replies.lock().unwrap();
            if replies.closed {
                return Err(not_connected());
            }
            replies.waiting.insert(handle, tx);
        }
        if queued {
            self.files.send(msg).await.map_err(|_| not_connected())?;
        } else {
            self.control.send(vec![msg]).map_err(|_| not_connected())?;
        }
        let answer = match wait {
            Some(wait) => tokio::time::timeout(wait, rx).await.map_err(|_| {
                io::Error::new(io::ErrorKind::TimedOut, "the broker did not answer")
            })?,
            None => rx.await,
        };
        match answer {
            Ok(Ok(answer)) => Ok(answer),
            Ok(Err(failed)) => Err(io::Error::other(failed)),
            Err(_) => Err(not_connected()),
        }
    }

    fn forget(&self, handle: u64) {
        let mut replies = self.replies.lock().unwrap();
        replies.waiting.remove(&handle);
        replies.failed.remove(&handle);
    }

    fn is_down(&self) -> bool {
        self.replies.lock().unwrap().closed
    }
}

/// Why a file to send could not be opened.
#[derive(Debug, thiserror::Error)]
pub(crate) enum OpenError {
    #[error("no broker is connected to read it as the user")]
    NoBroker,
    /// Nothing is there, or it is no file.
    #[error("{0}")]
    NotAFile(String),
    /// The user may not read it, or the broker could not.
    #[error("{0}")]
    Unreadable(String),
}

fn says(msg: agent_to_broker::Msg) -> AgentToBroker {
    AgentToBroker { msg: Some(msg) }
}

/// `header`, then the bytes of `parts` in [`ClipboardData`] pieces of at
/// most [`PIECE`], to be sent as one group.
fn in_pieces<'a>(
    header: AgentToBroker,
    parts: impl IntoIterator<Item = &'a [u8]>,
) -> Vec<AgentToBroker> {
    let mut group = vec![header];
    for part in parts {
        for piece in part.chunks(PIECE) {
            group.push(says(agent_to_broker::Msg::ClipboardData(ClipboardData {
                data: piece.to_vec(),
            })));
        }
    }
    group
}

/// What the agent answers for a promised format.
enum Answer {
    Content(Vec<u8>),
    /// Still fetching it for an earlier request.
    Busy,
    Unavailable,
}

/// The answer to `ask`, the content in pieces after its header.
fn rendered(ask: &RenderClipboard, answer: Answer) -> Vec<AgentToBroker> {
    let (ok, busy, data) = match &answer {
        Answer::Content(data) => (true, false, Some(data.as_slice())),
        Answer::Busy => (false, true, None),
        Answer::Unavailable => (false, false, None),
    };
    let header = says(agent_to_broker::Msg::ClipboardRendered(ClipboardRendered {
        promise_id: ask.promise_id,
        mime: ask.mime.clone(),
        ok,
        size: data.map_or(0, |d| d.len() as u64),
        busy,
    }));
    in_pieces(header, data)
}

impl Broker {
    pub(crate) fn new(clipboard_max: usize) -> Self {
        Self {
            state: Mutex::default(),
            asks: watch::Sender::new(0),
            folder: watch::Sender::new(None),
            history: watch::Sender::new(UserHistory::None),
            clipboard_max,
            app_clipboard: std::sync::OnceLock::new(),
        }
    }

    /// Hands promises that have files to `app_clipboard`, which the app
    /// holds; without it they are promised without their files.
    pub(crate) fn set_app_clipboard(&self, app_clipboard: Arc<AppClipboard>) {
        let _ = self.app_clipboard.set(app_clipboard);
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Counts a connected broker for as long as the guard lives.
    fn connected(self: &Arc<Self>) -> Connected {
        self.state().brokers += 1;
        Connected(self.clone())
    }

    /// Where received files go unless the user chose elsewhere: the
    /// folder the connected broker named. `None` while none is connected.
    pub(crate) fn receive_folder(&self) -> Option<PathBuf> {
        self.folder.borrow().clone()
    }

    pub(crate) fn watch_receive_folder(&self) -> watch::Receiver<Option<PathBuf>> {
        self.folder.subscribe()
    }

    /// Keeps the pictures a broker sent in place of the last ones. Unless
    /// they all keep to the limits, none are kept.
    fn received(&self, sent: &iw_proto::service::v1::DesktopPictures) -> Result<(), Refused> {
        let accepted = previews::accept_pictures(
            sent.pictures
                .iter()
                .map(|p| (p.display_id.as_str(), p.jpeg.as_slice())),
        );
        let (pictures, result) = match accepted {
            Ok(pictures) => (pictures, Ok(())),
            Err(e) => (Pictures::new(), Err(e)),
        };
        let mut state = self.state();
        // The first ones move the generation even when there are none, so
        // the agent reads them, now known, at once. Refused ones are not
        // known pictures.
        let first = result.is_ok() && !state.pictures_sent;
        state.pictures_sent |= result.is_ok();
        if first || state.pictures != pictures {
            state.pictures = pictures;
            state.generation += 1;
        }
        result
    }

    /// Requests to read the pictures again, for the connected brokers.
    fn watch_asks(&self) -> watch::Receiver<u64> {
        self.asks.subscribe()
    }

    /// Makes a new connection the one that does the clipboard and files.
    fn link_up(
        &self,
        control: mpsc::UnboundedSender<Vec<AgentToBroker>>,
        files: mpsc::Sender<AgentToBroker>,
    ) -> Link {
        let mut state = self.state();
        state.next_link += 1;
        let link = Link {
            id: state.next_link,
            control,
            files,
            replies: Arc::default(),
            rendering: Arc::default(),
        };
        state.link = Some(link.clone());
        // Its user's history, once loaded; until then, nobody's.
        self.history.send_replace(UserHistory::Loading(link.id));
        let clipboard = &mut state.clipboard;
        clipboard.pending.clear();
        // What the last broker was asked may never come.
        if clipboard.sync && clipboard.snapshot.0 != clipboard.count {
            self.ask_clipboard(&link, clipboard);
        }
        link
    }

    /// A connection ended. If it was the one doing the work, there is no
    /// clipboard, no folder to receive into, and nobody's history, until
    /// another connects.
    fn link_down(&self, link: &Link) {
        let gone = {
            let mut state = self.state();
            if state.link.as_ref().is_some_and(|l| l.id == link.id) {
                state.link = None;
                state.clipboard.pending.clear();
                self.folder.send_replace(None);
                self.history.send_replace(UserHistory::None);
            }
            // Nobody asks for their content any more: the broker takes a
            // connection's promises off the clipboard when it ends, or
            // its window takes them along when the broker goes.
            let gone = state.promises.remove(link.id, |_| true);
            for (id, _) in &gone {
                // Whoever waits for its answer hears it failed.
                state.acks.remove(id);
            }
            gone
        };
        for (_, source) in gone {
            source.finished();
        }
        link.rendering.lock().unwrap().clear();
        // Whoever waits for an answer on it gets an error.
        let mut replies = link.replies.lock().unwrap();
        replies.closed = true;
        replies.waiting.clear();
        replies.saves.clear();
    }

    /// Whose transfer history it is, and what the broker loaded.
    pub(crate) fn watch_history(&self) -> watch::Receiver<UserHistory> {
        self.history.subscribe()
    }

    /// The history connection `link` loaded for its user, if it is the
    /// current one and was asked; it is asked once.
    fn history_loaded(&self, link: u64, loaded: HistoryLoaded) {
        let state = self.state();
        let asked = Self::is_current(&state, link)
            && matches!(*self.history.borrow(), UserHistory::Loading(l) if l == link);
        if !asked {
            debug!("ignoring a transfer history not asked for");
            return;
        }
        self.history.send_replace(UserHistory::Loaded {
            link,
            toml: Arc::new(loaded.toml),
            error: loaded.error,
        });
    }

    /// Has the broker on connection `link` save `toml` as its user's
    /// transfer history, and waits for it to say it did. Only while that
    /// connection is the current one and the history was loaded from it:
    /// otherwise, or once it ends, the history is not that user's to save
    /// and the error is `NotConnected`.
    pub(crate) async fn save_history(&self, link: u64, toml: Vec<u8>) -> io::Result<()> {
        let current = {
            let state = self.state();
            let loaded =
                matches!(*self.history.borrow(), UserHistory::Loaded { link: l, .. } if l == link);
            match &state.link {
                Some(current) if current.id == link && loaded => current.clone(),
                _ => return Err(not_connected()),
            }
        };
        let answer = {
            let mut replies = current.replies.lock().unwrap();
            if replies.closed {
                return Err(not_connected());
            }
            let save = says(agent_to_broker::Msg::SaveHistory(SaveHistory { toml }));
            current
                .control
                .send(vec![save])
                .map_err(|_| not_connected())?;
            let (tx, rx) = oneshot::channel();
            replies.saves.push_back(tx);
            rx
        };
        let error = tokio::time::timeout(HISTORY_ANSWER, answer)
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "the broker did not answer"))?
            .map_err(|_| not_connected())?;
        if error.is_empty() {
            Ok(())
        } else {
            Err(io::Error::other(error))
        }
    }

    fn is_current(state: &State, link: u64) -> bool {
        state.link.as_ref().is_some_and(|l| l.id == link)
    }

    /// Asks `link` to read the clipboard for the current count.
    fn ask_clipboard(&self, link: &Link, clipboard: &mut ClipboardCache) {
        let ask = says(agent_to_broker::Msg::ReadClipboard(ReadClipboard {
            max_bytes: self.clipboard_max as u64,
        }));
        if link.control.send(vec![ask]).is_ok() {
            clipboard
                .pending
                .push_back(PendingRead::Ahead(clipboard.count));
        }
    }

    /// The most the oldest read asked of `link` may bring, which its answer
    /// is held to.
    fn answering_max(&self, link: u64) -> usize {
        let state = self.state();
        match state.clipboard.pending.front() {
            Some(PendingRead::Waited { max, .. }) if Self::is_current(&state, link) => *max,
            _ => self.clipboard_max,
        }
    }

    /// Another app changed the clipboard: count it, and read it while sync
    /// is on.
    fn clipboard_changed(&self, link: u64, sequence: u64) {
        let mut state = self.state();
        if !Self::is_current(&state, link) {
            return;
        }
        // A broker that restarts reports the clipboard again.
        if state.clipboard.sequence == Some(sequence) {
            return;
        }
        let current = state.link.clone();
        let clipboard = &mut state.clipboard;
        clipboard.sequence = Some(sequence);
        clipboard.count += 1;
        clipboard.large = None;
        if clipboard.sync
            && let Some(current) = current
        {
            self.ask_clipboard(&current, clipboard);
        }
    }

    /// The answer to the oldest read asked of `link`.
    fn clipboard_read(&self, link: u64, read: Snapshot) {
        let mut state = self.state();
        if !Self::is_current(&state, link) {
            return;
        }
        let clipboard = &mut state.clipboard;
        match clipboard.pending.pop_front() {
            Some(PendingRead::Ahead(count)) => clipboard.snapshot = (count, read),
            Some(PendingRead::Waited { answer, .. }) => {
                let _ = answer.send(read);
            }
            None => {}
        }
    }

    /// The broker asks for format `ask.mime` of a promise, which an app is
    /// pasting: its source pulls it, off the runtime, and the answer goes
    /// back on the connection. Nothing comes back for an unknown promise
    /// or format, or beyond what the broker takes; `Busy` while that
    /// promise is still being fetched for an earlier request. The agent
    /// stops waiting, and sends nothing, when the broker gives up
    /// (`RenderCancelled`) or after [`RENDER_WAIT`]; the promise stays busy
    /// until its fetch ends.
    fn render(&self, link: &Link, ask: RenderClipboard) {
        let source = self.state().promises.get(link.id, ask.promise_id);
        let format = ClipboardFormat::from_mime(&ask.mime);
        let (Some(source), Some(format)) = (source, format) else {
            debug!(promise = ask.promise_id, mime = %ask.mime, "the broker asks for no promise held");
            let _ = link.control.send(rendered(&ask, Answer::Unavailable));
            return;
        };
        let cancelled = {
            let mut rendering = link.rendering.lock().unwrap();
            if rendering.contains_key(&ask.promise_id) {
                drop(rendering);
                debug!(
                    promise = ask.promise_id,
                    "the broker asks for a promise still being fetched"
                );
                let _ = link.control.send(rendered(&ask, Answer::Busy));
                return;
            }
            let (cancel, cancelled) = oneshot::channel();
            rendering.insert(ask.promise_id, Some(cancel));
            cancelled
        };
        let link = link.clone();
        tokio::spawn(async move {
            let mut fetching = tokio::task::spawn_blocking(move || source.fetch(format));
            // A cancel that came wins over content fetched meanwhile.
            let fetched = tokio::select! {
                biased;
                _ = cancelled => None,
                data = &mut fetching => Some(data.ok().flatten()),
                () = tokio::time::sleep(RENDER_WAIT) => None,
            };
            let Some(data) = fetched else {
                debug!(
                    promise = ask.promise_id,
                    "the broker stopped waiting for a promised format"
                );
                let _ = fetching.await;
                link.rendering.lock().unwrap().remove(&ask.promise_id);
                return;
            };
            // More would end the broker's connection.
            let answer = match data {
                Some(data) if data.len() as u64 <= MAX_CLIPBOARD => Answer::Content(data),
                Some(data) => {
                    debug!(
                        bytes = data.len(),
                        "promised content larger than the broker takes"
                    );
                    Answer::Unavailable
                }
                None => Answer::Unavailable,
            };
            link.rendering.lock().unwrap().remove(&ask.promise_id);
            let _ = link.control.send(rendered(&ask, answer));
        });
    }

    /// The broker stopped waiting for promise `id`: nothing is sent for it.
    fn render_cancelled(&self, link: &Link, id: u64) {
        if let Some(cancel) = link
            .rendering
            .lock()
            .unwrap()
            .get_mut(&id)
            .and_then(Option::take)
        {
            let _ = cancel.send(());
        }
    }

    /// The broker says whether it put promise `id` on the clipboard.
    fn promised(&self, link: u64, id: u64, ok: bool) {
        let mut state = self.state();
        if state.promises.get(link, id).is_none() {
            return;
        }
        if let Some(waiting) = state.acks.remove(&id) {
            let _ = waiting.send(ok);
        }
    }

    /// The clipboard no longer holds promise `id`.
    fn promise_gone(&self, link: u64, id: u64) {
        let gone = self.state().promises.remove(link, |i| i == id);
        for (_, source) in gone {
            source.finished();
        }
    }

    fn set_folder(&self, link: u64, path: &str) {
        let state = self.state();
        if !Self::is_current(&state, link) {
            return;
        }
        let path = PathBuf::from(path);
        if path.is_absolute() {
            self.folder.send_replace(Some(path));
        } else {
            warn!("ignoring a receive folder that is not an absolute path");
        }
    }

    /// The connection that does the files work, and a new handle on it.
    fn new_handle(&self) -> Option<(Link, u64)> {
        let mut state = self.state();
        let link = state.link.clone()?;
        state.next_handle += 1;
        Some((link, state.next_handle))
    }

    /// Asks the connected broker to start the app as the user for `need`,
    /// unless it runs already. False if no broker is connected. For files
    /// on the clipboard the broker refuses while the user quit the app.
    pub(crate) fn launch_app(&self, need: Need) -> bool {
        let state = self.state();
        let reason = match need {
            Need::ClipboardFiles => LaunchReason::ClipboardFiles,
            // Started whether or not the user quit it, as on macOS and
            // Linux; it opens no window for it.
            Need::PairingCode | Need::FileOffer | Need::License => LaunchReason::AskUser,
        };
        let launch = says(agent_to_broker::Msg::LaunchApp(LaunchApp {
            reason: reason.into(),
        }));
        state
            .link
            .as_ref()
            .is_some_and(|link| link.control.send(vec![launch]).is_ok())
    }

    /// Has the connected broker open the part file for a transfer, in
    /// `folder`. Returns it with the bytes it already holds and the free
    /// space there.
    pub(crate) async fn open_file(
        &self,
        transfer_id: &str,
        size: u64,
        folder: &Path,
    ) -> io::Result<(BrokerFile, u64, Option<u64>)> {
        let (link, handle) = self.new_handle().ok_or_else(not_connected)?;
        let mut file = BrokerFile { link, handle };
        let open = says(agent_to_broker::Msg::OpenFile(OpenFile {
            handle,
            transfer_id: transfer_id.to_owned(),
            size,
            folder: folder.to_string_lossy().into_owned(),
        }));
        match file.ask(open).await? {
            broker_message::Msg::FileOpened(opened) => {
                let free = (opened.free_bytes != u64::MAX).then_some(opened.free_bytes);
                Ok((file, opened.offset, free))
            }
            _ => Err(io::Error::other("the broker answered something else")),
        }
    }

    /// Has the connected broker open `path`, which the user sends, as the
    /// user. Returns it with its size.
    pub(crate) async fn open_for_sending(
        &self,
        transfer_id: &str,
        path: &Path,
    ) -> Result<(BrokerSource, u64), OpenError> {
        let (link, handle) = self.new_handle().ok_or(OpenError::NoBroker)?;
        let open = says(agent_to_broker::Msg::OpenForSending(OpenForSending {
            handle,
            transfer_id: transfer_id.to_owned(),
            path: path.to_string_lossy().into_owned(),
        }));
        let answer = link.ask(handle, open, false, Some(OPEN_ANSWER)).await;
        let answer = answer.map_err(|e| {
            link.forget(handle);
            // In case it opens after all.
            let close = says(agent_to_broker::Msg::CloseSending(CloseSending { handle }));
            let _ = link.control.send(vec![close]);
            match e.kind() {
                io::ErrorKind::NotConnected => OpenError::NoBroker,
                _ => OpenError::Unreadable(e.to_string()),
            }
        })?;
        match answer {
            broker_message::Msg::OpenedForSending(opened) => {
                let source = BrokerSource {
                    link,
                    handle,
                    ahead: (0, Vec::new()),
                };
                Ok((source, opened.size))
            }
            broker_message::Msg::OpenFailed(failed) => Err(match failed.reason() {
                OpenFailure::NotAFile => OpenError::NotAFile(failed.error),
                OpenFailure::AccessDenied | OpenFailure::Unspecified => {
                    OpenError::Unreadable(failed.error)
                }
            }),
            _ => Err(OpenError::Unreadable(
                "the broker answered something else".to_owned(),
            )),
        }
    }

    /// Sends `msg`, made with a new handle, and waits up to
    /// [`OPEN_ANSWER`] for the broker's answer.
    async fn ask(&self, msg: impl FnOnce(u64) -> AgentToBroker) -> io::Result<broker_message::Msg> {
        let (link, handle) = self.new_handle().ok_or_else(not_connected)?;
        let answer = link
            .ask(handle, msg(handle), false, Some(OPEN_ANSWER))
            .await;
        link.forget(handle);
        answer
    }

    /// The path in `answer`, a FolderReady.
    fn folder_ready(answer: broker_message::Msg) -> io::Result<PathBuf> {
        match answer {
            broker_message::Msg::FolderReady(ready) => Ok(PathBuf::from(ready.path)),
            _ => Err(io::Error::other("the broker answered something else")),
        }
    }

    /// Has the broker create the folder `name` in `folder`, or `name (1)`…
    /// when taken, as the user; returns where it is.
    pub(crate) async fn claim_folder(&self, folder: &Path, name: &str) -> io::Result<PathBuf> {
        let answer = self
            .ask(|handle| {
                says(agent_to_broker::Msg::ClaimFolder(ClaimFolder {
                    handle,
                    folder: folder.to_string_lossy().into_owned(),
                    name: name.to_owned(),
                }))
            })
            .await?;
        Self::folder_ready(answer)
    }

    /// Has the broker create the folders `subfolder` ("/"-separated) in
    /// `folder`, as the user; returns where the last one is.
    pub(crate) async fn make_folder(&self, folder: &Path, subfolder: &str) -> io::Result<PathBuf> {
        let answer = self
            .ask(|handle| {
                says(agent_to_broker::Msg::MakeFolder(MakeFolder {
                    handle,
                    folder: folder.to_string_lossy().into_owned(),
                    subfolder: subfolder.to_owned(),
                }))
            })
            .await?;
        Self::folder_ready(answer)
    }

    /// Has the broker check that the user can create files in `folder`.
    pub(crate) async fn check_folder(&self, folder: &Path) -> io::Result<PathBuf> {
        let answer = self
            .ask(|handle| {
                says(agent_to_broker::Msg::CheckFolder(CheckFolder {
                    handle,
                    folder: folder.to_string_lossy().into_owned(),
                }))
            })
            .await?;
        Self::folder_ready(answer)
    }

    /// Has the broker list `path` as the user, with at most `max` entries.
    async fn list_path(&self, path: &Path, max: usize) -> io::Result<Listing> {
        let answer = self
            .ask(|handle| {
                says(agent_to_broker::Msg::ListPath(ListPath {
                    handle,
                    path: path.to_string_lossy().into_owned(),
                    max_entries: u32::try_from(max).unwrap_or(u32::MAX),
                }))
            })
            .await?;
        let broker_message::Msg::PathListed(listed) = answer else {
            return Err(io::Error::other("the broker answered something else"));
        };
        // More than asked for breaks the protocol as much as a lie would.
        if listed.entries.len() > max {
            return Err(io::Error::other("the broker listed more than asked for"));
        }
        let kind = |kind: PathKind, size: u64| match kind {
            PathKind::File => Kind::File(size),
            PathKind::Folder => Kind::Folder,
            PathKind::Link => Kind::Link,
            PathKind::Other => Kind::Other,
            PathKind::Missing => Kind::Missing,
            PathKind::Unreadable | PathKind::Unspecified => Kind::Unreadable,
        };
        Ok(Listing {
            kind: Some(kind(listed.kind(), listed.size)),
            entries: listed
                .entries
                .iter()
                .map(|e| (e.name.clone(), kind(e.kind(), e.size)))
                .collect(),
            truncated: listed.truncated,
            error: listed.error,
        })
    }
}

/// Lists what the user sends as the user, through the broker.
#[derive(Clone)]
pub(crate) struct BrokerLister(pub(crate) Arc<Broker>);

impl Lister for BrokerLister {
    async fn list(&self, path: &Path, max: usize) -> io::Result<Listing> {
        self.0.list_path(path, max).await
    }
}

impl DesktopPictures for Broker {
    /// The pictures last sent for `displays`; also asks the broker to read
    /// them again, as the agent calls this when the displays change and
    /// every so often. What it sends then comes with the next call.
    fn pictures(&self, displays: &[Display]) -> Result<HashMap<String, Vec<u8>>, PlatformError> {
        let (connected, shown) = {
            let state = self.state();
            let shown = displays
                .iter()
                .filter_map(|d| Some((d.id.clone(), state.pictures.get(&d.id)?.bytes().to_vec())))
                .collect();
            (state.brokers > 0, shown)
        };
        // Nobody listens while no broker is connected; one that connects
        // sends the pictures by itself.
        if connected {
            self.asks.send_modify(|n| *n = n.wrapping_add(1));
        }
        Ok(shown)
    }

    fn available(&self) -> bool {
        self.state().brokers > 0
    }

    fn generation(&self) -> u64 {
        self.state().generation
    }

    /// Not until a broker sent the pictures, as while no user is logged on
    /// or the shell is not ready to tell the broker: other devices keep
    /// the ones they have of this PC.
    fn known(&self) -> bool {
        self.state().pictures_sent
    }
}

fn not_connected() -> io::Error {
    io::Error::new(io::ErrorKind::NotConnected, "no broker is connected")
}

/// What a read larger than read ahead already found for `count`, as a
/// read of at most `max` bytes would find it: `None` to read again, when
/// nothing was read for it or the last read took less than `max`.
fn known_large(
    large: Option<&(u64, ClipboardRead, usize)>,
    count: u64,
    max: usize,
) -> Option<ClipboardRead> {
    let (_, read, limit) = large.filter(|(at, _, _)| *at == count)?;
    match read {
        ClipboardRead::Content(content) if content.len() > max => {
            Some(ClipboardRead::TooLarge(content.len()))
        }
        ClipboardRead::TooLarge(_) if *limit < max => None,
        read => Some(read.clone()),
    }
}

const NO_BROKER: PlatformError =
    PlatformError::Unavailable("no user is logged on for the broker to reach the clipboard");

/// This device's clipboard, through the broker.
pub(crate) struct BrokerClipboard(pub(crate) Arc<Broker>);

impl Clipboard for BrokerClipboard {
    fn change_count(&self) -> Result<u64, PlatformError> {
        let state = self.0.state();
        if state.link.is_none() {
            return Err(NO_BROKER);
        }
        Ok(state.clipboard.count)
    }

    fn read(&self, max_len: usize) -> Result<ClipboardRead, PlatformError> {
        let mut state = self.0.state();
        let Some(link) = state.link.clone() else {
            return Err(NO_BROKER);
        };
        let clipboard = &mut state.clipboard;
        if clipboard.snapshot.0 != clipboard.count {
            return Err(PlatformError::Unavailable(
                "the broker has not sent the clipboard yet",
            ));
        }
        match &clipboard.snapshot.1 {
            Snapshot::Read(ClipboardRead::Content(content), _) if content.len() > max_len => {
                Ok(ClipboardRead::TooLarge(content.len()))
            }
            // Larger than read ahead, and more is wanted: read it now, up
            // to what the broker reads at once.
            Snapshot::Read(ClipboardRead::TooLarge(_), _) if max_len > self.0.clipboard_max => {
                let max = max_len.min(MAX_CLIPBOARD as usize);
                let count = clipboard.count;
                if let Some(known) = known_large(clipboard.large.as_ref(), count, max) {
                    return Ok(known);
                }
                let (answer, answered) = std::sync::mpsc::sync_channel(1);
                let ask = says(agent_to_broker::Msg::ReadClipboard(ReadClipboard {
                    max_bytes: max as u64,
                }));
                link.control.send(vec![ask]).map_err(|_| NO_BROKER)?;
                clipboard
                    .pending
                    .push_back(PendingRead::Waited { max, answer });
                drop(state);
                match answered.recv_timeout(LARGE_READ_ANSWER) {
                    Ok(Snapshot::Read(read, _)) => {
                        // Kept for the same clipboard, read again for a
                        // pull or the next move.
                        let mut state = self.0.state();
                        if state.clipboard.count == count {
                            state.clipboard.large = Some((count, read.clone(), max));
                        }
                        Ok(read)
                    }
                    _ => Err(PlatformError::Unavailable(
                        "the broker could not read the clipboard",
                    )),
                }
            }
            Snapshot::Read(read, _) => Ok(read.clone()),
            Snapshot::Unavailable => {
                // Another app held the clipboard; try again for the next
                // time the cursor leaves.
                if !clipboard.reading_ahead(clipboard.count) {
                    self.0.ask_clipboard(&link, clipboard);
                }
                Err(PlatformError::Unavailable(
                    "the broker could not read the clipboard",
                ))
            }
        }
    }

    /// The files copied in File Explorer that read ahead found for the
    /// clipboard as it is now: paths the broker lists and reads as the
    /// user when they are sent, and this process never opens.
    fn files(&self) -> Result<Vec<PathBuf>, PlatformError> {
        let state = self.0.state();
        if state.link.is_none() {
            return Err(NO_BROKER);
        }
        let clipboard = &state.clipboard;
        match &clipboard.snapshot {
            (count, Snapshot::Read(_, files)) if *count == clipboard.count => Ok(files.clone()),
            _ => Err(PlatformError::Unavailable(
                "the broker has not sent the clipboard yet",
            )),
        }
    }

    fn write(&mut self, content: &ClipboardContent) -> Result<u64, PlatformError> {
        let state = self.0.state();
        let link = state.link.as_ref().ok_or(NO_BROKER)?;
        let header = says(agent_to_broker::Msg::WriteClipboard(WriteClipboard {
            parts: content
                .items
                .iter()
                .map(|(format, data)| ClipboardPart {
                    mime: format.mime().to_owned(),
                    size: data.len() as u64,
                })
                .collect(),
        }));
        let group = in_pieces(header, content.items.iter().map(|(_, d)| d.as_slice()));
        link.control.send(group).map_err(|_| NO_BROKER)?;
        // The broker does not report its own write as a change.
        Ok(state.clipboard.count)
    }

    /// Has the broker promise the content on the clipboard; it asks for
    /// each format as an app pastes it (`Broker::render`). Waits for the
    /// broker to say it did, for at most [`PROMISE_ANSWER`]: until then the
    /// content has not moved.
    ///
    /// A promise with files is handed to the app instead (`hand_over`);
    /// one here, when no app takes it, is promised without its files, and
    /// nothing at all without content, letting its source go.
    fn write_promise(&mut self, mut promise: ClipboardPromise) -> Result<u64, PlatformError> {
        if !promise.files.is_empty() {
            self.0.state().link.as_ref().ok_or(NO_BROKER)?;
            promise.files.clear();
            if promise.formats.is_empty() {
                promise.source.finished();
                return Err(PlatformError::Unavailable(
                    "no app takes files on the clipboard",
                ));
            }
        }
        let (ack, acked) = std::sync::mpsc::sync_channel(1);
        let (sent, gone) = {
            let mut state = self.0.state();
            let link = state.link.clone().ok_or(NO_BROKER)?;
            let (id, gone) = state.promises.add(link.id, promise.source);
            let ask = says(agent_to_broker::Msg::PromiseClipboard(PromiseClipboard {
                promise_id: id,
                mimes: promise
                    .formats
                    .iter()
                    .map(|f| f.mime().to_owned())
                    .collect(),
                size: promise.bytes,
            }));
            let sent = if link.control.send(vec![ask]).is_ok() {
                state.acks.insert(id, ack);
                // The broker does not report it as a change either.
                Some((link, id, state.clipboard.count))
            } else {
                state.promises.remove(link.id, |i| i == id);
                None
            };
            (sent, gone)
        };
        // Let go of those that made room, whatever happens to this one.
        for source in gone {
            source.finished();
        }
        let (link, id, count) = sent.ok_or(NO_BROKER)?;
        let answer = match acked.recv_timeout(promise_answer()) {
            Ok(ok) => Some(ok),
            Err(_) => {
                // The answer is handed over under the same lock: either it
                // came just now, or the agent gives up and withdraws it.
                let mut state = self.0.state();
                let waited = state.acks.remove(&id).is_some();
                let answer = if waited { None } else { acked.try_recv().ok() };
                if answer.is_none() {
                    state.promises.remove(link.id, |i| i == id);
                    let withdraw = says(agent_to_broker::Msg::WithdrawPromise(WithdrawPromise {
                        promise_id: id,
                    }));
                    let _ = link.control.send(vec![withdraw]);
                }
                answer
            }
        };
        match answer {
            Some(true) => Ok(count),
            // The broker could not, as when another app keeps the
            // clipboard open: the engine pulls the content and writes it
            // instead, which checks the clipboard did not change meanwhile.
            Some(false) => {
                self.0.state().promises.remove(link.id, |i| i == id);
                Err(PlatformError::Unsupported(
                    "the broker could not put the promise on the clipboard",
                ))
            }
            // No answer: the broker may be stuck, and a write would be too.
            None => Err(PlatformError::Unavailable(
                "the broker did not answer the promise",
            )),
        }
    }

    /// Hands a promise that has files to the app (`AppClipboard`), whose
    /// data object File Explorer pastes them from, and waits for it on a
    /// thread of its own, never the clipboard's. The broker does not report
    /// the app's object as a change: it carries the format of InputWeave's
    /// own writes. The app may put it there only while nothing was copied
    /// here or written by the agent (`current`) since.
    fn hand_over(
        &mut self,
        promise: ClipboardPromise,
        current: Current,
        done: HandedOver,
    ) -> Result<(), ClipboardPromise> {
        let Some(app) = self.0.app_clipboard.get().cloned() else {
            return Err(promise);
        };
        let count = {
            let state = self.0.state();
            if state.link.is_none() {
                return Err(promise);
            }
            state.clipboard.count
        };
        let unchanged: Unchanged = {
            let broker = self.0.clone();
            Arc::new(move || broker.state().clipboard.count == count && current())
        };
        // Taken back if no thread can wait.
        let handed = Arc::new(Mutex::new(Some((promise, done))));
        let waiting = handed.clone();
        let spawned = std::thread::Builder::new()
            .name("clipboard app".to_owned())
            .spawn(move || {
                let Some((promise, done)) =
                    waiting.lock().unwrap_or_else(|e| e.into_inner()).take()
                else {
                    return;
                };
                match app.promise(&promise, unchanged) {
                    Ok(()) => done(HandOver::Held),
                    Err(e) => {
                        debug!(error = %e, "the app does not hold the files on the clipboard");
                        done(HandOver::Returned(promise));
                    }
                }
            });
        match spawned {
            Ok(_) => Ok(()),
            Err(e) => {
                warn!(error = %e, "cannot wait for the app to put files on the clipboard");
                // The thread never ran, so the promise is still here.
                match handed.lock().unwrap_or_else(|e| e.into_inner()).take() {
                    Some((promise, _)) => Err(promise),
                    None => Ok(()),
                }
            }
        }
    }

    /// Through the app, when there is one to hold them (`hand_over`).
    fn promises_files(&self) -> bool {
        self.0.app_clipboard.get().is_some()
    }

    /// The app chooses the folder pasted files are fetched into, one of
    /// the user's that the agent never writes itself.
    fn stages_files(&self) -> bool {
        self.0.app_clipboard.get().is_some()
    }

    /// The broker takes no more at once; more would end its connection.
    fn max_write(&self) -> Option<usize> {
        usize::try_from(iw_proto::service::MAX_CLIPBOARD).ok()
    }

    fn set_sync(&mut self, on: bool) {
        let mut state = self.0.state();
        let link = state.link.clone();
        let clipboard = &mut state.clipboard;
        clipboard.sync = on;
        let unread =
            clipboard.snapshot.0 != clipboard.count && !clipboard.reading_ahead(clipboard.count);
        if on
            && unread
            && let Some(link) = link
        {
            self.0.ask_clipboard(&link, clipboard);
        }
    }
}

/// A part file the broker writes for a transfer.
pub(crate) struct BrokerFile {
    link: Link,
    handle: u64,
}

impl BrokerFile {
    /// Sends `msg` and waits for the broker's answer about this file.
    async fn ask(&mut self, msg: AgentToBroker) -> io::Result<broker_message::Msg> {
        self.link
            .ask(self.handle, msg, true, Some(FILE_ANSWER))
            .await
    }

    pub(crate) async fn write(&mut self, data: &[u8]) -> io::Result<()> {
        if self
            .link
            .replies
            .lock()
            .unwrap()
            .failed
            .contains(&self.handle)
        {
            return Err(io::Error::other("the broker could not write the file"));
        }
        let data = says(agent_to_broker::Msg::FileData(FileData {
            handle: self.handle,
            data: data.to_vec(),
        }));
        self.link
            .files
            .send(data)
            .await
            .map_err(|_| not_connected())
    }

    /// Has the broker write the file out and close it; returns its size and
    /// BLAKE3 hash.
    pub(crate) async fn finish(&mut self) -> io::Result<(u64, [u8; 32])> {
        let finish = says(agent_to_broker::Msg::FinishFile(FinishFile {
            handle: self.handle,
        }));
        match self.ask(finish).await? {
            broker_message::Msg::FileFinished(finished) => {
                let hash = <[u8; 32]>::try_from(finished.blake3.as_slice())
                    .map_err(|_| io::Error::other("the broker sent a malformed hash"))?;
                Ok((finished.size, hash))
            }
            _ => Err(io::Error::other("the broker answered something else")),
        }
    }

    /// Has the broker give the finished file `name` in the folders
    /// `subfolder` ("/"-separated, possibly none) of its folder, numbered
    /// if taken; returns where it is.
    pub(crate) async fn keep(mut self, name: &str, subfolder: &str) -> io::Result<PathBuf> {
        let keep = says(agent_to_broker::Msg::KeepFile(KeepFile {
            handle: self.handle,
            name: name.to_owned(),
            subfolder: subfolder.to_owned(),
        }));
        match self.ask(keep).await? {
            broker_message::Msg::FileKept(kept) => Ok(PathBuf::from(kept.path)),
            _ => Err(io::Error::other("the broker answered something else")),
        }
    }

    /// Has the broker delete the part file.
    pub(crate) async fn discard(self) {
        let discard = says(agent_to_broker::Msg::DiscardFile(DiscardFile {
            handle: self.handle,
        }));
        let _ = self.link.files.send(discard).await;
    }

    /// Has the broker close the part file and keep it, to resume.
    pub(crate) async fn close(self) {
        let close = says(agent_to_broker::Msg::CloseFile(CloseFile {
            handle: self.handle,
        }));
        let _ = self.link.files.send(close).await;
    }
}

impl Drop for BrokerFile {
    fn drop(&mut self) {
        self.link.forget(self.handle);
    }
}

/// A file the user sends, which the broker opened as the user. Dropping it
/// has the broker close it.
pub(crate) struct BrokerSource {
    link: Link,
    handle: u64,
    /// What the broker last read, from the offset given: it reads larger
    /// pieces than a transfer sends at once.
    ahead: (u64, Vec<u8>),
}

impl BrokerSource {
    /// Whether the broker that opened it went away, so that it must be
    /// opened again.
    pub(crate) fn is_lost(&self) -> bool {
        self.link.is_down()
    }

    /// Has the broker read the whole file and hash it; returns the size it
    /// read and the BLAKE3 hash. Large files take a while, so there is no
    /// time limit; the connection ending ends the wait.
    pub(crate) async fn hash(&mut self) -> io::Result<(u64, [u8; 32])> {
        let hash = says(agent_to_broker::Msg::HashFile(HashFile {
            handle: self.handle,
        }));
        match self.link.ask(self.handle, hash, false, None).await? {
            broker_message::Msg::FileHashed(hashed) => {
                let hash = <[u8; 32]>::try_from(hashed.blake3.as_slice())
                    .map_err(|_| io::Error::other("the broker sent a malformed hash"))?;
                Ok((hashed.size, hash))
            }
            _ => Err(io::Error::other("the broker answered something else")),
        }
    }

    /// Up to `max` bytes from `offset`; none at the end of the file.
    pub(crate) async fn read(&mut self, offset: u64, max: usize) -> io::Result<Vec<u8>> {
        let (start, data) = &self.ahead;
        let held = offset
            .checked_sub(*start)
            .and_then(|skip| usize::try_from(skip).ok())
            .filter(|&skip| skip < data.len());
        if let Some(skip) = held {
            let end = data.len().min(skip + max);
            return Ok(data[skip..end].to_vec());
        }
        let read = says(agent_to_broker::Msg::ReadChunk(ReadChunk {
            handle: self.handle,
            offset,
            len: PIECE as u32,
        }));
        let chunk = match self
            .link
            .ask(self.handle, read, false, Some(FILE_ANSWER))
            .await?
        {
            broker_message::Msg::ChunkData(chunk) => chunk,
            _ => return Err(io::Error::other("the broker answered something else")),
        };
        if chunk.offset != offset || chunk.data.len() > PIECE {
            return Err(io::Error::other("the broker read something else"));
        }
        let end = chunk.data.len().min(max);
        let piece = chunk.data[..end].to_vec();
        self.ahead = (offset, chunk.data);
        Ok(piece)
    }
}

impl Drop for BrokerSource {
    fn drop(&mut self) {
        self.link.forget(self.handle);
        let close = says(agent_to_broker::Msg::CloseSending(CloseSending {
            handle: self.handle,
        }));
        let _ = self.link.control.send(vec![close]);
    }
}

/// A broker counted as connected.
struct Connected(Arc<Broker>);

impl Drop for Connected {
    fn drop(&mut self) {
        self.0.state().brokers -= 1;
    }
}

/// Serves brokers on `listener`.
pub(crate) async fn serve(listener: IpcListener, broker: Arc<Broker>) {
    let mut brokers = JoinSet::new();
    loop {
        match listener.accept().await {
            Ok(conn) => {
                brokers.spawn(connection(conn, broker.clone()));
            }
            Err(e) => {
                warn!(error = %e, "accepting the broker failed");
                tokio::time::sleep(ACCEPT_RETRY).await;
            }
        }
        while brokers.try_join_next().is_some() {}
    }
}

#[derive(Debug, thiserror::Error)]
enum LinkError {
    #[error(transparent)]
    Frame(#[from] iw_ipc::FrameError),
    #[error("the broker broke the protocol: {0}")]
    Protocol(&'static str),
}

async fn connection(conn: IpcConnection, broker: Arc<Broker>) {
    let _connected = broker.connected();
    info!("the broker connected");
    let (mut reader, mut writer) = conn.into_split();
    let (control, mut control_rx) = mpsc::unbounded_channel::<Vec<AgentToBroker>>();
    let (files, mut files_rx) = mpsc::channel::<AgentToBroker>(FILE_QUEUE);
    let link = broker.link_up(control, files);
    // The user's transfer history, of which the agent keeps no copy.
    let load = says(agent_to_broker::Msg::LoadHistory(LoadHistory {}));
    let _ = link.control.send(vec![load]);
    let mut asks = broker.watch_asks();
    asks.mark_unchanged();
    let ask = says(agent_to_broker::Msg::RequestDesktopPictures(
        RequestDesktopPictures {},
    ));
    let writing = async {
        loop {
            tokio::select! {
                biased;
                Some(group) = control_rx.recv() => {
                    for msg in &group {
                        writer.send(msg).await?;
                    }
                }
                Some(msg) = files_rx.recv() => writer.send(&msg).await?,
                changed = asks.changed() => {
                    if changed.is_err() {
                        return Ok::<(), iw_ipc::FrameError>(());
                    }
                    writer.send(&ask).await?;
                }
            }
        }
    };
    let reading = async {
        while let Some(msg) = reader.recv::<BrokerMessage>().await? {
            on_message(&broker, &link, msg, &mut reader).await?;
        }
        Ok::<(), LinkError>(())
    };
    let ended: Result<(), LinkError> = tokio::select! {
        written = writing => written.map_err(LinkError::from),
        read = reading => read,
    };
    broker.link_down(&link);
    match ended {
        Ok(()) => info!("the broker disconnected"),
        Err(e) => warn!(error = %e, "dropping the broker's connection"),
    }
}

async fn on_message(
    broker: &Broker,
    link: &Link,
    msg: BrokerMessage,
    reader: &mut IpcReader,
) -> Result<(), LinkError> {
    use broker_message::Msg;

    // Anything unknown is something a newer broker sends.
    let Some(msg) = msg.msg else {
        return Ok(());
    };
    match msg {
        Msg::DesktopPictures(sent) => {
            if let Err(e) = broker.received(&sent) {
                warn!(error = %e, "dropping the desktop pictures the broker sent");
            }
        }
        Msg::ClipboardChanged(changed) => broker.clipboard_changed(link.id, changed.sequence),
        Msg::ClipboardContents(header) => {
            let max = broker.answering_max(link.id);
            let read = read_clipboard(reader, header, max).await?;
            broker.clipboard_read(link.id, read);
        }
        Msg::ClipboardData(_) => {
            return Err(LinkError::Protocol("clipboard data without a header"));
        }
        Msg::RenderClipboard(ask) => broker.render(link, ask),
        Msg::RenderCancelled(cancelled) => broker.render_cancelled(link, cancelled.promise_id),
        Msg::ClipboardPromised(promised) => {
            broker.promised(link.id, promised.promise_id, promised.ok);
        }
        Msg::PromiseGone(gone) => broker.promise_gone(link.id, gone.promise_id),
        Msg::ReceiveFolder(folder) => broker.set_folder(link.id, &folder.path),
        Msg::FileOpened(opened) => reply(link, opened.handle, Msg::FileOpened(opened)),
        Msg::FileFinished(finished) => reply(link, finished.handle, Msg::FileFinished(finished)),
        Msg::FileKept(kept) => reply(link, kept.handle, Msg::FileKept(kept)),
        Msg::OpenedForSending(opened) => {
            reply(link, opened.handle, Msg::OpenedForSending(opened));
        }
        Msg::OpenFailed(failed) => reply(link, failed.handle, Msg::OpenFailed(failed)),
        Msg::FileHashed(hashed) => reply(link, hashed.handle, Msg::FileHashed(hashed)),
        Msg::ChunkData(chunk) => reply(link, chunk.handle, Msg::ChunkData(chunk)),
        Msg::PathListed(listed) => reply(link, listed.handle, Msg::PathListed(listed)),
        Msg::FolderReady(ready) => reply(link, ready.handle, Msg::FolderReady(ready)),
        Msg::AppLaunched(launched) => {
            if !launched.ok {
                warn!(error = %launched.error, "the broker cannot start the app");
                if let Some(app_clipboard) = broker.app_clipboard.get() {
                    app_clipboard.start_failed();
                }
            } else if launched.started {
                info!("the broker started the app");
            } else {
                debug!("the app runs already");
            }
        }
        Msg::HistoryLoaded(loaded) => {
            if loaded.toml.len() > history::MAX_BYTES {
                return Err(LinkError::Protocol(
                    "a transfer history larger than allowed",
                ));
            }
            broker.history_loaded(link.id, loaded);
        }
        Msg::HistorySaved(saved) => {
            let waiting = link.replies.lock().unwrap().saves.pop_front();
            match waiting {
                Some(waiting) => {
                    let _ = waiting.send(saved.error);
                }
                None => debug!("the broker saved a transfer history not asked for"),
            }
        }
        Msg::FileFailed(failed) => {
            debug!(handle = failed.handle, error = %failed.error, "the broker failed with a file");
            let mut replies = link.replies.lock().unwrap();
            match replies.waiting.remove(&failed.handle) {
                Some(waiting) => {
                    let _ = waiting.send(Err(failed.error));
                }
                None => {
                    replies.failed.insert(failed.handle);
                }
            }
        }
    }
    Ok(())
}

/// Passes the broker's answer about a file to whoever waits for it.
fn reply(link: &Link, handle: u64, answer: broker_message::Msg) {
    if let Some(waiting) = link.replies.lock().unwrap().waiting.remove(&handle) {
        let _ = waiting.send(Ok(answer));
    }
}

/// The clipboard a ClipboardContents announces, with its content from the
/// ClipboardData that follow, and the files copied in File Explorer it
/// names. More than the agent asked for, pieces that do not add up, or
/// more files than the protocol allows break the protocol.
async fn read_clipboard(
    reader: &mut IpcReader,
    header: ClipboardContents,
    max: usize,
) -> Result<Snapshot, LinkError> {
    let files = copied_files(&header.files)?;
    let read = match header.state() {
        ClipboardState::Content => {
            if header.parts.len() > ClipboardFormat::ALL.len() {
                return Err(LinkError::Protocol("too many clipboard parts"));
            }
            let total = header
                .parts
                .iter()
                .try_fold(0u64, |sum, p| sum.checked_add(p.size));
            if !total.is_some_and(|total| total <= max as u64) {
                return Err(LinkError::Protocol("a clipboard larger than asked for"));
            }
            let mut content = ClipboardContent::default();
            for part in &header.parts {
                let format = ClipboardFormat::from_mime(&part.mime)
                    .ok_or(LinkError::Protocol("unknown clipboard format"))?;
                let size = part.size as usize;
                let mut data = Vec::with_capacity(size);
                while data.len() < size {
                    let piece = match reader.recv::<BrokerMessage>().await? {
                        Some(BrokerMessage {
                            msg: Some(broker_message::Msg::ClipboardData(piece)),
                        }) => piece.data,
                        _ => return Err(LinkError::Protocol("clipboard ended early")),
                    };
                    if data.len() + piece.len() > size {
                        return Err(LinkError::Protocol("clipboard part longer than announced"));
                    }
                    data.extend_from_slice(&piece);
                }
                content.items.push((format, data));
            }
            if content.is_empty() {
                ClipboardRead::Empty
            } else {
                ClipboardRead::Content(content)
            }
        }
        ClipboardState::Empty => ClipboardRead::Empty,
        ClipboardState::Sensitive => ClipboardRead::Sensitive,
        ClipboardState::TooLarge => {
            ClipboardRead::TooLarge(usize::try_from(header.size).unwrap_or(usize::MAX))
        }
        ClipboardState::Unavailable | ClipboardState::Unspecified => {
            return Ok(Snapshot::Unavailable);
        }
    };
    // Nothing of sensitive content goes, its files included.
    let files = if read == ClipboardRead::Sensitive {
        Vec::new()
    } else {
        files
    };
    Ok(Snapshot::Read(read, files))
}

/// The files copied in File Explorer that a ClipboardContents names, as
/// paths to have the broker list and read: more than the protocol allows
/// breaks it; paths that may not go (`copied_path`, which the broker keeps
/// to too) are left out.
fn copied_files(files: &[String]) -> Result<Vec<PathBuf>, LinkError> {
    if !within_copied_limits(files) {
        return Err(LinkError::Protocol("more copied files than allowed"));
    }
    Ok(files
        .iter()
        .filter(|path| copied_path(path))
        .map(PathBuf::from)
        .collect())
}

#[cfg(all(test, unix))]
mod tests {
    use iw_core::geometry::Rect;
    use iw_proto::service::v1::{DesktopPictures as Sent, DisplayPicture};

    use super::*;

    /// What a read found, without files.
    fn snapshot(read: ClipboardRead) -> Snapshot {
        Snapshot::Read(read, Vec::new())
    }

    fn display(id: &str) -> Display {
        Display {
            id: id.to_owned(),
            bounds: Rect::new(0.0, 0.0, 1920.0, 1080.0),
            scale: 1.0,
            primary: true,
        }
    }

    fn jpeg(tag: u8, len: usize) -> Vec<u8> {
        let mut bytes = vec![tag; len];
        bytes[..3].copy_from_slice(&[0xff, 0xd8, 0xff]);
        bytes
    }

    fn sent(pictures: &[(&str, Vec<u8>)]) -> Sent {
        Sent {
            pictures: pictures
                .iter()
                .map(|(id, jpeg)| DisplayPicture {
                    display_id: (*id).to_owned(),
                    jpeg: jpeg.clone(),
                })
                .collect(),
        }
    }

    /// `count` pictures of `len` bytes, for displays D0, D1, …
    fn many(count: usize, len: usize) -> Sent {
        Sent {
            pictures: (0..count)
                .map(|i| DisplayPicture {
                    display_id: format!("D{i}"),
                    jpeg: jpeg(1, len),
                })
                .collect(),
        }
    }

    #[test]
    fn copied_files_are_only_absolute_paths_and_never_too_many() {
        use iw_proto::service::{MAX_COPIED_FILES, MAX_COPIED_PATH_BYTES};

        let named = |paths: &[&str]| paths.iter().map(|p| (*p).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            copied_files(&named(&[
                "/Users/pc/a.txt",
                "relative/b",
                "/x/../etc/passwd",
                "/y/./z"
            ]))
            .unwrap(),
            vec![PathBuf::from("/Users/pc/a.txt")]
        );
        let many: Vec<String> = (0..=MAX_COPIED_FILES).map(|i| format!("/f/{i}")).collect();
        assert!(copied_files(&many).is_err());
        let long = format!("/{}", "a".repeat(MAX_COPIED_PATH_BYTES));
        assert!(copied_files(&[long]).is_err());
    }

    #[test]
    fn available_while_a_broker_is_connected_and_the_pictures_stay() {
        let pictures = Arc::new(Broker::new(1024));
        assert!(!pictures.available());
        let first = pictures.connected();
        assert!(pictures.available());
        pictures
            .received(&sent(&[(r"\\.\DISPLAY1", jpeg(1, 100))]))
            .unwrap();
        // A broker that restarts connects again before the old connection
        // is noticed gone.
        let second = pictures.connected();
        drop(first);
        assert!(pictures.available());
        drop(second);
        assert!(!pictures.available());
        let shown = pictures.pictures(&[display(r"\\.\DISPLAY1")]).unwrap();
        assert_eq!(shown[r"\\.\DISPLAY1"], jpeg(1, 100));
    }

    #[test]
    fn the_pictures_are_not_known_until_a_broker_sent_them() {
        let pictures = Arc::new(Broker::new(1024));
        let shown = [display(r"\\.\DISPLAY1")];
        // No user is logged on, or the broker has not read them yet: none,
        // and other devices keep what they have of this PC.
        assert!(!pictures.known());
        assert!(pictures.pictures(&shown).unwrap().is_empty());
        let link = pictures.connected();
        assert!(!pictures.known());
        // Pictures refused for the limits are no answer.
        assert!(
            pictures
                .received(&many(previews::MAX_PICTURES + 1, 10))
                .is_err()
        );
        assert!(!pictures.known());
        assert_eq!(pictures.generation(), 0);
        // A solid color is an answer, and is read at once.
        pictures.received(&sent(&[])).unwrap();
        assert!(pictures.known());
        assert_eq!(pictures.generation(), 1);
        pictures
            .received(&sent(&[(r"\\.\DISPLAY1", jpeg(1, 100))]))
            .unwrap();
        drop(link);
        // Known for as long as the agent runs, with no broker as well.
        assert!(pictures.known());
        assert_eq!(
            pictures.pictures(&shown).unwrap()[r"\\.\DISPLAY1"],
            jpeg(1, 100)
        );
    }

    #[test]
    fn only_new_pictures_change_the_generation() {
        let pictures = Broker::new(1024);
        assert_eq!(pictures.generation(), 0);
        let one = sent(&[(r"\\.\DISPLAY1", jpeg(1, 100))]);
        pictures.received(&one).unwrap();
        assert_eq!(pictures.generation(), 1);
        pictures.received(&one).unwrap();
        assert_eq!(pictures.generation(), 1, "the same again");
        // A solid color.
        pictures.received(&sent(&[])).unwrap();
        assert_eq!(pictures.generation(), 2);
        pictures.received(&sent(&[])).unwrap();
        assert_eq!(pictures.generation(), 2);
    }

    #[test]
    fn pictures_over_the_limits_are_all_dropped() {
        let pictures = Broker::new(1024);
        pictures
            .received(&sent(&[(r"\\.\DISPLAY1", jpeg(1, 100))]))
            .unwrap();
        let refused = [
            sent(&[
                (r"\\.\DISPLAY1", jpeg(1, 100)),
                (r"\\.\DISPLAY2", jpeg(2, previews::MAX_PICTURE + 1)),
            ]),
            sent(&[(r"\\.\DISPLAY1", b"not a jpeg".to_vec())]),
            sent(&[(&"x".repeat(previews::MAX_DISPLAY_ID + 1), jpeg(1, 100))]),
            many(previews::MAX_PICTURES + 1, 10),
            // Each within the limit, too many bytes together.
            many(
                previews::MAX_PICTURES_TOTAL / previews::MAX_PICTURE + 1,
                previews::MAX_PICTURE,
            ),
        ];
        for message in &refused {
            pictures
                .received(&sent(&[(r"\\.\DISPLAY1", jpeg(1, 100))]))
                .unwrap();
            let refusal = pictures.received(message);
            assert!(refusal.is_err());
            let shown = pictures.pictures(&[display(r"\\.\DISPLAY1")]).unwrap();
            assert!(shown.is_empty(), "{refusal:?}");
        }
        // At the limits, all are kept.
        let full = many(
            previews::MAX_PICTURES,
            previews::MAX_PICTURES_TOTAL / previews::MAX_PICTURES,
        );
        pictures.received(&full).unwrap();
        let ids: Vec<Display> = (0..previews::MAX_PICTURES)
            .map(|i| display(&format!("D{i}")))
            .collect();
        assert_eq!(
            pictures.pictures(&ids).unwrap().len(),
            previews::MAX_PICTURES
        );
    }

    #[test]
    fn each_read_asks_the_connected_brokers_to_look_again() {
        let pictures = Arc::new(Broker::new(1024));
        let shown = [display(r"\\.\DISPLAY1")];
        let mut asks = pictures.watch_asks();
        asks.mark_unchanged();
        // Nobody to ask.
        let _ = pictures.pictures(&shown);
        assert!(!asks.has_changed().unwrap());
        let _link = pictures.connected();
        // Also before it sent any.
        assert!(pictures.pictures(&shown).unwrap().is_empty());
        assert!(asks.has_changed().unwrap());
        asks.mark_unchanged();
        pictures.received(&sent(&[])).unwrap();
        pictures.pictures(&shown).unwrap();
        assert!(asks.has_changed().unwrap());
    }

    fn text(s: &str) -> ClipboardContent {
        ClipboardContent {
            items: vec![(ClipboardFormat::Text, s.as_bytes().to_vec())],
        }
    }

    /// A broker linked up, with what the agent sends it.
    fn linked(
        broker: &Broker,
    ) -> (
        Link,
        mpsc::UnboundedReceiver<Vec<AgentToBroker>>,
        mpsc::Receiver<AgentToBroker>,
    ) {
        let (control, control_rx) = mpsc::unbounded_channel();
        let (files, files_rx) = mpsc::channel(FILE_QUEUE);
        (broker.link_up(control, files), control_rx, files_rx)
    }

    fn asked_to_read(sent: &mut mpsc::UnboundedReceiver<Vec<AgentToBroker>>) -> bool {
        matches!(
            sent.try_recv().ok().as_deref(),
            Some([AgentToBroker {
                msg: Some(agent_to_broker::Msg::ReadClipboard(_))
            }])
        )
    }

    #[test]
    fn the_clipboard_is_read_ahead_and_writes_do_not_count_as_copies() {
        let broker = Arc::new(Broker::new(1024));
        let mut clipboard = BrokerClipboard(broker.clone());
        assert!(matches!(
            clipboard.change_count(),
            Err(PlatformError::Unavailable(_))
        ));
        assert!(clipboard.write(&text("x")).is_err());

        let (link, mut sent, _) = linked(&broker);
        // Nothing copied since the agent started.
        assert_eq!(clipboard.change_count().unwrap(), 0);
        assert_eq!(clipboard.read(1024).unwrap(), ClipboardRead::Empty);

        // A copy: read at once, and not readable until the answer comes.
        broker.clipboard_changed(link.id, 7);
        assert!(asked_to_read(&mut sent));
        assert_eq!(clipboard.change_count().unwrap(), 1);
        assert!(clipboard.read(1024).is_err());
        broker.clipboard_read(link.id, snapshot(ClipboardRead::Content(text("hi"))));
        assert_eq!(
            clipboard.read(1024).unwrap(),
            ClipboardRead::Content(text("hi"))
        );
        assert_eq!(clipboard.read(1).unwrap(), ClipboardRead::TooLarge(2));

        // The same sequence again, from a broker that restarted, is no copy.
        broker.clipboard_changed(link.id, 7);
        assert!(!asked_to_read(&mut sent));
        assert_eq!(clipboard.change_count().unwrap(), 1);

        // A write goes out in pieces and leaves the count alone.
        let big = vec![b'x'; PIECE + 1];
        let content = ClipboardContent {
            items: vec![(ClipboardFormat::Text, big)],
        };
        assert_eq!(clipboard.write(&content).unwrap(), 1);
        let group = sent.try_recv().unwrap();
        assert_eq!(group.len(), 3, "a header and two pieces");
        assert_eq!(clipboard.change_count().unwrap(), 1);

        // With sync off nothing is read; turned on, the latest copy is.
        clipboard.set_sync(false);
        broker.clipboard_changed(link.id, 9);
        assert!(!asked_to_read(&mut sent));
        assert!(clipboard.read(1024).is_err());
        clipboard.set_sync(true);
        assert!(asked_to_read(&mut sent));
        broker.clipboard_read(link.id, snapshot(ClipboardRead::Sensitive));
        assert_eq!(clipboard.read(1024).unwrap(), ClipboardRead::Sensitive);

        // An answer for an older copy is not taken for the newest.
        broker.clipboard_changed(link.id, 10);
        broker.clipboard_changed(link.id, 11);
        broker.clipboard_read(link.id, snapshot(ClipboardRead::Content(text("old"))));
        assert!(clipboard.read(1024).is_err());
        broker.clipboard_read(link.id, snapshot(ClipboardRead::Content(text("new"))));
        assert_eq!(
            clipboard.read(1024).unwrap(),
            ClipboardRead::Content(text("new"))
        );

        // A copy the broker could not read is asked for again, once.
        while sent.try_recv().is_ok() {}
        broker.clipboard_changed(link.id, 12);
        assert!(asked_to_read(&mut sent));
        broker.clipboard_read(link.id, Snapshot::Unavailable);
        assert!(clipboard.read(1024).is_err());
        assert!(asked_to_read(&mut sent));
        assert!(clipboard.read(1024).is_err());
        assert!(!asked_to_read(&mut sent), "already asked");
        broker.clipboard_read(link.id, snapshot(ClipboardRead::Content(text("again"))));
        assert_eq!(
            clipboard.read(1024).unwrap(),
            ClipboardRead::Content(text("again"))
        );

        // Without the broker there is no clipboard.
        broker.link_down(&link);
        assert!(clipboard.change_count().is_err());
    }

    /// Promised content, with what it is asked and whether it was told
    /// it is gone.
    #[derive(Default)]
    struct Source {
        content: ClipboardContent,
        asked: Mutex<Vec<ClipboardFormat>>,
        finished: std::sync::atomic::AtomicBool,
        /// Holds each fetch back until released.
        gate: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    }

    impl ClipboardSource for Source {
        fn fetch(&self, format: ClipboardFormat) -> Option<Vec<u8>> {
            self.asked.lock().unwrap().push(format);
            let gate = self.gate.lock().unwrap().take();
            if let Some(gate) = gate {
                let _ = gate.recv();
            }
            self.content.get(format).map(<[u8]>::to_vec)
        }

        fn finished(&self) {
            self.finished
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    impl Source {
        fn with(items: Vec<(ClipboardFormat, Vec<u8>)>) -> Arc<Self> {
            Arc::new(Self {
                content: ClipboardContent { items },
                ..Self::default()
            })
        }

        fn was_finished(&self) -> bool {
            self.finished.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    fn promise(source: &Arc<Source>, formats: &[ClipboardFormat], bytes: u64) -> ClipboardPromise {
        ClipboardPromise {
            formats: formats.to_vec(),
            bytes,
            files: Vec::new(),
            source: source.clone(),
        }
    }

    fn ask(promise_id: u64, mime: &str) -> RenderClipboard {
        RenderClipboard {
            promise_id,
            mime: mime.to_owned(),
        }
    }

    /// Promises `promise` through `broker`, as the engine's clipboard
    /// thread does, and has the broker answer `ok` once it is sent;
    /// returns the promise sent and what promising returned.
    fn promise_answered(
        broker: &Arc<Broker>,
        link: &Link,
        sent: &mut mpsc::UnboundedReceiver<Vec<AgentToBroker>>,
        promise: ClipboardPromise,
        ok: bool,
    ) -> (PromiseClipboard, Result<u64, PlatformError>) {
        let promising = std::thread::spawn({
            let broker = broker.clone();
            move || BrokerClipboard(broker).write_promise(promise)
        });
        let promised = loop {
            match sent.try_recv() {
                Ok(group) => match group.into_iter().next().and_then(|m| m.msg) {
                    Some(agent_to_broker::Msg::PromiseClipboard(p)) => break p,
                    other => panic!("expected a promise, got {other:?}"),
                },
                Err(_) => std::thread::sleep(Duration::from_millis(5)),
            }
        };
        broker.promised(link.id, promised.promise_id, ok);
        (promised, promising.join().unwrap())
    }

    /// The answer to a RenderClipboard, put back together.
    async fn rendered_from(
        sent: &mut mpsc::UnboundedReceiver<Vec<AgentToBroker>>,
    ) -> (ClipboardRendered, Vec<u8>) {
        let group = tokio::time::timeout(Duration::from_secs(5), sent.recv())
            .await
            .expect("the agent answers")
            .unwrap();
        let mut msgs = group.into_iter().map(|m| m.msg.unwrap());
        let Some(agent_to_broker::Msg::ClipboardRendered(header)) = msgs.next() else {
            panic!("expected a rendered format");
        };
        let mut data = Vec::new();
        for msg in msgs {
            let agent_to_broker::Msg::ClipboardData(piece) = msg else {
                panic!("expected clipboard data");
            };
            assert!(piece.data.len() <= PIECE);
            data.extend(piece.data);
        }
        assert_eq!(data.len() as u64, header.size);
        (header, data)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn promised_content_is_fetched_only_as_the_broker_asks() {
        let broker = Arc::new(Broker::new(1024));
        let mut clipboard = BrokerClipboard(broker.clone());
        let big = vec![b'x'; PIECE + 1];
        let source = Source::with(vec![(ClipboardFormat::Text, big.clone())]);
        let text_mime = ClipboardFormat::Text.mime();
        // Without a broker nothing can be promised.
        assert!(matches!(
            clipboard.write_promise(promise(&source, &[ClipboardFormat::Text], 9)),
            Err(PlatformError::Unavailable(_))
        ));

        let (link, mut sent, _) = linked(&broker);
        let (promised, result) = promise_answered(
            &broker,
            &link,
            &mut sent,
            promise(&source, &[ClipboardFormat::Text], 9),
            true,
        );
        assert_eq!(result.unwrap(), 0, "the count stays: it is no copy");
        assert_eq!(promised.mimes, vec![text_mime.to_owned()]);
        assert_eq!(promised.size, 9);
        let id = promised.promise_id;
        assert!(source.asked.lock().unwrap().is_empty());

        // An app pastes it: fetched, and sent in pieces.
        broker.render(&link, ask(id, text_mime));
        let (header, data) = rendered_from(&mut sent).await;
        assert!(header.ok && !header.busy);
        assert_eq!((header.promise_id, header.mime.as_str()), (id, text_mime));
        assert_eq!(data, big);

        // A format it does not have, a promise never made, and one on
        // another connection, are answered with nothing.
        broker.render(&link, ask(id, ClipboardFormat::Png.mime()));
        let (header, _) = rendered_from(&mut sent).await;
        assert!(!header.ok && !header.busy);
        broker.render(&link, ask(id + 1, text_mime));
        assert!(!rendered_from(&mut sent).await.0.ok);
        assert!(!rendered_from_other_link(&broker, id).await.ok);

        // Copied over on the PC: forgotten, and its source told.
        broker.promise_gone(link.id, id);
        assert!(source.was_finished());
        broker.render(&link, ask(id, text_mime));
        assert!(!rendered_from(&mut sent).await.0.ok);
        assert_eq!(
            *source.asked.lock().unwrap(),
            vec![ClipboardFormat::Text, ClipboardFormat::Png]
        );

        // More than the broker takes is never promised to it: the
        // engine's clipboard thread holds offers to this limit
        // (`local_clipboard`), as it does writes.
        assert_eq!(clipboard.max_write(), Some(MAX_CLIPBOARD as usize));
        assert!(sent.try_recv().is_err());
    }

    /// What a RenderClipboard for promise `id` gets on a connection that
    /// was not promised it.
    async fn rendered_from_other_link(broker: &Broker, id: u64) -> ClipboardRendered {
        let (control, mut sent) = mpsc::unbounded_channel();
        let (files, _) = mpsc::channel(FILE_QUEUE);
        let other = Link {
            id: u64::MAX,
            control,
            files,
            replies: Arc::default(),
            rendering: Arc::default(),
        };
        broker.render(&other, ask(id, ClipboardFormat::Text.mime()));
        rendered_from(&mut sent).await.0
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_promise_is_not_on_the_clipboard_until_the_broker_says_so() {
        let broker = Arc::new(Broker::new(1024));
        let source = Source::with(vec![(ClipboardFormat::Text, b"hi".to_vec())]);
        let (link, mut sent, _) = linked(&broker);
        // The broker could not put it there: the engine is told to write
        // the content instead, and the promise is forgotten.
        let (promised, result) = promise_answered(
            &broker,
            &link,
            &mut sent,
            promise(&source, &[ClipboardFormat::Text], 2),
            false,
        );
        assert!(matches!(result, Err(PlatformError::Unsupported(_))));
        broker.render(
            &link,
            ask(promised.promise_id, ClipboardFormat::Text.mime()),
        );
        assert!(!rendered_from(&mut sent).await.0.ok);

        // The broker goes before it answers: an error at once.
        let promising = std::thread::spawn({
            let broker = broker.clone();
            let promise = promise(&source, &[ClipboardFormat::Text], 2);
            move || BrokerClipboard(broker).write_promise(promise)
        });
        while sent.try_recv().is_err() {
            std::thread::sleep(Duration::from_millis(5));
        }
        let started = std::time::Instant::now();
        broker.link_down(&link);
        assert!(matches!(
            promising.join().unwrap(),
            Err(PlatformError::Unavailable(_))
        ));
        assert!(started.elapsed() < promise_answer());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_promise_the_broker_does_not_answer_is_withdrawn() {
        let broker = Arc::new(Broker::new(1024));
        let source = Source::with(vec![(ClipboardFormat::Text, b"hi".to_vec())]);
        let (link, mut sent, _) = linked(&broker);
        // No answer: an error, which the engine does not turn into a write
        // the stuck broker would not do either, and the broker is told to
        // leave it off the clipboard.
        let result = BrokerClipboard(broker.clone()).write_promise(promise(
            &source,
            &[ClipboardFormat::Text],
            2,
        ));
        assert!(matches!(result, Err(PlatformError::Unavailable(_))));
        let mut msgs = Vec::new();
        while let Ok(group) = sent.try_recv() {
            msgs.extend(group.into_iter().filter_map(|m| m.msg));
        }
        let [
            agent_to_broker::Msg::PromiseClipboard(promised),
            agent_to_broker::Msg::WithdrawPromise(withdrawn),
        ] = msgs.as_slice()
        else {
            panic!("expected a promise and its withdrawal, got {msgs:?}");
        };
        assert_eq!(promised.promise_id, withdrawn.promise_id);
        // A late answer changes nothing, and it is no longer rendered.
        broker.promised(link.id, promised.promise_id, true);
        broker.render(
            &link,
            ask(promised.promise_id, ClipboardFormat::Text.mime()),
        );
        assert!(!rendered_from(&mut sent).await.0.ok);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn each_promise_is_fetched_once_at_a_time_and_none_too_large() {
        let broker = Arc::new(Broker::new(1024));
        let (release, gate) = std::sync::mpsc::channel();
        let held_back = Arc::new(Source {
            content: ClipboardContent {
                items: vec![
                    (ClipboardFormat::Text, b"hi".to_vec()),
                    (ClipboardFormat::Png, vec![0; MAX_CLIPBOARD as usize + 1]),
                ],
            },
            gate: Mutex::new(Some(gate)),
            ..Source::default()
        });
        let other = Source::with(vec![(ClipboardFormat::Text, b"other".to_vec())]);
        let (link, mut sent, _) = linked(&broker);
        let formats = [ClipboardFormat::Text, ClipboardFormat::Png];
        let id = promise_answered(
            &broker,
            &link,
            &mut sent,
            promise(&held_back, &formats, 9),
            true,
        )
        .0
        .promise_id;
        let other_id = promise_answered(
            &broker,
            &link,
            &mut sent,
            promise(&other, &[ClipboardFormat::Text], 5),
            true,
        )
        .0
        .promise_id;

        // The text is held back: asking for the image of the same promise
        // meanwhile is busy, which is no failure; another promise is
        // fetched meanwhile.
        broker.render(&link, ask(id, ClipboardFormat::Text.mime()));
        broker.render(&link, ask(id, ClipboardFormat::Png.mime()));
        let (header, _) = rendered_from(&mut sent).await;
        assert!(!header.ok && header.busy);
        broker.render(&link, ask(other_id, ClipboardFormat::Text.mime()));
        let (header, data) = rendered_from(&mut sent).await;
        assert_eq!(
            (header.promise_id, data.as_slice()),
            (other_id, &b"other"[..])
        );
        release.send(()).unwrap();
        let (header, data) = rendered_from(&mut sent).await;
        assert!(header.ok);
        assert_eq!(data, b"hi");

        // An image larger than the broker takes is not sent to it.
        broker.render(&link, ask(id, ClipboardFormat::Png.mime()));
        let (header, data) = rendered_from(&mut sent).await;
        assert!(!header.ok && !header.busy);
        assert!(data.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn nothing_is_sent_once_the_broker_stopped_waiting() {
        let broker = Arc::new(Broker::new(1024));
        let (release, gate) = std::sync::mpsc::channel();
        let source = Arc::new(Source {
            content: ClipboardContent {
                items: vec![(ClipboardFormat::Text, b"late".to_vec())],
            },
            gate: Mutex::new(Some(gate)),
            ..Source::default()
        });
        let (link, mut sent, _) = linked(&broker);
        let id = promise_answered(
            &broker,
            &link,
            &mut sent,
            promise(&source, &[ClipboardFormat::Text], 4),
            true,
        )
        .0
        .promise_id;
        broker.render(&link, ask(id, ClipboardFormat::Text.mime()));
        tokio::time::sleep(Duration::from_millis(50)).await;
        broker.render_cancelled(&link, id);
        // Still fetching: busy, and nothing comes of the cancelled one.
        broker.render(&link, ask(id, ClipboardFormat::Text.mime()));
        assert!(rendered_from(&mut sent).await.0.busy);
        release.send(()).unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        let late = sent.try_recv();
        assert!(late.is_err(), "sent after the broker gave up: {late:?}");
        // Fetched by now: the next paste gets it.
        broker.render(&link, ask(id, ClipboardFormat::Text.mime()));
        let (header, data) = rendered_from(&mut sent).await;
        assert!(header.ok);
        assert_eq!(data, b"late");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn promises_end_with_their_broker() {
        let broker = Arc::new(Broker::new(1024));
        let (link, mut sent, _) = linked(&broker);
        let sources: Vec<Arc<Source>> = (0..MAX_PROMISES + 1)
            .map(|_| Arc::new(Source::default()))
            .collect();
        for source in &sources[..MAX_PROMISES] {
            let (_, result) = promise_answered(
                &broker,
                &link,
                &mut sent,
                promise(source, &[ClipboardFormat::Text], 1),
                true,
            );
            result.unwrap();
        }
        // Beyond the few remembered, the oldest is let go, even when the
        // newest cannot be sent.
        drop(sent);
        let failed = BrokerClipboard(broker.clone()).write_promise(promise(
            &sources[MAX_PROMISES],
            &[ClipboardFormat::Text],
            1,
        ));
        assert!(failed.is_err());
        assert!(sources[0].was_finished());
        assert!(!sources[1].was_finished());
        broker.link_down(&link);
        assert!(sources[..MAX_PROMISES].iter().all(|s| s.was_finished()));
    }

    #[test]
    fn a_clipboard_larger_than_read_ahead_is_read_when_more_is_wanted() {
        let broker = Arc::new(Broker::new(1024));
        let clipboard = BrokerClipboard(broker.clone());
        let (link, mut sent, _) = linked(&broker);
        broker.clipboard_changed(link.id, 3);
        assert!(asked_to_read(&mut sent));
        broker.clipboard_read(link.id, snapshot(ClipboardRead::TooLarge(5000)));
        // Read ahead, it was too large; the engine wants no more.
        assert_eq!(clipboard.read(1024).unwrap(), ClipboardRead::TooLarge(5000));
        assert!(sent.try_recv().is_err());

        // It wants more: read now, held to the broker's limit.
        let reading = std::thread::spawn({
            let broker = broker.clone();
            move || BrokerClipboard(broker).read(u64::MAX as usize)
        });
        let max = loop {
            match sent.try_recv() {
                Ok(group) => match group.into_iter().next().and_then(|m| m.msg) {
                    Some(agent_to_broker::Msg::ReadClipboard(read)) => break read.max_bytes,
                    other => panic!("expected a read, got {other:?}"),
                },
                Err(_) => std::thread::sleep(Duration::from_millis(5)),
            }
        };
        assert_eq!(max, MAX_CLIPBOARD);
        assert_eq!(broker.answering_max(link.id), MAX_CLIPBOARD as usize);
        let big = ClipboardContent {
            items: vec![(ClipboardFormat::Text, vec![b'x'; 5000])],
        };
        broker.clipboard_read(link.id, snapshot(ClipboardRead::Content(big.clone())));
        assert_eq!(
            reading.join().unwrap().unwrap(),
            ClipboardRead::Content(big.clone())
        );
        // What was read ahead stays as it was.
        assert_eq!(broker.answering_max(link.id), 1024);
        assert_eq!(clipboard.read(1024).unwrap(), ClipboardRead::TooLarge(5000));
        // Asked again for the same clipboard, as for a pull: not read again.
        assert_eq!(
            clipboard.read(u64::MAX as usize).unwrap(),
            ClipboardRead::Content(big)
        );
        assert_eq!(clipboard.read(4096).unwrap(), ClipboardRead::TooLarge(5000));
        assert!(sent.try_recv().is_err());

        // A new copy, too large even for the broker: read once, and
        // known too large after that.
        broker.clipboard_changed(link.id, 4);
        assert!(asked_to_read(&mut sent));
        broker.clipboard_read(link.id, snapshot(ClipboardRead::TooLarge(1 << 30)));
        let reading = std::thread::spawn({
            let broker = broker.clone();
            move || BrokerClipboard(broker).read(u64::MAX as usize)
        });
        while sent.try_recv().is_err() {
            std::thread::sleep(Duration::from_millis(5));
        }
        broker.clipboard_read(link.id, snapshot(ClipboardRead::TooLarge(1 << 30)));
        assert_eq!(
            reading.join().unwrap().unwrap(),
            ClipboardRead::TooLarge(1 << 30)
        );
        assert_eq!(
            clipboard.read(u64::MAX as usize).unwrap(),
            ClipboardRead::TooLarge(1 << 30)
        );
        assert!(sent.try_recv().is_err());
    }

    #[test]
    fn a_new_broker_reads_what_the_last_one_never_answered() {
        let broker = Arc::new(Broker::new(1024));
        let (old, mut old_sent, _) = linked(&broker);
        broker.clipboard_changed(old.id, 3);
        assert!(asked_to_read(&mut old_sent));
        let (new, mut new_sent, _) = linked(&broker);
        assert!(asked_to_read(&mut new_sent));
        // The old one's answer is no longer wanted.
        broker.clipboard_read(old.id, snapshot(ClipboardRead::Empty));
        broker.link_down(&old);
        broker.clipboard_read(new.id, snapshot(ClipboardRead::Content(text("a"))));
        let mut clipboard = BrokerClipboard(broker.clone());
        assert_eq!(
            clipboard.read(1024).unwrap(),
            ClipboardRead::Content(text("a"))
        );

        // With sync off, a broker that connects reads nothing.
        clipboard.set_sync(false);
        broker.clipboard_changed(new.id, 4);
        assert!(!asked_to_read(&mut new_sent));
        let (_, mut newest_sent, _) = linked(&broker);
        assert!(!asked_to_read(&mut newest_sent));
    }

    fn loaded(toml: &[u8]) -> HistoryLoaded {
        HistoryLoaded {
            toml: toml.to_vec(),
            error: String::new(),
        }
    }

    fn loaded_from(history: &watch::Receiver<UserHistory>) -> Option<(u64, Vec<u8>)> {
        match &*history.borrow() {
            UserHistory::Loaded { link, toml, .. } => Some((*link, toml.to_vec())),
            _ => None,
        }
    }

    /// Has the broker save `toml` through `link`, and waits for the
    /// message to go out.
    async fn saving(
        broker: &Arc<Broker>,
        link: u64,
        toml: &[u8],
        sent: &mut mpsc::UnboundedReceiver<Vec<AgentToBroker>>,
    ) -> tokio::task::JoinHandle<io::Result<()>> {
        let task = tokio::spawn({
            let (broker, toml) = (broker.clone(), toml.to_vec());
            async move { broker.save_history(link, toml).await }
        });
        let group = sent.recv().await.unwrap();
        assert!(matches!(group.as_slice(), [AgentToBroker {
            msg: Some(agent_to_broker::Msg::SaveHistory(s))
        }] if s.toml == toml));
        task
    }

    fn answer_save(link: &Link, error: &str) {
        let waiting = link.replies.lock().unwrap().saves.pop_front().unwrap();
        waiting.send(error.to_owned()).unwrap();
    }

    #[tokio::test]
    async fn the_history_is_saved_only_through_the_broker_it_was_loaded_from() {
        let broker = Arc::new(Broker::new(1024));
        let history = broker.watch_history();
        assert!(matches!(*history.borrow(), UserHistory::None));
        let not_connected = |result: io::Result<()>| matches!(result, Err(e) if e.kind() == io::ErrorKind::NotConnected);

        let (old, _, _) = linked(&broker);
        assert!(matches!(*history.borrow(), UserHistory::Loading(l) if l == old.id));
        // Not loaded yet: the history is nobody's to save.
        assert!(not_connected(
            broker.save_history(old.id, b"x".to_vec()).await
        ));

        // A broker that restarts connects again first: the old connection's
        // answer is no longer taken.
        let (new, mut sent, _) = linked(&broker);
        broker.history_loaded(old.id, loaded(b"old"));
        assert!(matches!(*history.borrow(), UserHistory::Loading(l) if l == new.id));
        broker.history_loaded(new.id, loaded(b"new"));
        assert_eq!(loaded_from(&history), Some((new.id, b"new".to_vec())));
        // Asked once, taken once.
        broker.history_loaded(new.id, loaded(b"again"));
        assert_eq!(loaded_from(&history), Some((new.id, b"new".to_vec())));

        // Saved through it, and told how that went.
        let saved = saving(&broker, new.id, b"saved", &mut sent).await;
        answer_save(&new, "");
        saved.await.unwrap().unwrap();
        let failed = saving(&broker, new.id, b"more", &mut sent).await;
        answer_save(&new, "the disk is full");
        let error = failed.await.unwrap().unwrap_err();
        assert!(error.to_string().contains("disk is full"), "{error}");
        // Not through the old connection.
        assert!(not_connected(
            broker.save_history(old.id, b"x".to_vec()).await
        ));
        broker.link_down(&old);
        assert_eq!(loaded_from(&history).map(|(link, _)| link), Some(new.id));

        // Once it goes, the history is nobody's; a save waiting ends.
        let cut = saving(&broker, new.id, b"last", &mut sent).await;
        broker.link_down(&new);
        assert!(not_connected(cut.await.unwrap()));
        assert!(matches!(*history.borrow(), UserHistory::None));
        assert!(not_connected(
            broker.save_history(new.id, b"x".to_vec()).await
        ));
    }

    #[test]
    fn the_folder_is_the_current_brokers() {
        let broker = Broker::new(1024);
        let (old, _, _) = linked(&broker);
        let (new, _, _) = linked(&broker);
        broker.set_folder(old.id, "/Users/a/Downloads/InputWeave");
        assert_eq!(broker.receive_folder(), None, "not the current broker");
        broker.set_folder(new.id, "relative");
        assert_eq!(broker.receive_folder(), None);
        broker.set_folder(new.id, "/Users/a/Downloads/InputWeave");
        assert_eq!(
            broker.receive_folder(),
            Some(PathBuf::from("/Users/a/Downloads/InputWeave"))
        );
        broker.link_down(&old);
        assert!(broker.receive_folder().is_some());
        broker.link_down(&new);
        assert_eq!(broker.receive_folder(), None);
    }
}
