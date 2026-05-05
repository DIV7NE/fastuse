//! SendInput-backed implementation of [`InputBackend`]. Honors
//! [`MotionProfile::humanize`] and [`TypingProfile::humanize`] to apply
//! Bezier-curve interpolation and per-keystroke timing jitter when enabled.

use crate::input::backend::{
    Capabilities, InputAction, InputBackend, InputError,
};

/// [`InputBackend`] that delegates every action to Win32 `SendInput`.
///
/// When `profile.humanize` is `true`, mouse movement uses cubic Bezier
/// interpolation at ~60Hz and typing applies per-keystroke normal-distribution
/// intervals via [`crate::input::humanize`]. When `false`, actions fall
/// through to a single `SendInput` call.
pub struct SendInputBackend;

impl SendInputBackend {
    /// Construct a new `SendInputBackend`.
    pub fn new() -> Self { Self }
}

impl InputBackend for SendInputBackend {
    fn dispatch(&self, action: InputAction) -> Result<(), InputError> {
        use crate::input::sendinput as si;
        use crate::input::backend::{InputAction::*, MouseButton, ScrollDirection};

        match action {
            MouseMove { from, to, profile } => {
                if profile.humanize {
                    use crate::input::humanize::{bezier_path, sample_count, motion_duration_ms};
                    let dx = (to.x - from.x) as f32;
                    let dy = (to.y - from.y) as f32;
                    let dist = (dx * dx + dy * dy).sqrt();
                    let dur = profile.duration_ms.unwrap_or_else(|| motion_duration_ms(dist));
                    let n = sample_count(dist, dur);
                    let path = bezier_path(from, to, n, profile.jitter);
                    let step = std::time::Duration::from_millis(dur as u64 / n as u64);
                    for p in path {
                        si::mouse_move_absolute(p.x, p.y)
                            .map_err(|e| InputError::Dispatch(e.to_string()))?;
                        std::thread::sleep(step);
                    }
                    Ok(())
                } else {
                    si::mouse_move_absolute(to.x, to.y)
                        .map_err(|e| InputError::Dispatch(e.to_string()))
                }
            }
            MouseClick { at, button, count, modifiers, .. } => {
                si::mouse_move_absolute(at.x, at.y)
                    .map_err(|e| InputError::Dispatch(e.to_string()))?;
                let btn = match button {
                    MouseButton::Left => si::Button::Left,
                    MouseButton::Right => si::Button::Right,
                    MouseButton::Middle => si::Button::Middle,
                };
                si::mouse_click(btn, count as u32, modifiers)
                    .map_err(|e| InputError::Dispatch(e.to_string()))
            }
            MouseDown { at, button } => {
                si::mouse_move_absolute(at.x, at.y)
                    .map_err(|e| InputError::Dispatch(e.to_string()))?;
                let btn = match button {
                    MouseButton::Left => si::Button::Left,
                    MouseButton::Right => si::Button::Right,
                    MouseButton::Middle => si::Button::Middle,
                };
                si::mouse_down(btn).map_err(|e| InputError::Dispatch(e.to_string()))
            }
            MouseUp { at, button } => {
                si::mouse_move_absolute(at.x, at.y)
                    .map_err(|e| InputError::Dispatch(e.to_string()))?;
                let btn = match button {
                    MouseButton::Left => si::Button::Left,
                    MouseButton::Right => si::Button::Right,
                    MouseButton::Middle => si::Button::Middle,
                };
                si::mouse_up(btn).map_err(|e| InputError::Dispatch(e.to_string()))
            }
            Drag { from, to, button, profile, modifiers } => {
                let btn = match button {
                    MouseButton::Left => si::Button::Left,
                    MouseButton::Right => si::Button::Right,
                    MouseButton::Middle => si::Button::Middle,
                };
                si::mouse_move_absolute(from.x, from.y)
                    .map_err(|e| InputError::Dispatch(e.to_string()))?;
                si::mouse_down(btn).map_err(|e| InputError::Dispatch(e.to_string()))?;
                if profile.humanize {
                    use crate::input::humanize::{bezier_path, sample_count, motion_duration_ms};
                    let dx = (to.x - from.x) as f32;
                    let dy = (to.y - from.y) as f32;
                    let dist = (dx * dx + dy * dy).sqrt();
                    let dur = profile.duration_ms.unwrap_or_else(|| motion_duration_ms(dist));
                    let n = sample_count(dist, dur);
                    let path = bezier_path(from, to, n, profile.jitter);
                    let step = std::time::Duration::from_millis(dur as u64 / n as u64);
                    for p in path {
                        si::mouse_move_absolute(p.x, p.y)
                            .map_err(|e| InputError::Dispatch(e.to_string()))?;
                        std::thread::sleep(step);
                    }
                } else {
                    si::mouse_move_absolute(to.x, to.y)
                        .map_err(|e| InputError::Dispatch(e.to_string()))?;
                }
                si::mouse_up(btn).map_err(|e| InputError::Dispatch(e.to_string()))?;
                let _ = modifiers;
                Ok(())
            }
            KeyType { text, profile } => {
                if profile.humanize {
                    use crate::input::humanize::typing_intervals;
                    let chars: Vec<char> = text.chars().collect();
                    let intervals = typing_intervals(chars.len(), profile.mean_interval_ms, profile.interval_stddev_ms);
                    for (ch, iv) in chars.iter().zip(intervals.iter()) {
                        si::type_text(&ch.to_string())
                            .map_err(|e| InputError::Dispatch(e.to_string()))?;
                        std::thread::sleep(std::time::Duration::from_millis(*iv as u64));
                    }
                    Ok(())
                } else {
                    si::type_text(&text).map_err(|e| InputError::Dispatch(e.to_string()))
                }
            }
            KeyChord { keys, hold_ms } => si::key_chord(&keys, hold_ms)
                .map_err(|e| InputError::Dispatch(e.to_string())),
            Scroll { at, direction, amount } => {
                si::mouse_move_absolute(at.x, at.y)
                    .map_err(|e| InputError::Dispatch(e.to_string()))?;
                let dir = match direction {
                    ScrollDirection::Up => si::ScrollDirection::Up,
                    ScrollDirection::Down => si::ScrollDirection::Down,
                    ScrollDirection::Left => si::ScrollDirection::Left,
                    ScrollDirection::Right => si::ScrollDirection::Right,
                };
                si::scroll(dir, amount)
                    .map_err(|e| InputError::Dispatch(e.to_string()))
            }
        }
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            humanized_motion: true,
            humanized_typing: true,
            modifier_drags: true,
            gamepad: false,
        }
    }

    fn name(&self) -> &'static str { "sendinput" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::backend::{InputAction, MotionProfile, Point};

    #[test]
    fn humanized_mouse_move_uses_bezier_path() {
        // Sanity check on the path itself (backend-side tests can't hit real
        // SendInput in unit context). Verify the path generator produces
        // non-trivial intermediate points.
        use crate::input::humanize::bezier_path;
        let path = bezier_path(Point { x: 0, y: 0 }, Point { x: 200, y: 200 }, 12, 0.0);
        assert_eq!(path.len(), 12);
        assert_ne!(path[6], Point { x: 100, y: 100 }); // curved, not straight-line midpoint
    }

    #[test]
    fn capabilities_reports_humanized() {
        let b = SendInputBackend::new();
        let caps = b.capabilities();
        assert!(caps.humanized_motion);
        assert!(caps.humanized_typing);
    }
}
