//! `install-autostart` / `uninstall-autostart` — register the daemon as an
//! at-logon Scheduled Task with highest privileges.
//!
//! Single UAC prompt at install time. Daemon then auto-starts at every Windows
//! logon at admin integrity level with no further UAC prompts. Solves the
//! "UAC keeps popping up" problem: the daemon is already running by the time
//! any CLI/MCP call reaches it, so the auto-spawn-via-runas path never fires.
//!
//! The Scheduled Task launches the daemon with `--idle-timeout 0` so it never
//! voluntarily shuts down between sessions.

use anyhow::{anyhow, Context, Result};
use std::path::PathBuf;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_CANCELLED};
use windows::Win32::System::Threading::{WaitForSingleObject, INFINITE};
use windows::Win32::UI::Shell::{
    ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

const TASK_NAME: &str = "fastuse-daemon";

/// Register the autostart Task Scheduler entry, kill any pre-existing daemon,
/// and trigger the task so the daemon is up immediately.
pub fn install() -> Result<()> {
    let daemon = locate_daemon()?;
    eprintln!("Installing fastuse autostart Scheduled Task...");
    eprintln!("(UAC will prompt once. Accept it.)");

    // One elevated cmd.exe runs every step, so a single UAC prompt suffices.
    let script = format!(
        r#"taskkill /F /IM fastuse-daemon.exe >nul 2>&1 & del /F /Q "%LOCALAPPDATA%\fastuse\daemon.pid" >nul 2>&1 & schtasks /Create /TN "{name}" /TR "\"{exe}\" --idle-timeout 0" /SC ONLOGON /RL HIGHEST /F & schtasks /Run /TN "{name}""#,
        name = TASK_NAME,
        exe = daemon.display(),
    );
    run_elevated_cmd(&script).context("failed to register Scheduled Task")?;
    eprintln!();
    eprintln!("Done.");
    eprintln!(
        "fastuse-daemon will now auto-start at every Windows logon with no further UAC prompts."
    );
    eprintln!("Verify with: fastuse-cli ping");
    println!("{{\"ok\":true,\"installed\":true,\"task_name\":\"{TASK_NAME}\"}}");
    Ok(())
}

/// Stop the running task and remove the Scheduled Task entry. Reverts to the
/// auto-spawn-via-UAC behavior on the next CLI/MCP call.
pub fn uninstall() -> Result<()> {
    eprintln!("Removing fastuse autostart Scheduled Task...");
    eprintln!("(UAC will prompt once.)");
    let script = format!(
        r#"schtasks /End /TN "{name}" >nul 2>&1 & schtasks /Delete /TN "{name}" /F"#,
        name = TASK_NAME,
    );
    run_elevated_cmd(&script).context("failed to remove Scheduled Task")?;
    eprintln!("Removed.");
    println!("{{\"ok\":true,\"installed\":false,\"task_name\":\"{TASK_NAME}\"}}");
    Ok(())
}

fn locate_daemon() -> Result<PathBuf> {
    let cli = std::env::current_exe().context("current_exe")?;
    let dir = cli
        .parent()
        .ok_or_else(|| anyhow!("current_exe has no parent"))?;
    let cand = dir.join("fastuse-daemon.exe");
    if cand.exists() {
        Ok(cand)
    } else {
        Err(anyhow!(
            "fastuse-daemon.exe not found next to fastuse-cli.exe (expected at {})",
            cand.display()
        ))
    }
}

/// Run `cmd.exe /C <script>` elevated via ShellExecuteExW("runas"). Blocks
/// until the child exits.
fn run_elevated_cmd(script: &str) -> Result<()> {
    let file_w: Vec<u16> = "cmd.exe\0".encode_utf16().collect();
    let params = format!("/C {script}\0");
    let params_w: Vec<u16> = params.encode_utf16().collect();
    let verb_w: Vec<u16> = "runas\0".encode_utf16().collect();

    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI,
        lpVerb: PCWSTR(verb_w.as_ptr()),
        lpFile: PCWSTR(file_w.as_ptr()),
        lpParameters: PCWSTR(params_w.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };

    // SAFETY: info is fully initialised; verb_w/file_w/params_w live to end of fn.
    let res = unsafe { ShellExecuteExW(&mut info) };
    if res.is_err() {
        let last = unsafe { GetLastError() };
        if last == ERROR_CANCELLED {
            return Err(anyhow!("UAC prompt declined"));
        }
        return Err(anyhow!("ShellExecuteExW failed: {res:?}"));
    }

    if !info.hProcess.is_invalid() {
        // SAFETY: hProcess returned by ShellExecuteExW with SEE_MASK_NOCLOSEPROCESS.
        unsafe {
            let _ = WaitForSingleObject(info.hProcess, INFINITE);
            let _ = CloseHandle(info.hProcess);
        }
    }
    Ok(())
}
