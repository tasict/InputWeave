//! Linux backends, chosen by the kind of graphical session the agent runs
//! in. The agent runs as the user, as a
//! service of the user's systemd, so it owns the clipboard and the files
//! as on macOS.
//!
//! X11 sessions have backends on XInput 2, XTest and RandR (`x11`).
//! Wayland sessions have theirs on the desktop's portals and libei
//! (`wayland`): X11 clients there (XWayland) see only their own windows.
//!
//! Beside them: the desktop's settings (`desktop`), its desktop pictures
//! (`pictures`), its natural scrolling (`natural_scroll`), ufw
//! (`firewall`), what the app is told of the session (`info`) and opening
//! the app (`app`).
//!
//! The pure parts (`session`, `bus`, `screen_lock`, `clipboard_formats`,
//! `selection`, `common`, `repeat`, `desktop`, `pictures`,
//! `natural_scroll`, `firewall`, `info`, `x11::translate`,
//! `wayland::translate`) are unit tested on every OS (`clipboard_formats`,
//! `selection`, `common` and `pictures` on every Unix: file paths are bytes
//! there, and pipes are polled).

#[cfg(any(target_os = "linux", all(test, unix)))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) mod clipboard_formats;
#[cfg(any(target_os = "linux", all(test, unix)))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) mod selection;
pub mod session;

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) mod app;
pub(crate) mod bus;
#[cfg(any(target_os = "linux", all(test, unix)))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) mod common;
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) mod desktop;
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) mod firewall;
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) mod info;
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) mod natural_scroll;
#[cfg(any(target_os = "linux", all(test, unix)))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) mod pictures;
pub(crate) mod repeat;
pub(crate) mod screen_lock;
pub mod wayland;
pub mod x11;

pub use session::{Session, SessionKind};

/// `mutex`, even if another thread panicked while holding it. Here, not in
/// `common`, for what is built on every OS.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// The backends for the session this process runs in.
#[cfg(target_os = "linux")]
pub(crate) fn native(
    options: &crate::NativeOptions,
) -> Result<crate::Native, crate::PlatformError> {
    use crate::PlatformError;

    let session = Session::current();
    tracing::info!(
        kind = ?session.kind,
        session_type = session.session_type.as_deref().unwrap_or(""),
        desktop = session.desktop.as_deref().unwrap_or(""),
        "graphical session"
    );
    let info = info::Info::of(&session);
    // The desktop's pictures, whichever kind of session shows them.
    let pictures = Box::new(pictures::LinuxPictures::new(info.desktop()));
    match session.kind {
        SessionKind::X11 => x11::native(&info, pictures),
        SessionKind::Wayland => wayland::native(options, &info, pictures),
        SessionKind::None => Err(PlatformError::Unavailable(
            "a graphical session: neither WAYLAND_DISPLAY nor DISPLAY is set",
        )),
    }
}
