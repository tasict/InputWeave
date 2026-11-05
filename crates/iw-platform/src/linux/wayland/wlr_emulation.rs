//! Input from other devices on a wlroots compositor, which has no
//! RemoteDesktop portal: a virtual
//! pointer (`zwlr_virtual_pointer_v1`) and keyboard
//! (`zwp_virtual_keyboard_v1`) of the seat.
//!
//! A thread of its own keeps the connection and sends what the agent
//! queues, so the agent never waits for the compositor. The keyboard types
//! with the seat's keymap, copied from its keyboard and again whenever it
//! changes, so keys mean what they mean on this desktop's own keyboard; a
//! seat without a keyboard gets a US keymap. The seat sends every client
//! the keymap of the keyboard it took input from last, this one's too, so
//! a keymap the keyboard has already is not given again: it would come
//! back again, round and round. wlroots takes the keyboard's keys as they
//! come, without working out the modifiers from them, so they are worked
//! out here from the keymap given (`modifiers`) and sent after each key
//! that changes them. Positions are spread over the box that bounds the
//! outputs, as the virtual pointer's absolute motion takes them. Once the
//! compositor goes, the thread connects again, from half a second up to
//! ten; nothing is held down on the new connection. Nobody is asked: no
//! portal stands between, so there is no permission.

use std::collections::VecDeque;
use std::os::fd::AsFd;
use std::os::unix::net::UnixDatagram;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iw_core::HidUsage;
use iw_core::geometry::Point;
use iw_core::input::Button;
use tracing::{debug, info, warn};

use super::modifiers::Modifiers;
use super::translate::{Held, ei_button, scroll};
use super::wlr::{
    self, AxisSource, ButtonState, Ending, Link, Served, VirtualKeyboard, VirtualPointer, Wants,
    WlAxis,
};
use super::wlroots::{self, Axis, US_KEYMAP};
use crate::linux::common::lock;
use crate::{InputEmulation, PlatformError};

/// How long dropping waits for what it let go of to reach the compositor.
const STOP_WAIT: Duration = Duration::from_secs(1);

/// How long a new connection waits for the seat's keymap.
const KEYMAP_WAIT: Duration = Duration::from_millis(500);

#[derive(Debug, PartialEq)]
enum Op {
    MoveTo(Point),
    MoveBy(f64, f64),
    Button(u32, bool),
    Key(u32, bool),
    Scroll(super::translate::Scroll),
}

/// What the agent queued for the connection, and which connection that is:
/// the two change together, under one lock, so what goes down is held on
/// the connection it went to.
#[derive(Default)]
struct Queue {
    ops: VecDeque<Op>,
    /// Counts the connections made.
    link: u64,
    /// Whether one runs now, with its virtual devices.
    connected: bool,
}

impl Queue {
    /// A connection runs: what was queued for the one before is not its.
    fn opened(&mut self) {
        self.ops.clear();
        self.link += 1;
        self.connected = true;
    }

    fn lost(&mut self) {
        self.connected = false;
        self.ops.clear();
    }

    fn push(&mut self, op: Op) -> Result<(), PlatformError> {
        if !self.connected {
            return Err(PlatformError::Unavailable("the Wayland compositor"));
        }
        self.ops.push_back(op);
        Ok(())
    }
}

/// The keys and buttons held down, and the connection they are held on.
#[derive(Default)]
struct Holds {
    link: u64,
    keys: Held,
    buttons: Held,
}

impl Holds {
    /// Queues the key (`key`) or button `code` going down or up, unless that
    /// changes nothing on the connection `queue` sends to; what is held is
    /// that connection's, as nothing is held down on a new one.
    fn press(
        &mut self,
        queue: &mut Queue,
        code: u32,
        pressed: bool,
        key: bool,
    ) -> Result<(), PlatformError> {
        if queue.link != self.link {
            *self = Self {
                link: queue.link,
                ..Self::default()
            };
        }
        let held = if key {
            &mut self.keys
        } else {
            &mut self.buttons
        };
        if !held.sends(code, pressed) {
            return Ok(());
        }
        queue.push(if key {
            Op::Key(code, pressed)
        } else {
            Op::Button(code, pressed)
        })?;
        held.sent(code, pressed);
        Ok(())
    }

    /// Lets go of everything held on the connection `queue` sends to.
    fn release_all(&mut self, queue: &mut Queue) {
        if queue.link != self.link || !queue.connected {
            return;
        }
        for code in self.keys.take() {
            queue.ops.push_back(Op::Key(code, false));
        }
        for code in self.buttons.take() {
            queue.ops.push_back(Op::Button(code, false));
        }
    }
}

#[derive(Default)]
struct Shared {
    queue: Mutex<Queue>,
    ending: Ending,
}

/// What other devices' input is told of the connection: whether input
/// reaches the desktop now.
#[derive(Clone)]
pub(crate) struct Reach(Arc<Shared>);

impl Reach {
    pub(crate) fn connected(&self) -> bool {
        lock(&self.0.queue).connected
    }
}

/// Injects input through the compositor's virtual devices.
pub struct WlrEmulation {
    shared: Arc<Shared>,
    wake: UnixDatagram,
    holds: Holds,
}

impl WlrEmulation {
    /// Starts the thread, which connects as it can.
    pub(crate) fn start() -> Result<Self, PlatformError> {
        let (wake, woken) = wlr::wake_pair()?;
        let shared = Arc::new(Shared::default());
        let thread_shared = shared.clone();
        std::thread::Builder::new()
            .name("inputweave-wayland-virtual-input".to_owned())
            .spawn(move || run(&thread_shared, &woken))
            .map_err(|e| {
                PlatformError::Os(format!("cannot start the virtual input thread: {e}"))
            })?;
        Ok(Self {
            shared,
            wake,
            holds: Holds::default(),
        })
    }

    /// Whether input reaches the desktop, for the session's status.
    pub(crate) fn reach(&self) -> Reach {
        Reach(self.shared.clone())
    }

    fn send(&self, op: Op) -> Result<(), PlatformError> {
        lock(&self.shared.queue).push(op)?;
        let _ = self.wake.send(&[1]);
        Ok(())
    }

    fn press(&mut self, code: u32, pressed: bool, key: bool) -> Result<(), PlatformError> {
        self.holds
            .press(&mut lock(&self.shared.queue), code, pressed, key)?;
        let _ = self.wake.send(&[1]);
        Ok(())
    }
}

impl InputEmulation for WlrEmulation {
    fn move_to(&mut self, position: Point) -> Result<(), PlatformError> {
        self.send(Op::MoveTo(position))
    }

    fn button(&mut self, button: Button, pressed: bool) -> Result<(), PlatformError> {
        self.press(ei_button(button), pressed, false)
    }

    fn key(&mut self, usage: HidUsage, pressed: bool) -> Result<(), PlatformError> {
        let code = u32::from(
            usage
                .to_evdev()
                .ok_or(PlatformError::Unsupported("a key Linux has no code for"))?,
        );
        self.press(code, pressed, true)
    }

    fn scroll(&mut self, dx: f64, dy: f64) -> Result<(), PlatformError> {
        match scroll(dx, dy) {
            Some(s) => self.send(Op::Scroll(s)),
            None => Ok(()),
        }
    }

    fn move_by(&mut self, dx: f64, dy: f64) -> Result<(), PlatformError> {
        self.send(Op::MoveBy(dx, dy))
    }
}

impl Drop for WlrEmulation {
    /// Lets go of what is held, and waits a moment for the compositor to
    /// have it: its virtual devices go with the connection.
    fn drop(&mut self) {
        self.holds.release_all(&mut lock(&self.shared.queue));
        self.shared.ending.ask(&self.wake, STOP_WAIT);
    }
}

/// Connects, serves, and connects again, until dropped.
fn run(shared: &Shared, woken: &UnixDatagram) {
    wlr::keep_connected(
        "inject input through the compositor",
        &shared.ending,
        woken,
        &mut (),
        |_| {
            let mut devices = match Devices::open() {
                Ok(devices) => devices,
                Err(e) => return Served::Unopened(e),
            };
            info!("injecting input through the compositor's virtual pointer and keyboard");
            lock(&shared.queue).opened();
            let why = devices.serve(shared, woken);
            lock(&shared.queue).lost();
            match why {
                Ok(()) => Served::Ended,
                Err(e) => Served::Lost(e),
            }
        },
        |_| {},
    );
}

/// A connection and its virtual devices.
struct Devices {
    link: Link,
    pointer: VirtualPointer,
    keyboard: VirtualKeyboard,
    /// The keymap the keyboard has, as given.
    uploaded: Vec<u8>,
    /// Its state, unless it would not read.
    modifiers: Option<Modifiers>,
    /// The keys down, for the state of a keymap given while they are.
    down: Vec<u32>,
}

impl Devices {
    fn open() -> Result<Self, String> {
        let mut link = Link::open(Wants {
            keyboard: true,
            keymap: true,
            pointer: false,
        })?;
        let state = &link.state;
        let (Some(seat), Some(pointers), Some(keyboards)) = (
            state.seat.clone(),
            state.pointer_manager.clone(),
            state.keyboard_manager.clone(),
        ) else {
            return Err("the compositor has no virtual pointer or keyboard".to_owned());
        };
        let pointer = pointers.create_virtual_pointer(Some(&seat), &state.qh, ());
        let keyboard = keyboards.create_virtual_keyboard(&seat, &state.qh, ());
        // The seat's keymap, if it has a keyboard, which says it as it is
        // made.
        if link.state.keyboard.is_some() && link.state.keymap.is_none() {
            let _ = link.sync(KEYMAP_WAIT);
        }
        let mut devices = Self {
            link,
            pointer,
            keyboard,
            uploaded: Vec::new(),
            modifiers: None,
            down: Vec::new(),
        };
        devices.upload_keymap()?;
        devices.link.pump()?;
        Ok(devices)
    }

    /// Gives the virtual keyboard the seat's keymap, or else a US one, unless
    /// it has that keymap already. Keys sent before it has one would end the
    /// connection.
    fn upload_keymap(&mut self) -> Result<(), String> {
        self.link.state.keymap_changed = false;
        let keymap = match self.link.state.keymap.take() {
            Some(keymap) => keymap,
            None if self.uploaded.is_empty() => {
                debug!("the seat has no keyboard: the virtual keyboard types with a US keymap");
                let mut text = US_KEYMAP.as_bytes().to_vec();
                text.push(0);
                text
            }
            None => return Ok(()),
        };
        if keymap == self.uploaded {
            return Ok(());
        }
        let fd =
            wlr::memfd_with("inputweave-keymap", &keymap).ok_or("cannot make a keymap file")?;
        const XKB_V1: u32 = 1;
        let size = u32::try_from(keymap.len()).map_err(|_| "a keymap too large")?;
        debug!(size, "the virtual keyboard types with the seat's keymap");
        self.keyboard.keymap(XKB_V1, fd.as_fd(), size);
        self.modifiers = match Modifiers::of(&keymap) {
            Ok(mut modifiers) => {
                // The compositor works the keys down into its keyboard's
                // state for the new keymap; so does this.
                for code in &self.down {
                    modifiers.key(*code, true);
                }
                if !self.down.is_empty() {
                    let mods = modifiers.mods();
                    self.keyboard
                        .modifiers(mods.depressed, mods.latched, mods.locked, mods.group);
                }
                Some(modifiers)
            }
            Err(e) => {
                warn!(
                    error = e,
                    "cannot read the keymap: keys go without their modifiers"
                );
                None
            }
        };
        self.uploaded = keymap;
        Ok(())
    }

    /// Sends what is queued until dropped (`Ok`) or the connection breaks.
    fn serve(&mut self, shared: &Shared, woken: &UnixDatagram) -> Result<(), String> {
        loop {
            let ops: Vec<Op> = lock(&shared.queue).ops.drain(..).collect();
            for op in ops {
                self.apply(op);
            }
            if self.link.state.keymap_changed {
                self.upload_keymap()?;
            }
            // Events are not this backend's to take: only the outputs and
            // the keymap matter, which the connection keeps.
            self.link.state.events.clear();
            if shared.ending.asked() {
                let ops: Vec<Op> = lock(&shared.queue).ops.drain(..).collect();
                for op in ops {
                    self.apply(op);
                }
                return self.link.sync(STOP_WAIT / 2);
            }
            self.link.wait(woken, None)?;
        }
    }

    fn apply(&mut self, op: Op) {
        let time = self.link.time();
        match op {
            Op::MoveTo(position) => match self.link.geometry().extent {
                Some(extent) => {
                    let (x, y, width, height) = extent.place(position);
                    self.pointer.motion_absolute(time, x, y, width, height);
                    self.pointer.frame();
                }
                None => debug!("no outputs to move the cursor on"),
            },
            Op::MoveBy(dx, dy) => {
                self.pointer.motion(time, dx, dy);
                self.pointer.frame();
            }
            Op::Button(code, pressed) => {
                let state = if pressed {
                    ButtonState::Pressed
                } else {
                    ButtonState::Released
                };
                self.pointer.button(time, code, state);
                self.pointer.frame();
            }
            Op::Key(code, pressed) => {
                self.keyboard.key(time, code, u32::from(pressed));
                self.down.retain(|c| *c != code);
                if pressed {
                    self.down.push(code);
                }
                // As a keyboard of the compositor's own tells them: the
                // key, then the modifiers it changed.
                if let Some(mods) = self.modifiers.as_mut().and_then(|m| m.key(code, pressed)) {
                    self.keyboard
                        .modifiers(mods.depressed, mods.latched, mods.locked, mods.group);
                }
            }
            Op::Scroll(s) => {
                let scroll = wlroots::axis_scroll(s);
                self.pointer.axis_source(if scroll.wheel {
                    AxisSource::Wheel
                } else {
                    AxisSource::Finger
                });
                for (axis, value, steps) in scroll.moves {
                    let axis = match axis {
                        Axis::Vertical => WlAxis::VerticalScroll,
                        Axis::Horizontal => WlAxis::HorizontalScroll,
                    };
                    match steps {
                        Some(steps) => self.pointer.axis_discrete(time, axis, value, steps),
                        None => self.pointer.axis(time, axis, value),
                    }
                }
                self.pointer.frame();
            }
        }
    }
}

impl Drop for Devices {
    fn drop(&mut self) {
        self.pointer.destroy();
        self.keyboard.destroy();
        let _ = self.link.pump();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_A: u32 = 30;
    const BTN_LEFT: u32 = 0x110;

    #[test]
    fn what_goes_down_goes_up_on_the_connection_it_went_to() {
        let mut queue = Queue::default();
        let mut holds = Holds::default();
        // Before a connection: nothing goes, and nothing is held.
        assert!(holds.press(&mut queue, KEY_A, true, true).is_err());
        assert!(holds.keys.sends(KEY_A, true));

        // A connection opened before the press got the queue: the key is
        // held on that one, so a repeat of it does not go again, and
        // letting go of everything lets go of it.
        queue.opened();
        queue.lost();
        queue.opened();
        holds.press(&mut queue, KEY_A, true, true).unwrap();
        holds.press(&mut queue, KEY_A, true, true).unwrap();
        holds.press(&mut queue, BTN_LEFT, true, false).unwrap();
        assert_eq!(
            queue.ops.drain(..).collect::<Vec<_>>(),
            [Op::Key(KEY_A, true), Op::Button(BTN_LEFT, true)]
        );
        holds.release_all(&mut queue);
        assert_eq!(
            queue.ops.drain(..).collect::<Vec<_>>(),
            [Op::Key(KEY_A, false), Op::Button(BTN_LEFT, false)]
        );

        // Held on a connection that went: nothing of it is held on the next,
        // where the key goes down again, and up.
        holds.press(&mut queue, KEY_A, true, true).unwrap();
        queue.lost();
        queue.opened();
        holds.press(&mut queue, KEY_A, true, true).unwrap();
        holds.press(&mut queue, KEY_A, false, true).unwrap();
        assert_eq!(
            queue.ops.drain(..).collect::<Vec<_>>(),
            [Op::Key(KEY_A, true), Op::Key(KEY_A, false)]
        );
        holds.release_all(&mut queue);
        assert!(queue.ops.is_empty());
    }

    #[test]
    fn what_was_held_on_a_connection_gone_is_not_let_go_of_on_the_next() {
        let mut queue = Queue::default();
        let mut holds = Holds::default();
        queue.opened();
        holds.press(&mut queue, KEY_A, true, true).unwrap();
        queue.lost();
        holds.release_all(&mut queue);
        queue.opened();
        holds.release_all(&mut queue);
        assert!(queue.ops.is_empty());
    }
}
