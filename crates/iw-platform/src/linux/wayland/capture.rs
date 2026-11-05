//! Capturing this device's keyboard and mouse on a Wayland desktop, through
//! the desktop's InputCapture portal and the libei connection it hands out.
//!
//! The desktop keeps input to itself until the cursor pushes through a
//! pointer barrier, put where the cursor leaves this device for another
//! (`InputCapture::set_exits`); it then hands input over through libei, to
//! this receiver, and keeps it from the local apps until it is released.
//! What an activation reports, and when its input is let go of, is
//! `barriers`'. Input is never grabbed at will: the cursor leaves this
//! device across an edge, not by a jump from here (`grab_ready`), unless
//! input is handed over already.
//!
//! A thread of its own runs the sessions on a small tokio runtime, started,
//! ended and started again as remote control's are (`portal_session`); the
//! agent's calls only queue commands.
//!
//! The desktop asks the user as the session starts. Version 2 of the portal
//! (`CreateSession2`, `Start`) hands out a restore token, kept in
//! `portal.toml` as `input_capture`, so later starts go without asking, as
//! at login; version 1 asks at every start, so the agent starts it only
//! when the app asks (`request_permission`), at every login. Tokens are kept
//! and forgotten as for remote control (`portal_session::run`): forgotten
//! when the user said no, stopped it, or a start may have used it up.
//!
//! - Barriers are put again whenever the exits or the desktop's zones
//!   change (`ZonesChanged`), but not while input is handed over: the
//!   portal suspends capture while they change, so that waits until it is
//!   let go of. Their ids are never given again, so an activation at a
//!   barrier from before, which the desktop ended as capture was disabled
//!   to put the new ones, is told apart and let go of, as is one at any
//!   barrier the portal names that is not among those put last.
//! - An activation counts once libei starts the devices for it (its
//!   `start_emulating` carries the activation's id) or its input comes:
//!   one the desktop ended before is never reported. Started, it begins
//!   once the desktop told the modifiers held, or a moment after, so a
//!   modifier that must be held to cross is known as the push is told.
//! - The portal's signals are heard from the portal alone (the unique name
//!   that owns its well-known one), for this session alone.
//! - Input handed over that ends, however, is followed by what is down
//!   reported up and, unless the cursor came back here as the agent asked,
//!   by `CaptureEnded`: control comes back from another device, or the
//!   cursor's push at the edge starts over. Coming back, a key or button
//!   that goes down before the agent says where the cursor goes lets input
//!   go at once. The desktop disabling capture by itself is followed by
//!   enabling it again, after a second, then twice as long each time in a
//!   row, up to half a minute, for as long as it takes; a `Disabled`
//!   the portal sent before it answered that capture was enabled again
//!   here (their serials tell) is about the `Disable` made here to put
//!   barriers.

use std::num::NonZeroU32;
use std::os::unix::net::UnixStream;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ashpd::desktop::input_capture::{
    Activated, Barrier as PortalBarrier, Capabilities, CreateSession2Options, CreateSessionOptions,
    Deactivated, Disabled, InputCapture as Portal, ReleaseOptions, StartOptions, ZonesChanged,
};
use ashpd::desktop::{PersistMode, ResponseError, Session};
use futures_util::{Stream, StreamExt};
use iw_core::HidUsage;
use iw_core::geometry::{Point, Rect};
use iw_core::hotkey::Hotkey;
use iw_core::input::InputEvent;
use iw_core::layout::Exit;
use reis::ei;
use reis::event::{DeviceCapability, EiEvent};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use zbus::zvariant::OwnedObjectPath;

use super::barriers::{self, Activation, Barrier, GrabTaken, Handover, Pressed, Verdict};
use super::portal_session::{
    self, CLOSED_WAIT, Commands, Ended, Kind, Lifecycle, STEP, STOP_WAIT, Start, Step, Why, call,
    closed_or_lost, outcome,
};
use super::tokens::Tokens;
use super::translate::Portal as State;
use crate::linux::common::lock;
use crate::linux::natural_scroll::NaturalScroll;
use crate::{Permission, PermissionState, PlatformError};

/// The key of the InputCapture portal's restore token in `portal.toml`.
const TOKEN_KEY: &str = "input_capture";
/// Its sessions, in the log.
const SESSION: &str = "input capture";

/// Restore tokens came with version 2 of the portal.
const TOKEN_VERSION: u32 = 2;

/// How long dropping the backend waits for the portal to take input back,
/// within `STOP_WAIT`.
const LET_GO_WAIT: Duration = Duration::from_millis(500);

/// How long each call that puts the barriers, or lets go of input, may
/// take: the session goes on meanwhile without reading its input.
const CALL_WAIT: Duration = Duration::from_secs(5);

/// Changes to the exits come in bursts (a device connecting changes the
/// layout, then its displays do): barriers are put once they settled.
const SETTLE: Duration = Duration::from_millis(50);

/// After barriers could not be put, how long until they are tried again.
const PLACE_RETRY: Duration = Duration::from_secs(5);

/// After the desktop disabled capture by itself, how long until it is
/// enabled again: this long at first, twice as long each time in a row
/// without input handed over, up to the longest. It is never given up on:
/// a long screen lock may disable it many times.
const REARM_WAIT: Duration = Duration::from_secs(1);
const REARM_MAX: Duration = Duration::from_secs(30);
/// So many times in a row and the desktop's disabling is worth a warning.
const REARMS: u32 = 3;

/// How long an activation libei started, or whose input came, waits for the
/// modifiers held, which the desktop may tell just after.
const MODIFIERS_WAIT: Duration = Duration::from_millis(20);

/// The most input kept for an activation that has not begun: what comes in
/// `MODIFIERS_WAIT`, many times over. Past it, the oldest movement goes;
/// presses, releases and frames are all kept (`barriers::keep_early`).
const EARLY_MAX: usize = 256;

/// The portal's name on the bus, its object and interface.
const DESKTOP: &str = "org.freedesktop.portal.Desktop";
const DESKTOP_PATH: &str = "/org/freedesktop/portal/desktop";
const INTERFACE: &str = "org.freedesktop.portal.InputCapture";

enum Command {
    /// The app asks for the permission.
    Ask,
    Exits(Vec<Exit>),
    /// The agent grabbed input, or let go of it, as the cursor crossed or
    /// came back, for the activation current as it did (`Grab::meant`).
    Grab(Grab),
    Warp(Point),
    /// The router's answer after the `0`th event reported.
    EdgeWait(u64, bool),
    /// The backend is dropped: let go of input and end.
    Stop(std::sync::mpsc::Sender<()>),
}

impl portal_session::Command for Command {
    fn kind(self) -> Kind<Self> {
        match self {
            Command::Ask => Kind::Ask,
            Command::Stop(done) => Kind::Stop(done),
            command => Kind::Own(command),
        }
    }
}

/// A grab or its end, and the activation it was made for: the one current
/// as the agent made it (`Shared::current`; 0: none), so one that ended
/// meanwhile, as the next began, is not taken for the next.
#[derive(Clone, Copy, Debug)]
struct Grab {
    meant: u64,
    grabbed: bool,
}

/// Where to reach the portal, and where to keep its token.
pub(crate) struct CaptureOptions {
    /// Where the sessions reach the portal, and how they start again.
    pub lifecycle: Lifecycle,
    /// How long each call that puts the barriers, or lets go of input, may
    /// take, and how long after barriers could not be put they are tried
    /// again.
    pub waits: Waits,
    /// The desktop's natural scrolling, turned back as scrolling is
    /// reported.
    pub natural: Arc<NaturalScroll>,
}

/// How long the calls of a running session wait (`CaptureOptions::waits`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Waits {
    pub call: Duration,
    pub place_retry: Duration,
    /// The first and longest wait to enable capture again after the
    /// desktop disabled it.
    pub rearm: Duration,
    pub rearm_max: Duration,
    /// How long to wait for where the cursor goes as it comes back.
    pub warp: Duration,
    /// How long input handed over waits for the desktop to tell the
    /// modifiers held.
    pub modifiers: Duration,
}

impl CaptureOptions {
    /// Options for the session bus of this process.
    pub(crate) fn new(tokens: Tokens, app_id: &'static str) -> Self {
        Self::with(Lifecycle::new(tokens, app_id))
    }

    /// Options whose sessions live as `lifecycle` says.
    pub(crate) fn with(lifecycle: Lifecycle) -> Self {
        Self {
            lifecycle,
            waits: Waits {
                call: CALL_WAIT,
                place_retry: PLACE_RETRY,
                rearm: REARM_WAIT,
                rearm_max: REARM_MAX,
                warp: barriers::WARP_WAIT,
                modifiers: MODIFIERS_WAIT,
            },
            natural: Arc::default(),
        }
    }
}

#[derive(Default)]
struct Shared {
    known: Mutex<Known>,
    running: AtomicBool,
    /// Counts the sessions that ran.
    sessions: AtomicU64,
    /// Where input goes, which activation holds it, and the hotkeys.
    handover: Handover,
    /// The desktop's natural scrolling (`CaptureOptions::natural`).
    natural: Arc<NaturalScroll>,
}

/// The session and the app's requests, changed and read together.
#[derive(Default)]
struct Known {
    state: State,
    /// Counts the times the app asked.
    asks: u64,
}

impl Shared {
    fn known(&self) -> std::sync::MutexGuard<'_, Known> {
        lock(&self.known)
    }

    fn set(&self, state: State) {
        let mut known = self.known();
        if known.state != state {
            info!(from = ?known.state, to = ?state, "input capture through the portal");
            self.running
                .store(state == State::Running, Ordering::SeqCst);
            // What the session reported as it ended went already; the agent
            // starts capture again once the next one runs.
            if state != State::Running {
                self.handover.stop_delivering();
            }
            known.state = state;
        }
    }
}

/// The session, run by a thread of its own.
pub(crate) struct Capture {
    shared: Arc<Shared>,
    commands: mpsc::UnboundedSender<Command>,
}

impl Capture {
    pub(crate) fn start(options: CaptureOptions) -> Result<Self, PlatformError> {
        let shared = Arc::new(Shared {
            natural: options.natural.clone(),
            ..Shared::default()
        });
        // Starting with what the user allowed before from the first, as the
        // thread will: `start` says it is starting, not that the permission
        // is missing, and the app does not ask meanwhile.
        if options.lifecycle.tokens.load(TOKEN_KEY).is_some() {
            shared.set(State::Restoring);
        }
        let (commands, rx) = mpsc::unbounded_channel();
        let thread_shared = shared.clone();
        std::thread::Builder::new()
            .name("inputweave-input-capture-portal".to_owned())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build();
                match runtime {
                    Ok(runtime) => runtime.block_on(run(&thread_shared, rx, options)),
                    Err(e) => thread_shared.set(State::Failed(format!("cannot run: {e}"))),
                }
            })
            .map_err(|e| PlatformError::Os(format!("cannot start the capture thread: {e}")))?;
        Ok(Self { shared, commands })
    }

    #[cfg(test)]
    pub(crate) fn state(&self) -> State {
        self.shared.known().state.clone()
    }

    /// As the agent grabs input, before its command arrives.
    #[cfg(test)]
    pub(crate) fn grab_ahead(&self) {
        self.shared.handover.grab(true);
    }

    /// The permission the session stands for, with the request it answers,
    /// read together.
    pub(crate) fn permission(&self) -> PermissionState {
        let known = self.shared.known();
        known.state.permission(Permission::InputCapture, known.asks)
    }

    pub(crate) fn running(&self) -> bool {
        self.shared.running.load(Ordering::SeqCst)
    }

    /// Counts the sessions that ran.
    pub(crate) fn sessions(&self) -> u64 {
        self.shared.sessions.load(Ordering::SeqCst)
    }

    /// Has the desktop ask the user, unless a session is starting or
    /// running already. Returns how many times the app asked now.
    pub(crate) fn ask(&self) -> Result<u64, PlatformError> {
        let asked = {
            let mut known = self.shared.known();
            if known.state.may_start() {
                known.state = State::Starting;
            }
            known.asks += 1;
            known.asks
        };
        if self.commands.send(Command::Ask).is_err() {
            self.shared.set(State::Failed(
                "the thread running the capture session ended".to_owned(),
            ));
        }
        Ok(asked)
    }

    fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }
}

impl Drop for Capture {
    /// Lets go of input and ends the session, waiting a moment for it.
    fn drop(&mut self) {
        let (done, wait) = std::sync::mpsc::channel();
        if self.commands.send(Command::Stop(done)).is_ok() {
            let _ = wait.recv_timeout(STOP_WAIT);
        }
    }
}

/// Capture of this device's keyboard and mouse through the portal. Until
/// the desktop allowed it, `start` is refused for want of the permission,
/// and the agent starts it again once it is granted.
pub struct PortalCapture {
    capture: Arc<Capture>,
}

impl PortalCapture {
    pub(crate) fn new(capture: Arc<Capture>) -> Self {
        Self { capture }
    }
}

impl crate::InputCapture for PortalCapture {
    /// Input goes to the agent only once this said capture runs, so the
    /// agent never gets input while it holds capture off: until then, and
    /// again from when the session ends, nothing is reported.
    fn start(&mut self, events: Sender<InputEvent>) -> Result<(), PlatformError> {
        let shared = &self.capture.shared;
        // Under the lock the session's state changes under, so a session
        // ending meanwhile drops these events.
        let known = shared.known();
        match known.state {
            State::Running => {
                shared.handover.deliver(events);
                Ok(())
            }
            // Starting is not wanting the permission: started again once it
            // runs, without a warning, as at every login with a token.
            State::Starting | State::Restoring => Err(PlatformError::Starting("input capture")),
            _ => Err(PlatformError::PermissionDenied(Permission::InputCapture)),
        }
    }

    /// A grab is of the activation current now; letting go is of the one
    /// grabbed.
    fn set_grabbed(&mut self, grabbed: bool) -> Result<(), PlatformError> {
        let meant = self.capture.shared.handover.grab(grabbed);
        self.capture.send(Command::Grab(Grab { meant, grabbed }));
        Ok(())
    }

    /// The desktop takes the keyboard and the pointer from the focused app
    /// as it hands input over, and with them every key and button down.
    fn releases_carried_input_locally(&self) -> bool {
        false
    }

    /// Whether input goes to the agent: the session runs and the agent
    /// started capture since it began. Once the desktop closed it, or it
    /// failed, or it went away and came back, the agent starts capture
    /// again.
    fn capturing(&self) -> bool {
        self.capture.running() && self.capture.shared.handover.delivering()
    }

    /// Only while input is handed over: as it is let go of, the cursor goes
    /// where the agent puts it, coming back from another device or held at
    /// an edge (`Activation::warped`). Otherwise nothing here can move it.
    fn can_warp(&self) -> bool {
        self.capture.shared.handover.holds()
    }

    fn watch_hotkeys(&mut self, hotkeys: &[Hotkey]) {
        self.capture.shared.handover.watch_hotkeys(hotkeys);
    }

    /// Only where the cursor comes back as input is let go of: Wayland
    /// lets no client move the pointer.
    fn warp(&mut self, position: Point) -> Result<(), PlatformError> {
        self.capture.send(Command::Warp(position));
        Ok(())
    }

    fn set_exits(&mut self, exits: &[Exit]) {
        self.capture.send(Command::Exits(exits.to_vec()));
    }

    /// Only while input is handed over: the cursor leaves this device across
    /// an edge.
    fn grab_ready(&self) -> bool {
        self.capture.shared.handover.holds()
    }

    fn wants_exits(&self) -> bool {
        true
    }

    fn edge_wait(&mut self, handled: u64, waiting: bool) {
        self.capture.send(Command::EdgeWait(handled, waiting));
    }

    /// The desktop hands input over only at the barriers.
    fn edges_only(&self) -> bool {
        true
    }
}

/// What outlives a session: where the cursor leaves this device.
#[derive(Default)]
struct Kept {
    exits: Vec<Exit>,
}

/// Runs capture's sessions until the backend is dropped.
async fn run(shared: &Shared, commands: mpsc::UnboundedReceiver<Command>, options: CaptureOptions) {
    let mut sessions = Sessions {
        shared,
        tokens: options.lifecycle.tokens.clone(),
        waits: options.waits,
    };
    let commands = Commands::new(commands, Kept::default());
    portal_session::run(&mut sessions, commands, options.lifecycle).await;
}

/// What is capture's own of its sessions.
struct Sessions<'a> {
    shared: &'a Shared,
    tokens: Tokens,
    waits: Waits,
}

impl<'a> portal_session::Sessions for Sessions<'a> {
    type Command = Command;
    type Idle = Kept;
    const TOKEN_KEY: &'static str = TOKEN_KEY;
    const SESSION: &'static str = SESSION;
    const DOES: &'static str = "input capture";

    fn set(&self, state: State) {
        self.shared.set(state);
    }

    async fn session(
        &mut self,
        conn: &zbus::Connection,
        start: Start,
        commands: &mut Commands<Command, Kept>,
    ) -> Ended {
        session(conn, start, &self.tokens, self.waits, self.shared, commands).await
    }
}

/// The commands no session takes: the exits are kept for the next. A grab
/// is not answered: no input reaches the agent while no session runs
/// (`Shared::set` drops the channel), and the agent, which hears that
/// capture stopped (`InputCapture::capturing`), gives control back itself.
impl portal_session::Idle<Command> for Kept {
    fn idle(&mut self, command: Command) {
        if let Command::Exits(exits) = command {
            self.exits = exits;
        }
    }
}

/// A session's object path, as the portal's signals name it.
fn path_of(session: &Session<Portal>) -> Option<OwnedObjectPath> {
    use zbus::zvariant::{Endian, serialized::Context, to_bytes};
    let data = to_bytes(Context::new_dbus(Endian::Little, 0), session).ok()?;
    data.deserialize::<OwnedObjectPath>().ok().map(|(p, _)| p)
}

/// One session: created, started (the desktop may ask the user, for up to
/// `start.wait`), and run until it ends.
async fn session(
    conn: &zbus::Connection,
    start: Start,
    tokens: &Tokens,
    waits: Waits,
    shared: &Shared,
    commands: &mut Commands<Command, Kept>,
) -> Ended {
    let Start { why, wait, .. } = start;
    let portal = match outcome(
        commands
            .step(STEP, Portal::with_connection(conn.clone()))
            .await,
        "the desktop has no InputCapture portal",
        true,
    ) {
        Ok(portal) => portal,
        Err(ended) => return ended,
    };
    if portal.version() == 0 {
        return Ended::Failed {
            why: "the desktop has no InputCapture portal".to_owned(),
            before_start: true,
        };
    }
    if why == Why::Remembered && portal.version() < TOKEN_VERSION {
        // It would ask the user, unattended: the token is forgotten.
        return Ended::Failed {
            why: format!(
                "the desktop's InputCapture portal (version {}) asks at every start",
                portal.version()
            ),
            before_start: false,
        };
    }
    let capabilities = Capabilities::Keyboard | Capabilities::Pointer;
    let session = if portal.version() >= TOKEN_VERSION {
        let session = match outcome(
            commands
                .step(
                    STEP,
                    portal.create_session2(CreateSession2Options::default()),
                )
                .await,
            "cannot create an input capture session",
            true,
        ) {
            Ok(session) => session,
            Err(ended) => return ended,
        };
        let mut options = StartOptions::default()
            .set_capabilities(capabilities)
            .set_persist_mode(PersistMode::ExplicitlyRevoked);
        if let Some(token) = tokens.load(TOKEN_KEY) {
            options = options.set_restore_token(token);
        }
        let start = async {
            portal
                .start(&session, None, options)
                .await
                .and_then(|r| r.response())
        };
        let started = match commands.step(wait, start).await {
            Step::Done(Ok(started)) => started,
            Step::Done(Err(ashpd::Error::Response(ResponseError::Cancelled))) => {
                portal_session::close(&session).await;
                return Ended::Denied;
            }
            Step::Done(Err(e)) => {
                portal_session::close(&session).await;
                return Ended::Failed {
                    why: format!("the desktop did not start the session: {e}"),
                    before_start: false,
                };
            }
            Step::TimedOut => {
                portal_session::close(&session).await;
                return Ended::Failed {
                    why: "the desktop did not start the session in time".to_owned(),
                    before_start: false,
                };
            }
            Step::Stop(done) => {
                portal_session::close(&session).await;
                return Ended::Stop(done);
            }
        };
        if let Err(e) = tokens.save(TOKEN_KEY, started.restore_token()) {
            warn!(error = %e, "cannot keep the portal's restore token");
        }
        if !started.capabilities().contains(capabilities) {
            info!(capabilities = %started.capabilities(), "the desktop allowed capturing only some devices");
        }
        session
    } else {
        // Version 1 asks the user as it creates the session.
        let create = portal.create_session(
            None,
            CreateSessionOptions::default().set_capabilities(capabilities),
        );
        match commands.step(wait, create).await {
            Step::Done(Ok((session, granted))) => {
                if !granted.contains(capabilities) {
                    info!(capabilities = %granted, "the desktop allowed capturing only some devices");
                }
                session
            }
            Step::Done(Err(ashpd::Error::Response(ResponseError::Cancelled))) => {
                return Ended::Denied;
            }
            Step::Done(Err(e)) => {
                return Ended::Failed {
                    why: format!("cannot create an input capture session: {e}"),
                    before_start: true,
                };
            }
            Step::TimedOut => {
                return Ended::Failed {
                    why: "the desktop did not create the session in time".to_owned(),
                    before_start: true,
                };
            }
            Step::Stop(done) => return Ended::Stop(done),
        }
    };
    let ended = started(&portal, &session, waits, shared, commands).await;
    portal_session::close(&session).await;
    ended
}

/// The portal's signals, followed from before capture is enabled.
struct Signals<'a> {
    closed: Pin<Box<dyn Stream<Item = ()> + 'a>>,
    said: Pin<Box<dyn Stream<Item = Said> + 'a>>,
}

/// What the portal says of capture. Followed as one stream, in the order
/// it says it: an activation it ended at once must not seem to begin after
/// it ended. `Disabled` comes with its message's serial, to tell it from
/// one sent before capture was enabled again here.
enum Said {
    Activated(Activated),
    Deactivated(Deactivated),
    Disabled(Disabled, u32),
    ZonesChanged(ZonesChanged),
}

impl Said {
    fn of(message: &zbus::Message) -> Option<Self> {
        let header = message.header();
        let body = message.body();
        match header.member()?.as_str() {
            "Activated" => body.deserialize().ok().map(Self::Activated),
            "Deactivated" => body.deserialize().ok().map(Self::Deactivated),
            "Disabled" => {
                let serial = message.primary_header().serial_num().get();
                body.deserialize().ok().map(|d| Self::Disabled(d, serial))
            }
            "ZonesChanged" => body.deserialize().ok().map(Self::ZonesChanged),
            _ => None,
        }
    }
}

/// The portal's signals, from the portal alone: from the unique name that
/// owns its well-known one, which the bus matches; a rule's sender is also
/// checked for signals sent to this connection alone, which reach it
/// whatever its rules.
async fn signals<'a>(
    portal: &'a Portal,
    session: &'a Session<Portal>,
) -> ashpd::Result<Signals<'a>> {
    let bus = zbus::fdo::DBusProxy::new(portal.connection()).await?;
    let owner = bus
        .get_name_owner(zbus::names::BusName::try_from(DESKTOP).map_err(zbus::Error::from)?)
        .await
        .map_err(zbus::Error::from)?;
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(owner.as_str())?
        .interface(INTERFACE)?
        .path(DESKTOP_PATH)?
        .build();
    let said = zbus::MessageStream::for_match_rule(rule, portal.connection(), None)
        .await?
        .filter_map(|message| {
            futures_util::future::ready(message.ok().as_ref().and_then(Said::of))
        });
    Ok(Signals {
        closed: Box::pin(session.receive_closed().await?.map(|_| ()).fuse()),
        said: Box::pin(said.fuse()),
    })
}

/// The session started: the libei connection, then capture until it ends.
async fn started(
    portal: &Portal,
    session: &Session<Portal>,
    waits: Waits,
    shared: &Shared,
    commands: &mut Commands<Command, Kept>,
) -> Ended {
    let signals = match outcome(
        commands.step(STEP, signals(portal, session)).await,
        "cannot follow the session",
        false,
    ) {
        Ok(signals) => signals,
        Err(ended) => return ended,
    };
    let fd = match outcome(
        commands
            .step(STEP, portal.connect_to_eis(session, Default::default()))
            .await,
        "the desktop did not hand out a libei socket",
        false,
    ) {
        Ok(fd) => fd,
        Err(ended) => return ended,
    };
    let broken = |what: &str, e: &dyn std::fmt::Display| Ended::Failed {
        why: format!("{what}: {e}"),
        before_start: false,
    };
    let socket = UnixStream::from(fd);
    if let Err(e) = socket.set_nonblocking(true) {
        return broken("cannot use the libei socket", &e);
    }
    let context = match ei::Context::new(socket) {
        Ok(context) => context,
        Err(e) => return broken("cannot use the libei socket", &e),
    };
    let handshake = context.handshake_tokio("InputWeave", ei::handshake::ContextType::Receiver);
    let (connection, events) = match commands.step(STEP, handshake).await {
        Step::Done(Ok(done)) => done,
        Step::Done(Err(e)) => return broken("the libei handshake failed", &e),
        Step::TimedOut => return broken("the libei handshake failed", &"no answer in time"),
        Step::Stop(done) => return Ended::Stop(done),
    };
    // Signals of other sessions are not this one's: without its path, none
    // could be told apart.
    let Some(path) = path_of(session) else {
        return broken("cannot tell the session's path", &"no path");
    };
    shared.sessions.fetch_add(1, Ordering::SeqCst);
    shared.set(State::Running);
    let mut running = Running {
        portal,
        session,
        waits,
        leaves: !commands.idle.exits.is_empty(),
        path,
        connection,
        zones: Vec::new(),
        zone_set: None,
        barriers: Vec::new(),
        enabled: Some(false),
        place_at: Some(Instant::now()),
        rearms: 0,
        enabled_at: None,
        next_id: 1,
        warned_unplaced: false,
        pending: None,
        begin_at: None,
        early: Vec::new(),
        started: None,
        activation: None,
        pressed: Pressed::default(),
        scroll: Scroll::default(),
        shared,
    };
    let ended = running.run(signals, events, commands).await;
    running.end();
    ended
}

/// An activation the portal told of, not yet started by libei.
#[derive(Clone, Copy, Debug)]
struct Pending {
    id: u32,
    cursor: Option<Point>,
    barrier: Option<u32>,
}

/// Scrolling in the frame under way.
#[derive(Default)]
struct Scroll {
    discrete: Option<(i32, i32)>,
    smooth: (f32, f32),
}

/// A session that runs: its barriers and the input handed over.
struct Running<'a> {
    portal: &'a Portal,
    session: &'a Session<Portal>,
    waits: Waits,
    /// Whether the cursor leaves this device anywhere, as the agent last
    /// said: not while another device controls this one, whose input then
    /// reaches the edges. Input handed over meanwhile, at barriers still
    /// there, is let go of rather than reported.
    leaves: bool,
    /// The session's object path, which its signals name.
    path: OwnedObjectPath,
    connection: reis::event::Connection,
    zones: Vec<Rect>,
    zone_set: Option<u32>,
    barriers: Vec<Barrier>,
    /// Whether capture is enabled, if known: not after a call to enable it
    /// failed or went unanswered, which the portal may still have done.
    enabled: Option<bool>,
    /// When to put the barriers (again).
    place_at: Option<Instant>,
    /// Times capture was enabled again after the desktop disabled it, since
    /// input was last handed over or the exits or zones changed.
    rearms: u32,
    /// The serial of the portal's answer as capture was last enabled here:
    /// a `Disabled` it sent before is older news.
    enabled_at: Option<u32>,
    /// The id of the next barrier to put: ids are not given again, so an
    /// activation at an earlier barrier is told from one at these.
    next_id: u32,
    /// Exits on no zone were warned of once.
    warned_unplaced: bool,
    pending: Option<Pending>,
    /// When the pending activation, which libei started or whose input
    /// came, begins at the latest.
    begin_at: Option<Instant>,
    /// Input of the pending activation that came before it began.
    early: Vec<EiEvent>,
    /// The sequence libei last started the devices with.
    started: Option<u32>,
    activation: Option<Activation>,
    pressed: Pressed,
    scroll: Scroll,
    shared: &'a Shared,
}

impl Running<'_> {
    async fn run(
        &mut self,
        mut signals: Signals<'_>,
        mut events: reis::tokio::EiConvertEventStream,
        commands: &mut Commands<Command, Kept>,
    ) -> Ended {
        let began = Instant::now();
        let gone = |closed: bool| closed_or_lost(closed, began.elapsed(), SESSION);
        loop {
            // Input that came before the activation began goes now, in
            // order, ahead of anything after; with none begun, it goes.
            if self.activation.is_some() {
                for event in std::mem::take(&mut self.early) {
                    self.libei(event).await;
                }
            } else if self.pending.is_none() {
                self.early.clear();
            }
            let tick_at = self.activation.as_ref().and_then(Activation::deadline);
            let begin_at = self.begin_at;
            let place_at = self.place_at.filter(|_| self.activation.is_none());
            tokio::select! {
                event = events.next() => {
                    match event {
                        Some(Ok(event)) => {
                            if self.libei(event).await {
                                continue;
                            }
                        }
                        Some(Err(e)) => warn!(error = %e, "the libei connection broke"),
                        None => {}
                    }
                    let heard = tokio::time::timeout(CLOSED_WAIT, signals.closed.next()).await;
                    return gone(matches!(heard, Ok(Some(()))));
                }
                heard = signals.closed.next() => return gone(heard.is_some()),
                Some(said) = signals.said.next() => match said {
                    Said::Activated(a) if self.ours(a.session_handle().as_str()) => {
                        self.activated(&a).await;
                    }
                    Said::Deactivated(d) if self.ours(d.session_handle().as_str()) => {
                        self.deactivated(d.activation_id());
                    }
                    Said::Disabled(d, serial) if self.ours(d.session_handle().as_str()) => {
                        self.disabled(serial);
                    }
                    Said::ZonesChanged(z) if self.ours(z.session_handle().as_str()) => {
                        self.zones_changed(z.zone_set());
                    }
                    _ => {}
                },
                command = commands.recv() => match command {
                    // Input goes back to the desktop first, and the backend
                    // hears so then: closing the session may take long, and
                    // the agent may be exiting.
                    Some(Command::Stop(done)) => {
                        self.release_within(None, LET_GO_WAIT).await;
                        let _ = done.send(());
                        return Ended::Stop(None);
                    }
                    None => {
                        self.release_within(None, LET_GO_WAIT).await;
                        return Ended::Stop(None);
                    }
                    Some(command) => self.command(command, &mut commands.idle).await,
                },
                () = sleep_until(tick_at), if tick_at.is_some() => {
                    self.catch_up();
                    let verdict = self
                        .activation
                        .as_mut()
                        .map_or(Verdict::Keep, |a| a.tick(Instant::now()));
                    self.follow(verdict).await;
                }
                () = sleep_until(place_at), if place_at.is_some() => self.place(&commands.idle.exits).await,
                () = sleep_until(begin_at), if begin_at.is_some() => self.begin().await,
            }
        }
    }

    /// Whether a signal is about this session.
    fn ours(&self, path: &str) -> bool {
        self.path.as_str() == path
    }

    async fn command(&mut self, command: Command, kept: &mut Kept) {
        let now = Instant::now();
        match command {
            Command::Ask | Command::Stop(_) => {}
            Command::Exits(exits) => {
                if exits != kept.exits {
                    self.leaves = !exits.is_empty();
                    kept.exits = exits;
                    self.place_soon();
                }
            }
            Command::Grab(grab) => {
                let current = self.shared.handover.current();
                let quiet = self.shared.handover.quiet();
                match barriers::grab_taken(grab.meant, grab.grabbed, current, quiet) {
                    GrabTaken::Apply => {
                        if let Some(a) = self.activation.as_mut() {
                            a.set_grabbed(grab.grabbed, now);
                        }
                    }
                    // Nothing is handed over to hold: control comes back.
                    GrabTaken::Ended => {
                        let _ = self.shared.handover.report(InputEvent::CaptureEnded);
                    }
                    GrabTaken::Ignore => {
                        debug!(meant = grab.meant, current, "a grab of input that ended");
                    }
                }
            }
            Command::Warp(to) => {
                let verdict = self
                    .activation
                    .as_mut()
                    .map_or(Verdict::Keep, |a| a.warped(to));
                self.follow(verdict).await;
            }
            Command::EdgeWait(handled, waiting) => {
                // The agent takes every event reported, once and in order
                // (`Handover::reported`): it cannot have handled more.
                debug_assert!(handled <= self.shared.handover.reported());
                self.catch_up();
                let verdict = self
                    .activation
                    .as_mut()
                    .map_or(Verdict::Keep, |a| a.edge_wait(handled, waiting));
                self.follow(verdict).await;
            }
        }
    }

    /// The exits or the zones changed: the barriers are put again once
    /// they settled, and capture the desktop disabled by itself may be
    /// enabled again.
    fn place_soon(&mut self) {
        self.rearms = 0;
        let at = Instant::now() + SETTLE;
        self.place_at = Some(self.place_at.map_or(at, |t| t.min(at)));
    }

    /// Puts the barriers where the cursor leaves now, on the zones the
    /// desktop has now; capture is enabled if there are any.
    async fn place(&mut self, exits: &[Exit]) {
        self.place_at = None;
        match self.try_place(exits).await {
            Ok(()) => {}
            Err(e) => {
                warn!(
                    error = e,
                    "cannot put the pointer barriers; trying again later"
                );
                self.place_at = Some(Instant::now() + self.waits.place_retry);
            }
        }
    }

    async fn try_place(&mut self, exits: &[Exit]) -> Result<(), String> {
        let zones = call(self.waits.call, async {
            self.portal
                .zones(self.session, Default::default())
                .await
                .and_then(|r| r.response())
        })
        .await?;
        self.zones = zones
            .regions()
            .iter()
            .map(|r| {
                Rect::new(
                    f64::from(r.x_offset()),
                    f64::from(r.y_offset()),
                    f64::from(r.width()),
                    f64::from(r.height()),
                )
            })
            .collect();
        self.zone_set = Some(zones.zone_set());
        let first_id = self.next_id;
        let mut barriers = barriers::barriers(exits, &self.zones, first_id);
        // Not those too short for a pixel, which make no barrier either.
        let unplaced = barriers::on_no_zone(exits, &self.zones);
        if !unplaced.is_empty() {
            if !std::mem::replace(&mut self.warned_unplaced, true) {
                warn!(
                    count = unplaced.len(),
                    exits = ?unplaced,
                    zones = ?self.zones,
                    "some exits are on no zone of the desktop's: the cursor does not leave there"
                );
            } else {
                debug!(count = unplaced.len(), "some exits are on no zone");
            }
        }
        // GNOME's portal takes new barriers only while capture is disabled;
        // disabled again if that is not known, which does no harm.
        if self.enabled != Some(false) {
            call(
                self.waits.call,
                self.portal.disable(self.session, Default::default()),
            )
            .await?;
            // Disabled only now: if the call failed, it is made again next
            // time, before barriers that the portal takes only then.
            self.enabled = Some(false);
            // Whatever the desktop was about to hand over ends with it.
            self.pending = None;
            self.begin_at = None;
            self.started = None;
        }
        let list: Vec<PortalBarrier> = barriers
            .iter()
            .filter_map(|b| Some(PortalBarrier::new(NonZeroU32::new(b.id)?, b.line())))
            .collect();
        let answer = call(self.waits.call, async {
            self.portal
                .set_pointer_barriers(self.session, &list, zones.zone_set(), Default::default())
                .await
                .and_then(|r| r.response())
        })
        .await?;
        let failed: Vec<u32> = answer.failed_barriers().iter().map(|id| id.get()).collect();
        if !failed.is_empty() {
            warn!(?failed, "the desktop refused some pointer barriers");
        }
        barriers.retain(|b| !failed.contains(&b.id));
        debug!(barriers = ?barriers, "pointer barriers put");
        self.next_id = first_id.wrapping_add(list.len() as u32).max(1);
        self.barriers = barriers;
        if !self.barriers.is_empty() {
            // Not known until the portal answers: one that failed, or came
            // too late, may have enabled it all the same.
            self.enabled = None;
            self.enabled_at = Some(self.enable().await?);
            self.enabled = Some(true);
        }
        Ok(())
    }

    /// Enables capture; the serial of the portal's answer. Made by hand for
    /// that serial, which orders the answer among the portal's signals.
    async fn enable(&self) -> Result<u32, String> {
        let options: std::collections::HashMap<&str, zbus::zvariant::Value<'_>> =
            Default::default();
        let body = (self.session, options);
        let enable = self.portal.connection().call_method(
            Some(DESKTOP),
            DESKTOP_PATH,
            Some(INTERFACE),
            "Enable",
            &body,
        );
        let reply = call(self.waits.call, enable).await?;
        Ok(reply.primary_header().serial_num().get())
    }

    fn zones_changed(&mut self, invalidated: Option<u32>) {
        // One older than the zones the barriers are on is no news: compare
        // with wraparound, as the portal says to.
        let newer = match (self.zone_set, invalidated) {
            (Some(ours), Some(theirs)) => (ours.wrapping_sub(theirs) as i32) <= 0,
            _ => true,
        };
        if newer {
            debug!(zone_set = ?invalidated, "the desktop's zones changed");
            self.place_soon();
        }
    }

    async fn activated(&mut self, activated: &Activated) {
        let Some(id) = activated.activation_id() else {
            debug!("an activation without an id; letting go of it");
            self.release_unknown(None).await;
            return;
        };
        let barrier = match activated.barrier_id() {
            Some(ashpd::desktop::input_capture::ActivatedBarrier::Barrier(id)) => Some(id.get()),
            _ => None,
        };
        let cursor = activated
            .cursor_position()
            .map(|(x, y)| Point::new(f64::from(x), f64::from(y)));
        debug!(id, ?barrier, ?cursor, "input capture activated");
        if self.activation.is_some() {
            // The last one never said it ended.
            self.end();
        }
        // Nor did one that has not begun: forgotten, as one begun is ended,
        // without a `Release` that might let go of this one. Its input and
        // its wait for the modifiers are not this one's.
        if let Some(superseded) = self.pending.take() {
            debug!(
                superseded = superseded.id,
                id, "an activation that had not begun was superseded"
            );
            self.begin_at = None;
            self.early.clear();
        }
        self.pending = Some(Pending {
            id,
            cursor,
            barrier,
        });
        if self.started == Some(id) {
            self.begin_soon();
        }
    }

    /// libei started the pending activation, or its input came: it begins
    /// once the desktop told the modifiers held, or a moment passed. KDE
    /// tells them as it starts the devices for each activation; GNOME tells
    /// them as they change (and as the keyboard is added), not then.
    fn begin_soon(&mut self) {
        self.begin_at
            .get_or_insert(Instant::now() + self.waits.modifiers);
    }

    /// The pending activation begins: what the modifiers hold, then the
    /// push through the edge.
    async fn begin(&mut self) {
        self.begin_at = None;
        let Some(pending) = self.pending.take() else {
            return;
        };
        // Leaving nowhere, as while controlled: what the barriers still up
        // handed over is not this device's input to report.
        if !self.leaves {
            debug!(
                id = pending.id,
                "input handed over while the cursor leaves nowhere; letting go"
            );
            self.release_unknown(Some(pending.id)).await;
            return;
        }
        // At a barrier the portal names that is none of these, as one from
        // before they were put again, which the desktop ended as capture
        // was disabled for that: let go of.
        let Some(barrier) =
            barriers::barrier_hit(&self.barriers, pending.barrier, pending.cursor).cloned()
        else {
            debug!(
                id = pending.id,
                "input handed over at no barrier of ours; letting go"
            );
            self.release_unknown(Some(pending.id)).await;
            return;
        };
        let (activation, push) =
            Activation::new(pending.id, barrier, pending.cursor, Instant::now());
        let mut activation = activation.with_warp_wait(self.waits.warp);
        self.rearms = 0;
        // Not grabbed yet: the agent has not heard of it, and a grab of one
        // before is not of this one.
        self.shared.handover.begin();
        let modifiers = self.pressed.follow_mask();
        self.shared.handover.report_all(modifiers);
        if let Some(count) = self.shared.handover.report(push) {
            activation.pushed(count);
        }
        self.activation = Some(activation);
    }

    /// Reports `event` of the input handed over.
    fn report(&mut self, event: InputEvent) {
        let count = self.shared.handover.report(event);
        if let (Some(a), Some(count)) = (self.activation.as_mut(), count) {
            a.reported(count, Instant::now());
        }
    }

    /// Lets go of input handed over that is not ours to report.
    async fn release_unknown(&mut self, id: Option<u32>) {
        let options = ReleaseOptions::default().set_activation_id(id);
        if let Err(e) = call(self.waits.call, self.portal.release(self.session, options)).await {
            debug!(error = e, "cannot let go of input");
        }
    }

    fn deactivated(&mut self, id: Option<u32>) {
        let ours = |a: u32| id.is_none_or(|id| id == a);
        if self.pending.is_some_and(|p| ours(p.id)) {
            self.pending = None;
            self.begin_at = None;
        }
        if self.activation.as_ref().is_some_and(|a| ours(a.id)) {
            debug!(?id, "the desktop ended input capture");
            self.end();
        }
    }

    /// The portal says capture was disabled, in its message of `serial`.
    /// Not news while it is disabled here, nor before the portal answered
    /// that it was enabled again here: the portal may still say so of the
    /// `Disable` made here to put barriers, though it says it sends none.
    /// Its signals and answers come in order, as do their serials.
    fn disabled(&mut self, serial: u32) {
        if self.enabled != Some(true) || self.enabled_at.is_some_and(|enabled| serial < enabled) {
            debug!(
                serial,
                "the desktop says capture was disabled, before it was enabled here"
            );
            return;
        }
        debug!("the desktop disabled input capture");
        self.enabled = Some(false);
        self.pending = None;
        self.begin_at = None;
        self.end();
        let wait = self
            .waits
            .rearm
            .saturating_mul(1 << self.rearms.min(16))
            .min(self.waits.rearm_max);
        if self.rearms == REARMS {
            warn!("the desktop keeps disabling input capture; enabling it again, less often");
        }
        self.rearms = self.rearms.saturating_add(1);
        self.place_at = Some(Instant::now() + wait);
    }

    /// Input handed over ends: what is down is reported up and, unless the
    /// cursor came back here already, the router hears that capture holds
    /// input no more (`CaptureEnded`): control comes back if it is on
    /// another device, and a push at the edge, waiting to cross, starts
    /// over, as what the cursor does next goes unseen. Barriers that
    /// changed meanwhile are put next.
    fn end(&mut self) {
        // A grab of this activation from now on is of one that ended.
        self.shared.handover.end(self.returning());
        let ups = self.pressed.release_all();
        self.shared.handover.report_all(ups);
        self.scroll = Scroll::default();
        if self.activation.take().is_some_and(|a| !a.returning()) {
            let _ = self.shared.handover.report(InputEvent::CaptureEnded);
        }
    }

    /// Does what a verdict says.
    async fn follow(&mut self, verdict: Verdict) {
        if let Verdict::Release(at) = verdict {
            self.release(at).await;
        }
    }

    /// Lets go of input handed over, with the cursor at `at` if anywhere.
    async fn release(&mut self, at: Option<Point>) {
        self.release_within(at, self.waits.call).await;
    }

    /// Lets go of input handed over, waiting at most `wait` for the portal.
    async fn release_within(&mut self, at: Option<Point>, wait: Duration) {
        let Some(id) = self.activation.as_ref().map(|a| a.id) else {
            return;
        };
        debug!(id, ?at, "letting go of input");
        // Ended before the portal is told, so input coming meanwhile is not
        // reported.
        self.end();
        let options = ReleaseOptions::default()
            .set_activation_id(id)
            .set_cursor_position(at.map(|p| (p.x, p.y)));
        if let Err(e) = call(wait, self.portal.release(self.session, options)).await {
            warn!(error = e, "cannot let go of input");
        }
    }

    /// Follows libei. False once it disconnected.
    async fn libei(&mut self, event: EiEvent) -> bool {
        // Input handed over that has not begun waits for the modifiers held,
        // which the desktop may tell after it (`MODIFIERS_WAIT`): kept, and
        // reported once it begins, after them and the push.
        if self.activation.is_none()
            && self.pending.is_some()
            && matches!(
                event,
                EiEvent::PointerMotion(_)
                    | EiEvent::Button(_)
                    | EiEvent::KeyboardKey(_)
                    | EiEvent::ScrollDiscrete(_)
                    | EiEvent::ScrollDelta(_)
                    | EiEvent::Frame(_)
            )
        {
            self.begin_soon();
            barriers::keep_early(&mut self.early, event, EARLY_MAX, |e| {
                matches!(e, EiEvent::PointerMotion(_))
            });
            return true;
        }
        match event {
            EiEvent::Disconnected(e) => {
                info!(
                    reason = ?e.reason,
                    explanation = e.explanation.as_deref().unwrap_or(""),
                    "libei disconnected"
                );
                return false;
            }
            EiEvent::SeatAdded(e) => {
                e.seat.bind_capabilities(
                    DeviceCapability::Pointer
                        | DeviceCapability::Keyboard
                        | DeviceCapability::Button
                        | DeviceCapability::Scroll,
                );
                let _ = self.connection.flush();
            }
            EiEvent::DevicePaused(e) => {
                // Its modifiers are all let go of, libei says.
                if e.device.has_capability(DeviceCapability::Keyboard) {
                    self.pressed.set_mask(0);
                }
            }
            EiEvent::KeyboardModifiers(e) => {
                self.pressed.set_mask(e.depressed);
                if self.activation.is_some() {
                    for change in self.pressed.follow_mask() {
                        self.report(change);
                    }
                } else if self.begin_at.is_some() {
                    // What the activation waited for.
                    self.begin().await;
                }
            }
            EiEvent::DeviceStopEmulating(e) => {
                // What it held is let go of with its input.
                if e.device.has_capability(DeviceCapability::Keyboard) {
                    self.pressed.set_mask(0);
                }
            }
            EiEvent::DeviceStartEmulating(e) => {
                self.started = Some(e.sequence);
                if self.pending.is_some_and(|p| p.id == e.sequence) {
                    self.begin_soon();
                }
            }
            EiEvent::PointerMotion(m) => {
                if self.input_begins().await {
                    let a = self.activation.as_mut().expect("an activation");
                    let (report, verdict) =
                        a.moved(f64::from(m.dx), f64::from(m.dy), Instant::now());
                    if let Some(report) = report {
                        self.report(report);
                    }
                    self.follow(verdict).await;
                }
            }
            EiEvent::Button(b) => {
                let Some(button) = super::translate::button_of_ei(b.button) else {
                    return true;
                };
                if self.input_begins().await {
                    let pressed = b.state == ei::button::ButtonState::Press;
                    if !self.returning()
                        && let Some(report) = self.pressed.button(button, pressed)
                    {
                        self.report(report);
                    }
                    if pressed {
                        self.pressed_while_held(false).await;
                    }
                }
            }
            EiEvent::KeyboardKey(k) => {
                let Some(usage) = u16::try_from(k.key).ok().and_then(HidUsage::from_evdev) else {
                    return true;
                };
                if self.input_begins().await {
                    let pressed = k.state == ei::keyboard::KeyState::Press;
                    if !self.returning()
                        && let Some(report) = self.pressed.key(usage, pressed)
                    {
                        self.report(report);
                    }
                    if pressed {
                        let kept = usage.is_modifier() || self.makes_hotkey(usage);
                        self.pressed_while_held(kept).await;
                    }
                }
            }
            EiEvent::ScrollDiscrete(s) => {
                let (x, y) = self.scroll.discrete.unwrap_or_default();
                self.scroll.discrete = Some((x + s.discrete_dx, y + s.discrete_dy));
            }
            EiEvent::ScrollDelta(s) => {
                self.scroll.smooth.0 += s.dx;
                self.scroll.smooth.1 += s.dy;
            }
            EiEvent::Frame(_) => {
                let scroll = std::mem::take(&mut self.scroll);
                let natural = self.shared.natural.get();
                if let Some((dx, dy)) = barriers::scroll_of(scroll.discrete, scroll.smooth, natural)
                    && self.input_begins().await
                {
                    if !self.returning() {
                        self.report(InputEvent::Scroll { dx, dy });
                    }
                    self.pressed_while_held(false).await;
                }
            }
            _ => {}
        }
        true
    }

    /// Whether input from libei is to be reported: none comes while none is
    /// handed over (input before an activation began was kept for it).
    async fn input_begins(&mut self) -> bool {
        self.catch_up();
        self.activation.is_some()
    }

    /// The agent grabbed input as the cursor crossed, though the command
    /// saying so is yet to come (`Handover::grab`): held at the edge no
    /// more, so the rules for letting go of it there no longer apply.
    fn catch_up(&mut self) {
        if self.shared.handover.grabbed_ahead()
            && let Some(a) = self.activation.as_mut()
            && a.held()
        {
            a.set_grabbed(true, Instant::now());
        }
    }

    /// Whether the cursor came back here: what comes now is not reported,
    /// as the router put the cursor here already.
    fn returning(&self) -> bool {
        self.activation.as_ref().is_some_and(Activation::returning)
    }

    /// Whether `key`, down with the modifiers held now, makes a hotkey the
    /// router acts on.
    fn makes_hotkey(&self, key: HidUsage) -> bool {
        self.shared.handover.makes_hotkey(key, &self.pressed)
    }

    async fn pressed_while_held(&mut self, kept: bool) {
        let verdict = self
            .activation
            .as_mut()
            .map_or(Verdict::Keep, |a| a.pressed(kept));
        self.follow(verdict).await;
    }
}

async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at.into()).await,
        None => std::future::pending().await,
    }
}
