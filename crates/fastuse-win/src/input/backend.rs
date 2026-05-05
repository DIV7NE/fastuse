//! InputBackend abstraction. Default impl is `SendInputBackend`; a future
//! `HardwareHidBackend` (USB-HID via Pico) can plug in without touching the
//! daemon.

use crate::input::sendinput::Modifiers;

/// Mouse button enum, mirrors fastuse_proto::wire but kept local to avoid the
/// crate dependency cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    /// Primary (left) mouse button.
    Left,
    /// Secondary (right) mouse button.
    Right,
    /// Middle mouse button / scroll wheel click.
    Middle,
}

/// Screen-space point in physical pixels (virtual-desktop origin).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Point {
    /// Horizontal pixel coordinate.
    pub x: i32,
    /// Vertical pixel coordinate.
    pub y: i32,
}

/// Scroll wheel direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollDirection {
    /// Scroll up (away from user).
    Up,
    /// Scroll down (toward user).
    Down,
    /// Scroll left.
    Left,
    /// Scroll right.
    Right,
}

/// Motion profile for `MouseMove` / `Drag`.
#[derive(Debug, Clone, Copy)]
pub struct MotionProfile {
    /// True = Bezier-interpolated; false = single SendInput teleport.
    pub humanize: bool,
    /// Total motion duration. None = derive from distance.
    pub duration_ms: Option<u32>,
    /// 0.0 = perfectly smooth Bezier; up to 1.0 = aggressive sample-level jitter.
    pub jitter: f32,
}

impl Default for MotionProfile {
    fn default() -> Self {
        Self { humanize: true, duration_ms: None, jitter: 0.5 }
    }
}

/// Typing profile for `KeyType`.
#[derive(Debug, Clone, Copy)]
pub struct TypingProfile {
    /// True = inter-keystroke jitter enabled.
    pub humanize: bool,
    /// Mean inter-keystroke interval in milliseconds.
    pub mean_interval_ms: u32,
    /// Standard deviation of inter-keystroke interval in milliseconds.
    pub interval_stddev_ms: u32,
}

impl Default for TypingProfile {
    fn default() -> Self {
        Self { humanize: true, mean_interval_ms: 80, interval_stddev_ms: 30 }
    }
}

/// All input actions a backend must support. Keep this intentionally small —
/// composition (e.g., right-drag) lives in the daemon, not the backend.
#[derive(Debug, Clone)]
pub enum InputAction {
    /// Move the cursor from `from` to `to` using the given motion profile.
    MouseMove {
        /// Starting cursor position.
        from: Point,
        /// Target cursor position.
        to: Point,
        /// Motion profile controlling humanization.
        profile: MotionProfile,
    },
    /// Click at `at` with the given button and optional modifiers.
    MouseClick {
        /// Position to click.
        at: Point,
        /// Which mouse button to press.
        button: MouseButton,
        /// Number of clicks (1 = single, 2 = double, etc.).
        count: u8,
        /// Modifier keys to hold during the click.
        modifiers: Modifiers,
        /// Whether to apply humanized timing.
        humanize: bool,
    },
    /// Press `button` down at `at` without releasing.
    MouseDown {
        /// Position for the button-down event.
        at: Point,
        /// Which mouse button to press.
        button: MouseButton,
    },
    /// Release `button` at `at`.
    MouseUp {
        /// Position for the button-up event.
        at: Point,
        /// Which mouse button to release.
        button: MouseButton,
    },
    /// Drag from `from` to `to` holding `button`.
    Drag {
        /// Drag start position.
        from: Point,
        /// Drag end position.
        to: Point,
        /// Button held during drag.
        button: MouseButton,
        /// Motion profile.
        profile: MotionProfile,
        /// Modifier keys held during drag.
        modifiers: Modifiers,
    },
    /// Type `text` as a sequence of Unicode key events.
    KeyType {
        /// The text to type.
        text: String,
        /// Typing profile controlling humanization.
        profile: TypingProfile,
    },
    /// Press and release a chord of virtual-key codes.
    KeyChord {
        /// Virtual-key (VK) codes to press simultaneously.
        keys: Vec<u16>,
        /// Optional hold duration in milliseconds before releasing.
        hold_ms: Option<u32>,
    },
    /// Scroll at `at` in `direction` by `amount` ticks.
    Scroll {
        /// Position at which to scroll.
        at: Point,
        /// Direction to scroll.
        direction: ScrollDirection,
        /// Number of wheel ticks.
        amount: i32,
    },
}

/// What this backend can do.
#[derive(Debug, Clone, Copy, Default)]
pub struct Capabilities {
    /// True if the backend Bezier-interpolates mouse motion.
    pub humanized_motion: bool,
    /// True if the backend applies per-keystroke timing jitter.
    pub humanized_typing: bool,
    /// True if the backend supports holding modifier keys during drags.
    pub modifier_drags: bool,
    /// True if the backend can inject gamepad events.
    pub gamepad: bool,
}

/// Errors that can arise from dispatching an [`InputAction`].
#[derive(Debug, thiserror::Error)]
pub enum InputError {
    /// The underlying input API rejected the event.
    #[error("input dispatch failed: {0}")]
    Dispatch(String),
    /// The backend is not available on this machine.
    #[error("backend unavailable: {0}")]
    Unavailable(String),
}

/// Abstraction over input injection backends. The default implementation is
/// [`crate::input::sendinput_backend::SendInputBackend`]. A future
/// `HardwareHidBackend` can be plugged in without modifying the daemon.
pub trait InputBackend: Send + Sync {
    /// Dispatch a single input action synchronously.
    fn dispatch(&self, action: InputAction) -> Result<(), InputError>;
    /// Return the static capability set for this backend.
    fn capabilities(&self) -> Capabilities;
    /// Short name for logging / telemetry (e.g. `"sendinput"`, `"hid"`).
    fn name(&self) -> &'static str;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Records every dispatched action. Used by tests.
    pub struct MockBackend {
        log: Mutex<Vec<InputAction>>,
    }

    impl MockBackend {
        pub fn new() -> Self { Self { log: Mutex::new(Vec::new()) } }
        pub fn log(&self) -> Vec<InputAction> { self.log.lock().unwrap().clone() }
    }

    impl InputBackend for MockBackend {
        fn dispatch(&self, action: InputAction) -> Result<(), InputError> {
            self.log.lock().unwrap().push(action);
            Ok(())
        }
        fn capabilities(&self) -> Capabilities { Capabilities::default() }
        fn name(&self) -> &'static str { "mock" }
    }

    #[test]
    fn mock_backend_records_dispatches() {
        let b = MockBackend::new();
        b.dispatch(InputAction::MouseClick {
            at: Point { x: 10, y: 20 },
            button: MouseButton::Left,
            count: 1,
            modifiers: Modifiers::default(),
            humanize: false,
        }).unwrap();
        let log = b.log();
        assert_eq!(log.len(), 1);
        assert!(matches!(log[0], InputAction::MouseClick { .. }));
    }

    #[test]
    fn motion_profile_defaults_humanized() {
        let p = MotionProfile::default();
        assert!(p.humanize);
        assert_eq!(p.duration_ms, None);
    }
}
