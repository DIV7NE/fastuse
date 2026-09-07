//! Throwaway spike: can a High-integrity process spawn a Medium-integrity
//! child? `drag_files` needs one, because an OLE drop target calls back into
//! the drag source's `IDataObject` and a medium-IL Chrome cannot make that
//! call into a high-IL process.
//!
//! Run it from an **elevated** shell:
//!   cargo run -p fastuse-win --example deelevate_spike
//!
//! Both approaches always run, even if the first succeeds: the elevated run is
//! a one-shot and knowing whether the fallback also works is worth the second
//! `cmd.exe`. Success for either is a child whose integrity SID is
//! `S-1-16-8192` (Medium Mandatory Level).
//!
//! If both pass, compare the Administrators group line in the two `whoami
//! /groups` files: approach A lowers the mandatory label only and keeps the
//! Administrators SID, approach B borrows Explorer's genuine user token.
//!
//! Run non-elevated the answer is meaningless — approach A trivially "succeeds"
//! by setting Medium on an already-Medium token — so the runner detects the
//! parent's integrity level and labels every verdict INCONCLUSIVE instead.

use std::path::Path;

use windows::core::{w, PWSTR};
use windows::Win32::Foundation::{CloseHandle, LocalFree, HANDLE, HLOCAL};
use windows::Win32::Security::Authorization::{ConvertSidToStringSidW, ConvertStringSidToSidW};
use windows::Win32::Security::{
    DuplicateTokenEx, GetLengthSid, GetTokenInformation, SecurityImpersonation,
    SetTokenInformation, TokenIntegrityLevel, TokenPrimary, PSID, SID_AND_ATTRIBUTES,
    TOKEN_ADJUST_DEFAULT, TOKEN_ALL_ACCESS, TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE,
    TOKEN_MANDATORY_LABEL, TOKEN_QUERY,
};
use windows::Win32::System::Threading::{
    CreateProcessAsUserW, CreateProcessWithTokenW, GetCurrentProcess, OpenProcess,
    OpenProcessToken, WaitForSingleObject, CREATE_NO_WINDOW, CREATE_PROCESS_LOGON_FLAGS, INFINITE,
    PROCESS_INFORMATION, PROCESS_QUERY_INFORMATION, STARTUPINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::{GetShellWindow, GetWindowThreadProcessId};

/// `SE_GROUP_INTEGRITY`. Lives under the `Win32_System_SystemServices` feature
/// (`Win32/System/SystemServices/mod.rs`), which this crate does not enable.
const SE_GROUP_INTEGRITY: u32 = 0x20;

/// Medium Mandatory Level — what a de-elevated child must report.
const MEDIUM_IL: &str = "S-1-16-8192";
/// Relative ID of High Mandatory Level; the parent needs at least this for the
/// spike to mean anything.
const HIGH_IL_RID: u32 = 12288;

fn main() {
    let parent_sid = unsafe { own_integrity_sid() };
    let parent_rid = parent_sid.as_deref().and_then(trailing_rid);
    let elevated = parent_rid.is_some_and(|rid| rid >= HIGH_IL_RID);

    println!(
        "parent integrity: {} ({})",
        parent_sid.as_deref().unwrap_or("<unreadable>"),
        if elevated {
            "High or above — verdicts below are real"
        } else {
            "NOT High — verdicts below are INCONCLUSIVE, re-run elevated"
        }
    );
    println!();

    let out_a = std::env::temp_dir().join("fastuse_spike_a.txt");
    report(
        "A (DuplicateTokenEx + TokenIntegrityLevel + CreateProcessAsUserW)",
        unsafe { approach_a(&out_a) },
        &out_a,
        elevated,
    );

    let out_b = std::env::temp_dir().join("fastuse_spike_b.txt");
    report(
        "B (shell token + CreateProcessWithTokenW)",
        unsafe { approach_b(&out_b) },
        &out_b,
        elevated,
    );

    if !elevated {
        println!("INCONCLUSIVE: this process is not High integrity. Nothing above answers the");
        println!("question. Re-run from an elevated shell and read the verdicts again.");
    }
}

/// A successful spawn.
struct Spawned {
    /// Integrity SID of the token actually handed to `CreateProcess*`.
    token_sid: String,
}

fn report(name: &str, result: windows::core::Result<Spawned>, out: &Path, elevated: bool) {
    println!("--- approach {name}");
    match result {
        Err(e) => {
            println!("  spawn FAILED: {e}");
            println!("  (no child ran; nothing to read at {})", out.display());
        }
        Ok(Spawned { token_sid }) => {
            println!("  token handed to CreateProcess: {token_sid}");
            match child_integrity_sid(out) {
                None => {
                    println!(
                        "  INDETERMINATE: spawn returned Ok but no S-1-16- line in {}",
                        out.display()
                    );
                    println!(
                        "  child output was: {:?}",
                        std::fs::read_to_string(out).unwrap_or_default()
                    );
                    println!(
                        "  (empty means the child died before writing — check the \
                         window-station/desktop DACL)"
                    );
                }
                Some(child) => {
                    let verdict = if !elevated {
                        "INCONCLUSIVE (parent not High integrity)"
                    } else if child == MEDIUM_IL {
                        "PASS — child is Medium integrity"
                    } else {
                        "FAIL — child is not Medium integrity"
                    };
                    println!("  child integrity: {child}");
                    println!("  VERDICT: {verdict}");
                }
            }
        }
    }
    println!();
}

/// Approach A: lower our own duplicated token to Medium, then spawn with it.
///
/// # Safety
/// Calls Win32 token APIs; every raw handle is closed on the success path.
unsafe fn approach_a(out: &Path) -> windows::core::Result<Spawned> {
    let mut own = HANDLE::default();
    OpenProcessToken(
        GetCurrentProcess(),
        TOKEN_DUPLICATE | TOKEN_ADJUST_DEFAULT | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY,
        &mut own,
    )?;

    let mut dup = HANDLE::default();
    // TOKEN_ALL_ACCESS on the duplicate, not just on the source: SetTokenInformation
    // is checked against the duplicate's access mask.
    let dup_result = DuplicateTokenEx(
        own,
        TOKEN_ALL_ACCESS,
        None,
        SecurityImpersonation,
        TokenPrimary,
        &mut dup,
    );
    let _ = CloseHandle(own);
    dup_result?;

    let mut sid = PSID::default();
    ConvertStringSidToSidW(w!("S-1-16-8192"), &mut sid)?;

    let til = TOKEN_MANDATORY_LABEL {
        Label: SID_AND_ATTRIBUTES {
            Sid: sid,
            Attributes: SE_GROUP_INTEGRITY,
        },
    };
    let size = size_of::<TOKEN_MANDATORY_LABEL>() as u32 + GetLengthSid(sid);
    let set_result = SetTokenInformation(
        dup,
        TokenIntegrityLevel,
        &til as *const _ as *const core::ffi::c_void,
        size,
    );
    let _ = LocalFree(Some(HLOCAL(sid.0)));
    if let Err(e) = set_result {
        let _ = CloseHandle(dup);
        return Err(e);
    }

    let token_sid = integrity_sid(dup)?;
    let mut cmd = child_command_line(out);
    let si = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut pi = PROCESS_INFORMATION::default();
    let spawn = CreateProcessAsUserW(
        Some(dup),
        w!("C:\\Windows\\System32\\cmd.exe"),
        Some(PWSTR(cmd.as_mut_ptr())),
        None,
        None,
        false,
        CREATE_NO_WINDOW,
        None,
        windows::core::PCWSTR::null(),
        &si,
        &mut pi,
    );
    let _ = CloseHandle(dup);
    spawn?;

    wait_and_close(&pi);
    Ok(Spawned { token_sid })
}

/// Approach B: borrow the shell's (medium-IL) token and spawn through seclogon.
///
/// # Safety
/// Calls Win32 token APIs; every raw handle is closed on the success path.
unsafe fn approach_b(out: &Path) -> windows::core::Result<Spawned> {
    let shell = GetShellWindow();
    if shell.0.is_null() {
        return Err(windows::core::Error::new(
            windows::Win32::Foundation::E_FAIL,
            "GetShellWindow returned null — no Explorer shell, or it is running elevated",
        ));
    }
    let mut pid = 0u32;
    GetWindowThreadProcessId(shell, Some(&mut pid));
    if pid == 0 {
        return Err(windows::core::Error::new(
            windows::Win32::Foundation::E_FAIL,
            "GetWindowThreadProcessId gave no pid for the shell window",
        ));
    }

    let shell_proc = OpenProcess(PROCESS_QUERY_INFORMATION, false, pid)?;
    let mut shell_token = HANDLE::default();
    let open = OpenProcessToken(shell_proc, TOKEN_DUPLICATE, &mut shell_token);
    let _ = CloseHandle(shell_proc);
    open?;

    let mut dup = HANDLE::default();
    let dup_result = DuplicateTokenEx(
        shell_token,
        TOKEN_ALL_ACCESS,
        None,
        SecurityImpersonation,
        TokenPrimary,
        &mut dup,
    );
    let _ = CloseHandle(shell_token);
    dup_result?;

    let token_sid = integrity_sid(dup)?;
    let mut cmd = child_command_line(out);
    let si = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut pi = PROCESS_INFORMATION::default();
    let spawn = CreateProcessWithTokenW(
        dup,
        CREATE_PROCESS_LOGON_FLAGS(0),
        w!("C:\\Windows\\System32\\cmd.exe"),
        Some(PWSTR(cmd.as_mut_ptr())),
        CREATE_NO_WINDOW,
        None,
        windows::core::PCWSTR::null(),
        &si,
        &mut pi,
    );
    let _ = CloseHandle(dup);
    spawn?;

    wait_and_close(&pi);
    Ok(Spawned { token_sid })
}

/// `whoami /groups` into an absolute path. The path is resolved here rather
/// than left as `%TEMP%` for `cmd` to expand, so the file we read back is
/// provably the one this run asked for.
fn child_command_line(out: &Path) -> Vec<u16> {
    // Delete first: a leftover file from an earlier run would otherwise read as
    // this run's verdict.
    let _ = std::fs::remove_file(out);
    // stderr is redirected too: if `whoami` cannot run under the lowered token,
    // its complaint is the diagnosis and it must not vanish into a hidden console.
    // Absolute path to whoami.exe: the child inherits our environment, and a
    // Git Bash / MSYS PATH puts a POSIX `whoami` ahead of the Windows one.
    let line = format!(
        r#"cmd.exe /c C:\Windows\System32\whoami.exe /groups > "{}" 2>&1"#,
        out.display()
    );
    line.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The `S-1-16-*` line the child's `whoami /groups` wrote, if it wrote one.
fn child_integrity_sid(out: &Path) -> Option<String> {
    let text = std::fs::read_to_string(out).ok()?;
    text.split_whitespace()
        .find(|w| w.starts_with("S-1-16-"))
        .map(str::to_owned)
}

/// Integrity SID of this process's own token, best-effort.
///
/// # Safety
/// Calls Win32 token APIs.
unsafe fn own_integrity_sid() -> Option<String> {
    let mut token = HANDLE::default();
    OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;
    let sid = integrity_sid(token).ok();
    let _ = CloseHandle(token);
    sid
}

/// Read `TokenIntegrityLevel` off a token and stringify the SID.
///
/// # Safety
/// `token` must be a valid token handle opened with `TOKEN_QUERY`.
unsafe fn integrity_sid(token: HANDLE) -> windows::core::Result<String> {
    let mut len = 0u32;
    // Sizing call: this one is *expected* to fail with ERROR_INSUFFICIENT_BUFFER.
    let _ = GetTokenInformation(token, TokenIntegrityLevel, None, 0, &mut len);
    let mut buf = vec![0u8; len as usize];
    GetTokenInformation(
        token,
        TokenIntegrityLevel,
        Some(buf.as_mut_ptr().cast()),
        len,
        &mut len,
    )?;

    let label = &*(buf.as_ptr() as *const TOKEN_MANDATORY_LABEL);
    let mut text = PWSTR::null();
    ConvertSidToStringSidW(label.Label.Sid, &mut text)?;
    let s = text.to_string().unwrap_or_default();
    let _ = LocalFree(Some(HLOCAL(text.0.cast())));
    Ok(s)
}

/// Trailing RID of a SID string, e.g. `S-1-16-12288` -> `12288`.
fn trailing_rid(sid: &str) -> Option<u32> {
    sid.rsplit('-').next()?.parse().ok()
}

/// # Safety
/// `pi` must hold handles from a successful `CreateProcess*` call.
unsafe fn wait_and_close(pi: &PROCESS_INFORMATION) {
    WaitForSingleObject(pi.hProcess, INFINITE);
    let _ = CloseHandle(pi.hProcess);
    let _ = CloseHandle(pi.hThread);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rid_parsing_separates_medium_from_high() {
        assert_eq!(trailing_rid("S-1-16-8192"), Some(8192));
        assert_eq!(trailing_rid("S-1-16-12288"), Some(HIGH_IL_RID));
        assert!(trailing_rid("S-1-16-8192").is_some_and(|r| r < HIGH_IL_RID));
        assert_eq!(trailing_rid("garbage"), None);
    }
}
