//! Input without portals on headless sway: the virtual pointer and
//! keyboard inject into a window of the test's own, and capture at layer
//! surfaces takes what the test's own virtual pointer and keyboard (the
//! user's mouse and keyboard, as it were) do at an exit. A session lock of
//! the test's own takes the keyboard and the pointer from capture.

use std::os::fd::AsFd;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use iw_core::geometry::{Point, Rect};
use iw_core::hotkey::Modifiers;
use iw_core::input::{Button, InputEvent};
use iw_core::layout::{Edge, Exit};
use iw_core::router::Hotkey;
use iw_keymap::usage;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_keyboard, wl_output, wl_pointer, wl_registry, wl_seat, wl_shm,
    wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum};
use wayland_protocols::ext::session_lock::v1::client::{
    ext_session_lock_manager_v1, ext_session_lock_surface_v1, ext_session_lock_v1,
};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1, zwp_virtual_keyboard_v1,
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1, zwlr_virtual_pointer_v1,
};

use super::super::wlr;
use super::super::wlroots::{Globals, US_KEYMAP};
use super::super::{WaylandDisplays, WlrCapture, WlrEmulation};
use super::{Sway, wait_until};
use crate::{InputCapture, InputEmulation};

/// What the window heard.
#[derive(Clone, Debug, PartialEq)]
enum Heard {
    Pointer(f64, f64),
    Button(u32, bool),
    Axis(f64),
    Key(u32, bool),
    /// `wl_keyboard.modifiers`: depressed and locked.
    Modifiers(u32, u32),
    KeyboardEnter,
    KeyboardLeave,
}

/// A fullscreen window of the test's own, which hears the seat's input; with
/// a virtual pointer and keyboard of its own if asked, standing for the
/// user's mouse and keyboard.
struct Window {
    queue: EventQueue<Win>,
    win: Win,
    started: Instant,
}

struct Win {
    qh: QueueHandle<Win>,
    shm: wl_shm::WlShm,
    surface: wl_surface::WlSurface,
    size: (i32, i32),
    mapped: bool,
    heard: Vec<Heard>,
    pointer: Option<wl_pointer::WlPointer>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    mouse: Option<zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1>,
    keys: Option<zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1>,
    /// The keymap the seat last sent, and how many it sent.
    keymap: String,
    keymaps: usize,
}

impl Window {
    /// With a virtual pointer and keyboard typing with `keymap`, if given.
    fn open(devices: Option<&str>) -> Self {
        let conn = Connection::connect_to_env().unwrap();
        let (globals, queue) = registry_queue_init::<Win>(&conn).unwrap();
        let qh = queue.handle();
        let compositor: wl_compositor::WlCompositor = globals.bind(&qh, 1..=4, ()).unwrap();
        let wm: xdg_wm_base::XdgWmBase = globals.bind(&qh, 1..=1, ()).unwrap();
        let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=7, ()).unwrap();
        let shm = globals.bind(&qh, 1..=1, ()).unwrap();
        let surface = compositor.create_surface(&qh, ());
        let xdg = wm.get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg.get_toplevel(&qh, ());
        toplevel.set_title("inputweave test window".to_owned());
        toplevel.set_fullscreen(None);
        surface.commit();
        let (mouse, keys) = if let Some(keymap) = devices {
            let pointers: zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1 =
                globals.bind(&qh, 1..=1, ()).unwrap();
            let keyboards: zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1 =
                globals.bind(&qh, 1..=1, ()).unwrap();
            let mouse = pointers.create_virtual_pointer(Some(&seat), &qh, ());
            let keys = keyboards.create_virtual_keyboard(&seat, &qh, ());
            let mut text = keymap.as_bytes().to_vec();
            text.push(0);
            let fd = wlr::memfd_with("test-keymap", &text).unwrap();
            keys.keymap(1, fd.as_fd(), text.len() as u32);
            (Some(mouse), Some(keys))
        } else {
            (None, None)
        };
        let mut window = Self {
            queue,
            win: Win {
                qh,
                shm,
                surface,
                size: (0, 0),
                mapped: false,
                heard: Vec::new(),
                pointer: None,
                keyboard: None,
                mouse,
                keys,
                keymap: String::new(),
                keymaps: 0,
            },
            started: Instant::now(),
        };
        window.until("the window to show", |w| w.mapped);
        window
    }

    fn roundtrip(&mut self) {
        self.queue.roundtrip(&mut self.win).unwrap();
    }

    /// Pumps until `done`, for at most ten seconds.
    fn until(&mut self, what: &str, mut done: impl FnMut(&Win) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            self.roundtrip();
            if done(&self.win) {
                return;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Pumps for `wait`.
    fn pump_for(&mut self, wait: Duration) {
        let until = Instant::now() + wait;
        while Instant::now() < until {
            self.roundtrip();
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn time(&self) -> u32 {
        self.started.elapsed().as_millis() as u32
    }

    fn mouse_to(&mut self, x: u32, y: u32, extent: (u32, u32)) {
        let time = self.time();
        let mouse = self.win.mouse.as_ref().unwrap();
        mouse.motion_absolute(time, x, y, extent.0, extent.1);
        mouse.frame();
        self.roundtrip();
    }

    fn mouse_by(&mut self, dx: f64, dy: f64) {
        let time = self.time();
        let mouse = self.win.mouse.as_ref().unwrap();
        mouse.motion(time, dx, dy);
        mouse.frame();
        self.roundtrip();
    }

    fn button(&mut self, code: u32, pressed: bool) {
        let time = self.time();
        let mouse = self.win.mouse.as_ref().unwrap();
        let state = if pressed {
            wl_pointer::ButtonState::Pressed
        } else {
            wl_pointer::ButtonState::Released
        };
        mouse.button(time, code, state);
        mouse.frame();
        self.roundtrip();
    }

    fn key(&mut self, code: u32, pressed: bool) {
        let time = self.time();
        self.win
            .keys
            .as_ref()
            .unwrap()
            .key(time, code, u32::from(pressed));
        self.roundtrip();
    }

    fn take(&mut self) -> Vec<Heard> {
        std::mem::take(&mut self.win.heard)
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Win {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for Win {
    fn event(
        _: &mut Self,
        wm: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm.pong(serial);
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for Win {
    fn event(
        win: &mut Self,
        _: &xdg_toplevel::XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Configure { width, height, .. } = event
            && width > 0
            && height > 0
        {
            win.size = (width, height);
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for Win {
    fn event(
        win: &mut Self,
        xdg: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let xdg_surface::Event::Configure { serial } = event else {
            return;
        };
        xdg.ack_configure(serial);
        let (w, h) = if win.size.0 > 0 { win.size } else { (100, 100) };
        let size = (w * h * 4) as u64;
        let fd = wlr::memfd("test-window", size).unwrap();
        let pool = win.shm.create_pool(fd.as_fd(), size as i32, &win.qh, ());
        let buffer = pool.create_buffer(0, w, h, w * 4, wl_shm::Format::Argb8888, &win.qh, ());
        pool.destroy();
        win.surface.attach(Some(&buffer), 0, 0);
        win.surface.damage_buffer(0, 0, w, h);
        win.surface.commit();
        win.mapped = true;
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for Win {
    fn event(
        win: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(caps),
        } = event
        {
            if caps.contains(wl_seat::Capability::Pointer) && win.pointer.is_none() {
                win.pointer = Some(seat.get_pointer(qh, ()));
            }
            if caps.contains(wl_seat::Capability::Keyboard) && win.keyboard.is_none() {
                win.keyboard = Some(seat.get_keyboard(qh, ()));
            }
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for Win {
    fn event(
        win: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter {
                surface_x,
                surface_y,
                ..
            }
            | wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => win.heard.push(Heard::Pointer(surface_x, surface_y)),
            wl_pointer::Event::Button { button, state, .. } => win.heard.push(Heard::Button(
                button,
                state == WEnum::Value(wl_pointer::ButtonState::Pressed),
            )),
            wl_pointer::Event::Axis { value, .. } => win.heard.push(Heard::Axis(value)),
            _ => {}
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for Win {
    fn event(
        win: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Key { key, state, .. } => win.heard.push(Heard::Key(
                key,
                state == WEnum::Value(wl_keyboard::KeyState::Pressed),
            )),
            wl_keyboard::Event::Enter { .. } => win.heard.push(Heard::KeyboardEnter),
            wl_keyboard::Event::Keymap { fd, size, .. } => {
                use std::os::unix::fs::FileExt as _;
                let mut text = vec![0; size as usize];
                let _ = std::fs::File::from(fd).read_exact_at(&mut text, 0);
                win.keymap = String::from_utf8_lossy(&text).into_owned();
                win.keymaps += 1;
            }
            wl_keyboard::Event::Modifiers {
                mods_depressed,
                mods_locked,
                ..
            } => win
                .heard
                .push(Heard::Modifiers(mods_depressed, mods_locked)),
            wl_keyboard::Event::Leave { .. } => win.heard.push(Heard::KeyboardLeave),
            _ => {}
        }
    }
}

wayland_client::delegate_noop!(Win: ignore wl_compositor::WlCompositor);
wayland_client::delegate_noop!(Win: ignore wl_surface::WlSurface);
wayland_client::delegate_noop!(Win: ignore wl_shm::WlShm);
wayland_client::delegate_noop!(Win: ignore wl_shm_pool::WlShmPool);
wayland_client::delegate_noop!(Win: ignore wl_buffer::WlBuffer);
wayland_client::delegate_noop!(Win: ignore zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1);
wayland_client::delegate_noop!(Win: ignore zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1);
wayland_client::delegate_noop!(Win: ignore zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1);
wayland_client::delegate_noop!(Win: ignore zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1);

/// Sway with one output of 1920 × 1080 at the origin, which this process
/// reaches from here on.
fn sway() -> Sway {
    let sway = Sway::start();
    sway.msg("output HEADLESS-1 resolution 1920x1080 position 0 0 scale 1");
    // SAFETY: the ignored tests run one at a time.
    unsafe {
        std::env::set_var("XDG_RUNTIME_DIR", sway.runtime.path());
        std::env::set_var("WAYLAND_DISPLAY", &sway.display);
    }
    sway
}

const KEY_A: u32 = 30;
const KEY_B: u32 = 48;
const KEY_C: u32 = 46;
const BTN_LEFT: u32 = 0x110;

#[test]
#[ignore = "needs sway; scripts/linux-wayland-tests.sh"]
fn sway_offers_what_input_without_portals_needs() {
    let _sway = sway();
    assert_eq!(
        WaylandDisplays::connect().unwrap().offers(),
        Globals {
            virtual_pointer: true,
            virtual_keyboard: true,
            layer_shell: true,
            relative_pointer: true,
            shortcuts_inhibit: true,
            data_control: true,
        }
    );
}

#[test]
#[ignore = "needs sway; scripts/linux-wayland-tests.sh"]
fn virtual_devices_put_other_devices_input_into_the_focused_window() {
    let _sway = sway();
    // The seat has no keyboard of its own here: a US keymap.
    let mut emulation = WlrEmulation::start().unwrap();
    let reach = emulation.reach();
    wait_until("the virtual devices", || reach.connected());
    let mut window = Window::open(None);

    emulation.move_to(Point::new(500.0, 400.0)).unwrap();
    window.until("the cursor on the window", |w| {
        w.heard.contains(&Heard::Pointer(500.0, 400.0))
    });
    emulation.move_by(10.0, -5.0).unwrap();
    window.until("the cursor to move", |w| {
        w.heard.contains(&Heard::Pointer(510.0, 395.0))
    });
    emulation.button(Button::Left, true).unwrap();
    emulation.button(Button::Left, false).unwrap();
    window.until("the click", |w| {
        w.heard.contains(&Heard::Button(BTN_LEFT, false))
    });
    // A key down twice goes down once.
    emulation.key(usage::A, true).unwrap();
    emulation.key(usage::A, true).unwrap();
    emulation.key(usage::A, false).unwrap();
    window.until("the key", |w| w.heard.contains(&Heard::Key(KEY_A, false)));
    // One notch towards the user scrolls down 15 pixels.
    emulation.scroll(0.0, -1.0).unwrap();
    window.until("the scroll", |w| w.heard.contains(&Heard::Axis(15.0)));
    let heard = window.take();
    let keys: Vec<&Heard> = heard
        .iter()
        .filter(|h| matches!(h, Heard::Key(..)))
        .collect();
    assert_eq!(keys, [&Heard::Key(KEY_A, true), &Heard::Key(KEY_A, false)]);
    let buttons: Vec<&Heard> = heard
        .iter()
        .filter(|h| matches!(h, Heard::Button(..)))
        .collect();
    assert_eq!(
        buttons,
        [
            &Heard::Button(BTN_LEFT, true),
            &Heard::Button(BTN_LEFT, false)
        ]
    );

    // Dropped with a key down, it lets go of it.
    emulation.key(usage::C, true).unwrap();
    window.until("the key down", |w| {
        w.heard.contains(&Heard::Key(KEY_C, true))
    });
    drop(emulation);
    window.until("the key let go of", |w| {
        w.heard.contains(&Heard::Key(KEY_C, false))
    });
}

/// The events reported, and how many the agent took in all.
struct Reported {
    rx: Receiver<InputEvent>,
    count: u64,
}

impl Reported {
    /// Waits for an event `want` says it is, taking those before it.
    fn until(&mut self, what: &str, mut want: impl FnMut(&InputEvent) -> bool) -> InputEvent {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let event = self
                .rx
                .recv_timeout(left)
                .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
            self.count += 1;
            if want(&event) {
                return event;
            }
        }
    }

    /// What comes within `wait`.
    fn during(&mut self, wait: Duration) -> Vec<InputEvent> {
        let until = Instant::now() + wait;
        let mut events = Vec::new();
        while let Ok(event) = self
            .rx
            .recv_timeout(until.saturating_duration_since(Instant::now()))
        {
            self.count += 1;
            events.push(event);
        }
        events
    }
}

fn right_exit(display: &str, bounds: Rect) -> Exit {
    Exit {
        display: display.to_owned(),
        bounds,
        edge: Edge::Right,
        at: bounds.right(),
        start: bounds.top(),
        end: bounds.bottom(),
    }
}

/// Moves the user's mouse to the right edge at `y` of an output whose right
/// side is at `right`, then pushes out.
fn push_right(window: &mut Window, right: u32, y: u32, extent: (u32, u32)) {
    window.mouse_to(right - 20, y, extent);
    window.mouse_by(40.0, 0.0);
    window.pump_for(Duration::from_millis(100));
    window.mouse_by(5.0, 0.0);
}

#[test]
#[ignore = "needs sway; scripts/linux-wayland-tests.sh"]
fn the_cursor_pushing_out_at_an_edge_hands_input_over() {
    let sway = sway();
    let extent = (1920, 1080);
    let mut window = Window::open(Some(US_KEYMAP));
    let mut capture = WlrCapture::start().unwrap();
    let (tx, rx) = mpsc::channel();
    wait_until("capture to run", || capture.start(tx.clone()).is_ok());
    let mut reported = Reported { rx, count: 0 };
    let screen = Rect::new(0.0, 0.0, 1920.0, 1080.0);
    capture.set_exits(&[right_exit("HEADLESS-1", screen)]);
    window.pump_for(Duration::from_millis(300));

    // At the edge and pushing out: the push is reported at the edge.
    push_right(&mut window, 1920, 500, extent);
    let push = reported.until("the push", |e| {
        matches!(e, InputEvent::PointerMotion { .. })
    });
    assert_eq!(
        push,
        InputEvent::PointerMotion {
            position: Point::new(1919.0, 500.0),
            delta: Point::new(1.0, 0.0),
        }
    );
    assert!(capture.grab_ready());
    // The router crosses: what the mouse and keyboard do is reported, and
    // the window hears none of it. The movement that made the push is not
    // reported again after it.
    capture.set_grabbed(true).unwrap();
    let after = reported.during(Duration::from_millis(100));
    assert!(
        !after
            .iter()
            .any(|e| matches!(e, InputEvent::PointerMotion { .. })),
        "the push reported twice: {after:?}"
    );
    window.pump_for(Duration::from_millis(100));
    window.take();
    window.mouse_by(-30.0, 10.0);
    let moved = reported.until(
        "a movement",
        |e| matches!(e, InputEvent::PointerMotion { delta, .. } if delta.x < 0.0),
    );
    assert_eq!(
        moved,
        InputEvent::PointerMotion {
            position: Point::new(960.0, 540.0),
            delta: Point::new(-30.0, 10.0),
        }
    );
    window.key(KEY_A, true);
    window.key(KEY_A, false);
    reported.until("the key up", |e| {
        *e == InputEvent::Key {
            usage: usage::A,
            pressed: false,
        }
    });
    window.pump_for(Duration::from_millis(100));
    let heard = window.take();
    assert!(
        !heard
            .iter()
            .any(|h| matches!(h, Heard::Key(..) | Heard::Pointer(..))),
        "the window heard input meant for another device: {heard:?}"
    );

    // The cursor comes back where the agent puts it, and the window has the
    // keyboard and the pointer again.
    capture.set_grabbed(false).unwrap();
    capture.warp(Point::new(1900.0, 300.0)).unwrap();
    window.until("the cursor back on the window", |w| {
        w.heard.contains(&Heard::Pointer(1900.0, 300.0))
    });
    window.key(KEY_B, true);
    window.key(KEY_B, false);
    window.until("the window's key", |w| {
        w.heard.contains(&Heard::Key(KEY_B, false))
    });
    assert!(!capture.grab_ready());
    // Coming back as asked is no news to the router.
    assert!(
        !reported
            .during(Duration::from_millis(200))
            .contains(&InputEvent::CaptureEnded)
    );

    // Pushed again, but the router does not cross: let go of, which the
    // router hears, with the cursor where the user left it; moving away,
    // it is the window's again.
    window.take();
    push_right(&mut window, 1920, 700, extent);
    reported.until("the push", |e| {
        matches!(e, InputEvent::PointerMotion { .. })
    });
    capture.edge_wait(reported.count, false);
    reported.until("capture to end", |e| *e == InputEvent::CaptureEnded);
    assert!(!capture.grab_ready());
    window.mouse_by(-50.0, 0.0);
    window.until("the cursor back on the window", |w| {
        w.heard.contains(&Heard::Pointer(1869.0, 700.0))
    });
    // Moving off the edge while pushing on it lets go too.
    window.take();
    push_right(&mut window, 1920, 600, extent);
    reported.until("the push", |e| {
        matches!(e, InputEvent::PointerMotion { .. })
    });
    window.mouse_by(-50.0, 0.0);
    reported.until("capture to end", |e| *e == InputEvent::CaptureEnded);
    window.until("the cursor back on the window", |w| {
        w.heard.contains(&Heard::Pointer(1869.0, 600.0))
    });

    // A jump here moves the cursor without input handed over.
    assert!(capture.can_warp());
    capture.warp(Point::new(200.0, 200.0)).unwrap();
    window.until("the cursor where it jumped", |w| {
        w.heard.contains(&Heard::Pointer(200.0, 200.0))
    });

    // Nowhere to leave to: pushing out hands nothing over.
    capture.set_exits(&[]);
    window.pump_for(Duration::from_millis(300));
    push_right(&mut window, 1920, 500, extent);
    assert!(
        !reported
            .during(Duration::from_millis(300))
            .iter()
            .any(|e| matches!(e, InputEvent::PointerMotion { .. })),
        "a push with nowhere to go"
    );

    // Another output to the right, with the exit on its far side: its
    // surface is put there as the outputs change.
    sway.msg("create_output");
    sway.msg("output HEADLESS-2 resolution 1280x720 position 1920 0 scale 1");
    window.pump_for(Duration::from_millis(300));
    let second = Rect::new(1920.0, 0.0, 1280.0, 720.0);
    capture.set_exits(&[right_exit("HEADLESS-2", second)]);
    window.pump_for(Duration::from_millis(300));
    push_right(&mut window, 3200, 300, (3200, 1080));
    let push = reported.until("the push on the second output", |e| {
        matches!(e, InputEvent::PointerMotion { .. })
    });
    assert_eq!(
        push,
        InputEvent::PointerMotion {
            position: Point::new(3199.0, 300.0),
            delta: Point::new(1.0, 0.0),
        }
    );

    // Dropped while it holds input: let go of, and the window has the
    // keyboard again once the cursor is back on it (sway's focus follows
    // the cursor; the second output has no window).
    drop(capture);
    window.mouse_to(500, 500, (3200, 1080));
    window.pump_for(Duration::from_millis(200));
    window.take();
    window.key(KEY_C, true);
    window.key(KEY_C, false);
    window.until("the window's key after capture went", |w| {
        w.heard.contains(&Heard::Key(KEY_C, false))
    });
}

#[test]
#[ignore = "needs sway; scripts/linux-wayland-tests.sh"]
fn a_modifier_held_as_the_cursor_pushes_out_is_reported_first() {
    let _sway = sway();
    let extent = (1920, 1080);
    let mut window = Window::open(Some(US_KEYMAP));
    let mut capture = WlrCapture::start().unwrap();
    let (tx, rx) = mpsc::channel();
    wait_until("capture to run", || capture.start(tx.clone()).is_ok());
    let mut reported = Reported { rx, count: 0 };
    capture.set_exits(&[right_exit(
        "HEADLESS-1",
        Rect::new(0.0, 0.0, 1920.0, 1080.0),
    )]);
    window.pump_for(Duration::from_millis(300));

    const KEY_LEFTSHIFT: u32 = 42;
    window.key(KEY_LEFTSHIFT, true);
    push_right(&mut window, 1920, 500, extent);
    let first = reported.until("the first event", |_| true);
    assert_eq!(
        first,
        InputEvent::Key {
            usage: usage::LEFT_SHIFT,
            pressed: true,
        }
    );
    let push = reported.until("the push", |_| true);
    assert!(matches!(push, InputEvent::PointerMotion { .. }), "{push:?}");
    // Let go of, Shift is reported up.
    capture.edge_wait(reported.count, false);
    let after = reported.during(Duration::from_millis(500));
    assert!(
        after.contains(&InputEvent::Key {
            usage: usage::LEFT_SHIFT,
            pressed: false,
        }),
        "{after:?}"
    );
    assert!(after.contains(&InputEvent::CaptureEnded), "{after:?}");
    window.key(KEY_LEFTSHIFT, false);
}

#[test]
#[ignore = "needs sway; scripts/linux-wayland-tests.sh"]
fn input_without_portals_comes_back_with_the_compositor() {
    let sway = sway();
    let mut emulation = WlrEmulation::start().unwrap();
    let reach = emulation.reach();
    let mut capture = WlrCapture::start().unwrap();
    let (tx, rx) = mpsc::channel();
    wait_until("both to run", || {
        reach.connected() && capture.start(tx.clone()).is_ok()
    });
    let screen = Rect::new(0.0, 0.0, 1920.0, 1080.0);
    capture.set_exits(&[right_exit("HEADLESS-1", screen)]);

    // The compositor goes, as at a logout while the service lives on:
    // nothing reaches it meanwhile, and capture says it stopped.
    let runtime = sway.runtime.clone();
    drop(sway);
    wait_until("both to see it gone", || {
        !reach.connected() && !capture.capturing()
    });
    assert!(emulation.key(usage::A, true).is_err());

    // Back under the same name: both connect again, capture starts again
    // as the agent asks, and the exits it was told hold. (The sway that
    // went left its IPC socket behind.)
    for entry in std::fs::read_dir(runtime.path()).unwrap().flatten() {
        if entry.file_name().to_string_lossy().contains("sway-ipc") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    let sway = Sway::start_in(runtime.clone());
    wait_until("sway's IPC socket", || {
        std::fs::read_dir(runtime.path())
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().contains("sway-ipc"))
    });
    sway.msg("output HEADLESS-1 resolution 1920x1080 position 0 0 scale 1");
    wait_until("the virtual devices again", || reach.connected());
    wait_until("capture again", || capture.start(tx.clone()).is_ok());
    let mut window = Window::open(Some(US_KEYMAP));
    window.pump_for(Duration::from_millis(300));
    emulation.move_to(Point::new(300.0, 300.0)).unwrap();
    window.until("injected input on the new compositor", |w| {
        w.heard.contains(&Heard::Pointer(300.0, 300.0))
    });
    let mut reported = Reported { rx, count: 0 };
    push_right(&mut window, 1920, 400, (1920, 1080));
    reported.until(
        "a push on the new compositor",
        |e| matches!(e, InputEvent::PointerMotion { position, .. } if position.y == 400.0),
    );
}

/// A German keymap, which has keys the US one has not.
const GERMAN: &str = "xkb_keymap {\n\
    \txkb_keycodes { include \"evdev+aliases(qwertz)\" };\n\
    \txkb_types { include \"complete\" };\n\
    \txkb_compat { include \"complete\" };\n\
    \txkb_symbols { include \"pc+de+inet(evdev)\" };\n\
    \txkb_geometry { include \"pc(pc105)\" };\n\
    };\n";

#[test]
#[ignore = "needs sway; scripts/linux-wayland-tests.sh"]
fn virtual_keys_type_with_the_seats_own_keymap() {
    let _sway = sway();
    // The user's keyboard is German.
    let mut window = Window::open(Some(GERMAN));
    window.key(KEY_A, true);
    window.key(KEY_A, false);
    window.until("the German keymap", |w| w.keymap.contains("adiaeresis"));
    let mut emulation = WlrEmulation::start().unwrap();
    let reach = emulation.reach();
    wait_until("the virtual devices", || reach.connected());
    // As its keys come, the seat's keymap is the virtual keyboard's, which
    // is the user's own.
    window.take();
    window.win.keymap.clear();
    emulation.key(usage::A, true).unwrap();
    emulation.key(usage::A, false).unwrap();
    window.until("the virtual keyboard's keys", |w| {
        w.heard.contains(&Heard::Key(KEY_A, false))
    });
    window.until("a keymap from the virtual keyboard", |w| {
        !w.keymap.is_empty()
    });
    assert!(
        window.win.keymap.contains("adiaeresis"),
        "the virtual keyboard did not type with the seat's keymap"
    );
}

const SHIFT: u32 = 1 << 0;
const LOCK: u32 = 1 << 1;
const CONTROL: u32 = 1 << 2;

/// The modifiers the window last heard before the `nth` time it heard
/// `heard` (from 0), if it did.
fn modifiers_at(heard: &[Heard], nth: usize, at: &Heard) -> Option<(u32, u32)> {
    let index = heard
        .iter()
        .enumerate()
        .filter(|(_, h)| *h == at)
        .nth(nth)?
        .0;
    heard[..index].iter().rev().find_map(|h| match h {
        Heard::Modifiers(depressed, locked) => Some((*depressed, *locked)),
        _ => None,
    })
}

#[test]
#[ignore = "needs sway; scripts/linux-wayland-tests.sh"]
fn virtual_keys_carry_their_modifiers_and_the_keymap_settles() {
    let _sway = sway();
    let mut emulation = WlrEmulation::start().unwrap();
    let reach = emulation.reach();
    wait_until("the virtual devices", || reach.connected());
    let mut window = Window::open(None);
    emulation.move_to(Point::new(500.0, 400.0)).unwrap();
    window.until("the cursor on the window", |w| {
        w.heard.contains(&Heard::Pointer(500.0, 400.0))
    });

    // Shift+A arrives shifted, and Shift let go of no longer holds.
    for (usage, pressed) in [
        (usage::LEFT_SHIFT, true),
        (usage::A, true),
        (usage::A, false),
        (usage::LEFT_SHIFT, false),
        (usage::A, true),
        (usage::A, false),
    ] {
        emulation.key(usage, pressed).unwrap();
    }
    window.until("the keys", |w| {
        w.heard
            .iter()
            .filter(|h| **h == Heard::Key(KEY_A, false))
            .count()
            == 2
    });
    let heard = window.take();
    assert_eq!(
        modifiers_at(&heard, 0, &Heard::Key(KEY_A, true)),
        Some((SHIFT, 0)),
        "{heard:?}"
    );
    assert_eq!(
        modifiers_at(&heard, 1, &Heard::Key(KEY_A, true)),
        Some((0, 0)),
        "{heard:?}"
    );

    // Ctrl held shows as Control; Caps Lock locks, until pressed again.
    emulation.key(usage::LEFT_CONTROL, true).unwrap();
    window.until("Control held", |w| {
        w.heard.contains(&Heard::Modifiers(CONTROL, 0))
    });
    emulation.key(usage::LEFT_CONTROL, false).unwrap();
    emulation.key(usage::CAPS_LOCK, true).unwrap();
    emulation.key(usage::CAPS_LOCK, false).unwrap();
    window.until("Caps Lock on", |w| {
        w.heard.contains(&Heard::Modifiers(0, LOCK))
    });
    window.take();
    emulation.key(usage::CAPS_LOCK, true).unwrap();
    emulation.key(usage::CAPS_LOCK, false).unwrap();
    window.until("Caps Lock off", |w| {
        w.heard.contains(&Heard::Modifiers(0, 0))
    });

    // The keymap the seat sends as the virtual keyboard types is not given
    // to it again and again: after a moment, no more come.
    window.pump_for(Duration::from_millis(500));
    let keymaps = window.win.keymaps;
    window.pump_for(Duration::from_secs(1));
    assert_eq!(
        window.win.keymaps, keymaps,
        "the seat keeps sending keymaps"
    );
    assert!(keymaps <= 3, "{keymaps} keymaps");
}

/// A session lock of the test's own, as a screen locker makes: a lock
/// surface over each output takes the keyboard and the pointer.
struct Locker {
    queue: EventQueue<LockState>,
    state: LockState,
    lock: ext_session_lock_v1::ExtSessionLockV1,
}

struct LockState {
    qh: QueueHandle<LockState>,
    shm: wl_shm::WlShm,
    locked: bool,
}

impl Locker {
    fn lock() -> Self {
        let conn = Connection::connect_to_env().unwrap();
        let (globals, mut queue) = registry_queue_init::<LockState>(&conn).unwrap();
        let qh = queue.handle();
        let compositor: wl_compositor::WlCompositor = globals.bind(&qh, 1..=4, ()).unwrap();
        let manager: ext_session_lock_manager_v1::ExtSessionLockManagerV1 =
            globals.bind(&qh, 1..=1, ()).unwrap();
        let shm = globals.bind(&qh, 1..=1, ()).unwrap();
        let outputs: Vec<wl_output::WlOutput> = globals.contents().with_list(|list| {
            list.iter()
                .filter(|g| g.interface == "wl_output")
                .map(|g| globals.registry().bind(g.name, 1, &qh, ()))
                .collect()
        });
        let lock = manager.lock(&qh, ());
        for output in &outputs {
            let surface = compositor.create_surface(&qh, ());
            lock.get_lock_surface(&surface, output, &qh, surface.clone());
        }
        let mut state = LockState {
            qh,
            shm,
            locked: false,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while !state.locked {
            assert!(Instant::now() < deadline, "the session did not lock");
            queue.roundtrip(&mut state).unwrap();
        }
        // The lock surfaces shown.
        queue.roundtrip(&mut state).unwrap();
        Self { queue, state, lock }
    }
}

impl Drop for Locker {
    fn drop(&mut self) {
        self.lock.unlock_and_destroy();
        let _ = self.queue.roundtrip(&mut self.state);
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for LockState {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ext_session_lock_v1::ExtSessionLockV1, ()> for LockState {
    fn event(
        state: &mut Self,
        _: &ext_session_lock_v1::ExtSessionLockV1,
        event: ext_session_lock_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_session_lock_v1::Event::Locked => state.locked = true,
            ext_session_lock_v1::Event::Finished => panic!("sway would not lock"),
            _ => {}
        }
    }
}

impl Dispatch<ext_session_lock_surface_v1::ExtSessionLockSurfaceV1, wl_surface::WlSurface>
    for LockState
{
    fn event(
        state: &mut Self,
        lock_surface: &ext_session_lock_surface_v1::ExtSessionLockSurfaceV1,
        event: ext_session_lock_surface_v1::Event,
        surface: &wl_surface::WlSurface,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let ext_session_lock_surface_v1::Event::Configure {
            serial,
            width,
            height,
        } = event
        else {
            return;
        };
        lock_surface.ack_configure(serial);
        let (w, h) = (width as i32, height as i32);
        let fd = wlr::memfd("test-lock", (w * h * 4) as u64).unwrap();
        let pool = state.shm.create_pool(fd.as_fd(), w * h * 4, &state.qh, ());
        let buffer = pool.create_buffer(0, w, h, w * 4, wl_shm::Format::Argb8888, &state.qh, ());
        pool.destroy();
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, w, h);
        surface.commit();
    }
}

wayland_client::delegate_noop!(LockState: ignore wl_compositor::WlCompositor);
wayland_client::delegate_noop!(LockState: ignore wl_surface::WlSurface);
wayland_client::delegate_noop!(LockState: ignore wl_shm::WlShm);
wayland_client::delegate_noop!(LockState: ignore wl_shm_pool::WlShmPool);
wayland_client::delegate_noop!(LockState: ignore wl_buffer::WlBuffer);
wayland_client::delegate_noop!(LockState: ignore wl_output::WlOutput);
wayland_client::delegate_noop!(LockState: ignore ext_session_lock_manager_v1::ExtSessionLockManagerV1);

/// Sway with one output, a window with the user's mouse and keyboard,
/// capture with an exit on the output's right edge, and input handed over
/// there and grabbed.
fn grabbed_at_the_right_edge() -> (Sway, Window, WlrCapture, Reported) {
    let sway = sway();
    let mut window = Window::open(Some(US_KEYMAP));
    let mut capture = WlrCapture::start().unwrap();
    let (tx, rx) = mpsc::channel();
    wait_until("capture to run", || capture.start(tx.clone()).is_ok());
    let mut reported = Reported { rx, count: 0 };
    capture.set_exits(&[right_exit(
        "HEADLESS-1",
        Rect::new(0.0, 0.0, 1920.0, 1080.0),
    )]);
    window.pump_for(Duration::from_millis(300));
    push_right(&mut window, 1920, 500, (1920, 1080));
    reported.until("the push", |e| {
        matches!(e, InputEvent::PointerMotion { .. })
    });
    capture.set_grabbed(true).unwrap();
    window.pump_for(Duration::from_millis(200));
    (sway, window, capture, reported)
}

#[test]
#[ignore = "needs sway; scripts/linux-wayland-tests.sh"]
fn what_is_down_goes_up_as_a_screen_lock_takes_the_input() {
    let (_sway, mut window, _capture, mut reported) = grabbed_at_the_right_edge();
    window.key(KEY_A, true);
    window.button(BTN_LEFT, true);
    reported.until("the button down", |e| {
        *e == InputEvent::Button {
            button: Button::Left,
            pressed: true,
        }
    });
    // The lock takes the keyboard and the pointer from capture's surfaces,
    // where the ups would never come: they are reported now.
    let locker = Locker::lock();
    let after = reported.during(Duration::from_millis(500));
    for up in [
        InputEvent::Key {
            usage: usage::A,
            pressed: false,
        },
        InputEvent::Button {
            button: Button::Left,
            pressed: false,
        },
    ] {
        assert!(after.contains(&up), "{up:?} not in {after:?}");
    }
    window.key(KEY_A, false);
    window.button(BTN_LEFT, false);
    drop(locker);
}

#[test]
#[ignore = "needs sway; scripts/linux-wayland-tests.sh"]
fn an_output_added_while_grabbed_is_covered_at_once() {
    let (sway, mut window, _capture, mut reported) = grabbed_at_the_right_edge();
    // Shift, held all along, stays down there: sway takes the keyboard
    // from the strip a moment as the output comes, which reports it up,
    // and gives it back with Shift listed down.
    const KEY_LEFTSHIFT: u32 = 42;
    let shift = |pressed| InputEvent::Key {
        usage: usage::LEFT_SHIFT,
        pressed,
    };
    window.key(KEY_LEFTSHIFT, true);
    reported.until("Shift down", |e| *e == shift(true));
    sway.msg("create_output");
    sway.msg("output HEADLESS-2 resolution 1280x720 position 1920 0 scale 1");
    window.pump_for(Duration::from_millis(300));
    let before = reported.during(Duration::from_millis(100));
    // The cursor, hidden, goes on onto the new output as the mouse moves:
    // every movement still comes here.
    for _ in 0..8 {
        window.mouse_by(50.0, 0.0);
    }
    let all = reported.during(Duration::from_millis(500));
    let moved: Vec<&InputEvent> = all
        .iter()
        .filter(|e| matches!(e, InputEvent::PointerMotion { .. }))
        .collect();
    assert_eq!(moved.len(), 8, "before {before:?}, then {all:?}");
    let shifts: Vec<&InputEvent> = (before.iter().chain(&all))
        .filter(|e| matches!(e, InputEvent::Key { usage: u, .. } if *u == usage::LEFT_SHIFT))
        .collect();
    assert!(
        shifts.last().is_none_or(|e| **e == shift(true)),
        "before {before:?}, then {all:?}"
    );
    window.key(KEY_A, true);
    reported.until("A down", |e| {
        *e == InputEvent::Key {
            usage: usage::A,
            pressed: true,
        }
    });
    window.key(KEY_A, false);
    window.key(KEY_LEFTSHIFT, false);
    reported.until("Shift up", |e| *e == shift(false));
    // A button down over the new output's cover goes up as the output
    // goes, the cover with it.
    window.button(BTN_LEFT, true);
    reported.until("the button down", |e| {
        *e == InputEvent::Button {
            button: Button::Left,
            pressed: true,
        }
    });
    sway.msg("output HEADLESS-2 unplug");
    reported.until("the button up", |e| {
        *e == InputEvent::Button {
            button: Button::Left,
            pressed: false,
        }
    });
    window.button(BTN_LEFT, false);
}

#[test]
#[ignore = "needs sway; scripts/linux-wayland-tests.sh"]
fn our_own_warp_across_a_strip_is_no_push() {
    let sway = sway();
    // The keyboard stays with the window wherever the cursor goes, so only
    // a push would take it.
    sway.msg("focus_follows_mouse no");
    sway.msg("create_output");
    sway.msg("output HEADLESS-2 resolution 1280x720 position 1920 0 scale 1");
    let extent = (3200, 1080);
    let mut window = Window::open(Some(US_KEYMAP));
    let mut capture = WlrCapture::start().unwrap();
    let (tx, rx) = mpsc::channel();
    wait_until("capture to run", || capture.start(tx.clone()).is_ok());
    let mut reported = Reported { rx, count: 0 };
    // An exit on the first output's right edge, with the second beyond it.
    capture.set_exits(&[right_exit(
        "HEADLESS-1",
        Rect::new(0.0, 0.0, 1920.0, 1080.0),
    )]);
    window.pump_for(Duration::from_millis(300));
    // The cursor on the strip, from inside, which is no push.
    window.mouse_to(1919, 500, extent);
    window.pump_for(Duration::from_millis(200));
    assert!(reported.during(Duration::from_millis(100)).is_empty());
    window.take();
    // A jump to the second output: the warp moves out across the strip,
    // which the compositor tells the strip as relative motion. Taken for a
    // push, the strip would take the keyboard, a moment at least.
    capture.warp(Point::new(2500.0, 500.0)).unwrap();
    let after = reported.during(Duration::from_millis(500));
    window.pump_for(Duration::from_millis(100));
    assert!(
        !after
            .iter()
            .any(|e| matches!(e, InputEvent::PointerMotion { .. })),
        "our own warp taken for a push: {after:?}"
    );
    let heard = window.take();
    assert!(
        !heard.contains(&Heard::KeyboardLeave),
        "our own warp took the keyboard: {heard:?}"
    );
    assert!(!capture.grab_ready());
}

#[test]
#[ignore = "needs sway; scripts/linux-wayland-tests.sh"]
fn a_key_held_at_the_edge_repeats_once_grabbed() {
    let _sway = sway();
    let mut window = Window::open(Some(US_KEYMAP));
    let mut capture = WlrCapture::start().unwrap();
    let (tx, rx) = mpsc::channel();
    wait_until("capture to run", || capture.start(tx.clone()).is_ok());
    let mut reported = Reported { rx, count: 0 };
    capture.set_exits(&[right_exit(
        "HEADLESS-1",
        Rect::new(0.0, 0.0, 1920.0, 1080.0),
    )]);
    capture.watch_hotkeys(&[Hotkey::new(Modifiers::CONTROL_ALT_SHIFT, usage::L)]);
    window.pump_for(Duration::from_millis(300));
    push_right(&mut window, 1920, 500, (1920, 1080));
    reported.until("the push", |e| {
        matches!(e, InputEvent::PointerMotion { .. })
    });
    // Held at the edge, keys that make a hotkey keep input held: L goes
    // down there, and is still down as the router grabs.
    const KEY_LEFTCTRL: u32 = 29;
    const KEY_LEFTALT: u32 = 56;
    const KEY_LEFTSHIFT: u32 = 42;
    const KEY_L: u32 = 38;
    for key in [KEY_LEFTCTRL, KEY_LEFTALT, KEY_LEFTSHIFT, KEY_L] {
        window.key(key, true);
    }
    let l_down = InputEvent::Key {
        usage: usage::L,
        pressed: true,
    };
    reported.until("L down", |e| *e == l_down);
    capture.set_grabbed(true).unwrap();
    // It repeats at the seat's pace, from when it went down.
    reported.until("L repeating", |e| *e == l_down);
    for key in [KEY_L, KEY_LEFTSHIFT, KEY_LEFTALT, KEY_LEFTCTRL] {
        window.key(key, false);
    }
}

#[test]
#[ignore = "needs sway; scripts/linux-wayland-tests.sh"]
fn a_button_held_across_the_outputs_stays_down() {
    let (sway, mut window, _capture, mut reported) = grabbed_at_the_right_edge();
    sway.msg("create_output");
    sway.msg("output HEADLESS-2 resolution 1280x720 position 1920 0 scale 1");
    window.pump_for(Duration::from_millis(300));
    reported.during(Duration::from_millis(100));
    // A drag on the other device: the button goes down, and the hidden
    // cursor moves on from the first output's cover to the second's.
    window.button(BTN_LEFT, true);
    reported.until("the button down", |e| {
        *e == InputEvent::Button {
            button: Button::Left,
            pressed: true,
        }
    });
    for _ in 0..8 {
        window.mouse_by(50.0, 0.0);
    }
    for _ in 0..8 {
        window.mouse_by(-50.0, 0.0);
    }
    let during = reported.during(Duration::from_millis(500));
    assert!(
        !during
            .iter()
            .any(|e| matches!(e, InputEvent::Button { pressed: false, .. })),
        "let go of mid-drag: {during:?}"
    );
    // Back on the first output's cover, the second output goes, and its
    // cover with it: the button is still down.
    sway.msg("output HEADLESS-2 unplug");
    window.pump_for(Duration::from_millis(300));
    let unplugged = reported.during(Duration::from_millis(200));
    assert!(
        !unplugged
            .iter()
            .any(|e| matches!(e, InputEvent::Button { pressed: false, .. })),
        "let go of as another output went: {unplugged:?}"
    );
    // Up as the button comes up, and not before.
    window.button(BTN_LEFT, false);
    reported.until("the button up", |e| {
        *e == InputEvent::Button {
            button: Button::Left,
            pressed: false,
        }
    });
}
