//! `shell_exec` runner.

use std::process::Stdio;
use std::time::{Duration, Instant};

use fastuse_core::shell::quoting::{build_argv, Shell};
use fastuse_core::FastuseError;
use fastuse_proto::{Redact, ShellExec, ShellExecResult, ShellKind};
use tokio::io::{AsyncReadExt, BufReader};
use tokio::process::Command;

use super::cap::OutputCap;

/// Default timeout: 30s.
pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;
/// Combined stdout+stderr cap: 1 MiB.
pub const OUTPUT_CAP_BYTES: usize = 1024 * 1024;
/// Default streaming chunk size: 4 KiB.
pub const DEFAULT_CHUNK_SIZE: u32 = 4096;

/// Windows-only spawn flag: don't open a console window for the child.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn shellkind_to_quoting(s: ShellKind) -> Shell {
    match s {
        ShellKind::Cmd => Shell::Cmd,
        ShellKind::Powershell => Shell::Powershell,
        ShellKind::Pwsh => Shell::Pwsh,
        ShellKind::Bash => Shell::Bash,
    }
}

/// Run a shell command and return aggregated output. Output is capped at
/// [`OUTPUT_CAP_BYTES`] combined; on cap-hit the result has `truncated:true`.
/// Timeout defaults to 30s; on timeout the child is killed (kill_on_drop)
/// and `FastuseError::ShellTimeout` is returned.
///
/// Streaming chunks are NOT emitted by this function — the daemon-level MCP
/// streaming surface wraps this. Phase 4 ships the aggregate-result form
/// over the request/response wire; chunking can be layered on later.
#[tracing::instrument(skip(req), fields(shell, cmd_bytes = req.command.as_inner().len(), timeout_ms))]
pub async fn shell_exec(req: ShellExec) -> Result<ShellExecResult, FastuseError> {
    let kind = req.shell.unwrap_or(ShellKind::Cmd);
    tracing::Span::current().record("shell", tracing::field::display(format!("{kind:?}")));
    let timeout_ms = req.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
    tracing::Span::current().record("timeout_ms", timeout_ms);

    let argv = build_argv(shellkind_to_quoting(kind), req.command.as_inner());
    if argv.is_empty() {
        return Err(FastuseError::Internal("empty argv".into()));
    }

    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .kill_on_drop(true);

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    if let Some(cwd) = req.cwd.as_ref() {
        cmd.current_dir(cwd);
    }
    if let Some(env) = req.env.as_ref() {
        for (k, v) in env {
            cmd.env(k, v.as_inner());
        }
    }

    let started = Instant::now();
    let mut child = cmd
        .spawn()
        .map_err(|e| FastuseError::Io(format!("spawn: {e}")))?;

    let mut stdout = BufReader::new(child.stdout.take().expect("stdout piped"));
    let mut stderr = BufReader::new(child.stderr.take().expect("stderr piped"));

    let mut stdout_buf: Vec<u8> = Vec::new();
    let mut stderr_buf: Vec<u8> = Vec::new();
    let mut cap = OutputCap::new(OUTPUT_CAP_BYTES);

    let chunk_size = req.stream_chunk_size.unwrap_or(DEFAULT_CHUNK_SIZE) as usize;
    let mut so_chunk = vec![0u8; chunk_size];
    let mut se_chunk = vec![0u8; chunk_size];

    let deadline = tokio::time::sleep(Duration::from_millis(timeout_ms));
    tokio::pin!(deadline);

    let status = loop {
        tokio::select! {
            biased;
            // Timeout — drop child (kill_on_drop fires).
            _ = &mut deadline => {
                drop(stdout);
                drop(stderr);
                drop(child);
                return Err(FastuseError::ShellTimeout { ms: timeout_ms });
            }
            // stdout drain.
            r = stdout.read(&mut so_chunk) => {
                match r {
                    Ok(0) => {
                        // stdout closed — keep draining stderr until child exits.
                        // Fall through to stderr-only loop.
                        break drain_remainder(&mut child, &mut stderr, &mut stderr_buf, &mut cap, &mut se_chunk).await?;
                    }
                    Ok(n) => {
                        let take = cap.admit(n);
                        if take > 0 { stdout_buf.extend_from_slice(&so_chunk[..take]); }
                    }
                    Err(e) => return Err(FastuseError::Io(format!("stdout: {e}"))),
                }
            }
            r = stderr.read(&mut se_chunk) => {
                match r {
                    Ok(0) => {
                        break drain_remainder_stdout(&mut child, &mut stdout, &mut stdout_buf, &mut cap, &mut so_chunk).await?;
                    }
                    Ok(n) => {
                        let take = cap.admit(n);
                        if take > 0 { stderr_buf.extend_from_slice(&se_chunk[..take]); }
                    }
                    Err(e) => return Err(FastuseError::Io(format!("stderr: {e}"))),
                }
            }
            // Child exited and pipes might still have buffered data.
            ws = child.wait() => {
                let s = ws.map_err(|e| FastuseError::Io(format!("wait: {e}")))?;
                // Drain remaining bytes from both pipes.
                let _ = drain_pipe(&mut stdout, &mut stdout_buf, &mut cap, &mut so_chunk).await;
                let _ = drain_pipe(&mut stderr, &mut stderr_buf, &mut cap, &mut se_chunk).await;
                break s;
            }
        }
    };

    let duration_ms = started.elapsed().as_millis() as u64;
    let exit = status.code().unwrap_or(-1);
    tracing::info!(
        exit,
        truncated = cap.truncated(),
        stdout_bytes = stdout_buf.len(),
        stderr_bytes = stderr_buf.len(),
        duration_ms,
        "shell_exec done"
    );
    Ok(ShellExecResult {
        stdout: Redact::new(stdout_buf),
        stderr: Redact::new(stderr_buf),
        status: exit,
        truncated: cap.truncated(),
        duration_ms,
    })
}

async fn drain_remainder(
    child: &mut tokio::process::Child,
    stderr: &mut BufReader<tokio::process::ChildStderr>,
    stderr_buf: &mut Vec<u8>,
    cap: &mut OutputCap,
    se_chunk: &mut [u8],
) -> Result<std::process::ExitStatus, FastuseError> {
    loop {
        tokio::select! {
            r = stderr.read(se_chunk) => {
                match r {
                    Ok(0) => {}
                    Ok(n) => {
                        let take = cap.admit(n);
                        if take > 0 { stderr_buf.extend_from_slice(&se_chunk[..take]); }
                        continue;
                    }
                    Err(e) => return Err(FastuseError::Io(format!("stderr: {e}"))),
                }
            }
            ws = child.wait() => {
                let _ = drain_pipe(stderr, stderr_buf, cap, se_chunk).await;
                return ws.map_err(|e| FastuseError::Io(format!("wait: {e}")));
            }
        }
        // stderr returned 0 and we are not done — break and wait for exit.
        let s = child.wait().await.map_err(|e| FastuseError::Io(format!("wait: {e}")))?;
        return Ok(s);
    }
}

async fn drain_remainder_stdout(
    child: &mut tokio::process::Child,
    stdout: &mut BufReader<tokio::process::ChildStdout>,
    stdout_buf: &mut Vec<u8>,
    cap: &mut OutputCap,
    so_chunk: &mut [u8],
) -> Result<std::process::ExitStatus, FastuseError> {
    loop {
        tokio::select! {
            r = stdout.read(so_chunk) => {
                match r {
                    Ok(0) => {}
                    Ok(n) => {
                        let take = cap.admit(n);
                        if take > 0 { stdout_buf.extend_from_slice(&so_chunk[..take]); }
                        continue;
                    }
                    Err(e) => return Err(FastuseError::Io(format!("stdout: {e}"))),
                }
            }
            ws = child.wait() => {
                let _ = drain_pipe(stdout, stdout_buf, cap, so_chunk).await;
                return ws.map_err(|e| FastuseError::Io(format!("wait: {e}")));
            }
        }
        let s = child.wait().await.map_err(|e| FastuseError::Io(format!("wait: {e}")))?;
        return Ok(s);
    }
}

async fn drain_pipe<R: tokio::io::AsyncRead + Unpin>(
    pipe: &mut R,
    buf: &mut Vec<u8>,
    cap: &mut OutputCap,
    chunk: &mut [u8],
) -> std::io::Result<()> {
    loop {
        match pipe.read(chunk).await {
            Ok(0) => return Ok(()),
            Ok(n) => {
                let take = cap.admit(n);
                if take > 0 {
                    buf.extend_from_slice(&chunk[..take]);
                }
            }
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(cmd: &str, kind: ShellKind) -> ShellExec {
        ShellExec {
            command: Redact::new(cmd.into()),
            shell: Some(kind),
            env: None,
            cwd: None,
            timeout_ms: Some(5_000),
            stream_chunk_size: None,
        }
    }

    #[tokio::test]
    async fn cmd_echo_works() {
        let r = shell_exec(req("echo hello", ShellKind::Cmd)).await.unwrap();
        assert_eq!(r.status, 0);
        let so = String::from_utf8_lossy(r.stdout.as_inner());
        assert!(so.contains("hello"), "stdout: {so:?}");
        assert!(!r.truncated);
    }

    #[tokio::test]
    async fn timeout_kills_child() {
        // ping -n 60 127.0.0.1 sleeps ~60s; timeout at 500ms.
        let mut req = req("ping -n 60 127.0.0.1 >nul", ShellKind::Cmd);
        req.timeout_ms = Some(500);
        let r = shell_exec(req).await;
        match r {
            Err(FastuseError::ShellTimeout { ms }) => assert_eq!(ms, 500),
            other => panic!("expected ShellTimeout, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn nonzero_exit_propagates() {
        let r = shell_exec(req("exit 7", ShellKind::Cmd)).await.unwrap();
        assert_eq!(r.status, 7);
    }

    #[tokio::test]
    async fn cwd_is_honored() {
        let mut r = req("cd", ShellKind::Cmd);
        r.cwd = Some("C:\\".into());
        let res = shell_exec(r).await.unwrap();
        let so = String::from_utf8_lossy(res.stdout.as_inner());
        assert!(so.trim().to_ascii_uppercase().starts_with("C:"), "got {so:?}");
    }

    #[tokio::test]
    async fn env_is_honored() {
        let mut r = req("echo %FOO%", ShellKind::Cmd);
        r.env = Some(vec![("FOO".into(), Redact::new("bar".into()))]);
        let res = shell_exec(r).await.unwrap();
        let so = String::from_utf8_lossy(res.stdout.as_inner());
        assert!(so.contains("bar"), "got {so:?}");
    }
}
