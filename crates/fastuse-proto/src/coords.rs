//! Phase 2 wire types: coordinates, monitors, windows, mouse buttons, scroll
//! direction.
//!
//! All rectangles are in **physical pixels**, **virtual-desktop origin**
//! (Phase 1 D-24 + CONTEXT.md hard rule). HWND / HMONITOR are carried as `u64`
//! so this crate never depends on `windows`.

use serde::{Deserialize, Serialize};

/// A 2-D point in physical pixels, virtual-desktop space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Point {
    /// X coordinate in physical pixels (virtual-desktop origin).
    pub x: i32,
    /// Y coordinate in physical pixels (virtual-desktop origin).
    pub y: i32,
}

/// Axis-aligned rectangle in physical pixels, virtual-desktop space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Rect {
    /// Top-left x in physical pixels (virtual-desktop origin).
    pub x: i32,
    /// Top-left y in physical pixels (virtual-desktop origin).
    pub y: i32,
    /// Width in physical pixels.
    pub w: i32,
    /// Height in physical pixels.
    pub h: i32,
}

impl Rect {
    /// True if `(px, py)` lies inside the rectangle (inclusive of left/top,
    /// exclusive of right/bottom).
    pub fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && py >= self.y && px < self.x.saturating_add(self.w) && py < self.y.saturating_add(self.h)
    }
}

/// Mouse button identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum MouseButton {
    /// Primary (left) mouse button.
    Left,
    /// Secondary (right) mouse button.
    Right,
    /// Middle (wheel) mouse button.
    Middle,
}

/// Scroll direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ScrollDirection {
    /// Wheel up (positive vertical).
    Up,
    /// Wheel down (negative vertical).
    Down,
    /// Wheel left (negative horizontal — HWHEEL).
    Left,
    /// Wheel right (positive horizontal — HWHEEL).
    Right,
}

/// Information about a single display monitor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct MonitorInfo {
    /// Opaque HMONITOR cast to `u64`.
    pub id: u64,
    /// Human-readable adapter name (`\\.\DISPLAY1`).
    pub name: String,
    /// Bounds in physical pixels (virtual-desktop origin).
    pub bounds: Rect,
    /// `dpi / 96.0` — 1.0 at 100% scaling, 1.5 at 150%, etc.
    pub dpi_scale: f32,
    /// True if this is the primary monitor.
    pub is_primary: bool,
}

/// Information about a single top-level window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct WindowInfo {
    /// Opaque HWND cast to `u64`.
    pub hwnd: u64,
    /// Window title (best-effort via `SendMessageTimeoutW(WM_GETTEXT, ABORT_IF_HUNG)`).
    pub title: String,
    /// Window class name.
    pub class: String,
    /// Owning process basename (e.g. `"notepad.exe"`).
    pub process_name: String,
    /// Owning process id.
    pub pid: u32,
    /// Bounds in physical pixels (virtual-desktop origin).
    pub bounds: Rect,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{decode_frame, encode_frame};
    use std::io::Cursor;

    fn rt<T>(v: &T) -> T
    where
        T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug + Clone,
    {
        let bytes = encode_frame(v).unwrap();
        let mut cur = Cursor::new(bytes);
        decode_frame::<T, _>(&mut cur).unwrap()
    }

    #[test]
    fn rect_round_trips_and_contains() {
        let r = Rect { x: -100, y: 50, w: 200, h: 300 };
        assert_eq!(rt(&r), r);
        assert!(r.contains(-100, 50));
        assert!(r.contains(0, 100));
        assert!(!r.contains(100, 100)); // right edge exclusive
        assert!(!r.contains(-200, 100));
    }

    #[test]
    fn mouse_button_round_trips() {
        for b in [MouseButton::Left, MouseButton::Right, MouseButton::Middle] {
            assert_eq!(rt(&b), b);
        }
    }

    #[test]
    fn scroll_direction_round_trips() {
        for d in [ScrollDirection::Up, ScrollDirection::Down, ScrollDirection::Left, ScrollDirection::Right] {
            assert_eq!(rt(&d), d);
        }
    }

    #[test]
    fn monitor_info_round_trips() {
        let m = MonitorInfo {
            id: 0xDEAD_BEEF,
            name: r"\\.\DISPLAY1".into(),
            bounds: Rect { x: 0, y: 0, w: 2560, h: 1440 },
            dpi_scale: 1.5,
            is_primary: true,
        };
        assert_eq!(rt(&m), m);
    }

    #[test]
    fn window_info_round_trips() {
        let w = WindowInfo {
            hwnd: 0x1234_5678,
            title: "Untitled - Notepad".into(),
            class: "Notepad".into(),
            process_name: "notepad.exe".into(),
            pid: 4242,
            bounds: Rect { x: 100, y: 100, w: 800, h: 600 },
        };
        assert_eq!(rt(&w), w);
    }
}
