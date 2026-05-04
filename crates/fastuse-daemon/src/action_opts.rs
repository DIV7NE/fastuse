//! Post-action perception helper (`ActionOpts`).
//!
//! Exactly one entry point: `apply` runs `wait_for`, `verify`, and
//! `screenshot_after` against the existing UIA pool + capture thread,
//! returning a `Response::ActionResult` that bundles them. Inner action
//! must have already executed before `apply` is called.

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

/// Apply post-action perception. `inner_ok` reports whether the action
/// itself succeeded; if false we still try to gather perception (best-effort)
/// but mark `ok: false`.
pub fn apply(opts: ActionOpts, inner_ok: bool, ctx: OptsCtx<'_>) -> Response {
    let timeout_ms = opts.wait_timeout_ms.unwrap_or(DEFAULT_WAIT_TIMEOUT_MS);

    let (wait_matched, waited_ms) = match opts.wait_for.as_ref() {
        None => (None, 0u32),
        Some(sel) => {
            let start = Instant::now();
            let m = poll_selector(ctx.uia, sel.clone(), timeout_ms);
            (Some(m), start.elapsed().as_millis() as u32)
        }
    };

    let verify_ok = match opts.verify.as_ref() {
        None => true,
        Some(sel) => poll_selector(ctx.uia, sel.clone(), timeout_ms),
    };

    let screenshot = opts
        .screenshot_after
        .and_then(|so| capture_after(ctx.capture, so).ok());

    Response::ActionResult {
        ok: inner_ok && verify_ok,
        wait_matched,
        waited_ms,
        screenshot: screenshot.map(Box::new),
    }
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
