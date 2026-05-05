//! SendInput-backed implementation of [`InputBackend`]. v1: pure dispatch, no
//! humanization (Bezier curves and timing jitter ship in the next task).

use crate::input::backend::{
    Capabilities, InputAction, InputBackend, InputError,
};

/// [`InputBackend`] that delegates every action directly to Win32 `SendInput`.
///
/// Humanization (Bezier motion, keystroke jitter) is not applied at this
/// stage — that arrives in Task 4 via the humanize layer.
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
            MouseMove { to, .. } => si::mouse_move_absolute(to.x, to.y)
                .map_err(|e| InputError::Dispatch(e.to_string())),
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
            Drag { from, to, button, modifiers, .. } => {
                si::mouse_move_absolute(from.x, from.y)
                    .map_err(|e| InputError::Dispatch(e.to_string()))?;
                let btn = match button {
                    MouseButton::Left => si::Button::Left,
                    MouseButton::Right => si::Button::Right,
                    MouseButton::Middle => si::Button::Middle,
                };
                si::mouse_down(btn).map_err(|e| InputError::Dispatch(e.to_string()))?;
                si::mouse_move_absolute(to.x, to.y)
                    .map_err(|e| InputError::Dispatch(e.to_string()))?;
                si::mouse_up(btn).map_err(|e| InputError::Dispatch(e.to_string()))?;
                let _ = modifiers;
                Ok(())
            }
            KeyType { text, .. } => si::type_text(&text)
                .map_err(|e| InputError::Dispatch(e.to_string())),
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
            humanized_motion: false,  // bumped to true in next task
            humanized_typing: false,
            modifier_drags: true,
            gamepad: false,
        }
    }

    fn name(&self) -> &'static str { "sendinput" }
}
