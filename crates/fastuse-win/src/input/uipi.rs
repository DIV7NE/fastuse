//! UI Privilege Isolation foreground-integrity check.
//!
//! Returns `Err(UipiBlocked)` with a descriptive `hint` when the foreground
//! window is at higher integrity level than the daemon. Called as the FIRST
//! action in every input handler that injects events (CONTEXT.md hard rule:
//! never silent no-op).
//!
//! Race-safe: the check + the SendInput dispatch happen sequentially on the
//! same input-thread iteration (no other dispatch can race).

use std::sync::OnceLock;

use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND};
use windows::Win32::Security::{
    GetTokenInformation, TokenIntegrityLevel, TOKEN_MANDATORY_LABEL, TOKEN_QUERY,
};
use windows::Win32::System::ProcessStatus::{GetModuleBaseNameW};
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

use fastuse_proto::{Error as ProtoError, ErrorCode};

const SECURITY_MANDATORY_UNTRUSTED_RID: u32 = 0x0000_0000;
const SECURITY_MANDATORY_LOW_RID: u32 = 0x0000_1000;
const SECURITY_MANDATORY_MEDIUM_RID: u32 = 0x0000_2000;
const SECURITY_MANDATORY_HIGH_RID: u32 = 0x0000_3000;
const SECURITY_MANDATORY_SYSTEM_RID: u32 = 0x0000_4000;

static OUR_INTEGRITY: OnceLock<u32> = OnceLock::new();

/// Compute and cache the daemon's own integrity level. Should be called once
/// at daemon startup; subsequent calls are free.
pub fn init_our_integrity_level() {
    let _ = OUR_INTEGRITY.get_or_init(|| read_self_integrity().unwrap_or(SECURITY_MANDATORY_MEDIUM_RID));
}

fn our_integrity() -> u32 {
    *OUR_INTEGRITY.get_or_init(|| read_self_integrity().unwrap_or(SECURITY_MANDATORY_MEDIUM_RID))
}

fn read_self_integrity() -> Result<u32, ()> {
    // SAFETY: GetCurrentProcess returns a pseudo-handle; safe to use without
    // closing.
    let proc = unsafe { GetCurrentProcess() };
    let mut token = HANDLE::default();
    // SAFETY: opening a token on our own process for QUERY.
    unsafe {
        OpenProcessToken(proc, TOKEN_QUERY, &mut token).map_err(|_| ())?;
    }
    let res = read_token_integrity(token);
    // SAFETY: closing our own token handle.
    let _ = unsafe { CloseHandle(token) };
    res
}

fn read_token_integrity(token: HANDLE) -> Result<u32, ()> {
    // First call: get required size.
    let mut needed: u32 = 0;
    // SAFETY: passing None length pointer + None buffer to query the size.
    let _ = unsafe {
        GetTokenInformation(token, TokenIntegrityLevel, None, 0, &mut needed)
    };
    if needed == 0 {
        return Err(());
    }
    let mut buf = vec![0u8; needed as usize];
    // SAFETY: buf is sized per the previous query; ptr is valid for `needed` bytes.
    unsafe {
        GetTokenInformation(
            token,
            TokenIntegrityLevel,
            Some(buf.as_mut_ptr() as *mut _),
            needed,
            &mut needed,
        )
        .map_err(|_| ())?;
    }
    // The buffer holds a TOKEN_MANDATORY_LABEL whose Label.Sid points into
    // the same buffer. Walk the SID's last sub-authority.
    if buf.len() < std::mem::size_of::<TOKEN_MANDATORY_LABEL>() {
        return Err(());
    }
    // SAFETY: buf is properly sized and aligned per GetTokenInformation contract.
    let label = unsafe { std::ptr::read_unaligned(buf.as_ptr() as *const TOKEN_MANDATORY_LABEL) };
    let sid = label.Label.Sid;
    if sid.is_invalid() {
        return Err(());
    }
    // SAFETY: SID is a valid pointer per Windows TOKEN_MANDATORY_LABEL contract.
    unsafe {
        // SubAuthorityCount is at offset 1 (u8); SubAuthority array begins
        // at offset 8 (4-byte revision + 4-byte identifierauthority padding).
        // Use the Win32 helpers via raw pointer arithmetic, conservatively:
        // last sub-authority = SubAuthority[SubAuthorityCount - 1].
        // We replicate `GetSidSubAuthority(sid, count - 1)` manually.
        let count_byte = *(sid.0.add(1) as *const u8);
        if count_byte == 0 {
            return Err(());
        }
        // SubAuthority array starts at offset 8 from sid.0.
        let arr = sid.0.add(8) as *const u32;
        let last = std::ptr::read_unaligned(arr.add((count_byte - 1) as usize));
        Ok(last)
    }
}

/// Check the foreground window's integrity. Returns `Ok(())` if our daemon
/// can drive it. Returns `Err(UipiBlocked)` if the foreground window's
/// process runs at HIGHER integrity than the daemon.
pub fn check_foreground_integrity() -> Result<(), ProtoError> {
    // SAFETY: GetForegroundWindow is always safe.
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.0 as isize == 0 {
        // No foreground — no UIPI block (e.g. lock screen). Let the caller
        // decide; SendInput will simply land nowhere.
        return Ok(());
    }
    let mut pid: u32 = 0;
    // SAFETY: GetWindowThreadProcessId writes pid; HWND is a kernel handle
    // we just got from GetForegroundWindow.
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    if pid == 0 {
        return Ok(());
    }
    let proc = match unsafe {
        OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
    } {
        Ok(h) => h,
        Err(_) => {
            // We can't even open it — almost certainly a higher-IL process
            // (typical on protected PIDs). Conservatively report blocked.
            return Err(uipi_blocked(pid, "<unknown>", our_integrity(), our_integrity().saturating_add(1)));
        }
    };
    let mut token = HANDLE::default();
    // SAFETY: opening token on a process handle we own.
    let opened = unsafe { OpenProcessToken(proc, TOKEN_QUERY, &mut token) };
    let fg_il = match opened {
        Ok(()) => match read_token_integrity(token) {
            Ok(il) => {
                let _ = unsafe { CloseHandle(token) };
                il
            }
            Err(_) => {
                let _ = unsafe { CloseHandle(token) };
                let _ = unsafe { CloseHandle(proc) };
                return Ok(());
            }
        },
        Err(_) => {
            // Couldn't open the token — pessimistic: report blocked.
            let _ = unsafe { CloseHandle(proc) };
            return Err(uipi_blocked(pid, "<unknown>", our_integrity(), our_integrity().saturating_add(1)));
        }
    };
    let our_il = our_integrity();
    if fg_il > our_il {
        let name = process_basename(proc).unwrap_or_else(|| "<unknown>".to_string());
        let _ = unsafe { CloseHandle(proc) };
        return Err(uipi_blocked(pid, &name, our_il, fg_il));
    }
    let _ = unsafe { CloseHandle(proc) };
    Ok(())
}

fn process_basename(proc: HANDLE) -> Option<String> {
    let mut buf = [0u16; 512];
    // SAFETY: buf is on the stack; len is in u16 units per win32 contract.
    let n = unsafe {
        GetModuleBaseNameW(proc, None, &mut buf)
    };
    if n == 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..n as usize]))
}

fn integrity_label(level: u32) -> &'static str {
    match level {
        SECURITY_MANDATORY_UNTRUSTED_RID => "UNTRUSTED",
        SECURITY_MANDATORY_LOW_RID => "LOW",
        SECURITY_MANDATORY_MEDIUM_RID => "MEDIUM",
        SECURITY_MANDATORY_HIGH_RID => "HIGH",
        SECURITY_MANDATORY_SYSTEM_RID => "SYSTEM",
        _ => "UNKNOWN",
    }
}

fn uipi_blocked(pid: u32, proc_name: &str, our_il: u32, fg_il: u32) -> ProtoError {
    ProtoError::new(
        ErrorCode::UipiBlocked,
        format!(
            "foreground process '{proc_name}' (pid {pid}) runs at {} integrity; daemon runs at {} — UIPI blocks input from a lower-integrity sender",
            integrity_label(fg_il),
            integrity_label(our_il)
        ),
    )
    .with_hint(format!(
        "launch fastuse elevated to drive '{proc_name}' (right-click the launcher → Run as administrator)"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integrity_label_table() {
        assert_eq!(integrity_label(SECURITY_MANDATORY_LOW_RID), "LOW");
        assert_eq!(integrity_label(SECURITY_MANDATORY_MEDIUM_RID), "MEDIUM");
        assert_eq!(integrity_label(SECURITY_MANDATORY_HIGH_RID), "HIGH");
        assert_eq!(integrity_label(SECURITY_MANDATORY_SYSTEM_RID), "SYSTEM");
    }

    #[test]
    fn our_integrity_resolves() {
        let il = our_integrity();
        // Whatever this binary runs at, it should be one of the standard
        // integrity levels.
        assert!(matches!(
            il,
            SECURITY_MANDATORY_LOW_RID
                | SECURITY_MANDATORY_MEDIUM_RID
                | SECURITY_MANDATORY_HIGH_RID
                | SECURITY_MANDATORY_SYSTEM_RID
        ), "unexpected integrity {il:#x}");
    }

    #[test]
    fn uipi_blocked_error_has_hint() {
        let e = uipi_blocked(1234, "cmd.exe", SECURITY_MANDATORY_MEDIUM_RID, SECURITY_MANDATORY_HIGH_RID);
        assert_eq!(e.code, ErrorCode::UipiBlocked);
        assert!(e.message.contains("cmd.exe"));
        assert!(e.message.contains("HIGH"));
        assert!(e.hint.unwrap().contains("elevated"));
    }

    #[test]
    fn check_foreground_does_not_panic() {
        // Smoke test: must complete without panicking on any system.
        let _ = check_foreground_integrity();
    }
}
