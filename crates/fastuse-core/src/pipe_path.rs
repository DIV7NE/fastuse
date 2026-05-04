//! Resolve the session-scoped pipe path: `\\.\pipe\fastuse-{session}-{sid_short}`.
//!
//! Per D-21, the pattern is owned by `fastuse-proto` (platform-free) and the
//! actual resolution lives here because it requires Win32 calls.

use std::ffi::c_void;
use std::mem::size_of;

use thiserror::Error;
use windows::core::PWSTR;
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_INSUFFICIENT_BUFFER, HANDLE, HLOCAL,
};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, OpenProcessToken,
};

/// Errors returned by [`pipe_path_resolve`].
#[derive(Debug, Error)]
pub enum PipeIdentityError {
    /// `ProcessIdToSessionId` failed.
    #[error("ProcessIdToSessionId failed: {0}")]
    SessionLookup(u32),
    /// `OpenProcessToken` failed.
    #[error("OpenProcessToken failed: {0}")]
    OpenToken(u32),
    /// `GetTokenInformation(TokenUser)` failed.
    #[error("GetTokenInformation failed: {0}")]
    TokenInfo(u32),
    /// `ConvertSidToStringSidW` failed.
    #[error("ConvertSidToStringSidW failed: {0}")]
    SidConvert(u32),
}

/// Resolved pipe identity for the current process.
#[derive(Debug, Clone)]
pub struct PipeIdentity {
    /// WTS console session id of this process.
    pub session_id: u32,
    /// Last RID of the current user's SID (compact identifier for the pipe name).
    pub user_sid_short: u32,
    /// Full pipe path string.
    pub path: String,
}

/// Resolve the fully qualified pipe path for the current process.
pub fn pipe_path_resolve() -> Result<PipeIdentity, PipeIdentityError> {
    let pid = unsafe { GetCurrentProcessId() };
    let mut session_id: u32 = 0;
    // SAFETY: ProcessIdToSessionId writes to our local u32 on success.
    let ok = unsafe { ProcessIdToSessionId(pid, &mut session_id) };
    if ok.is_err() {
        let err = unsafe { GetLastError().0 };
        return Err(PipeIdentityError::SessionLookup(err));
    }

    let sid_short = current_user_sid_short()?;
    let path = format!(r"\\.\pipe\fastuse-{}-{}", session_id, sid_short);
    Ok(PipeIdentity {
        session_id,
        user_sid_short: sid_short,
        path,
    })
}

fn current_user_sid_short() -> Result<u32, PipeIdentityError> {
    // SAFETY: GetCurrentProcess returns a pseudo-handle that doesn't need closing.
    let process = unsafe { GetCurrentProcess() };
    let mut token_handle = HANDLE::default();
    // SAFETY: token_handle is a valid out-pointer.
    let res = unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token_handle) };
    if res.is_err() {
        let err = unsafe { GetLastError().0 };
        return Err(PipeIdentityError::OpenToken(err));
    }

    let mut needed: u32 = 0;
    // First call: probe for required size; expected to fail with ERROR_INSUFFICIENT_BUFFER.
    let _ = unsafe { GetTokenInformation(token_handle, TokenUser, None, 0, &mut needed) };
    let last = unsafe { GetLastError() };
    if last != ERROR_INSUFFICIENT_BUFFER {
        unsafe { let _ = CloseHandle(token_handle); }
        return Err(PipeIdentityError::TokenInfo(last.0));
    }

    let mut buf = vec![0u8; needed as usize];
    let res = unsafe {
        GetTokenInformation(
            token_handle,
            TokenUser,
            Some(buf.as_mut_ptr() as *mut c_void),
            needed,
            &mut needed,
        )
    };
    if res.is_err() {
        let err = unsafe { GetLastError().0 };
        unsafe { let _ = CloseHandle(token_handle); }
        return Err(PipeIdentityError::TokenInfo(err));
    }

    // SAFETY: buf holds a TOKEN_USER followed by the SID payload; layout-compatible.
    // Use read_unaligned because Vec<u8> is only byte-aligned but TOKEN_USER
    // contains pointer fields that require pointer alignment (CR-03).
    if buf.len() < size_of::<TOKEN_USER>() {
        unsafe { let _ = CloseHandle(token_handle); }
        return Err(PipeIdentityError::TokenInfo(0));
    }
    let sid_ptr = unsafe {
        let p = buf.as_ptr() as *const TOKEN_USER;
        std::ptr::read_unaligned(p).User.Sid
    };

    let mut sid_string_ptr = PWSTR::null();
    let res = unsafe { ConvertSidToStringSidW(sid_ptr, &mut sid_string_ptr) };
    if res.is_err() || sid_string_ptr.is_null() {
        let err = unsafe { GetLastError().0 };
        unsafe { let _ = CloseHandle(token_handle); }
        return Err(PipeIdentityError::SidConvert(err));
    }

    // SAFETY: sid_string_ptr was set by ConvertSidToStringSidW; null-terminated UTF-16.
    let sid_string = unsafe { sid_string_ptr.to_string() }.unwrap_or_default();
    // Free the buffer allocated by Windows.
    unsafe {
        let _ = LocalFree(Some(HLOCAL(sid_string_ptr.0 as *mut _)));
        let _ = CloseHandle(token_handle);
    }

    // Extract the last RID (after the final '-').
    let short = sid_string
        .rsplit('-')
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    Ok(short)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_path_resolves() {
        let id = pipe_path_resolve().expect("resolve must succeed on Windows dev box");
        assert!(id.path.starts_with(r"\\.\pipe\fastuse-"));
        assert!(id.path.contains(&id.session_id.to_string()));
        // Should not contain placeholder text from the pattern.
        assert!(!id.path.contains("{session_id}"));
        assert!(!id.path.contains("{user_sid_short}"));
    }
}
