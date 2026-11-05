//! The virtual keyboard and mouse of `hid_report` as a device in the
//! kernel, created with `IOHIDUserDeviceCreateWithProperties`. Its reports
//! reach the window server the way a real keyboard's and mouse's do, so
//! they work on the lock screen and at the login window, which drop the
//! events other processes post (docs/architecture.md §2).
//!
//! Creating the device needs the managed entitlement
//! `com.apple.developer.hid.virtual.device`, which Apple grants per team
//! and which a provisioning profile in the agent's bundle carries
//! (apps/macos/scripts/embed-agent.sh). Without it creation fails, and the
//! caller keeps posting Quartz events.
//!
//! The device has no dispatch queue and is never activated: those only
//! deliver the kernel's get and set report requests, and with no feature
//! or output report in its descriptor there are none. Releasing the last
//! reference removes it from the system.

use std::ffi::c_void;
use std::ptr::{self, NonNull};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use objc2_core_foundation::{
    CFBoolean, CFData, CFDictionary, CFNumber, CFRetained, CFString, CFType,
};

use crate::PlatformError;
use crate::hid_report::DESCRIPTOR;

/// What lets this process create the device.
pub(crate) const ENTITLEMENT: &str = "com.apple.developer.hid.virtual.device";

/// `kIOHIDStandardTypeANSI`: tells the system the keyboard's layout
/// standard, which it otherwise asks the user for with the Keyboard Setup
/// Assistant when a keyboard it does not know appears.
const STANDARD_TYPE_ANSI: i32 = 0;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C-unwind" {
    fn IOHIDUserDeviceCreateWithProperties(
        allocator: *const c_void,
        properties: *const c_void,
        options: u32,
    ) -> Option<NonNull<CFType>>;
    fn IOHIDUserDeviceHandleReportWithTimeStamp(
        device: NonNull<CFType>,
        timestamp: u64,
        report: *const u8,
        report_length: isize,
    ) -> i32;
}

#[link(name = "Security", kind = "framework")]
unsafe extern "C-unwind" {
    fn SecTaskCreateFromSelf(allocator: *const c_void) -> Option<NonNull<CFType>>;
    fn SecTaskCopyValueForEntitlement(
        task: NonNull<CFType>,
        entitlement: NonNull<CFString>,
        error: *mut *mut c_void,
    ) -> Option<NonNull<CFType>>;
}

unsafe extern "C" {
    fn mach_absolute_time() -> u64;
}

/// Whether this process carries the entitlement, asked of its own code
/// signature once, without creating a device.
pub(crate) fn entitled() -> bool {
    static ENTITLED: OnceLock<bool> = OnceLock::new();
    *ENTITLED.get_or_init(check_entitlement)
}

/// How long after the device stops being used its last reports may still
/// reach the event tap.
const AFTERGLOW_MS: u64 = 200;

/// What the virtual device is doing, shared by injection, which creates it,
/// capture, which must not take its events for the user's own, and the
/// session status, which tells other devices whether input reaches this
/// lock screen and notes, about every second, whether it is still up.
///
/// Times are milliseconds since `epoch`, never 0, which stands for never.
#[derive(Debug, Default)]
pub(crate) struct HidState {
    in_use: AtomicBool,
    /// When injection started using it.
    used_since_ms: AtomicU64,
    /// Whether the lock screen was up when the session status last looked;
    /// set again when injection starts using the device, which just saw it.
    lock_screen_up: AtomicBool,
    /// When it stopped owning the input: injection let it go, or the session
    /// status saw the lock screen gone while it was still in use.
    stopped_ms: AtomicU64,
    /// Creating it failed although this build is entitled to it; from then
    /// on it is not tried again.
    failed: AtomicBool,
}

fn epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

/// Now, in milliseconds since `epoch`.
pub(crate) fn now_ms() -> u64 {
    epoch().elapsed().as_millis() as u64 + 1
}

impl HidState {
    /// Whether input goes through the device now.
    pub(crate) fn set_in_use(&self, in_use: bool) {
        self.set_in_use_at(in_use, now_ms());
    }

    fn set_in_use_at(&self, in_use: bool, now: u64) {
        if in_use {
            self.used_since_ms.store(now, Ordering::SeqCst);
            self.lock_screen_up.store(true, Ordering::SeqCst);
        } else if self.in_use.load(Ordering::SeqCst) {
            self.stopped_ms.store(now, Ordering::SeqCst);
        }
        self.in_use.store(in_use, Ordering::SeqCst);
    }

    /// What the session status saw, from a read that began at `read_at`:
    /// whether the lock screen is up (and the session on the console). Once
    /// it is gone, the device no longer owns the input, even if injection
    /// has not let it go yet: then someone at this Mac is seen again within
    /// about a second. A read that began before injection started using the
    /// device is older than what injection saw, and is ignored.
    pub(crate) fn note_lock_screen(&self, up: bool, read_at: u64) {
        self.note_lock_screen_at(up, read_at, now_ms());
    }

    fn note_lock_screen_at(&self, up: bool, read_at: u64, now: u64) {
        let in_use = self.in_use.load(Ordering::SeqCst);
        if !up && in_use && self.used_since_ms.load(Ordering::SeqCst) >= read_at {
            return;
        }
        let was_up = self.lock_screen_up.swap(up, Ordering::SeqCst);
        if was_up && !up && in_use {
            self.stopped_ms.store(now, Ordering::SeqCst);
        }
    }

    /// Whether the events the tap sees now may be the device's: it is in
    /// use while the lock screen is up, or stopped a moment ago and its
    /// last reports are still on the way. Meanwhile capture takes no input
    /// for the user's own, so the device's input never counts as someone at
    /// this Mac taking over.
    pub(crate) fn owns_input(&self) -> bool {
        self.owns_input_at(now_ms())
    }

    fn owns_input_at(&self, now: u64) -> bool {
        if self.in_use.load(Ordering::SeqCst) && self.lock_screen_up.load(Ordering::SeqCst) {
            return true;
        }
        let stopped = self.stopped_ms.load(Ordering::SeqCst);
        stopped != 0 && now < stopped + AFTERGLOW_MS
    }

    pub(crate) fn set_failed(&self) {
        self.failed.store(true, Ordering::SeqCst);
    }

    pub(crate) fn failed(&self) -> bool {
        self.failed.load(Ordering::SeqCst)
    }

    /// Whether input from other devices reaches this session's lock
    /// screen: this build may create the device, creating it never failed,
    /// and the session is on the console. The device is the whole
    /// system's, so from a session fast user switching moved off the
    /// console its input would reach another user's.
    pub(crate) fn lock_screen_input(&self) -> bool {
        entitled() && !self.failed() && super::on_console()
    }
}

fn check_entitlement() -> bool {
    // SAFETY: takes an optional allocator; returns an owned reference.
    let Some(task) = (unsafe { SecTaskCreateFromSelf(ptr::null()) }) else {
        return false;
    };
    // SAFETY: SecTaskCreateFromSelf follows the create rule.
    let task = unsafe { CFRetained::from_raw(task) };
    let name = CFString::from_static_str(ENTITLEMENT);
    // SAFETY: both references are valid for the call; the error is not
    // asked for. The value follows the copy rule.
    let value = unsafe {
        SecTaskCopyValueForEntitlement(
            CFRetained::as_ptr(&task),
            CFRetained::as_ptr(&name),
            ptr::null_mut(),
        )
    };
    // SAFETY: owned, as above.
    value
        .map(|v| unsafe { CFRetained::from_raw(v) })
        .and_then(|v| v.downcast::<CFBoolean>().ok())
        .is_some_and(|b| b.as_bool())
}

/// The device, while it exists.
pub(crate) struct VirtualHid {
    device: CFRetained<CFType>,
}

// SAFETY: an IOHIDUserDevice without a dispatch queue is only used to hand
// reports to the kernel, through `&self` of one owner at a time; CF
// reference counting is atomic.
unsafe impl Send for VirtualHid {}

impl VirtualHid {
    /// Creates the device. Fails without the entitlement.
    pub(crate) fn new() -> Result<Self, PlatformError> {
        let descriptor = CFData::from_bytes(DESCRIPTOR);
        let zero = CFNumber::new_i32(0);
        let standard = CFNumber::new_i32(STANDARD_TYPE_ANSI);
        let product = CFString::from_static_str("InputWeave Virtual Keyboard and Mouse");
        let manufacturer = CFString::from_static_str("InputWeave");
        let serial = CFString::from_static_str("InputWeave-1");
        let transport = CFString::from_static_str("Virtual");
        let keys = [
            CFString::from_static_str("ReportDescriptor"),
            // InputWeave owns no USB vendor ID; the device is known by its
            // product name and serial number.
            CFString::from_static_str("VendorID"),
            CFString::from_static_str("ProductID"),
            CFString::from_static_str("Product"),
            CFString::from_static_str("Manufacturer"),
            CFString::from_static_str("SerialNumber"),
            CFString::from_static_str("Transport"),
            CFString::from_static_str("StandardType"),
        ];
        let values: [&CFType; 8] = [
            &descriptor,
            &zero,
            &zero,
            &product,
            &manufacturer,
            &serial,
            &transport,
            &standard,
        ];
        let keys: Vec<&CFString> = keys.iter().map(|k| &**k).collect();
        let properties = CFDictionary::<CFString, CFType>::from_slices(&keys, &values);
        // SAFETY: the dictionary is valid for the call; no options. The
        // device follows the create rule.
        let device = unsafe {
            IOHIDUserDeviceCreateWithProperties(
                ptr::null(),
                CFRetained::as_ptr(&properties).as_ptr().cast(),
                0,
            )
        }
        .ok_or_else(|| {
            PlatformError::Os(format!(
                "cannot create the virtual keyboard and mouse (is {ENTITLEMENT} granted?)"
            ))
        })?;
        Ok(Self {
            // SAFETY: owned, as above.
            device: unsafe { CFRetained::from_raw(device) },
        })
    }

    /// Hands one report to the kernel, as if the device had sent it now.
    pub(crate) fn send(&self, report: &[u8]) -> Result<(), PlatformError> {
        // SAFETY: the device lives as long as `self`, and the report buffer
        // for the call.
        let rc = unsafe {
            IOHIDUserDeviceHandleReportWithTimeStamp(
                CFRetained::as_ptr(&self.device),
                mach_absolute_time(),
                report.as_ptr(),
                report.len() as isize,
            )
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(PlatformError::Os(format!(
                "the virtual keyboard and mouse refused a report ({rc:#x})"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_device_owns_the_input_while_the_lock_screen_is_up() {
        let state = HidState::default();
        assert!(!state.owns_input_at(50));
        // Letting go of a device never used changes nothing.
        state.set_in_use_at(false, 60);
        assert!(!state.owns_input_at(70));
        state.set_in_use_at(true, 100);
        assert!(state.owns_input_at(150));
        // A read that began before the device was used is older news.
        state.note_lock_screen_at(false, 90, 160);
        assert!(state.owns_input_at(170));
        // Unlocked at this Mac while still in use: its last reports have a
        // moment, then the Mac's own input counts again.
        state.note_lock_screen_at(false, 1000, 1001);
        assert!(state.owns_input_at(1100));
        assert!(!state.owns_input_at(1201));
        // Locked again before injection let it go.
        state.note_lock_screen_at(true, 2000, 2001);
        assert!(state.owns_input_at(2002));
        // Injection lets it go.
        state.set_in_use_at(false, 3000);
        assert!(state.owns_input_at(3100));
        assert!(!state.owns_input_at(3201));
        state.note_lock_screen_at(false, 4000, 4001);
        assert!(!state.owns_input_at(4002));
        state.set_failed();
        assert!(state.failed());
    }

    /// A test binary is not signed with the entitlement, so the check says
    /// so and the device is not created: nothing is added to this Mac.
    #[test]
    fn without_the_entitlement_no_device_is_created() {
        assert!(!entitled());
        assert!(!HidState::default().lock_screen_input());
        let error = VirtualHid::new()
            .err()
            .expect("created without the entitlement");
        assert!(error.to_string().contains(ENTITLEMENT), "{error}");
    }
}
