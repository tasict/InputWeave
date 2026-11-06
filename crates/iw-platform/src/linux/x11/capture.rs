use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use iw_core::HidUsage;
use iw_core::geometry::Point;
use iw_core::hotkey::HeldHotkeys;
use iw_core::input::InputEvent;
use iw_core::layout::Display;
use iw_core::router::Hotkey;
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::randr::{ConnectionExt as _, NotifyMask, Rotation};
use x11rb::protocol::xfixes::ConnectionExt as _;
use x11rb::protocol::xinput::{
    self, ChangeReason, ConnectionExt as _, DeviceClassData, DeviceType, PointerEventFlags,
    ScrollType, ValuatorMode, XIEventMask, XIGetPropertyItems,
};
use x11rb::protocol::xkb::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{
    AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, GrabMode, GrabStatus, ModMask,
    Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{COPY_DEPTH_FROM_PARENT, CURRENT_TIME, NONE};

use super::platform::{DISPLAYS_STALE, connect, monitors};
use super::translate::{
    AbsoluteAxis, Axes, Drift, KeyRepeat, ModifierMasks, RepeatSettings, Reports, ScrollAxis,
    Watched, button_of, fixed, hold_point, is_xtest_device, key_of, keycode, root_size, valuators,
    wheel_notches,
};
use crate::linux::common::{Backoff, os};
use crate::{InputCapture, PlatformError};

/// XI2's id for every device.
const ALL_DEVICES: u16 = 0;

/// How far the hidden cursor may wander from where grabbed capture holds
/// it before it is put back there, in pixels: never near an edge, a hot
/// corner or another display, while warps stay rare.
const DRIFT: f64 = 200.0;

/// How often to try grabbing again while another client holds the pointer
/// or the keyboard, as a menu, a password prompt or a screen locker does.
const GRAB_RETRY: Duration = Duration::from_millis(50);

/// How long after letting go of the grab the cursor shows without a warp
/// putting it somewhere first. The agent warps it right after, so it
/// shows where it comes back rather than flashing in the middle.
const SHOW_LATER: Duration = Duration::from_millis(100);

/// How long `reserve_hotkeys` waits for the worker before answering that
/// none of the hotkeys is held back (yet); capture tells the agent when
/// they are (`InputEvent::HotkeysHeldBack`).
const RESERVE_WAIT: Duration = Duration::from_millis(200);

/// How long after a hotkey was left to local apps, because another app
/// holds it, it is grabbed again, so it is held back once that app lets go
/// of it; twice as long after each try that leaves the same hotkeys, up to
/// [`RETRY_HOTKEYS_MAX`], and no more after [`RETRY_HOTKEYS_TIMES`] such
/// tries, until the keyboard mapping or the hotkeys change, injection lets
/// go of them or the server connects again.
const RETRY_HOTKEYS: Duration = Duration::from_secs(3);
const RETRY_HOTKEYS_MAX: Duration = Duration::from_secs(60);
const RETRY_HOTKEYS_TIMES: u32 = 5;

/// How long after injection last held a key the hotkeys are grabbed again
/// (`HotkeyPause`): not after every keystroke of someone typing here from
/// another device.
const RESUME_HOTKEYS: Duration = Duration::from_secs(1);

/// Reads local input from XInput 2 raw events on the root window. Raw
/// events come before any grab and from every client's point of view at
/// once, so they are what capture reports whether or not anything is
/// grabbed, and they report how far the mouse moved even where the cursor
/// stops at the edge of the screen. Events of the server's XTest devices
/// are left out: every injection by XTest, this process's own included,
/// arrives through them.
///
/// A worker thread owns the connection: it reads the events and carries
/// out the agent's requests to grab, let go and warp, which only queue
/// them and wake it, so the agent never waits on the server's round trips.
/// When the connection breaks, as when the display server restarts, what
/// was down is reported released, and the worker connects again, backing
/// off, and restores the grab and the reserved hotkeys.
///
/// While grabbed, capture holds an active grab of the pointer and the
/// keyboard, so local clients get none of it, hides the cursor (XFixes)
/// and keeps it in the middle of its display. Input never goes to another
/// device while it also reaches local apps: while another client holds a
/// grab (a menu, a password prompt, an X11 screen locker), capture reports
/// nothing but the release of what it reported down, and tries to grab
/// again every [`GRAB_RETRY`] until it can, or until the agent no longer
/// wants it. Letting go, it shows the cursor only once it is warped to
/// where it comes back (or after [`SHOW_LATER`]).
///
/// Raw events carry no key repeats, which the server makes up for the
/// focused window alone, so a timer thread repeats the key held down
/// while grabbed, at the server's own pace (`KeyRepeat`), whether it went
/// down before or after the grab. Reserved hotkeys are passive key grabs,
/// so local clients never see them; the server hands their release to the
/// grab too. They are grabbed again when the keyboard mapping changes them,
/// one another app holds now and then ([`RETRY_HOTKEYS`]), and capture
/// tells the agent which are held back as that changes, in order with the
/// keys and numbered (`InputEvent::HotkeysHeldBack`): none while another
/// client's grab takes every key.
///
/// Wheel scrolling comes from the scrolling axes of smooth scrolling
/// (XI 2.1) where a device has them, as all do under the libinput and
/// evdev drivers; the clicks of buttons 4–7 the server makes up from them
/// are not counted again. Only devices without such axes report scrolling
/// as those buttons. libinput's natural scrolling is undone, following the
/// device's setting as it changes.
///
/// Devices that report positions (tablets, touch screens, the pointers of
/// virtual machines) move by their own values, scaled to the root window
/// they are taken to span (`AbsoluteAxis`; a device mapped onto one
/// monitor by its Coordinate Transformation Matrix moves too fast): they
/// go on past an edge while grabbed, and a warp or an injection in between
/// is no movement of theirs. Their cursor is not held in the middle while
/// grabbed, as their next event puts it back where they point; after the
/// grab, their first event moves nothing.
pub struct X11Capture {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
    /// Injected events are reported too, for the tests against Xvfb,
    /// whose only input is XTest's.
    report_injected: bool,
}

/// A connection to the X server and what capture made on it.
struct Link {
    conn: RustConnection,
    root: Window,
    /// An input-only window of this connection's, which the worker is
    /// woken through.
    window: Window,
    min_keycode: u8,
    max_keycode: u8,
    /// The server speaks XKB, whose controls say how keys repeat.
    xkb: bool,
}

impl Link {
    fn open() -> Result<Self, PlatformError> {
        let (conn, root) = connect()?;
        let version = conn
            .xinput_xi_query_version(2, 2)
            .map_err(|e| os("the X server has no XInput 2", e))?
            .reply()
            .map_err(|e| os("the X server has no XInput 2", e))?;
        if (version.major_version, version.minor_version) < (2, 2) {
            return Err(PlatformError::Unsupported("an X server without XInput 2.2"));
        }
        // XFixes and XKB answer only clients that said which version they
        // speak.
        conn.xfixes_query_version(5, 0)
            .map_err(|e| os("the X server has no XFixes", e))?
            .reply()
            .map_err(|e| os("the X server has no XFixes", e))?;
        let xkb = conn
            .xkb_use_extension(1, 0)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some_and(|r| r.supported);
        let window = conn
            .generate_id()
            .map_err(|e| os("cannot create a window", e))?;
        conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            window,
            root,
            -1,
            -1,
            1,
            1,
            0,
            WindowClass::INPUT_ONLY,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new(),
        )
        .map_err(|e| os("cannot create a window", e))?;
        conn.flush().map_err(|e| os("cannot create a window", e))?;
        let (min_keycode, max_keycode) = (conn.setup().min_keycode, conn.setup().max_keycode);
        Ok(Self {
            conn,
            root,
            window,
            min_keycode,
            max_keycode,
            xkb,
        })
    }

    /// Wakes the worker, which waits for the server's next event.
    fn wake(&self) {
        super::platform::wake(&self.conn, self.window);
    }

    fn pointer(&self) -> Result<Point, PlatformError> {
        let reply = self
            .conn
            .query_pointer(self.root)
            .map_err(|e| os("cannot read where the cursor is", e))?
            .reply()
            .map_err(|e| os("cannot read where the cursor is", e))?;
        Ok(Point::new(reply.root_x.into(), reply.root_y.into()))
    }

    fn warp(&self, to: Point) -> Result<(), PlatformError> {
        let c = |v: f64| v.round().clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16;
        self.conn
            .warp_pointer(NONE, self.root, 0, 0, 0, 0, c(to.x), c(to.y))
            .map_err(|e| os("cannot move the cursor", e))?;
        self.conn
            .flush()
            .map_err(|e| os("cannot move the cursor", e))
    }

    /// Takes the pointer and the keyboard from local clients, both or
    /// neither.
    fn grab(&self) -> Result<(), PlatformError> {
        let failed = |what: &str, status: GrabStatus| {
            PlatformError::Os(format!("another app holds the {what} ({status:?})"))
        };
        let status = self
            .conn
            .grab_pointer(
                false,
                self.root,
                EventMask::NO_EVENT,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
                NONE,
                NONE,
                CURRENT_TIME,
            )
            .map_err(|e| os("cannot grab the pointer", e))?
            .reply()
            .map_err(|e| os("cannot grab the pointer", e))?
            .status;
        if status != GrabStatus::SUCCESS {
            return Err(failed("pointer", status));
        }
        let status = self
            .conn
            .grab_keyboard(
                false,
                self.root,
                CURRENT_TIME,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
            )
            .map_err(|e| os("cannot grab the keyboard", e))?
            .reply()
            .map_err(|e| os("cannot grab the keyboard", e))?
            .status;
        if status != GrabStatus::SUCCESS {
            let _ = self.conn.ungrab_pointer(CURRENT_TIME);
            let _ = self.conn.flush();
            return Err(failed("keyboard", status));
        }
        Ok(())
    }

    fn ungrab(&self) {
        let _ = self.conn.ungrab_keyboard(CURRENT_TIME);
        let _ = self.conn.ungrab_pointer(CURRENT_TIME);
        let _ = self.conn.flush();
    }

    fn show_cursor(&self) {
        let _ = self.conn.xfixes_show_cursor(self.root);
        let _ = self.conn.flush();
    }

    /// How the server repeats keys, as XKB says now; X's defaults without
    /// XKB.
    fn repeat_settings(&self) -> RepeatSettings {
        if !self.xkb {
            return RepeatSettings::default();
        }
        let reply = self
            .conn
            .xkb_get_controls(xkb::ID::USE_CORE_KBD.into())
            .map(|c| c.reply());
        match reply {
            Ok(Ok(c)) => RepeatSettings {
                on: u32::from(c.enabled_controls) & u32::from(xkb::BoolCtrl::REPEAT_KEYS) != 0,
                delay: Duration::from_millis(c.repeat_delay.into()),
                interval: Duration::from_millis(c.repeat_interval.max(1).into()),
                per_key: c.per_key_repeat,
            },
            _ => RepeatSettings::default(),
        }
    }

    fn ungrab_keys(&self, grabs: &[(u8, u16)]) {
        for &(code, modifiers) in grabs {
            let _ = self
                .conn
                .ungrab_key(code, self.root, ModMask::from(modifiers));
        }
        let _ = self.conn.flush();
    }

    /// Grabs what `hotkeys` need now, given the grabs `held` already,
    /// hotkey by hotkey: each in every modifier state it may come with
    /// (`ModifierMasks::grab_states`). A hotkey this server has no key code
    /// or modifier for, or that another client holds a grab of, as the
    /// desktop's own shortcuts may (GNOME's XF86Tools is F13), is left to
    /// local apps, all of its grabs let go of; the others stay held. New
    /// grabs are made before the ones no longer needed are let go of, so a
    /// key changing state is never left ungrabbed in between; nothing is
    /// asked of the server when nothing changed. The requests all go out
    /// before any answer is waited for. Trying `again`, a hotkey none of
    /// whose grabs is held is first tried by its own modifiers alone, the
    /// state another app most likely holds, and the rest only if that is
    /// taken: one still held so is found without grabbing, and letting go of
    /// at once, the states that app does not hold. Returns the grabs held
    /// now and the hotkeys left to local apps.
    fn regrab(&self, hotkeys: &[Hotkey], held: &[(u8, u16)], again: bool) -> (Grabs, Vec<Hotkey>) {
        let masks = match self.conn.get_modifier_mapping().map(|c| c.reply()) {
            Ok(Ok(reply)) => ModifierMasks::from_mapping(&reply.keycodes),
            _ => {
                self.ungrab_keys(held);
                return (Vec::new(), hotkeys.to_vec());
            }
        };
        // Each hotkey's grabs; none when it cannot have them here.
        let wanted: Vec<(Hotkey, Option<Grabs>)> = hotkeys
            .iter()
            .map(|hotkey| {
                let grabs = keycode(hotkey.key, self.min_keycode, self.max_keycode)
                    .zip(masks.grab_states(hotkey.modifiers))
                    .map(|(code, states)| states.into_iter().map(|state| (code, state)).collect());
                (*hotkey, grabs)
            })
            .collect();
        // Each of `grabs`, whether the server took it.
        let grab_all = |grabs: &[(u8, u16)]| -> Vec<((u8, u16), bool)> {
            let cookies: Vec<_> = grabs
                .iter()
                .map(|&(code, state)| {
                    self.conn.grab_key(
                        false,
                        self.root,
                        ModMask::from(state),
                        code,
                        GrabMode::ASYNC,
                        GrabMode::ASYNC,
                    )
                })
                .collect();
            cookies
                .into_iter()
                .zip(grabs)
                .map(|(cookie, &grab)| (grab, matches!(cookie.map(|c| c.check()), Ok(Ok(())))))
                .collect()
        };
        let mut grabbed = Vec::new();
        let mut refused_grabs = Vec::new();
        let sort = |answers: Vec<((u8, u16), bool)>, grabbed: &mut Grabs, refused: &mut Grabs| {
            for (grab, taken) in answers {
                if taken {
                    grabbed.push(grab);
                } else {
                    refused.push(grab);
                }
            }
        };
        if again {
            let mut firsts: Vec<(u8, u16)> = wanted
                .iter()
                .filter_map(|(_, grabs)| grabs.as_ref())
                .filter(|grabs| !grabs.iter().any(|g| held.contains(g)))
                .filter_map(|grabs| grabs.first().copied())
                .collect();
            firsts.sort_unstable();
            firsts.dedup();
            sort(grab_all(&firsts), &mut grabbed, &mut refused_grabs);
        }
        let mut new: Vec<(u8, u16)> = wanted
            .iter()
            .filter_map(|(_, grabs)| grabs.as_ref())
            .filter(|grabs| !grabs.iter().any(|g| refused_grabs.contains(g)))
            .flatten()
            .filter(|g| !held.contains(g) && !grabbed.contains(g))
            .copied()
            .collect();
        new.sort_unstable();
        new.dedup();
        sort(grab_all(&new), &mut grabbed, &mut refused_grabs);
        let mut kept = Vec::new();
        let mut left = Vec::new();
        for (hotkey, grabs) in wanted {
            match grabs {
                Some(grabs) if !grabs.iter().any(|g| refused_grabs.contains(g)) => {
                    kept.extend(grabs)
                }
                _ => left.push(hotkey),
            }
        }
        kept.sort_unstable();
        kept.dedup();
        // What is held, or was just grabbed, that no hotkey kept needs.
        let mut stale: Vec<(u8, u16)> = held
            .iter()
            .chain(&grabbed)
            .filter(|g| !kept.contains(g))
            .copied()
            .collect();
        stale.sort_unstable();
        stale.dedup();
        if !stale.is_empty() {
            self.ungrab_keys(&stale);
        }
        (kept, left)
    }
}

/// Passive key grabs: a key code and a modifier state each.
type Grabs = Vec<(u8, u16)>;

/// What the agent asks of the worker.
enum Command {
    Grab,
    Ungrab,
    Warp(Point),
    /// No warp came after letting go: show the cursor where it is.
    ShowCursor,
    /// Another client held a grab: try again.
    RetryGrab,
    /// Grab these hotkeys, and say which are held.
    Reserve(Vec<Hotkey>, std::sync::mpsc::Sender<HeldHotkeys>),
    /// Grab again the hotkeys another app held.
    RetryHotkeys,
    /// Injection held no key for a while: grab the hotkeys again.
    ResumeHotkeys,
    /// Answered once everything asked before is done.
    #[cfg(test)]
    Settle(std::sync::mpsc::Sender<()>),
}

struct Shared {
    /// The worker's connection, for waking it and for reserving hotkeys;
    /// none while it connects again.
    link: Mutex<Option<Arc<Link>>>,
    /// The worker's queue, and whether to stop: under one lock, so a stop
    /// is never missed by a worker about to wait.
    commands: Mutex<Commands>,
    /// Wakes the worker while it has no connection to be woken through.
    commanded: Condvar,
    hotkeys: Mutex<Reserved>,
    /// The hotkeys held back now, under a lock of their own, so telling
    /// them never waits on whatever holds `hotkeys`.
    held: Mutex<Held>,
    /// What the router acts on while grabbed, which goes out even while
    /// input waits for a grab (`InputCapture::watch_hotkeys`,
    /// `InputCapture::watch_double_press`).
    watched: Mutex<Watched>,
    timer: Mutex<Timer>,
    /// Wakes the timer thread when what it waits for changed.
    timer_wake: Condvar,
}

/// What capture tells of the hotkeys held back (`InputEvent::HotkeysHeldBack`):
/// those its passive grabs hold, but none while another client's grab
/// takes every key; numbered, so the agent never takes a set told before
/// the one it has; and where its events go once it started.
#[derive(Default)]
struct Held {
    grabbed: HeldHotkeys,
    /// Another client's grab takes every key (`Worker::waiting_for_grab`).
    waiting: bool,
    told: HeldHotkeys,
    events: Option<Sender<InputEvent>>,
}

impl Held {
    /// The set held back now, told as the next change if it changed.
    fn tell(&mut self) -> HeldHotkeys {
        let now = if self.waiting {
            HeldHotkeys::default()
        } else {
            self.grabbed
        };
        if now != self.told {
            self.told = now.as_change(self.told.change() + 1);
            if let Some(events) = &self.events {
                let _ = events.send(InputEvent::HotkeysHeldBack(self.told));
            }
        }
        self.told
    }
}

#[derive(Default)]
struct Commands {
    queue: VecDeque<Command>,
    stop: bool,
}

/// The hotkeys the agent reserved, and the passive grabs holding them.
#[derive(Default)]
struct Reserved {
    hotkeys: Vec<Hotkey>,
    grabs: Vec<(u8, u16)>,
    /// Those of `hotkeys` not held back, last time they were grabbed.
    left: Vec<Hotkey>,
    /// How many tries again in a row left the same hotkeys to local apps.
    tries: u32,
    /// Let go of while injection holds keys (`HotkeyPause`).
    paused: bool,
}

/// What the timer thread keeps: key repeats, which it and the worker both
/// send while holding this lock, so a repeat never follows its key's
/// release; when to show the cursor without a warp; and when to try
/// grabbing again.
#[derive(Default)]
struct Timer {
    keys: KeyRepeat,
    settings: RepeatSettings,
    grabbed: bool,
    show_at: Option<Instant>,
    retry_grab_at: Option<Instant>,
    resume_hotkeys_at: Option<Instant>,
    retry_hotkeys_at: Option<Instant>,
    stop: bool,
}

impl Shared {
    fn link(&self) -> Option<Arc<Link>> {
        self.link
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn set_link(&self, link: Option<Arc<Link>>) {
        *self.link.lock().unwrap_or_else(PoisonError::into_inner) = link;
    }

    fn commands(&self) -> MutexGuard<'_, Commands> {
        self.commands.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn stopping(&self) -> bool {
        self.commands().stop
    }

    fn hotkeys(&self) -> MutexGuard<'_, Reserved> {
        self.hotkeys.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn held(&self) -> MutexGuard<'_, Held> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The passive grabs hold back `hotkeys` from now on; tells the agent
    /// if what is held back changed, after the keys capture reported
    /// before. What is held back now, numbered.
    fn set_held(&self, hotkeys: HeldHotkeys) -> HeldHotkeys {
        let mut held = self.held();
        held.grabbed = hotkeys;
        held.tell()
    }

    /// Another client's grab takes every key from now on, or no longer.
    fn set_waiting(&self, waiting: bool) {
        let mut held = self.held();
        held.waiting = waiting;
        held.tell();
    }

    fn watched(&self) -> MutexGuard<'_, Watched> {
        self.watched.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn timer(&self) -> MutexGuard<'_, Timer> {
        self.timer.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn command(&self, command: Command) {
        self.commands().queue.push_back(command);
        self.commanded.notify_all();
        if let Some(link) = self.link() {
            link.wake();
        }
    }

    /// Tells the worker and the timer to stop, and wakes them.
    fn stop(&self) {
        self.commands().stop = true;
        self.commanded.notify_all();
        {
            let mut timer = self.timer();
            timer.stop = true;
            self.timer_wake.notify_all();
        }
        if let Some(link) = self.link() {
            link.wake();
        }
    }

    fn take_commands(&self) -> VecDeque<Command> {
        std::mem::take(&mut self.commands().queue)
    }

    /// Tells the timer whether capture is grabbed, and so whether keys
    /// repeat.
    fn grabbed(&self, grabbed: bool) {
        self.timer().grabbed = grabbed;
        self.timer_wake.notify_all();
    }

    fn set_repeat_settings(&self, settings: RepeatSettings) {
        self.timer().settings = settings;
    }

    fn show_cursor_at(&self, at: Option<Instant>) {
        self.timer().show_at = at;
        self.timer_wake.notify_all();
    }

    fn retry_grab_at(&self, at: Option<Instant>) {
        self.timer().retry_grab_at = at;
        self.timer_wake.notify_all();
    }

    fn resume_hotkeys_at(&self, at: Option<Instant>) {
        self.timer().resume_hotkeys_at = at;
        self.timer_wake.notify_all();
    }

    fn retry_hotkeys_at(&self, at: Option<Instant>) {
        self.timer().retry_hotkeys_at = at;
        self.timer_wake.notify_all();
    }

    /// Grabs `hotkeys` on `link`, unless injection holds them paused,
    /// trying `again` a hotkey left to local apps before. What is held back
    /// now, numbered. One left to local apps is tried again after
    /// [`RETRY_HOTKEYS`], backing off while the same ones stay left.
    fn reserve(
        &self,
        link: &Link,
        reserved: &mut Reserved,
        hotkeys: &[Hotkey],
        again: bool,
    ) -> HeldHotkeys {
        reserved.hotkeys = hotkeys.to_vec();
        if reserved.paused {
            return self.held().told;
        }
        let (grabs, left) = link.regrab(hotkeys, &reserved.grabs, again);
        reserved.grabs = grabs;
        // Said as it changes, not at every try again.
        for hotkey in left.iter().filter(|h| !reserved.left.contains(h)) {
            tracing::info!(
                %hotkey,
                "another app holds this hotkey, or this X server cannot grab it; it reaches local apps"
            );
        }
        for hotkey in reserved
            .left
            .iter()
            .filter(|h| hotkeys.contains(h) && !left.contains(h))
        {
            tracing::info!(%hotkey, "this hotkey is held back again");
        }
        let held: HeldHotkeys = hotkeys
            .iter()
            .filter(|h| !left.contains(h))
            .copied()
            .collect();
        reserved.tries = if again && left == reserved.left {
            reserved.tries + 1
        } else {
            0
        };
        let retry = if left.is_empty() {
            None
        } else {
            retry_after(reserved.tries)
        };
        if !left.is_empty() && retry.is_none() && reserved.tries == RETRY_HOTKEYS_TIMES {
            tracing::info!(
                "the hotkeys another app holds are not tried again until the keyboard mapping or the hotkeys change"
            );
        }
        self.retry_hotkeys_at(retry.map(|after| Instant::now() + after));
        reserved.left = left;
        self.set_held(held)
    }
}

/// How long to wait before trying again hotkeys another app holds, after
/// `tries` tries in a row that left the same ones; none after
/// [`RETRY_HOTKEYS_TIMES`].
fn retry_after(tries: u32) -> Option<Duration> {
    (tries < RETRY_HOTKEYS_TIMES).then(|| {
        RETRY_HOTKEYS
            .saturating_mul(1 << tries.min(16))
            .min(RETRY_HOTKEYS_MAX)
    })
}

/// Keeps capture's passive key grabs from taking keys injected here: the
/// server matches them against injected keys too, so a keystroke another
/// device sends, as one matching a hotkey of this computer's, would never
/// reach the focused app. Before a key goes down by injection the grabs are
/// let go of, the server done with that before the key is sent; they are
/// taken again once injection held no key for [`RESUME_HOTKEYS`].
/// Meanwhile capture tells the agent none is held back.
#[derive(Clone)]
pub struct HotkeyPause {
    shared: Arc<Shared>,
}

impl HotkeyPause {
    /// A key is about to go down by injection.
    pub(crate) fn injecting(&self) {
        let s = &self.shared;
        s.resume_hotkeys_at(None);
        let mut reserved = s.hotkeys();
        if reserved.paused {
            return;
        }
        reserved.paused = true;
        if let Some(link) = s.link() {
            link.ungrab_keys(&std::mem::take(&mut reserved.grabs));
            let _ = link.conn.sync();
        }
        reserved.grabs.clear();
        s.set_held(HeldHotkeys::default());
    }

    /// Injection let go of a key; `idle` once it holds none.
    pub(crate) fn released(&self, idle: bool) {
        if idle && self.shared.hotkeys().paused {
            self.shared
                .resume_hotkeys_at(Some(Instant::now() + RESUME_HOTKEYS));
        }
    }
}

impl X11Capture {
    pub fn connect() -> Result<Self, PlatformError> {
        let link = Arc::new(Link::open()?);
        Ok(Self {
            shared: Arc::new(Shared {
                link: Mutex::new(Some(link)),
                commands: Mutex::new(Commands::default()),
                commanded: Condvar::new(),
                hotkeys: Mutex::new(Reserved::default()),
                held: Mutex::new(Held::default()),
                watched: Mutex::new(Watched::default()),
                timer: Mutex::new(Timer::default()),
                timer_wake: Condvar::new(),
            }),
            threads: Vec::new(),
            report_injected: false,
        })
    }

    /// The hotkeys held back now, as capture last told them.
    #[cfg(test)]
    pub(crate) fn hotkeys_held_back(&self) -> HeldHotkeys {
        self.shared.held().told
    }

    /// For injection into the same session, which lets go of the hotkeys
    /// while it holds keys.
    pub fn hotkey_pause(&self) -> HotkeyPause {
        HotkeyPause {
            shared: self.shared.clone(),
        }
    }

    #[cfg(test)]
    pub(crate) fn reporting_injected(mut self) -> Self {
        self.report_injected = true;
        self
    }

    /// Waits until the worker has done everything asked of it so far.
    #[cfg(test)]
    pub(crate) fn settle(&self) {
        let (tx, rx) = std::sync::mpsc::channel();
        self.shared.command(Command::Settle(tx));
        rx.recv_timeout(Duration::from_secs(5))
            .expect("the capture worker answers");
    }
}

impl InputCapture for X11Capture {
    /// Selects the events before it returns, so no input after it goes
    /// unseen; the threads run once both started.
    fn start(&mut self, events: Sender<InputEvent>) -> Result<(), PlatformError> {
        if !self.threads.is_empty() {
            return Ok(());
        }
        let link = self
            .shared
            .link()
            .ok_or(PlatformError::Unavailable("the X server"))?;
        let mut worker = Worker {
            shared: self.shared.clone(),
            events: events.clone(),
            report_injected: self.report_injected,
            natural_atom: None,
            devices: HashMap::new(),
            displays: None,
            root_size: Point::new(0.0, 0.0),
            want_grabbed: false,
            grabbed: false,
            waiting_for_grab: false,
            cursor_hidden: false,
            show_on_warp: false,
            hold: Point::new(0.0, 0.0),
            drift: Drift::default(),
            reports: Reports::default(),
            mapping_changed: false,
        };
        worker.listen(&link)?;
        self.shared.held().events = Some(events.clone());
        let spawn = |name: &str, run: Box<dyn FnOnce() + Send>| {
            std::thread::Builder::new()
                .name(name.to_owned())
                .spawn(run)
                .map_err(|e| PlatformError::Os(e.to_string()))
        };
        let shared = self.shared.clone();
        let timer = spawn(
            "inputweave-x11-timer",
            Box::new(move || run_timer(&shared, &events)),
        )?;
        let worker = match spawn("inputweave-x11-capture", Box::new(move || worker.run())) {
            Ok(worker) => worker,
            Err(e) => {
                self.shared.stop();
                let _ = timer.join();
                return Err(e);
            }
        };
        self.threads = vec![timer, worker];
        Ok(())
    }

    /// Queued for the worker; errors are its to log.
    fn set_grabbed(&mut self, grabbed: bool) -> Result<(), PlatformError> {
        self.shared.command(if grabbed {
            Command::Grab
        } else {
            Command::Ungrab
        });
        Ok(())
    }

    fn warp(&mut self, position: Point) -> Result<(), PlatformError> {
        self.shared.command(Command::Warp(position));
        Ok(())
    }

    /// Hotkey by hotkey (`Link::regrab`), on the current connection: one
    /// another app holds is left to local apps and the others held back,
    /// and this returns those held back. The worker grabs them again when
    /// the keyboard mapping changes or it connects again, one another app
    /// holds every [`RETRY_HOTKEYS`], and tells each change
    /// (`InputEvent::HotkeysHeldBack`). A grab already active, for a
    /// key held down, lasts until the key is released, whatever becomes of
    /// the passive grab it came from.
    ///
    /// Before `start`, done here; after, by the worker, waited for at most
    /// [`RESERVE_WAIT`] so a hung server never holds up the agent: what was
    /// held back before then, and capture tells when that changes.
    fn reserve_hotkeys(&mut self, hotkeys: &[Hotkey]) -> HeldHotkeys {
        let s = &self.shared;
        if self.threads.is_empty() {
            let mut reserved = s.hotkeys();
            let Some(link) = s.link() else {
                reserved.hotkeys = hotkeys.to_vec();
                return s.set_held(HeldHotkeys::default());
            };
            return s.reserve(&link, &mut reserved, hotkeys, false);
        }
        // Not told here: what the worker tells comes in order with the
        // keys, and later than this answer, which is no later than it.
        let (tx, rx) = std::sync::mpsc::channel();
        s.command(Command::Reserve(hotkeys.to_vec(), tx));
        rx.recv_timeout(RESERVE_WAIT)
            .unwrap_or_else(|_| s.held().told)
    }

    fn watch_hotkeys(&mut self, hotkeys: &[Hotkey]) {
        self.shared.watched().hotkeys = hotkeys.to_vec();
    }

    fn watch_double_press(&mut self, on: bool) {
        self.shared.watched().double_press = on;
    }
}

impl Drop for X11Capture {
    fn drop(&mut self) {
        if self.threads.is_empty() {
            // Never started: only the hotkeys to let go of.
            if let Some(link) = self.shared.link() {
                link.ungrab_keys(&self.shared.hotkeys().grabs);
                let _ = link.conn.destroy_window(link.window);
                let _ = link.conn.flush();
            }
            return;
        }
        self.shared.stop();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

/// Sends the repeats of the key held down while grabbed, and asks the
/// worker to show the cursor when no warp came and to try grabbing again,
/// until told to stop or nobody listens.
fn run_timer(shared: &Shared, events: &Sender<InputEvent>) {
    let mut timer = shared.timer();
    loop {
        if timer.stop {
            return;
        }
        let now = Instant::now();
        let due = |at: &mut Option<Instant>| {
            let due = at.is_some_and(|at| now >= at);
            if due {
                *at = None;
            }
            due
        };
        let commands: Vec<Command> = [
            (due(&mut timer.show_at), Command::ShowCursor),
            (due(&mut timer.retry_grab_at), Command::RetryGrab),
            (due(&mut timer.resume_hotkeys_at), Command::ResumeHotkeys),
            (due(&mut timer.retry_hotkeys_at), Command::RetryHotkeys),
        ]
        .into_iter()
        .filter_map(|(due, command)| due.then_some(command))
        .collect();
        if !commands.is_empty() {
            drop(timer);
            for command in commands {
                shared.command(command);
            }
            timer = shared.timer();
            continue;
        }
        let t = &mut *timer;
        if t.grabbed
            && let Some(usage) = t.keys.due(now, &t.settings)
        {
            let key = InputEvent::Key {
                usage,
                pressed: true,
            };
            if events.send(key).is_err() {
                return;
            }
            continue;
        }
        let repeat = t.grabbed.then(|| t.keys.deadline()).flatten();
        let deadline = [
            repeat,
            t.show_at,
            t.retry_grab_at,
            t.resume_hotkeys_at,
            t.retry_hotkeys_at,
        ]
        .into_iter()
        .flatten()
        .min();
        timer = match deadline {
            Some(deadline) => {
                let wait = deadline.saturating_duration_since(now);
                shared
                    .timer_wake
                    .wait_timeout(timer, wait)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0
            }
            None => shared
                .timer_wake
                .wait(timer)
                .unwrap_or_else(PoisonError::into_inner),
        };
    }
}

/// A device as capture reads its raw events.
#[derive(Clone, Debug, Default)]
struct Device {
    /// One of the server's XTest devices.
    xtest: bool,
    axes: Axes,
    /// Its first two axes, when they report positions.
    absolute: [Option<AbsoluteAxis>; 2],
}

/// The worker thread's own state.
struct Worker {
    shared: Arc<Shared>,
    events: Sender<InputEvent>,
    report_injected: bool,
    natural_atom: Option<u32>,
    devices: HashMap<u16, Device>,
    /// The monitors and when they were read, until RandR says they changed
    /// or they are [`DISPLAYS_STALE`].
    displays: Option<(Vec<Display>, Instant)>,
    /// The root window's size, which devices reporting positions span.
    root_size: Point,
    /// Grabbed, as the agent last asked; restored after connecting again.
    want_grabbed: bool,
    grabbed: bool,
    /// Another client held a grab when the agent asked for one, and still
    /// does: input is held back meanwhile.
    waiting_for_grab: bool,
    cursor_hidden: bool,
    /// The cursor shows at the next warp, or when the timer says.
    show_on_warp: bool,
    /// Where grabbed capture holds the cursor.
    hold: Point,
    /// How far the hidden cursor went since it was last looked for.
    drift: Drift,
    /// What went out down, and what goes out while input waits for a grab.
    reports: Reports,
    /// The keyboard mapping changed in the batch read: the hotkeys are
    /// grabbed again after it, once.
    mapping_changed: bool,
}

/// Motion read but not reported yet: motion events come in bursts, and
/// one report for each burst saves reading where the cursor is each time.
#[derive(Default)]
struct Pending {
    delta: Point,
    moved: bool,
    /// A device reporting movements moved, so the hidden cursor may have
    /// wandered from the middle. One reporting positions puts it where it
    /// points: not capture's to keep in the middle.
    relative: bool,
}

/// Why serving a connection ended.
enum Served {
    Stopped,
    Lost(String),
}

impl Worker {
    /// Serves the connection `start` listened on, then each new one.
    fn run(mut self) {
        let mut link = self.shared.link();
        let mut listening = true;
        let mut backoff = Backoff::new();
        loop {
            if let Some(l) = link.take() {
                match self.serve(&l, std::mem::take(&mut listening)) {
                    Served::Stopped => {
                        self.finish(&l);
                        return;
                    }
                    Served::Lost(e) => {
                        tracing::warn!(error = %e, "lost the X server; connecting again");
                    }
                }
                self.shared.set_link(None);
                self.lost();
            }
            if self.wait_offline(backoff.delay()) {
                return;
            }
            match Link::open() {
                Ok(l) => {
                    tracing::info!("connected to the X server again");
                    let l = Arc::new(l);
                    self.shared.set_link(Some(l.clone()));
                    self.reserve_again(&l, false);
                    link = Some(l);
                    backoff.succeeded();
                }
                Err(e) => {
                    tracing::debug!(error = %e, "the X server is not back yet");
                    backoff.failed();
                }
            }
        }
    }

    /// What a broken connection took with it: its grabs, hidden cursor and
    /// passive grabs. What was down is reported released, so other devices
    /// let go of it, and keys stop repeating.
    fn lost(&mut self) {
        self.grabbed = false;
        if std::mem::take(&mut self.waiting_for_grab) {
            self.shared.set_waiting(false);
        }
        self.cursor_hidden = false;
        self.show_on_warp = false;
        self.displays = None;
        // Read afresh on the next connection: where they were is nothing
        // to measure from.
        self.devices.clear();
        {
            let mut reserved = self.shared.hotkeys();
            reserved.grabs.clear();
            self.shared.set_held(HeldHotkeys::default());
        }
        {
            let mut timer = self.shared.timer();
            timer.keys.clear();
            timer.grabbed = false;
            timer.show_at = None;
            timer.retry_grab_at = None;
            timer.retry_hotkeys_at = None;
            let (keys, buttons) = self.reports.release_all();
            for usage in keys {
                let _ = self.events.send(InputEvent::Key {
                    usage,
                    pressed: false,
                });
            }
            for button in buttons {
                let _ = self.events.send(InputEvent::Button {
                    button,
                    pressed: false,
                });
            }
        }
        self.shared.timer_wake.notify_all();
    }

    /// Waits `backoff` while there is no connection, keeping track of
    /// what the agent asks meanwhile. Returns whether to stop.
    fn wait_offline(&mut self, backoff: Duration) -> bool {
        let deadline = Instant::now() + backoff;
        let mut commands = self.shared.commands();
        loop {
            if commands.stop {
                return true;
            }
            for command in std::mem::take(&mut commands.queue) {
                match command {
                    Command::Grab => self.want_grabbed = true,
                    Command::Ungrab => self.want_grabbed = false,
                    Command::Reserve(hotkeys, done) => {
                        self.shared.hotkeys().hotkeys = hotkeys;
                        let _ = done.send(HeldHotkeys::default());
                    }
                    Command::ResumeHotkeys => self.shared.hotkeys().paused = false,
                    // Grabbed again once connected.
                    Command::Warp(_)
                    | Command::ShowCursor
                    | Command::RetryGrab
                    | Command::RetryHotkeys => {}
                    #[cfg(test)]
                    Command::Settle(done) => {
                        let _ = done.send(());
                    }
                }
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            commands = self
                .shared
                .commanded
                .wait_timeout(commands, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Reads and serves a connection until it breaks or capture stops;
    /// `listening` when its events are selected already.
    fn serve(&mut self, link: &Link, listening: bool) -> Served {
        if !listening && let Err(e) = self.listen(link) {
            return Served::Lost(e.to_string());
        }
        if self.want_grabbed {
            self.grab(link);
        }
        self.run_commands(link);
        loop {
            let first = match link.conn.wait_for_event() {
                Ok(e) => e,
                Err(e) => return Served::Lost(e.to_string()),
            };
            let mut batch = vec![first];
            while let Ok(Some(e)) = link.conn.poll_for_event() {
                batch.push(e);
            }
            if self.shared.stopping() {
                return Served::Stopped;
            }
            self.run_commands(link);
            if self.read(link, batch).is_err() {
                // Nobody listens any more.
                return Served::Stopped;
            }
            if std::mem::take(&mut self.mapping_changed) {
                self.reserve_again(link, false);
            }
        }
    }

    /// Selects the events capture reads, and reads the devices and how
    /// keys repeat.
    fn listen(&mut self, link: &Link) -> Result<(), PlatformError> {
        let mask = XIEventMask::RAW_MOTION
            | XIEventMask::RAW_BUTTON_PRESS
            | XIEventMask::RAW_BUTTON_RELEASE
            | XIEventMask::RAW_KEY_PRESS
            | XIEventMask::RAW_KEY_RELEASE
            | XIEventMask::HIERARCHY
            | XIEventMask::DEVICE_CHANGED
            | XIEventMask::PROPERTY;
        // Every device: each raw event then comes from the device it
        // happened on as well as from its master; only the first copy is
        // read (`Worker::reads`).
        link.conn
            .xinput_xi_select_events(
                link.root,
                &[xinput::EventMask {
                    deviceid: ALL_DEVICES,
                    mask: vec![mask],
                }],
            )
            .map_err(|e| os("cannot read input", e))?
            .check()
            .map_err(|e| os("cannot read input", e))?;
        // The root window's size and the monitors, which change together.
        let _ = link.conn.randr_select_input(
            link.root,
            NotifyMask::SCREEN_CHANGE | NotifyMask::CRTC_CHANGE | NotifyMask::OUTPUT_CHANGE,
        );
        // Mapping changes, after which the hotkeys are grabbed again, and
        // changes of how keys repeat.
        if link.xkb {
            let events = xkb::EventType::NEW_KEYBOARD_NOTIFY
                | xkb::EventType::MAP_NOTIFY
                | xkb::EventType::CONTROLS_NOTIFY;
            let _ = link.conn.xkb_select_events(
                xkb::ID::USE_CORE_KBD.into(),
                xkb::EventType::from(0u16),
                events,
                xkb::MapPart::from(0xffu16),
                xkb::MapPart::from(0xffu16),
                &xkb::SelectEventsAux::new(),
            );
        }
        let screen = link.conn.setup().roots.iter().find(|s| s.root == link.root);
        if let Some(s) = screen {
            self.root_size = Point::new(s.width_in_pixels.into(), s.height_in_pixels.into());
        }
        self.displays = None;
        self.shared.set_repeat_settings(link.repeat_settings());
        self.natural_atom = link
            .conn
            .intern_atom(true, b"libinput Natural Scrolling Enabled")
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|r| r.atom)
            .filter(|&a| a != u32::from(AtomEnum::NONE));
        self.refresh_devices(link);
        Ok(())
    }

    fn run_commands(&mut self, link: &Link) {
        for command in self.shared.take_commands() {
            match command {
                Command::Grab => {
                    self.want_grabbed = true;
                    self.grab(link);
                }
                Command::RetryGrab => self.grab(link),
                Command::Ungrab => {
                    self.want_grabbed = false;
                    if std::mem::take(&mut self.waiting_for_grab) {
                        self.shared.retry_grab_at(None);
                        self.shared.set_waiting(false);
                    }
                    if self.grabbed {
                        self.ungrab(link);
                    }
                }
                Command::Warp(to) => {
                    if let Err(e) = link.warp(to) {
                        tracing::warn!(error = %e, "could not move the local cursor");
                    }
                    self.show_if_due(link);
                }
                Command::ShowCursor => self.show_if_due(link),
                Command::Reserve(hotkeys, done) => {
                    let mut reserved = self.shared.hotkeys();
                    let held = self.shared.reserve(link, &mut reserved, &hotkeys, false);
                    let _ = done.send(held);
                }
                Command::ResumeHotkeys => {
                    let paused = std::mem::take(&mut self.shared.hotkeys().paused);
                    if paused {
                        self.reserve_again(link, false);
                    }
                }
                Command::RetryHotkeys => {
                    let left = !self.shared.hotkeys().left.is_empty();
                    if left {
                        self.reserve_again(link, true);
                    }
                }
                #[cfg(test)]
                Command::Settle(done) => {
                    let _ = link.conn.sync();
                    let _ = done.send(());
                }
            }
        }
    }

    /// The monitors, read again once RandR said they changed or they are
    /// stale.
    fn displays(&mut self, link: &Link) -> Result<&[Display], PlatformError> {
        if self
            .displays
            .as_ref()
            .is_none_or(|(_, at)| at.elapsed() >= DISPLAYS_STALE)
        {
            self.displays = Some((monitors(&link.conn, link.root)?, Instant::now()));
        }
        Ok(self.displays.as_ref().map_or(&[], |(d, _)| d.as_slice()))
    }

    /// Grabs, hides the cursor and holds it in the middle of its display,
    /// if the agent wants that and it is not so yet. When another client
    /// holds a grab, input waits and the timer has it tried again. The
    /// cursor is never left hidden without a grab.
    fn grab(&mut self, link: &Link) {
        if !self.want_grabbed || self.grabbed {
            return;
        }
        let hold = link.pointer().and_then(|at| {
            let displays = self.displays(link)?;
            Ok(hold_point(displays, at).unwrap_or(at))
        });
        let grabbed = hold.and_then(|hold| link.grab().map(|()| hold));
        let hold = match grabbed {
            Ok(hold) => hold,
            Err(e) => {
                if !std::mem::replace(&mut self.waiting_for_grab, true) {
                    tracing::warn!(error = %e, "cannot take the keyboard and mouse yet; their input waits until it can");
                    self.shared.set_waiting(true);
                }
                self.shared.retry_grab_at(Some(Instant::now() + GRAB_RETRY));
                return;
            }
        };
        // A cursor still hidden from the last grab, not shown yet, stays
        // so: hiding twice would need showing twice.
        if !self.cursor_hidden {
            if let Err(e) = link
                .conn
                .xfixes_hide_cursor(link.root)
                .map_err(|e| os("cannot hide the cursor", e))
            {
                link.ungrab();
                tracing::warn!(error = %e, "could not grab local input");
                self.shared.retry_grab_at(Some(Instant::now() + GRAB_RETRY));
                return;
            }
            self.cursor_hidden = true;
        }
        self.show_on_warp = false;
        self.shared.show_cursor_at(None);
        if let Err(e) = link.warp(hold) {
            tracing::debug!(error = %e, "could not hold the cursor in the middle");
        }
        if std::mem::take(&mut self.waiting_for_grab) {
            tracing::info!("took the keyboard and mouse after all");
            self.shared.set_waiting(false);
        }
        self.grabbed = true;
        self.hold = hold;
        self.drift.reset();
        self.shared.grabbed(true);
    }

    /// Lets go of the grab; the cursor shows at the warp that comes next.
    fn ungrab(&mut self, link: &Link) {
        link.ungrab();
        self.grabbed = false;
        self.shared.grabbed(false);
        // Their first event after this is a jump from where they pointed
        // while grabbed.
        for device in self.devices.values_mut() {
            for axis in device.absolute.iter_mut().flatten() {
                axis.last = None;
            }
        }
        if self.cursor_hidden {
            self.show_on_warp = true;
            self.shared
                .show_cursor_at(Some(Instant::now() + SHOW_LATER));
        }
    }

    fn show_if_due(&mut self, link: &Link) {
        if self.show_on_warp && !self.grabbed {
            link.show_cursor();
            self.show_on_warp = false;
            self.cursor_hidden = false;
            self.shared.show_cursor_at(None);
        }
    }

    /// Lets go of everything as capture stops.
    fn finish(&mut self, link: &Link) {
        if self.grabbed {
            link.ungrab();
        }
        if self.cursor_hidden {
            link.show_cursor();
        }
        link.ungrab_keys(&std::mem::take(&mut self.shared.hotkeys().grabs));
        let _ = link.conn.destroy_window(link.window);
        let _ = link.conn.sync();
    }

    /// Grabs the reserved hotkeys again, hotkey by hotkey (`Link::regrab`):
    /// on a new connection, a changed keyboard mapping or injection letting
    /// go of them, or `again`, trying the ones another app held.
    fn reserve_again(&self, link: &Link, again: bool) {
        let mut reserved = self.shared.hotkeys();
        if reserved.hotkeys.is_empty() || reserved.paused {
            return;
        }
        let hotkeys = reserved.hotkeys.clone();
        let was = self.shared.held().grabbed.len();
        let held = self.shared.reserve(link, &mut reserved, &hotkeys, again);
        let held = held.len();
        if held < was {
            tracing::warn!("fewer hotkeys can be held back now; the others reach local apps");
        }
    }

    /// Whether input waits for a grab another client holds: nothing goes
    /// out then but releases of what went out down.
    fn waiting(&self) -> bool {
        self.want_grabbed && !self.grabbed
    }

    fn read(&mut self, link: &Link, batch: Vec<Event>) -> Result<(), ()> {
        let mut pending = Pending::default();
        for event in batch {
            match event {
                Event::XinputRawMotion(e) => {
                    let values = raw_values(&e.valuator_mask, &e.axisvalues);
                    let span = self.root_size;
                    let waiting = self.waiting();
                    let Some(device) = self.own_mut(e.deviceid, e.sourceid) else {
                        continue;
                    };
                    let scroll = device.axes.scroll(&values);
                    let mut delta = Point::new(0.0, 0.0);
                    let mut moved = false;
                    let relative = if let Some(d) = device.axes.motion(&values) {
                        delta = d;
                        moved = true;
                        true
                    } else {
                        // Followed even while input waits, so the next
                        // movement after is not a jump.
                        for &(axis, value) in &values {
                            let Some(Some(a)) = device.absolute.get_mut(usize::from(axis)) else {
                                continue;
                            };
                            if axis == 0 {
                                delta.x += a.delta(value, span.x);
                            } else {
                                delta.y += a.delta(value, span.y);
                            }
                            moved = true;
                        }
                        false
                    };
                    // Scrolling between the two presses of the double press
                    // starts it over, whether or not it goes out.
                    if scroll.is_some() {
                        self.reports.interrupt_double_press();
                    }
                    if waiting {
                        continue;
                    }
                    if moved {
                        pending.delta = pending.delta + delta;
                        pending.moved = true;
                        pending.relative |= relative;
                    }
                    if let Some((dx, dy)) = scroll {
                        self.flush(link, &mut pending)?;
                        self.send(InputEvent::Scroll { dx, dy })?;
                    }
                }
                Event::XinputRawButtonPress(e) | Event::XinputRawButtonRelease(e) => {
                    let Some(device) = self.own(e.deviceid, e.sourceid) else {
                        continue;
                    };
                    let pressed = e.event_type == xinput::RAW_BUTTON_PRESS_EVENT;
                    let emulated =
                        u32::from(e.flags) & u32::from(PointerEventFlags::POINTER_EMULATED) != 0;
                    let scrolls_smoothly = device.axes.scrolls_smoothly(e.detail);
                    self.flush(link, &mut pending)?;
                    if let Some(button) = button_of(e.detail) {
                        self.button(button, pressed)?;
                        continue;
                    }
                    // A wheel click, or a button capture does not report:
                    // the double press starts over, as for any button.
                    self.reports.interrupt_double_press();
                    if let Some((dx, dy)) = wheel_notches(e.detail)
                        && pressed
                        && !emulated
                        && !scrolls_smoothly
                        && !self.waiting()
                    {
                        self.send(InputEvent::Scroll { dx, dy })?;
                    }
                }
                Event::XinputRawKeyPress(e) | Event::XinputRawKeyRelease(e) => {
                    if self.own(e.deviceid, e.sourceid).is_none() {
                        continue;
                    }
                    self.flush(link, &mut pending)?;
                    let pressed = e.event_type == xinput::RAW_KEY_PRESS_EVENT;
                    if let (Some(usage), Ok(code)) = (key_of(e.detail), u8::try_from(e.detail)) {
                        self.key(code, usage, pressed, e.time)?;
                    }
                }
                Event::XinputHierarchy(_) => self.refresh_devices(link),
                // Not when another device of a master takes over
                // (`SLAVE_SWITCH`): only a device's own classes changing
                // changes how it is read.
                Event::XinputDeviceChanged(e) if e.reason == ChangeReason::DEVICE_CHANGE => {
                    self.refresh_devices(link);
                }
                Event::XinputProperty(e) if Some(e.property) == self.natural_atom => {
                    self.natural_scrolling_changed(link, e.deviceid);
                }
                Event::RandrScreenChangeNotify(e) => {
                    let quarter = u16::from(Rotation::ROTATE90) | u16::from(Rotation::ROTATE270);
                    let turned = u16::from(e.rotation) & quarter != 0;
                    self.root_size = root_size(e.width, e.height, turned);
                    self.displays = None;
                }
                Event::RandrNotify(_) => self.displays = None,
                // Each change comes as several of these: grabbed again once
                // after the batch.
                Event::MappingNotify(_)
                | Event::XkbMapNotify(_)
                | Event::XkbNewKeyboardNotify(_) => self.mapping_changed = true,
                Event::XkbControlsNotify(_) => {
                    self.shared.set_repeat_settings(link.repeat_settings());
                }
                Event::Error(e) => {
                    tracing::debug!(error = ?e, "the X server refused a request of capture's")
                }
                _ => {}
            }
        }
        self.flush(link, &mut pending)
    }

    fn send(&self, event: InputEvent) -> Result<(), ()> {
        self.events.send(event).map_err(|_| ())
    }

    /// Reports a button, but while input waits for a grab only the release
    /// of one reported down (`Reports`).
    fn button(&mut self, button: iw_core::input::Button, pressed: bool) -> Result<(), ()> {
        if !self.reports.button(button, pressed, self.waiting()) {
            return Ok(());
        }
        self.send(InputEvent::Button { button, pressed })
    }

    /// Reports a key, but while input waits for a grab only the release of
    /// one reported down, the hotkeys the router acts on and, as such, the
    /// double press, timed by `time`, the server's (`Reports`); and has the
    /// timer repeat one reported down while grabbed. Under the timer's
    /// lock, so a repeat never follows the release.
    fn key(&mut self, code: u8, usage: HidUsage, pressed: bool, time: u32) -> Result<(), ()> {
        let waiting = self.waiting();
        let out = if waiting {
            let watched = self.shared.watched();
            self.reports.key(usage, pressed, true, &watched, time)
        } else {
            self.reports
                .key(usage, pressed, false, &Watched::default(), time)
        };
        let mut timer = self.shared.timer();
        let t = &mut *timer;
        if !pressed {
            t.keys.release(code);
        } else if out.contains(&(usage, true)) {
            t.keys.press(code, usage, &t.settings, Instant::now());
        }
        self.shared.timer_wake.notify_all();
        for (usage, pressed) in out {
            self.events
                .send(InputEvent::Key { usage, pressed })
                .map_err(|_| ())?;
        }
        if self.reports.take_double_press() {
            self.send(InputEvent::DoublePressed)?;
        }
        Ok(())
    }

    /// Whether to read a raw event: the copy from the device itself (not
    /// its master's), of a device that is not XTest's.
    fn reads(&self, deviceid: u16, sourceid: u16) -> bool {
        deviceid == sourceid
            && (self.report_injected || !self.devices.get(&sourceid).is_some_and(|d| d.xtest))
    }

    fn own(&self, deviceid: u16, sourceid: u16) -> Option<&Device> {
        const UNKNOWN: &Device = &Device {
            xtest: false,
            axes: Axes {
                absolute: false,
                scroll: Vec::new(),
                natural: false,
            },
            absolute: [None, None],
        };
        self.reads(deviceid, sourceid)
            .then(|| self.devices.get(&sourceid).unwrap_or(UNKNOWN))
    }

    fn own_mut(&mut self, deviceid: u16, sourceid: u16) -> Option<&mut Device> {
        if !self.reads(deviceid, sourceid) {
            return None;
        }
        Some(self.devices.entry(sourceid).or_default())
    }

    /// Reports the motion read since the last report: at where the cursor
    /// is, or, while grabbed, where it is held. While grabbed the hidden
    /// cursor still moves with the mouse, by however far the server's
    /// acceleration takes it; once it is far from the middle, it is put
    /// back there.
    fn flush(&mut self, link: &Link, pending: &mut Pending) -> Result<(), ()> {
        let pending = std::mem::take(pending);
        if !pending.moved || self.waiting() {
            return Ok(());
        }
        let position = if self.grabbed {
            // Read where it really is only every so often: the movements
            // read are the cursor's, acceleration and all, but it stops at
            // edges, so only the real position says to put it back.
            if pending.relative && self.drift.moved(pending.delta) {
                match link.pointer() {
                    Ok(at)
                        if (at.x - self.hold.x).abs() > DRIFT
                            || (at.y - self.hold.y).abs() > DRIFT =>
                    {
                        let _ = link.warp(self.hold);
                    }
                    Ok(_) => {}
                    Err(e) => tracing::debug!(error = %e, "cannot read where the cursor is"),
                }
            }
            self.hold
        } else {
            match link.pointer() {
                Ok(at) => at,
                Err(e) => {
                    tracing::debug!(error = %e, "cannot read where the cursor is");
                    return Ok(());
                }
            }
        };
        let delta = pending.delta;
        if delta.x == 0.0 && delta.y == 0.0 {
            return Ok(());
        }
        self.send(InputEvent::PointerMotion { position, delta })
    }

    /// Reads which devices there are: the XTest ones, and how each pointer
    /// reports moving and scrolling.
    fn refresh_devices(&mut self, link: &Link) {
        let reply = match link
            .conn
            .xinput_xi_query_device(ALL_DEVICES)
            .map(|c| c.reply())
        {
            Ok(Ok(reply)) => reply,
            _ => {
                tracing::debug!("cannot list the input devices");
                return;
            }
        };
        let mut devices = HashMap::new();
        for info in reply.infos {
            let slave = matches!(
                info.type_,
                DeviceType::SLAVE_POINTER | DeviceType::SLAVE_KEYBOARD | DeviceType::FLOATING_SLAVE
            );
            let mut device = Device {
                xtest: slave && is_xtest_device(&info.name),
                ..Device::default()
            };
            for class in &info.classes {
                match &class.data {
                    DeviceClassData::Valuator(v) if v.number < 2 => {
                        if v.mode == ValuatorMode::ABSOLUTE {
                            device.axes.absolute = true;
                            let (min, max) = (
                                fixed(v.min.integral, v.min.frac),
                                fixed(v.max.integral, v.max.frac),
                            );
                            // Where it last was, if it was known before.
                            let last = self
                                .devices
                                .get(&info.deviceid)
                                .and_then(|d| d.absolute[usize::from(v.number)])
                                .and_then(|a| a.last);
                            device.absolute[usize::from(v.number)] = Some(AbsoluteAxis {
                                last,
                                ..AbsoluteAxis::new(min, max)
                            });
                        }
                    }
                    DeviceClassData::Scroll(s) => device.axes.scroll.push(ScrollAxis {
                        number: s.number,
                        vertical: s.scroll_type == ScrollType::VERTICAL,
                        increment: fixed(s.increment.integral, s.increment.frac),
                    }),
                    _ => {}
                }
            }
            if !device.axes.scroll.is_empty()
                && let Some(atom) = self.natural_atom
            {
                device.axes.natural = natural_scrolling(&link.conn, info.deviceid, atom);
            }
            devices.insert(info.deviceid, device);
        }
        self.devices = devices;
    }

    /// The user turned natural scrolling on or off for a device.
    fn natural_scrolling_changed(&mut self, link: &Link, deviceid: u16) {
        let Some(atom) = self.natural_atom else {
            return;
        };
        if let Some(device) = self.devices.get_mut(&deviceid)
            && !device.axes.scroll.is_empty()
        {
            device.axes.natural = natural_scrolling(&link.conn, deviceid, atom);
        }
    }
}

fn raw_values(mask: &[u32], values: &[xinput::Fp3232]) -> Vec<(u16, f64)> {
    let values: Vec<f64> = values.iter().map(|v| fixed(v.integral, v.frac)).collect();
    valuators(mask, &values)
}

/// Whether libinput's natural scrolling is on for a device.
fn natural_scrolling(conn: &RustConnection, device: u16, atom: u32) -> bool {
    let reply = conn
        .xinput_xi_get_property(device, false, atom, u32::from(AtomEnum::ANY), 0, 1)
        .map(|c| c.reply());
    matches!(reply, Ok(Ok(r)) if matches!(r.items, XIGetPropertyItems::Data8(ref d) if d.first() == Some(&1)))
}

#[cfg(test)]
mod retry_tests {
    use super::*;

    /// Hotkeys another app holds are tried again after 3 s, then twice as
    /// long after each try that leaves the same ones, five times.
    #[test]
    fn hotkeys_another_app_holds_are_tried_again_less_and_less_often() {
        let waits: Vec<_> = (0..7).map(retry_after).collect();
        let secs = |s| Some(Duration::from_secs(s));
        assert_eq!(
            waits,
            vec![secs(3), secs(6), secs(12), secs(24), secs(48), None, None]
        );
        assert!(waits.iter().flatten().all(|w| *w <= RETRY_HOTKEYS_MAX));
    }
}
