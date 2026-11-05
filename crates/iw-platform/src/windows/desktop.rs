//! Following the input desktop.
//!
//! Windows shows UAC prompts and the lock and sign-in screens on the
//! `Winlogon` desktop, screen savers on `Screen-saver`, and everything
//! else on `Default`. Low-level hooks only see input on the desktop of the
//! thread that installed them, and `SendInput` only reaches the desktop of
//! the thread that calls it, so the capture and injection threads move
//! themselves to whichever desktop receives input. Only SYSTEM may open
//! `Winlogon`; an agent running as the user stays on `Default` while the
//! secure desktop is up.

use ::windows::Win32::Foundation::HANDLE;
use ::windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, DESKTOP_ACCESS_FLAGS, DESKTOP_CONTROL_FLAGS, DESKTOP_HOOKCONTROL,
    DESKTOP_JOURNALPLAYBACK, DESKTOP_READOBJECTS, DESKTOP_WRITEOBJECTS, GetThreadDesktop,
    GetUserObjectInformationW, HDESK, OpenInputDesktop, SetThreadDesktop, UOI_NAME,
};
use ::windows::Win32::System::Threading::GetCurrentThreadId;

use crate::PlatformError;

/// Rights a capture or injection thread needs on its desktop: reading its
/// name, installing hooks, and sending input.
const ACCESS: DESKTOP_ACCESS_FLAGS = DESKTOP_ACCESS_FLAGS(
    DESKTOP_READOBJECTS.0
        | DESKTOP_WRITEOBJECTS.0
        | DESKTOP_HOOKCONTROL.0
        | DESKTOP_JOURNALPLAYBACK.0,
);

/// A desktop handle this thread opened, closed on drop.
#[derive(Debug)]
pub(crate) struct Desktop {
    handle: HDESK,
    pub name: String,
}

impl Drop for Desktop {
    fn drop(&mut self) {
        // SAFETY: the handle came from OpenInputDesktop and is closed once.
        // Closing fails while a thread still uses it, which `Follower`
        // avoids by switching away first.
        let _ = unsafe { CloseDesktop(self.handle) };
    }
}

/// Keeps the calling thread on the input desktop. Owned by one thread.
#[derive(Debug)]
pub(crate) struct Follower {
    name: String,
    /// The desktop this thread switched to, if it switched at all; the
    /// desktop it started on belongs to the process and is not closed.
    current: Option<Desktop>,
}

impl Follower {
    /// Starts out on the calling thread's desktop.
    pub(crate) fn new() -> Self {
        // SAFETY: GetThreadDesktop does not transfer ownership; the handle
        // is only read here.
        let name = unsafe { GetThreadDesktop(GetCurrentThreadId()) }
            .ok()
            .and_then(name_of)
            .unwrap_or_default();
        Self {
            name,
            current: None,
        }
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// The input desktop, if it is not the one this thread is on. `Err`
    /// when it cannot be opened, as for a non-SYSTEM process while the
    /// secure desktop is up.
    pub(crate) fn changed(&self) -> Result<Option<Desktop>, PlatformError> {
        // SAFETY: no pointers; the returned handle is owned by `Desktop`.
        let handle = unsafe { OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, ACCESS) }
            .map_err(|e| PlatformError::Os(format!("cannot open the input desktop: {e}")))?;
        let name = name_of(handle).unwrap_or_default();
        let desktop = Desktop { handle, name };
        Ok((desktop.name != self.name).then_some(desktop))
    }

    /// Moves the calling thread to `desktop`. The thread must have no
    /// hooks or windows on its current desktop.
    pub(crate) fn switch(&mut self, desktop: Desktop) -> Result<(), PlatformError> {
        // SAFETY: the handle is open for as long as `desktop` lives, which
        // is until the thread switches away from it again.
        unsafe { SetThreadDesktop(desktop.handle) }.map_err(|e| {
            PlatformError::Os(format!("cannot switch to desktop {:?}: {e}", desktop.name))
        })?;
        self.name.clone_from(&desktop.name);
        // Replacing the previous desktop closes it, now that no thread of
        // ours uses it.
        self.current = Some(desktop);
        Ok(())
    }
}

fn name_of(desktop: HDESK) -> Option<String> {
    let mut buf = [0u16; 256];
    let mut needed = 0u32;
    // SAFETY: the buffer is valid for the size passed, in bytes.
    unsafe {
        GetUserObjectInformationW(
            HANDLE(desktop.0),
            UOI_NAME,
            Some(buf.as_mut_ptr().cast()),
            size_of_val(&buf) as u32,
            Some(&mut needed),
        )
    }
    .ok()?;
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}
