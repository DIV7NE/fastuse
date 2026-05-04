//! Screenshot + screenshot_region handlers (Phase 3 Task 07, CAP-01..02, CAP-06).
//!
//! Both go through the capture-thread closure-dispatch path
//! (`CaptureThreadHandle::run`). The cached `CaptureState` lives in the
//! thread-local set up in `capture::dxgi`; both handlers reuse the same
//! object — `screenshot_region` MUST NOT cause reacquisition (CAP-02).
//!
//! Bytes ride back wrapped in `Redact<Vec<u8>>` (T-03-03 — pixel bytes never
//! reach Debug/tracing). The MCP edge unwraps and re-encodes to base64 for
//! the JSON `image` content block.

use fastuse_proto::{
    coords::Rect, Error as ProtoError, ErrorCode, ImageFormat, Redact, Response,
};

use crate::capture::dxgi::{capture_into_staging, FrameBuf};
use crate::capture::encode::encode;
use crate::capture_thread::CaptureThreadHandle;

/// Full-monitor screenshot (CAP-01).
pub fn handle_screenshot(
    handle: &CaptureThreadHandle,
    monitor: Option<u32>,
    format: Option<ImageFormat>,
) -> Result<Response, ProtoError> {
    let mon = monitor.unwrap_or(0);
    let fmt = format.unwrap_or_default();
    let raw: ScreenshotRaw = handle.run(move || {
        let buf = capture_into_staging(mon, None)?;
        let img = encode(&buf, fmt)?;
        Ok(ScreenshotRaw {
            bytes: img.bytes,
            mime: img.mime.to_string(),
            width: buf.w,
            height: buf.h,
        })
    })?;
    tracing::debug!(width = raw.width, height = raw.height, mime = %raw.mime, bytes_len = raw.bytes.len(), "screenshot ok");
    Ok(Response::Screenshot {
        bytes: Redact::new(raw.bytes),
        mime: raw.mime,
        width: raw.width,
        height: raw.height,
    })
}

/// Sub-rectangle screenshot (CAP-02). Reuses the cached duplication object —
/// no reacquisition is permitted by this path.
pub fn handle_screenshot_region(
    handle: &CaptureThreadHandle,
    region: Rect,
    monitor: Option<u32>,
    format: Option<ImageFormat>,
) -> Result<Response, ProtoError> {
    if region.w <= 0 || region.h <= 0 {
        return Err(ProtoError::new(
            ErrorCode::Internal,
            "region width and height must be > 0".to_string(),
        ));
    }
    let mon = monitor.unwrap_or(0);
    let fmt = format.unwrap_or_default();
    let raw: ScreenshotRaw = handle.run(move || {
        let buf: FrameBuf = capture_into_staging(mon, Some(region.clone()))?;
        let img = encode(&buf, fmt)?;
        Ok(ScreenshotRaw {
            bytes: img.bytes,
            mime: img.mime.to_string(),
            width: buf.w,
            height: buf.h,
        })
    })?;
    tracing::debug!(
        width = raw.width,
        height = raw.height,
        mime = %raw.mime,
        bytes_len = raw.bytes.len(),
        "screenshot_region ok"
    );
    Ok(Response::Screenshot {
        bytes: Redact::new(raw.bytes),
        mime: raw.mime,
        width: raw.width,
        height: raw.height,
    })
}

/// Mono-typed wire across the capture-thread closure boundary
/// (`CaptureThreadHandle::run` requires `Serialize + DeserializeOwned`).
#[derive(serde::Serialize, serde::Deserialize)]
struct ScreenshotRaw {
    bytes: Vec<u8>,
    mime: String,
    width: u32,
    height: u32,
}
