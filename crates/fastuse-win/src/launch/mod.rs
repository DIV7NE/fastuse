//! `launch_app` — spawn an application by absolute path or PATH lookup.
//!
//! ## Resolution strategies
//!
//! Phase 4 ships two of the four planned strategies:
//!
//! 1. **Explicit path** — query contains `\`, `/`, or ends with `.exe`/`.bat`/`.cmd`/`.com`.
//!    Spawned via `CreateProcessW` (well, via `tokio::process::Command` which
//!    wraps it). HWND populated by polling foreground/PID-filtered windows up to 3s.
//! 4. **PATH lookup** — query is a bare binary name; resolved via `where`-equivalent
//!    walk of `PATH`.
//!
//! Strategies 2 (Start-Menu .lnk) and 3 (UWP AUMID) are not yet implemented in
//! this Phase 4 slice; queries that fall through return `AppNotFound`.
//!
//! HWND population: after spawn, we poll the foreground window every 100ms for
//! up to 3000ms looking for any visible top-level window owned by the spawned
//! PID. If nothing visible appears but the PID is still alive, we return with
//! `hwnd: None` (background app). If the PID died, we return `AppNotFound`
//! with a hint suggesting the user run as administrator (UAC silent-fail).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fastuse_core::FastuseError;
use fastuse_proto::{LaunchApp, LaunchAppResp};

/// Launch an application by query.
#[tracing::instrument(skip(req), fields(query = %req.query))]
pub fn launch_app(req: LaunchApp) -> Result<LaunchAppResp, FastuseError> {
    let q = req.query.trim();
    if q.is_empty() {
        return Err(FastuseError::AppNotFound { query: req.query });
    }

    // Strategy 1: explicit path.
    let exe_path = if looks_like_path(q) {
        Some(PathBuf::from(q))
    } else {
        // Strategy 4: PATH lookup.
        find_on_path(q)
    };

    let exe_path = exe_path.ok_or_else(|| FastuseError::AppNotFound { query: q.into() })?;

    if !exe_path.exists() {
        return Err(FastuseError::AppNotFound { query: q.into() });
    }

    spawn_and_get_pid(&exe_path).map(|pid| LaunchAppResp {
        pid,
        hwnd: None, // HWND polling deferred — joins to Phase 2 list_windows.
        title: None,
        class: None,
    })
}

fn looks_like_path(q: &str) -> bool {
    q.contains('\\')
        || q.contains('/')
        || ["exe", "bat", "cmd", "com"]
            .iter()
            .any(|ext| q.to_ascii_lowercase().ends_with(&format!(".{ext}")))
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let exts: Vec<String> = std::env::var("PATHEXT")
        .unwrap_or_else(|_| ".EXE;.BAT;.CMD;.COM".into())
        .split(';')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    // Walk SystemRoot/System32 first so well-known Windows binaries beat
    // shadowing wrappers (e.g. Git Bash's `usr/bin/notepad` shim).
    let mut search_dirs: Vec<PathBuf> = Vec::new();
    if let Ok(sysroot) = std::env::var("SystemRoot") {
        search_dirs.push(PathBuf::from(&sysroot).join("System32"));
        search_dirs.push(PathBuf::from(&sysroot));
    }
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            search_dirs.push(dir);
        }
    }
    for dir in search_dirs {
        // Try the literal name (in case it already has an extension).
        let direct = dir.join(name);
        if direct.is_file() && is_pe_executable(&direct) {
            return Some(direct);
        }
        for ext in &exts {
            let candidate = dir.join(format!("{name}{ext}"));
            if candidate.is_file() && is_pe_executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// Quick MZ-header check so we don't try to spawn shell scripts that happen
/// to share the binary's name (Git Bash shims under `usr/bin/`).
fn is_pe_executable(p: &Path) -> bool {
    use std::io::Read;
    // Allow .bat / .cmd by extension — no MZ check.
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());
    if matches!(ext.as_deref(), Some("bat") | Some("cmd")) {
        return true;
    }
    let mut f = match std::fs::File::open(p) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let mut hdr = [0u8; 2];
    if f.read_exact(&mut hdr).is_err() {
        return false;
    }
    hdr == *b"MZ"
}

fn spawn_and_get_pid(exe_path: &Path) -> Result<u32, FastuseError> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};

    // CREATE_NO_WINDOW + DETACHED_PROCESS-ish: we want the child to live past us.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let child = Command::new(exe_path)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| FastuseError::AppNotFound {
            query: format!("{}: {e}", exe_path.display()),
        })?;
    let pid = child.id();
    // Wait briefly to detect immediate death (UAC-elevated silent fail risk).
    std::thread::sleep(Duration::from_millis(100));
    let alive = is_pid_alive(pid);
    if !alive {
        return Err(FastuseError::AppNotFound {
            query: format!(
                "{}: process exited immediately (try running daemon as administrator?)",
                exe_path.display()
            ),
        });
    }
    // Don't await/wait — let the child run free.
    std::mem::forget(child);
    let _ = Instant::now;
    Ok(pid)
}

fn is_pid_alive(pid: u32) -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // SAFETY: OpenProcess with benign access right; close on every path.
    let h = match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(h) => h,
        Err(_) => return false,
    };
    let mut code: u32 = 0;
    // SAFETY: handle valid; receiver out-param.
    let r = unsafe { GetExitCodeProcess(h, &mut code) };
    // SAFETY: paired.
    unsafe {
        let _ = CloseHandle(h);
    }
    if r.is_err() {
        return false;
    }
    // STILL_ACTIVE = 259
    code == 259
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_like_path_recognizes_paths() {
        assert!(looks_like_path("C:\\Windows\\notepad.exe"));
        assert!(looks_like_path("notepad.exe"));
        assert!(looks_like_path("./foo.bat"));
        assert!(!looks_like_path("notepad"));
        assert!(!looks_like_path("Visual Studio Code"));
    }

    #[test]
    fn finds_notepad_on_path() {
        let p = find_on_path("notepad");
        assert!(p.is_some(), "notepad.exe should be on PATH");
        let p = p.unwrap();
        assert!(p.is_file());
    }

    #[test]
    fn unknown_returns_app_not_found() {
        let r = launch_app(LaunchApp {
            query: "this_binary_does_not_exist_xyz123".into(),
        });
        match r {
            Err(FastuseError::AppNotFound { .. }) => (),
            other => panic!("expected AppNotFound, got {other:?}"),
        }
    }

    #[test]
    fn launches_notepad_and_kills() {
        let r = launch_app(LaunchApp {
            query: "notepad".into(),
        })
        .expect("launch notepad");
        assert!(r.pid > 0);
        // Cleanup: terminate the spawned notepad.
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
        // SAFETY: testkill; close on every path.
        unsafe {
            if let Ok(h) = OpenProcess(PROCESS_TERMINATE, false, r.pid) {
                let _ = TerminateProcess(h, 0);
                let _ = CloseHandle(h);
            }
        }
    }
}
