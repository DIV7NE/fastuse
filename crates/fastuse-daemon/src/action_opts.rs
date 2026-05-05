//! Post-action perception helper (`ActionOpts`).
//!
//! Exactly one entry point: `apply` runs `wait_for`, `expect`, and
//! `screenshot_after` against the existing UIA pool + capture thread,
//! returning a `Response::ActionResult` that bundles them. Inner action
//! must have already executed before `apply` is called.

use std::sync::Arc;
use std::time::{Duration, Instant};

use fastuse_proto::wire::{ExpectClause, Strategy, VerificationEvidence};
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
/// but mark `verified: false`. `strategy_used` is propagated from the caller
/// (Phase 4 / pre-targeting paths pass a placeholder; refined by Task 12).
pub fn apply(
    opts: ActionOpts,
    inner_ok: bool,
    ctx: OptsCtx<'_>,
    strategy_used: Strategy,
) -> Response {
    let timeout_ms = opts.wait_timeout_ms.unwrap_or(DEFAULT_WAIT_TIMEOUT_MS);

    let mut waited_ms: Option<u32> = None;
    let mut evidence = VerificationEvidence::Unverified;
    let mut verified = inner_ok;

    // wait_for: non-failing, populates evidence on match.
    if let Some(sel) = opts.wait_for.as_ref() {
        let start = Instant::now();
        let matched = poll_selector(ctx.uia, sel.clone(), timeout_ms);
        let ms = start.elapsed().as_millis() as u32;
        waited_ms = Some(waited_ms.unwrap_or(0).saturating_add(ms));
        if matched {
            evidence = VerificationEvidence::WaitForMatched;
            verified = inner_ok;
        }
    }

    // expect: failing — timeout = verified false.
    if let Some(exp) = opts.expect.as_ref() {
        let start = Instant::now();
        let matched = poll_expect(ctx.uia, exp, timeout_ms);
        let ms = start.elapsed().as_millis() as u32;
        waited_ms = Some(waited_ms.unwrap_or(0).saturating_add(ms));
        if matched {
            evidence = VerificationEvidence::WaitForMatched;
            verified = inner_ok;
        } else {
            verified = false;
        }
    }

    let screenshot = opts
        .screenshot_after
        .and_then(|so| capture_after(ctx.capture, so).ok());

    Response::ActionResult {
        verified,
        evidence,
        strategy_used,
        waited_ms,
        screenshot: screenshot.map(Box::new),
    }
}

/// Selector with hard-fail on timeout. Non-`SelectorMatches` variants of
/// `ExpectClause` are honored by `targeting::execute` (Task 11); from this
/// pre-targeting path we treat them as unsatisfied.
fn poll_expect(
    uia: Option<&Arc<UiaPoolHandle>>,
    exp: &ExpectClause,
    timeout_ms: u32,
) -> bool {
    match exp {
        ExpectClause::SelectorMatches(sel) => poll_selector(uia, sel.clone(), timeout_ms),
        _ => false,
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
