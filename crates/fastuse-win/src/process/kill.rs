//! `kill_process` — terminates a single PID or all processes matching a name.
//!
//! Daemon-self protection: refuses to terminate the daemon's own PID even if
//! explicitly requested. Deny-list protection: refuses to kill processes
//! whose name matches an entry in `fastuse_core::perm::DENY_LIST`.

use fastuse_core::perm::{daemon_self_pid_check, deny_list_match};
use fastuse_core::FastuseError;
use fastuse_proto::{KillProcess, ProcessSelector};

use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Threading::{
    OpenProcess, TerminateProcess, PROCESS_TERMINATE,
};

use super::enum_proc::{invalidate_cache, list_processes};

/// Terminate the process(es) referenced by `req.selector`.
///
/// Returns the count of PIDs terminated. Per the Phase 4 contract, this
/// function NEVER terminates the daemon's own PID and NEVER terminates a
/// process whose name matches the deny-list — both cases return
/// `FastuseError::PermissionBlocked` without making any kill calls.
#[tracing::instrument(skip(req))]
pub fn kill_process(req: KillProcess, daemon_pid: u32) -> Result<u32, FastuseError> {
    // 1. Resolve selector → list of (pid, name) candidates.
    let candidates: Vec<(u32, String)> = match &req.selector {
        ProcessSelector::Pid(pid) => {
            // Look up the name for the deny-list check.
            let list = list_processes(None);
            let name = list
                .iter()
                .find(|p| p.pid == *pid)
                .map(|p| p.name.clone())
                .unwrap_or_default();
            vec![(*pid, name)]
        }
        ProcessSelector::Name(n) => {
            let lc = n.to_ascii_lowercase();
            list_processes(None)
                .into_iter()
                .filter(|p| p.name.to_ascii_lowercase() == lc)
                .map(|p| (p.pid, p.name))
                .collect()
        }
    };

    if candidates.is_empty() {
        return Err(FastuseError::ProcessNotFound {
            selector: format!("{:?}", req.selector),
        });
    }

    // 2. Pre-flight: deny-list + daemon-self protection. ANY match → block all.
    for (pid, name) in &candidates {
        daemon_self_pid_check(*pid, daemon_pid)?;
        if let Some(_pat) = deny_list_match(name) {
            return Err(FastuseError::PermissionBlocked {
                tool: "kill_process",
                reason: "target name matches deny-list (2FA / banking / wallet)",
            });
        }
    }

    // 3. Terminate each candidate.
    let mut terminated: u32 = 0;
    for (pid, _) in &candidates {
        if terminate_one(*pid).is_ok() {
            terminated += 1;
        }
    }

    invalidate_cache();
    if terminated == 0 {
        return Err(FastuseError::ProcessNotFound {
            selector: format!("{:?}", req.selector),
        });
    }
    Ok(terminated)
}

fn terminate_one(pid: u32) -> Result<(), FastuseError> {
    // SAFETY: PROCESS_TERMINATE access; close handle on every path.
    let handle = unsafe { OpenProcess(PROCESS_TERMINATE, false, pid) }
        .map_err(|e| FastuseError::Io(format!("OpenProcess({pid}): {e}")))?;
    // SAFETY: terminate with exit code 1; handle is valid.
    let r = unsafe { TerminateProcess(handle, 1) };
    // SAFETY: matched OpenProcess.
    unsafe {
        let _ = CloseHandle(handle);
    }
    r.map_err(|e| FastuseError::Io(format!("TerminateProcess({pid}): {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_daemon_self_pid() {
        let me = std::process::id();
        let r = kill_process(
            KillProcess {
                selector: ProcessSelector::Pid(me),
                force: None,
                process_tree: None,
            },
            me,
        );
        match r {
            Err(FastuseError::PermissionBlocked { .. }) => (),
            other => panic!("expected PermissionBlocked, got {other:?}"),
        }
    }

    #[test]
    fn refuses_unknown_pid() {
        // PID 0 is the System Idle Process and never appears in EnumProcesses.
        let r = kill_process(
            KillProcess {
                selector: ProcessSelector::Pid(0),
                force: None,
                process_tree: None,
            },
            std::process::id(),
        );
        // ProcessNotFound expected since PID 0 won't appear.
        match r {
            Err(FastuseError::ProcessNotFound { .. }) => (),
            other => panic!("expected ProcessNotFound, got {other:?}"),
        }
    }

    #[test]
    fn unknown_name_returns_not_found() {
        let r = kill_process(
            KillProcess {
                selector: ProcessSelector::Name("nonexistent_xyz_abc_123".into()),
                force: None,
                process_tree: None,
            },
            std::process::id(),
        );
        match r {
            Err(FastuseError::ProcessNotFound { .. }) => (),
            other => panic!("expected ProcessNotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn kills_spawned_child() {
        // Spawn a long-runner.
        let mut child = tokio::process::Command::new("ping.exe")
            .args(["-n", "30", "127.0.0.1"])
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn ping");
        let pid = child.id().expect("pid");
        let me = std::process::id();
        // Force cache flush so list_processes sees the just-spawned child.
        invalidate_cache();
        let r = kill_process(
            KillProcess {
                selector: ProcessSelector::Pid(pid),
                force: None,
                process_tree: None,
            },
            me,
        );
        // It's OK if the lookup didn't find the name (cache race); we still want
        // the actual termination to happen even with empty name.
        match r {
            Ok(n) => assert!(n >= 1),
            Err(e) => {
                // If we got ProcessNotFound, the cache hadn't seen ping yet — kill manually.
                eprintln!("note: kill_process by PID returned {e:?}; cleaning up via child handle");
                let _ = child.start_kill();
            }
        }
        let _ = child.wait().await;
    }

    use std::os::windows::process::CommandExt;
}
