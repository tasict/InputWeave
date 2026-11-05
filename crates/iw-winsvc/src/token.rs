//! Access tokens for the processes the service starts.

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

use windows::Win32::Foundation::HANDLE;
use windows::Win32::Security::{
    DuplicateTokenEx, GetTokenInformation, IsWellKnownSid, SecurityImpersonation,
    SetTokenInformation, TOKEN_ADJUST_DEFAULT, TOKEN_ADJUST_SESSIONID, TOKEN_ALL_ACCESS,
    TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE, TOKEN_QUERY, TOKEN_USER, TokenPrimary, TokenSessionId,
    TokenUser, WinLocalSystemSid,
};
use windows::Win32::System::RemoteDesktop::WTSQueryUserToken;
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::Result;

pub(crate) fn raw(token: &OwnedHandle) -> HANDLE {
    HANDLE(token.as_raw_handle())
}

/// This process's own token: SYSTEM's, when running as the service.
fn own_token(access: windows::Win32::Security::TOKEN_ACCESS_MASK) -> Result<OwnedHandle> {
    let mut token = HANDLE::default();
    // SAFETY: the pseudo handle needs no closing; the out pointer is valid
    // and the token returned is ours to close.
    unsafe {
        OpenProcessToken(GetCurrentProcess(), access, &mut token)?;
        Ok(OwnedHandle::from_raw_handle(token.0))
    }
}

/// A copy of this process's SYSTEM token placed in `session`, for starting
/// the agent there. Changing a token's session takes the TCB privilege,
/// which LocalSystem holds.
pub(crate) fn system_token_for(session: u32) -> Result<OwnedHandle> {
    let own = own_token(
        TOKEN_DUPLICATE
            | TOKEN_QUERY
            | TOKEN_ASSIGN_PRIMARY
            | TOKEN_ADJUST_DEFAULT
            | TOKEN_ADJUST_SESSIONID,
    )?;
    let mut copy = HANDLE::default();
    // SAFETY: the out pointer is valid; the new token is ours to close.
    let copy = unsafe {
        DuplicateTokenEx(
            raw(&own),
            TOKEN_ALL_ACCESS,
            None,
            SecurityImpersonation,
            TokenPrimary,
            &mut copy,
        )?;
        OwnedHandle::from_raw_handle(copy.0)
    };
    // SAFETY: the buffer is a u32 of the size passed.
    unsafe {
        SetTokenInformation(
            raw(&copy),
            TokenSessionId,
            (&raw const session).cast(),
            size_of::<u32>() as u32,
        )?;
    }
    Ok(copy)
}

/// The token of the user logged on to `session`. Fails if nobody is.
pub(crate) fn user_token(session: u32) -> Result<OwnedHandle> {
    let mut token = HANDLE::default();
    // SAFETY: the out pointer is valid; the token returned is ours to close.
    unsafe {
        WTSQueryUserToken(session, &mut token)?;
        Ok(OwnedHandle::from_raw_handle(token.0))
    }
}

/// Whether a user is logged on to `session`.
pub(crate) fn has_user(session: u32) -> bool {
    user_token(session).is_ok()
}

/// Whether this process runs as LocalSystem.
pub(crate) fn running_as_local_system() -> Result<bool> {
    let token = own_token(TOKEN_QUERY)?;
    let mut len = 0u32;
    // SAFETY: asking for the size only; this fails with
    // ERROR_INSUFFICIENT_BUFFER and sets `len`.
    let _ = unsafe { GetTokenInformation(raw(&token), TokenUser, None, 0, &mut len) };
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: the buffer is at least `len` bytes and aligned for TOKEN_USER;
    // the SID it points to lies inside the buffer.
    unsafe {
        GetTokenInformation(
            raw(&token),
            TokenUser,
            Some(buf.as_mut_ptr().cast()),
            len,
            &mut len,
        )?;
        let user = &*buf.as_ptr().cast::<TOKEN_USER>();
        Ok(IsWellKnownSid(user.User.Sid, WinLocalSystemSid).as_bool())
    }
}
