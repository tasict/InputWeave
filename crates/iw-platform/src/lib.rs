//! OS integration behind small traits, so the agent can run against real
//! backends or the [`mock`] ones.
//!
//! Platform backends live in `cfg(target_os)`-gated modules built on
//! binding-only crates (`objc2-*`, `windows`) and, on Linux, pure Rust
//! protocol crates (`x11rb`, `zbus`): `macos`, `windows` and `linux`.
//! [`native`] returns the ones for the running OS. The FFI-free parts of
//! the Windows and Linux backends also build in tests on other systems.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Sender;

use iw_core::HidUsage;
use iw_core::geometry::Point;
use iw_core::hotkey::HeldHotkeys;
use iw_core::input::{Button, InputEvent};
use iw_core::layout::{Display, Exit};
use iw_core::router::Hotkey;

#[cfg(any(target_os = "macos", test))]
pub(crate) mod hid_report;
#[cfg(any(target_os = "linux", test))]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
pub mod mock;
pub mod network;
pub mod preview;
pub mod system;
/// Compiled everywhere for `windows::clipboard_representations`, which the
/// agent's tests use too; the rest of it only on Windows and in tests.
pub mod windows;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Permission {
    Accessibility,
    InputMonitoring,
    LocalNetwork,
    /// Wayland: other devices may control this one, through the desktop's
    /// RemoteDesktop portal.
    RemoteControl,
    /// Wayland: this device's keyboard and mouse may control others,
    /// through the desktop's InputCapture portal.
    InputCapture,
}

impl Permission {
    pub const ALL: [Permission; 5] = [
        Self::Accessibility,
        Self::InputMonitoring,
        Self::LocalNetwork,
        Self::RemoteControl,
        Self::InputCapture,
    ];

    /// Its name as people read it, such as "input monitoring".
    pub fn name(self) -> &'static str {
        match self {
            Self::Accessibility => "accessibility",
            Self::InputMonitoring => "input monitoring",
            Self::LocalNetwork => "local network",
            Self::RemoteControl => "remote control",
            Self::InputCapture => "input capture",
        }
    }

    /// Its name on command lines, such as `input-monitoring`.
    pub fn key(self) -> &'static str {
        match self {
            Self::Accessibility => "accessibility",
            Self::InputMonitoring => "input-monitoring",
            Self::LocalNetwork => "local-network",
            Self::RemoteControl => "remote-control",
            Self::InputCapture => "input-capture",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.key() == key)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionState {
    pub permission: Permission,
    pub granted: bool,
    /// Whether the agent asks for it itself, and what came of asking.
    pub ask: PermissionAsk,
    /// How many times the agent asked for it since it started
    /// ([`SessionStatus::request_permission`]): the request this state
    /// answers, so it can be told to be from after a given one.
    pub asked: u64,
    /// Why asking failed ([`PermissionAsk::Failed`]).
    pub failure: Option<String>,
}

impl PermissionState {
    /// A permission the user grants elsewhere, as in macOS's System
    /// Settings, which the agent never asks for.
    pub fn new(permission: Permission, granted: bool) -> Self {
        Self {
            permission,
            granted,
            ask: PermissionAsk::NotAskable,
            asked: 0,
            failure: None,
        }
    }

    /// As `ctl status` and diagnostics list it, such as "remote control
    /// missing (being asked)".
    pub fn describe(&self) -> String {
        let state = if self.granted { "granted" } else { "missing" };
        let ask = match (self.granted, self.ask) {
            (_, PermissionAsk::Asking) => " (being asked)".to_owned(),
            (false, PermissionAsk::Askable) => format!(
                " (not asked yet; `inputweave ctl permission {}` asks)",
                self.permission.key()
            ),
            (false, PermissionAsk::Restoring) => {
                " (starting with what the user allowed before)".to_owned()
            }
            (false, PermissionAsk::Stopped) => format!(
                " (the user stopped it; `inputweave ctl permission {}` asks again)",
                self.permission.key()
            ),
            (false, PermissionAsk::Denied) => " (the user said no)".to_owned(),
            (false, PermissionAsk::Failed) => match &self.failure {
                Some(why) => format!(" (asking failed: {why})"),
                None => " (asking failed)".to_owned(),
            },
            _ => String::new(),
        };
        format!("{} {state}{ask}", self.permission.name())
    }
}

/// Where the agent asks for a permission itself
/// ([`SessionStatus::request_permission`]), as the desktop's portals let it
/// on Wayland: what came of the last time it asked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PermissionAsk {
    /// The agent cannot ask: the user grants it elsewhere.
    #[default]
    NotAskable,
    /// The agent can ask, and has not since it started.
    Askable,
    /// Starting with what the user allowed before, as the desktop
    /// remembers it: no dialog is expected.
    Restoring,
    /// The desktop is asking the user.
    Asking,
    /// The user stopped what they allowed (sharing); the agent can ask
    /// again.
    Stopped,
    /// The user said no the last time.
    Denied,
    /// Asking failed: the desktop has no such portal, or it broke.
    Failed,
}

#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    #[error("missing permission: {0:?}")]
    PermissionDenied(Permission),
    #[error("not supported on this platform: {0}")]
    Unsupported(&'static str),
    /// Nothing can do it for now, as on Windows while no user is logged
    /// on to run the broker; worth no warning.
    #[error("not available now: {0}")]
    Unavailable(&'static str),
    /// Not yet: what the user allowed before is starting, as a Wayland
    /// desktop's portal session does at every login. It comes once the
    /// permission shows as granted; worth no warning.
    #[error("starting: {0}")]
    Starting(&'static str),
    #[error("{0}")]
    Os(String),
}

/// Whether this computer's keyboard can report `key` at all: whether this
/// platform has a key code for it. A Mac has none for Scroll Lock or Pause.
pub fn keyboard_reports(key: HidUsage) -> bool {
    #[cfg(target_os = "macos")]
    return key.to_mac().is_some();
    #[cfg(windows)]
    return key.to_windows().is_some();
    #[cfg(target_os = "linux")]
    return key.to_evdev().is_some();
    #[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
    {
        let _ = key;
        true
    }
}

/// Reads local keyboard and mouse input.
///
/// Implementations run their own thread (with a run loop or message loop as
/// the OS requires) and push events into the channel given to `start`.
///
/// Implementations must report only physical input and never events that
/// this process injected (Windows marks them `LLMHF_INJECTED`; on macOS the
/// agent tags its own event source). Otherwise the receiver would mistake
/// its own injections for the local user taking over, and the router would
/// see phantom motion.
pub trait InputCapture: Send {
    fn start(&mut self, events: Sender<InputEvent>) -> Result<(), PlatformError>;

    /// While grabbed, input is consumed instead of reaching local apps and
    /// the local cursor is hidden and held in place.
    fn set_grabbed(&mut self, grabbed: bool) -> Result<(), PlatformError>;

    /// Moves the local cursor, in this machine's desktop coordinates.
    fn warp(&mut self, position: Point) -> Result<(), PlatformError>;

    /// Hotkeys InputWeave acts on whichever device the cursor is on. Their
    /// key is kept from local apps even while input is not grabbed, and is
    /// still reported. Backends that cannot hold keys back while not
    /// grabbed let them through to local apps.
    ///
    /// Called again whenever the user changes the hotkeys, with the whole
    /// new set. A key already held back stays so until it is released, and
    /// a key that went down before its hotkey was reserved stays the apps'.
    ///
    /// Returns those of them this backend holds back, so local apps never
    /// see their keys go down; the others reach local apps. A backend that
    /// may lose some or get them back afterwards (X11, as another app grabs
    /// or lets go of one) tells each change as it comes, in order with the
    /// keys (`InputEvent::HotkeysHeldBack`).
    fn reserve_hotkeys(&mut self, hotkeys: &[Hotkey]) -> HeldHotkeys {
        let _ = hotkeys;
        HeldHotkeys::default()
    }

    /// Every hotkey the router acts on while input is grabbed: taking
    /// control back, locking, jumping and relative mode. A backend that may
    /// hold input back while grabbed but not yet holding the input (X11,
    /// while another app holds a grab) still reports these, so control can
    /// be taken back meanwhile. Called again whenever they change; nothing
    /// is done with them by default.
    fn watch_hotkeys(&mut self, hotkeys: &[Hotkey]) {
        let _ = hotkeys;
    }

    /// Whether Control pressed twice quickly takes control back
    /// (`Hotkeys::take_back_double`). A backend that may hold input back
    /// while grabbed but not yet holding the input (X11, while another app
    /// holds a grab) tells such a double press meanwhile as
    /// `InputEvent::DoublePressed` once it is complete
    /// (`iw_core::hotkey::DoublePressWatch`), and none of those keys, which
    /// went to that app. Called again whenever it changes; nothing is done
    /// with it by default.
    fn watch_double_press(&mut self, on: bool) {
        let _ = on;
    }

    /// Whether a key or button the local session saw go down before input
    /// was grabbed must be let go of locally as it is released or as the
    /// cursor leaves (`Action::LocalKey`, `Action::LocalButton`): not where
    /// grabbing takes the keyboard and pointer from the local apps already,
    /// as a Wayland desktop does as it hands input over (the focused app
    /// loses them, and with them every key and button down), and injecting
    /// the release would let go of what the injecting device never pressed.
    fn releases_carried_input_locally(&self) -> bool {
        true
    }

    /// Whether capture still captures, once started: a backend whose
    /// capture may stop for good by itself (the desktop's session closed,
    /// as when the user stopped it) says so here, and the agent starts it
    /// again once its permission is granted again. Asked about once a
    /// second, so it must be cheap.
    fn capturing(&self) -> bool {
        true
    }

    /// Whether `warp` moves the cursor now. Wayland lets a client move the
    /// pointer only as input handed over goes back to the desktop; the
    /// agent moves it otherwise through input emulation.
    fn can_warp(&self) -> bool {
        true
    }

    /// Whether this backend needs to know where the cursor leaves this
    /// device (`set_exits`), which takes a while to work out.
    fn wants_exits(&self) -> bool {
        false
    }

    /// Where the cursor leaves this device for another now
    /// (`Router::exits`), for a backend that `wants_exits`: called again
    /// whenever that changes, empty while it leaves nowhere, as while
    /// another device controls this one. A backend that only gets input
    /// when the desktop hands it over at the edges of the screen (Wayland's
    /// InputCapture portal) asks for it there.
    fn set_exits(&mut self, exits: &[Exit]) {
        let _ = exits;
    }

    /// Whether `set_grabbed(true)` takes hold of input now. A backend that
    /// only gets input when the desktop hands it over (Wayland's
    /// InputCapture portal, as the cursor reaches an exit) cannot grab at
    /// will: the cursor leaves this device across an edge, not by a jump
    /// from here.
    fn grab_ready(&self) -> bool {
        true
    }

    /// After each input event the router handled while input was not
    /// grabbed: whether the cursor now waits at an edge to cross, pushing
    /// against it for the dwell time (`EdgePolicy.dwell_ms`). `handled`
    /// counts the events taken from the channel given to `start`, this one
    /// included, so the backend knows which event this answers. A backend
    /// that holds input at an edge as the desktop hands it over (Wayland's
    /// InputCapture portal) lets go of it unless the cursor crosses or
    /// waits to; others need not know.
    fn edge_wait(&mut self, handled: u64, waiting: bool) {
        let _ = (handled, waiting);
    }

    /// Whether the cursor leaves this device only across the edges of its
    /// screens, never by a jump (the jump hotkeys, a click on another
    /// device in the app): a backend that only gets input as the desktop
    /// hands it over at an edge (Wayland's InputCapture portal). Unlike
    /// `grab_ready`, it does not change while the agent runs, so the apps
    /// can leave jumps out.
    fn edges_only(&self) -> bool {
        false
    }
}

/// Capture for a device that is only ever controlled, as the macOS login
/// window's agent: it reports nothing and never grabs.
#[derive(Debug, Default)]
pub struct NoCapture;

impl InputCapture for NoCapture {
    fn start(&mut self, _events: Sender<InputEvent>) -> Result<(), PlatformError> {
        Ok(())
    }

    fn set_grabbed(&mut self, _grabbed: bool) -> Result<(), PlatformError> {
        Ok(())
    }

    fn warp(&mut self, _position: Point) -> Result<(), PlatformError> {
        Ok(())
    }
}

/// Injects input into the local session.
pub trait InputEmulation: Send {
    /// Moves the cursor to `position` in this machine's desktop coordinates.
    fn move_to(&mut self, position: Point) -> Result<(), PlatformError>;
    fn button(&mut self, button: Button, pressed: bool) -> Result<(), PlatformError>;
    fn key(&mut self, usage: HidUsage, pressed: bool) -> Result<(), PlatformError>;
    fn scroll(&mut self, dx: f64, dy: f64) -> Result<(), PlatformError>;

    /// Moves the pointer by (`dx`, `dy`) logical units, as a mouse moving
    /// that far would: for relative mode, where a game that holds the
    /// pointer reads movements rather than positions. Fractions left over
    /// count towards the next movement.
    fn move_by(&mut self, dx: f64, dy: f64) -> Result<(), PlatformError> {
        let _ = (dx, dy);
        Err(PlatformError::Unsupported("relative pointer movements"))
    }

    /// Where the pointer is now, in logical units of this device's desktop,
    /// if the backend can tell: as relative mode ends, the device
    /// controlling this one carries on from there.
    fn position(&mut self) -> Option<Point> {
        None
    }

    /// Called about once a second, whether or not anything is injected, so
    /// a backend can catch up with the session: on macOS, hand input back
    /// from the virtual device once the lock screen is gone, even if it was
    /// unlocked at the Mac itself and nothing is injected anymore.
    fn refresh(&mut self) {}
}

/// Enumerates the local displays in logical desktop coordinates.
pub trait Displays: Send {
    fn displays(&self) -> Result<Vec<Display>, PlatformError>;
}

/// Small previews of the picture on each display's desktop, for the
/// arrangement canvas. Reading them needs no permission.
pub trait DesktopPictures: Send + Sync {
    /// A JPEG of the desktop picture on each of `displays`, by display id:
    /// its long edge at most [`preview::LONG_EDGE`] pixels and its size at
    /// most [`preview::MAX_BYTES`]. Displays showing a solid color, or a
    /// picture that cannot be read, are left out.
    ///
    /// It may decode large images, so call it off the async runtime.
    /// Backends keep what they made, so asking again about pictures that
    /// did not change is cheap.
    fn pictures(&self, displays: &[Display]) -> Result<HashMap<String, Vec<u8>>, PlatformError>;

    /// Whether this backend reads desktop pictures at all, so the UI can
    /// tell a computer that never sends them from one whose displays show
    /// a solid color. It may change while the agent runs, as when the
    /// process that reads them for the agent comes and goes.
    fn available(&self) -> bool {
        true
    }

    /// A number that changes whenever the pictures may have changed for a
    /// reason the displays do not show, as when another process sends
    /// new ones; the agent then asks for them again. Backends that only
    /// learn of a new picture when asked keep the default.
    fn generation(&self) -> u64 {
        0
    }
}

/// No previews, for platforms where nothing reads the desktop picture for
/// the agent.
#[derive(Debug, Default)]
pub struct NoDesktopPictures;

impl DesktopPictures for NoDesktopPictures {
    fn pictures(&self, _displays: &[Display]) -> Result<HashMap<String, Vec<u8>>, PlatformError> {
        Ok(HashMap::new())
    }

    fn available(&self) -> bool {
        false
    }
}

/// A clipboard representation InputWeave carries between devices.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ClipboardFormat {
    /// UTF-8 text.
    Text,
    Rtf,
    Html,
    Png,
}

impl ClipboardFormat {
    pub const ALL: [ClipboardFormat; 4] = [Self::Text, Self::Rtf, Self::Html, Self::Png];

    /// How the format travels between peers.
    pub fn mime(self) -> &'static str {
        match self {
            Self::Text => "text/plain;charset=utf-8",
            Self::Rtf => "text/rtf",
            Self::Html => "text/html",
            Self::Png => "image/png",
        }
    }

    pub fn from_mime(mime: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.mime() == mime)
    }
}

/// Clipboard contents: the same content in one or more formats.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClipboardContent {
    pub items: Vec<(ClipboardFormat, Vec<u8>)>,
}

impl ClipboardContent {
    pub fn len(&self) -> usize {
        self.items.iter().map(|(_, data)| data.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn get(&self, format: ClipboardFormat) -> Option<&[u8]> {
        self.items
            .iter()
            .find(|(f, _)| *f == format)
            .map(|(_, data)| data.as_slice())
    }
}

/// What reading the clipboard found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClipboardRead {
    Content(ClipboardContent),
    /// Nothing in a format InputWeave carries.
    Empty,
    /// The source marked it as not to be shared, as password managers do
    /// (macOS `org.nspasteboard.ConcealedType`, Windows
    /// `ExcludeClipboardContentFromMonitorProcessing`).
    Sensitive,
    /// Larger than the limit asked for, in bytes.
    TooLarge(usize),
}

/// Where a promised clipboard's content comes from when an app pastes:
/// another device, through the agent (docs/architecture.md §8).
pub trait ClipboardSource: Send + Sync {
    /// The bytes of `format`. The first call fetches the content in every
    /// promised format, blocking until it arrives, for at most
    /// [`PROMISE_WAIT`]; later calls answer from what was fetched, and,
    /// once fetching failed, at once with nothing. `None` when it cannot be
    /// had, as when the device it was copied on has had something else
    /// copied since.
    fn fetch(&self, format: ClipboardFormat) -> Option<Vec<u8>>;

    /// Like [`fetch`](Self::fetch), but telling a source still busy with
    /// an earlier request for the content from one that cannot give it,
    /// for a platform that asks again later rather than give up.
    fn try_fetch(&self, format: ClipboardFormat) -> Fetch {
        self.fetch(format)
            .map_or(Fetch::Unavailable, Fetch::Content)
    }

    /// The files promised (`ClipboardPromise::files`), fetched into a
    /// folder of the agent's the first time, which takes as long as they
    /// take to arrive: the call waits for them for at most
    /// [`PROMISE_WAIT`], and says `Busy` past that while they keep
    /// coming, so a later paste gets them. Ready, it has the local path of
    /// each promised file, in order, or `None` for one that could not be
    /// had.
    fn fetch_files(&self) -> FilesFetch {
        FilesFetch::Unavailable
    }

    /// Like [`fetch_files`](Self::fetch_files), but into `folder`, which
    /// the platform chose and the agent checked this device's user can
    /// write in, as on Windows, where the app pastes them; and without
    /// waiting: `done` hears what arrived once they all ended, for as long
    /// as they keep coming (the app shows the copy's progress, and the
    /// agent gives up on files that stall), or at once when they cannot be
    /// had. Every call hears, but only the first has them fetched.
    fn fetch_files_into(&self, folder: PathBuf, done: FilesDone) {
        let _ = folder;
        done(FilesFetch::Unavailable);
    }

    /// Like [`fetch`](Self::fetch), ahead of any paste, as for content
    /// handed along with files: content that cannot be had now is not
    /// taken to be gone, and a paste fetches it again.
    fn fetch_ahead(&self, format: ClipboardFormat) -> Option<Vec<u8>> {
        self.fetch(format)
    }

    /// The promise left the clipboard: something else was copied over it.
    /// What was fetched for it is let go. It never waits for a fetch.
    fn finished(&self) {}

    /// The id of the device the content was copied on, when known.
    fn origin(&self) -> Option<String> {
        None
    }
}

/// Hears what [`ClipboardSource::fetch_files_into`] found, once.
pub type FilesDone = Box<dyn FnOnce(FilesFetch) + Send>;

/// Whether nothing was written to a clipboard since a promise was handed
/// over ([`Clipboard::hand_over`]).
pub type Current = std::sync::Arc<dyn Fn() -> bool + Send + Sync>;

/// Hears, once, what came of a promise handed over.
pub type HandedOver = Box<dyn FnOnce(HandOver) + Send>;

/// What came of a promise handed over ([`Clipboard::hand_over`]).
pub enum HandOver {
    /// It is on the clipboard, or taken to be: what holds it lets its
    /// source know once it is not.
    Held,
    /// Not taken: it is the caller's again, to promise otherwise.
    Returned(ClipboardPromise),
}

/// What asking a [`ClipboardSource`] for its files found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FilesFetch {
    /// Where each promised file is now, in order; `None` for one that did
    /// not come.
    Ready(Vec<Option<PathBuf>>),
    /// Still coming; a later paste may get them.
    Busy,
    /// They cannot be had.
    Unavailable,
}

/// A file or folder promised on the clipboard, copied on another device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromisedFile {
    /// Its name, without any folder.
    pub name: String,
    pub folder: bool,
}

/// What asking a [`ClipboardSource`] for a format found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fetch {
    Content(Vec<u8>),
    /// Not now: the content is still on its way for an earlier request.
    /// Nothing failed, and a later paste may get it.
    Busy,
    /// It cannot be had.
    Unavailable,
}

/// How long pasting waits for promised content: under the 120 seconds
/// macOS gives a pasteboard's owner.
pub const PROMISE_WAIT: std::time::Duration = std::time::Duration::from_secs(100);

/// Clipboard content promised in some formats rather than written, fetched
/// from `source` only when an app pastes; and files, which
/// `ClipboardSource::fetch_files` brings.
#[derive(Clone)]
pub struct ClipboardPromise {
    pub formats: Vec<ClipboardFormat>,
    /// All formats together, in bytes, as offered.
    pub bytes: u64,
    /// Files and folders copied on another device, in order. Empty for
    /// content in formats only.
    pub files: Vec<PromisedFile>,
    pub source: std::sync::Arc<dyn ClipboardSource>,
}

impl std::fmt::Debug for ClipboardPromise {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClipboardPromise")
            .field("formats", &self.formats)
            .field("bytes", &self.bytes)
            .field("files", &self.files)
            .finish_non_exhaustive()
    }
}

/// The local clipboard.
pub trait Clipboard: Send {
    /// A number that changes whenever the clipboard's contents change.
    fn change_count(&self) -> Result<u64, PlatformError>;

    /// The contents in every format carried, unless they are sensitive or
    /// together larger than `max_len` bytes.
    fn read(&self, max_len: usize) -> Result<ClipboardRead, PlatformError>;

    /// Replaces the contents and returns the change count afterwards, so
    /// the write is not mistaken for a copy by the user.
    fn write(&mut self, content: &ClipboardContent) -> Result<u64, PlatformError>;

    /// Replaces the contents with a promise of `promise.formats`, whose
    /// bytes the platform asks `promise.source` for only when an app
    /// pastes, and returns the change count afterwards, as `write` does.
    /// Backends that cannot promise return `Unsupported`, and the agent
    /// fetches the content at once and writes it.
    fn write_promise(&mut self, promise: ClipboardPromise) -> Result<u64, PlatformError> {
        let _ = promise;
        Err(PlatformError::Unsupported("promising clipboard content"))
    }

    /// Files and folders on the clipboard, as copied in the file manager:
    /// their local paths, in order. Empty when there are none, and from
    /// backends that cannot read them. Whether the clipboard is sensitive
    /// is for `read` to say.
    fn files(&self) -> Result<Vec<PathBuf>, PlatformError> {
        Ok(Vec::new())
    }

    /// Whether this clipboard can promise files (`ClipboardPromise::files`).
    /// Offers of files are not taken otherwise: they cannot be written whole.
    fn promises_files(&self) -> bool {
        false
    }

    /// Whether, promising files, this clipboard chooses the folder they
    /// are fetched into itself (`ClipboardSource::fetch_files_into`), as
    /// the Windows app does, rather than the agent keeping one.
    fn stages_files(&self) -> bool {
        false
    }

    /// Hands `promise`, which has files, to what holds such promises for
    /// this clipboard when it cannot itself, as the Windows app does,
    /// without waiting for it: `done` hears, on another thread, whether it
    /// took it. Until then it may put it on the clipboard only while
    /// `current` says nothing else was written there since. Returns the
    /// promise when nothing takes such promises here.
    fn hand_over(
        &mut self,
        promise: ClipboardPromise,
        current: Current,
        done: HandedOver,
    ) -> Result<(), ClipboardPromise> {
        let _ = (current, done);
        Err(promise)
    }

    /// The most this clipboard takes in one write, in bytes, when it has a
    /// limit, as the Windows broker does. Larger content is not offered to
    /// it.
    fn max_write(&self) -> Option<usize> {
        None
    }

    /// Whether clipboard sync is on. A backend that reads the clipboard
    /// ahead of time, as the agent does through the Windows broker, reads
    /// nothing while it is off.
    fn set_sync(&mut self, on: bool) {
        let _ = on;
    }
}

/// Facts about the local session that the UI explains to the user. Checks
/// must be cheap and must never prompt; the agent polls them.
pub trait SessionStatus: Send + Sync {
    /// The permissions this platform needs, and whether each is granted.
    fn permissions(&self) -> Vec<PermissionState>;

    /// Whether another app has turned on secure keyboard entry (macOS),
    /// which keeps keystrokes from reaching capture.
    fn secure_input(&self) -> bool {
        false
    }

    /// Whether this session's screen is locked: the macOS lock screen, or
    /// a locked Windows session. Other devices are told, since input may
    /// not reach a locked Mac.
    fn screen_locked(&self) -> bool {
        false
    }

    /// Whether input from other devices reaches this session's lock
    /// screen: on Windows always, since the agent follows the secure
    /// desktop; on macOS only through the virtual keyboard and mouse, which
    /// need an entitlement from Apple. A controller does not move the
    /// cursor onto a locked device that says no.
    fn lock_screen_input(&self) -> bool {
        false
    }

    /// Whether an app here holds the pointer, hidden and kept in its window,
    /// as games do; the device controlling this one may then switch to
    /// relative mode. Windows tells; elsewhere it is never so.
    fn pointer_captured(&self) -> bool {
        false
    }

    /// Asks the user for `permission`, which `permissions` lists as one the
    /// agent asks for itself ([`PermissionAsk`]): the desktop shows its
    /// dialog. Returns at once, with how many times it was asked for now
    /// ([`PermissionState::asked`]); `permissions` says it is being asked
    /// about from then on, and later what came of it. Only the app calls
    /// this, as the user asked it to: the agent never asks by itself.
    fn request_permission(&self, permission: Permission) -> Result<u64, PlatformError> {
        let _ = permission;
        Err(PlatformError::Unsupported("asking for this permission"))
    }

    /// Whether input from other devices reaches this session now. It does
    /// not while the platform cannot inject at all: on Wayland until the
    /// desktop allows remote control, on macOS without the Accessibility
    /// permission. Other devices then keep the cursor off this one.
    fn input_available(&self) -> bool {
        true
    }

    /// What kind of session this is and what serves each part of it, for
    /// the app's troubleshooting page. `None` where there is nothing to
    /// choose between (macOS, Windows). Asked about once a second, so it
    /// must be cheap.
    fn session_info(&self) -> Option<SessionInfo> {
        None
    }

    /// Whether [`firewall`](Self::firewall) says anything here, so the
    /// agent asks it at all: on Linux, where ufw is read.
    fn checks_firewall(&self) -> bool {
        cfg!(target_os = "linux")
    }

    /// Whether a firewall here keeps other devices from reaching the
    /// agent on UDP `port` (and mDNS). Never prompts and never needs
    /// privileges: what cannot be read is [`Firewall::Unknown`]. Asked
    /// about once a second where [`checks_firewall`](Self::checks_firewall),
    /// off the async runtime (it may read files). On Linux, ufw, whatever
    /// the session (`linux::firewall`, read again only as its files
    /// change); nothing elsewhere.
    fn firewall(&self, port: u16) -> Firewall {
        #[cfg(target_os = "linux")]
        {
            crate::linux::firewall::ufw_now(port)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = port;
            Firewall::Unknown
        }
    }

    /// The network this device is on now ([`network::current`]); none
    /// while it has no route to the internet. Asked about once a second,
    /// off the async runtime. Never prompts and needs no permission.
    fn network(&self) -> Result<Option<network::NetworkSighting>, PlatformError> {
        network::current()
    }
}

/// What kind of graphical session the agent runs in, and what serves each
/// part of it ([`SessionStatus::session_info`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionInfo {
    pub kind: SessionType,
    /// The desktop, as `XDG_CURRENT_DESKTOP` names it ("GNOME", "KDE");
    /// empty when not known.
    pub desktop: String,
    pub backends: Vec<Backend>,
}

/// The kind of graphical session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SessionType {
    #[default]
    Unknown,
    X11,
    Wayland,
}

/// What serves one part of the session, or why nothing does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Backend {
    pub role: BackendRole,
    /// What serves it ("XTest", "RemoteDesktop portal"); empty when nothing
    /// does.
    pub implementation: String,
    /// Why nothing serves it, or what keeps it from working now; empty when
    /// it works.
    pub problem: String,
}

/// The parts of a session the agent needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendRole {
    /// Input from other devices.
    Input,
    /// This device's keyboard and mouse, for controlling others.
    Capture,
    Clipboard,
    Displays,
    DesktopPictures,
}

impl Backend {
    /// `role`, served by `implementation`.
    pub fn serving(role: BackendRole, implementation: &str) -> Self {
        Self {
            role,
            implementation: implementation.to_owned(),
            problem: String::new(),
        }
    }

    /// `role`, served by `implementation` but kept from working now by
    /// `problem` (none when empty).
    pub fn held_back(role: BackendRole, implementation: &str, problem: &str) -> Self {
        Self {
            role,
            implementation: implementation.to_owned(),
            problem: problem.to_owned(),
        }
    }

    /// Nothing serves `role`, because of `problem`.
    pub fn missing(role: BackendRole, problem: &str) -> Self {
        Self {
            role,
            implementation: String::new(),
            problem: problem.to_owned(),
        }
    }
}

/// The UDP port of mDNS, by which other devices find this one: a firewall
/// must let it in as well as the agent's port.
pub const MDNS_PORT: u16 = 5353;

/// Whether a firewall lets other devices reach the agent
/// ([`SessionStatus::firewall`]): on its UDP port, and on mDNS's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Firewall {
    /// Not known: no firewall this can read, or not on this platform.
    #[default]
    Unknown,
    /// The firewall (ufw) is off.
    Off,
    /// The firewall is on and lets both ports in.
    Allows,
    /// The firewall is on and keeps out what this says.
    Blocks(Blocked),
    /// The firewall is on, but its rules cannot be read without
    /// privileges, so whether it lets the ports in is not known.
    OnUnreadable,
}

/// The ports a firewall keeps out ([`Firewall::Blocks`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Blocked {
    /// The agent's port: other devices cannot connect.
    pub port: bool,
    /// mDNS ([`MDNS_PORT`]): other devices cannot find this one.
    pub mdns: bool,
}

impl Blocked {
    /// The UDP ports kept out, the agent's being `port`.
    pub fn ports(self, port: u16) -> Vec<u16> {
        let mut ports = Vec::new();
        if self.port {
            ports.push(port);
        }
        if self.mdns {
            ports.push(MDNS_PORT);
        }
        ports
    }
}

/// The real backends for the running OS.
pub struct Native {
    pub capture: Box<dyn InputCapture>,
    pub emulation: Box<dyn InputEmulation>,
    pub displays: Box<dyn Displays>,
    pub pictures: Box<dyn DesktopPictures>,
    pub status: Box<dyn SessionStatus>,
    /// `None` where the agent does not own the clipboard: on Windows the
    /// user-context broker does.
    pub clipboard: Option<Box<dyn Clipboard>>,
}

/// What the real backends are given by the agent.
#[derive(Clone, Default)]
pub struct NativeOptions {
    /// A folder of the agent's where a backend may keep what it must
    /// remember, as the Wayland portals' restore tokens; nothing is kept
    /// without one.
    pub state_dir: Option<PathBuf>,
    /// Checks the permissions afresh, in a process of its own, while one
    /// is missing: macOS answers a process's checks once and keeps the
    /// answer, so the agent would never see the user grant one. `None`
    /// when it cannot.
    pub fresh_permissions: Option<PermissionCheck>,
}

/// Reads the permissions afresh ([`NativeOptions::fresh_permissions`]).
pub type PermissionCheck = std::sync::Arc<dyn Fn() -> Option<Vec<PermissionState>> + Send + Sync>;

impl std::fmt::Debug for NativeOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeOptions")
            .field("state_dir", &self.state_dir)
            .field("fresh_permissions", &self.fresh_permissions.is_some())
            .finish()
    }
}

/// Creates the real backends for the running OS.
pub fn native() -> Result<Native, PlatformError> {
    native_with(&NativeOptions::default())
}

/// Creates the real backends for the running OS, with `options`.
pub fn native_with(options: &NativeOptions) -> Result<Native, PlatformError> {
    let _ = options;
    #[cfg(target_os = "macos")]
    return macos::native(options);
    #[cfg(windows)]
    return windows::native();
    #[cfg(target_os = "linux")]
    return linux::native(options);
    #[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
    Err(PlatformError::Unsupported(
        "native input backends are not implemented for this OS yet",
    ))
}

/// The name this computer goes by on the network, offered as the default
/// device name. On Unix this is the host name without a `.local` suffix.
pub fn computer_name() -> Option<String> {
    let name = os_computer_name()?;
    let name = name.strip_suffix(".local").unwrap_or(&name).trim();
    (!name.is_empty()).then(|| name.to_owned())
}

#[cfg(unix)]
fn os_computer_name() -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer is valid for its whole length, and gethostname
    // writes at most that many bytes.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if rc != 0 {
        return None;
    }
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8(buf[..len].to_vec()).ok()
}

#[cfg(windows)]
fn os_computer_name() -> Option<String> {
    std::env::var("COMPUTERNAME").ok()
}

#[cfg(not(any(unix, windows)))]
fn os_computer_name() -> Option<String> {
    None
}
