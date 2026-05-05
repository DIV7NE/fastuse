//! Post-action perception helper (`ActionOpts`).
//!
//! Simplified for v2 pivot: `expect` and `escalate` have been removed.
//! Only `wait_for` (non-failing UIA poll) and `screenshot_after` remain.
//! `ActionResult` wire variant is gone; this module returns `Response::Ack`
//! on success with an optional appended screenshot path for callers to use.
//!
//! Full `computer`-action perception will be wired in Task 10.

use std::sync::Arc;
use std::time::{Duration, Instant};

use fastuse_proto::{
    coords::Rect, ActionOpts, Error, ErrorCode, ImageFormat, RegionSpec, Response,
    ScreenshotPayload, Selector,
};
use fastuse_win::capture::{handle_screenshot, handle_screenshot_region};
use fastuse_win::capture_thread::CaptureThreadHandle;
use fastuse_win::uia::{
    automation::foreground_hwnd, cache::get_or_fetch, find::find_first,
};
use fastuse_win::uia_pool::UiaPoolHandle;

const DEFAULT_WAIT_TIMEOUT_MS: u32 = 2000;
const POLL_CADENCE: Duration = Duration::from_millis(50);

pub struct OptsCtx<'a> {
    pub uia: Option<&'a Arc<UiaPoolHandle>>,
    pub capture: Option<&'a Arc<CaptureThreadHandle>>,
}

/// Apply post-action perception. Returns the inner response when `opts` is
/// `None`. When `opts` is `Some`, runs `wait_for` (non-failing) and
/// `screenshot_after`, then returns a `Response::Screenshot` if a screenshot
/// was requested and the inner action succeeded, or the inner response
/// otherwise.
///
/// NOTE: `ActionResult` wire variant has been removed in the v2 pivot.
/// This function now returns the inner response unchanged (after running
/// wait_for as a side-effect). Screenshot-after results are folded into
/// a `Response::Screenshot` if the inner action was an `Ack`.
/// Full perception will be re-wired in Task 10.
pub fn apply(opts: ActionOpts, inner: Response, ctx: OptsCtx<'_>) -> Response {
    let timeout_ms = opts.wait_timeout_ms.unwrap_or(DEFAULT_WAIT_TIMEOUT_MS);

    // wait_for: non-failing side-effect poll.
    if let Some(sel) = opts.wait_for.as_ref() {
        poll_selector(ctx.uia, sel.clone(), timeout_ms);
    }

    // screenshot_after: if inner was Ack and a screenshot was requested,
    // return the screenshot; otherwise return inner unchanged.
    if let Some(so) = opts.screenshot_after {
        if matches!(inner, Response::Ack { .. }) {
            match capture_after(ctx.capture, so) {
                Ok(payload) => {
                    return Response::Screenshot {
                        bytes: payload.bytes,
                        mime: payload.mime,
                        width: payload.width,
                        height: payload.height,
                    };
                }
                Err(()) => {
                    return Response::Error(Error::new(
                        ErrorCode::Internal,
                        "screenshot_after: capture unavailable".to_string(),
                    ));
                }
            }
        }
    }

    inner
}

fn poll_selector(uia: Option<&Arc<UiaPoolHandle>>, sel: Selector, timeout_ms: u32) -> bool {
    let Some(pool) = uia else { return false };
    let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64);
    loop {
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        let s = sel.clone();
        let hwnd = match foreground_hwnd() {
            Some(h) => h,
            None => {
                std::thread::sleep(POLL_CADENCE);
                continue;
            }
        };
        let probe: Result<Option<fastuse_proto::UIANode>, Error> = pool.run(move |uia| {
            let root = get_or_fetch(uia, hwnd).map_err(|_| {
                Error::new(ErrorCode::WindowNotFound, "foreground HWND vanished".to_string())
            })?;
            find_first(uia, &root, &s)
        });
        if matches!(probe, Ok(Some(_))) {
            return true;
        }
        let remaining = POLL_CADENCE.saturating_sub(now.elapsed());
        if !remaining.is_zero() {
            std::thread::sleep(remaining);
        }
    }
}

fn capture_after(
    capture: Option<&Arc<CaptureThreadHandle>>,
    opts: fastuse_proto::ScreenshotOpts,
) -> Result<ScreenshotPayload, ()> {
    let cap = capture.ok_or(())?;
    let format = opts.format.unwrap_or(ImageFormat::Jpeg);
    let resp = match opts.region {
        None => handle_screenshot(cap, None, Some(format)).map_err(|_| ())?,
        Some(RegionSpec::Auto) => {
            let (x, y, w, h) = foreground_rect().ok_or(())?;
            handle_screenshot_region(cap, Rect { x, y, w: w as i32, h: h as i32 }, None, Some(format))
                .map_err(|_| ())?
        }
        Some(RegionSpec::Rect { x, y, w, h }) => {
            handle_screenshot_region(cap, Rect { x, y, w: w as i32, h: h as i32 }, None, Some(format))
                .map_err(|_| ())?
        }
    };
    match resp {
        Response::Screenshot { bytes, mime, width, height } => {
            Ok(ScreenshotPayload { bytes, mime, width, height })
        }
        _ => Err(()),
    }
}

fn foreground_rect() -> Option<(i32, i32, u32, u32)> {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowRect};
    // SAFETY: GetForegroundWindow is always safe. GetWindowRect writes to our RECT.
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_invalid() {
            return None;
        }
        let mut r = RECT::default();
        if GetWindowRect(hwnd, &mut r).is_err() {
            return None;
        }
        let w = (r.right - r.left) as u32;
        let h = (r.bottom - r.top) as u32;
        Some((r.left, r.top, w, h))
    }
}
