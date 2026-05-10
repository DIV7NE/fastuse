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
//!
//! v2 helpers (`handle_screenshot_v2`, `handle_zoom_v2`) add JPEG encoding
//! with coordinate-space scaling metadata for the vision-first computer-use
//! path (Task 10).

use fastuse_proto::{
    coords::Rect, Error as ProtoError, ErrorCode, ImageFormat, Redact, Response,
};

use crate::capture::dxgi::{capture_into_staging, FrameBuf};
use crate::capture::encode::{encode, encode_jpeg_rgba};
use crate::capture_thread::CaptureThreadHandle;
use crate::scaling::{compute_ratio, scaled_dims, ScaleSnapshot};
use crate::window::monitors::list_monitors;

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

/// Window-targeted screenshot. Captures the client area of `hwnd` in
/// virtual-desktop coords and returns image bytes + the monitor offset and
/// DPI scale needed to translate window-local pixel offsets back to
/// monitor-absolute coordinates.
///
/// Uses `GetClientRect` + `ClientToScreen` to compute the rect (so we do not
/// capture the title bar / borders), `GetDpiForWindow` for the DPI scale, and
/// `MonitorFromWindow` to pick the duplication object that contains the rect.
/// Capture itself reuses `capture_into_staging` — same cached duplication
/// surface as `screenshot-region`.
pub fn handle_screenshot_window(
    handle: &CaptureThreadHandle,
    hwnd: u64,
    format: Option<ImageFormat>,
) -> Result<Response, ProtoError> {
    use windows::Win32::Foundation::{HWND, POINT, RECT};
    use windows::Win32::Graphics::Gdi::{
        MonitorFromWindow, HMONITOR, MONITOR_DEFAULTTONEAREST,
    };
    use windows::Win32::Graphics::Gdi::ClientToScreen;
    use windows::Win32::UI::HiDpi::GetDpiForWindow;
    use windows::Win32::UI::WindowsAndMessaging::{GetClientRect, IsWindow};

    let h = HWND(hwnd as *mut core::ffi::c_void);
    // SAFETY: IsWindow accepts any HWND, returns false on stale.
    if !unsafe { IsWindow(Some(h)) }.as_bool() {
        return Err(ProtoError::new(
            ErrorCode::WindowNotFound,
            format!("HWND {hwnd:#x} is not a live window"),
        ));
    }

    let mut rect = RECT::default();
    // SAFETY: out-pointer; HWND is live.
    if unsafe { GetClientRect(h, &mut rect) }.is_err() {
        return Err(ProtoError::new(
            ErrorCode::Internal,
            format!("GetClientRect failed for HWND {hwnd:#x}"),
        ));
    }
    let mut origin = POINT { x: 0, y: 0 };
    // SAFETY: ClientToScreen mutates point in place; HWND live.
    if !unsafe { ClientToScreen(h, &mut origin) }.as_bool() {
        return Err(ProtoError::new(
            ErrorCode::Internal,
            format!("ClientToScreen failed for HWND {hwnd:#x}"),
        ));
    }
    let client_w = (rect.right - rect.left).max(0) as u32;
    let client_h = (rect.bottom - rect.top).max(0) as u32;
    if client_w == 0 || client_h == 0 {
        return Err(ProtoError::new(
            ErrorCode::Internal,
            format!(
                "HWND {hwnd:#x} client area has zero dimensions ({client_w}x{client_h}) — \
                 window may be minimized"
            ),
        ));
    }
    // SAFETY: GetDpiForWindow accepts any HWND; returns 0 on failure.
    let dpi = unsafe { GetDpiForWindow(h) };
    let dpi_scale = if dpi == 0 { 1.0_f32 } else { (dpi as f32) / 96.0 };

    // Locate the monitor that contains the window — look up the matching
    // index in the cached monitor list so `capture_into_staging` reuses the
    // existing duplication object.
    // SAFETY: MonitorFromWindow accepts any HWND.
    let hmon: HMONITOR = unsafe { MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST) };
    let monitors = list_monitors()?;
    let mon_idx = monitors
        .iter()
        .position(|m| m.id == hmon.0 as u64)
        .unwrap_or(0) as u32;

    let region = Rect {
        x: origin.x,
        y: origin.y,
        w: client_w as i32,
        h: client_h as i32,
    };
    let fmt = format.unwrap_or_default();
    let raw: ScreenshotRaw = handle.run(move || {
        let buf: FrameBuf = capture_into_staging(mon_idx, Some(region.clone()))?;
        let img = encode(&buf, fmt)?;
        Ok(ScreenshotRaw {
            bytes: img.bytes,
            mime: img.mime.to_string(),
            width: buf.w,
            height: buf.h,
        })
    })?;
    tracing::debug!(
        hwnd = format_args!("{hwnd:#x}"),
        width = raw.width,
        height = raw.height,
        mime = %raw.mime,
        bytes_len = raw.bytes.len(),
        dpi_scale,
        "screenshot_window ok"
    );
    Ok(Response::ScreenshotWindow {
        bytes: Redact::new(raw.bytes),
        mime: raw.mime,
        client_w: raw.width,
        client_h: raw.height,
        monitor_offset_x: origin.x,
        monitor_offset_y: origin.y,
        dpi_scale,
    })
}

// ---------------------------------------------------------------------------
// v2 vision-first helpers (Task 10)
// ---------------------------------------------------------------------------

/// Serializable result of a v2 screenshot or zoom capture. Carries the JPEG
/// bytes plus the scaling metadata needed to build a [`ScaleSnapshot`]. The
/// `Instant` field is omitted — the caller stamps `captured_at` after
/// deserialization.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct ScreenshotV2Raw {
    /// JPEG-encoded bytes.
    pub bytes: Vec<u8>,
    /// Uniform scale ratio (`native / scaled`).
    pub ratio: f64,
    /// Top-left corner of the captured region in virtual-desktop coords.
    pub monitor_origin_x: i32,
    /// Top-left y of the captured region in virtual-desktop coords.
    pub monitor_origin_y: i32,
    /// Native capture width in pixels.
    pub native_w: u32,
    /// Native capture height in pixels.
    pub native_h: u32,
    /// Scaled image width (what the model sees).
    pub scaled_w: u32,
    /// Scaled image height (what the model sees).
    pub scaled_h: u32,
}

impl ScreenshotV2Raw {
    /// Build a [`ScaleSnapshot`] from this raw result.
    pub fn to_snapshot(&self) -> ScaleSnapshot {
        use fastuse_proto::coords::Point;
        ScaleSnapshot {
            ratio: self.ratio,
            monitor_origin: Point { x: self.monitor_origin_x, y: self.monitor_origin_y },
            native_w: self.native_w,
            native_h: self.native_h,
            scaled_w: self.scaled_w,
            scaled_h: self.scaled_h,
            captured_at: std::time::Instant::now(),
        }
    }
}

/// Full-monitor screenshot v2 (vision-first, Task 10).
///
/// Captures the monitor specified by `monitor` (or the foreground monitor when
/// `None`), downscales so the longest side is ≤ `target_max` pixels, encodes
/// to JPEG, and returns the bytes plus scaling metadata. Runs on the capture
/// thread inside a `CaptureThreadHandle::run` closure.
pub fn handle_screenshot_v2(
    handle: &CaptureThreadHandle,
    monitor: Option<u32>,
    target_max: u32,
) -> Result<ScreenshotV2Raw, ProtoError> {
    // Determine origin (virtual-desktop top-left) for the chosen monitor.
    let (mon_idx, origin_x, origin_y) = resolve_monitor_origin(monitor)?;
    handle.run(move || {
        let buf = capture_into_staging(mon_idx, None)?;
        screenshot_v2_from_buf(&buf, origin_x, origin_y, target_max)
    })
}

/// Zoom capture v2 (vision-first, Task 10).
///
/// Captures a cropped native region around `coordinate` (in scaled image
/// space) of size `(base.scaled_w / zoom_factor) × (base.scaled_h /
/// zoom_factor)` pixels, upscales to `target_max`, and returns the result.
/// Runs on the capture thread.
pub fn handle_zoom_v2(
    handle: &CaptureThreadHandle,
    base: ScaleSnapshot,
    coordinate: [i32; 2],
    zoom_factor: f32,
    target_max: u32,
) -> Result<ScreenshotV2Raw, ProtoError> {
    use fastuse_proto::coords::Point;
    // Compute native rect of the zoom region.
    let scaled_center = Point { x: coordinate[0], y: coordinate[1] };
    let region_scaled_w = ((base.scaled_w as f32) / zoom_factor).max(8.0) as i32;
    let region_scaled_h = ((base.scaled_h as f32) / zoom_factor).max(8.0) as i32;
    let scaled_x = (scaled_center.x - region_scaled_w / 2).max(0);
    let scaled_y = (scaled_center.y - region_scaled_h / 2).max(0);
    let native_x = ((scaled_x as f64) * base.ratio).round() as i32 + base.monitor_origin.x;
    let native_y = ((scaled_y as f64) * base.ratio).round() as i32 + base.monitor_origin.y;
    let native_w = ((region_scaled_w as f64) * base.ratio).round() as i32;
    let native_h = ((region_scaled_h as f64) * base.ratio).round() as i32;
    if native_w <= 0 || native_h <= 0 {
        return Err(ProtoError::new(
            ErrorCode::Internal,
            "zoom region has zero/negative native dimensions".to_string(),
        ));
    }
    let region = Rect { x: native_x, y: native_y, w: native_w, h: native_h };
    handle.run(move || {
        let buf = capture_into_staging(0, Some(region))?;
        screenshot_v2_from_buf(&buf, native_x, native_y, target_max)
    })
}

/// Shared inner: downscale BGRA `buf` to `target_max`, JPEG-encode, return raw.
fn screenshot_v2_from_buf(
    buf: &FrameBuf,
    origin_x: i32,
    origin_y: i32,
    target_max: u32,
) -> Result<ScreenshotV2Raw, ProtoError> {
    if buf.w == 0 || buf.h == 0 {
        return Err(ProtoError::new(ErrorCode::Internal, "zero-sized capture".to_string()));
    }
    let ratio = compute_ratio(buf.w, buf.h, target_max);
    let (sw, sh) = scaled_dims(buf.w, buf.h, ratio);
    let rgba = resize_bgra_to_rgba(buf, sw, sh);
    let img = encode_jpeg_rgba(&rgba, sw, sh)?;
    Ok(ScreenshotV2Raw {
        bytes: img.bytes,
        ratio,
        monitor_origin_x: origin_x,
        monitor_origin_y: origin_y,
        native_w: buf.w,
        native_h: buf.h,
        scaled_w: sw,
        scaled_h: sh,
    })
}

/// BGRA → RGBA channel swap + resize to `(sw, sh)`.
fn resize_bgra_to_rgba(frame: &FrameBuf, sw: u32, sh: u32) -> Vec<u8> {
    use image::imageops::FilterType;
    use image::{ImageBuffer, Rgba};
    let mut rgba_native = Vec::with_capacity((frame.w * frame.h * 4) as usize);
    for chunk in frame.bgra.chunks_exact(4) {
        rgba_native.push(chunk[2]); // R
        rgba_native.push(chunk[1]); // G
        rgba_native.push(chunk[0]); // B
        rgba_native.push(chunk[3]); // A
    }
    // If already at target size, skip resize to avoid a copy.
    if sw == frame.w && sh == frame.h {
        return rgba_native;
    }
    let buf: ImageBuffer<Rgba<u8>, _> =
        ImageBuffer::from_raw(frame.w, frame.h, rgba_native).expect("buffer size matches");
    let resized = image::imageops::resize(&buf, sw, sh, FilterType::Triangle);
    resized.into_raw()
}

/// Return `(monitor_index, origin_x, origin_y)` for the given monitor slot.
/// `None` selects the primary monitor.
fn resolve_monitor_origin(monitor: Option<u32>) -> Result<(u32, i32, i32), ProtoError> {
    let mons = list_monitors()?;
    if mons.is_empty() {
        return Err(ProtoError::new(ErrorCode::Internal, "no monitors found".to_string()));
    }
    let idx = match monitor {
        None => {
            // Default to the primary monitor (or index 0 if none flagged primary).
            mons.iter()
                .position(|m| m.is_primary)
                .unwrap_or(0) as u32
        }
        Some(n) => {
            if (n as usize) >= mons.len() {
                return Err(ProtoError::new(
                    ErrorCode::Internal,
                    format!("monitor index {n} out of range (0..{})", mons.len()),
                ));
            }
            n
        }
    };
    let m = &mons[idx as usize];
    Ok((idx, m.bounds.x, m.bounds.y))
}
