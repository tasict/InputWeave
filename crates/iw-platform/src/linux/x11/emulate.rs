use std::time::{Duration, Instant};

use iw_core::HidUsage;
use iw_core::geometry::Point;
use iw_core::input::Button;
use x11rb::CURRENT_TIME;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, ConnectionExt as _, KEY_PRESS_EVENT,
    KEY_RELEASE_EVENT, MOTION_NOTIFY_EVENT, Window,
};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use super::capture::HotkeyPause;
use super::platform::connect;
use super::translate::{Held, RelativeMover, WheelClicks, keycode, x_button};
use crate::linux::common::os;
use crate::{InputEmulation, PlatformError};

/// How long after the connection broke, or connecting failed, to try
/// connecting again.
const RECONNECT: Duration = Duration::from_secs(1);

/// Injects input with XTest. Moves are absolute in root window
/// coordinates, except relative mode's (`move_by`); keys go by key code,
/// so the server's keyboard layout interprets them. Requests go out
/// without waiting for the server; errors are logged as they come back.
///
/// The server repeats a key held down by itself, so the controlling
/// device's repeats, presses of a key already down, are not sent (`Held`).
/// What it holds down is let go of when it is dropped: XTest leaves keys
/// and buttons down when the client that pressed them goes.
///
/// When the connection breaks, as when the display server restarts, the
/// next injection after [`RECONNECT`] connects again; until then
/// injections fail. What was held down went with the old server.
///
/// Injecting into the session capture reads, it has capture let go of its
/// hotkey grabs while it holds keys (`HotkeyPause`), so keys from other
/// devices reach the focused app even when they match this computer's
/// hotkeys.
pub struct X11Emulation {
    link: Option<Link>,
    /// When connecting may be tried again.
    retry_at: Option<Instant>,
    wheel: WheelClicks,
    mover: RelativeMover,
    keys: Held,
    buttons: Held,
    /// The last error the server sent back, logged once.
    failure: Option<String>,
    pause: Option<HotkeyPause>,
}

struct Link {
    conn: RustConnection,
    root: Window,
    min_keycode: u8,
    max_keycode: u8,
}

impl Link {
    fn open() -> Result<Self, PlatformError> {
        let (conn, root) = connect()?;
        conn.xtest_get_version(2, 2)
            .map_err(|e| os("the X server has no XTest", e))?
            .reply()
            .map_err(|e| os("the X server has no XTest", e))?;
        let (min_keycode, max_keycode) = (conn.setup().min_keycode, conn.setup().max_keycode);
        Ok(Self {
            conn,
            root,
            min_keycode,
            max_keycode,
        })
    }
}

impl X11Emulation {
    pub fn connect() -> Result<Self, PlatformError> {
        Ok(Self {
            link: Some(Link::open()?),
            retry_at: None,
            wheel: WheelClicks::default(),
            mover: RelativeMover::default(),
            keys: Held::default(),
            buttons: Held::default(),
            failure: None,
            pause: None,
        })
    }

    /// Injects into the session `pause` capture's hotkeys in.
    pub fn pausing(mut self, pause: HotkeyPause) -> Self {
        self.pause = Some(pause);
        self
    }

    /// The connection, connecting again once its time came.
    fn link(&mut self) -> Result<&Link, PlatformError> {
        if self.link.is_none() && self.retry_at.is_none_or(|at| Instant::now() >= at) {
            match Link::open() {
                Ok(link) => {
                    tracing::info!("injecting on the X server again");
                    self.link = Some(link);
                    self.retry_at = None;
                    // A new server has nothing down.
                    self.keys.take();
                    self.buttons.take();
                }
                Err(e) => {
                    tracing::debug!(error = %e, "the X server is not back yet");
                    self.retry_at = Some(Instant::now() + RECONNECT);
                }
            }
        }
        self.link
            .as_ref()
            .ok_or(PlatformError::Unavailable("the X server"))
    }

    /// Sends XTest events, then flushes; a broken connection is dropped,
    /// to be made again.
    fn fake(&mut self, events: &[(u8, u8, i16, i16)]) -> Result<(), PlatformError> {
        let link = self.link()?;
        let sent = events
            .iter()
            .try_for_each(|&(kind, detail, x, y)| {
                link.conn
                    .xtest_fake_input(kind, detail, CURRENT_TIME, link.root, x, y, 0)
                    .map(drop)
            })
            .and_then(|()| link.conn.flush());
        sent.map_err(|e| {
            tracing::warn!(error = %e, "lost the X server; injecting again once it is back");
            self.lost();
            os("cannot inject input", e)
        })
    }

    fn lost(&mut self) {
        self.link = None;
        self.retry_at = Some(Instant::now() + RECONNECT);
        // Down on a server that is gone.
        self.keys.take();
        self.buttons.take();
    }

    /// Held down only once the server has it: a press that could not be
    /// sent leaves the key up, so its next press goes.
    fn press(&mut self, kind: u8, detail: u8, pressed: bool) -> Result<(), PlatformError> {
        let key = kind == KEY_PRESS_EVENT;
        let held = if key { &self.keys } else { &self.buttons };
        if !held.sends(detail, pressed) {
            return Ok(());
        }
        if key
            && pressed
            && let Some(pause) = &self.pause
        {
            pause.injecting();
        }
        let event = match (key, pressed) {
            (true, true) => KEY_PRESS_EVENT,
            (true, false) => KEY_RELEASE_EVENT,
            (false, true) => BUTTON_PRESS_EVENT,
            (false, false) => BUTTON_RELEASE_EVENT,
        };
        let sent = self.fake(&[(event, detail, 0, 0)]);
        let held = if key {
            &mut self.keys
        } else {
            &mut self.buttons
        };
        if sent.is_ok() || !pressed {
            held.sent(detail, pressed);
        }
        if key
            && !pressed
            && let Some(pause) = &self.pause
        {
            pause.released(self.keys.is_empty());
        }
        sent
    }
}

/// The 16 bits X coordinates have.
fn coordinate(v: f64) -> i16 {
    v.round().clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16
}

impl InputEmulation for X11Emulation {
    fn move_to(&mut self, position: Point) -> Result<(), PlatformError> {
        let (x, y) = (coordinate(position.x), coordinate(position.y));
        self.fake(&[(MOTION_NOTIFY_EVENT, 0, x, y)])
    }

    fn button(&mut self, button: Button, pressed: bool) -> Result<(), PlatformError> {
        self.press(BUTTON_PRESS_EVENT, x_button(button), pressed)
    }

    fn key(&mut self, usage: HidUsage, pressed: bool) -> Result<(), PlatformError> {
        let link = self.link()?;
        let code = keycode(usage, link.min_keycode, link.max_keycode)
            .ok_or(PlatformError::Unsupported("key without an X key code"))?;
        self.press(KEY_PRESS_EVENT, code, pressed)
    }

    fn scroll(&mut self, dx: f64, dy: f64) -> Result<(), PlatformError> {
        let clicks: Vec<_> = self
            .wheel
            .clicks(dx, dy)
            .into_iter()
            .flat_map(|b| {
                [
                    (BUTTON_PRESS_EVENT, b, 0, 0),
                    (BUTTON_RELEASE_EVENT, b, 0, 0),
                ]
            })
            .collect();
        if clicks.is_empty() {
            return Ok(());
        }
        self.fake(&clicks)
    }

    /// Pixels, which are X11's logical units too.
    fn move_by(&mut self, dx: f64, dy: f64) -> Result<(), PlatformError> {
        let Some((x, y)) = self.mover.move_by(dx, dy) else {
            return Ok(());
        };
        // Detail 1: relative to where the cursor is.
        self.fake(&[(MOTION_NOTIFY_EVENT, 1, x, y)])
    }

    fn position(&mut self) -> Option<Point> {
        let link = self.link().ok()?;
        let reply = link.conn.query_pointer(link.root).ok()?.reply().ok()?;
        Some(Point::new(reply.root_x.into(), reply.root_y.into()))
    }

    /// Logs what the server sent back, as nothing else reads this
    /// connection, and connects again after the connection broke.
    fn refresh(&mut self) {
        let Ok(link) = self.link() else {
            return;
        };
        let (mut refused, mut broke) = (Vec::new(), false);
        loop {
            match link.conn.poll_for_event() {
                Ok(Some(x11rb::protocol::Event::Error(e))) => refused.push(format!("{e:?}")),
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(_) => {
                    broke = true;
                    break;
                }
            }
        }
        for e in refused {
            if self.failure.as_deref() != Some(e.as_str()) {
                tracing::warn!(error = %e, "the X server refused an injection");
                self.failure = Some(e);
            }
        }
        if broke {
            tracing::warn!("lost the X server; injecting again once it is back");
            self.lost();
        }
    }
}

impl Drop for X11Emulation {
    fn drop(&mut self) {
        let Some(link) = &self.link else {
            return;
        };
        for code in self.keys.take() {
            let _ = link.conn.xtest_fake_input(
                KEY_RELEASE_EVENT,
                code,
                CURRENT_TIME,
                link.root,
                0,
                0,
                0,
            );
        }
        for button in self.buttons.take() {
            let _ = link.conn.xtest_fake_input(
                BUTTON_RELEASE_EVENT,
                button,
                CURRENT_TIME,
                link.root,
                0,
                0,
                0,
            );
        }
        // Until the server has them: requests it has not read when the
        // connection closes are lost.
        let _ = link.conn.sync();
    }
}
