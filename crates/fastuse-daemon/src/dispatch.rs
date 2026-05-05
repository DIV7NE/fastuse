//! Per-request dispatch with Phase 4 permission gating.
//!
//! Phase 1 handled `Hello`/`Ping`/`Shutdown`. Phase 2 extends with every
//! input + window/monitor primitive. Each Phase 2 arm:
//!
//!   1. Builds a closure capturing args.
//!   2. Routes to the input thread via `InputThreadHandle::run` (D-26).
//!   3. Maps the handler's `Result<T, Error>` into a `Response::*` /
//!      `Response::Error`.
//!
//! `Wait { duration_ms }` is special: handled directly with
//! `std::thread::sleep` so it doesn't occupy the input-thread slot.
//!
//! Phase 4 adds permission tier resolution before any tool handler runs:
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
    coords::WindowInfo as ProtoWindowInfo, ClipboardSet, Error, ErrorCode, MonitorInfo, MouseButton,
    ProcessSelector, Request, Response, ScrollDirection,
};
use fastuse_win::capture::{handle_screenshot, handle_screenshot_region};
use fastuse_win::capture_thread::CaptureThreadHandle;
use fastuse_win::input::handlers as ih;
use fastuse_win::input_thread::{InputJob, InputThreadHandle};
use fastuse_win::uia::{
    handle_click_element, handle_inspect_at_point, handle_scroll_into_view, handle_type_into_element,
    handle_uia_query, handle_uia_tree, handle_wait_for_element,
};
use fastuse_win::uia_pool::UiaPoolHandle;
use fastuse_win::window::{
    cursor_position::cursor_position, focus::focus_window, foreground::foreground_window,
    list_windows::list_windows, monitors::list_monitors, move_resize::resize_move_window,
};

use crate::session::Session;

/// Shared dispatcher context.
pub struct DispatchCtx {
    /// WTS console session id of the daemon.
    pub session_id: u32,
    /// Optional input-thread handle (Phase 1 plumbing).
    pub input: Option<Arc<InputThreadHandle>>,
    /// Optional UIA pool handle (Phase 3 perception).
    pub uia: Option<Arc<UiaPoolHandle>>,
    /// Optional capture thread handle (Phase 3 perception).
    pub capture: Option<Arc<CaptureThreadHandle>>,
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
        // -------- Phase 1 --------
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
        Request::Warmup => {
            let w_start = Instant::now();
            let resp = crate::warmup::run(
                ctx.uia.as_ref().map(|a| &**a),
                ctx.capture.as_ref().map(|a| &**a),
            );
            win32_us = w_start.elapsed().as_micros() as i64;
            resp
        }

        // -------- Phase 2: input --------
        Request::Click {
            x,
            y,
            button,
            count,
            modifiers,
            skip_set_cursor_pos,
            opts,
        } => {
            let inner = run_unit(ctx, &mut win32_us, move || {
                ih::click(x, y, button, count, &modifiers, skip_set_cursor_pos)
            });
            finalize(inner, opts, ctx)
        }
        Request::MouseMove { x, y, opts } => {
            let inner = run_unit(ctx, &mut win32_us, move || ih::mouse_move(x, y));
            finalize(inner, opts, ctx)
        }
        Request::MouseDown { button, opts } => {
            let inner = run_unit(ctx, &mut win32_us, move || ih::mouse_down(button));
            finalize(inner, opts, ctx)
        }
        Request::MouseUp { button, opts } => {
            let inner = run_unit(ctx, &mut win32_us, move || ih::mouse_up(button));
            finalize(inner, opts, ctx)
        }
        Request::Drag {
            start_x,
            start_y,
            end_x,
            end_y,
            button,
            modifiers,
            opts,
        } => {
            let inner = run_unit(ctx, &mut win32_us, move || {
                ih::drag(start_x, start_y, end_x, end_y, button, &modifiers)
            });
            finalize(inner, opts, ctx)
        }
        Request::Scroll {
            x,
            y,
            direction,
            amount,
            modifiers,
            opts,
        } => {
            let inner = run_unit(ctx, &mut win32_us, move || {
                ih::scroll(x, y, direction, amount, &modifiers)
            });
            finalize(inner, opts, ctx)
        }
        Request::Type { text, opts } => {
            // Expose the redacted payload only inside this scope; never log it.
            let payload = text.into_inner();
            tracing::debug!(text_len = payload.len(), "type tool dispatch");
            let inner = run_unit(ctx, &mut win32_us, move || ih::type_text(&payload));
            finalize(inner, opts, ctx)
        }
        Request::Key { chord, repeat, opts } => {
            let inner = run_unit(ctx, &mut win32_us, move || ih::key(&chord, repeat));
            finalize(inner, opts, ctx)
        }
        Request::HoldKey { chord, duration_ms, opts } => {
            let inner = run_unit(ctx, &mut win32_us, move || ih::hold_key(&chord, duration_ms));
            finalize(inner, opts, ctx)
        }
        Request::Wait { duration_ms } => {
            // Handled directly on the dispatch thread (NOT the input thread)
            // so concurrent input calls aren't blocked by sleeps.
            let w_start = Instant::now();
            std::thread::sleep(std::time::Duration::from_millis(duration_ms as u64));
            let slept = w_start.elapsed().as_micros() as u64;
            Response::Ack { slept_us: Some(slept) }
        }

        // -------- Phase 2: window/monitor --------
        Request::ListMonitors => run_value::<Vec<MonitorInfo>, _>(ctx, &mut win32_us, list_monitors)
            .map_or_else(Response::Error, Response::Monitors),
        Request::CursorPosition => match run_value::<(i32, i32, u64), _>(ctx, &mut win32_us, cursor_position) {
            Ok((x, y, monitor_id)) => Response::CursorPos { x, y, monitor_id },
            Err(e) => Response::Error(e),
        },
        Request::ForegroundWindow => run_value::<ProtoWindowInfo, _>(ctx, &mut win32_us, foreground_window)
            .map_or_else(Response::Error, Response::Window),
        Request::ListWindows {
            process_name,
            title_substring,
            visible_only,
        } => run_value::<Vec<ProtoWindowInfo>, _>(ctx, &mut win32_us, move || {
            list_windows(process_name.as_deref(), title_substring.as_deref(), visible_only)
        })
        .map_or_else(Response::Error, Response::Windows),
        Request::FocusWindow { hwnd, opts } => {
            let inner = run_unit(ctx, &mut win32_us, move || focus_window(hwnd));
            finalize(inner, opts, ctx)
        }
        Request::ResizeMoveWindow { hwnd, x, y, w, h, opts } => {
            let inner = run_unit(ctx, &mut win32_us, move || resize_move_window(hwnd, x, y, w, h));
            finalize(inner, opts, ctx)
        }

        // -------- Phase 4: system surface (gated by perm::resolve) --------
        // (Phase 3 perception arms appended after Phase 4 below.)
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
        Request::ClipboardSet { req: s, opts } => {
            let tool = match &s {
                ClipboardSet::Text(_) => "clipboard_set_text",
                ClipboardSet::Image { .. } => "clipboard_set_image",
            };
            let inner = gate_then(tool, None, ctx, move |_| {
                let w = Instant::now();
                let r = fastuse_win::clipboard::clipboard_set(s);
                (
                    r.map(|_| Response::ClipboardSet),
                    w.elapsed().as_micros() as i64,
                )
            })
            .await
            .into_response_or_err(&mut win32_us);
            finalize(inner, opts, ctx)
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
        Request::LaunchApp { req: la, opts } => {
            let target = la.query.clone();
            let inner = gate_then("launch_app", Some(&target), ctx, move |_| {
                let w = Instant::now();
                let r = fastuse_win::launch::launch_app(la);
                (
                    r.map(Response::LaunchApp),
                    w.elapsed().as_micros() as i64,
                )
            })
            .await
            .into_response_or_err(&mut win32_us);
            finalize(inner, opts, ctx)
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

        // -------- Phase 3: capture (CAP-01..06) --------
        Request::Screenshot { monitor, format } => match ctx.capture.as_ref() {
            None => Response::Error(Error::new(
                ErrorCode::Internal,
                "capture thread unavailable".to_string(),
            )),
            Some(capture) => {
                let w_start = Instant::now();
                let r = handle_screenshot(capture, monitor, format);
                win32_us = w_start.elapsed().as_micros() as i64;
                r.unwrap_or_else(Response::Error)
            }
        },
        Request::ScreenshotRegion {
            x,
            y,
            w,
            h,
            monitor,
            format,
        } => match ctx.capture.as_ref() {
            None => Response::Error(Error::new(
                ErrorCode::Internal,
                "capture thread unavailable".to_string(),
            )),
            Some(capture) => {
                let region = fastuse_proto::coords::Rect {
                    x,
                    y,
                    w: w as i32,
                    h: h as i32,
                };
                let w_start = Instant::now();
                let r = handle_screenshot_region(capture, region, monitor, format);
                win32_us = w_start.elapsed().as_micros() as i64;
                r.unwrap_or_else(Response::Error)
            }
        },

        // -------- Phase 3: UIA (UIA-02..08) --------
        Request::UiaTree { hwnd, depth, view } => match ctx.uia.as_ref() {
            None => Response::Error(Error::new(
                ErrorCode::Internal,
                "uia pool unavailable".to_string(),
            )),
            Some(uia) => {
                let w_start = Instant::now();
                let r = handle_uia_tree(uia, hwnd, depth, view);
                win32_us = w_start.elapsed().as_micros() as i64;
                r.unwrap_or_else(Response::Error)
            }
        },
        Request::UiaQuery { selector, root_hwnd } => match ctx.uia.as_ref() {
            None => Response::Error(Error::new(
                ErrorCode::Internal,
                "uia pool unavailable".to_string(),
            )),
            Some(uia) => {
                let w_start = Instant::now();
                let r = handle_uia_query(uia, selector, root_hwnd);
                win32_us = w_start.elapsed().as_micros() as i64;
                r.unwrap_or_else(Response::Error)
            }
        },
        Request::InspectAtPoint { x, y } => match ctx.uia.as_ref() {
            None => Response::Error(Error::new(
                ErrorCode::Internal,
                "uia pool unavailable".to_string(),
            )),
            Some(uia) => {
                let w_start = Instant::now();
                let r = handle_inspect_at_point(uia, x, y);
                win32_us = w_start.elapsed().as_micros() as i64;
                r.unwrap_or_else(Response::Error)
            }
        },
        Request::ClickElement { selector, modifiers, opts } => {
            let inner = match (ctx.uia.as_ref(), ctx.input.as_ref()) {
                (Some(uia), Some(input)) => {
                    let w_start = Instant::now();
                    let r = handle_click_element(uia, input, selector, modifiers);
                    win32_us = w_start.elapsed().as_micros() as i64;
                    r.unwrap_or_else(Response::Error)
                }
                _ => Response::Error(Error::new(
                    ErrorCode::Internal,
                    "uia pool or input thread unavailable".to_string(),
                )),
            };
            finalize(inner, opts, ctx)
        }
        Request::TypeIntoElement { selector, text, opts } => {
            let inner = match (ctx.uia.as_ref(), ctx.input.as_ref()) {
                (Some(uia), Some(input)) => {
                    let w_start = Instant::now();
                    let r = handle_type_into_element(uia, input, selector, text);
                    win32_us = w_start.elapsed().as_micros() as i64;
                    r.unwrap_or_else(Response::Error)
                }
                _ => Response::Error(Error::new(
                    ErrorCode::Internal,
                    "uia pool or input thread unavailable".to_string(),
                )),
            };
            finalize(inner, opts, ctx)
        }
        Request::WaitForElement { selector, timeout_ms } => match ctx.uia.as_ref() {
            None => Response::Error(Error::new(
                ErrorCode::Internal,
                "uia pool unavailable".to_string(),
            )),
            Some(uia) => {
                let w_start = Instant::now();
                let r = handle_wait_for_element(uia, selector, timeout_ms);
                win32_us = w_start.elapsed().as_micros() as i64;
                r.unwrap_or_else(Response::Error)
            }
        },
        Request::ScrollIntoView { selector, opts } => {
            let inner = match ctx.uia.as_ref() {
                None => Response::Error(Error::new(
                    ErrorCode::Internal,
                    "uia pool unavailable".to_string(),
                )),
                Some(uia) => {
                    let w_start = Instant::now();
                    let r = handle_scroll_into_view(uia, selector);
                    win32_us = w_start.elapsed().as_micros() as i64;
                    r.unwrap_or_else(Response::Error)
                }
            };
            finalize(inner, opts, ctx)
        }
    };

    DispatchResult {
        response,
        daemon_dispatch_us: start.elapsed().as_micros() as i64,
        win32_work_us: win32_us,
    }
}

// ---- Phase 2 helpers ----

fn run_unit<F>(ctx: &DispatchCtx, win32_us: &mut i64, f: F) -> Response
where
    F: FnOnce() -> Result<(), Error> + Send + 'static,
{
    let Some(input) = ctx.input.as_ref() else {
        return Response::Error(Error::new(
            ErrorCode::Internal,
            "input thread unavailable".to_string(),
        ));
    };
    let w_start = Instant::now();
    let res: Result<(), Error> = input.run(move || f());
    *win32_us = w_start.elapsed().as_micros() as i64;
    match res {
        Ok(()) => Response::Ack { slept_us: None },
        Err(e) => Response::Error(e),
    }
}

fn run_value<T, F>(ctx: &DispatchCtx, win32_us: &mut i64, f: F) -> Result<T, Error>
where
    F: FnOnce() -> Result<T, Error> + Send + 'static,
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let Some(input) = ctx.input.as_ref() else {
        return Err(Error::new(
            ErrorCode::Internal,
            "input thread unavailable".to_string(),
        ));
    };
    let w_start = Instant::now();
    let r = input.run(f);
    *win32_us = w_start.elapsed().as_micros() as i64;
    r
}

// ---- Phase 4 permission-gate helpers ----

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

// Suppress unused-import warning for proto re-exports.
#[allow(dead_code)]
fn _types_used(_: MouseButton, _: ScrollDirection) {}

/// Apply `ActionOpts` post-action perception. Returns the inner response
/// unchanged when `opts` is `None`; otherwise delegates to `action_opts::apply`.
fn finalize(inner: Response, opts: Option<fastuse_proto::ActionOpts>, ctx: &DispatchCtx) -> Response {
    let Some(opts) = opts else { return inner };
    let inner_ok = matches!(inner, Response::Ack { .. } | Response::Element { matched: true });
    // Strategy is unknown at this layer — targeting::execute (Task 12) will
    // refine it. Default to BoundsClickGeometry placeholder for pre-targeting
    // / Phase 4 / bare-coord action arms.
    crate::action_opts::apply(
        opts,
        inner_ok,
        crate::action_opts::OptsCtx {
            uia: ctx.uia.as_ref(),
            capture: ctx.capture.as_ref(),
        },
        fastuse_proto::wire::Strategy::BoundsClickGeometry,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastuse_proto::{ClipboardGet, Redact, ShellExec};

    fn ctx_with(allow: Vec<String>) -> DispatchCtx {
        DispatchCtx {
            session_id: 1,
            input: None,
            uia: None,
            capture: None,
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
            Request::LaunchApp {
                req: fastuse_proto::LaunchApp { query: "Authy".into() },
                opts: None,
            },
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
