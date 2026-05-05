# fastuse v2 vision-first Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Pivot fastuse from UIA+OCR targeting to vision-first computer-use primitives, matching Anthropic's `computer_20251124` schema, with Windows-specific helpers as separate MCP tools and UIA demoted to read-only inspection.

**Architecture:** Three Rust binaries built from one workspace (`fastuse-cli`, `fastuse-daemon`, `fastuse-mcp`) talking via Windows named pipes. Daemon hosts MTA-isolated COM threads (capture, UIA pool, input). Vision-first model-side targeting; daemon only dispatches primitives. Tier 1 humanization (Bezier curves + jitter) on by default.

**Tech Stack:** Rust 1.83+, tokio 1.49, windows 0.62.2, uiautomation 0.24.4, windows-capture 2.0, rmcp 1.6, clap 4.6, tracing 0.1, image 0.25.

**Spec:** [docs/superpowers/specs/2026-05-05-fastuse-v2-vision-first-design.md](../specs/2026-05-05-fastuse-v2-vision-first-design.md), commit `026ee64`.

**Branch context:** Working on `v1.0`. Tag `v1.0-pre-pivot` (pushed to origin) preserves rollback to UIA+OCR architecture.

**Build/test/lint commands:**
- Build: `cargo build --release --workspace`
- Test: `cargo test --release --workspace --lib --bins`
- Lint: `cargo run -p xtask -- lints`

**Conventions:**
- No AI-attribution footers in commits (per global CLAUDE.md)
- Atomic commits, conventional message style matching prior commits on `v1.0`
- Use `git commit -F <tempfile>` since `lean-ctx` cargo wrapper mangles inline HEREDOCs

**Locked invariants** (lint-enforced via `cargo run -p xtask -- lints`):
- D-10 (Redact wrapping), D-21 (pipe paths), D-24 (PerMonitorV2 first call), D-25 (no `windows::*` on tokio threads), D-26 (DXGI on capture thread only)

---

## File structure overview

**New files:**
- `crates/fastuse-win/src/scaling.rs` — coordinate scaling state machine
- `crates/fastuse-win/src/input/backend.rs` — `InputBackend` trait + `InputAction` enum
- `crates/fastuse-win/src/input/sendinput_backend.rs` — `SendInputBackend` impl
- `crates/fastuse-win/src/input/humanize.rs` — Bezier curves, motion/typing profiles
- `crates/fastuse-win/src/permissions.rs` — gating logic
- `crates/fastuse-core/src/config.rs` — `config.toml` loader
- `crates/fastuse-mcp/src/tools/computer.rs` — `computer` MCP tool implementation
- `crates/fastuse-mcp/src/tools/windows.rs` — Windows helper MCP tools
- `crates/fastuse-mcp/src/tools/inspection.rs` — UIA inspection MCP tools
- `crates/fastuse-mcp/src/tools/meta.rs` — daemon meta MCP tools
- `crates/fastuse-eval/` — new crate for evaluation suites (Cargo.toml + src/lib.rs + src/bin/eval.rs)

**Files deleted:**
- `crates/fastuse-win/src/targeting/` — entire directory
- `crates/fastuse-win/src/ocr/` — entire directory
- `crates/fastuse-win/src/ocr_thread.rs`

**Files rewritten:**
- `crates/fastuse-proto/src/wire.rs` — new wire types (`computer` action enum, Windows helper requests)
- `crates/fastuse-daemon/src/dispatch.rs` — handlers for new wire types
- `crates/fastuse-cli/src/main.rs` and subcommand modules
- `crates/fastuse-mcp/src/server.rs` — primary surface
- `CLAUDE.md` — MCP-primary guidance
- `README.md` — overview, install, quick start

**Files lightly modified:**
- `crates/fastuse-win/src/capture/screenshot.rs` — emit scaling metadata
- `crates/fastuse-win/src/input/mod.rs` — refactor for backend trait
- `crates/fastuse-win/src/input_thread.rs` — dispatch through backend
- `crates/fastuse-win/src/lib.rs` — module declarations (remove ocr, ocr_thread, targeting; add scaling, permissions)
- `xtask/src/check_com.rs` — drop `ocr_thread` from allowed COM-thread allowlist
- `Cargo.toml` (workspace) — add `fastuse-eval`

---

<!-- TASKS_BEGIN -->

### Task 1: Demolish targeting and OCR modules

**Files:**
- Delete: `crates/fastuse-win/src/targeting/` (entire directory)
- Delete: `crates/fastuse-win/src/ocr/` (entire directory)
- Delete: `crates/fastuse-win/src/ocr_thread.rs`
- Modify: `crates/fastuse-win/src/lib.rs` (remove `pub mod ocr;`, `pub mod ocr_thread;`, `pub mod targeting;`, and the `pub use ocr_thread::*` re-export)
- Modify: `crates/fastuse-proto/src/wire.rs` (delete targeting wire types)
- Modify: `crates/fastuse-daemon/src/dispatch.rs` (delete `Request::ClickElement`, `Request::TypeIntoElement`, `Request::WaitForElement` handlers; mark unreachable for the wire types now gone)
- Modify: `crates/fastuse-cli/src/main.rs` and any cli subcommand files (delete `click-element`, `type-into-element`, `wait-for-element` subcommands)
- Modify: `xtask/src/check_com.rs` (remove `ocr_thread` from allowed COM-thread allowlist)

- [ ] **Step 1: Confirm v1.0-pre-pivot tag exists on origin (pre-flight)**

Run: `git tag -l v1.0-pre-pivot && git ls-remote --tags origin v1.0-pre-pivot`
Expected: both print `v1.0-pre-pivot`. If missing, abort and create the tag before destructive deletion.

- [ ] **Step 2: Delete the directories**

Run:
```
rm -rf crates/fastuse-win/src/targeting crates/fastuse-win/src/ocr
rm -f crates/fastuse-win/src/ocr_thread.rs
```

- [ ] **Step 3: Remove module declarations and re-exports from `crates/fastuse-win/src/lib.rs`**

Edit out these lines:
```rust
pub mod ocr;
pub mod ocr_thread;
pub mod targeting;
```
And the re-export line:
```rust
pub use ocr_thread::{spawn_ocr_thread, OcrThreadError, OcrThreadHandle};
```

- [ ] **Step 4: Delete targeting wire types from `crates/fastuse-proto/src/wire.rs`**

Delete these enums/structs (and any `From`/`Display`/`serde` impls bound to them):
- `Strategy`, `VerificationEvidence`, `ExpectClause`, `EscalatePolicy`
- `ActionResult` variant of `Response` (and its struct fields)
- Any `Request` variants that reference targeting: `ClickElement`, `TypeIntoElement`, `WaitForElement`
- The `ActionOpts` `verify`/`expect`/`escalate` fields if they exist (the `wait_for`, `screenshot_after`, `wait_timeout_ms` fields stay — they're used by the new `computer` action)

Replace `Response::ActionResult { ... }` call sites in the daemon dispatcher with a temporary `Response::Error(...)` returning `ErrorCode::Internal` saying `"removed: rebuild in progress"`. We'll wire the new types in a later task.

- [ ] **Step 5: Delete CLI subcommands**

Edit `crates/fastuse-cli/src/main.rs` (and any subcommand-specific files) to remove `click-element`, `type-into-element`, `wait-for-element` subcommand variants from clap derive structures. Remove the dispatch arms that called daemon for those.

- [ ] **Step 6: Update xtask COM allowlist**

Edit `xtask/src/check_com.rs` to remove `ocr_thread` from the allowed-modules list (the comment-based allowlist that authorizes which modules may host COM threads).

- [ ] **Step 7: Verify clean compile**

Run: `cargo build --release --workspace`
Expected: PASS. Any compile errors are stragglers — fix the call site or import path. Common stragglers: imports of `crate::ocr::*` or `crate::targeting::*` in other modules; references to `OcrThreadHandle` in `fastuse-daemon`'s server context; `fastuse-mcp` shim if it referenced any deleted type.

- [ ] **Step 8: Run lints**

Run: `cargo run -p xtask -- lints`
Expected: PASS.

- [ ] **Step 9: Run remaining tests**

Run: `cargo test --release --workspace --lib --bins`
Expected: PASS. Targeting/OCR tests are gone with their modules; surviving tests should still pass.

- [ ] **Step 10: Commit**

Write the commit message to a tempfile, then commit. Stage explicitly (no `git add -A`):
```bash
git add crates/fastuse-win/src/lib.rs crates/fastuse-proto/src/wire.rs crates/fastuse-daemon/src/dispatch.rs crates/fastuse-cli/src/main.rs xtask/src/check_com.rs
git add -u  # stage the deletions
git commit -F <(printf '%s\n' \
  "refactor: demolish targeting and OCR modules" \
  "" \
  "Removes the entire UIA+OCR targeting layer per v2 vision-first design." \
  "Predecessor architecture preserved at tag v1.0-pre-pivot." \
  "" \
  "Deleted modules: targeting/, ocr/, ocr_thread. Wire types: Strategy," \
  "VerificationEvidence, ExpectClause, EscalatePolicy, ActionResult variant," \
  "ClickElement/TypeIntoElement/WaitForElement requests. CLI subcommands:" \
  "click-element, type-into-element, wait-for-element. xtask allowlist updated." \
  "" \
  "New 'computer' action enum + Windows helpers + UIA inspection wire" \
  "in subsequent commits.")
```

### Task 2: Introduce InputBackend trait and refactor existing dispatch behind it

**Files:**
- Create: `crates/fastuse-win/src/input/backend.rs`
- Create: `crates/fastuse-win/src/input/sendinput_backend.rs`
- Modify: `crates/fastuse-win/src/input/mod.rs` (add `pub mod backend; pub mod sendinput_backend;`, re-exports)
- Modify: `crates/fastuse-win/src/input_thread.rs` (route through `InputBackend`)
- Test: `crates/fastuse-win/src/input/backend.rs` (unit tests inline `#[cfg(test)]`)

- [ ] **Step 1: Write the failing tests for the trait contract and a mock backend**

Create `crates/fastuse-win/src/input/backend.rs` with the trait + mock + tests:

```rust
//! InputBackend abstraction. Default impl is `SendInputBackend`; a future
//! `HardwareHidBackend` (USB-HID via Pico) can plug in without touching the
//! daemon.

use crate::input::sendinput::Modifiers;

/// Mouse button enum, mirrors fastuse_proto::wire but kept local to avoid the
/// crate dependency cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollDirection {
    Up,
    Down,
    Left,
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
    pub humanize: bool,
    pub mean_interval_ms: u32,
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
    MouseMove { from: Point, to: Point, profile: MotionProfile },
    MouseClick { at: Point, button: MouseButton, count: u8, modifiers: Modifiers, humanize: bool },
    MouseDown { at: Point, button: MouseButton },
    MouseUp { at: Point, button: MouseButton },
    Drag { from: Point, to: Point, button: MouseButton, profile: MotionProfile, modifiers: Modifiers },
    KeyType { text: String, profile: TypingProfile },
    KeyChord { keys: Vec<u16>, hold_ms: Option<u32> }, // VK codes
    Scroll { at: Point, direction: ScrollDirection, amount: i32 },
}

/// What this backend can do.
#[derive(Debug, Clone, Copy, Default)]
pub struct Capabilities {
    pub humanized_motion: bool,
    pub humanized_typing: bool,
    pub modifier_drags: bool,
    pub gamepad: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum InputError {
    #[error("input dispatch failed: {0}")]
    Dispatch(String),
    #[error("backend unavailable: {0}")]
    Unavailable(String),
}

pub trait InputBackend: Send + Sync {
    fn dispatch(&self, action: InputAction) -> Result<(), InputError>;
    fn capabilities(&self) -> Capabilities;
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
```

- [ ] **Step 2: Run test to verify it fails on the trait not yet wired into `mod.rs`**

Run: `cargo test --release -p fastuse-win --lib input::backend`
Expected: FAIL with "module `backend` not found" or similar — the file exists but isn't declared in `mod.rs`.

- [ ] **Step 3: Declare the module in `crates/fastuse-win/src/input/mod.rs`**

Add to the top of `crates/fastuse-win/src/input/mod.rs`:
```rust
pub mod backend;
pub mod sendinput_backend;
```

- [ ] **Step 4: Create the SendInputBackend implementation (no humanization yet)**

Create `crates/fastuse-win/src/input/sendinput_backend.rs`:

```rust
//! SendInput-backed implementation of InputBackend. v1: pure dispatch, no
//! humanization (Bezier curves and timing jitter ship in the next task).

use crate::input::backend::{
    Capabilities, InputAction, InputBackend, InputError, MotionProfile,
};

pub struct SendInputBackend;

impl SendInputBackend {
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
```

If `crates/fastuse-win/src/input/sendinput.rs` does not yet expose `mouse_move_absolute`, `mouse_click`, `mouse_down`, `mouse_up`, `type_text`, `key_chord`, `scroll`, `Button`, `ScrollDirection`, `Modifiers`: read that file and add thin shim functions wrapping the existing internal API (do NOT rewrite the internals). This keeps the refactor surgical.

- [ ] **Step 5: Run unit tests**

Run: `cargo test --release -p fastuse-win --lib input::backend`
Expected: PASS (mock test + default test).

- [ ] **Step 6: Build whole workspace**

Run: `cargo build --release --workspace`
Expected: PASS. The `input_thread` and dispatch handlers haven't been routed through the trait yet — that's the next task. For now both old call sites and `SendInputBackend` coexist.

- [ ] **Step 7: Run lints**

Run: `cargo run -p xtask -- lints`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/fastuse-win/src/input/backend.rs crates/fastuse-win/src/input/sendinput_backend.rs crates/fastuse-win/src/input/mod.rs
# also any sendinput.rs shim functions you added
git commit -F <(printf '%s\n' \
  "feat(input): InputBackend trait + SendInputBackend impl" \
  "" \
  "Adds the abstraction the v2 humanization layer (Tier 1: Bezier curves +" \
  "timing jitter) and the future HardwareHidBackend (USB-HID via Pico) plug" \
  "into. SendInputBackend implements the trait with the existing sendinput" \
  "primitives unchanged — humanization arrives in the next commit." \
  "" \
  "MockBackend lives in tests for downstream unit-test use.")
```

### Task 3: Humanization layer — Bezier curves and typing profile

**Files:**
- Create: `crates/fastuse-win/src/input/humanize.rs`
- Modify: `crates/fastuse-win/src/input/mod.rs` (add `pub mod humanize;`)
- Test: inline `#[cfg(test)]` in `humanize.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/fastuse-win/src/input/humanize.rs`:

```rust
//! Tier 1 humanization: Bezier-curve mouse motion + per-keystroke jitter.
//! Defeats web behavioral bot detection (Cloudflare, hCaptcha, Google's
//! Play Console heuristics). Does NOT bypass kernel-mode anti-cheat or
//! Windows' LLMHF_INJECTED flag — see spec § Anti-cheat (out of scope).

use crate::input::backend::Point;

/// Sample a cubic Bezier from `p0` to `p3` with two control points offset
/// perpendicular to the path. Returns `n_samples` interpolated points.
/// `n_samples` includes both endpoints (≥ 2).
pub fn bezier_path(p0: Point, p3: Point, n_samples: usize, jitter: f32) -> Vec<Point> {
    let n = n_samples.max(2);
    let dx = (p3.x - p0.x) as f32;
    let dy = (p3.y - p0.y) as f32;
    // Perpendicular vector for control points; offset = ~15% of distance.
    let dist = (dx * dx + dy * dy).sqrt();
    let perp = (-dy / dist.max(1.0), dx / dist.max(1.0));
    let offset = dist * 0.15;
    // Two control points pulled toward the perpendicular at 1/3 and 2/3.
    let p1 = (
        p0.x as f32 + dx * 0.33 + perp.0 * offset * 0.5,
        p0.y as f32 + dy * 0.33 + perp.1 * offset * 0.5,
    );
    let p2 = (
        p0.x as f32 + dx * 0.66 - perp.0 * offset * 0.3,
        p0.y as f32 + dy * 0.66 - perp.1 * offset * 0.3,
    );

    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f32 / (n - 1) as f32;
        // Bell-curve velocity: ease-in-out (cosine-based).
        let t = 0.5 - 0.5 * (std::f32::consts::PI * t).cos();
        let one_t = 1.0 - t;
        let bx = one_t.powi(3) * p0.x as f32
            + 3.0 * one_t.powi(2) * t * p1.0
            + 3.0 * one_t * t.powi(2) * p2.0
            + t.powi(3) * p3.x as f32;
        let by = one_t.powi(3) * p0.y as f32
            + 3.0 * one_t.powi(2) * t * p1.1
            + 3.0 * one_t * t.powi(2) * p2.1
            + t.powi(3) * p3.y as f32;
        // Per-sample pixel jitter scaled by `jitter` factor (1 px max at 1.0).
        let jx = if jitter > 0.0 { (deterministic_jitter(i, 0) * jitter) as i32 } else { 0 };
        let jy = if jitter > 0.0 { (deterministic_jitter(i, 1) * jitter) as i32 } else { 0 };
        out.push(Point { x: bx.round() as i32 + jx, y: by.round() as i32 + jy });
    }
    out
}

/// Tiny deterministic per-sample jitter. Range: roughly -1.5..1.5 (px).
/// Deterministic so tests are reproducible; for production randomness wrap a
/// real RNG at the call site if needed.
fn deterministic_jitter(i: usize, axis: u8) -> f32 {
    let h = (i as u64).wrapping_mul(2654435761).wrapping_add(axis as u64 * 73);
    ((h % 31) as f32 - 15.0) / 10.0
}

/// Recommended sample count for a motion of `dist` pixels at ~60Hz over
/// `duration_ms`. Returns ≥ 2.
pub fn sample_count(dist_px: f32, duration_ms: u32) -> usize {
    // 60 samples per second; at least one per 10px of distance.
    let by_time = (duration_ms as f32 / (1000.0 / 60.0)).round() as usize;
    let by_dist = (dist_px / 10.0).ceil() as usize;
    by_time.max(by_dist).max(2)
}

/// Recommended motion duration for `dist_px`. Short moves: ~150ms; cross-screen
/// (≥1500px): ~500ms. Linear-ish in between.
pub fn motion_duration_ms(dist_px: f32) -> u32 {
    let clamped = dist_px.clamp(0.0, 1500.0);
    150 + ((clamped / 1500.0) * 350.0) as u32
}

/// Per-key interval generator for typing. Returns intervals (ms) for `n_keys`
/// keystrokes, drawn from a normal distribution (mean, stddev). Adds a
/// natural longer pause every 8-15 keys.
pub fn typing_intervals(n_keys: usize, mean_ms: u32, stddev_ms: u32) -> Vec<u32> {
    let mut out = Vec::with_capacity(n_keys);
    for i in 0..n_keys {
        let base = sample_normal(i, mean_ms as f32, stddev_ms as f32) as u32;
        // Natural longer pause every ~10 keys (8-15 range).
        let extra = if i > 0 && i % (8 + (i % 8)) == 0 {
            200 + (deterministic_jitter(i, 7).abs() * 200.0) as u32
        } else {
            0
        };
        out.push((base + extra).max(10));
    }
    out
}

/// Box-Muller-ish deterministic normal sample at index `i`.
fn sample_normal(i: usize, mean: f32, stddev: f32) -> f32 {
    let u1 = ((i as u64 * 9301 + 49297) % 233280) as f32 / 233280.0;
    let u2 = (((i + 7) as u64 * 4159 + 12349) % 233280) as f32 / 233280.0;
    let z = (-2.0 * u1.max(1e-6).ln()).sqrt() * (2.0 * std::f32::consts::PI * u2).cos();
    mean + stddev * z
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bezier_path_endpoints_match() {
        let p0 = Point { x: 0, y: 0 };
        let p3 = Point { x: 100, y: 100 };
        let path = bezier_path(p0, p3, 10, 0.0);
        assert_eq!(path.first(), Some(&p0));
        assert_eq!(path.last(), Some(&p3));
    }

    #[test]
    fn bezier_path_min_two_samples() {
        let path = bezier_path(Point { x: 0, y: 0 }, Point { x: 10, y: 10 }, 0, 0.0);
        assert_eq!(path.len(), 2);
    }

    #[test]
    fn bezier_path_curves_not_straight_with_jitter_zero() {
        // Even with zero jitter, the Bezier should curve — the perpendicular
        // control offset means the midpoint should be off the straight line.
        let p0 = Point { x: 0, y: 0 };
        let p3 = Point { x: 200, y: 0 };
        let path = bezier_path(p0, p3, 21, 0.0);
        let mid = path[10];
        // Straight line midpoint would be (100, 0). Bezier should differ.
        assert!((mid.y).abs() > 0, "midpoint y={} expected non-zero", mid.y);
    }

    #[test]
    fn sample_count_at_least_two() {
        assert!(sample_count(0.0, 0) >= 2);
        assert!(sample_count(1000.0, 200) >= 2);
    }

    #[test]
    fn motion_duration_short_moves_around_150ms() {
        assert!(motion_duration_ms(50.0) < 200);
    }

    #[test]
    fn motion_duration_long_moves_around_500ms() {
        assert!(motion_duration_ms(2000.0) >= 450);
    }

    #[test]
    fn typing_intervals_have_correct_count() {
        let v = typing_intervals(20, 80, 30);
        assert_eq!(v.len(), 20);
    }

    #[test]
    fn typing_intervals_have_minimum_floor() {
        let v = typing_intervals(50, 5, 5);
        for &iv in &v {
            assert!(iv >= 10, "interval {} below floor", iv);
        }
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test --release -p fastuse-win --lib input::humanize`
Expected: all 8 tests PASS.

- [ ] **Step 3: Declare module**

Add to `crates/fastuse-win/src/input/mod.rs`:
```rust
pub mod humanize;
```

- [ ] **Step 4: Build**

Run: `cargo build --release -p fastuse-win`
Expected: PASS.

- [ ] **Step 5: Lints**

Run: `cargo run -p xtask -- lints`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/fastuse-win/src/input/humanize.rs crates/fastuse-win/src/input/mod.rs
git commit -F <(printf '%s\n' \
  "feat(input): Tier 1 humanization primitives" \
  "" \
  "Bezier-curve path generator (cubic with perpendicular control offsets," \
  "ease-in-out velocity, optional per-sample jitter), motion duration heuristic" \
  "(~150ms short / ~500ms cross-screen), and typing-interval generator with" \
  "natural longer pauses every 8-15 keystrokes." \
  "" \
  "Defeats web behavioral bot detection (Cloudflare-class). Does not bypass" \
  "Windows' LLMHF_INJECTED flag or kernel-mode anti-cheat — out of scope." \
  "" \
  "Wired into SendInputBackend in next commit.")
```

### Task 4: Wire humanization into SendInputBackend and route input_thread through the trait

**Files:**
- Modify: `crates/fastuse-win/src/input/sendinput_backend.rs`
- Modify: `crates/fastuse-win/src/input_thread.rs`
- Test: inline `#[cfg(test)]` in `sendinput_backend.rs` using a recording inner-shim, OR a separate integration test that exercises the mock backend through the input thread

- [ ] **Step 1: Update `sendinput_backend.rs` MouseMove and Drag arms to interpolate when humanize=true**

Replace the `MouseMove` arm in `dispatch`:
```rust
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
```

Replace `Drag` analogously: humanized → MouseDown, then walk Bezier path with sleep, then MouseUp. Non-humanized → MouseDown, MouseMoveAbsolute(to), MouseUp.

Replace `KeyType` to honor `profile.humanize`:
```rust
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
```

Also bump `capabilities()` to set `humanized_motion: true, humanized_typing: true`.

- [ ] **Step 2: Add a unit test that humanized MouseMove dispatches multiple points**

In `sendinput_backend.rs`:

```rust
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
}
```

- [ ] **Step 3: Refactor `input_thread.rs` to dispatch through `Box<dyn InputBackend>`**

In `input_thread.rs`:
1. Change the worker's owned state to `Box<dyn InputBackend>` (boxed because trait objects).
2. The worker's spawn function instantiates `SendInputBackend::new()` by default and stores it as the boxed backend.
3. `InputJob` variants that previously called `sendinput::*` directly now translate to `InputAction` enum and call `backend.dispatch(action)`.

The `InputJob` enum may have variants like `Click`, `Type`, `Key`, `Move`, `Drag`, `Scroll`, `FlushHeldModifiers` — keep `FlushHeldModifiers` as a special-case (modifier guard); convert the rest into `InputAction`s. If any existing variant carries a `humanize: bool` flag (none should yet), pipe it through; otherwise default to `MotionProfile::default()` / `TypingProfile::default()`.

- [ ] **Step 4: Run tests**

Run: `cargo test --release -p fastuse-win --lib input`
Expected: PASS (humanize tests + sendinput_backend test + any existing input_thread tests).

- [ ] **Step 5: Build full workspace**

Run: `cargo build --release --workspace`
Expected: PASS.

- [ ] **Step 6: Lints**

Run: `cargo run -p xtask -- lints`
Expected: PASS. Confirms D-25 and D-26 still hold (the backend trait dispatches on the input STA thread, not tokio).

- [ ] **Step 7: Commit**

```bash
git add crates/fastuse-win/src/input/sendinput_backend.rs crates/fastuse-win/src/input_thread.rs
git commit -F <(printf '%s\n' \
  "feat(input): humanized MouseMove, Drag, KeyType in SendInputBackend" \
  "" \
  "Bezier path interpolation at ~60Hz with ease-in-out velocity for mouse" \
  "motion; per-keystroke normal-distribution intervals with natural longer" \
  "pauses for KeyType. Honors MotionProfile.humanize / TypingProfile.humanize" \
  "for opt-out." \
  "" \
  "input_thread now routes all jobs through Box<dyn InputBackend>." \
  "FlushHeldModifiers retained as backend-bypassing special case for the" \
  "modifier_guard recovery path.")
```

### Task 5: Coordinate scaling — pure math module

**Files:**
- Create: `crates/fastuse-win/src/scaling.rs`
- Modify: `crates/fastuse-win/src/lib.rs` (add `pub mod scaling;`)
- Test: inline `#[cfg(test)]` in `scaling.rs`

- [ ] **Step 1: Write the tests first**

Create `crates/fastuse-win/src/scaling.rs`:

```rust
//! Coordinate scaling between native virtual-desktop pixels and the scaled
//! image space Claude reasons in (target ~1024 longest side per Anthropic
//! `computer_20251124`). Pure math here; the per-session state machine lives
//! in scaling::context (next task).

use fastuse_proto::coords::{Point, Rect};

/// Default target for the longest side, in scaled pixels.
pub const DEFAULT_TARGET_MAX: u32 = 1024;

/// Compute the uniform scale ratio for a monitor of `(native_w, native_h)`.
/// `target_max` is the desired longest-side length in scaled space. The
/// returned ratio is `native / scaled`, i.e. divide native by ratio to get
/// scaled, multiply scaled by ratio to get native.
pub fn compute_ratio(native_w: u32, native_h: u32, target_max: u32) -> f64 {
    let longest = native_w.max(native_h) as f64;
    let target = target_max.max(1) as f64;
    (longest / target).max(1.0)
}

/// Scaled dimensions for a native monitor at the given ratio.
pub fn scaled_dims(native_w: u32, native_h: u32, ratio: f64) -> (u32, u32) {
    let w = ((native_w as f64) / ratio).round() as u32;
    let h = ((native_h as f64) / ratio).round() as u32;
    (w.max(1), h.max(1))
}

/// Map a native rect to scaled image coords.
pub fn scale_rect_to_image(native: Rect, ratio: f64) -> Rect {
    Rect {
        x: ((native.x as f64) / ratio).round() as i32,
        y: ((native.y as f64) / ratio).round() as i32,
        w: ((native.w as f64) / ratio).round() as i32,
        h: ((native.h as f64) / ratio).round() as i32,
    }
}

/// Map a point from scaled image coords to native virtual-desktop coords.
/// `monitor_origin` is the captured monitor's top-left in virtual-desktop
/// space (top-left of leftmost monitor on the desktop = (0, 0); origins for
/// secondary monitors are e.g. (-1920, 0) or (1440, 0) depending on layout).
pub fn point_to_native(scaled: Point, ratio: f64, monitor_origin: Point) -> Point {
    Point {
        x: ((scaled.x as f64) * ratio).round() as i32 + monitor_origin.x,
        y: ((scaled.y as f64) * ratio).round() as i32 + monitor_origin.y,
    }
}

/// True when `scaled` is within the `(0..scaled_w, 0..scaled_h)` box.
pub fn point_in_scaled_bounds(scaled: Point, scaled_w: u32, scaled_h: u32) -> bool {
    scaled.x >= 0 && scaled.y >= 0
        && (scaled.x as u32) < scaled_w
        && (scaled.y as u32) < scaled_h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratio_for_1440p_to_1024() {
        let r = compute_ratio(2560, 1440, 1024);
        assert!((r - 2.5).abs() < 1e-6);
    }

    #[test]
    fn ratio_for_1080p_to_1024() {
        let r = compute_ratio(1920, 1080, 1024);
        assert!((r - 1.875).abs() < 1e-6);
    }

    #[test]
    fn ratio_never_below_one_for_small_screens() {
        // If native is already smaller than target, we don't upscale (ratio
        // clamped at 1.0, image returned at native size).
        let r = compute_ratio(800, 600, 1024);
        assert!((r - 1.0).abs() < 1e-6);
    }

    #[test]
    fn scaled_dims_preserve_aspect() {
        let (w, h) = scaled_dims(2560, 1440, 2.5);
        assert_eq!((w, h), (1024, 576));
    }

    #[test]
    fn round_trip_point_within_one_px() {
        let ratio = 2.5;
        let origin = Point { x: 0, y: 0 };
        let native_in = Point { x: 1280, y: 720 };
        // native -> scaled -> native
        let scaled = Point {
            x: ((native_in.x as f64) / ratio).round() as i32,
            y: ((native_in.y as f64) / ratio).round() as i32,
        };
        let native_out = point_to_native(scaled, ratio, origin);
        assert!((native_out.x - native_in.x).abs() <= 1);
        assert!((native_out.y - native_in.y).abs() <= 1);
    }

    #[test]
    fn point_to_native_offsets_by_monitor_origin() {
        let origin = Point { x: -1920, y: 0 };
        let p = point_to_native(Point { x: 100, y: 200 }, 1.0, origin);
        assert_eq!(p, Point { x: -1820, y: 200 });
    }

    #[test]
    fn point_in_bounds_basic() {
        assert!(point_in_scaled_bounds(Point { x: 0, y: 0 }, 1024, 576));
        assert!(point_in_scaled_bounds(Point { x: 1023, y: 575 }, 1024, 576));
        assert!(!point_in_scaled_bounds(Point { x: 1024, y: 0 }, 1024, 576));
        assert!(!point_in_scaled_bounds(Point { x: -1, y: 0 }, 1024, 576));
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test --release -p fastuse-win --lib scaling`
Expected: 7 tests PASS.

- [ ] **Step 3: Declare module in `lib.rs`**

Add to `crates/fastuse-win/src/lib.rs`:
```rust
pub mod scaling;
```

- [ ] **Step 4: Build**

Run: `cargo build --release -p fastuse-win`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/fastuse-win/src/scaling.rs crates/fastuse-win/src/lib.rs
git commit -F <(printf '%s\n' \
  "feat(scaling): pure coordinate scaling math" \
  "" \
  "compute_ratio, scaled_dims, scale_rect_to_image, point_to_native," \
  "point_in_scaled_bounds. Default target_max=1024 matches Anthropic" \
  "computer_20251124. Round-trip identity within 1 px (rounding tolerance)." \
  "" \
  "Stateful per-session ScaleContext lives in next commit.")
```

### Task 6: Coordinate scaling — per-session state machine

**Files:**
- Modify: `crates/fastuse-win/src/scaling.rs` (add `Context`, `Stack`)
- Test: inline `#[cfg(test)]` in `scaling.rs`

- [ ] **Step 1: Append the state machine to `scaling.rs`**

Add after the existing module content:

```rust
use std::time::Instant;

/// Snapshot of one screenshot's scaling state.
#[derive(Debug, Clone)]
pub struct ScaleSnapshot {
    pub ratio: f64,
    pub monitor_origin: Point,
    pub native_w: u32,
    pub native_h: u32,
    pub scaled_w: u32,
    pub scaled_h: u32,
    pub captured_at: Instant,
}

/// Per-session stack of scale contexts. The top is the "current" scale
/// (last screenshot's). `zoom` actions push; `screenshot` resets to a single-
/// element stack (popping back to fullscreen).
#[derive(Debug, Default)]
pub struct ScaleStack {
    stack: Vec<ScaleSnapshot>,
}

impl ScaleStack {
    pub fn new() -> Self { Self { stack: Vec::new() } }

    /// Replace the stack with a single snapshot (`screenshot` action).
    pub fn reset(&mut self, snap: ScaleSnapshot) {
        self.stack.clear();
        self.stack.push(snap);
    }

    /// Push a snapshot on top (`zoom` action).
    pub fn push(&mut self, snap: ScaleSnapshot) {
        self.stack.push(snap);
    }

    /// Pop one level (no-op if empty or stack length 1 — we never pop the
    /// fullscreen base; if you want to reset, call `reset` with a fresh
    /// screenshot).
    pub fn pop_zoom(&mut self) {
        if self.stack.len() > 1 {
            self.stack.pop();
        }
    }

    pub fn current(&self) -> Option<&ScaleSnapshot> {
        self.stack.last()
    }

    pub fn translate(&self, scaled: Point) -> Result<Point, ScaleError> {
        let s = self.stack.last().ok_or(ScaleError::NoContext)?;
        if !point_in_scaled_bounds(scaled, s.scaled_w, s.scaled_h) {
            return Err(ScaleError::OutOfBounds {
                point: scaled,
                bounds: (s.scaled_w, s.scaled_h),
            });
        }
        Ok(point_to_native(scaled, s.ratio, s.monitor_origin))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ScaleError {
    #[error("no scale context — call screenshot first")]
    NoContext,
    #[error("scaled point {point:?} out of bounds {bounds:?}")]
    OutOfBounds { point: Point, bounds: (u32, u32) },
}

#[cfg(test)]
mod state_tests {
    use super::*;

    fn snap(ratio: f64, origin: (i32, i32), nw: u32, nh: u32) -> ScaleSnapshot {
        let (sw, sh) = scaled_dims(nw, nh, ratio);
        ScaleSnapshot {
            ratio,
            monitor_origin: Point { x: origin.0, y: origin.1 },
            native_w: nw, native_h: nh,
            scaled_w: sw, scaled_h: sh,
            captured_at: Instant::now(),
        }
    }

    #[test]
    fn empty_stack_returns_no_context_error() {
        let s = ScaleStack::new();
        assert!(matches!(s.translate(Point { x: 0, y: 0 }), Err(ScaleError::NoContext)));
    }

    #[test]
    fn reset_then_translate_works() {
        let mut s = ScaleStack::new();
        s.reset(snap(2.5, (0, 0), 2560, 1440));
        let native = s.translate(Point { x: 100, y: 100 }).unwrap();
        assert_eq!(native, Point { x: 250, y: 250 });
    }

    #[test]
    fn out_of_bounds_returns_error() {
        let mut s = ScaleStack::new();
        s.reset(snap(2.5, (0, 0), 2560, 1440)); // scaled = 1024x576
        assert!(matches!(
            s.translate(Point { x: 2000, y: 0 }),
            Err(ScaleError::OutOfBounds { .. })
        ));
    }

    #[test]
    fn zoom_push_uses_top_of_stack() {
        let mut s = ScaleStack::new();
        s.reset(snap(2.5, (0, 0), 2560, 1440));
        // Simulate a zoom that gives ratio 0.5 (upscaled region).
        s.push(snap(0.5, (500, 300), 200, 150));
        let native = s.translate(Point { x: 50, y: 50 }).unwrap();
        // 50 * 0.5 + 500 = 525; 50 * 0.5 + 300 = 325
        assert_eq!(native, Point { x: 525, y: 325 });
    }

    #[test]
    fn pop_zoom_keeps_at_least_base() {
        let mut s = ScaleStack::new();
        s.reset(snap(2.5, (0, 0), 2560, 1440));
        s.push(snap(0.5, (500, 300), 200, 150));
        s.pop_zoom();
        s.pop_zoom();
        assert!(s.current().is_some()); // base still there
    }

    #[test]
    fn secondary_monitor_origin_offset() {
        let mut s = ScaleStack::new();
        // Secondary monitor to the left at native origin (-1920, 0).
        s.reset(snap(1.875, (-1920, 0), 1920, 1080));
        let native = s.translate(Point { x: 100, y: 100 }).unwrap();
        // 100 * 1.875 - 1920 = -1732.5 → -1733; 100 * 1.875 + 0 = 187.5 → 188
        assert_eq!(native, Point { x: -1733, y: 188 });
    }
}
```

- [ ] **Step 2: Add `thiserror` to fastuse-win Cargo.toml dependencies if not already**

Check `crates/fastuse-win/Cargo.toml`:
```
grep '^thiserror' crates/fastuse-win/Cargo.toml
```
If absent, add under `[dependencies]`:
```toml
thiserror = { workspace = true }
```
(or the explicit version pin matching the workspace).

- [ ] **Step 3: Run tests**

Run: `cargo test --release -p fastuse-win --lib scaling`
Expected: 7 (math) + 6 (state) = 13 tests PASS.

- [ ] **Step 4: Build**

Run: `cargo build --release -p fastuse-win`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/fastuse-win/src/scaling.rs crates/fastuse-win/Cargo.toml
git commit -F <(printf '%s\n' \
  "feat(scaling): per-session ScaleStack state machine" \
  "" \
  "ScaleSnapshot captures one screenshot's ratio + monitor origin + dims." \
  "ScaleStack supports reset (screenshot), push (zoom), pop_zoom, translate." \
  "translate returns NoContext error when stack empty, OutOfBounds when the" \
  "scaled point falls outside the captured image." \
  "" \
  "Multi-monitor offset verified — secondary at (-1920, 0) translates" \
  "correctly.")
```

### Task 7: Permissions module

**Files:**
- Create: `crates/fastuse-win/src/permissions.rs`
- Modify: `crates/fastuse-win/src/lib.rs` (add `pub mod permissions;`)
- Test: inline `#[cfg(test)]`

- [ ] **Step 1: Write the failing tests first**

Create `crates/fastuse-win/src/permissions.rs`:

```rust
//! Tool permission gating. Default = open. Safe-mode (env `FASTUSE_SAFE_MODE=1`
//! or config.toml `[permissions] safe_mode = true`) gates the destructive set.

use std::collections::HashSet;

/// Default tools gated when safe_mode is active.
pub const DEFAULT_GATED: &[&str] = &[
    "launch_app",
    "kill_process",
    "shell_exec",
    "clipboard_set_text",
    "clipboard_set_image",
];

#[derive(Debug, Clone)]
pub struct Permissions {
    pub safe_mode: bool,
    pub gated: HashSet<String>,
}

impl Default for Permissions {
    fn default() -> Self {
        // Default = wide open. safe_mode off, no tools gated.
        Self { safe_mode: false, gated: HashSet::new() }
    }
}

impl Permissions {
    /// Build a `Permissions` from environment + (optional) config.
    /// Precedence: per-call env (caller resolves) > daemon-launched env > config.
    pub fn from_env_and_config(safe_mode_cfg: bool, gated_cfg: Vec<String>) -> Self {
        let env_safe = std::env::var("FASTUSE_SAFE_MODE")
            .ok()
            .and_then(|v| match v.as_str() {
                "1" | "true" | "TRUE" => Some(true),
                "0" | "false" | "FALSE" => Some(false),
                _ => None,
            });
        let safe_mode = env_safe.unwrap_or(safe_mode_cfg);
        let gated = if safe_mode {
            if gated_cfg.is_empty() {
                DEFAULT_GATED.iter().map(|s| s.to_string()).collect()
            } else {
                gated_cfg.into_iter().collect()
            }
        } else {
            HashSet::new()
        };
        Self { safe_mode, gated }
    }

    pub fn is_allowed(&self, tool: &str) -> bool {
        if !self.safe_mode { return true; }
        !self.gated.contains(tool)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_permissions_allow_everything() {
        let p = Permissions::default();
        assert!(p.is_allowed("kill_process"));
        assert!(p.is_allowed("launch_app"));
        assert!(p.is_allowed("computer"));
    }

    #[test]
    fn safe_mode_with_default_gates_destructive_tools() {
        // Note: from_env_and_config reads FASTUSE_SAFE_MODE — for this test
        // we construct directly to avoid env races.
        let p = Permissions {
            safe_mode: true,
            gated: DEFAULT_GATED.iter().map(|s| s.to_string()).collect(),
        };
        assert!(!p.is_allowed("kill_process"));
        assert!(!p.is_allowed("launch_app"));
        assert!(!p.is_allowed("shell_exec"));
        assert!(p.is_allowed("computer"));
        assert!(p.is_allowed("list_windows"));
    }

    #[test]
    fn custom_gated_list_overrides_default() {
        let p = Permissions {
            safe_mode: true,
            gated: ["focus_window".to_string()].into_iter().collect(),
        };
        assert!(!p.is_allowed("focus_window"));
        assert!(p.is_allowed("kill_process")); // not in custom list
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test --release -p fastuse-win --lib permissions`
Expected: 3 tests PASS.

- [ ] **Step 3: Declare module**

Add to `crates/fastuse-win/src/lib.rs`:
```rust
pub mod permissions;
```

- [ ] **Step 4: Build**

Run: `cargo build --release -p fastuse-win`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/fastuse-win/src/permissions.rs crates/fastuse-win/src/lib.rs
git commit -F <(printf '%s\n' \
  "feat(permissions): safe-mode gating module" \
  "" \
  "Default = open. Safe-mode (FASTUSE_SAFE_MODE=1 env or config.toml" \
  "[permissions] safe_mode=true) gates launch_app, kill_process," \
  "shell_exec, clipboard_set_text, clipboard_set_image. Custom gated list" \
  "in config replaces defaults entirely.")
```

### Task 8: Config loader

**Files:**
- Create: `crates/fastuse-core/src/config.rs`
- Modify: `crates/fastuse-core/src/lib.rs` (add `pub mod config;`)
- Modify: `crates/fastuse-core/Cargo.toml` (add `toml`, `serde` if missing)
- Test: inline `#[cfg(test)]`

- [ ] **Step 1: Add dependencies**

Edit `crates/fastuse-core/Cargo.toml`. Add under `[dependencies]` if absent:
```toml
serde = { workspace = true, features = ["derive"] }
toml = "0.8"
```

- [ ] **Step 2: Write the tests first**

Create `crates/fastuse-core/src/config.rs`:

```rust
//! `%LOCALAPPDATA%\fastuse\config.toml` loader.
//!
//! Optional file. Sensible defaults if absent. Sections:
//! - [permissions] safe_mode (bool), gated_tools (Vec<String>)
//! - [input] default_humanize (bool), typing_mean_ms (u32), typing_stddev_ms (u32)
//! - [scaling] target_max (u32, default 1024)

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub permissions: PermissionsConfig,
    pub input: InputConfig,
    pub scaling: ScalingConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PermissionsConfig {
    pub safe_mode: bool,
    pub gated_tools: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct InputConfig {
    pub default_humanize: bool,
    pub typing_mean_ms: u32,
    pub typing_stddev_ms: u32,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self { default_humanize: true, typing_mean_ms: 80, typing_stddev_ms: 30 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ScalingConfig {
    pub target_max: u32,
}

impl Default for ScalingConfig {
    fn default() -> Self { Self { target_max: 1024 } }
}

/// Default config path: `%LOCALAPPDATA%\fastuse\config.toml`.
pub fn default_path() -> Option<PathBuf> {
    crate::local_app_data().ok().map(|p| p.join("config.toml"))
}

/// Load config from `path`. Missing file = `Config::default()`. Parse errors
/// logged via `tracing` and fall back to defaults.
pub fn load_or_default(path: &Path) -> Config {
    match std::fs::read_to_string(path) {
        Ok(s) => match toml::from_str(&s) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, path = ?path, "config parse failed; using defaults");
                Config::default()
            }
        },
        Err(_) => Config::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sensible() {
        let c = Config::default();
        assert!(!c.permissions.safe_mode);
        assert!(c.permissions.gated_tools.is_empty());
        assert!(c.input.default_humanize);
        assert_eq!(c.input.typing_mean_ms, 80);
        assert_eq!(c.scaling.target_max, 1024);
    }

    #[test]
    fn parses_full_config() {
        let toml_str = r#"
            [permissions]
            safe_mode = true
            gated_tools = ["kill_process", "shell_exec"]

            [input]
            default_humanize = false
            typing_mean_ms = 120
            typing_stddev_ms = 40

            [scaling]
            target_max = 1280
        "#;
        let c: Config = toml::from_str(toml_str).unwrap();
        assert!(c.permissions.safe_mode);
        assert_eq!(c.permissions.gated_tools.len(), 2);
        assert!(!c.input.default_humanize);
        assert_eq!(c.input.typing_mean_ms, 120);
        assert_eq!(c.scaling.target_max, 1280);
    }

    #[test]
    fn parses_partial_config() {
        let toml_str = r#"
            [permissions]
            safe_mode = true
        "#;
        let c: Config = toml::from_str(toml_str).unwrap();
        assert!(c.permissions.safe_mode);
        assert!(c.permissions.gated_tools.is_empty()); // default
        assert!(c.input.default_humanize); // default true
        assert_eq!(c.scaling.target_max, 1024); // default
    }

    #[test]
    fn missing_file_returns_defaults() {
        let nowhere = std::path::Path::new("Z:\\definitely\\not\\here\\config.toml");
        let c = load_or_default(nowhere);
        assert_eq!(c.scaling.target_max, 1024);
    }
}
```

- [ ] **Step 3: Declare in `crates/fastuse-core/src/lib.rs`**

Add:
```rust
pub mod config;
```

- [ ] **Step 4: Run tests**

Run: `cargo test --release -p fastuse-core --lib config`
Expected: 4 tests PASS.

- [ ] **Step 5: Build**

Run: `cargo build --release --workspace`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/fastuse-core/src/config.rs crates/fastuse-core/src/lib.rs crates/fastuse-core/Cargo.toml
git commit -F <(printf '%s\n' \
  "feat(core): config.toml loader" \
  "" \
  "Optional %LOCALAPPDATA%\\\\fastuse\\\\config.toml. Sections: permissions" \
  "(safe_mode, gated_tools), input (default_humanize, typing intervals)," \
  "scaling (target_max). Missing file or parse failure -> defaults; failure" \
  "logged via tracing.")
```

### Task 9: Wire types — `computer` action enum and Windows helper requests

**Files:**
- Modify: `crates/fastuse-proto/src/wire.rs` (add new request/response types)
- Test: inline `#[cfg(test)]` in `wire.rs` covering serde round-trip

- [ ] **Step 1: Append the new types to `crates/fastuse-proto/src/wire.rs`**

```rust
//! `computer` action enum mirrors Anthropic computer_20251124.
//! Coordinates are in scaled image-pixel space (Section 4 of the spec).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ComputerAction {
    Screenshot {
        #[serde(default)]
        monitor: Option<u32>,
    },
    LeftClick {
        coordinate: [i32; 2],
        #[serde(default)]
        text: Option<String>, // modifier chord like "ctrl+shift"
        #[serde(default = "yes")]
        humanize: bool,
    },
    RightClick { coordinate: [i32; 2], #[serde(default = "yes")] humanize: bool },
    MiddleClick { coordinate: [i32; 2], #[serde(default = "yes")] humanize: bool },
    DoubleClick { coordinate: [i32; 2], #[serde(default = "yes")] humanize: bool },
    TripleClick { coordinate: [i32; 2], #[serde(default = "yes")] humanize: bool },
    LeftClickDrag {
        start_coordinate: [i32; 2],
        coordinate: [i32; 2],
        #[serde(default = "yes")]
        humanize: bool,
        #[serde(default)]
        text: Option<String>, // modifier chord
    },
    LeftMouseDown {
        #[serde(default)]
        coordinate: Option<[i32; 2]>,
    },
    LeftMouseUp {
        #[serde(default)]
        coordinate: Option<[i32; 2]>,
    },
    MouseMove { coordinate: [i32; 2], #[serde(default = "yes")] humanize: bool },
    CursorPosition,
    Type {
        text: String,
        #[serde(default = "yes")]
        humanize: bool,
    },
    Key {
        text: String, // chord like "ctrl+l", "enter", "alt+f4"
    },
    HoldKey { text: String, duration: u32 },
    Scroll {
        coordinate: [i32; 2],
        scroll_direction: ScrollDir,
        scroll_amount: i32,
    },
    Wait { duration: u32 },
    Zoom {
        coordinate: [i32; 2],
        zoom_factor: f32,
    },
}

fn yes() -> bool { true }

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScrollDir { Up, Down, Left, Right }

/// Result of a `computer` action.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ComputerResult {
    /// True if the daemon successfully dispatched. False = error.
    pub ok: bool,
    /// Set for `Screenshot`/`Zoom` actions.
    #[serde(default)]
    pub image: Option<ImagePayload>,
    /// Set for `CursorPosition`.
    #[serde(default)]
    pub cursor: Option<[i32; 2]>,
    /// Scale snapshot the image was captured under (for Screenshot/Zoom).
    #[serde(default)]
    pub scale: Option<ScaleInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImagePayload {
    pub format: String, // "jpeg" | "png"
    pub width: u32,
    pub height: u32,
    /// Base64 (no data: prefix). MCP layer wraps as ImageContent; CLI may
    /// emit to file when `--out` provided.
    pub data_base64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScaleInfo {
    pub ratio: f64,
    pub monitor_origin: [i32; 2],
    pub native_w: u32,
    pub native_h: u32,
    pub scaled_w: u32,
    pub scaled_h: u32,
}

#[cfg(test)]
mod computer_action_tests {
    use super::*;

    #[test]
    fn screenshot_action_round_trips() {
        let a = ComputerAction::Screenshot { monitor: Some(1) };
        let json = serde_json::to_string(&a).unwrap();
        let back: ComputerAction = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
        assert!(json.contains("\"action\":\"screenshot\""));
    }

    #[test]
    fn left_click_action_round_trips_with_humanize_default() {
        let json = r#"{"action":"left_click","coordinate":[100,200]}"#;
        let a: ComputerAction = serde_json::from_str(json).unwrap();
        match a {
            ComputerAction::LeftClick { coordinate, humanize, .. } => {
                assert_eq!(coordinate, [100, 200]);
                assert!(humanize, "humanize defaults to true");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn left_click_action_honors_explicit_humanize_false() {
        let json = r#"{"action":"left_click","coordinate":[1,2],"humanize":false}"#;
        let a: ComputerAction = serde_json::from_str(json).unwrap();
        match a {
            ComputerAction::LeftClick { humanize, .. } => assert!(!humanize),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn key_chord_is_a_string_field() {
        let a = ComputerAction::Key { text: "ctrl+s".into() };
        let json = serde_json::to_string(&a).unwrap();
        assert!(json.contains("\"text\":\"ctrl+s\""));
    }

    #[test]
    fn scroll_direction_serializes_snake_case() {
        let a = ComputerAction::Scroll {
            coordinate: [10, 20],
            scroll_direction: ScrollDir::Down,
            scroll_amount: 3,
        };
        let json = serde_json::to_string(&a).unwrap();
        assert!(json.contains("\"scroll_direction\":\"down\""));
    }
}
```

- [ ] **Step 2: Add Windows helper request types**

Append in the same `wire.rs`:

```rust
/// Top-level wire `Request` enum gets these new variants. Add them to the
/// existing enum (don't redefine it; just add variants):
///
/// ```ignore
/// pub enum Request {
///     // ... existing variants like Ping, Status, Stop, Warmup, ListWindows, etc.
///     Computer(ComputerRequest),
///     ListWindowsV2(ListWindowsRequest),
///     FocusWindow(FocusWindowRequest),
///     ForegroundWindow,
///     WaitForWindow(WaitForWindowRequest),
///     ListProcesses(ListProcessesRequest),
///     KillProcess(KillProcessRequest),
///     LaunchApp(LaunchAppRequest),
///     ShellExec(ShellExecRequest),
///     ClipboardGetText,
///     ClipboardSetText(ClipboardSetTextRequest),
///     InspectAt(InspectAtRequest),
///     UiaQuery(UiaQueryRequest),
///     UiaTree(UiaTreeRequest),
/// }
/// ```
///
/// Some of these may already exist on `Request` from v1 (e.g. ListWindows,
/// ListProcesses). Reuse where the shape matches; add new variants only when
/// the v2 shape differs. The v2 versions are documented below — keep names
/// stable and pick whichever variant name doesn't collide.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ComputerRequest {
    pub action: ComputerAction,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ListWindowsRequest {
    pub title_substr: Option<String>,
    pub process_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FocusWindowRequest { pub hwnd: u64 }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct WaitForWindowRequest {
    pub title_substr: Option<String>,
    pub process_name: Option<String>,
    pub timeout_ms: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ListProcessesRequest {
    pub name_substr: Option<String>,
    pub visible_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KillProcessRequest {
    Pid { pid: u32 },
    Name { stem: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LaunchAppRequest { pub target: String }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ShellExecRequest {
    pub command: String,
    pub shell: Option<String>, // "powershell" | "cmd" | None (default)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClipboardSetTextRequest { pub text: String }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InspectAtRequest { pub x: i32, pub y: i32 }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UiaQueryRequest {
    /// JSON selector grammar from v1 (ByName / ByControlType / ByClass /
    /// ByAutomationId / And / Or / Not). Reuse the existing Selector type.
    pub selector: crate::selector::Selector,
    pub root_hwnd: Option<u64>,
    pub max_results: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UiaTreeRequest {
    pub hwnd: Option<u64>,
    pub max_depth: Option<u32>,
}
```

If the existing `Request` enum already has `ListWindows`, `ListProcesses`, `KillProcess`, `LaunchApp`, `ShellExec`, `ClipboardGetText`, `ClipboardSetText`, `InspectAt`, `UiaQuery`, `UiaTree`: keep them, just confirm their request structs match the shapes above. Otherwise add the variants and the structs.

- [ ] **Step 3: Run tests**

Run: `cargo test --release -p fastuse-proto --lib`
Expected: existing serde tests + 5 new `computer_action_tests` PASS.

- [ ] **Step 4: Build**

Run: `cargo build --release --workspace`
Expected: PASS. (Daemon dispatch may have un-wired `Request` arms — that's the next task; mark them with a temporary `_ => Response::Error(...)`.)

- [ ] **Step 5: Commit**

```bash
git add crates/fastuse-proto/src/wire.rs
git commit -F <(printf '%s\n' \
  "feat(proto): computer action enum + Windows helper request types" \
  "" \
  "Mirrors Anthropic computer_20251124 schema (snake_case action tag," \
  "coordinate as [i32; 2], snake_case scroll_direction). Extension fields:" \
  "humanize (default true) on motion/typing actions, monitor (optional) on" \
  "Screenshot." \
  "" \
  "ComputerResult carries optional ImagePayload (base64 JPEG/PNG)," \
  "cursor position, ScaleInfo. Windows helpers: ListWindowsRequest," \
  "FocusWindowRequest, WaitForWindowRequest, ListProcessesRequest," \
  "KillProcessRequest (kind=pid/name), LaunchAppRequest, ShellExecRequest," \
  "ClipboardSetTextRequest, InspectAtRequest, UiaQueryRequest," \
  "UiaTreeRequest.")
```

### Task 10: Daemon dispatch — `computer` action handlers

**Files:**
- Modify: `crates/fastuse-daemon/src/dispatch.rs`
- Modify: `crates/fastuse-daemon/src/server.rs` if it owns the `ScaleStack` per-session state
- Modify: `crates/fastuse-win/src/capture/screenshot.rs` (emit `ScaleSnapshot` data alongside the encoded image)

- [ ] **Step 1: Update `screenshot.rs` to return scaling metadata**

In `crates/fastuse-win/src/capture/screenshot.rs`: change the return type to include scaling info. If existing return is `Result<EncodedImage, Error>`, change to `Result<(EncodedImage, fastuse_win::scaling::ScaleSnapshot), Error>` (or wrap them in a new struct). The capture path must:
1. Determine target monitor (foreground HWND → `MonitorFromWindow` or explicit monitor index)
2. Fetch monitor's native dimensions and virtual-desktop origin
3. Compute `ratio = scaling::compute_ratio(native_w, native_h, target_max)`
4. Capture native pixels into staging buffer (existing code)
5. Resize to scaled dimensions (use `image::imageops::resize` with `FilterType::Triangle`)
6. Encode to JPEG via existing encoder
7. Build `ScaleSnapshot { ratio, monitor_origin, native_w, native_h, scaled_w, scaled_h, captured_at: Instant::now() }`

The same logic for `handle_screenshot_region` / zoom path — different ratio (zoom upscales by `zoom_factor`, snapshot's ratio reflects `native_region_size / target_max`).

- [ ] **Step 2: Add per-session `ScaleStack` to the daemon's session context**

In `dispatch.rs` (or wherever `Ctx`/session state is held), add:
```rust
pub scale: Arc<Mutex<fastuse_win::scaling::ScaleStack>>,
```
Initialize as empty in the session-spawn path.

- [ ] **Step 3: Add the `computer` dispatch handler**

In `dispatch.rs` add an arm for `Request::Computer(ComputerRequest { action })`:

```rust
Request::Computer(ComputerRequest { action }) => {
    use fastuse_proto::wire::{ComputerAction, ComputerResult, ImagePayload, ScaleInfo, ScrollDir};
    use fastuse_win::input::backend::{InputAction, MotionProfile, MouseButton, Point, ScrollDirection, TypingProfile};
    use fastuse_win::scaling::{ScaleSnapshot, ScaleStack};

    match action {
        ComputerAction::Screenshot { monitor } => {
            let cap = ctx.capture.as_ref().ok_or_else(|| Error::new(ErrorCode::Internal, "capture unavailable"))?;
            let (img, snap) = cap.run(move || {
                fastuse_win::capture::handle_screenshot_v2(monitor, /* target_max */ ctx.config.scaling.target_max)
            }).map_err(|e| Error::new(ErrorCode::Internal, e.to_string()))?
              .map_err(|e| e)?;
            ctx.scale.lock().unwrap().reset(snap.clone());
            Response::Computer(ComputerResult {
                ok: true,
                image: Some(ImagePayload {
                    format: img.format.into(),
                    width: snap.scaled_w,
                    height: snap.scaled_h,
                    data_base64: base64::engine::general_purpose::STANDARD.encode(&img.bytes),
                }),
                cursor: None,
                scale: Some(scale_info_from(&snap)),
            })
        }
        ComputerAction::LeftClick { coordinate, text, humanize } => {
            let modifiers = parse_chord_modifiers(text.as_deref());
            let native = translate_or_err(&ctx.scale, coordinate)?;
            let action = InputAction::MouseClick {
                at: native, button: MouseButton::Left, count: 1,
                modifiers, humanize,
            };
            ctx.input.send(InputJob::Backend(action))
                .map_err(|e| Error::new(ErrorCode::Internal, e.to_string()))?;
            Response::Computer(ComputerResult { ok: true, image: None, cursor: None, scale: None })
        }
        // ... analogous arms for RightClick, MiddleClick, DoubleClick (count: 2), TripleClick (count: 3)
        ComputerAction::Type { text, humanize } => {
            let action = InputAction::KeyType {
                text,
                profile: TypingProfile { humanize, ..TypingProfile::default() },
            };
            ctx.input.send(InputJob::Backend(action))
                .map_err(|e| Error::new(ErrorCode::Internal, e.to_string()))?;
            Response::Computer(ComputerResult { ok: true, image: None, cursor: None, scale: None })
        }
        ComputerAction::Key { text } => {
            let (vks, _) = parse_key_chord(&text)?;
            ctx.input.send(InputJob::Backend(InputAction::KeyChord { keys: vks, hold_ms: None }))
                .map_err(|e| Error::new(ErrorCode::Internal, e.to_string()))?;
            Response::Computer(ComputerResult { ok: true, image: None, cursor: None, scale: None })
        }
        ComputerAction::HoldKey { text, duration } => {
            let (vks, _) = parse_key_chord(&text)?;
            ctx.input.send(InputJob::Backend(InputAction::KeyChord { keys: vks, hold_ms: Some(duration) }))
                .map_err(|e| Error::new(ErrorCode::Internal, e.to_string()))?;
            Response::Computer(ComputerResult { ok: true, image: None, cursor: None, scale: None })
        }
        ComputerAction::Scroll { coordinate, scroll_direction, scroll_amount } => {
            let native = translate_or_err(&ctx.scale, coordinate)?;
            let dir = match scroll_direction {
                ScrollDir::Up => ScrollDirection::Up,
                ScrollDir::Down => ScrollDirection::Down,
                ScrollDir::Left => ScrollDirection::Left,
                ScrollDir::Right => ScrollDirection::Right,
            };
            ctx.input.send(InputJob::Backend(InputAction::Scroll {
                at: native, direction: dir, amount: scroll_amount,
            })).map_err(|e| Error::new(ErrorCode::Internal, e.to_string()))?;
            Response::Computer(ComputerResult { ok: true, image: None, cursor: None, scale: None })
        }
        ComputerAction::Wait { duration } => {
            tokio::time::sleep(std::time::Duration::from_millis(duration as u64)).await;
            Response::Computer(ComputerResult { ok: true, image: None, cursor: None, scale: None })
        }
        ComputerAction::CursorPosition => {
            let pos = fastuse_win::input::cursor_position();
            Response::Computer(ComputerResult {
                ok: true, image: None,
                cursor: Some([pos.x, pos.y]),
                scale: None,
            })
        }
        ComputerAction::MouseMove { coordinate, humanize } => {
            let from = fastuse_win::input::cursor_position();
            let to = translate_or_err(&ctx.scale, coordinate)?;
            ctx.input.send(InputJob::Backend(InputAction::MouseMove {
                from: Point { x: from.x, y: from.y }, to,
                profile: MotionProfile { humanize, ..MotionProfile::default() },
            })).map_err(|e| Error::new(ErrorCode::Internal, e.to_string()))?;
            Response::Computer(ComputerResult { ok: true, image: None, cursor: None, scale: None })
        }
        ComputerAction::LeftClickDrag { start_coordinate, coordinate, humanize, text } => {
            let modifiers = parse_chord_modifiers(text.as_deref());
            let from = translate_or_err(&ctx.scale, start_coordinate)?;
            let to = translate_or_err(&ctx.scale, coordinate)?;
            ctx.input.send(InputJob::Backend(InputAction::Drag {
                from, to, button: MouseButton::Left,
                profile: MotionProfile { humanize, ..MotionProfile::default() },
                modifiers,
            })).map_err(|e| Error::new(ErrorCode::Internal, e.to_string()))?;
            Response::Computer(ComputerResult { ok: true, image: None, cursor: None, scale: None })
        }
        ComputerAction::LeftMouseDown { coordinate } | ComputerAction::LeftMouseUp { coordinate } => {
            let at = match coordinate {
                Some(c) => translate_or_err(&ctx.scale, c)?,
                None => {
                    let p = fastuse_win::input::cursor_position();
                    Point { x: p.x, y: p.y }
                }
            };
            let act = match action {
                ComputerAction::LeftMouseDown { .. } => InputAction::MouseDown { at, button: MouseButton::Left },
                _ => InputAction::MouseUp { at, button: MouseButton::Left },
            };
            ctx.input.send(InputJob::Backend(act))
                .map_err(|e| Error::new(ErrorCode::Internal, e.to_string()))?;
            Response::Computer(ComputerResult { ok: true, image: None, cursor: None, scale: None })
        }
        ComputerAction::Zoom { coordinate, zoom_factor } => {
            // crop a region around `coordinate` of size scaled_w/zoom_factor
            // x scaled_h/zoom_factor, capture native pixels for that region,
            // upscale to standard target dimensions, push ScaleSnapshot.
            let snap_now = ctx.scale.lock().unwrap().current().cloned()
                .ok_or_else(|| Error::new(ErrorCode::Internal, "no scale context — call screenshot first"))?;
            let cap = ctx.capture.as_ref().ok_or_else(|| Error::new(ErrorCode::Internal, "capture unavailable"))?;
            let (img, snap_zoom) = cap.run(move || {
                fastuse_win::capture::handle_zoom_v2(snap_now, coordinate, zoom_factor, ctx.config.scaling.target_max)
            }).map_err(|e| Error::new(ErrorCode::Internal, e.to_string()))?
              .map_err(|e| e)?;
            ctx.scale.lock().unwrap().push(snap_zoom.clone());
            Response::Computer(ComputerResult {
                ok: true,
                image: Some(ImagePayload {
                    format: img.format.into(),
                    width: snap_zoom.scaled_w,
                    height: snap_zoom.scaled_h,
                    data_base64: base64::engine::general_purpose::STANDARD.encode(&img.bytes),
                }),
                cursor: None,
                scale: Some(scale_info_from(&snap_zoom)),
            })
        }
    }
}
```

Add the helpers `parse_chord_modifiers`, `parse_key_chord`, `translate_or_err`, `scale_info_from` either inline or in a sibling `dispatch_computer.rs`. `parse_key_chord` parses `"ctrl+l"` / `"enter"` / `"alt+f4"` into `Vec<u16>` of VK codes; reuse existing chord parsing if the v1 `Key` request had it.

Add `InputJob::Backend(InputAction)` variant to `crates/fastuse-win/src/input_thread.rs::InputJob` if missing — this is the "any backend action" job kind.

- [ ] **Step 2: Add `screenshot_v2` and `zoom_v2` helpers in capture**

In `crates/fastuse-win/src/capture/screenshot.rs` (or new `screenshot_v2.rs`):

```rust
use crate::scaling::{compute_ratio, scaled_dims, ScaleSnapshot};
use fastuse_proto::coords::{Point, Rect};
use std::time::Instant;

pub fn handle_screenshot_v2(monitor: Option<u32>, target_max: u32)
    -> Result<(EncodedImage, ScaleSnapshot), fastuse_proto::error::Error>
{
    let mon = match monitor {
        Some(n) => crate::window::monitors::get_monitor(n)?,
        None => crate::window::monitors::foreground_monitor()?,
    };
    let frame = crate::capture::capture_into_staging(mon.index, None)?;
    let ratio = compute_ratio(frame.w, frame.h, target_max);
    let (sw, sh) = scaled_dims(frame.w, frame.h, ratio);
    let resized = resize_bgra_to_rgba(&frame, sw, sh);
    let img = crate::capture::encode::encode_jpeg(&resized, sw, sh)?;
    let snap = ScaleSnapshot {
        ratio,
        monitor_origin: Point { x: mon.origin_x, y: mon.origin_y },
        native_w: frame.w, native_h: frame.h,
        scaled_w: sw, scaled_h: sh,
        captured_at: Instant::now(),
    };
    Ok((img, snap))
}

pub fn handle_zoom_v2(base: ScaleSnapshot, coordinate: [i32; 2], zoom_factor: f32, target_max: u32)
    -> Result<(EncodedImage, ScaleSnapshot), fastuse_proto::error::Error>
{
    // Compute native rect of the zoom region.
    let scaled_center = Point { x: coordinate[0], y: coordinate[1] };
    let region_scaled_w = (base.scaled_w as f32 / zoom_factor).max(8.0) as i32;
    let region_scaled_h = (base.scaled_h as f32 / zoom_factor).max(8.0) as i32;
    let scaled_x = (scaled_center.x - region_scaled_w / 2).max(0);
    let scaled_y = (scaled_center.y - region_scaled_h / 2).max(0);
    let native_x = (scaled_x as f64 * base.ratio).round() as i32 + base.monitor_origin.x;
    let native_y = (scaled_y as f64 * base.ratio).round() as i32 + base.monitor_origin.y;
    let native_w = (region_scaled_w as f64 * base.ratio).round() as i32;
    let native_h = (region_scaled_h as f64 * base.ratio).round() as i32;
    let region = Rect { x: native_x, y: native_y, w: native_w, h: native_h };
    // Capture and upscale to target_max.
    let frame = crate::capture::capture_into_staging(0, Some(region))?;
    let new_ratio = compute_ratio(frame.w, frame.h, target_max);
    let (sw, sh) = scaled_dims(frame.w, frame.h, new_ratio);
    let resized = resize_bgra_to_rgba(&frame, sw, sh);
    let img = crate::capture::encode::encode_jpeg(&resized, sw, sh)?;
    let snap = ScaleSnapshot {
        ratio: new_ratio,
        monitor_origin: Point { x: native_x, y: native_y },
        native_w: frame.w, native_h: frame.h,
        scaled_w: sw, scaled_h: sh,
        captured_at: Instant::now(),
    };
    Ok((img, snap))
}

fn resize_bgra_to_rgba(frame: &crate::capture::FrameBuf, sw: u32, sh: u32) -> Vec<u8> {
    use image::imageops::FilterType;
    use image::{ImageBuffer, Rgba};
    let mut rgba_native = Vec::with_capacity((frame.w * frame.h * 4) as usize);
    for chunk in frame.bgra.chunks_exact(4) {
        rgba_native.extend_from_slice(&[chunk[2], chunk[1], chunk[0], chunk[3]]);
    }
    let buf: ImageBuffer<Rgba<u8>, _> =
        ImageBuffer::from_raw(frame.w, frame.h, rgba_native).expect("buffer size matches");
    let resized = image::imageops::resize(&buf, sw, sh, FilterType::Triangle);
    resized.into_raw()
}
```

(`encode_jpeg` may already exist as `encode_jpeg(rgba, w, h)`; if signature differs, adapt or add a thin wrapper. Add `EncodedImage::format: ImageFormat` if not already there.)

- [ ] **Step 3: Build**

Run: `cargo build --release --workspace`
Expected: PASS.

- [ ] **Step 4: Verify lints**

Run: `cargo run -p xtask -- lints`
Expected: PASS — D-26 still holds (capture work is on capture thread; resize runs there too which is fine since `image` is pure Rust).

- [ ] **Step 5: Smoke test the screenshot path**

Quick manual end-to-end:
```
cargo build --release -p fastuse-cli -p fastuse-daemon
taskkill /F /IM fastuse-daemon.exe 2>$null
.\target\release\fastuse-cli.exe ping
# (after adding the CLI subcommand in Task 13, this becomes the real check)
```
For now confirm `ping` works and the daemon log shows no startup errors.

- [ ] **Step 6: Commit**

```bash
git add crates/fastuse-daemon/src/dispatch.rs crates/fastuse-win/src/capture/screenshot.rs crates/fastuse-win/src/input_thread.rs
git commit -F <(printf '%s\n' \
  "feat(daemon): computer action dispatch handlers" \
  "" \
  "Implements all ComputerAction variants. Screenshot + Zoom emit" \
  "ImagePayload (base64 JPEG) + ScaleInfo and update the per-session" \
  "ScaleStack. Click/Type/Key/Scroll translate scaled coords -> native via" \
  "ScaleStack.translate, dispatch through InputBackend on the input STA" \
  "thread. CursorPosition reads cursor without input-thread round-trip.")
```

### Task 11: Daemon dispatch — Windows helpers and UIA inspection handlers

**Files:**
- Modify: `crates/fastuse-daemon/src/dispatch.rs`
- Modify: `crates/fastuse-win/src/window/` (add `wait_for_window` if missing)

- [ ] **Step 1: Add the missing `wait_for_window` helper**

In `crates/fastuse-win/src/window/mod.rs` or appropriate sibling:

```rust
use std::time::{Duration, Instant};
use fastuse_proto::wire::{ListWindowsRequest, WaitForWindowRequest};

pub fn wait_for_window(req: &WaitForWindowRequest) -> Option<crate::window::WindowInfo> {
    let deadline = Instant::now() + Duration::from_millis(req.timeout_ms as u64);
    loop {
        let list = list_windows(&ListWindowsRequest {
            title_substr: req.title_substr.clone(),
            process_name: req.process_name.clone(),
        });
        if let Some(first) = list.into_iter().next() {
            return Some(first);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
```

- [ ] **Step 2: Add Windows helper dispatch arms**

In `dispatch.rs`, add arms for:
- `Request::ListWindowsV2(req)` → `Response::ListWindows(fastuse_win::window::list_windows(&req))` (gate via `permissions.is_allowed("list_windows")`)
- `Request::FocusWindow(FocusWindowRequest { hwnd })` → `Response::Ok` if `fastuse_win::window::focus(hwnd)` succeeds, else `Error::Internal`
- `Request::ForegroundWindow` → `Response::Window(fastuse_win::window::foreground())` or `Error::WindowNotFound`
- `Request::WaitForWindow(req)` → `Response::Window(...)` if `wait_for_window` returns Some, else `Error::WindowNotFound` with hint about timeout
- `Request::ListProcesses(req)` → `Response::Processes(fastuse_win::process::list_processes(&req))` (no gate)
- `Request::KillProcess(req)` → gate via `permissions.is_allowed("kill_process")`; if gated return `Error::PermissionRequired` with hint; else `fastuse_win::process::kill(&req)`
- `Request::LaunchApp(req)` → gate via `permissions.is_allowed("launch_app")`; else `fastuse_win::launch::launch(&req.target)`
- `Request::ShellExec(req)` → gate via `permissions.is_allowed("shell_exec")`; else `fastuse_win::shell::shell_exec(&req).await`
- `Request::ClipboardGetText` → `Response::Text(fastuse_win::clipboard::get_text())`
- `Request::ClipboardSetText(req)` → gate via `permissions.is_allowed("clipboard_set_text")`; else `fastuse_win::clipboard::set_text(&req.text)`
- `Request::InspectAt(InspectAtRequest { x, y })` → `Response::UiaElement(fastuse_win::uia::inspect_at(uia, x, y))`
- `Request::UiaQuery(req)` → `Response::UiaElements(fastuse_win::uia::query(uia, &req))`
- `Request::UiaTree(req)` → `Response::UiaTree(fastuse_win::uia::tree(uia, req.hwnd, req.max_depth))`

The permission gate looks like:
```rust
if !ctx.permissions.is_allowed("kill_process") {
    return Response::Error(
        Error::new(ErrorCode::PermissionRequired,
                   "tool 'kill_process' is gated in safe mode")
            .with_hint("set FASTUSE_SAFE_MODE=0 or remove from gated_tools in config.toml"),
    );
}
```

- [ ] **Step 3: Build**

Run: `cargo build --release --workspace`
Expected: PASS. Any missing helper functions in `window/`, `process/`, `clipboard/`, `launch/`, `shell/`, `uia/` get added (most exist already from v1; adapt signatures).

- [ ] **Step 4: Lints**

Run: `cargo run -p xtask -- lints`
Expected: PASS.

- [ ] **Step 5: Tests**

Run: `cargo test --release --workspace --lib --bins`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/fastuse-daemon/src/dispatch.rs crates/fastuse-win/src/window/ crates/fastuse-win/src/uia/
git commit -F <(printf '%s\n' \
  "feat(daemon): Windows helper + UIA inspection dispatch" \
  "" \
  "list_windows, focus_window, foreground_window, wait_for_window," \
  "list_processes, kill_process (gated), launch_app (gated)," \
  "shell_exec (gated), clipboard_get_text, clipboard_set_text (gated)," \
  "inspect_at (read-only), uia_query (read-only), uia_tree (read-only)." \
  "" \
  "Permission gate returns PERMISSION_REQUIRED with actionable hint when" \
  "safe_mode is on and the tool is in the gated set.")
```

### Task 12: CLI rewrite — `computer` subcommands

**Files:**
- Modify: `crates/fastuse-cli/src/main.rs`
- Create: `crates/fastuse-cli/src/cmd_computer.rs` (subcommand handlers)
- Modify: `crates/fastuse-cli/src/lib.rs` if exists

- [ ] **Step 1: Add the `Computer` subcommand tree**

In `crates/fastuse-cli/src/main.rs`, add to the clap derive `Cli` enum (or whatever the top-level `Subcommand` is named):

```rust
#[derive(clap::Subcommand)]
enum Cmd {
    // ... existing subcommands (Ping, Status, Stop, Warmup, ListWindows, etc.)
    Computer(ComputerArgs),
    // ... new top-level subcommands added in next task
}

#[derive(clap::Args)]
struct ComputerArgs {
    #[command(subcommand)]
    action: ComputerAction,
}

#[derive(clap::Subcommand)]
enum ComputerAction {
    Screenshot {
        #[arg(long)]
        out: Option<std::path::PathBuf>,
        #[arg(long)]
        monitor: Option<u32>,
        #[arg(long, default_value = "jpeg")]
        format: String,
    },
    LeftClick { x: i32, y: i32, #[arg(long)] modifiers: Option<String>, #[arg(long)] instant: bool },
    RightClick { x: i32, y: i32, #[arg(long)] instant: bool },
    MiddleClick { x: i32, y: i32, #[arg(long)] instant: bool },
    DoubleClick { x: i32, y: i32, #[arg(long)] instant: bool },
    TripleClick { x: i32, y: i32, #[arg(long)] instant: bool },
    Drag { sx: i32, sy: i32, ex: i32, ey: i32, #[arg(long)] modifiers: Option<String>, #[arg(long)] instant: bool },
    Type { text: String, #[arg(long)] instant: bool },
    Key { chord: String },
    HoldKey { chord: String, #[arg(long)] ms: u32 },
    Scroll { x: i32, y: i32, #[arg(long)] direction: String, #[arg(long)] amount: i32 },
    MouseMove { x: i32, y: i32, #[arg(long)] instant: bool },
    CursorPosition,
    Wait { ms: u32 },
    Zoom { x: i32, y: i32, #[arg(long, default_value = "2.0")] factor: f32 },
    LeftMouseDown { #[arg(long)] x: Option<i32>, #[arg(long)] y: Option<i32> },
    LeftMouseUp { #[arg(long)] x: Option<i32>, #[arg(long)] y: Option<i32> },
}
```

**Coordinate convention for CLI:** native virtual-desktop pixels (no scaling). Different from MCP. The handler for `LeftClick` etc. wraps the native coords in a no-op scale snapshot so the daemon's translate path works:

```rust
// In cmd_computer.rs handler:
async fn handle_computer(args: ComputerArgs) -> anyhow::Result<()> {
    use fastuse_proto::wire::{ComputerAction, ComputerRequest, Request, Response, ScrollDir};
    let req = match args.action {
        ComputerAction::Screenshot { out, monitor, format } => {
            // Send Screenshot request
            // ... receive Response::Computer(ComputerResult) ...
            // If `out` provided: decode base64, write bytes to file.
            // Print JSON minus the image bytes.
            Request::Computer(ComputerRequest {
                action: fastuse_proto::wire::ComputerAction::Screenshot { monitor },
            })
        }
        ComputerAction::LeftClick { x, y, modifiers, instant } => {
            // For CLI we send native coords. Daemon needs them as-is.
            // Strategy: bypass the scaling translate by setting a no-op
            // ScaleStack snapshot via a parallel CLI path, OR have the daemon
            // accept a `scaled: false` flag.
            //
            // Simpler: add an internal Request::ComputerNative variant that
            // skips translation. Use that here.
            Request::ComputerNative(/* native coords + ComputerAction-like payload */)
        }
        // ...
    };
    let resp = client::send(req).await?;
    print_response(&resp, /* out_path */);
    Ok(())
}
```

To avoid two near-identical wire types: add a single `coordinates_native: bool` field on `ComputerRequest`:

```rust
pub struct ComputerRequest {
    pub action: ComputerAction,
    /// True = coordinates are native virtual-desktop pixels; daemon skips
    /// scale translation. CLI sets this; MCP leaves it false.
    #[serde(default)]
    pub coordinates_native: bool,
}
```

Update the dispatch in Task 10 to honor `coordinates_native`: when true, build native `Point` directly from `[x, y]` rather than going through `ScaleStack::translate`.

- [ ] **Step 2: Implement the handler module**

Create `crates/fastuse-cli/src/cmd_computer.rs` with the full match implementing each `ComputerAction` variant. For `Screenshot { out: Some(path), .. }`: receive response, decode `data_base64` (base64 crate), write raw bytes to `path`, print scale info JSON to stdout. For `Screenshot { out: None, .. }`: print full JSON (caller redirects).

For `Drag { sx, sy, ex, ey, modifiers, instant }`: build `ComputerAction::LeftClickDrag { start_coordinate: [sx, sy], coordinate: [ex, ey], humanize: !instant, text: modifiers }`.

`scroll --direction up|down|left|right` mapped to `ScrollDir`. Bad direction string → exit code 2 with clear error.

- [ ] **Step 3: Build**

Run: `cargo build --release -p fastuse-cli`
Expected: PASS.

- [ ] **Step 4: Smoke test against a fresh daemon**

```
taskkill /F /IM fastuse-daemon.exe 2>$null
.\target\release\fastuse-cli.exe ping
.\target\release\fastuse-cli.exe computer screenshot --out screenshot.jpg
# Verify screenshot.jpg is a valid image, dimensions ~1024 wide.
.\target\release\fastuse-cli.exe computer left-click 100 200 --instant
# Cursor should jump to (100, 200) (native coords, no humanization).
.\target\release\fastuse-cli.exe computer type "hello" --instant
# Should type 'hello' wherever focus is.
```

- [ ] **Step 5: Commit**

```bash
git add crates/fastuse-cli/src/main.rs crates/fastuse-cli/src/cmd_computer.rs crates/fastuse-proto/src/wire.rs
git commit -F <(printf '%s\n' \
  "feat(cli): computer subcommands matching computer_20251124 actions" \
  "" \
  "fastuse-cli computer {screenshot,left-click,right-click,middle-click," \
  "double-click,triple-click,drag,type,key,hold-key,scroll,mouse-move," \
  "cursor-position,wait,zoom,left-mouse-down,left-mouse-up}." \
  "" \
  "CLI uses native virtual-desktop pixel coordinates (no scaling). Wire" \
  "ComputerRequest gains coordinates_native: bool so the daemon skips the" \
  "scale-translate step for CLI requests. --instant on motion/typing actions" \
  "opts out of humanization.")
```

### Task 13: CLI rewrite — Windows helpers, ops, setup-mcp

**Files:**
- Modify: `crates/fastuse-cli/src/main.rs`
- Create: `crates/fastuse-cli/src/cmd_setup_mcp.rs`

- [ ] **Step 1: Add or adapt these subcommands in the clap derive**

```rust
enum Cmd {
    // ... Computer(ComputerArgs) from Task 12
    Ping,
    Status,
    Stop,
    Warmup,
    ListWindows {
        #[arg(long)] title: Option<String>,
        #[arg(long)] process: Option<String>,
    },
    FocusWindow { hwnd: u64 },
    ForegroundWindow,
    WaitForWindow {
        #[arg(long)] title: Option<String>,
        #[arg(long)] process: Option<String>,
        #[arg(long, default_value = "5000")] timeout_ms: u32,
    },
    ListProcesses {
        #[arg(long)] name: Option<String>,
        #[arg(long)] visible_only: bool,
    },
    KillProcess { spec: String }, // "pid:1234" or "name:notepad"
    LaunchApp { target: String },
    ShellExec { command: String, #[arg(long)] shell: Option<String> },
    ClipboardGetText,
    ClipboardSetText { text: String },
    InspectAt { x: i32, y: i32 },
    UiaQuery {
        selector: String, // raw JSON
        #[arg(long)] root_hwnd: Option<u64>,
        #[arg(long)] max_results: Option<u32>,
    },
    UiaTree { #[arg(long)] hwnd: Option<u64>, #[arg(long)] max_depth: Option<u32> },
    SetupMcp(SetupMcpArgs),
}

#[derive(clap::Args)]
struct SetupMcpArgs {
    /// Write to user-level (~/.claude/settings.json) instead of project-level
    #[arg(long)] user: bool,
    /// Write to project-level (.claude/settings.json in cwd)
    #[arg(long)] project: bool,
}
```

- [ ] **Step 2: Implement `setup-mcp` in `cmd_setup_mcp.rs`**

```rust
//! `fastuse-cli setup-mcp [--user | --project]` — register fastuse-mcp.exe in
//! Claude Code's settings.json idempotently. Reads existing JSON, adds
//! mcp.servers.fastuse, writes back. Prints next-step instruction.

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

pub fn run(user: bool, project: bool) -> Result<()> {
    let target_path = if user || (!user && !project) {
        let home = std::env::var("USERPROFILE").context("USERPROFILE not set")?;
        PathBuf::from(home).join(".claude").join("settings.json")
    } else {
        std::env::current_dir()?.join(".claude").join("settings.json")
    };
    let mcp_exe = locate_mcp_exe()?;

    let mut root: Value = if target_path.exists() {
        let s = std::fs::read_to_string(&target_path)
            .with_context(|| format!("read {target_path:?}"))?;
        if s.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&s)
                .with_context(|| format!("parse {target_path:?}"))?
        }
    } else {
        std::fs::create_dir_all(target_path.parent().unwrap())?;
        json!({})
    };

    let root_obj = root.as_object_mut().ok_or_else(|| anyhow!("settings root is not an object"))?;
    let mcp = root_obj.entry("mcp").or_insert_with(|| json!({}));
    let mcp_obj = mcp.as_object_mut().ok_or_else(|| anyhow!("mcp is not an object"))?;
    let servers = mcp_obj.entry("servers").or_insert_with(|| json!({}));
    let servers_obj = servers.as_object_mut().ok_or_else(|| anyhow!("mcp.servers is not an object"))?;

    let entry = json!({
        "command": mcp_exe.to_string_lossy(),
        "args": [],
        "env": {}
    });
    let updated = match servers_obj.get("fastuse") {
        Some(existing) if existing == &entry => false,
        _ => {
            servers_obj.insert("fastuse".to_string(), entry);
            true
        }
    };

    if updated {
        let pretty = serde_json::to_string_pretty(&root)?;
        std::fs::write(&target_path, pretty)?;
        println!("Wrote {target_path:?}. Restart Claude Code to pick up the change.");
    } else {
        println!("fastuse already registered in {target_path:?} (no change).");
    }
    Ok(())
}

fn locate_mcp_exe() -> Result<PathBuf> {
    let cli = std::env::current_exe()?;
    let dir = cli.parent().ok_or_else(|| anyhow!("current_exe has no parent"))?;
    let candidate = dir.join("fastuse-mcp.exe");
    if candidate.exists() {
        Ok(candidate)
    } else {
        Err(anyhow!("fastuse-mcp.exe not found next to fastuse-cli.exe; \
            expected at {candidate:?}"))
    }
}
```

- [ ] **Step 3: Wire each subcommand to its dispatcher**

Each variant: build `Request::*`, send via the existing CLI client, format `Response::*` as JSON to stdout (matching v1's CLI output style — single-line JSON).

For `KillProcess { spec }`: parse `"pid:1234"` → `KillProcessRequest::Pid { pid: 1234 }`; `"name:notepad.exe"` → `KillProcessRequest::Name { stem: ... }`. Bad input → exit code 2.

For `UiaQuery { selector, ... }`: `serde_json::from_str(&selector)` → `Selector`. Parse error → exit 2 with hint.

- [ ] **Step 4: Build**

Run: `cargo build --release -p fastuse-cli`
Expected: PASS.

- [ ] **Step 5: Smoke test**

```
.\target\release\fastuse-cli.exe ping
.\target\release\fastuse-cli.exe list-windows
.\target\release\fastuse-cli.exe foreground-window
.\target\release\fastuse-cli.exe inspect-at 500 300
.\target\release\fastuse-cli.exe setup-mcp --user
# Verify the user's ~/.claude/settings.json now has mcp.servers.fastuse
```

- [ ] **Step 6: Commit**

```bash
git add crates/fastuse-cli/src/main.rs crates/fastuse-cli/src/cmd_setup_mcp.rs
git commit -F <(printf '%s\n' \
  "feat(cli): Windows helpers, ops, setup-mcp subcommands" \
  "" \
  "list-windows, focus-window, foreground-window, wait-for-window," \
  "list-processes, kill-process, launch-app, shell-exec, clipboard-get-text," \
  "clipboard-set-text, inspect-at, uia-query, uia-tree, ping, status, stop," \
  "warmup, setup-mcp." \
  "" \
  "setup-mcp --user writes ~/.claude/settings.json idempotently; --project" \
  "writes .claude/settings.json in cwd. Resolves fastuse-mcp.exe alongside" \
  "fastuse-cli.exe.")
```

### Task 14: MCP server scaffolding — connect to daemon, register tools

**Files:**
- Modify: `crates/fastuse-mcp/Cargo.toml` (add `rmcp = "1.6"`, `serde_json`, `tokio`, `base64`, dependencies on `fastuse-proto` and a thin pipe client)
- Modify: `crates/fastuse-mcp/src/main.rs` (or `lib.rs` + `bin/server.rs` — match existing layout)
- Create: `crates/fastuse-mcp/src/server.rs`
- Create: `crates/fastuse-mcp/src/client.rs` (named-pipe client to the daemon)
- Create: `crates/fastuse-mcp/src/tools/mod.rs`

- [ ] **Step 1: Audit the existing fastuse-mcp crate**

```
cat crates/fastuse-mcp/src/main.rs
cat crates/fastuse-mcp/Cargo.toml
```

If the v1 shim already wires rmcp + a daemon client, keep that scaffolding; you're swapping out the tool list. If v1 was a near-empty placeholder, build from scratch using the rmcp 1.6 stdio server pattern.

- [ ] **Step 2: Create the named-pipe client to the daemon**

`crates/fastuse-mcp/src/client.rs`:

```rust
//! Thin client: spawn-or-connect to fastuse-daemon over named pipe, send
//! Request, await Response. Reuses fastuse-cli's spawn logic (ShellExecuteExW
//! runas) — extracted to a shared crate or duplicated minimally.

use anyhow::{anyhow, Context, Result};
use fastuse_proto::wire::{Request, Response};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};

pub struct DaemonClient {
    pipe: tokio::sync::Mutex<NamedPipeClient>,
}

impl DaemonClient {
    pub async fn connect_or_spawn() -> Result<Self> {
        // Reuse fastuse-cli's spawn::connect_or_spawn. Either move it to a
        // shared `fastuse-ipc` crate or vendor a copy here.
        let pipe = fastuse_cli::spawn::connect_or_spawn(&pipe_path()).await?;
        Ok(Self { pipe: tokio::sync::Mutex::new(pipe) })
    }

    pub async fn send(&self, req: Request) -> Result<Response> {
        let mut buf = serde_json::to_vec(&req)?;
        buf.push(b'\n');
        let mut pipe = self.pipe.lock().await;
        pipe.write_all(&buf).await?;
        pipe.flush().await?;
        let mut response = Vec::new();
        let mut line = [0u8; 65536];
        loop {
            let n = pipe.read(&mut line).await?;
            if n == 0 { return Err(anyhow!("pipe closed")); }
            response.extend_from_slice(&line[..n]);
            if response.last() == Some(&b'\n') { break; }
        }
        let resp: Response = serde_json::from_slice(&response[..response.len()-1])?;
        Ok(resp)
    }
}

fn pipe_path() -> String {
    let session = unsafe { windows::Win32::System::Threading::GetCurrentProcessId() };
    let _ = session; // session id resolved via WTSGetActiveConsoleSessionId; for now use 1
    format!(r"\\.\pipe\fastuse-1-1001") // D-21 invariant; match daemon's path
}
```

This duplicates a slice of `fastuse-cli::spawn`. Optionally extract `spawn` and `pipe_path` into a new crate `fastuse-ipc` and depend on that from both `fastuse-cli` and `fastuse-mcp`. Add the new crate to the workspace if so.

- [ ] **Step 3: Tool module skeleton**

`crates/fastuse-mcp/src/tools/mod.rs`:

```rust
//! Each MCP tool is a thin shim: parse args -> build Request -> daemon.send
//! -> format Response into MCP tool result. Image responses become
//! ImageContent inline.

pub mod computer;
pub mod windows;
pub mod inspection;
pub mod meta;
```

Empty stubs for each (`pub fn register(server: &mut Server) {}`) so the crate compiles. Real registration in next tasks.

- [ ] **Step 4: Server bootstrap**

`crates/fastuse-mcp/src/server.rs`:

```rust
use anyhow::Result;
use rmcp::{Server, transport::StdioTransport};

pub async fn run() -> Result<()> {
    let client = std::sync::Arc::new(crate::client::DaemonClient::connect_or_spawn().await?);
    let mut server = Server::new(/* server info: name "fastuse-mcp", version "2.0.0" */);

    crate::tools::computer::register(&mut server, client.clone());
    crate::tools::windows::register(&mut server, client.clone());
    crate::tools::inspection::register(&mut server, client.clone());
    crate::tools::meta::register(&mut server, client.clone());

    let transport = StdioTransport::new();
    server.serve(transport).await?;
    Ok(())
}
```

(Exact rmcp 1.6 API may differ — adapt to whatever `Server`/`StdioTransport`/`tool` macros the version exposes. Read `https://docs.rs/rmcp/latest/rmcp/` if signatures don't match.)

`main.rs`:
```rust
fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "info".into()))
        .json()
        .init();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(fastuse_mcp::server::run())
}
```

- [ ] **Step 5: Build**

Run: `cargo build --release -p fastuse-mcp`
Expected: PASS. Stub tool modules ⇒ server registers nothing yet, but starts, accepts MCP handshake, advertises empty tool list.

- [ ] **Step 6: Smoke test (no real Claude Code yet)**

Optional sanity: pipe an MCP `initialize` request via stdin, verify response shape. rmcp may have a test harness. Skip if it costs > 10 minutes.

- [ ] **Step 7: Commit**

```bash
git add crates/fastuse-mcp/ crates/fastuse-cli/src/spawn.rs Cargo.toml
git commit -F <(printf '%s\n' \
  "feat(mcp): server scaffolding + daemon client" \
  "" \
  "DaemonClient connects-or-spawns the daemon over named pipe (D-21" \
  "compliant). Server boots rmcp stdio transport with empty tool registry —" \
  "concrete tools (computer, Windows helpers, UIA inspection, meta) wired" \
  "in subsequent commits.")
```

### Task 15: MCP `computer` tool with ImageContent return

**Files:**
- Modify: `crates/fastuse-mcp/src/tools/computer.rs`

- [ ] **Step 1: Implement the `computer` MCP tool**

```rust
//! The single `computer` MCP tool exposing computer_20251124's action enum.
//! Returns ImageContent inline for screenshot/zoom; text content for others.

use crate::client::DaemonClient;
use anyhow::{anyhow, Result};
use base64::Engine;
use fastuse_proto::wire::{ComputerAction, ComputerRequest, Request, Response, ScrollDir};
use rmcp::{
    server::Server,
    schema::{CallToolResult, Content, ImageContent, TextContent, Tool, ToolInputSchema},
};
use serde_json::{json, Value};
use std::sync::Arc;

pub fn register(server: &mut Server, client: Arc<DaemonClient>) {
    let schema = ToolInputSchema::from_value(json!({
        "type": "object",
        "properties": {
            "action": {
                "type": "string",
                "enum": [
                    "screenshot", "left_click", "right_click", "middle_click",
                    "double_click", "triple_click", "left_click_drag",
                    "left_mouse_down", "left_mouse_up", "mouse_move",
                    "cursor_position", "type", "key", "hold_key", "scroll",
                    "wait", "zoom"
                ],
            },
            "coordinate": { "type": "array", "items": { "type": "integer" }, "minItems": 2, "maxItems": 2 },
            "start_coordinate": { "type": "array", "items": { "type": "integer" }, "minItems": 2, "maxItems": 2 },
            "text": { "type": "string" },
            "duration": { "type": "integer", "minimum": 0 },
            "scroll_direction": { "type": "string", "enum": ["up","down","left","right"] },
            "scroll_amount": { "type": "integer" },
            "humanize": { "type": "boolean" },
            "monitor": { "type": "integer", "minimum": 0 },
            "zoom_factor": { "type": "number", "minimum": 1.0 },
        },
        "required": ["action"]
    }));

    let tool = Tool {
        name: "computer".into(),
        description: "Drive the Windows desktop. Vision-first: take screenshots, then click coordinates returned by analyzing the image. Matches Anthropic computer_20251124 schema.".into(),
        input_schema: schema,
    };

    server.register_tool(tool, move |args: Value| {
        let client = client.clone();
        async move {
            let action: ComputerAction = parse_action(args)?;
            let resp = client.send(Request::Computer(ComputerRequest {
                action: action.clone(),
                coordinates_native: false,
            })).await?;
            match resp {
                Response::Computer(result) => Ok(format_result(&action, result)),
                Response::Error(e) => Err(anyhow!("daemon error [{}]: {}", e.code, e.message)),
                other => Err(anyhow!("unexpected response: {other:?}")),
            }
        }
    });
}

fn parse_action(args: Value) -> Result<ComputerAction> {
    serde_json::from_value(args).map_err(|e| anyhow!("invalid computer action: {e}"))
}

fn format_result(action: &ComputerAction, result: fastuse_proto::wire::ComputerResult) -> CallToolResult {
    let mut contents: Vec<Content> = Vec::new();
    if let Some(img) = result.image {
        let mime = match img.format.as_str() {
            "jpeg" | "jpg" => "image/jpeg",
            "png" => "image/png",
            _ => "application/octet-stream",
        };
        contents.push(Content::Image(ImageContent {
            data: img.data_base64.clone(),
            mime_type: mime.into(),
        }));
    }
    let scale_json = result.scale.map(|s| json!({
        "ratio": s.ratio,
        "monitor_origin": s.monitor_origin,
        "native": [s.native_w, s.native_h],
        "scaled": [s.scaled_w, s.scaled_h],
    }));
    let body = json!({
        "ok": result.ok,
        "action": match action {
            ComputerAction::Screenshot { .. } => "screenshot",
            ComputerAction::LeftClick { .. } => "left_click",
            ComputerAction::RightClick { .. } => "right_click",
            ComputerAction::MiddleClick { .. } => "middle_click",
            ComputerAction::DoubleClick { .. } => "double_click",
            ComputerAction::TripleClick { .. } => "triple_click",
            ComputerAction::LeftClickDrag { .. } => "left_click_drag",
            ComputerAction::LeftMouseDown { .. } => "left_mouse_down",
            ComputerAction::LeftMouseUp { .. } => "left_mouse_up",
            ComputerAction::MouseMove { .. } => "mouse_move",
            ComputerAction::CursorPosition => "cursor_position",
            ComputerAction::Type { .. } => "type",
            ComputerAction::Key { .. } => "key",
            ComputerAction::HoldKey { .. } => "hold_key",
            ComputerAction::Scroll { .. } => "scroll",
            ComputerAction::Wait { .. } => "wait",
            ComputerAction::Zoom { .. } => "zoom",
        },
        "cursor": result.cursor,
        "scale": scale_json,
    });
    contents.push(Content::Text(TextContent { text: serde_json::to_string(&body).unwrap() }));
    CallToolResult { content: contents, is_error: !result.ok }
}
```

(Adapt rmcp type names to match the actual 1.6 API — `Server::register_tool`, `Tool`, `Content::Image`, etc. may differ slightly.)

- [ ] **Step 2: Build**

Run: `cargo build --release -p fastuse-mcp`
Expected: PASS.

- [ ] **Step 3: End-to-end smoke test with Claude Code**

In an elevated PowerShell:
```
.\target\release\fastuse-cli.exe setup-mcp --user
# Restart Claude Code, then in a new conversation:
# "Take a screenshot of my screen using the fastuse computer tool"
# Claude should call mcp__fastuse__computer({action:"screenshot"}) and see the image inline.
```

- [ ] **Step 4: Commit**

```bash
git add crates/fastuse-mcp/src/tools/computer.rs
git commit -F <(printf '%s\n' \
  "feat(mcp): computer tool with ImageContent inline return" \
  "" \
  "Single MCP tool 'computer' exposes Anthropic computer_20251124 action enum." \
  "Screenshot and Zoom return base64 JPEG as MCP ImageContent — Claude sees" \
  "the image in the same turn (no Read step). Other actions return text" \
  "content with action name + scale info + cursor.")
```

### Task 16: MCP Windows helpers, UIA inspection, meta tools

**Files:**
- Modify: `crates/fastuse-mcp/src/tools/windows.rs`
- Modify: `crates/fastuse-mcp/src/tools/inspection.rs`
- Modify: `crates/fastuse-mcp/src/tools/meta.rs`

- [ ] **Step 1: Implement each tool's `register` function**

Each tool follows the same pattern: declare schema, build `Tool`, register handler that builds `Request::*`, sends, formats response as text content. ImageContent only for `computer` (none of these return images).

`tools/windows.rs` registers: `list_windows`, `focus_window`, `foreground_window`, `wait_for_window`, `list_processes`, `kill_process`, `launch_app`, `shell_exec`, `clipboard_get_text`, `clipboard_set_text`.

`tools/inspection.rs` registers: `inspect_at`, `uia_query`, `uia_tree`.

`tools/meta.rs` registers: `ping`, `status`, `stop`, `warmup`.

Example (`list_windows`):

```rust
let schema = ToolInputSchema::from_value(json!({
    "type": "object",
    "properties": {
        "title_substr": { "type": "string" },
        "process_name": { "type": "string" }
    }
}));
let tool = Tool {
    name: "list_windows".into(),
    description: "List visible top-level windows. Filter by title substring or process name.".into(),
    input_schema: schema,
};
server.register_tool(tool, move |args| {
    let client = client.clone();
    async move {
        let req: ListWindowsRequest = serde_json::from_value(args)?;
        let resp = client.send(Request::ListWindowsV2(req)).await?;
        match resp {
            Response::ListWindows(list) => Ok(json_text(&list)),
            Response::Error(e) => Err(anyhow!("[{}] {}", e.code, e.message)),
            _ => Err(anyhow!("unexpected response")),
        }
    }
});
```

`fn json_text<T: serde::Serialize>(v: &T) -> CallToolResult` returns a `CallToolResult` with one `TextContent` containing pretty JSON.

- [ ] **Step 2: Build**

Run: `cargo build --release -p fastuse-mcp`
Expected: PASS.

- [ ] **Step 3: Verify with Claude Code**

```
# In Claude Code conversation after setup-mcp + restart:
# Ask Claude: "List all top-level windows whose titles contain 'L-Connect'"
# Claude should call mcp__fastuse__list_windows({title_substr: "L-Connect"})
# and return the JSON.
```

- [ ] **Step 4: Run lints + tests**

```
cargo run -p xtask -- lints
cargo test --release --workspace --lib --bins
```

- [ ] **Step 5: Commit**

```bash
git add crates/fastuse-mcp/src/tools/
git commit -F <(printf '%s\n' \
  "feat(mcp): Windows helpers, UIA inspection, meta tools" \
  "" \
  "Registers list_windows, focus_window, foreground_window, wait_for_window," \
  "list_processes, kill_process, launch_app, shell_exec, clipboard_get_text," \
  "clipboard_set_text, inspect_at, uia_query, uia_tree, ping, status, stop," \
  "warmup as separate MCP tools. UIA tools are read-only (DevTools-style" \
  "structure inspection); they return data, never click.")
```

### Task 17: Permission gating in daemon dispatch

**Files:**
- Modify: `crates/fastuse-daemon/src/main.rs` (build `Permissions` at startup)
- Modify: `crates/fastuse-daemon/src/dispatch.rs` (gate destructive tools)
- Modify: `crates/fastuse-daemon/src/server.rs` if dispatch context is constructed there

- [ ] **Step 1: Load config + build Permissions in `main.rs`**

In `crates/fastuse-daemon/src/main.rs` after tracing init and before server start:

```rust
let cfg_path = fastuse_core::config::default_path();
let config = match cfg_path.as_ref() {
    Some(p) => fastuse_core::config::load_or_default(p),
    None => fastuse_core::config::Config::default(),
};
let permissions = fastuse_win::permissions::Permissions::from_env_and_config(
    config.permissions.safe_mode,
    config.permissions.gated_tools.clone(),
);
tracing::info!(safe_mode = permissions.safe_mode,
               gated_count = permissions.gated.len(),
               "permissions resolved");
```

Pass `permissions` (and `config`) into the dispatch context (`Ctx`/session-state struct).

- [ ] **Step 2: Add gate-check helper**

In `dispatch.rs`:

```rust
fn gate(ctx: &Ctx, tool: &str) -> Result<(), Response> {
    if ctx.permissions.is_allowed(tool) {
        Ok(())
    } else {
        Err(Response::Error(
            fastuse_proto::error::Error::new(
                fastuse_proto::error::ErrorCode::PermissionRequired,
                format!("tool '{tool}' is gated in safe mode"),
            )
            .with_hint("set FASTUSE_SAFE_MODE=0 to disable, or remove from gated_tools in config.toml"),
        ))
    }
}
```

In each gated handler arm:
```rust
Request::KillProcess(req) => {
    if let Err(resp) = gate(&ctx, "kill_process") { return resp; }
    // ... existing dispatch
}
Request::LaunchApp(req) => {
    if let Err(resp) = gate(&ctx, "launch_app") { return resp; }
    // ...
}
Request::ShellExec(req) => {
    if let Err(resp) = gate(&ctx, "shell_exec") { return resp; }
    // ...
}
Request::ClipboardSetText(req) => {
    if let Err(resp) = gate(&ctx, "clipboard_set_text") { return resp; }
    // ...
}
```

`computer` action handlers stay ungated even in safe_mode (per spec § Permission model).

- [ ] **Step 3: Test the gate**

```
$env:FASTUSE_SAFE_MODE = "1"
taskkill /F /IM fastuse-daemon.exe 2>$null
.\target\release\fastuse-cli.exe ping
.\target\release\fastuse-cli.exe kill-process pid:12345
# Expected: exit code 1, JSON error with code PERMISSION_REQUIRED
.\target\release\fastuse-cli.exe computer screenshot --out test.jpg
# Expected: success — screenshot is not gated
$env:FASTUSE_SAFE_MODE = "0"
```

- [ ] **Step 4: Commit**

```bash
git add crates/fastuse-daemon/src/main.rs crates/fastuse-daemon/src/dispatch.rs crates/fastuse-daemon/src/server.rs
git commit -F <(printf '%s\n' \
  "feat(daemon): permission gating from config + env" \
  "" \
  "Daemon resolves Permissions on startup from config.toml + FASTUSE_SAFE_MODE." \
  "Gated tools (kill_process, launch_app, shell_exec, clipboard_set_text)" \
  "return PERMISSION_REQUIRED with hint when safe_mode active. Computer" \
  "actions, screenshots, window/process listings, UIA inspection always allowed.")
```

### Task 18: Eval crate scaffold + Tier 1 cooperative-app scenarios

**Files:**
- Create: `crates/fastuse-eval/Cargo.toml`
- Create: `crates/fastuse-eval/src/lib.rs`
- Create: `crates/fastuse-eval/src/scenario.rs`
- Create: `crates/fastuse-eval/src/bin/eval.rs`
- Create: `crates/fastuse-eval/scenarios/calculator.toml`
- Create: `crates/fastuse-eval/scenarios/notepad.toml`
- Modify: workspace `Cargo.toml` (add `crates/fastuse-eval` to `members`)

- [ ] **Step 1: New crate scaffold**

`crates/fastuse-eval/Cargo.toml`:
```toml
[package]
name = "fastuse-eval"
version = "0.1.0"
edition = "2021"
publish = false

[dependencies]
anyhow = { workspace = true }
serde = { workspace = true, features = ["derive"] }
toml = "0.8"
tokio = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }

[[bin]]
name = "eval"
path = "src/bin/eval.rs"
```

`crates/fastuse-eval/src/lib.rs`:
```rust
//! Scenario-driven evaluation suite for fastuse v2. Drives a Claude Code
//! conversation against a fixed task, captures the transcript, runs a
//! deterministic success check, records pass/fail.

pub mod scenario;
```

- [ ] **Step 2: Scenario format**

`crates/fastuse-eval/src/scenario.rs`:
```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scenario {
    pub name: String,
    pub tier: u8, // 1, 2, or 3
    pub launch: LaunchSpec,
    pub task: String,        // natural-language prompt for Claude
    pub success: SuccessCheck,
    pub timeout_seconds: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchSpec {
    pub command: String,
    pub args: Vec<String>,
    pub wait_for_window_title_substr: Option<String>,
    pub wait_timeout_ms: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum SuccessCheck {
    /// Read the value of a UIA element and assert it matches.
    UiaValueEquals { selector: String, expected: String },
    /// Run an OCR pass against a region after the run; pass if needle found.
    /// (OCR-for-verification, NOT for targeting — we removed that.)
    ScreenshotContainsText { region_native: Option<[i32; 4]>, needle: String },
    /// A custom shell predicate that exits 0 on success.
    Shell { command: String },
}
```

- [ ] **Step 3: Calculator scenario**

`crates/fastuse-eval/scenarios/calculator.toml`:
```toml
name = "calculator-arithmetic"
tier = 1
task = """
Open the Windows Calculator if it's not already open. Then click the buttons
to compute 9 + 10 and press equals. Verify the result shows 19.
"""
timeout_seconds = 60

[launch]
command = "calc.exe"
args = []
wait_for_window_title_substr = "Calculator"
wait_timeout_ms = 5000

[success]
kind = "UiaValueEquals"
selector = '{"ByAutomationId": "CalculatorResults"}'
expected = "Display is 19"
```

- [ ] **Step 4: Notepad scenario**

`crates/fastuse-eval/scenarios/notepad.toml`:
```toml
name = "notepad-edit-save"
tier = 1
task = """
Open Notepad. Type "Hello fastuse v2 eval". Save the file as
%TEMP%\\fastuse-eval-notepad.txt. Close Notepad.
"""
timeout_seconds = 90

[launch]
command = "notepad.exe"
args = []
wait_for_window_title_substr = "Notepad"
wait_timeout_ms = 5000

[success]
kind = "Shell"
command = 'powershell -Command "if ((Get-Content $env:TEMP\\fastuse-eval-notepad.txt -Raw) -match \"Hello fastuse v2 eval\") { exit 0 } else { exit 1 }"'
```

- [ ] **Step 5: Driver binary skeleton**

`crates/fastuse-eval/src/bin/eval.rs`:
```rust
//! Usage: cargo run --release -p fastuse-eval -- run scenarios/calculator.toml
//! Usage: cargo run --release -p fastuse-eval -- run-tier 1

use anyhow::{Context, Result};
use fastuse_eval::scenario::{Scenario, SuccessCheck};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 { eprintln!("usage: eval run <scenario.toml> | run-tier <N>"); std::process::exit(2); }
    match args[1].as_str() {
        "run" => run_one(PathBuf::from(&args[2])).await,
        "run-tier" => {
            let tier: u8 = args[2].parse()?;
            let dir = PathBuf::from("crates/fastuse-eval/scenarios");
            let mut failures = 0;
            for entry in std::fs::read_dir(&dir)? {
                let path = entry?.path();
                if path.extension().and_then(|s| s.to_str()) != Some("toml") { continue; }
                let raw = std::fs::read_to_string(&path)?;
                let s: Scenario = toml::from_str(&raw)?;
                if s.tier != tier { continue; }
                println!("=> {} (tier {})", s.name, s.tier);
                if let Err(e) = run_one(path).await {
                    eprintln!("FAIL: {e:#}");
                    failures += 1;
                }
            }
            if failures > 0 { std::process::exit(1); }
            Ok(())
        }
        other => { eprintln!("unknown subcommand: {other}"); std::process::exit(2); }
    }
}

async fn run_one(path: PathBuf) -> Result<()> {
    let raw = std::fs::read_to_string(&path).with_context(|| format!("read {path:?}"))?;
    let scenario: Scenario = toml::from_str(&raw).with_context(|| format!("parse {path:?}"))?;
    // 1. Launch the target app via fastuse-cli (uses fastuse-mcp under the hood).
    // 2. Spawn `claude` (Claude Code headless) with the task as prompt.
    //    For v1 of the eval harness, document this as TODO at the runtime
    //    level — manual "you run Claude Code, paste this prompt, press
    //    enter, watch what happens" is acceptable for the first rev.
    //    Automating the headless invocation depends on Claude Code's
    //    --print/--prompt mode; revisit when we have time.
    eprintln!("TODO: drive Claude Code with prompt:\n{}", scenario.task);
    eprintln!("After Claude finishes, run success check manually:");
    match &scenario.success {
        SuccessCheck::UiaValueEquals { selector, expected } => {
            eprintln!("  fastuse-cli uia-query '{selector}' (expect value {expected:?})");
        }
        SuccessCheck::ScreenshotContainsText { region_native, needle } => {
            eprintln!("  Screenshot region {region_native:?}, find {needle:?} in OCR output");
        }
        SuccessCheck::Shell { command } => {
            eprintln!("  Run: {command}");
        }
    }
    Ok(())
}
```

The first rev intentionally stops short of fully automated Claude Code orchestration — that's an additional engineering effort (headless Claude Code invocation, conversation transcript capture, parsing). Document it as a known follow-up; the manual harness is enough to drive scenarios during v2 stabilization.

- [ ] **Step 6: Add to workspace**

Edit workspace `Cargo.toml` `members` array to include `crates/fastuse-eval`.

- [ ] **Step 7: Build**

Run: `cargo build --release -p fastuse-eval`
Expected: PASS.

- [ ] **Step 8: Smoke test**

```
.\target\release\eval.exe run crates/fastuse-eval/scenarios/calculator.toml
# Should print the prompt and success check command.
```

- [ ] **Step 9: Commit**

```bash
git add crates/fastuse-eval/ Cargo.toml
git commit -F <(printf '%s\n' \
  "feat(eval): scenario harness skeleton + Tier 1 calculator/notepad" \
  "" \
  "Scenario format (TOML): name, tier, launch spec, natural-language task," \
  "deterministic success check (UiaValueEquals, ScreenshotContainsText," \
  "Shell). v1 harness prints the prompt and the success-check command for" \
  "manual orchestration; full Claude-Code-headless automation deferred." \
  "" \
  "Tier 1 scenarios: calculator-arithmetic, notepad-edit-save.")
```

### Task 19: Tier 1 expansion — Chrome, VS Code, Discord scenarios

**Files:**
- Create: `crates/fastuse-eval/scenarios/chrome-form.toml`
- Create: `crates/fastuse-eval/scenarios/vscode-edit.toml`
- Create: `crates/fastuse-eval/scenarios/discord-message.toml`

- [ ] **Step 1: Chrome scenario**

`chrome-form.toml`:
```toml
name = "chrome-fill-search"
tier = 1
task = """
Open Google Chrome. In the address bar, navigate to https://www.google.com.
Type "fastuse computer use" into the search box and press Enter. Verify
search results loaded.
"""
timeout_seconds = 90

[launch]
command = "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe"
args = ["--new-window", "https://www.google.com"]
wait_for_window_title_substr = "Google"
wait_timeout_ms = 8000

[success]
kind = "ScreenshotContainsText"
needle = "fastuse"
```

- [ ] **Step 2: VS Code scenario**

`vscode-edit.toml`:
```toml
name = "vscode-edit-save"
tier = 1
task = """
Open VS Code with no folder. Use Ctrl+N to create a new file. Type a small
Python function that returns 42. Save the file as %TEMP%\\fastuse-eval-vscode.py.
"""
timeout_seconds = 120

[launch]
command = "code"
args = ["--new-window", "--disable-extensions"]
wait_for_window_title_substr = "Visual Studio Code"
wait_timeout_ms = 10000

[success]
kind = "Shell"
command = 'powershell -Command "if ((Get-Content $env:TEMP\\fastuse-eval-vscode.py -Raw) -match \"return 42\") { exit 0 } else { exit 1 }"'
```

- [ ] **Step 3: Discord scenario**

`discord-message.toml`:
```toml
name = "discord-send-self-dm"
tier = 1
task = """
Discord is already running and logged in (user prerequisite). Focus the
Discord window. Open the Discord quick switcher with Ctrl+K. Search for the
"#fastuse-eval-channel" text channel (user prerequisite — must exist).
Send the message "fastuse v2 eval ping" in that channel.
"""
timeout_seconds = 90

[launch]
command = "discord-prereq"  # fake — user must have Discord running
args = []
wait_for_window_title_substr = "Discord"
wait_timeout_ms = 5000

[success]
kind = "ScreenshotContainsText"
needle = "fastuse v2 eval ping"
```

The Discord scenario assumes a logged-in Discord and a `#fastuse-eval-channel` channel — these are user-side prerequisites documented in `docs/eval-prereqs.md` (write that doc as part of this task — short, just lists what the user needs set up).

- [ ] **Step 4: Write the prereqs doc**

Create `docs/eval-prereqs.md`:
```markdown
# Evaluation suite prerequisites

Before running the eval suite, set up:

1. **Discord** installed and logged in. Create a private text channel
   `#fastuse-eval-channel` in any server you control.
2. **VS Code** on PATH (`code` command works).
3. **Google Chrome** at default install path (`C:\Program Files\Google\Chrome\Application\chrome.exe`).
4. **L-Connect3** installed (Tier 2 scenarios reference it; skip those if absent).

Run scenarios via:
```
.\target\release\eval.exe run-tier 1
```
```

- [ ] **Step 5: Build (no code change, but make sure scenarios parse)**

```
cargo build --release -p fastuse-eval
.\target\release\eval.exe run-tier 1
# Should print prompts for all 5 Tier 1 scenarios. No PASS/FAIL automation
# yet; manual orchestration.
```

- [ ] **Step 6: Commit**

```bash
git add crates/fastuse-eval/scenarios/ docs/eval-prereqs.md
git commit -F <(printf '%s\n' \
  "test(eval): Tier 1 scenarios for Chrome, VS Code, Discord" \
  "" \
  "Five total Tier 1 scenarios: calculator-arithmetic, notepad-edit-save," \
  "chrome-fill-search, vscode-edit-save, discord-send-self-dm. Documented" \
  "user-side prerequisites in docs/eval-prereqs.md (logged-in Discord," \
  "code on PATH, Chrome at default path).")
```

### Task 20: Tier 2 adversarial scenarios

**Files:**
- Create: `crates/fastuse-eval/scenarios/lconnect3-settings.toml`
- Create: `crates/fastuse-eval/scenarios/photoshop-or-gimp.toml`
- Create: `crates/fastuse-eval/scenarios/steam-launcher.toml`
- Create: `crates/fastuse-eval/scenarios/tauri-app.toml`

- [ ] **Step 1: L-Connect3 scenario (motivating case)**

`lconnect3-settings.toml`:
```toml
name = "lconnect3-open-settings"
tier = 2
task = """
L-Connect3 is open and visible on screen (user must launch it manually —
the install path varies). Click the Settings tab in L-Connect3, then click
the "System info" toggle to enable the floating system info window.
"""
timeout_seconds = 120

[launch]
command = "lconnect3-prereq"
args = []
wait_for_window_title_substr = "L-Connect"
wait_timeout_ms = 5000

[success]
kind = "ScreenshotContainsText"
needle = "System info"
```

- [ ] **Step 2: Photoshop or GIMP scenario**

`photoshop-or-gimp.toml`:
```toml
name = "image-editor-basic-edit"
tier = 2
task = """
Open GIMP (or Photoshop if installed). Create a new 100x100 image with white
background. Use the bucket fill tool to fill it red. Save as %TEMP%\\fastuse-eval-red.png.
"""
timeout_seconds = 180

[launch]
command = "gimp"
args = []
wait_for_window_title_substr = "GIMP"
wait_timeout_ms = 15000

[success]
kind = "Shell"
command = 'powershell -Command "if (Test-Path $env:TEMP\\fastuse-eval-red.png) { exit 0 } else { exit 1 }"'
```

- [ ] **Step 3: Steam launcher scenario**

`steam-launcher.toml`:
```toml
name = "steam-search-game"
tier = 2
task = """
Steam is open and signed in (user prerequisite). In the Steam window, click
the "Store" tab, then click into the search box. Type "Portal 2" and verify
the search results show Portal 2 in the results list.
"""
timeout_seconds = 90

[launch]
command = "steam-prereq"
args = []
wait_for_window_title_substr = "Steam"
wait_timeout_ms = 5000

[success]
kind = "ScreenshotContainsText"
needle = "Portal 2"
```

- [ ] **Step 4: Custom Tauri / Iced / egui app scenario**

`tauri-app.toml`:
```toml
name = "compiled-app-test"
tier = 2
task = """
A user-built compiled application is at $env:FASTUSE_TEST_APP_PATH (set this
env variable before running). Launch the app, wait for its main window, then
exercise the primary feature (clicking the most prominent button or filling
the most prominent input).
"""
timeout_seconds = 90

[launch]
command = "${FASTUSE_TEST_APP_PATH}"
args = []
wait_for_window_title_substr = "${FASTUSE_TEST_APP_TITLE}"
wait_timeout_ms = 8000

[success]
kind = "ScreenshotContainsText"
needle = "${FASTUSE_TEST_APP_SUCCESS_NEEDLE}"
```

(The `${...}` placeholders document a deferred capability: the eval driver should expand env vars in launch + success fields. Add the expansion in `eval.rs` if not already there — `shellexpand` crate is the common Rust way: add `shellexpand = "3"` to `fastuse-eval/Cargo.toml`. For now, simple `std::env::var` substitution in `eval.rs` is fine.)

- [ ] **Step 5: Update `docs/eval-prereqs.md`**

Add to `docs/eval-prereqs.md`:
```markdown
## Tier 2 prerequisites

5. **L-Connect3** installed and visible on screen.
6. **GIMP** installed (or Photoshop, if you have a license — adapt scenario).
7. **Steam** installed and signed in.
8. **A test app you compiled.** Set:
   - `FASTUSE_TEST_APP_PATH` to its `.exe` path
   - `FASTUSE_TEST_APP_TITLE` to a substring of its window title
   - `FASTUSE_TEST_APP_SUCCESS_NEEDLE` to a string visible after the primary
     interaction succeeds
```

- [ ] **Step 6: Build**

```
cargo build --release -p fastuse-eval
.\target\release\eval.exe run-tier 2
```

- [ ] **Step 7: Commit**

```bash
git add crates/fastuse-eval/scenarios/ docs/eval-prereqs.md crates/fastuse-eval/Cargo.toml crates/fastuse-eval/src/bin/eval.rs
git commit -F <(printf '%s\n' \
  "test(eval): Tier 2 adversarial-app scenarios" \
  "" \
  "L-Connect3 (custom-rendered WPF, original motivating case for v2 pivot)," \
  "GIMP/Photoshop (image editor with non-trivial UI), Steam launcher" \
  "(Chromium-based proprietary client), and a templated 'compiled app' slot" \
  "via env vars for testing user-built Tauri/Iced/egui apps. Eval driver" \
  "expands env vars in launch and success fields.")
```

### Task 21: Tier 3 multi-step task chains (manual)

**Files:**
- Create: `docs/eval-tier3-tasks.md`

- [ ] **Step 1: Document Tier 3 tasks as natural-language workflows**

Tier 3 isn't fully automated — it's a curated list of real workflows you'd ask Claude to do. Each is graded manually by you, recorded for trend tracking.

`docs/eval-tier3-tasks.md`:
```markdown
# Tier 3 — multi-step task chains

These are end-to-end workflows that exercise mixed apps. They're not
automated; you ask Claude in a Claude Code conversation, watch it work, then
record pass/fail in `docs/eval-tier3-results.csv`.

Pass criterion: the task finished as you'd want a competent human to do it,
within reasonable time, with no irreversible mistakes.

## T3.1 — Discord-to-OneNote relay

> Open Discord, find the user **<your Discord friend's name>**, send them
> the message "fastuse v2 ping". Take a screenshot of their reply. Open
> OneNote and paste the screenshot into the **fastuse-eval** notebook.
> Save.

## T3.2 — Test a compiled app

> I just compiled my Tauri app at `$env:FASTUSE_TEST_APP_PATH`. Launch it,
> verify the new search feature works (try searching "hello"), report
> back what you see.

## T3.3 — Google Play Console dashboard

> Take a screenshot of the Google Play Console dashboard and tell me which
> of my apps are restricted, suspended, or have policy issues.

## T3.4 — Mixed-app research summary

> Open Chrome, search for "OSWorld benchmark 2026 latest results", read the
> top 3 results, then summarize what you found in a new Notepad document.
> Save the document as %TEMP%\\fastuse-eval-research.txt.

## T3.5 — File reorganization

> Open File Explorer, navigate to %TEMP%, find all *.txt files created in
> the last hour, and move them into a new subfolder called
> `fastuse-eval-archive`.

## Recording results

After each run, append a row to `docs/eval-tier3-results.csv`:

```
date,task,pass,duration_min,notes
2026-05-15,T3.1,true,4,one retry on Discord focus
```
```

- [ ] **Step 2: Initialize the results file**

```
echo "date,task,pass,duration_min,notes" > docs/eval-tier3-results.csv
```

- [ ] **Step 3: Commit**

```bash
git add docs/eval-tier3-tasks.md docs/eval-tier3-results.csv
git commit -F <(printf '%s\n' \
  "test(eval): Tier 3 multi-step task chains (manual evaluation)" \
  "" \
  "Five curated workflows that mix apps and exercise the agent loop end-to-end:" \
  "Discord -> OneNote relay, compiled-app testing, Google Play Console" \
  "dashboard read, mixed-app research summary, file reorganization. Pass" \
  "criterion: task finished as a competent human would, within reasonable" \
  "time, with no irreversible mistakes. Results tracked in CSV for trends.")
```

### Task 22: CLAUDE.md and README rewrite

**Files:**
- Modify: `CLAUDE.md`
- Modify: `README.md`
- Create: `docs/architecture.md`
- Create: `docs/configuration.md`
- Create: `docs/permissions.md`
- Create: `docs/migration-from-v1.md`

- [ ] **Step 1: Rewrite `CLAUDE.md` (project-root)**

Replace the project-level CLAUDE.md content (the project block, NOT the user's global CLAUDE.md) with:

```markdown
## Project

**fastuse v2 — vision-first Windows computer-use control plane**

Drives the Windows desktop like a human: see the screen, click, type, scroll.
Vision-first; no accessibility-tree-based targeting. Works on cooperative apps
(Calculator, browsers, IDEs) and adversarial apps (Electron, custom-rendered
WPF, Direct2D) because the substrate is vision, not UIA introspection.

**Primary path: MCP tools.**

After running `fastuse-cli setup-mcp --user` once and restarting Claude Code,
the following tools are available:

- `mcp__fastuse__computer({action: "screenshot"})` — returns ImageContent inline
- `mcp__fastuse__computer({action: "left_click", coordinate: [x, y]})`
- `mcp__fastuse__computer({action: "type", text: "hello"})`
- `mcp__fastuse__computer({action: "key", text: "ctrl+s"})`
- `mcp__fastuse__computer({action: "scroll", coordinate: [x, y], scroll_direction: "down", scroll_amount: 3})`
- `mcp__fastuse__computer({action: "left_click_drag", start_coordinate: [...], coordinate: [...]})`
- `mcp__fastuse__computer({action: "zoom", coordinate: [x, y], zoom_factor: 2.5})` — for fine-detail inspection

Plus Windows-specific helpers as separate MCP tools:
`mcp__fastuse__list_windows`, `mcp__fastuse__focus_window`,
`mcp__fastuse__foreground_window`, `mcp__fastuse__wait_for_window`,
`mcp__fastuse__list_processes`, `mcp__fastuse__kill_process`,
`mcp__fastuse__launch_app`, `mcp__fastuse__shell_exec`,
`mcp__fastuse__clipboard_get_text`, `mcp__fastuse__clipboard_set_text`,
`mcp__fastuse__inspect_at`, `mcp__fastuse__uia_query`,
`mcp__fastuse__uia_tree`.

UIA tools are read-only — they return structure for grounding (DevTools-style),
they do not click. All clicking goes through `computer` with coordinates.

**Secondary path: Bash → CLI** for one-shot actions, debugging, scripting:
- `fastuse-cli computer screenshot --out file.jpg`
- `fastuse-cli computer left-click 100 200 --instant`
- `fastuse-cli list-windows --title "Discord"`

**Coordinate system.** MCP screenshots are scaled to ~1024-wide image space.
Click coordinates Claude returns in MCP are in *that scaled space*; the daemon
translates to native pixels. CLI uses native virtual-desktop pixels (no scaling).

**Always take a screenshot before clicking** unless you're sure the layout
hasn't changed since the last screenshot. Without a recent screenshot, click
coords have no scale context and the daemon will reject them.

**Permission model.** Default: all tools available. Set `FASTUSE_SAFE_MODE=1`
or edit `%LOCALAPPDATA%\fastuse\config.toml` to gate `kill_process`,
`shell_exec`, `launch_app`, `clipboard_set_*`. Computer actions, listings,
and UIA inspection always allowed.

**Daemon lifecycle.** Auto-spawns elevated via UAC on first call. Idle-timeout
5 min. On `DAEMON_SPAWN_FAILED`: kill stale daemon, remove pid sentinel, retry.

**Humanization (default-on).** Mouse moves use Bezier curves; keystrokes have
timing jitter. Defeats web behavioral bot detection (Cloudflare-class). Pass
`humanize: false` (MCP) or `--instant` (CLI) to opt out for speed-over-realism
cases (driving your own apps for testing, automated scripts).

**Out of scope.** Anti-cheat circumvention, kernel-mode drivers, hardware HID
firmware (latter deferred to a follow-up project).
```

- [ ] **Step 2: Rewrite `README.md`**

```markdown
# fastuse — Windows computer-use control plane for AI agents

Vision-first Rust daemon + CLI + MCP server for Claude Code (and any
MCP-compatible agent). Drives the Windows desktop like a human: see, click,
type, scroll. Sub-50ms primitive RTT. Works on cooperative apps (Calculator,
browsers, IDEs) and adversarial apps (Electron, custom-rendered WPF, Direct2D).

## Install

```powershell
git clone https://github.com/DIV7NE/fastuse
cd fastuse
cargo build --release --workspace
.\target\release\fastuse-cli.exe setup-mcp --user
# Restart Claude Code. Done.
```

## Quick start

```powershell
# Sanity check
.\target\release\fastuse-cli.exe ping

# Take a screenshot
.\target\release\fastuse-cli.exe computer screenshot --out screen.jpg

# Click somewhere (native coords)
.\target\release\fastuse-cli.exe computer left-click 100 200

# In Claude Code (after setup-mcp): just ask Claude to do things.
# "Take a screenshot and tell me what's on screen."
# "Open Notepad and type 'hello world'."
```

## Architecture

See [`docs/architecture.md`](docs/architecture.md). Three Rust binaries from
one workspace, talking to one daemon over Windows named pipes.

## Configuration

See [`docs/configuration.md`](docs/configuration.md). Optional `config.toml`
for permissions, input humanization, screenshot scaling target.

## Permissions

See [`docs/permissions.md`](docs/permissions.md). Default open;
`FASTUSE_SAFE_MODE=1` gates destructive tools.

## v1 → v2 migration

See [`docs/migration-from-v1.md`](docs/migration-from-v1.md). v1 used
UIA+OCR targeting (`click-element {ByName: "OK"}`). v2 is vision-first
(`computer({action: "left_click", coordinate: [x, y]})`). Old binaries
preserved at tag `v1.0-pre-pivot`.

## Spec & plan

- Design: [`docs/superpowers/specs/2026-05-05-fastuse-v2-vision-first-design.md`](docs/superpowers/specs/2026-05-05-fastuse-v2-vision-first-design.md)
- Plan: [`docs/superpowers/plans/2026-05-05-fastuse-v2-vision-first.md`](docs/superpowers/plans/2026-05-05-fastuse-v2-vision-first.md)
```

- [ ] **Step 3: Architecture doc**

`docs/architecture.md`: copy the architecture diagram and crate breakdown from the spec, light prose around it. ~150 lines.

- [ ] **Step 4: Configuration doc**

`docs/configuration.md`:
```markdown
# Configuration

fastuse reads `%LOCALAPPDATA%\fastuse\config.toml` if present. All sections
optional; sensible defaults if absent.

## `[permissions]`

```toml
[permissions]
safe_mode = false
gated_tools = []  # if non-empty, replaces the default gated set
```

`safe_mode = true` activates gating. Default gated set when no custom list:
`launch_app, kill_process, shell_exec, clipboard_set_text, clipboard_set_image`.

Override at runtime: `FASTUSE_SAFE_MODE=1` (or `0` to force off) takes precedence.

## `[input]`

```toml
[input]
default_humanize = true
typing_mean_ms = 80
typing_stddev_ms = 30
```

## `[scaling]`

```toml
[scaling]
target_max = 1024
```

Longest side of scaled screenshots. Anthropic's `computer_20251124`
recommends 1024. Larger values increase fidelity at the cost of token usage.
```

- [ ] **Step 5: Permissions doc**

`docs/permissions.md`:
```markdown
# Permissions and safe mode

Default: all tools available. Rationale: Claude controls the computer like a
human would; humans don't need permission to open Notepad.

Safe mode: opt-in lockdown for unattended use or sharing the daemon with
untrusted agents.

## Activate safe mode

Three ways, in priority order:

1. **Per-session env**: `FASTUSE_SAFE_MODE=1` set when the daemon spawns.
2. **Per-call env (CLI only)**: `FASTUSE_SAFE_MODE=1 fastuse-cli launch-app calc.exe`.
3. **Persistent**: `%LOCALAPPDATA%\fastuse\config.toml` `[permissions] safe_mode = true`.

## Default gated tools (when safe_mode is on)

- `launch_app` — could spawn arbitrary executables
- `kill_process` — can kill user's work or system processes
- `shell_exec` — arbitrary command execution
- `clipboard_set_text` — overwrites user's clipboard
- `clipboard_set_image` — same

Override the default list with `[permissions] gated_tools = ["..."]` in config.

## Always allowed

`computer` actions (visible to user, reversible), window listing/focus/wait,
process listing, clipboard *read*, UIA inspection, daemon meta tools.

## Failure mode

Gated tool when safe_mode active returns:

```json
{
  "error": {
    "code": "PERMISSION_REQUIRED",
    "message": "tool 'kill_process' is gated in safe mode",
    "hint": "set FASTUSE_SAFE_MODE=0 or remove from gated_tools in config.toml"
  }
}
```

CLI exit code 1; MCP returns isError=true with the message.
```

- [ ] **Step 6: Migration doc**

`docs/migration-from-v1.md`:
```markdown
# Migrating fastuse scripts from v1 to v2

v2 deletes the entire UIA+OCR targeting layer. The fast-path "click an
element by name" workflow becomes "screenshot, look, click coordinates."

## CLI subcommand changes

| v1 | v2 equivalent |
|---|---|
| `click-element '{"ByName":"OK"}'` | `computer screenshot` then `computer left-click X Y` (Claude reads the screenshot and decides X,Y) |
| `type-into-element '{"ByAutomationId":"input"}' "hello"` | `click-element` analog: focus via screenshot+click, then `computer type "hello"` |
| `wait-for-element '{...}' --timeout-ms N` | `wait-for-window --title <substr> --timeout-ms N` (window-level only) |

UIA inspection still works as read-only:

| v1 | v2 |
|---|---|
| `uia-query '{...}'` | unchanged (read-only) |
| `uia-tree --hwnd N` | unchanged (read-only) |
| `inspect-at X Y` | unchanged (read-only) |

## Wire protocol changes

Wire types removed: `Strategy`, `VerificationEvidence`, `ExpectClause`,
`EscalatePolicy`, `Response::ActionResult`. Replaced by `ComputerAction`
enum + `ComputerResult` (carries optional `ImagePayload` and `ScaleInfo`).

If you wrote scripts that parsed `ActionResult` JSON: they need to read
`ComputerResult` instead.

## Tag v1.0-pre-pivot

v1 binaries preserved at git tag `v1.0-pre-pivot` (pushed to origin).
Roll back with `git checkout v1.0-pre-pivot && cargo build --release`.

## What stays

- Daemon lifecycle (auto-spawn elevated, named pipes, idle timeout, sentinel)
- Window/process/clipboard/launch/shell helpers
- UIA pool + capture thread infrastructure
- Latency budgets and locked invariants (D-10, D-21, D-24, D-25, D-26)
```

- [ ] **Step 7: Build, lint**

```
cargo build --release --workspace
cargo run -p xtask -- lints
cargo test --release --workspace --lib --bins
```

- [ ] **Step 8: Commit**

```bash
git add CLAUDE.md README.md docs/architecture.md docs/configuration.md docs/permissions.md docs/migration-from-v1.md
git commit -F <(printf '%s\n' \
  "docs: rewrite CLAUDE.md, README, and reference docs for v2" \
  "" \
  "MCP-primary guidance, computer_20251124 schema reference, coordinate" \
  "scaling notes, permission model, daemon lifecycle. Architecture diagram," \
  "configuration reference (config.toml sections), permissions matrix," \
  "v1->v2 migration guide listing CLI changes and removed wire types." \
  "" \
  "v1 binaries preserved at tag v1.0-pre-pivot.")
```

### Task 23: Final verification, version bump, tag v2.0

**Files:**
- Modify: each crate's `Cargo.toml` (version bump to `2.0.0`)
- Modify: workspace `Cargo.toml` (workspace version if used)
- Modify: `CHANGELOG.md` (create or append)

- [ ] **Step 1: Bump versions**

For each crate (`fastuse-proto`, `fastuse-core`, `fastuse-win`, `fastuse-daemon`, `fastuse-cli`, `fastuse-mcp`, `fastuse-eval`):

Edit `Cargo.toml`:
```toml
version = "2.0.0"
```

If the workspace uses `package.version.workspace = true`, bump in the workspace `Cargo.toml`'s `[workspace.package]` section instead — single bump.

- [ ] **Step 2: Create or append `CHANGELOG.md`**

```markdown
# Changelog

## 2.0.0 — 2026-05-XX (set actual release date)

Vision-first computer-use rebuild. Targeting layer (UIA+OCR) deleted; primary
surface is the `computer` MCP tool matching Anthropic `computer_20251124`.
UIA demoted to read-only inspection. Tier 1 humanization (Bezier curves +
timing jitter) on by default for input.

### Added
- `computer` MCP tool (Anthropic schema fidelity)
- `fastuse-mcp` server with image-content inline screenshots
- `wait_for_window` Windows helper
- `zoom` action for fine-detail inspection
- Coordinate scaling state machine (per-session ScaleStack)
- `InputBackend` trait abstraction (default: `SendInputBackend` with humanization)
- `permissions` module with safe-mode gating
- `config.toml` loader (`[permissions]`, `[input]`, `[scaling]` sections)
- `setup-mcp` CLI subcommand (registers fastuse with Claude Code)
- `fastuse-eval` crate with Tier 1, 2, 3 evaluation scenarios

### Removed
- `targeting/` module (execute, profile, candidate, strategy, hit_test, verify)
- `ocr/` module (cropped, cache) and `ocr_thread`
- CLI subcommands: `click-element`, `type-into-element`, `wait-for-element`
- Wire types: `Strategy`, `VerificationEvidence`, `ExpectClause`,
  `EscalatePolicy`, `Response::ActionResult`

### Changed
- CLAUDE.md rewritten around MCP-primary surface
- README rewritten with v2 quick-start and migration link

### Migration
v1 binaries preserved at tag `v1.0-pre-pivot`. See
[docs/migration-from-v1.md](docs/migration-from-v1.md).
```

- [ ] **Step 3: Final full-workspace verification**

```
cargo build --release --workspace
cargo test --release --workspace --lib --bins
cargo run -p xtask -- lints
```

All three must PASS. Any failure → fix before tagging.

- [ ] **Step 4: Run Tier 1 eval suite manually**

```
.\target\release\eval.exe run-tier 1
```

For each scenario, drive Claude Code with the prompt, observe the result, run the success-check command. Record results in a temp file.

Pass criterion (per spec § Success criteria): ≥95% pass on Tier 1 (5 scenarios × 10 runs each = 50 trials, allow up to 2 failures). If pass rate is materially worse, investigate root cause before tagging.

- [ ] **Step 5: Smoke-test Tier 2 manually**

```
.\target\release\eval.exe run-tier 2
```

L-Connect3 specifically must work — that's the motivating case. The other Tier 2 scenarios are nice-to-have for v2.0.0; record results, address failures only if blocking.

- [ ] **Step 6: Commit version bump and changelog**

```bash
# Stage version files individually
git add Cargo.toml crates/*/Cargo.toml CHANGELOG.md
git commit -F <(printf '%s\n' \
  "chore: bump to 2.0.0" \
  "" \
  "Vision-first rebuild complete. v1 preserved at tag v1.0-pre-pivot.")
```

- [ ] **Step 7: Tag and push**

```bash
git tag -a v2.0.0 -m "fastuse v2.0.0 — vision-first computer-use rebuild"
git push origin v1.0
git push origin v2.0.0
```

- [ ] **Step 8: Optionally create a GitHub Release**

```bash
gh release create v2.0.0 --title "fastuse v2.0.0 — vision-first" --notes-file CHANGELOG.md
```

(Skip if you'd rather draft the release manually in the GitHub UI.)

<!-- TASKS_END -->
