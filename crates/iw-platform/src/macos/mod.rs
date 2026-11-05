//! macOS backends on Quartz event services.
//!
//! Capture is an active event tap on a thread of its own; injection posts
//! events tagged with [`INJECTED`] so capture can tell them from physical
//! input. Both need the Accessibility permission, and the tap needs Input
//! Monitoring to see the keyboard. Private and Carbon calls live in
//! `private`.
//!
//! The lock screen and the login window drop posted events; there input
//! goes through a virtual keyboard and mouse (`hid`), when this build has
//! the entitlement Apple grants for it.
//!
//! AppKit is used off the main thread, from whichever thread the agent
//! calls in on: the pasteboard (`clipboard`), and the screens, workspace
//! and images behind desktop picture previews (`desktop_pictures`), which
//! need no permission. The main thread only runs its run loop
//! (`run_beside_main_loop`), through which AppKit asks for clipboard
//! content promised to other apps.

mod capture;
mod clipboard;
mod desktop_pictures;
mod displays;
mod emulate;
mod hid;
mod main_loop;
mod private;
mod translate;

use std::sync::Arc;
use std::time::{Duration, Instant};

use objc2_application_services::AXIsProcessTrusted;
use objc2_core_foundation::{
    CFBoolean, CFNumber, CFPreferencesCopyAppValue, CFPreferencesCopyValue, CFString, CFType,
    kCFPreferencesAnyApplication, kCFPreferencesAnyHost, kCFPreferencesAnyUser,
};
use objc2_core_graphics::{CGPreflightListenEventAccess, CGSessionCopyCurrentDictionary};

pub use capture::MacCapture;
pub use clipboard::MacClipboard;
pub use desktop_pictures::MacDesktopPictures;
pub use displays::MacDisplays;
pub use emulate::MacEmulation;
use hid::HidState;
pub use main_loop::run_beside_main_loop;

use crate::{Native, Permission, PermissionState, PlatformError, SessionStatus};
use translate::ClickPolicy;

/// Written into `kCGEventSourceUserData` of every event this process posts
/// ("IWeave" in ASCII).
pub(crate) const INJECTED: i64 = 0x4957_6561_7665;

/// Permissions, secure keyboard entry and the lock screen, read on demand.
/// No check prompts the user.
#[derive(Default)]
pub struct MacStatus {
    hid: Arc<HidState>,
    /// Reads the permissions in a new process while one is missing: macOS
    /// answers a process's checks once and keeps the answer.
    fresh: Option<crate::PermissionCheck>,
    /// Whether Accessibility was granted, as last read, fresh or not.
    accessibility: std::sync::atomic::AtomicBool,
}

impl std::fmt::Debug for MacStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MacStatus")
            .field("hid", &self.hid)
            .field("fresh", &self.fresh.is_some())
            .finish_non_exhaustive()
    }
}

impl MacStatus {
    /// Answering for the virtual device `hid` describes, and reading the
    /// permissions with `fresh` while one is missing.
    pub(crate) fn with_hid(hid: Arc<HidState>, fresh: Option<crate::PermissionCheck>) -> Self {
        Self {
            hid,
            fresh,
            accessibility: Default::default(),
        }
    }

    /// The permissions as this process sees them, which macOS fixes the
    /// first time it is asked.
    pub fn kept_permissions() -> Vec<PermissionState> {
        vec![
            PermissionState::new(
                Permission::Accessibility,
                // SAFETY: takes no arguments.
                unsafe { AXIsProcessTrusted() },
            ),
            PermissionState::new(Permission::InputMonitoring, CGPreflightListenEventAccess()),
        ]
    }
}

impl SessionStatus for MacStatus {
    /// As this process sees them, or, while one is missing, read afresh
    /// in a new process (`NativeOptions::fresh_permissions`). When that
    /// cannot run, what this process first saw stands until the agent
    /// starts over.
    fn permissions(&self) -> Vec<PermissionState> {
        let kept = Self::kept_permissions();
        let states = if kept.iter().all(|p| p.granted) {
            kept
        } else {
            self.fresh.as_ref().and_then(|read| read()).unwrap_or(kept)
        };
        let accessibility = states
            .iter()
            .any(|p| p.permission == Permission::Accessibility && p.granted);
        self.accessibility
            .store(accessibility, std::sync::atomic::Ordering::SeqCst);
        states
    }

    fn secure_input(&self) -> bool {
        private::secure_input_enabled()
    }

    /// The lock screen is up, or fast user switching has moved this session
    /// off the console, where input reaches no screen either. Also notes
    /// for capture whether the lock screen the virtual device may be
    /// driving is still up.
    fn screen_locked(&self) -> bool {
        let read_at = hid::now_ms();
        let (up, console) = (lock_screen_up(), on_console());
        self.hid.note_lock_screen(up && console, read_at);
        up || !console
    }

    /// Only through the virtual keyboard and mouse, the only input the lock
    /// screen takes (see [`HidState::lock_screen_input`]).
    fn lock_screen_input(&self) -> bool {
        self.hid.lock_screen_input()
    }

    /// Events posted without the Accessibility permission are dropped,
    /// but the virtual keyboard and mouse, the lock screen's only input,
    /// need none. Accessibility as last read afresh counts too (the agent
    /// starts over once it is granted).
    fn input_available(&self) -> bool {
        // SAFETY: takes no arguments.
        let trusted = unsafe { AXIsProcessTrusted() }
            || self.accessibility.load(std::sync::atomic::Ordering::SeqCst);
        trusted || (lock_screen_up() && self.hid.lock_screen_input())
    }
}

/// Whether this session's lock screen is up.
pub(crate) fn lock_screen_up() -> bool {
    session_flag("CGSSessionScreenIsLocked") == Some(true)
}

/// Whether this session is the one on the console: not moved aside by fast
/// user switching.
pub(crate) fn on_console() -> bool {
    session_flag("kCGSSessionOnConsoleKey") != Some(false)
}

/// A flag in the window server's dictionary for this login session, if it
/// has one. `CGSSessionScreenIsLocked` is not documented, but is the only
/// way to tell the lock screen is up short of asking loginwindow.
fn session_flag(key: &'static str) -> Option<bool> {
    let session = CGSessionCopyCurrentDictionary()?;
    // SAFETY: the session dictionary maps strings to property list values.
    let session = unsafe { session.cast_unchecked::<CFString, CFType>() };
    session
        .get(&CFString::from_static_str(key))
        .and_then(|v| v.downcast::<CFBoolean>().ok())
        .map(|b| b.as_bool())
}

/// The account that last logged in, as loginwindow keeps it for every
/// user (`/Library/Preferences/com.apple.loginwindow`, `lastUserName`).
pub fn last_login_user() -> Option<String> {
    let key = CFString::from_static_str("lastUserName");
    let app = CFString::from_static_str("com.apple.loginwindow");
    // SAFETY: reading immutable framework constants.
    let (user, host) = unsafe { (kCFPreferencesAnyUser, kCFPreferencesAnyHost) };
    let name = CFPreferencesCopyValue(&key, &app, user, host)?
        .downcast::<CFString>()
        .ok()?
        .to_string();
    (!name.is_empty()).then_some(name)
}

pub fn native(options: &crate::NativeOptions) -> Result<Native, PlatformError> {
    // Injection creates the virtual device; capture must know its events
    // are not the user's, and the status tells other devices whether it
    // can be used.
    let hid = Arc::new(HidState::default());
    Ok(Native {
        capture: Box::new(MacCapture::with_hid(hid.clone())),
        emulation: Box::new(MacEmulation::with_hid(hid.clone())?),
        displays: Box::new(MacDisplays),
        pictures: Box::new(MacDesktopPictures::default()),
        status: Box::new(MacStatus::with_hid(hid, options.fresh_permissions.clone())),
        clipboard: Some(Box::new(MacClipboard::general())),
    })
}

/// The user's input settings that change how events translate. Re-read
/// every couple of seconds, so changes apply without a restart.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Preferences {
    natural_scrolling: bool,
    double_click: Duration,
    read_at: Instant,
}

impl Preferences {
    const MAX_AGE: Duration = Duration::from_secs(2);

    pub(crate) fn read() -> Self {
        let natural_scrolling = global("com.apple.swipescrolldirection")
            .and_then(|v| v.downcast::<CFBoolean>().ok())
            .is_none_or(|b| b.as_bool());
        let double_click = global("com.apple.mouse.doubleClickThreshold")
            .and_then(|v| v.downcast::<CFNumber>().ok())
            .and_then(|n| n.as_f64())
            .filter(|s| s.is_finite() && *s > 0.0 && *s < 5.0)
            .map_or(Duration::from_millis(500), Duration::from_secs_f64);
        Self {
            natural_scrolling,
            double_click,
            read_at: Instant::now(),
        }
    }

    pub(crate) fn stale(&self) -> bool {
        self.read_at.elapsed() > Self::MAX_AGE
    }

    /// macOS turns natural scrolling on unless the user turned it off.
    pub(crate) fn natural_scrolling(&mut self) -> bool {
        self.natural_scrolling
    }

    pub(crate) fn clicks(&mut self) -> ClickPolicy {
        ClickPolicy {
            interval: self.double_click,
            ..ClickPolicy::default()
        }
    }
}

fn global(
    key: &'static str,
) -> Option<objc2_core_foundation::CFRetained<objc2_core_foundation::CFType>> {
    let key = CFString::from_static_str(key);
    // SAFETY: reading an immutable framework constant.
    CFPreferencesCopyAppValue(&key, unsafe { kCFPreferencesAnyApplication })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reading permissions and preferences must never prompt or fail.
    #[test]
    fn reads_status_and_preferences_without_prompting() {
        let status = MacStatus::default();
        let permissions = status.permissions();
        assert_eq!(permissions.len(), 2);
        let _ = status.secure_input();
        let _ = status.screen_locked();
        // Whoever logged in last, if anyone has.
        assert!(last_login_user().is_none_or(|name| !name.is_empty()));
        // A test binary is not signed with the virtual device's entitlement.
        assert!(!status.lock_screen_input());
        let mut p = Preferences::read();
        assert!(p.clicks().interval > Duration::ZERO);
        assert!(!p.stale());
    }
}
