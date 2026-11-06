//! In-memory backends for tests and headless runs.

use std::collections::HashMap;
use std::sync::mpsc::{SendError, Sender};
use std::sync::{Arc, Mutex};

use iw_core::HidUsage;
use iw_core::geometry::Point;
use iw_core::hotkey::HeldHotkeys;
use iw_core::input::{Button, InputEvent};
use iw_core::layout::{Display, Exit};
use iw_core::router::Hotkey;

use crate::{
    Clipboard, ClipboardContent, ClipboardFormat, ClipboardPromise, ClipboardRead, DesktopPictures,
    Displays, Firewall, InputCapture, InputEmulation, Permission, PermissionAsk, PermissionState,
    PlatformError, SessionInfo, SessionStatus,
};

#[derive(Debug, Default)]
struct CaptureState {
    events: Option<Sender<InputEvent>>,
    grabbed: bool,
    warps: Vec<Point>,
    /// Starting fails as if Accessibility were missing.
    denied: bool,
    /// The hotkeys last reserved.
    reserved: Vec<Hotkey>,
    /// Every set of exits given, the latest last.
    exits: Vec<Vec<Exit>>,
    /// What the agent said after each input event it handled while input
    /// was not grabbed (`InputCapture::edge_wait`).
    edge_waits: Vec<(u64, bool)>,
    /// Input is taken only as the cursor crosses an edge, as on Wayland:
    /// `grab_ready` is false.
    edge_only: bool,
    /// Grabbing takes the keys and buttons down from the local apps, as on
    /// Wayland (`releases_carried_input_locally` is false).
    grab_releases_input: bool,
    /// Capture stopped by itself (`capturing` is false).
    stopped: bool,
    /// `warp` cannot move the cursor now (`can_warp` is false).
    no_warp: bool,
    /// `start` says capture is starting, not yet allowed.
    starting: bool,
    /// What `watch_double_press` was last told.
    double_press: bool,
}

/// Capture driven by the test: events passed to [`MockCaptureHandle::emit`]
/// are delivered as if a user produced them.
#[derive(Debug, Default)]
pub struct MockCapture {
    state: Arc<Mutex<CaptureState>>,
}

/// The test's side of a [`MockCapture`].
#[derive(Clone, Debug)]
pub struct MockCaptureHandle {
    state: Arc<Mutex<CaptureState>>,
}

impl MockCapture {
    pub fn new() -> (Self, MockCaptureHandle) {
        let capture = Self::default();
        let handle = MockCaptureHandle {
            state: capture.state.clone(),
        };
        (capture, handle)
    }
}

impl MockCaptureHandle {
    /// Delivers an event. Fails if capture was not started or the receiver
    /// is gone.
    pub fn emit(&self, event: InputEvent) -> Result<(), SendError<InputEvent>> {
        match &self.state.lock().unwrap().events {
            Some(tx) => tx.send(event),
            None => Err(SendError(event)),
        }
    }

    pub fn is_grabbed(&self) -> bool {
        self.state.lock().unwrap().grabbed
    }

    pub fn warps(&self) -> Vec<Point> {
        self.state.lock().unwrap().warps.clone()
    }

    /// Makes `start` fail with a missing permission until allowed again.
    pub fn deny(&self, denied: bool) {
        self.state.lock().unwrap().denied = denied;
    }

    pub fn is_started(&self) -> bool {
        self.state.lock().unwrap().events.is_some()
    }

    /// Every set of exits the agent gave (`InputCapture::set_exits`), the
    /// latest last.
    pub fn exits(&self) -> Vec<Vec<Exit>> {
        self.state.lock().unwrap().exits.clone()
    }

    /// Whether the cursor waits at an edge to cross, as the agent said after
    /// each input event it handled while input was not grabbed
    /// (`InputCapture::edge_wait`), with the count of events it handled.
    pub fn edge_waits(&self) -> Vec<(u64, bool)> {
        self.state.lock().unwrap().edge_waits.clone()
    }

    /// Takes input only as the cursor crosses an edge (`grab_ready` false),
    /// as Wayland's InputCapture portal does, or at will again.
    pub fn set_edge_only(&self, edge_only: bool) {
        self.state.lock().unwrap().edge_only = edge_only;
    }

    /// Grabbing takes the keys and buttons down from the local apps, as on
    /// Wayland, so none is let go of locally
    /// (`releases_carried_input_locally`).
    pub fn set_grab_releases_input(&self, releases: bool) {
        self.state.lock().unwrap().grab_releases_input = releases;
    }

    /// Capture stops by itself, as a Wayland portal session the user ended,
    /// and input no longer reaches the agent until it starts capture again
    /// (`capturing`); or capture may start again, `start` saying meanwhile
    /// that it is starting.
    pub fn set_stopped(&self, stopped: bool) {
        let mut state = self.state.lock().unwrap();
        state.stopped = stopped;
        if stopped {
            state.events = None;
        }
    }

    /// `warp` cannot move the cursor now, as on Wayland with nothing handed
    /// over (`can_warp`), or can again.
    pub fn set_no_warp(&self, no_warp: bool) {
        self.state.lock().unwrap().no_warp = no_warp;
    }

    /// `start` says capture is starting, as while the desktop starts what
    /// was allowed before (`PlatformError::Starting`), or not.
    pub fn set_starting(&self, starting: bool) {
        self.state.lock().unwrap().starting = starting;
    }

    /// The hotkeys the agent last reserved. This backend does not hold
    /// their keys back.
    pub fn reserved(&self) -> Vec<Hotkey> {
        self.state.lock().unwrap().reserved.clone()
    }

    /// Whether the agent last told capture that Control pressed twice
    /// takes control back (`InputCapture::watch_double_press`).
    pub fn watches_double_press(&self) -> bool {
        self.state.lock().unwrap().double_press
    }

    /// Says capture holds back `held` of the hotkeys from now on, as X11's
    /// does as that changes (`InputEvent::HotkeysHeldBack`), after the
    /// events emitted before.
    pub fn set_hotkeys_held_back(&self, held: HeldHotkeys) {
        let _ = self.emit(InputEvent::HotkeysHeldBack(held));
    }
}

impl InputCapture for MockCapture {
    fn start(&mut self, events: Sender<InputEvent>) -> Result<(), PlatformError> {
        let mut state = self.state.lock().unwrap();
        if state.denied {
            return Err(PlatformError::PermissionDenied(Permission::Accessibility));
        }
        if state.starting || state.stopped {
            return Err(PlatformError::Starting("capture"));
        }
        state.events = Some(events);
        Ok(())
    }

    fn set_grabbed(&mut self, grabbed: bool) -> Result<(), PlatformError> {
        self.state.lock().unwrap().grabbed = grabbed;
        Ok(())
    }

    fn warp(&mut self, position: Point) -> Result<(), PlatformError> {
        self.state.lock().unwrap().warps.push(position);
        Ok(())
    }

    fn reserve_hotkeys(&mut self, hotkeys: &[Hotkey]) -> HeldHotkeys {
        self.state.lock().unwrap().reserved = hotkeys.to_vec();
        HeldHotkeys::default()
    }

    fn watch_double_press(&mut self, on: bool) {
        self.state.lock().unwrap().double_press = on;
    }

    /// The agent's tests follow what it would tell a backend that does.
    fn wants_exits(&self) -> bool {
        true
    }

    fn set_exits(&mut self, exits: &[Exit]) {
        self.state.lock().unwrap().exits.push(exits.to_vec());
    }

    fn grab_ready(&self) -> bool {
        !self.state.lock().unwrap().edge_only
    }

    fn edges_only(&self) -> bool {
        self.state.lock().unwrap().edge_only
    }

    fn releases_carried_input_locally(&self) -> bool {
        !self.state.lock().unwrap().grab_releases_input
    }

    fn capturing(&self) -> bool {
        let state = self.state.lock().unwrap();
        !state.stopped && state.events.is_some()
    }

    fn can_warp(&self) -> bool {
        !self.state.lock().unwrap().no_warp
    }

    fn edge_wait(&mut self, handled: u64, waiting: bool) {
        self.state
            .lock()
            .unwrap()
            .edge_waits
            .push((handled, waiting));
    }
}

/// One call made on a [`RecordingEmulation`].
#[derive(Clone, Debug, PartialEq)]
pub enum Emulated {
    MoveTo(Point),
    Button(Button, bool),
    Key(HidUsage, bool),
    Scroll(f64, f64),
    MoveBy(f64, f64),
}

/// Emulation that records every call instead of injecting it.
#[derive(Clone, Debug, Default)]
pub struct RecordingEmulation {
    calls: Arc<Mutex<Vec<Emulated>>>,
}

impl RecordingEmulation {
    /// Everything injected so far, in order.
    pub fn calls(&self) -> Vec<Emulated> {
        self.calls.lock().unwrap().clone()
    }

    fn record(&self, call: Emulated) -> Result<(), PlatformError> {
        self.calls.lock().unwrap().push(call);
        Ok(())
    }
}

impl InputEmulation for RecordingEmulation {
    fn move_to(&mut self, position: Point) -> Result<(), PlatformError> {
        self.record(Emulated::MoveTo(position))
    }

    fn button(&mut self, button: Button, pressed: bool) -> Result<(), PlatformError> {
        self.record(Emulated::Button(button, pressed))
    }

    fn key(&mut self, usage: HidUsage, pressed: bool) -> Result<(), PlatformError> {
        self.record(Emulated::Key(usage, pressed))
    }

    fn scroll(&mut self, dx: f64, dy: f64) -> Result<(), PlatformError> {
        self.record(Emulated::Scroll(dx, dy))
    }

    fn move_by(&mut self, dx: f64, dy: f64) -> Result<(), PlatformError> {
        self.record(Emulated::MoveBy(dx, dy))
    }

    /// Where the moves recorded so far put the pointer: the last absolute
    /// one, and every relative one since.
    fn position(&mut self) -> Option<Point> {
        self.calls().iter().fold(None, |at, call| match *call {
            Emulated::MoveTo(p) => Some(p),
            Emulated::MoveBy(dx, dy) => at.map(|p| p + Point::new(dx, dy)),
            _ => at,
        })
    }
}

/// A fixed set of displays.
#[derive(Clone, Debug, Default)]
pub struct StaticDisplays(pub Vec<Display>);

impl Displays for StaticDisplays {
    fn displays(&self) -> Result<Vec<Display>, PlatformError> {
        Ok(self.0.clone())
    }
}

/// Displays a test can change while the agent runs, as when a monitor is
/// plugged in.
#[derive(Clone, Debug, Default)]
pub struct SharedDisplays(Arc<Mutex<Vec<Display>>>);

impl SharedDisplays {
    pub fn new(displays: Vec<Display>) -> Self {
        Self(Arc::new(Mutex::new(displays)))
    }

    pub fn set(&self, displays: Vec<Display>) {
        *self.0.lock().unwrap() = displays;
    }
}

impl Displays for SharedDisplays {
    fn displays(&self) -> Result<Vec<Display>, PlatformError> {
        Ok(self.0.lock().unwrap().clone())
    }
}

/// Desktop pictures a test sets, by display id. None until it sets some.
/// Clones share them, so a test can change them while the agent runs.
#[derive(Clone, Debug, Default)]
pub struct MockDesktopPictures(Arc<Mutex<HashMap<String, Vec<u8>>>>);

impl MockDesktopPictures {
    /// Shows `jpeg` on the display `display_id`. The bytes are passed on as
    /// they are, without the size limits real backends keep to.
    pub fn set(&self, display_id: &str, jpeg: Vec<u8>) {
        self.0.lock().unwrap().insert(display_id.to_owned(), jpeg);
    }

    /// As if the display showed a solid color.
    pub fn clear(&self, display_id: &str) {
        self.0.lock().unwrap().remove(display_id);
    }
}

impl DesktopPictures for MockDesktopPictures {
    fn pictures(&self, displays: &[Display]) -> Result<HashMap<String, Vec<u8>>, PlatformError> {
        let pictures = self.0.lock().unwrap();
        Ok(displays
            .iter()
            .filter_map(|d| Some((d.id.clone(), pictures.get(&d.id)?.clone())))
            .collect())
    }
}

#[derive(Debug, Default)]
struct ClipboardState {
    count: u64,
    content: ClipboardContent,
    /// Files copied, as in a file manager, beside `content`.
    files: Vec<std::path::PathBuf>,
    sensitive: bool,
    /// What the clipboard promises, until an app pastes it.
    promise: Option<ClipboardPromise>,
    /// Promises are refused, as by a backend that cannot make them.
    no_promises: bool,
    max_write: Option<usize>,
    /// How long each read takes, as reading a large clipboard does.
    read_delay: std::time::Duration,
}

impl ClipboardState {
    /// Replaces what the clipboard holds, telling the source of a promise
    /// it held, as macOS does.
    fn replace(&mut self, content: ClipboardContent, sensitive: bool) {
        self.count += 1;
        self.content = content;
        self.files.clear();
        self.sensitive = sensitive;
        if let Some(promise) = self.promise.take() {
            promise.source.finished();
        }
    }
}

/// An in-memory clipboard. Clones share it, so a test can copy and paste
/// while the agent holds another clone.
#[derive(Clone, Debug, Default)]
pub struct MockClipboard {
    state: Arc<Mutex<ClipboardState>>,
}

impl MockClipboard {
    /// As if the user copied `content`.
    pub fn copy(&self, content: ClipboardContent, sensitive: bool) {
        self.state.lock().unwrap().replace(content, sensitive);
    }

    /// As if the user copied the files and folders at `paths` in a file
    /// manager, which also puts `content` (their names, say) beside them.
    pub fn copy_files(&self, paths: Vec<std::path::PathBuf>, content: ClipboardContent) {
        let mut state = self.state.lock().unwrap();
        state.replace(content, false);
        state.files = paths;
    }

    /// The files and folders promised and not yet pasted.
    pub fn promised_files(&self) -> Option<Vec<crate::PromisedFile>> {
        let state = self.state.lock().unwrap();
        state.promise.as_ref().map(|p| p.files.clone())
    }

    /// As if a file manager pasted the files promised: they are fetched
    /// from the promise's source, which blocks, so call it off the async
    /// runtime. `None` without a promise of files.
    pub fn paste_files(&self) -> Option<crate::FilesFetch> {
        let promise = self.state.lock().unwrap().promise.clone()?;
        (!promise.files.is_empty()).then(|| promise.source.fetch_files())
    }

    /// What the clipboard holds; empty while a promise is not yet pasted.
    pub fn content(&self) -> ClipboardContent {
        self.state.lock().unwrap().content.clone()
    }

    /// The formats promised and not yet pasted.
    pub fn promised(&self) -> Option<Vec<ClipboardFormat>> {
        let state = self.state.lock().unwrap();
        state.promise.as_ref().map(|p| p.formats.clone())
    }

    /// As if an app pasted: a promise is fetched from its source, which
    /// blocks, so call it off the async runtime. Returns what was pasted.
    pub fn paste(&self) -> ClipboardContent {
        let promise = self.state.lock().unwrap().promise.clone();
        let Some(promise) = promise else {
            return self.content();
        };
        let fetched = ClipboardContent {
            items: promise
                .formats
                .iter()
                .filter_map(|f| Some((*f, promise.source.fetch(*f)?)))
                .collect(),
        };
        let mut state = self.state.lock().unwrap();
        // Fulfilling a promise is not a change of the clipboard.
        if state
            .promise
            .as_ref()
            .is_some_and(|p| Arc::ptr_eq(&p.source, &promise.source))
        {
            state.content = fetched.clone();
        }
        fetched
    }

    /// Refuse promises from now on, as a backend that cannot make them.
    pub fn refuse_promises(&self) {
        self.state.lock().unwrap().no_promises = true;
    }

    /// Take at most `max` bytes in one write, as the Windows broker does.
    pub fn limit_writes(&self, max: usize) {
        self.state.lock().unwrap().max_write = Some(max);
    }

    /// Make each read take `delay`.
    pub fn delay_reads(&self, delay: std::time::Duration) {
        self.state.lock().unwrap().read_delay = delay;
    }
}

impl Clipboard for MockClipboard {
    fn change_count(&self) -> Result<u64, PlatformError> {
        Ok(self.state.lock().unwrap().count)
    }

    fn read(&self, max_len: usize) -> Result<ClipboardRead, PlatformError> {
        let delay = self.state.lock().unwrap().read_delay;
        std::thread::sleep(delay);
        let state = self.state.lock().unwrap();
        Ok(if state.sensitive {
            ClipboardRead::Sensitive
        } else if state.content.is_empty() {
            ClipboardRead::Empty
        } else if state.content.len() > max_len {
            ClipboardRead::TooLarge(state.content.len())
        } else {
            ClipboardRead::Content(state.content.clone())
        })
    }

    fn write(&mut self, content: &ClipboardContent) -> Result<u64, PlatformError> {
        let mut state = self.state.lock().unwrap();
        state.replace(content.clone(), false);
        Ok(state.count)
    }

    fn files(&self) -> Result<Vec<std::path::PathBuf>, PlatformError> {
        let state = self.state.lock().unwrap();
        Ok(if state.sensitive {
            Vec::new()
        } else {
            state.files.clone()
        })
    }

    fn promises_files(&self) -> bool {
        !self.state.lock().unwrap().no_promises
    }

    fn max_write(&self) -> Option<usize> {
        self.state.lock().unwrap().max_write
    }

    fn write_promise(&mut self, promise: ClipboardPromise) -> Result<u64, PlatformError> {
        let mut state = self.state.lock().unwrap();
        if state.no_promises {
            return Err(PlatformError::Unsupported("promising clipboard content"));
        }
        // Files are promised marked so clipboard managers leave them be,
        // as the macOS backend does: read again, they are not to be shared.
        state.replace(ClipboardContent::default(), !promise.files.is_empty());
        state.promise = Some(promise);
        Ok(state.count)
    }
}

/// Session status a test controls: which permissions are granted, whether
/// secure input is on, whether the screen is locked and whether input
/// reaches it then.
#[derive(Clone, Debug, Default)]
pub struct MockStatus {
    state: Arc<Mutex<MockSession>>,
}

#[derive(Debug, Default)]
struct MockSession {
    permissions: Vec<PermissionState>,
    secure_input: bool,
    screen_locked: bool,
    lock_screen_input: bool,
    pointer_captured: bool,
    /// What the agent asked for (`request_permission`), in order.
    requests: Vec<Permission>,
    /// Asking for these fails at once, as without a portal.
    failing: Vec<Permission>,
    /// Input from other devices cannot reach this session.
    input_unavailable: bool,
    session_info: Option<SessionInfo>,
    firewall: Firewall,
    /// The ports the agent asked about the firewall for.
    firewall_ports: Vec<u16>,
}

impl MockStatus {
    /// Every permission granted, secure input off, the screen unlocked and
    /// no input reaching it once locked.
    pub fn granted() -> Self {
        let status = Self::default();
        status.set_permission(Permission::Accessibility, true);
        status
    }

    pub fn set_permission(&self, permission: Permission, granted: bool) {
        let mut state = self.state.lock().unwrap();
        match state
            .permissions
            .iter_mut()
            .find(|p| p.permission == permission)
        {
            Some(p) => p.granted = granted,
            None => state
                .permissions
                .push(PermissionState::new(permission, granted)),
        }
    }

    /// Lets the agent ask for `permission` itself, as Wayland's portals
    /// let it: not granted, and not asked for yet.
    pub fn set_askable(&self, permission: Permission) {
        let mut state = self.state.lock().unwrap();
        state.permissions.retain(|p| p.permission != permission);
        state.permissions.push(PermissionState {
            permission,
            granted: false,
            ask: PermissionAsk::Askable,
            asked: 0,
            failure: None,
        });
    }

    /// Asking for `permission` fails at once from now on.
    pub fn fail_requests(&self, permission: Permission) {
        self.state.lock().unwrap().failing.push(permission);
    }

    /// Whether input from other devices reaches this session.
    pub fn set_input_available(&self, available: bool) {
        self.state.lock().unwrap().input_unavailable = !available;
    }

    /// The user's answer to the agent asking for `permission`.
    pub fn answer(&self, permission: Permission, granted: bool) {
        let mut state = self.state.lock().unwrap();
        if let Some(p) = state
            .permissions
            .iter_mut()
            .find(|p| p.permission == permission)
        {
            p.granted = granted;
            p.ask = if granted {
                PermissionAsk::Askable
            } else {
                PermissionAsk::Denied
            };
        }
    }

    /// What the agent asked for, in order.
    pub fn requests(&self) -> Vec<Permission> {
        self.state.lock().unwrap().requests.clone()
    }

    pub fn set_secure_input(&self, on: bool) {
        self.state.lock().unwrap().secure_input = on;
    }

    pub fn set_screen_locked(&self, locked: bool) {
        self.state.lock().unwrap().screen_locked = locked;
    }

    pub fn set_lock_screen_input(&self, takes_input: bool) {
        self.state.lock().unwrap().lock_screen_input = takes_input;
    }

    pub fn set_pointer_captured(&self, captured: bool) {
        self.state.lock().unwrap().pointer_captured = captured;
    }

    /// What the session says it is and what serves it.
    pub fn set_session_info(&self, info: Option<SessionInfo>) {
        self.state.lock().unwrap().session_info = info;
    }

    /// What the firewall says.
    pub fn set_firewall(&self, firewall: Firewall) {
        self.state.lock().unwrap().firewall = firewall;
    }

    /// The last port the agent asked about the firewall for.
    pub fn firewall_port(&self) -> Option<u16> {
        self.state.lock().unwrap().firewall_ports.last().copied()
    }
}

impl SessionStatus for MockStatus {
    fn permissions(&self) -> Vec<PermissionState> {
        self.state.lock().unwrap().permissions.clone()
    }

    fn secure_input(&self) -> bool {
        self.state.lock().unwrap().secure_input
    }

    fn screen_locked(&self) -> bool {
        self.state.lock().unwrap().screen_locked
    }

    fn lock_screen_input(&self) -> bool {
        self.state.lock().unwrap().lock_screen_input
    }

    fn pointer_captured(&self) -> bool {
        self.state.lock().unwrap().pointer_captured
    }

    fn request_permission(&self, permission: Permission) -> Result<u64, PlatformError> {
        let mut state = self.state.lock().unwrap();
        let fails = state.failing.contains(&permission);
        let Some(p) = state
            .permissions
            .iter_mut()
            .find(|p| p.permission == permission && p.ask != PermissionAsk::NotAskable)
        else {
            return Err(PlatformError::Unsupported("asking for this permission"));
        };
        p.asked += 1;
        let asked = p.asked;
        if fails {
            p.ask = PermissionAsk::Failed;
            p.failure = Some("the mock fails it".to_owned());
        } else if !p.granted {
            p.ask = PermissionAsk::Asking;
            p.failure = None;
        }
        state.requests.push(permission);
        Ok(asked)
    }

    fn input_available(&self) -> bool {
        !self.state.lock().unwrap().input_unavailable
    }

    fn session_info(&self) -> Option<SessionInfo> {
        self.state.lock().unwrap().session_info.clone()
    }

    fn checks_firewall(&self) -> bool {
        true
    }

    fn firewall(&self, port: u16) -> Firewall {
        let mut state = self.state.lock().unwrap();
        if state.firewall_ports.last() != Some(&port) {
            state.firewall_ports.push(port);
        }
        state.firewall
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    #[test]
    fn capture_delivers_emitted_events_after_start() {
        let (mut capture, handle) = MockCapture::new();
        let event = InputEvent::Scroll { dx: 0.0, dy: -3.0 };
        assert!(handle.emit(event).is_err());

        let (tx, rx) = mpsc::channel();
        capture.start(tx).unwrap();
        handle.emit(event).unwrap();
        assert_eq!(rx.recv().unwrap(), event);

        capture.set_grabbed(true).unwrap();
        capture.warp(Point::new(10.0, 20.0)).unwrap();
        assert!(handle.is_grabbed());
        assert_eq!(handle.warps(), vec![Point::new(10.0, 20.0)]);
    }

    #[test]
    fn pictures_are_reported_for_the_displays_asked_about() {
        let display = |id: &str| Display {
            id: id.to_owned(),
            bounds: iw_core::geometry::Rect::new(0.0, 0.0, 100.0, 100.0),
            scale: 1.0,
            primary: false,
        };
        let pictures = MockDesktopPictures::default();
        let shared = pictures.clone();
        assert!(pictures.pictures(&[display("a")]).unwrap().is_empty());
        shared.set("a", vec![0xff, 0xd8, 0xff]);
        shared.set("gone", vec![1]);
        let read = pictures.pictures(&[display("a"), display("b")]).unwrap();
        assert_eq!(
            read,
            HashMap::from([("a".to_owned(), vec![0xff, 0xd8, 0xff])])
        );
        shared.clear("a");
        assert!(pictures.pictures(&[display("a")]).unwrap().is_empty());
    }

    #[test]
    fn emulation_records_calls_across_clones() {
        let emulation = RecordingEmulation::default();
        let mut injector = emulation.clone();
        injector.move_to(Point::new(1.0, 2.0)).unwrap();
        injector.button(Button::Left, true).unwrap();
        assert_eq!(
            emulation.calls(),
            vec![
                Emulated::MoveTo(Point::new(1.0, 2.0)),
                Emulated::Button(Button::Left, true),
            ]
        );
    }
}
