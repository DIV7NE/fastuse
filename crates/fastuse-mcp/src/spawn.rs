//! Auto-spawn the daemon if not reachable; connect with backoff. Duplicated
//! from fastuse-cli — Phase 2 will lift to fastuse-core if a third caller
//! appears.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::time::Duration;

use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
use tokio::time::sleep;
use windows::core::PWSTR;
use windows::Win32::System::Threading::{
    CreateProcessW, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, DETACHED_PROCESS,
    PROCESS_INFORMATION, STARTUPINFOW,
};

/// Win32 `ERROR_PIPE_BUSY`. The server exists but every instance is momentarily
/// taken — a wait-and-retry condition, not a reason to spawn a second daemon.
const ERROR_PIPE_BUSY: i32 = 231;

/// Open the pipe, retrying briefly while the server reports "all instances
/// busy". Any other error returns immediately so the caller still reaches the
/// spawn path when the daemon is genuinely absent.
async fn open_with_busy_retry(pipe_path: &str) -> std::io::Result<NamedPipeClient> {
    let mut last = match ClientOptions::new().open(pipe_path) {
        Ok(c) => return Ok(c),
        Err(e) => e,
    };
    for ms in [1u64, 2, 5, 10, 25, 50, 100, 200, 400] {
        if last.raw_os_error() != Some(ERROR_PIPE_BUSY) {
            return Err(last);
        }
        sleep(Duration::from_millis(ms)).await;
        match ClientOptions::new().open(pipe_path) {
            Ok(c) => return Ok(c),
            Err(e) => last = e,
        }
    }
    Err(last)
}

pub async fn connect_or_spawn(pipe_path: &str) -> std::io::Result<NamedPipeClient> {
    if let Ok(c) = open_with_busy_retry(pipe_path).await {
        return Ok(c);
    }
    spawn_daemon_detached()?;
    let backoffs = [50u64, 100, 200, 400, 800, 800];
    for ms in backoffs {
        sleep(Duration::from_millis(ms)).await;
        if let Ok(c) = open_with_busy_retry(pipe_path).await {
            return Ok(c);
        }
    }
    // Surface the typed protocol error as the io::Error source so the MCP
    // edge can downcast to `fastuse_proto::Error` and serialize per D-18
    // instead of leaking a substring-encoded code (WR-07).
    let err = fastuse_proto::Error::new(
        fastuse_proto::ErrorCode::DaemonSpawnFailed,
        format!("daemon pipe {pipe_path} did not appear after auto-spawn backoff"),
    );
    Err(std::io::Error::new(std::io::ErrorKind::TimedOut, err))
}

fn locate_daemon() -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    if let Some(dir) = exe.parent() {
        let cand = dir.join("fastuse-daemon.exe");
        if cand.exists() {
            return Ok(cand);
        }
    }
    Ok(PathBuf::from("fastuse-daemon.exe"))
}

fn spawn_daemon_detached() -> std::io::Result<()> {
    let path = locate_daemon()?;
    let cmdline = format!("\"{}\"", path.display());
    let mut cmd_w: Vec<u16> = OsStr::new(&cmdline)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut si = STARTUPINFOW::default();
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    let mut pi = PROCESS_INFORMATION::default();
    let creation_flags = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW;
    // SAFETY: cmd_w outlives the call; CreateProcessW writes parsed cmdline back.
    let res = unsafe {
        CreateProcessW(
            None,
            Some(PWSTR(cmd_w.as_mut_ptr())),
            None,
            None,
            false,
            creation_flags,
            None,
            None,
            &si,
            &mut pi,
        )
    };
    if res.is_err() {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: handles returned by CreateProcessW.
    unsafe {
        let _ = windows::Win32::Foundation::CloseHandle(pi.hProcess);
        let _ = windows::Win32::Foundation::CloseHandle(pi.hThread);
    }
    Ok(())
}
