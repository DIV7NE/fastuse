//! Pre-click hit-test gate. `ElementFromPoint` at the target pixel; if the
//! returned element doesn't match the intended target, abort.
//! Real implementation in Task 6.

use fastuse_proto::coords::Rect;

/// Outcome of the hit-test gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitTestVerdict {
    /// Hit-test confirmed — safe to click.
    Match,
    /// Different element under the cursor — abort, do not click.
    Mismatch,
    /// Could not hit-test (window vanished, point offscreen, etc.).
    Unknown,
}

/// Verify that `point` (physical pixels) hits the same target as `expected_runtime_id`,
/// or that the bounding box overlap with `expected_bounds` exceeds 70%.
pub fn verify_hit(
    _point: (i32, i32),
    _expected_runtime_id: Option<&[i32]>,
    _expected_bounds: Option<Rect>,
) -> HitTestVerdict {
    // Task 6 implements via uia_pool + ElementFromPoint.
    HitTestVerdict::Unknown
}
