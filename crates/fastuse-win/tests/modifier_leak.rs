//! Task 16 — modifier-leak panic recovery test.
//!
//! Plan placement was `crates/fastuse-daemon/tests/modifier_leak.rs`, but
//! the `mock-sendinput` cargo feature lives on `fastuse-win` only, and the
//! daemon dispatcher does not (and should not) enable it transitively.
//! Hosting the test inside `fastuse-win` keeps the feature local to where
//! it's defined (Rule 3: auto-fix blocking issue — feature plumbing).
//!
//! Verifies ROADMAP §Phase 2 SC-3 + INP-10:
//!   - hold_key("ctrl+shift", N) records both modifiers in the registry.
//!   - A simulated panic mid-handler skips the `_guard` Drop.
//!   - The panic-hook path (`flush_held_modifiers` → `release_all_held`)
//!     emits a single batched SendInput with 2 KEYUP events for VK_CONTROL
//!     and VK_SHIFT.
//!   - After flush the registry is empty so subsequent dispatch starts clean.
//!
//! Run with: `cargo test --release -p fastuse-win --features mock-sendinput modifier_leak`

#![cfg(all(target_os = "windows", feature = "mock-sendinput"))]

use fastuse_proto::ModKey;
use fastuse_win::input::handlers::flush_held_modifiers;
use fastuse_win::input::modifier_guard::{clear_for_test, held_count, press_chord};
use fastuse_win::input::sendinput::{mock_sink, RecordedInput};

fn flags_keyup() -> u32 {
    windows::Win32::UI::Input::KeyboardAndMouse::KEYEVENTF_KEYUP.0
}

#[test]
fn panic_hook_flushes_held_modifiers() {
    clear_for_test();
    let _ = mock_sink().take();

    // Simulate the body of `hold_key`: press_chord then "panic" by forgetting
    // the guard so its Drop never runs (mimics the bypass that catch_unwind
    // would prevent in production — without catch_unwind in our test harness,
    // forget() is the deterministic equivalent).
    let guard = press_chord(&[ModKey::Ctrl, ModKey::Shift]);
    assert_eq!(held_count(), 2, "registry should record both modifiers");

    // Drain the DOWN events so we only see the post-flush KEYUPs.
    let down_events = mock_sink().take();
    assert_eq!(
        down_events.len(),
        2,
        "expected 2 DOWN events for ctrl+shift, got {:?}",
        down_events
    );
    for ev in &down_events {
        match ev {
            RecordedInput::Key { flags, .. } => {
                assert_eq!(*flags, 0, "DOWN events should not carry KEYUP flag");
            }
            other => panic!("expected key event, got {other:?}"),
        }
    }

    // "Panic" — guard's Drop is skipped:
    std::mem::forget(guard);
    assert_eq!(held_count(), 2, "registry still records both modifiers");

    // Daemon's panic hook would invoke flush_held_modifiers via the input
    // thread channel. Here we call it directly on this thread, which has
    // its own thread-local registry. To realistically test the daemon
    // wiring we'd need a daemon spawn; the registry-flush mechanic is what
    // matters — we verify it on the same thread it was populated on.
    flush_held_modifiers();

    assert_eq!(held_count(), 0, "registry should be empty after flush");

    let up_events = mock_sink().take();
    assert_eq!(
        up_events.len(),
        2,
        "expected 2 KEYUP events from flush, got {:?}",
        up_events
    );
    for ev in &up_events {
        match ev {
            RecordedInput::Key { flags, .. } => {
                assert_eq!(
                    *flags,
                    flags_keyup(),
                    "expected KEYUP flag on flush event, got 0x{:x}",
                    flags
                );
            }
            other => panic!("expected key event, got {other:?}"),
        }
    }

    // Verify the two VKs are exactly Ctrl + Shift (the BTreeSet ordering
    // makes this deterministic).
    let vks: Vec<u16> = up_events
        .iter()
        .filter_map(|e| match e {
            RecordedInput::Key { vk, .. } => Some(*vk),
            _ => None,
        })
        .collect();
    assert!(vks.contains(&ModKey::Ctrl.vk()), "missing VK_CONTROL: {vks:?}");
    assert!(vks.contains(&ModKey::Shift.vk()), "missing VK_SHIFT: {vks:?}");

    // Subsequent dispatch starts clean.
    let _ = mock_sink().take();
    let _g = press_chord(&[ModKey::Alt]);
    assert_eq!(held_count(), 1, "stale state would have made this 2");
}
