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
use std::sync::{Arc, Mutex};
use std::time::Instant;

use base64::Engine as _;
use fastuse_core::perm::{self, Tier};
use fastuse_core::FastuseError;
use fastuse_proto::{
    coords::WindowInfo as ProtoWindowInfo, ClipboardSet, Error, ErrorCode, MonitorInfo,
    MouseButton, ProcessSelector, Request, Response, ScrollDirection,
};
use fastuse_win::capture::{
    handle_screenshot, handle_screenshot_region, handle_screenshot_v2, handle_screenshot_window,
    handle_zoom_v2,
};
use fastuse_win::capture_thread::CaptureThreadHandle;
use fastuse_win::input::handlers as ih;
use fastuse_win::input_thread::{InputJob, InputThreadHandle};
use fastuse_win::scaling::ScaleStack;
use fastuse_win::uia::{
    handle_inspect_at_point, handle_scroll_into_view, handle_uia_query, handle_uia_tree,
};
use fastuse_win::uia_pool::UiaPoolHandle;
use fastuse_win::window::{
    cursor_position::cursor_position, focus::focus_window, foreground::foreground_window,
    list_windows::list_windows, monitors::list_monitors, move_resize::resize_move_window,
    wait_for_window::wait_for_window,
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
    /// Per-session scale-context stack for vision-first coordinate translation
    /// (Task 10). Reset on every `Screenshot`; pushed on every `Zoom`.
    pub scale: Arc<Mutex<ScaleStack>>,
    /// Safe-mode permission policy resolved at daemon startup from
    /// `config.toml` and `FASTUSE_SAFE_MODE`. When `safe_mode` is active
    /// and the tool is in the gated set, dispatch returns
    /// `PermissionRequired` before the normal allow-list check runs.
    pub permissions: Arc<fastuse_win::permissions::Permissions>,
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
            // v2 spec: default open. Only safe_mode gates.
            if let Err(resp) = safe_mode_gate(ctx, "shell_exec") {
                resp
            } else {
                let w = Instant::now();
                let r = fastuse_win::shell::shell_exec(se).await;
                win32_us = w.elapsed().as_micros() as i64;
                match r {
                    Ok(res) => Response::ShellExec(res),
                    Err(e) => Response::Error(e.to_wire()),
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
        Request::TypeRated { text, rate_ms, opts } => {
            let payload = text.into_inner();
            tracing::debug!(text_len = payload.len(), rate_ms, "type_rated tool dispatch");
            let inner = run_unit(ctx, &mut win32_us, move || {
                ih::type_text_rated(&payload, rate_ms)
            });
            finalize(inner, opts, ctx)
        }
        Request::WaitForIdle { hwnd, timeout_ms } => {
            let w_start = Instant::now();
            let r = fastuse_win::window::wait_for_idle::wait_for_idle(hwnd, timeout_ms);
            win32_us = w_start.elapsed().as_micros() as i64;
            match r {
                Ok(r) => Response::Idle {
                    waited_ms: r.waited_ms,
                    paint_observed: r.paint_observed,
                    focus_settled: r.focus_settled,
                },
                Err(e) => Response::Error(e),
            }
        }
        Request::ScreenshotWindow { hwnd, format } => match ctx.capture.as_ref() {
            None => Response::Error(Error::new(
                ErrorCode::Internal,
                "capture thread unavailable".to_string(),
            )),
            Some(capture) => {
                let w_start = Instant::now();
                let r = handle_screenshot_window(capture, hwnd, format);
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

        // --- v2 computer action dispatch (Task 10) ---
        Request::Computer(req) => {
            let w_start = Instant::now();
            let r = dispatch_computer(req, ctx).await;
            win32_us = w_start.elapsed().as_micros() as i64;
            r
        }
        Request::WaitForWindowV2(req) => {
            let w_start = Instant::now();
            let result = wait_for_window(&req);
            win32_us = w_start.elapsed().as_micros() as i64;
            match result {
                Some(info) => Response::Window(info),
                None => Response::Error(
                    Error::new(
                        ErrorCode::WindowNotFound,
                        format!(
                            "no window matching title={:?} process={:?} appeared within {}ms",
                            req.title_substr, req.process_name, req.timeout_ms
                        ),
                    )
                    .with_hint(
                        "increase timeout_ms or verify the process name/title substring",
                    ),
                ),
            }
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
///
/// Safe-mode is checked first: if `ctx.permissions.is_allowed(tool)` returns
/// `false`, `PermissionRequired` is returned immediately before the normal
/// allow-list check runs.  This means even a session with `--allow=*` cannot
/// invoke a gated tool while `FASTUSE_SAFE_MODE=1`.
async fn gate_then<F>(
    tool: &'static str,
    target: Option<&str>,
    ctx: &DispatchCtx,
    f: F,
) -> GateOutcome
where
    F: FnOnce(&DispatchCtx) -> (Result<Response, FastuseError>, i64),
{
    // v2 spec: default open. Only safe_mode gates. The legacy Tier system
    // (perm::resolve / FASTUSE_ALLOW) was replaced by safe_mode in v2.
    let _ = target;
    if let Err(resp) = safe_mode_gate(ctx, tool) {
        return GateOutcome {
            result: Ok(resp),
            win32_us: 0,
        };
    }
    let (r, us) = f(ctx);
    GateOutcome {
        result: r,
        win32_us: us,
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

/// Check the safe-mode `Permissions` policy before invoking a gated tool.
///
/// Returns `Err(Response::Error(PermissionRequired))` with a hint when
/// `safe_mode` is active and `tool` is in the gated set.  Returns `Ok(())`
/// when the policy allows invocation, letting the caller proceed to the
/// normal `perm::resolve` allow-list check.
fn safe_mode_gate(ctx: &DispatchCtx, tool: &str) -> Result<(), Response> {
    if ctx.permissions.is_allowed(tool) {
        Ok(())
    } else {
        Err(Response::Error(
            Error::new(
                ErrorCode::PermissionRequired,
                format!("tool '{tool}' is gated in safe mode"),
            )
            .with_hint(
                "set FASTUSE_SAFE_MODE=0 to disable, or remove from gated_tools in config.toml",
            ),
        ))
    }
}

/// Build a typed `Response::Error` for known error conditions.
#[allow(dead_code)]
pub fn err_response(code: ErrorCode, msg: impl Into<String>) -> Response {
    Response::Error(Error::new(code, msg))
}

// Suppress unused-import warning for proto re-exports.
#[allow(dead_code)]
fn _types_used(_: MouseButton, _: ScrollDirection) {}

// ---------------------------------------------------------------------------
// v2 computer action dispatch (Task 10)
// ---------------------------------------------------------------------------

/// Dispatch a single `ComputerRequest` to the appropriate input/capture path.
async fn dispatch_computer(
    req: fastuse_proto::wire::ComputerRequest,
    ctx: &DispatchCtx,
) -> Response {
    use fastuse_proto::wire::{ComputerAction, ComputerResult, ImagePayload, ScrollDir};
    use fastuse_win::input::backend::{
        InputAction, MotionProfile, MouseButton as BackendButton,
        ScrollDirection as BackendScrollDir, TypingProfile,
    };
    use fastuse_win::input::sendinput::Modifiers;

    let coordinates_native = req.coordinates_native;

    let ok_response = || Response::Computer(ComputerResult {
        ok: true,
        image: None,
        cursor: None,
        scale: None,
    });

    match req.action {
        // ------------------------------------------------------------------
        ComputerAction::Screenshot { monitor, format } => {
            let Some(capture) = ctx.capture.as_ref() else {
                return Response::Error(Error::new(
                    ErrorCode::Internal,
                    "capture thread unavailable".to_string(),
                ));
            };
            let target_max = fastuse_win::scaling::DEFAULT_TARGET_MAX;
            match handle_screenshot_v2(capture, monitor, target_max, format.unwrap_or_default()) {
                Err(e) => Response::Error(e),
                Ok(raw) => {
                    let snap = raw.to_snapshot();
                    ctx.scale.lock().unwrap().reset(snap.clone());
                    let scale_info = scale_info_from(&snap);
                    Response::Computer(ComputerResult {
                        ok: true,
                        image: Some(ImagePayload {
                            format: mime_label(&raw.mime).into(),
                            width: raw.scaled_w,
                            height: raw.scaled_h,
                            data_base64: base64::engine::general_purpose::STANDARD
                                .encode(&raw.bytes),
                        }),
                        cursor: None,
                        scale: Some(scale_info),
                    })
                }
            }
        }

        // ------------------------------------------------------------------
        ComputerAction::LeftClick { coordinate, text, humanize } => {
            let modifiers = parse_chord_modifiers(text.as_ref().map(|r| r.as_inner().as_str()));
            match resolve_coord(&ctx.scale, coordinate, coordinates_native) {
                Err(r) => r,
                Ok(at) => dispatch_input(
                    ctx,
                    InputAction::MouseClick {
                        at,
                        button: BackendButton::Left,
                        count: 1,
                        modifiers,
                        humanize,
                    },
                ),
            }
        }

        // ------------------------------------------------------------------
        ComputerAction::RightClick { coordinate, humanize } => {
            match resolve_coord(&ctx.scale, coordinate, coordinates_native) {
                Err(r) => r,
                Ok(at) => dispatch_input(
                    ctx,
                    InputAction::MouseClick {
                        at,
                        button: BackendButton::Right,
                        count: 1,
                        modifiers: Modifiers::default(),
                        humanize,
                    },
                ),
            }
        }

        // ------------------------------------------------------------------
        ComputerAction::MiddleClick { coordinate, humanize } => {
            match resolve_coord(&ctx.scale, coordinate, coordinates_native) {
                Err(r) => r,
                Ok(at) => dispatch_input(
                    ctx,
                    InputAction::MouseClick {
                        at,
                        button: BackendButton::Middle,
                        count: 1,
                        modifiers: Modifiers::default(),
                        humanize,
                    },
                ),
            }
        }

        // ------------------------------------------------------------------
        ComputerAction::DoubleClick { coordinate, humanize } => {
            match resolve_coord(&ctx.scale, coordinate, coordinates_native) {
                Err(r) => r,
                Ok(at) => dispatch_input(
                    ctx,
                    InputAction::MouseClick {
                        at,
                        button: BackendButton::Left,
                        count: 2,
                        modifiers: Modifiers::default(),
                        humanize,
                    },
                ),
            }
        }

        // ------------------------------------------------------------------
        ComputerAction::TripleClick { coordinate, humanize } => {
            match resolve_coord(&ctx.scale, coordinate, coordinates_native) {
                Err(r) => r,
                Ok(at) => dispatch_input(
                    ctx,
                    InputAction::MouseClick {
                        at,
                        button: BackendButton::Left,
                        count: 3,
                        modifiers: Modifiers::default(),
                        humanize,
                    },
                ),
            }
        }

        // ------------------------------------------------------------------
        ComputerAction::LeftClickDrag { start_coordinate, coordinate, humanize, text } => {
            let modifiers = parse_chord_modifiers(text.as_ref().map(|r| r.as_inner().as_str()));
            let from = match resolve_coord(&ctx.scale, start_coordinate, coordinates_native) {
                Err(r) => return r,
                Ok(p) => p,
            };
            let to = match resolve_coord(&ctx.scale, coordinate, coordinates_native) {
                Err(r) => return r,
                Ok(p) => p,
            };
            dispatch_input(
                ctx,
                InputAction::Drag {
                    from,
                    to,
                    button: BackendButton::Left,
                    profile: MotionProfile { humanize, ..MotionProfile::default() },
                    modifiers,
                },
            )
        }

        // ------------------------------------------------------------------
        ComputerAction::LeftMouseDown { coordinate } => {
            let at = match coordinate {
                Some(c) => match resolve_coord(&ctx.scale, c, coordinates_native) {
                    Err(r) => return r,
                    Ok(p) => p,
                },
                None => {
                    match cursor_position_backend() {
                        Err(r) => return r,
                        Ok(p) => p,
                    }
                }
            };
            dispatch_input(ctx, InputAction::MouseDown { at, button: BackendButton::Left })
        }

        // ------------------------------------------------------------------
        ComputerAction::LeftMouseUp { coordinate } => {
            let at = match coordinate {
                Some(c) => match resolve_coord(&ctx.scale, c, coordinates_native) {
                    Err(r) => return r,
                    Ok(p) => p,
                },
                None => {
                    match cursor_position_backend() {
                        Err(r) => return r,
                        Ok(p) => p,
                    }
                }
            };
            dispatch_input(ctx, InputAction::MouseUp { at, button: BackendButton::Left })
        }

        // ------------------------------------------------------------------
        ComputerAction::MouseMove { coordinate, humanize } => {
            let to = match resolve_coord(&ctx.scale, coordinate, coordinates_native) {
                Err(r) => return r,
                Ok(p) => p,
            };
            let from = match cursor_position_backend() {
                Err(r) => return r,
                Ok(p) => p,
            };
            dispatch_input(
                ctx,
                InputAction::MouseMove {
                    from,
                    to,
                    profile: MotionProfile { humanize, ..MotionProfile::default() },
                },
            )
        }

        // ------------------------------------------------------------------
        ComputerAction::CursorPosition => {
            match cursor_position() {
                Err(e) => Response::Error(e),
                Ok((x, y, _)) => Response::Computer(ComputerResult {
                    ok: true,
                    image: None,
                    cursor: Some([x, y]),
                    scale: None,
                }),
            }
        }

        // ------------------------------------------------------------------
        ComputerAction::Type { text, humanize } => {
            // Unwrap the Redact only here; never pass it to a format macro.
            let payload = text.into_inner();
            dispatch_input(
                ctx,
                InputAction::KeyType {
                    text: payload,
                    profile: TypingProfile { humanize, ..TypingProfile::default() },
                },
            )
        }

        // ------------------------------------------------------------------
        ComputerAction::Key { text } => {
            match parse_key_chord(&text.into_inner()) {
                Err(e) => e,
                Ok(keys) => dispatch_input(
                    ctx,
                    InputAction::KeyChord { keys, hold_ms: None },
                ),
            }
        }

        // ------------------------------------------------------------------
        ComputerAction::HoldKey { text, duration } => {
            match parse_key_chord(&text.into_inner()) {
                Err(e) => e,
                Ok(keys) => dispatch_input(
                    ctx,
                    InputAction::KeyChord { keys, hold_ms: Some(duration) },
                ),
            }
        }

        // ------------------------------------------------------------------
        ComputerAction::Scroll { coordinate, scroll_direction, scroll_amount } => {
            let at = match resolve_coord(&ctx.scale, coordinate, coordinates_native) {
                Err(r) => return r,
                Ok(p) => p,
            };
            let direction = match scroll_direction {
                ScrollDir::Up => BackendScrollDir::Up,
                ScrollDir::Down => BackendScrollDir::Down,
                ScrollDir::Left => BackendScrollDir::Left,
                ScrollDir::Right => BackendScrollDir::Right,
            };
            dispatch_input(ctx, InputAction::Scroll { at, direction, amount: scroll_amount })
        }

        // ------------------------------------------------------------------
        ComputerAction::Wait { duration } => {
            tokio::time::sleep(std::time::Duration::from_millis(duration as u64)).await;
            ok_response()
        }

        // ------------------------------------------------------------------
        ComputerAction::Zoom { coordinate, zoom_factor } => {
            let snap_now = {
                let locked = ctx.scale.lock().unwrap();
                match locked.current().cloned() {
                    None => {
                        return Response::Error(Error::new(
                            ErrorCode::Internal,
                            "no scale context — call screenshot first".to_string(),
                        ))
                    }
                    Some(s) => s,
                }
            };
            let Some(capture) = ctx.capture.as_ref() else {
                return Response::Error(Error::new(
                    ErrorCode::Internal,
                    "capture thread unavailable".to_string(),
                ));
            };
            let target_max = fastuse_win::scaling::DEFAULT_TARGET_MAX;
            // Zoom has no format knob on the wire: the Anthropic-shaped
            // computer tool cannot carry one, so JPEG is the only reachable
            // choice. The capture layer still takes it for uniformity.
            match handle_zoom_v2(
                capture,
                snap_now,
                coordinate,
                zoom_factor,
                target_max,
                fastuse_proto::ImageFormat::Jpeg,
            ) {
                Err(e) => Response::Error(e),
                Ok(raw) => {
                    let snap_zoom = raw.to_snapshot();
                    ctx.scale.lock().unwrap().push(snap_zoom.clone());
                    let scale_info = scale_info_from(&snap_zoom);
                    Response::Computer(ComputerResult {
                        ok: true,
                        image: Some(ImagePayload {
                            format: mime_label(&raw.mime).into(),
                            width: raw.scaled_w,
                            height: raw.scaled_h,
                            data_base64: base64::engine::general_purpose::STANDARD
                                .encode(&raw.bytes),
                        }),
                        cursor: None,
                        scale: Some(scale_info),
                    })
                }
            }
        }
    }
}

/// Resolve a coordinate to a native virtual-desktop `Point`.
///
/// When `native` is `true` the coordinate is already in native pixels and the
/// `ScaleStack` is bypassed entirely (CLI path, `coordinates_native: true`).
/// When `native` is `false` the coordinate is in scaled image-pixel space and
/// is translated through the current `ScaleStack` snapshot (MCP path).
fn resolve_coord(
    scale: &Arc<Mutex<ScaleStack>>,
    coordinate: [i32; 2],
    native: bool,
) -> Result<fastuse_win::input::backend::Point, Response> {
    if native {
        Ok(fastuse_win::input::backend::Point { x: coordinate[0], y: coordinate[1] })
    } else {
        translate_or_err(scale, coordinate)
    }
}

/// Translate a scaled image-space coordinate to native virtual-desktop pixels.
/// Returns `Response::Error` (not `Err`) so callers can return it directly.
fn translate_or_err(
    scale: &Arc<Mutex<ScaleStack>>,
    coordinate: [i32; 2],
) -> Result<fastuse_win::input::backend::Point, Response> {
    use fastuse_proto::coords::Point as ProtoPoint;
    use fastuse_win::input::backend::Point as BackendPoint;
    use fastuse_win::scaling::ScaleError;

    let p = ProtoPoint { x: coordinate[0], y: coordinate[1] };
    let locked = scale.lock().unwrap();
    match locked.translate(p) {
        Ok(native) => Ok(BackendPoint { x: native.x, y: native.y }),
        Err(ScaleError::NoContext) => Err(Response::Error(
            Error::new(ErrorCode::Internal, "no scale context — call screenshot first".to_string())
                .with_hint("send a Screenshot action before any click/scroll/drag"),
        )),
        Err(ScaleError::OutOfBounds { point, bounds }) => Err(Response::Error(
            Error::new(
                ErrorCode::Internal,
                format!(
                    "coordinate [{}, {}] out of scaled image bounds {}×{}",
                    point.x, point.y, bounds.0, bounds.1
                ),
            )
            .with_hint("coordinates must be within the scaled image dimensions from the last screenshot"),
        )),
    }
}

/// Dispatch an [`InputAction`] to the input thread and return `Response::Computer`.
fn dispatch_input(ctx: &DispatchCtx, action: fastuse_win::input::backend::InputAction) -> Response {
    let Some(input) = ctx.input.as_ref() else {
        return Response::Error(Error::new(
            ErrorCode::Internal,
            "input thread unavailable".to_string(),
        ));
    };
    match input.dispatch(action) {
        Ok(()) => Response::Computer(fastuse_proto::wire::ComputerResult {
            ok: true,
            image: None,
            cursor: None,
            scale: None,
        }),
        Err(e) => Response::Error(e),
    }
}

/// Read the current cursor position and convert to a backend `Point`.
fn cursor_position_backend() -> Result<fastuse_win::input::backend::Point, Response> {
    use fastuse_win::input::backend::Point as BackendPoint;
    cursor_position()
        .map(|(x, y, _)| BackendPoint { x, y })
        .map_err(Response::Error)
}

/// Parse a modifier chord string like `"ctrl+shift"` into [`Modifiers`].
/// Unknown tokens are silently ignored (best-effort, matches v1 behavior).
fn parse_chord_modifiers(text: Option<&str>) -> fastuse_win::input::sendinput::Modifiers {
    use fastuse_proto::chord::ModKey;
    use fastuse_win::input::sendinput::Modifiers;
    let Some(s) = text else { return Modifiers::default() };
    let mut m = Modifiers::default();
    // Split on '+' and check each token for modifier keywords.
    for token in s.split('+') {
        let lc: String = token.chars().flat_map(|c| c.to_lowercase()).collect();
        match lc.trim() {
            "ctrl" | "control" => m.ctrl = true,
            "shift" => m.shift = true,
            "alt" => m.alt = true,
            "win" | "super" | "meta" => m.win = true,
            _ => {}
        }
    }
    m
}

/// Parse a key chord string like `"ctrl+l"` or `"enter"` into VK codes.
/// Returns `Err(Response::Error(...))` if the chord is malformed.
fn parse_key_chord(
    text: &str,
) -> Result<Vec<u16>, Response> {
    use fastuse_proto::chord::{parse_chord, ChordKey, ModKey};
    parse_chord(text)
        .map(|chord| {
            let mut keys: Vec<u16> = Vec::new();
            for m in &chord.mods {
                keys.push(m.vk());
            }
            match chord.key {
                ChordKey::Vk(vk) => keys.push(vk),
                ChordKey::Unicode(c) => {
                    // Unicode scalar: emit as a VK code if ASCII letter/digit,
                    // otherwise fall back to the char's u16 value for SendInput.
                    let vk = (c as u32).min(0xFFFF) as u16;
                    keys.push(vk);
                }
            }
            keys
        })
        .map_err(|e| {
            Response::Error(
                Error::new(
                    ErrorCode::Internal,
                    format!("invalid key chord {:?}: {e}", text),
                )
                .with_hint("use xdotool-style syntax: ctrl+l, enter, alt+f4, win+d"),
            )
        })
}

/// Build a [`ScaleInfo`] wire type from a [`ScaleSnapshot`].
/// `ImagePayload.format` carries the short label ("jpeg"/"png"), while the
/// capture layer reports a MIME type. Keep the wire label unchanged.
fn mime_label(mime: &str) -> &'static str {
    if mime == "image/png" {
        "png"
    } else {
        "jpeg"
    }
}

fn scale_info_from(snap: &fastuse_win::scaling::ScaleSnapshot) -> fastuse_proto::wire::ScaleInfo {
    fastuse_proto::wire::ScaleInfo {
        ratio: snap.ratio,
        monitor_origin: [snap.monitor_origin.x, snap.monitor_origin.y],
        native_w: snap.native_w,
        native_h: snap.native_h,
        scaled_w: snap.scaled_w,
        scaled_h: snap.scaled_h,
    }
}

/// Apply `ActionOpts` post-action perception. Returns the inner response
/// unchanged when `opts` is `None`; otherwise delegates to `action_opts::apply`.
fn finalize(inner: Response, opts: Option<fastuse_proto::ActionOpts>, ctx: &DispatchCtx) -> Response {
    let Some(opts) = opts else { return inner };
    crate::action_opts::apply(
        opts,
        inner,
        crate::action_opts::OptsCtx {
            uia: ctx.uia.as_ref(),
            capture: ctx.capture.as_ref(),
        },
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
            scale: Arc::new(Mutex::new(ScaleStack::new())),
            // Tests default to safe_mode off so existing allow-list tests pass.
            permissions: Arc::new(fastuse_win::permissions::Permissions::default()),
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
                req: fastuse_proto::LaunchApp { query: "Authy".into() , capture_output: false },
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

    /// Helper: build a `DispatchCtx` with safe_mode active (default gated set).
    fn ctx_safe_mode(allow: Vec<String>) -> DispatchCtx {
        use fastuse_win::permissions::{Permissions, DEFAULT_GATED};
        DispatchCtx {
            session_id: 1,
            input: None,
            uia: None,
            capture: None,
            shutdown_flag: Arc::new(AtomicBool::new(false)),
            idle_timeout_secs: 300,
            session: Session::new(allow),
            scale: Arc::new(Mutex::new(ScaleStack::new())),
            permissions: Arc::new(Permissions {
                safe_mode: true,
                gated: DEFAULT_GATED.iter().map(|s| s.to_string()).collect(),
            }),
        }
    }

    #[tokio::test]
    async fn safe_mode_blocks_kill_process_even_with_allow() {
        // kill_process is in allow list but safe_mode should gate it first.
        let ctx = ctx_safe_mode(vec!["kill_process".into()]);
        let r = handle(
            Request::KillProcess(fastuse_proto::KillProcess {
                selector: ProcessSelector::Pid(99999),
                force: None,
                process_tree: None,
            }),
            &ctx,
        )
        .await;
        match r.response {
            Response::Error(e) => {
                assert_eq!(e.code, ErrorCode::PermissionRequired);
                let hint = e.hint.expect("hint must be present");
                assert!(hint.contains("FASTUSE_SAFE_MODE=0"));
            }
            other => panic!("expected PermissionRequired, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn safe_mode_blocks_shell_exec_even_with_allow() {
        let ctx = ctx_safe_mode(vec!["shell_exec".into()]);
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
                let hint = e.hint.expect("hint must be present");
                assert!(hint.contains("FASTUSE_SAFE_MODE=0"));
            }
            other => panic!("expected PermissionRequired, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn safe_mode_does_not_block_list_processes() {
        let ctx = ctx_safe_mode(vec![]);
        let r = handle(
            Request::ListProcesses(fastuse_proto::ListProcesses { filter: None }),
            &ctx,
        )
        .await;
        match r.response {
            Response::ListProcesses(_) => (),
            other => panic!("expected ListProcesses (not gated), got {other:?}"),
        }
    }
}
