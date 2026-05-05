//! Safe `SendInput` wrapper.
//!
//! Hard rules (CONTEXT.md):
//!   * NEVER `keybd_event` / `mouse_event`. SendInput only.
//!   * Always check the SendInput return value; `< nInputs` ⇒ typed error.
//!   * Mouse absolute coords use `MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK`
//!     and are normalized over the full virtual-screen rect (PITFALLS #5).
//!   * Unicode characters are typed via `KEYEVENTF_UNICODE`; surrogate pairs
//!     are split into two INPUT entries.
//!
//! All `unsafe` blocks carry `// SAFETY:` notes per `unsafe_op_in_unsafe_fn`.

use std::sync::Mutex;

use once_cell::sync::Lazy;
use thiserror::Error;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK,
    MOUSEEVENTF_WHEEL, MOUSEINPUT, MOUSE_EVENT_FLAGS, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use fastuse_proto::{Error as ProtoError, ErrorCode};

/// SendInput failures.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum InputErr {
    /// SendInput accepted fewer events than we sent. Likely a low-level hook.
    #[error("SendInput accepted {sent}/{expected} events; likely low-level hook intercepting")]
    BlockedHang {
        /// Events actually accepted.
        sent: u32,
        /// Events submitted.
        expected: u32,
    },
}

impl From<InputErr> for ProtoError {
    fn from(e: InputErr) -> ProtoError {
        match &e {
            InputErr::BlockedHang { sent, expected } => ProtoError::new(
                ErrorCode::InputBlockedHang,
                format!("SendInput accepted {sent}/{expected} events"),
            )
            .with_hint(
                "a low-level keyboard/mouse hook (anti-cheat, password manager, screen reader) is dropping our input"
                    .to_string(),
            ),
        }
    }
}

/// Mock SendInput recorder. Only used when the `mock-sendinput` cargo
/// feature is enabled. Production builds use [`send`] which calls real
/// `SendInput`.
pub struct MockSink {
    inner: Mutex<Vec<RecordedInput>>,
}

/// Type-erased echo of an `INPUT` structure for tests. Carries the fields
/// the test actually inspects so it doesn't have to import `windows`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordedInput {
    /// Keyboard event.
    Key {
        /// Virtual-key code (0 if Unicode-typed).
        vk: u16,
        /// Scan code (Unicode scalar when KEYEVENTF_UNICODE is set).
        scan: u16,
        /// Raw flags (`KEYEVENTF_*`).
        flags: u32,
    },
    /// Mouse event.
    Mouse {
        /// dx in the SendInput frame (normalized for ABSOLUTE).
        dx: i32,
        /// dy in the SendInput frame.
        dy: i32,
        /// `mouseData` (wheel delta or XBUTTON id).
        mouse_data: u32,
        /// Raw flags (`MOUSEEVENTF_*`).
        flags: u32,
    },
}

impl MockSink {
    fn new() -> Self {
        Self { inner: Mutex::new(Vec::new()) }
    }

    /// Drain and return everything recorded since the last call.
    pub fn take(&self) -> Vec<RecordedInput> {
        std::mem::take(&mut *self.inner.lock().unwrap())
    }

    /// Push a recorded event (called by [`send`] when `mock-sendinput`).
    fn push(&self, ev: RecordedInput) {
        self.inner.lock().unwrap().push(ev);
    }
}

static MOCK_SINK: Lazy<MockSink> = Lazy::new(MockSink::new);

/// Access the global mock sink (test helpers).
pub fn mock_sink() -> &'static MockSink {
    &MOCK_SINK
}

/// Submit one or more INPUT events to Windows (or the mock sink under the
/// `mock-sendinput` feature).
pub fn send(events: &[INPUT]) -> Result<u32, InputErr> {
    if events.is_empty() {
        return Ok(0);
    }

    #[cfg(feature = "mock-sendinput")]
    {
        for ev in events {
            // SAFETY: we ONLY read; INPUT_0 is a union, but the discriminant
            // (`type`) tells us which member is initialized.
            let recorded = unsafe { record_one(ev) };
            MOCK_SINK.push(recorded);
        }
        return Ok(events.len() as u32);
    }

    #[cfg(not(feature = "mock-sendinput"))]
    {
        // SAFETY: events is a valid slice; cbSize is the size of one INPUT.
        let sent =
            unsafe { SendInput(events, std::mem::size_of::<INPUT>() as i32) };
        let expected = events.len() as u32;
        if sent < expected {
            return Err(InputErr::BlockedHang { sent, expected });
        }
        Ok(sent)
    }
}

#[cfg(feature = "mock-sendinput")]
unsafe fn record_one(ev: &INPUT) -> RecordedInput {
    if ev.r#type == INPUT_KEYBOARD {
        // SAFETY: discriminant says ki is initialized.
        let ki = unsafe { ev.Anonymous.ki };
        RecordedInput::Key {
            vk: ki.wVk.0,
            scan: ki.wScan,
            flags: ki.dwFlags.0,
        }
    } else {
        // SAFETY: discriminant says mi is initialized.
        let mi = unsafe { ev.Anonymous.mi };
        RecordedInput::Mouse {
            dx: mi.dx,
            dy: mi.dy,
            mouse_data: mi.mouseData,
            flags: mi.dwFlags.0,
        }
    }
}

/// Build a keyboard `INPUT` with a virtual-key code.
pub fn key_vk(vk: u16, up: bool) -> INPUT {
    let mut flags = KEYBD_EVENT_FLAGS(0);
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Build a keyboard `INPUT` with a single Unicode scalar (BMP only). Use
/// [`key_unicode_str`] for full-codepoint emission with surrogate pairs.
pub fn key_unicode(scan: u16, up: bool) -> INPUT {
    let mut flags = KEYEVENTF_UNICODE;
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Encode a string as a sequence of `KEYEVENTF_UNICODE` DOWN+UP INPUT events,
/// splitting non-BMP scalars into UTF-16 surrogate pairs.
///
/// Returns DOWN/UP for each UTF-16 code unit so a `🎉` (U+1F389) becomes
/// `[high_down, high_up, low_down, low_up]` (4 events), and `a` becomes
/// `[a_down, a_up]` (2 events).
pub fn key_unicode_str(s: &str) -> Vec<INPUT> {
    let mut out = Vec::with_capacity(s.len() * 4);
    let mut buf = [0u16; 2];
    for c in s.chars() {
        let units = c.encode_utf16(&mut buf);
        for &u in units.iter() {
            out.push(key_unicode(u, false));
            out.push(key_unicode(u, true));
        }
    }
    out
}

/// Build a mouse `INPUT` for absolute movement to `(x, y)` in physical
/// pixels (virtual-desktop origin). Normalizes to 0..65535 over the
/// virtual-screen rect and sets `MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK`.
pub fn mouse_absolute(x: i32, y: i32, extra_flags: MOUSE_EVENT_FLAGS) -> INPUT {
    let (dx, dy) = normalize_to_virtual_desk(x, y);
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: 0,
                dwFlags: MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK | extra_flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Build a mouse `INPUT` for a wheel/hwheel scroll. `delta` is signed;
/// positive = up / right.
pub fn mouse_wheel(horizontal: bool, delta: i32) -> INPUT {
    let flags = if horizontal { MOUSEEVENTF_HWHEEL } else { MOUSEEVENTF_WHEEL };
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: delta as u32,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Build a button DOWN flag for a [`MouseButton`-like role].
pub fn mouse_button_flags(button: fastuse_proto::MouseButton, up: bool) -> MOUSE_EVENT_FLAGS {
    use fastuse_proto::MouseButton::*;
    match (button, up) {
        (Left, false) => MOUSEEVENTF_LEFTDOWN,
        (Left, true) => MOUSEEVENTF_LEFTUP,
        (Right, false) => MOUSEEVENTF_RIGHTDOWN,
        (Right, true) => MOUSEEVENTF_RIGHTUP,
        (Middle, false) => MOUSEEVENTF_MIDDLEDOWN,
        (Middle, true) => MOUSEEVENTF_MIDDLEUP,
    }
}

/// Build a button INPUT at `(x, y)` (absolute). Rolls up `mouse_absolute`
/// + the button DOWN/UP flag in one call.
pub fn mouse_button_absolute(
    button: fastuse_proto::MouseButton,
    up: bool,
    x: i32,
    y: i32,
) -> INPUT {
    mouse_absolute(x, y, MOUSEEVENTF_MOVE | mouse_button_flags(button, up))
}

/// Snapshot of the virtual-screen rect from `GetSystemMetrics`. We compute
/// this lazily per-call (each call is a few ns and avoids races with display
/// changes).
fn virtual_screen() -> (i32, i32, i32, i32) {
    // SAFETY: GetSystemMetrics is always safe; SM_* are stable indices.
    unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    }
}

/// Normalize `(x, y)` physical pixels (virtual-desktop origin) to the
/// `0..65535` ABSOLUTE space SendInput expects. Mid-rect values round to
/// even bins so a "click at the centre" lands at the centre.
pub fn normalize_to_virtual_desk(x: i32, y: i32) -> (i32, i32) {
    let (vsx, vsy, vscx, vscy) = virtual_screen();
    normalize_with_rect(x, y, vsx, vsy, vscx, vscy)
}

/// Pure normalization helper, separated for unit tests.
fn normalize_with_rect(
    x: i32,
    y: i32,
    vsx: i32,
    vsy: i32,
    vscx: i32,
    vscy: i32,
) -> (i32, i32) {
    // (x - vsx) / (vscx - 1) ∈ [0..1]; scale to 65535.
    // Use i64 arithmetic to avoid overflow on full HD multi-monitor virtual
    // screens (8K wide × 65535 ≈ 5e8, fits i64).
    let denom_x = (vscx - 1).max(1) as i64;
    let denom_y = (vscy - 1).max(1) as i64;
    let dx = (((x - vsx) as i64) * 65535 + denom_x / 2) / denom_x;
    let dy = (((y - vsy) as i64) * 65535 + denom_y / 2) / denom_y;
    (dx as i32, dy as i32)
}

// ── High-level shims for InputBackend ────────────────────────────────────────
//
// These thin wrappers present a stable API surface that `SendInputBackend`
// calls.  They do NOT duplicate the internals of the existing primitives above;
// they compose them.

/// Mouse button discriminant used by the high-level shim API. Kept local to
/// avoid pulling `fastuse_proto` into the trait boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    /// Primary (left) button.
    Left,
    /// Secondary (right) button.
    Right,
    /// Middle button.
    Middle,
}

impl Button {
    fn to_proto(self) -> fastuse_proto::MouseButton {
        match self {
            Button::Left => fastuse_proto::MouseButton::Left,
            Button::Right => fastuse_proto::MouseButton::Right,
            Button::Middle => fastuse_proto::MouseButton::Middle,
        }
    }
}

/// Scroll direction discriminant for the high-level shim API.
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

/// Modifier keys held during a click/drag. An empty `Modifiers` means no
/// modifiers are held — the common case.
#[derive(Debug, Clone, Default)]
pub struct Modifiers {
    /// Hold the left Ctrl key.
    pub ctrl: bool,
    /// Hold the left Shift key.
    pub shift: bool,
    /// Hold the left Alt key.
    pub alt: bool,
    /// Hold the left Win key.
    pub win: bool,
}

const VK_LCONTROL: u16 = 0xA2;
const VK_LSHIFT: u16 = 0xA0;
const VK_LMENU: u16 = 0xA4;
const VK_LWIN: u16 = 0x5B;

/// Move the cursor to `(x, y)` in physical pixels (virtual-desktop origin)
/// without pressing any buttons.
pub fn mouse_move_absolute(x: i32, y: i32) -> Result<(), InputErr> {
    send(&[mouse_absolute(x, y, MOUSEEVENTF_MOVE)])?;
    Ok(())
}

/// Press and release `button` at the current cursor position `count` times,
/// optionally holding `modifiers` throughout.
pub fn mouse_click(button: Button, count: u32, modifiers: Modifiers) -> Result<(), InputErr> {
    let mod_events = build_modifier_events(&modifiers, false);
    let mod_up_events = build_modifier_events(&modifiers, true);

    let mut events = mod_events;
    let btn = button.to_proto();
    for _ in 0..count.max(1) {
        events.push(INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: 0,
                    dy: 0,
                    mouseData: 0,
                    dwFlags: mouse_button_flags(btn, false),
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        });
        events.push(INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: 0,
                    dy: 0,
                    mouseData: 0,
                    dwFlags: mouse_button_flags(btn, true),
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        });
    }
    events.extend(mod_up_events);
    send(&events)?;
    Ok(())
}

/// Press `button` down at the current cursor position (no release).
pub fn mouse_down(button: Button) -> Result<(), InputErr> {
    let btn = button.to_proto();
    send(&[INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: 0,
                dwFlags: mouse_button_flags(btn, false),
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }])?;
    Ok(())
}

/// Release `button` at the current cursor position.
pub fn mouse_up(button: Button) -> Result<(), InputErr> {
    let btn = button.to_proto();
    send(&[INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: 0,
                dwFlags: mouse_button_flags(btn, true),
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }])?;
    Ok(())
}

/// Emit each character in `text` as a KEYEVENTF_UNICODE DOWN+UP pair.
pub fn type_text(text: &str) -> Result<(), InputErr> {
    let events = key_unicode_str(text);
    if events.is_empty() {
        return Ok(());
    }
    for chunk in events.chunks(256) {
        send(chunk)?;
    }
    Ok(())
}

/// Press and release a chord of virtual-key codes. If `hold_ms` is provided the
/// keys are held for that many milliseconds (implemented as a sleep) between
/// down and up; otherwise down + up are sent back-to-back.
pub fn key_chord(keys: &[u16], hold_ms: Option<u32>) -> Result<(), InputErr> {
    if keys.is_empty() {
        return Ok(());
    }
    let down: Vec<INPUT> = keys.iter().map(|&vk| key_vk(vk, false)).collect();
    send(&down)?;
    if let Some(ms) = hold_ms {
        std::thread::sleep(std::time::Duration::from_millis(ms as u64));
    }
    let up: Vec<INPUT> = keys.iter().rev().map(|&vk| key_vk(vk, true)).collect();
    send(&up)?;
    Ok(())
}

/// Scroll at the current cursor position. `amount` is the number of wheel
/// ticks (each tick = WHEEL_DELTA = 120 units).
pub fn scroll(direction: ScrollDirection, amount: i32) -> Result<(), InputErr> {
    const WHEEL_DELTA: i32 = 120;
    let (horizontal, sign) = match direction {
        ScrollDirection::Up => (false, 1),
        ScrollDirection::Down => (false, -1),
        ScrollDirection::Right => (true, 1),
        ScrollDirection::Left => (true, -1),
    };
    send(&[mouse_wheel(horizontal, sign * amount * WHEEL_DELTA)])?;
    Ok(())
}

/// Build modifier key-down or key-up events for a [`Modifiers`] set.
fn build_modifier_events(modifiers: &Modifiers, up: bool) -> Vec<INPUT> {
    let mut out = Vec::with_capacity(4);
    if modifiers.ctrl  { out.push(key_vk(VK_LCONTROL, up)); }
    if modifiers.shift { out.push(key_vk(VK_LSHIFT,   up)); }
    if modifiers.alt   { out.push(key_vk(VK_LMENU,    up)); }
    if modifiers.win   { out.push(key_vk(VK_LWIN,     up)); }
    out
}

// ── End high-level shims ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_top_left_of_primary() {
        // Virtual screen rooted at (0,0), 1920×1080 single monitor.
        let (dx, dy) = normalize_with_rect(0, 0, 0, 0, 1920, 1080);
        assert_eq!(dx, 0);
        assert_eq!(dy, 0);
    }

    #[test]
    fn normalize_bottom_right_of_primary() {
        let (dx, dy) = normalize_with_rect(1919, 1079, 0, 0, 1920, 1080);
        assert_eq!(dx, 65535);
        assert_eq!(dy, 65535);
    }

    #[test]
    fn normalize_centre_of_primary() {
        let (dx, dy) = normalize_with_rect(960, 540, 0, 0, 1920, 1080);
        // Pixel-grid 1920 has 1919 gaps; pixel 960 maps to 960/1919 of full
        // range. Allow ~50 units of slack (≈0.08% of 65535).
        assert!((dx - 32768).abs() < 50, "dx = {dx}");
        assert!((dy - 32768).abs() < 50, "dy = {dy}");
    }

    #[test]
    fn normalize_with_negative_origin_multimon() {
        // Two monitors: primary 1920×1080 at (0,0), secondary 1920×1080 to the
        // LEFT at (-1920, 0). Virtual rect: x=-1920, y=0, cx=3840, cy=1080.
        let (dx, dy) = normalize_with_rect(-1920, 0, -1920, 0, 3840, 1080);
        assert_eq!(dx, 0);
        assert_eq!(dy, 0);
        let (dx, dy) = normalize_with_rect(1919, 1079, -1920, 0, 3840, 1080);
        assert_eq!(dx, 65535);
        assert_eq!(dy, 65535);
        // Origin of primary in this multi-monitor layout — should be roughly
        // half of the dx range. 1920 / 3839 * 65535 ≈ 32777.
        let (dx, _) = normalize_with_rect(0, 0, -1920, 0, 3840, 1080);
        assert!((dx - 32768).abs() < 50, "dx={dx}");
    }

    #[test]
    fn key_unicode_str_handles_bmp_chars() {
        let inputs = key_unicode_str("a");
        assert_eq!(inputs.len(), 2); // down + up
        // SAFETY: we just built these as keyboard inputs.
        unsafe {
            assert_eq!(inputs[0].Anonymous.ki.wScan, b'a' as u16);
            assert_eq!(inputs[0].Anonymous.ki.dwFlags, KEYEVENTF_UNICODE);
            assert_eq!(
                inputs[1].Anonymous.ki.dwFlags,
                KEYEVENTF_UNICODE | KEYEVENTF_KEYUP
            );
        }
    }

    #[test]
    fn key_unicode_str_splits_surrogates() {
        // 🎉 = U+1F389, encoded as UTF-16 surrogate pair (0xD83C, 0xDF89).
        let inputs = key_unicode_str("🎉");
        assert_eq!(inputs.len(), 4); // 2 code units × (down + up)
        // SAFETY: keyboard inputs.
        unsafe {
            assert_eq!(inputs[0].Anonymous.ki.wScan, 0xD83C);
            assert_eq!(inputs[2].Anonymous.ki.wScan, 0xDF89);
        }
    }

    #[test]
    fn key_unicode_str_combined() {
        // "a🎉" → 2 + 4 = 6 events.
        let inputs = key_unicode_str("a🎉");
        assert_eq!(inputs.len(), 6);
    }

    #[test]
    fn key_vk_sets_keyup_flag() {
        let down = key_vk(0x41, false);
        let up = key_vk(0x41, true);
        // SAFETY: keyboard inputs.
        unsafe {
            assert_eq!(down.Anonymous.ki.dwFlags, KEYBD_EVENT_FLAGS(0));
            assert_eq!(up.Anonymous.ki.dwFlags, KEYEVENTF_KEYUP);
            assert_eq!(down.Anonymous.ki.wVk.0, 0x41);
        }
    }

    #[test]
    fn mouse_button_flags_table() {
        use fastuse_proto::MouseButton::*;
        assert_eq!(mouse_button_flags(Left, false), MOUSEEVENTF_LEFTDOWN);
        assert_eq!(mouse_button_flags(Left, true), MOUSEEVENTF_LEFTUP);
        assert_eq!(mouse_button_flags(Right, false), MOUSEEVENTF_RIGHTDOWN);
        assert_eq!(mouse_button_flags(Right, true), MOUSEEVENTF_RIGHTUP);
        assert_eq!(mouse_button_flags(Middle, false), MOUSEEVENTF_MIDDLEDOWN);
        assert_eq!(mouse_button_flags(Middle, true), MOUSEEVENTF_MIDDLEUP);
    }
}
