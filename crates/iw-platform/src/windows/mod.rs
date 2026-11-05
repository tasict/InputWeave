//! Windows backends on low-level hooks and `SendInput`.
//!
//! The agent runs as SYSTEM in the user's console session (see
//! `docs/architecture.md` §2) and must keep working when the input
//! desktop switches to the secure desktop for UAC or the lock screen.
//! Hooks and injection only reach the desktop of the thread that installs
//! or sends them, so capture and injection each run on a thread of their
//! own that follows the input desktop (`desktop`).
//!
//! The process is made per-monitor DPI aware (v2) before anything reads
//! coordinates, so Windows reports physical pixels; `monitors` maps them to
//! the logical units of the layout.
//!
//! The desktop pictures belong to the user, so the agent does not read
//! them: the broker, running as the user, reads them with
//! `WallpaperPreviews` (`desktop_pictures`, matched to displays in
//! `wallpaper`) and sends them to the agent. The clipboard is the user's
//! too: the broker reads and writes it with `WinClipboard` (`clipboard`,
//! its formats laid out in `clipboard_formats`).
//!
//! This code compiles for `x86_64-pc-windows-msvc` and
//! `aarch64-pc-windows-msvc`. On x64 Windows 11 the agent, as SYSTEM in
//! the console session, has been seen to install its hooks on the
//! `Default` desktop and read the monitors; capturing and injecting real
//! input and following the secure desktop have not been tried there yet.
//! The pure parts (`monitors`, `translate`, `wallpaper`,
//! `clipboard_formats`, `promise`, `session`) are unit tested on every OS.

// Used off Windows only for `clipboard_representations` and tests.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) mod clipboard_formats;
#[cfg(any(windows, test))]
pub(crate) mod monitors;
#[cfg(any(windows, test))]
pub(crate) mod pointer;
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) mod promise;
#[cfg(any(windows, test))]
pub(crate) mod session;
#[cfg(any(windows, test))]
pub(crate) mod translate;
#[cfg(any(windows, test))]
pub(crate) mod wallpaper;

#[cfg(windows)]
mod capture;
#[cfg(windows)]
mod clipboard;
#[cfg(windows)]
mod desktop;
#[cfg(windows)]
mod desktop_pictures;
#[cfg(windows)]
mod displays;
#[cfg(windows)]
mod emulate;

#[cfg(windows)]
pub use self::{
    capture::WinCapture,
    clipboard::{OnChange, WinClipboard},
    desktop_pictures::WallpaperPreviews,
    displays::WinDisplays,
    emulate::WinEmulation,
};

#[cfg(windows)]
use crate::{Native, PermissionState, PlatformError, SessionStatus};

use crate::ClipboardFormat;

/// A format as the Windows clipboard knows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowsFormat {
    /// A standard format, by number: `CF_UNICODETEXT` (13), `CF_DIB` (8).
    Standard(u32),
    /// A format registered by name ("HTML Format", "Rich Text Format",
    /// "PNG").
    Registered(&'static str),
}

/// What the Windows clipboard holds for `data` in `format`, the same bytes
/// the broker writes there: `CF_UNICODETEXT` with CRLF line ends, "HTML
/// Format" with its header, "Rich Text Format". For content the Windows app
/// puts on the clipboard with files (docs/architecture.md §9), which the
/// SYSTEM agent converts: string work only. An image is left out, nothing
/// at all: the agent decodes no image from another device, and a PNG
/// without the bitmap the broker would draw beside it is not put there
/// either.
pub fn clipboard_representations(
    format: ClipboardFormat,
    data: &[u8],
) -> Option<(WindowsFormat, Vec<u8>)> {
    let windows = match format {
        ClipboardFormat::Text => WindowsFormat::Standard(clipboard_formats::CF_UNICODETEXT),
        ClipboardFormat::Rtf => WindowsFormat::Registered(clipboard_formats::RTF_FORMAT),
        ClipboardFormat::Html => WindowsFormat::Registered(clipboard_formats::HTML_FORMAT),
        ClipboardFormat::Png => return None,
    };
    Some((windows, promise::clipboard_bytes(format, data)))
}

/// Windows asks for no permissions at run time: installing the service
/// takes one UAC prompt, and the agent then runs as SYSTEM.
#[cfg(windows)]
#[derive(Debug, Default)]
pub struct WinStatus {
    /// The last error reading the session's state, logged once.
    failure: std::sync::Mutex<Option<String>>,
    /// The last error telling whether an app holds the pointer, logged
    /// once.
    pointer_failure: std::sync::Mutex<Option<String>>,
    /// Asks on the input desktop; started at the first question.
    pointer: std::sync::Mutex<Option<pointer::PointerProbe>>,
}

#[cfg(windows)]
impl SessionStatus for WinStatus {
    fn permissions(&self) -> Vec<PermissionState> {
        Vec::new()
    }

    fn screen_locked(&self) -> bool {
        let read = session::session_locked();
        let mut failure = self.failure.lock().unwrap_or_else(|e| e.into_inner());
        match read {
            Ok(locked) => {
                *failure = None;
                locked
            }
            Err(e) => {
                let e = e.to_string();
                if failure.as_deref() != Some(e.as_str()) {
                    tracing::warn!(error = %e, "cannot tell whether the session is locked");
                    *failure = Some(e);
                }
                false
            }
        }
    }

    /// The agent follows the input desktop onto the secure desktop
    /// (`desktop`), so a locked session takes input like any other.
    fn lock_screen_input(&self) -> bool {
        true
    }

    fn pointer_captured(&self) -> bool {
        let read = {
            let mut probe = self.pointer.lock().unwrap_or_else(|e| e.into_inner());
            if probe.is_none() {
                *probe = pointer::PointerProbe::start().ok();
            }
            match probe.as_ref() {
                Some(probe) => probe.held(),
                None => Err(PlatformError::Os(
                    "cannot start the thread that asks".to_owned(),
                )),
            }
        };
        let mut failure = self
            .pointer_failure
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match read {
            Ok(held) => {
                *failure = None;
                held
            }
            Err(e) => {
                // As if not, as on the secure desktop.
                let e = e.to_string();
                if failure.as_deref() != Some(e.as_str()) {
                    tracing::debug!(error = %e, "cannot tell whether an app holds the pointer");
                    *failure = Some(e);
                }
                false
            }
        }
    }
}

#[cfg(windows)]
pub fn native() -> Result<Native, PlatformError> {
    use std::sync::Arc;

    make_per_monitor_aware()?;
    let monitors = Arc::new(displays::MonitorCache::default());
    Ok(Native {
        capture: Box::new(WinCapture::new(monitors.clone())),
        emulation: Box::new(WinEmulation::new(monitors.clone())?),
        displays: Box::new(WinDisplays::new(monitors)),
        // The desktop picture belongs to the user, and SYSTEM does not read
        // the user's files: the agent gets the previews from the broker
        // instead (`WallpaperPreviews`).
        pictures: Box::new(crate::NoDesktopPictures),
        status: Box::new(WinStatus::default()),
        // The broker owns the clipboard, as the user.
        clipboard: None,
    })
}

/// Makes Windows report physical pixels to this process on every monitor.
/// The executable has no manifest, so this call is what sets it. Should a
/// manifest declare it, the call fails and the awareness it set is
/// accepted.
#[cfg(windows)]
pub(crate) fn make_per_monitor_aware() -> Result<(), PlatformError> {
    use ::windows::Win32::UI::HiDpi::{
        AreDpiAwarenessContextsEqual, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        GetThreadDpiAwarenessContext, SetProcessDpiAwarenessContext,
    };

    let wanted = DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2;
    // SAFETY: plain calls on a predefined awareness context.
    unsafe {
        if SetProcessDpiAwarenessContext(wanted).is_ok()
            || AreDpiAwarenessContextsEqual(GetThreadDpiAwarenessContext(), wanted).as_bool()
        {
            return Ok(());
        }
    }
    Err(PlatformError::Os(
        "the process is not per-monitor DPI aware (v2), so coordinates would be scaled".to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(text: &str) -> Vec<u8> {
        text.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    /// Text only gets CRLF line ends: a form feed or a line separator stay
    /// as they are.
    #[test]
    fn text_is_represented_with_crlf_only() {
        let text = "a\u{c}b\u{2028}c\nd\r\ne";
        assert_eq!(
            clipboard_representations(ClipboardFormat::Text, text.as_bytes()),
            Some((
                WindowsFormat::Standard(13),
                utf16("a\u{c}b\u{2028}c\r\nd\r\ne\0")
            ))
        );
    }

    #[test]
    fn html_and_rtf_are_represented_as_the_broker_writes_them() {
        let html = clipboard_representations(ClipboardFormat::Html, b"<b>x</b>");
        let Some((WindowsFormat::Registered("HTML Format"), bytes)) = &html else {
            panic!("{html:?}");
        };
        assert!(bytes.starts_with(b"Version:"));
        assert_eq!(
            *bytes,
            [clipboard_formats::cf_html_from_html(b"<b>x</b>"), vec![0]].concat()
        );
        assert_eq!(
            clipboard_representations(ClipboardFormat::Rtf, b"{\\rtf1}"),
            Some((
                WindowsFormat::Registered("Rich Text Format"),
                b"{\\rtf1}\0".to_vec()
            ))
        );
    }

    /// An image copied with files is left out: neither decoded nor passed
    /// on as a PNG without its bitmap.
    #[test]
    fn an_image_copied_with_files_is_left_out() {
        assert_eq!(
            clipboard_representations(ClipboardFormat::Png, b"\x89PNG\r\n\x1a\n"),
            None
        );
    }
}
