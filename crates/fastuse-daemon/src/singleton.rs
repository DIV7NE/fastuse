//! Per-session singleton mutex (D-03 / D-04).
//!
//! Uses `Local\fastuse-daemon-singleton-{session_id}` so that a stale OS
//! handle from a crashed daemon is automatically released by Windows.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE,
};
use windows::Win32::System::Threading::CreateMutexW;

/// Outcome of [`acquire_singleton`].
#[derive(Debug)]
pub enum AcquireOutcome {
    /// We are the sole daemon; hold this guard for the process lifetime.
    Acquired(SingletonGuard),
    /// Another daemon already holds the mutex; the caller should exit 0.
    AlreadyRunning,
}

/// RAII guard for the singleton mutex; releases on Drop.
#[derive(Debug)]
pub struct SingletonGuard {
    handle: HANDLE,
}

impl Drop for SingletonGuard {
    fn drop(&mut self) {
        // SAFETY: handle was returned by CreateMutexW.
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

/// Try to acquire the per-session daemon singleton mutex.
pub fn acquire_singleton(session_id: u32) -> std::io::Result<AcquireOutcome> {
    let name = format!("Local\\fastuse-daemon-singleton-{}", session_id);
    let wide: Vec<u16> = OsStr::new(&name)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // SAFETY: CreateMutexW with a valid wide-null-terminated name.
    let handle = unsafe { CreateMutexW(None, true, PCWSTR(wide.as_ptr())) }
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("CreateMutexW: {e}")))?;
    // SAFETY: GetLastError is always callable.
    let last = unsafe { GetLastError() };
    if last == ERROR_ALREADY_EXISTS {
        // SAFETY: closing our (non-owning) handle to the existing mutex.
        unsafe {
            let _ = CloseHandle(handle);
        }
        return Ok(AcquireOutcome::AlreadyRunning);
    }
    Ok(AcquireOutcome::Acquired(SingletonGuard { handle }))
}
