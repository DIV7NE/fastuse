//! Per-request dispatch.
//!
//! Phase 1 only handles `Hello`/`Ping`/`Shutdown`. Future phases extend the
//! match arm by appending new variants.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use fastuse_proto::{Error, ErrorCode, Request, Response};
use fastuse_win::input_thread::{InputJob, InputThreadHandle};

/// Shared dispatcher context.
pub struct DispatchCtx {
    pub session_id: u32,
    pub input: Option<Arc<InputThreadHandle>>,
    pub shutdown_flag: Arc<AtomicBool>,
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
        // Hello is normally consumed in the connection handshake; if we get a
        // second one, treat it as a no-op echo.
        Request::Hello { .. } => Response::Welcome {
            daemon_version: env!("CARGO_PKG_VERSION").to_string(),
            current_idle_timeout_secs: 0,
        },
        Request::Ping { ts_us } => {
            // Optionally fan out a Noop to the input thread to populate
            // win32_work_us; this proves the cross-thread channel is healthy
            // and gives us a real number to report.
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
                current_idle_timeout_secs: 0,
            }
        }
    };

    DispatchResult {
        response,
        daemon_dispatch_us: start.elapsed().as_micros() as i64,
        win32_work_us: win32_us,
    }
}

/// Build a typed `Response::Error` for known error conditions.
#[allow(dead_code)]
pub fn err_response(code: ErrorCode, msg: impl Into<String>) -> Response {
    Response::Error(Error::new(code, msg))
}
