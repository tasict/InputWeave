//! Whether the agent's session is locked, from the flags Terminal Services
//! reports for it (`WTSINFOEX_LEVEL1.SessionFlags`).

/// `WTS_SESSIONSTATE_LOCK`. An unlocked session reports
/// `WTS_SESSIONSTATE_UNLOCK` (1), and one Windows cannot tell
/// `WTS_SESSIONSTATE_UNKNOWN` (-1).
const LOCKED: i32 = 0;

/// Whether session flags say the session is locked. Windows 7 swapped lock
/// and unlock; InputWeave needs Windows 10 or later.
pub(crate) fn locked(flags: i32) -> bool {
    flags == LOCKED
}

/// Whether this process's session is locked now.
#[cfg(windows)]
pub(crate) fn session_locked() -> Result<bool, crate::PlatformError> {
    use ::windows::Win32::System::RemoteDesktop::{
        WTS_CURRENT_SERVER_HANDLE, WTS_CURRENT_SESSION, WTSFreeMemory, WTSINFOEXW,
        WTSQuerySessionInformationW, WTSSessionInfoEx,
    };
    use ::windows::core::PWSTR;

    let mut buffer = PWSTR::null();
    let mut len = 0u32;
    // SAFETY: Windows allocates the buffer and reports its length; it is
    // freed below with WTSFreeMemory.
    unsafe {
        WTSQuerySessionInformationW(
            Some(WTS_CURRENT_SERVER_HANDLE),
            WTS_CURRENT_SESSION,
            WTSSessionInfoEx,
            &mut buffer,
            &mut len,
        )
    }
    .map_err(|e| crate::PlatformError::Os(format!("cannot read the session's state: {e}")))?;
    let info = buffer.0.cast::<WTSINFOEXW>();
    let flags = (!info.is_null() && len as usize >= std::mem::size_of::<WTSINFOEXW>())
        // SAFETY: the buffer holds a WTSINFOEXW, whose only level is 1.
        .then(|| unsafe { ((*info).Level == 1).then(|| (*info).Data.WTSInfoExLevel1.SessionFlags) })
        .flatten();
    // SAFETY: allocated by WTSQuerySessionInformationW, freed once.
    unsafe { WTSFreeMemory(buffer.0.cast()) };
    flags
        .map(locked)
        .ok_or_else(|| crate::PlatformError::Os("the session's state is not level 1".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_lock_flag_means_locked() {
        assert!(locked(0));
        assert!(!locked(1));
        assert!(!locked(-1));
    }
}
