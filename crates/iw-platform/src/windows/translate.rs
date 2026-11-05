//! Translation between low-level hook data, InputWeave's input events and
//! `SendInput` requests, kept free of FFI so it can be tested anywhere.

#![cfg_attr(not(windows), allow(dead_code))]

use std::sync::Arc;

use iw_core::HidUsage;
use iw_core::geometry::Point;
use iw_core::input::{Button, InputEvent};
use iw_keymap::{ScanCode, usage};

use super::monitors::{DesktopMap, Pixel};

/// Win32 values the translation depends on. The FFI layer reads hook data
/// with the `windows` crate; these copies keep this module portable, and a
/// Windows-only test checks them against the crate.
pub(crate) mod win32 {
    pub const WM_MOUSEMOVE: u32 = 0x0200;
    pub const WM_LBUTTONDOWN: u32 = 0x0201;
    pub const WM_LBUTTONUP: u32 = 0x0202;
    pub const WM_RBUTTONDOWN: u32 = 0x0204;
    pub const WM_RBUTTONUP: u32 = 0x0205;
    pub const WM_MBUTTONDOWN: u32 = 0x0207;
    pub const WM_MBUTTONUP: u32 = 0x0208;
    pub const WM_MOUSEWHEEL: u32 = 0x020A;
    pub const WM_XBUTTONDOWN: u32 = 0x020B;
    pub const WM_XBUTTONUP: u32 = 0x020C;
    pub const WM_MOUSEHWHEEL: u32 = 0x020E;
    pub const XBUTTON1: u16 = 0x0001;
    pub const XBUTTON2: u16 = 0x0002;
    pub const WHEEL_DELTA: f64 = 120.0;
    pub const VK_CANCEL: u32 = 0x03;
    pub const VK_PAUSE: u32 = 0x13;
    pub const VK_NUMLOCK: u32 = 0x90;
}

/// How far, in logical units, a pointer held against the edge of the
/// desktop is reported to push past it. One logical unit is at least one
/// pixel at every Windows scale.
const EDGE_PUSH: f64 = 1.0;

/// The fields of a `KBDLLHOOKSTRUCT` that capture looks at, for an event
/// not flagged as injected.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RawKey {
    pub vk: u32,
    pub scan: u32,
    /// `LLKHF_EXTENDED`: the key was sent with an `0xE0` prefix.
    pub extended: bool,
    /// `LLKHF_UP`.
    pub up: bool,
}

/// A key event from the keyboard hook. `None` for keys without a HID usage
/// and for the fake keys Windows generates itself (scan codes above 0xFF,
/// such as the left Control that comes with AltGr on some layouts).
pub(crate) fn key_event(key: RawKey) -> Option<InputEvent> {
    let pressed = !key.up;
    let usage = match key.vk {
        // Pause arrives as scan 0x45 without the extended flag, Ctrl+Pause
        // as Break (VK_CANCEL, extended 0x46); the virtual key tells them
        // apart from NumLock and ScrollLock.
        win32::VK_PAUSE => usage::PAUSE,
        win32::VK_CANCEL if key.scan == 0x46 && key.extended => usage::PAUSE,
        win32::VK_NUMLOCK => usage::NUM_LOCK,
        _ => {
            let code = u8::try_from(key.scan).ok().filter(|&c| c != 0)?;
            HidUsage::from_windows(ScanCode {
                code,
                extended: key.extended,
            })?
        }
    };
    Some(InputEvent::Key { usage, pressed })
}

/// A mouse hook message, decoded.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum RawMouse {
    /// The cursor would move to `to`.
    Move {
        to: Pixel,
    },
    Button {
        button: Button,
        pressed: bool,
    },
    /// Wheel movement in notches, positive away from the user or to the
    /// right.
    Wheel {
        horizontal: bool,
        notches: f64,
    },
}

/// Decodes a mouse hook message (`wParam`) and the `mouseData` and `pt` of
/// its `MSLLHOOKSTRUCT`. `None` for messages capture ignores.
pub(crate) fn decode_mouse(message: u32, mouse_data: u32, pt: Pixel) -> Option<RawMouse> {
    use win32::*;
    let high = (mouse_data >> 16) as u16;
    let button = |button, pressed| Some(RawMouse::Button { button, pressed });
    match message {
        WM_MOUSEMOVE => Some(RawMouse::Move { to: pt }),
        WM_LBUTTONDOWN => button(Button::Left, true),
        WM_LBUTTONUP => button(Button::Left, false),
        WM_RBUTTONDOWN => button(Button::Right, true),
        WM_RBUTTONUP => button(Button::Right, false),
        WM_MBUTTONDOWN => button(Button::Middle, true),
        WM_MBUTTONUP => button(Button::Middle, false),
        WM_XBUTTONDOWN | WM_XBUTTONUP => {
            let pressed = message == WM_XBUTTONDOWN;
            match high {
                XBUTTON1 => button(Button::Back, pressed),
                XBUTTON2 => button(Button::Forward, pressed),
                _ => None,
            }
        }
        WM_MOUSEWHEEL | WM_MOUSEHWHEEL => Some(RawMouse::Wheel {
            horizontal: message == WM_MOUSEHWHEEL,
            notches: f64::from(high as i16) / WHEEL_DELTA,
        }),
        _ => None,
    }
}

/// Turns mouse hook messages into input events.
#[derive(Debug, Default)]
pub(crate) struct MouseTranslator {
    pub map: Arc<DesktopMap>,
    /// Whether input is grabbed: the hook swallows motion, so the cursor
    /// stays where it is.
    pub grabbed: bool,
    /// Where the cursor was last warped to. Windows may report the warp
    /// itself as motion; that one event is dropped.
    pub warped_to: Option<Pixel>,
}

impl MouseTranslator {
    /// Translates one mouse message. `cursor` is where the cursor was
    /// before the message: the hook runs before Windows moves it.
    pub(crate) fn translate(&mut self, raw: RawMouse, cursor: Pixel) -> Option<InputEvent> {
        match raw {
            RawMouse::Move { to } => self.motion(to, cursor),
            RawMouse::Button { button, pressed } => Some(InputEvent::Button { button, pressed }),
            RawMouse::Wheel {
                horizontal,
                notches,
            } => {
                if notches == 0.0 {
                    return None;
                }
                let (dx, dy) = if horizontal {
                    (notches, 0.0)
                } else {
                    (0.0, notches)
                };
                Some(InputEvent::Scroll { dx, dy })
            }
        }
    }

    fn motion(&mut self, to: Pixel, cursor: Pixel) -> Option<InputEvent> {
        if let Some(target) = self.warped_to.take()
            && to == target
            && cursor != target
        {
            return None;
        }
        let mut delta = self.map.logical_delta(cursor, to);
        let position = if self.grabbed {
            // The swallowed motion leaves the cursor in place; `to` is only
            // where it would have gone.
            self.map.to_logical(cursor)?
        } else {
            if to == cursor {
                delta = self.push_past_edge(cursor);
            }
            self.map.to_logical(to)?
        };
        if delta == Point::default() {
            return None;
        }
        Some(InputEvent::PointerMotion { position, delta })
    }

    /// The motion to report for an event that did not move a cursor resting
    /// on the edge of the desktop: Windows keeps the cursor on the desktop,
    /// so a pointer pushed against an edge arrives as motion to where it
    /// already is. Reporting a push past each edge it rests on lets the
    /// router cross to a device beyond that edge.
    fn push_past_edge(&self, cursor: Pixel) -> Point {
        let edges = self.map.outer_edges(cursor);
        let x = match (edges.left, edges.right) {
            (true, false) => -EDGE_PUSH,
            (false, true) => EDGE_PUSH,
            _ => 0.0,
        };
        let y = match (edges.top, edges.bottom) {
            (true, false) => -EDGE_PUSH,
            (false, true) => EDGE_PUSH,
            _ => 0.0,
        };
        Point::new(x, y)
    }
}

/// One `INPUT` for `SendInput`, in portable terms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Synth {
    /// An absolute move over the whole virtual desktop, in `SendInput`'s
    /// normalized 0..=65535 units.
    MoveTo {
        x: i32,
        y: i32,
    },
    Button {
        button: Button,
        pressed: bool,
    },
    /// A key by scan code.
    Key {
        scan: ScanCode,
        pressed: bool,
    },
    /// Pause, which has no scan code `SendInput` accepts and goes by
    /// virtual key.
    Pause {
        pressed: bool,
    },
    /// A relative move by so many pixels, as a mouse moving: Windows
    /// applies the pointer speed and acceleration to the cursor, and raw
    /// input, which games read, sees these very counts.
    MoveBy {
        dx: i32,
        dy: i32,
    },
    /// Wheel movement in `WHEEL_DELTA` units (120 per notch).
    Wheel {
        horizontal: bool,
        delta: i32,
    },
    /// Ctrl+Alt+Del, which no injected key reaches: the service sends it.
    SecureAttention,
}

/// The value of an absolute `SendInput` move along one axis of the virtual
/// desktop (`MOUSEEVENTF_VIRTUALDESK`) that lands on `pixel`, where the
/// axis starts at `origin` and is `size` pixels long.
///
/// Windows maps 0..=65535 onto the desktop without documenting how it
/// rounds. This picks the middle of the values that map onto the pixel, so
/// it lands there whether Windows truncates `n * size / 65536` or rounds
/// `n * (size - 1) / 65535`; both are checked for every pixel of common
/// desktop widths in the tests.
pub(crate) fn normalize(pixel: i32, origin: i32, size: i32) -> i32 {
    let size = i64::from(size.max(1));
    let offset = (i64::from(pixel) - i64::from(origin)).clamp(0, size - 1);
    ((2 * offset + 1) * 65536 / (2 * size)).clamp(0, 65535) as i32
}

/// What injection remembers between calls: which keys it holds and the
/// wheel movement too small to send yet.
#[derive(Debug, Default)]
pub(crate) struct Injector {
    held: Vec<HidUsage>,
    /// Delete went out as Ctrl+Alt+Del, so its release is not injected.
    delete_was_sas: bool,
    /// Wheel movement below one `WHEEL_DELTA` unit, horizontal and
    /// vertical.
    wheel_remainder: (f64, f64),
}

/// Relative movements in physical pixels, on the injection thread, which
/// knows where the cursor is.
#[derive(Debug, Default)]
pub(crate) struct RelativeMover {
    /// Movement below one pixel, horizontal and vertical.
    remainder: (f64, f64),
}

impl RelativeMover {
    /// A relative move by (`dx`, `dy`) logical units on a monitor of
    /// `scale`: so many physical pixels, fractions adding up across calls.
    /// `None` while less than a pixel has added up.
    pub(crate) fn move_by(&mut self, scale: f64, dx: f64, dy: f64) -> Option<Synth> {
        if !dx.is_finite() || !dy.is_finite() || !scale.is_finite() || scale <= 0.0 {
            return None;
        }
        let x = dx * scale + self.remainder.0;
        let y = dy * scale + self.remainder.1;
        let (whole_x, whole_y) = (x.trunc(), y.trunc());
        self.remainder = (x - whole_x, y - whole_y);
        // SendInput takes a LONG; a movement beyond it is no mouse's.
        let pixels = |v: f64| v.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;
        (whole_x != 0.0 || whole_y != 0.0).then(|| Synth::MoveBy {
            dx: pixels(whole_x),
            dy: pixels(whole_y),
        })
    }
}

impl Injector {
    pub(crate) fn move_to(&self, map: &DesktopMap, position: Point) -> Option<Synth> {
        let pixel = map.to_physical(position)?;
        let screen = map.virtual_screen()?;
        Some(Synth::MoveTo {
            x: normalize(pixel.x, screen.left, screen.width()),
            y: normalize(pixel.y, screen.top, screen.height()),
        })
    }

    /// `Ok(None)` when there is nothing to inject. Fails for keys Windows
    /// has no scan code for.
    pub(crate) fn key(&mut self, key: HidUsage, pressed: bool) -> Result<Option<Synth>, HidUsage> {
        if pressed {
            if !self.held.contains(&key) {
                self.held.push(key);
            }
        } else {
            self.held.retain(|k| *k != key);
        }
        if key == usage::DELETE {
            if pressed && self.holds_ctrl_and_alt() {
                self.delete_was_sas = true;
                return Ok(Some(Synth::SecureAttention));
            }
            if !pressed && std::mem::take(&mut self.delete_was_sas) {
                return Ok(None);
            }
        }
        if key == usage::PAUSE {
            return Ok(Some(Synth::Pause { pressed }));
        }
        key.to_windows()
            .map(|scan| Some(Synth::Key { scan, pressed }))
            .ok_or(key)
    }

    fn holds_ctrl_and_alt(&self) -> bool {
        let holds = |a, b| self.held.contains(&a) || self.held.contains(&b);
        holds(usage::LEFT_CONTROL, usage::RIGHT_CONTROL) && holds(usage::LEFT_ALT, usage::RIGHT_ALT)
    }

    /// Wheel events for `dx`/`dy` notches. Fractions add up across calls,
    /// so precise scrolling arrives in full.
    pub(crate) fn scroll(&mut self, dx: f64, dy: f64) -> Vec<Synth> {
        let mut out = Vec::new();
        for (horizontal, notches, remainder) in [
            (false, dy, &mut self.wheel_remainder.1),
            (true, dx, &mut self.wheel_remainder.0),
        ] {
            if !notches.is_finite() {
                continue;
            }
            let total = notches * win32::WHEEL_DELTA + *remainder;
            let whole = total.trunc();
            *remainder = total - whole;
            if whole != 0.0 {
                out.push(Synth::Wheel {
                    horizontal,
                    delta: whole as i32,
                });
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::super::monitors::{Monitor, PixelRect};
    use super::*;

    fn map() -> DesktopMap {
        DesktopMap::new(vec![
            Monitor {
                id: "A".to_owned(),
                pixels: PixelRect::new(0, 0, 1920, 1080),
                scale: 1.0,
                primary: true,
            },
            Monitor {
                id: "B".to_owned(),
                pixels: PixelRect::new(1920, 0, 5760, 2160),
                scale: 2.0,
                primary: false,
            },
        ])
    }

    fn key(scan: u32, extended: bool) -> RawKey {
        RawKey {
            vk: 0,
            scan,
            extended,
            up: false,
        }
    }

    fn motion(x: f64, y: f64, dx: f64, dy: f64) -> Option<InputEvent> {
        Some(InputEvent::PointerMotion {
            position: Point::new(x, y),
            delta: Point::new(dx, dy),
        })
    }

    #[test]
    fn keys_translate_by_scan_code() {
        assert_eq!(
            key_event(key(0x1e, false)),
            Some(InputEvent::Key {
                usage: usage::A,
                pressed: true
            })
        );
        assert_eq!(
            key_event(RawKey {
                up: true,
                ..key(0x5b, true)
            }),
            Some(InputEvent::Key {
                usage: usage::LEFT_GUI,
                pressed: false
            })
        );
        // The fake left Control of AltGr, and an unknown scan code.
        assert_eq!(key_event(key(0x21d, false)), None);
        assert_eq!(key_event(key(0, false)), None);
    }

    #[test]
    fn pause_and_num_lock_go_by_virtual_key() {
        let pause = |vk, scan, extended| {
            key_event(RawKey {
                vk,
                scan,
                extended,
                up: false,
            })
        };
        let pressed = |usage| {
            Some(InputEvent::Key {
                usage,
                pressed: true,
            })
        };
        assert_eq!(pause(win32::VK_PAUSE, 0x45, false), pressed(usage::PAUSE));
        assert_eq!(pause(win32::VK_CANCEL, 0x46, true), pressed(usage::PAUSE));
        assert_eq!(
            pause(win32::VK_NUMLOCK, 0x45, true),
            pressed(usage::NUM_LOCK)
        );
        // Even if a driver leaves the extended flag off.
        assert_eq!(
            pause(win32::VK_NUMLOCK, 0x45, false),
            pressed(usage::NUM_LOCK)
        );
    }

    #[test]
    fn mouse_messages_decode() {
        let pt = Pixel::new(5, 6);
        assert_eq!(
            decode_mouse(win32::WM_MOUSEMOVE, 0, pt),
            Some(RawMouse::Move { to: pt })
        );
        assert_eq!(
            decode_mouse(win32::WM_XBUTTONDOWN, 0x0001_0000, pt),
            Some(RawMouse::Button {
                button: Button::Back,
                pressed: true
            })
        );
        assert_eq!(
            decode_mouse(win32::WM_XBUTTONUP, 0x0002_0000, pt),
            Some(RawMouse::Button {
                button: Button::Forward,
                pressed: false
            })
        );
        // One notch towards the user is -120 in the high word.
        assert_eq!(
            decode_mouse(win32::WM_MOUSEWHEEL, 0xff88_0000, pt),
            Some(RawMouse::Wheel {
                horizontal: false,
                notches: -1.0
            })
        );
        assert_eq!(
            decode_mouse(win32::WM_MOUSEHWHEEL, 0x003c_0000, pt),
            Some(RawMouse::Wheel {
                horizontal: true,
                notches: 0.5
            })
        );
        assert_eq!(decode_mouse(0x0203, 0, pt), None);
    }

    #[test]
    fn wheel_and_buttons_become_events() {
        let mut t = MouseTranslator::default();
        let cursor = Pixel::new(0, 0);
        assert_eq!(
            t.translate(
                RawMouse::Wheel {
                    horizontal: false,
                    notches: 1.0
                },
                cursor
            ),
            Some(InputEvent::Scroll { dx: 0.0, dy: 1.0 })
        );
        assert_eq!(
            t.translate(
                RawMouse::Wheel {
                    horizontal: true,
                    notches: -0.5
                },
                cursor
            ),
            Some(InputEvent::Scroll { dx: -0.5, dy: 0.0 })
        );
        assert_eq!(
            t.translate(
                RawMouse::Button {
                    button: Button::Middle,
                    pressed: true
                },
                cursor
            ),
            Some(InputEvent::Button {
                button: Button::Middle,
                pressed: true
            })
        );
    }

    #[test]
    fn motion_reports_logical_position_and_delta() {
        let mut t = MouseTranslator {
            map: Arc::new(map()),
            ..MouseTranslator::default()
        };
        let mv = |to| RawMouse::Move { to };
        assert_eq!(
            t.translate(mv(Pixel::new(110, 95)), Pixel::new(100, 100)),
            motion(110.0, 95.0, 10.0, -5.0)
        );
        // Onto the 200% monitor.
        assert_eq!(
            t.translate(mv(Pixel::new(1924, 100)), Pixel::new(1918, 100)),
            motion(1922.0, 50.0, 4.0, -50.0)
        );
        // No movement, away from any edge: nothing to report.
        assert_eq!(
            t.translate(mv(Pixel::new(500, 500)), Pixel::new(500, 500)),
            None
        );
    }

    #[test]
    fn a_cursor_held_against_an_outer_edge_pushes_past_it() {
        let mut t = MouseTranslator {
            map: Arc::new(map()),
            ..MouseTranslator::default()
        };
        let mv = |to| RawMouse::Move { to };
        // Right edge of the desktop, on the 200% monitor.
        let edge = Pixel::new(5759, 1000);
        assert_eq!(
            t.translate(mv(edge), edge),
            motion(1920.0 + 3839.0 / 2.0, 500.0, 1.0, 0.0)
        );
        // Bottom left corner of the primary: the bottom is an outer edge
        // there because the primary is shorter.
        let corner = Pixel::new(0, 1079);
        assert_eq!(
            t.translate(mv(corner), corner),
            motion(0.0, 1079.0, -1.0, 1.0)
        );
        // If Windows reports motion past the edge instead, it passes on.
        assert_eq!(
            t.translate(mv(Pixel::new(5765, 1000)), edge),
            motion(1920.0 + 3839.0 / 2.0, 500.0, 3.0, 0.0)
        );
    }

    #[test]
    fn grabbed_motion_is_measured_from_the_held_cursor() {
        let mut t = MouseTranslator {
            map: Arc::new(map()),
            grabbed: true,
            ..MouseTranslator::default()
        };
        let held = Pixel::new(960, 540);
        assert_eq!(
            t.translate(
                RawMouse::Move {
                    to: Pixel::new(970, 530)
                },
                held
            ),
            motion(960.0, 540.0, 10.0, -10.0)
        );
        // Held on the 200% monitor, deltas are in its logical units.
        let held = Pixel::new(3840, 1080);
        assert_eq!(
            t.translate(
                RawMouse::Move {
                    to: Pixel::new(3850, 1080)
                },
                held
            ),
            motion(2880.0, 540.0, 5.0, 0.0)
        );
        // No push while grabbed, even on an edge.
        let edge = Pixel::new(5759, 1000);
        assert_eq!(t.translate(RawMouse::Move { to: edge }, edge), None);
    }

    #[test]
    fn the_motion_of_a_warp_is_dropped_once() {
        let mut t = MouseTranslator {
            map: Arc::new(map()),
            ..MouseTranslator::default()
        };
        let target = Pixel::new(960, 540);
        t.warped_to = Some(target);
        assert_eq!(
            t.translate(RawMouse::Move { to: target }, Pixel::new(10, 10)),
            None
        );
        // A later move to the same place is real.
        assert_eq!(
            t.translate(RawMouse::Move { to: target }, Pixel::new(950, 540)),
            motion(960.0, 540.0, 10.0, 0.0)
        );
        // If the warp produced no motion, the first real move is kept.
        t.warped_to = Some(target);
        assert_eq!(
            t.translate(
                RawMouse::Move {
                    to: Pixel::new(961, 540)
                },
                target
            ),
            motion(961.0, 540.0, 1.0, 0.0)
        );
        assert_eq!(t.warped_to, None);
    }

    #[test]
    fn normalized_coordinates_land_on_every_pixel() {
        for size in [
            1, 2, 3, 768, 1080, 1366, 1920, 2560, 3840, 5120, 7680, 15360,
        ] {
            for offset in 0..size {
                let n = i64::from(normalize(offset - 100, -100, size));
                let size = i64::from(size);
                assert!((0..=65535).contains(&n));
                assert_eq!(
                    n * size / 65536,
                    i64::from(offset),
                    "truncating, size {size}"
                );
                let rounded = (n * (size - 1) * 2 + 65535) / (65535 * 2);
                assert_eq!(rounded, i64::from(offset), "rounding, size {size}");
            }
        }
        // Pixels off the desktop go to its edges.
        assert_eq!(normalize(-5, 0, 1920), normalize(0, 0, 1920));
        assert_eq!(normalize(5000, 0, 1920), normalize(1919, 0, 1920));
    }

    #[test]
    fn moves_cover_the_virtual_desktop() {
        let i = Injector::default();
        let map = map();
        // The desktop is 5760 x 2160 pixels; the top left pixel of B.
        assert_eq!(
            i.move_to(&map, Point::new(1920.0, 0.0)),
            Some(Synth::MoveTo {
                x: normalize(1920, 0, 5760),
                y: normalize(0, 0, 2160)
            })
        );
        assert_eq!(
            i.move_to(&DesktopMap::default(), Point::new(0.0, 0.0)),
            None
        );
    }

    #[test]
    fn keys_go_by_scan_code_and_pause_by_virtual_key() {
        let mut i = Injector::default();
        assert_eq!(
            i.key(usage::LEFT_GUI, true),
            Ok(Some(Synth::Key {
                scan: ScanCode {
                    code: 0x5b,
                    extended: true
                },
                pressed: true
            }))
        );
        assert_eq!(
            i.key(usage::PAUSE, false),
            Ok(Some(Synth::Pause { pressed: false }))
        );
        // Keyboard page usages Windows has no scan code for.
        assert_eq!(i.key(HidUsage::new(0xa5), true), Err(HidUsage::new(0xa5)));
    }

    #[test]
    fn ctrl_alt_del_becomes_a_secure_attention_request() {
        let mut i = Injector::default();
        let delete = Synth::Key {
            scan: usage::DELETE.to_windows().unwrap(),
            pressed: true,
        };
        // Delete alone, and with only Control, is a key.
        assert_eq!(i.key(usage::DELETE, true), Ok(Some(delete)));
        i.key(usage::DELETE, false).unwrap();
        i.key(usage::RIGHT_CONTROL, true).unwrap();
        assert_eq!(i.key(usage::DELETE, true), Ok(Some(delete)));
        i.key(usage::DELETE, false).unwrap();

        i.key(usage::LEFT_ALT, true).unwrap();
        assert_eq!(i.key(usage::DELETE, true), Ok(Some(Synth::SecureAttention)));
        // Its release is not injected, and the next Delete is a key again.
        assert_eq!(i.key(usage::DELETE, false), Ok(None));
        i.key(usage::LEFT_ALT, false).unwrap();
        assert_eq!(i.key(usage::DELETE, true), Ok(Some(delete)));
    }

    #[test]
    fn fractional_scrolling_adds_up() {
        let mut i = Injector::default();
        assert_eq!(
            i.scroll(0.0, 1.0),
            vec![Synth::Wheel {
                horizontal: false,
                delta: 120
            }]
        );
        // 1/128 of a notch is 0.9375 units, too little to send on its own;
        // sixteen of them add up to fifteen units.
        let step = 1.0 / 128.0;
        assert!(i.scroll(step, -step).is_empty());
        let small: Vec<Synth> = (1..16).flat_map(|_| i.scroll(step, -step)).collect();
        let sum = |horizontal| -> i32 {
            small
                .iter()
                .filter_map(|s| match s {
                    Synth::Wheel {
                        horizontal: h,
                        delta,
                    } if *h == horizontal => Some(*delta),
                    _ => None,
                })
                .sum()
        };
        assert_eq!((sum(true), sum(false)), (15, -15));
        assert!(i.scroll(f64::NAN, 0.0).is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn win32_values_match_the_windows_crate() {
        use ::windows::Win32::UI::Input::KeyboardAndMouse as kbm;
        use ::windows::Win32::UI::WindowsAndMessaging as wm;
        assert_eq!(win32::WM_MOUSEMOVE, wm::WM_MOUSEMOVE);
        assert_eq!(win32::WM_LBUTTONDOWN, wm::WM_LBUTTONDOWN);
        assert_eq!(win32::WM_LBUTTONUP, wm::WM_LBUTTONUP);
        assert_eq!(win32::WM_RBUTTONDOWN, wm::WM_RBUTTONDOWN);
        assert_eq!(win32::WM_RBUTTONUP, wm::WM_RBUTTONUP);
        assert_eq!(win32::WM_MBUTTONDOWN, wm::WM_MBUTTONDOWN);
        assert_eq!(win32::WM_MBUTTONUP, wm::WM_MBUTTONUP);
        assert_eq!(win32::WM_MOUSEWHEEL, wm::WM_MOUSEWHEEL);
        assert_eq!(win32::WM_XBUTTONDOWN, wm::WM_XBUTTONDOWN);
        assert_eq!(win32::WM_XBUTTONUP, wm::WM_XBUTTONUP);
        assert_eq!(win32::WM_MOUSEHWHEEL, wm::WM_MOUSEHWHEEL);
        assert_eq!(win32::XBUTTON1, wm::XBUTTON1);
        assert_eq!(win32::XBUTTON2, wm::XBUTTON2);
        assert_eq!(win32::WHEEL_DELTA, f64::from(wm::WHEEL_DELTA));
        assert_eq!(win32::VK_CANCEL, u32::from(kbm::VK_CANCEL.0));
        assert_eq!(win32::VK_PAUSE, u32::from(kbm::VK_PAUSE.0));
        assert_eq!(win32::VK_NUMLOCK, u32::from(kbm::VK_NUMLOCK.0));
    }

    #[test]
    fn relative_moves_go_in_physical_pixels_and_keep_the_fractions() {
        let mut injector = RelativeMover::default();
        // 1.5 logical units at 150% are 2.25 pixels: 2 now, the quarter
        // later.
        assert_eq!(
            injector.move_by(1.5, 1.5, -1.5),
            Some(Synth::MoveBy { dx: 2, dy: -2 })
        );
        assert_eq!(
            injector.move_by(1.5, 0.5, 0.0),
            Some(Synth::MoveBy { dx: 1, dy: 0 })
        );
        // Less than a pixel waits, and the rest adds up.
        assert_eq!(injector.move_by(1.0, 0.25, 0.25), None);
        assert_eq!(injector.move_by(1.0, 0.5, 0.5), None);
        assert_eq!(
            injector.move_by(1.0, 0.25, 0.5),
            Some(Synth::MoveBy { dx: 1, dy: 1 })
        );
        assert_eq!(injector.move_by(1.0, f64::NAN, 1.0), None);
        assert_eq!(injector.move_by(0.0, 1.0, 1.0), None);
    }
}
