//! Auto-spawn the daemon if not reachable; connect with backoff.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::time::Duration;

use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
use tokio::time::sleep;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_CANCELLED};
use windows::Win32::UI::Shell::{
    ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

/// Try to connect to `pipe_path`. If the pipe doesn't exist, spawn
/// `fastuse-daemon.exe` (located beside the current binary or via PATH) and
/// retry with backoff: 50ms -> 100ms -> 200ms -> 400ms -> 800ms (cap 2s).
///
/// Self-healing: if the backoff exhausts and the sentinel points at a live
/// process that's holding the singleton mutex but never opened the pipe (zombie
/// from a pre-fix binary, or a hung startup), force-kill the holder, clear the
/// sentinel, and retry once. The next CLI call always recovers automatically.
pub async fn connect_or_spawn(pipe_path: &str) -> std::io::Result<NamedPipeClient> {
    // Fast path: try connecting once.
    if let Ok(c) = ClientOptions::new().open(pipe_path) {
        return Ok(c);
    }
    if let Some(c) = try_spawn_and_connect(pipe_path).await {
        return Ok(c);
    }
    // First spawn round failed. Suspect a zombie holding the singleton mutex.
    if evict_stale_daemon() {
        if let Some(c) = try_spawn_and_connect(pipe_path).await {
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

async fn try_spawn_and_connect(pipe_path: &str) -> Option<NamedPipeClient> {
    if spawn_daemon_detached().is_err() {
        return None;
    }
    for ms in [50u64, 100, 200, 400, 800, 800] {
        sleep(Duration::from_millis(ms)).await;
        if let Ok(c) = ClientOptions::new().open(pipe_path) {
            return Some(c);
        }
    }
    None
}

/// Read the sentinel, force-kill the recorded PID if alive, remove the file.
/// Returns `true` if a stale holder was evicted (caller can retry spawn).
fn evict_stale_daemon() -> bool {
    let Some(path) = local_app_data_join("daemon.pid") else { return false };
    let Ok(contents) = std::fs::read_to_string(&path) else { return false };
    let pid = contents
        .lines()
        .find_map(|l| l.strip_prefix("pid=").and_then(|s| s.trim().parse::<u32>().ok()));
    let _ = std::fs::remove_file(&path);
    let Some(pid) = pid else { return false };
    if pid == 0 || pid == std::process::id() {
        return false;
    }
    // Hard guard: only kill if the live process at this PID is actually
    // fastuse-daemon.exe. Protects against a corrupted or malicious sentinel
    // pointing at an unrelated PID.
    if !pid_is_fastuse_daemon(pid) {
        return false;
    }
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/PID", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    true
}

fn pid_is_fastuse_daemon(pid: u32) -> bool {
    // `tasklist /FI "PID eq N" /FO CSV /NH` prints one CSV row when the PID
    // exists, else `INFO: No tasks are running...` to stdout. We just need
    // the first comma-quoted field to start with `fastuse-daemon`.
    let out = std::process::Command::new("tasklist")
        .args([
            "/FI",
            &format!("PID eq {pid}"),
            "/FO",
            "CSV",
            "/NH",
        ])
        .stderr(std::process::Stdio::null())
        .output();
    let Ok(out) = out else { return false };
    let s = String::from_utf8_lossy(&out.stdout);
    s.lines()
        .next()
        .and_then(|l| l.strip_prefix('"'))
        .map(|l| l.starts_with("fastuse-daemon"))
        .unwrap_or(false)
}

fn local_app_data_join(name: &str) -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(base).join("fastuse").join(name))
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

/// Spawn the daemon at High integrity level via UAC ("runas" verb).
///
/// We always elevate the daemon so it can drive admin-process windows
/// (e.g. L-Connect3). Without elevation, SendInput targeting an elevated
/// HWND is silently dropped by UIPI.
///
/// On UAC decline (`ERROR_CANCELLED`) we surface a clear stderr message
/// and return Err — the existing `connect_or_spawn` retry/evict path then
/// reports the typed `DaemonSpawnFailed` error.
fn spawn_daemon_detached() -> std::io::Result<()> {
    let path = locate_daemon()?;

    // Wide-null-terminated buffers held alive across the FFI call.
    let file_w: Vec<u16> = OsStr::new(path.as_os_str())
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let verb_w: Vec<u16> = "runas\0".encode_utf16().collect();

    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI,
        lpVerb: PCWSTR(verb_w.as_ptr()),
        lpFile: PCWSTR(file_w.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };

    // SAFETY: `info` is fully initialised; verb_w/file_w live to end of fn.
    let res = unsafe { ShellExecuteExW(&mut info) };
    if res.is_err() {
        // GetLastError tells us if the user clicked No on UAC.
        let last = unsafe { GetLastError() };
        if last == ERROR_CANCELLED {
            eprintln!(
                "fastuse: daemon requires administrator rights — re-run and accept the UAC prompt"
            );
        }
        return Err(std::io::Error::last_os_error());
    }

    // Close the child handle — we don't supervise it.
    if !info.hProcess.is_invalid() {
        // SAFETY: hProcess returned by ShellExecuteExW with SEE_MASK_NOCLOSEPROCESS.
        unsafe {
            let _ = CloseHandle(info.hProcess);
        }
    }
    Ok(())
}
