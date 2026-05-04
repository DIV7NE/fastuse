//! Per-request dispatch with Phase 4 permission gating.
//!
//! Permission tier resolution happens HERE before any tool handler runs:
//!
//! 1. Compute (tool_name, target) for the request.
//! 2. Call `perm::resolve(tool_name, target, session.allow())`.
//! 3. Blocked → `PermissionBlocked`. Confirmed → `PermissionRequired`.
//!    Free → invoke handler.
//!
//! Per-call `allow:true` in tool args is FORBIDDEN — the dispatcher does
//! NOT read any per-call allow field, even if a future wire variant
//! accidentally exposes one. (Codex review fix; closes self-grant hole.)

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use fastuse_core::perm::{self, Tier};
use fastuse_core::FastuseError;
use fastuse_proto::{
    ClipboardSet, Error, ErrorCode, ProcessSelector, Request, Response,
};
use fastuse_win::input_thread::{InputJob, InputThreadHandle};

use crate::session::Session;

/// Shared dispatcher context.
pub struct DispatchCtx {
    /// WTS console session id of the daemon.
    pub session_id: u32,
    /// Optional input-thread handle (Phase 1 plumbing).
    pub input: Option<Arc<InputThreadHandle>>,
    /// Cooperative shutdown signal.
    pub shutdown_flag: Arc<AtomicBool>,
    /// Reconciled idle timeout reported on every Welcome (D-22).
    pub idle_timeout_secs: u64,
    /// Per-connection session state (immutable allow-list, etc.).
    pub session: Session,
}

/// Outcome of dispatching a single frame.
pub struct DispatchResult {
    /// Reply to ship back over the wire.
    pub response: Response,
    /// Microseconds spent inside dispatch logic itself.
    pub daemon_dispatch_us: i64,
    /// Microseconds spent inside Win32 work (input thread / shell / clipboard).
    pub win32_work_us: i64,
}

/// Dispatch a single request.
pub async fn handle(req: Request, ctx: &DispatchCtx) -> DispatchResult {
    let start = Instant::now();
    let mut win32_us: i64 = 0;

    let response = match req {
        Request::Hello { .. } => Response::Welcome {
            daemon_version: env!("CARGO_PKG_VERSION").to_string(),
            current_idle_timeout_secs: ctx.idle_timeout_secs as u32,
        },
        Request::Ping { ts_us } => {
            if let Some(input) = ctx.input.as_ref() {
                let w_start = Instant::now();
                let _ = input.send(InputJob::Noop);
                win32_us = w_start.elapsed().as_micros() as i64;
            }
            Response::Pong {
                ts_us,
                daemon_pid: std::process::id(),
                session_id: ctx.session_id,
            }
        }
        Request::Shutdown => {
            ctx.shutdown_flag.store(true, Ordering::SeqCst);
            Response::Welcome {
                daemon_version: env!("CARGO_PKG_VERSION").to_string(),
                current_idle_timeout_secs: ctx.idle_timeout_secs as u32,
            }
        }
        // ---- Phase 4 tools (gated by perm::resolve) ----
        Request::ClipboardGet(g) => {
            let tool = match g.format {
                Some(fastuse_proto::ClipFormat::Image) => "clipboard_get_image",
                _ => "clipboard_get_text",
            };
            gate_then(tool, None, ctx, |_| {
                let w = Instant::now();
                let r = fastuse_win::clipboard::clipboard_get(g);
                (
                    r.map(Response::ClipboardGet),
                    w.elapsed().as_micros() as i64,
                )
            })
            .await
            .into_response_or_err(&mut win32_us)
        }
        Request::ClipboardSet(s) => {
            let tool = match &s {
                ClipboardSet::Text(_) => "clipboard_set_text",
                ClipboardSet::Image { .. } => "clipboard_set_image",
            };
            gate_then(tool, None, ctx, move |_| {
                let w = Instant::now();
                let r = fastuse_win::clipboard::clipboard_set(s);
                (
                    r.map(|_| Response::ClipboardSet),
                    w.elapsed().as_micros() as i64,
                )
            })
            .await
            .into_response_or_err(&mut win32_us)
        }
        Request::ShellExec(se) => {
            let tier = perm::resolve("shell_exec", None, ctx.session.allow());
            match tier {
                Tier::Blocked => err_blocked("shell_exec", "deny-list"),
                Tier::Confirmed => err_required("shell_exec"),
                Tier::Free => {
                    let w = Instant::now();
                    let r = fastuse_win::shell::shell_exec(se).await;
                    win32_us = w.elapsed().as_micros() as i64;
                    match r {
                        Ok(res) => Response::ShellExec(res),
                        Err(e) => Response::Error(e.to_wire()),
                    }
                }
            }
        }
        Request::LaunchApp(la) => {
            let target = la.query.clone();
            gate_then("launch_app", Some(&target), ctx, move |_| {
                let w = Instant::now();
                let r = fastuse_win::launch::launch_app(la);
                (
                    r.map(Response::LaunchApp),
                    w.elapsed().as_micros() as i64,
                )
            })
            .await
            .into_response_or_err(&mut win32_us)
        }
        Request::ListProcesses(lp) => {
            // list_processes is Free — no gate.
            let w = Instant::now();
            let list = fastuse_win::process::list_processes(lp.filter);
            win32_us = w.elapsed().as_micros() as i64;
            Response::ListProcesses(list)
        }
        Request::KillProcess(kp) => {
            let target = match &kp.selector {
                ProcessSelector::Name(n) => Some(n.clone()),
                ProcessSelector::Pid(_) => None,
            };
            let target_ref = target.as_deref();
            gate_then("kill_process", target_ref, ctx, move |_| {
                let w = Instant::now();
                let r = fastuse_win::process::kill_process(kp, std::process::id());
                (
                    r.map(|n| Response::KillProcess { terminated: n }),
                    w.elapsed().as_micros() as i64,
                )
            })
            .await
            .into_response_or_err(&mut win32_us)
        }
    };

    DispatchResult {
        response,
        daemon_dispatch_us: start.elapsed().as_micros() as i64,
        win32_work_us: win32_us,
    }
}

/// Result helper for the gated handlers.
struct GateOutcome {
    result: Result<Response, FastuseError>,
    win32_us: i64,
}

impl GateOutcome {
    fn into_response_or_err(self, win32_acc: &mut i64) -> Response {
        *win32_acc = self.win32_us;
        match self.result {
            Ok(r) => r,
            Err(e) => Response::Error(e.to_wire()),
        }
    }
}

/// Run permission resolution then invoke the handler closure if Free.
async fn gate_then<F>(
    tool: &'static str,
    target: Option<&str>,
    ctx: &DispatchCtx,
    f: F,
) -> GateOutcome
where
    F: FnOnce(&DispatchCtx) -> (Result<Response, FastuseError>, i64),
{
    match perm::resolve(tool, target, ctx.session.allow()) {
        Tier::Blocked => GateOutcome {
            result: Err(FastuseError::PermissionBlocked {
                tool,
                reason: "deny-list or daemon-self protection",
            }),
            win32_us: 0,
        },
        Tier::Confirmed => GateOutcome {
            result: Err(FastuseError::PermissionRequired {
                tool,
                hint: perm::hint(tool),
            }),
            win32_us: 0,
        },
        Tier::Free => {
            let (r, us) = f(ctx);
            GateOutcome {
                result: r,
                win32_us: us,
            }
        }
    }
}

fn err_required(tool: &'static str) -> Response {
    Response::Error(
        Error::new(ErrorCode::PermissionRequired, format!("{tool} requires --allow"))
            .with_hint(perm::hint(tool)),
    )
}

fn err_blocked(tool: &'static str, reason: &'static str) -> Response {
    Response::Error(
        Error::new(
            ErrorCode::PermissionBlocked,
            format!("{tool} blocked: {reason}"),
        )
        .with_hint(reason),
    )
}

/// Build a typed `Response::Error` for known error conditions.
#[allow(dead_code)]
pub fn err_response(code: ErrorCode, msg: impl Into<String>) -> Response {
    Response::Error(Error::new(code, msg))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastuse_proto::{ClipboardGet, Redact, ShellExec};

    fn ctx_with(allow: Vec<String>) -> DispatchCtx {
        DispatchCtx {
            session_id: 1,
            input: None,
            shutdown_flag: Arc::new(AtomicBool::new(false)),
            idle_timeout_secs: 300,
            session: Session::new(allow),
        }
    }

    #[tokio::test]
    async fn shell_exec_without_allow_returns_required() {
        let ctx = ctx_with(vec![]);
        let r = handle(
            Request::ShellExec(ShellExec {
                command: Redact::new("echo hi".into()),
                shell: None,
                env: None,
                cwd: None,
                timeout_ms: Some(2_000),
                stream_chunk_size: None,
            }),
            &ctx,
        )
        .await;
        match r.response {
            Response::Error(e) => {
                assert_eq!(e.code, ErrorCode::PermissionRequired);
                let hint = e.hint.expect("hint must be set");
                assert!(hint.contains("--allow=shell_exec"));
                assert!(hint.contains("FASTUSE_ALLOW=shell_exec"));
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn shell_exec_with_allow_succeeds() {
        let ctx = ctx_with(vec!["shell_exec".into()]);
        let r = handle(
            Request::ShellExec(ShellExec {
                command: Redact::new("echo hi".into()),
                shell: None,
                env: None,
                cwd: None,
                timeout_ms: Some(5_000),
                stream_chunk_size: None,
            }),
            &ctx,
        )
        .await;
        match r.response {
            Response::ShellExec(res) => assert_eq!(res.status, 0),
            Response::Error(e) => panic!("unexpected error: {e:?}"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn kill_authy_blocked_even_with_wildcard() {
        let ctx = ctx_with(vec!["*".into()]);
        let r = handle(
            Request::KillProcess(fastuse_proto::KillProcess {
                selector: ProcessSelector::Name("Authy".into()),
                force: None,
                process_tree: None,
            }),
            &ctx,
        )
        .await;
        match r.response {
            Response::Error(e) => assert_eq!(e.code, ErrorCode::PermissionBlocked),
            other => panic!("expected blocked, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn launch_authy_blocked() {
        let ctx = ctx_with(vec!["launch_app".into()]);
        let r = handle(
            Request::LaunchApp(fastuse_proto::LaunchApp {
                query: "Authy".into(),
            }),
            &ctx,
        )
        .await;
        match r.response {
            Response::Error(e) => assert_eq!(e.code, ErrorCode::PermissionBlocked),
            other => panic!("expected blocked, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn clipboard_get_text_is_free() {
        let ctx = ctx_with(vec![]);
        let r = handle(
            Request::ClipboardGet(ClipboardGet {
                format: Some(fastuse_proto::ClipFormat::Text),
            }),
            &ctx,
        )
        .await;
        // Either succeeds (clipboard had something) or returns ClipboardGet(None).
        // Critical: NOT a permission error.
        match r.response {
            Response::ClipboardGet(_) => (),
            Response::Error(e) => panic!(
                "clipboard_get_text should be Free; got {} {}",
                e.code.as_str(),
                e.message
            ),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn clipboard_get_image_requires_allow() {
        let ctx = ctx_with(vec![]);
        let r = handle(
            Request::ClipboardGet(ClipboardGet {
                format: Some(fastuse_proto::ClipFormat::Image),
            }),
            &ctx,
        )
        .await;
        match r.response {
            Response::Error(e) => assert_eq!(e.code, ErrorCode::PermissionRequired),
            other => panic!("expected PermissionRequired, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn list_processes_is_free() {
        let ctx = ctx_with(vec![]);
        let r = handle(
            Request::ListProcesses(fastuse_proto::ListProcesses { filter: None }),
            &ctx,
        )
        .await;
        match r.response {
            Response::ListProcesses(_) => (),
            other => panic!("expected ListProcesses, got {other:?}"),
        }
    }
}
