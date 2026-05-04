//! Tracked-modifier registry with `Drop` and panic-hook based KEYUP flush.
//!
//! Lives on the input STA thread (Phase 1 D-26). All access is through the
//! thread-local `HELD` registry — there are no `Mutex`es because the only
//! writer is the single input thread.

use std::cell::RefCell;

use fastuse_proto::ModKey;
use windows::Win32::UI::Input::KeyboardAndMouse::INPUT;

use crate::input::sendinput::{key_vk, send};

thread_local! {
    /// Modifiers currently DOWN on the input thread, in press order.
    /// WR-01: ordered `Vec` (with manual dedupe) so the panic-flush path
    /// can release in reverse-press order — `BTreeSet` iteration gave
    /// tag-ascending order which leaks Ctrl+Win as a Win-up-while-Alt-down
    /// trigger on some drivers.
    static HELD: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    /// CR-03: primary key currently DOWN as part of an in-flight `hold_key`.
    /// Stored as a single pre-built KEYUP `INPUT` so the panic-hook flush
    /// can release it without re-deriving the chord. `None` when no hold is
    /// in flight.
    static PRIMARY_KEY_UP: RefCell<Option<INPUT>> = const { RefCell::new(None) };
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
    let mut owned: Vec<u8> = Vec::with_capacity(mods.len());
    HELD.with(|h| {
        let mut held = h.borrow_mut();
        for &m in mods {
            let tag = modkey_tag(m);
            if !held.contains(&tag) {
                held.push(tag);
                events.push(key_vk(m.vk(), false));
            }
            if !owned.contains(&tag) {
                owned.push(tag);
            }
        }
    });
    if !events.is_empty() {
        // Best-effort: ignore Err here — caller's surrounding handler will
        // propagate via send() return value on the next call.
        let _ = send(&events);
    }
    ModifierGuard { owned }
}

/// Release every modifier currently held in this thread's registry, plus any
/// in-flight primary key tracked by `hold_key` (CR-03).
///
/// Emits a single batched `SendInput` with all KEYUPs (CONTEXT.md hard rule).
/// Modifiers are released in reverse-press order (WR-01).
///
/// WR-11 / IN-01: if `SendInput` fails (e.g. an LL hook is blocking), the
/// registry is left intact and a tracing::error! is emitted so a subsequent
/// call (or the next handler invocation) can retry. Previously the registry
/// was cleared pre-send so `held_count() == 0` lied about OS state.
pub fn release_all_held() {
    let primary = PRIMARY_KEY_UP.with(|p| p.borrow_mut().take());
    let mut events: Vec<INPUT> = Vec::new();
    // INPUT is Copy via the windows-rs binding (it's a plain POD union);
    // copying for the rollback path keeps the source-of-truth in `primary`.
    if let Some(ref ev) = primary {
        events.push(*ev);
    }
    let snapshot: Vec<u8> = HELD.with(|h| h.borrow().iter().rev().copied().collect());
    for &tag in &snapshot {
        events.push(key_vk(tag_to_vk(tag), true));
    }
    if events.is_empty() {
        return;
    }
    match send(&events) {
        Ok(_) => {
            HELD.with(|h| h.borrow_mut().clear());
        }
        Err(e) => {
            // Re-register the primary key so a follow-up call retries it,
            // and leave HELD intact so held_count reflects the OS state we
            // last knew about.
            if let Some(ev) = primary {
                PRIMARY_KEY_UP.with(|p| *p.borrow_mut() = Some(ev));
            }
            tracing::error!(
                err = ?e,
                "modifier flush partial — OS may still see modifier(s) pressed; will retry on next call"
            );
        }
    }
}



/// CR-03: register the in-flight primary-key UP event so a panic during
/// `hold_key`'s sleep flushes it via `release_all_held`. Replaces any prior
/// in-flight key (callers must not stack holds on the same input thread).
pub(crate) fn set_primary_key_up(ev: INPUT) {
    PRIMARY_KEY_UP.with(|p| *p.borrow_mut() = Some(ev));
}

/// CR-03: clear the primary-key registration after a successful manual
/// release; idempotent.
pub(crate) fn clear_primary_key_up() {
    PRIMARY_KEY_UP.with(|p| *p.borrow_mut() = None);
}

/// Number of modifiers currently held (test helper).
pub fn held_count() -> usize {
    HELD.with(|h| h.borrow().len())
}

/// Forcibly clear the registry without emitting any events (test helper).
#[cfg(any(test, feature = "mock-sendinput"))]
pub fn clear_for_test() {
    HELD.with(|h| h.borrow_mut().clear());
    PRIMARY_KEY_UP.with(|p| *p.borrow_mut() = None);
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
        // WR-11 / IN-01: build the SendInput batch from a snapshot, only
        // mutate HELD on success. On failure, log and leave HELD intact so
        // the next handler can retry.
        let mut events: Vec<INPUT> = Vec::with_capacity(self.owned.len());
        let mut tags_to_clear: Vec<u8> = Vec::with_capacity(self.owned.len());
        HELD.with(|h| {
            let held = h.borrow();
            // Reverse order to match press_chord's semantics (last-pressed
            // released first — matches user intent).
            for &tag in self.owned.iter().rev() {
                if held.iter().any(|&t| t == tag) {
                    events.push(key_vk(tag_to_vk(tag), true));
                    tags_to_clear.push(tag);
                }
            }
        });
        if events.is_empty() {
            return;
        }
        match send(&events) {
            Ok(_) => {
                HELD.with(|h| {
                    let mut held = h.borrow_mut();
                    for tag in &tags_to_clear {
                        if let Some(pos) = held.iter().position(|t| t == tag) {
                            held.remove(pos);
                        }
                    }
                });
            }
            Err(e) => {
                tracing::error!(
                    err = ?e,
                    "modifier guard release partial — OS may still see modifier(s) pressed"
                );
            }
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
