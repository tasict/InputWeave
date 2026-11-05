//! Capture through the InputCapture portal against stand-ins: a portal
//! of the test's own on a private D-Bus daemon, handing out a libei server
//! of the test's own that sends input as a compositor would to a receiver.

use std::collections::{HashMap, VecDeque};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iw_core::geometry::{Point, Rect};
use iw_core::input::{Button, InputEvent};
use iw_core::layout::{Edge, Exit};
use iw_keymap::usage;
use reis::PendingRequestResult;
use reis::eis;
use reis::request::{DeviceCapability, EisRequest, EisRequestConverter};
use zbus::object_server::SignalEmitter;
use zbus::{ObjectPath, OwnedObjectPath, OwnedVariant, Variant};

use super::super::capture::{Capture, CaptureOptions, PortalCapture, Waits};
use super::super::portal_session::Lifecycle;
use super::super::tokens::Tokens;
use super::super::translate::Portal as State;
use super::{
    Answer, Bus, FakeSession, option, readable, request_path, respond, respond_after, string,
    value, wait_until,
};
use crate::{InputCapture, Permission, PermissionAsk, PlatformError};

/// A barrier as the portal took it: its id and line.
type Line = (u32, (i32, i32, i32, i32));
/// A release: the activation, and where the cursor goes.
type Release = (Option<u32>, Option<(f64, f64)>);

/// What the fake portal was asked, and answers.
#[derive(Default)]
struct Asked {
    version: u32,
    calls: Vec<&'static str>,
    persist_modes: Vec<u32>,
    restore_tokens: Vec<Option<String>>,
    answers: VecDeque<Answer>,
    /// How long `Start` takes to answer; 20 ms when `None`.
    start_delay: Option<Duration>,
    tokens_given: u32,
    sessions: Vec<OwnedObjectPath>,
    zones: Vec<(u32, u32, i32, i32)>,
    zone_set: u32,
    /// Each set of barriers asked for: id and line.
    barriers: Vec<Vec<Line>>,
    /// The ids of barriers to refuse.
    refuse: Vec<u32>,
    enabled: bool,
    /// `Disable` sends `Disabled` before it answers, as the portal may.
    echo_disabled: bool,
    /// The unique name of the capture's connection.
    client: Option<String>,
    /// So many `Disable` calls fail next, and so many after them never
    /// answer in time; neither disables capture.
    fail_disable: u32,
    hang_disable: u32,
    /// So many `Enable` calls enable capture and then fail, and so many
    /// after them enable it but answer too late.
    fail_enable: u32,
    slow_enable: u32,
    /// Each release: the activation and where the cursor goes.
    releases: Vec<Release>,
    /// A session's `Close` never returns.
    hang_close: bool,
    /// The libei server of the last connection handed out.
    eis: Option<Sender<Emit>>,
}

/// What the libei server sends, as the compositor would.
#[derive(Clone, Copy, Debug)]
enum Emit {
    Start(u32),
    Motion(f32, f32),
    Button(u32, bool),
    Key(u32, bool),
    Wheel(i32, i32),
    Modifiers(u32),
}

struct FakeCapture {
    asked: Arc<Mutex<Asked>>,
    eis: Sender<UnixStream>,
}

impl FakeCapture {
    fn session_at(
        header: &zbus::message::Header<'_>,
        options: &HashMap<String, OwnedVariant>,
    ) -> OwnedObjectPath {
        let sender = header
            .sender()
            .unwrap()
            .trim_start_matches(':')
            .replace('.', "_");
        let token = string(options, "session_handle_token").unwrap();
        ObjectPath::try_from(format!(
            "/org/freedesktop/portal/desktop/session/{sender}/{token}"
        ))
        .unwrap()
        .into()
    }

    async fn serve_session(&self, server: &zbus::ObjectServer, session: &OwnedObjectPath) {
        let hang_close = self.asked.lock().unwrap().hang_close;
        let fake = FakeSession {
            asked: Arc::new(Mutex::new(super::Asked {
                hang_close,
                ..Default::default()
            })),
            path: session.clone(),
        };
        server.at(session, fake).await.unwrap();
        let mut asked = self.asked.lock().unwrap();
        asked.sessions.push(session.clone());
        asked.enabled = false;
    }

    /// What `Start` (or version 1's `CreateSession`) answers: the user's
    /// answer, with a token from version 2.
    fn answer(asked: &mut Asked) -> (u32, HashMap<String, OwnedVariant>) {
        match asked.answers.pop_front().unwrap_or(Answer::Allow) {
            Answer::Allow => {
                let mut results =
                    HashMap::from([("capabilities".to_owned(), value(Variant::from(3u32)))]);
                if asked.version >= 2 {
                    asked.tokens_given += 1;
                    results.insert(
                        "restore_token".to_owned(),
                        value(Variant::from(format!("token-{}", asked.tokens_given))),
                    );
                }
                (0, results)
            }
            Answer::Refuse => (1, HashMap::new()),
            Answer::Fail => (2, HashMap::new()),
        }
    }
}

fn object_path(path: &OwnedObjectPath) -> OwnedVariant {
    value(Variant::from(ObjectPath::try_from(path.as_str()).unwrap()))
}

#[zbus::interface(name = "org.freedesktop.portal.InputCapture")]
impl FakeCapture {
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        self.asked.lock().unwrap().version
    }

    #[zbus(property, name = "SupportedCapabilities")]
    fn supported_capabilities(&self) -> u32 {
        3
    }

    /// Version 1: the user is asked as the session is created.
    async fn create_session(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(object_server)] server: &zbus::ObjectServer,
        _parent_window: &str,
        options: HashMap<String, OwnedVariant>,
    ) -> OwnedObjectPath {
        let session = Self::session_at(&header, &options);
        self.serve_session(server, &session).await;
        let mut asked = self.asked.lock().unwrap();
        asked.calls.push("CreateSession");
        let (code, mut results) = Self::answer(&mut asked);
        if code == 0 {
            results.insert("session_handle".to_owned(), object_path(&session));
        }
        let request = request_path(&header, &options);
        respond(conn, request.clone(), code, results);
        request
    }

    #[zbus(name = "CreateSession2")]
    async fn create_session2(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(object_server)] server: &zbus::ObjectServer,
        options: HashMap<String, OwnedVariant>,
    ) -> HashMap<String, OwnedVariant> {
        let session = Self::session_at(&header, &options);
        self.serve_session(server, &session).await;
        let mut asked = self.asked.lock().unwrap();
        asked.calls.push("CreateSession2");
        asked.client = header.sender().map(|s| s.to_string());
        HashMap::from([("session_handle".to_owned(), object_path(&session))])
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
        let persist = option(&options, "persist_mode")
            .and_then(|v| v.downcast_ref::<u32>().ok())
            .unwrap_or(0);
        asked.persist_modes.push(persist);
        asked.restore_tokens.push(string(&options, "restore_token"));
        let delay = asked.start_delay.unwrap_or(Duration::from_millis(20));
        let (code, results) = Self::answer(&mut asked);
        let request = request_path(&header, &options);
        respond_after(conn, request.clone(), code, results, delay);
        request
    }

    async fn get_zones(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        _session: ObjectPath<'_>,
        options: HashMap<String, OwnedVariant>,
    ) -> OwnedObjectPath {
        let mut asked = self.asked.lock().unwrap();
        asked.calls.push("GetZones");
        let request = request_path(&header, &options);
        respond(
            conn,
            request.clone(),
            0,
            HashMap::from([
                (
                    "zones".to_owned(),
                    value(Variant::from(asked.zones.clone())),
                ),
                ("zone_set".to_owned(), value(Variant::from(asked.zone_set))),
            ]),
        );
        request
    }

    async fn set_pointer_barriers(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        _session: ObjectPath<'_>,
        options: HashMap<String, OwnedVariant>,
        barriers: Vec<HashMap<String, OwnedVariant>>,
        zone_set: u32,
    ) -> OwnedObjectPath {
        let mut asked = self.asked.lock().unwrap();
        asked.calls.push("SetPointerBarriers");
        assert_eq!(zone_set, asked.zone_set, "barriers on the zones just told");
        assert!(!asked.enabled, "barriers change while capture is disabled");
        let set: Vec<Line> = barriers
            .iter()
            .map(|b| {
                let id = b["barrier_id"].downcast_ref::<u32>().unwrap();
                let position: Variant<'_> = b["position"].try_clone().unwrap().into();
                (id, position.try_into().unwrap())
            })
            .collect();
        let failed: Vec<u32> = set
            .iter()
            .map(|(id, _)| *id)
            .filter(|id| asked.refuse.contains(id))
            .collect();
        asked.barriers.push(set);
        let request = request_path(&header, &options);
        respond(
            conn,
            request.clone(),
            0,
            HashMap::from([("failed_barriers".to_owned(), value(Variant::from(failed)))]),
        );
        request
    }

    async fn enable(
        &self,
        _session: ObjectPath<'_>,
        _options: HashMap<String, OwnedVariant>,
    ) -> zbus::fdo::Result<()> {
        let (fail, slow) = {
            let mut asked = self.asked.lock().unwrap();
            asked.calls.push("Enable");
            asked.enabled = true;
            let fail = asked.fail_enable > 0;
            let slow = !fail && asked.slow_enable > 0;
            if fail {
                asked.fail_enable -= 1;
            } else if slow {
                asked.slow_enable -= 1;
            }
            (fail, slow)
        };
        if fail {
            return Err(zbus::fdo::Error::Failed("cannot enable now".to_owned()));
        }
        if slow {
            tokio::time::sleep(Duration::from_millis(1500)).await;
        }
        Ok(())
    }

    async fn disable(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
        session: ObjectPath<'_>,
        _options: HashMap<String, OwnedVariant>,
    ) -> zbus::fdo::Result<()> {
        let (fail, hang) = {
            let mut asked = self.asked.lock().unwrap();
            asked.calls.push("Disable");
            let fail = asked.fail_disable > 0;
            let hang = !fail && asked.hang_disable > 0;
            if fail {
                asked.fail_disable -= 1;
            } else if hang {
                asked.hang_disable -= 1;
            }
            (fail, hang)
        };
        if fail {
            return Err(zbus::fdo::Error::Failed("cannot disable now".to_owned()));
        }
        if hang {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            return Ok(());
        }
        let echo = {
            let mut asked = self.asked.lock().unwrap();
            asked.enabled = false;
            asked.echo_disabled
        };
        if echo {
            let options: HashMap<&str, Variant<'_>> = HashMap::new();
            conn.emit_signal(
                None::<()>,
                "/org/freedesktop/portal/desktop",
                "org.freedesktop.portal.InputCapture",
                "Disabled",
                &(session, options),
            )
            .await
            .unwrap();
        }
        Ok(())
    }

    async fn release(&self, _session: ObjectPath<'_>, options: HashMap<String, OwnedVariant>) {
        let id = option(&options, "activation_id").and_then(|v| v.downcast_ref::<u32>().ok());
        let at = option(&options, "cursor_position").map(|v| {
            let v: Variant<'_> = v.try_clone().unwrap().into();
            v.try_into().unwrap()
        });
        let mut asked = self.asked.lock().unwrap();
        asked.calls.push("Release");
        asked.releases.push((id, at));
    }

    #[zbus(name = "ConnectToEIS")]
    async fn connect_to_eis(
        &self,
        _session: ObjectPath<'_>,
        _options: HashMap<String, OwnedVariant>,
    ) -> zbus::OwnedFd {
        let (ours, theirs) = UnixStream::pair().unwrap();
        self.asked.lock().unwrap().calls.push("ConnectToEIS");
        self.eis.send(ours).unwrap();
        std::os::fd::OwnedFd::from(theirs).into()
    }
}

/// The fake portal on its bus, and the libei server behind it.
struct Desktop {
    bus: Bus,
    asked: Arc<Mutex<Asked>>,
    /// The libei server has the devices bound.
    bound: Arc<Mutex<bool>>,
    conn: zbus::Connection,
    runtime: tokio::runtime::Runtime,
}

impl Desktop {
    /// A portal of `version`, with one zone of 1920 × 1080 and another to its
    /// right, lower.
    fn start(version: u32) -> Self {
        let bus = Bus::start();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let asked = Arc::new(Mutex::new(Asked {
            version,
            zones: vec![(1920, 1080, 0, 0), (1280, 1024, 1920, 200)],
            zone_set: 7,
            ..Default::default()
        }));
        let bound = Arc::new(Mutex::new(false));
        let (eis, connections) = std::sync::mpsc::channel::<UnixStream>();
        let server_asked = asked.clone();
        let server_bound = bound.clone();
        std::thread::spawn(move || {
            while let Ok(stream) = connections.recv() {
                let (emit, emits) = std::sync::mpsc::channel();
                server_asked.lock().unwrap().eis = Some(emit);
                *server_bound.lock().unwrap() = false;
                let bound = server_bound.clone();
                std::thread::spawn(move || serve_eis(stream, &bound, &emits));
            }
        });
        let portal = FakeCapture {
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
            bound,
            conn,
            runtime,
        }
    }

    fn capture(&self, tokens: &std::path::Path) -> Arc<Capture> {
        self.capture_waiting(tokens, Duration::from_millis(20))
    }

    /// Capture whose input handed over waits `modifiers` for the modifiers
    /// held.
    fn capture_waiting(&self, tokens: &std::path::Path, modifiers: Duration) -> Arc<Capture> {
        Arc::new(
            Capture::start(CaptureOptions {
                waits: Waits {
                    call: Duration::from_millis(700),
                    place_retry: Duration::from_millis(300),
                    rearm: Duration::from_millis(100),
                    rearm_max: Duration::from_millis(400),
                    warp: Duration::from_secs(3),
                    modifiers,
                },
                ..CaptureOptions::with(Lifecycle {
                    bus: Some(self.bus.address.clone()),
                    retries: vec![Duration::from_millis(100)],
                    ..Lifecycle::new(Tokens::in_dir(Some(tokens)), super::super::APP_ID)
                })
            })
            .unwrap(),
        )
    }

    fn calls(&self) -> Vec<&'static str> {
        self.asked.lock().unwrap().calls.clone()
    }

    fn forget_calls(&self) {
        self.asked.lock().unwrap().calls.clear();
    }

    fn barriers(&self) -> Vec<Vec<Line>> {
        self.asked.lock().unwrap().barriers.clone()
    }

    fn releases(&self) -> Vec<Release> {
        self.asked.lock().unwrap().releases.clone()
    }

    fn emit(&self, emits: &[Emit]) {
        let tx = self.asked.lock().unwrap().eis.clone().unwrap();
        for e in emits {
            tx.send(*e).unwrap();
        }
    }

    fn signal(&self, name: &str, options: HashMap<&str, Variant<'_>>) {
        let session = self.asked.lock().unwrap().sessions.last().unwrap().clone();
        self.runtime.block_on(async {
            self.conn
                .emit_signal(
                    None::<()>,
                    "/org/freedesktop/portal/desktop",
                    "org.freedesktop.portal.InputCapture",
                    name,
                    &(ObjectPath::try_from(session.as_str()).unwrap(), options),
                )
                .await
                .unwrap();
        });
    }

    /// The cursor pushes through barrier `barrier` at `at`: the portal says
    /// so, and libei starts the devices.
    fn activate(&self, id: u32, barrier: u32, at: (f64, f64)) {
        self.signal(
            "Activated",
            HashMap::from([
                ("activation_id", Variant::from(id)),
                ("barrier_id", Variant::from(barrier)),
                ("cursor_position", Variant::from(at)),
            ]),
        );
        self.emit(&[Emit::Start(id)]);
    }

    fn close_last_session(&self) {
        let path = self.asked.lock().unwrap().sessions.last().unwrap().clone();
        self.runtime.block_on(async {
            let emitter = SignalEmitter::new(&self.conn, path).unwrap();
            FakeSession::closed(&emitter, HashMap::new()).await.unwrap();
        });
    }
}

/// A libei server like a compositor's for a receiver: one seat with a
/// pointer and a keyboard, sending what the test says.
fn serve_eis(stream: UnixStream, bound: &Mutex<bool>, emits: &Receiver<Emit>) {
    stream.set_nonblocking(true).unwrap();
    let context = eis::Context::new(stream.try_clone().unwrap()).unwrap();
    let mut handshaker = reis::handshake::EisHandshaker::new(&context, 1);
    let mut converter: Option<EisRequestConverter> = None;
    let mut _seat = None;
    let mut devices: Option<(reis::request::Device, reis::request::Device)> = None;
    let mut time = 1u64;
    loop {
        readable(&stream);
        match context.read() {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
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
            if let EisRequest::Bind(bind) = request {
                let pointer = bind.seat.add_device(
                    Some("pointer"),
                    eis::device::DeviceType::Virtual,
                    DeviceCapability::Pointer | DeviceCapability::Button | DeviceCapability::Scroll,
                    |_| {},
                );
                let keyboard = bind.seat.add_device(
                    Some("keyboard"),
                    eis::device::DeviceType::Virtual,
                    DeviceCapability::Keyboard.into(),
                    |_| {},
                );
                pointer.resumed();
                keyboard.resumed();
                devices = Some((pointer, keyboard));
                *bound.lock().unwrap() = true;
            }
        }
        if let Some((pointer, keyboard)) = &devices {
            while let Ok(emit) = emits.try_recv() {
                time += 1000;
                match emit {
                    Emit::Start(sequence) => {
                        pointer.start_emulating(sequence);
                        keyboard.start_emulating(sequence);
                    }
                    Emit::Motion(dx, dy) => {
                        pointer
                            .interface::<eis::Pointer>()
                            .unwrap()
                            .motion_relative(dx, dy);
                        pointer.frame(time);
                    }
                    Emit::Button(code, pressed) => {
                        let state = if pressed {
                            eis::button::ButtonState::Press
                        } else {
                            eis::button::ButtonState::Released
                        };
                        pointer
                            .interface::<eis::Button>()
                            .unwrap()
                            .button(code, state);
                        pointer.frame(time);
                    }
                    Emit::Wheel(x, y) => {
                        pointer
                            .interface::<eis::Scroll>()
                            .unwrap()
                            .scroll_discrete(x, y);
                        // The same turn as smooth scrolling, which is not
                        // counted again.
                        pointer
                            .interface::<eis::Scroll>()
                            .unwrap()
                            .scroll(x as f32 / 8.0, y as f32 / 8.0);
                        pointer.frame(time);
                    }
                    Emit::Key(code, pressed) => {
                        let state = if pressed {
                            eis::keyboard::KeyState::Press
                        } else {
                            eis::keyboard::KeyState::Released
                        };
                        keyboard
                            .interface::<eis::Keyboard>()
                            .unwrap()
                            .key(code, state);
                        keyboard.frame(time);
                    }
                    Emit::Modifiers(mask) => {
                        let kb = keyboard.interface::<eis::Keyboard>().unwrap();
                        c.handle()
                            .with_next_serial(|serial| kb.modifiers(serial, mask, 0, 0, 0));
                    }
                }
            }
        }
        let _ = c.handle().flush();
    }
}

/// The capture under test, and what it reports.
struct Captured {
    capture: Arc<Capture>,
    backend: PortalCapture,
    events: Receiver<InputEvent>,
    /// The events taken, as the agent counts them.
    taken: std::cell::Cell<u64>,
}

impl Captured {
    fn new(capture: Arc<Capture>) -> (Self, Result<(), PlatformError>) {
        let mut backend = PortalCapture::new(capture.clone());
        let (tx, events) = std::sync::mpsc::channel();
        let started = backend.start(tx);
        let captured = Self {
            capture,
            backend,
            events,
            taken: Default::default(),
        };
        (captured, started)
    }

    /// Starts capture again, as the agent does once the permission is
    /// granted.
    fn restart(&mut self) -> Result<(), PlatformError> {
        let (tx, events) = std::sync::mpsc::channel();
        self.events = events;
        self.backend.start(tx)
    }

    fn next(&self) -> InputEvent {
        let event = self
            .events
            .recv_timeout(super::WAIT)
            .expect("an event from capture");
        self.taken.set(self.taken.get() + 1);
        event
    }

    /// Answers the last event taken, as the agent does once the router
    /// handled it while input was not grabbed.
    fn answer(&mut self, waiting: bool) {
        let taken = self.taken.get();
        self.backend.edge_wait(taken, waiting);
    }

    fn expect(&self, events: &[InputEvent]) {
        for e in events {
            assert_eq!(self.next(), *e);
        }
    }

    fn quiet(&self) {
        if let Ok(e) = self.events.recv_timeout(Duration::from_millis(300)) {
            panic!("unexpected event {e:?}");
        }
    }
}

/// The desktop's displays, as the fake portal's zones are at first.
const FIRST: Rect = Rect {
    x: 0.0,
    y: 0.0,
    width: 1920.0,
    height: 1080.0,
};
const SECOND: Rect = Rect {
    x: 1920.0,
    y: 200.0,
    width: 1280.0,
    height: 1024.0,
};

/// An exit of display `bounds`.
fn exit(bounds: Rect, edge: Edge, at: f64, start: f64, end: f64) -> Exit {
    Exit {
        display: "DP-1".to_owned(),
        bounds,
        edge,
        at,
        start,
        end,
    }
}

/// The right edge of the first display, where it is not beside the second.
fn right_exit() -> Exit {
    exit(FIRST, Edge::Right, 1920.0, 20.0, 200.0)
}

fn push(x: f64, y: f64, dx: f64, dy: f64) -> InputEvent {
    InputEvent::PointerMotion {
        position: Point::new(x, y),
        delta: Point::new(dx, dy),
    }
}

fn key(usage: iw_core::HidUsage, pressed: bool) -> InputEvent {
    InputEvent::Key { usage, pressed }
}

/// Asks, and waits for capture to run with its barriers enabled.
fn running(desktop: &Desktop, captured: &mut Captured) {
    captured.capture.ask().unwrap();
    wait_until("capture to run", || {
        captured.capture.state() == State::Running
    });
    wait_until("the devices to be bound", || *desktop.bound.lock().unwrap());
    wait_until("capture to be enabled", || {
        desktop.asked.lock().unwrap().enabled
    });
    // Input goes to the agent once it started capture, as the agent does
    // once the session runs.
    assert!(!captured.backend.capturing());
    assert!(captured.restart().is_ok());
    assert!(captured.backend.capturing());
    // The devices resumed reach the agent a moment after the bind.
    std::thread::sleep(Duration::from_millis(200));
}

const ENDED: InputEvent = InputEvent::CaptureEnded;

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn input_capture_is_asked_for_and_puts_its_barriers_where_the_cursor_leaves() {
    let desktop = Desktop::start(2);
    let state = tempfile::tempdir().unwrap();
    let capture = desktop.capture(state.path());
    let (mut captured, started) = Captured::new(capture.clone());
    assert!(matches!(
        started,
        Err(PlatformError::PermissionDenied(Permission::InputCapture))
    ));
    assert!(captured.backend.wants_exits());
    captured.backend.set_exits(&[right_exit()]);

    // Nothing remembered: the agent does not ask by itself.
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(desktop.calls(), Vec::<&str>::new());
    let before = capture.permission();
    assert_eq!(
        (before.granted, before.ask),
        (false, PermissionAsk::Askable)
    );
    assert!(!captured.backend.grab_ready());

    running(&desktop, &mut captured);
    let granted = capture.permission();
    assert!(granted.granted);
    assert_eq!(granted.asked, 1);
    assert_eq!(
        desktop.calls(),
        [
            "CreateSession2",
            "Start",
            "ConnectToEIS",
            "GetZones",
            "SetPointerBarriers",
            "Enable"
        ]
    );
    {
        let asked = desktop.asked.lock().unwrap();
        assert_eq!(asked.persist_modes, [2], "until the user revokes it");
        assert_eq!(asked.restore_tokens, [None]);
    }
    assert_eq!(
        Tokens::in_dir(Some(state.path()))
            .load("input_capture")
            .as_deref(),
        Some("token-1")
    );
    assert_eq!(desktop.barriers(), [vec![(1, (1920, 20, 1920, 199))]]);

    // The exits change: the barriers are put again, with capture disabled
    // meanwhile, under ids not given before.
    desktop.forget_calls();
    captured.backend.set_exits(&[
        right_exit(),
        exit(FIRST, Edge::Top, 0.0, 20.0, 1900.0),
        // The second zone's right edge, out of the first's zones.
        exit(SECOND, Edge::Right, 3200.0, 220.0, 1204.0),
    ]);
    wait_until("the new barriers", || desktop.barriers().len() == 2);
    wait_until("capture enabled again", || {
        desktop.asked.lock().unwrap().enabled
    });
    assert_eq!(
        desktop.calls(),
        ["GetZones", "Disable", "SetPointerBarriers", "Enable"]
    );
    assert_eq!(
        desktop.barriers()[1],
        [
            (2, (1920, 20, 1920, 199)),
            (3, (20, 0, 1899, 0)),
            (4, (3200, 220, 3200, 1203)),
        ]
    );

    // The zones change: the barriers follow; one the desktop refuses is
    // left out. An older zone set's news is none.
    desktop.forget_calls();
    {
        let mut asked = desktop.asked.lock().unwrap();
        asked.zones = vec![(1920, 1080, 0, 0)];
        asked.zone_set = 9;
        asked.refuse = vec![6];
    }
    desktop.signal(
        "ZonesChanged",
        HashMap::from([("zone_set", Variant::from(7u32))]),
    );
    wait_until("barriers on the new zones", || {
        desktop.barriers().len() == 3
    });
    assert_eq!(
        desktop.barriers()[2],
        [(5, (1920, 20, 1920, 199)), (6, (20, 0, 1899, 0))],
        "the second zone is gone, and its exit with it"
    );
    desktop.signal(
        "ZonesChanged",
        HashMap::from([("zone_set", Variant::from(8u32))]),
    );
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(desktop.barriers().len(), 3, "older than the zones in use");

    // The refused barrier is none of those put: input the portal says came
    // through it is let go of, not reported. One it cannot tell goes to the
    // nearest.
    desktop.activate(1, 6, (1921.0, 100.0));
    wait_until("the release", || desktop.releases().len() == 1);
    assert_eq!(desktop.releases()[0], (Some(1), None));
    captured.quiet();
    desktop.activate(2, 0, (1921.0, 100.0));
    captured.expect(&[push(1919.0, 100.0, 1.0, 0.0)]);
    captured.answer(false);
    wait_until("the release", || desktop.releases().len() == 2);
    captured.expect(&[ENDED]);

    // Nowhere to leave: no barriers, capture stays disabled.
    desktop.forget_calls();
    captured.backend.set_exits(&[]);
    wait_until("no barriers", || desktop.barriers().len() == 4);
    assert_eq!(desktop.barriers()[3], []);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        desktop.calls(),
        ["GetZones", "Disable", "SetPointerBarriers"]
    );
    assert!(!desktop.asked.lock().unwrap().enabled);

    // A new agent starts with what the user allowed, by itself.
    drop(captured);
    drop(capture);
    let capture = desktop.capture(state.path());
    wait_until("capture to run again", || capture.state() == State::Running);
    assert_eq!(
        desktop.asked.lock().unwrap().restore_tokens,
        [None, Some("token-1".to_owned())]
    );
    assert_eq!(
        Tokens::in_dir(Some(state.path()))
            .load("input_capture")
            .as_deref(),
        Some("token-2")
    );
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn input_is_handed_over_at_a_barrier_and_given_back_where_the_cursor_returns() {
    let desktop = Desktop::start(2);
    let state = tempfile::tempdir().unwrap();
    let (mut captured, _) = Captured::new(desktop.capture(state.path()));
    captured.backend.set_exits(&[right_exit()]);
    running(&desktop, &mut captured);

    // Input before any activation is not reported.
    desktop.emit(&[Emit::Motion(3.0, 0.0)]);
    captured.quiet();

    desktop.activate(5, 1, (1920.4, 150.0));
    captured.expect(&[push(1919.0, 150.0, 1.0, 0.0)]);
    assert!(captured.backend.grab_ready(), "input is handed over");
    // The agent crosses. Input goes as it comes, movements from the middle
    // of the zone, away from any exit.
    captured.backend.set_grabbed(true).unwrap();
    desktop.emit(&[
        Emit::Motion(-10.0, 3.0),
        Emit::Key(30, true),
        Emit::Button(0x110, true),
        Emit::Wheel(0, -120),
        Emit::Button(0x113, true),
        Emit::Button(0x113, false),
    ]);
    captured.expect(&[
        push(960.0, 540.0, -10.0, 3.0),
        key(usage::A, true),
        InputEvent::Button {
            button: Button::Left,
            pressed: true,
        },
        InputEvent::Scroll { dx: 0.0, dy: 1.0 },
        InputEvent::Button {
            button: Button::Back,
            pressed: true,
        },
        InputEvent::Button {
            button: Button::Back,
            pressed: false,
        },
    ]);

    // The cursor comes back: let go of where the agent puts it, and what is
    // down is reported up, as the local apps have it now. Nothing more: the
    // router put the cursor here itself.
    captured.backend.set_grabbed(false).unwrap();
    captured.backend.warp(Point::new(1919.0, 120.0)).unwrap();
    wait_until("the release", || !desktop.releases().is_empty());
    assert_eq!(desktop.releases(), [(Some(5), Some((1919.0, 120.0)))]);
    captured.expect(&[
        key(usage::A, false),
        InputEvent::Button {
            button: Button::Left,
            pressed: false,
        },
    ]);
    assert!(!captured.backend.grab_ready());
    // What libei sends after is not reported.
    desktop.emit(&[Emit::Motion(1.0, 0.0), Emit::Key(30, false)]);
    captured.quiet();

    // The portal's id may come after libei's input: the input begins it.
    desktop.signal(
        "Activated",
        HashMap::from([
            ("activation_id", Variant::from(6u32)),
            ("barrier_id", Variant::from(1u32)),
            ("cursor_position", Variant::from((1921.0, 60.0))),
        ]),
    );
    std::thread::sleep(Duration::from_millis(100));
    desktop.emit(&[Emit::Motion(2.0, 0.0)]);
    captured.expect(&[push(1919.0, 60.0, 1.0, 0.0), push(1919.0, 60.0, 2.0, 0.0)]);
    captured.backend.set_grabbed(true).unwrap();
    desktop.emit(&[Emit::Key(48, true)]);
    let b = iw_core::HidUsage::from_evdev(48).unwrap();
    captured.expect(&[key(b, true)]);
    // The desktop ends it while the cursor is on the other device: what is
    // down goes up, and control comes back.
    desktop.signal(
        "Deactivated",
        HashMap::from([("activation_id", Variant::from(6u32))]),
    );
    captured.expect(&[key(b, false), ENDED]);
    assert_eq!(desktop.releases().len(), 1, "not released by the agent");

    // Grabbing with nothing handed over gives control back at once.
    captured.backend.set_grabbed(true).unwrap();
    captured.expect(&[ENDED]);

    // An activation the desktop ended before libei started it is never
    // reported.
    desktop.signal(
        "Activated",
        HashMap::from([
            ("activation_id", Variant::from(7u32)),
            ("barrier_id", Variant::from(1u32)),
        ]),
    );
    desktop.signal(
        "Deactivated",
        HashMap::from([("activation_id", Variant::from(7u32))]),
    );
    // Heard before libei's start comes, as the test cannot tell when.
    std::thread::sleep(Duration::from_millis(100));
    desktop.emit(&[Emit::Start(7), Emit::Motion(1.0, 0.0)]);
    captured.quiet();
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn a_modifier_held_as_input_is_handed_over_crosses_and_old_answers_are_not_about_it() {
    let desktop = Desktop::start(2);
    let state = tempfile::tempdir().unwrap();
    let (mut captured, _) = Captured::new(desktop.capture(state.path()));
    captured.backend.set_exits(&[right_exit()]);
    running(&desktop, &mut captured);

    // Control held before the cursor reached the edge: reported first, and
    // the agent answers it before the router saw the push, which it does
    // not wait at, as no push is under way.
    desktop.emit(&[Emit::Modifiers(1 << 2)]);
    desktop.activate(1, 1, (1920.4, 150.0));
    captured.expect(&[key(usage::LEFT_CONTROL, true)]);
    captured.answer(false);
    captured.expect(&[push(1919.0, 150.0, 1.0, 0.0)]);
    let first_push = captured.taken.get();
    // With Control held, the router crosses at the push.
    captured.backend.set_grabbed(true).unwrap();
    desktop.emit(&[Emit::Motion(5.0, 0.0)]);
    captured.expect(&[push(960.0, 540.0, 5.0, 0.0)]);
    assert!(desktop.releases().is_empty(), "held on the other device");
    // Control let go of: the mask says so.
    desktop.emit(&[Emit::Modifiers(0)]);
    captured.expect(&[key(usage::LEFT_CONTROL, false)]);
    captured.backend.set_grabbed(false).unwrap();
    captured.backend.warp(Point::new(1919.0, 150.0)).unwrap();
    wait_until("the release", || desktop.releases().len() == 1);

    // The next activation; an answer about the first comes late: it is not
    // about this one.
    desktop.activate(2, 1, (1920.0, 160.0));
    captured.expect(&[push(1919.0, 160.0, 1.0, 0.0)]);
    captured.backend.edge_wait(first_push, false);
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(desktop.releases().len(), 1, "still held");
    captured.answer(false);
    wait_until("the release", || desktop.releases().len() == 2);
    captured.expect(&[ENDED]);
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn input_held_at_an_edge_is_let_go_of_unless_the_cursor_waits_to_cross() {
    let desktop = Desktop::start(2);
    let state = tempfile::tempdir().unwrap();
    let (mut captured, _) = Captured::new(desktop.capture(state.path()));
    captured.backend.set_exits(&[right_exit()]);
    running(&desktop, &mut captured);

    // The agent says the cursor does not cross (a modifier to hold, say):
    // let go of, and the router hears so.
    desktop.activate(1, 1, (1920.0, 100.0));
    captured.expect(&[push(1919.0, 100.0, 1.0, 0.0)]);
    captured.answer(false);
    wait_until("the release", || desktop.releases().len() == 1);
    assert_eq!(desktop.releases()[0], (Some(1), Some((1919.0, 100.0))));
    captured.expect(&[ENDED]);

    // It waits to cross: held while the cursor pushes; moving back lets go
    // there.
    desktop.activate(2, 1, (1920.0, 100.0));
    captured.expect(&[push(1919.0, 100.0, 1.0, 0.0)]);
    captured.answer(true);
    desktop.emit(&[Emit::Motion(4.0, 2.0)]);
    captured.expect(&[push(1919.0, 100.0, 4.0, 2.0)]);
    captured.answer(true);
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(desktop.releases().len(), 1, "still held");
    desktop.emit(&[Emit::Motion(-6.0, 0.0)]);
    captured.expect(&[push(1919.0, 102.0, -6.0, 0.0)]);
    wait_until("the release", || desktop.releases().len() == 2);
    assert_eq!(desktop.releases()[1], (Some(2), Some((1913.0, 102.0))));
    captured.expect(&[ENDED]);

    // The agent says nothing: let go of soon.
    desktop.activate(3, 1, (1920.0, 50.0));
    captured.expect(&[push(1919.0, 50.0, 1.0, 0.0)]);
    wait_until("the release", || desktop.releases().len() == 3);
    captured.expect(&[ENDED]);

    // The cursor rests at the edge while it waits to cross: let go of, and
    // the router hears so, as it cannot see the cursor go away next.
    desktop.activate(4, 1, (1920.0, 50.0));
    captured.expect(&[push(1919.0, 50.0, 1.0, 0.0)]);
    captured.answer(true);
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(desktop.releases().len(), 3, "still held");
    wait_until("the release", || desktop.releases().len() == 4);
    captured.expect(&[ENDED]);

    // A click held at the edge lets go, and is reported up and down.
    desktop.activate(5, 1, (1920.0, 50.0));
    captured.expect(&[push(1919.0, 50.0, 1.0, 0.0)]);
    captured.answer(true);
    desktop.emit(&[Emit::Button(0x111, true)]);
    let click = |pressed| InputEvent::Button {
        button: Button::Right,
        pressed,
    };
    captured.expect(&[click(true), click(false), ENDED]);
    wait_until("the release", || desktop.releases().len() == 5);
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn barriers_put_again_tell_an_old_activation_and_their_own_disabling_apart() {
    let desktop = Desktop::start(2);
    desktop.asked.lock().unwrap().echo_disabled = true;
    let state = tempfile::tempdir().unwrap();
    let (mut captured, _) = Captured::new(desktop.capture(state.path()));
    captured.backend.set_exits(&[right_exit()]);
    running(&desktop, &mut captured);

    // Put again, under new ids; the portal says it disabled capture, as it
    // was disabled here for that, before it answers that capture is enabled
    // again: no news.
    captured
        .backend
        .set_exits(&[right_exit(), exit(FIRST, Edge::Top, 0.0, 20.0, 1900.0)]);
    wait_until("the new barriers", || desktop.barriers().len() == 2);
    assert_eq!(desktop.barriers()[1][0].0, 2);
    wait_until("capture enabled again", || {
        desktop.asked.lock().unwrap().enabled
    });
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(desktop.barriers().len(), 2, "not put again for it");
    assert!(desktop.asked.lock().unwrap().enabled);

    // An activation at a barrier from before is one the desktop ended as it
    // disabled capture: let go of, not reported.
    desktop.activate(3, 1, (1921.0, 100.0));
    wait_until("the release", || desktop.releases().len() == 1);
    assert_eq!(desktop.releases()[0], (Some(3), None));
    captured.quiet();
    // One at the new barriers is.
    desktop.activate(4, 2, (1921.0, 100.0));
    captured.expect(&[push(1919.0, 100.0, 1.0, 0.0)]);
    captured.answer(false);
    wait_until("the release", || desktop.releases().len() == 2);
    captured.expect(&[ENDED]);

    // The desktop disabling capture by itself is news, however soon after
    // capture was disabled and enabled again here (a portal that sends
    // nothing for the app's own `Disable`): enabled again.
    desktop.asked.lock().unwrap().echo_disabled = false;
    desktop.forget_calls();
    captured.backend.set_exits(&[right_exit()]);
    wait_until("the new barriers", || desktop.barriers().len() == 3);
    wait_until("capture enabled again", || {
        desktop.calls().contains(&"Enable")
    });
    {
        let mut asked = desktop.asked.lock().unwrap();
        asked.enabled = false;
        asked.calls.clear();
    }
    desktop.signal("Disabled", HashMap::new());
    wait_until("barriers put again", || desktop.barriers().len() == 4);
    wait_until("capture enabled again", || {
        desktop.calls().contains(&"Enable")
    });
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn signals_only_the_portal_sends_about_this_session_are_heard() {
    let desktop = Desktop::start(2);
    let state = tempfile::tempdir().unwrap();
    let (mut captured, _) = Captured::new(desktop.capture(state.path()));
    captured.backend.set_exits(&[right_exit()]);
    running(&desktop, &mut captured);
    let (client, session) = {
        let asked = desktop.asked.lock().unwrap();
        (
            asked.client.clone().unwrap(),
            asked.sessions.last().unwrap().clone(),
        )
    };
    let activated = |id: u32| {
        HashMap::from([
            ("activation_id", Variant::from(id)),
            ("barrier_id", Variant::from(1u32)),
            ("cursor_position", Variant::from((1921.0, 100.0))),
        ])
    };

    // Another client on the bus says input was handed over, to all and to
    // the capture alone: not heard.
    desktop.runtime.block_on(async {
        let spoof = zbus::connection::Builder::address(desktop.bus.address.as_str())
            .build()
            .await
            .unwrap();
        for to in [None, Some(client.as_str())] {
            spoof
                .emit_signal(
                    to,
                    "/org/freedesktop/portal/desktop",
                    "org.freedesktop.portal.InputCapture",
                    "Activated",
                    &(
                        ObjectPath::try_from(session.as_str()).unwrap(),
                        activated(1),
                    ),
                )
                .await
                .unwrap();
        }
    });
    // The portal, of another session: not heard either.
    desktop.runtime.block_on(async {
        desktop
            .conn
            .emit_signal(
                None::<()>,
                "/org/freedesktop/portal/desktop",
                "org.freedesktop.portal.InputCapture",
                "Activated",
                &(
                    ObjectPath::try_from("/org/freedesktop/portal/desktop/session/x/y").unwrap(),
                    activated(1),
                ),
            )
            .await
            .unwrap();
    });
    std::thread::sleep(Duration::from_millis(100));
    desktop.emit(&[Emit::Start(1), Emit::Motion(1.0, 0.0)]);
    captured.quiet();
    assert!(desktop.releases().is_empty());

    // The portal, of this session: heard.
    desktop.activate(2, 1, (1921.0, 100.0));
    captured.expect(&[push(1919.0, 100.0, 1.0, 0.0)]);
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn modifiers_told_just_after_libei_starts_the_devices_count() {
    let desktop = Desktop::start(2);
    let state = tempfile::tempdir().unwrap();
    let (mut captured, _) = Captured::new(desktop.capture(state.path()));
    captured.backend.set_exits(&[right_exit()]);
    running(&desktop, &mut captured);

    // The desktop starts the devices, then tells that Control is held, as
    // it may on resuming them: the push waits for it.
    desktop.signal(
        "Activated",
        HashMap::from([
            ("activation_id", Variant::from(1u32)),
            ("barrier_id", Variant::from(1u32)),
            ("cursor_position", Variant::from((1921.0, 100.0))),
        ]),
    );
    std::thread::sleep(Duration::from_millis(100));
    desktop.emit(&[Emit::Start(1), Emit::Modifiers(1 << 2)]);
    captured.expect(&[
        key(usage::LEFT_CONTROL, true),
        push(1919.0, 100.0, 1.0, 0.0),
    ]);
    captured.answer(false);
    captured.expect(&[key(usage::LEFT_CONTROL, false), ENDED]);

    // Nothing told: it begins a moment after, all the same.
    desktop.emit(&[Emit::Modifiers(0)]);
    desktop.activate(2, 1, (1921.0, 100.0));
    captured.expect(&[push(1919.0, 100.0, 1.0, 0.0)]);
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn a_capture_session_the_desktop_closes_gives_control_back_and_waits_for_the_app() {
    let desktop = Desktop::start(2);
    let state = tempfile::tempdir().unwrap();
    let capture = desktop.capture(state.path());
    let (mut captured, _) = Captured::new(capture.clone());
    captured.backend.set_exits(&[right_exit()]);
    running(&desktop, &mut captured);
    desktop.activate(1, 1, (1920.0, 100.0));
    captured.expect(&[push(1919.0, 100.0, 1.0, 0.0)]);
    captured.backend.set_grabbed(true).unwrap();
    desktop.emit(&[Emit::Key(30, true)]);
    captured.expect(&[key(usage::A, true)]);

    desktop.close_last_session();
    captured.expect(&[key(usage::A, false), ENDED]);
    wait_until("the stop", || capture.state() == State::Stopped);
    let stopped = capture.permission();
    assert_eq!(
        (stopped.granted, stopped.ask),
        (false, PermissionAsk::Stopped)
    );
    assert_eq!(
        Tokens::in_dir(Some(state.path())).load("input_capture"),
        None,
        "not started again by itself"
    );
    assert!(!captured.backend.grab_ready());
    assert!(!captured.backend.capturing(), "the agent hears it stopped");

    // The app asks again: a new session, its barriers put.
    desktop.forget_calls();
    running(&desktop, &mut captured);
    assert_eq!(capture.permission().asked, 2);
    assert!(desktop.calls().contains(&"Enable"));
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn a_first_version_portal_asks_at_every_start() {
    let desktop = Desktop::start(1);
    let state = tempfile::tempdir().unwrap();
    desktop.asked.lock().unwrap().answers = [Answer::Refuse].into();
    let capture = desktop.capture(state.path());
    let (mut captured, _) = Captured::new(capture.clone());
    captured.backend.set_exits(&[right_exit()]);

    capture.ask().unwrap();
    wait_until("the refusal", || capture.state() == State::Denied);
    assert_eq!(capture.permission().ask, PermissionAsk::Denied);

    running(&desktop, &mut captured);
    assert_eq!(
        desktop.calls(),
        [
            "CreateSession",
            "CreateSession",
            "ConnectToEIS",
            "GetZones",
            "SetPointerBarriers",
            "Enable"
        ]
    );
    assert_eq!(
        Tokens::in_dir(Some(state.path())).load("input_capture"),
        None
    );

    // A token kept from a newer portal is not tried on this one, which
    // would ask the user unattended: it is forgotten.
    drop(captured);
    drop(capture);
    Tokens::in_dir(Some(state.path()))
        .save("input_capture", Some("token-9"))
        .unwrap();
    desktop.forget_calls();
    let capture = desktop.capture(state.path());
    wait_until("the failure", || {
        matches!(capture.state(), State::Failed(_))
    });
    assert!(!desktop.calls().contains(&"CreateSession"));
    assert_eq!(
        Tokens::in_dir(Some(state.path())).load("input_capture"),
        None
    );
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn barriers_are_put_again_after_a_disable_that_failed_or_never_answered() {
    let desktop = Desktop::start(2);
    let state = tempfile::tempdir().unwrap();
    let (mut captured, _) = Captured::new(desktop.capture(state.path()));
    captured.backend.set_exits(&[right_exit()]);
    running(&desktop, &mut captured);

    // Disabling fails: tried again, still disabled first, as the portal
    // takes new barriers only then (the fake asserts it does).
    desktop.forget_calls();
    desktop.asked.lock().unwrap().fail_disable = 1;
    captured
        .backend
        .set_exits(&[right_exit(), exit(FIRST, Edge::Top, 0.0, 20.0, 1900.0)]);
    wait_until("the new barriers", || desktop.barriers().len() == 2);
    wait_until("capture enabled again", || {
        desktop.asked.lock().unwrap().enabled
    });
    assert_eq!(
        desktop.calls(),
        [
            "GetZones",
            "Disable",
            "GetZones",
            "Disable",
            "SetPointerBarriers",
            "Enable"
        ]
    );

    // Disabling never answers in time: the same.
    desktop.forget_calls();
    desktop.asked.lock().unwrap().hang_disable = 1;
    captured.backend.set_exits(&[right_exit()]);
    wait_until("the new barriers", || desktop.barriers().len() == 3);
    wait_until("capture enabled again", || {
        desktop.asked.lock().unwrap().enabled
    });
    let calls = desktop.calls();
    assert_eq!(
        calls.iter().filter(|c| **c == "Disable").count(),
        2,
        "{calls:?}"
    );
    assert_eq!(&calls[calls.len() - 2..], ["SetPointerBarriers", "Enable"]);

    // While the cursor leaves nowhere (as while controlled), what barriers
    // still up hand over is let go of, not reported: here they stay up, as
    // disabling keeps failing.
    desktop.asked.lock().unwrap().fail_disable = 1000;
    desktop.forget_calls();
    captured.backend.set_exits(&[]);
    wait_until("disabling tried", || desktop.calls().contains(&"Disable"));
    let barrier = desktop.barriers()[2][0].0;
    desktop.activate(1, barrier, (1921.0, 100.0));
    wait_until("the release", || !desktop.releases().is_empty());
    assert_eq!(desktop.releases()[0], (Some(1), None));
    captured.quiet();
    assert_eq!(desktop.barriers().len(), 3, "still up");
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn an_enable_that_failed_or_answered_late_is_followed_by_a_disable() {
    let desktop = Desktop::start(2);
    let state = tempfile::tempdir().unwrap();
    let (mut captured, _) = Captured::new(desktop.capture(state.path()));
    captured.backend.set_exits(&[right_exit()]);
    running(&desktop, &mut captured);

    // Enabling fails, though the portal enabled capture: tried again,
    // disabled first, as capture may be enabled (the fake asserts it is
    // not as barriers come).
    for (fail, slow, more) in [(1, 0, true), (0, 1, false)] {
        desktop.forget_calls();
        {
            let mut asked = desktop.asked.lock().unwrap();
            asked.fail_enable = fail;
            asked.slow_enable = slow;
        }
        let before = desktop.barriers().len();
        let exits = if more {
            vec![right_exit(), exit(FIRST, Edge::Top, 0.0, 20.0, 1900.0)]
        } else {
            vec![right_exit()]
        };
        captured.backend.set_exits(&exits);
        wait_until("barriers put twice", || {
            desktop.barriers().len() == before + 2
        });
        wait_until("enabled at last", || {
            desktop.calls().iter().filter(|c| **c == "Enable").count() == 2
        });
        let calls = desktop.calls();
        let second = calls
            .iter()
            .rposition(|c| *c == "SetPointerBarriers")
            .unwrap();
        assert_eq!(calls[second - 1], "Disable", "{calls:?}");
    }
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn capture_the_desktop_keeps_disabling_is_enabled_again_all_the_same() {
    let desktop = Desktop::start(2);
    let state = tempfile::tempdir().unwrap();
    let (mut captured, _) = Captured::new(desktop.capture(state.path()));
    captured.backend.set_exits(&[right_exit()]);
    running(&desktop, &mut captured);

    // Disabled again and again, as through a long screen lock: enabled
    // again each time, past the warning, waiting longer each time.
    for time in 1..=6 {
        wait_until("capture enabled", || desktop.asked.lock().unwrap().enabled);
        let before = desktop.barriers().len();
        desktop.asked.lock().unwrap().enabled = false;
        desktop.signal("Disabled", HashMap::new());
        wait_until(&format!("capture enabled again, time {time}"), || {
            desktop.barriers().len() == before + 1 && desktop.asked.lock().unwrap().enabled
        });
    }
    // And input handed over still comes.
    let barrier = desktop.barriers().last().unwrap()[0].0;
    desktop.activate(1, barrier, (1921.0, 100.0));
    captured.expect(&[push(1919.0, 100.0, 1.0, 0.0)]);
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn a_hotkey_pressed_at_the_edge_keeps_input_held() {
    let desktop = Desktop::start(2);
    let state = tempfile::tempdir().unwrap();
    let (mut captured, _) = Captured::new(desktop.capture(state.path()));
    captured.backend.set_exits(&[right_exit()]);
    let jump: iw_core::router::Hotkey = "control+alt+shift+Digit2".parse().unwrap();
    captured.backend.watch_hotkeys(&[jump]);
    running(&desktop, &mut captured);
    let digit2 = iw_core::HidUsage::from_name("Digit2").unwrap();

    // Held at the edge with Control, Alt and Shift down: the key that makes
    // the jump hotkey with them is reported, and input stays held, for the
    // router to act on it.
    desktop.emit(&[Emit::Modifiers(1 | 1 << 2 | 1 << 3)]);
    desktop.activate(1, 1, (1920.0, 100.0));
    captured.expect(&[
        key(usage::LEFT_SHIFT, true),
        key(usage::LEFT_CONTROL, true),
        key(usage::LEFT_ALT, true),
    ]);
    captured.answer(false);
    captured.expect(&[push(1919.0, 100.0, 1.0, 0.0)]);
    captured.answer(true);
    desktop.emit(&[Emit::Key(3, true)]);
    captured.expect(&[key(digit2, true)]);
    captured.answer(true);
    std::thread::sleep(Duration::from_millis(100));
    assert!(desktop.releases().is_empty(), "still held");
    // The hotkey jumps to this device, the agent putting the cursor in the
    // middle of its main display (nothing was grabbed to let go of): input
    // goes back there, what is down reported up.
    captured.backend.set_grabbed(false).unwrap();
    assert!(captured.backend.can_warp());
    captured.backend.warp(Point::new(960.0, 540.0)).unwrap();
    captured.expect(&[
        key(usage::LEFT_SHIFT, false),
        key(usage::LEFT_CONTROL, false),
        key(usage::LEFT_ALT, false),
        key(digit2, false),
        ENDED,
    ]);
    wait_until("the release", || desktop.releases().len() == 1);
    assert_eq!(desktop.releases()[0], (Some(1), Some((960.0, 540.0))));
    assert!(!captured.backend.can_warp());

    // Another key lets go at once, as any press held at the edge does.
    desktop.activate(2, 1, (1920.0, 100.0));
    captured.expect(&[push(1919.0, 100.0, 1.0, 0.0)]);
    captured.answer(true);
    desktop.emit(&[Emit::Key(30, true)]);
    captured.expect(&[key(usage::A, true), key(usage::A, false), ENDED]);
    wait_until("the release", || desktop.releases().len() == 2);
    assert_eq!(desktop.releases()[1], (Some(2), Some((1919.0, 100.0))));
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn dropped_while_input_is_handed_over_it_lets_go_at_once() {
    let desktop = Desktop::start(2);
    desktop.asked.lock().unwrap().hang_close = true;
    let state = tempfile::tempdir().unwrap();
    let (mut captured, _) = Captured::new(desktop.capture(state.path()));
    captured.backend.set_exits(&[right_exit()]);
    running(&desktop, &mut captured);
    desktop.activate(1, 1, (1920.0, 100.0));
    captured.expect(&[push(1919.0, 100.0, 1.0, 0.0)]);
    captured.backend.set_grabbed(true).unwrap();

    // The agent exits while it holds input: input goes back to the desktop,
    // and dropping returns once it did, though the desktop is slow to close
    // the session.
    let Captured {
        capture, backend, ..
    } = captured;
    drop(backend);
    let dropping = std::time::Instant::now();
    drop(capture);
    assert!(
        dropping.elapsed() < Duration::from_millis(600),
        "{:?}",
        dropping.elapsed()
    );
    assert_eq!(desktop.releases(), [(Some(1), None)]);
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn a_press_as_the_cursor_comes_back_lets_input_go_at_once() {
    let desktop = Desktop::start(2);
    let state = tempfile::tempdir().unwrap();
    let (mut captured, _) = Captured::new(desktop.capture(state.path()));
    captured.backend.set_exits(&[right_exit()]);
    running(&desktop, &mut captured);
    desktop.activate(1, 1, (1920.0, 100.0));
    captured.expect(&[push(1919.0, 100.0, 1.0, 0.0)]);
    captured.backend.set_grabbed(true).unwrap();

    // The cursor comes back, and a key goes down before the agent says
    // where: let go of at once, without a place, the key not reported (it
    // is lost to the local apps either way), so what follows reaches them.
    captured.backend.set_grabbed(false).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    desktop.emit(&[Emit::Key(30, true)]);
    wait_until("the release", || !desktop.releases().is_empty());
    assert_eq!(desktop.releases(), [(Some(1), None)]);
    captured.quiet();
    // The place said later changes nothing.
    captured.backend.warp(Point::new(1919.0, 100.0)).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(desktop.releases().len(), 1);
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn input_grabbed_as_the_cursor_crosses_is_held_before_the_grab_is_told() {
    let desktop = Desktop::start(2);
    let state = tempfile::tempdir().unwrap();
    let capture = desktop.capture(state.path());
    let (mut captured, _) = Captured::new(capture.clone());
    captured.backend.set_exits(&[right_exit()]);
    running(&desktop, &mut captured);
    desktop.activate(1, 1, (1920.0, 100.0));
    captured.expect(&[push(1919.0, 100.0, 1.0, 0.0)]);

    // The agent crosses at the push; its command saying so is yet to come
    // when the cursor jitters back a pixel: not let go of, the movement
    // reported as on the other device, from the middle of the zone.
    capture.grab_ahead();
    desktop.emit(&[Emit::Motion(-1.0, 0.0)]);
    captured.expect(&[push(960.0, 540.0, -1.0, 0.0)]);
    std::thread::sleep(Duration::from_millis(400));
    assert!(
        desktop.releases().is_empty(),
        "held past the rest and the answer waits"
    );
    captured.quiet();
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn capture_starting_with_a_token_is_not_a_missing_permission() {
    let desktop = Desktop::start(2);
    desktop.asked.lock().unwrap().start_delay = Some(Duration::from_millis(800));
    let state = tempfile::tempdir().unwrap();
    Tokens::in_dir(Some(state.path()))
        .save("input_capture", Some("token-0"))
        .unwrap();
    // Restoring from the first, before the thread runs: as the agent
    // starts at login, capture is starting, not missing its permission,
    // and the app does not ask meanwhile.
    let capture = desktop.capture(state.path());
    assert_eq!(capture.state(), State::Restoring);
    assert_eq!(capture.permission().ask, PermissionAsk::Restoring);
    let (mut captured, started) = Captured::new(capture.clone());
    assert!(
        matches!(started, Err(PlatformError::Starting(_))),
        "{started:?}"
    );
    wait_until("capture to run", || capture.state() == State::Running);
    // Nothing goes to the agent until it started capture.
    assert!(!captured.backend.capturing());
    assert!(captured.restart().is_ok());
    assert!(captured.backend.capturing());
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn an_activation_superseded_before_it_began_leaves_nothing_to_the_next() {
    let desktop = Desktop::start(2);
    let state = tempfile::tempdir().unwrap();
    let (mut captured, _) =
        Captured::new(desktop.capture_waiting(state.path(), Duration::from_millis(500)));
    captured.backend.set_exits(&[right_exit()]);
    running(&desktop, &mut captured);
    let activated = |id: u32, y: f64| {
        desktop.signal(
            "Activated",
            HashMap::from([
                ("activation_id", Variant::from(id)),
                ("barrier_id", Variant::from(1u32)),
                ("cursor_position", Variant::from((1921.0, y))),
            ]),
        );
    };

    // The first is told, and its input comes: it waits for the modifiers.
    activated(1, 100.0);
    std::thread::sleep(Duration::from_millis(100));
    desktop.emit(&[Emit::Motion(5.0, 3.0)]);
    std::thread::sleep(Duration::from_millis(100));
    // The next is told before the first began, which never said it ended:
    // it does not begin when the first would have.
    activated(2, 150.0);
    std::thread::sleep(Duration::from_millis(500));
    captured.quiet();
    // libei starts it and the modifiers come: only its own push is
    // reported, not the first's input, until, unanswered, it is let go of.
    // The first is forgotten, not let go of.
    desktop.emit(&[Emit::Start(2), Emit::Modifiers(0)]);
    captured.expect(&[push(1919.0, 150.0, 1.0, 0.0)]);
    captured.expect(&[ENDED]);
    wait_until("the release", || !desktop.releases().is_empty());
    assert_eq!(desktop.releases(), [(Some(2), Some((1919.0, 150.0)))]);
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn input_before_the_modifiers_waits_for_them() {
    let desktop = Desktop::start(2);
    let state = tempfile::tempdir().unwrap();
    let (mut captured, _) = Captured::new(desktop.capture(state.path()));
    captured.backend.set_exits(&[right_exit()]);
    running(&desktop, &mut captured);

    // The desktop's input comes first, the modifiers held just after: the
    // modifier is reported before the push, the input after it.
    desktop.signal(
        "Activated",
        HashMap::from([
            ("activation_id", Variant::from(1u32)),
            ("barrier_id", Variant::from(1u32)),
            ("cursor_position", Variant::from((1921.0, 100.0))),
        ]),
    );
    std::thread::sleep(Duration::from_millis(100));
    desktop.emit(&[
        Emit::Motion(2.0, 0.0),
        Emit::Key(30, true),
        Emit::Modifiers(1 << 2),
    ]);
    captured.expect(&[
        key(usage::LEFT_CONTROL, true),
        push(1919.0, 100.0, 1.0, 0.0),
        push(1919.0, 100.0, 2.0, 0.0),
    ]);
    // The key, a press held at the edge, lets go: reported, then what is
    // down goes up.
    captured.expect(&[
        key(usage::A, true),
        key(usage::LEFT_CONTROL, false),
        key(usage::A, false),
        ENDED,
    ]);
}
