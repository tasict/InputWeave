//! Carbon and private window-server functions that objc2 does not bind,
//! isolated here so they are easy to find when macOS changes them.

use objc2_core_foundation::{CFBoolean, CFString, CFType, kCFBooleanTrue};

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn IsSecureEventInputEnabled() -> u8;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn _CGSDefaultConnection() -> i32;
    fn CGSSetConnectionProperty(cid: i32, target: i32, key: &CFString, value: &CFType) -> i32;
}

/// Whether some app has turned on secure keyboard entry (a focused password
/// field, for example). Keystrokes then never reach event taps.
pub(crate) fn secure_input_enabled() -> bool {
    // SAFETY: takes no arguments and only reads window server state.
    unsafe { IsSecureEventInputEnabled() != 0 }
}

/// Lets this process hide the cursor while another app is frontmost, which
/// `CGDisplayHideCursor` otherwise ignores. This is a private window
/// server property, the same one other KVM software relies on; if a macOS
/// release drops it, the cursor stays visible while input is grabbed.
pub(crate) fn allow_background_cursor_control() -> bool {
    let Some(yes): Option<&CFBoolean> = (unsafe { kCFBooleanTrue }) else {
        return false;
    };
    let key = CFString::from_static_str("SetsCursorInBackground");
    // SAFETY: the connection id comes from the window server for this
    // process, and the key and value are valid CF objects for the call.
    unsafe {
        let cid = _CGSDefaultConnection();
        CGSSetConnectionProperty(cid, cid, &key, yes.as_ref()) == 0
    }
}
