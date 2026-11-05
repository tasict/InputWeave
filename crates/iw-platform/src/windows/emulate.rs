use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ::windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSE_EVENT_FLAGS,
    MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN,
    MOUSEEVENTF_XUP, MOUSEINPUT, SendInput, VIRTUAL_KEY, VK_PAUSE,
};
use ::windows::Win32::UI::WindowsAndMessaging::{XBUTTON1, XBUTTON2};
use iw_core::HidUsage;
use iw_core::geometry::Point;
use iw_core::input::Button;

use super::desktop::Follower;
use super::displays::MonitorCache;
use super::monitors::Pixel;
use super::translate::{Injector, RelativeMover, Synth};
use crate::{InputEmulation, PlatformError};

/// How often injection checks that it is still on the input desktop. It
/// also checks whenever `SendInput` fails.
const FOLLOW_INTERVAL: Duration = Duration::from_millis(250);

/// Injects input with `SendInput` from a thread of its own that follows the
/// input desktop, so injection reaches UAC prompts and the lock screen when
/// the agent runs as SYSTEM. Each call waits for the injection thread, so
/// errors come back to the caller.
///
/// Moves are absolute over the virtual desktop, except relative mode's
/// (`move_by`), which go as a mouse's; keys go by scan code, so
/// the receiving machine's layout and input method interpret them.
/// Windows counts double clicks itself.
pub struct WinEmulation {
    monitors: Arc<MonitorCache>,
    injector: Injector,
    jobs: Option<Sender<Job>>,
    results: Receiver<Result<Option<Point>, PlatformError>>,
    thread: Option<JoinHandle<()>>,
}

/// What the injection thread is asked to do.
enum Job {
    Inject(Vec<Synth>),
    /// A relative movement by so many logical units, scaled on the monitor
    /// the cursor is on, which that thread sees on the input desktop.
    MoveBy {
        dx: f64,
        dy: f64,
    },
    /// Where the cursor is, in logical units.
    Position,
}

impl WinEmulation {
    pub(crate) fn new(monitors: Arc<MonitorCache>) -> Result<Self, PlatformError> {
        let (jobs, job_rx) = mpsc::channel();
        let (result_tx, results) = mpsc::channel();
        let thread_monitors = monitors.clone();
        let thread = std::thread::Builder::new()
            .name("inputweave-inject".to_owned())
            .spawn(move || run(job_rx, result_tx, &thread_monitors))
            .map_err(|e| PlatformError::Os(e.to_string()))?;
        Ok(Self {
            monitors,
            injector: Injector::default(),
            jobs: Some(jobs),
            results,
            thread: Some(thread),
        })
    }

    fn inject(&mut self, synths: Vec<Synth>) -> Result<(), PlatformError> {
        if synths.is_empty() {
            return Ok(());
        }
        self.ask(Job::Inject(synths)).map(|_| ())
    }

    fn ask(&mut self, job: Job) -> Result<Option<Point>, PlatformError> {
        let stopped = || PlatformError::Os("the injection thread stopped".to_owned());
        self.jobs
            .as_ref()
            .ok_or_else(stopped)?
            .send(job)
            .map_err(|_| stopped())?;
        self.results.recv().map_err(|_| stopped())?
    }
}

impl Drop for WinEmulation {
    fn drop(&mut self) {
        // Closing the channel ends the thread.
        self.jobs = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl InputEmulation for WinEmulation {
    fn move_to(&mut self, position: Point) -> Result<(), PlatformError> {
        let map = self.monitors.current()?;
        let synth = self
            .injector
            .move_to(&map, position)
            .ok_or_else(|| PlatformError::Os("no monitors to move the cursor on".to_owned()))?;
        self.inject(vec![synth])
    }

    fn button(&mut self, button: Button, pressed: bool) -> Result<(), PlatformError> {
        self.inject(vec![Synth::Button { button, pressed }])
    }

    fn key(&mut self, usage: HidUsage, pressed: bool) -> Result<(), PlatformError> {
        match self.injector.key(usage, pressed) {
            Ok(synth) => self.inject(synth.into_iter().collect()),
            Err(_) => Err(PlatformError::Unsupported(
                "key without a Windows scan code",
            )),
        }
    }

    fn scroll(&mut self, dx: f64, dy: f64) -> Result<(), PlatformError> {
        let synths = self.injector.scroll(dx, dy);
        self.inject(synths)
    }

    /// As many physical pixels as the logical units come to on the monitor
    /// the cursor is on.
    fn move_by(&mut self, dx: f64, dy: f64) -> Result<(), PlatformError> {
        self.ask(Job::MoveBy { dx, dy }).map(|_| ())
    }

    fn position(&mut self) -> Option<Point> {
        self.ask(Job::Position).ok().flatten()
    }
}

/// Where the cursor is, in physical pixels, as the input desktop has it.
fn cursor_pixel() -> Result<Pixel, PlatformError> {
    let mut point = ::windows::Win32::Foundation::POINT::default();
    // SAFETY: `point` is valid for writing.
    unsafe { ::windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut point) }
        .map_err(|e| PlatformError::Os(format!("cannot read where the cursor is: {e}")))?;
    Ok(Pixel::new(point.x, point.y))
}

/// Relative movements on the injection thread.
#[derive(Default)]
struct Relative {
    mover: RelativeMover,
    /// The last error telling which monitor the cursor is on, logged once.
    failure: Option<String>,
}

impl Relative {
    /// The scale of the monitor the cursor is on; 1 when that cannot be
    /// told, which is logged once.
    fn scale(&mut self, monitors: &MonitorCache) -> f64 {
        let scale = monitors
            .current()
            .and_then(|map| Ok(map.scale_at(cursor_pixel()?)));
        match scale {
            Ok(scale) => {
                self.failure = None;
                scale
            }
            Err(e) => {
                let e = e.to_string();
                if self.failure.as_deref() != Some(e.as_str()) {
                    tracing::warn!(
                        error = %e,
                        "cannot tell which monitor the cursor is on; relative movements go unscaled"
                    );
                    self.failure = Some(e);
                }
                1.0
            }
        }
    }
}

fn run(
    jobs: Receiver<Job>,
    results: Sender<Result<Option<Point>, PlatformError>>,
    monitors: &MonitorCache,
) {
    let mut follower = Follower::new();
    let mut checked: Option<Instant> = None;
    let mut relative = Relative::default();
    while let Ok(job) = jobs.recv() {
        if checked.is_none_or(|at| at.elapsed() >= FOLLOW_INTERVAL) {
            follow(&mut follower);
            checked = Some(Instant::now());
        }
        let result = match job {
            Job::Inject(synths) => send(&mut follower, &synths).map(|()| None),
            Job::MoveBy { dx, dy } => {
                let scale = relative.scale(monitors);
                match relative.mover.move_by(scale, dx, dy) {
                    Some(synth) => send(&mut follower, &[synth]).map(|()| None),
                    None => Ok(None),
                }
            }
            Job::Position => monitors
                .current()
                .and_then(|map| Ok(map.to_logical(cursor_pixel()?))),
        };
        if results.send(result).is_err() {
            break;
        }
    }
}

/// Injects `job`, once more after following the input desktop if that
/// failed.
fn send(follower: &mut Follower, job: &[Synth]) -> Result<(), PlatformError> {
    let result = execute(job);
    if result.is_err() && follow(follower) {
        return execute(job);
    }
    result
}

/// Moves this thread to the input desktop if that changed. Returns whether
/// it moved.
fn follow(follower: &mut Follower) -> bool {
    let Ok(Some(desktop)) = follower.changed() else {
        return false;
    };
    let name = desktop.name.clone();
    match follower.switch(desktop) {
        Ok(()) => {
            tracing::info!(desktop = %name, "injecting on the new input desktop");
            true
        }
        Err(e) => {
            tracing::warn!(error = %e, "cannot follow the input desktop");
            false
        }
    }
}

fn execute(job: &[Synth]) -> Result<(), PlatformError> {
    if job.contains(&Synth::SecureAttention) {
        return iw_winsvc::sas::request().map_err(|e| PlatformError::Os(e.to_string()));
    }
    let inputs: Vec<INPUT> = job.iter().map(input).collect();
    // SAFETY: the inputs are fully initialized and the size passed is the
    // size of one INPUT, as SendInput requires.
    let sent = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
    if sent as usize == inputs.len() {
        Ok(())
    } else {
        Err(PlatformError::Os(format!(
            "SendInput injected {sent} of {} events: {}",
            inputs.len(),
            ::windows::core::Error::from_thread()
        )))
    }
}

fn input(synth: &Synth) -> INPUT {
    match *synth {
        Synth::MoveTo { x, y } => mouse(
            x,
            y,
            0,
            MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
        ),
        Synth::MoveBy { dx, dy } => mouse(dx, dy, 0, MOUSEEVENTF_MOVE),
        Synth::Button { button, pressed } => {
            let (down, up, data) = match button {
                Button::Left => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, 0),
                Button::Right => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, 0),
                Button::Middle => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, 0),
                Button::Back => (MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, XBUTTON1),
                Button::Forward => (MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, XBUTTON2),
            };
            mouse(0, 0, u32::from(data), if pressed { down } else { up })
        }
        Synth::Wheel { horizontal, delta } => {
            let flags = if horizontal {
                MOUSEEVENTF_HWHEEL
            } else {
                MOUSEEVENTF_WHEEL
            };
            // `mouseData` is a DWORD that Windows reads as signed for
            // wheel events.
            mouse(0, 0, delta as u32, flags)
        }
        Synth::Key { scan, pressed } => {
            let mut flags = KEYEVENTF_SCANCODE;
            if scan.extended {
                flags |= KEYEVENTF_EXTENDEDKEY;
            }
            keyboard(VIRTUAL_KEY(0), u16::from(scan.code), flags, pressed)
        }
        // Pause's real scan sequence starts with 0xE1, which SendInput
        // cannot express; by virtual key it arrives as Pause. 0x45 is the
        // scan code apps see for it.
        Synth::Pause { pressed } => keyboard(VK_PAUSE, 0x45, KEYBD_EVENT_FLAGS(0), pressed),
        Synth::SecureAttention => unreachable!("handled before building inputs"),
    }
}

fn mouse(dx: i32, dy: i32, data: u32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn keyboard(vk: VIRTUAL_KEY, scan: u16, mut flags: KEYBD_EVENT_FLAGS, pressed: bool) -> INPUT {
    if !pressed {
        flags |= KEYEVENTF_KEYUP;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use ::windows::Win32::Foundation::POINT;
    use ::windows::Win32::UI::Input::{
        GetRawInputData, HRAWINPUT, MOUSE_MOVE_ABSOLUTE, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER,
        RID_INPUT, RIDEV_INPUTSINK, RIM_TYPEMOUSE, RegisterRawInputDevices,
    };
    use ::windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, GetCursorPos, HWND_MESSAGE, MSG, PM_REMOVE, PeekMessageW,
        WINDOW_EX_STYLE, WINDOW_STYLE, WM_INPUT,
    };
    use ::windows::core::w;

    use super::*;

    fn cursor() -> (i32, i32) {
        let mut point = POINT::default();
        // SAFETY: `point` is valid for writing.
        unsafe { GetCursorPos(&mut point) }.unwrap();
        (point.x, point.y)
    }

    /// Moves the real pointer: run it by hand in a signed-in session, not
    /// over ssh. Relative movements reach Raw Input, which games read, as
    /// the physical pixels the logical units come to, fractions carried
    /// over, and the cursor moves too, by however far the pointer speed
    /// takes it.
    #[test]
    #[ignore = "moves the signed-in user's pointer"]
    fn relative_movements_reach_raw_input_in_pixels() {
        super::super::make_per_monitor_aware().unwrap();
        // SAFETY: a message-only window of a system class, on this thread,
        // destroyed below.
        let window = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!(""),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                None,
                None,
            )
        }
        .unwrap();
        let mouse = RAWINPUTDEVICE {
            usUsagePage: 1,
            usUsage: 2,
            dwFlags: RIDEV_INPUTSINK,
            hwndTarget: window,
        };
        // SAFETY: one device, described in full.
        unsafe { RegisterRawInputDevices(&[mouse], size_of::<RAWINPUTDEVICE>() as u32) }.unwrap();
        // The relative movements Raw Input reported in the next half second,
        // added up, and how many there were.
        let raw = || {
            let mut moved = (0, 0, 0);
            let until = Instant::now() + Duration::from_millis(500);
            while Instant::now() < until {
                let mut msg = MSG::default();
                // SAFETY: `msg` is valid for writing.
                while unsafe { PeekMessageW(&mut msg, Some(window), WM_INPUT, WM_INPUT, PM_REMOVE) }
                    .as_bool()
                {
                    let mut input = RAWINPUT::default();
                    let mut size = size_of::<RAWINPUT>() as u32;
                    // SAFETY: `input` holds `size` bytes; the handle is the
                    // message's.
                    let read = unsafe {
                        GetRawInputData(
                            HRAWINPUT(msg.lParam.0 as _),
                            RID_INPUT,
                            Some(&mut input as *mut RAWINPUT as *mut _),
                            &mut size,
                            size_of::<RAWINPUTHEADER>() as u32,
                        )
                    };
                    if read == u32::MAX || input.header.dwType != RIM_TYPEMOUSE.0 {
                        continue;
                    }
                    // SAFETY: a mouse's input, as the header says.
                    let mouse = unsafe { input.data.mouse };
                    if mouse.usFlags.0 & MOUSE_MOVE_ABSOLUTE.0 == 0 {
                        moved = (moved.0 + mouse.lLastX, moved.1 + mouse.lLastY, moved.2 + 1);
                    }
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            moved
        };

        let monitors = Arc::new(MonitorCache::default());
        let mut emulation = WinEmulation::new(monitors.clone()).unwrap();
        let map = monitors.current().unwrap();
        let primary = map.displays().into_iter().find(|d| d.primary).unwrap();
        emulation.move_to(primary.bounds.center()).unwrap();
        raw();
        let start = cursor();
        let scale = map.scale_at(Pixel::new(start.0, start.1));
        emulation.move_by(40.0, -25.0).unwrap();
        // Halves add up to a whole unit.
        emulation.move_by(0.5, 0.5).unwrap();
        emulation.move_by(0.5, 0.5).unwrap();
        let (dx, dy, count) = raw();
        let end = cursor();
        println!(
            "scale {scale}: Raw Input ({dx}, {dy}) in {count} movements; cursor {start:?} to {end:?}"
        );
        let pixels = |units: f64| (units * scale).trunc() as i32;
        assert!((dx - pixels(41.0)).abs() <= 1, "{dx}");
        assert!((dy - pixels(-24.0)).abs() <= 1, "{dy}");
        assert!(end.0 > start.0 && end.1 < start.1, "{start:?} to {end:?}");
        // SAFETY: the window made above.
        unsafe { DestroyWindow(window) }.unwrap();
    }
}
