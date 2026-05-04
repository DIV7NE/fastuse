//! Modifier-chord parser shared by CLI, MCP, and daemon (CONTEXT.md hard rule).
//!
//! Tokens supported:
//! - Modifiers: `ctrl`, `shift`, `alt`, `win` / `super` / `meta`
//! - Named keys: `f1`..`f24`, `a`..`z`, `0`..`9`, `enter`, `tab`, `esc`,
//!   `space`, `backspace`, `delete`, `insert`, `home`, `end`, `pageup`,
//!   `pagedown`, `up`, `down`, `left`, `right`, `plus`, `minus`, etc.
//! - Bare Unicode scalars (single-char unknown tokens) → typed via
//!   `KEYEVENTF_UNICODE`.
//!
//! `parse_chord` is `no_std`-friendly modulo `String`/`Vec` allocations
//! (we already depend on `alloc` via `serde`/`postcard`). It does NOT depend
//! on the `windows` crate; numeric VK constants are embedded as `u16`
//! literals (CONTEXT.md hard rule).

use serde::{Deserialize, Serialize};

/// Modifier keys that can prefix a chord.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ModKey {
    /// Either Control key (left preferred for emission).
    Ctrl,
    /// Either Shift key (left preferred for emission).
    Shift,
    /// Either Alt key (left preferred for emission). Maps to `MENU` in VK speak.
    Alt,
    /// Either Win / Super / Meta key (left preferred for emission).
    Win,
}

impl ModKey {
    /// Win32 virtual-key code corresponding to the LEFT-handed variant.
    /// Used by the input thread when emitting KEYDOWN/KEYUP via SendInput.
    pub const fn vk(self) -> u16 {
        match self {
            // VK_LCONTROL=0xA2, VK_LSHIFT=0xA0, VK_LMENU=0xA4, VK_LWIN=0x5B
            ModKey::Ctrl => 0xA2,
            ModKey::Shift => 0xA0,
            ModKey::Alt => 0xA4,
            ModKey::Win => 0x5B,
        }
    }
}

/// The "primary" key in a chord — either a virtual-key code or a Unicode
/// scalar typed via `KEYEVENTF_UNICODE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ChordKey {
    /// Win32 virtual-key code (`VK_*`).
    Vk(u16),
    /// Unicode scalar typed via `KEYEVENTF_UNICODE`.
    Unicode(char),
}

/// A parsed chord: zero-or-more modifiers + exactly one primary key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chord {
    /// Modifier keys held during the press (deduplicated, parse-order).
    pub mods: Vec<ModKey>,
    /// Primary key.
    pub key: ChordKey,
}

/// Errors returned by [`parse_chord`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChordError {
    /// Empty input or trailing/leading `+`.
    #[error("empty chord token")]
    Empty,
    /// More than one primary key in the chord.
    #[error("multiple primary keys in chord")]
    MultipleKeys,
    /// No primary key (only modifiers).
    #[error("chord has no primary key")]
    NoKey,
    /// Token does not name a modifier or a known VK and is not a single
    /// Unicode scalar.
    #[error("unknown chord token: {0:?}")]
    UnknownToken(String),
    /// Input length exceeds the 256-character cap (T-02-02 mitigation).
    #[error("chord too long")]
    TooLong,
}

/// Maximum chord-string length accepted at the parser edge (T-02-02).
pub const MAX_CHORD_LEN: usize = 256;

/// Parse a chord string like `"ctrl+shift+a"` into a [`Chord`].
///
/// Matching contract (WR-05):
/// - Modifier and named-key tokens (`ctrl`, `enter`, `f4`, …) are matched
///   ASCII-case-insensitively after Unicode-lowercase folding. Non-ASCII
///   modifier names (e.g. fullwidth `Ｃｔｒｌ`) intentionally do NOT match.
/// - The single-character Unicode fallback preserves the **original** token
///   so emission via `KEYEVENTF_UNICODE` reflects the user-typed case
///   (`"A"` → VK_A via ASCII fallback; `"Ä"` → `Unicode('Ä')` preserving the
///   diaeresis). This asymmetry is intentional: ASCII letters resolve to
///   virtual-key codes which are case-insensitive at the OS layer, while
///   non-ASCII characters round-trip through Unicode injection where case
///   matters.
pub fn parse_chord(s: &str) -> Result<Chord, ChordError> {
    // WR-04: enforce the documented 256-CHARACTER cap, not a 256-byte cap;
    // a chord like "🎉🎉…" of valid 4-byte scalars was previously rejected.
    if s.chars().count() > MAX_CHORD_LEN {
        return Err(ChordError::TooLong);
    }
    if s.is_empty() {
        return Err(ChordError::Empty);
    }

    let mut mods: Vec<ModKey> = Vec::new();
    let mut key: Option<ChordKey> = None;

    // Parsing rule: split on '+'. An empty token (leading/trailing/double '+')
    // is an error.
    for raw in s.split('+') {
        if raw.is_empty() {
            return Err(ChordError::Empty);
        }
        let lc: String = raw.chars().flat_map(|c| c.to_lowercase()).collect();

        // Modifier?
        if let Some(m) = mod_from_token(&lc) {
            if !mods.contains(&m) {
                mods.push(m);
            }
            continue;
        }

        // Named VK?
        if let Some(vk) = vk_from_token(&lc) {
            if key.is_some() {
                return Err(ChordError::MultipleKeys);
            }
            key = Some(ChordKey::Vk(vk));
            continue;
        }

        // Single Unicode scalar fallback. Use the ORIGINAL token (preserve
        // case for emoji / non-ASCII).
        let mut chars = raw.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => {
                if key.is_some() {
                    return Err(ChordError::MultipleKeys);
                }
                key = Some(ChordKey::Unicode(c));
            }
            _ => return Err(ChordError::UnknownToken(raw.to_string())),
        }
    }

    let key = key.ok_or(ChordError::NoKey)?;
    Ok(Chord { mods, key })
}

fn mod_from_token(lc: &str) -> Option<ModKey> {
    match lc {
        "ctrl" => Some(ModKey::Ctrl),
        "shift" => Some(ModKey::Shift),
        "alt" => Some(ModKey::Alt),
        "win" | "super" | "meta" => Some(ModKey::Win),
        _ => None,
    }
}

/// Map a lowercase named-key token to its Win32 virtual-key code.
///
/// This table embeds numeric VK_* values directly so `fastuse-proto` does NOT
/// depend on the `windows` crate (D-11). Source: Microsoft Learn
/// "Virtual-Key Codes" (winuser.h).
fn vk_from_token(lc: &str) -> Option<u16> {
    // Single ASCII letter a-z → VK_A..VK_Z = 0x41..0x5A
    if lc.len() == 1 {
        let c = lc.chars().next().unwrap();
        if c.is_ascii_alphabetic() {
            return Some(c.to_ascii_uppercase() as u16);
        }
        if c.is_ascii_digit() {
            return Some(c as u16);
        }
    }

    // F1..F24 = 0x70..0x87
    if let Some(rest) = lc.strip_prefix('f') {
        if let Ok(n) = rest.parse::<u16>() {
            if (1..=24).contains(&n) {
                return Some(0x6F + n);
            }
        }
    }

    Some(match lc {
        "enter" | "return" => 0x0D,    // VK_RETURN
        "tab" => 0x09,                  // VK_TAB
        "esc" | "escape" => 0x1B,      // VK_ESCAPE
        "space" => 0x20,                // VK_SPACE
        "backspace" => 0x08,            // VK_BACK
        "delete" | "del" => 0x2E,      // VK_DELETE
        "insert" | "ins" => 0x2D,      // VK_INSERT
        "home" => 0x24,                 // VK_HOME
        "end" => 0x23,                  // VK_END
        "pageup" | "pgup" => 0x21,    // VK_PRIOR
        "pagedown" | "pgdn" => 0x22,  // VK_NEXT
        "up" => 0x26,                   // VK_UP
        "down" => 0x28,                 // VK_DOWN
        "left" => 0x25,                 // VK_LEFT
        "right" => 0x27,                // VK_RIGHT
        "plus" => 0xBB,                 // VK_OEM_PLUS
        "minus" => 0xBD,                // VK_OEM_MINUS
        "comma" => 0xBC,                // VK_OEM_COMMA
        "period" | "dot" => 0xBE,     // VK_OEM_PERIOD
        "capslock" => 0x14,             // VK_CAPITAL
        "printscreen" | "prtsc" => 0x2C, // VK_SNAPSHOT
        "scrolllock" => 0x91,           // VK_SCROLL
        "pause" => 0x13,                // VK_PAUSE
        "numlock" => 0x90,              // VK_NUMLOCK
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vk(c: char) -> u16 {
        c as u16
    }

    #[test]
    fn parses_ctrl_shift_a() {
        let c = parse_chord("ctrl+shift+a").unwrap();
        assert_eq!(c.mods, vec![ModKey::Ctrl, ModKey::Shift]);
        assert_eq!(c.key, ChordKey::Vk(vk('A')));
    }

    #[test]
    fn parses_alt_f4() {
        let c = parse_chord("alt+f4").unwrap();
        assert_eq!(c.mods, vec![ModKey::Alt]);
        assert_eq!(c.key, ChordKey::Vk(0x73)); // VK_F4
    }

    #[test]
    fn parses_win_d() {
        let c = parse_chord("win+d").unwrap();
        assert_eq!(c.mods, vec![ModKey::Win]);
        assert_eq!(c.key, ChordKey::Vk(vk('D')));
    }

    #[test]
    fn parses_ctrl_plus() {
        let c = parse_chord("ctrl+plus").unwrap();
        assert_eq!(c.mods, vec![ModKey::Ctrl]);
        assert_eq!(c.key, ChordKey::Vk(0xBB)); // VK_OEM_PLUS
    }

    #[test]
    fn parses_single_letter() {
        let c = parse_chord("a").unwrap();
        assert!(c.mods.is_empty());
        assert_eq!(c.key, ChordKey::Vk(vk('A')));
    }

    #[test]
    fn parses_unicode_scalar() {
        let c = parse_chord("🎉").unwrap();
        assert!(c.mods.is_empty());
        assert_eq!(c.key, ChordKey::Unicode('🎉'));
    }

    #[test]
    fn rejects_unknown_token() {
        let r = parse_chord("foo+bar");
        assert!(matches!(r, Err(ChordError::UnknownToken(_))));
    }

    #[test]
    fn rejects_empty() {
        assert_eq!(parse_chord(""), Err(ChordError::Empty));
    }

    #[test]
    fn rejects_trailing_plus() {
        assert_eq!(parse_chord("ctrl+"), Err(ChordError::Empty));
    }

    #[test]
    fn rejects_leading_plus() {
        assert_eq!(parse_chord("+ctrl"), Err(ChordError::Empty));
    }

    #[test]
    fn super_and_meta_alias_to_win() {
        assert_eq!(parse_chord("super+a").unwrap().mods, vec![ModKey::Win]);
        assert_eq!(parse_chord("meta+a").unwrap().mods, vec![ModKey::Win]);
    }

    #[test]
    fn ctl_is_rejected() {
        // "ctl" is two ASCII letters, not a modifier and not a single-char
        // Unicode fallback → UnknownToken.
        let r = parse_chord("ctl+a");
        assert!(matches!(r, Err(ChordError::UnknownToken(_))));
    }

    #[test]
    fn case_insensitive() {
        let lower = parse_chord("ctrl+shift+a").unwrap();
        let mixed = parse_chord("Ctrl+Shift+A").unwrap();
        let upper = parse_chord("CTRL+SHIFT+A").unwrap();
        assert_eq!(lower, mixed);
        assert_eq!(lower, upper);
    }

    #[test]
    fn rejects_multiple_primary_keys() {
        let r = parse_chord("a+b");
        assert_eq!(r, Err(ChordError::MultipleKeys));
    }

    #[test]
    fn rejects_no_primary_key() {
        let r = parse_chord("ctrl+shift");
        assert_eq!(r, Err(ChordError::NoKey));
        // alt+ctrl+win → all mods, no key
        let r2 = parse_chord("alt+ctrl+win");
        assert_eq!(r2, Err(ChordError::NoKey));
    }

    #[test]
    fn rejects_oversize() {
        let big: String = "a".repeat(MAX_CHORD_LEN + 1);
        assert_eq!(parse_chord(&big), Err(ChordError::TooLong));
    }

    #[test]
    fn dedups_modifiers() {
        let c = parse_chord("ctrl+ctrl+a").unwrap();
        assert_eq!(c.mods, vec![ModKey::Ctrl]);
    }

    #[test]
    fn modkey_vk_codes_are_left_handed() {
        assert_eq!(ModKey::Ctrl.vk(), 0xA2);
        assert_eq!(ModKey::Shift.vk(), 0xA0);
        assert_eq!(ModKey::Alt.vk(), 0xA4);
        assert_eq!(ModKey::Win.vk(), 0x5B);
    }
}
