//! Whether an app holds the pointer, as games do: the cursor hidden and
//! kept inside a window (`ClipCursor`). The device controlling this one may
//! then switch to relative mode, which such games need.
//!
//! The cursor and its clip are the input desktop's, so a thread of its own
//! that follows the input desktop asks (`PointerProbe`). On a desktop other
//! than the user's (the secure desktop of UAC and the lock screen,
//! Winlogon's) no game runs: it is never held there.

use super::monitors::PixelRect;

/// Whether the cursor, shown or not, and kept within `clip` on a virtual
/// screen of `screen` means an app holds it. A hidden cursor alone does
/// not (text boxes hide it while typing), nor does a clip that is the
/// whole screen, which is no clip at all.
pub(crate) fn held(showing: bool, clip: PixelRect, screen: PixelRect) -> bool {
    !showing && (clip.width() < screen.width() || clip.height() < screen.height())
}

/// Whether the input desktop named `name` is the user's, where apps run.
pub(crate) fn user_desktop(name: &str) -> bool {
    name.eq_ignore_ascii_case("Default")
}

/// A thread that follows the input desktop and tells whether an app holds
/// the pointer there. It ends with the probe.
#[cfg(windows)]
#[derive(Debug)]
pub(crate) struct PointerProbe {
    asks: std::sync::mpsc::Sender<std::sync::mpsc::Sender<Result<bool, crate::PlatformError>>>,
}

#[cfg(windows)]
impl PointerProbe {
    pub(crate) fn start() -> Result<Self, crate::PlatformError> {
        let (asks, questions) = std::sync::mpsc::channel::<
            std::sync::mpsc::Sender<Result<bool, crate::PlatformError>>,
        >();
        std::thread::Builder::new()
            .name("inputweave-pointer".to_owned())
            .spawn(move || {
                let mut follower = super::desktop::Follower::new();
                while let Ok(answer) = questions.recv() {
                    let _ = answer.send(held_on_input_desktop(&mut follower));
                }
            })
            .map_err(|e| crate::PlatformError::Os(e.to_string()))?;
        Ok(Self { asks })
    }

    /// Whether an app holds the pointer now, on the input desktop.
    pub(crate) fn held(&self) -> Result<bool, crate::PlatformError> {
        let stopped = || crate::PlatformError::Os("the pointer thread stopped".to_owned());
        let (answer, answered) = std::sync::mpsc::channel();
        self.asks.send(answer).map_err(|_| stopped())?;
        answered.recv().map_err(|_| stopped())?
    }
}

/// On the thread `follower` moves: onto the input desktop first.
#[cfg(windows)]
fn held_on_input_desktop(
    follower: &mut super::desktop::Follower,
) -> Result<bool, crate::PlatformError> {
    if let Some(desktop) = follower.changed()? {
        follower.switch(desktop)?;
    }
    if !user_desktop(follower.name()) {
        return Ok(false);
    }
    pointer_held()
}

/// Whether an app holds the pointer now, on this thread's desktop.
#[cfg(windows)]
pub(crate) fn pointer_held() -> Result<bool, crate::PlatformError> {
    use ::windows::Win32::Foundation::RECT;
    use ::windows::Win32::UI::WindowsAndMessaging::{
        CURSOR_SHOWING, CURSORINFO, GetClipCursor, GetCursorInfo, GetSystemMetrics,
        SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    };

    let os = |what: &str, e: ::windows::core::Error| {
        crate::PlatformError::Os(format!("cannot read the {what}: {e}"))
    };
    let mut info = CURSORINFO {
        cbSize: size_of::<CURSORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: `info` is valid for writing and its size is set.
    unsafe { GetCursorInfo(&mut info) }.map_err(|e| os("cursor", e))?;
    let mut clip = RECT::default();
    // SAFETY: `clip` is valid for writing.
    unsafe { GetClipCursor(&mut clip) }.map_err(|e| os("cursor's clip", e))?;
    // SAFETY: plain metrics queries.
    let (x, y, width, height) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    Ok(held(
        info.flags.0 & CURSOR_SHOWING.0 != 0,
        PixelRect::new(clip.left, clip.top, clip.right, clip.bottom),
        PixelRect::new(x, y, x + width, y + height),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hidden_cursor_kept_in_a_window_is_held() {
        let screen = PixelRect::new(-1920, 0, 1920, 1080);
        let window = PixelRect::new(100, 100, 1380, 820);
        assert!(held(false, window, screen));
        // Shown, or free to roam the screen, it is not.
        assert!(!held(true, window, screen));
        assert!(!held(false, screen, screen));
        // A clip to one of two monitors still holds it.
        assert!(held(false, PixelRect::new(0, 0, 1920, 1080), screen));
    }

    #[test]
    fn only_the_users_desktop_runs_games() {
        assert!(user_desktop("Default"));
        assert!(user_desktop("default"));
        assert!(!user_desktop("Winlogon"));
        assert!(!user_desktop("Screen-saver"));
        assert!(!user_desktop(""));
    }
}

#[cfg(all(test, windows))]
mod live {
    use std::time::{Duration, Instant};

    use ::windows::Win32::Foundation::{POINT, RECT};
    use ::windows::Win32::UI::WindowsAndMessaging::{
        ClipCursor, CreateWindowExW, DestroyWindow, DispatchMessageW, GetCursorPos, HWND_TOPMOST,
        MSG, PM_REMOVE, PeekMessageW, SWP_NOMOVE, SWP_NOSIZE, SetCursor, SetCursorPos,
        SetWindowPos, ShowCursor, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
    };
    use ::windows::core::w;

    use super::PointerProbe;

    /// Lets go of the cursor whatever happens.
    struct Released(POINT);

    impl Drop for Released {
        fn drop(&mut self) {
            // SAFETY: plain calls; no clip frees the cursor.
            unsafe {
                let _ = ClipCursor(None);
                while ShowCursor(true) < 0 {}
                let _ = SetCursorPos(self.0.x, self.0.y);
            }
        }
    }

    fn pump(for_: Duration) {
        let until = Instant::now() + for_;
        while Instant::now() < until {
            let mut msg = MSG::default();
            // SAFETY: `msg` is valid for writing; this thread's messages.
            while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
                // SAFETY: a message just taken off the queue.
                unsafe { DispatchMessageW(&msg) };
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Hides and holds the real cursor for a moment, as a game does: run
    /// it by hand in a signed-in session, not over ssh.
    #[test]
    #[ignore = "hides and holds the signed-in user's cursor"]
    fn a_window_hiding_and_clipping_the_cursor_holds_it() {
        crate::windows::make_per_monitor_aware().unwrap();
        // Asked as the agent asks, from the thread on the input desktop.
        let probe = PointerProbe::start().unwrap();
        let pointer_held = || probe.held();
        let mut was = POINT::default();
        // SAFETY: `was` is valid for writing.
        unsafe { GetCursorPos(&mut was) }.unwrap();
        assert!(
            !pointer_held().unwrap(),
            "something already holds the pointer"
        );
        // SAFETY: a small popup window of a system class, on this thread,
        // destroyed below.
        let window = unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                w!("STATIC"),
                w!("InputWeave pointer test"),
                WS_POPUP | WS_VISIBLE,
                200,
                200,
                320,
                240,
                None,
                None,
                None,
                None,
            )
        }
        .unwrap();
        let _released = Released(was);
        let clip = RECT {
            left: 200,
            top: 200,
            right: 520,
            bottom: 440,
        };
        // SAFETY: plain calls on the window made above.
        unsafe {
            SetWindowPos(
                window,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE,
            )
            .unwrap();
            SetCursorPos(360, 320).unwrap();
        }
        pump(Duration::from_millis(300));
        // Shown, or hidden but free to go anywhere, it is not held.
        let shown_and_clipped = {
            // SAFETY: `clip` is a valid rectangle.
            unsafe { ClipCursor(Some(&clip)) }.unwrap();
            pump(Duration::from_millis(200));
            pointer_held().unwrap()
        };
        // SAFETY: plain calls; the cursor is over this thread's window.
        unsafe {
            while ShowCursor(false) >= 0 {}
            SetCursor(None);
        }
        pump(Duration::from_millis(300));
        let hidden_and_clipped = pointer_held().unwrap();
        // SAFETY: no clip frees the cursor.
        unsafe { ClipCursor(None) }.unwrap();
        pump(Duration::from_millis(200));
        let hidden_only = pointer_held().unwrap();
        println!(
            "shown and clipped {shown_and_clipped}, hidden and clipped {hidden_and_clipped}, \
             hidden only {hidden_only}"
        );
        drop(_released);
        // SAFETY: the window made above.
        unsafe { DestroyWindow(window) }.unwrap();
        assert!(!shown_and_clipped && hidden_and_clipped && !hidden_only);
        assert!(!pointer_held().unwrap());
    }
}
