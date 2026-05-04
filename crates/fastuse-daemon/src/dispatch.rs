//! Per-request dispatch.
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
//! `tokio::time::sleep` so it doesn't occupy the input-thread slot.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use fastuse_proto::{
    coords::WindowInfo as ProtoWindowInfo, Error, ErrorCode, MonitorInfo, MouseButton, Request,
    Response, ScrollDirection,
};
use fastuse_win::input::handlers as ih;
use fastuse_win::input_thread::{InputJob, InputThreadHandle};
use fastuse_win::window::{
    cursor_position::cursor_position, focus::focus_window, foreground::foreground_window,
    list_windows::list_windows, monitors::list_monitors, move_resize::resize_move_window,
};

/// Shared dispatcher context.
pub struct DispatchCtx {
    pub session_id: u32,
    pub input: Option<Arc<InputThreadHandle>>,
    pub shutdown_flag: Arc<AtomicBool>,
    /// Reconciled idle timeout reported on every Welcome (D-22).
    pub idle_timeout_secs: u64,
}

/// Outcome of dispatching a single frame.
pub struct DispatchResult {
    pub response: Response,
    /// Microseconds spent inside dispatch logic itself.
    pub daemon_dispatch_us: i64,
    /// Microseconds spent inside Win32 work (input thread round-trip).
    pub win32_work_us: i64,
}

/// Dispatch a single request.
pub fn handle(req: Request, ctx: &DispatchCtx) -> DispatchResult {
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

        // -------- Phase 2: input --------
        Request::Click {
            x,
            y,
            button,
            count,
            modifiers,
            skip_set_cursor_pos,
        } => run_unit(ctx, &mut win32_us, move || {
            ih::click(x, y, button, count, &modifiers, skip_set_cursor_pos)
        }),
        Request::MouseMove { x, y } => run_unit(ctx, &mut win32_us, move || ih::mouse_move(x, y)),
        Request::MouseDown { button } => run_unit(ctx, &mut win32_us, move || ih::mouse_down(button)),
        Request::MouseUp { button } => run_unit(ctx, &mut win32_us, move || ih::mouse_up(button)),
        Request::Drag {
            start_x,
            start_y,
            end_x,
            end_y,
            button,
            modifiers,
        } => run_unit(ctx, &mut win32_us, move || {
            ih::drag(start_x, start_y, end_x, end_y, button, &modifiers)
        }),
        Request::Scroll {
            x,
            y,
            direction,
            amount,
            modifiers,
        } => run_unit(ctx, &mut win32_us, move || {
            ih::scroll(x, y, direction, amount, &modifiers)
        }),
        Request::Type { text } => {
            // Expose the redacted payload only inside this scope; never log it.
            let payload = text.into_inner();
            tracing::debug!(text_len = payload.len(), "type tool dispatch");
            run_unit(ctx, &mut win32_us, move || ih::type_text(&payload))
        }
        Request::Key { chord, repeat } => run_unit(ctx, &mut win32_us, move || ih::key(&chord, repeat)),
        Request::HoldKey { chord, duration_ms } => run_unit(ctx, &mut win32_us, move || ih::hold_key(&chord, duration_ms)),
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
        Request::FocusWindow { hwnd } => run_unit(ctx, &mut win32_us, move || focus_window(hwnd)),
        Request::ResizeMoveWindow { hwnd, x, y, w, h } => {
            run_unit(ctx, &mut win32_us, move || resize_move_window(hwnd, x, y, w, h))
        }

        // -------- Phase 3: capture + UIA (handlers land in Tasks 04-18) --------
        // These arms exist so the wire surface is exhaustive against
        // Request; full DXGI / UIA handlers are wired in subsequent commits.
        Request::Screenshot { .. }
        | Request::ScreenshotRegion { .. }
        | Request::UiaTree { .. }
        | Request::UiaQuery { .. }
        | Request::InspectAtPoint { .. }
        | Request::ClickElement { .. }
        | Request::TypeIntoElement { .. }
        | Request::WaitForElement { .. }
        | Request::ScrollIntoView { .. } => Response::Error(Error::new(
            ErrorCode::Internal,
            "Phase 3 handler not yet wired — Tasks 04-18 land DXGI/UIA backends".to_string(),
        )
        .with_hint("rebuild after subsequent Phase 3 commits".to_string())),
    };

    DispatchResult {
        response,
        daemon_dispatch_us: start.elapsed().as_micros() as i64,
        win32_work_us: win32_us,
    }
}

// ---- helpers ----

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

/// Build a typed `Response::Error` for known error conditions.
#[allow(dead_code)]
pub fn err_response(code: ErrorCode, msg: impl Into<String>) -> Response {
    Response::Error(Error::new(code, msg))
}

// Suppress unused-import warning for proto re-exports.
#[allow(dead_code)]
fn _types_used(_: MouseButton, _: ScrollDirection) {}
