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
    _automation: &uiautomation::UIAutomation,
    point: (i32, i32),
    expected_runtime_id: Option<&[i32]>,
    expected_bounds: Option<Rect>,
) -> HitTestVerdict {
    // Implementer roadmap (plug in once the singleton accessor is exposed
    // from uia_pool / walker — Task 4 follow-up):
    // 1. Build a POINT { x: point.0, y: point.1 }.
    // 2. Call automation.element_from_point(point) -> UIElement.
    // 3. If runtime_id provided: compare hit_element.runtime_id() bytes to
    //    the expected slice. Equal -> Match.
    // 4. Else if bounds provided: read hit_element.bounding_rectangle() and
    //    compute intersection-over-union. >= 0.7 -> Match.
    // 5. Else (neither expected provided): Match (caller has no constraint).
    // 6. Errors map to Unknown.

    let _ = (point, expected_runtime_id, expected_bounds);
    HitTestVerdict::Unknown
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
