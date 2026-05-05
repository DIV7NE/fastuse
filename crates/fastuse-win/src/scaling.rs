//! Coordinate scaling between native virtual-desktop pixels and the scaled
//! image space Claude reasons in (target ~1024 longest side per Anthropic
//! `computer_20251124`). Pure math here; the per-session state machine lives
//! in scaling::context (next task).

use fastuse_proto::coords::{Point, Rect};

/// Default target for the longest side, in scaled pixels.
pub const DEFAULT_TARGET_MAX: u32 = 1024;

/// Compute the uniform scale ratio for a monitor of `(native_w, native_h)`.
/// `target_max` is the desired longest-side length in scaled space. The
/// returned ratio is `native / scaled`, i.e. divide native by ratio to get
/// scaled, multiply scaled by ratio to get native.
pub fn compute_ratio(native_w: u32, native_h: u32, target_max: u32) -> f64 {
    let longest = native_w.max(native_h) as f64;
    let target = target_max.max(1) as f64;
    (longest / target).max(1.0)
}

/// Scaled dimensions for a native monitor at the given ratio.
pub fn scaled_dims(native_w: u32, native_h: u32, ratio: f64) -> (u32, u32) {
    let w = ((native_w as f64) / ratio).round() as u32;
    let h = ((native_h as f64) / ratio).round() as u32;
    (w.max(1), h.max(1))
}

/// Map a native rect to scaled image coords.
pub fn scale_rect_to_image(native: Rect, ratio: f64) -> Rect {
    Rect {
        x: ((native.x as f64) / ratio).round() as i32,
        y: ((native.y as f64) / ratio).round() as i32,
        w: ((native.w as f64) / ratio).round() as i32,
        h: ((native.h as f64) / ratio).round() as i32,
    }
}

/// Map a point from scaled image coords to native virtual-desktop coords.
/// `monitor_origin` is the captured monitor's top-left in virtual-desktop
/// space (top-left of leftmost monitor on the desktop = (0, 0); origins for
/// secondary monitors are e.g. (-1920, 0) or (1440, 0) depending on layout).
pub fn point_to_native(scaled: Point, ratio: f64, monitor_origin: Point) -> Point {
    Point {
        x: ((scaled.x as f64) * ratio).round() as i32 + monitor_origin.x,
        y: ((scaled.y as f64) * ratio).round() as i32 + monitor_origin.y,
    }
}

/// True when `scaled` is within the `(0..scaled_w, 0..scaled_h)` box.
pub fn point_in_scaled_bounds(scaled: Point, scaled_w: u32, scaled_h: u32) -> bool {
    scaled.x >= 0
        && scaled.y >= 0
        && (scaled.x as u32) < scaled_w
        && (scaled.y as u32) < scaled_h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratio_for_1440p_to_1024() {
        let r = compute_ratio(2560, 1440, 1024);
        assert!((r - 2.5).abs() < 1e-6);
    }

    #[test]
    fn ratio_for_1080p_to_1024() {
        let r = compute_ratio(1920, 1080, 1024);
        assert!((r - 1.875).abs() < 1e-6);
    }

    #[test]
    fn ratio_never_below_one_for_small_screens() {
        // If native is already smaller than target, we don't upscale (ratio
        // clamped at 1.0, image returned at native size).
        let r = compute_ratio(800, 600, 1024);
        assert!((r - 1.0).abs() < 1e-6);
    }

    #[test]
    fn scaled_dims_preserve_aspect() {
        let (w, h) = scaled_dims(2560, 1440, 2.5);
        assert_eq!((w, h), (1024, 576));
    }

    #[test]
    fn round_trip_point_within_one_px() {
        let ratio = 2.5;
        let origin = Point { x: 0, y: 0 };
        let native_in = Point { x: 1280, y: 720 };
        // native -> scaled -> native
        let scaled = Point {
            x: ((native_in.x as f64) / ratio).round() as i32,
            y: ((native_in.y as f64) / ratio).round() as i32,
        };
        let native_out = point_to_native(scaled, ratio, origin);
        assert!((native_out.x - native_in.x).abs() <= 1);
        assert!((native_out.y - native_in.y).abs() <= 1);
    }

    #[test]
    fn point_to_native_offsets_by_monitor_origin() {
        let origin = Point { x: -1920, y: 0 };
        let p = point_to_native(Point { x: 100, y: 200 }, 1.0, origin);
        assert_eq!(p, Point { x: -1820, y: 200 });
    }

    #[test]
    fn point_in_bounds_basic() {
        assert!(point_in_scaled_bounds(Point { x: 0, y: 0 }, 1024, 576));
        assert!(point_in_scaled_bounds(Point { x: 1023, y: 575 }, 1024, 576));
        assert!(!point_in_scaled_bounds(Point { x: 1024, y: 0 }, 1024, 576));
        assert!(!point_in_scaled_bounds(Point { x: -1, y: 0 }, 1024, 576));
    }
}
