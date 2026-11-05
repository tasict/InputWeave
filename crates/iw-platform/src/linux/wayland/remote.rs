//! The RemoteDesktop portal's session, and the libei connection it hands
//! out, through which other devices' input reaches a Wayland desktop.
//!
//! A thread of its own runs the session on a small tokio runtime: D-Bus to
//! the portal (ashpd), then libei to the compositor (reis), as a sender.
//! Injection only queues what to send, so the engine never waits on the
//! desktop.
//!
//! How sessions start, end and start again, and which restore tokens are
//! kept, is `portal_session`'s, which capture shares; what is remote
//! control's own is here: asking for the clipboard with the session,
//! and sending input.

use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ashpd::desktop::remote_desktop::{
    ConnectToEISOptions, DeviceType, RemoteDesktop, SelectDevicesOptions, StartOptions,
};
use ashpd::desktop::{CreateSessionOptions, PersistMode, ResponseError, Session};
use futures_util::{Stream, StreamExt};
use iw_core::geometry::{Point, Rect};
use reis::ei;
use reis::event::{DeviceCapability, EiEvent};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::portal_clipboard::{Portal as Clipboard, Sharing};
use super::portal_session::{
    self, CLOSED_WAIT, Commands, Ended, Kind, Lifecycle, STEP, STOP_WAIT, Start, Step,
    closed_or_lost, outcome,
};
use super::tokens::Tokens;
use super::translate::{Portal, Scroll, within_regions};
use crate::PlatformError;
use crate::linux::common::lock;

/// The key of the RemoteDesktop portal's restore token in `portal.toml`.
const TOKEN_KEY: &str = "remote_desktop";
/// Its sessions, in the log.
const SESSION: &str = "remote desktop";
/// Whether the session the token is of had the clipboard (`yes`; missing
/// is no): a desktop restores only a session that asks for it alike (KDE;
/// GNOME keeps the choice in the token).
const CLIPBOARD_KEY: &str = "remote_desktop_clipboard";
/// How many times a session asks for the clipboard before it goes on
/// without it, unless it is tried again later (`ClipboardAsk`).
const CLIPBOARD_TRIES: u32 = 2;

/// The portal must hand out libei sockets: `ConnectToEIS` came with
/// version 2 (GNOME 46, KDE Plasma 6.1).
const EIS_VERSION: u32 = 2;

/// What injection sends to the compositor, already in libei's terms.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Input {
    /// To a point of the compositor's logical space.
    MoveTo(Point),
    MoveBy(f64, f64),
    /// An evdev button code, down or up.
    Button(u32, bool),
    /// An evdev key code, down or up.
    Key(u32, bool),
    Scroll(Scroll),
}

enum Command {
    /// The app asks for the permission.
    Ask,
    Input(Input),
    /// The backend is dropped: let go of what is held and end.
    Stop(std::sync::mpsc::Sender<()>),
}

impl portal_session::Command for Command {
    fn kind(self) -> Kind<Self> {
        match self {
            Command::Ask => Kind::Ask,
            Command::Stop(done) => Kind::Stop(done),
            input => Kind::Own(input),
        }
    }
}

/// Input while no session runs to take it is dropped.
struct Dropped;

impl portal_session::Idle<Command> for Dropped {
    fn idle(&mut self, _: Command) {}
}

/// Where to reach the portal, and where to keep its token.
pub(crate) struct RemoteOptions {
    /// Where the sessions reach the portal, and how they start again.
    pub lifecycle: Lifecycle,
    /// The clipboard, on a desktop that shares it only with a remote
    /// desktop session (GNOME): asked for with each session.
    pub clipboard: Option<Arc<Clipboard>>,
    /// How long asking for the clipboard waits for the desktop.
    pub clipboard_wait: Duration,
}

impl RemoteOptions {
    /// Options for the session bus of this process.
    pub(crate) fn new(tokens: Tokens, app_id: &'static str) -> Self {
        Self::with(Lifecycle::new(tokens, app_id))
    }

    /// Options whose sessions live as `lifecycle` says.
    pub(crate) fn with(lifecycle: Lifecycle) -> Self {
        Self {
            lifecycle,
            clipboard: None,
            clipboard_wait: STEP,
        }
    }
}

#[derive(Default)]
struct Shared {
    known: Mutex<Known>,
    /// Whether the session runs, read for every input without the lock.
    running: AtomicBool,
    /// Counts the sessions that ran, so injection forgets what it held
    /// down in an earlier one.
    sessions: AtomicU64,
}

/// The session and the app's requests, changed and read together.
#[derive(Default)]
struct Known {
    portal: Portal,
    /// Counts the times the app asked.
    asks: u64,
}

impl Shared {
    fn known(&self) -> std::sync::MutexGuard<'_, Known> {
        lock(&self.known)
    }

    fn set(&self, portal: Portal) {
        let mut known = self.known();
        if known.portal != portal {
            info!(from = ?known.portal, to = ?portal, "remote control through the portal");
            self.running
                .store(portal == Portal::Running, Ordering::SeqCst);
            known.portal = portal;
        }
    }
}

/// The session, run by a thread of its own.
pub(crate) struct Remote {
    shared: Arc<Shared>,
    commands: mpsc::UnboundedSender<Command>,
}

impl Remote {
    pub(crate) fn start(options: RemoteOptions) -> Result<Self, PlatformError> {
        let shared = Arc::new(Shared::default());
        // Starting with what the user allowed before from the first, as
        // the thread will: never seen as waiting for the app meanwhile.
        if options.lifecycle.tokens.load(TOKEN_KEY).is_some() {
            shared.set(Portal::Restoring);
        }
        let (commands, rx) = mpsc::unbounded_channel();
        let thread_shared = shared.clone();
        std::thread::Builder::new()
            .name("inputweave-portal".to_owned())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build();
                match runtime {
                    Ok(runtime) => runtime.block_on(run(&thread_shared, rx, options)),
                    Err(e) => thread_shared.set(Portal::Failed(format!("cannot run: {e}"))),
                }
            })
            .map_err(|e| PlatformError::Os(format!("cannot start the portal thread: {e}")))?;
        Ok(Self { shared, commands })
    }

    #[cfg(test)]
    pub(crate) fn portal(&self) -> Portal {
        self.shared.known().portal.clone()
    }

    /// The permission the session stands for, with the request it answers,
    /// read together.
    pub(crate) fn permission(&self, permission: crate::Permission) -> crate::PermissionState {
        let known = self.shared.known();
        known.portal.permission(permission, known.asks)
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
            // Shown as being asked about at once, as the app waits for it;
            // the request counted with it, under one lock, so a state is
            // never read as this request's before it is.
            let mut known = self.shared.known();
            if known.portal.may_start() {
                known.portal = Portal::Starting;
            }
            known.asks += 1;
            known.asks
        };
        if self.commands.send(Command::Ask).is_err() {
            // The thread is gone; asking cannot go anywhere.
            self.shared.set(Portal::Failed(
                "the thread running the portal session ended".to_owned(),
            ));
        }
        Ok(asked)
    }

    /// Queues `input`, while the session runs.
    pub(crate) fn send(&self, input: Input) -> Result<(), PlatformError> {
        if !self.running() {
            return Err(PlatformError::Unavailable(
                "remote control: the desktop has not allowed it",
            ));
        }
        self.commands
            .send(Command::Input(input))
            .map_err(|_| PlatformError::Unavailable("the portal thread ended"))
    }
}

impl Drop for Remote {
    /// Lets go of what is held and ends the session, waiting a moment for
    /// it so a held key is not left down.
    fn drop(&mut self) {
        let (done, wait) = std::sync::mpsc::channel();
        if self.commands.send(Command::Stop(done)).is_ok() {
            let _ = wait.recv_timeout(STOP_WAIT);
        }
    }
}

/// How a session asks for the clipboard.
#[derive(Clone, Copy)]
struct ClipboardAsk<'a> {
    clipboard: &'a Arc<Clipboard>,
    /// How long the desktop has to answer.
    wait: Duration,
    /// The start is tried again if it fails before it starts: unattended,
    /// with tries left.
    retry_later: bool,
}

/// Runs remote control's sessions until the backend is dropped.
async fn run(shared: &Shared, commands: mpsc::UnboundedReceiver<Command>, options: RemoteOptions) {
    let mut sessions = Sessions {
        shared,
        tokens: options.lifecycle.tokens.clone(),
        clipboard: options.clipboard,
        clipboard_wait: options.clipboard_wait,
    };
    let commands = Commands::new(commands, Dropped);
    portal_session::run(&mut sessions, commands, options.lifecycle).await;
}

/// What is remote control's own of its sessions.
struct Sessions<'a> {
    shared: &'a Shared,
    tokens: Tokens,
    clipboard: Option<Arc<Clipboard>>,
    clipboard_wait: Duration,
}

impl portal_session::Sessions for Sessions<'_> {
    type Command = Command;
    type Idle = Dropped;
    const TOKEN_KEY: &'static str = TOKEN_KEY;
    const SESSION: &'static str = SESSION;
    const DOES: &'static str = "remote control";

    fn set(&self, portal: Portal) {
        self.shared.set(portal);
    }

    async fn session(
        &mut self,
        conn: &zbus::Connection,
        start: Start,
        commands: &mut Commands<Command, Dropped>,
    ) -> Ended {
        let clipboard = self.clipboard.as_ref().map(|clipboard| ClipboardAsk {
            clipboard,
            wait: self.clipboard_wait,
            retry_later: start.retried,
        });
        session(
            conn,
            start.wait,
            &self.tokens,
            clipboard,
            self.shared,
            commands,
        )
        .await
    }
}

/// One session: started, waiting `wait` for the desktop to start it, and
/// run until it ends. Which tokens are kept is `portal_session::run`'s to
/// decide from how it ended; here the one the desktop hands out as it
/// starts the session is only written down.
async fn session(
    conn: &zbus::Connection,
    wait: Duration,
    tokens: &Tokens,
    clipboard: Option<ClipboardAsk<'_>>,
    shared: &Shared,
    commands: &mut Commands<Command, Dropped>,
) -> Ended {
    let portal = match outcome(
        commands
            .step(STEP, RemoteDesktop::with_connection(conn.clone()))
            .await,
        "the desktop has no RemoteDesktop portal",
        true,
    ) {
        Ok(portal) => portal,
        Err(ended) => return ended,
    };
    if portal.version() < EIS_VERSION {
        return Ended::Failed {
            why: format!(
                "the desktop's RemoteDesktop portal is missing or too old (version {}); libei \
                 needs version {EIS_VERSION}",
                portal.version()
            ),
            before_start: true,
        };
    }
    let session = match outcome(
        commands
            .step(STEP, portal.create_session(CreateSessionOptions::default()))
            .await,
        "cannot create a remote desktop session",
        true,
    ) {
        // Shared with the clipboard's task, while it runs.
        Ok(session) => Arc::new(session),
        Err(ended) => return ended,
    };
    let ended = started(&portal, &session, wait, tokens, clipboard, shared, commands).await;
    portal_session::close(&session).await;
    ended
}

/// The session created: started and run until it ends.
async fn started(
    portal: &RemoteDesktop,
    session: &Arc<Session<RemoteDesktop>>,
    wait: Duration,
    tokens: &Tokens,
    clipboard: Option<ClipboardAsk<'_>>,
    shared: &Shared,
    commands: &mut Commands<Command, Dropped>,
) -> Ended {
    // Followed from the start, so a close at any point is heard.
    let mut closed = match outcome(
        commands.step(STEP, session.receive_closed()).await,
        "cannot follow the session",
        true,
    ) {
        Ok(closed) => closed,
        Err(ended) => return ended,
    };
    let token = tokens.load(TOKEN_KEY);
    // Asked for before the session starts, as the portal wants. Restored,
    // the session asks as the one whose token it is did, since the desktop
    // restores only a session that asks alike (KDE). The desktop declining
    // it, the session goes on without. Asking failing on this side, or not
    // answered: a session restored unattended with tries left fails this
    // start before it used anything up, to be tried again; otherwise, as
    // on the last try or with someone there to answer a dialog, it asks
    // again, and failing again goes on without: remote control matters
    // more. Restored so on KDE, the start may then fail and forget the
    // token, and the app asks the user again.
    let ask = token.is_none() || tokens.load(CLIPBOARD_KEY).as_deref() == Some("yes");
    let restored = token.is_some();
    let mut following = None;
    if let Some(asking_for) = clipboard.filter(|_| ask) {
        let mut failed = 0;
        loop {
            let asking = async {
                if following.is_none() {
                    let clipboard = asking_for.clipboard;
                    following = Some(clipboard.follow(portal.connection(), session).await?);
                }
                let following = following.as_ref().expect("followed above");
                Ok::<_, Sharing>(following.ask().await)
            };
            let why = match commands.step(asking_for.wait, asking).await {
                Step::Done(Ok(Sharing::Failed(why)) | Err(Sharing::Failed(why))) => why,
                Step::Done(_) => break,
                Step::TimedOut => "the desktop did not answer".to_owned(),
                Step::Stop(done) => return Ended::Stop(done),
            };
            if restored && asking_for.retry_later {
                return Ended::Failed {
                    why: format!("cannot ask for the clipboard: {why}"),
                    before_start: true,
                };
            }
            failed += 1;
            if failed == CLIPBOARD_TRIES {
                warn!(
                    why,
                    "cannot ask for the clipboard; remote control goes on without it"
                );
                break;
            }
            debug!(why, "cannot ask for the clipboard; asking again");
        }
    }
    let mut select = SelectDevicesOptions::default()
        .set_devices(DeviceType::Keyboard | DeviceType::Pointer)
        .set_persist_mode(PersistMode::ExplicitlyRevoked);
    if let Some(token) = token.as_deref() {
        select = select.set_restore_token(token);
    }
    let choose = async {
        portal
            .select_devices(session, select)
            .await
            .and_then(|r| r.response())
    };
    if let Err(ended) = outcome(
        commands.step(STEP, choose).await,
        "cannot choose the devices to control",
        true,
    ) {
        return ended;
    }
    // The desktop may ask the user now, which takes as long as they take,
    // up to a point; it may end the session meanwhile, or the backend may
    // be dropped.
    let answer = tokio::select! {
        answer = tokio::time::timeout(wait, portal.start(session, None, StartOptions::default())) => answer,
        heard = closed.next() => {
            return match heard {
                Some(_) => closed_or_lost(true, Duration::ZERO, SESSION),
                // The token went to the desktop, which went away before
                // handing out another.
                None => Ended::Failed {
                    why: "the desktop's session went away as it started".to_owned(),
                    before_start: false,
                },
            };
        }
        stop = commands.stopped() => return Ended::Stop(stop),
    };
    let selected = match answer.map(|started| started.and_then(|r| r.response())) {
        Ok(Ok(selected)) => selected,
        Ok(Err(ashpd::Error::Response(ResponseError::Cancelled))) => return Ended::Denied,
        Ok(Err(e)) => {
            return Ended::Failed {
                why: format!("the desktop did not start the session: {e}"),
                before_start: false,
            };
        }
        Err(_) => {
            return Ended::Failed {
                why: "the desktop did not start the session in time".to_owned(),
                before_start: false,
            };
        }
    };
    // The desktop says whether the session has the clipboard: only a
    // request that reached it gets it one (a request not answered in time
    // may have). Without, the desktop or the user declined it, or asking
    // for it failed, and a session restored with this token does not ask.
    let had_clipboard = selected.is_clipboard_enabled();
    let kept = tokens.save_all(&[
        (TOKEN_KEY, selected.restore_token()),
        (
            CLIPBOARD_KEY,
            Some(if had_clipboard { "yes" } else { "no" }),
        ),
    ]);
    if let Err(e) = kept {
        warn!(error = %e, "cannot keep the portal's restore token");
    }
    if !selected
        .devices()
        .contains(DeviceType::Keyboard | DeviceType::Pointer)
    {
        info!(devices = ?selected.devices(), "the desktop allowed only some devices");
    }
    let fd = match outcome(
        commands
            .step(
                STEP,
                portal.connect_to_eis(session, ConnectToEISOptions::default()),
            )
            .await,
        "the desktop did not hand out a libei socket",
        false,
    ) {
        Ok(fd) => fd,
        Err(ended) => return ended,
    };
    // Shared with this session, while it runs: as the desktop says, however
    // asking for it went (a request not answered in time may have reached
    // it).
    let _clipboard = following.map(|f| f.start(had_clipboard));
    connected(UnixStream::from(fd), closed, shared, commands).await
}

/// Runs the libei connection on `socket` until the session ends.
async fn connected<C: Stream + Unpin>(
    socket: UnixStream,
    mut closed: C,
    shared: &Shared,
    commands: &mut Commands<Command, Dropped>,
) -> Ended {
    let broken = |what: &str, e: &dyn std::fmt::Display| Ended::Failed {
        why: format!("{what}: {e}"),
        before_start: false,
    };
    if let Err(e) = socket.set_nonblocking(true) {
        return broken("cannot use the libei socket", &e);
    }
    let context = match ei::Context::new(socket) {
        Ok(context) => context,
        Err(e) => return broken("cannot use the libei socket", &e),
    };
    let handshake = context.handshake_tokio("InputWeave", ei::handshake::ContextType::Sender);
    let (connection, mut events) = tokio::select! {
        done = tokio::time::timeout(STEP, handshake) => match done {
            Ok(Ok(done)) => done,
            Ok(Err(e)) => return broken("the libei handshake failed", &e),
            Err(_) => return broken("the libei handshake failed", &"no answer in time"),
        },
        heard = closed.next() => return closed_or_lost(heard.is_some(), Duration::ZERO, SESSION),
        // The handshake may hang; dropping the backend still ends it.
        stop = commands.stopped() => return Ended::Stop(stop),
    };
    shared.sessions.fetch_add(1, Ordering::SeqCst);
    shared.set(Portal::Running);
    let began = std::time::Instant::now();
    let mut sender = Sender::new(connection);
    loop {
        tokio::select! {
            event = events.next() => {
                match event {
                    Some(Ok(event)) => {
                        if sender.handle(event) {
                            continue;
                        }
                    }
                    Some(Err(e)) => warn!(error = %e, "the libei connection broke"),
                    None => {}
                }
                // The desktop says Closed as it ends a session; without
                // that, the session went away with what ran it.
                let heard = tokio::time::timeout(CLOSED_WAIT, closed.next()).await;
                return closed_or_lost(matches!(heard, Ok(Some(_))), began.elapsed(), SESSION);
            }
            heard = closed.next() => return closed_or_lost(heard.is_some(), began.elapsed(), SESSION),
            command = commands.recv() => match command {
                Some(Command::Input(input)) => sender.send(&input),
                Some(Command::Ask) => {}
                Some(Command::Stop(done)) => {
                    // Let go of, and said so at once: closing the session
                    // may take long, and the agent may be exiting.
                    sender.release_all();
                    let _ = done.send(());
                    return Ended::Stop(None);
                }
                None => {
                    sender.release_all();
                    return Ended::Stop(None);
                }
            },
        }
    }
}

/// A device the compositor gave, and how far along it is.
struct Device {
    device: reis::event::Device,
    /// Its regions, in the compositor's logical space, for an absolute
    /// pointer.
    regions: Vec<Rect>,
    resumed: bool,
    emulating: bool,
}

/// The sending end of the libei connection.
struct Sender {
    connection: reis::event::Connection,
    devices: Vec<Device>,
    sequence: u32,
    /// The device the pointer last moved with, whose buttons and wheel go
    /// with it.
    pointer: Option<reis::event::Device>,
    /// What went down through this connection, to let go of at the end.
    down: Vec<Input>,
    /// Input nothing could take was reported once.
    dropped: bool,
}

impl Sender {
    fn new(connection: reis::event::Connection) -> Self {
        Self {
            connection,
            devices: Vec::new(),
            sequence: 0,
            pointer: None,
            down: Vec::new(),
            dropped: false,
        }
    }

    /// Follows the compositor's seats and devices. False once it
    /// disconnected.
    fn handle(&mut self, event: EiEvent) -> bool {
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
                        | DeviceCapability::PointerAbsolute
                        | DeviceCapability::Keyboard
                        | DeviceCapability::Button
                        | DeviceCapability::Scroll,
                );
                let _ = self.connection.flush();
            }
            EiEvent::DeviceAdded(e) => {
                debug!(name = e.device.name().unwrap_or(""), "libei device added");
                let regions = e
                    .device
                    .regions()
                    .iter()
                    .map(|r| {
                        Rect::new(
                            f64::from(r.x),
                            f64::from(r.y),
                            f64::from(r.width),
                            f64::from(r.height),
                        )
                    })
                    .collect();
                self.devices.push(Device {
                    device: e.device,
                    regions,
                    resumed: false,
                    emulating: false,
                });
            }
            EiEvent::DeviceRemoved(e) => {
                self.devices.retain(|d| d.device != e.device);
                if self.pointer.as_ref() == Some(&e.device) {
                    self.pointer = None;
                }
            }
            EiEvent::DevicePaused(e) => {
                if let Some(d) = self.devices.iter_mut().find(|d| d.device == e.device) {
                    d.resumed = false;
                    d.emulating = false;
                }
            }
            EiEvent::DeviceResumed(e) => {
                if let Some(d) = self.devices.iter_mut().find(|d| d.device == e.device) {
                    d.resumed = true;
                }
            }
            _ => {}
        }
        true
    }

    /// A resumed device that can, `preferred` first.
    fn find(
        &mut self,
        capability: DeviceCapability,
        preferred: Option<&reis::event::Device>,
    ) -> Option<usize> {
        let able = |d: &Device| d.resumed && d.device.has_capability(capability);
        preferred
            .and_then(|p| self.devices.iter().position(|d| &d.device == p && able(d)))
            .or_else(|| self.devices.iter().position(able))
    }

    /// Sends `input`, remembering what went down to let go of it at the
    /// end.
    fn send(&mut self, input: &Input) {
        if !self.emit(input) {
            return;
        }
        match *input {
            Input::Button(code, pressed) => {
                remember(&mut self.down, Input::Button(code, true), pressed);
            }
            Input::Key(code, pressed) => remember(&mut self.down, Input::Key(code, true), pressed),
            _ => {}
        }
    }

    /// Sends `input` through the device that takes it; whether one did.
    fn emit(&mut self, input: &Input) -> bool {
        let (capability, preferred) = match input {
            Input::MoveTo(_) => (DeviceCapability::PointerAbsolute, None),
            Input::MoveBy(..) => (DeviceCapability::Pointer, None),
            Input::Button(..) => (DeviceCapability::Button, self.pointer.clone()),
            Input::Scroll(_) => (DeviceCapability::Scroll, self.pointer.clone()),
            Input::Key(..) => (DeviceCapability::Keyboard, None),
        };
        let Some(index) = self.find(capability, preferred.as_ref()) else {
            if !std::mem::replace(&mut self.dropped, true) {
                warn!(
                    ?capability,
                    "no libei device can take this input; dropping such input"
                );
            }
            return false;
        };
        let serial = self.connection.serial();
        let device = &mut self.devices[index];
        if !device.emulating {
            device
                .device
                .device()
                .start_emulating(serial, self.sequence);
            self.sequence = self.sequence.wrapping_add(1);
            device.emulating = true;
        }
        let d = &device.device;
        let sent = match *input {
            Input::MoveTo(point) => d.interface::<ei::PointerAbsolute>().map(|p| {
                let point = within_regions(point, &device.regions).unwrap_or(point);
                p.motion_absolute(point.x as f32, point.y as f32);
            }),
            Input::MoveBy(dx, dy) => d
                .interface::<ei::Pointer>()
                .map(|p| p.motion_relative(dx as f32, dy as f32)),
            Input::Button(code, pressed) => d.interface::<ei::Button>().map(|b| {
                let state = if pressed {
                    ei::button::ButtonState::Press
                } else {
                    ei::button::ButtonState::Released
                };
                b.button(code, state);
            }),
            Input::Scroll(Scroll::Steps(x, y)) => {
                d.interface::<ei::Scroll>().map(|s| s.scroll_discrete(x, y))
            }
            Input::Scroll(Scroll::Pixels(x, y)) => {
                d.interface::<ei::Scroll>().map(|s| s.scroll(x, y))
            }
            Input::Key(code, pressed) => d.interface::<ei::Keyboard>().map(|k| {
                let state = if pressed {
                    ei::keyboard::KeyState::Press
                } else {
                    ei::keyboard::KeyState::Released
                };
                k.key(code, state);
            }),
        };
        if sent.is_none() {
            return false;
        }
        d.device().frame(serial, now_micros());
        if let Err(e) = self.connection.flush() {
            debug!(error = %e, "cannot flush libei");
        }
        if matches!(input, Input::MoveTo(_) | Input::MoveBy(..)) {
            self.pointer = Some(d.clone());
        }
        true
    }

    /// Lets go of every key and button that went down.
    fn release_all(&mut self) {
        for input in std::mem::take(&mut self.down) {
            let up = match input {
                Input::Button(code, _) => Input::Button(code, false),
                Input::Key(code, _) => Input::Key(code, false),
                _ => continue,
            };
            self.emit(&up);
        }
    }
}

fn remember(down: &mut Vec<Input>, press: Input, pressed: bool) {
    down.retain(|d| *d != press);
    if pressed {
        down.push(press);
    }
}

/// libei's timestamps: `CLOCK_MONOTONIC`, in microseconds.
fn now_micros() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is valid for writing; CLOCK_MONOTONIC always exists.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    (ts.tv_sec as u64).saturating_mul(1_000_000) + (ts.tv_nsec as u64) / 1_000
}
