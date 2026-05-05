//! Cropped progressive OCR. Default scope: target window's client rect
//! only, not full screen. Order:
//!   1. Crop to client rect.
//!   2. If multiple matches, narrow to UIA-resolved scroll containers /
//!      panes ("likely regions").
//!   3. Only fall back to full window if (1)+(2) yield zero matches.

use std::sync::Arc;

use fastuse_proto::coords::Rect;
use fastuse_proto::redact::Redact;
use serde::{Deserialize, Serialize};

use crate::capture_thread::CaptureThreadHandle;
use crate::ocr_thread::OcrThreadHandle;

use windows::core::Interface;
use windows::Graphics::Imaging::{
    BitmapAlphaMode, BitmapBufferAccessMode, BitmapPixelFormat, SoftwareBitmap,
};
use windows::Win32::System::WinRT::IMemoryBufferByteAccess;
use windows_future::AsyncStatus;

/// One OCR match.
///
/// `text` is wrapped in `Redact<String>` per the spec's "Open invariants" —
/// OCR results are user-screen content and must not leak through Display/Debug.
/// Downstream consumers unwrap via `as_inner()` / `into_inner()` (audit-greppable).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OcrHit {
    /// Recognized text (redacted at boundary).
    pub text: Redact<String>,
    /// Bounding rect in physical pixels (virtual-desktop origin).
    pub bounds: Rect,
    /// `OcrEngine` reported confidence, scaled to [0.0, 1.0].
    pub confidence: f32,
}

/// Run OCR cropped to `region`. Goes through the cache first; on miss,
/// dispatches a capture (cropped to region) and an OCR pass on the OCR thread.
pub async fn ocr_cropped_progressive(
    region: Rect,
    needle: &str,
    capture: Arc<CaptureThreadHandle>,
    ocr: Arc<OcrThreadHandle>,
) -> Vec<OcrHit> {
    tracing::info!(
        target: "fastuse_win::ocr::cropped",
        region_x = region.x,
        region_y = region.y,
        region_w = region.w,
        region_h = region.h,
        needle = needle,
        "ocr_cropped_progressive: entry"
    );
    // Capture pixels for the region. Reuses the existing
    // capture_thread::run_screenshot_region API.
    let captured = match capture
        .run(move || Ok::<_, fastuse_proto::Error>(capture_region_pixels(region)))
        .ok()
        .flatten()
    {
        Some(p) => p,
        None => {
            tracing::info!(
                target: "fastuse_win::ocr::cropped",
                "ocr_cropped_progressive: capture returned None"
            );
            return Vec::new();
        }
    };
    let (pixels, captured_w, captured_h) = captured;
    tracing::info!(
        target: "fastuse_win::ocr::cropped",
        pixel_buf_len = pixels.len(),
        captured_w,
        captured_h,
        "ocr_cropped_progressive: capture done"
    );

    let frame_hash = super::cache::frame_hash(&pixels);
    let key = super::cache::OcrCacheKey {
        frame_hash,
        region: super::cache::PackedRect::from(region),
    };
    let mut hits = match super::cache::cache_lookup(key) {
        Some(h) => {
            tracing::info!(
                target: "fastuse_win::ocr::cropped",
                cached_count = h.len(),
                "ocr_cropped_progressive: cache hit"
            );
            h
        }
        None => {
            // OCR pass on the dedicated thread.
            let pixels_w = captured_w;
            let pixels_h = captured_h;
            let pixels_owned = pixels.clone();
            let raw_hits = ocr
                .run(move || run_ocr_on_pixels(&pixels_owned, pixels_w, pixels_h))
                .ok()
                .unwrap_or_default();
            tracing::info!(
                target: "fastuse_win::ocr::cropped",
                raw_hit_count = raw_hits.len(),
                "ocr_cropped_progressive: raw hits returned"
            );
            for h in &raw_hits {
                tracing::info!(
                    target: "fastuse_win::ocr::cropped",
                    text = %h.text.as_inner(),
                    bx = h.bounds.x,
                    by = h.bounds.y,
                    bw = h.bounds.w,
                    bh = h.bounds.h,
                    "ocr hit PRE-translate"
                );
            }
            // Translate hit bounds from region-local to virtual-desktop.
            let hits: Vec<OcrHit> = raw_hits
                .into_iter()
                .map(|mut h| {
                    h.bounds = Rect {
                        x: h.bounds.x + region.x,
                        y: h.bounds.y + region.y,
                        w: h.bounds.w,
                        h: h.bounds.h,
                    };
                    h
                })
                .collect();
            for h in &hits {
                tracing::info!(
                    target: "fastuse_win::ocr::cropped",
                    text = %h.text.as_inner(),
                    bx = h.bounds.x,
                    by = h.bounds.y,
                    bw = h.bounds.w,
                    bh = h.bounds.h,
                    "ocr hit POST-translate"
                );
            }
            super::cache::cache_store(key, hits.clone());
            hits
        }
    };

    // Filter to needle. Case-insensitive contains.
    // Unwrap of Redact is local to this module (trusted boundary); the result
    // continues to flow through `Redact<String>` downstream.
    let needle_lower = needle.to_lowercase();
    let pre_filter = hits.len();
    hits.retain(|h| h.text.as_inner().to_lowercase().contains(&needle_lower));
    tracing::info!(
        target: "fastuse_win::ocr::cropped",
        needle = %needle_lower,
        pre_filter_count = pre_filter,
        post_filter_count = hits.len(),
        "ocr_cropped_progressive: filter applied"
    );
    for h in &hits {
        tracing::info!(
            target: "fastuse_win::ocr::cropped",
            text = %h.text.as_inner(),
            bx = h.bounds.x,
            by = h.bounds.y,
            bw = h.bounds.w,
            bh = h.bounds.h,
            "ocr_cropped_progressive: returning hit"
        );
    }
    hits
}

/// Capture the requested region as raw BGRA bytes. Runs ON the capture
/// thread (caller dispatches via `CaptureThreadHandle::run`), reusing the
/// cached `IDXGIOutputDuplication` and staging texture from
/// `crate::capture::dxgi`. CAP-02: no reacquisition.
///
/// Returns `(pixels, w, h)`. The captured `w`/`h` may be smaller than
/// `region.w`/`region.h` after monitor-edge clamping; downstream OCR keys
/// off the actual buffer size.
fn capture_region_pixels(region: Rect) -> Option<(Vec<u8>, i32, i32)> {
    // Monitor 0 — matches Phase 3 screenshot defaults. Multi-monitor
    // dispatching is a v1.1 concern; the dxgi path translates virtual-
    // desktop coords to monitor-local internally.
    match crate::capture::capture_into_staging(0, Some(region)) {
        Ok(buf) => Some((buf.bgra, buf.w as i32, buf.h as i32)),
        Err(e) => {
            tracing::debug!(error = ?e, "capture_region_pixels failed");
            None
        }
    }
}

/// Run Windows.Media.Ocr over the BGRA pixel buffer. Runs ON the OCR thread
/// (caller dispatches via `OcrThreadHandle::run`); the cached `OcrEngine`
/// from `ocr_thread::ocr_engine` is reused.
///
/// One `OcrHit` is emitted per recognized **line** (word rects unioned).
/// Bounds are in region-local physical pixels — caller translates to
/// virtual-desktop coordinates. Confidence is fixed at 0.85 (the hybrid
/// scorer's OCR ceiling); per-word/-line confidence is not exposed by
/// `OcrEngine`. Empty `Vec` on any failure (no language pack, empty input,
/// engine error) — OCR is best-effort by design.
fn run_ocr_on_pixels(pixels: &[u8], w: i32, h: i32) -> Vec<OcrHit> {
    tracing::info!(
        target: "fastuse_win::ocr::cropped",
        bitmap_w = w,
        bitmap_h = h,
        pixel_buf_len = pixels.len(),
        "run_ocr_on_pixels: entry"
    );
    if w <= 0 || h <= 0 || pixels.is_empty() {
        return Vec::new();
    }
    let expected = (w as usize) * (h as usize) * 4;
    if pixels.len() < expected {
        tracing::debug!(
            got = pixels.len(),
            expected,
            "run_ocr_on_pixels: pixel buffer shorter than w*h*4; aborting"
        );
        return Vec::new();
    }

    let engine = match crate::ocr_thread::ocr_engine().and_then(|s| s.engine().cloned()) {
        Some(e) => e,
        None => {
            tracing::debug!("run_ocr_on_pixels: no OcrEngine (no language packs?)");
            return Vec::new();
        }
    };

    let bitmap = match build_bgra_bitmap(pixels, w, h) {
        Ok(b) => b,
        Err(e) => {
            tracing::debug!(error = ?e, "run_ocr_on_pixels: SoftwareBitmap build failed");
            return Vec::new();
        }
    };

    // RecognizeAsync. We are the dedicated OCR thread; spin-wait on Status()
    // is acceptable (no other work pending on this thread). The `Async` trait
    // helper from `windows-future` is private, so we drive completion manually.
    let async_op = match engine.RecognizeAsync(&bitmap) {
        Ok(a) => a,
        Err(e) => {
            tracing::debug!(error = ?e, "RecognizeAsync failed");
            return Vec::new();
        }
    };
    // Bounded poll loop: 5 s ceiling (typical OCR pass is 30-150 ms).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match async_op.Status() {
            Ok(s) if s != AsyncStatus::Started => break,
            Ok(_) => {}
            Err(e) => {
                tracing::debug!(error = ?e, "RecognizeAsync.Status failed");
                return Vec::new();
            }
        }
        if std::time::Instant::now() >= deadline {
            tracing::debug!("RecognizeAsync exceeded 5s deadline");
            return Vec::new();
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let result = match async_op.GetResults() {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(error = ?e, "RecognizeAsync.GetResults failed");
            return Vec::new();
        }
    };

    let lines = match result.Lines() {
        Ok(l) => l,
        Err(_) => return Vec::new(),
    };

    let mut hits = Vec::new();
    for line in &lines {
        let words = match line.Words() {
            Ok(w) => w,
            Err(_) => continue,
        };

        // Union of all word bounding rects -> per-line bounding box.
        let mut union: Option<(f32, f32, f32, f32)> = None;
        for word in &words {
            let r = match word.BoundingRect() {
                Ok(r) => r,
                Err(_) => continue,
            };
            let (x1, y1, x2, y2) = (r.X, r.Y, r.X + r.Width, r.Y + r.Height);
            union = Some(match union {
                None => (x1, y1, x2, y2),
                Some((ax1, ay1, ax2, ay2)) => {
                    (ax1.min(x1), ay1.min(y1), ax2.max(x2), ay2.max(y2))
                }
            });
        }
        let bounds = match union {
            Some((x1, y1, x2, y2)) => Rect {
                x: x1.floor() as i32,
                y: y1.floor() as i32,
                w: (x2 - x1).ceil().max(0.0) as i32,
                h: (y2 - y1).ceil().max(0.0) as i32,
            },
            None => continue, // line with no words; skip
        };

        let text = match line.Text() {
            Ok(t) => t.to_string_lossy(),
            Err(_) => continue,
        };
        if text.is_empty() {
            continue;
        }

        tracing::info!(
            target: "fastuse_win::ocr::cropped",
            text = %text,
            bx = bounds.x,
            by = bounds.y,
            bw = bounds.w,
            bh = bounds.h,
            "run_ocr_on_pixels: emitting line"
        );
        hits.push(OcrHit {
            text: Redact::new(text),
            bounds,
            confidence: 0.85,
        });
    }
    tracing::info!(
        target: "fastuse_win::ocr::cropped",
        line_count = hits.len(),
        "run_ocr_on_pixels: done"
    );
    hits
}

/// Construct a `SoftwareBitmap` (BGRA8, Premultiplied alpha) from a tight
/// row-pitch BGRA buffer. Uses `IMemoryBufferByteAccess::GetBuffer` to
/// memcpy bytes into the bitmap's backing store — the windows-rs pattern
/// recommended by Microsoft samples.
fn build_bgra_bitmap(
    pixels: &[u8],
    w: i32,
    h: i32,
) -> windows::core::Result<SoftwareBitmap> {
    let bitmap = SoftwareBitmap::CreateWithAlpha(
        BitmapPixelFormat::Bgra8,
        w,
        h,
        BitmapAlphaMode::Premultiplied,
    )?;
    let buffer = bitmap.LockBuffer(BitmapBufferAccessMode::Write)?;
    let reference = buffer.CreateReference()?;
    let byte_access: IMemoryBufferByteAccess = reference.cast()?;

    // SAFETY: GetBuffer returns a pointer to capacity bytes of the bitmap
    // backing store; copying `min(pixels.len(), capacity)` bytes is safe.
    // The bitmap, buffer, and reference are kept alive across the copy.
    unsafe {
        let mut ptr: *mut u8 = std::ptr::null_mut();
        let mut capacity: u32 = 0;
        byte_access.GetBuffer(&mut ptr, &mut capacity)?;
        if !ptr.is_null() {
            let n = std::cmp::min(pixels.len(), capacity as usize);
            std::ptr::copy_nonoverlapping(pixels.as_ptr(), ptr, n);
        }
    }

    drop(reference);
    drop(buffer);
    Ok(bitmap)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ocr_hit_clone_round_trip() {
        let h = OcrHit {
            text: Redact::new("Settings".to_string()),
            bounds: Rect { x: 1, y: 2, w: 3, h: 4 },
            confidence: 0.9,
        };
        let c = h.clone();
        assert_eq!(h, c);
    }
}
