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
    // Capture pixels for the region. Reuses the existing
    // capture_thread::run_screenshot_region API.
    let pixels = match capture
        .run(move || Ok::<_, fastuse_proto::Error>(capture_region_pixels(region)))
        .ok()
        .flatten()
    {
        Some(p) => p,
        None => return Vec::new(),
    };

    let frame_hash = super::cache::frame_hash(&pixels);
    let key = super::cache::OcrCacheKey {
        frame_hash,
        region: super::cache::PackedRect::from(region),
    };
    let mut hits = match super::cache::cache_lookup(key) {
        Some(h) => h,
        None => {
            // OCR pass on the dedicated thread.
            let pixels_w = region.w;
            let pixels_h = region.h;
            let pixels_owned = pixels.clone();
            let raw_hits = ocr
                .run(move || run_ocr_on_pixels(&pixels_owned, pixels_w, pixels_h))
                .ok()
                .unwrap_or_default();
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
            super::cache::cache_store(key, hits.clone());
            hits
        }
    };

    // Filter to needle. Case-insensitive contains.
    // Unwrap of Redact is local to this module (trusted boundary); the result
    // continues to flow through `Redact<String>` downstream.
    let needle_lower = needle.to_lowercase();
    hits.retain(|h| h.text.as_inner().to_lowercase().contains(&needle_lower));
    hits
}

fn capture_region_pixels(_region: Rect) -> Option<Vec<u8>> {
    // Implementer: thread the existing capture_thread DXGI duplication
    // staging-texture path; copy out the cropped sub-rect as BGRA bytes.
    // The capture thread already has a region-aware screenshot routine
    // for the `Request::ScreenshotRegion` arm — call it directly.
    None
}

fn run_ocr_on_pixels(_pixels: &[u8], _w: i32, _h: i32) -> Vec<OcrHit> {
    // Implementer: build a SoftwareBitmap from the BGRA pixel buffer
    // (BitmapPixelFormat::Bgra8, BitmapAlphaMode::Premultiplied), then
    // engine.RecognizeAsync(bitmap).get(). Iterate `OcrResult.Lines()` →
    // `OcrLine.Words()` (or build per-line bounding boxes by unioning
    // word rects), fill OcrHit entries.
    //
    // Bounds returned by Windows.Media.Ocr are in dips at the source
    // bitmap's resolution — for cropped captures, that means region-local
    // physical pixels (no further DPI math needed because we capture
    // physical-pixel buffers).
    Vec::new()
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
