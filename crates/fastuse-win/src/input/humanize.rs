//! Tier 1 humanization: Bezier-curve mouse motion + per-keystroke jitter.
//! Defeats web behavioral bot detection (Cloudflare, hCaptcha, Google's
//! Play Console heuristics). Does NOT bypass kernel-mode anti-cheat or
//! Windows' LLMHF_INJECTED flag — see spec § Anti-cheat (out of scope).

use crate::input::backend::Point;

/// Sample a cubic Bezier from `p0` to `p3` with two control points offset
/// perpendicular to the path. Returns `n_samples` interpolated points.
/// `n_samples` includes both endpoints (≥ 2).
pub fn bezier_path(p0: Point, p3: Point, n_samples: usize, jitter: f32) -> Vec<Point> {
    let n = n_samples.max(2);
    let dx = (p3.x - p0.x) as f32;
    let dy = (p3.y - p0.y) as f32;
    // Perpendicular vector for control points; offset = ~15% of distance.
    let dist = (dx * dx + dy * dy).sqrt();
    let perp = (-dy / dist.max(1.0), dx / dist.max(1.0));
    let offset = dist * 0.15;
    // Two control points pulled toward the perpendicular at 1/3 and 2/3.
    let p1 = (
        p0.x as f32 + dx * 0.33 + perp.0 * offset * 0.5,
        p0.y as f32 + dy * 0.33 + perp.1 * offset * 0.5,
    );
    let p2 = (
        p0.x as f32 + dx * 0.66 - perp.0 * offset * 0.3,
        p0.y as f32 + dy * 0.66 - perp.1 * offset * 0.3,
    );

    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f32 / (n - 1) as f32;
        // Bell-curve velocity: ease-in-out (cosine-based).
        let t = 0.5 - 0.5 * (std::f32::consts::PI * t).cos();
        let one_t = 1.0 - t;
        let bx = one_t.powi(3) * p0.x as f32
            + 3.0 * one_t.powi(2) * t * p1.0
            + 3.0 * one_t * t.powi(2) * p2.0
            + t.powi(3) * p3.x as f32;
        let by = one_t.powi(3) * p0.y as f32
            + 3.0 * one_t.powi(2) * t * p1.1
            + 3.0 * one_t * t.powi(2) * p2.1
            + t.powi(3) * p3.y as f32;
        // Per-sample pixel jitter scaled by `jitter` factor (1 px max at 1.0).
        let jx = if jitter > 0.0 { (deterministic_jitter(i, 0) * jitter) as i32 } else { 0 };
        let jy = if jitter > 0.0 { (deterministic_jitter(i, 1) * jitter) as i32 } else { 0 };
        out.push(Point { x: bx.round() as i32 + jx, y: by.round() as i32 + jy });
    }
    out
}

/// Tiny deterministic per-sample jitter. Range: roughly -1.5..1.5 (px).
/// Deterministic so tests are reproducible; for production randomness wrap a
/// real RNG at the call site if needed.
fn deterministic_jitter(i: usize, axis: u8) -> f32 {
    let h = (i as u64).wrapping_mul(2654435761).wrapping_add(axis as u64 * 73);
    ((h % 31) as f32 - 15.0) / 10.0
}

/// Recommended sample count for a motion of `dist` pixels at ~60Hz over
/// `duration_ms`. Returns ≥ 2.
pub fn sample_count(dist_px: f32, duration_ms: u32) -> usize {
    // 60 samples per second; at least one per 10px of distance.
    let by_time = (duration_ms as f32 / (1000.0 / 60.0)).round() as usize;
    let by_dist = (dist_px / 10.0).ceil() as usize;
    by_time.max(by_dist).max(2)
}

/// Recommended motion duration for `dist_px`. Short moves: ~150ms; cross-screen
/// (≥1500px): ~500ms. Linear-ish in between.
pub fn motion_duration_ms(dist_px: f32) -> u32 {
    let clamped = dist_px.clamp(0.0, 1500.0);
    150 + ((clamped / 1500.0) * 350.0) as u32
}

/// Per-key interval generator for typing. Returns intervals (ms) for `n_keys`
/// keystrokes, drawn from a normal distribution (mean, stddev). Adds a
/// natural longer pause every 8-15 keys.
pub fn typing_intervals(n_keys: usize, mean_ms: u32, stddev_ms: u32) -> Vec<u32> {
    let mut out = Vec::with_capacity(n_keys);
    for i in 0..n_keys {
        let base = sample_normal(i, mean_ms as f32, stddev_ms as f32) as u32;
        // Natural longer pause every ~10 keys (8-15 range).
        let extra = if i > 0 && i % (8 + (i % 8)) == 0 {
            200 + (deterministic_jitter(i, 7).abs() * 200.0) as u32
        } else {
            0
        };
        out.push((base + extra).max(10));
    }
    out
}

/// Box-Muller-ish deterministic normal sample at index `i`.
fn sample_normal(i: usize, mean: f32, stddev: f32) -> f32 {
    let u1 = ((i as u64 * 9301 + 49297) % 233280) as f32 / 233280.0;
    let u2 = (((i + 7) as u64 * 4159 + 12349) % 233280) as f32 / 233280.0;
    let z = (-2.0 * u1.max(1e-6).ln()).sqrt() * (2.0 * std::f32::consts::PI * u2).cos();
    mean + stddev * z
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bezier_path_endpoints_match() {
        let p0 = Point { x: 0, y: 0 };
        let p3 = Point { x: 100, y: 100 };
        let path = bezier_path(p0, p3, 10, 0.0);
        assert_eq!(path.first(), Some(&p0));
        assert_eq!(path.last(), Some(&p3));
    }

    #[test]
    fn bezier_path_min_two_samples() {
        let path = bezier_path(Point { x: 0, y: 0 }, Point { x: 10, y: 10 }, 0, 0.0);
        assert_eq!(path.len(), 2);
    }

    #[test]
    fn bezier_path_curves_not_straight_with_jitter_zero() {
        // Even with zero jitter, the Bezier should curve — the perpendicular
        // control offset means the midpoint should be off the straight line.
        let p0 = Point { x: 0, y: 0 };
        let p3 = Point { x: 200, y: 0 };
        let path = bezier_path(p0, p3, 21, 0.0);
        let mid = path[10];
        // Straight line midpoint would be (100, 0). Bezier should differ.
        assert!((mid.y).abs() > 0, "midpoint y={} expected non-zero", mid.y);
    }

    #[test]
    fn sample_count_at_least_two() {
        assert!(sample_count(0.0, 0) >= 2);
        assert!(sample_count(1000.0, 200) >= 2);
    }

    #[test]
    fn motion_duration_short_moves_around_150ms() {
        assert!(motion_duration_ms(50.0) < 200);
    }

    #[test]
    fn motion_duration_long_moves_around_500ms() {
        assert!(motion_duration_ms(2000.0) >= 450);
    }

    #[test]
    fn typing_intervals_have_correct_count() {
        let v = typing_intervals(20, 80, 30);
        assert_eq!(v.len(), 20);
    }

    #[test]
    fn typing_intervals_have_minimum_floor() {
        let v = typing_intervals(50, 5, 5);
        for &iv in &v {
            assert!(iv >= 10, "interval {} below floor", iv);
        }
    }
}
