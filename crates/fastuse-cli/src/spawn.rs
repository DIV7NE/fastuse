//! Auto-spawn the daemon if not reachable; connect with backoff.

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

/// Try to connect to `pipe_path`. If the pipe doesn't exist, spawn
/// `fastuse-daemon.exe` (located beside the current binary or via PATH) and
/// retry with backoff: 50ms -> 100ms -> 200ms -> 400ms -> 800ms (cap 2s).
pub async fn connect_or_spawn(pipe_path: &str) -> std::io::Result<NamedPipeClient> {
    // Fast path: try connecting once.
    if let Ok(c) = ClientOptions::new().open(pipe_path) {
        return Ok(c);
    }
    // Spawn the daemon.
    spawn_daemon_detached()?;

    // Backoff retry.
    let backoffs = [50u64, 100, 200, 400, 800, 800];
    for ms in backoffs {
        sleep(Duration::from_millis(ms)).await;
        if let Ok(c) = ClientOptions::new().open(pipe_path) {
            return Ok(c);
        }
    }
    // Surface the typed protocol error as the io::Error source so downstream
    // consumers (e.g. MCP edge serialization, test asserts) can downcast to
    // `fastuse_proto::Error` and match on `ErrorCode::DaemonSpawnFailed`
    // instead of substring-matching the message (WR-07 / D-18).
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
    // Fallback: rely on PATH.
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
    // SAFETY: cmd_w is a wide-null-terminated mutable buffer; CreateProcessW
    // writes the parsed command line back into it during invocation.
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
    // Close handles immediately — we don't supervise the child.
    // SAFETY: hProcess and hThread are returned by CreateProcessW.
    unsafe {
        let _ = windows::Win32::Foundation::CloseHandle(pi.hProcess);
        let _ = windows::Win32::Foundation::CloseHandle(pi.hThread);
    }
    Ok(())
}
