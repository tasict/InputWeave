//! The broker: the user-side helper the service starts with the logged-on
//! user's token.
//!
//! What must not run as SYSTEM is done here, as the user, for the agent:
//! a bug such as a path traversal in SYSTEM's code would become a
//! privilege escalation. That is reading the desktop pictures, whose files
//! and settings are the user's; reading and writing the user's clipboard;
//! writing the files and folders other devices send, and checking the
//! folder the user drops files from another device in (`files`); and
//! listing and reading the files and folders the user sends (`sending`),
//! which any process of the user can ask the agent for; and keeping the
//! user's transfer history, which holds the names of the user's files, in
//! the user's own folder (`history`). The broker also starts the app when
//! the user has to see or decide something and no app is connected to the
//! agent (`app`).
//!
//! The broker connects to the pipe its session's agent serves it on
//! (`\\.\pipe\InputWeave\broker-<SessionId>`) and speaks
//! `inputweave.service.v1` there, reconnecting whenever the agent
//! restarts. Once connected it sends where received files go
//! (`ReceiveFolder`), the previews of the desktop pictures, and the
//! clipboard's sequence number unless the clipboard holds what InputWeave
//! wrote; after that, the previews again whenever the agent asks, each
//! change other apps make to the clipboard (`ClipboardChanged`), the
//! clipboard's content for each `ReadClipboard`, the answers about files,
//! the transfer history for `LoadHistory` and `SaveHistory`, and whether
//! the app runs for each `LaunchApp`. Clipboard content
//! travels in pieces after its header, never split by other messages,
//! since frames are at most 4 MiB.
//!
//! Content another device offers comes as a promise (`PromiseClipboard`,
//! answered with `ClipboardPromised`): the clipboard is given its formats
//! without data, and when an app pastes one the broker asks the agent for
//! it (`RenderClipboard`) and waits, at most [`RENDER_WAIT`], for the
//! answer (`ClipboardRendered`), or tells the agent it stopped waiting
//! (`RenderCancelled`), which leaves that paste empty but the promise in
//! place; once something else is copied over it the agent is told
//! (`PromiseGone`). A promise the agent withdrew (`WithdrawPromise`), or
//! one whose connection ended, is not put on the clipboard, or is taken
//! off it again while the clipboard still holds it.
//!
//! The platform parts come from the caller (`Services`): the readers of
//! the desktop pictures and of the clipboard live with the other Windows
//! platform code in `iw-platform`, which depends on this crate.
//!
//! The service starts the broker alongside the agent, and restarts the
//! agent on logons, so the broker usually looks for the agent's pipe a
//! moment before the agent creates it. That is only worth a warning when
//! the pipe is still missing after a few seconds, or when connecting fails
//! in any other way (`Outage`).

mod app;
mod files;
mod history;
mod sending;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iw_ipc::{FrameError, IpcConnection, IpcReader};
use iw_proto::service::copied_paths;
use iw_proto::service::v1::{
    AgentToBroker, AppLaunched, BrokerMessage, ClipboardChanged, ClipboardContents, ClipboardData,
    ClipboardPart, ClipboardPromised, ClipboardRendered, ClipboardState, DesktopPictures,
    DisplayPicture, LaunchReason, PromiseGone, ReceiveFolder, RenderCancelled, RenderClipboard,
    agent_to_broker, broker_message,
};
use tokio::sync::{mpsc, watch};

pub use app::Launch;
#[cfg(windows)]
pub use app::launch_installed_app;
use files::{Cleaned, PartFiles};
use sending::Outgoing;

/// Reads previews of the pictures on the user's desktops: JPEGs by display
/// id, as the agent names displays. It may decode large pictures, so it
/// runs off the async runtime.
pub type ReadPictures = Arc<dyn Fn() -> Result<BTreeMap<String, Vec<u8>>, String> + Send + Sync>;

/// The user's clipboard, with its content by MIME type as devices
/// exchange it ("text/plain;charset=utf-8", "text/rtf", "text/html",
/// "image/png"). Its calls block, so they run off the async runtime.
pub trait UserClipboard: Send + Sync {
    /// The content in every format carried, unless it is sensitive or
    /// larger than `max_len` bytes together.
    fn read(&self, max_len: usize) -> Result<ClipboardRead, String>;

    /// Reads it as `read` does, with the paths of the files copied in File
    /// Explorer, as the user reads them and as they are: the broker checks
    /// them and keeps to the protocol's limits
    /// (`iw_proto::service::copied_paths`). None when the content is
    /// sensitive.
    fn read_with_files(&self, max_len: usize) -> Result<(ClipboardRead, Vec<String>), String> {
        Ok((self.read(max_len)?, Vec::new()))
    }

    /// Replaces the content, marked as InputWeave's so that the change is
    /// not reported as one the user made.
    fn write(&self, parts: &[(String, Vec<u8>)]) -> Result<(), String>;

    /// Replaces the content with a promise of `mimes`, `size` bytes
    /// together, marked as InputWeave's like a write: each is asked of
    /// `content` only when an app pastes it, and `content` hears when
    /// something else is copied over it. Returns a token to withdraw it by.
    fn promise(
        &self,
        mimes: &[String],
        size: u64,
        content: Arc<dyn PromisedContent>,
    ) -> Result<u64, String> {
        let _ = (mimes, size, content);
        Err("this clipboard cannot promise content".to_owned())
    }

    /// Takes the promise placed as `token` off the clipboard, if the
    /// clipboard still holds it: nothing can render it any more.
    fn withdraw(&self, token: u64) -> Result<(), String> {
        let _ = token;
        Ok(())
    }
}

/// Content promised on the clipboard, which the agent has.
pub trait PromisedContent: Send + Sync {
    /// The bytes of `mime`, which an app is pasting. It blocks until the
    /// agent answers, for at most [`RENDER_WAIT`]; after that it is
    /// [`Render::Busy`], so a later paste asks again.
    fn render(&self, mime: &str) -> Render;

    /// The clipboard no longer holds the promise.
    fn gone(&self);
}

/// What the agent answered for a promised format.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Render {
    Content(Vec<u8>),
    /// Still fetching it, for this paste or an earlier one, past
    /// [`RENDER_WAIT`]: nothing failed, and a later paste may get it.
    Busy,
    /// It cannot be had.
    Unavailable,
}

/// What reading the clipboard found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClipboardRead {
    Content(Vec<(String, Vec<u8>)>),
    Empty,
    Sensitive,
    /// Larger than asked for, in bytes.
    TooLarge(u64),
}

/// The clipboard and the changes other apps make to it.
#[derive(Clone)]
pub struct ClipboardAccess {
    pub clipboard: Arc<dyn UserClipboard>,
    /// The clipboard's sequence number after the latest change another app
    /// made; `None` until there is one, as when the clipboard holds what
    /// InputWeave wrote.
    pub changes: watch::Receiver<Option<u64>>,
}

/// Starts the app unless it runs already (`launch_installed_app`). It
/// starts a process, so it runs off the async runtime.
pub type LaunchApp = Arc<dyn Fn() -> Result<Launch, String> + Send + Sync>;

/// What the broker does its work with.
#[derive(Clone)]
pub struct Services {
    pub read_pictures: ReadPictures,
    /// `None` if the clipboard cannot be reached; the agent then has none.
    pub clipboard: Option<ClipboardAccess>,
    /// Where received files go unless the user chose another folder:
    /// the user's Downloads\InputWeave. `None` if it is not known; files
    /// then go only to a folder the user chose.
    pub receive_folder: Option<PathBuf>,
    /// Where the user's transfer history is kept
    /// (`default_history_file`). `None` if it is not known; the history
    /// then lasts until the agent stops.
    pub history_file: Option<PathBuf>,
    pub launch_app: LaunchApp,
}

#[derive(Debug, thiserror::Error)]
pub enum BrokerError {
    #[error(
        "the broker must run as the logged-on user, never as SYSTEM: it handles the user's \
         files"
    )]
    RunningAsSystem,
    #[error("cannot start the broker: {0}")]
    Os(io::Error),
}

/// Pieces clipboard content travels in.
const PIECE: usize = 1024 * 1024;
/// The most clipboard content the broker takes from the agent, whatever
/// it asks for.
const MAX_CLIPBOARD: u64 = iw_proto::service::MAX_CLIPBOARD;
/// Formats in one clipboard; there are four.
const MAX_PARTS: usize = 4;
/// How long an app pasting promised content waits for the agent's answer.
/// Meanwhile the clipboard thread answers `WM_RENDERFORMAT` and the
/// clipboard stays locked for every other app, so it is much shorter than
/// the Mac's 100 seconds; content that takes longer is not rendered, and
/// the agent is told to stop (`RenderCancelled`).
pub const RENDER_WAIT: Duration = iw_proto::service::RENDER_WAIT;

/// [`RENDER_WAIT`], or a moment in tests, which see the broker give up.
fn render_wait() -> Duration {
    if cfg!(test) {
        Duration::from_millis(300)
    } else {
        RENDER_WAIT
    }
}

/// Runs the broker until the process is stopped.
#[cfg(windows)]
pub fn run(services: Services) -> Result<(), BrokerError> {
    const RECONNECT: Duration = Duration::from_secs(1);

    let system =
        crate::token::running_as_local_system().map_err(|e| BrokerError::Os(io::Error::from(e)))?;
    if system {
        return Err(BrokerError::RunningAsSystem);
    }
    let pipe = iw_ipc::broker_pipe_name().map_err(BrokerError::Os)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(BrokerError::Os)?;
    tracing::info!(
        pipe = %pipe.display(),
        clipboard = services.clipboard.is_some(),
        receive_folder = ?services.receive_folder,
        history_file = ?services.history_file,
        "broker running"
    );
    runtime.block_on(async {
        let memory = Arc::new(Memory::default());
        let mut outage = Outage::new(Instant::now());
        loop {
            match connect(&pipe).await {
                Ok(conn) => {
                    tracing::info!("connected to the agent");
                    match serve(conn, &services, &memory).await {
                        Ok(()) => tracing::info!("the agent closed the connection"),
                        Err(e) => tracing::info!(error = %e, "lost the connection to the agent"),
                    }
                    outage = Outage::new(Instant::now());
                }
                Err(e) => match outage.failed(Instant::now(), e.agent_missing()) {
                    Report::Quiet => {
                        tracing::debug!(error = %e, "not connected to the agent yet; retrying");
                    }
                    Report::Warn => {
                        tracing::warn!(error = %e, "not connected to the agent; retrying");
                    }
                },
            }
            tokio::time::sleep(RECONNECT).await;
        }
    })
}

/// The user's Downloads\InputWeave, where received files go unless the
/// user chose another folder. The shell knows where Downloads is, which
/// the user may have moved.
#[cfg(windows)]
pub fn default_receive_folder() -> Result<PathBuf, io::Error> {
    use windows::Win32::UI::Shell::FOLDERID_Downloads;

    Ok(known_folder(&FOLDERID_Downloads)?.join("InputWeave"))
}

/// The user's %LOCALAPPDATA%\InputWeave\transfers.toml, where the
/// transfer history is kept. Asked of the shell, as the environment of a
/// process started with the user's token may not say.
#[cfg(windows)]
pub fn default_history_file() -> Result<PathBuf, io::Error> {
    use windows::Win32::UI::Shell::FOLDERID_LocalAppData;

    Ok(history::history_file(&known_folder(
        &FOLDERID_LocalAppData,
    )?))
}

/// Where the shell says the user's known folder `id` is.
#[cfg(windows)]
fn known_folder(id: &windows::core::GUID) -> Result<PathBuf, io::Error> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{KF_FLAG_DEFAULT, SHGetKnownFolderPath};

    // SAFETY: the GUID is a constant; the string returned is the caller's,
    // freed once it is copied.
    let folder = unsafe {
        let path = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).map_err(io::Error::other)?;
        let text = path.to_string();
        CoTaskMemFree(Some(path.0 as *const _));
        text.map_err(io::Error::other)?
    };
    Ok(PathBuf::from(folder))
}

/// How long the agent's pipe may be missing, after the broker starts or
/// loses its connection, before that is worth a warning. The agent creates
/// it well within a second of starting.
const GRACE: Duration = Duration::from_secs(5);

/// How to log a failed attempt to connect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Report {
    /// Expected, or already reported: debug level.
    Quiet,
    Warn,
}

/// The time since the broker started or was last connected, which decides
/// which failed attempts get a warning: one per outage, once the pipe has
/// been missing for longer than [`GRACE`] or at the first other failure.
/// Free of FFI and clocks (time is passed in), so it is tested on every OS.
#[derive(Debug)]
pub(crate) struct Outage {
    since: Instant,
    warned: bool,
}

impl Outage {
    /// An outage starting at `now`: the broker started, or its connection
    /// ended.
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            since: now,
            warned: false,
        }
    }

    /// An attempt failed at `now`; `agent_missing` if the agent's pipe did
    /// not exist.
    pub(crate) fn failed(&mut self, now: Instant, agent_missing: bool) -> Report {
        let starting = agent_missing && now.saturating_duration_since(self.since) < GRACE;
        if self.warned || starting {
            return Report::Quiet;
        }
        self.warned = true;
        Report::Warn
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SessionError {
    #[error("cannot reach the agent: {0}")]
    Connect(io::Error),
}

impl SessionError {
    /// The agent's pipe does not exist: the agent is not running, or has
    /// not created it yet.
    pub(crate) fn agent_missing(&self) -> bool {
        matches!(self, SessionError::Connect(e) if e.kind() == io::ErrorKind::NotFound)
    }
}

/// Connects to the pipe the agent serves the broker on.
pub(crate) async fn connect(pipe: &Path) -> Result<IpcConnection, SessionError> {
    IpcConnection::connect(pipe)
        .await
        .map_err(SessionError::Connect)
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ServeError {
    #[error(transparent)]
    Frame(#[from] FrameError),
    #[error("the agent broke the protocol: {0}")]
    Protocol(&'static str),
}

/// What outlives a connection to the agent.
#[derive(Debug, Default)]
pub(crate) struct Memory {
    /// The last failure to read the desktop pictures, so that one that
    /// lasts is logged once rather than at every request.
    failures: Mutex<Option<String>>,
    /// The last failure to read or write the clipboard, likewise.
    clipboard_failures: Mutex<Option<String>>,
    cleaned: Cleaned,
    /// When the app was last started.
    app: Mutex<app::Gate>,
}

/// Messages to the agent; each group goes out whole, in order.
type Out = mpsc::UnboundedSender<Vec<BrokerMessage>>;

fn message(msg: broker_message::Msg) -> BrokerMessage {
    BrokerMessage { msg: Some(msg) }
}

/// Serves the agent until it closes the connection.
pub(crate) async fn serve(
    conn: IpcConnection,
    services: &Services,
    memory: &Arc<Memory>,
) -> Result<(), ServeError> {
    let (mut reader, mut writer) = conn.into_split();
    let (out, mut outgoing) = mpsc::unbounded_channel::<Vec<BrokerMessage>>();
    let writing = async move {
        while let Some(group) = outgoing.recv().await {
            for msg in &group {
                writer.send(msg).await?;
            }
        }
        Ok::<(), FrameError>(())
    };
    let session = Session::start(services, memory, out);
    tokio::select! {
        written = writing => Ok(written?),
        served = session.run(&mut reader) => served,
    }
}

enum ClipboardJob {
    Read(usize),
    Write(Vec<(String, Vec<u8>)>),
    Promise {
        promise_id: u64,
        mimes: Vec<String>,
        size: u64,
        content: Arc<dyn PromisedContent>,
    },
    /// The agent gave up on it: off the clipboard if it is there.
    Withdraw(u64),
}

/// The promises one connection put on the clipboard, by number, with the
/// token each can be withdrawn by.
#[derive(Default)]
struct Placed {
    /// The connection ended: those placed after are withdrawn at once.
    ended: bool,
    tokens: HashMap<u64, u64>,
}

/// The agent's answers to what the clipboard asks of its promises, one at
/// a time, as the clipboard's thread asks.
#[derive(Default)]
struct Renders {
    state: Mutex<RendersState>,
}

#[derive(Default)]
struct RendersState {
    /// The connection ended: nothing more is answered.
    closed: bool,
    waiting: Option<Waiting>,
}

/// A format the clipboard waits for.
struct Waiting {
    promise_id: u64,
    mime: String,
    answer: std::sync::mpsc::SyncSender<Render>,
}

impl Renders {
    fn state(&self) -> std::sync::MutexGuard<'_, RendersState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether the clipboard waits for `mime` of promise `promise_id`.
    fn waits(&self, promise_id: u64, mime: &str) -> bool {
        self.state()
            .waiting
            .as_ref()
            .is_some_and(|w| w.promise_id == promise_id && w.mime == mime)
    }

    /// Hands the agent's answer to whoever waits for it.
    fn answered(&self, promise_id: u64, mime: &str, render: Render) {
        let mut state = self.state();
        let waits = state
            .waiting
            .as_ref()
            .is_some_and(|w| w.promise_id == promise_id && w.mime == mime);
        if let Some(waiting) = state.waiting.take_if(|_| waits) {
            let _ = waiting.answer.send(render);
        }
    }

    /// The connection ended: whoever waits, and anyone asking later, gets
    /// nothing.
    fn close(&self) {
        let mut state = self.state();
        state.closed = true;
        state.waiting = None;
    }
}

/// A promise the agent made, which the clipboard asks for its formats.
struct AgentPromise {
    promise_id: u64,
    renders: Arc<Renders>,
    out: Out,
    placed: Arc<Mutex<Placed>>,
}

impl PromisedContent for AgentPromise {
    fn render(&self, mime: &str) -> Render {
        let (answer, answered) = std::sync::mpsc::sync_channel(1);
        {
            let mut state = self.renders.state();
            if state.closed {
                return Render::Unavailable;
            }
            state.waiting = Some(Waiting {
                promise_id: self.promise_id,
                mime: mime.to_owned(),
                answer,
            });
        }
        let ask = message(broker_message::Msg::RenderClipboard(RenderClipboard {
            promise_id: self.promise_id,
            mime: mime.to_owned(),
        }));
        let sent = self.out.send(vec![ask]).is_ok();
        if sent && let Ok(render) = answered.recv_timeout(render_wait()) {
            return render;
        }
        // Either the wait ran out, or the answer came just as it did: the
        // answer is handed over under the same lock, so not both.
        let (gave_up, closed) = {
            let mut state = self.renders.state();
            let gave_up = state
                .waiting
                .take_if(|w| w.promise_id == self.promise_id && w.mime == mime)
                .is_some();
            (gave_up, state.closed)
        };
        if !gave_up && let Ok(render) = answered.try_recv() {
            return render;
        }
        if closed || !sent {
            return Render::Unavailable;
        }
        // So the agent stops sending it; the promise stays, and a later
        // paste gets what the agent fetched meanwhile.
        let cancel = message(broker_message::Msg::RenderCancelled(RenderCancelled {
            promise_id: self.promise_id,
            mime: mime.to_owned(),
        }));
        let _ = self.out.send(vec![cancel]);
        Render::Busy
    }

    fn gone(&self) {
        lock(&self.placed).tokens.remove(&self.promise_id);
        let gone = message(broker_message::Msg::PromiseGone(PromiseGone {
            promise_id: self.promise_id,
        }));
        let _ = self.out.send(vec![gone]);
    }
}

/// One connection's work.
struct Session {
    out: Out,
    read_pictures: ReadPictures,
    memory: Arc<Memory>,
    /// The clipboard's changes, and the queue of its jobs, which a task
    /// does in order: answers to reads go out in the order asked.
    clipboard: Option<(
        watch::Receiver<Option<u64>>,
        mpsc::UnboundedSender<ClipboardJob>,
    )>,
    files: PartFiles,
    outgoing: Outgoing,
    /// Where the user's transfer history is kept.
    history: Option<PathBuf>,
    launch_app: LaunchApp,
    /// Answers to the clipboard's requests for promised content.
    renders: Arc<Renders>,
    /// Cleared when the session ends, so the clipboard jobs it queued and
    /// that did not start yet never do: a promise from an agent that went
    /// must not replace what is on the clipboard, nor a newer one.
    alive: Arc<AtomicBool>,
    /// The promises this session put on the clipboard, withdrawn when it
    /// ends; the broker and its window may well live on.
    placed: Arc<Mutex<Placed>>,
    /// Promises the agent withdrew before they were placed.
    withdrawn: Arc<Mutex<HashSet<u64>>>,
    user_clipboard: Option<Arc<dyn UserClipboard>>,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::SeqCst);
        self.renders.close();
        let tokens: Vec<u64> = {
            let mut placed = lock(&self.placed);
            placed.ended = true;
            placed.tokens.drain().map(|(_, token)| token).collect()
        };
        if let Some(clipboard) = &self.user_clipboard {
            for token in tokens {
                withdraw(clipboard.clone(), token);
            }
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// Takes the promise placed as `token` off the clipboard, off the runtime.
fn withdraw(clipboard: Arc<dyn UserClipboard>, token: u64) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    runtime.spawn_blocking(move || {
        if let Err(e) = clipboard.withdraw(token) {
            tracing::debug!(error = %e, "cannot take a promise off the clipboard");
        }
    });
}

enum Event {
    Message(Option<AgentToBroker>),
    Changed(Option<u64>),
}

impl Session {
    fn start(services: &Services, memory: &Arc<Memory>, out: Out) -> Self {
        if let Some(folder) = &services.receive_folder {
            let _ = out.send(vec![message(broker_message::Msg::ReceiveFolder(
                ReceiveFolder {
                    path: folder.to_string_lossy().into_owned(),
                },
            ))]);
        }
        let alive = Arc::new(AtomicBool::new(true));
        let placed = Arc::<Mutex<Placed>>::default();
        let withdrawn = Arc::<Mutex<HashSet<u64>>>::default();
        let clipboard = services.clipboard.as_ref().map(|access| {
            let (jobs, queue) = mpsc::unbounded_channel();
            tokio::spawn(clipboard_jobs(
                access.clipboard.clone(),
                queue,
                out.clone(),
                memory.clone(),
                Shared {
                    alive: alive.clone(),
                    placed: placed.clone(),
                    withdrawn: withdrawn.clone(),
                },
            ));
            let mut changes = access.changes.clone();
            if let Some(sequence) = *changes.borrow_and_update() {
                let _ = out.send(vec![changed(sequence)]);
            }
            (changes, jobs)
        });
        let session = Self {
            out,
            read_pictures: services.read_pictures.clone(),
            memory: memory.clone(),
            clipboard,
            files: PartFiles::new(services.receive_folder.clone(), memory.cleaned.clone()),
            outgoing: Outgoing::default(),
            history: services.history_file.clone(),
            launch_app: services.launch_app.clone(),
            renders: Arc::default(),
            alive,
            placed,
            withdrawn,
            user_clipboard: services.clipboard.as_ref().map(|a| a.clipboard.clone()),
        };
        session.send_pictures();
        session
    }

    async fn run(mut self, reader: &mut IpcReader) -> Result<(), ServeError> {
        loop {
            let event = match &mut self.clipboard {
                Some((changes, _)) => tokio::select! {
                    msg = reader.recv::<AgentToBroker>() => Event::Message(msg?),
                    Ok(()) = changes.changed() => Event::Changed(*changes.borrow_and_update()),
                },
                None => Event::Message(reader.recv::<AgentToBroker>().await?),
            };
            let msg = match event {
                Event::Message(Some(msg)) => msg,
                Event::Message(None) => return Ok(()),
                Event::Changed(sequence) => {
                    if let Some(sequence) = sequence {
                        let _ = self.out.send(vec![changed(sequence)]);
                    }
                    continue;
                }
            };
            self.handle(msg, reader).await?;
        }
    }

    async fn handle(
        &mut self,
        msg: AgentToBroker,
        reader: &mut IpcReader,
    ) -> Result<(), ServeError> {
        use agent_to_broker::Msg;

        // Anything unknown is something a newer agent asks for, which this
        // broker does not do.
        let Some(msg) = msg.msg else {
            return Ok(());
        };
        let reply = match msg {
            Msg::RequestDesktopPictures(_) => {
                self.send_pictures();
                None
            }
            Msg::ReadClipboard(read) => {
                let max = read.max_bytes.min(MAX_CLIPBOARD) as usize;
                self.clipboard_job(ClipboardJob::Read(max));
                None
            }
            Msg::WriteClipboard(write) => {
                let parts = read_parts(reader, &write.parts).await?;
                self.clipboard_job(ClipboardJob::Write(parts));
                None
            }
            Msg::ClipboardData(_) => {
                return Err(ServeError::Protocol("clipboard data without a header"));
            }
            Msg::PromiseClipboard(promise) => {
                if promise.mimes.len() > MAX_PARTS {
                    return Err(ServeError::Protocol("too many clipboard parts"));
                }
                if promise.size > MAX_CLIPBOARD {
                    return Err(ServeError::Protocol("clipboard larger than allowed"));
                }
                let content = Arc::new(AgentPromise {
                    promise_id: promise.promise_id,
                    renders: self.renders.clone(),
                    out: self.out.clone(),
                    placed: self.placed.clone(),
                });
                self.clipboard_job(ClipboardJob::Promise {
                    promise_id: promise.promise_id,
                    mimes: promise.mimes,
                    size: promise.size,
                    content,
                });
                None
            }
            Msg::WithdrawPromise(withdrawn) => {
                // Not placed yet: never is. Placed already, or while this
                // is read: taken off by the job queued behind it.
                lock(&self.withdrawn).insert(withdrawn.promise_id);
                self.clipboard_job(ClipboardJob::Withdraw(withdrawn.promise_id));
                None
            }
            Msg::ClipboardRendered(rendered) => {
                if self.renders.waits(rendered.promise_id, &rendered.mime) {
                    let render = read_rendered(reader, &rendered).await?;
                    self.renders
                        .answered(rendered.promise_id, &rendered.mime, render);
                } else {
                    // Late: the clipboard stopped waiting for it.
                    tracing::debug!(
                        promise = rendered.promise_id,
                        "dropping a late rendered format"
                    );
                    skip_rendered(reader, &rendered).await?;
                }
                None
            }
            Msg::OpenFile(open) => Some(self.files.open(open).await),
            Msg::FileData(data) => self.files.write(data).await,
            Msg::FinishFile(finish) => Some(self.files.finish(finish).await),
            Msg::KeepFile(keep) => Some(self.files.keep(keep).await),
            Msg::DiscardFile(discard) => {
                self.files.discard(discard).await;
                None
            }
            Msg::CloseFile(close) => {
                self.files.close(close);
                None
            }
            Msg::OpenForSending(open) => Some(self.outgoing.open(open).await),
            Msg::HashFile(hash) => {
                let (work, out) = (self.outgoing.hash(hash.handle), self.out.clone());
                tokio::spawn(async move {
                    let _ = out.send(vec![work.await]);
                });
                None
            }
            Msg::ReadChunk(read) => Some(self.outgoing.read(read).await),
            Msg::CloseSending(close) => {
                self.outgoing.close(close.handle);
                None
            }
            Msg::LaunchApp(launch) => {
                self.launch_app(launch.reason());
                None
            }
            Msg::ListPath(list) => Some(sending::list(list).await),
            Msg::ClaimFolder(claim) => Some(files::claim_folder(claim).await),
            Msg::MakeFolder(make) => Some(files::make_folder(make).await),
            Msg::CheckFolder(check) => Some(files::check_folder(check).await),
            // Answered in order, as the agent expects.
            Msg::LoadHistory(_) => Some(history::load(self.history.clone()).await),
            Msg::SaveHistory(save) => Some(history::save(self.history.clone(), save.toml).await),
        };
        if let Some(reply) = reply {
            let _ = self.out.send(vec![reply]);
        }
        Ok(())
    }

    fn clipboard_job(&self, job: ClipboardJob) {
        match &self.clipboard {
            Some((_, jobs)) => {
                let _ = jobs.send(job);
            }
            // The agent must hear back about every read, and that a
            // promise is not held.
            None => match job {
                ClipboardJob::Read(_) => {
                    let _ = self.out.send(contents(Err(())));
                }
                ClipboardJob::Promise { promise_id, .. } => {
                    let _ = self.out.send(vec![promised(promise_id, false)]);
                }
                ClipboardJob::Write(_) | ClipboardJob::Withdraw(_) => {}
            },
        }
    }

    /// Starts the app off the async runtime, unless it was started moments
    /// ago, and says how that went.
    fn launch_app(&self, reason: LaunchReason) {
        let (launch, memory, out) = (
            self.launch_app.clone(),
            self.memory.clone(),
            self.out.clone(),
        );
        tokio::spawn(async move {
            let answer = launch_app(launch, &memory, reason).await;
            let _ = out.send(vec![message(broker_message::Msg::AppLaunched(answer))]);
        });
    }

    /// Reads the desktop pictures off the async runtime and sends them.
    /// When they cannot be read nothing is sent, and the agent keeps what
    /// it had.
    fn send_pictures(&self) {
        let (read, memory, out) = (
            self.read_pictures.clone(),
            self.memory.clone(),
            self.out.clone(),
        );
        tokio::spawn(async move {
            if let Some(pictures) = read_pictures(read, &memory).await {
                let _ = out.send(vec![message(broker_message::Msg::DesktopPictures(
                    pictures,
                ))]);
            }
        });
    }
}

fn promised(promise_id: u64, ok: bool) -> BrokerMessage {
    message(broker_message::Msg::ClipboardPromised(ClipboardPromised {
        promise_id,
        ok,
    }))
}

fn changed(sequence: u64) -> BrokerMessage {
    message(broker_message::Msg::ClipboardChanged(ClipboardChanged {
        sequence,
    }))
}

/// The clipboard's content as a header and its pieces, with the files
/// copied in File Explorer that may go (`copied_paths`); `Err` if it could
/// not be read.
fn contents(read: Result<(ClipboardRead, Vec<String>), ()>) -> Vec<BrokerMessage> {
    let (read, files) = match read {
        Ok((read, files)) => (Ok(read), copied_paths(files)),
        Err(()) => (Err(()), Vec::new()),
    };
    let header = |state: ClipboardState, parts: Vec<ClipboardPart>, size: u64| {
        // Nothing of sensitive content goes, its files included.
        let files = match state {
            ClipboardState::Content | ClipboardState::Empty | ClipboardState::TooLarge => {
                files.clone()
            }
            _ => Vec::new(),
        };
        message(broker_message::Msg::ClipboardContents(ClipboardContents {
            state: state.into(),
            parts,
            size,
            files,
        }))
    };
    match read {
        Ok(ClipboardRead::Content(items)) => {
            let parts = items
                .iter()
                .map(|(mime, data)| ClipboardPart {
                    mime: mime.clone(),
                    size: data.len() as u64,
                })
                .collect();
            let mut group = vec![header(ClipboardState::Content, parts, 0)];
            for (_, data) in &items {
                for piece in data.chunks(PIECE) {
                    group.push(message(broker_message::Msg::ClipboardData(ClipboardData {
                        data: piece.to_vec(),
                    })));
                }
            }
            group
        }
        Ok(ClipboardRead::Empty) => vec![header(ClipboardState::Empty, vec![], 0)],
        Ok(ClipboardRead::Sensitive) => vec![header(ClipboardState::Sensitive, vec![], 0)],
        Ok(ClipboardRead::TooLarge(size)) => vec![header(ClipboardState::TooLarge, vec![], size)],
        Err(()) => vec![header(ClipboardState::Unavailable, vec![], 0)],
    }
}

/// What the clipboard jobs share with their session.
struct Shared {
    alive: Arc<AtomicBool>,
    placed: Arc<Mutex<Placed>>,
    withdrawn: Arc<Mutex<HashSet<u64>>>,
}

/// Does the clipboard's jobs in order, until the connection ends.
async fn clipboard_jobs(
    clipboard: Arc<dyn UserClipboard>,
    mut jobs: mpsc::UnboundedReceiver<ClipboardJob>,
    out: Out,
    memory: Arc<Memory>,
    shared: Shared,
) {
    while let Some(job) = jobs.recv().await {
        // Its session ended: nobody waits for the answer, and the
        // clipboard is no longer that agent's to change.
        if !shared.alive.load(Ordering::SeqCst) {
            return;
        }
        let clipboard = clipboard.clone();
        match job {
            ClipboardJob::Read(max) => {
                let read = tokio::task::spawn_blocking(move || clipboard.read_with_files(max))
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()));
                let read = note_clipboard(&memory, read, "read");
                if out.send(contents(read)).is_err() {
                    return;
                }
            }
            ClipboardJob::Promise {
                promise_id,
                mimes,
                size,
                content,
            } => {
                // Withdrawn before its turn: the clipboard is left alone.
                if lock(&shared.withdrawn).remove(&promise_id) {
                    let _ = out.send(vec![promised(promise_id, false)]);
                    continue;
                }
                let placing = clipboard.clone();
                let done =
                    tokio::task::spawn_blocking(move || placing.promise(&mimes, size, content))
                        .await
                        .unwrap_or_else(|e| Err(e.to_string()));
                let token = note_clipboard(&memory, done, "promise content on").ok();
                if let Some(token) = token {
                    let mut placed = lock(&shared.placed);
                    if placed.ended {
                        // Its session ended meanwhile: nothing renders it.
                        drop(placed);
                        withdraw(clipboard, token);
                    } else {
                        placed.tokens.insert(promise_id, token);
                    }
                }
                if out
                    .send(vec![promised(promise_id, token.is_some())])
                    .is_err()
                {
                    return;
                }
            }
            ClipboardJob::Withdraw(promise_id) => {
                lock(&shared.withdrawn).remove(&promise_id);
                let token = lock(&shared.placed).tokens.remove(&promise_id);
                if let Some(token) = token {
                    let done = tokio::task::spawn_blocking(move || clipboard.withdraw(token))
                        .await
                        .unwrap_or_else(|e| Err(e.to_string()));
                    let _ = note_clipboard(&memory, done, "withdraw a promise from");
                }
            }
            ClipboardJob::Write(parts) => {
                let written = tokio::task::spawn_blocking(move || clipboard.write(&parts))
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()));
                let _ = note_clipboard(&memory, written, "write");
            }
        }
    }
}

/// Logs a clipboard failure once for as long as it lasts.
fn note_clipboard<T>(memory: &Memory, result: Result<T, String>, doing: &str) -> Result<T, ()> {
    let mut last = memory.clipboard_failures.lock().unwrap();
    match result {
        Ok(value) => {
            if last.take().is_some() {
                tracing::info!("the clipboard works again");
            }
            Ok(value)
        }
        Err(e) => {
            if last.as_deref() != Some(e.as_str()) {
                tracing::warn!(error = %e, "cannot {doing} the clipboard");
                *last = Some(e);
            }
            Err(())
        }
    }
}

/// The content a WriteClipboard announces, from the ClipboardData that
/// follow it.
async fn read_parts(
    reader: &mut IpcReader,
    parts: &[ClipboardPart],
) -> Result<Vec<(String, Vec<u8>)>, ServeError> {
    if parts.len() > MAX_PARTS {
        return Err(ServeError::Protocol("too many clipboard parts"));
    }
    let total = parts
        .iter()
        .try_fold(0u64, |sum, p| sum.checked_add(p.size));
    if !total.is_some_and(|total| total <= MAX_CLIPBOARD) {
        return Err(ServeError::Protocol("clipboard larger than allowed"));
    }
    let mut content = Vec::with_capacity(parts.len());
    for part in parts {
        let size = part.size as usize;
        let mut data = Vec::with_capacity(size);
        while data.len() < size {
            let piece = match reader.recv::<AgentToBroker>().await? {
                Some(AgentToBroker {
                    msg: Some(agent_to_broker::Msg::ClipboardData(piece)),
                }) => piece.data,
                _ => return Err(ServeError::Protocol("clipboard ended early")),
            };
            if data.len() + piece.len() > size {
                return Err(ServeError::Protocol("clipboard part longer than announced"));
            }
            data.extend_from_slice(&piece);
        }
        content.push((part.mime.clone(), data));
    }
    Ok(content)
}

/// What a ClipboardRendered announces, with its content from the
/// ClipboardData that follow it.
async fn read_rendered(
    reader: &mut IpcReader,
    rendered: &ClipboardRendered,
) -> Result<Render, ServeError> {
    if !rendered.ok {
        if rendered.size != 0 {
            return Err(ServeError::Protocol("clipboard data for nothing rendered"));
        }
        return Ok(if rendered.busy {
            Render::Busy
        } else {
            Render::Unavailable
        });
    }
    let part = ClipboardPart {
        mime: rendered.mime.clone(),
        size: rendered.size,
    };
    let mut parts = read_parts(reader, std::slice::from_ref(&part)).await?;
    Ok(parts
        .pop()
        .map_or(Render::Unavailable, |(_, data)| Render::Content(data)))
}

/// Reads past the ClipboardData of a ClipboardRendered nobody waits for,
/// keeping none of it, held to the same limits.
async fn skip_rendered(
    reader: &mut IpcReader,
    rendered: &ClipboardRendered,
) -> Result<(), ServeError> {
    let size = if rendered.ok { rendered.size } else { 0 };
    if !rendered.ok && rendered.size != 0 {
        return Err(ServeError::Protocol("clipboard data for nothing rendered"));
    }
    if size > MAX_CLIPBOARD {
        return Err(ServeError::Protocol("clipboard larger than allowed"));
    }
    let mut read = 0u64;
    while read < size {
        let piece = match reader.recv::<AgentToBroker>().await? {
            Some(AgentToBroker {
                msg: Some(agent_to_broker::Msg::ClipboardData(piece)),
            }) => piece.data,
            _ => return Err(ServeError::Protocol("clipboard ended early")),
        };
        read += piece.len() as u64;
        if read > size {
            return Err(ServeError::Protocol("clipboard part longer than announced"));
        }
    }
    Ok(())
}

/// Starts the app, at most once per [`app::INTERVAL`].
async fn launch_app(launch: LaunchApp, memory: &Memory, reason: LaunchReason) -> AppLaunched {
    let answer = |ok, error: String, started| AppLaunched { ok, error, started };
    // Before the gate: a start refused does not hold off the next one.
    let quit = tokio::task::spawn_blocking(app::quit_by_user)
        .await
        .unwrap_or(false);
    if app::refused(reason, quit) {
        tracing::debug!("the user quit the app; not starting it for files on the clipboard");
        return answer(false, "the user quit the app".to_owned(), false);
    }
    if !memory.app.lock().unwrap().open(Instant::now()) {
        tracing::debug!("the app was started moments ago");
        return answer(true, String::new(), false);
    }
    match tokio::task::spawn_blocking(move || launch()).await {
        Ok(Ok(Launch::Started)) => answer(true, String::new(), true),
        Ok(Ok(Launch::AlreadyRunning)) => {
            tracing::debug!("the app runs already");
            answer(true, String::new(), false)
        }
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "cannot start the app");
            answer(false, e, false)
        }
        Err(e) => answer(false, e.to_string(), false),
    }
}

/// Reads the desktop pictures off the async runtime. `None` if they cannot
/// be read.
async fn read_pictures(read: ReadPictures, memory: &Memory) -> Option<DesktopPictures> {
    let failure = match tokio::task::spawn_blocking(move || read()).await {
        Ok(Ok(pictures)) => {
            if memory.failures.lock().unwrap().take().is_some() {
                tracing::info!("reading the desktop pictures again");
            }
            tracing::debug!(count = pictures.len(), "read the desktop pictures");
            return Some(DesktopPictures {
                pictures: pictures
                    .into_iter()
                    .map(|(display_id, jpeg)| DisplayPicture { display_id, jpeg })
                    .collect(),
            });
        }
        Ok(Err(e)) => e,
        Err(e) => e.to_string(),
    };
    let mut last = memory.failures.lock().unwrap();
    if last.as_deref() != Some(failure.as_str()) {
        tracing::warn!(error = %failure, "cannot read the desktop pictures");
        *last = Some(failure);
    }
    None
}

#[cfg(all(test, unix))]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use iw_ipc::IpcListener;
    use iw_proto::service::v1::{
        CloseSending, FileData, FinishFile, HashFile, KeepFile, LaunchApp as LaunchAppMsg,
        LoadHistory, OpenFailure, OpenFile, OpenForSending, PromiseClipboard, ReadChunk,
        ReadClipboard, RequestDesktopPictures, SaveHistory, WithdrawPromise, WriteClipboard,
    };

    use super::*;

    fn ask() -> AgentToBroker {
        AgentToBroker {
            msg: Some(agent_to_broker::Msg::RequestDesktopPictures(
                RequestDesktopPictures {},
            )),
        }
    }

    fn agent_says(msg: agent_to_broker::Msg) -> AgentToBroker {
        AgentToBroker { msg: Some(msg) }
    }

    async fn next(agent: &mut IpcConnection) -> broker_message::Msg {
        tokio::time::timeout(Duration::from_secs(5), agent.recv::<BrokerMessage>())
            .await
            .expect("the broker answers")
            .unwrap()
            .unwrap()
            .msg
            .unwrap()
    }

    async fn pictures_from(agent: &mut IpcConnection) -> Vec<(String, Vec<u8>)> {
        match next(agent).await {
            broker_message::Msg::DesktopPictures(p) => p
                .pictures
                .into_iter()
                .map(|p| (p.display_id, p.jpeg))
                .collect(),
            other => panic!("expected the desktop pictures, got {other:?}"),
        }
    }

    /// What the desktops show, or why that cannot be read.
    type Shown = Arc<Mutex<Result<Vec<(String, Vec<u8>)>, String>>>;

    /// A reader that shows what `shown` holds, counting its reads.
    fn reader(shown: &Shown) -> (ReadPictures, Arc<AtomicUsize>) {
        let reads = Arc::new(AtomicUsize::new(0));
        let read: ReadPictures = Arc::new({
            let (shown, reads) = (shown.clone(), reads.clone());
            move || {
                reads.fetch_add(1, Ordering::SeqCst);
                shown
                    .lock()
                    .unwrap()
                    .clone()
                    .map(|p| p.into_iter().collect())
            }
        });
        (read, reads)
    }

    fn services(read_pictures: ReadPictures) -> Services {
        Services {
            read_pictures,
            clipboard: None,
            receive_folder: None,
            history_file: None,
            launch_app: no_app(),
        }
    }

    /// No app is installed.
    fn no_app() -> LaunchApp {
        Arc::new(|| Err("the app is not installed".to_owned()))
    }

    /// Connects a broker with `services` to a new agent's socket in `dir`.
    async fn connected(
        dir: &Path,
        services: Services,
    ) -> (
        IpcConnection,
        tokio::task::JoinHandle<Result<(), ServeError>>,
        Arc<Memory>,
    ) {
        let path = dir.join("broker.sock");
        let listener = IpcListener::bind(&path).unwrap();
        let memory = Arc::new(Memory::default());
        let broker = tokio::spawn({
            let memory = memory.clone();
            async move {
                let conn = connect(&path).await.unwrap();
                serve(conn, &services, &memory).await
            }
        });
        (listener.accept().await.unwrap(), broker, memory)
    }

    #[tokio::test]
    async fn sends_the_pictures_once_connected_and_whenever_asked() {
        let dir = tempfile::tempdir().unwrap();
        let shown: Shown = Arc::new(Mutex::new(Ok(vec![
            (r"\\.\DISPLAY1".to_owned(), vec![0xff, 0xd8, 0xff, 1]),
            (r"\\.\DISPLAY2".to_owned(), vec![0xff, 0xd8, 0xff, 2]),
        ])));
        let (read, reads) = reader(&shown);
        let (mut agent, broker, _) = connected(dir.path(), services(read)).await;

        assert_eq!(
            pictures_from(&mut agent).await,
            vec![
                (r"\\.\DISPLAY1".to_owned(), vec![0xff, 0xd8, 0xff, 1]),
                (r"\\.\DISPLAY2".to_owned(), vec![0xff, 0xd8, 0xff, 2]),
            ]
        );
        // Not read again until asked.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(reads.load(Ordering::SeqCst), 1);

        // The user picks a solid color.
        *shown.lock().unwrap() = Ok(Vec::new());
        agent.send(&ask()).await.unwrap();
        assert_eq!(pictures_from(&mut agent).await, Vec::new());

        // A message from a newer agent is passed over.
        agent.send(&AgentToBroker { msg: None }).await.unwrap();
        agent.send(&ask()).await.unwrap();
        assert_eq!(pictures_from(&mut agent).await, Vec::new());
        assert_eq!(reads.load(Ordering::SeqCst), 3);

        drop(agent);
        broker.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn sends_nothing_when_the_pictures_cannot_be_read() {
        let dir = tempfile::tempdir().unwrap();
        let shown: Shown = Arc::new(Mutex::new(Err("the shell is not running".to_owned())));
        let (read, reads) = reader(&shown);
        let (mut agent, broker, memory) = connected(dir.path(), services(read)).await;

        // The first read failed; once the shell is up, the next request
        // gets them.
        while reads.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(memory.failures.lock().unwrap().is_some());
        *shown.lock().unwrap() = Ok(vec![(r"\\.\DISPLAY1".to_owned(), vec![0xff, 0xd8, 0xff])]);
        agent.send(&ask()).await.unwrap();
        assert_eq!(
            pictures_from(&mut agent).await,
            vec![(r"\\.\DISPLAY1".to_owned(), vec![0xff, 0xd8, 0xff])]
        );

        drop(agent);
        broker.await.unwrap().unwrap();
        assert_eq!(
            *memory.failures.lock().unwrap(),
            None,
            "the failure is over"
        );
    }

    /// Clipboard content by MIME type.
    type Parts = Vec<(String, Vec<u8>)>;

    /// A promise put on the fake clipboard: its formats, its size, and
    /// what renders it.
    type Promise = (Vec<String>, u64, Arc<dyn PromisedContent>);

    /// A clipboard in memory that counts reads and records writes and
    /// promises.
    #[derive(Default)]
    struct FakeClipboard {
        shows: Mutex<Option<ClipboardRead>>,
        written: Mutex<Vec<Parts>>,
        reads: AtomicUsize,
        promised: Mutex<Vec<Promise>>,
        /// Promising fails, as when another app keeps the clipboard open.
        refuses: AtomicBool,
        /// Holds the next write back until released.
        gate: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
        /// A write is under way.
        writing: AtomicBool,
        /// Holds the next promise back until released.
        promise_gate: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
        /// A promise is being placed.
        promising: AtomicBool,
        /// The tokens of the promises withdrawn.
        withdrawn: Mutex<Vec<u64>>,
    }

    impl UserClipboard for FakeClipboard {
        fn read(&self, max_len: usize) -> Result<ClipboardRead, String> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            match self.shows.lock().unwrap().clone() {
                Some(ClipboardRead::Content(items))
                    if items.iter().map(|(_, d)| d.len()).sum::<usize>() > max_len =>
                {
                    Ok(ClipboardRead::TooLarge(
                        items.iter().map(|(_, d)| d.len() as u64).sum(),
                    ))
                }
                Some(read) => Ok(read),
                None => Err("another app keeps the clipboard open".to_owned()),
            }
        }

        fn write(&self, parts: &[(String, Vec<u8>)]) -> Result<(), String> {
            self.writing.store(true, Ordering::SeqCst);
            let gate = self.gate.lock().unwrap().take();
            if let Some(gate) = gate {
                let _ = gate.recv();
            }
            self.written.lock().unwrap().push(parts.to_vec());
            Ok(())
        }

        fn promise(
            &self,
            mimes: &[String],
            size: u64,
            content: Arc<dyn PromisedContent>,
        ) -> Result<u64, String> {
            self.promising.store(true, Ordering::SeqCst);
            let gate = self.promise_gate.lock().unwrap().take();
            if let Some(gate) = gate {
                let _ = gate.recv();
            }
            if self.refuses.load(Ordering::SeqCst) {
                return Err("another app keeps the clipboard open".to_owned());
            }
            let mut promised = self.promised.lock().unwrap();
            promised.push((mimes.to_vec(), size, content));
            // Its token is its place in the list, from 1.
            Ok(promised.len() as u64)
        }

        fn withdraw(&self, token: u64) -> Result<(), String> {
            self.withdrawn.lock().unwrap().push(token);
            Ok(())
        }
    }

    /// Waits for the fake clipboard to have withdrawn `tokens`.
    async fn withdrawn(clipboard: &FakeClipboard, tokens: &[u64]) {
        for _ in 0..500 {
            if clipboard.withdrawn.lock().unwrap().as_slice() == tokens {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!(
            "withdrew {:?}, not {tokens:?}",
            clipboard.withdrawn.lock().unwrap()
        );
    }

    fn withdraw_promise(promise_id: u64) -> AgentToBroker {
        agent_says(agent_to_broker::Msg::WithdrawPromise(WithdrawPromise {
            promise_id,
        }))
    }

    /// The broker's answer to a PromiseClipboard.
    async fn promise_answer(agent: &mut IpcConnection) -> ClipboardPromised {
        match next(agent).await {
            broker_message::Msg::ClipboardPromised(answer) => answer,
            other => panic!("expected an answer to a promise, got {other:?}"),
        }
    }

    fn with_clipboard(clipboard: &Arc<FakeClipboard>) -> Services {
        let (read, _) = reader(&Arc::new(Mutex::new(Ok(Vec::new()))));
        Services {
            clipboard: Some(ClipboardAccess {
                clipboard: clipboard.clone(),
                changes: watch::channel(None).1,
            }),
            ..services(read)
        }
    }

    fn promise_clipboard(promise_id: u64, mimes: &[&str], size: u64) -> AgentToBroker {
        agent_says(agent_to_broker::Msg::PromiseClipboard(PromiseClipboard {
            promise_id,
            mimes: mimes.iter().map(|m| (*m).to_owned()).collect(),
            size,
        }))
    }

    /// The agent's answer to a RenderClipboard: a header, and the bytes in
    /// pieces.
    async fn render_answer(agent: &mut IpcConnection, promise_id: u64, mime: &str, answer: Render) {
        let (ok, busy, data) = match &answer {
            Render::Content(data) => (true, false, data.as_slice()),
            Render::Busy => (false, true, &[][..]),
            Render::Unavailable => (false, false, &[][..]),
        };
        agent
            .send(&agent_says(agent_to_broker::Msg::ClipboardRendered(
                ClipboardRendered {
                    promise_id,
                    mime: mime.to_owned(),
                    ok,
                    size: data.len() as u64,
                    busy,
                },
            )))
            .await
            .unwrap();
        for piece in data.chunks(PIECE) {
            agent
                .send(&agent_says(agent_to_broker::Msg::ClipboardData(
                    ClipboardData {
                        data: piece.to_vec(),
                    },
                )))
                .await
                .unwrap();
        }
    }

    /// Waits for the fake clipboard to hold a promise, and returns the
    /// newest.
    async fn promised(clipboard: &FakeClipboard) -> Promise {
        for _ in 0..500 {
            if let Some(promise) = clipboard.promised.lock().unwrap().last() {
                return (promise.0.clone(), promise.1, promise.2.clone());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("nothing was promised");
    }

    /// Renders `mime` of `content` off the runtime, as the clipboard's
    /// thread does on WM_RENDERFORMAT.
    fn paste(content: &Arc<dyn PromisedContent>, mime: &str) -> tokio::task::JoinHandle<Render> {
        let (content, mime) = (content.clone(), mime.to_owned());
        tokio::task::spawn_blocking(move || content.render(&mime))
    }

    /// The next request for a promised format the broker sends.
    async fn asked_to_render(agent: &mut IpcConnection) -> RenderClipboard {
        match next(agent).await {
            broker_message::Msg::RenderClipboard(asked) => asked,
            other => panic!("expected a request for a promised format, got {other:?}"),
        }
    }

    const TEXT: &str = "text/plain;charset=utf-8";

    #[tokio::test]
    async fn promised_formats_are_asked_of_the_agent_as_apps_paste_them() {
        let dir = tempfile::tempdir().unwrap();
        let clipboard = Arc::new(FakeClipboard::default());
        let (mut agent, broker, _) = connected(dir.path(), with_clipboard(&clipboard)).await;
        assert!(matches!(
            next(&mut agent).await,
            broker_message::Msg::DesktopPictures(_)
        ));

        agent
            .send(&promise_clipboard(3, &[TEXT, "image/png"], 9))
            .await
            .unwrap();
        assert!(matches!(
            next(&mut agent).await,
            broker_message::Msg::ClipboardPromised(ClipboardPromised {
                promise_id: 3,
                ok: true
            })
        ));
        let (mimes, size, content) = promised(&clipboard).await;
        assert_eq!(mimes, vec![TEXT.to_owned(), "image/png".to_owned()]);
        assert_eq!(size, 9);

        // An app pastes the text: the agent is asked, and its answer, in
        // pieces, is what the app gets.
        let big = vec![b'x'; 2 * PIECE + 1];
        let pasting = paste(&content, TEXT);
        let asked = asked_to_render(&mut agent).await;
        assert_eq!((asked.promise_id, asked.mime.as_str()), (3, TEXT));
        render_answer(&mut agent, 3, TEXT, Render::Content(big.clone())).await;
        assert_eq!(pasting.await.unwrap(), Render::Content(big));

        // What the agent cannot have, the app does not get; nor what it is
        // still busy fetching, which is no failure.
        for answer in [Render::Unavailable, Render::Busy] {
            let pasting = paste(&content, "image/png");
            asked_to_render(&mut agent).await;
            render_answer(&mut agent, 3, "image/png", answer.clone()).await;
            assert_eq!(pasting.await.unwrap(), answer);
        }

        // The agent takes too long: that paste gets nothing, but the
        // promise stays; the broker says it gave up, and reads past the
        // answer that comes late.
        let pasting = paste(&content, TEXT);
        asked_to_render(&mut agent).await;
        assert_eq!(pasting.await.unwrap(), Render::Busy);
        assert!(matches!(
            next(&mut agent).await,
            broker_message::Msg::RenderCancelled(RenderCancelled { promise_id: 3, .. })
        ));
        render_answer(&mut agent, 3, TEXT, Render::Content(vec![b'y'; PIECE + 1])).await;
        agent.send(&ask()).await.unwrap();
        assert!(matches!(
            next(&mut agent).await,
            broker_message::Msg::DesktopPictures(_)
        ));
        // Pasted again, it gets what the agent fetched meanwhile.
        let pasting = paste(&content, TEXT);
        asked_to_render(&mut agent).await;
        render_answer(&mut agent, 3, TEXT, Render::Content(b"later".to_vec())).await;
        assert_eq!(pasting.await.unwrap(), Render::Content(b"later".to_vec()));

        // Copied over: the agent hears, and the promise is not withdrawn
        // when the agent goes, since the clipboard holds something else.
        content.gone();
        assert!(matches!(
            next(&mut agent).await,
            broker_message::Msg::PromiseGone(PromiseGone { promise_id: 3 })
        ));

        // The agent goes while an app waits: the app gets nothing, at once.
        let pasting = paste(&content, TEXT);
        asked_to_render(&mut agent).await;
        drop(agent);
        broker.await.unwrap().unwrap();
        let started = Instant::now();
        assert_eq!(pasting.await.unwrap(), Render::Unavailable);
        assert!(started.elapsed() < Duration::from_secs(5));
        // And so does any app pasting later.
        assert_eq!(paste(&content, TEXT).await.unwrap(), Render::Unavailable);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(clipboard.withdrawn.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_withdrawn_promise_never_stays_on_the_clipboard() {
        let dir = tempfile::tempdir().unwrap();
        let clipboard = Arc::new(FakeClipboard::default());
        let (release, gate) = std::sync::mpsc::channel();
        *clipboard.gate.lock().unwrap() = Some(gate);
        let (mut agent, broker, _) = connected(dir.path(), with_clipboard(&clipboard)).await;
        let _pictures = next(&mut agent).await;

        // Withdrawn while it waits its turn behind a slow write: never
        // placed, and the agent is told so.
        agent
            .send(&agent_says(agent_to_broker::Msg::WriteClipboard(
                WriteClipboard {
                    parts: vec![ClipboardPart {
                        mime: TEXT.to_owned(),
                        size: 2,
                    }],
                },
            )))
            .await
            .unwrap();
        agent
            .send(&agent_says(agent_to_broker::Msg::ClipboardData(
                ClipboardData {
                    data: b"hi".to_vec(),
                },
            )))
            .await
            .unwrap();
        agent.send(&promise_clipboard(1, &[TEXT], 1)).await.unwrap();
        agent.send(&withdraw_promise(1)).await.unwrap();
        while !clipboard.writing.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        release.send(()).unwrap();
        let answer = promise_answer(&mut agent).await;
        assert_eq!((answer.promise_id, answer.ok), (1, false));
        assert!(clipboard.promised.lock().unwrap().is_empty());

        // Withdrawn after it was placed: taken off again.
        agent.send(&promise_clipboard(2, &[TEXT], 1)).await.unwrap();
        assert!(promise_answer(&mut agent).await.ok);
        agent.send(&withdraw_promise(2)).await.unwrap();
        withdrawn(&clipboard, &[1]).await;

        // Withdrawn while it is being placed: taken off once it is there.
        let (release, gate) = std::sync::mpsc::channel();
        *clipboard.promise_gate.lock().unwrap() = Some(gate);
        clipboard.promising.store(false, Ordering::SeqCst);
        agent.send(&promise_clipboard(3, &[TEXT], 1)).await.unwrap();
        while !clipboard.promising.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        agent.send(&withdraw_promise(3)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        release.send(()).unwrap();
        assert!(promise_answer(&mut agent).await.ok);
        withdrawn(&clipboard, &[1, 2]).await;

        drop(agent);
        broker.await.unwrap().unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(*clipboard.withdrawn.lock().unwrap(), vec![1, 2]);
    }

    #[tokio::test]
    async fn the_promises_of_a_connection_that_ends_are_withdrawn() {
        let dir = tempfile::tempdir().unwrap();
        let clipboard = Arc::new(FakeClipboard::default());
        let (mut agent, broker, _) = connected(dir.path(), with_clipboard(&clipboard)).await;
        let _pictures = next(&mut agent).await;
        agent.send(&promise_clipboard(1, &[TEXT], 1)).await.unwrap();
        assert!(promise_answer(&mut agent).await.ok);
        // The agent goes, the broker and its clipboard stay: what it
        // promised is taken off, as nothing can render it.
        drop(agent);
        broker.await.unwrap().unwrap();
        withdrawn(&clipboard, &[1]).await;

        // Going while its promise is being placed: taken off once there.
        let (release, gate) = std::sync::mpsc::channel();
        *clipboard.promise_gate.lock().unwrap() = Some(gate);
        clipboard.promising.store(false, Ordering::SeqCst);
        let dir = tempfile::tempdir().unwrap();
        let (mut agent, broker, _) = connected(dir.path(), with_clipboard(&clipboard)).await;
        let _pictures = next(&mut agent).await;
        agent.send(&promise_clipboard(1, &[TEXT], 1)).await.unwrap();
        while !clipboard.promising.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        drop(agent);
        broker.await.unwrap().unwrap();
        release.send(()).unwrap();
        withdrawn(&clipboard, &[1, 2]).await;
    }

    #[tokio::test]
    async fn a_promise_the_clipboard_cannot_hold_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let clipboard = Arc::new(FakeClipboard::default());
        clipboard.refuses.store(true, Ordering::SeqCst);
        let (mut agent, broker, _) = connected(dir.path(), with_clipboard(&clipboard)).await;
        let _pictures = next(&mut agent).await;
        agent.send(&promise_clipboard(5, &[TEXT], 1)).await.unwrap();
        assert!(matches!(
            next(&mut agent).await,
            broker_message::Msg::ClipboardPromised(ClipboardPromised {
                promise_id: 5,
                ok: false
            })
        ));
        drop(agent);
        broker.await.unwrap().unwrap();

        // Without a clipboard at all, likewise.
        let dir = tempfile::tempdir().unwrap();
        let (read, _) = reader(&Arc::new(Mutex::new(Ok(Vec::new()))));
        let (mut agent, broker, _) = connected(dir.path(), services(read)).await;
        let _pictures = next(&mut agent).await;
        agent.send(&promise_clipboard(6, &[TEXT], 1)).await.unwrap();
        assert!(matches!(
            next(&mut agent).await,
            broker_message::Msg::ClipboardPromised(ClipboardPromised {
                promise_id: 6,
                ok: false
            })
        ));
        drop(agent);
        broker.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn what_a_session_left_queued_is_not_done_once_it_ended() {
        let dir = tempfile::tempdir().unwrap();
        let clipboard = Arc::new(FakeClipboard::default());
        let (release, gate) = std::sync::mpsc::channel();
        *clipboard.gate.lock().unwrap() = Some(gate);
        let (mut agent, broker, _) = connected(dir.path(), with_clipboard(&clipboard)).await;
        let _pictures = next(&mut agent).await;
        // A write the clipboard takes its time with, and a promise behind
        // it.
        agent
            .send(&agent_says(agent_to_broker::Msg::WriteClipboard(
                WriteClipboard {
                    parts: vec![ClipboardPart {
                        mime: TEXT.to_owned(),
                        size: 2,
                    }],
                },
            )))
            .await
            .unwrap();
        agent
            .send(&agent_says(agent_to_broker::Msg::ClipboardData(
                ClipboardData {
                    data: b"hi".to_vec(),
                },
            )))
            .await
            .unwrap();
        agent.send(&promise_clipboard(7, &[TEXT], 1)).await.unwrap();
        while !clipboard.writing.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // The agent goes: the promise never replaces what is there.
        drop(agent);
        broker.await.unwrap().unwrap();
        release.send(()).unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(clipboard.written.lock().unwrap().len(), 1);
        assert!(clipboard.promised.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn promises_are_held_to_the_clipboards_limits() {
        let rendered = |ok, size| {
            agent_says(agent_to_broker::Msg::ClipboardRendered(ClipboardRendered {
                promise_id: 1,
                mime: TEXT.to_owned(),
                ok,
                size,
                busy: false,
            }))
        };
        for broken in [
            promise_clipboard(1, &[TEXT], MAX_CLIPBOARD + 1),
            promise_clipboard(1, &[TEXT; MAX_PARTS + 1], 1),
            rendered(false, 3),
            rendered(true, MAX_CLIPBOARD + 1),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let clipboard = Arc::new(FakeClipboard::default());
            let (mut agent, broker, _) = connected(dir.path(), with_clipboard(&clipboard)).await;
            agent.send(&broken).await.unwrap();
            assert!(matches!(
                broker.await.unwrap(),
                Err(ServeError::Protocol(_))
            ));
            assert!(clipboard.promised.lock().unwrap().is_empty());
        }
    }

    fn read_clipboard(max_bytes: u64) -> AgentToBroker {
        agent_says(agent_to_broker::Msg::ReadClipboard(ReadClipboard {
            max_bytes,
        }))
    }

    /// The next clipboard the broker sends, put back together.
    async fn clipboard_from(agent: &mut IpcConnection) -> (ClipboardContents, Vec<Vec<u8>>) {
        let broker_message::Msg::ClipboardContents(header) = next(agent).await else {
            panic!("expected clipboard contents");
        };
        let mut parts = Vec::new();
        for part in &header.parts {
            let mut data = Vec::new();
            while (data.len() as u64) < part.size {
                let broker_message::Msg::ClipboardData(piece) = next(agent).await else {
                    panic!("expected clipboard data");
                };
                assert!(piece.data.len() <= PIECE);
                data.extend(piece.data);
            }
            parts.push(data);
        }
        (header, parts)
    }

    #[tokio::test]
    async fn reports_changes_reads_in_order_and_writes_in_pieces() {
        let dir = tempfile::tempdir().unwrap();
        let clipboard = Arc::new(FakeClipboard::default());
        let big = vec![7u8; 2 * PIECE + 10];
        *clipboard.shows.lock().unwrap() = Some(ClipboardRead::Content(vec![
            ("text/plain;charset=utf-8".to_owned(), b"hello".to_vec()),
            ("image/png".to_owned(), big.clone()),
        ]));
        let (changes_tx, changes) = watch::channel(Some(41));
        let (read, _) = reader(&Arc::new(Mutex::new(Ok(Vec::new()))));
        let services = Services {
            read_pictures: read,
            clipboard: Some(ClipboardAccess {
                clipboard: clipboard.clone(),
                changes,
            }),
            receive_folder: Some(dir.path().join("Downloads")),
            history_file: None,
            launch_app: no_app(),
        };
        let (mut agent, broker, _) = connected(dir.path(), services).await;

        // Once connected: the folder, the clipboard's sequence number and
        // the pictures, though the pictures are read off the runtime.
        let mut first = Vec::new();
        for _ in 0..3 {
            first.push(next(&mut agent).await);
        }
        assert!(first.iter().any(|m| matches!(m,
            broker_message::Msg::ReceiveFolder(f) if Path::new(&f.path) == dir.path().join("Downloads"))));
        assert!(first.iter().any(|m| matches!(m,
            broker_message::Msg::ClipboardChanged(c) if c.sequence == 41)));
        assert!(
            first
                .iter()
                .any(|m| matches!(m, broker_message::Msg::DesktopPictures(_)))
        );
        assert_eq!(
            clipboard.reads.load(Ordering::SeqCst),
            0,
            "nothing read unasked"
        );

        // Reads are answered in order: content, too large, unreadable.
        agent.send(&read_clipboard(8 << 20)).await.unwrap();
        agent.send(&read_clipboard(100)).await.unwrap();
        let (header, parts) = clipboard_from(&mut agent).await;
        assert_eq!(header.state(), ClipboardState::Content);
        assert_eq!(parts, vec![b"hello".to_vec(), big.clone()]);
        let (header, _) = clipboard_from(&mut agent).await;
        assert_eq!(header.state(), ClipboardState::TooLarge);
        assert_eq!(header.size, big.len() as u64 + 5);
        *clipboard.shows.lock().unwrap() = None;
        agent.send(&read_clipboard(100)).await.unwrap();
        assert_eq!(
            clipboard_from(&mut agent).await.0.state(),
            ClipboardState::Unavailable
        );

        // Another app copies.
        changes_tx.send_replace(Some(42));
        assert!(matches!(next(&mut agent).await,
            broker_message::Msg::ClipboardChanged(c) if c.sequence == 42));

        // A write arrives in pieces.
        agent
            .send(&agent_says(agent_to_broker::Msg::WriteClipboard(
                WriteClipboard {
                    parts: vec![ClipboardPart {
                        mime: "image/png".to_owned(),
                        size: big.len() as u64,
                    }],
                },
            )))
            .await
            .unwrap();
        for piece in big.chunks(PIECE) {
            agent
                .send(&agent_says(agent_to_broker::Msg::ClipboardData(
                    ClipboardData {
                        data: piece.to_vec(),
                    },
                )))
                .await
                .unwrap();
        }
        // Reads come after earlier writes.
        *clipboard.shows.lock().unwrap() = Some(ClipboardRead::Empty);
        agent.send(&read_clipboard(100)).await.unwrap();
        assert_eq!(
            clipboard_from(&mut agent).await.0.state(),
            ClipboardState::Empty
        );
        assert_eq!(
            *clipboard.written.lock().unwrap(),
            vec![vec![("image/png".to_owned(), big)]]
        );

        // Pieces that do not add up end the connection.
        agent
            .send(&agent_says(agent_to_broker::Msg::WriteClipboard(
                WriteClipboard {
                    parts: vec![ClipboardPart {
                        mime: "text/plain;charset=utf-8".to_owned(),
                        size: 2,
                    }],
                },
            )))
            .await
            .unwrap();
        agent.send(&ask()).await.unwrap();
        assert!(matches!(
            broker.await.unwrap(),
            Err(ServeError::Protocol(_))
        ));
    }

    #[tokio::test]
    async fn writes_files_into_the_folder_for_the_agent() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("Downloads").join("InputWeave");
        let (read, _) = reader(&Arc::new(Mutex::new(Err("none".to_owned()))));
        let services = Services {
            read_pictures: read,
            clipboard: None,
            receive_folder: Some(folder.clone()),
            history_file: None,
            launch_app: no_app(),
        };
        let (mut agent, broker, _) = connected(dir.path(), services).await;
        assert!(matches!(
            next(&mut agent).await,
            broker_message::Msg::ReceiveFolder(_)
        ));

        // Without a clipboard, reads are answered all the same.
        agent.send(&read_clipboard(100)).await.unwrap();
        assert_eq!(
            clipboard_from(&mut agent).await.0.state(),
            ClipboardState::Unavailable
        );

        let send = agent_says;
        agent
            .send(&send(agent_to_broker::Msg::OpenFile(OpenFile {
                handle: 9,
                transfer_id: "abc-1".to_owned(),
                size: 11,
                folder: String::new(),
            })))
            .await
            .unwrap();
        assert!(matches!(next(&mut agent).await,
            broker_message::Msg::FileOpened(o) if o.handle == 9 && o.offset == 0));
        for piece in [&b"hello "[..], b"world"] {
            agent
                .send(&send(agent_to_broker::Msg::FileData(FileData {
                    handle: 9,
                    data: piece.to_vec(),
                })))
                .await
                .unwrap();
        }
        agent
            .send(&send(agent_to_broker::Msg::FinishFile(FinishFile {
                handle: 9,
            })))
            .await
            .unwrap();
        let broker_message::Msg::FileFinished(finished) = next(&mut agent).await else {
            panic!("expected the file finished");
        };
        assert_eq!(finished.blake3, blake3::hash(b"hello world").as_bytes());
        agent
            .send(&send(agent_to_broker::Msg::KeepFile(KeepFile {
                handle: 9,
                name: "hello.txt".to_owned(),
                subfolder: String::new(),
            })))
            .await
            .unwrap();
        let broker_message::Msg::FileKept(kept) = next(&mut agent).await else {
            panic!("expected the file kept");
        };
        assert_eq!(Path::new(&kept.path), folder.join("hello.txt"));
        assert_eq!(
            std::fs::read(folder.join("hello.txt")).unwrap(),
            b"hello world"
        );

        drop(agent);
        broker.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn reads_files_to_send_for_the_agent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, "hello world").unwrap();
        let (read, _) = reader(&Arc::new(Mutex::new(Err("none".to_owned()))));
        let (mut agent, broker, _) = connected(dir.path(), services(read)).await;

        let send = agent_says;
        agent
            .send(&send(agent_to_broker::Msg::OpenForSending(
                OpenForSending {
                    handle: 3,
                    transfer_id: "abc-1".to_owned(),
                    path: path.to_string_lossy().into_owned(),
                },
            )))
            .await
            .unwrap();
        assert!(matches!(next(&mut agent).await,
            broker_message::Msg::OpenedForSending(o) if o.handle == 3 && o.size == 11));
        agent
            .send(&send(agent_to_broker::Msg::HashFile(HashFile {
                handle: 3,
            })))
            .await
            .unwrap();
        let broker_message::Msg::FileHashed(hashed) = next(&mut agent).await else {
            panic!("expected the hash");
        };
        assert_eq!(hashed.blake3, blake3::hash(b"hello world").as_bytes());
        agent
            .send(&send(agent_to_broker::Msg::ReadChunk(ReadChunk {
                handle: 3,
                offset: 6,
                len: 100,
            })))
            .await
            .unwrap();
        assert!(matches!(next(&mut agent).await,
            broker_message::Msg::ChunkData(c) if c.offset == 6 && c.data == b"world"));
        agent
            .send(&send(agent_to_broker::Msg::CloseSending(CloseSending {
                handle: 3,
            })))
            .await
            .unwrap();

        // A file the user cannot open is refused, with the reason.
        agent
            .send(&send(agent_to_broker::Msg::OpenForSending(
                OpenForSending {
                    handle: 4,
                    transfer_id: "abc-2".to_owned(),
                    path: dir.path().join("gone.txt").to_string_lossy().into_owned(),
                },
            )))
            .await
            .unwrap();
        assert!(matches!(next(&mut agent).await,
            broker_message::Msg::OpenFailed(f) if f.handle == 4 && f.reason() == OpenFailure::NotAFile));

        drop(agent);
        broker.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn starts_the_app_at_most_once_in_a_while() {
        let dir = tempfile::tempdir().unwrap();
        let (read, _) = reader(&Arc::new(Mutex::new(Err("none".to_owned()))));
        let starts = Arc::new(AtomicUsize::new(0));
        let launch_app: LaunchApp = Arc::new({
            let starts = starts.clone();
            move || {
                starts.fetch_add(1, Ordering::SeqCst);
                Ok(Launch::Started)
            }
        });
        let with_app = Services {
            launch_app,
            ..services(read)
        };
        let (mut agent, broker, _) = connected(dir.path(), with_app).await;
        let launch = agent_says(agent_to_broker::Msg::LaunchApp(LaunchAppMsg::default()));

        agent.send(&launch).await.unwrap();
        assert!(matches!(next(&mut agent).await,
            broker_message::Msg::AppLaunched(a) if a.ok && a.started));
        // Asked again at once: it was just started.
        agent.send(&launch).await.unwrap();
        assert!(matches!(next(&mut agent).await,
            broker_message::Msg::AppLaunched(a) if a.ok && !a.started));
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        drop(agent);
        broker.await.unwrap().unwrap();

        // Failures are reported.
        let other = tempfile::tempdir().unwrap();
        let (read, _) = reader(&Arc::new(Mutex::new(Ok(Vec::new()))));
        let (mut agent, broker, _) = connected(other.path(), services(read)).await;
        agent.send(&launch).await.unwrap();
        loop {
            match next(&mut agent).await {
                broker_message::Msg::AppLaunched(a) => {
                    assert!(!a.ok && !a.started);
                    assert!(a.error.contains("not installed"), "{}", a.error);
                    break;
                }
                broker_message::Msg::DesktopPictures(_) => continue,
                other => panic!("expected the app's state, got {other:?}"),
            }
        }
        drop(agent);
        broker.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn keeps_the_transfer_history_in_the_users_folder() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("AppData").join("Local");
        let file = history::history_file(&local);
        let (read, _) = reader(&Arc::new(Mutex::new(Err("none".to_owned()))));
        let with_history = Services {
            history_file: Some(file.clone()),
            ..services(read)
        };
        let (mut agent, broker, _) = connected(dir.path(), with_history).await;
        let load = agent_says(agent_to_broker::Msg::LoadHistory(LoadHistory {}));
        let save = |toml: &[u8]| {
            agent_says(agent_to_broker::Msg::SaveHistory(SaveHistory {
                toml: toml.to_vec(),
            }))
        };

        // None yet; then what the agent saves, answered in order.
        agent.send(&load).await.unwrap();
        assert!(matches!(next(&mut agent).await,
            broker_message::Msg::HistoryLoaded(h) if h.toml.is_empty() && h.error.is_empty()));
        agent.send(&save(b"first")).await.unwrap();
        agent.send(&save(b"second")).await.unwrap();
        agent.send(&load).await.unwrap();
        for _ in 0..2 {
            assert!(matches!(next(&mut agent).await,
                broker_message::Msg::HistorySaved(s) if s.error.is_empty()));
        }
        assert!(matches!(next(&mut agent).await,
            broker_message::Msg::HistoryLoaded(h) if h.toml == b"second"));
        assert_eq!(std::fs::read(&file).unwrap(), b"second");

        // More than the limit is refused, and the file stays.
        agent
            .send(&save(&vec![b'#'; history::MAX_BYTES + 1]))
            .await
            .unwrap();
        assert!(matches!(next(&mut agent).await,
            broker_message::Msg::HistorySaved(s) if !s.error.is_empty()));
        assert_eq!(std::fs::read(&file).unwrap(), b"second");

        drop(agent);
        broker.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn tells_a_missing_agent_from_other_failures() {
        let dir = tempfile::tempdir().unwrap();
        let missing = connect(&dir.path().join("broker.sock")).await.unwrap_err();
        assert!(missing.agent_missing(), "{missing}");
        assert!(!SessionError::Connect(io::ErrorKind::PermissionDenied.into()).agent_missing());
    }
}

#[cfg(test)]
mod contents_tests {
    use super::*;

    fn header(group: &[BrokerMessage]) -> &ClipboardContents {
        match &group[0].msg {
            Some(broker_message::Msg::ClipboardContents(header)) => header,
            other => panic!("not a header: {other:?}"),
        }
    }

    /// `name` as an absolute path where the tests run.
    fn at(name: &str) -> String {
        if cfg!(windows) {
            format!(r"C:\Users\pc\{name}")
        } else {
            format!("/Users/pc/{name}")
        }
    }

    #[test]
    fn files_copied_go_with_the_clipboard_unless_it_is_sensitive() {
        let files = vec![at("a.txt"), at("b")];
        let text =
            ClipboardRead::Content(vec![("text/plain;charset=utf-8".to_owned(), b"a".to_vec())]);
        for read in [text, ClipboardRead::Empty, ClipboardRead::TooLarge(9)] {
            let group = contents(Ok((read, files.clone())));
            assert_eq!(header(&group).files, files);
        }
        let group = contents(Ok((ClipboardRead::Sensitive, files.clone())));
        assert!(header(&group).files.is_empty());
        assert!(header(&contents(Err(()))).files.is_empty());
        // Paths that may not go are left out, the rest goes.
        let group = contents(Ok((
            ClipboardRead::Empty,
            vec!["relative".to_owned(), at("c")],
        )));
        assert_eq!(header(&group).files, vec![at("c")]);
    }

    #[test]
    fn no_more_files_go_than_the_protocol_takes() {
        use iw_proto::service::MAX_COPIED_FILES;

        let many: Vec<String> = (0..MAX_COPIED_FILES + 3)
            .map(|i| at(&i.to_string()))
            .collect();
        let group = contents(Ok((ClipboardRead::Empty, many)));
        assert_eq!(header(&group).files.len(), MAX_COPIED_FILES);
        let long = at(&"a".repeat(300 * 1024));
        let group = contents(Ok((ClipboardRead::Empty, vec![long.clone(), long])));
        assert_eq!(header(&group).files.len(), 1);
    }
}

#[cfg(test)]
mod outage_tests {
    use super::*;

    #[test]
    fn a_missing_pipe_is_quiet_while_the_agent_starts() {
        let start = Instant::now();
        let mut outage = Outage::new(start);
        for ms in [10, 1_000, 2_000, 4_999] {
            let at = start + Duration::from_millis(ms);
            assert_eq!(outage.failed(at, true), Report::Quiet, "{ms} ms");
        }
        // Still missing: one warning, then quiet again.
        assert_eq!(outage.failed(start + GRACE, true), Report::Warn);
        assert_eq!(outage.failed(start + GRACE * 2, true), Report::Quiet);
        assert_eq!(outage.failed(start + GRACE * 3, false), Report::Quiet);
    }

    #[test]
    fn other_failures_warn_at_once() {
        let start = Instant::now();
        let mut outage = Outage::new(start);
        let soon = start + Duration::from_millis(10);
        assert_eq!(outage.failed(soon, false), Report::Warn);
        assert_eq!(outage.failed(soon, false), Report::Quiet);
        // Past the grace the pipe going missing was said already.
        assert_eq!(outage.failed(start + GRACE, true), Report::Quiet);
    }

    #[test]
    fn each_lost_connection_gets_its_own_grace_and_warning() {
        let start = Instant::now();
        let mut outage = Outage::new(start);
        assert_eq!(outage.failed(start + GRACE, true), Report::Warn);

        // Connected, then the agent restarts an hour later.
        let lost = start + Duration::from_secs(3_600);
        let mut outage = Outage::new(lost);
        assert_eq!(
            outage.failed(lost + Duration::from_millis(300), true),
            Report::Quiet
        );
        assert_eq!(outage.failed(lost + GRACE, true), Report::Warn);
    }
}
