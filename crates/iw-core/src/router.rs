//! The controller-side routing state machine.
//!
//! The machine whose physical keyboard and mouse are in use runs a `Router`.
//! It keeps a virtual cursor on the shared canvas, decides when the cursor
//! leaves this machine or hops between remote ones, and turns captured local
//! input into actions: grab or release local input, and send absolute
//! positions, buttons, keys and scrolling to the device under the cursor.
//!
//! It also watches for the mouse being shaken, wherever the cursor is, and
//! then asks for the cursor to be shown; that changes nothing about where
//! input goes.
//!
//! On a remote device the cursor moves at that device's speed
//! (`Router::set_speeds`): the part of a movement it makes there is scaled
//! by that speed. A movement that crosses an edge goes at the speed of the
//! device it starts on until the crossing and at the speed of the device it
//! enters after it (`Layout::step_at`): from this machine to another, the
//! part here moves as the mouse does and the rest at the other's speed;
//! back here, the part left after the crossing moves as the mouse does
//! again. Relative movements are never scaled.
//!
//! Hotkeys (`Hotkeys`) take control back, lock the cursor to the device it
//! is on, or ask for the cursor on the device with a number
//! (`Action::Jump`); their keys are never forwarded. Control pressed twice
//! quickly takes control back too, when that is on
//! (`Hotkeys::take_back_double`); those presses are forwarded. With a switch
//! modifier set (`EdgePolicy::switch_modifier`), the cursor crosses edges
//! only while that modifier is held.
//!
//! A held button keeps the cursor on its device, so a drag never ends up
//! split between two, except while files are dragged across: from this
//! machine (`Event::DragActive`), or from the remote device the cursor is
//! on (`Event::RemoteDrag`). Then the button is let go of where the cursor
//! leaves, pressed again on each device it enters, and the drag ends where
//! it is released (`Action::DragReleased`).
//!
//! In relative mode (`Router::set_relative`, the relative mode hotkey, or
//! the device saying an app there holds the pointer:
//! `Event::PointerCaptured`) the cursor stays on the remote device it is
//! on and crosses no edge, and its movements go there as such
//! (`Action::RelativeMotion`) rather than as positions: games that hide
//! and hold the cursor read movements. Taking control back or jumping
//! elsewhere ends it. Ended with the cursor still there, the cursor
//! carries on from where the device says its pointer went
//! (`Event::PointerAt`).
//!
//! The caller passes the current time with every event; the router only
//! needs it to tell how long the cursor has been pushing against an edge,
//! how quickly the mouse turns back and forth, and how quickly Control was
//! pressed twice.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::time::{Duration, Instant};

use iw_keymap::ModifierRemap;

use crate::HidUsage;
use crate::geometry::Point;
pub use crate::hotkey::Hotkey;
use crate::hotkey::{DoublePressWatch, HeldHotkeys, Hotkeys, Modifier};
use crate::input::{Button, InputEvent};
use crate::layout::{Crossing, DeviceId, EdgePolicy, Exit, Layout};
use crate::shake::ShakeDetector;
use crate::speed::CursorSpeed;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RouterConfig {
    pub edge: EdgePolicy,
    /// Taking control back, locking the cursor and jumping to a device by
    /// its number; none of them is ever forwarded to a remote device.
    pub hotkeys: Hotkeys,
    /// The reserved hotkeys (`Hotkeys::reserved`) whose keys capture keeps
    /// from local apps even while input is not grabbed, so such a key still
    /// down when input is grabbed needs no release on this machine; the
    /// others reach local apps, which must see them released. Capture
    /// tells it again as it changes (`InputEvent::HotkeysHeldBack`); a set
    /// told before the one in place is ignored (`HeldHotkeys::take`).
    pub hotkeys_held_back: HeldHotkeys,
}

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    /// Local input drives this machine.
    Local,
    /// Local input is grabbed and drives `device`; `cursor` is on the canvas.
    Remote { device: DeviceId, cursor: Point },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Input(InputEvent),
    PeerConnected(DeviceId),
    PeerLost(DeviceId),
    /// The device's screen locked where input from here cannot reach it
    /// (true), or it takes input again (false). Meanwhile it is left out
    /// of edges and jumps like a device not connected, and control comes
    /// back from it.
    PeerScreenLocked(DeviceId, bool),
    /// Input from here cannot reach the device at all now (true), as on a
    /// Wayland desktop that has not allowed remote control, or it can again
    /// (false). Meanwhile it is left out like a locked one.
    PeerInputUnavailable(DeviceId, bool),
    /// The device paused sharing (true), or resumed it (false): meanwhile it
    /// is left out like a locked one, and control comes back from it.
    PeerSharingPaused(DeviceId, bool),
    /// Someone used the remote device's own keyboard or mouse.
    RemoteActivity(DeviceId),
    SetSharing(bool),
    /// Locks the cursor to the device it is on, or unlocks it.
    SetLocked(bool),
    /// A drag of files from this device began (an app reported it while a
    /// mouse button is held), or ended without the router seeing it end.
    /// While it lasts, the cursor crosses edges with the button held: the
    /// buttons are released here as it leaves this device and pressed on
    /// each device it enters. It ends when the buttons are released, or
    /// the keyboard and mouse come back here; either way `DragReleased`
    /// says where. Begun with no button held, it ends at once.
    DragActive(bool),
    /// A drag of files on the remote device began there (its app reported
    /// it), or ended there. It crosses edges like a drag from here: the
    /// remote device lets go of the button as the cursor leaves it (`Leave`
    /// releases everything), and each device entered presses it again.
    /// Reported while the cursor is not on that device, or with no button
    /// held, it ends at once; ask `can_carry_drag_from` first.
    RemoteDrag(DeviceId, bool),
    /// Whether an app on the device holds the pointer (hidden and kept in
    /// its window, as games do), as the device last said. With automatic
    /// relative mode on (`Router::set_relative_auto`), the device the
    /// cursor is on enters relative mode when it says so, or when the
    /// cursor enters it while held, and leaves it when it lets go. Only a
    /// change counts: relative mode turned on or off by hand stays so
    /// until the device next says otherwise.
    PointerCaptured(DeviceId, bool),
    /// Where the device's pointer is, in its desktop coordinates, as it
    /// says once relative mode ends there: the cursor carries on from
    /// there, rather than from where relative mode began. Movements wait
    /// for it, up to `RESYNC_WAIT`.
    PointerAt(DeviceId, Point),
    /// The drag of files carried here from a remote device (`DragHere`)
    /// was taken up by this device's app, which pressed the button for a
    /// drag of its own: it is let go of here as the cursor leaves again.
    /// Without it nothing is let go of here, where the button never went
    /// down.
    DragHeldHere,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReturnReason {
    /// The take-back hotkey, or Control pressed twice
    /// (`Hotkeys::take_back_double`).
    Hotkey,
    PeerLost(DeviceId),
    RemoteActivity(DeviceId),
    SharingPaused,
    LayoutChanged,
    /// The controlled device's screen locked, and input cannot reach it.
    ScreenLocked(DeviceId),
    /// The controlled device can take no input from other devices now.
    InputUnavailable(DeviceId),
    /// The controlled device paused sharing.
    PeerPaused(DeviceId),
    /// Capture lost hold of the input (`InputEvent::CaptureEnded`).
    CaptureEnded,
}

/// Why relative mode began (`Action::RelativeStarted`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelativeStart {
    /// The relative mode hotkey.
    Hotkey,
    /// Asked for by an app or `ctl` (`Router::set_relative`).
    Asked,
    /// The device said an app there holds the pointer
    /// (`Event::PointerCaptured`).
    Captured,
}

/// Why relative mode ended (`Action::RelativeEnded`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelativeEnd {
    /// Its hotkey, or `Router::set_relative`.
    TurnedOff,
    /// The device said the app there let go of the pointer.
    Released,
    /// Automatic relative mode was turned off, ending what began by itself
    /// (`Router::set_relative_auto`).
    AutoOff,
    /// The cursor jumped elsewhere (`Router::jump_to`).
    Jumped,
    /// Sharing was paused, and the keyboard and mouse came back here.
    Paused,
    /// The device went away, and the keyboard and mouse came back here.
    Lost,
    /// The keyboard and mouse came back here otherwise;
    /// `Action::ControlReturned` says why.
    Returned,
}

impl RelativeEnd {
    /// Whether the cursor stays on the device as relative mode ends so, to
    /// carry on from where the device says its pointer went.
    fn stays(self) -> bool {
        matches!(self, Self::TurnedOff | Self::Released | Self::AutoOff)
    }
}

/// Why relative mode cannot be turned on.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum RelativeError {
    #[error("the cursor is on this device")]
    NotControlling,
    #[error("device {0} cannot take relative movements")]
    NotSupported(DeviceId),
}

/// Something the caller must carry out. Positions sent to a device are in
/// that device's desktop coordinates.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// The cursor crossed from one device to another through an edge,
    /// leaving and entering where `Crossing` says (each in its own device's
    /// desktop coordinates). Comes first, before the actions that carry the
    /// cursor over (`Grab`, `Leave`, `Enter`, `Ungrab`), so the caller
    /// knows where those happen. Only for a real crossing of an edge: not
    /// for a jump, the take-back hotkey, control coming back
    /// (`ControlReturned`) or relative motion.
    Crossed(Crossing),
    /// Start consuming local input and hide the local cursor.
    Grab,
    /// Stop consuming local input and show the cursor at `warp_to`
    /// (this machine's desktop coordinates).
    Ungrab {
        warp_to: Point,
    },
    /// Replay a key event on this machine. Used to release keys that were
    /// already down when input was grabbed, so the local OS does not keep
    /// them pressed.
    LocalKey {
        usage: HidUsage,
        pressed: bool,
    },
    /// Replay a button event on this machine at `position` (this
    /// machine's desktop coordinates): a drag leaving it lets go of the
    /// button here, before input is grabbed, so the drag here ends where
    /// the cursor leaves.
    LocalButton {
        button: Button,
        pressed: bool,
        position: Point,
    },
    Enter {
        device: DeviceId,
        position: Point,
        /// Modifiers held at the moment of entry, to be pressed on arrival.
        modifiers: Vec<HidUsage>,
        /// Buttons held while dragging files across, to be pressed on
        /// arrival; empty otherwise.
        buttons: Vec<Button>,
    },
    /// The receiver releases every key and button it is holding.
    Leave {
        device: DeviceId,
    },
    Motion {
        device: DeviceId,
        position: Point,
    },
    /// `position` in the device's coordinates, where the press goes; none
    /// in relative mode and until the device says where its pointer went
    /// after it (`Event::PointerAt`): there, wherever its pointer is.
    Button {
        device: DeviceId,
        button: Button,
        pressed: bool,
        position: Option<Point>,
    },
    Key {
        device: DeviceId,
        usage: HidUsage,
        pressed: bool,
    },
    Scroll {
        device: DeviceId,
        dx: f64,
        dy: f64,
    },
    /// Tell the user why the keyboard and mouse came back.
    ControlReturned(ReturnReason),
    /// The cursor was locked to the device it is on, or unlocked.
    LockChanged(bool),
    /// The jump hotkey with digit `slot` (1 to 9) was pressed: the cursor
    /// goes to the device numbered so, as `Router::jump_to` takes it there,
    /// if it can. Which device has which number is the caller's.
    Jump {
        slot: u8,
    },
    /// The user shook the mouse: show where the cursor is. `device` is the
    /// one it is on, this one or another; `position` is in its desktop
    /// coordinates.
    LocateCursor {
        device: DeviceId,
        position: Point,
    },
    /// The cursor crossed back onto this device with a drag of files on
    /// remote device `source` (`Event::RemoteDrag`): it may be dropped here.
    /// Not when the keyboard and mouse come back here otherwise, which ends
    /// the drag (`DragReleased`).
    DragHere {
        source: DeviceId,
    },
    /// The drag of files ended with the cursor on `device`: its buttons
    /// were released there, or the keyboard and mouse came back to this
    /// device, which is then `device`.
    DragReleased {
        device: DeviceId,
    },
    /// Relative mode began on `device`, the remote device the cursor is on.
    RelativeStarted {
        device: DeviceId,
        cause: RelativeStart,
    },
    /// Relative mode ended on `device`, the remote device the cursor was on.
    RelativeEnded {
        device: DeviceId,
        reason: RelativeEnd,
    },
    /// In relative mode, the pointer moved by `delta`: logical units, as
    /// captured here, which the device takes as its own.
    RelativeMotion {
        device: DeviceId,
        delta: Point,
    },
    /// The relative mode hotkey was pressed on `device`, which does not
    /// take relative movements; the key went there as any other.
    RelativeUnsupported {
        device: DeviceId,
    },
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum RouterError {
    #[error("this device ({0}) is not part of the layout")]
    LocalDeviceMissing(DeviceId),
}

/// Why the cursor cannot jump to a device.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum JumpError {
    #[error("sharing is paused")]
    SharingPaused,
    #[error("the cursor is locked to the device it is on")]
    Locked,
    /// As with crossing an edge: the button would stay down where the drag
    /// started.
    #[error("a mouse button is held down")]
    ButtonHeld,
    #[error("device {0} is not connected")]
    NotConnected(DeviceId),
    #[error("device {0} has no place in the arrangement")]
    NotArranged(DeviceId),
    #[error("the screen of device {0} is locked, and input cannot reach it")]
    ScreenLocked(DeviceId),
    #[error("device {0} can take no input from other devices now")]
    InputUnavailable(DeviceId),
    #[error("device {0} paused sharing")]
    PeerPaused(DeviceId),
}

/// Where the cursor may jump (`Router::jump_target`), for `Router::jump`.
#[derive(Clone, Debug, PartialEq)]
pub struct JumpTarget {
    device: DeviceId,
    /// Canvas coordinates: the middle of its primary display.
    position: Point,
}

/// Whether the cursor has pushed against an edge long enough to cross it.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Dwell {
    Over,
    /// Not yet: the cursor stays at this canvas point, as far as the edge
    /// lets it go.
    Waiting(Point),
}

/// How long movements wait for the device to say where its pointer is
/// once relative mode ends there (`Event::PointerAt`), from the first that
/// waits, before the cursor carries on from where it was.
pub const RESYNC_WAIT: Duration = Duration::from_millis(200);

/// Relative mode ended with the cursor still on `device`, which is to say
/// where its pointer went.
#[derive(Clone, Debug, PartialEq)]
struct Resync {
    device: DeviceId,
    /// When the first movement began to wait.
    since: Option<Instant>,
    /// The movements that waited, added up.
    pending: Point,
}

/// The cursor pushing against an edge towards `target` since `since`.
#[derive(Clone, Debug, PartialEq)]
struct Push {
    target: DeviceId,
    since: Instant,
    /// The part of the movement the edge held back, which says which way
    /// the cursor pushes.
    toward: Point,
}

/// What keeps input from a connected device now.
#[derive(Clone, Copy, Debug, Default)]
struct Unreachable {
    /// Its screen is locked, and input cannot reach the lock screen.
    screen_locked: bool,
    /// It cannot take input at all now.
    input_unavailable: bool,
    /// It paused sharing.
    sharing_paused: bool,
}

impl Unreachable {
    fn any(self) -> bool {
        self.screen_locked || self.input_unavailable || self.sharing_paused
    }
}

/// One reason a device is unreachable.
#[derive(Clone, Copy, Debug)]
enum Barrier {
    ScreenLocked,
    InputUnavailable,
    SharingPaused,
}

#[derive(Debug)]
pub struct Router {
    layout: Layout,
    local: DeviceId,
    config: RouterConfig,
    connected: HashSet<DeviceId>,
    /// Connected devices input cannot reach now, and why.
    unreachable: HashMap<DeviceId, Unreachable>,
    remaps: HashMap<DeviceId, ModifierRemap>,
    /// How fast the cursor moves on remote devices; one left out moves at
    /// the normal speed.
    speeds: HashMap<DeviceId, CursorSpeed>,
    sharing: bool,
    /// The cursor stays on the device it is on.
    locked: bool,
    state: State,
    held_keys: BTreeSet<HidUsage>,
    held_buttons: BTreeSet<Button>,
    /// Keys that were already down when input was grabbed.
    carried: BTreeSet<HidUsage>,
    /// Keys whose press triggered the lock, a jump or the relative mode
    /// hotkey; their repeats and release are not forwarded either. With
    /// whether local apps saw the key go down: not while input was grabbed,
    /// nor for a hotkey capture held back then.
    consumed: BTreeMap<HidUsage, bool>,
    /// Control pressed toward the double press that takes control back.
    double: DoublePressWatch,
    /// Set while the cursor waits at an edge for the dwell time.
    push: Option<Push>,
    /// Last local cursor position, in this machine's desktop coordinates.
    last_local: Point,
    shake: ShakeDetector,
    /// Files are being dragged across: from this machine
    /// (`Event::DragActive`) or from `drag_source`.
    drag: bool,
    /// The remote device the dragged files are on (`Event::RemoteDrag`);
    /// `None` while they are on this machine.
    drag_source: Option<DeviceId>,
    /// Buttons of the drag already let go of here, as it left this machine.
    lifted: BTreeSet<Button>,
    /// Relative mode is on for the remote device the cursor is on, and why.
    relative: Option<RelativeStart>,
    /// Devices enter relative mode when an app there holds the pointer.
    relative_auto: bool,
    /// Devices that said an app there holds the pointer.
    captured: HashSet<DeviceId>,
    /// Connected devices that take relative movements.
    relative_capable: HashSet<DeviceId>,
    /// Set from the end of relative mode until the device says where its
    /// pointer is.
    resync: Option<Resync>,
}

impl Router {
    pub fn new(layout: Layout, local: DeviceId, config: RouterConfig) -> Result<Self, RouterError> {
        let last_local = layout
            .device(&local)
            .and_then(|d| d.primary_display())
            .map(|d| d.bounds.center())
            .ok_or_else(|| RouterError::LocalDeviceMissing(local.clone()))?;
        Ok(Self {
            layout,
            local,
            config,
            connected: HashSet::new(),
            unreachable: HashMap::new(),
            remaps: HashMap::new(),
            speeds: HashMap::new(),
            sharing: true,
            locked: false,
            state: State::Local,
            held_keys: BTreeSet::new(),
            held_buttons: BTreeSet::new(),
            carried: BTreeSet::new(),
            consumed: BTreeMap::new(),
            double: DoublePressWatch::default(),
            push: None,
            last_local,
            shake: ShakeDetector::new(),
            drag: false,
            drag_source: None,
            lifted: BTreeSet::new(),
            relative: None,
            relative_auto: true,
            captured: HashSet::new(),
            relative_capable: HashSet::new(),
            resync: None,
        })
    }

    /// The device in relative mode, and why it entered it.
    pub fn relative(&self) -> Option<(&DeviceId, RelativeStart)> {
        match (&self.state, self.relative) {
            (State::Remote { device, .. }, Some(cause)) => Some((device, cause)),
            _ => None,
        }
    }

    /// Whether devices enter relative mode by themselves when an app there
    /// holds the pointer, and leave it when it lets go. On by default.
    /// Turning it off ends relative mode that began so; relative mode
    /// turned on by hand stays.
    pub fn set_relative_auto(&mut self, on: bool) -> Vec<Action> {
        self.relative_auto = on;
        if !on && self.relative == Some(RelativeStart::Captured) {
            return self.end_relative(RelativeEnd::AutoOff);
        }
        Vec::new()
    }

    /// Whether connected `device` takes relative movements, which a device
    /// that does not cannot be put in relative mode.
    pub fn set_relative_capable(&mut self, device: DeviceId, capable: bool) {
        if capable {
            self.relative_capable.insert(device);
        } else {
            self.relative_capable.remove(&device);
        }
    }

    /// Turns relative mode on or off for the remote device the cursor is
    /// on. Off when it is not on, or the cursor is here, does nothing.
    pub fn set_relative(&mut self, on: bool) -> Result<Vec<Action>, RelativeError> {
        let State::Remote { device, .. } = &self.state else {
            return if on {
                Err(RelativeError::NotControlling)
            } else {
                Ok(Vec::new())
            };
        };
        if !on {
            return Ok(self.end_relative(RelativeEnd::TurnedOff));
        }
        if !self.relative_capable.contains(device) {
            return Err(RelativeError::NotSupported(device.clone()));
        }
        Ok(self.start_relative(RelativeStart::Asked))
    }

    fn start_relative(&mut self, cause: RelativeStart) -> Vec<Action> {
        let State::Remote { device, .. } = &self.state else {
            return Vec::new();
        };
        if self.relative.is_some() || !self.relative_capable.contains(device) {
            return Vec::new();
        }
        self.relative = Some(cause);
        self.push = None;
        self.resync = None;
        // Presses before it count toward no double press after it.
        self.double.interrupt();
        vec![Action::RelativeStarted {
            device: device.clone(),
            cause,
        }]
    }

    /// Ends relative mode. Unless the cursor is leaving the device or
    /// moving elsewhere on it, it carries on from where the device says its
    /// pointer went.
    fn end_relative(&mut self, reason: RelativeEnd) -> Vec<Action> {
        let on = self.relative.take().is_some();
        if on {
            // Nor do presses in it count toward one after it.
            self.double.interrupt();
        }
        match &self.state {
            State::Remote { device, .. } if on => {
                if reason.stays() {
                    self.resync = Some(Resync {
                        device: device.clone(),
                        since: None,
                        pending: Point::default(),
                    });
                }
                vec![Action::RelativeEnded {
                    device: device.clone(),
                    reason,
                }]
            }
            _ => Vec::new(),
        }
    }

    /// The device said where its pointer is, as relative mode ended there.
    fn pointer_at(&mut self, device: DeviceId, position: Point, now: Instant) -> Vec<Action> {
        if !self.is_remote(&device) || self.resync.as_ref().is_none_or(|r| r.device != device) {
            return Vec::new();
        }
        let Some(resync) = self.resync.take() else {
            return Vec::new();
        };
        let cursor = self.layout.device(&device).and_then(|d| {
            let canvas = d.to_canvas(position);
            d.nearest_display(canvas)
                .map(|(_, bounds)| bounds.clamp(canvas))
        });
        let Some(cursor) = cursor.filter(|c| c.x.is_finite() && c.y.is_finite()) else {
            return Vec::new();
        };
        self.state = State::Remote { device, cursor };
        if resync.pending == Point::default() {
            return Vec::new();
        }
        // What moved meanwhile goes on from there.
        self.route_motion(self.last_local, resync.pending, now)
    }

    /// Relative mode for the device the cursor just entered, when an app
    /// there holds the pointer and that turns it on by itself.
    fn relative_on_entry(&mut self) -> Vec<Action> {
        match &self.state {
            State::Remote { device, .. }
                if self.relative_auto && self.captured.contains(device) =>
            {
                self.start_relative(RelativeStart::Captured)
            }
            _ => Vec::new(),
        }
    }

    fn pointer_captured(&mut self, device: DeviceId, captured: bool) -> Vec<Action> {
        let changed = if captured {
            self.captured.insert(device.clone())
        } else {
            self.captured.remove(&device)
        };
        if !changed || !self.is_remote(&device) {
            return Vec::new();
        }
        if captured {
            if self.relative_auto {
                return self.start_relative(RelativeStart::Captured);
            }
            Vec::new()
        } else if self.relative_auto || self.relative == Some(RelativeStart::Captured) {
            // What began as the app held the pointer ends as it lets go.
            self.end_relative(RelativeEnd::Released)
        } else {
            Vec::new()
        }
    }

    /// Whether files are being dragged across.
    pub fn dragging(&self) -> bool {
        self.drag
    }

    /// Whether a drag of files reported by `device` can be carried: the
    /// cursor is on it, a button held, and no drag of this device's own is
    /// going on.
    pub fn can_carry_drag_from(&self, device: &DeviceId) -> bool {
        self.is_remote(device)
            && !self.held_buttons.is_empty()
            && !(self.drag && self.drag_source.is_none())
    }

    /// The device the files dragged across are on: this one, or a remote
    /// device that reported the drag. `None` while nothing is dragged.
    pub fn drag_source(&self) -> Option<&DeviceId> {
        if !self.drag {
            return None;
        }
        Some(self.drag_source.as_ref().unwrap_or(&self.local))
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn locked(&self) -> bool {
        self.locked
    }

    /// Where the cursor leaves this device for another now, in its desktop
    /// coordinates (`Layout::exits`): nowhere while sharing is paused or
    /// the cursor is locked.
    pub fn exits(&self) -> Vec<Exit> {
        if !self.sharing || self.locked {
            return Vec::new();
        }
        self.layout
            .exits(&self.local, &self.config.edge, |d| self.can_enter(d))
    }

    /// Whether the cursor waits at an edge to cross, pushing against it
    /// for the dwell time (`EdgePolicy.dwell_ms`).
    pub fn waiting_at_edge(&self) -> bool {
        self.push.is_some()
    }

    /// Replaces every device's key remapping with `remaps`; a device left
    /// out gets none.
    pub fn set_remaps(&mut self, remaps: impl IntoIterator<Item = (DeviceId, ModifierRemap)>) {
        self.remaps = remaps.into_iter().collect();
    }

    /// How fast the cursor moves on each device from the next movement on,
    /// replacing what was given before; a device left out moves at the
    /// normal speed, and a speed for this machine changes nothing.
    pub fn set_speeds(&mut self, speeds: impl IntoIterator<Item = (DeviceId, CursorSpeed)>) {
        self.speeds = speeds.into_iter().collect();
    }

    /// Changes how the cursor crosses edges from now on.
    pub fn set_edge(&mut self, edge: EdgePolicy) {
        self.config.edge = edge;
        self.push = None;
    }

    /// Changes the hotkeys from now on, and which of them capture holds
    /// back (`RouterConfig::hotkeys_held_back`). A key that triggered a
    /// hotkey and is still down stays that hotkey's until it is released,
    /// so its repeats and release go nowhere, as capture keeps it from
    /// local apps until then too, if it did as it went down.
    pub fn set_hotkeys(&mut self, hotkeys: Hotkeys, held_back: HeldHotkeys) {
        self.config.hotkeys = hotkeys;
        self.config.hotkeys_held_back.take(held_back);
    }

    /// Replaces the arrangement. If the device under the cursor disappeared
    /// from it, control returns to this machine.
    pub fn set_layout(&mut self, layout: Layout) -> Result<Vec<Action>, RouterError> {
        if layout.device(&self.local).is_none() {
            return Err(RouterError::LocalDeviceMissing(self.local.clone()));
        }
        self.layout = layout;
        self.push = None;
        let gone = match &self.state {
            State::Remote { device, .. } => self.layout.device(device).is_none(),
            State::Local => false,
        };
        Ok(if gone {
            self.return_control(ReturnReason::LayoutChanged, false)
        } else {
            Vec::new()
        })
    }

    /// Handles `event`, which happened at `now`.
    pub fn handle(&mut self, event: Event, now: Instant) -> Vec<Action> {
        match event {
            Event::Input(input) => self.handle_input(input, now),
            Event::PeerConnected(device) => {
                self.connected.insert(device);
                Vec::new()
            }
            Event::PeerLost(device) => {
                self.connected.remove(&device);
                self.unreachable.remove(&device);
                self.captured.remove(&device);
                self.relative_capable.remove(&device);
                if self.is_remote(&device) {
                    self.return_control(ReturnReason::PeerLost(device), false)
                } else {
                    Vec::new()
                }
            }
            Event::PeerScreenLocked(device, locked) => {
                self.set_unreachable(device, Barrier::ScreenLocked, locked)
            }
            Event::PeerInputUnavailable(device, unavailable) => {
                self.set_unreachable(device, Barrier::InputUnavailable, unavailable)
            }
            Event::PeerSharingPaused(device, paused) => {
                self.set_unreachable(device, Barrier::SharingPaused, paused)
            }
            Event::RemoteActivity(device) => {
                if self.is_remote(&device) {
                    self.return_control(ReturnReason::RemoteActivity(device), true)
                } else {
                    Vec::new()
                }
            }
            Event::SetSharing(enabled) => {
                self.sharing = enabled;
                self.push = None;
                if !enabled && matches!(self.state, State::Remote { .. }) {
                    self.return_control(ReturnReason::SharingPaused, true)
                } else {
                    Vec::new()
                }
            }
            Event::SetLocked(locked) => self.set_locked(locked),
            Event::DragActive(active) => self.set_drag(active),
            Event::RemoteDrag(device, active) => self.set_remote_drag(device, active),
            Event::PointerCaptured(device, captured) => self.pointer_captured(device, captured),
            Event::PointerAt(device, position) => self.pointer_at(device, position, now),
            Event::DragHeldHere => {
                if self.drag && self.drag_source.is_some() && self.state == State::Local {
                    self.lifted.clear();
                }
                Vec::new()
            }
        }
    }

    fn set_drag(&mut self, active: bool) -> Vec<Action> {
        if !active {
            if self.drag_source.is_none() {
                self.end_drag();
            }
            return Vec::new();
        }
        if self.held_buttons.is_empty() {
            // Released before the drag was reported.
            return vec![Action::DragReleased {
                device: self.current(),
            }];
        }
        self.end_drag();
        self.drag = true;
        Vec::new()
    }

    fn set_remote_drag(&mut self, device: DeviceId, active: bool) -> Vec<Action> {
        if !active {
            if self.drag_source.as_ref() == Some(&device) {
                self.end_drag();
            }
            return Vec::new();
        }
        if !self.is_remote(&device) || self.held_buttons.is_empty() {
            // Released, or gone from it, before the drag was reported.
            return vec![Action::DragReleased {
                device: self.current(),
            }];
        }
        self.end_drag();
        self.drag = true;
        self.drag_source = Some(device);
        // This machine never saw the button go down: nothing to let go of
        // here unless the drag comes here.
        self.lifted = self.held_buttons.clone();
        Vec::new()
    }

    fn end_drag(&mut self) {
        self.drag = false;
        self.drag_source = None;
        self.lifted.clear();
    }

    /// The device the cursor is on.
    fn current(&self) -> DeviceId {
        match &self.state {
            State::Local => self.local.clone(),
            State::Remote { device, .. } => device.clone(),
        }
    }

    /// Moves the cursor to the middle of `device`'s primary display, as if
    /// it had travelled there: into a remote device, or back to this one.
    pub fn jump_to(&mut self, device: &DeviceId) -> Result<Vec<Action>, JumpError> {
        let target = self.jump_target(device)?;
        Ok(self.jump(target))
    }

    /// Moves the cursor where `jump_target` said it may go, as `jump_to`
    /// does, without asking again: nothing the router heard since may have
    /// changed that.
    pub fn jump(&mut self, target: JumpTarget) -> Vec<Action> {
        let JumpTarget {
            device,
            position: target,
        } = target;
        let device = &device;
        // Already there, in relative mode: it stays so.
        if self.relative.is_some() && self.is_remote(device) {
            return Vec::new();
        }
        self.push = None;
        let mut actions = self.end_relative(RelativeEnd::Jumped);
        self.resync = None;
        match self.state.clone() {
            State::Remote {
                device: current, ..
            } if current == *device => {
                actions.extend(self.move_within(current, target));
                // As on any entry.
                actions.extend(self.relative_on_entry());
                return actions;
            }
            State::Remote {
                device: current, ..
            } => {
                actions.push(Action::Leave { device: current });
            }
            State::Local if *device != self.local => {
                self.carry_held_keys();
                actions.push(Action::Grab);
            }
            State::Local => {}
        }
        if *device == self.local {
            let warp_to = self.to_local_of(&self.local, target);
            self.state = State::Local;
            self.carried.clear();
            self.last_local = warp_to;
            actions.push(Action::Ungrab { warp_to });
        } else {
            actions.push(self.enter(device.clone(), target));
            actions.extend(self.relative_on_entry());
        }
        actions
    }

    /// Whether the cursor may jump to `device` now (`jump_to`), without
    /// moving it: where it would land, to `jump` there, or why not.
    pub fn jump_target(&self, device: &DeviceId) -> Result<JumpTarget, JumpError> {
        if !self.sharing {
            return Err(JumpError::SharingPaused);
        }
        if self.locked {
            return Err(JumpError::Locked);
        }
        if !self.held_buttons.is_empty() {
            return Err(JumpError::ButtonHeld);
        }
        if *device != self.local && !self.connected.contains(device) {
            return Err(JumpError::NotConnected(device.clone()));
        }
        if let Some(unreachable) = self.unreachable.get(device) {
            return Err(if unreachable.screen_locked {
                JumpError::ScreenLocked(device.clone())
            } else if unreachable.input_unavailable {
                JumpError::InputUnavailable(device.clone())
            } else {
                JumpError::PeerPaused(device.clone())
            });
        }
        let position = self
            .layout
            .device(device)
            .and_then(|d| Some(d.to_canvas(d.primary_display()?.bounds.center())))
            .ok_or_else(|| JumpError::NotArranged(device.clone()))?;
        Ok(JumpTarget {
            device: device.clone(),
            position,
        })
    }

    fn set_locked(&mut self, locked: bool) -> Vec<Action> {
        if self.locked == locked {
            return Vec::new();
        }
        self.locked = locked;
        self.push = None;
        vec![Action::LockChanged(locked)]
    }

    /// Whether the cursor may move onto remote `device`: it is connected,
    /// and input can reach it.
    fn can_enter(&self, device: &DeviceId) -> bool {
        self.connected.contains(device) && !self.unreachable.contains_key(device)
    }

    /// `device` became unreachable for `barrier` (`on`), or no longer for
    /// it. Becoming so, it is left out of edges and jumps, and control
    /// comes back from it.
    fn set_unreachable(&mut self, device: DeviceId, barrier: Barrier, on: bool) -> Vec<Action> {
        let mut reasons = self.unreachable.get(&device).copied().unwrap_or_default();
        match barrier {
            Barrier::ScreenLocked => reasons.screen_locked = on,
            Barrier::InputUnavailable => reasons.input_unavailable = on,
            Barrier::SharingPaused => reasons.sharing_paused = on,
        }
        if !reasons.any() {
            self.unreachable.remove(&device);
            return Vec::new();
        }
        self.unreachable.insert(device.clone(), reasons);
        if !on {
            return Vec::new();
        }
        self.push = None;
        if self.is_remote(&device) {
            let reason = match barrier {
                Barrier::ScreenLocked => ReturnReason::ScreenLocked(device),
                Barrier::InputUnavailable => ReturnReason::InputUnavailable(device),
                Barrier::SharingPaused => ReturnReason::PeerPaused(device),
            };
            self.return_control(reason, true)
        } else {
            Vec::new()
        }
    }

    fn is_remote(&self, device: &DeviceId) -> bool {
        matches!(&self.state, State::Remote { device: d, .. } if d == device)
    }

    fn handle_input(&mut self, input: InputEvent, now: Instant) -> Vec<Action> {
        match input {
            InputEvent::PointerMotion { position, delta } => self.motion(position, delta, now),
            InputEvent::Button { button, pressed } => {
                // As any key between the two presses would.
                self.double.interrupt();
                if pressed {
                    self.held_buttons.insert(button);
                } else {
                    self.held_buttons.remove(&button);
                }
                let mut actions = match &self.state {
                    State::Local => Vec::new(),
                    State::Remote { device, cursor } => vec![Action::Button {
                        device: device.clone(),
                        button,
                        pressed,
                        // In relative mode, and until the device says where
                        // its pointer went after it, the cursor here is not
                        // where the pointer is there.
                        position: (self.relative.is_none() && self.resync.is_none())
                            .then(|| self.to_local_of(device, *cursor)),
                    }],
                };
                // The drop: after the release, so the device under the
                // cursor sees the drag end first.
                if self.drag && self.held_buttons.is_empty() {
                    self.end_drag();
                    actions.push(Action::DragReleased {
                        device: self.current(),
                    });
                }
                actions
            }
            InputEvent::Key { usage, pressed } => self.key(usage, pressed, now),
            InputEvent::Scroll { dx, dy } => {
                // Control held to zoom, say, is no press toward the double.
                self.double.interrupt();
                match &self.state {
                    State::Local => Vec::new(),
                    State::Remote { device, .. } => vec![Action::Scroll {
                        device: device.clone(),
                        dx,
                        dy,
                    }],
                }
            }
            InputEvent::DoublePressed => {
                self.double.interrupt();
                if self.double_press_counts() {
                    self.return_control(ReturnReason::Hotkey, true)
                } else {
                    Vec::new()
                }
            }
            InputEvent::CaptureEnded => {
                // What the cursor does next goes unseen: a push against an
                // edge, waiting to cross, starts over.
                self.push = None;
                self.return_control(ReturnReason::CaptureEnded, true)
            }
            // No input: what a key going down from now on counts as.
            InputEvent::HotkeysHeldBack(held) => {
                self.config.hotkeys_held_back.take(held);
                Vec::new()
            }
        }
    }

    /// Routes a movement of the pointer. A shake shows where the cursor
    /// ended up, after whatever the movement did.
    fn motion(&mut self, position: Point, delta: Point, now: Instant) -> Vec<Action> {
        // A game swings the mouse back and forth: that is no shake.
        let shaken = self.relative.is_none() && self.shake.moved(delta, now);
        let mut actions = self.route_motion(position, delta, now);
        if shaken {
            actions.push(self.locate_cursor());
        }
        actions
    }

    /// Where the cursor is, to be shown.
    fn locate_cursor(&self) -> Action {
        match &self.state {
            State::Local => Action::LocateCursor {
                device: self.local.clone(),
                position: self.last_local,
            },
            State::Remote { device, cursor } => Action::LocateCursor {
                device: device.clone(),
                position: self.to_local_of(device, *cursor),
            },
        }
    }

    fn route_motion(&mut self, position: Point, delta: Point, now: Instant) -> Vec<Action> {
        // In relative mode the movement goes as such, wherever it leads.
        if self.relative.is_some()
            && let State::Remote { device, .. } = &self.state
        {
            return vec![Action::RelativeMotion {
                device: device.clone(),
                delta,
            }];
        }
        // Once relative mode ends, movements wait for the device to say
        // where its pointer went: from where relative mode began, the
        // cursor would jump back there.
        let mut delta = delta;
        if let Some(resync) = self.resync.as_mut() {
            let since = *resync.since.get_or_insert(now);
            if now.saturating_duration_since(since) < RESYNC_WAIT {
                resync.pending = resync.pending + delta;
                return Vec::new();
            }
            // Never said: on from where the cursor was.
            delta = delta + resync.pending;
            self.resync = None;
        }
        // A held button stays where the drag started, unless files are
        // dragged across. A switch modifier must be held, if there is one.
        let switch_held = self
            .config
            .edge
            .switch_modifier
            .is_none_or(|m| m.is_held(|k| self.held_keys.contains(&k)));
        let can_cross = self.sharing
            && !self.locked
            && switch_held
            && (self.held_buttons.is_empty() || self.drag);
        if !can_cross {
            self.push = None;
        }
        match self.state.clone() {
            State::Local => {
                self.last_local = position;
                if !can_cross {
                    return Vec::new();
                }
                let Some(local) = self.layout.device(&self.local) else {
                    return Vec::new();
                };
                let from = local.to_canvas(position);
                let Some((step, _)) = self.layout.step_at(
                    &self.local,
                    from,
                    delta,
                    &self.config.edge,
                    |d| self.can_enter(d),
                    |d| self.speed_on(d),
                ) else {
                    return Vec::new();
                };
                if step.device == self.local {
                    self.ease_off(delta);
                    return Vec::new();
                }
                // Meanwhile the edge of the screen holds the cursor back.
                if let Dwell::Waiting(_) =
                    self.dwell(&self.local.clone(), from, delta, &step.device, now)
                {
                    return Vec::new();
                }
                self.carry_held_keys();
                let mut actions: Vec<Action> =
                    step.crossing.into_iter().map(Action::Crossed).collect();
                // Let go of a dragged button before grabbing, while the
                // cursor here is still where it leaves.
                actions.extend(self.lift_dragged_buttons());
                actions.push(Action::Grab);
                actions.push(self.enter(step.device, step.position));
                actions.extend(self.relative_on_entry());
                actions
            }
            State::Remote { device, cursor } => {
                // `delta` as the cursor moves here, at this device's speed;
                // so it also slides along an edge it pushes against.
                let Some((step, delta)) = self.layout.step_at(
                    &device,
                    cursor,
                    delta,
                    &self.config.edge,
                    |d| can_cross && (*d == self.local || self.can_enter(d)),
                    |d| self.speed_on(d),
                ) else {
                    return Vec::new();
                };
                if step.device == device {
                    self.ease_off(delta);
                    return self.move_within(device, step.position);
                }
                if let Dwell::Waiting(held) = self.dwell(&device, cursor, delta, &step.device, now)
                {
                    return self.move_within(device, held);
                }
                let mut actions: Vec<Action> =
                    step.crossing.into_iter().map(Action::Crossed).collect();
                actions.push(Action::Leave { device });
                if step.device == self.local {
                    let warp_to = self.to_local_of(&self.local, step.position);
                    self.last_local = warp_to;
                    self.state = State::Local;
                    self.carried.clear();
                    actions.push(Action::Ungrab { warp_to });
                    if let Some(source) = self.drag_source.clone().filter(|_| self.drag) {
                        actions.push(Action::DragHere { source });
                    }
                } else {
                    actions.push(self.enter(step.device, step.position));
                    actions.extend(self.relative_on_entry());
                }
                actions
            }
        }
    }

    /// Moves the cursor to canvas point `cursor` on `device`, the remote
    /// device it is already on.
    fn move_within(&mut self, device: DeviceId, cursor: Point) -> Vec<Action> {
        self.state = State::Remote {
            device: device.clone(),
            cursor,
        };
        let position = self.to_local_of(&device, cursor);
        vec![Action::Motion { device, position }]
    }

    /// Whether the cursor, moving from canvas point `from` on `current` by
    /// `delta` across an edge into `target`, has pushed against that edge
    /// for the dwell time. The first push starts the clock.
    fn dwell(
        &mut self,
        current: &DeviceId,
        from: Point,
        delta: Point,
        target: &DeviceId,
        now: Instant,
    ) -> Dwell {
        let dwell = Duration::from_millis(self.config.edge.dwell_ms.into());
        if dwell.is_zero() {
            return Dwell::Over;
        }
        let pushing = self.push.as_ref().filter(|push| push.target == *target);
        if let Some(push) = pushing
            && now.saturating_duration_since(push.since) >= dwell
        {
            self.push = None;
            return Dwell::Over;
        }
        let held = self
            .layout
            .step(current, from, delta, &self.config.edge, |_| false)
            .map_or(from, |s| s.position);
        if pushing.is_none() {
            self.push = Some(Push {
                target: target.clone(),
                since: now,
                toward: from + delta - held,
            });
        }
        Dwell::Waiting(held)
    }

    /// A movement that did not cross an edge. Sliding along the edge goes on
    /// pushing, so a shaky hand does not start the dwell over; moving back
    /// from the edge does.
    fn ease_off(&mut self, delta: Point) {
        if let Some(push) = &self.push
            && push.toward.x * delta.x + push.toward.y * delta.y < 0.0
        {
            self.push = None;
        }
    }

    /// Remembers the keys down as input is grabbed, which the local session
    /// saw go down and must see released. A hotkey's key that it never saw
    /// (`consumed`) is left out.
    fn carry_held_keys(&mut self) {
        self.carried = self
            .held_keys
            .iter()
            .filter(|k| self.consumed.get(k) != Some(&false))
            .copied()
            .collect();
    }

    /// `usage` went down for `hotkey`: its repeats and release go nowhere.
    /// The local session saw it go down unless capture holds that hotkey
    /// back, as it says it did then, whether or not input was grabbed (X11
    /// holds none back while another app's grab takes every key): a key
    /// held as a hotkey's stays held so until it is released, whatever
    /// becomes of the hotkey meanwhile.
    fn consume(&mut self, usage: HidUsage, hotkey: Option<Hotkey>) {
        let held_back = hotkey.is_some_and(|h| self.config.hotkeys_held_back.contains(&h));
        self.consumed.insert(usage, !held_back);
    }

    /// A drag leaving this machine lets go of its buttons here, where the
    /// cursor leaves, once: the drag here ends at the edge, and the button
    /// goes down again where the cursor enters.
    fn lift_dragged_buttons(&mut self) -> Vec<Action> {
        if !self.drag {
            return Vec::new();
        }
        let lift: Vec<Button> = self
            .held_buttons
            .difference(&self.lifted)
            .copied()
            .collect();
        self.lifted.extend(lift.iter().copied());
        let position = self.last_local;
        lift.into_iter()
            .map(|button| Action::LocalButton {
                button,
                pressed: false,
                position,
            })
            .collect()
    }

    /// The cursor goes into `device` at canvas point `cursor`, with the
    /// modifiers held here pressed there; all but the switch modifier, held
    /// only to cross, whose release the device then ignores.
    fn enter(&mut self, device: DeviceId, cursor: Point) -> Action {
        let remap = self.remap_for(&device);
        let switch = self.config.edge.switch_modifier;
        let modifiers = self
            .held_keys
            .iter()
            .filter(|k| k.is_modifier())
            .filter(|k| switch.is_none() || Modifier::of(**k) != switch)
            .map(|k| remap.apply(*k))
            .collect();
        let buttons = if self.drag {
            self.held_buttons.iter().copied().collect()
        } else {
            Vec::new()
        };
        let position = self.to_local_of(&device, cursor);
        self.state = State::Remote {
            device: device.clone(),
            cursor,
        };
        // Presses toward the double count on one device at a time.
        self.double.interrupt();
        Action::Enter {
            device,
            position,
            modifiers,
            buttons,
        }
    }

    fn key(&mut self, usage: HidUsage, pressed: bool, now: Instant) -> Vec<Action> {
        let double = self.double_pressed(usage, pressed, now);
        if self.consumed.contains_key(&usage) {
            self.track_key(usage, pressed);
            if pressed {
                return Vec::new();
            }
            self.consumed.remove(&usage);
            // Grabbed while the hotkey was held: unless capture held the key
            // back, the local session saw it go down.
            if self.carried.remove(&usage) {
                return vec![Action::LocalKey {
                    usage,
                    pressed: false,
                }];
            }
            return Vec::new();
        }
        // Auto-repeat must not toggle the lock back and forth.
        let repeat = self.held_keys.contains(&usage);
        if pressed
            && !repeat
            && self
                .config
                .hotkeys
                .lock
                .matches(usage, |k| self.held_keys.contains(&k))
        {
            self.track_key(usage, pressed);
            self.consume(usage, Some(self.config.hotkeys.lock));
            return self.set_locked(!self.locked);
        }
        if pressed
            && !repeat
            && let Some(slot) = self
                .config
                .hotkeys
                .jump_slot(usage, |k| self.held_keys.contains(&k))
        {
            self.track_key(usage, pressed);
            self.consume(usage, self.config.hotkeys.jump_hotkey(slot));
            return vec![Action::Jump { slot }];
        }
        let State::Remote { device, .. } = self.state.clone() else {
            self.track_key(usage, pressed);
            return Vec::new();
        };
        if pressed
            && self
                .config
                .hotkeys
                .take_back
                .matches(usage, |k| self.held_keys.contains(&k))
        {
            self.track_key(usage, pressed);
            return self.return_control(ReturnReason::Hotkey, true);
        }
        let mut actions = Vec::new();
        if pressed
            && !repeat
            && self
                .config
                .hotkeys
                .relative
                .is_some_and(|h| h.matches(usage, |k| self.held_keys.contains(&k)))
        {
            if self.relative.is_some() || self.relative_capable.contains(&device) {
                self.track_key(usage, pressed);
                self.consume(usage, self.config.hotkeys.relative);
                return if self.relative.is_some() {
                    self.end_relative(RelativeEnd::TurnedOff)
                } else {
                    self.start_relative(RelativeStart::Hotkey)
                };
            }
            // A device that cannot take it gets the key as any other.
            actions.push(Action::RelativeUnsupported {
                device: device.clone(),
            });
        }
        actions.extend(self.forward_key(device, usage, pressed));
        // After the release went, so the device sees Control go up before
        // the cursor leaves it.
        if double {
            actions.extend(self.return_control(ReturnReason::Hotkey, true));
        }
        actions
    }

    /// Whether `usage` going down or up at `now`, before it is tracked,
    /// completes the double press that takes control back
    /// (`Hotkeys::take_back_double`, `DoublePressWatch`).
    fn double_pressed(&mut self, usage: HidUsage, pressed: bool, now: Instant) -> bool {
        if !self.double_press_counts() {
            self.double.interrupt();
            return false;
        }
        let buttons_held = !self.held_buttons.is_empty();
        self.double.key(
            usage,
            pressed,
            self.held_keys.iter().copied(),
            buttons_held,
            now,
        )
    }

    /// Whether Control pressed twice takes control back now: while it is
    /// on and this machine controls a remote device, but not in relative
    /// mode, where games often take Control for themselves (to crouch, say).
    fn double_press_counts(&self) -> bool {
        self.config.hotkeys.take_back_double
            && matches!(self.state, State::Remote { .. })
            && self.relative.is_none()
    }

    /// Sends a key to remote `device`, the one the cursor is on.
    fn forward_key(&mut self, device: DeviceId, usage: HidUsage, pressed: bool) -> Vec<Action> {
        self.track_key(usage, pressed);

        let remap = self.remap_for(&device);
        if self.carried.contains(&usage) {
            if pressed {
                // Auto-repeat of a key that belongs to the local session.
                return Vec::new();
            }
            self.carried.remove(&usage);
            let mut actions = vec![Action::LocalKey {
                usage,
                pressed: false,
            }];
            if usage.is_modifier() {
                actions.push(Action::Key {
                    device,
                    usage: remap.apply(usage),
                    pressed: false,
                });
            }
            return actions;
        }
        vec![Action::Key {
            device,
            usage: remap.apply(usage),
            pressed,
        }]
    }

    fn track_key(&mut self, usage: HidUsage, pressed: bool) {
        if pressed {
            self.held_keys.insert(usage);
        } else {
            self.held_keys.remove(&usage);
        }
    }

    fn return_control(&mut self, reason: ReturnReason, send_leave: bool) -> Vec<Action> {
        let mut actions = self.end_relative(match reason {
            ReturnReason::SharingPaused => RelativeEnd::Paused,
            ReturnReason::PeerLost(_) => RelativeEnd::Lost,
            _ => RelativeEnd::Returned,
        });
        self.resync = None;
        let State::Remote { device, .. } = std::mem::replace(&mut self.state, State::Local) else {
            return actions;
        };
        self.carried.clear();
        self.push = None;
        if send_leave {
            actions.push(Action::Leave { device });
        }
        actions.push(Action::Ungrab {
            warp_to: self.last_local,
        });
        actions.push(Action::ControlReturned(reason));
        if self.drag {
            self.end_drag();
            actions.push(Action::DragReleased {
                device: self.local.clone(),
            });
        }
        actions
    }

    fn remap_for(&self, device: &DeviceId) -> ModifierRemap {
        self.remaps.get(device).copied().unwrap_or_default()
    }

    fn speed_of(&self, device: &DeviceId) -> CursorSpeed {
        self.speeds.get(device).copied().unwrap_or_default()
    }

    /// How fast the cursor moves on `device`; on this machine, as the mouse
    /// moves it.
    fn speed_on(&self, device: &DeviceId) -> CursorSpeed {
        if *device == self.local {
            CursorSpeed::NORMAL
        } else {
            self.speed_of(device)
        }
    }

    fn to_local_of(&self, device: &DeviceId, canvas: Point) -> Point {
        self.layout
            .device(device)
            .map_or(canvas, |d| d.to_local(canvas))
    }
}

#[cfg(test)]
mod tests {
    use iw_keymap::usage;

    use super::*;
    use crate::hotkey::Modifiers;
    use crate::layout::Edge;
    use crate::test_support::{desk, id, staggered};

    fn router_with(config: RouterConfig) -> Router {
        let now = Instant::now();
        let mut r = Router::new(desk(), id("mac"), config).unwrap();
        r.handle(Event::PeerConnected(id("win")), now);
        r.handle(Event::PeerConnected(id("mini")), now);
        r
    }

    fn router() -> Router {
        router_with(RouterConfig::default())
    }

    /// What the router does, but for where the cursor crossed
    /// (`Action::Crossed`), which `crossings` tests on its own.
    fn handle(r: &mut Router, event: Event) -> Vec<Action> {
        but_crossings(r.handle(event, Instant::now()))
    }

    fn but_crossings(actions: Vec<Action>) -> Vec<Action> {
        actions
            .into_iter()
            .filter(|a| !matches!(a, Action::Crossed(_)))
            .collect()
    }

    fn motion_at(
        r: &mut Router,
        now: Instant,
        position: (f64, f64),
        delta: (f64, f64),
    ) -> Vec<Action> {
        but_crossings(r.handle(Event::Input(pointer(position, delta)), now))
    }

    fn pointer(position: (f64, f64), delta: (f64, f64)) -> InputEvent {
        InputEvent::PointerMotion {
            position: Point::new(position.0, position.1),
            delta: Point::new(delta.0, delta.1),
        }
    }

    /// Where the cursor crossed, of what the router did for `event`.
    fn crossings(r: &mut Router, event: Event) -> Vec<Crossing> {
        r.handle(event, Instant::now())
            .into_iter()
            .filter_map(|a| match a {
                Action::Crossed(c) => Some(c),
                _ => None,
            })
            .collect()
    }

    fn motion(r: &mut Router, position: (f64, f64), delta: (f64, f64)) -> Vec<Action> {
        motion_at(r, Instant::now(), position, delta)
    }

    fn key(r: &mut Router, usage: HidUsage, pressed: bool) -> Vec<Action> {
        handle(r, Event::Input(InputEvent::Key { usage, pressed }))
    }

    fn button(r: &mut Router, pressed: bool) -> Vec<Action> {
        handle(
            r,
            Event::Input(InputEvent::Button {
                button: Button::Left,
                pressed,
            }),
        )
    }

    /// Holds Control, Option and Shift, presses `usage`, and returns what
    /// the router did for the modifiers and for the key.
    fn chord(r: &mut Router, usage: HidUsage) -> (Vec<Action>, Vec<Action>) {
        let mut modifiers = Vec::new();
        for m in [usage::LEFT_CONTROL, usage::LEFT_ALT, usage::LEFT_SHIFT] {
            modifiers.extend(key(r, m, true));
        }
        (modifiers, key(r, usage, true))
    }

    fn release_chord(r: &mut Router, usage: HidUsage) -> Vec<Action> {
        let mut actions = key(r, usage, false);
        for m in [usage::LEFT_CONTROL, usage::LEFT_ALT, usage::LEFT_SHIFT] {
            actions.extend(key(r, m, false));
        }
        actions
    }

    /// Moves from the Mac's right edge into the Windows PC.
    /// Lands at canvas (1516, 900), which is (4, 900) on the PC.
    fn enter_win(r: &mut Router) -> Vec<Action> {
        motion(r, (1511.0, 500.0), (5.0, 0.0))
    }

    #[test]
    fn requires_local_device_in_layout() {
        assert_eq!(
            Router::new(desk(), id("ghost"), RouterConfig::default()).unwrap_err(),
            RouterError::LocalDeviceMissing(id("ghost"))
        );
    }

    #[test]
    fn local_motion_inside_the_screen_does_nothing() {
        let mut r = router();
        assert!(motion(&mut r, (700.0, 500.0), (10.0, 0.0)).is_empty());
        assert_eq!(*r.state(), State::Local);
    }

    #[test]
    fn crossing_a_seam_grabs_and_enters() {
        let mut r = router();
        assert_eq!(
            enter_win(&mut r),
            vec![
                Action::Grab,
                Action::Enter {
                    device: id("win"),
                    position: Point::new(4.0, 900.0),
                    modifiers: vec![],
                    buttons: vec![],
                },
            ]
        );
        assert_eq!(
            *r.state(),
            State::Remote {
                device: id("win"),
                cursor: Point::new(1516.0, 900.0),
            }
        );
    }

    #[test]
    fn remote_motion_sends_absolute_positions() {
        let mut r = router();
        enter_win(&mut r);
        assert_eq!(
            motion(&mut r, (0.0, 0.0), (10.0, 10.0)),
            vec![Action::Motion {
                device: id("win"),
                position: Point::new(14.0, 910.0),
            }]
        );
    }

    #[test]
    fn crossing_back_ungrabs_at_the_seam() {
        let mut r = router();
        enter_win(&mut r);
        assert_eq!(
            motion(&mut r, (0.0, 0.0), (-10.0, 0.0)),
            vec![
                Action::Leave { device: id("win") },
                Action::Ungrab {
                    warp_to: Point::new(1506.0, 500.0),
                },
            ]
        );
        assert_eq!(*r.state(), State::Local);
    }

    #[test]
    fn hops_directly_between_remote_devices() {
        let mut r = router();
        enter_win(&mut r);
        motion(&mut r, (0.0, 0.0), (484.0, -895.0)); // to canvas (2000, 5)
        assert_eq!(
            motion(&mut r, (0.0, 0.0), (0.0, -10.0)),
            vec![
                Action::Leave { device: id("win") },
                Action::Enter {
                    device: id("mini"),
                    position: Point::new(488.0, 1075.0),
                    modifiers: vec![],
                    buttons: vec![],
                },
            ]
        );
    }

    #[test]
    fn does_not_enter_disconnected_devices() {
        let mut r = router();
        handle(&mut r, Event::PeerLost(id("win")));
        handle(&mut r, Event::PeerLost(id("mini")));
        assert!(enter_win(&mut r).is_empty());
        assert_eq!(*r.state(), State::Local);

        // With only the PC gone, the Mac mini is the nearest device to the
        // right of the Mac.
        handle(&mut r, Event::PeerConnected(id("mini")));
        assert!(matches!(
            enter_win(&mut r).as_slice(),
            [Action::Grab, Action::Enter { device, .. }] if *device == id("mini")
        ));
    }

    #[test]
    fn returning_beside_this_device_slides_to_its_nearest_point() {
        let mut r = router();
        enter_win(&mut r);
        // Up the PC's left edge to canvas y 100, above the Mac's top at 400.
        motion(&mut r, (0.0, 0.0), (0.0, -800.0));
        assert_eq!(
            motion(&mut r, (0.0, 0.0), (-10.0, 0.0)),
            vec![
                Action::Leave { device: id("win") },
                Action::Ungrab {
                    warp_to: Point::new(1506.0, 0.0),
                },
            ]
        );
        assert_eq!(*r.state(), State::Local);
    }

    #[test]
    fn hops_across_a_gap_to_the_nearest_remote_display() {
        let mut r = router();
        enter_win(&mut r);
        motion(&mut r, (0.0, 0.0), (484.0, -895.0)); // to canvas (2000, 5)
        motion(&mut r, (0.0, 0.0), (0.0, -10.0)); // into the mini
        motion(&mut r, (0.0, 0.0), (1431.0, -895.0)); // to its right edge
        // Beyond it, 640 away, only the PC's side display: in at its top left
        // corner, canvas (4076, -300).
        assert_eq!(
            motion(&mut r, (0.0, 0.0), (5.0, 0.0)),
            vec![
                Action::Leave { device: id("mini") },
                Action::Enter {
                    device: id("win"),
                    position: Point::new(2564.0, -300.0),
                    modifiers: vec![],
                    buttons: vec![],
                },
            ]
        );
    }

    /// The speed of `percent`, which is one.
    fn speed(percent: u32) -> CursorSpeed {
        CursorSpeed::from_percent(percent).unwrap()
    }

    #[test]
    fn the_cursor_moves_at_the_speed_of_the_device_it_is_on() {
        let mut r = router();
        r.set_speeds([(id("win"), speed(200)), (id("mini"), speed(50))]);
        // Of the 5 that leave this machine, 1 is here, as the mouse moves;
        // the other 4 go twice as far on the PC.
        assert_eq!(
            enter_win(&mut r)[1],
            Action::Enter {
                device: id("win"),
                position: Point::new(8.0, 900.0),
                modifiers: vec![],
                buttons: vec![],
            }
        );
        assert_eq!(
            motion(&mut r, (0.0, 0.0), (10.0, 10.0)),
            vec![Action::Motion {
                device: id("win"),
                position: Point::new(28.0, 920.0),
            }]
        );
        // Changed while the cursor is there, from the next movement on.
        r.set_speeds([(id("win"), speed(150))]);
        assert_eq!(
            motion(&mut r, (0.0, 0.0), (-4.0, 2.0)),
            vec![Action::Motion {
                device: id("win"),
                position: Point::new(22.0, 923.0),
            }]
        );
        r.set_speeds([(id("win"), CursorSpeed::NORMAL)]);
        assert_eq!(
            motion(&mut r, (0.0, 0.0), (2.0, 0.0)),
            vec![Action::Motion {
                device: id("win"),
                position: Point::new(24.0, 923.0),
            }]
        );
    }

    #[test]
    fn a_crossing_between_two_fast_devices_goes_at_each_ones_speed() {
        let mut r = router();
        enter_win(&mut r);
        r.set_speeds([(id("win"), speed(200)), (id("mini"), speed(50))]);
        // Twice as far: to canvas (2000, 5), as `hops_directly_between_remote_devices`
        // gets there at the normal speed.
        motion(&mut r, (0.0, 0.0), (242.0, -447.5));
        assert_eq!(
            *r.state(),
            State::Remote {
                device: id("win"),
                cursor: Point::new(2000.0, 5.0),
            }
        );
        // Up by 5: twice as far on the PC, so half of it takes the cursor to
        // the PC's top edge; the other half goes half as far on the mini.
        assert_eq!(
            motion(&mut r, (0.0, 0.0), (0.0, -5.0)),
            vec![
                Action::Leave { device: id("win") },
                Action::Enter {
                    device: id("mini"),
                    position: Point::new(488.0, 1078.75),
                    modifiers: vec![],
                    buttons: vec![],
                },
            ]
        );
        // On the mini, at the mini's.
        assert_eq!(
            motion(&mut r, (0.0, 0.0), (10.0, -10.0)),
            vec![Action::Motion {
                device: id("mini"),
                position: Point::new(493.0, 1073.75),
            }]
        );
    }

    #[test]
    fn back_on_this_machine_the_cursor_moves_as_the_mouse_does() {
        let mut r = router();
        // A speed for this machine changes nothing.
        r.set_speeds([(id("mac"), speed(300))]);
        enter_win(&mut r);
        r.set_speeds([(id("mac"), speed(300)), (id("win"), speed(300))]);
        // 4 inside the PC, a flick of 20 to the left: three times as far on
        // the PC, so 4/3 of it takes the cursor to the seam, and the other
        // 56/3 go on here as the mouse moves.
        let actions = motion(&mut r, (0.0, 0.0), (-20.0, 0.0));
        let [Action::Leave { device }, Action::Ungrab { warp_to }] = actions.as_slice() else {
            panic!("{actions:?}");
        };
        assert_eq!(*device, id("win"));
        assert!(
            (warp_to.x - (1512.0 - 56.0 / 3.0)).abs() < 1e-9 && warp_to.y == 500.0,
            "{warp_to:?}"
        );
        assert_eq!(*r.state(), State::Local);
        // Here it moves as the mouse does; crossing again, the part beyond
        // the seam goes at the PC's speed.
        assert!(motion(&mut r, (1508.0, 500.0), (3.0, 0.0)).is_empty());
        assert_eq!(
            motion(&mut r, (1511.0, 500.0), (5.0, 0.0))[1],
            Action::Enter {
                device: id("win"),
                position: Point::new(12.0, 900.0),
                modifiers: vec![],
                buttons: vec![],
            }
        );
    }

    #[test]
    fn speeds_given_together_replace_the_earlier_ones() {
        let mut r = router();
        r.set_speeds([(id("win"), speed(200))]);
        // The PC left out: back to the normal speed.
        r.set_speeds([(id("mini"), speed(50)), (id("mac"), CursorSpeed::NORMAL)]);
        enter_win(&mut r);
        assert_eq!(
            motion(&mut r, (0.0, 0.0), (10.0, 10.0)),
            vec![Action::Motion {
                device: id("win"),
                position: Point::new(14.0, 910.0),
            }]
        );
    }

    #[test]
    fn relative_movements_go_at_the_speed_of_the_mouse() {
        let mut r = relative_router();
        r.set_speeds([(id("win"), speed(250))]);
        enter_win(&mut r);
        r.set_relative(true).unwrap();
        // Games scale movements themselves.
        assert_eq!(
            motion(&mut r, (0.0, 0.0), (-6.0, 3.5)),
            vec![moved_by(-6.0, 3.5)]
        );
        r.set_relative(false).unwrap();
        handle(
            &mut r,
            Event::PointerAt(id("win"), Point::new(100.0, 800.0)),
        );
        // Out of relative mode, positions go at the PC's speed again.
        assert_eq!(
            motion(&mut r, (0.0, 0.0), (2.0, 0.0)),
            vec![Action::Motion {
                device: id("win"),
                position: Point::new(105.0, 800.0),
            }]
        );
    }

    /// The Mac of [`staggered`], with both computers to its right connected.
    fn staggered_router(config: RouterConfig) -> Router {
        let now = Instant::now();
        let mut r = Router::new(staggered(), id("mac"), config).unwrap();
        r.handle(Event::PeerConnected(id("upper")), now);
        r.handle(Event::PeerConnected(id("lower")), now);
        r
    }

    #[test]
    fn crossing_a_gap_enters_at_the_nearest_point_and_returns() {
        let mut r = staggered_router(RouterConfig::default());
        assert_eq!(
            motion(&mut r, (1511.0, 900.0), (5.0, 0.0)),
            vec![
                Action::Grab,
                Action::Enter {
                    device: id("lower"),
                    position: Point::new(4.0, 300.0),
                    modifiers: vec![],
                    buttons: vec![],
                },
            ]
        );
        assert_eq!(
            motion(&mut r, (0.0, 0.0), (-10.0, 0.0)),
            vec![
                Action::Leave {
                    device: id("lower")
                },
                Action::Ungrab {
                    warp_to: Point::new(1506.0, 900.0),
                },
            ]
        );
    }

    #[test]
    fn held_button_blocks_crossing() {
        let mut r = router();
        button(&mut r, true);
        assert!(enter_win(&mut r).is_empty());
        button(&mut r, false);

        enter_win(&mut r);
        button(&mut r, true);
        let actions = motion(&mut r, (0.0, 0.0), (-10.0, 0.0));
        assert!(
            matches!(actions.as_slice(), [Action::Motion { device, .. }] if *device == id("win"))
        );
    }

    #[test]
    fn buttons_carry_the_cursor_position() {
        let mut r = router();
        enter_win(&mut r);
        assert_eq!(
            button(&mut r, true),
            vec![Action::Button {
                device: id("win"),
                button: Button::Left,
                pressed: true,
                position: Some(Point::new(4.0, 900.0)),
            }]
        );
    }

    #[test]
    fn keys_are_forwarded_only_while_remote_and_remapped() {
        let mut r = router();
        r.set_remaps([(
            id("win"),
            ModifierRemap {
                swap_gui_and_control: true,
            },
        )]);
        assert!(key(&mut r, usage::C, true).is_empty());
        key(&mut r, usage::C, false);

        enter_win(&mut r);
        assert_eq!(
            key(&mut r, usage::LEFT_GUI, true),
            vec![Action::Key {
                device: id("win"),
                usage: usage::LEFT_CONTROL,
                pressed: true,
            }]
        );
    }

    #[test]
    fn exits_follow_what_the_cursor_may_enter() {
        let mut r = router();
        let exits = |r: &Router| {
            r.exits()
                .into_iter()
                .map(|e| (e.edge, e.at, e.start, e.end))
                .collect::<Vec<_>>()
        };
        // The PC beside it, and the Mac mini above and across a gap; in
        // the Mac's own coordinates, short of the corners.
        assert_eq!(
            exits(&r),
            [
                (Edge::Right, 1512.0, 20.0, 962.0),
                (Edge::Top, 0.0, 20.0, 1492.0)
            ]
        );
        handle(&mut r, Event::PeerLost(id("mini")));
        assert_eq!(exits(&r), [(Edge::Right, 1512.0, 20.0, 962.0)]);
        handle(&mut r, Event::PeerScreenLocked(id("win"), true));
        assert_eq!(exits(&r), []);
        handle(&mut r, Event::PeerScreenLocked(id("win"), false));
        assert_eq!(exits(&r).len(), 1);
        handle(&mut r, Event::SetLocked(true));
        assert_eq!(exits(&r), []);
        handle(&mut r, Event::SetLocked(false));
        handle(&mut r, Event::SetSharing(false));
        assert_eq!(exits(&r), []);
        handle(&mut r, Event::SetSharing(true));
        assert_eq!(exits(&r).len(), 1);
    }

    #[test]
    fn control_comes_back_when_capture_ends() {
        let mut r = router();
        let ended = Event::Input(InputEvent::CaptureEnded);
        // Nothing to give back while the cursor is here.
        assert!(handle(&mut r, ended.clone()).is_empty());
        enter_win(&mut r);
        assert_eq!(
            handle(&mut r, ended.clone()),
            [
                Action::Leave { device: id("win") },
                Action::Ungrab {
                    warp_to: Point::new(1511.0, 500.0)
                },
                Action::ControlReturned(ReturnReason::CaptureEnded),
            ]
        );
        assert_eq!(*r.state(), State::Local);
    }

    #[test]
    fn modifiers_held_while_crossing_are_carried_and_released_locally() {
        let mut r = router();
        key(&mut r, usage::LEFT_SHIFT, true);
        assert_eq!(
            enter_win(&mut r)[1],
            Action::Enter {
                device: id("win"),
                position: Point::new(4.0, 900.0),
                modifiers: vec![usage::LEFT_SHIFT],
                buttons: vec![],
            }
        );
        // Auto-repeat of the carried key is not forwarded.
        assert!(key(&mut r, usage::LEFT_SHIFT, true).is_empty());
        assert_eq!(
            key(&mut r, usage::LEFT_SHIFT, false),
            vec![
                Action::LocalKey {
                    usage: usage::LEFT_SHIFT,
                    pressed: false,
                },
                Action::Key {
                    device: id("win"),
                    usage: usage::LEFT_SHIFT,
                    pressed: false,
                },
            ]
        );
    }

    fn spot(device: &str, display: &str, edge: Edge, along: f64) -> crate::layout::EdgeSpot {
        crate::layout::EdgeSpot {
            device: id(device),
            display: display.to_owned(),
            edge,
            along,
        }
    }

    #[test]
    fn crossing_an_edge_says_where_it_left_and_entered_first() {
        let mut r = router();
        // Out of the Mac into the PC, before the grab and the entry.
        let actions = r.handle(
            Event::Input(pointer((1511.0, 500.0), (5.0, 0.0))),
            Instant::now(),
        );
        assert_eq!(
            actions[0],
            Action::Crossed(Crossing {
                left: spot("mac", "builtin", Edge::Right, 500.0),
                entered: spot("win", "main", Edge::Left, 900.0),
            })
        );
        assert_eq!(actions[1], Action::Grab);
        assert_eq!(actions.len(), 3);
        // Up from the PC into the mini: one crossing for the hop, the
        // controller's own screen out of it.
        motion(&mut r, (0.0, 0.0), (1484.0, -899.0));
        assert_eq!(
            crossings(&mut r, Event::Input(pointer((0.0, 0.0), (0.0, -5.0)))),
            [Crossing {
                left: spot("win", "main", Edge::Top, 1488.0),
                entered: spot("mini", "ext", Edge::Bottom, 1488.0),
            }]
        );
        // Down again, and back into the Mac by its right edge: before the
        // leaving and the ungrab.
        motion(&mut r, (0.0, 0.0), (0.0, 500.0));
        motion(&mut r, (0.0, 0.0), (-1484.0, 0.0));
        let actions = r.handle(
            Event::Input(pointer((0.0, 0.0), (-10.0, 0.0))),
            Instant::now(),
        );
        assert!(
            matches!(
                &actions[..],
                [Action::Crossed(c), Action::Leave { .. }, Action::Ungrab { .. }]
                    if c.left.device == id("win") && c.left.edge == Edge::Left
                        && c.entered.device == id("mac") && c.entered.edge == Edge::Right
            ),
            "{actions:?}"
        );
    }

    #[test]
    fn every_crossing_comes_before_the_leave_and_enter_it_goes_with() {
        // The engine tells the devices where the cursor crossed by putting
        // it on the `Leave` and `Enter` that follow it in the same batch.
        let check = |actions: &[Action]| {
            let crossings: Vec<(usize, &Crossing)> = actions
                .iter()
                .enumerate()
                .filter_map(|(i, a)| match a {
                    Action::Crossed(c) => Some((i, c)),
                    _ => None,
                })
                .collect();
            assert!(crossings.len() <= 1, "{actions:?}");
            for (at, c) in crossings {
                for (i, a) in actions.iter().enumerate() {
                    match a {
                        Action::Leave { device } | Action::Enter { device, .. }
                            if *device == c.left.device || *device == c.entered.device =>
                        {
                            assert!(at < i, "{actions:?}");
                        }
                        _ => {}
                    }
                }
            }
        };
        let mut r = router();
        let each = |r: &mut Router, from: (f64, f64), delta: (f64, f64)| {
            let actions = r.handle(Event::Input(pointer(from, delta)), Instant::now());
            assert!(
                actions.iter().any(|a| matches!(a, Action::Crossed(_))),
                "{actions:?}"
            );
            check(&actions);
        };
        // Into the PC, up into the mini, down again, back into the Mac.
        each(&mut r, (1511.0, 500.0), (5.0, 0.0));
        motion(&mut r, (0.0, 0.0), (1484.0, -899.0));
        each(&mut r, (0.0, 0.0), (0.0, -5.0));
        each(&mut r, (0.0, 0.0), (0.0, 500.0));
        motion(&mut r, (0.0, 0.0), (-1484.0, 0.0));
        each(&mut r, (0.0, 0.0), (-10.0, 0.0));
    }

    #[test]
    fn only_crossing_an_edge_crosses() {
        let mut r = router();
        // A jump, there and back.
        let jumped = r.jump_to(&id("win")).unwrap();
        assert!(!jumped.iter().any(|a| matches!(a, Action::Crossed(_))));
        let back = r.jump_to(&id("mac")).unwrap();
        assert!(!back.iter().any(|a| matches!(a, Action::Crossed(_))));
        // The take-back hotkey.
        enter_win(&mut r);
        for usage in [usage::LEFT_CONTROL, usage::LEFT_ALT, usage::LEFT_SHIFT] {
            assert!(
                crossings(
                    &mut r,
                    Event::Input(InputEvent::Key {
                        usage,
                        pressed: true
                    })
                )
                .is_empty()
            );
        }
        let took_back = r.handle(
            Event::Input(InputEvent::Key {
                usage: usage::ESCAPE,
                pressed: true,
            }),
            Instant::now(),
        );
        assert!(took_back.iter().any(|a| matches!(a, Action::Ungrab { .. })));
        assert!(!took_back.iter().any(|a| matches!(a, Action::Crossed(_))));
        for usage in [
            usage::ESCAPE,
            usage::LEFT_CONTROL,
            usage::LEFT_ALT,
            usage::LEFT_SHIFT,
        ] {
            handle(
                &mut r,
                Event::Input(InputEvent::Key {
                    usage,
                    pressed: false,
                }),
            );
        }
        // Control coming back for any other reason.
        for event in [
            Event::PeerLost(id("win")),
            Event::Input(InputEvent::CaptureEnded),
            Event::RemoteActivity(id("win")),
        ] {
            r.handle(Event::PeerConnected(id("win")), Instant::now());
            enter_win(&mut r);
            let actions = r.handle(event, Instant::now());
            assert!(actions.iter().any(|a| matches!(a, Action::Ungrab { .. })));
            assert!(!actions.iter().any(|a| matches!(a, Action::Crossed(_))));
        }
        // Relative motion, even past where an edge would be.
        r.set_relative_capable(id("win"), true);
        enter_win(&mut r);
        r.set_relative(true).unwrap();
        for _ in 0..5 {
            let moved = crossings(&mut r, Event::Input(pointer((0.0, 0.0), (-2000.0, 0.0))));
            assert!(moved.is_empty());
        }
    }

    #[test]
    fn take_back_hotkey_returns_control_and_is_not_forwarded() {
        let mut r = router();
        motion(&mut r, (1200.0, 300.0), (0.0, 0.0));
        enter_win(&mut r);
        key(&mut r, usage::LEFT_CONTROL, true);
        key(&mut r, usage::LEFT_ALT, true);
        key(&mut r, usage::RIGHT_SHIFT, true);
        assert_eq!(
            key(&mut r, usage::ESCAPE, true),
            vec![
                Action::Leave { device: id("win") },
                Action::Ungrab {
                    warp_to: Point::new(1511.0, 500.0),
                },
                Action::ControlReturned(ReturnReason::Hotkey),
            ]
        );
        assert!(key(&mut r, usage::ESCAPE, false).is_empty());
    }

    #[test]
    fn losing_the_peer_returns_control_without_leave() {
        let mut r = router();
        enter_win(&mut r);
        assert_eq!(
            handle(&mut r, Event::PeerLost(id("win"))),
            vec![
                Action::Ungrab {
                    warp_to: Point::new(1511.0, 500.0),
                },
                Action::ControlReturned(ReturnReason::PeerLost(id("win"))),
            ]
        );
    }

    #[test]
    fn a_locked_screen_input_cannot_reach_is_left_alone() {
        let mut r = router();
        handle(&mut r, Event::PeerScreenLocked(id("win"), true));
        handle(&mut r, Event::PeerScreenLocked(id("mini"), true));
        // The edge holds, and neither can be jumped to.
        assert!(enter_win(&mut r).is_empty());
        assert_eq!(
            r.jump_to(&id("win")),
            Err(JumpError::ScreenLocked(id("win")))
        );
        // Like a device not connected, a locked one is passed over for the
        // next one that way.
        handle(&mut r, Event::PeerScreenLocked(id("mini"), false));
        let crossed = enter_win(&mut r);
        assert!(
            matches!(crossed.last(), Some(Action::Enter { device, .. }) if *device == id("mini")),
            "{crossed:?}"
        );
        // Unlocked, it can be entered again.
        let mut r = router();
        handle(&mut r, Event::PeerScreenLocked(id("win"), true));
        handle(&mut r, Event::PeerScreenLocked(id("win"), false));
        let crossed = enter_win(&mut r);
        assert!(
            matches!(crossed.last(), Some(Action::Enter { device, .. }) if *device == id("win")),
            "{crossed:?}"
        );
    }

    #[test]
    fn a_device_input_cannot_reach_is_left_alone_and_hands_control_back() {
        let mut r = router();
        handle(&mut r, Event::PeerInputUnavailable(id("win"), true));
        assert!(
            matches!(enter_win(&mut r).last(), Some(Action::Enter { device, .. }) if *device == id("mini")),
            "passed over for the next one that way"
        );
        let mut r = router();
        handle(&mut r, Event::PeerInputUnavailable(id("win"), true));
        handle(&mut r, Event::PeerInputUnavailable(id("mini"), true));
        assert!(enter_win(&mut r).is_empty());
        assert_eq!(
            r.jump_to(&id("win")),
            Err(JumpError::InputUnavailable(id("win")))
        );
        // Taking input again, it can be entered; becoming unreachable while
        // controlled, it gives control back.
        handle(&mut r, Event::PeerInputUnavailable(id("win"), false));
        assert!(
            matches!(enter_win(&mut r).last(), Some(Action::Enter { device, .. }) if *device == id("win"))
        );
        assert_eq!(
            handle(&mut r, Event::PeerInputUnavailable(id("win"), true)),
            vec![
                Action::Leave { device: id("win") },
                Action::Ungrab {
                    warp_to: Point::new(1511.0, 500.0),
                },
                Action::ControlReturned(ReturnReason::InputUnavailable(id("win"))),
            ]
        );
        // Losing the connection forgets it: a new session says again.
        handle(&mut r, Event::PeerLost(id("win")));
        handle(&mut r, Event::PeerConnected(id("win")));
        handle(&mut r, Event::PeerInputUnavailable(id("mini"), false));
        assert!(
            matches!(enter_win(&mut r).last(), Some(Action::Enter { device, .. }) if *device == id("win"))
        );
    }

    #[test]
    fn a_device_that_paused_sharing_is_left_alone_and_hands_control_back() {
        let mut r = router();
        handle(&mut r, Event::PeerSharingPaused(id("win"), true));
        assert!(
            matches!(enter_win(&mut r).last(), Some(Action::Enter { device, .. }) if *device == id("mini")),
            "passed over for the next one that way"
        );
        assert_eq!(r.jump_to(&id("win")), Err(JumpError::PeerPaused(id("win"))));
        assert_eq!(
            r.jump_target(&id("win")),
            Err(JumpError::PeerPaused(id("win")))
        );
        // Locked as well, the lock is told first.
        handle(&mut r, Event::PeerScreenLocked(id("win"), true));
        assert_eq!(
            r.jump_to(&id("win")),
            Err(JumpError::ScreenLocked(id("win")))
        );
        handle(&mut r, Event::PeerScreenLocked(id("win"), false));
        // Resumed, it is entered; pausing while controlled gives control
        // back, for its own reason.
        let mut r = router();
        handle(&mut r, Event::PeerSharingPaused(id("win"), true));
        handle(&mut r, Event::PeerSharingPaused(id("win"), false));
        assert!(
            matches!(enter_win(&mut r).last(), Some(Action::Enter { device, .. }) if *device == id("win"))
        );
        assert_eq!(
            handle(&mut r, Event::PeerSharingPaused(id("win"), true)),
            vec![
                Action::Leave { device: id("win") },
                Action::Ungrab {
                    warp_to: Point::new(1511.0, 500.0),
                },
                Action::ControlReturned(ReturnReason::PeerPaused(id("win"))),
            ]
        );
    }

    #[test]
    fn the_jump_target_says_where_or_why_not_and_moves_nothing() {
        let mut r = router();
        let target = r.jump_target(&id("win")).unwrap();
        assert_eq!(*r.state(), State::Local, "nothing moved");
        // Jumping there does what jump_to does.
        let mut other = router();
        assert_eq!(r.jump(target), other.jump_to(&id("win")).unwrap());
        handle(&mut r, Event::SetLocked(true));
        assert_eq!(r.jump_target(&id("win")), Err(JumpError::Locked));
        handle(&mut r, Event::SetLocked(false));
        handle(&mut r, Event::PeerLost(id("win")));
        assert_eq!(
            r.jump_target(&id("win")),
            Err(JumpError::NotConnected(id("win")))
        );
    }

    #[test]
    fn a_controlled_device_whose_screen_locks_hands_control_back() {
        let mut r = router();
        enter_win(&mut r);
        assert!(handle(&mut r, Event::PeerScreenLocked(id("mini"), true)).is_empty());
        assert_eq!(
            handle(&mut r, Event::PeerScreenLocked(id("win"), true)),
            vec![
                Action::Leave { device: id("win") },
                Action::Ungrab {
                    warp_to: Point::new(1511.0, 500.0),
                },
                Action::ControlReturned(ReturnReason::ScreenLocked(id("win"))),
            ]
        );
        // Losing the connection forgets the lock: a new session says again.
        handle(&mut r, Event::PeerLost(id("win")));
        handle(&mut r, Event::PeerConnected(id("win")));
        assert!(r.jump_to(&id("win")).is_ok());
    }

    #[test]
    fn remote_activity_returns_control() {
        let mut r = router();
        enter_win(&mut r);
        assert!(handle(&mut r, Event::RemoteActivity(id("mini"))).is_empty());
        assert_eq!(
            handle(&mut r, Event::RemoteActivity(id("win"))),
            vec![
                Action::Leave { device: id("win") },
                Action::Ungrab {
                    warp_to: Point::new(1511.0, 500.0),
                },
                Action::ControlReturned(ReturnReason::RemoteActivity(id("win"))),
            ]
        );
    }

    #[test]
    fn pausing_returns_control_and_stops_crossing() {
        let mut r = router();
        enter_win(&mut r);
        let actions = handle(&mut r, Event::SetSharing(false));
        assert_eq!(
            actions.last(),
            Some(&Action::ControlReturned(ReturnReason::SharingPaused))
        );
        assert!(enter_win(&mut r).is_empty());
        handle(&mut r, Event::SetSharing(true));
        assert_eq!(enter_win(&mut r)[0], Action::Grab);
    }

    #[test]
    fn removing_the_current_device_from_the_layout_returns_control() {
        let mut r = router();
        enter_win(&mut r);
        let devices = desk()
            .devices()
            .iter()
            .filter(|d| d.id != id("win"))
            .cloned()
            .collect();
        let actions = r.set_layout(Layout::new(devices).unwrap()).unwrap();
        assert_eq!(
            actions.last(),
            Some(&Action::ControlReturned(ReturnReason::LayoutChanged))
        );
    }

    #[test]
    fn locking_keeps_the_cursor_on_this_device() {
        let mut r = router();
        assert_eq!(
            handle(&mut r, Event::SetLocked(true)),
            vec![Action::LockChanged(true)]
        );
        assert!(handle(&mut r, Event::SetLocked(true)).is_empty());
        assert!(r.locked());
        assert!(enter_win(&mut r).is_empty());
        assert_eq!(*r.state(), State::Local);

        assert_eq!(
            handle(&mut r, Event::SetLocked(false)),
            vec![Action::LockChanged(false)]
        );
        assert_eq!(enter_win(&mut r)[0], Action::Grab);
    }

    #[test]
    fn locking_keeps_the_cursor_on_the_remote_device() {
        let mut r = router();
        enter_win(&mut r);
        handle(&mut r, Event::SetLocked(true));
        // Held at the PC's left edge instead of going back to the Mac.
        assert_eq!(
            motion(&mut r, (0.0, 0.0), (-10.0, 0.0)),
            vec![Action::Motion {
                device: id("win"),
                position: Point::new(0.0, 900.0),
            }]
        );
        // Nor does it hop up to the Mac mini.
        motion(&mut r, (0.0, 0.0), (488.0, -895.0)); // to canvas (2000, 5)
        assert_eq!(
            motion(&mut r, (0.0, 0.0), (0.0, -10.0)),
            vec![Action::Motion {
                device: id("win"),
                position: Point::new(488.0, 0.0),
            }]
        );
        assert!(matches!(r.state(), State::Remote { device, .. } if *device == id("win")));
    }

    #[test]
    fn lock_hotkey_toggles_and_is_not_forwarded() {
        let mut r = router();
        // On this device.
        assert_eq!(chord(&mut r, usage::L).1, vec![Action::LockChanged(true)]);
        // Auto-repeat does not toggle it back.
        assert!(key(&mut r, usage::L, true).is_empty());
        assert!(release_chord(&mut r, usage::L).is_empty());
        assert!(r.locked());
        assert_eq!(chord(&mut r, usage::L).1, vec![Action::LockChanged(false)]);
        release_chord(&mut r, usage::L);

        // On another device: the modifiers go there, the key does not.
        enter_win(&mut r);
        let (modifiers, l) = chord(&mut r, usage::L);
        assert_eq!(modifiers.len(), 3);
        assert_eq!(l, vec![Action::LockChanged(true)]);
        assert!(key(&mut r, usage::L, true).is_empty());
        let released = release_chord(&mut r, usage::L);
        assert!(
            !released
                .iter()
                .any(|a| matches!(a, Action::Key { usage: u, .. } if *u == usage::L)),
            "{released:?}"
        );
        assert_eq!(released.len(), 3);
        assert!(r.locked());

        // A plain L is just a key.
        assert_eq!(
            key(&mut r, usage::L, true),
            vec![Action::Key {
                device: id("win"),
                usage: usage::L,
                pressed: true,
            }]
        );
    }

    #[test]
    fn a_lock_hotkey_held_into_a_grab_is_released_locally() {
        let mut r = router();
        handle(&mut r, Event::SetLocked(true));
        assert_eq!(chord(&mut r, usage::L).1, vec![Action::LockChanged(false)]);
        // Still holding the chord, the cursor goes to the PC.
        assert_eq!(r.jump_to(&id("win")).unwrap()[0], Action::Grab);
        assert_eq!(
            key(&mut r, usage::L, false),
            vec![Action::LocalKey {
                usage: usage::L,
                pressed: false,
            }]
        );
        for m in [usage::LEFT_CONTROL, usage::LEFT_ALT, usage::LEFT_SHIFT] {
            assert_eq!(
                key(&mut r, m, false)[0],
                Action::LocalKey {
                    usage: m,
                    pressed: false,
                }
            );
        }
        // A plain L afterwards belongs to the PC.
        assert_eq!(
            key(&mut r, usage::L, true),
            vec![Action::Key {
                device: id("win"),
                usage: usage::L,
                pressed: true,
            }]
        );
    }

    #[test]
    fn a_held_back_lock_hotkey_key_is_not_released_locally() {
        // As on macOS, where capture keeps the hotkey's key from local apps.
        let mut r = router_with(RouterConfig {
            hotkeys_held_back: Hotkeys::default().reserved().into_iter().collect(),
            ..RouterConfig::default()
        });
        handle(&mut r, Event::SetLocked(true));
        assert_eq!(chord(&mut r, usage::L).1, vec![Action::LockChanged(false)]);
        assert_eq!(r.jump_to(&id("win")).unwrap()[0], Action::Grab);
        // The local session never saw L go down.
        assert!(key(&mut r, usage::L, true).is_empty());
        assert!(key(&mut r, usage::L, false).is_empty());
        // It did see the modifiers.
        for m in [usage::LEFT_CONTROL, usage::LEFT_ALT, usage::LEFT_SHIFT] {
            assert_eq!(
                key(&mut r, m, false)[0],
                Action::LocalKey {
                    usage: m,
                    pressed: false,
                }
            );
        }
        assert_eq!(
            key(&mut r, usage::L, true),
            vec![Action::Key {
                device: id("win"),
                usage: usage::L,
                pressed: true,
            }]
        );
        assert!(!r.locked());

        // Crossing an edge with the chord held is the same.
        let mut r = router_with(RouterConfig {
            hotkeys_held_back: Hotkeys::default().reserved().into_iter().collect(),
            ..RouterConfig::default()
        });
        assert_eq!(chord(&mut r, usage::L).1, vec![Action::LockChanged(true)]);
        handle(&mut r, Event::SetLocked(false));
        assert_eq!(enter_win(&mut r)[0], Action::Grab);
        assert!(key(&mut r, usage::L, false).is_empty());
        assert_eq!(
            key(&mut r, usage::LEFT_SHIFT, false)[0],
            Action::LocalKey {
                usage: usage::LEFT_SHIFT,
                pressed: false,
            }
        );
    }

    #[test]
    fn take_back_still_works_while_locked() {
        let mut r = router();
        motion(&mut r, (1200.0, 300.0), (0.0, 0.0));
        enter_win(&mut r);
        handle(&mut r, Event::SetLocked(true));
        let (_, escape) = chord(&mut r, usage::ESCAPE);
        assert_eq!(
            escape,
            vec![
                Action::Leave { device: id("win") },
                Action::Ungrab {
                    warp_to: Point::new(1511.0, 500.0),
                },
                Action::ControlReturned(ReturnReason::Hotkey),
            ]
        );
        release_chord(&mut r, usage::ESCAPE);
        assert!(r.locked());
        assert!(enter_win(&mut r).is_empty());
    }

    #[test]
    fn pausing_leaves_the_lock_as_it_was() {
        let mut r = router();
        handle(&mut r, Event::SetLocked(true));
        assert!(handle(&mut r, Event::SetSharing(false)).is_empty());
        handle(&mut r, Event::SetSharing(true));
        assert!(r.locked());
    }

    #[test]
    fn jumps_to_a_remote_device_and_back() {
        let mut r = router();
        key(&mut r, usage::LEFT_SHIFT, true);
        assert_eq!(
            r.jump_to(&id("win")).unwrap(),
            vec![
                Action::Grab,
                Action::Enter {
                    device: id("win"),
                    position: Point::new(1280.0, 720.0),
                    modifiers: vec![usage::LEFT_SHIFT],
                    buttons: vec![],
                },
            ]
        );
        assert_eq!(
            *r.state(),
            State::Remote {
                device: id("win"),
                cursor: Point::new(2792.0, 720.0),
            }
        );
        // Shift was down before the jump, so it belongs to this device too.
        assert_eq!(
            key(&mut r, usage::LEFT_SHIFT, false)[0],
            Action::LocalKey {
                usage: usage::LEFT_SHIFT,
                pressed: false,
            }
        );

        assert_eq!(
            r.jump_to(&id("win")).unwrap(),
            vec![Action::Motion {
                device: id("win"),
                position: Point::new(1280.0, 720.0),
            }]
        );
        assert_eq!(
            r.jump_to(&id("mini")).unwrap(),
            vec![
                Action::Leave { device: id("win") },
                Action::Enter {
                    device: id("mini"),
                    position: Point::new(960.0, 540.0),
                    modifiers: vec![],
                    buttons: vec![],
                },
            ]
        );
        assert_eq!(
            r.jump_to(&id("mac")).unwrap(),
            vec![
                Action::Leave { device: id("mini") },
                Action::Ungrab {
                    warp_to: Point::new(756.0, 491.0),
                },
            ]
        );
        assert_eq!(*r.state(), State::Local);
        // Already here: the cursor still goes to the middle.
        assert_eq!(
            r.jump_to(&id("mac")).unwrap(),
            vec![Action::Ungrab {
                warp_to: Point::new(756.0, 491.0),
            }]
        );
    }

    #[test]
    fn jumps_are_refused_when_they_cannot_happen() {
        let mut r = router();
        handle(&mut r, Event::SetLocked(true));
        assert_eq!(r.jump_to(&id("win")), Err(JumpError::Locked));
        assert_eq!(r.jump_to(&id("mac")), Err(JumpError::Locked));
        handle(&mut r, Event::SetLocked(false));

        handle(&mut r, Event::SetSharing(false));
        assert_eq!(r.jump_to(&id("win")), Err(JumpError::SharingPaused));
        handle(&mut r, Event::SetSharing(true));

        button(&mut r, true);
        assert_eq!(r.jump_to(&id("win")), Err(JumpError::ButtonHeld));
        button(&mut r, false);

        handle(&mut r, Event::PeerLost(id("win")));
        assert_eq!(
            r.jump_to(&id("win")),
            Err(JumpError::NotConnected(id("win")))
        );
        handle(&mut r, Event::PeerConnected(id("tablet")));
        assert_eq!(
            r.jump_to(&id("tablet")),
            Err(JumpError::NotArranged(id("tablet")))
        );
        assert_eq!(*r.state(), State::Local);
    }

    fn digit(n: u16) -> HidUsage {
        HidUsage::new(usage::DIGIT1.id() + n - 1)
    }

    #[test]
    fn jump_hotkeys_ask_for_a_device_by_number_and_are_not_forwarded() {
        let mut r = router();
        assert_eq!(chord(&mut r, digit(2)).1, vec![Action::Jump { slot: 2 }]);
        // Auto-repeat does not ask again.
        assert!(key(&mut r, digit(2), true).is_empty());
        assert!(release_chord(&mut r, digit(2)).is_empty());

        // On another device: the modifiers go there, the digit does not.
        enter_win(&mut r);
        let (modifiers, nine) = chord(&mut r, digit(9));
        assert_eq!(modifiers.len(), 3);
        assert_eq!(nine, vec![Action::Jump { slot: 9 }]);
        let released = release_chord(&mut r, digit(9));
        assert!(
            !released
                .iter()
                .any(|a| matches!(a, Action::Key { usage: u, .. } if *u == digit(9))),
            "{released:?}"
        );
        // Digit 0 and a plain digit are just keys.
        let (_, zero) = chord(&mut r, HidUsage::new(0x27));
        assert!(matches!(
            zero.as_slice(),
            [Action::Key { pressed: true, .. }]
        ));
        release_chord(&mut r, HidUsage::new(0x27));
        assert_eq!(
            key(&mut r, digit(1), true),
            vec![Action::Key {
                device: id("win"),
                usage: digit(1),
                pressed: true,
            }]
        );
    }

    #[test]
    fn jumps_turned_off_leave_the_digits_alone() {
        let mut r = router_with(RouterConfig {
            hotkeys: Hotkeys {
                jump: None,
                ..Hotkeys::default()
            },
            ..RouterConfig::default()
        });
        enter_win(&mut r);
        let (_, two) = chord(&mut r, digit(2));
        assert_eq!(
            two,
            vec![Action::Key {
                device: id("win"),
                usage: digit(2),
                pressed: true,
            }]
        );
    }

    #[test]
    fn a_jump_hotkey_held_into_the_jump_is_released_as_it_was_pressed() {
        // Where capture lets the digit through, the local session saw it.
        let mut r = router();
        assert_eq!(chord(&mut r, digit(2)).1, vec![Action::Jump { slot: 2 }]);
        r.jump_to(&id("win")).unwrap();
        assert_eq!(
            key(&mut r, digit(2), false),
            vec![Action::LocalKey {
                usage: digit(2),
                pressed: false,
            }]
        );
        // Where capture holds it back, it never did.
        let mut r = router_with(RouterConfig {
            hotkeys_held_back: Hotkeys::default().reserved().into_iter().collect(),
            ..RouterConfig::default()
        });
        chord(&mut r, digit(2));
        r.jump_to(&id("win")).unwrap();
        assert!(key(&mut r, digit(2), false).is_empty());
    }

    #[test]
    fn a_hotkey_counts_as_held_back_as_capture_last_said_before_its_key() {
        // Capture tells the change in order with the keys: the press right
        // after it goes by it.
        let all: HeldHotkeys = Hotkeys::default().reserved().into_iter().collect();
        let but_2: HeldHotkeys = all.iter().filter(|h| h.key != digit(2)).collect();
        assert_eq!((all.len(), but_2.len()), (10, 9));
        let said = |held| Event::Input(InputEvent::HotkeysHeldBack(held));
        // Another app grabbed the jump to 2: it reaches local apps.
        let mut r = router_with(RouterConfig {
            hotkeys_held_back: all,
            ..RouterConfig::default()
        });
        assert!(handle(&mut r, said(but_2)).is_empty());
        assert_eq!(chord(&mut r, digit(2)).1, vec![Action::Jump { slot: 2 }]);
        r.jump_to(&id("win")).unwrap();
        assert_eq!(
            key(&mut r, digit(2), false),
            vec![Action::LocalKey {
                usage: digit(2),
                pressed: false,
            }]
        );
        // And let go of it: held back again.
        let mut r = router_with(RouterConfig {
            hotkeys_held_back: but_2,
            ..RouterConfig::default()
        });
        assert!(handle(&mut r, said(all)).is_empty());
        assert_eq!(chord(&mut r, digit(2)).1, vec![Action::Jump { slot: 2 }]);
        r.jump_to(&id("win")).unwrap();
        assert!(key(&mut r, digit(2), false).is_empty());

        // One told before the set in place, read after it, is ignored:
        // all held back as of the fifth change, not the fourth's.
        let mut r = router_with(RouterConfig {
            hotkeys_held_back: all.as_change(5),
            ..RouterConfig::default()
        });
        assert!(handle(&mut r, said(but_2.as_change(4))).is_empty());
        chord(&mut r, digit(2));
        r.jump_to(&id("win")).unwrap();
        assert!(key(&mut r, digit(2), false).is_empty());
        r.jump_to(&id("mac")).unwrap();
        // The sixth replaces it.
        handle(&mut r, said(but_2.as_change(6)));
        r.set_hotkeys(Hotkeys::default(), all.as_change(5));
        chord(&mut r, digit(2));
        r.jump_to(&id("win")).unwrap();
        assert_eq!(
            key(&mut r, digit(2), false),
            vec![Action::LocalKey {
                usage: digit(2),
                pressed: false,
            }]
        );
    }

    #[test]
    fn only_the_hotkeys_capture_holds_back_are_not_released_locally() {
        // As on X11 with another app holding the jump to 2: the lock and
        // the other jumps are held back, that one reaches local apps.
        let held: HeldHotkeys = Hotkeys::default()
            .reserved()
            .into_iter()
            .filter(|h| h.key != digit(2))
            .collect();
        let config = RouterConfig {
            hotkeys_held_back: held,
            ..RouterConfig::default()
        };
        let mut r = router_with(config);
        assert_eq!(chord(&mut r, digit(2)).1, vec![Action::Jump { slot: 2 }]);
        r.jump_to(&id("win")).unwrap();
        assert_eq!(
            key(&mut r, digit(2), false),
            vec![Action::LocalKey {
                usage: digit(2),
                pressed: false,
            }]
        );
        let mut r = router_with(config);
        assert_eq!(chord(&mut r, digit(3)).1, vec![Action::Jump { slot: 3 }]);
        r.jump_to(&id("win")).unwrap();
        assert!(key(&mut r, digit(3), false).is_empty());
        let mut r = router_with(config);
        assert_eq!(chord(&mut r, usage::L).1, vec![Action::LockChanged(true)]);
        handle(&mut r, Event::SetLocked(false));
        assert_eq!(enter_win(&mut r)[0], Action::Grab);
        assert!(key(&mut r, usage::L, false).is_empty());

        // Pressed while controlling the PC, held through coming back and
        // going over again, it goes by what capture said too: as on X11
        // while another app's grab took the keys, holding none back, it is
        // let go of here; held back (macOS), it is not.
        for (held_back, released) in [
            (HeldHotkeys::default(), true),
            (config.hotkeys_held_back, false),
        ] {
            let mut r = router_with(RouterConfig {
                hotkeys_held_back: held_back,
                ..RouterConfig::default()
            });
            enter_win(&mut r);
            assert_eq!(chord(&mut r, usage::L).1, vec![Action::LockChanged(true)]);
            handle(&mut r, Event::SetLocked(false));
            r.jump_to(&id("mac")).unwrap();
            assert_eq!(enter_win(&mut r)[0], Action::Grab);
            let local = vec![Action::LocalKey {
                usage: usage::L,
                pressed: false,
            }];
            assert_eq!(key(&mut r, usage::L, false) == local, released);
        }
    }

    #[test]
    fn changed_hotkeys_apply_at_once_and_a_held_one_ends_as_it_began() {
        let mut r = router();
        let control_alt = Modifiers::NONE.with(Modifier::Control).with(Modifier::Alt);
        let hotkeys = Hotkeys {
            take_back: Hotkey::new(control_alt, usage::ESCAPE),
            take_back_double: false,
            lock: Hotkey::new(control_alt, usage::C),
            jump: None,
            relative: Some(Hotkeys::DEFAULT_RELATIVE),
        };
        enter_win(&mut r);
        // Control-Option-Shift-L locks, and L is still down as the hotkeys
        // change.
        assert_eq!(chord(&mut r, usage::L).1, vec![Action::LockChanged(true)]);
        r.set_hotkeys(hotkeys, HeldHotkeys::default());
        // L goes on being the old lock's: its repeat and release go nowhere.
        assert!(key(&mut r, usage::L, true).is_empty());
        assert!(key(&mut r, usage::L, false).is_empty());
        // Now the old chord is just L on the PC, the jumps are off, and
        // Control-Option-C locks.
        assert_eq!(
            key(&mut r, usage::L, true),
            vec![Action::Key {
                device: id("win"),
                usage: usage::L,
                pressed: true,
            }]
        );
        key(&mut r, usage::L, false);
        assert!(matches!(
            key(&mut r, digit(2), true).as_slice(),
            [Action::Key { .. }]
        ));
        key(&mut r, digit(2), false);
        key(&mut r, usage::LEFT_SHIFT, false);
        assert_eq!(
            key(&mut r, usage::C, true),
            vec![Action::LockChanged(false)]
        );
        key(&mut r, usage::C, false);
        // And Control-Option-Escape takes control back.
        assert_eq!(
            key(&mut r, usage::ESCAPE, true).last(),
            Some(&Action::ControlReturned(ReturnReason::Hotkey))
        );
    }

    fn switching_with(modifier: Modifier) -> Router {
        router_with(RouterConfig {
            edge: EdgePolicy {
                switch_modifier: Some(modifier),
                ..EdgePolicy::default()
            },
            ..RouterConfig::default()
        })
    }

    #[test]
    fn a_switch_modifier_must_be_held_to_cross_either_way() {
        let mut r = switching_with(Modifier::Shift);
        assert!(enter_win(&mut r).is_empty());
        assert_eq!(*r.state(), State::Local);
        // Either Shift will do. It is not pressed on the PC, which was not
        // told of it going down; its release only lets go of it here.
        key(&mut r, usage::RIGHT_SHIFT, true);
        assert_eq!(
            enter_win(&mut r),
            vec![
                Action::Grab,
                Action::Enter {
                    device: id("win"),
                    position: Point::new(4.0, 900.0),
                    modifiers: vec![],
                    buttons: vec![],
                },
            ]
        );
        assert_eq!(
            key(&mut r, usage::RIGHT_SHIFT, false),
            vec![
                Action::LocalKey {
                    usage: usage::RIGHT_SHIFT,
                    pressed: false,
                },
                Action::Key {
                    device: id("win"),
                    usage: usage::RIGHT_SHIFT,
                    pressed: false,
                },
            ]
        );
        // Without Shift the cursor stays on the PC, against its edge.
        assert_eq!(
            motion(&mut r, (0.0, 0.0), (-10.0, 0.0)),
            vec![Action::Motion {
                device: id("win"),
                position: Point::new(0.0, 900.0),
            }]
        );
        // Shift pressed on the PC is the PC's, and lets the cursor back.
        key(&mut r, usage::LEFT_SHIFT, true);
        assert_eq!(
            motion(&mut r, (0.0, 0.0), (-10.0, 0.0)),
            vec![
                Action::Leave { device: id("win") },
                Action::Ungrab {
                    warp_to: Point::new(1502.0, 500.0),
                },
            ]
        );
        key(&mut r, usage::LEFT_SHIFT, false);
        // Jumping does not need it.
        assert_eq!(r.jump_to(&id("win")).unwrap()[0], Action::Grab);
    }

    #[test]
    fn other_modifiers_held_while_crossing_still_go_along() {
        let mut r = switching_with(Modifier::Gui);
        key(&mut r, usage::LEFT_SHIFT, true);
        key(&mut r, usage::LEFT_GUI, true);
        assert_eq!(
            enter_win(&mut r)[1],
            Action::Enter {
                device: id("win"),
                position: Point::new(4.0, 900.0),
                modifiers: vec![usage::LEFT_SHIFT],
                buttons: vec![],
            }
        );
    }

    fn dwelling(ms: u32) -> Router {
        router_with(RouterConfig {
            edge: EdgePolicy {
                dwell_ms: ms,
                ..EdgePolicy::default()
            },
            ..RouterConfig::default()
        })
    }

    fn after(start: Instant, ms: u64) -> Instant {
        start + Duration::from_millis(ms)
    }

    /// Five strokes of 40 back and forth, a twentieth of a second apart
    /// from `t`, starting to the right from local position `from`: a shake
    /// once the fifth turns the fourth time. Returns what the router did
    /// for each.
    fn shake(r: &mut Router, t: Instant, from: (f64, f64)) -> Vec<Vec<Action>> {
        let mut x = from.0;
        (0..5u64)
            .map(|i| {
                let dx = if i % 2 == 0 { 40.0 } else { -40.0 };
                x += dx;
                motion_at(r, after(t, 50 * i), (x, from.1), (dx, 0.0))
            })
            .collect()
    }

    #[test]
    fn shaking_the_mouse_here_shows_the_cursor_here() {
        let mut r = router();
        let actions = shake(&mut r, Instant::now(), (700.0, 500.0));
        assert!(actions[..4].iter().all(Vec::is_empty), "{actions:?}");
        assert_eq!(
            actions[4],
            vec![Action::LocateCursor {
                device: id("mac"),
                position: Point::new(740.0, 500.0),
            }]
        );
        assert_eq!(*r.state(), State::Local);
    }

    #[test]
    fn shaking_the_mouse_on_another_device_shows_the_cursor_there() {
        let mut r = router();
        enter_win(&mut r);
        let t = Instant::now();
        motion_at(&mut r, t, (0.0, 0.0), (200.0, 0.0)); // to (204, 900) on the PC
        let actions = shake(&mut r, after(t, 10), (0.0, 0.0));
        // Input goes where it would have gone anyway; the cursor is shown
        // where the last movement left it.
        let moved = |x| Action::Motion {
            device: id("win"),
            position: Point::new(x, 900.0),
        };
        assert_eq!(
            actions,
            vec![
                vec![moved(244.0)],
                vec![moved(204.0)],
                vec![moved(244.0)],
                vec![moved(204.0)],
                vec![
                    moved(244.0),
                    Action::LocateCursor {
                        device: id("win"),
                        position: Point::new(244.0, 900.0),
                    },
                ],
            ]
        );
    }

    #[test]
    fn a_long_shake_shows_the_cursor_once() {
        let mut r = router();
        let t = Instant::now();
        let mut shown = 0;
        for burst in 0..4 {
            let actions = shake(&mut r, after(t, 250 * burst), (700.0, 500.0));
            shown += actions.concat().len();
        }
        assert_eq!(shown, 1);
    }

    #[test]
    fn dwell_holds_the_cursor_at_the_edge_until_it_is_over() {
        let mut r = dwelling(150);
        let t = Instant::now();
        let push = (1511.0, 500.0);
        assert!(!r.waiting_at_edge());
        assert!(motion_at(&mut r, t, push, (5.0, 0.0)).is_empty());
        assert!(r.waiting_at_edge());
        assert!(motion_at(&mut r, after(t, 100), push, (5.0, 0.0)).is_empty());
        // Sliding along the edge goes on pushing.
        assert!(motion_at(&mut r, after(t, 120), push, (0.0, 3.0)).is_empty());
        assert!(r.waiting_at_edge());
        assert_eq!(*r.state(), State::Local);
        assert_eq!(
            motion_at(&mut r, after(t, 150), push, (5.0, 0.0))[0],
            Action::Grab
        );
        assert!(!r.waiting_at_edge(), "crossed");
    }

    #[test]
    fn capture_letting_go_at_an_edge_starts_the_dwell_over() {
        let mut r = dwelling(150);
        let t = Instant::now();
        let push = (1511.0, 500.0);
        motion_at(&mut r, t, push, (5.0, 0.0));
        assert!(r.waiting_at_edge());
        // Capture lets go: what the cursor does next is not seen.
        assert!(handle(&mut r, Event::Input(InputEvent::CaptureEnded)).is_empty());
        assert!(!r.waiting_at_edge());
        // Back at the edge later, long after the first push: the dwell
        // starts over rather than crossing at once.
        assert!(motion_at(&mut r, after(t, 400), push, (5.0, 0.0)).is_empty());
        assert!(r.waiting_at_edge());
        assert_eq!(
            motion_at(&mut r, after(t, 550), push, (5.0, 0.0))[0],
            Action::Grab
        );
    }

    #[test]
    fn the_cursor_waits_at_an_edge_only_where_it_may_cross() {
        let mut r = dwelling(150);
        let t = Instant::now();
        let push = (1511.0, 500.0);
        motion_at(&mut r, t, push, (5.0, 0.0));
        assert!(r.waiting_at_edge());
        // Moving back from the edge.
        motion_at(&mut r, after(t, 50), (1500.0, 500.0), (-11.0, 0.0));
        assert!(!r.waiting_at_edge());
        // Locked, it crosses nowhere.
        handle(&mut r, Event::SetLocked(true));
        motion_at(&mut r, after(t, 60), push, (5.0, 0.0));
        assert!(!r.waiting_at_edge());
    }

    #[test]
    fn moving_back_from_the_edge_starts_the_dwell_over() {
        let mut r = dwelling(150);
        let t = Instant::now();
        let push = (1511.0, 500.0);
        assert!(motion_at(&mut r, t, push, (5.0, 0.0)).is_empty());
        assert!(motion_at(&mut r, after(t, 50), (1500.0, 500.0), (-11.0, 0.0)).is_empty());
        assert!(motion_at(&mut r, after(t, 160), push, (5.0, 0.0)).is_empty());
        assert!(motion_at(&mut r, after(t, 300), push, (5.0, 0.0)).is_empty());
        assert_eq!(
            motion_at(&mut r, after(t, 310), push, (5.0, 0.0))[0],
            Action::Grab
        );
    }

    #[test]
    fn dwell_applies_on_remote_devices_too() {
        let mut r = dwelling(150);
        let t = Instant::now();
        motion_at(&mut r, t, (1511.0, 500.0), (5.0, 0.0));
        motion_at(&mut r, after(t, 200), (1511.0, 500.0), (5.0, 0.0));
        assert!(matches!(r.state(), State::Remote { device, .. } if *device == id("win")));

        let t = after(t, 1000);
        assert_eq!(
            motion_at(&mut r, t, (0.0, 0.0), (-10.0, 0.0)),
            vec![Action::Motion {
                device: id("win"),
                position: Point::new(0.0, 900.0),
            }]
        );
        assert_eq!(
            motion_at(&mut r, after(t, 149), (0.0, 0.0), (-10.0, 0.0)),
            vec![Action::Motion {
                device: id("win"),
                position: Point::new(0.0, 900.0),
            }]
        );
        assert_eq!(
            motion_at(&mut r, after(t, 150), (0.0, 0.0), (-10.0, 0.0)),
            vec![
                Action::Leave { device: id("win") },
                Action::Ungrab {
                    warp_to: Point::new(1502.0, 500.0),
                },
            ]
        );
    }

    #[test]
    fn waiting_at_a_remote_edge_slides_along_it() {
        let mut r = dwelling(150);
        let t = Instant::now();
        motion_at(&mut r, t, (1511.0, 500.0), (5.0, 0.0));
        motion_at(&mut r, after(t, 200), (1511.0, 500.0), (5.0, 0.0));
        let t = after(t, 1000);
        for (ms, dy, y) in [(0, 0.0, 900.0), (50, 20.0, 920.0), (100, 20.0, 940.0)] {
            assert_eq!(
                motion_at(&mut r, after(t, ms), (0.0, 0.0), (-10.0, dy)),
                vec![Action::Motion {
                    device: id("win"),
                    position: Point::new(0.0, y),
                }]
            );
        }
    }

    #[test]
    fn dwell_can_be_changed_while_running() {
        let mut r = router();
        r.set_edge(EdgePolicy {
            dwell_ms: 150,
            ..EdgePolicy::default()
        });
        assert!(enter_win(&mut r).is_empty());
        r.set_edge(EdgePolicy::default());
        assert_eq!(enter_win(&mut r)[0], Action::Grab);
    }

    #[test]
    fn dwell_starts_over_when_the_nearest_target_changes() {
        let mut r = staggered_router(RouterConfig {
            edge: EdgePolicy {
                dwell_ms: 150,
                ..EdgePolicy::default()
            },
            ..RouterConfig::default()
        });
        let t = Instant::now();
        // High up the Mac's right edge, the push heads for the upper
        // computer; lower down, for the lower one, and the wait starts over.
        assert!(motion_at(&mut r, t, (1511.0, 100.0), (5.0, 0.0)).is_empty());
        assert!(motion_at(&mut r, after(t, 100), (1511.0, 500.0), (5.0, 0.0)).is_empty());
        assert!(motion_at(&mut r, after(t, 200), (1511.0, 500.0), (5.0, 0.0)).is_empty());
        assert_eq!(*r.state(), State::Local);
        assert_eq!(
            motion_at(&mut r, after(t, 250), (1511.0, 500.0), (5.0, 0.0)),
            vec![
                Action::Grab,
                Action::Enter {
                    device: id("lower"),
                    position: Point::new(4.0, 0.0),
                    modifiers: vec![],
                    buttons: vec![],
                },
            ]
        );
    }

    /// Presses the left button and reports a drag of files from the Mac.
    fn start_dragging(r: &mut Router) {
        button(r, true);
        assert!(handle(r, Event::DragActive(true)).is_empty());
        assert!(r.dragging());
    }

    /// The left button let go of at the Mac's right edge, where
    /// [`enter_win`] leaves.
    fn left_up_here() -> Action {
        Action::LocalButton {
            button: Button::Left,
            pressed: false,
            position: Point::new(1511.0, 500.0),
        }
    }

    #[test]
    fn dragging_files_crosses_with_the_button_and_drops_where_released() {
        let mut r = router();
        start_dragging(&mut r);
        // The button goes up here as the drag leaves, before input is
        // grabbed, and down on the PC.
        assert_eq!(
            enter_win(&mut r),
            vec![
                left_up_here(),
                Action::Grab,
                Action::Enter {
                    device: id("win"),
                    position: Point::new(4.0, 900.0),
                    modifiers: vec![],
                    buttons: vec![Button::Left],
                },
            ]
        );
        // The drop: the release reaches the PC first.
        assert_eq!(
            button(&mut r, false),
            vec![
                Action::Button {
                    device: id("win"),
                    button: Button::Left,
                    pressed: false,
                    position: Some(Point::new(4.0, 900.0)),
                },
                Action::DragReleased { device: id("win") },
            ]
        );
        assert!(!r.dragging());
    }

    #[test]
    fn a_drag_hops_between_devices_and_back_letting_go_here_once() {
        let mut r = router();
        start_dragging(&mut r);
        enter_win(&mut r);
        motion(&mut r, (0.0, 0.0), (484.0, -895.0)); // to canvas (2000, 5)
        assert_eq!(
            motion(&mut r, (0.0, 0.0), (0.0, -10.0)),
            vec![
                Action::Leave { device: id("win") },
                Action::Enter {
                    device: id("mini"),
                    position: Point::new(488.0, 1075.0),
                    modifiers: vec![],
                    buttons: vec![Button::Left],
                },
            ]
        );
        // Back down and home: the button stays up here.
        motion(&mut r, (0.0, 0.0), (0.0, 10.0));
        motion(&mut r, (0.0, 0.0), (-484.0, 895.0));
        assert_eq!(
            motion(&mut r, (0.0, 0.0), (-10.0, 0.0)),
            vec![
                Action::Leave { device: id("win") },
                Action::Ungrab {
                    warp_to: Point::new(1506.0, 500.0),
                },
            ]
        );
        // Out again: it was let go of here already.
        assert!(matches!(
            enter_win(&mut r).as_slice(),
            [Action::Grab, Action::Enter { buttons, .. }] if *buttons == [Button::Left]
        ));
        motion(&mut r, (0.0, 0.0), (-10.0, 0.0));
        // Released here: nowhere takes the files.
        assert_eq!(
            button(&mut r, false),
            vec![Action::DragReleased { device: id("mac") }]
        );
    }

    #[test]
    fn a_drag_reported_once_the_button_is_up_ends_at_once() {
        let mut r = router();
        assert_eq!(
            handle(&mut r, Event::DragActive(true)),
            vec![Action::DragReleased { device: id("mac") }]
        );
        assert!(!r.dragging());
        assert!(matches!(
            enter_win(&mut r).as_slice(),
            [Action::Grab, Action::Enter { buttons, .. }] if buttons.is_empty()
        ));
    }

    #[test]
    fn control_coming_back_ends_a_drag() {
        let mut r = router();
        start_dragging(&mut r);
        enter_win(&mut r);
        assert_eq!(
            handle(&mut r, Event::PeerLost(id("win"))),
            vec![
                Action::Ungrab {
                    warp_to: Point::new(1511.0, 500.0),
                },
                Action::ControlReturned(ReturnReason::PeerLost(id("win"))),
                Action::DragReleased { device: id("mac") },
            ]
        );
        assert!(button(&mut r, false).is_empty());
    }

    #[test]
    fn a_drag_called_off_keeps_the_button_here_again() {
        let mut r = router();
        start_dragging(&mut r);
        assert!(handle(&mut r, Event::DragActive(false)).is_empty());
        assert!(enter_win(&mut r).is_empty());
        // Nor can a held button jump, drag or not.
        start_dragging_again(&mut r);
        assert_eq!(r.jump_to(&id("win")), Err(JumpError::ButtonHeld));
    }

    fn start_dragging_again(r: &mut Router) {
        assert!(handle(r, Event::DragActive(true)).is_empty());
    }
    /// On the PC, presses the left button there and reports a drag of
    /// files from it.
    fn start_dragging_on_win(r: &mut Router) {
        enter_win(r);
        assert!(matches!(
            button(r, true).as_slice(),
            [Action::Button { device, pressed: true, .. }] if *device == id("win")
        ));
        assert!(handle(r, Event::RemoteDrag(id("win"), true)).is_empty());
        assert!(r.dragging());
        assert_eq!(r.drag_source(), Some(&id("win")));
    }

    fn left_up_on_win() -> Action {
        Action::Button {
            device: id("win"),
            button: Button::Left,
            pressed: false,
            position: Some(Point::new(4.0, 900.0)),
        }
    }

    #[test]
    fn a_drag_on_the_device_controlled_comes_back_here() {
        let mut r = router();
        start_dragging_on_win(&mut r);
        // The PC lets go of the button as the cursor leaves it; nothing is
        // let go of here, which never saw it go down. It may be dropped here.
        assert_eq!(
            motion(&mut r, (0.0, 0.0), (-10.0, 0.0)),
            vec![
                Action::Leave { device: id("win") },
                Action::Ungrab {
                    warp_to: Point::new(1506.0, 500.0),
                },
                Action::DragHere { source: id("win") },
            ]
        );
        // Released here: this device's app takes the drop.
        assert_eq!(
            button(&mut r, false),
            vec![Action::DragReleased { device: id("mac") }]
        );
        assert!(!r.dragging());
        assert_eq!(r.drag_source(), None);
    }

    #[test]
    fn a_drag_on_the_device_controlled_goes_on_to_another() {
        let mut r = router();
        start_dragging_on_win(&mut r);
        motion(&mut r, (0.0, 0.0), (484.0, -895.0)); // to canvas (2000, 5)
        assert_eq!(
            motion(&mut r, (0.0, 0.0), (0.0, -10.0)),
            vec![
                Action::Leave { device: id("win") },
                Action::Enter {
                    device: id("mini"),
                    position: Point::new(488.0, 1075.0),
                    modifiers: vec![],
                    buttons: vec![Button::Left],
                },
            ]
        );
        assert_eq!(
            button(&mut r, false),
            vec![
                Action::Button {
                    device: id("mini"),
                    button: Button::Left,
                    pressed: false,
                    position: Some(Point::new(488.0, 1075.0)),
                },
                Action::DragReleased { device: id("mini") },
            ]
        );
    }

    #[test]
    fn a_drag_brought_here_from_the_device_controlled_is_let_go_of_as_it_leaves() {
        let mut r = router();
        start_dragging_on_win(&mut r);
        motion(&mut r, (0.0, 0.0), (-10.0, 0.0));
        assert!(handle(&mut r, Event::DragHeldHere).is_empty());
        // This device's app pressed the button for its own drag: going
        // back to the PC lets go of it here first, and presses it there.
        assert_eq!(
            motion(&mut r, (1506.0, 500.0), (10.0, 0.0)),
            vec![
                Action::LocalButton {
                    button: Button::Left,
                    pressed: false,
                    position: Point::new(1506.0, 500.0),
                },
                Action::Grab,
                Action::Enter {
                    device: id("win"),
                    position: Point::new(4.0, 900.0),
                    modifiers: vec![],
                    buttons: vec![Button::Left],
                },
            ]
        );
        // Released on the device it began on: no drop anywhere else.
        assert_eq!(
            button(&mut r, false),
            vec![left_up_on_win(), Action::DragReleased { device: id("win") }]
        );
    }

    #[test]
    fn a_drag_brought_here_but_not_taken_up_here_lets_go_of_nothing_here() {
        let mut r = router();
        start_dragging_on_win(&mut r);
        // No app here took it up: the button never went down here.
        motion(&mut r, (0.0, 0.0), (-10.0, 0.0));
        assert_eq!(
            motion(&mut r, (1506.0, 500.0), (10.0, 0.0)),
            vec![
                Action::Grab,
                Action::Enter {
                    device: id("win"),
                    position: Point::new(4.0, 900.0),
                    modifiers: vec![],
                    buttons: vec![Button::Left],
                },
            ]
        );
        // Nor does a press reported while elsewhere count.
        assert!(handle(&mut r, Event::DragHeldHere).is_empty());
        motion(&mut r, (0.0, 0.0), (-10.0, 0.0));
        assert!(!matches!(
            motion(&mut r, (1506.0, 500.0), (10.0, 0.0)).first(),
            Some(Action::LocalButton { .. })
        ));
    }

    #[test]
    fn a_drag_is_carried_only_from_the_device_the_cursor_is_on_with_a_button_held() {
        let mut r = router();
        assert!(!r.can_carry_drag_from(&id("win")));
        enter_win(&mut r);
        assert!(!r.can_carry_drag_from(&id("win")));
        button(&mut r, true);
        assert!(r.can_carry_drag_from(&id("win")));
        assert!(!r.can_carry_drag_from(&id("mini")));
        assert!(!r.can_carry_drag_from(&id("mac")));
        // Nor while a drag of this device's own goes on there.
        let mut r = router();
        start_dragging(&mut r);
        enter_win(&mut r);
        assert!(!r.can_carry_drag_from(&id("win")));
    }

    #[test]
    fn a_drag_comes_here_only_by_crossing() {
        // The keyboard and mouse coming back otherwise end it here.
        let mut r = router();
        start_dragging_on_win(&mut r);
        let actions = handle(&mut r, Event::RemoteActivity(id("win")));
        assert!(
            !actions.iter().any(|a| matches!(a, Action::DragHere { .. })),
            "{actions:?}"
        );
        assert_eq!(
            actions.last(),
            Some(&Action::DragReleased { device: id("mac") })
        );
        // The take-back hotkey: the same.
        let mut r = router();
        start_dragging_on_win(&mut r);
        let (_, actions) = chord(&mut r, usage::ESCAPE);
        assert!(
            !actions.iter().any(|a| matches!(a, Action::DragHere { .. })),
            "{actions:?}"
        );
        assert_eq!(
            actions.last(),
            Some(&Action::DragReleased { device: id("mac") })
        );
        // And a jump cannot take it anywhere: the button is held.
        let mut r = router();
        start_dragging_on_win(&mut r);
        assert_eq!(r.jump_to(&id("mac")), Err(JumpError::ButtonHeld));
        assert!(r.dragging());
    }

    #[test]
    fn a_button_held_on_the_device_controlled_stays_there_without_a_drag() {
        let mut r = router();
        enter_win(&mut r);
        button(&mut r, true);
        assert_eq!(
            motion(&mut r, (0.0, 0.0), (-10.0, 0.0)),
            vec![Action::Motion {
                device: id("win"),
                position: Point::new(0.0, 900.0),
            }]
        );
        // Called off: the button is held there again.
        assert!(handle(&mut r, Event::RemoteDrag(id("win"), true)).is_empty());
        assert!(handle(&mut r, Event::RemoteDrag(id("win"), false)).is_empty());
        assert!(!r.dragging());
        assert!(matches!(
            motion(&mut r, (0.0, 0.0), (-10.0, 0.0)).as_slice(),
            [Action::Motion { .. }]
        ));
        // A drag from here ending says nothing about the PC's.
        assert!(handle(&mut r, Event::RemoteDrag(id("win"), true)).is_empty());
        assert!(handle(&mut r, Event::DragActive(false)).is_empty());
        assert!(r.dragging());
    }

    #[test]
    fn a_drag_reported_late_or_from_elsewhere_ends_at_once() {
        let mut r = router();
        // The cursor is not on the PC.
        assert_eq!(
            handle(&mut r, Event::RemoteDrag(id("win"), true)),
            vec![Action::DragReleased { device: id("mac") }]
        );
        // On it, but the button is up already.
        enter_win(&mut r);
        assert_eq!(
            handle(&mut r, Event::RemoteDrag(id("win"), true)),
            vec![Action::DragReleased { device: id("win") }]
        );
        // Another device's drag, while the cursor is on the PC.
        button(&mut r, true);
        assert_eq!(
            handle(&mut r, Event::RemoteDrag(id("mini"), true)),
            vec![Action::DragReleased { device: id("win") }]
        );
        assert!(!r.dragging());
    }

    #[test]
    fn a_drag_on_the_device_controlled_keeps_to_the_lock() {
        let mut r = router();
        start_dragging_on_win(&mut r);
        handle(&mut r, Event::SetLocked(true));
        assert!(matches!(
            motion(&mut r, (0.0, 0.0), (-10.0, 0.0)).as_slice(),
            [Action::Motion { .. }]
        ));
        // A screen locked where input cannot reach is passed over.
        handle(&mut r, Event::SetLocked(false));
        handle(&mut r, Event::PeerScreenLocked(id("mini"), true));
        motion(&mut r, (0.0, 0.0), (484.0, -895.0));
        assert!(matches!(
            motion(&mut r, (0.0, 0.0), (0.0, -10.0)).as_slice(),
            [Action::Motion { .. }]
        ));
        assert!(r.dragging());
    }

    #[test]
    fn losing_the_device_dragged_from_ends_its_drag() {
        let mut r = router();
        start_dragging_on_win(&mut r);
        let actions = handle(&mut r, Event::PeerLost(id("win")));
        assert_eq!(
            actions.last(),
            Some(&Action::DragReleased { device: id("mac") })
        );
        assert!(!r.dragging());
    }

    /// A router on the PC, which takes relative movements.
    fn relative_router() -> Router {
        let mut r = router();
        r.set_relative_capable(id("win"), true);
        r
    }

    fn started(cause: RelativeStart) -> Action {
        Action::RelativeStarted {
            device: id("win"),
            cause,
        }
    }

    fn ended(reason: RelativeEnd) -> Action {
        Action::RelativeEnded {
            device: id("win"),
            reason,
        }
    }

    fn moved_by(dx: f64, dy: f64) -> Action {
        Action::RelativeMotion {
            device: id("win"),
            delta: Point::new(dx, dy),
        }
    }

    #[test]
    fn relative_mode_sends_movements_and_crosses_no_edge() {
        let mut r = relative_router();
        enter_win(&mut r);
        assert_eq!(
            r.set_relative(true),
            Ok(vec![started(RelativeStart::Asked)])
        );
        assert_eq!(r.relative(), Some((&id("win"), RelativeStart::Asked)));
        // Back towards the Mac, far beyond the PC's left edge: it stays on
        // the PC and the movement goes as such.
        assert_eq!(
            motion(&mut r, (0.0, 0.0), (-500.0, 3.5)),
            vec![moved_by(-500.0, 3.5)]
        );
        assert_eq!(
            motion(&mut r, (0.0, 0.0), (-2.0, 0.0)),
            vec![moved_by(-2.0, 0.0)]
        );
        assert_eq!(
            *r.state(),
            State::Remote {
                device: id("win"),
                cursor: Point::new(1516.0, 900.0),
            }
        );
        // Asking again changes nothing; off goes back to positions.
        assert_eq!(r.set_relative(true), Ok(vec![]));
        assert_eq!(
            r.set_relative(false),
            Ok(vec![ended(RelativeEnd::TurnedOff)])
        );
        assert_eq!(r.relative(), None);
        // Once the PC says where its pointer is.
        assert!(handle(&mut r, Event::PointerAt(id("win"), Point::new(4.0, 900.0))).is_empty());
        assert_eq!(
            motion(&mut r, (0.0, 0.0), (10.0, 0.0)),
            vec![Action::Motion {
                device: id("win"),
                position: Point::new(14.0, 900.0),
            }]
        );
    }

    #[test]
    fn relative_mode_needs_a_remote_device_that_takes_it() {
        let mut r = router();
        assert_eq!(r.set_relative(true), Err(RelativeError::NotControlling));
        assert_eq!(r.set_relative(false), Ok(vec![]));
        enter_win(&mut r);
        assert_eq!(
            r.set_relative(true),
            Err(RelativeError::NotSupported(id("win")))
        );
        // Nor does an app holding the pointer there do it, or its hotkey,
        // whose key then goes there as any other, and is told of.
        assert!(handle(&mut r, Event::PointerCaptured(id("win"), true)).is_empty());
        assert_eq!(
            chord(&mut r, usage::G).1,
            vec![
                Action::RelativeUnsupported { device: id("win") },
                Action::Key {
                    device: id("win"),
                    usage: usage::G,
                    pressed: true,
                },
            ]
        );
        assert_eq!(
            key(&mut r, usage::G, false),
            vec![Action::Key {
                device: id("win"),
                usage: usage::G,
                pressed: false,
            }]
        );
        assert_eq!(r.relative(), None);
    }

    #[test]
    fn relative_hotkey_toggles_and_is_not_forwarded() {
        let mut r = relative_router();
        // Here it is just G for this device's apps.
        assert_eq!(chord(&mut r, usage::G).1, vec![]);
        release_chord(&mut r, usage::G);
        enter_win(&mut r);
        let (_, on) = chord(&mut r, usage::G);
        assert_eq!(on, vec![started(RelativeStart::Hotkey)]);
        // Its repeat and release go nowhere.
        assert!(key(&mut r, usage::G, true).is_empty());
        assert!(key(&mut r, usage::G, false).is_empty());
        assert_eq!(
            key(&mut r, usage::G, true),
            vec![ended(RelativeEnd::TurnedOff)]
        );
        assert!(key(&mut r, usage::G, false).is_empty());
        // Other keys still go to the PC.
        assert_eq!(
            key(&mut r, usage::A, true),
            vec![Action::Key {
                device: id("win"),
                usage: usage::A,
                pressed: true,
            }]
        );
    }

    #[test]
    fn taking_control_back_ends_relative_mode() {
        let mut r = relative_router();
        enter_win(&mut r);
        r.set_relative(true).unwrap();
        let (_, back) = chord(&mut r, usage::ESCAPE);
        assert_eq!(
            back,
            vec![
                ended(RelativeEnd::Returned),
                Action::Leave { device: id("win") },
                Action::Ungrab {
                    warp_to: Point::new(1511.0, 500.0),
                },
                Action::ControlReturned(ReturnReason::Hotkey),
            ]
        );
        assert_eq!(r.relative(), None);
        // And here movements are positions again.
        assert!(motion(&mut r, (700.0, 500.0), (10.0, 0.0)).is_empty());
    }

    #[test]
    fn jumping_ends_relative_mode_then_jumps() {
        let mut r = relative_router();
        enter_win(&mut r);
        r.set_relative(true).unwrap();
        let jumped = r.jump_to(&id("mini")).unwrap();
        assert_eq!(jumped[0], ended(RelativeEnd::Jumped));
        assert_eq!(jumped[1], Action::Leave { device: id("win") });
        assert!(matches!(jumped[2], Action::Enter { ref device, .. } if *device == id("mini")));
        assert_eq!(jumped.len(), 3);
        // Onto the device it is on: nothing happens, relative mode stays.
        let mut r = relative_router();
        enter_win(&mut r);
        r.set_relative(true).unwrap();
        assert_eq!(r.jump_to(&id("win")), Ok(vec![]));
        assert_eq!(r.relative(), Some((&id("win"), RelativeStart::Asked)));
        // Out of relative mode, the cursor goes to the middle, and enters
        // it where an app there holds the pointer, as on any entry: here
        // one that held it all along, with relative mode turned off by hand.
        r.set_relative(false).unwrap();
        handle(&mut r, Event::PointerCaptured(id("win"), true));
        assert_eq!(r.relative(), Some((&id("win"), RelativeStart::Captured)));
        r.set_relative(false).unwrap();
        let jumped = r.jump_to(&id("win")).unwrap();
        assert!(matches!(jumped[0], Action::Motion { .. }), "{jumped:?}");
        assert_eq!(jumped[1], started(RelativeStart::Captured));
        assert_eq!(jumped.len(), 2);
    }

    #[test]
    fn the_cursor_carries_on_from_where_relative_mode_left_the_pointer() {
        let mut r = relative_router();
        enter_win(&mut r);
        r.set_relative(true).unwrap();
        motion(&mut r, (0.0, 0.0), (300.0, 50.0));
        let now = Instant::now();
        assert_eq!(
            r.set_relative(false),
            Ok(vec![ended(RelativeEnd::TurnedOff)])
        );
        // Movements wait for the PC to say where its pointer is...
        assert!(motion_at(&mut r, now, (0.0, 0.0), (10.0, 0.0)).is_empty());
        assert!(motion_at(&mut r, now, (0.0, 0.0), (0.0, 5.0)).is_empty());
        // ...and go on from there: (304, 950) on the PC, not (4, 900).
        assert_eq!(
            r.handle(Event::PointerAt(id("win"), Point::new(304.0, 950.0)), now),
            vec![Action::Motion {
                device: id("win"),
                position: Point::new(314.0, 955.0),
            }]
        );
        assert_eq!(
            motion_at(&mut r, now, (0.0, 0.0), (1.0, 0.0)),
            vec![Action::Motion {
                device: id("win"),
                position: Point::new(315.0, 955.0),
            }]
        );
        // Said again, or by another device, it changes nothing.
        assert!(handle(&mut r, Event::PointerAt(id("win"), Point::new(9.0, 9.0))).is_empty());

        // Ended by its hotkey too; a pointer off the screens is brought
        // back onto them.
        r.set_relative(true).unwrap();
        chord(&mut r, usage::G);
        assert!(motion_at(&mut r, now, (0.0, 0.0), (0.0, 1.0)).is_empty());
        assert_eq!(
            r.handle(Event::PointerAt(id("win"), Point::new(-50.0, 100.0)), now),
            vec![Action::Motion {
                device: id("win"),
                position: Point::new(0.0, 101.0),
            }]
        );
    }

    #[test]
    fn a_click_as_relative_mode_ends_goes_where_the_pointer_is() {
        let mut r = relative_router();
        enter_win(&mut r);
        let at = |position: Option<(f64, f64)>, pressed: bool| Action::Button {
            device: id("win"),
            button: Button::Left,
            pressed,
            position: position.map(|(x, y)| Point::new(x, y)),
        };
        // In relative mode, and until the PC says where its pointer went,
        // the cursor here says nothing of where it is there.
        r.set_relative(true).unwrap();
        assert_eq!(button(&mut r, true), vec![at(None, true)]);
        assert_eq!(button(&mut r, false), vec![at(None, false)]);
        r.set_relative(false).unwrap();
        assert_eq!(button(&mut r, true), vec![at(None, true)]);
        assert_eq!(button(&mut r, false), vec![at(None, false)]);
        handle(
            &mut r,
            Event::PointerAt(id("win"), Point::new(300.0, 400.0)),
        );
        assert_eq!(button(&mut r, true), vec![at(Some((300.0, 400.0)), true)]);
    }

    #[test]
    fn turning_automatic_relative_mode_off_ends_what_it_began() {
        let mut r = relative_router();
        enter_win(&mut r);
        handle(&mut r, Event::PointerCaptured(id("win"), true));
        assert_eq!(r.relative(), Some((&id("win"), RelativeStart::Captured)));
        assert_eq!(
            r.set_relative_auto(false),
            vec![ended(RelativeEnd::AutoOff)]
        );
        assert_eq!(r.relative(), None);
        // Turned on by hand, it stays, and the app letting go changes
        // nothing.
        r.set_relative(true).unwrap();
        assert_eq!(r.set_relative_auto(false), vec![]);
        assert!(handle(&mut r, Event::PointerCaptured(id("win"), false)).is_empty());
        assert_eq!(r.relative(), Some((&id("win"), RelativeStart::Asked)));
    }

    #[test]
    fn a_device_that_never_says_where_its_pointer_is_holds_the_cursor_only_briefly() {
        let mut r = relative_router();
        enter_win(&mut r);
        r.set_relative(true).unwrap();
        r.set_relative(false).unwrap();
        let now = Instant::now();
        assert!(motion_at(&mut r, now, (0.0, 0.0), (10.0, 0.0)).is_empty());
        // Then on from where it was, with what waited.
        assert_eq!(
            motion_at(&mut r, now + RESYNC_WAIT, (0.0, 0.0), (5.0, 0.0)),
            vec![Action::Motion {
                device: id("win"),
                position: Point::new(19.0, 900.0),
            }]
        );
        // Too late, it changes nothing.
        assert!(
            r.handle(Event::PointerAt(id("win"), Point::new(300.0, 300.0)), now)
                .is_empty()
        );
        // Leaving the device, or jumping, waits for nothing.
        r.set_relative(true).unwrap();
        let jumped = r.jump_to(&id("mini")).unwrap();
        assert_eq!(jumped[0], ended(RelativeEnd::Jumped));
        assert!(!motion_at(&mut r, now, (0.0, 0.0), (1.0, 0.0)).is_empty());
    }

    #[test]
    fn losing_the_device_pausing_or_its_screen_locking_ends_relative_mode() {
        for (event, cause) in [
            (Event::PeerLost(id("win")), RelativeEnd::Lost),
            (Event::SetSharing(false), RelativeEnd::Paused),
            (
                Event::PeerScreenLocked(id("win"), true),
                RelativeEnd::Returned,
            ),
            (Event::RemoteActivity(id("win")), RelativeEnd::Returned),
        ] {
            let mut r = relative_router();
            enter_win(&mut r);
            r.set_relative(true).unwrap();
            let actions = handle(&mut r, event.clone());
            assert_eq!(actions[0], ended(cause), "{event:?}");
            assert!(
                actions.contains(&Action::Ungrab {
                    warp_to: Point::new(1511.0, 500.0),
                }),
                "{event:?}"
            );
            assert_eq!(r.relative(), None);
        }
    }

    #[test]
    fn the_lock_and_relative_mode_go_their_own_ways() {
        let mut r = relative_router();
        enter_win(&mut r);
        r.set_relative(true).unwrap();
        assert_eq!(chord(&mut r, usage::L).1, vec![Action::LockChanged(true)]);
        release_chord(&mut r, usage::L);
        assert_eq!(
            motion(&mut r, (0.0, 0.0), (-50.0, 0.0)),
            vec![moved_by(-50.0, 0.0)]
        );
        assert_eq!(chord(&mut r, usage::L).1, vec![Action::LockChanged(false)]);
        assert_eq!(r.relative(), Some((&id("win"), RelativeStart::Asked)));
    }

    #[test]
    fn a_device_holding_the_pointer_enters_and_leaves_relative_mode() {
        let mut r = relative_router();
        // Held before the cursor comes: entering puts it in relative mode.
        assert!(handle(&mut r, Event::PointerCaptured(id("win"), true)).is_empty());
        let entered = enter_win(&mut r);
        assert_eq!(entered.last(), Some(&started(RelativeStart::Captured)));
        // Let go of: relative mode ends.
        assert_eq!(
            handle(&mut r, Event::PointerCaptured(id("win"), false)),
            vec![ended(RelativeEnd::Released)]
        );
        // Held again while the cursor is there.
        assert_eq!(
            handle(&mut r, Event::PointerCaptured(id("win"), true)),
            vec![started(RelativeStart::Captured)]
        );
        // Another device holding its pointer changes nothing here.
        assert!(handle(&mut r, Event::PointerCaptured(id("mini"), true)).is_empty());
    }

    #[test]
    fn a_hand_turned_relative_mode_stays_until_the_device_next_changes() {
        let mut r = relative_router();
        enter_win(&mut r);
        handle(&mut r, Event::PointerCaptured(id("win"), true));
        // Turned off by hand while the game holds the pointer...
        assert_eq!(
            r.set_relative(false),
            Ok(vec![ended(RelativeEnd::TurnedOff)])
        );
        // ...the same report again does not turn it back on.
        assert!(handle(&mut r, Event::PointerCaptured(id("win"), true)).is_empty());
        assert_eq!(r.relative(), None);
        // Turned on by hand, letting go turns it off: the next change wins.
        r.set_relative(true).unwrap();
        handle(&mut r, Event::PointerCaptured(id("win"), false));
        assert_eq!(r.relative(), None);
    }

    #[test]
    fn without_automatic_relative_mode_holding_the_pointer_changes_nothing() {
        let mut r = relative_router();
        r.set_relative_auto(false);
        handle(&mut r, Event::PointerCaptured(id("win"), true));
        let entered = enter_win(&mut r);
        assert!(
            !entered
                .iter()
                .any(|a| matches!(a, Action::RelativeStarted { .. }))
        );
        assert!(handle(&mut r, Event::PointerCaptured(id("win"), false)).is_empty());
        // Turned on by hand, letting go leaves it on.
        r.set_relative(true).unwrap();
        assert!(handle(&mut r, Event::PointerCaptured(id("win"), true)).is_empty());
        assert!(handle(&mut r, Event::PointerCaptured(id("win"), false)).is_empty());
        assert_eq!(r.relative(), Some((&id("win"), RelativeStart::Asked)));
    }

    #[test]
    fn swinging_the_mouse_in_relative_mode_is_no_shake() {
        let mut r = relative_router();
        enter_win(&mut r);
        r.set_relative(true).unwrap();
        let actions = shake(&mut r, Instant::now(), (0.0, 0.0));
        assert!(
            actions
                .iter()
                .flatten()
                .all(|a| matches!(a, Action::RelativeMotion { .. })),
            "{actions:?}"
        );
    }

    fn double_press_router() -> Router {
        router_with(RouterConfig {
            hotkeys: Hotkeys {
                take_back_double: true,
                ..Hotkeys::default()
            },
            ..RouterConfig::default()
        })
    }

    /// A router on the Windows PC with the double press on, and a moment
    /// to count from.
    fn fresh() -> (Router, Instant) {
        let mut r = double_press_router();
        let start = on_win(&mut r);
        (r, start)
    }

    /// The router in the Windows PC, and a moment to count from.
    fn on_win(r: &mut Router) -> Instant {
        enter_win(r);
        Instant::now()
    }

    fn key_at(r: &mut Router, usage: HidUsage, pressed: bool, at: Instant) -> Vec<Action> {
        but_crossings(r.handle(Event::Input(InputEvent::Key { usage, pressed }), at))
    }

    /// Presses and lets go of `usage` from `down` to `up` milliseconds
    /// after `start`: what the router did for the release.
    fn tap(r: &mut Router, usage: HidUsage, start: Instant, down: u64, up: u64) -> Vec<Action> {
        let ms = Duration::from_millis;
        key_at(r, usage, true, start + ms(down));
        key_at(r, usage, false, start + ms(up))
    }

    fn sent(usage: HidUsage, pressed: bool) -> Action {
        Action::Key {
            device: id("win"),
            usage,
            pressed,
        }
    }

    fn took_back(actions: &[Action]) -> bool {
        actions.contains(&Action::ControlReturned(ReturnReason::Hotkey))
    }

    #[test]
    fn control_pressed_twice_quickly_takes_control_back_once_let_go_of() {
        let mut r = double_press_router();
        motion(&mut r, (1200.0, 300.0), (0.0, 0.0));
        let start = on_win(&mut r);
        let ms = Duration::from_millis;
        // Each press and release goes to the PC as usual.
        assert_eq!(
            key_at(&mut r, usage::LEFT_CONTROL, true, start),
            vec![sent(usage::LEFT_CONTROL, true)]
        );
        assert_eq!(
            key_at(&mut r, usage::LEFT_CONTROL, false, start + ms(80)),
            vec![sent(usage::LEFT_CONTROL, false)]
        );
        assert_eq!(
            key_at(&mut r, usage::LEFT_CONTROL, true, start + ms(160)),
            vec![sent(usage::LEFT_CONTROL, true)]
        );
        // The second release goes too, before the cursor leaves.
        assert_eq!(
            key_at(&mut r, usage::LEFT_CONTROL, false, start + ms(240)),
            vec![
                sent(usage::LEFT_CONTROL, false),
                Action::Leave { device: id("win") },
                Action::Ungrab {
                    warp_to: Point::new(1511.0, 500.0),
                },
                Action::ControlReturned(ReturnReason::Hotkey),
            ]
        );
        assert_eq!(*r.state(), State::Local);

        // Either Control key, the same one both times; 400 ms at most from
        // the first press to the second release.
        let start = on_win(&mut r);
        assert!(!took_back(&tap(
            &mut r,
            usage::RIGHT_CONTROL,
            start,
            0,
            100
        )));
        assert!(took_back(&tap(
            &mut r,
            usage::RIGHT_CONTROL,
            start,
            300,
            400
        )));
        let start = on_win(&mut r);
        tap(&mut r, usage::LEFT_CONTROL, start, 0, 100);
        assert!(!took_back(&tap(
            &mut r,
            usage::LEFT_CONTROL,
            start,
            300,
            401
        )));
        assert!(matches!(r.state(), State::Remote { device, .. } if *device == id("win")));
    }

    #[test]
    fn control_twice_counts_only_alone_and_quick() {
        let ms = Duration::from_millis;

        // Another key between.
        let (mut r, start) = fresh();
        tap(&mut r, usage::LEFT_CONTROL, start, 0, 50);
        tap(&mut r, usage::A, start, 80, 100);
        assert!(!took_back(&tap(
            &mut r,
            usage::LEFT_CONTROL,
            start,
            120,
            160
        )));
        // Held for a while: too slow to make two with the next.
        let (mut r, start) = fresh();
        tap(&mut r, usage::LEFT_CONTROL, start, 0, 300);
        assert!(!took_back(&tap(
            &mut r,
            usage::LEFT_CONTROL,
            start,
            350,
            450
        )));
        // The second press can be the first of the next two.
        assert!(took_back(&tap(
            &mut r,
            usage::LEFT_CONTROL,
            start,
            500,
            600
        )));

        // With another modifier held, or Control as part of a combination.
        let (mut r, start) = fresh();
        key_at(&mut r, usage::LEFT_SHIFT, true, start);
        tap(&mut r, usage::LEFT_CONTROL, start, 10, 50);
        assert!(!took_back(&tap(
            &mut r,
            usage::LEFT_CONTROL,
            start,
            80,
            120
        )));
        key_at(&mut r, usage::LEFT_SHIFT, false, start + ms(130));
        tap(&mut r, usage::LEFT_CONTROL, start, 140, 180);
        key_at(&mut r, usage::LEFT_CONTROL, true, start + ms(200));
        tap(&mut r, usage::C, start, 210, 220);
        assert!(!took_back(&key_at(
            &mut r,
            usage::LEFT_CONTROL,
            false,
            start + ms(240)
        )));
        // The other Control key: not the same one twice.
        let (mut r, start) = fresh();
        tap(&mut r, usage::LEFT_CONTROL, start, 0, 50);
        assert!(!took_back(&tap(
            &mut r,
            usage::RIGHT_CONTROL,
            start,
            80,
            120
        )));
        // Both down at once.
        let (mut r, start) = fresh();
        tap(&mut r, usage::LEFT_CONTROL, start, 0, 50);
        key_at(&mut r, usage::LEFT_CONTROL, true, start + ms(80));
        key_at(&mut r, usage::RIGHT_CONTROL, true, start + ms(90));
        key_at(&mut r, usage::RIGHT_CONTROL, false, start + ms(100));
        assert!(!took_back(&key_at(
            &mut r,
            usage::LEFT_CONTROL,
            false,
            start + ms(120)
        )));
        // Another modifier pressed twice.
        let (mut r, start) = fresh();
        tap(&mut r, usage::LEFT_ALT, start, 0, 50);
        assert!(!took_back(&tap(&mut r, usage::LEFT_ALT, start, 80, 120)));
        // A click, or scrolling (Control held to zoom), between.
        let click = |pressed| InputEvent::Button {
            button: Button::Left,
            pressed,
        };
        for between in [
            vec![click(true), click(false)],
            vec![InputEvent::Scroll { dx: 0.0, dy: 1.0 }],
        ] {
            let (mut r, start) = fresh();
            tap(&mut r, usage::LEFT_CONTROL, start, 0, 50);
            for event in between {
                handle(&mut r, Event::Input(event));
            }
            assert!(!took_back(&tap(
                &mut r,
                usage::LEFT_CONTROL,
                start,
                80,
                120
            )));
        }
        // Moving the mouse is no key.
        let (mut r, start) = fresh();
        tap(&mut r, usage::LEFT_CONTROL, start, 0, 50);
        motion(&mut r, (0.0, 0.0), (3.0, 1.0));
        assert!(took_back(&tap(&mut r, usage::LEFT_CONTROL, start, 80, 120)));

        // Auto-repeat while held is the same press.
        let (mut r, start) = fresh();
        key_at(&mut r, usage::LEFT_CONTROL, true, start);
        assert_eq!(
            key_at(&mut r, usage::LEFT_CONTROL, true, start + ms(40)),
            vec![sent(usage::LEFT_CONTROL, true)]
        );
        key_at(&mut r, usage::LEFT_CONTROL, false, start + ms(60));
        assert!(took_back(&tap(
            &mut r,
            usage::LEFT_CONTROL,
            start,
            100,
            150
        )));
    }

    #[test]
    fn control_twice_does_nothing_here_or_when_off() {
        let ms = Duration::from_millis;
        // On this machine: Control is this machine's.
        let mut r = double_press_router();
        let start = Instant::now();
        for (down, up) in [(0, 50), (80, 120), (150, 190)] {
            assert!(tap(&mut r, usage::LEFT_CONTROL, start, down, up).is_empty());
        }
        // Off, the default: forwarded and nothing else.
        let mut r = router();
        let start = on_win(&mut r);
        tap(&mut r, usage::LEFT_CONTROL, start, 0, 50);
        assert_eq!(
            tap(&mut r, usage::LEFT_CONTROL, start, 80, 120),
            vec![sent(usage::LEFT_CONTROL, false)]
        );
        // Turned on, it counts from the next press.
        r.set_hotkeys(
            Hotkeys {
                take_back_double: true,
                ..Hotkeys::default()
            },
            HeldHotkeys::default(),
        );
        tap(&mut r, usage::LEFT_CONTROL, start, 150, 190);
        assert!(took_back(&tap(
            &mut r,
            usage::LEFT_CONTROL,
            start,
            220,
            260
        )));

        // A third press after the two is this machine's, and counts toward
        // nothing once the cursor is back on the PC.
        let mut r = double_press_router();
        let start = on_win(&mut r);
        tap(&mut r, usage::LEFT_CONTROL, start, 0, 50);
        assert!(took_back(&tap(&mut r, usage::LEFT_CONTROL, start, 80, 120)));
        assert!(tap(&mut r, usage::LEFT_CONTROL, start, 150, 190).is_empty());
        enter_win(&mut r);
        assert!(!took_back(&tap(
            &mut r,
            usage::LEFT_CONTROL,
            start,
            220,
            260
        )));
        // Nor do presses on one device count with those on another.
        let mut r = double_press_router();
        let start = on_win(&mut r);
        tap(&mut r, usage::LEFT_CONTROL, start, 0, 50);
        r.jump_to(&id("mini")).unwrap();
        assert!(!took_back(&tap(
            &mut r,
            usage::LEFT_CONTROL,
            start,
            80,
            120
        )));
        // A Control held as the cursor crossed is let go of there, but its
        // press was this machine's: no first press.
        let mut r = double_press_router();
        let start = Instant::now();
        key_at(&mut r, usage::LEFT_CONTROL, true, start);
        enter_win(&mut r);
        key_at(&mut r, usage::LEFT_CONTROL, false, start + ms(40));
        assert!(!took_back(&tap(
            &mut r,
            usage::LEFT_CONTROL,
            start,
            80,
            120
        )));
        assert!(took_back(&tap(
            &mut r,
            usage::LEFT_CONTROL,
            start,
            150,
            190
        )));
    }

    /// In relative mode games often take Control (to crouch, say): the
    /// double press goes to the game and nothing comes back, and presses
    /// in it, or before it, count toward none after it.
    #[test]
    fn control_twice_is_the_games_in_relative_mode() {
        let (mut r, start) = fresh();
        let ms = Duration::from_millis;
        r.set_relative_capable(id("win"), true);
        r.set_relative(true).unwrap();
        tap(&mut r, usage::LEFT_CONTROL, start, 0, 50);
        assert_eq!(
            tap(&mut r, usage::LEFT_CONTROL, start, 80, 120),
            vec![sent(usage::LEFT_CONTROL, false)]
        );
        assert!(handle(&mut r, Event::Input(InputEvent::DoublePressed)).is_empty());
        // A press in it, let go of after it: no first press.
        key_at(&mut r, usage::LEFT_CONTROL, true, start + ms(150));
        r.set_relative(false).unwrap();
        key_at(&mut r, usage::LEFT_CONTROL, false, start + ms(190));
        assert!(!took_back(&tap(
            &mut r,
            usage::LEFT_CONTROL,
            start,
            220,
            260
        )));
        // A tap just before it neither.
        let (mut r, start) = fresh();
        r.set_relative_capable(id("win"), true);
        tap(&mut r, usage::LEFT_CONTROL, start, 0, 40);
        r.set_relative(true).unwrap();
        r.set_relative(false).unwrap();
        assert!(!took_back(&tap(
            &mut r,
            usage::LEFT_CONTROL,
            start,
            60,
            100
        )));
        // Out of it, the double press takes control back again.
        assert!(took_back(&tap(
            &mut r,
            usage::LEFT_CONTROL,
            start,
            120,
            160
        )));
    }

    /// Capture that told the double press as such, its keys gone to a
    /// local app: control comes back, and nothing goes to the PC.
    #[test]
    fn a_double_press_told_by_capture_takes_control_back_and_sends_nothing() {
        let (mut r, _) = fresh();
        assert_eq!(
            handle(&mut r, Event::Input(InputEvent::DoublePressed)),
            vec![
                Action::Leave { device: id("win") },
                Action::Ungrab {
                    warp_to: Point::new(1511.0, 500.0),
                },
                Action::ControlReturned(ReturnReason::Hotkey),
            ]
        );
        // Here, or with the double press off: nothing.
        assert!(handle(&mut r, Event::Input(InputEvent::DoublePressed)).is_empty());
        let mut off = router();
        enter_win(&mut off);
        assert!(handle(&mut off, Event::Input(InputEvent::DoublePressed)).is_empty());
        assert!(matches!(off.state(), State::Remote { .. }));
    }

    /// The PC sees Control go down and up twice, then the cursor leave:
    /// nothing stays pressed there.
    #[test]
    fn control_twice_leaves_nothing_pressed_on_the_device() {
        use crate::receiver::{Injection, Receiver, RemoteEvent};

        let mut r = double_press_router();
        let mut pc = Receiver::default();
        let mut down = BTreeSet::new();
        let mut deliver = |actions: Vec<Action>| {
            for action in actions {
                let event = match action {
                    Action::Enter {
                        position,
                        modifiers,
                        buttons,
                        ..
                    } => RemoteEvent::Enter {
                        from: id("mac"),
                        position,
                        modifiers,
                        buttons,
                    },
                    Action::Key { usage, pressed, .. } => RemoteEvent::Key {
                        from: id("mac"),
                        usage,
                        pressed,
                    },
                    Action::Leave { .. } => RemoteEvent::Leave { from: id("mac") },
                    _ => continue,
                };
                for injection in pc.handle(event) {
                    if let Injection::Key { usage, pressed } = injection {
                        if pressed {
                            down.insert(usage);
                        } else {
                            down.remove(&usage);
                        }
                    }
                }
            }
        };
        deliver(enter_win(&mut r));
        let start = Instant::now();
        let ms = Duration::from_millis;
        for (at, pressed) in [(0, true), (60, false), (120, true)] {
            deliver(key_at(&mut r, usage::LEFT_CONTROL, pressed, start + ms(at)));
        }
        let last = key_at(&mut r, usage::LEFT_CONTROL, false, start + ms(180));
        assert!(took_back(&last));
        deliver(last);
        assert!(down.is_empty(), "{down:?}");
        assert_eq!(pc.controller(), None);
    }
}
