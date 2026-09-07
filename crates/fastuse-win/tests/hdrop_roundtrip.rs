//! Publish CF_HDROP to the real clipboard and read it back.
//!
//! Run with: `cargo test -p fastuse-win --test hdrop_roundtrip -- --ignored`
//! Ignored by default: it clobbers the developer's clipboard.

#![cfg(target_os = "windows")]

use fastuse_proto::{ClipboardSet, Redact};

#[test]
#[ignore = "clobbers the real clipboard"]
fn cf_hdrop_round_trips_with_copy_effect() {
    let p = std::env::temp_dir().join("fastuse_hdrop_roundtrip.txt");
    std::fs::write(&p, b"x").unwrap();

    fastuse_win::clipboard::clipboard_set(ClipboardSet::Files {
        paths: Redact::new(vec![p.to_string_lossy().into_owned()]),
        paste: false,
        hwnd: None,
    })
    .expect("clipboard_set files");

    let names = fastuse_win::clipboard::read_back_hdrop_for_test().expect("read back");
    assert_eq!(names.len(), 1);
    assert!(names[0].ends_with("fastuse_hdrop_roundtrip.txt"), "got {}", names[0]);

    // Without a preferred-drop-effect of COPY, Explorer treats a paste as a
    // MOVE and the user's source file disappears. That is data loss, so the
    // effect is asserted, not assumed.
    let effect = fastuse_win::clipboard::read_back_preferred_effect_for_test().expect("effect");
    const DROPEFFECT_COPY: u32 = 1;
    assert_eq!(effect, DROPEFFECT_COPY);

    std::fs::remove_file(&p).ok();
}
