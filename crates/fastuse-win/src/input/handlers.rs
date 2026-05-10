//! Per-tool input handlers — click/mouse_*/drag/scroll/type/key/hold_key.
//!
//! Each handler runs on the input STA thread (Phase 1 D-26). The dispatcher
//! routes a request to the input thread; the handler:
//!
//!   1. Calls `uipi::check_foreground_integrity()` (CONTEXT.md hard rule).
//!   2. `press_chord` to establish modifiers (RAII via [`ModifierGuard`]).
//!   3. Optionally `set_cursor_pos` (default-on for absolute clicks).
//!   4. Builds INPUT events and calls `sendinput::send`.
//!   5. `release_chord` (or rely on Drop).
//!
//! `wait` is intentionally NOT here — it lives in the daemon dispatcher so
//! it doesn't occupy the input-thread slot.

use std::time::{Duration, Instant};

use fastuse_proto::{
    Chord, ChordKey, Error as ProtoError, ErrorCode, ModKey, MouseButton, ScrollDirection,
};
use windows::Win32::UI::Input::KeyboardAndMouse::INPUT;

use crate::input::cursor::set_cursor_pos;
use crate::input::modifier_guard::{
    clear_primary_key_up, press_chord, release_all_held, set_primary_key_up,
};
use crate::input::sendinput::{
    key_unicode, key_unicode_str, key_vk, mouse_button_absolute, mouse_button_flags, mouse_wheel, send,
};
use crate::input::uipi::check_foreground_integrity;

const WHEEL_DELTA: i32 = 120;

fn invalid_chord(s: &str, why: impl std::fmt::Display) -> ProtoError {
    ProtoError::new(
        ErrorCode::InvalidChord,
        format!("invalid chord {s:?}: {why}"),
    )
}

fn parse(s: &str) -> Result<Chord, ProtoError> {
    fastuse_proto::parse_chord(s).map_err(|e| invalid_chord(s, e))
}

fn parse_mods(tokens: &[String]) -> Result<Vec<ModKey>, ProtoError> {
    let mut out = Vec::with_capacity(tokens.len());
    for t in tokens {
        let lc: String = t.chars().flat_map(|c| c.to_lowercase()).collect();
        let m = match lc.as_str() {
            "ctrl" => ModKey::Ctrl,
            "shift" => ModKey::Shift,
            "alt" => ModKey::Alt,
            "win" | "super" | "meta" => ModKey::Win,
            other => return Err(invalid_chord(other, "not a modifier")),
        };
        if !out.contains(&m) {
            out.push(m);
        }
    }
    Ok(out)
}

/// `click(x, y, button, count, mods, skip_set_cursor_pos)`.
pub fn click(
    x: i32,
    y: i32,
    button: MouseButton,
    count: u8,
    modifiers: &[String],
    skip_set_cursor_pos: bool,
) -> Result<(), ProtoError> {
    check_foreground_integrity()?;
    let mods = parse_mods(modifiers)?;
    let _guard = press_chord(&mods);
    if !skip_set_cursor_pos {
        let _ = set_cursor_pos(x, y);
    }
    let mut events: Vec<INPUT> = Vec::with_capacity((count as usize) * 2);
    for _ in 0..count.max(1) {
        events.push(mouse_button_absolute(button, false, x, y));
        events.push(mouse_button_absolute(button, true, x, y));
    }
    send(&events).map_err(ProtoError::from).map(|_| ())
}

/// `mouse_move(x, y)` — pure cursor move (no buttons).
pub fn mouse_move(x: i32, y: i32) -> Result<(), ProtoError> {
    check_foreground_integrity()?;
    set_cursor_pos(x, y)
}

/// `mouse_down(button)` at the current cursor position.
pub fn mouse_down(button: MouseButton) -> Result<(), ProtoError> {
    check_foreground_integrity()?;
    let ev = INPUT {
        r#type: windows::Win32::UI::Input::KeyboardAndMouse::INPUT_MOUSE,
        Anonymous: windows::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
            mi: windows::Win32::UI::Input::KeyboardAndMouse::MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: 0,
                dwFlags: mouse_button_flags(button, false),
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    send(&[ev]).map_err(ProtoError::from).map(|_| ())
}

/// `mouse_up(button)`.
pub fn mouse_up(button: MouseButton) -> Result<(), ProtoError> {
    check_foreground_integrity()?;
    let ev = INPUT {
        r#type: windows::Win32::UI::Input::KeyboardAndMouse::INPUT_MOUSE,
        Anonymous: windows::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
            mi: windows::Win32::UI::Input::KeyboardAndMouse::MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: 0,
                dwFlags: mouse_button_flags(button, true),
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    send(&[ev]).map_err(ProtoError::from).map(|_| ())
}

/// `drag` — fused MOVE + DOWN + MOVE + UP with modifiers held throughout.
pub fn drag(
    sx: i32,
    sy: i32,
    ex: i32,
    ey: i32,
    button: MouseButton,
    modifiers: &[String],
) -> Result<(), ProtoError> {
    check_foreground_integrity()?;
    let mods = parse_mods(modifiers)?;
    let _guard = press_chord(&mods);
    set_cursor_pos(sx, sy)?;
    send(&[mouse_button_absolute(button, false, sx, sy)]).map_err(ProtoError::from)?;
    set_cursor_pos(ex, ey)?;
    send(&[mouse_button_absolute(button, true, ex, ey)]).map_err(ProtoError::from)?;
    Ok(())
}

/// `scroll(x, y, direction, amount, mods)`.
pub fn scroll(
    x: i32,
    y: i32,
    direction: ScrollDirection,
    amount: i32,
    modifiers: &[String],
) -> Result<(), ProtoError> {
    check_foreground_integrity()?;
    let mods = parse_mods(modifiers)?;
    let _guard = press_chord(&mods);
    set_cursor_pos(x, y)?;
    let (horizontal, sign) = match direction {
        ScrollDirection::Up => (false, 1),
        ScrollDirection::Down => (false, -1),
        ScrollDirection::Right => (true, 1),
        ScrollDirection::Left => (true, -1),
    };
    let delta = sign * amount * WHEEL_DELTA;
    send(&[mouse_wheel(horizontal, delta)]).map_err(ProtoError::from).map(|_| ())
}

/// `type(text)` — emits each char via `KEYEVENTF_UNICODE`.
///
/// SECURITY: `text` MUST be the exposed payload from a `Redact<String>` —
/// this function NEVER `tracing::info!`s the text, only its length.
pub fn type_text(text: &str) -> Result<(), ProtoError> {
    check_foreground_integrity()?;
    let events = key_unicode_str(text);
    if events.is_empty() {
        return Ok(());
    }
    // Chunk SendInput into groups of 256 to avoid platform limits on a
    // single call; each chunk is one syscall.
    for chunk in events.chunks(256) {
        send(chunk).map_err(ProtoError::from)?;
    }
    Ok(())
}

/// `type(text)` with a fixed inter-character delay. Each `char` is sent as
/// its own SendInput pair (DOWN+UP `KEYEVENTF_UNICODE`) followed by a sleep
/// of `rate_ms` milliseconds before the next char. Defeats apps that
/// debounce or drop keystrokes when the input event queue floods (some
/// ImGui text inputs, terminal widgets that throttle, etc.).
///
/// `rate_ms == 0` is permitted and behaves like the bulk path but still
/// emits per-char syscalls (no benefit; callers should use [`type_text`]
/// for the unrestricted-rate case).
pub fn type_text_rated(text: &str, rate_ms: u32) -> Result<(), ProtoError> {
    check_foreground_integrity()?;
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return Ok(());
    }
    let delay = std::time::Duration::from_millis(rate_ms as u64);
    let last = chars.len() - 1;
    for (idx, ch) in chars.iter().enumerate() {
        let mut s = [0u8; 4];
        let events = key_unicode_str(ch.encode_utf8(&mut s));
        if !events.is_empty() {
            for chunk in events.chunks(256) {
                send(chunk).map_err(ProtoError::from)?;
            }
        }
        if rate_ms > 0 && idx != last {
            std::thread::sleep(delay);
        }
    }
    Ok(())
}

/// `key(chord, repeat)`.
pub fn key(chord_str: &str, repeat: u32) -> Result<(), ProtoError> {
    check_foreground_integrity()?;
    let chord = parse(chord_str)?;
    let _guard = press_chord(&chord.mods);
    let mut events: Vec<INPUT> = Vec::with_capacity((repeat as usize) * 2);
    for _ in 0..repeat.max(1) {
        match chord.key {
            ChordKey::Vk(vk) => {
                events.push(key_vk(vk, false));
                events.push(key_vk(vk, true));
            }
            ChordKey::Unicode(c) => {
                let mut buf = [0u16; 2];
                for &u in c.encode_utf16(&mut buf).iter() {
                    events.push(key_unicode(u, false));
                    events.push(key_unicode(u, true));
                }
            }
        }
    }
    for chunk in events.chunks(256) {
        send(chunk).map_err(ProtoError::from)?;
    }
    Ok(())
}

/// `hold_key(chord, duration_ms)` — press, sleep on the input thread, release.
///
/// Held modifiers + primary key are released by the [`ModifierGuard`] /
/// `release_all_held` flush even if this function panics.
pub fn hold_key(chord_str: &str, duration_ms: u32) -> Result<(), ProtoError> {
    check_foreground_integrity()?;
    let chord = parse(chord_str)?;
    let _guard = press_chord(&chord.mods);

    // Build DOWN/UP for the primary key.
    let (down_ev, up_ev) = match chord.key {
        ChordKey::Vk(vk) => (key_vk(vk, false), key_vk(vk, true)),
        ChordKey::Unicode(c) => {
            let mut buf = [0u16; 2];
            let units = c.encode_utf16(&mut buf);
            // Only a single-unit Unicode hold supported (most common).
            (key_unicode(units[0], false), key_unicode(units[0], true))
        }
    };

    // CR-03: register the UP event in the thread-local registry BEFORE we
    // send the DOWN. If a panic fires during the sleep below, the
    // panic-hook flush (`release_all_held`) emits this UP and the user's
    // primary key does not stay stuck.
    set_primary_key_up(up_ev);
    send(&[down_ev]).map_err(|e| {
        // Roll back registration on send failure — there is no down to
        // pair the up with.
        clear_primary_key_up();
        ProtoError::from(e)
    })?;

    // Sleep on the input thread (fine — caller queued exactly this work).
    // panic::catch_unwind is set up at the dispatcher edge (Task 13 wiring).
    let start = Instant::now();
    let dur = Duration::from_millis(duration_ms as u64);
    while start.elapsed() < dur {
        // Sleep in small slices to keep response to shutdown reasonable.
        let remaining = dur.saturating_sub(start.elapsed());
        std::thread::sleep(remaining.min(Duration::from_millis(50)));
    }

    // Normal-path release: clear the registry first so the panic-hook
    // doesn't double-release, then emit the UP.
    clear_primary_key_up();
    send(&[up_ev]).map_err(ProtoError::from)?;
    // _guard drops → modifiers released.
    Ok(())
}

/// Belt-and-suspenders: flush all held modifiers. Called by the daemon's
/// panic hook and by the input-thread teardown.
///
/// **Must be called on the input STA thread** (WR-08). The held-modifier
/// registry is `thread_local!`, so calling this from any other thread
/// (e.g. a tokio worker) reads an empty registry and silently no-ops.
/// The daemon's panic hook routes this through `InputJob::FlushHeldModifiers`
/// when the panicking thread is NOT the input thread, and inlines the call
/// when it IS the input thread (CR-04).
pub fn flush_held_modifiers() {
    release_all_held();
}

#[cfg(test)]
#[cfg(feature = "mock-sendinput")]
mod tests {
    use super::*;
    use crate::input::sendinput::{mock_sink, RecordedInput};

    fn flags_keyup() -> u32 {
        windows::Win32::UI::Input::KeyboardAndMouse::KEYEVENTF_KEYUP.0
    }

    fn setup() {
        crate::input::modifier_guard::clear_for_test();
        let _ = mock_sink().take();
    }

    #[test]
    fn type_emits_one_pair_per_bmp_char() {
        setup();
        type_text("a").unwrap();
        let evs = mock_sink().take();
        assert_eq!(evs.len(), 2);
    }

    #[test]
    fn type_emits_two_pairs_for_surrogate_emoji() {
        setup();
        type_text("a🎉").unwrap();
        let evs = mock_sink().take();
        // a → 2 events; 🎉 → 4 events (surrogate pair down+up each)
        assert_eq!(evs.len(), 6, "got {evs:?}");
    }

    #[test]
    fn key_ctrl_s_emits_modifier_then_key() {
        setup();
        key("ctrl+s", 1).unwrap();
        let evs = mock_sink().take();
        // Sequence: Ctrl-DOWN (press_chord), S-DOWN, S-UP, Ctrl-UP (guard drop)
        assert_eq!(evs.len(), 4, "got {evs:?}");
        match (&evs[0], &evs[1], &evs[2], &evs[3]) {
            (
                RecordedInput::Key { vk: c_d, flags: c_d_f, .. },
                RecordedInput::Key { vk: s_d, flags: s_d_f, .. },
                RecordedInput::Key { vk: s_u, flags: s_u_f, .. },
                RecordedInput::Key { vk: c_u, flags: c_u_f, .. },
            ) => {
                assert_eq!(*c_d, ModKey::Ctrl.vk());
                assert_eq!(*c_d_f, 0);
                assert_eq!(*s_d, b'S' as u16);
                assert_eq!(*s_d_f, 0);
                assert_eq!(*s_u, b'S' as u16);
                assert_eq!(*s_u_f, flags_keyup());
                assert_eq!(*c_u, ModKey::Ctrl.vk());
                assert_eq!(*c_u_f, flags_keyup());
            }
            other => panic!("unexpected sequence: {other:?}"),
        }
    }

    #[test]
    fn click_double_emits_four_mouse_events() {
        setup();
        click(100, 100, MouseButton::Left, 2, &[], true).unwrap();
        let evs = mock_sink().take();
        assert_eq!(evs.len(), 4, "got {evs:?}");
        for ev in &evs {
            assert!(matches!(ev, RecordedInput::Mouse { .. }));
        }
    }

    #[test]
    fn drag_emits_two_button_events() {
        setup();
        drag(0, 0, 200, 200, MouseButton::Left, &[]).unwrap();
        let evs = mock_sink().take();
        // We send 2 SendInput calls for buttons (DOWN, UP); each is one event.
        assert_eq!(evs.len(), 2, "got {evs:?}");
    }
}
