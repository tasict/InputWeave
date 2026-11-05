//! The IPC client of the background service: it connects to the agent's
//! socket, says Hello, reads everything once and then follows the
//! service's events, and reconnects whenever the service goes away, as the
//! Windows app's `AgentModel` does. Decisions stay in the service; this
//! only mirrors and forwards.
//!
//! One thread per [`Agent`] owns the connection and reads it. Everything
//! the service sends is applied to the [`Snapshot`] in the order it was
//! sent: a command's response before any event that follows it.
//! [`Agent::request`] may be called from any thread and returns a future
//! that resolves on the reading thread, so the UI awaits it on its own
//! main loop. Commands go out through a writing thread, so no caller ever
//! waits on the socket; a write that fails ends the connection, which then
//! starts over, rather than leave half a frame on it.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use bytes::BytesMut;
use futures_channel::oneshot;
use iw_proto::app::v1::{
    AgentMessage, ClientHello, ClientMessage, CursorCrossed, DeclineDrag, Event, GetArrangement,
    GetNearby, GetPairing, GetStatus, GetTransfers, Response, agent_message, client_message, event,
    response,
};
use iw_proto::frame;

use crate::log;
use crate::snapshot::{Link, Snapshot};

/// The IPC version this app speaks, IPC_VERSION in the agent.
pub const IPC_VERSION: u32 = 1;

/// The IPC version an agent that refused this app's says it speaks, from
/// its refusal's detail ("agent speaks IPC version 2"); `None` from one
/// that says it otherwise.
pub fn ipc_version_in(detail: &str) -> Option<u32> {
    detail
        .strip_prefix("agent speaks IPC version ")?
        .trim()
        .parse()
        .ok()
}

/// A command with no answer by then fails as if the service were gone,
/// unless it is one the agent may take longer over (`timeout_for`).
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the app waits for a command's answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wait {
    /// [`REQUEST_TIMEOUT`].
    Usual,
    /// Longer, for a command the agent itself may take longer over.
    Up(Duration),
    /// As long as the connection lasts.
    Unlimited,
}

/// How long to wait for `command`:
/// - SubmitPairingCode waits for the other device's verdict, up to the
///   agent's `VERIFY_TIMEOUT` of 30 s: 35 s;
/// - StartPairing resolves the address first, for as long as the system's
///   resolver takes, which the agent does not limit, and then answers at
///   once (it never waits for the other device). A deadline of the app's
///   would only disagree with the agent about whether pairing started: it
///   would give up on an attempt the agent then goes on with. So there is
///   none; a lost connection still ends it, and closing Add Device cancels
///   whatever started;
/// - SendFiles and BeginDrag list the folders given before answering, up to
///   10,000 files, maybe on a slow share: 120 s;
/// - GetDiagnostics reads the end of the logs: 30 s.
pub fn timeout_for(command: &client_message::Command) -> Wait {
    use client_message::Command as C;
    match command {
        C::StartPairing(_) => Wait::Unlimited,
        C::SubmitPairingCode(_) => Wait::Up(Duration::from_secs(35)),
        C::SendFiles(_) | C::BeginDrag(_) => Wait::Up(Duration::from_secs(120)),
        C::GetDiagnostics(_) => Wait::Up(Duration::from_secs(30)),
        _ => Wait::Usual,
    }
}

/// Reconnecting starts quickly, as systemd restarts the agent in well under
/// a second, and backs off to the last.
pub const FIRST_RETRY: Duration = Duration::from_millis(250);
pub const LAST_RETRY: Duration = Duration::from_secs(2);

/// How often the reading thread looks up from the socket: for requests
/// that timed out, and to stop.
const TICK: Duration = Duration::from_millis(200);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentError {
    /// The service refused, with its stable code (`unknown_device`…).
    Refused { code: String, detail: String },
    /// No connection, or none any more, or no answer in time.
    Unavailable(String),
}

impl AgentError {
    pub fn code(&self) -> Option<&str> {
        match self {
            AgentError::Refused { code, .. } => Some(code),
            AgentError::Unavailable(_) => None,
        }
    }
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentError::Refused { code, detail } => write!(f, "refused: {code}: {detail}"),
            AgentError::Unavailable(why) => write!(f, "unavailable: {why}"),
        }
    }
}

impl std::error::Error for AgentError {}

impl From<AgentError> for io::Error {
    fn from(e: AgentError) -> Self {
        io::Error::other(e.to_string())
    }
}

/// The code of the refusal this client makes up for an answer without a
/// result: no answer from the agent, rather than its refusal.
pub const NO_RESULT: &str = "unexpected";

/// What a command's answer leaves for the caller. Status, pairing, nearby
/// devices, arrangement and transfers go into the snapshot (moved, not
/// copied), which has them by the time the caller hears; anything else
/// comes as it is.
#[derive(Debug)]
pub enum Answer {
    /// In the snapshot now.
    Applied,
    Other(Box<response::Result>),
}

type Connect = Box<dyn Fn() -> io::Result<UnixStream> + Send + Sync>;
type OnChange = Box<dyn Fn() + Send + Sync>;
type Reply = oneshot::Sender<Result<Answer, AgentError>>;

/// What a request waits for its answer with.
enum Waiter {
    /// A command from the app.
    /// With its deadline; none for a command waited for as long as the
    /// connection lasts.
    Caller(Reply, Option<Instant>),
    /// Part of setting up the connection.
    Setup(Setup, Instant),
    /// Nothing waits; the answer is applied and dropped.
    Ignored,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Setup {
    Hello,
    Status,
    Pairing,
    Nearby,
    Arrangement,
    Transfers,
}

/// The writing side of a session: frames queued for its writing thread,
/// and the socket, to shut it down.
struct Writer {
    frames: mpsc::Sender<Vec<u8>>,
    socket: UnixStream,
}

impl Writer {
    /// Starts the writing thread for `socket`. A write that fails, or that
    /// the agent does not take within `timeout`, shuts the socket down, so
    /// the reading thread sees the session end and starts another.
    fn start(socket: &UnixStream, timeout: Duration) -> io::Result<Writer> {
        let mut out = socket.try_clone()?;
        out.set_write_timeout(Some(timeout))?;
        let (frames, queued) = mpsc::channel::<Vec<u8>>();
        thread::Builder::new()
            .name("agent-ipc-write".into())
            .spawn(move || {
                for frame in queued {
                    if let Err(e) = out.write_all(&frame) {
                        log::info(&format!("writing to the background service failed: {e}"));
                        let _ = out.shutdown(std::net::Shutdown::Both);
                        return;
                    }
                }
            })?;
        Ok(Writer {
            frames,
            socket: socket.try_clone()?,
        })
    }

    fn close(self) {
        let _ = self.socket.shutdown(std::net::Shutdown::Both);
    }
}

/// At most this many crossings wait for the UI to take them; older ones
/// go first (a UI that never looks keeps no backlog).
const CROSSINGS_KEPT: usize = 64;

struct State {
    snapshot: Arc<Snapshot>,
    /// Event.cursor_crossed, each one an event of its own, oldest first,
    /// until the UI takes them: a snapshot would keep only the last of two
    /// that come together (the cursor out and straight back).
    crossings: VecDeque<CursorCrossed>,
    /// The session's writing side while one is set up or going.
    writer: Option<Writer>,
    waiting: HashMap<u64, Waiter>,
    next_id: u64,
}

struct Shared {
    state: Mutex<State>,
    on_change: OnChange,
    /// Called for each crossing queued, where set (`Agent::on_crossing`),
    /// in place of `on_change`: the glow need not wait for the rest of the
    /// app to catch up.
    on_crossing: Mutex<Option<OnChange>>,
    stopping: AtomicBool,
    connect: Option<Connect>,
    client: String,
    timeout: Duration,
    /// For a model made with [`Agent::fixed`], what commands the UI sent
    /// and how the next one is answered, for the GTK tests.
    script: Mutex<Script>,
}

#[derive(Default)]
struct Script {
    recording: bool,
    sent: Vec<client_message::Command>,
    refuse_next: Option<String>,
}

/// The background service as the app sees it. Cheap to clone; every clone
/// is the same connection.
#[derive(Clone)]
pub struct Agent {
    shared: Arc<Shared>,
}

impl Agent {
    /// Connects to the agent's socket at `socket`, and keeps reconnecting
    /// until dropped. `on_change` runs on the reading thread whenever the
    /// snapshot changes. `client` is how the app introduces itself
    /// ("linux-ui/0.1.0").
    pub fn start(
        socket: PathBuf,
        client: &str,
        on_change: impl Fn() + Send + Sync + 'static,
    ) -> Agent {
        Agent::start_with(
            Box::new(move || UnixStream::connect(&socket)),
            client,
            Box::new(on_change),
        )
    }

    fn start_with(connect: Connect, client: &str, on_change: OnChange) -> Agent {
        Agent::start_timed(connect, client, on_change, REQUEST_TIMEOUT)
    }

    fn start_timed(
        connect: Connect,
        client: &str,
        on_change: OnChange,
        timeout: Duration,
    ) -> Agent {
        let mut agent = Agent::new(
            Some(connect),
            client,
            on_change,
            Snapshot {
                unreachable_since: Some(SystemTime::now()),
                ..Default::default()
            },
        );
        Arc::get_mut(&mut agent.shared)
            .expect("not shared yet")
            .timeout = timeout;
        let reader = agent.clone();
        thread::Builder::new()
            .name("agent-ipc".into())
            .spawn(move || reader.run())
            .expect("starting the IPC thread");
        agent
    }

    /// A model that never connects and only shows `snapshot`, as for
    /// screenshots; every command fails as unavailable.
    pub fn fixed(snapshot: Snapshot) -> Agent {
        Agent::new(None, "fixed", Box::new(|| {}), snapshot)
    }

    fn new(
        connect: Option<Connect>,
        client: &str,
        on_change: OnChange,
        snapshot: Snapshot,
    ) -> Agent {
        Agent {
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    snapshot: Arc::new(snapshot),
                    crossings: VecDeque::new(),
                    writer: None,
                    waiting: HashMap::new(),
                    next_id: 0,
                }),
                on_change,
                on_crossing: Mutex::new(None),
                stopping: AtomicBool::new(false),
                connect,
                client: client.to_owned(),
                timeout: REQUEST_TIMEOUT,
                script: Mutex::default(),
            }),
        }
    }

    /// Has a model made with [`Agent::fixed`] keep the commands it is sent
    /// and take them, rather than say there is no service: the GTK tests
    /// check what the UI sends.
    pub fn record_fixed(&self) {
        if self.shared.connect.is_none() {
            self.script().recording = true;
        }
    }

    /// The commands a recording fixed model was sent, oldest first.
    pub fn sent(&self) -> Vec<client_message::Command> {
        self.script().sent.clone()
    }

    /// The next command a recording fixed model is sent is refused with
    /// `code`.
    pub fn refuse_next(&self, code: &str) {
        self.script().refuse_next = Some(code.to_owned());
    }

    fn script(&self) -> std::sync::MutexGuard<'_, Script> {
        self.shared
            .script
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Shows `snapshot` instead, for a model made with [`Agent::fixed`]
    /// (tests, screenshots); a connecting one keeps what the agent says.
    pub fn show_fixed(&self, snapshot: Snapshot) {
        if self.shared.connect.is_none() {
            self.lock().snapshot = Arc::new(snapshot);
        }
    }

    /// What the agent last reported. Shared, not copied: the reading thread
    /// copies it only to change it while someone still holds this one.
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.lock().snapshot.clone()
    }

    /// Calls `crossed` (on the IPC thread) for each crossing queued for
    /// `take_crossings`, in place of the change callback. Crossings queued
    /// before are dropped: a glow is worth showing only as it happens, not
    /// later beside an unrelated one.
    pub fn on_crossing(&self, crossed: impl Fn() + Send + Sync + 'static) {
        *self
            .shared
            .on_crossing
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(Box::new(crossed));
        self.lock().crossings.clear();
    }

    /// The crossings the service told of since the last call, oldest
    /// first, each to be shown.
    pub fn take_crossings(&self) -> Vec<CursorCrossed> {
        std::mem::take(&mut self.lock().crossings).into()
    }

    /// How many crossings wait, leaving them there.
    #[cfg(test)]
    fn queued_crossings(&self) -> Vec<CursorCrossed> {
        self.lock().crossings.iter().cloned().collect()
    }

    /// Stops reconnecting and closes the connection.
    pub fn stop(&self) {
        self.shared.stopping.store(true, Ordering::SeqCst);
        let mut state = self.lock();
        if let Some(writer) = state.writer.take() {
            writer.close();
        }
    }

    /// Sends a command; the future resolves with the service's answer,
    /// already applied to the snapshot. A refusal is `AgentError::Refused`.
    pub fn request(
        &self,
        command: client_message::Command,
    ) -> impl Future<Output = Result<Answer, AgentError>> + 'static {
        let (reply, answer) = oneshot::channel();
        let scripted = {
            let mut script = self.script();
            script.recording.then(|| {
                script.sent.push(command.clone());
                match script.refuse_next.take() {
                    Some(code) => Err(AgentError::Refused {
                        code,
                        detail: "refused for a test".into(),
                    }),
                    None => Ok(Answer::Applied),
                }
            })
        };
        let sent = if let Some(result) = scripted {
            let _ = reply.send(result);
            Ok(())
        } else {
            let mut state = self.lock();
            if state.snapshot.link != Link::Connected {
                Err(AgentError::Unavailable(
                    "not connected to the background service".into(),
                ))
            } else {
                let deadline = match timeout_for(&command) {
                    Wait::Usual => Some(Instant::now() + self.shared.timeout),
                    Wait::Up(t) => Some(Instant::now() + t.max(self.shared.timeout)),
                    Wait::Unlimited => None,
                };
                send(&mut state, command, Waiter::Caller(reply, deadline)).map(|_| ())
            }
        };
        async move {
            sent?;
            answer.await.unwrap_or_else(|_| {
                Err(AgentError::Unavailable(
                    "the connection to the background service ended".into(),
                ))
            })
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.shared.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn changed(&self) {
        (self.shared.on_change)();
    }

    fn update(&self, change: impl FnOnce(&mut Snapshot)) {
        {
            let mut state = self.lock();
            let snapshot = Arc::make_mut(&mut state.snapshot);
            change(snapshot);
            let dwell = snapshot.status.edge_dwell_ms;
            if dwell != 0 {
                snapshot.last_switch_delay_ms = dwell;
            }
        }
        self.changed();
    }

    /// The reading thread: one session after another until stopped.
    fn run(self) {
        let mut retry = FIRST_RETRY;
        let mut last_failure: Option<String> = None;
        while !self.shared.stopping.load(Ordering::SeqCst) {
            let mut connected = false;
            let result = self.session(&mut connected);
            // Refused for another IPC version: still updating, rather than
            // gone, until a connection fails otherwise.
            let incompatible = matches!(&result, Err(e) if e.kind() == io::ErrorKind::Unsupported);
            if let Err(e) = result {
                let failure = e.to_string();
                if connected {
                    log::info(&format!("lost the background service: {failure}"));
                    last_failure = None;
                } else if last_failure.as_deref() != Some(&failure) {
                    log::info(&format!("cannot reach the background service: {failure}"));
                    last_failure = Some(failure);
                }
            }
            self.disconnected(incompatible);
            if connected {
                retry = FIRST_RETRY;
            }
            let until = Instant::now() + retry;
            while Instant::now() < until && !self.shared.stopping.load(Ordering::SeqCst) {
                thread::sleep(TICK.min(until.saturating_duration_since(Instant::now())));
            }
            retry = (retry * 2).min(LAST_RETRY);
        }
        self.disconnected(false);
    }

    fn session(&self, connected: &mut bool) -> io::Result<()> {
        let connect = self.shared.connect.as_ref().expect("a connecting agent");
        let mut stream = connect()?;
        stream.set_read_timeout(Some(TICK))?;
        let writer = Writer::start(&stream, self.shared.timeout)?;
        let deadline = Instant::now() + self.shared.timeout;
        {
            let mut state = self.lock();
            state.writer = Some(writer);
            let hello = client_message::Command::Hello(ClientHello {
                ipc_version: IPC_VERSION,
                client: self.shared.client.clone(),
                takes_clipboard_files: false,
            });
            send(&mut state, hello, Waiter::Setup(Setup::Hello, deadline))?;
            for (setup, command) in [
                (
                    Setup::Status,
                    client_message::Command::GetStatus(GetStatus {}),
                ),
                (
                    Setup::Pairing,
                    client_message::Command::GetPairing(GetPairing {}),
                ),
                (
                    Setup::Nearby,
                    client_message::Command::GetNearby(GetNearby {}),
                ),
                (
                    Setup::Arrangement,
                    client_message::Command::GetArrangement(GetArrangement {}),
                ),
                (
                    Setup::Transfers,
                    client_message::Command::GetTransfers(GetTransfers {}),
                ),
            ] {
                send(&mut state, command, Waiter::Setup(setup, deadline))?;
            }
        }
        let mut pending_setup = 6;
        let mut buf = BytesMut::with_capacity(64 * 1024);
        let mut chunk = vec![0u8; 64 * 1024];
        let mut expired_at = Instant::now();
        loop {
            if self.shared.stopping.load(Ordering::SeqCst) {
                return Ok(());
            }
            // Not only when the socket is quiet: an agent busy sending
            // events may still never answer a command.
            if expired_at.elapsed() >= TICK {
                expired_at = Instant::now();
                if self.expire(*connected) {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "the background service did not answer",
                    ));
                }
            }
            match frame::decode::<AgentMessage>(&mut buf) {
                Ok(Some(message)) => {
                    match self.handle(message) {
                        Handled::Setup(Err(e)) => return Err(e),
                        Handled::Setup(Ok(())) => {
                            pending_setup -= 1;
                            if pending_setup == 0 {
                                *connected = true;
                                let version = self.lock().snapshot.agent_version.clone();
                                log::info(&format!(
                                    "connected to the background service {version} as {}",
                                    self.shared.client
                                ));
                                self.update(|s| {
                                    s.link = Link::Connected;
                                    s.unreachable_since = None;
                                });
                            }
                        }
                        Handled::Other => {}
                    }
                    continue;
                }
                Ok(None) => {}
                Err(e) => return Err(io::Error::new(io::ErrorKind::InvalidData, e.to_string())),
            }
            match stream.read(&mut chunk) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "the background service closed the connection",
                    ));
                }
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                // Quiet for a tick: the deadlines are looked at above.
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }

    /// Fails the requests that waited too long. Returns whether setting up
    /// the connection did, which ends the session.
    fn expire(&self, connected: bool) -> bool {
        let now = Instant::now();
        let mut expired = Vec::new();
        let mut setup_late = false;
        {
            let mut state = self.lock();
            let late: Vec<u64> = state
                .waiting
                .iter()
                .filter(|(_, w)| match w {
                    Waiter::Caller(_, deadline) => deadline.is_some_and(|d| d <= now),
                    Waiter::Setup(_, deadline) => *deadline <= now,
                    Waiter::Ignored => false,
                })
                .map(|(id, _)| *id)
                .collect();
            for id in late {
                match state.waiting.remove(&id) {
                    Some(Waiter::Caller(reply, _)) => expired.push(reply),
                    Some(Waiter::Setup(..)) => setup_late = !connected,
                    _ => {}
                }
            }
        }
        for reply in expired {
            let _ = reply.send(Err(AgentError::Unavailable(
                "the background service did not answer".into(),
            )));
        }
        setup_late
    }

    fn handle(&self, message: AgentMessage) -> Handled {
        match message.msg {
            Some(agent_message::Msg::Response(response)) => self.on_response(response),
            Some(agent_message::Msg::Event(event)) => {
                self.on_event(event);
                Handled::Other
            }
            None => Handled::Other,
        }
    }

    fn on_response(&self, response: Response) -> Handled {
        let waiter = self.lock().waiting.remove(&response.request_id);
        match waiter {
            Some(Waiter::Setup(setup, _)) => Handled::Setup(self.on_setup(setup, response)),
            Some(Waiter::Caller(reply, _)) => {
                let result = match response.result {
                    Some(response::Result::Error(e)) => Err(AgentError::Refused {
                        code: e.code,
                        detail: e.detail,
                    }),
                    None => Err(AgentError::Refused {
                        code: NO_RESULT.into(),
                        detail: "empty response".into(),
                    }),
                    Some(result) => Ok(self.apply(result)),
                };
                let _ = reply.send(result);
                Handled::Other
            }
            Some(Waiter::Ignored) => {
                if let Some(result) = response.result {
                    self.apply(result);
                }
                Handled::Other
            }
            None => Handled::Other,
        }
    }

    fn on_setup(&self, setup: Setup, response: Response) -> io::Result<()> {
        match (setup, response.result) {
            (Setup::Hello, Some(response::Result::Hello(hello))) => {
                self.update(|s| {
                    s.agent_version = hello.agent_version;
                    s.agent_digest = hello.agent_digest;
                    s.agent_ipc_version = None;
                });
                Ok(())
            }
            (_, Some(response::Result::Error(e))) => {
                if setup == Setup::Hello && e.code == "ipc_version_mismatch" {
                    // Usually an update in progress: the service restarts soon.
                    // Its own version is in the detail, which tells which
                    // of the two is behind.
                    let theirs = ipc_version_in(&e.detail);
                    self.update(|s| {
                        s.link = Link::Incompatible;
                        s.agent_ipc_version = theirs;
                    });
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "refused: ipc_version_mismatch",
                    ));
                }
                Err(io::Error::other(format!("refused: {}", e.code)))
            }
            (Setup::Status, Some(response::Result::Status(status))) => {
                self.update(|s| s.status = status);
                Ok(())
            }
            (Setup::Pairing, Some(response::Result::Pairing(pairing))) => {
                self.update(|s| s.pairing = pairing);
                Ok(())
            }
            (Setup::Nearby, Some(response::Result::Nearby(nearby))) => {
                self.update(|s| s.nearby = nearby.devices);
                Ok(())
            }
            (Setup::Arrangement, Some(response::Result::Arrangement(arrangement))) => {
                self.update(|s| s.arrangement = Arc::new(arrangement));
                Ok(())
            }
            (Setup::Transfers, Some(response::Result::Transfers(transfers))) => {
                self.update(|s| s.transfers = Arc::new(transfers));
                Ok(())
            }
            (setup, _) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unexpected answer to {setup:?}"),
            )),
        }
    }

    /// A command's answer changes the snapshot as the event would; what
    /// goes into it is moved there.
    fn apply(&self, result: response::Result) -> Answer {
        match result {
            response::Result::Status(status) => self.update(|s| s.status = status),
            response::Result::Arrangement(arrangement) => {
                self.update(|s| s.arrangement = Arc::new(arrangement))
            }
            response::Result::Transfers(transfers) => {
                self.update(|s| s.transfers = Arc::new(transfers))
            }
            response::Result::Pairing(pairing) => self.update(|s| s.pairing = pairing),
            response::Result::Nearby(nearby) => self.update(|s| s.nearby = nearby.devices),
            other => return Answer::Other(Box::new(other)),
        }
        Answer::Applied
    }

    fn on_event(&self, e: Event) {
        // Linux takes no part in drags across computers, and the agent
        // should not offer this app one (`refuses_drags`); should one come
        // anyway (another backend, an older agent), decline it at once so
        // the other computer is not left waiting.
        if let Some(arrived) = &e.drag_arrived {
            log::info(&format!("declining drag {}", arrived.drag_id));
            let decline = client_message::Command::DeclineDrag(DeclineDrag {
                drag_id: arrived.drag_id.clone(),
            });
            let _ = send(&mut self.lock(), decline, Waiter::Ignored);
        }
        if let Some(crossed) = e.cursor_crossed {
            let mut state = self.lock();
            if state.crossings.len() >= CROSSINGS_KEPT {
                state.crossings.pop_front();
            }
            state.crossings.push_back(crossed);
            drop(state);
            // An event carries one thing (app.proto): nothing else to apply.
            let crossing = self
                .shared
                .on_crossing
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            match crossing.as_ref() {
                Some(crossed) => crossed(),
                None => {
                    drop(crossing);
                    self.changed();
                }
            }
            return;
        }
        // One change for the whole event, which may carry several things.
        self.update(|s| {
            // Outside the oneof (app.proto): an event may carry these.
            if let Some(identified) = e.screens_identified {
                s.screens_identified += 1;
                s.identify_ms = identified.duration_ms;
            }
            if let Some(unsupported) = e.relative_unsupported {
                s.relative_refusals += 1;
                s.relative_refused_device = unsupported.device_id;
            }
            match e.event {
                Some(event::Event::StatusChanged(status)) => s.status = status,
                Some(event::Event::PairingChanged(pairing)) => s.pairing = pairing,
                Some(event::Event::NearbyChanged(nearby)) => s.nearby = nearby.devices,
                Some(event::Event::ArrangementChanged(arrangement)) => {
                    s.arrangement = Arc::new(arrangement)
                }
                Some(event::Event::TransfersChanged(transfers)) => {
                    s.transfers = Arc::new(transfers)
                }
                Some(event::Event::CursorLocated(_)) => s.cursor_located += 1,
                None => {}
            }
        });
    }

    /// The session ended: refused for another IPC version
    /// (`incompatible`), or otherwise.
    fn disconnected(&self, incompatible: bool) {
        let replies: Vec<Reply> = {
            let mut state = self.lock();
            if let Some(writer) = state.writer.take() {
                writer.close();
            }
            state
                .waiting
                .drain()
                .filter_map(|(_, w)| match w {
                    Waiter::Caller(reply, _) => Some(reply),
                    _ => None,
                })
                .collect()
        };
        for reply in replies {
            let _ = reply.send(Err(AgentError::Unavailable(
                "not connected to the background service".into(),
            )));
        }
        let now = SystemTime::now();
        self.update(|s| {
            match s.link {
                _ if incompatible => s.link = Link::Incompatible,
                Link::Connected => s.link = Link::Connecting,
                Link::Incompatible => {
                    // The agent that refused is gone: out of reach from now,
                    // not since the update began.
                    s.link = Link::Connecting;
                    s.unreachable_since = Some(now);
                }
                Link::Connecting => {}
            }
            if s.unreachable_since.is_none() {
                s.unreachable_since = Some(now);
            }
            // A pairing goes with the agent: one that shows a code must not
            // show it, or be cancelled, while nobody can answer.
            s.pairing = Default::default();
        });
    }
}

enum Handled {
    /// An answer while setting up the connection: whether it was good.
    Setup(io::Result<()>),
    Other,
}

/// Queues a command for the writing thread and remembers what waits for
/// its answer.
fn send(
    state: &mut State,
    command: client_message::Command,
    waiter: Waiter,
) -> Result<u64, AgentError> {
    let Some(writer) = state.writer.as_ref() else {
        return Err(AgentError::Unavailable(
            "not connected to the background service".into(),
        ));
    };
    state.next_id += 1;
    let id = state.next_id;
    let message = ClientMessage {
        request_id: id,
        command: Some(command),
    };
    let mut buf = BytesMut::new();
    frame::encode(&message, &mut buf).map_err(|e| AgentError::Unavailable(e.to_string()))?;
    writer.frames.send(buf.to_vec()).map_err(|_| {
        AgentError::Unavailable("the connection to the background service ended".into())
    })?;
    state.waiting.insert(id, waiter);
    Ok(id)
}

#[cfg(test)]
mod tests;
