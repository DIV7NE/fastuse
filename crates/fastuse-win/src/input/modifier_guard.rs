//! Tracked-modifier registry with `Drop` and panic-hook based KEYUP flush.
//!
//! Lives on the input STA thread (Phase 1 D-26). All access is through the
//! thread-local `HELD` registry — there are no `Mutex`es because the only
//! writer is the single input thread.

use std::cell::RefCell;
use std::collections::BTreeSet;

use fastuse_proto::ModKey;
use windows::Win32::UI::Input::KeyboardAndMouse::INPUT;

use crate::input::sendinput::{key_vk, send};

thread_local! {
    /// Modifiers currently DOWN on the input thread. Use `BTreeSet` so the
    /// emission order is deterministic for tests.
    static HELD: RefCell<BTreeSet<u8>> = const { RefCell::new(BTreeSet::new()) };
}

fn modkey_tag(m: ModKey) -> u8 {
    match m {
        ModKey::Ctrl => 0,
        ModKey::Shift => 1,
        ModKey::Alt => 2,
        ModKey::Win => 3,
    }
}

fn tag_to_vk(tag: u8) -> u16 {
    match tag {
        0 => ModKey::Ctrl.vk(),
        1 => ModKey::Shift.vk(),
        2 => ModKey::Alt.vk(),
        3 => ModKey::Win.vk(),
        _ => unreachable!(),
    }
}

/// Press a list of modifiers (DOWN) and record them in the registry.
/// Returns a [`ModifierGuard`] whose `Drop` releases everything still held.
pub fn press_chord(mods: &[ModKey]) -> ModifierGuard {
    let mut events: Vec<INPUT> = Vec::with_capacity(mods.len());
    HELD.with(|h| {
        let mut held = h.borrow_mut();
        for &m in mods {
            let tag = modkey_tag(m);
            if held.insert(tag) {
                events.push(key_vk(m.vk(), false));
            }
        }
    });
    if !events.is_empty() {
        // Best-effort: ignore Err here — caller's surrounding handler will
        // propagate via send() return value on the next call.
        let _ = send(&events);
    }
    ModifierGuard {
        owned: mods.iter().map(|m| modkey_tag(*m)).collect(),
    }
}

/// Release every modifier currently held in this thread's registry.
///
/// Emits a single batched `SendInput` with all KEYUPs (CONTEXT.md hard rule).
pub fn release_all_held() {
    HELD.with(|h| {
        let mut held = h.borrow_mut();
        if held.is_empty() {
            return;
        }
        let events: Vec<INPUT> = held.iter().map(|&tag| key_vk(tag_to_vk(tag), true)).collect();
        held.clear();
        let _ = send(&events);
    });
}

/// Number of modifiers currently held (test helper).
pub fn held_count() -> usize {
    HELD.with(|h| h.borrow().len())
}

/// Forcibly clear the registry without emitting any events (test helper).
#[cfg(test)]
pub fn clear_for_test() {
    HELD.with(|h| h.borrow_mut().clear());
}

/// RAII wrapper. On drop, releases the modifiers it knows about (and any
/// that fellow guards have already released will be skipped). To release
/// the entire registry, prefer [`release_all_held`].
pub struct ModifierGuard {
    owned: Vec<u8>,
}

impl ModifierGuard {
    /// Release the modifiers this guard owns. Idempotent.
    pub fn release(mut self) {
        self.release_inner();
        // Don't double-release in Drop:
        self.owned.clear();
    }

    fn release_inner(&mut self) {
        if self.owned.is_empty() {
            return;
        }
        let mut events: Vec<INPUT> = Vec::with_capacity(self.owned.len());
        HELD.with(|h| {
            let mut held = h.borrow_mut();
            // Reverse order to match press_chord's semantics (last-pressed
            // released first — matches user intent).
            for &tag in self.owned.iter().rev() {
                if held.remove(&tag) {
                    events.push(key_vk(tag_to_vk(tag), true));
                }
            }
        });
        if !events.is_empty() {
            let _ = send(&events);
        }
    }
}

impl Drop for ModifierGuard {
    fn drop(&mut self) {
        self.release_inner();
    }
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
        clear_for_test();
        let _ = mock_sink().take();
    }

    #[test]
    fn press_then_drop_releases() {
        setup();
        {
            let _g = press_chord(&[ModKey::Shift]);
            assert_eq!(held_count(), 1);
        }
        // Drop ran release.
        assert_eq!(held_count(), 0);
        let events = mock_sink().take();
        // Expect one DOWN, one UP.
        assert_eq!(events.len(), 2);
        match (&events[0], &events[1]) {
            (RecordedInput::Key { vk: d_vk, flags: d_f, .. }, RecordedInput::Key { vk: u_vk, flags: u_f, .. }) => {
                assert_eq!(*d_vk, ModKey::Shift.vk());
                assert_eq!(*d_f, 0);
                assert_eq!(*u_vk, ModKey::Shift.vk());
                assert_eq!(*u_f, flags_keyup());
            }
            other => panic!("unexpected events: {other:?}"),
        }
    }

    #[test]
    fn release_all_held_batches_keyups() {
        setup();
        // Press Ctrl+Shift, then forget them (simulate panic mid-handler).
        std::mem::forget(press_chord(&[ModKey::Ctrl, ModKey::Shift]));
        assert_eq!(held_count(), 2);
        let _ = mock_sink().take(); // drop the DOWN events
        release_all_held();
        assert_eq!(held_count(), 0);
        let events = mock_sink().take();
        assert_eq!(events.len(), 2, "expected 2 batched KEYUPs, got {events:?}");
        for ev in &events {
            match ev {
                RecordedInput::Key { flags, .. } => assert_eq!(*flags, flags_keyup()),
                _ => panic!(),
            }
        }
    }

    #[test]
    fn drop_release_idempotent() {
        setup();
        let g = press_chord(&[ModKey::Win]);
        g.release(); // explicit
        // No second release in Drop should reach SendInput.
        let _ = mock_sink().take();
        // Already-cleared.
        assert_eq!(held_count(), 0);
    }
}
