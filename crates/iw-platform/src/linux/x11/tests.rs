//! Against a real X server, which the Linux container starts as Xvfb
//! (`scripts/linux-x11-tests.sh`): ignored otherwise. Xvfb's only input is
//! XTest's, so capture is told to report injected input here. The tests
//! share the server; run them one at a time.

use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use iw_core::HidUsage;
use iw_core::geometry::{Point, Rect};
use iw_core::hotkey::Modifiers;
use iw_core::input::{Button, InputEvent};
use iw_core::router::Hotkey;
use iw_keymap::usage;
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::randr::{ConnectionExt as _, MonitorInfo};
use x11rb::protocol::xproto::{
    ConnectionExt as _, CreateWindowAux, EventMask, GrabMode, GrabStatus, InputFocus, ModMask,
    Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::{COPY_DEPTH_FROM_PARENT, COPY_FROM_PARENT, CURRENT_TIME, NONE};

use super::platform::connect;
use super::{X11Capture, X11Displays, X11Emulation};
use crate::{Displays, InputCapture, InputEmulation};

const WAIT: Duration = Duration::from_secs(2);
/// How long to wait for something that should not come.
const QUIET: Duration = Duration::from_millis(300);

/// Another client of the display: a window over the whole screen with the
/// keyboard focus, which hears the keys and buttons local apps would.
struct Probe {
    conn: RustConnection,
    root: Window,
    window: Window,
}

impl Probe {
    fn new() -> Self {
        let (conn, root) = connect().unwrap();
        let screen = conn.setup().roots[0].clone();
        let window = conn.generate_id().unwrap();
        let events = EventMask::KEY_PRESS
            | EventMask::KEY_RELEASE
            | EventMask::BUTTON_PRESS
            | EventMask::BUTTON_RELEASE;
        conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            window,
            root,
            0,
            0,
            screen.width_in_pixels,
            screen.height_in_pixels,
            0,
            WindowClass::INPUT_ONLY,
            COPY_FROM_PARENT,
            &CreateWindowAux::new().event_mask(events),
        )
        .unwrap();
        conn.map_window(window).unwrap();
        conn.flush().unwrap();
        // Mapped before the focus can go there.
        conn.get_input_focus().unwrap().reply().unwrap();
        conn.set_input_focus(InputFocus::POINTER_ROOT, window, CURRENT_TIME)
            .unwrap();
        conn.get_input_focus().unwrap().reply().unwrap();
        Self { conn, root, window }
    }

    /// The keys pressed (X key codes) and buttons pressed it heard within
    /// `within`.
    fn heard(&self, within: Duration) -> (Vec<u8>, Vec<u8>) {
        let (mut keys, mut buttons) = (Vec::new(), Vec::new());
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            match self.conn.poll_for_event().unwrap() {
                Some(Event::KeyPress(e)) => keys.push(e.detail),
                Some(Event::ButtonPress(e)) => buttons.push(e.detail),
                Some(_) => {}
                None => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        (keys, buttons)
    }

    fn pointer(&self) -> Point {
        let r = self.conn.query_pointer(self.root).unwrap().reply().unwrap();
        Point::new(r.root_x.into(), r.root_y.into())
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        let _ = self.conn.destroy_window(self.window);
        let _ = self.conn.flush();
    }
}

fn tap(emulation: &mut X11Emulation, key: HidUsage) {
    emulation.key(key, true).unwrap();
    emulation.key(key, false).unwrap();
}

/// What capture reported within `within`.
fn reported(events: &Receiver<InputEvent>, within: Duration) -> Vec<InputEvent> {
    let deadline = Instant::now() + within;
    let mut out = Vec::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match events.recv_timeout(left) {
            Ok(e) => out.push(e),
            Err(_) => break,
        }
    }
    out
}

fn keys(events: &[InputEvent]) -> Vec<(HidUsage, bool)> {
    events
        .iter()
        .filter_map(|e| match e {
            InputEvent::Key { usage, pressed } => Some((*usage, *pressed)),
            _ => None,
        })
        .collect()
}

#[test]
#[ignore = "needs an X server: scripts/linux-x11-tests.sh"]
fn displays_are_randrs_monitors() {
    let (conn, root) = connect().unwrap();
    let output = conn
        .randr_get_screen_resources(root)
        .unwrap()
        .reply()
        .unwrap()
        .outputs[0];
    let atom = |name: &str| {
        conn.intern_atom(false, name.as_bytes())
            .unwrap()
            .reply()
            .unwrap()
            .atom
    };
    let monitor = |name, x, outputs| MonitorInfo {
        name: atom(name),
        primary: x == 0,
        automatic: false,
        x,
        y: 0,
        width: 1920,
        height: 1080,
        width_in_millimeters: 0,
        height_in_millimeters: 0,
        outputs,
    };
    // The first takes the screen's output, and the monitor RandR made for
    // it goes.
    conn.randr_set_monitor(root, monitor("IW-LEFT", 0, vec![output]))
        .unwrap()
        .check()
        .unwrap();
    conn.randr_set_monitor(root, monitor("IW-RIGHT", 1920, vec![]))
        .unwrap()
        .check()
        .unwrap();
    let displays = X11Displays::connect().unwrap().displays().unwrap();
    for name in ["IW-LEFT", "IW-RIGHT"] {
        conn.randr_delete_monitor(root, atom(name)).unwrap();
    }
    conn.flush().unwrap();
    let mut found: Vec<_> = displays
        .iter()
        .map(|d| (d.id.as_str(), d.bounds, d.primary))
        .collect();
    found.sort_by(|a, b| a.1.x.total_cmp(&b.1.x));
    assert_eq!(
        found,
        vec![
            ("IW-LEFT", Rect::new(0.0, 0.0, 1920.0, 1080.0), true),
            ("IW-RIGHT", Rect::new(1920.0, 0.0, 1920.0, 1080.0), false),
        ]
    );
}

#[test]
#[ignore = "needs an X server: scripts/linux-x11-tests.sh"]
fn injected_input_reaches_other_clients() {
    let probe = Probe::new();
    let mut emulation = X11Emulation::connect().unwrap();
    emulation.move_to(Point::new(100.0, 200.0)).unwrap();
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(probe.pointer(), Point::new(100.0, 200.0));
    assert_eq!(emulation.position(), Some(Point::new(100.0, 200.0)));
    // Fractions carry over to the next move.
    emulation.move_by(10.5, -5.5).unwrap();
    emulation.move_by(10.5, -5.5).unwrap();
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(probe.pointer(), Point::new(121.0, 189.0));

    tap(&mut emulation, usage::A);
    emulation.button(Button::Left, true).unwrap();
    emulation.button(Button::Left, false).unwrap();
    emulation.button(Button::Back, true).unwrap();
    emulation.button(Button::Back, false).unwrap();
    // A notch up, half a notch right twice.
    emulation.scroll(0.0, 1.0).unwrap();
    emulation.scroll(0.5, 0.0).unwrap();
    emulation.scroll(0.5, 0.0).unwrap();
    let (keys, buttons) = probe.heard(QUIET);
    assert_eq!(keys, vec![38]);
    assert_eq!(buttons, vec![1, 8, 4, 7]);
}

#[test]
#[ignore = "needs an X server: scripts/linux-x11-tests.sh"]
fn capture_reports_input_but_not_injections() {
    let mut emulation = X11Emulation::connect().unwrap();
    let (tx, events) = mpsc::channel();
    let mut capture = X11Capture::connect().unwrap().reporting_injected();
    capture.start(tx).unwrap();
    let (quiet_tx, quiet) = mpsc::channel();
    let mut filtering = X11Capture::connect().unwrap();
    filtering.start(quiet_tx).unwrap();

    emulation.move_to(Point::new(500.0, 500.0)).unwrap();
    let _ = reported(&events, QUIET);
    emulation.move_by(7.0, -3.0).unwrap();
    tap(&mut emulation, usage::A);
    emulation.button(Button::Right, true).unwrap();
    emulation.button(Button::Right, false).unwrap();
    emulation.scroll(0.0, -1.0).unwrap();
    let got = reported(&events, WAIT.min(QUIET * 2));
    let motion: Vec<_> = got
        .iter()
        .filter_map(|e| match e {
            InputEvent::PointerMotion { position, delta } => Some((*position, *delta)),
            _ => None,
        })
        .collect();
    assert_eq!(
        motion,
        vec![(Point::new(507.0, 497.0), Point::new(7.0, -3.0))],
        "{got:?}"
    );
    assert_eq!(keys(&got), vec![(usage::A, true), (usage::A, false)]);
    assert!(got.contains(&InputEvent::Button {
        button: Button::Right,
        pressed: true
    }));
    assert!(got.contains(&InputEvent::Button {
        button: Button::Right,
        pressed: false
    }));
    assert_eq!(
        got.iter()
            .filter(|e| matches!(e, InputEvent::Scroll { .. }))
            .collect::<Vec<_>>(),
        vec![&InputEvent::Scroll { dx: 0.0, dy: -1.0 }]
    );
    // What XTest injects is no one's at this computer.
    assert_eq!(reported(&quiet, QUIET), Vec::new());
}

#[test]
#[ignore = "needs an X server: scripts/linux-x11-tests.sh"]
fn grabbed_input_reaches_no_other_client() {
    let probe = Probe::new();
    let mut emulation = X11Emulation::connect().unwrap();
    let (tx, events) = mpsc::channel();
    let mut capture = X11Capture::connect().unwrap().reporting_injected();
    capture.start(tx).unwrap();
    emulation.move_to(Point::new(10.0, 20.0)).unwrap();
    std::thread::sleep(Duration::from_millis(50));

    capture.set_grabbed(true).unwrap();
    capture.settle();
    // Held in the middle of the display it was on.
    let held = probe.pointer();
    let screen = probe.conn.setup().roots[0].clone();
    assert!(
        held.x < f64::from(screen.width_in_pixels) && held.x > 10.0 && held.y > 20.0,
        "{held:?}"
    );
    let _ = reported(&events, QUIET);
    tap(&mut emulation, usage::A);
    emulation.button(Button::Left, true).unwrap();
    emulation.button(Button::Left, false).unwrap();
    emulation.move_by(40.0, 0.0).unwrap();
    let (keys_heard, buttons) = probe.heard(QUIET);
    assert_eq!((keys_heard, buttons), (vec![], vec![]));
    let got = reported(&events, QUIET);
    assert_eq!(keys(&got), vec![(usage::A, true), (usage::A, false)]);
    // Motion while grabbed: where the cursor is held, and how far it went.
    assert!(
        got.contains(&InputEvent::PointerMotion {
            position: held,
            delta: Point::new(40.0, 0.0)
        }),
        "{got:?}"
    );
    // Far from the middle, the hidden cursor is put back there.
    emulation.move_by(300.0, 0.0).unwrap();
    let _ = reported(&events, QUIET);
    assert_eq!(probe.pointer(), held);
    let taken = probe
        .conn
        .grab_pointer(
            false,
            probe.root,
            EventMask::NO_EVENT,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
            NONE,
            NONE,
            CURRENT_TIME,
        )
        .unwrap()
        .reply()
        .unwrap()
        .status;
    assert_eq!(taken, GrabStatus::ALREADY_GRABBED);

    capture.set_grabbed(false).unwrap();
    capture.settle();
    tap(&mut emulation, usage::A);
    let (keys_heard, _) = probe.heard(QUIET);
    assert_eq!(keys_heard, vec![38]);
    let taken = probe
        .conn
        .grab_pointer(
            false,
            probe.root,
            EventMask::NO_EVENT,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
            NONE,
            NONE,
            CURRENT_TIME,
        )
        .unwrap()
        .reply()
        .unwrap()
        .status;
    assert_eq!(taken, GrabStatus::SUCCESS);
    probe.conn.ungrab_pointer(CURRENT_TIME).unwrap();
    probe.conn.flush().unwrap();
}

#[test]
#[ignore = "needs an X server: scripts/linux-x11-tests.sh"]
fn reserved_hotkeys_are_kept_from_other_clients() {
    let probe = Probe::new();
    let mut emulation = X11Emulation::connect().unwrap();
    let (tx, events) = mpsc::channel();
    let mut capture = X11Capture::connect().unwrap().reporting_injected();
    capture.start(tx).unwrap();
    let lock = Hotkey::new(Modifiers::CONTROL_ALT_SHIFT, usage::L);
    assert!(capture.reserve_hotkeys(&[lock]));

    let press = |e: &mut X11Emulation| {
        for k in [usage::LEFT_CONTROL, usage::LEFT_ALT, usage::LEFT_SHIFT] {
            e.key(k, true).unwrap();
        }
        tap(e, usage::L);
        for k in [usage::LEFT_SHIFT, usage::LEFT_ALT, usage::LEFT_CONTROL] {
            e.key(k, false).unwrap();
        }
    };
    let _ = reported(&events, QUIET);
    press(&mut emulation);
    // L alone is the apps'.
    tap(&mut emulation, usage::L);
    let (keys_heard, _) = probe.heard(QUIET);
    // Control, Alt and Shift, then L by itself only.
    assert_eq!(keys_heard, vec![37, 64, 50, 46]);
    // Capture still hears the hotkey's key.
    let got = keys(&reported(&events, QUIET));
    assert!(
        got.contains(&(usage::L, true)) && got.contains(&(usage::L, false)),
        "{got:?}"
    );

    // With Caps Lock on, as well.
    tap(&mut emulation, usage::CAPS_LOCK);
    press(&mut emulation);
    tap(&mut emulation, usage::CAPS_LOCK);
    let (keys_heard, _) = probe.heard(QUIET);
    assert!(!keys_heard.contains(&46), "{keys_heard:?}");

    // Let go of: the apps have it again.
    assert!(capture.reserve_hotkeys(&[]));
    press(&mut emulation);
    let (keys_heard, _) = probe.heard(QUIET);
    assert!(keys_heard.contains(&46), "{keys_heard:?}");
}

/// Whether X key code `code` is down, as another client sees it.
fn key_down(code: u8) -> bool {
    let (conn, _) = connect().unwrap();
    let keys = conn.query_keymap().unwrap().reply().unwrap().keys;
    keys[usize::from(code / 8)] & (1 << (code % 8)) != 0
}

#[test]
#[ignore = "needs an X server: scripts/linux-x11-tests.sh"]
fn what_is_held_down_is_let_go_of_when_dropped() {
    use x11rb::protocol::xproto::{KEY_PRESS_EVENT, KEY_RELEASE_EVENT};
    use x11rb::protocol::xtest::ConnectionExt as _;
    use x11rb::wrapper::ConnectionExt as _;

    // XTest by itself leaves a key down when its client goes.
    let (conn, root) = connect().unwrap();
    conn.xtest_fake_input(KEY_PRESS_EVENT, 38, CURRENT_TIME, root, 0, 0, 0)
        .unwrap();
    conn.sync().unwrap();
    drop(conn);
    std::thread::sleep(Duration::from_millis(100));
    let left_down = key_down(38);
    let (conn, root) = connect().unwrap();
    conn.xtest_fake_input(KEY_RELEASE_EVENT, 38, CURRENT_TIME, root, 0, 0, 0)
        .unwrap();
    conn.sync().unwrap();
    assert!(left_down, "the X server let go of the key itself");
    assert!(!key_down(38));

    let mut emulation = X11Emulation::connect().unwrap();
    emulation.key(usage::A, true).unwrap();
    emulation.button(Button::Middle, true).unwrap();
    std::thread::sleep(Duration::from_millis(50));
    assert!(key_down(38));
    drop(emulation);
    std::thread::sleep(Duration::from_millis(100));
    assert!(!key_down(38));
    let (conn, root) = connect().unwrap();
    let mask = conn.query_pointer(root).unwrap().reply().unwrap().mask;
    assert_eq!(
        u16::from(mask) & u16::from(x11rb::protocol::xproto::KeyButMask::BUTTON2),
        0
    );
}

#[test]
#[ignore = "needs an X server: scripts/linux-x11-tests.sh"]
fn a_key_pressed_again_while_down_is_left_to_the_servers_repeat() {
    let probe = Probe::new();
    let mut emulation = X11Emulation::connect().unwrap();
    // The controlling device's repeat, well before the server's own.
    emulation.key(usage::A, true).unwrap();
    emulation.key(usage::A, true).unwrap();
    emulation.key(usage::A, false).unwrap();
    let (keys_heard, _) = probe.heard(QUIET);
    assert_eq!(keys_heard, vec![38]);
}

fn presses_of(events: &[InputEvent], key: HidUsage) -> usize {
    keys(events)
        .iter()
        .filter(|(k, pressed)| *k == key && *pressed)
        .count()
}

#[test]
#[ignore = "needs an X server: scripts/linux-x11-tests.sh"]
fn a_held_key_repeats_for_the_device_in_control_only() {
    let mut emulation = X11Emulation::connect().unwrap();
    let (tx, events) = mpsc::channel();
    let mut capture = X11Capture::connect().unwrap().reporting_injected();
    capture.start(tx).unwrap();

    // Not grabbed: the server repeats it for local apps, capture does not.
    emulation.key(usage::A, true).unwrap();
    std::thread::sleep(Duration::from_millis(1000));
    emulation.key(usage::A, false).unwrap();
    assert_eq!(presses_of(&reported(&events, QUIET), usage::A), 1);

    // Grabbed: after Xvfb's 660 ms, 25 a second.
    capture.set_grabbed(true).unwrap();
    capture.settle();
    emulation.key(usage::A, true).unwrap();
    std::thread::sleep(Duration::from_millis(1000));
    emulation.key(usage::A, false).unwrap();
    let got = reported(&events, QUIET);
    let presses = presses_of(&got, usage::A);
    assert!((5..=12).contains(&presses), "{presses} presses: {got:?}");
    // None after the release.
    let release = got
        .iter()
        .position(|e| {
            *e == InputEvent::Key {
                usage: usage::A,
                pressed: false,
            }
        })
        .unwrap();
    assert_eq!(release, got.len() - 1, "{got:?}");
    assert_eq!(presses_of(&reported(&events, QUIET), usage::A), 0);

    // A modifier does not repeat.
    emulation.key(usage::LEFT_SHIFT, true).unwrap();
    std::thread::sleep(Duration::from_millis(900));
    emulation.key(usage::LEFT_SHIFT, false).unwrap();
    assert_eq!(presses_of(&reported(&events, QUIET), usage::LEFT_SHIFT), 1);
    capture.set_grabbed(false).unwrap();
    capture.settle();
}

#[test]
#[ignore = "needs an X server: scripts/linux-x11-tests.sh"]
fn hotkeys_are_all_left_to_apps_when_another_holds_one() {
    let probe = Probe::new();
    // Another app's shortcut: Control+Alt+Shift+1, Alt being Mod1.
    let one = 10;
    let state = ModMask::CONTROL | ModMask::SHIFT | ModMask::M1;
    probe
        .conn
        .grab_key(
            true,
            probe.root,
            state,
            one,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
        )
        .unwrap()
        .check()
        .unwrap();
    let mut emulation = X11Emulation::connect().unwrap();
    let mut capture = X11Capture::connect().unwrap();
    let lock = Hotkey::new(Modifiers::CONTROL_ALT_SHIFT, usage::L);
    let jump = Hotkey::new(Modifiers::CONTROL_ALT_SHIFT, usage::DIGIT1);
    assert!(!capture.reserve_hotkeys(&[lock, jump]));
    assert_eq!(capture.hotkeys_held_back(), Some(false));
    // Not even the lock is held back.
    for k in [usage::LEFT_CONTROL, usage::LEFT_ALT, usage::LEFT_SHIFT] {
        emulation.key(k, true).unwrap();
    }
    tap(&mut emulation, usage::L);
    for k in [usage::LEFT_SHIFT, usage::LEFT_ALT, usage::LEFT_CONTROL] {
        emulation.key(k, false).unwrap();
    }
    let (keys_heard, _) = probe.heard(QUIET);
    assert!(keys_heard.contains(&46), "{keys_heard:?}");
    probe.conn.ungrab_key(one, probe.root, state).unwrap();
    probe.conn.flush().unwrap();
}

#[test]
#[ignore = "needs an X server: scripts/linux-x11-tests.sh"]
fn a_key_held_while_crossing_goes_on_repeating() {
    let mut emulation = X11Emulation::connect().unwrap();
    let (tx, events) = mpsc::channel();
    let mut capture = X11Capture::connect().unwrap().reporting_injected();
    capture.start(tx).unwrap();
    emulation.key(usage::A, true).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    capture.set_grabbed(true).unwrap();
    capture.settle();
    std::thread::sleep(Duration::from_millis(700));
    emulation.key(usage::A, false).unwrap();
    let got = reported(&events, QUIET);
    // The press before the grab, then repeats from 660 ms after it.
    let presses = presses_of(&got, usage::A);
    assert!((4..=13).contains(&presses), "{presses} presses: {got:?}");
    capture.set_grabbed(false).unwrap();
    capture.settle();
}

#[test]
#[ignore = "needs an X server: scripts/linux-x11-tests.sh"]
fn hotkeys_follow_a_changed_modifier_mapping() {
    let probe = Probe::new();
    let mut emulation = X11Emulation::connect().unwrap();
    let (tx, _events) = mpsc::channel();
    let mut capture = X11Capture::connect().unwrap();
    capture.start(tx).unwrap();
    let lock = Hotkey::new(Modifiers::CONTROL_ALT_SHIFT, usage::L);
    assert!(capture.reserve_hotkeys(&[lock]));
    let press = |e: &mut X11Emulation| {
        for k in [usage::LEFT_CONTROL, usage::LEFT_ALT, usage::LEFT_SHIFT] {
            e.key(k, true).unwrap();
        }
        tap(e, usage::L);
        for k in [usage::LEFT_SHIFT, usage::LEFT_ALT, usage::LEFT_CONTROL] {
            e.key(k, false).unwrap();
        }
    };
    let set = |keycodes: &[u8]| {
        let reply = probe
            .conn
            .set_modifier_mapping(keycodes)
            .unwrap()
            .reply()
            .unwrap();
        assert_eq!(
            reply.status,
            x11rb::protocol::xproto::MappingStatus::SUCCESS
        );
        // The worker hears of it and grabs again.
        std::thread::sleep(QUIET);
        capture.settle();
    };
    let original = probe
        .conn
        .get_modifier_mapping()
        .unwrap()
        .reply()
        .unwrap()
        .keycodes;
    // Alt moves from Mod1 to Mod3.
    let per = original.len() / 8;
    let mut moved = original.clone();
    for i in 0..per {
        moved[5 * per + i] = moved[3 * per + i];
        moved[3 * per + i] = 0;
    }
    set(&moved);
    press(&mut emulation);
    let (keys_heard, _) = probe.heard(QUIET);
    assert!(!keys_heard.contains(&46), "{keys_heard:?}");
    set(&original);
    press(&mut emulation);
    let (keys_heard, _) = probe.heard(QUIET);
    assert!(!keys_heard.contains(&46), "{keys_heard:?}");
}

/// An Xvfb of the test's own, on a display nobody uses.
/// An X server of a test's own, for tests that stop it and start it
/// again, `DISPLAY` naming it while it lives. Dropped, even as a test
/// panics, it is killed and `DISPLAY` names the shared server again.
pub(super) struct OwnXvfb {
    server: Option<std::process::Child>,
    display: String,
    shared: Option<std::ffi::OsString>,
}

impl OwnXvfb {
    /// Starts one on a display it picks itself (`-displayfd`), as the
    /// script does, since other tests may be starting servers too.
    pub(super) fn start() -> Self {
        let (server, display) = xvfb(None);
        let shared = std::env::var_os("DISPLAY");
        // SAFETY: the tests run one at a time, and nothing else reads the
        // environment meanwhile.
        unsafe { std::env::set_var("DISPLAY", &display) };
        Self {
            server: Some(server),
            display,
            shared,
        }
    }

    /// Stops the server, as the display server goes away at a logout.
    ///
    /// The display stays this test's until it starts again: a killed Xvfb
    /// leaves its lock file behind, which another Xvfb picking a display
    /// (`-displayfd`, as other test runs in the same container do) takes
    /// for free once its process is gone. So the lock is written again
    /// with this process, which is alive.
    pub(super) fn stop(&mut self) {
        if let Some(mut server) = self.server.take() {
            let _ = server.kill();
            let _ = server.wait();
            let lock = self.lock_file();
            // Xvfb's lock is read-only; it is replaced, not written to.
            let _ = std::fs::remove_file(&lock);
            let _ = std::fs::write(&lock, format!("{:>10}\n", std::process::id()));
        }
        let _ = std::fs::remove_file(format!("/tmp/.X11-unix/X{}", &self.display[1..]));
    }

    /// Starts it again on the same display, as at a new login.
    pub(super) fn start_again(&mut self) {
        self.stop();
        let _ = std::fs::remove_file(self.lock_file());
        self.server = Some(xvfb(Some(&self.display)).0);
    }

    fn lock_file(&self) -> String {
        format!("/tmp/.X{}-lock", &self.display[1..])
    }
}

impl Drop for OwnXvfb {
    fn drop(&mut self) {
        self.stop();
        let _ = std::fs::remove_file(self.lock_file());
        // SAFETY: as in `start`.
        unsafe {
            match &self.shared {
                Some(display) => std::env::set_var("DISPLAY", display),
                None => std::env::remove_var("DISPLAY"),
            }
        }
    }
}

/// Starts Xvfb on `display`, or one it picks, and waits until it says it
/// is ready by writing the display's number.
fn xvfb(display: Option<&str>) -> (std::process::Child, String) {
    use std::io::Read as _;
    use std::os::fd::FromRawFd as _;

    let mut ends = [0; 2];
    // SAFETY: room for the pipe's two ends.
    assert_eq!(unsafe { libc::pipe(ends.as_mut_ptr()) }, 0);
    let (read, write) = (ends[0], ends[1]);
    // SAFETY: the read end, which Xvfb has no use for, stays here.
    unsafe { libc::fcntl(read, libc::F_SETFD, libc::FD_CLOEXEC) };
    let mut command = std::process::Command::new("Xvfb");
    command.args(display);
    let server = command
        .args(["-displayfd", &write.to_string()])
        .args([
            "-screen",
            "0",
            "1280x800x24",
            "-nolisten",
            "tcp",
            "-noreset",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    // SAFETY: the parent's copy of the write end; Xvfb has its own.
    unsafe { libc::close(write) };
    // SAFETY: the read end, owned from here on.
    let mut said = unsafe { std::fs::File::from_raw_fd(read) };
    let mut ready = libc::pollfd {
        fd: read,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one valid pollfd.
    let ready = unsafe { libc::poll(&mut ready, 1, 10_000) } == 1;
    assert!(ready, "Xvfb did not start");
    let mut number = [0; 16];
    let len = said.read(&mut number).unwrap();
    let number = String::from_utf8_lossy(&number[..len]).trim().to_owned();
    assert!(!number.is_empty(), "Xvfb did not say its display");
    let display = format!(":{number}");
    // Ready as it says, but under load the first connection has been
    // turned away ("Connection reset by peer"): wait until one is taken.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match x11rb::connect(Some(&display)) {
            Ok(_) => break,
            Err(e) => {
                assert!(
                    Instant::now() < deadline,
                    "Xvfb on {display} takes no connection: {e}"
                );
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
    (server, display)
}

#[test]
#[ignore = "needs an X server: scripts/linux-x11-tests.sh"]
fn capture_and_injection_come_back_after_the_server_restarts() {
    let mut server = OwnXvfb::start();
    let mut emulation = X11Emulation::connect().unwrap();
    let (tx, events) = mpsc::channel();
    let mut capture = X11Capture::connect().unwrap().reporting_injected();
    capture.start(tx).unwrap();
    let lock = Hotkey::new(Modifiers::CONTROL_ALT_SHIFT, usage::L);
    assert!(capture.reserve_hotkeys(&[lock]));
    assert_eq!(capture.hotkeys_held_back(), Some(true));
    emulation.key(usage::C, true).unwrap();
    assert_eq!(keys(&reported(&events, QUIET)), vec![(usage::C, true)]);

    server.stop();
    // What was down is let go of, wherever it went.
    assert_eq!(keys(&reported(&events, QUIET)), vec![(usage::C, false)]);
    assert_eq!(capture.hotkeys_held_back(), Some(false));
    // The display server starts over, as at a new login.
    server.start_again();
    // Capture tries again 0.5, 1.5, 3.5, 7.5, 15.5 and 25.5 s after it
    // lost the server (`Backoff`); under load a try may come before the
    // server takes connections, so allow for the one after.
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        assert!(Instant::now() < deadline, "capture did not come back");
        // Fails while injection connects again.
        let _ = emulation.key(usage::A, true);
        let _ = emulation.key(usage::A, false);
        if keys(&reported(&events, Duration::from_millis(200))).contains(&(usage::A, true)) {
            break;
        }
    }
    // The hotkey is held back on the new server too.
    assert_eq!(capture.hotkeys_held_back(), Some(true));
    let probe = Probe::new();
    for k in [usage::LEFT_CONTROL, usage::LEFT_ALT, usage::LEFT_SHIFT] {
        emulation.key(k, true).unwrap();
    }
    tap(&mut emulation, usage::L);
    for k in [usage::LEFT_SHIFT, usage::LEFT_ALT, usage::LEFT_CONTROL] {
        emulation.key(k, false).unwrap();
    }
    let (keys_heard, _) = probe.heard(QUIET);
    assert_eq!(keys_heard, vec![37, 64, 50]);

    drop((probe, capture, emulation, server));
}

#[test]
#[ignore = "needs an X server: scripts/linux-x11-tests.sh"]
fn nothing_goes_out_while_another_app_holds_the_keyboard() {
    let probe = Probe::new();
    let mut emulation = X11Emulation::connect().unwrap();
    let (tx, events) = mpsc::channel();
    let mut capture = X11Capture::connect().unwrap().reporting_injected();
    capture.start(tx).unwrap();
    // Before, as the key goes down for the remote's sake.
    emulation.key(usage::C, true).unwrap();
    assert_eq!(keys(&reported(&events, QUIET)), vec![(usage::C, true)]);

    // A password prompt holds the keyboard.
    let grab_keyboard = || {
        probe
            .conn
            .grab_keyboard(
                false,
                probe.window,
                CURRENT_TIME,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
            )
            .unwrap()
            .reply()
            .unwrap()
            .status
    };
    assert_eq!(grab_keyboard(), GrabStatus::SUCCESS);
    capture.set_grabbed(true).unwrap();
    capture.settle();
    tap(&mut emulation, usage::A);
    emulation.move_by(5.0, 0.0).unwrap();
    // Only the release of what went out down.
    emulation.key(usage::C, false).unwrap();
    assert_eq!(keys(&reported(&events, QUIET)), vec![(usage::C, false)]);
    let (keys_heard, _) = probe.heard(QUIET);
    assert!(keys_heard.contains(&38), "{keys_heard:?}");
    // Taking control back still goes out, with the modifiers it needs.
    let take_back = Hotkey::new(Modifiers::CONTROL_ALT_SHIFT, usage::ESCAPE);
    capture.watch_hotkeys(&[take_back]);
    let modifiers = [usage::LEFT_CONTROL, usage::LEFT_ALT, usage::LEFT_SHIFT];
    for k in modifiers {
        emulation.key(k, true).unwrap();
    }
    tap(&mut emulation, usage::ESCAPE);
    for k in modifiers {
        emulation.key(k, false).unwrap();
    }
    let mut expected: Vec<_> = modifiers.iter().map(|k| (*k, true)).collect();
    expected.extend([(usage::ESCAPE, true), (usage::ESCAPE, false)]);
    expected.extend(modifiers.iter().map(|k| (*k, false)));
    assert_eq!(keys(&reported(&events, QUIET)), expected);
    let _ = probe.heard(QUIET);

    // It lets go: capture takes over within its retries.
    probe.conn.ungrab_keyboard(CURRENT_TIME).unwrap();
    probe.conn.flush().unwrap();
    std::thread::sleep(Duration::from_millis(300));
    capture.settle();
    tap(&mut emulation, usage::A);
    assert_eq!(
        keys(&reported(&events, QUIET)),
        vec![(usage::A, true), (usage::A, false)]
    );
    let (keys_heard, _) = probe.heard(QUIET);
    assert!(keys_heard.is_empty(), "{keys_heard:?}");
    assert_eq!(grab_keyboard(), GrabStatus::ALREADY_GRABBED);
    capture.set_grabbed(false).unwrap();
    capture.settle();
}

#[test]
#[ignore = "needs an X server: scripts/linux-x11-tests.sh"]
fn keys_from_other_devices_reach_the_app_even_when_they_are_hotkeys_here() {
    let probe = Probe::new();
    let (tx, _events) = mpsc::channel();
    let mut capture = X11Capture::connect().unwrap();
    capture.start(tx).unwrap();
    let lock = Hotkey::new(Modifiers::CONTROL_ALT_SHIFT, usage::L);
    assert!(capture.reserve_hotkeys(&[lock]));
    let chord = |e: &mut X11Emulation| {
        for k in [usage::LEFT_CONTROL, usage::LEFT_ALT, usage::LEFT_SHIFT] {
            e.key(k, true).unwrap();
        }
        tap(e, usage::L);
        for k in [usage::LEFT_SHIFT, usage::LEFT_ALT, usage::LEFT_CONTROL] {
            e.key(k, false).unwrap();
        }
    };
    // Another device sends the chord, which is this computer's lock.
    let mut injection = X11Emulation::connect()
        .unwrap()
        .pausing(capture.hotkey_pause());
    chord(&mut injection);
    let (keys_heard, _) = probe.heard(QUIET);
    assert_eq!(keys_heard, vec![37, 64, 50, 46]);
    assert_eq!(capture.hotkeys_held_back(), Some(false));

    // Once injection held no key for a while, they are held back again.
    std::thread::sleep(Duration::from_millis(1500));
    capture.settle();
    assert_eq!(capture.hotkeys_held_back(), Some(true));
    let mut other = X11Emulation::connect().unwrap();
    chord(&mut other);
    let (keys_heard, _) = probe.heard(QUIET);
    assert_eq!(keys_heard, vec![37, 64, 50]);
}
