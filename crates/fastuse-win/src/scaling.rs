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

use std::time::Instant;

/// Snapshot of one screenshot's scaling state.
#[derive(Debug, Clone)]
pub struct ScaleSnapshot {
    /// Uniform scale ratio (`native / scaled`). Multiply scaled by ratio to get native.
    pub ratio: f64,
    /// Top-left corner of the captured monitor in virtual-desktop coordinates.
    pub monitor_origin: Point,
    /// Native monitor width in pixels.
    pub native_w: u32,
    /// Native monitor height in pixels.
    pub native_h: u32,
    /// Scaled image width (what the model sees).
    pub scaled_w: u32,
    /// Scaled image height (what the model sees).
    pub scaled_h: u32,
    /// Wall-clock time when this snapshot was captured.
    pub captured_at: Instant,
}

/// Per-session stack of scale contexts.
///
/// The top entry is the "current" scale (last screenshot's). `zoom` actions push
/// a new entry; `screenshot` resets to a single-element stack (fullscreen base).
#[derive(Debug, Default)]
pub struct ScaleStack {
    stack: Vec<ScaleSnapshot>,
}

impl ScaleStack {
    /// Create an empty stack.
    pub fn new() -> Self {
        Self { stack: Vec::new() }
    }

    /// Replace the entire stack with a single snapshot (`screenshot` action).
    pub fn reset(&mut self, snap: ScaleSnapshot) {
        self.stack.clear();
        self.stack.push(snap);
    }

    /// Push a snapshot on top (`zoom` action).
    pub fn push(&mut self, snap: ScaleSnapshot) {
        self.stack.push(snap);
    }

    /// Pop one zoom level.
    ///
    /// No-op when the stack is empty or has only the fullscreen base entry —
    /// we never pop the base. To get back to fullscreen, call `reset` with a
    /// fresh screenshot snapshot.
    pub fn pop_zoom(&mut self) {
        if self.stack.len() > 1 {
            self.stack.pop();
        }
    }

    /// Return a reference to the current (top-of-stack) snapshot, or `None` if empty.
    pub fn current(&self) -> Option<&ScaleSnapshot> {
        self.stack.last()
    }

    /// Translate a scaled image point to native virtual-desktop coordinates.
    ///
    /// Returns `Err(ScaleError::NoContext)` when the stack is empty and
    /// `Err(ScaleError::OutOfBounds)` when `scaled` falls outside the captured
    /// image dimensions.
    pub fn translate(&self, scaled: Point) -> Result<Point, ScaleError> {
        let s = self.stack.last().ok_or(ScaleError::NoContext)?;
        if !point_in_scaled_bounds(scaled, s.scaled_w, s.scaled_h) {
            return Err(ScaleError::OutOfBounds {
                point: scaled,
                bounds: (s.scaled_w, s.scaled_h),
            });
        }
        Ok(point_to_native(scaled, s.ratio, s.monitor_origin))
    }
}

/// Errors returned by [`ScaleStack::translate`].
#[derive(Debug, thiserror::Error)]
pub enum ScaleError {
    /// No snapshot has been pushed yet — call `screenshot` first.
    #[error("no scale context — call screenshot first")]
    NoContext,
    /// The scaled point lies outside the captured image bounds.
    #[error("scaled point {point:?} out of bounds {bounds:?}")]
    OutOfBounds {
        /// The out-of-bounds point.
        point: Point,
        /// The `(scaled_w, scaled_h)` of the current snapshot.
        bounds: (u32, u32),
    },
}

#[cfg(test)]
mod state_tests {
    use super::*;

    fn snap(ratio: f64, origin: (i32, i32), nw: u32, nh: u32) -> ScaleSnapshot {
        let (sw, sh) = scaled_dims(nw, nh, ratio);
        ScaleSnapshot {
            ratio,
            monitor_origin: Point { x: origin.0, y: origin.1 },
            native_w: nw,
            native_h: nh,
            scaled_w: sw,
            scaled_h: sh,
            captured_at: Instant::now(),
        }
    }

    #[test]
    fn empty_stack_returns_no_context_error() {
        let s = ScaleStack::new();
        assert!(matches!(s.translate(Point { x: 0, y: 0 }), Err(ScaleError::NoContext)));
    }

    #[test]
    fn reset_then_translate_works() {
        let mut s = ScaleStack::new();
        s.reset(snap(2.5, (0, 0), 2560, 1440));
        let native = s.translate(Point { x: 100, y: 100 }).unwrap();
        assert_eq!(native, Point { x: 250, y: 250 });
    }

    #[test]
    fn out_of_bounds_returns_error() {
        let mut s = ScaleStack::new();
        s.reset(snap(2.5, (0, 0), 2560, 1440)); // scaled = 1024x576
        assert!(matches!(
            s.translate(Point { x: 2000, y: 0 }),
            Err(ScaleError::OutOfBounds { .. })
        ));
    }

    #[test]
    fn zoom_push_uses_top_of_stack() {
        let mut s = ScaleStack::new();
        s.reset(snap(2.5, (0, 0), 2560, 1440));
        // Simulate a zoom that gives ratio 0.5 (upscaled region).
        s.push(snap(0.5, (500, 300), 200, 150));
        let native = s.translate(Point { x: 50, y: 50 }).unwrap();
        // 50 * 0.5 + 500 = 525; 50 * 0.5 + 300 = 325
        assert_eq!(native, Point { x: 525, y: 325 });
    }

    #[test]
    fn pop_zoom_keeps_at_least_base() {
        let mut s = ScaleStack::new();
        s.reset(snap(2.5, (0, 0), 2560, 1440));
        s.push(snap(0.5, (500, 300), 200, 150));
        s.pop_zoom();
        s.pop_zoom();
        assert!(s.current().is_some()); // base still there
    }

    #[test]
    fn secondary_monitor_origin_offset() {
        let mut s = ScaleStack::new();
        // Secondary monitor to the left at native origin (-1920, 0).
        s.reset(snap(1.875, (-1920, 0), 1920, 1080));
        let native = s.translate(Point { x: 100, y: 100 }).unwrap();
        // (100.0 * 1.875).round() = 188; 188 + (-1920) = -1732
        // 100 * 1.875 + 0 = 187.5 → 188
        assert_eq!(native, Point { x: -1732, y: 188 });
    }
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
