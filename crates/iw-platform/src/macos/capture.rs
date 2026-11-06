use std::ffi::c_void;
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

use iw_core::geometry::Point;
use iw_core::hotkey::HeldHotkeys;
use iw_core::input::InputEvent;
use iw_core::router::Hotkey;
use objc2_core_foundation::{CFMachPort, CFRetained, CFRunLoop, CGPoint, kCFRunLoopCommonModes};
use objc2_core_graphics::{
    CGAssociateMouseAndMouseCursorPosition, CGDisplayHideCursor, CGDisplayShowCursor, CGEvent,
    CGEventField, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventTapProxy,
    CGEventType, CGMainDisplayID, CGWarpMouseCursorPosition,
};

use super::hid::HidState;
use super::translate::is_own;
use super::translate::{HeldBack, RawEvent, Translator, Warp, kind};
use super::{INJECTED, Preferences, private};
use crate::{InputCapture, Permission, PlatformError};

/// Reads local input with an active event tap at the HID level, on a
/// thread of its own running a CFRunLoop.
///
/// While grabbed, the tap swallows input so local apps never see it, the
/// cursor is hidden and detached from the mouse, and only the deltas are
/// reported. Reserved hotkeys are swallowed at any time. Events this
/// process injected carry [`INJECTED`] and pass through unreported. So do
/// all events while the virtual keyboard and mouse inject (the lock
/// screen, `hid`): theirs carry no tag and look like hardware, and taken
/// for the user's they would hand control back to this Mac. The price is
/// that someone at this Mac's own keyboard and mouse meanwhile does not
/// take control back either. The first motion after a warp is measured
/// from where the warp put the cursor (see [`Warp`]).
pub struct MacCapture {
    shared: Arc<Shared>,
    tap: Option<TapThread>,
    cursor_hidden: bool,
}

struct Shared {
    grabbed: AtomicBool,
    hid: Arc<HidState>,
    reserved: Mutex<Vec<Hotkey>>,
    /// The last warp, until a motion event follows it.
    warp: Mutex<Option<Warp>>,
}

impl Shared {
    fn warp(&self) -> MutexGuard<'_, Option<Warp>> {
        self.warp.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

struct TapThread {
    run_loop: RunLoop,
    thread: JoinHandle<()>,
}

/// The tap thread's run loop, kept to stop it.
struct RunLoop(CFRetained<CFRunLoop>);

// SAFETY: the run loop is only used to call CFRunLoopStop, which Core
// Foundation allows from any thread.
unsafe impl Send for RunLoop {}

impl MacCapture {
    pub fn new() -> Self {
        Self::with_hid(Arc::default())
    }

    /// Capture that takes no input for the user's while the virtual device
    /// `hid` describes is injecting.
    pub(crate) fn with_hid(hid: Arc<HidState>) -> Self {
        // Without this, hiding the cursor only works while this process is
        // frontmost, which the agent never is.
        if !private::allow_background_cursor_control() {
            tracing::warn!(
                "cannot hide the cursor in the background; it stays visible while grabbed"
            );
        }
        Self {
            shared: Arc::new(Shared {
                grabbed: AtomicBool::new(false),
                hid,
                reserved: Mutex::new(Vec::new()),
                warp: Mutex::new(None),
            }),
            tap: None,
            cursor_hidden: false,
        }
    }

    fn release(&mut self) {
        self.shared.grabbed.store(false, Ordering::SeqCst);
        CGAssociateMouseAndMouseCursorPosition(true);
        if self.cursor_hidden {
            CGDisplayShowCursor(CGMainDisplayID());
            self.cursor_hidden = false;
        }
    }
}

impl Default for MacCapture {
    fn default() -> Self {
        Self::new()
    }
}

impl InputCapture for MacCapture {
    fn start(&mut self, events: Sender<InputEvent>) -> Result<(), PlatformError> {
        if self.tap.is_some() {
            return Ok(());
        }
        let (ready_tx, ready_rx) = mpsc::channel();
        let shared = self.shared.clone();
        let thread = std::thread::Builder::new()
            .name("inputweave-tap".to_owned())
            .spawn(move || run_tap(shared, events, ready_tx))
            .map_err(|e| PlatformError::Os(e.to_string()))?;
        match ready_rx.recv() {
            Ok(Ok(run_loop)) => {
                self.tap = Some(TapThread { run_loop, thread });
                Ok(())
            }
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => Err(PlatformError::Os("the event tap thread stopped".to_owned())),
        }
    }

    fn set_grabbed(&mut self, grabbed: bool) -> Result<(), PlatformError> {
        if !grabbed {
            self.release();
            return Ok(());
        }
        self.shared.grabbed.store(true, Ordering::SeqCst);
        CGAssociateMouseAndMouseCursorPosition(false);
        if !self.cursor_hidden {
            CGDisplayHideCursor(CGMainDisplayID());
            self.cursor_hidden = true;
        }
        Ok(())
    }

    fn warp(&mut self, position: Point) -> Result<(), PlatformError> {
        {
            let mut warp = self.shared.warp();
            *warp = Some(match *warp {
                Some(pending) => pending.then(position),
                None => Warp {
                    from: cursor_position(),
                    to: position,
                },
            });
        }
        CGWarpMouseCursorPosition(CGPoint::new(position.x, position.y));
        // Warping makes macOS ignore the mouse for a quarter second;
        // re-associating cancels that.
        let grabbed = self.shared.grabbed.load(Ordering::SeqCst);
        CGAssociateMouseAndMouseCursorPosition(!grabbed);
        Ok(())
    }

    fn reserve_hotkeys(&mut self, hotkeys: &[Hotkey]) -> HeldHotkeys {
        *self
            .shared
            .reserved
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = hotkeys.to_vec();
        hotkeys.iter().copied().collect()
    }
}

impl Drop for MacCapture {
    fn drop(&mut self) {
        self.release();
        if let Some(tap) = self.tap.take() {
            tap.run_loop.0.stop();
            let _ = tap.thread.join();
        }
    }
}

/// What the tap callback works with. Owned by the tap thread; the callback
/// only runs on that thread, inside its run loop.
struct TapContext {
    shared: Arc<Shared>,
    events: Sender<InputEvent>,
    translator: Translator,
    held_back: HeldBack,
    preferences: Preferences,
    tap: Option<CFRetained<CFMachPort>>,
}

fn run_tap(
    shared: Arc<Shared>,
    events: Sender<InputEvent>,
    ready: mpsc::Sender<Result<RunLoop, PlatformError>>,
) {
    let mut preferences = Preferences::read();
    let context = Box::into_raw(Box::new(TapContext {
        shared,
        events,
        translator: Translator::new(preferences.natural_scrolling()),
        held_back: HeldBack::default(),
        preferences,
        tap: None,
    }));
    let mask = [
        kind::LEFT_DOWN,
        kind::LEFT_UP,
        kind::RIGHT_DOWN,
        kind::RIGHT_UP,
        kind::MOVED,
        kind::LEFT_DRAGGED,
        kind::RIGHT_DRAGGED,
        kind::KEY_DOWN,
        kind::KEY_UP,
        kind::FLAGS_CHANGED,
        kind::SCROLL,
        kind::OTHER_DOWN,
        kind::OTHER_UP,
        kind::OTHER_DRAGGED,
    ]
    .iter()
    .fold(0u64, |mask, k| mask | (1 << k));
    // SAFETY: the callback matches the required signature and `context`
    // stays valid until after the tap is invalidated below.
    let tap = unsafe {
        CGEvent::tap_create(
            CGEventTapLocation::HIDEventTap,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::Default,
            mask,
            Some(tap_callback),
            context.cast(),
        )
    };
    let Some(tap) = tap else {
        // SAFETY: no tap holds the pointer.
        drop(unsafe { Box::from_raw(context) });
        let _ = ready.send(Err(PlatformError::PermissionDenied(
            Permission::Accessibility,
        )));
        return;
    };
    let Some(source) = CFMachPort::new_run_loop_source(None, Some(&tap), 0) else {
        tap.invalidate();
        // SAFETY: the tap was invalidated and will not call back.
        drop(unsafe { Box::from_raw(context) });
        let _ = ready.send(Err(PlatformError::Os(
            "cannot create a run loop source for the event tap".to_owned(),
        )));
        return;
    };
    // SAFETY: the callback has not run yet; it only runs inside the run
    // loop on this thread.
    unsafe { (*context).tap = Some(tap.clone()) };
    let run_loop = CFRunLoop::current().expect("every thread has a run loop");
    // SAFETY: reading an immutable framework constant.
    run_loop.add_source(Some(&source), unsafe { kCFRunLoopCommonModes });
    CGEvent::tap_enable(&tap, true);
    if ready.send(Ok(RunLoop(run_loop.clone()))).is_err() {
        tap.invalidate();
        // SAFETY: as above.
        drop(unsafe { Box::from_raw(context) });
        return;
    }
    CFRunLoop::run();
    CGEvent::tap_enable(&tap, false);
    tap.invalidate();
    // SAFETY: the run loop has stopped and the tap is invalidated, so the
    // callback cannot run again.
    drop(unsafe { Box::from_raw(context) });
}

unsafe extern "C-unwind" fn tap_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: NonNull<CGEvent>,
    user_info: *mut c_void,
) -> *mut CGEvent {
    // SAFETY: `user_info` is the `TapContext` created in `run_tap`, alive
    // while the tap is, and only touched on this thread.
    let ctx = unsafe { &mut *user_info.cast::<TapContext>() };
    let passthrough = event.as_ptr();
    if event_type == CGEventType::TapDisabledByTimeout
        || event_type == CGEventType::TapDisabledByUserInput
    {
        // macOS turns a slow tap off. Turn it back on, or a grab would end
        // silently with the cursor still hidden.
        if let Some(tap) = &ctx.tap {
            CGEvent::tap_enable(tap, true);
        }
        return passthrough;
    }
    // SAFETY: the event is valid for the duration of the callback.
    let event = unsafe { event.as_ref() };
    let tagged =
        CGEvent::integer_value_field(Some(event), CGEventField::EventSourceUserData) == INJECTED;
    if is_own(tagged, ctx.shared.hid.owns_input()) {
        // An injected move puts the cursor where it says, and later motion
        // is measured from there.
        if kind::is_motion(event_type.0) {
            *ctx.shared.warp() = None;
        }
        return passthrough;
    }
    if ctx.preferences.stale() {
        ctx.preferences = Preferences::read();
        ctx.translator.natural_scrolling = ctx.preferences.natural_scrolling();
    }
    let mut raw = read(event_type, event);
    if kind::is_motion(raw.kind) {
        let mut warp = ctx.shared.warp();
        if let Some(pending) = *warp
            && pending.discount(&mut raw)
        {
            *warp = None;
        }
    }
    let inputs = ctx.translator.translate(&raw);
    let held_back = matches!(inputs.as_slice(), [InputEvent::Key { .. }]) && {
        let reserved = ctx
            .shared
            .reserved
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        ctx.held_back.holds(
            &reserved,
            &inputs,
            ctx.translator.held_modifiers(),
            raw.autorepeat,
        )
    };
    for input in inputs {
        let _ = ctx.events.send(input);
    }
    if held_back || ctx.shared.grabbed.load(Ordering::Relaxed) {
        ptr::null_mut()
    } else {
        passthrough
    }
}

/// Where the cursor is now.
fn cursor_position() -> Point {
    let event = CGEvent::new(None);
    let p = CGEvent::location(event.as_deref());
    Point::new(p.x, p.y)
}

fn read(event_type: CGEventType, event: &CGEvent) -> RawEvent {
    let e = Some(event);
    let int = |field| CGEvent::integer_value_field(e, field);
    let double = |field| CGEvent::double_value_field(e, field);
    let location = CGEvent::location(e);
    RawEvent {
        kind: event_type.0,
        location: Point::new(location.x, location.y),
        delta: Point::new(
            double(CGEventField::MouseEventDeltaX),
            double(CGEventField::MouseEventDeltaY),
        ),
        button_number: int(CGEventField::MouseEventButtonNumber),
        keycode: int(CGEventField::KeyboardEventKeycode) as u16,
        autorepeat: int(CGEventField::KeyboardEventAutorepeat) != 0,
        flags: CGEvent::flags(e).bits(),
        scroll_continuous: int(CGEventField::ScrollWheelEventIsContinuous) != 0,
        scroll_lines: Point::new(
            double(CGEventField::ScrollWheelEventFixedPtDeltaAxis2),
            double(CGEventField::ScrollWheelEventFixedPtDeltaAxis1),
        ),
        scroll_pixels: Point::new(
            double(CGEventField::ScrollWheelEventPointDeltaAxis2),
            double(CGEventField::ScrollWheelEventPointDeltaAxis1),
        ),
    }
}

#[cfg(test)]
mod tests {
    use iw_core::input::Button;
    use iw_keymap::usage;
    use objc2_core_graphics::CGMouseButton;

    use super::*;

    /// Events constructed in memory need no permission, so the field
    /// reading can be checked against real Quartz events.
    #[test]
    fn reads_quartz_events() {
        let key = CGEvent::new_keyboard_event(None, 0x00, true).unwrap();
        let mut t = Translator::default();
        assert_eq!(
            t.translate(&read(CGEventType::KeyDown, &key)),
            vec![InputEvent::Key {
                usage: usage::A,
                pressed: true
            }]
        );

        let click = CGEvent::new_mouse_event(
            None,
            CGEventType::OtherMouseDown,
            CGPoint::new(12.0, 34.0),
            CGMouseButton(4),
        )
        .unwrap();
        assert_eq!(
            t.translate(&read(CGEventType::OtherMouseDown, &click)),
            vec![InputEvent::Button {
                button: Button::Forward,
                pressed: true
            }]
        );

        let moved = CGEvent::new_mouse_event(
            None,
            CGEventType::MouseMoved,
            CGPoint::new(100.0, 200.0),
            CGMouseButton::Left,
        )
        .unwrap();
        CGEvent::set_integer_value_field(Some(&moved), CGEventField::MouseEventDeltaX, 3);
        CGEvent::set_integer_value_field(Some(&moved), CGEventField::MouseEventDeltaY, -4);
        assert_eq!(
            t.translate(&read(CGEventType::MouseMoved, &moved)),
            vec![InputEvent::PointerMotion {
                position: Point::new(100.0, 200.0),
                delta: Point::new(3.0, -4.0),
            }]
        );
    }
}
