use std::cell::RefCell;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;

use ::windows::Win32::Foundation::{LPARAM, LRESULT, POINT, WPARAM};
use ::windows::Win32::System::LibraryLoader::GetModuleHandleW;
use ::windows::Win32::System::Threading::GetCurrentThreadId;
use ::windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetCursorPos, GetMessageW, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT,
    KillTimer, LLKHF_EXTENDED, LLKHF_INJECTED, LLKHF_UP, LLMHF_INJECTED, MSG, MSLLHOOKSTRUCT,
    PM_NOREMOVE, PeekMessageW, PostThreadMessageW, SetCursorPos, SetTimer, SetWindowsHookExW,
    TranslateMessage, UnhookWindowsHookEx, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_APP, WM_TIMER, WM_USER,
};
use iw_core::geometry::Point;
use iw_core::input::InputEvent;

use super::desktop::Follower;
use super::displays::MonitorCache;
use super::monitors::Pixel;
use super::translate::{MouseTranslator, RawKey, RawMouse, decode_mouse, key_event};
use crate::{InputCapture, PlatformError};

/// Posted to the hook thread when commands are waiting.
const WM_COMMAND: u32 = WM_APP + 1;
/// Posted to the hook thread to make it exit.
const WM_STOP: u32 = WM_APP + 2;
/// How often the hook thread checks the input desktop and the monitors.
const TICK_MS: u32 = 250;

/// Reads local input with low-level mouse and keyboard hooks, on a thread
/// of its own running a message loop. That thread follows the input
/// desktop, re-installing the hooks there, and re-installs them if
/// installing failed.
///
/// While grabbed, the hooks swallow input so local apps never see it. The
/// cursor is held in the middle of its monitor and stays visible there:
/// Windows has no supported way for one process to hide the cursor over
/// other processes' windows, and replacing the system cursors with blank
/// ones would leave the user without a cursor if the agent died while
/// grabbed. Events flagged as injected (by this or any other process) pass
/// through unreported.
pub struct WinCapture {
    monitors: Arc<MonitorCache>,
    thread: Option<HookThread>,
}

struct HookThread {
    id: u32,
    commands: Sender<Command>,
    join: JoinHandle<()>,
}

enum Command {
    Grab(bool),
    Warp(Point),
}

impl WinCapture {
    pub(crate) fn new(monitors: Arc<MonitorCache>) -> Self {
        Self {
            monitors,
            thread: None,
        }
    }

    fn command(&self, command: Command) -> Result<(), PlatformError> {
        let thread = self
            .thread
            .as_ref()
            .ok_or_else(|| PlatformError::Os("input capture has not started".to_owned()))?;
        thread.commands.send(command).map_err(|_| stopped())?;
        // SAFETY: no pointers are passed; the id is the hook thread's,
        // which has a message queue until it exits.
        unsafe { PostThreadMessageW(thread.id, WM_COMMAND, WPARAM(0), LPARAM(0)) }
            .map_err(|_| stopped())
    }
}

fn stopped() -> PlatformError {
    PlatformError::Os("the input hook thread stopped".to_owned())
}

impl InputCapture for WinCapture {
    /// Succeeds once the hook thread runs, even if the hooks could not be
    /// installed yet: the thread keeps trying, since the agent does not
    /// call `start` again.
    fn start(&mut self, events: Sender<InputEvent>) -> Result<(), PlatformError> {
        if self.thread.is_some() {
            return Ok(());
        }
        let (ready_tx, ready_rx) = mpsc::channel();
        let (commands, command_rx) = mpsc::channel();
        let monitors = self.monitors.clone();
        let join = std::thread::Builder::new()
            .name("inputweave-hooks".to_owned())
            .spawn(move || run(monitors, events, command_rx, ready_tx))
            .map_err(|e| PlatformError::Os(e.to_string()))?;
        let id = ready_rx.recv().map_err(|_| stopped())?;
        self.thread = Some(HookThread { id, commands, join });
        Ok(())
    }

    fn set_grabbed(&mut self, grabbed: bool) -> Result<(), PlatformError> {
        if !grabbed && self.thread.is_none() {
            return Ok(());
        }
        self.command(Command::Grab(grabbed))
    }

    fn warp(&mut self, position: Point) -> Result<(), PlatformError> {
        self.command(Command::Warp(position))
    }
}

impl Drop for WinCapture {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            // SAFETY: as in `command`.
            let posted = unsafe { PostThreadMessageW(thread.id, WM_STOP, WPARAM(0), LPARAM(0)) };
            if posted.is_ok() {
                let _ = thread.join.join();
            }
        }
    }
}

/// What the hook procedures work with. Low-level hook procedures get no
/// context pointer; Windows calls them on the thread that installed them,
/// from inside its message loop, so the state lives in a thread local of
/// the hook thread.
struct HookState {
    events: Sender<InputEvent>,
    mouse: MouseTranslator,
}

thread_local! {
    static STATE: RefCell<Option<HookState>> = const { RefCell::new(None) };
}

/// Runs `f` on the hook thread's state. `None` if there is none, or if a
/// hook procedure is re-entered while it is borrowed.
fn with_state<R>(f: impl FnOnce(&mut HookState) -> R) -> Option<R> {
    STATE.with(|state| {
        let mut state = state.try_borrow_mut().ok()?;
        state.as_mut().map(f)
    })
}

/// Both hooks, removed on drop.
struct Hooks {
    mouse: HHOOK,
    keyboard: HHOOK,
}

impl Hooks {
    fn install() -> Result<Self, PlatformError> {
        let os =
            |e: ::windows::core::Error| PlatformError::Os(format!("cannot install a hook: {e}"));
        // SAFETY: the procedures live in this executable, whose module
        // handle is passed; low-level hooks run them on this thread.
        unsafe {
            let module = GetModuleHandleW(None).map_err(os)?;
            let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), Some(module.into()), 0)
                .map_err(os)?;
            match SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), Some(module.into()), 0) {
                Ok(keyboard) => Ok(Self { mouse, keyboard }),
                Err(e) => {
                    let _ = UnhookWindowsHookEx(mouse);
                    Err(os(e))
                }
            }
        }
    }
}

impl Drop for Hooks {
    fn drop(&mut self) {
        // SAFETY: both hooks were installed by this thread and are removed
        // once.
        unsafe {
            let _ = UnhookWindowsHookEx(self.mouse);
            let _ = UnhookWindowsHookEx(self.keyboard);
        }
    }
}

/// The hook thread's own bookkeeping, outside what the hooks read.
struct Thread {
    monitors: Arc<MonitorCache>,
    follower: Follower,
    hooks: Option<Hooks>,
    /// Whether the last failure was logged, so a lasting one is logged once.
    install_failed: bool,
    desktop_unreachable: bool,
}

fn run(
    monitors: Arc<MonitorCache>,
    events: Sender<InputEvent>,
    commands: Receiver<Command>,
    ready: Sender<u32>,
) {
    let mut msg = MSG::default();
    // SAFETY: `msg` is valid for the call. Peeking creates this thread's
    // message queue, so messages posted as soon as its id is known are kept.
    unsafe {
        let _ = PeekMessageW(&mut msg, None, WM_USER, WM_USER, PM_NOREMOVE);
    }
    STATE.with(|state| {
        *state.borrow_mut() = Some(HookState {
            events,
            mouse: MouseTranslator {
                map: monitors.current().unwrap_or_default(),
                ..MouseTranslator::default()
            },
        });
    });
    let mut thread = Thread {
        monitors,
        follower: Follower::new(),
        hooks: None,
        install_failed: false,
        desktop_unreachable: false,
    };
    thread.tick();
    // SAFETY: a timer without a window posts WM_TIMER to this thread.
    let timer = unsafe { SetTimer(None, 0, TICK_MS, None) };
    // SAFETY: takes no arguments.
    let id = unsafe { GetCurrentThreadId() };
    if ready.send(id).is_ok() {
        loop {
            // SAFETY: `msg` is valid for the call.
            let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
            // 0 is WM_QUIT, -1 an error.
            if got.0 <= 0 {
                break;
            }
            match msg.message {
                WM_STOP => break,
                WM_COMMAND => {
                    while let Ok(command) = commands.try_recv() {
                        thread.command(command);
                    }
                }
                WM_TIMER => thread.tick(),
                _ => {
                    // SAFETY: `msg` came from GetMessageW.
                    unsafe {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
            }
        }
    }
    // SAFETY: the timer was created by this thread.
    unsafe {
        let _ = KillTimer(None, timer);
    }
    thread.hooks = None;
    STATE.with(|state| *state.borrow_mut() = None);
}

impl Thread {
    /// Picks up monitor changes, follows the input desktop and installs the
    /// hooks if they are missing.
    fn tick(&mut self) {
        if let Ok(map) = self.monitors.current() {
            with_state(|s| s.mouse.map = map);
        }
        match self.follower.changed() {
            Ok(Some(desktop)) => {
                // A thread with hooks on its desktop cannot leave it.
                self.hooks = None;
                let name = desktop.name.clone();
                match self.follower.switch(desktop) {
                    Ok(()) => tracing::info!(desktop = %name, "capturing on the new input desktop"),
                    Err(e) => tracing::warn!(error = %e, "cannot follow the input desktop"),
                }
                self.desktop_unreachable = false;
            }
            Ok(None) => self.desktop_unreachable = false,
            Err(e) => {
                if !self.desktop_unreachable {
                    tracing::info!(
                        error = %e,
                        desktop = %self.follower.name(),
                        "staying on the current desktop"
                    );
                    self.desktop_unreachable = true;
                }
            }
        }
        if self.hooks.is_none() {
            match Hooks::install() {
                Ok(hooks) => {
                    self.hooks = Some(hooks);
                    self.install_failed = false;
                    tracing::info!(desktop = %self.follower.name(), "input hooks installed");
                }
                Err(e) => {
                    if !self.install_failed {
                        tracing::warn!(error = %e, "cannot capture input yet; retrying");
                        self.install_failed = true;
                    }
                }
            }
        }
    }

    fn command(&mut self, command: Command) {
        match command {
            Command::Grab(true) => {
                with_state(|s| s.mouse.grabbed = true);
                // Hold the cursor in the middle of its monitor: the motion
                // Windows reports for a held cursor is cut short at the
                // edges of the desktop, and it was just at one.
                let center = cursor_position().and_then(|cursor| {
                    with_state(|s| s.mouse.map.monitor_center(cursor)).flatten()
                });
                if let Some(center) = center {
                    move_cursor(center);
                }
            }
            Command::Grab(false) => {
                with_state(|s| s.mouse.grabbed = false);
            }
            Command::Warp(position) => {
                if let Some(target) = with_state(|s| s.mouse.map.to_physical(position)).flatten() {
                    move_cursor(target);
                }
            }
        }
    }
}

fn move_cursor(target: Pixel) {
    with_state(|s| s.mouse.warped_to = Some(target));
    // SAFETY: no pointers. The thread is on the input desktop, which
    // SetCursorPos requires.
    if let Err(e) = unsafe { SetCursorPos(target.x, target.y) } {
        tracing::warn!(error = %e, "cannot move the cursor");
    }
}

fn cursor_position() -> Option<Pixel> {
    let mut p = POINT::default();
    // SAFETY: `p` is valid for the call.
    unsafe { GetCursorPos(&mut p) }.ok()?;
    Some(Pixel::new(p.x, p.y))
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY: for HC_ACTION, `lparam` points to an MSLLHOOKSTRUCT valid
        // for the duration of the call.
        let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        if info.flags & LLMHF_INJECTED == 0 && on_mouse(wparam.0 as u32, info) {
            return LRESULT(1);
        }
    }
    // SAFETY: passes the hook data on unchanged.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Reports one physical mouse event. Returns whether to swallow it. Must
/// return quickly: Windows removes hooks that keep it waiting.
fn on_mouse(message: u32, info: &MSLLHOOKSTRUCT) -> bool {
    let to = Pixel::new(info.pt.x, info.pt.y);
    let raw = decode_mouse(message, info.mouseData, to);
    // The hook runs before Windows moves the cursor, so this is where the
    // motion starts.
    let cursor = match raw {
        Some(RawMouse::Move { .. }) => cursor_position().unwrap_or(to),
        _ => to,
    };
    with_state(|s| {
        if let Some(event) = raw.and_then(|raw| s.mouse.translate(raw, cursor)) {
            let _ = s.events.send(event);
        }
        s.mouse.grabbed
    })
    .unwrap_or(false)
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY: for HC_ACTION, `lparam` points to a KBDLLHOOKSTRUCT valid
        // for the duration of the call.
        let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        if !info.flags.contains(LLKHF_INJECTED) && on_key(info) {
            return LRESULT(1);
        }
    }
    // SAFETY: passes the hook data on unchanged.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Reports one physical key event. Returns whether to swallow it.
fn on_key(info: &KBDLLHOOKSTRUCT) -> bool {
    let raw = RawKey {
        vk: info.vkCode,
        scan: info.scanCode,
        extended: info.flags.contains(LLKHF_EXTENDED),
        up: info.flags.contains(LLKHF_UP),
    };
    with_state(|s| {
        if let Some(event) = key_event(raw) {
            let _ = s.events.send(event);
        }
        s.mouse.grabbed
    })
    .unwrap_or(false)
}
