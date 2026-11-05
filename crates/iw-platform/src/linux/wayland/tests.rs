//! The Wayland backends against stand-ins: a RemoteDesktop portal of the
//! test's own on a private D-Bus daemon, handing out a libei server of the
//! test's own, and headless sway for the outputs. Ignored, as they need
//! `dbus-daemon` and sway; `scripts/linux-wayland-tests.sh` runs them in the
//! Linux container. The clipboards' are in `clipboard`.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iw_core::geometry::{Point, Rect};
use iw_core::input::Button;
use iw_keymap::usage;
use reis::PendingRequestResult;
use reis::eis;
use reis::request::{DeviceCapability, EisRequest, EisRequestConverter};
use zbus::object_server::SignalEmitter;
use zbus::{ObjectPath, OwnedObjectPath, OwnedVariant, Variant};

use super::portal_session::Lifecycle;
use super::remote::{Remote, RemoteOptions};
use super::tokens::Tokens;
use super::translate::Portal;
use super::{PortalEmulation, Reaches, WaylandStatus};
use crate::{Displays, InputEmulation, Permission, PermissionAsk, SessionStatus};

mod clipboard;

const WAIT: Duration = Duration::from_secs(10);

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A session bus of the test's own.
struct Bus {
    child: Child,
    address: String,
    dir: Arc<tempfile::TempDir>,
}

impl Bus {
    fn start() -> Self {
        Self::start_at(Arc::new(tempfile::tempdir().unwrap()))
    }

    /// A bus at `dir`'s address, as again after the bus restarted.
    fn start_at(dir: Arc<tempfile::TempDir>) -> Self {
        let _ = std::fs::remove_file(dir.path().join("bus"));
        let mut child = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--nopidfile", "--print-address=1"])
            .arg(format!(
                "--address=unix:path={}",
                dir.path().join("bus").display()
            ))
            // Its own, whatever another test left in this process'.
            .env("XDG_RUNTIME_DIR", dir.path())
            .stdout(Stdio::piped())
            .spawn()
            .expect("dbus-daemon");
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        Self {
            child,
            address: line.trim().to_owned(),
            dir,
        }
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Answer {
    Allow,
    Refuse,
    /// The desktop fails to start the session, as other than the user's no.
    Fail,
}

/// What the fake portal was asked, and answers.
#[derive(Default)]
struct Asked {
    calls: Vec<&'static str>,
    persist_modes: Vec<u32>,
    restore_tokens: Vec<Option<String>>,
    answers: VecDeque<Answer>,
    tokens_given: u32,
    sessions: Vec<OwnedObjectPath>,
    closed: Vec<OwnedObjectPath>,
    /// The libei servers' ends of their sockets, to end them as a
    /// compositor going away would.
    eis_sockets: Vec<UnixStream>,
    /// How long `Start` takes to answer, as while the desktop asks the
    /// user; 20 ms when `None`.
    start_delay: Option<Duration>,
    /// Each libei connection ends this long after it was handed out, as a
    /// compositor that keeps going away.
    eis_lifetime: Option<Duration>,
    /// The libei server never answers, so the handshake hangs.
    eis_silent: bool,
    /// `CreateSession` never returns.
    hang_create: bool,
    /// A session's `Close` never returns, as a desktop busy at logout.
    hang_close: bool,
    /// The clipboard was asked for (`clipboard::FakeClipboard`), so
    /// `Start` shares it.
    clipboard_requested: bool,
    /// `Start` shares no clipboard even so, as when the desktop or the
    /// user does not allow it.
    clipboard_refused: bool,
}

struct FakePortal {
    asked: Arc<Mutex<Asked>>,
    eis: std::sync::mpsc::Sender<UnixStream>,
}

fn option<'a>(options: &'a HashMap<String, OwnedVariant>, key: &str) -> Option<&'a OwnedVariant> {
    options.get(key)
}

fn string(options: &HashMap<String, OwnedVariant>, key: &str) -> Option<String> {
    option(options, key).and_then(|v| v.downcast_ref::<&str>().ok().map(str::to_owned))
}

fn request_path(
    header: &zbus::message::Header<'_>,
    options: &HashMap<String, OwnedVariant>,
) -> OwnedObjectPath {
    let sender = header
        .sender()
        .unwrap()
        .trim_start_matches(':')
        .replace('.', "_");
    let token = string(options, "handle_token").unwrap();
    ObjectPath::try_from(format!(
        "/org/freedesktop/portal/desktop/request/{sender}/{token}"
    ))
    .unwrap()
    .into()
}

/// Answers the request at `path`, a moment after the call returned it.
fn respond(
    conn: &zbus::Connection,
    path: OwnedObjectPath,
    code: u32,
    results: HashMap<String, OwnedVariant>,
) {
    respond_after(conn, path, code, results, Duration::from_millis(20));
}

/// Answers the request at `path`, `delay` after the call returned it.
fn respond_after(
    conn: &zbus::Connection,
    path: OwnedObjectPath,
    code: u32,
    results: HashMap<String, OwnedVariant>,
    delay: Duration,
) {
    let conn = conn.clone();
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        conn.emit_signal(
            None::<()>,
            &path,
            "org.freedesktop.portal.Request",
            "Response",
            &(code, results),
        )
        .await
        .unwrap();
    });
}

fn value(v: Variant<'_>) -> OwnedVariant {
    v.try_to_owned().unwrap()
}

#[zbus::interface(name = "org.freedesktop.portal.RemoteDesktop")]
impl FakePortal {
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        2
    }

    #[zbus(property, name = "AvailableDeviceTypes")]
    fn available_device_types(&self) -> u32 {
        3
    }

    async fn create_session(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(object_server)] server: &zbus::ObjectServer,
        options: HashMap<String, OwnedVariant>,
    ) -> OwnedObjectPath {
        let hang = {
            let mut asked = self.asked.lock().unwrap();
            if asked.hang_create {
                asked.calls.push("CreateSession");
            }
            asked.hang_create
        };
        if hang {
            std::future::pending::<()>().await;
        }
        let sender = header
            .sender()
            .unwrap()
            .trim_start_matches(':')
            .replace('.', "_");
        let token = string(&options, "session_handle_token").unwrap();
        let session: OwnedObjectPath = ObjectPath::try_from(format!(
            "/org/freedesktop/portal/desktop/session/{sender}/{token}"
        ))
        .unwrap()
        .into();
        server
            .at(
                &session,
                FakeSession {
                    asked: self.asked.clone(),
                    path: session.clone(),
                },
            )
            .await
            .unwrap();
        let mut asked = self.asked.lock().unwrap();
        asked.calls.push("CreateSession");
        asked.sessions.push(session.clone());
        let request = request_path(&header, &options);
        respond(
            conn,
            request.clone(),
            0,
            HashMap::from([(
                "session_handle".to_owned(),
                value(Variant::from(session.as_str().to_owned())),
            )]),
        );
        request
    }

    async fn select_devices(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        _session: ObjectPath<'_>,
        options: HashMap<String, OwnedVariant>,
    ) -> OwnedObjectPath {
        let mut asked = self.asked.lock().unwrap();
        asked.calls.push("SelectDevices");
        let persist = option(&options, "persist_mode")
            .and_then(|v| v.downcast_ref::<u32>().ok())
            .unwrap_or(0);
        asked.persist_modes.push(persist);
        asked.restore_tokens.push(string(&options, "restore_token"));
        let request = request_path(&header, &options);
        respond(conn, request.clone(), 0, HashMap::new());
        request
    }

    async fn start(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        _session: ObjectPath<'_>,
        _parent_window: &str,
        options: HashMap<String, OwnedVariant>,
    ) -> OwnedObjectPath {
        let mut asked = self.asked.lock().unwrap();
        asked.calls.push("Start");
        let request = request_path(&header, &options);
        let delay = asked.start_delay.unwrap_or(Duration::from_millis(20));
        match asked.answers.pop_front().unwrap_or(Answer::Allow) {
            Answer::Allow => {
                asked.tokens_given += 1;
                let token = format!("token-{}", asked.tokens_given);
                let mut results = HashMap::from([
                    ("devices".to_owned(), value(Variant::from(3u32))),
                    ("restore_token".to_owned(), value(Variant::from(token))),
                ]);
                if std::mem::take(&mut asked.clipboard_requested) && !asked.clipboard_refused {
                    results.insert("clipboard_enabled".to_owned(), value(Variant::from(true)));
                }
                respond_after(conn, request.clone(), 0, results, delay);
            }
            // As xdg-desktop-portal answers when the user cancels.
            Answer::Refuse => respond_after(conn, request.clone(), 1, HashMap::new(), delay),
            Answer::Fail => respond_after(conn, request.clone(), 2, HashMap::new(), delay),
        }
        request
    }

    #[zbus(name = "ConnectToEIS")]
    async fn connect_to_eis(
        &self,
        _session: ObjectPath<'_>,
        _options: HashMap<String, OwnedVariant>,
    ) -> zbus::OwnedFd {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let (silent, lifetime) = {
            let mut asked = self.asked.lock().unwrap();
            asked.calls.push("ConnectToEIS");
            asked.eis_sockets.push(ours.try_clone().unwrap());
            (asked.eis_silent, asked.eis_lifetime)
        };
        if let Some(lifetime) = lifetime {
            let end = ours.try_clone().unwrap();
            std::thread::spawn(move || {
                std::thread::sleep(lifetime);
                let _ = end.shutdown(std::net::Shutdown::Both);
            });
        }
        if !silent {
            self.eis.send(ours).unwrap();
        }
        std::os::fd::OwnedFd::from(theirs).into()
    }
}

struct FakeSession {
    asked: Arc<Mutex<Asked>>,
    path: OwnedObjectPath,
}

#[zbus::interface(name = "org.freedesktop.portal.Session")]
impl FakeSession {
    async fn close(&self) {
        let hang = {
            let mut asked = self.asked.lock().unwrap();
            asked.closed.push(self.path.clone());
            asked.hang_close
        };
        if hang {
            std::future::pending::<()>().await;
        }
    }

    #[zbus(signal)]
    async fn closed(
        emitter: &SignalEmitter<'_>,
        details: HashMap<&str, Variant<'_>>,
    ) -> zbus::Result<()>;
}

/// What the libei server heard.
#[derive(Clone, Debug, PartialEq)]
enum Heard {
    Bound,
    MotionAbsolute(f32, f32),
    Motion(f32, f32),
    Button(u32, bool),
    Key(u32, bool),
    Scroll(i32, i32),
    ScrollSmooth(f32, f32),
}

/// The fake portal on its bus, and the libei server behind it.
struct Desktop {
    bus: Bus,
    asked: Arc<Mutex<Asked>>,
    heard: Arc<Mutex<Vec<Heard>>>,
    conn: zbus::Connection,
    runtime: tokio::runtime::Runtime,
}

impl Desktop {
    fn start() -> Self {
        Self::serve(Bus::start())
    }

    /// The fake portal on `bus`.
    fn serve(bus: Bus) -> Self {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let asked = Arc::new(Mutex::new(Asked::default()));
        let heard = Arc::new(Mutex::new(Vec::new()));
        let (eis, connections) = std::sync::mpsc::channel::<UnixStream>();
        let server_heard = heard.clone();
        std::thread::spawn(move || {
            while let Ok(stream) = connections.recv() {
                let heard = server_heard.clone();
                std::thread::spawn(move || serve_eis(stream, &heard));
            }
        });
        let portal = FakePortal {
            asked: asked.clone(),
            eis,
        };
        let conn = runtime.block_on(async {
            zbus::connection::Builder::address(bus.address.as_str())
                .name("org.freedesktop.portal.Desktop")
                .serve_at("/org/freedesktop/portal/desktop", portal)
                .build()
                .await
                .unwrap()
        });
        Self {
            bus,
            asked,
            heard,
            conn,
            runtime,
        }
    }

    fn remote(&self, tokens: &Path) -> Arc<Remote> {
        Arc::new(
            Remote::start(RemoteOptions::with(Lifecycle {
                bus: Some(self.bus.address.clone()),
                retries: vec![Duration::from_millis(100)],
                ..Lifecycle::new(Tokens::in_dir(Some(tokens)), super::APP_ID)
            }))
            .unwrap(),
        )
    }

    fn calls(&self) -> Vec<&'static str> {
        self.asked.lock().unwrap().calls.clone()
    }

    /// The compositor goes away: the libei connections end, and the portal
    /// says nothing.
    fn end_libei(&self) {
        for socket in self.asked.lock().unwrap().eis_sockets.drain(..) {
            let _ = socket.shutdown(std::net::Shutdown::Both);
        }
    }

    /// The desktop ends the last session, as when the user stops sharing.
    fn close_last_session(&self) {
        let path = self.asked.lock().unwrap().sessions.last().unwrap().clone();
        self.runtime.block_on(async {
            let emitter = SignalEmitter::new(&self.conn, path).unwrap();
            FakeSession::closed(&emitter, HashMap::new()).await.unwrap();
        });
    }

    fn heard(&self) -> Vec<Heard> {
        self.heard.lock().unwrap().clone()
    }

    /// Waits for `what` to be heard last, and forgets what was heard.
    fn expect(&self, what: &[Heard]) {
        wait_until(&format!("{what:?}"), || self.heard().ends_with(what));
        self.heard.lock().unwrap().clear();
    }
}

/// Waits until `fd` has something to read, or a moment passed.
fn readable(fd: &impl AsRawFd) {
    let mut poll = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one valid pollfd.
    unsafe { libc::poll(&mut poll, 1, 100) };
}

/// A libei server like a compositor's: one seat; a relative pointer, an
/// absolute one over two displays side by side, and a keyboard.
fn serve_eis(stream: UnixStream, heard: &Mutex<Vec<Heard>>) {
    stream.set_nonblocking(true).unwrap();
    let context = eis::Context::new(stream.try_clone().unwrap()).unwrap();
    let mut handshaker = reis::handshake::EisHandshaker::new(&context, 1);
    let mut converter: Option<EisRequestConverter> = None;
    // Kept for as long as the connection: the seat the devices are on.
    let mut _seat = None;
    loop {
        readable(&stream);
        // Nothing read is not the end: that is UnexpectedEof.
        match context.read() {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(_) => return,
        }
        while let Some(result) = context.pending_request() {
            let PendingRequestResult::Request(request) = result else {
                return;
            };
            match &mut converter {
                None => {
                    if let Some(resp) = handshaker.handle_request(request).unwrap() {
                        let c = EisRequestConverter::new(&context, resp, 1);
                        _seat = Some(c.handle().add_seat(
                            Some("default"),
                            DeviceCapability::Pointer
                                | DeviceCapability::PointerAbsolute
                                | DeviceCapability::Keyboard
                                | DeviceCapability::Button
                                | DeviceCapability::Scroll,
                        ));
                        let _ = c.handle().flush();
                        converter = Some(c);
                    }
                }
                Some(c) => c.handle_request(request).unwrap(),
            }
        }
        let Some(c) = &mut converter else {
            let _ = context.flush();
            continue;
        };
        while let Some(request) = c.next_request() {
            let mut heard = heard.lock().unwrap();
            match request {
                EisRequest::Bind(bind) => {
                    let pointer = bind.seat.add_device(
                        Some("pointer"),
                        eis::device::DeviceType::Virtual,
                        DeviceCapability::Pointer
                            | DeviceCapability::Button
                            | DeviceCapability::Scroll,
                        |_| {},
                    );
                    let absolute = bind.seat.add_device(
                        Some("absolute"),
                        eis::device::DeviceType::Virtual,
                        DeviceCapability::PointerAbsolute
                            | DeviceCapability::Button
                            | DeviceCapability::Scroll,
                        |d| {
                            d.device().region(0, 0, 1920, 1080, 1.0);
                            d.device().region(1920, 0, 1280, 1024, 1.0);
                        },
                    );
                    let keyboard = bind.seat.add_device(
                        Some("keyboard"),
                        eis::device::DeviceType::Virtual,
                        DeviceCapability::Keyboard.into(),
                        |_| {},
                    );
                    for d in [pointer, absolute, keyboard] {
                        d.resumed();
                    }
                    heard.push(Heard::Bound);
                }
                EisRequest::PointerMotionAbsolute(m) => {
                    heard.push(Heard::MotionAbsolute(m.dx_absolute, m.dy_absolute));
                }
                EisRequest::PointerMotion(m) => heard.push(Heard::Motion(m.dx, m.dy)),
                EisRequest::Button(b) => heard.push(Heard::Button(
                    b.button,
                    b.state == eis::button::ButtonState::Press,
                )),
                EisRequest::KeyboardKey(k) => {
                    heard.push(Heard::Key(k.key, k.state == eis::keyboard::KeyState::Press))
                }
                EisRequest::ScrollDiscrete(s) => {
                    heard.push(Heard::Scroll(s.discrete_dx, s.discrete_dy));
                }
                EisRequest::ScrollDelta(s) => heard.push(Heard::ScrollSmooth(s.dx, s.dy)),
                _ => {}
            }
        }
        let _ = c.handle().flush();
    }
}

fn remote_control(status: &WaylandStatus) -> crate::PermissionState {
    status
        .permissions()
        .into_iter()
        .find(|p| p.permission == Permission::RemoteControl)
        .unwrap()
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn remote_control_is_asked_for_once_and_injects() {
    let desktop = Desktop::start();
    let state = tempfile::tempdir().unwrap();
    let remote = desktop.remote(state.path());
    let status = WaylandStatus::new(Reaches::Portal(remote.clone()));
    let mut emulation = PortalEmulation::new(remote.clone());

    // Nothing remembered: the agent does not ask by itself.
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(desktop.calls(), Vec::<&str>::new());
    let before = remote_control(&status);
    assert!(!before.granted);
    assert_eq!(before.ask, PermissionAsk::Askable);
    assert!(!status.input_available(), "other devices keep off it");
    assert!(matches!(
        emulation.key(usage::A, true),
        Err(crate::PlatformError::Unavailable(_))
    ));

    assert_eq!(
        status
            .request_permission(Permission::RemoteControl)
            .unwrap(),
        1
    );
    let asking = remote_control(&status);
    assert_eq!((asking.ask, asking.asked), (PermissionAsk::Asking, 1));
    wait_until("the session to run", || remote.portal() == Portal::Running);
    assert!(remote_control(&status).granted);
    assert!(status.input_available());
    assert_eq!(
        desktop.calls(),
        ["CreateSession", "SelectDevices", "Start", "ConnectToEIS"]
    );
    {
        let asked = desktop.asked.lock().unwrap();
        assert_eq!(asked.persist_modes, [2], "until the user revokes it");
        assert_eq!(asked.restore_tokens, [None]);
    }
    assert_eq!(
        Tokens::in_dir(Some(state.path()))
            .load("remote_desktop")
            .as_deref(),
        Some("token-1")
    );
    desktop.expect(&[Heard::Bound]);
    // The devices resumed reach the agent a moment after the bind.
    std::thread::sleep(Duration::from_millis(200));

    emulation.move_to(Point::new(100.0, 200.0)).unwrap();
    desktop.expect(&[Heard::MotionAbsolute(100.0, 200.0)]);
    // Below the shorter display: onto its bottom row.
    emulation.move_to(Point::new(2500.0, 1060.0)).unwrap();
    desktop.expect(&[Heard::MotionAbsolute(2500.0, 1023.0)]);
    emulation.button(Button::Left, true).unwrap();
    emulation.button(Button::Left, false).unwrap();
    desktop.expect(&[Heard::Button(0x110, true), Heard::Button(0x110, false)]);
    emulation.key(usage::A, true).unwrap();
    // The controller's repeat of a key held is not pressed again.
    emulation.key(usage::A, true).unwrap();
    emulation.key(usage::A, false).unwrap();
    desktop.expect(&[Heard::Key(30, true), Heard::Key(30, false)]);
    emulation.scroll(0.0, 1.0).unwrap();
    desktop.expect(&[Heard::Scroll(0, -120)]);
    // A trackpad's fractions scroll smoothly.
    emulation.scroll(0.5, -0.25).unwrap();
    desktop.expect(&[Heard::ScrollSmooth(7.5, 3.75)]);
    emulation.move_by(5.0, -3.0).unwrap();
    desktop.expect(&[Heard::Motion(5.0, -3.0)]);

    // Dropped with a key held: it is let go of.
    emulation.key(usage::C, true).unwrap();
    desktop.expect(&[Heard::Key(46, true)]);
    drop(emulation);
    desktop.expect(&[Heard::Key(46, false)]);
    drop(status);
    drop(remote);
    wait_until("the session to be closed", || {
        desktop.asked.lock().unwrap().closed.len() == 1
    });

    // A new agent starts with what the user allowed, by itself, and keeps
    // the next token.
    let remote = desktop.remote(state.path());
    wait_until("the session to run again", || {
        remote.portal() == Portal::Running
    });
    let asked = desktop.asked.lock().unwrap();
    assert_eq!(asked.restore_tokens, [None, Some("token-1".to_owned())]);
    drop(asked);
    assert_eq!(
        Tokens::in_dir(Some(state.path()))
            .load("remote_desktop")
            .as_deref(),
        Some("token-2")
    );
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn refused_and_closed_sessions_wait_for_the_app() {
    let desktop = Desktop::start();
    let state = tempfile::tempdir().unwrap();
    desktop.asked.lock().unwrap().answers = [Answer::Refuse].into();
    let remote = desktop.remote(state.path());
    let status = WaylandStatus::new(Reaches::Portal(remote.clone()));
    let mut emulation = PortalEmulation::new(remote.clone());

    status
        .request_permission(Permission::RemoteControl)
        .unwrap();
    wait_until("the refusal", || remote.portal() == Portal::Denied);
    let refused = remote_control(&status);
    assert!(!refused.granted);
    assert_eq!(refused.ask, PermissionAsk::Denied);
    assert_eq!(
        Tokens::in_dir(Some(state.path())).load("remote_desktop"),
        None
    );
    wait_until("the refused session to be closed", || {
        desktop.asked.lock().unwrap().closed.len() == 1
    });
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(desktop.calls().len(), 3, "not asked again by itself");

    status
        .request_permission(Permission::RemoteControl)
        .unwrap();
    wait_until("the session to run", || remote.portal() == Portal::Running);
    desktop.expect(&[Heard::Bound]);
    std::thread::sleep(Duration::from_millis(200));
    emulation.key(usage::A, true).unwrap();
    desktop.expect(&[Heard::Key(30, true)]);

    // The user stops sharing.
    desktop.close_last_session();
    wait_until("the session to end", || remote.portal() == Portal::Stopped);
    let ended = remote_control(&status);
    assert!(!ended.granted);
    assert_eq!(ended.ask, PermissionAsk::Stopped);
    assert!(ended.describe().contains("the user stopped it"));
    assert!(matches!(
        emulation.key(usage::A, false),
        Err(crate::PlatformError::Unavailable(_))
    ));
    let calls = desktop.calls().len();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(desktop.calls().len(), calls, "not started again by itself");
    // Nor at the next start: the token is forgotten.
    assert_eq!(
        Tokens::in_dir(Some(state.path())).load("remote_desktop"),
        None
    );

    // Asked again, a new session runs; nothing is held in it.
    status
        .request_permission(Permission::RemoteControl)
        .unwrap();
    wait_until("the session to run again", || {
        remote.portal() == Portal::Running
    });
    desktop.expect(&[Heard::Bound]);
    std::thread::sleep(Duration::from_millis(200));
    emulation.key(usage::A, true).unwrap();
    desktop.expect(&[Heard::Key(30, true)]);
    assert!(
        status.request_permission(Permission::InputCapture).is_err(),
        "not yet"
    );
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn a_session_that_went_away_starts_again_with_its_token() {
    let desktop = Desktop::start();
    let state = tempfile::tempdir().unwrap();
    let remote = Arc::new(
        Remote::start(RemoteOptions::with(Lifecycle {
            bus: Some(desktop.bus.address.clone()),
            retries: vec![Duration::from_millis(300); 8],
            ..Lifecycle::new(Tokens::in_dir(Some(state.path())), super::APP_ID)
        }))
        .unwrap(),
    );
    let status = WaylandStatus::new(Reaches::Portal(remote.clone()));
    status
        .request_permission(Permission::RemoteControl)
        .unwrap();
    wait_until("the session to run", || remote.portal() == Portal::Running);

    // The compositor goes away without the portal closing the session:
    // it starts again by itself, with the token, and no dialog expected.
    desktop.end_libei();
    wait_until("the session to run again", || {
        desktop.asked.lock().unwrap().restore_tokens.len() == 2
            && remote.portal() == Portal::Running
    });
    assert_eq!(
        desktop.asked.lock().unwrap().restore_tokens,
        [None, Some("token-1".to_owned())]
    );

    // The session bus goes away and comes back at the same address, with
    // the portal: a new connection, and the session starts again.
    let dir = desktop.bus.dir.clone();
    drop(desktop);
    let desktop = Desktop::serve(Bus::start_at(dir));
    wait_until("the session to run on the new bus", || {
        remote.portal() == Portal::Running && !desktop.calls().is_empty()
    });
    assert_eq!(
        desktop.asked.lock().unwrap().restore_tokens,
        [Some("token-2".to_owned())]
    );
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn no_portal_fails_when_asked() {
    let bus = Bus::start();
    let state = tempfile::tempdir().unwrap();
    let remote = Arc::new(
        Remote::start(RemoteOptions::with(Lifecycle {
            bus: Some(bus.address.clone()),
            ..Lifecycle::new(Tokens::in_dir(Some(state.path())), super::APP_ID)
        }))
        .unwrap(),
    );
    let status = WaylandStatus::new(Reaches::Portal(remote.clone()));
    // Failing at once, twice: each request has its own round.
    assert_eq!(
        status
            .request_permission(Permission::RemoteControl)
            .unwrap(),
        1
    );
    wait_until("the failure", || {
        matches!(remote.portal(), Portal::Failed(_))
    });
    assert_eq!(remote_control(&status).ask, PermissionAsk::Failed);
    assert_eq!(
        status
            .request_permission(Permission::RemoteControl)
            .unwrap(),
        2
    );
    wait_until("the failure again", || {
        matches!(remote.portal(), Portal::Failed(_))
    });
    let failed = remote_control(&status);
    assert_eq!((failed.ask, failed.asked), (PermissionAsk::Failed, 2));
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn a_token_said_no_to_at_start_is_forgotten() {
    let desktop = Desktop::start();
    let state = tempfile::tempdir().unwrap();
    // A token the user revoked: the desktop asks, and the user says no.
    Tokens::in_dir(Some(state.path()))
        .save("remote_desktop", Some("revoked"))
        .unwrap();
    desktop.asked.lock().unwrap().answers = [Answer::Refuse].into();
    let remote = desktop.remote(state.path());
    wait_until("the refusal", || remote.portal() == Portal::Denied);
    assert_eq!(
        desktop.asked.lock().unwrap().restore_tokens,
        [Some("revoked".to_owned())]
    );
    assert_eq!(
        Tokens::in_dir(Some(state.path())).load("remote_desktop"),
        None
    );
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(desktop.calls().len(), 3, "not tried again");
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn a_start_with_a_token_waits_for_the_portal() {
    let bus = Bus::start();
    let state = tempfile::tempdir().unwrap();
    Tokens::in_dir(Some(state.path()))
        .save("remote_desktop", Some("kept"))
        .unwrap();
    // At login, before the portal is up.
    let remote = Arc::new(
        Remote::start(RemoteOptions::with(Lifecycle {
            bus: Some(bus.address.clone()),
            retries: vec![Duration::from_millis(300); 5],
            ..Lifecycle::new(Tokens::in_dir(Some(state.path())), super::APP_ID)
        }))
        .unwrap(),
    );
    wait_until("the first try to fail", || {
        matches!(remote.portal(), Portal::Failed(_))
    });
    let desktop = Desktop::serve(bus);
    wait_until("the session to run", || remote.portal() == Portal::Running);
    assert_eq!(
        desktop.asked.lock().unwrap().restore_tokens,
        [Some("kept".to_owned())]
    );
}

/// How many times the desktop was asked to start a session.
fn starts(desktop: &Desktop) -> usize {
    desktop.calls().iter().filter(|c| **c == "Start").count()
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn a_session_that_keeps_going_away_is_not_started_for_ever() {
    // Each session ends at once: the tries count down, and then stop.
    let desktop = Desktop::start();
    desktop.asked.lock().unwrap().eis_lifetime = Some(Duration::from_millis(150));
    let state = tempfile::tempdir().unwrap();
    Tokens::in_dir(Some(state.path()))
        .save("remote_desktop", Some("kept"))
        .unwrap();
    let remote = Remote::start(RemoteOptions::with(Lifecycle {
        bus: Some(desktop.bus.address.clone()),
        retries: vec![Duration::from_millis(100); 3],
        steady: Duration::from_secs(60),
        ..Lifecycle::new(Tokens::in_dir(Some(state.path())), super::APP_ID)
    }))
    .unwrap();
    wait_until("the tries to run out", || {
        starts(&desktop) == 4 && matches!(remote.portal(), Portal::Failed(_))
    });
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(starts(&desktop), 4, "not started again for ever");
    drop(remote);

    // Sessions that held for a while: the tries start over each time.
    let desktop = Desktop::start();
    desktop.asked.lock().unwrap().eis_lifetime = Some(Duration::from_millis(300));
    let state = tempfile::tempdir().unwrap();
    Tokens::in_dir(Some(state.path()))
        .save("remote_desktop", Some("kept"))
        .unwrap();
    let remote = Remote::start(RemoteOptions::with(Lifecycle {
        bus: Some(desktop.bus.address.clone()),
        retries: vec![Duration::from_millis(100); 3],
        steady: Duration::from_millis(100),
        ..Lifecycle::new(Tokens::in_dir(Some(state.path())), super::APP_ID)
    }))
    .unwrap();
    wait_until("more starts than the tries", || starts(&desktop) >= 6);
    drop(remote);
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn asking_while_a_start_with_the_token_waits_is_kept() {
    let desktop = Desktop::start();
    {
        let mut asked = desktop.asked.lock().unwrap();
        // The start with the token fails after the desktop took it; the
        // one asked for then runs.
        asked.answers = [Answer::Fail, Answer::Allow].into();
        asked.start_delay = Some(Duration::from_millis(500));
    }
    let state = tempfile::tempdir().unwrap();
    Tokens::in_dir(Some(state.path()))
        .save("remote_desktop", Some("kept"))
        .unwrap();
    let remote = desktop.remote(state.path());
    let status = WaylandStatus::new(Reaches::Portal(remote.clone()));
    wait_until("the start with the token", || {
        remote.portal() == Portal::Restoring && starts(&desktop) == 1
    });
    status
        .request_permission(Permission::RemoteControl)
        .unwrap();
    wait_until("the asked session to run", || {
        remote.portal() == Portal::Running
    });
    assert_eq!(
        desktop.asked.lock().unwrap().restore_tokens,
        [Some("kept".to_owned()), None]
    );
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn dropped_while_the_libei_handshake_hangs_it_ends() {
    let desktop = Desktop::start();
    desktop.asked.lock().unwrap().eis_silent = true;
    let state = tempfile::tempdir().unwrap();
    let remote = Remote::start(RemoteOptions::with(Lifecycle {
        bus: Some(desktop.bus.address.clone()),
        ..Lifecycle::new(Tokens::in_dir(Some(state.path())), super::APP_ID)
    }))
    .unwrap();
    remote.ask().unwrap();
    wait_until("the libei socket", || {
        desktop.calls().contains(&"ConnectToEIS")
    });
    let dropping = Instant::now();
    drop(remote);
    assert!(
        dropping.elapsed() < Duration::from_millis(800),
        "the stop was not heard during the handshake"
    );
    wait_until("the session to be closed", || {
        desktop.asked.lock().unwrap().closed.len() == 1
    });
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn a_start_that_takes_too_long_ends_and_its_token_goes() {
    // A start with the token that the desktop holds up, as with a dialog
    // nobody answers: it ends, the token goes, and it is not tried again.
    let desktop = Desktop::start();
    desktop.asked.lock().unwrap().start_delay = Some(Duration::from_secs(3));
    let state = tempfile::tempdir().unwrap();
    Tokens::in_dir(Some(state.path()))
        .save("remote_desktop", Some("kept"))
        .unwrap();
    let remote = Remote::start(RemoteOptions::with(Lifecycle {
        bus: Some(desktop.bus.address.clone()),
        retries: vec![Duration::from_millis(100); 3],
        restore_wait: Duration::from_millis(300),
        ..Lifecycle::new(Tokens::in_dir(Some(state.path())), super::APP_ID)
    }))
    .unwrap();
    // Restoring from the first, before the thread runs, so the app does not
    // ask meanwhile.
    assert_eq!(remote.portal(), Portal::Restoring);
    wait_until("the start to give up", || {
        matches!(remote.portal(), Portal::Failed(_))
    });
    assert_eq!(
        Tokens::in_dir(Some(state.path())).load("remote_desktop"),
        None
    );
    wait_until("the session to be closed", || {
        desktop.asked.lock().unwrap().closed.len() == 1
    });
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(starts(&desktop), 1, "not tried again");
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn a_session_closed_while_the_desktop_asks_ends() {
    let desktop = Desktop::start();
    desktop.asked.lock().unwrap().start_delay = Some(Duration::from_secs(5));
    let state = tempfile::tempdir().unwrap();
    let remote = Remote::start(RemoteOptions::with(Lifecycle {
        bus: Some(desktop.bus.address.clone()),
        ..Lifecycle::new(Tokens::in_dir(Some(state.path())), super::APP_ID)
    }))
    .unwrap();
    remote.ask().unwrap();
    wait_until("the start", || starts(&desktop) == 1);
    let closing = Instant::now();
    desktop.close_last_session();
    wait_until("the session to end", || remote.portal() == Portal::Stopped);
    assert!(
        closing.elapsed() < Duration::from_secs(3),
        "not heard while asking"
    );
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn dropped_while_the_portal_does_not_answer_it_ends() {
    let desktop = Desktop::start();
    desktop.asked.lock().unwrap().hang_create = true;
    let state = tempfile::tempdir().unwrap();
    let remote = Remote::start(RemoteOptions::with(Lifecycle {
        bus: Some(desktop.bus.address.clone()),
        ..Lifecycle::new(Tokens::in_dir(Some(state.path())), super::APP_ID)
    }))
    .unwrap();
    remote.ask().unwrap();
    wait_until("the session to be asked for", || {
        desktop.calls().contains(&"CreateSession")
    });
    let dropping = Instant::now();
    drop(remote);
    assert!(
        dropping.elapsed() < Duration::from_millis(800),
        "the stop was not heard while the portal did not answer"
    );
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn a_start_the_app_asked_for_that_fails_forgets_the_token() {
    let bus = Bus::start();
    let state = tempfile::tempdir().unwrap();
    Tokens::in_dir(Some(state.path()))
        .save("remote_desktop", Some("kept"))
        .unwrap();
    // No portal yet, and no tries again: the start with the token fails
    // before the desktop sees it, and the token stays.
    let remote = Remote::start(RemoteOptions::with(Lifecycle {
        bus: Some(bus.address.clone()),
        retries: Vec::new(),
        ..Lifecycle::new(Tokens::in_dir(Some(state.path())), super::APP_ID)
    }))
    .unwrap();
    wait_until("the first try to fail", || {
        matches!(remote.portal(), Portal::Failed(_))
    });
    assert_eq!(
        Tokens::in_dir(Some(state.path())).load("remote_desktop"),
        Some("kept".to_owned())
    );
    // The app asks, and the desktop fails the start after taking the
    // token: it goes.
    let desktop = Desktop::serve(bus);
    desktop.asked.lock().unwrap().answers = [Answer::Fail].into();
    remote.ask().unwrap();
    wait_until("the asked start to fail", || {
        starts(&desktop) == 1 && matches!(remote.portal(), Portal::Failed(_))
    });
    assert_eq!(
        desktop.asked.lock().unwrap().restore_tokens,
        [Some("kept".to_owned())]
    );
    assert_eq!(
        Tokens::in_dir(Some(state.path())).load("remote_desktop"),
        None
    );
}

/// Headless sway, with its outputs laid out by `swaymsg`.
struct Sway {
    child: Child,
    runtime: Arc<tempfile::TempDir>,
    display: String,
}

impl Sway {
    fn start() -> Self {
        Self::start_in(Arc::new(tempfile::tempdir().unwrap()))
    }

    /// Sway with its sockets in `runtime`, as again after a crash.
    fn start_in(runtime: Arc<tempfile::TempDir>) -> Self {
        let config = runtime.path().join("config");
        std::fs::write(&config, "").unwrap();
        let child = Command::new("sway")
            .arg("-c")
            .arg(&config)
            .env("XDG_RUNTIME_DIR", runtime.path())
            .env("WLR_BACKENDS", "headless")
            .env("WLR_RENDERER", "pixman")
            .env("WLR_LIBINPUT_NO_DEVICES", "1")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DISPLAY")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("sway");
        let mut sway = Self {
            child,
            runtime,
            display: String::new(),
        };
        wait_until("sway's socket", || sway.socket().is_some());
        sway.display = sway.socket().unwrap();
        sway
    }

    fn socket(&self) -> Option<String> {
        std::fs::read_dir(self.runtime.path())
            .ok()?
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .find(|n| n.starts_with("wayland-") && !n.ends_with(".lock"))
    }

    fn ipc(&self) -> PathBuf {
        std::fs::read_dir(self.runtime.path())
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .find(|p| p.to_string_lossy().contains("sway-ipc"))
            .expect("sway's IPC socket")
    }

    fn msg(&self, command: &str) {
        let status = Command::new("swaymsg")
            .arg("-s")
            .arg(self.ipc())
            .arg(command)
            .stdout(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "swaymsg {command}");
    }
}

impl Drop for Sway {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
#[ignore = "needs sway; scripts/linux-wayland-tests.sh"]
fn displays_are_the_logical_outputs() {
    let sway = Sway::start();
    sway.msg("create_output");
    sway.msg("output HEADLESS-1 resolution 1920x1080 position 0 0 scale 1");
    sway.msg("output HEADLESS-2 resolution 2560x1440 position 1920 0 scale 2");
    // SAFETY: the ignored tests run one at a time.
    unsafe {
        std::env::set_var("XDG_RUNTIME_DIR", sway.runtime.path());
        std::env::set_var("WAYLAND_DISPLAY", &sway.display);
    }
    let displays = super::WaylandDisplays::connect().unwrap();
    let mut seen = displays.displays().unwrap();
    seen.sort_by(|a, b| a.id.cmp(&b.id));
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert_eq!(seen[0].id, "HEADLESS-1");
    assert_eq!(seen[0].bounds, Rect::new(0.0, 0.0, 1920.0, 1080.0));
    assert_eq!(seen[0].scale, 1.0);
    assert!(seen[0].primary);
    assert_eq!(seen[1].id, "HEADLESS-2");
    assert_eq!(seen[1].bounds, Rect::new(1920.0, 0.0, 1280.0, 720.0));
    assert_eq!(seen[1].scale, 2.0);
    assert!(!seen[1].primary);

    // Changes are followed.
    sway.msg("output HEADLESS-2 disable");
    wait_until("the output to go", || {
        displays.displays().unwrap().len() == 1
    });
    sway.msg("output HEADLESS-2 enable");
    sway.msg("output HEADLESS-1 position 0 360");
    wait_until("the outputs to move", || {
        let seen = displays.displays().unwrap();
        seen.len() == 2
            && seen
                .iter()
                .any(|d| d.id == "HEADLESS-1" && d.bounds.y == 360.0)
    });

    // The compositor goes away: the outputs last seen stand, and once it
    // is back, under the same name, its outputs are followed again.
    let before = displays.displays().unwrap();
    let runtime = sway.runtime.clone();
    let display = sway.display.clone();
    drop(sway);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(displays.displays().unwrap(), before);
    let sway = Sway::start_in(runtime);
    assert_eq!(sway.display, display, "the same socket name");
    wait_until("the outputs of the new compositor", || {
        let seen = displays.displays().unwrap();
        seen.len() == 1 && seen[0].bounds.y == 0.0
    });
}

/// Capture through the InputCapture portal.
mod capture;

/// Input without portals, on headless sway.
mod wlroots;
