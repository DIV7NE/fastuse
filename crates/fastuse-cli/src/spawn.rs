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
/// Recovery order (every call is idempotent):
///   1. Fast-path open. Healthy daemon -> done.
///   2. Pre-spawn dead-sentinel sweep. If `daemon.pid` exists and the recorded
///      PID is either no longer alive or not a fastuse-daemon process, delete
///      the sentinel silently so we don't trigger UAC on a phantom holder.
///   3. Spawn + backoff (UAC prompt). Most-common path on a cold machine.
///   4. Zombie eviction. If the spawn round failed (typically because the
///      singleton mutex is held by a hung pre-fix binary or a daemon that
///      crashed before opening its pipe), force-kill the recorded PID and
///      retry the spawn once.
///
/// On final failure, the typed `DaemonSpawnFailed` error carries a hint
/// listing what was tried so callers can tell the difference between "UAC
/// declined", "stale sentinel cleared but spawn still failed", and "zombie
/// killed but spawn still failed".
pub async fn connect_or_spawn(pipe_path: &str) -> std::io::Result<NamedPipeClient> {
    // Fast path.
    if let Ok(c) = ClientOptions::new().open(pipe_path) {
        return Ok(c);
    }

    // Pre-spawn dead-sentinel sweep. Idempotent no-op when the sentinel is
    // healthy (alive + fastuse-daemon). Tracks whether we cleared anything
    // for the final error hint.
    let stale_sentinel_evicted = try_evict_dead_sentinel();

    // Spawn + backoff. UAC fires here on first call after reboot.
    if let Some(c) = try_spawn_and_connect(pipe_path).await {
        return Ok(c);
    }

    // Spawn round failed. Suspect a zombie holding the singleton mutex.
    let zombie_killed = evict_live_zombie();
    if zombie_killed {
        if let Some(c) = try_spawn_and_connect(pipe_path).await {
            return Ok(c);
        }
    }

    // Surface the typed protocol error as the io::Error source so downstream
    // consumers (e.g. MCP edge serialization, test asserts) can downcast to
    // `fastuse_proto::Error` and match on `ErrorCode::DaemonSpawnFailed`
    // instead of substring-matching the message (WR-07 / D-18).
    let last_pipe_err = ClientOptions::new()
        .open(pipe_path)
        .err()
        .map(|e| e.to_string())
        .unwrap_or_else(|| "pipe never appeared".to_string());
    let err = fastuse_proto::Error::new(
        fastuse_proto::ErrorCode::DaemonSpawnFailed,
        format!("daemon pipe {pipe_path} did not appear after auto-spawn backoff"),
    )
    .with_hint(format!(
        "stale_sentinel_evicted={stale_sentinel_evicted} zombie_killed={zombie_killed} \
         last_pipe_error={last_pipe_err}"
    ));
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

/// Pre-spawn sweep: read the sentinel, and if the recorded PID is not a
/// live fastuse-daemon, delete the file. Never calls taskkill — this path
/// is for *dead* PIDs only, so there is nothing to kill. Returns `true` if
/// we removed a stale file.
fn try_evict_dead_sentinel() -> bool {
    let Some(path) = local_app_data_join("daemon.pid") else { return false };
    let Ok(contents) = std::fs::read_to_string(&path) else { return false };
    let pid = contents
        .lines()
        .find_map(|l| l.strip_prefix("pid=").and_then(|s| s.trim().parse::<u32>().ok()));
    let dead = match pid {
        None => true,
        Some(0) => true,
        Some(p) if p == std::process::id() => true,
        Some(p) => !pid_is_fastuse_daemon(p),
    };
    if dead {
        std::fs::remove_file(&path).is_ok()
    } else {
        false
    }
}

/// Post-spawn-failure path: the sentinel points at a live fastuse-daemon
/// that nevertheless isn't servicing the pipe (zombie / hung startup).
/// Force-kill it and clear the sentinel so the next spawn round succeeds.
/// Returns `true` if a zombie was killed.
fn evict_live_zombie() -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Serialize tests that mutate the real %LOCALAPPDATA%\fastuse\daemon.pid —
    // the file is a process-global resource on this user's machine.
    static SENTINEL_LOCK: Mutex<()> = Mutex::new(());

    /// Writing a sentinel pointing at a PID that is not a fastuse-daemon
    /// process must cause `try_evict_dead_sentinel` to delete the file and
    /// return true.
    #[test]
    fn try_evict_dead_sentinel_removes_dead_pid() {
        let _g = SENTINEL_LOCK.lock().unwrap();
        let Some(path) = local_app_data_join("daemon.pid") else {
            // Skip on machines without %LOCALAPPDATA% (CI containers, etc.).
            return;
        };
        // Back up an existing real sentinel so we don't clobber a running daemon.
        let backup = std::fs::read(&path).ok();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // PID 999999 is virtually guaranteed not to exist; even if it did, it
        // would not be fastuse-daemon.exe.
        std::fs::write(&path, "pid=999999\n").unwrap();
        let evicted = try_evict_dead_sentinel();
        assert!(evicted, "expected dead sentinel to be evicted");
        assert!(!path.exists(), "expected sentinel file to be removed");
        if let Some(bytes) = backup {
            let _ = std::fs::write(&path, bytes);
        }
    }

    /// Empty / unparseable sentinel must also be treated as dead and removed.
    #[test]
    fn try_evict_dead_sentinel_removes_unparseable() {
        let _g = SENTINEL_LOCK.lock().unwrap();
        let Some(path) = local_app_data_join("daemon.pid") else { return };
        let backup = std::fs::read(&path).ok();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&path, "garbage\n").unwrap();
        let evicted = try_evict_dead_sentinel();
        assert!(evicted);
        assert!(!path.exists());
        if let Some(bytes) = backup {
            let _ = std::fs::write(&path, bytes);
        }
    }
}
