//! Pre-click hit-test gate. Calls `IUIAutomation::ElementFromPoint` at the
//! intended click coordinate and verifies the returned element matches the
//! intended target. Without this, "click missed" is unobservable — the
//! L-Connect3 motivating failure.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use fastuse_proto::coords::Rect;

use crate::uia_pool::UiaPoolHandle;

/// Outcome of the hit-test gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HitTestVerdict {
    /// Hit-test confirmed — safe to click.
    Match,
    /// Different element under the cursor — abort, do not click.
    Mismatch,
    /// Could not hit-test (window vanished, point offscreen, COM error).
    /// Caller must decide; default policy is "abort, return Unverified".
    Unknown,
}

/// Verify that `point` (physical pixels) hits a target equivalent to either
/// `expected_runtime_id` (preferred for UIA candidates) or `expected_bounds`
/// (for geometry/OCR candidates, with >=70% bbox overlap).
///
/// Runs on the UIA pool MTA worker (D-25).
pub fn verify_hit(
    point: (i32, i32),
    expected_runtime_id: Option<&[i32]>,
    expected_bounds: Option<Rect>,
    uia: &Arc<UiaPoolHandle>,
) -> HitTestVerdict {
    let runtime_id = expected_runtime_id.map(<[i32]>::to_vec);
    let bounds = expected_bounds;
    uia.run(move |automation| {
        Ok(verify_on_uia_thread(
            automation,
            point,
            runtime_id.as_deref(),
            bounds,
        ))
    })
    .unwrap_or(HitTestVerdict::Unknown)
}

fn verify_on_uia_thread(
    automation: &uiautomation::UIAutomation,
    point: (i32, i32),
    expected_runtime_id: Option<&[i32]>,
    expected_bounds: Option<Rect>,
) -> HitTestVerdict {
    use uiautomation::types::{Point, UIProperty};

    // Caller has no constraint — nothing to verify against.
    if expected_runtime_id.is_none() && expected_bounds.is_none() {
        return HitTestVerdict::Match;
    }

    // Build a one-shot CacheRequest covering the properties we need to read
    // off the hit element without any `Current*` round-trips (UIA-12). Only
    // BoundingRectangle is needed here — RuntimeId comes from
    // `get_runtime_id()` (an instance accessor, not a `Current*` property).
    let req = match automation.create_cache_request() {
        Ok(r) => r,
        Err(_) => return HitTestVerdict::Unknown,
    };
    if req.add_property(UIProperty::BoundingRectangle).is_err() {
        return HitTestVerdict::Unknown;
    }

    let element = match automation
        .element_from_point_build_cache(Point::new(point.0, point.1), &req)
    {
        Ok(e) => e,
        Err(_) => return HitTestVerdict::Unknown,
    };

    if let Some(expected) = expected_runtime_id {
        let hit_rid = match element.get_runtime_id() {
            Ok(r) => r,
            Err(_) => return HitTestVerdict::Unknown,
        };
        return if hit_rid.as_slice() == expected {
            HitTestVerdict::Match
        } else {
            HitTestVerdict::Mismatch
        };
    }

    if let Some(expected) = expected_bounds {
        let rect = match element.get_cached_bounding_rectangle() {
            Ok(r) => r,
            Err(_) => return HitTestVerdict::Unknown,
        };
        let hit_bounds = Rect {
            x: rect.get_left(),
            y: rect.get_top(),
            w: (rect.get_right() - rect.get_left()).max(0),
            h: (rect.get_bottom() - rect.get_top()).max(0),
        };
        return if iou(hit_bounds, expected) >= 0.70 {
            HitTestVerdict::Match
        } else {
            HitTestVerdict::Mismatch
        };
    }

    HitTestVerdict::Match
}

/// Compute intersection-over-union for two rects.
pub fn iou(a: Rect, b: Rect) -> f32 {
    let ax2 = a.x as i64 + a.w as i64;
    let ay2 = a.y as i64 + a.h as i64;
    let bx2 = b.x as i64 + b.w as i64;
    let by2 = b.y as i64 + b.h as i64;

    let ix1 = (a.x as i64).max(b.x as i64);
    let iy1 = (a.y as i64).max(b.y as i64);
    let ix2 = ax2.min(bx2);
    let iy2 = ay2.min(by2);

    if ix2 <= ix1 || iy2 <= iy1 {
        return 0.0;
    }
    let inter = (ix2 - ix1) * (iy2 - iy1);
    let area_a = (a.w as i64) * (a.h as i64);
    let area_b = (b.w as i64) * (b.h as i64);
    let union = area_a + area_b - inter;
    if union <= 0 {
        return 0.0;
    }
    (inter as f64 / union as f64) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iou_identical_is_one() {
        let r = Rect { x: 0, y: 0, w: 100, h: 100 };
        assert!((iou(r, r) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn iou_disjoint_is_zero() {
        let a = Rect { x: 0, y: 0, w: 10, h: 10 };
        let b = Rect { x: 100, y: 100, w: 10, h: 10 };
        assert_eq!(iou(a, b), 0.0);
    }

    #[test]
    fn iou_half_overlap() {
        let a = Rect { x: 0, y: 0, w: 10, h: 10 };
        let b = Rect { x: 5, y: 0, w: 10, h: 10 };
        // intersection 5x10=50; union 10*10+10*10-50=150; 50/150=0.333..
        let v = iou(a, b);
        assert!((v - 1.0 / 3.0).abs() < 1e-6, "got {v}");
    }

    #[test]
    fn iou_70_threshold_check() {
        // 70% overlap is the spec's threshold for geometry-candidate match.
        let a = Rect { x: 0, y: 0, w: 100, h: 100 };
        let b = Rect { x: 7, y: 7, w: 93, h: 93 };
        let v = iou(a, b);
        // 93*93=8649 intersection; union = 10000+8649-8649=10000; 8649/10000=0.8649
        assert!(v > 0.70, "got {v}");
    }
}
