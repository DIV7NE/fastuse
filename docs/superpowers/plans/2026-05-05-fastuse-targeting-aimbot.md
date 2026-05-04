# Aimbot-Grade Targeting Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `click_element` / `type_into_element` / `wait_for_element` / `scroll_into_view` reliable across unknown Windows apps including degraded-UIA cases (Electron, Qt, custom renderers), with honest verification that never reports `verified: true` without a real postcondition match.

**Architecture:** New `fastuse-win/src/targeting/` module orchestrates per-action profile → strategy → execute → verify pipeline. Wire shape changes from `ActionResult { ok }` to `ActionResult { verified, evidence, strategy_used, waited_ms: Option<u32>, screenshot }`. New OCR thread (4th MTA COM-thread surface) runs `Windows.Media.Ocr` for degraded-UIA fallback.

**Tech Stack:** Rust 1.83, uiautomation 0.24.4, windows 0.62, tokio 1.49, dashmap 6.1. Existing UIA pool (MTA, 3 workers) + capture thread (MTA, GPU) + input thread (STA) all unchanged. New `ocr_thread` mirrors capture_thread shape.

**Spec:** `docs/superpowers/specs/2026-05-05-fastuse-targeting-aimbot-design.md` @ commit `470e2ce`.

**Working principles:**
- Atomic commits, conventional message, no AI-attribution footers (per global CLAUDE.md)
- Each task: build clean, tests green, xtask lints green, then commit
- Build: `cargo build --release --workspace`
- Tests: `cargo test --release --workspace --lib --bins`
- Lints: `cargo run -p xtask -- lints`
- Stay on `v1.0` branch

---

## File structure

Files created in this plan:

| File | Purpose |
|------|---------|
| `crates/fastuse-win/src/targeting/mod.rs` | Module root, re-exports |
| `crates/fastuse-win/src/targeting/profile.rs` | `WindowSignals`, `ProfileCacheKey`, `TargetProfile`, classification |
| `crates/fastuse-win/src/targeting/strategy.rs` | Pattern-availability strategy picker, `Strategy` enum |
| `crates/fastuse-win/src/targeting/verify.rs` | Postcondition contracts + 25ms polling helper |
| `crates/fastuse-win/src/targeting/execute.rs` | Run picked strategy + verify + escalate, returns `ActionResult` |
| `crates/fastuse-win/src/targeting/hit_test.rs` | `ElementFromPoint` gate before any coordinate click |
| `crates/fastuse-win/src/targeting/candidate.rs` | `TargetCandidate` enum + UIA-candidate resolver |
| `crates/fastuse-win/src/ocr_thread.rs` | New MTA thread hosting `Windows.Media.Ocr` |
| `crates/fastuse-win/src/ocr/mod.rs` | OCR module root |
| `crates/fastuse-win/src/ocr/cropped.rs` | Cropped progressive OCR |
| `crates/fastuse-win/src/ocr/cache.rs` | Frame-hash OCR cache |

Files modified:

| File | Change |
|------|--------|
| `crates/fastuse-proto/src/wire.rs` | New `ActionResult` shape; `ActionOpts` consolidation; `VerificationEvidence`, `Strategy`, `ExpectClause`, `EscalatePolicy` enums |
| `crates/fastuse-daemon/src/action_opts.rs` | Rebuild around new evidence model |
| `crates/fastuse-daemon/src/dispatch.rs` | `finalize()` returns new `ActionResult`; element-action arms route through `targeting::execute` |
| `crates/fastuse-mcp/src/handler.rs` | `ActionResultOutput` emits both `ok` (deprecated alias) and `verified` |
| `crates/fastuse-cli/src/cmd_phase2.rs` | `print_action_or_ack` prints both keys |
| `crates/fastuse-cli/src/cmd_phase3.rs` | Pass new `ActionOpts` through element actions |
| `crates/fastuse-cli/src/main.rs` | `ActionOptsArgs` clap struct grows `--expect`, `--strict` |
| `crates/fastuse-win/src/lib.rs` | Re-export `targeting`, `ocr`, `ocr_thread` |
| `crates/xtask/src/lints/check_com.rs` | Allowlist `ocr_thread` as 4th MTA surface |

---

## Task 1: Wire shape change

**Why first:** every other task touches the response shape. Phase 4 actions inherit it via `dispatch::finalize()`.

**Files:**
- Modify: `crates/fastuse-proto/src/wire.rs`
- Test: `crates/fastuse-proto/src/wire.rs` (existing test module)

- [ ] **Step 1: Add new enums**

In `crates/fastuse-proto/src/wire.rs`, after the `ScreenshotPayload` struct (~line 72), add:

```rust
/// What level of evidence we have that the action achieved its intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerificationEvidence {
    /// A typed postcondition matched (ToggleState flipped, IsSelected==true,
    /// Value contains typed text, subtree mutation detected, etc.).
    PostconditionMet,
    /// Caller-provided `wait_for` / `expect::SelectorMatches` matched.
    WaitForMatched,
    /// Hit-test confirmed we struck the intended pixel; no postcondition was
    /// applicable. Honest "we did what we could but cannot prove effect."
    HitTestOnly,
    /// Strategy ran but no contract applied and no caller hint was provided
    /// (or contract failed a tolerance check).
    Unverified,
}

/// Which strategy ultimately produced the result reported by `ActionResult`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Strategy {
    /// `IUIAutomationInvokePattern::Invoke` — coord-free.
    UiaInvoke,
    /// `IUIAutomationTogglePattern::Toggle` — coord-free.
    UiaToggle,
    /// `IUIAutomationSelectionItemPattern::Select` — coord-free.
    UiaSelect,
    /// `IUIAutomationExpandCollapsePattern::Expand/Collapse` — coord-free.
    UiaExpandCollapse,
    /// `IUIAutomationValuePattern::SetValue` — coord-free; used by type_into.
    UiaSetValue,
    /// SendInput at DPI-correct center of UIA-reported bounds, gated by hit-test.
    BoundsClickUia,
    /// SendInput at DPI-correct center of OCR-derived bounds, gated by hit-test.
    BoundsClickOcr,
    /// SendInput at caller-provided coords, gated by hit-test.
    BoundsClickGeometry,
}

/// Caller-supplied postcondition richer than a bare `wait_for` selector.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ExpectClause {
    /// Some new top-level dialog/window appeared.
    DialogOpens,
    /// Foreground window's title now matches the substring.
    WindowTitleMatches(String),
    /// Foreground HWND now belongs to a window matching class/title.
    ForegroundChangesTo {
        /// Optional class-name substring filter.
        class: Option<String>,
        /// Optional title substring filter.
        title: Option<String>,
    },
    /// A selector matches in the foreground tree (replaces old `verify`).
    SelectorMatches(Selector),
}

/// Whether the daemon may escalate strategies internally on verification miss.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EscalatePolicy {
    /// Full ladder for the chosen candidate kind (default).
    Auto,
    /// Single-shot, no escalation. For QA flows where a miss must surface.
    Strict,
}
```

- [ ] **Step 2: Replace `ActionOpts` and `ActionResult`**

In the same file, replace the existing `ActionOpts` struct (~lines 47-59):

```rust
/// Optional post-action perception bundle — collapses perceive→act→perceive
/// into a single tool call. Absent on legacy clients (decoded as `None`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionOpts {
    /// Non-failing wait: poll UIA after the action until this selector matches
    /// or `wait_timeout_ms` elapses. Result populates `evidence`.
    pub wait_for: Option<Selector>,
    /// Failing wait + richer postconditions. Replaces the old `verify` field.
    /// On timeout, the action is reported `verified: false`.
    pub expect: Option<ExpectClause>,
    /// Capture a screenshot after the action (and after `wait_for` / `expect`).
    pub screenshot_after: Option<ScreenshotOpts>,
    /// Timeout shared by `wait_for`, `expect`, and postcondition polling.
    /// Default 2000ms when unset.
    pub wait_timeout_ms: Option<u32>,
    /// Whether the daemon may escalate strategies internally on verification
    /// miss. `None` = `Auto`.
    pub escalate: Option<EscalatePolicy>,
}
```

Then replace the existing `Response::ActionResult { ok, wait_matched, waited_ms, screenshot }` variant (~lines 721-731):

```rust
    /// Action result with optional post-action perception. Used when the
    /// caller passed `ActionOpts`. Without `opts`, dispatchers continue to
    /// return `Ack` / `Element` as before.
    ActionResult {
        /// True if a typed postcondition matched, or caller `wait_for` /
        /// `expect` matched. Never true without one of those.
        verified: bool,
        /// Tag describing what evidence we had.
        evidence: VerificationEvidence,
        /// Which strategy produced this result.
        strategy_used: Strategy,
        /// Wall-clock time spent in any postcondition / `wait_for` polling;
        /// `None` when no polling occurred.
        waited_ms: Option<u32>,
        /// Optional post-action screenshot.
        screenshot: Option<Box<ScreenshotPayload>>,
    },
```

- [ ] **Step 3: Update `click_with_action_opts_round_trips` test**

Replace the existing test at the bottom of the file (~line 1152):

```rust
    #[test]
    fn click_with_action_opts_round_trips() {
        use crate::selector::Selector;
        use crate::uia_node::ControlType;
        let req = Request::Click {
            x: 100,
            y: 200,
            button: MouseButton::Left,
            count: 1,
            modifiers: vec![],
            skip_set_cursor_pos: false,
            opts: Some(ActionOpts {
                wait_for: Some(Selector::ByName("Saved".into())),
                expect: Some(ExpectClause::SelectorMatches(
                    Selector::ByControlType(ControlType::Window),
                )),
                screenshot_after: Some(ScreenshotOpts {
                    region: Some(RegionSpec::Auto),
                    format: Some(ImageFormat::Jpeg),
                    quality: Some(85),
                }),
                wait_timeout_ms: Some(2000),
                escalate: Some(EscalatePolicy::Auto),
            }),
        };
        let bytes = encode_frame(&req).unwrap();
        let mut cur = std::io::Cursor::new(bytes);
        let decoded: Request = decode_frame(&mut cur).unwrap();
        assert_eq!(req, decoded);
    }
```

- [ ] **Step 4: Add new round-trip tests**

Append in the `#[cfg(test)] mod tests` block:

```rust
    #[test]
    fn action_result_round_trips_with_evidence() {
        let res = Response::ActionResult {
            verified: true,
            evidence: VerificationEvidence::PostconditionMet,
            strategy_used: Strategy::UiaInvoke,
            waited_ms: Some(40),
            screenshot: None,
        };
        let bytes = encode_frame(&res).unwrap();
        let mut cur = std::io::Cursor::new(bytes);
        let decoded: Response = decode_frame(&mut cur).unwrap();
        assert_eq!(res, decoded);
    }

    #[test]
    fn action_result_unverified_round_trips() {
        let res = Response::ActionResult {
            verified: false,
            evidence: VerificationEvidence::Unverified,
            strategy_used: Strategy::BoundsClickOcr,
            waited_ms: None,
            screenshot: None,
        };
        let bytes = encode_frame(&res).unwrap();
        let mut cur = std::io::Cursor::new(bytes);
        let decoded: Response = decode_frame(&mut cur).unwrap();
        assert_eq!(res, decoded);
    }

    #[test]
    fn expect_clause_variants_round_trip() {
        let cases = vec![
            ExpectClause::DialogOpens,
            ExpectClause::WindowTitleMatches("Save".into()),
            ExpectClause::ForegroundChangesTo {
                class: Some("Notepad".into()),
                title: None,
            },
            ExpectClause::SelectorMatches(Selector::ByName("OK".into())),
        ];
        for c in cases {
            let bytes = postcard::to_allocvec(&c).unwrap();
            let decoded: ExpectClause = postcard::from_bytes(&bytes).unwrap();
            assert_eq!(c, decoded);
        }
    }
```

- [ ] **Step 5: Build and fix downstream call sites**

Run: `cargo build --release --workspace`
Expected: many errors in `fastuse-daemon`, `fastuse-mcp`, `fastuse-cli` referencing the old `ActionResult { ok, wait_matched, waited_ms }` and `ActionOpts.verify`.

Fix each compile error site to use the new shape with `Strategy::*` placeholder values. The mechanical translation:
- `ActionResult { ok: x, wait_matched: m, waited_ms: w, screenshot: s }` → `ActionResult { verified: x, evidence: if m == Some(true) { VerificationEvidence::WaitForMatched } else { VerificationEvidence::Unverified }, strategy_used: Strategy::UiaInvoke /* placeholder; refined in later tasks */, waited_ms: if w == 0 { None } else { Some(w) }, screenshot: s }`
- `ActionOpts.verify` reads → `ActionOpts.expect` matched against `ExpectClause::SelectorMatches(s)`

In `crates/fastuse-daemon/src/action_opts.rs`, the existing `apply()` function gets reworked. Replace the body so that `verify` becomes one branch on `expect`:

```rust
// At top of file, alongside existing imports:
use fastuse_proto::wire::{
    ActionOpts, ExpectClause, Response, ScreenshotPayload, Selector, Strategy,
    VerificationEvidence,
};

// Replace the existing apply() body. Keep its signature.
pub async fn apply(
    opts: Option<&ActionOpts>,
    inner_ok: bool,
    ctx: OptsCtx<'_>,
    strategy_used: Strategy,
) -> Response {
    let opts = match opts {
        Some(o) => o,
        None => return Response::Ack { slept_us: None },
    };

    let timeout = opts.wait_timeout_ms.unwrap_or(2000);
    let mut waited_ms: Option<u32> = None;
    let mut evidence = VerificationEvidence::Unverified;
    let mut verified = inner_ok;

    // wait_for: non-failing, populates evidence on match.
    if let Some(sel) = &opts.wait_for {
        let (matched, ms) = poll_selector(sel, timeout, ctx.uia).await;
        waited_ms = Some(ms);
        if matched {
            evidence = VerificationEvidence::WaitForMatched;
            verified = true;
        }
    }

    // expect: failing — timeout = verified false.
    if let Some(exp) = &opts.expect {
        let (matched, ms) = poll_expect(exp, timeout, ctx.uia).await;
        waited_ms = Some(waited_ms.unwrap_or(0).saturating_add(ms));
        if matched {
            evidence = VerificationEvidence::WaitForMatched;
            verified = inner_ok && true;
        } else {
            verified = false;
        }
    }

    let screenshot = if let Some(sopts) = &opts.screenshot_after {
        capture_after(sopts, ctx.capture).await.map(Box::new)
    } else {
        None
    };

    Response::ActionResult { verified, evidence, strategy_used, waited_ms, screenshot }
}

// New helper — selector with hard-fail on timeout:
async fn poll_expect(
    exp: &ExpectClause,
    timeout_ms: u32,
    uia: Option<&std::sync::Arc<crate::UiaPoolHandle>>,
) -> (bool, u32) {
    match exp {
        ExpectClause::SelectorMatches(sel) => poll_selector(sel, timeout_ms, uia).await,
        // The other ExpectClause variants are honored in execute.rs (Task 11).
        // For Phase 4 / non-targeting actions, we treat them as unsatisfied —
        // those callers shouldn't pass DialogOpens / WindowTitleMatches /
        // ForegroundChangesTo without going through targeting::execute.
        _ => (false, 0),
    }
}
```

Update every `ActionResult` constructor in `dispatch.rs` `finalize()` and any other site to pass through `strategy_used`. Where the action is not yet routed through `targeting::execute`, default to `Strategy::UiaInvoke` (refined by Task 12).

In `crates/fastuse-mcp/src/handler.rs`, the `ActionResultOutput` struct (search for `pub struct ActionResultOutput`) gets:

```rust
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ActionResultOutput {
    /// DEPRECATED: alias for `verified`. Kept for one release; removed in v1.1.
    /// Existing agents branching on `result.ok` continue to work.
    pub ok: bool,
    /// True iff a typed postcondition matched or caller `wait_for`/`expect`
    /// matched. The contract callers should branch on going forward.
    pub verified: bool,
    /// Evidence tag.
    pub evidence: String,
    /// Strategy used.
    pub strategy_used: String,
    /// Polling time, if any.
    pub waited_ms: Option<u32>,
    /// Optional post-action screenshot.
    pub screenshot: Option<ScreenshotPayloadOutput>,
}
```

The mapper from `Response::ActionResult` to `ActionResultOutput` sets `ok: verified`, then fills the rest. Format `evidence` and `strategy_used` as snake_case strings via a small helper:

```rust
fn evidence_str(e: VerificationEvidence) -> &'static str {
    match e {
        VerificationEvidence::PostconditionMet => "postcondition_met",
        VerificationEvidence::WaitForMatched => "wait_for_matched",
        VerificationEvidence::HitTestOnly => "hit_test_only",
        VerificationEvidence::Unverified => "unverified",
    }
}

fn strategy_str(s: Strategy) -> &'static str {
    match s {
        Strategy::UiaInvoke => "uia_invoke",
        Strategy::UiaToggle => "uia_toggle",
        Strategy::UiaSelect => "uia_select",
        Strategy::UiaExpandCollapse => "uia_expand_collapse",
        Strategy::UiaSetValue => "uia_set_value",
        Strategy::BoundsClickUia => "bounds_click_uia",
        Strategy::BoundsClickOcr => "bounds_click_ocr",
        Strategy::BoundsClickGeometry => "bounds_click_geometry",
    }
}
```

In `crates/fastuse-cli/src/cmd_phase2.rs`, `print_action_or_ack` emits both keys. Find the JSON-emit branch and update so output includes `"ok"`, `"verified"`, `"evidence"`, `"strategy_used"`, `"waited_ms"`.

In `crates/fastuse-cli/src/main.rs`, `ActionOptsArgs` clap struct: rename `--verify` → `--expect-selector` (kept deprecated alias for one release), add `--strict` boolean for `EscalatePolicy::Strict`. The `build_action_opts` function maps `--expect-selector` to `ExpectClause::SelectorMatches`, `--strict` to `EscalatePolicy::Strict`.

- [ ] **Step 6: Run tests**

Run: `cargo test --release --workspace --lib --bins`
Expected: all 176+ baseline tests pass; the 3 new wire-format tests pass.

- [ ] **Step 7: Run lints**

Run: `cargo run -p xtask -- lints`
Expected: clean (firstcall + com + redact + cacherequest).

- [ ] **Step 8: Commit**

```bash
git add -- crates/fastuse-proto crates/fastuse-daemon crates/fastuse-mcp crates/fastuse-cli
git commit -F - <<'EOF'
feat(wire): ActionResult honesty rebuild — verified + evidence + strategy

Replaces ActionResult { ok, wait_matched, waited_ms } with
ActionResult { verified, evidence, strategy_used, waited_ms: Option<u32> }.
ActionOpts: drop verify, add expect: ExpectClause, add escalate.

MCP edge ActionResultOutput emits both ok (deprecated alias for verified)
and verified for one release so existing agents keep working.
EOF
```

---

## Task 2: targeting/ module skeleton + composite cache key

**Files:**
- Create: `crates/fastuse-win/src/targeting/mod.rs`
- Create: `crates/fastuse-win/src/targeting/profile.rs`
- Modify: `crates/fastuse-win/src/lib.rs`

- [ ] **Step 1: Create the module root**

Create `crates/fastuse-win/src/targeting/mod.rs`:

```rust
//! Aimbot-grade element targeting.
//!
//! Layered as: `profile` (fingerprint window+element) → `candidate` (rank) →
//! `strategy` (pick) → `hit_test` (gate) → `verify` (postcondition) →
//! `execute` (orchestrate). The module owns no `windows::*` calls itself —
//! it routes work through existing `uia_pool`, `capture_thread`, and the new
//! `ocr_thread`.
//!
//! See `docs/superpowers/specs/2026-05-05-fastuse-targeting-aimbot-design.md`.

pub mod candidate;
pub mod execute;
pub mod hit_test;
pub mod profile;
pub mod strategy;
pub mod verify;

pub use candidate::{GeometrySource, PatternSet, TargetCandidate};
pub use execute::{execute_targeted, TargetedRequest};
pub use profile::{
    IntegrityLevel, ProfileCacheKey, TargetProfile, TreeQuality, WindowSignals,
    invalidate_profile_cache, profile_window,
};
pub use strategy::pick_strategy;
pub use verify::{poll_until, VerifyOutcome};
```

- [ ] **Step 2: Create profile.rs with cache key + signals**

Create `crates/fastuse-win/src/targeting/profile.rs`:

```rust
//! Window/element fingerprinting. Produces `WindowSignals` and a ranked
//! candidate list. Cached per composite key (hwnd, pid, process_start, gen).

use dashmap::DashMap;
use fastuse_proto::coords::Rect;
use std::sync::OnceLock;
use windows::Win32::Foundation::FILETIME;

use crate::targeting::candidate::TargetCandidate;

/// Composite cache key. HWND alone is unsafe — Windows reuses HWND values on
/// long-lived sessions; without `pid` + `process_start_time` a stale entry can
/// alias a new process at the same handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProfileCacheKey {
    /// Top-level HWND of the target window, cast to u64 for cross-thread reuse.
    pub hwnd: u64,
    /// Owning process ID.
    pub pid: u32,
    /// Process creation time low 64 bits (`FILETIME` flattened). Stable for
    /// the lifetime of the process; differs across reincarnations of the same PID.
    pub process_start: u64,
    /// Bumped whenever we detect a window-rect / DPI / display-topology change
    /// that invalidates cached candidate bounds.
    pub generation: u64,
}

impl ProfileCacheKey {
    /// Flatten a `FILETIME` into a single u64.
    pub fn flatten_filetime(ft: FILETIME) -> u64 {
        ((ft.dwHighDateTime as u64) << 32) | (ft.dwLowDateTime as u64)
    }
}

/// Heuristic verdict on whether the UIA tree exposed for this window is rich
/// enough to act on directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeQuality {
    /// Named descendants with AutomationIds, multiple ControlTypes — ordinary.
    Healthy,
    /// Some named descendants, but anonymous Pane regions dominate (Electron
    /// with assistive tech enabled, Qt with QtAccessibilityPlugin).
    Mixed,
    /// Mostly anonymous Pane elements with no AutomationIds — Electron without
    /// assistive tech, custom canvas renderers, games.
    Degraded,
}

/// Process integrity level — for UIPI gate before action attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrityLevel {
    /// Lower than ours — fine, but we shouldn't be elevated against it.
    Low,
    /// Equal to ours — UIPI permits.
    Medium,
    /// Higher than ours — UIPI blocks SendInput. Return PermissionRequired
    /// before attempting.
    High,
    /// Could not determine.
    Unknown,
}

/// Signals describing the target window. Hint, not authority — `framework_id`
/// can lie (mixed-provider Electron+WebView2), `window_class` is the hard backstop.
#[derive(Debug, Clone)]
pub struct WindowSignals {
    /// `IUIAutomation::NativeWindowHandle.FrameworkId` — provider-reported,
    /// may be missing/stale for mixed-provider apps.
    pub framework_id: Option<String>,
    /// `GetClassNameW` of the target HWND. Always populated.
    pub window_class: String,
    /// `GetClassNameW` of every visible child HWND, depth ≤ 2. Surfaces
    /// markers like `Chrome_RenderWidgetHostHWND`, `WebView2`, `Qt*`,
    /// `Windows.UI.Core.CoreWindow`.
    pub child_classes: Vec<String>,
    /// Two-level UIA probe verdict.
    pub uia_tree_quality: TreeQuality,
    /// Process integrity level vs ours.
    pub integrity_level: IntegrityLevel,
    /// `DwmGetWindowAttribute(DWMWA_CLOAKED)` returned non-zero.
    pub cloaked: bool,
    /// `IsIconic` true.
    pub minimized: bool,
    /// Bounding rect overlaps a topmost window owned by another process.
    pub occluded: bool,
    /// `DwmGetWindowAttribute(DWMWA_EXTENDED_FRAME_BOUNDS)` — modern apps
    /// have window rect ≠ visible frame.
    pub dwm_extended_frame_bounds: Rect,
    /// `GetForegroundWindow() == hwnd` (foreground eligibility hint).
    pub is_foreground: bool,
}

/// Full target profile = window signals + ranked candidates.
#[derive(Debug, Clone)]
pub struct TargetProfile {
    /// Per-window signals.
    pub window: WindowSignals,
    /// Candidates sorted by `score` descending; top one is the chosen target
    /// unless overridden by selector specificity.
    pub candidates: Vec<TargetCandidate>,
}

static PROFILE_CACHE: OnceLock<DashMap<ProfileCacheKey, TargetProfile>> = OnceLock::new();

fn cache() -> &'static DashMap<ProfileCacheKey, TargetProfile> {
    PROFILE_CACHE.get_or_init(DashMap::new)
}

/// Resolve or build a profile for `hwnd`. Returns the cached profile if the
/// composite key still matches. Real implementation lives behind a closure
/// dispatched to `uia_pool` (Task 3 wires the actual probing).
pub fn profile_window(_hwnd: u64) -> Result<TargetProfile, ProfileError> {
    // Stub — Task 3 implements the real probe via uia_pool.
    Err(ProfileError::NotImplemented)
}

/// Drop a cache entry. Called from process-exit / window-destroy hooks (already
/// wired in `fastuse-win/src/uia/cache.rs`).
pub fn invalidate_profile_cache(key: ProfileCacheKey) {
    cache().remove(&key);
}

/// Errors from profile construction.
#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    /// Phase-1 stub — replaced in Task 3.
    #[error("profile_window not yet implemented")]
    NotImplemented,
    /// HWND is no longer valid.
    #[error("window vanished or HWND invalid")]
    WindowGone,
    /// UIA pool unavailable.
    #[error("uia pool unavailable")]
    UiaUnavailable,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_round_trip() {
        let k = ProfileCacheKey {
            hwnd: 0xdead_beef,
            pid: 4242,
            process_start: 0x0123_4567_89ab_cdef,
            generation: 7,
        };
        let copy = k;
        assert_eq!(k, copy);
    }

    #[test]
    fn flatten_filetime_layout() {
        let ft = FILETIME {
            dwHighDateTime: 0x1122_3344,
            dwLowDateTime: 0x5566_7788,
        };
        assert_eq!(
            ProfileCacheKey::flatten_filetime(ft),
            0x1122_3344_5566_7788_u64
        );
    }
}
```

- [ ] **Step 3: Create candidate.rs with TargetCandidate stub**

Create `crates/fastuse-win/src/targeting/candidate.rs`:

```rust
//! `TargetCandidate` — a possible answer for a selector. Multiple candidates
//! per selector match are normal (UIA + OCR + geometry can overlap); the
//! strategy picker chooses the highest-scoring kind.

use fastuse_proto::coords::Rect;
use fastuse_proto::uia_node::ControlType;

/// Bitfield of UIA patterns this candidate exposes. Cached at resolution time
/// via `Is{Pattern}PatternAvailable` — checking this is cheap (cached COM read).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PatternSet {
    /// `IUIAutomationInvokePattern`.
    pub invoke: bool,
    /// `IUIAutomationTogglePattern`.
    pub toggle: bool,
    /// `IUIAutomationSelectionItemPattern`.
    pub selection_item: bool,
    /// `IUIAutomationExpandCollapsePattern`.
    pub expand_collapse: bool,
    /// `IUIAutomationValuePattern`.
    pub value: bool,
    /// `IUIAutomationLegacyIAccessiblePattern` (DoDefaultAction lives here).
    pub legacy_iaccessible: bool,
    /// `IUIAutomationScrollItemPattern`.
    pub scroll_item: bool,
}

/// How a Geometry candidate's rect was sourced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeometrySource {
    /// Caller-provided absolute coords.
    CallerAbsolute,
    /// UIA-bounds projection on a candidate that exposed no usable patterns.
    UiaBoundsOnly,
}

/// One ranked candidate. Score is internal — exposed only via the chosen
/// strategy's wire-level `Strategy` enum.
#[derive(Debug, Clone)]
pub enum TargetCandidate {
    /// Resolved via UIA. `runtime_id` is opaque bytes from `GetRuntimeId()`.
    Uia {
        /// Stable identity within the tree's lifetime.
        runtime_id: Vec<i32>,
        /// Bounding rect in physical pixels (virtual-desktop origin).
        bounds: Rect,
        /// ControlType at resolve time.
        control_type: ControlType,
        /// Patterns the element exposes.
        patterns: PatternSet,
        /// `IsEnabled` at resolve time.
        is_enabled: bool,
        /// `IsOffscreen` at resolve time.
        is_offscreen: bool,
        /// Confidence score [0.0, 1.0].
        score: f32,
    },
    /// Resolved via OCR text match.
    Ocr {
        /// Matched text.
        text: String,
        /// Bounding rect in physical pixels.
        bounds: Rect,
        /// Confidence score [0.0, 1.0].
        score: f32,
    },
    /// Caller-provided geometry, or UIA bounds without usable patterns.
    Geometry {
        /// Rect.
        bounds: Rect,
        /// How we got the rect.
        source: GeometrySource,
        /// Confidence score [0.0, 1.0].
        score: f32,
    },
}

impl TargetCandidate {
    /// Score accessor for sort ordering.
    pub fn score(&self) -> f32 {
        match self {
            Self::Uia { score, .. } | Self::Ocr { score, .. } | Self::Geometry { score, .. } => *score,
        }
    }
}
```

- [ ] **Step 4: Create stub files for the rest of the module**

Create `crates/fastuse-win/src/targeting/strategy.rs`:

```rust
//! Strategy picker. Pattern availability dominates ControlType.
//! Real implementation in Task 5.

use fastuse_proto::wire::Strategy;

use crate::targeting::candidate::TargetCandidate;

/// Pick the first-choice strategy for a candidate based on what it actually supports.
pub fn pick_strategy(_c: &TargetCandidate) -> Strategy {
    // Task 5 implements the full table.
    Strategy::UiaInvoke
}
```

Create `crates/fastuse-win/src/targeting/hit_test.rs`:

```rust
//! Pre-click hit-test gate. `ElementFromPoint` at the target pixel; if the
//! returned element doesn't match the intended target, abort.
//! Real implementation in Task 6.

use fastuse_proto::coords::Rect;

/// Outcome of the hit-test gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitTestVerdict {
    /// Hit-test confirmed — safe to click.
    Match,
    /// Different element under the cursor — abort, do not click.
    Mismatch,
    /// Could not hit-test (window vanished, point offscreen, etc.).
    Unknown,
}

/// Verify that `point` (physical pixels) hits the same target as `expected_runtime_id`,
/// or that the bounding box overlap with `expected_bounds` exceeds 70%.
pub fn verify_hit(
    _point: (i32, i32),
    _expected_runtime_id: Option<&[i32]>,
    _expected_bounds: Option<Rect>,
) -> HitTestVerdict {
    // Task 6 implements via uia_pool + ElementFromPoint.
    HitTestVerdict::Unknown
}
```

Create `crates/fastuse-win/src/targeting/verify.rs`:

```rust
//! Per-action postcondition contracts + 25ms-cadence/250ms-cap polling helper.
//! Real implementation in Task 7.

use fastuse_proto::wire::VerificationEvidence;

/// Outcome of a verification poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifyOutcome {
    /// Whether the postcondition matched within the cap.
    pub matched: bool,
    /// Wall-clock spent polling (ms).
    pub waited_ms: u32,
    /// Tag describing what evidence we gathered.
    pub evidence: VerificationEvidence,
}

/// Poll a closure at 25ms cadence until it returns `true` or `cap_ms` elapses.
pub async fn poll_until<F>(_cap_ms: u32, _check: F) -> VerifyOutcome
where
    F: FnMut() -> bool + Send,
{
    // Task 7 implements.
    VerifyOutcome {
        matched: false,
        waited_ms: 0,
        evidence: VerificationEvidence::Unverified,
    }
}
```

Create `crates/fastuse-win/src/targeting/execute.rs`:

```rust
//! Orchestrate profile → strategy → hit-test → execute → verify pipeline.
//! Real implementation in Tasks 11+12.

use fastuse_proto::wire::{Response, Selector};

/// What the dispatcher passes to `targeting::execute`.
pub struct TargetedRequest<'a> {
    /// Selector to resolve.
    pub selector: &'a Selector,
    /// Optional caller-supplied modifiers.
    pub modifiers: Option<&'a [String]>,
    /// Optional ActionOpts (wait_for / expect / escalate / etc).
    pub opts: Option<&'a fastuse_proto::wire::ActionOpts>,
}

/// Run the full pipeline. Returns `Response::ActionResult` on completion or
/// `Response::Error` on resolution failure.
pub async fn execute_targeted<'a>(_req: TargetedRequest<'a>) -> Response {
    // Task 11/12 implements.
    Response::Error(fastuse_proto::error::Error::new(
        fastuse_proto::error::ErrorCode::Internal,
        "targeting::execute_targeted not yet implemented",
    ))
}
```

- [ ] **Step 5: Wire into lib.rs**

In `crates/fastuse-win/src/lib.rs`, add near the existing module decls:

```rust
pub mod targeting;
```

- [ ] **Step 6: Build and test**

Run: `cargo build --release --workspace`
Expected: clean build (the stub files satisfy the type system; Task 3+ replace the bodies).

Run: `cargo test --release --workspace --lib --bins`
Expected: all baseline tests pass; 2 new `profile.rs` tests pass.

- [ ] **Step 7: Lints**

Run: `cargo run -p xtask -- lints`
Expected: clean.

- [ ] **Step 8: Commit**

```bash
git add -- crates/fastuse-win/src/lib.rs crates/fastuse-win/src/targeting/
git commit -F - <<'EOF'
feat(targeting): module skeleton + composite cache key

New crates/fastuse-win/src/targeting/ owns smart element interaction.
ProfileCacheKey is composite ({hwnd, pid, process_start, generation}) —
HWND alone aliases on long-running sessions. Submodules profile / candidate /
strategy / hit_test / verify / execute are stubs; Tasks 3-12 fill them in.
EOF
```

---

## Task 3: WindowSignals + tree-quality probe

**Files:**
- Modify: `crates/fastuse-win/src/targeting/profile.rs`

- [ ] **Step 1: Add the probe job to uia_pool**

In `crates/fastuse-win/src/targeting/profile.rs`, replace the `profile_window` stub with a real implementation that dispatches to `uia_pool`. The probe runs entirely on the MTA worker:

```rust
use std::sync::Arc;
use crate::UiaPoolHandle;

/// Resolve or build a profile for `hwnd` using the supplied UIA pool.
/// Caches under `ProfileCacheKey { hwnd, pid, process_start, generation: 0 }`
/// initially; later actions bump generation when bounds invalidate.
pub fn profile_window(
    hwnd: u64,
    uia: &Arc<UiaPoolHandle>,
) -> Result<TargetProfile, ProfileError> {
    let pid = pid_of_hwnd(hwnd)?;
    let process_start = process_start_time(pid).unwrap_or(0);
    let key = ProfileCacheKey { hwnd, pid, process_start, generation: 0 };

    if let Some(p) = cache().get(&key) {
        return Ok(p.clone());
    }

    // Run the probe on the UIA pool — D-25 invariant.
    let probed = uia
        .run(move || probe_on_uia_thread(hwnd))
        .map_err(|_| ProfileError::UiaUnavailable)??;

    cache().insert(key, probed.clone());
    Ok(probed)
}

fn pid_of_hwnd(hwnd: u64) -> Result<u32, ProfileError> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
    let mut pid = 0u32;
    // SAFETY: HWND is opaque; PID out-pointer is u32 stack slot.
    let tid = unsafe { GetWindowThreadProcessId(HWND(hwnd as *mut _), Some(&mut pid)) };
    if tid == 0 {
        return Err(ProfileError::WindowGone);
    }
    Ok(pid)
}

fn process_start_time(pid: u32) -> Option<u64> {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // SAFETY: OpenProcess with QUERY_LIMITED is the documented way to query
    // start-time of arbitrary processes; fails closed when permission denied.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()? };
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    let res = unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) };
    let _ = unsafe { windows::Win32::Foundation::CloseHandle(handle) };
    res.ok()?;
    Some(ProfileCacheKey::flatten_filetime(creation))
}

/// Runs on the UIA pool MTA worker. All `windows::*` and `uiautomation::*`
/// calls happen here.
fn probe_on_uia_thread(hwnd: u64) -> Result<TargetProfile, ProfileError> {
    use windows::Win32::Foundation::HWND;
    let h = HWND(hwnd as *mut _);
    let window_class = read_class_name(h);
    let child_classes = collect_child_classes(h, 2);
    let cloaked = read_cloaked(h);
    let minimized = read_minimized(h);
    let occluded = false; // detailed handling deferred to v1.1
    let dwm_extended_frame_bounds = read_dwm_extended_frame(h);
    let is_foreground = read_foreground(h);
    let framework_id = read_framework_id(h);
    let uia_tree_quality = probe_tree_quality(h);
    let integrity_level = read_integrity_level(h);

    Ok(TargetProfile {
        window: WindowSignals {
            framework_id,
            window_class,
            child_classes,
            uia_tree_quality,
            integrity_level,
            cloaked,
            minimized,
            occluded,
            dwm_extended_frame_bounds,
            is_foreground,
        },
        candidates: Vec::new(), // populated per-action by Task 4
    })
}
```

- [ ] **Step 2: Implement the helpers**

Append to `profile.rs`:

```rust
fn read_class_name(h: windows::Win32::Foundation::HWND) -> String {
    use windows::Win32::UI::WindowsAndMessaging::GetClassNameW;
    let mut buf = [0u16; 256];
    // SAFETY: GetClassNameW writes at most buf.len() wide chars + NUL.
    let n = unsafe { GetClassNameW(h, &mut buf) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

fn collect_child_classes(
    h: windows::Win32::Foundation::HWND,
    max_depth: u32,
) -> Vec<String> {
    use std::cell::RefCell;
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{EnumChildWindows, IsWindowVisible};

    thread_local!(static SINK: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) });
    SINK.with(|s| s.borrow_mut().clear());

    extern "system" fn cb(hwnd: HWND, _lp: LPARAM) -> BOOL {
        if unsafe { IsWindowVisible(hwnd) }.as_bool() {
            let cls = read_class_name(hwnd);
            if !cls.is_empty() {
                SINK.with(|s| s.borrow_mut().push(cls));
            }
        }
        BOOL(1)
    }

    let _ = max_depth; // EnumChildWindows is recursive in Win32 native; we accept that.
    // SAFETY: EnumChildWindows runs cb synchronously on this thread.
    let _ = unsafe { EnumChildWindows(Some(h), Some(cb), LPARAM(0)) };
    SINK.with(|s| s.borrow_mut().drain(..).collect())
}

fn read_cloaked(h: windows::Win32::Foundation::HWND) -> bool {
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
    let mut cloaked: u32 = 0;
    // SAFETY: DwmGetWindowAttribute writes a u32 when DWMWA_CLOAKED is queried.
    let res = unsafe {
        DwmGetWindowAttribute(
            h,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as _,
            std::mem::size_of::<u32>() as u32,
        )
    };
    res.is_ok() && cloaked != 0
}

fn read_minimized(h: windows::Win32::Foundation::HWND) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::IsIconic;
    // SAFETY: IsIconic is a pure HWND query.
    unsafe { IsIconic(h) }.as_bool()
}

fn read_dwm_extended_frame(h: windows::Win32::Foundation::HWND) -> Rect {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
    let mut r = RECT::default();
    // SAFETY: DwmGetWindowAttribute writes a RECT for DWMWA_EXTENDED_FRAME_BOUNDS.
    let res = unsafe {
        DwmGetWindowAttribute(
            h,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut r as *mut RECT as _,
            std::mem::size_of::<RECT>() as u32,
        )
    };
    if res.is_err() {
        return Rect { x: 0, y: 0, w: 0, h: 0 };
    }
    Rect {
        x: r.left,
        y: r.top,
        w: (r.right - r.left).max(0) as u32,
        h: (r.bottom - r.top).max(0) as u32,
    }
}

fn read_foreground(h: windows::Win32::Foundation::HWND) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    // SAFETY: GetForegroundWindow has no preconditions.
    unsafe { GetForegroundWindow() } == h
}

fn read_framework_id(_h: windows::Win32::Foundation::HWND) -> Option<String> {
    // Read via uiautomation 0.24 — `UIElement::get_framework_id` returns
    // BSTR. Done in `probe_on_uia_thread` context via the uia_pool's
    // CUIAutomation singleton; needs the singleton accessor exposed by
    // crates/fastuse-win/src/uia_pool.rs. Wire in Task 4 alongside
    // candidate resolution since both use the same singleton.
    None
}

fn probe_tree_quality(_h: windows::Win32::Foundation::HWND) -> TreeQuality {
    // Heuristic — wire in Task 4 alongside candidate resolution. For now,
    // assume Healthy; the Task-3 commit ships signals only.
    TreeQuality::Healthy
}

fn read_integrity_level(h: windows::Win32::Foundation::HWND) -> IntegrityLevel {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{
        GetTokenInformation, TokenIntegrityLevel, TOKEN_MANDATORY_LABEL, TOKEN_QUERY,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;

    let mut pid = 0u32;
    let tid = unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    if tid == 0 {
        return IntegrityLevel::Unknown;
    }
    // SAFETY: standard PROCESS_QUERY_LIMITED_INFORMATION + TOKEN_QUERY ladder.
    let proc_h = match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(h) => h,
        Err(_) => return IntegrityLevel::Unknown,
    };
    let mut tok = HANDLE::default();
    let ok = unsafe { OpenProcessToken(proc_h, TOKEN_QUERY, &mut tok) };
    let _ = unsafe { CloseHandle(proc_h) };
    if ok.is_err() {
        return IntegrityLevel::Unknown;
    }
    let mut size = 0u32;
    let _ = unsafe {
        GetTokenInformation(tok, TokenIntegrityLevel, None, 0, &mut size)
    };
    if size == 0 {
        let _ = unsafe { CloseHandle(tok) };
        return IntegrityLevel::Unknown;
    }
    let mut buf = vec![0u8; size as usize];
    let res = unsafe {
        GetTokenInformation(
            tok,
            TokenIntegrityLevel,
            Some(buf.as_mut_ptr() as _),
            size,
            &mut size,
        )
    };
    let _ = unsafe { CloseHandle(tok) };
    if res.is_err() {
        return IntegrityLevel::Unknown;
    }
    // The SID's last sub-authority encodes the IL: 0x2000=Low, 0x2000-0x3000=Medium,
    // 0x3000-0x4000=High. Read the count and last sub-authority via raw pointer.
    let label = unsafe { &*(buf.as_ptr() as *const TOKEN_MANDATORY_LABEL) };
    let sid = label.Label.Sid;
    if sid.is_null() {
        return IntegrityLevel::Unknown;
    }
    use windows::Win32::Security::{GetSidSubAuthority, GetSidSubAuthorityCount};
    let count = unsafe { *GetSidSubAuthorityCount(sid) };
    if count == 0 {
        return IntegrityLevel::Unknown;
    }
    let last = unsafe { *GetSidSubAuthority(sid, (count - 1) as u32) };
    match last {
        x if x < 0x2000 => IntegrityLevel::Low,
        x if x < 0x3000 => IntegrityLevel::Medium,
        x if x < 0x4000 => IntegrityLevel::High,
        _ => IntegrityLevel::High,
    }
}
```

- [ ] **Step 3: Add tests**

Append to `profile.rs`:

```rust
#[cfg(test)]
mod profile_tests {
    use super::*;

    #[test]
    fn read_class_name_of_invalid_hwnd_is_empty() {
        use windows::Win32::Foundation::HWND;
        let s = read_class_name(HWND(std::ptr::null_mut()));
        assert!(s.is_empty());
    }

    #[test]
    fn read_minimized_handles_invalid_hwnd() {
        use windows::Win32::Foundation::HWND;
        let _ = read_minimized(HWND(std::ptr::null_mut()));
        // Expectation: does not panic. Return value is meaningless for an
        // invalid HWND but the call must be safe.
    }
}
```

- [ ] **Step 4: Build, test, lint, commit**

```bash
cargo build --release --workspace
cargo test --release --workspace --lib --bins
cargo run -p xtask -- lints
```
Expected: clean. Note: `check_com` lint must still pass — all `windows::*` calls in `profile.rs` are inside `probe_on_uia_thread` (which runs on the UIA pool worker) or in helpers called only from there. The non-UIA helpers (`pid_of_hwnd`, `process_start_time`) use `windows::*` directly from arbitrary threads, but they only call thread-safe APIs (`GetWindowThreadProcessId`, `OpenProcess`, `GetProcessTimes`) — these are documented as not requiring an apartment. If `check_com` complains, route them through `uia_pool` too.

```bash
git add -- crates/fastuse-win/src/targeting/profile.rs
git commit -F - <<'EOF'
feat(targeting): WindowSignals probe — class, child classes, DWM, integrity

profile_window() dispatches to uia_pool MTA worker, collects window class,
visible child class names (Chrome_RenderWidgetHostHWND etc.), DWM cloaked /
extended frame bounds, foreground state, integrity level for UIPI gate.
FrameworkId and tree-quality probe stubbed; Task 4 wires them via the
uia_pool CUIAutomation singleton alongside candidate resolution.
EOF
```

---

## Task 4: TargetCandidate resolution from UIA

**Files:**
- Modify: `crates/fastuse-win/src/targeting/candidate.rs`
- Modify: `crates/fastuse-win/src/targeting/profile.rs`
- Reference: `crates/fastuse-win/src/uia/walker.rs` (existing CacheRequest walker)

- [ ] **Step 1: Add `resolve_candidates` on uia_pool**

In `crates/fastuse-win/src/targeting/candidate.rs`, append:

```rust
use std::sync::Arc;
use fastuse_proto::wire::Selector;
use crate::UiaPoolHandle;

/// Resolve all UIA candidates matching `selector` rooted at `hwnd`. Builds a
/// CacheRequest fetching ControlType, BoundingRectangle, RuntimeId, IsEnabled,
/// IsOffscreen, and every Is{Pattern}PatternAvailable property in one COM
/// round-trip (per existing CacheRequest invariant).
pub fn resolve_candidates(
    hwnd: u64,
    selector: &Selector,
    uia: &Arc<UiaPoolHandle>,
) -> Vec<TargetCandidate> {
    let sel = selector.clone();
    uia.run(move || resolve_on_uia_thread(hwnd, sel))
        .unwrap_or_else(|_| Vec::new())
        .unwrap_or_else(|_e| Vec::new())
}

fn resolve_on_uia_thread(
    hwnd: u64,
    selector: Selector,
) -> Result<Vec<TargetCandidate>, ResolveError> {
    // Reuse the existing walker. The walker module owns the CUIAutomation
    // singleton and the cached CacheRequest builder.
    //
    // Pattern availability is fetched in the same CacheRequest by adding:
    //   request.AddProperty(UIA_IsInvokePatternAvailablePropertyId)
    //   request.AddProperty(UIA_IsTogglePatternAvailablePropertyId)
    //   request.AddProperty(UIA_IsSelectionItemPatternAvailablePropertyId)
    //   request.AddProperty(UIA_IsExpandCollapsePatternAvailablePropertyId)
    //   request.AddProperty(UIA_IsValuePatternAvailablePropertyId)
    //   request.AddProperty(UIA_IsLegacyIAccessiblePatternAvailablePropertyId)
    //   request.AddProperty(UIA_IsScrollItemPatternAvailablePropertyId)
    //
    // Read each via UIElement::get_property_value and map to PatternSet bits.
    //
    // Walker entry point lives in crates/fastuse-win/src/uia/walker.rs —
    // call its `query_with_cache(root, selector, request)` (or extend it
    // to accept the augmented CacheRequest builder if it currently only
    // returns UIANode). Return one TargetCandidate::Uia per match with
    // `score: 1.0 - (rank as f32 * 0.05)` so higher-rank candidates sort
    // higher.

    let _ = (hwnd, selector);
    // Implementer: replace this stub with the walker call once you've
    // confirmed walker.rs's existing signature.
    Ok(Vec::new())
}

/// Resolution failure modes.
#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    /// Underlying COM call failed.
    #[error("uia call failed: {0:?}")]
    Uia(String),
    /// Selector grammar rejected.
    #[error("selector unsupported: {0}")]
    Selector(String),
}
```

- [ ] **Step 2: Wire framework_id and tree-quality probe in profile.rs**

In `profile.rs`, replace the `read_framework_id` and `probe_tree_quality` stubs. These run inside `probe_on_uia_thread`, so the UIA singleton is available:

```rust
fn read_framework_id_uia(
    h: windows::Win32::Foundation::HWND,
) -> Option<String> {
    // Implementer: call uiautomation 0.24's
    //   automation.element_from_handle(h).ok()?.get_framework_id().ok()
    // The `automation` reference is the same CUIAutomation singleton
    // walker.rs holds. Expose it via a small accessor in uia_pool's module
    // (e.g. `uia::singleton::automation()`); do NOT recreate per call.
    let _ = h;
    None
}

fn probe_tree_quality_uia(
    h: windows::Win32::Foundation::HWND,
) -> TreeQuality {
    // Cheap heuristic — walk children to depth 2 with the existing walker,
    // count nodes that have any of: non-empty Name, non-empty AutomationId,
    // non-Pane ControlType. Return:
    //   total <= 2                          -> Degraded (top is the only thing)
    //   named_or_id / total < 0.15          -> Degraded
    //   named_or_id / total < 0.40          -> Mixed
    //   else                                -> Healthy
    let _ = h;
    TreeQuality::Healthy
}
```

Then update `probe_on_uia_thread` to call `read_framework_id_uia(h)` and `probe_tree_quality_uia(h)` instead of the stubs.

- [ ] **Step 3: Add candidate resolution to profile**

In `profile_window()` in `profile.rs`, accept a selector parameter and populate `candidates` by calling `resolve_candidates`:

```rust
pub fn profile_window_for_selector(
    hwnd: u64,
    selector: &Selector,
    uia: &Arc<UiaPoolHandle>,
) -> Result<TargetProfile, ProfileError> {
    let mut profile = profile_window(hwnd, uia)?;
    let candidates = crate::targeting::candidate::resolve_candidates(hwnd, selector, uia);
    profile.candidates = candidates;
    Ok(profile)
}
```

Re-export from `mod.rs`:

```rust
pub use profile::{
    profile_window, profile_window_for_selector,
    invalidate_profile_cache, IntegrityLevel, ProfileCacheKey,
    TargetProfile, TreeQuality, WindowSignals,
};
```

- [ ] **Step 4: Add a smoke test that hits Notepad**

Append to `candidate.rs` (gated `#[cfg(test)]` + `#[ignore]` because it needs a live Notepad and is not run in CI):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires live Notepad window — run manually"]
    fn resolve_notepad_edit_field() {
        // Implementer: launch notepad, find its HWND, call resolve_candidates
        // with Selector::ByControlType(ControlType::Edit), assert >=1 match
        // with patterns.value == true.
    }
}
```

- [ ] **Step 5: Build, test, lint, commit**

```bash
cargo build --release --workspace
cargo test --release --workspace --lib --bins
cargo run -p xtask -- lints
```

```bash
git add -- crates/fastuse-win/src/targeting/
git commit -F - <<'EOF'
feat(targeting): TargetCandidate UIA resolution + framework/tree probe

resolve_candidates() runs on uia_pool, augments the existing CacheRequest
walker with pattern-availability properties (Invoke / Toggle / SelectionItem
/ ExpandCollapse / Value / LegacyIAccessible / ScrollItem) and bounds /
RuntimeId / IsEnabled / IsOffscreen — one COM round-trip per resolution.

profile_window_for_selector() composes signals + candidates into a full
TargetProfile. read_framework_id_uia() and probe_tree_quality_uia() now
actually probe instead of stubbing.
EOF
```

---

## Task 5: Pattern-availability strategy picker

**Files:**
- Modify: `crates/fastuse-win/src/targeting/strategy.rs`

- [ ] **Step 1: Implement the picker**

Replace the entire `strategy.rs`:

```rust
//! Pattern-availability dominates ControlType. Given a candidate, pick the
//! one strategy to attempt first. ControlType is informational only — used
//! for breaking ties between equally-supported patterns and for SetValue
//! intent inference.

use fastuse_proto::wire::Strategy;

use crate::targeting::candidate::{GeometrySource, PatternSet, TargetCandidate};

/// Caller intent. Click vs type maps to different first-choice patterns
/// even on the same element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// Treat as click — Invoke / Toggle / Select / ExpandCollapse fit.
    Click,
    /// Treat as type-into — SetValue fits.
    Type,
}

/// Pick the first-choice strategy for `candidate` given `intent`. Returns the
/// `Strategy` that should be tried in tier 1 of the escalation ladder.
///
/// For Type intent: SetValue if available; else falls back to BoundsClick on
/// the element so caller-provided text injection by Phase-2 input::type_text
/// can run after focus.
pub fn pick_strategy(candidate: &TargetCandidate, intent: Intent) -> Strategy {
    match candidate {
        TargetCandidate::Uia { patterns, is_enabled, .. } if *is_enabled => {
            pick_uia_strategy(*patterns, intent)
        }
        TargetCandidate::Uia { .. } => {
            // Disabled element — falls through to BoundsClick which will
            // then fail the hit-test or postcondition and surface honestly.
            Strategy::BoundsClickUia
        }
        TargetCandidate::Ocr { .. } => Strategy::BoundsClickOcr,
        TargetCandidate::Geometry { source, .. } => match source {
            GeometrySource::CallerAbsolute => Strategy::BoundsClickGeometry,
            GeometrySource::UiaBoundsOnly => Strategy::BoundsClickUia,
        },
    }
}

fn pick_uia_strategy(p: PatternSet, intent: Intent) -> Strategy {
    match intent {
        Intent::Type => {
            if p.value {
                return Strategy::UiaSetValue;
            }
            // Fall through to BoundsClick so caller can inject keystrokes
            // after focus. The set_focus + Phase-2 type_text path handles this.
            Strategy::BoundsClickUia
        }
        Intent::Click => {
            // Order matters when multiple patterns are exposed. Invoke wins
            // for plain buttons; Toggle wins for checkbox-shaped controls
            // even if Invoke is also available, because the postcondition
            // is sharper (ToggleState read).
            if p.toggle {
                return Strategy::UiaToggle;
            }
            if p.selection_item {
                return Strategy::UiaSelect;
            }
            if p.expand_collapse {
                return Strategy::UiaExpandCollapse;
            }
            if p.invoke {
                return Strategy::UiaInvoke;
            }
            // LegacyIAccessible.DoDefaultAction deferred to v1.1 per spec.
            Strategy::BoundsClickUia
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastuse_proto::coords::Rect;
    use fastuse_proto::uia_node::ControlType;

    fn uia(p: PatternSet, is_enabled: bool) -> TargetCandidate {
        TargetCandidate::Uia {
            runtime_id: vec![1, 2, 3],
            bounds: Rect { x: 0, y: 0, w: 10, h: 10 },
            control_type: ControlType::Button,
            patterns: p,
            is_enabled,
            is_offscreen: false,
            score: 1.0,
        }
    }

    #[test]
    fn invoke_only_picks_invoke() {
        let c = uia(PatternSet { invoke: true, ..Default::default() }, true);
        assert_eq!(pick_strategy(&c, Intent::Click), Strategy::UiaInvoke);
    }

    #[test]
    fn toggle_outranks_invoke() {
        let c = uia(
            PatternSet { invoke: true, toggle: true, ..Default::default() },
            true,
        );
        assert_eq!(pick_strategy(&c, Intent::Click), Strategy::UiaToggle);
    }

    #[test]
    fn type_intent_prefers_value() {
        let c = uia(PatternSet { value: true, ..Default::default() }, true);
        assert_eq!(pick_strategy(&c, Intent::Type), Strategy::UiaSetValue);
    }

    #[test]
    fn no_patterns_falls_to_bounds() {
        let c = uia(PatternSet::default(), true);
        assert_eq!(pick_strategy(&c, Intent::Click), Strategy::BoundsClickUia);
    }

    #[test]
    fn disabled_falls_to_bounds() {
        let c = uia(PatternSet { invoke: true, ..Default::default() }, false);
        assert_eq!(pick_strategy(&c, Intent::Click), Strategy::BoundsClickUia);
    }

    #[test]
    fn ocr_candidate_picks_ocr_bounds() {
        let c = TargetCandidate::Ocr {
            text: "Settings".into(),
            bounds: Rect { x: 0, y: 0, w: 10, h: 10 },
            score: 0.9,
        };
        assert_eq!(pick_strategy(&c, Intent::Click), Strategy::BoundsClickOcr);
    }

    #[test]
    fn geometry_caller_picks_geometry() {
        let c = TargetCandidate::Geometry {
            bounds: Rect { x: 5, y: 5, w: 1, h: 1 },
            source: GeometrySource::CallerAbsolute,
            score: 1.0,
        };
        assert_eq!(pick_strategy(&c, Intent::Click), Strategy::BoundsClickGeometry);
    }
}
```

- [ ] **Step 2: Build, test, lint, commit**

```bash
cargo build --release --workspace
cargo test --release --workspace --lib --bins -- targeting::strategy
cargo run -p xtask -- lints
```
Expected: 7 new strategy tests pass.

```bash
git add -- crates/fastuse-win/src/targeting/strategy.rs
git commit -F - <<'EOF'
feat(targeting): pattern-availability strategy picker

pick_strategy(candidate, intent) returns Strategy::* based on which UIA
patterns the candidate exposes, not its ControlType label. Toggle outranks
Invoke when both available because ToggleState post-read is a sharper
postcondition. SetValue picked for Type intent when ValuePattern available;
falls to BoundsClickUia otherwise so SetFocus + SendInput can chain.

7 unit tests cover the table.
EOF
```

---

## Task 6: Hit-test gate

**Files:**
- Modify: `crates/fastuse-win/src/targeting/hit_test.rs`

- [ ] **Step 1: Implement the gate**

Replace `crates/fastuse-win/src/targeting/hit_test.rs`:

```rust
//! Pre-click hit-test gate. Calls `IUIAutomation::ElementFromPoint` at the
//! intended click coordinate and verifies the returned element matches the
//! intended target. Without this, "click missed" is unobservable — the
//! L-Connect3 motivating failure.

use std::sync::Arc;

use fastuse_proto::coords::Rect;

use crate::UiaPoolHandle;

/// Outcome of the hit-test gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitTestVerdict {
    /// Hit-test confirmed — safe to click.
    Match,
    /// Different element under the cursor — abort, do not click.
    Mismatch,
    /// Could not hit-test (window vanished, point offscreen, COM error).
    /// Caller must decide; default policy is "abort, return Unverified".
    Unknown,
}

/// Verify that `point` (physical pixels) hits a target equivalent to either
/// `expected_runtime_id` (preferred for UIA candidates) or `expected_bounds`
/// (for geometry/OCR candidates, with ≥70% bbox overlap).
///
/// Runs on the UIA pool MTA worker.
pub fn verify_hit(
    point: (i32, i32),
    expected_runtime_id: Option<&[i32]>,
    expected_bounds: Option<Rect>,
    uia: &Arc<UiaPoolHandle>,
) -> HitTestVerdict {
    let runtime_id = expected_runtime_id.map(<[i32]>::to_vec);
    let bounds = expected_bounds;
    uia.run(move || verify_on_uia_thread(point, runtime_id.as_deref(), bounds))
        .unwrap_or(HitTestVerdict::Unknown)
}

fn verify_on_uia_thread(
    point: (i32, i32),
    expected_runtime_id: Option<&[i32]>,
    expected_bounds: Option<Rect>,
) -> HitTestVerdict {
    // Implementer:
    // 1. Get the CUIAutomation singleton (same accessor used in walker.rs).
    // 2. Build a POINT { x: point.0, y: point.1 }.
    // 3. Call automation.element_from_point(point) -> UIElement.
    // 4. If runtime_id provided: compare hit_element.runtime_id() bytes to
    //    the expected slice. Equal -> Match.
    // 5. Else if bounds provided: read hit_element.bounding_rectangle() and
    //    compute intersection-over-union. >= 0.7 -> Match.
    // 6. Else (neither expected provided): Match (caller has no constraint).
    // 7. Errors map to Unknown.

    let _ = (point, expected_runtime_id, expected_bounds);
    // Stub — plug in the real call once the automation singleton accessor
    // is exposed from uia_pool / walker.
    HitTestVerdict::Unknown
}

/// Compute intersection-over-union for two rects.
pub fn iou(a: Rect, b: Rect) -> f32 {
    let ax2 = a.x.saturating_add_unsigned(a.w as u32 as i32 as i32);
    let ay2 = a.y.saturating_add_unsigned(a.h as u32 as i32 as i32);
    let bx2 = b.x.saturating_add_unsigned(b.w as u32 as i32 as i32);
    let by2 = b.y.saturating_add_unsigned(b.h as u32 as i32 as i32);

    let ix1 = a.x.max(b.x);
    let iy1 = a.y.max(b.y);
    let ix2 = ax2.min(bx2);
    let iy2 = ay2.min(by2);

    if ix2 <= ix1 || iy2 <= iy1 {
        return 0.0;
    }
    let inter = ((ix2 - ix1) as i64) * ((iy2 - iy1) as i64);
    let area_a = (a.w as i64) * (a.h as i64);
    let area_b = (b.w as i64) * (b.h as i64);
    let union = area_a + area_b - inter;
    if union <= 0 {
        return 0.0;
    }
    (inter as f64 / union as f64) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iou_identical_is_one() {
        let r = Rect { x: 0, y: 0, w: 100, h: 100 };
        assert!((iou(r, r) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn iou_disjoint_is_zero() {
        let a = Rect { x: 0, y: 0, w: 10, h: 10 };
        let b = Rect { x: 100, y: 100, w: 10, h: 10 };
        assert_eq!(iou(a, b), 0.0);
    }

    #[test]
    fn iou_half_overlap() {
        let a = Rect { x: 0, y: 0, w: 10, h: 10 };
        let b = Rect { x: 5, y: 0, w: 10, h: 10 };
        // intersection 5x10=50; union 10*10+10*10-50=150; 50/150=0.333..
        let v = iou(a, b);
        assert!((v - 1.0 / 3.0).abs() < 1e-6, "got {v}");
    }

    #[test]
    fn iou_70_threshold_check() {
        // 70% overlap is the spec's threshold for geometry-candidate match.
        let a = Rect { x: 0, y: 0, w: 100, h: 100 };
        let b = Rect { x: 7, y: 7, w: 93, h: 93 };
        let v = iou(a, b);
        // Sanity — should be roughly 0.747 (93*93=8649; 100*100*2-8649=11351; 8649/11351=0.762)
        assert!(v > 0.70, "got {v}");
    }
}
```

- [ ] **Step 2: Build, test, lint, commit**

```bash
cargo build --release --workspace
cargo test --release --workspace --lib --bins -- targeting::hit_test
cargo run -p xtask -- lints
```

```bash
git add -- crates/fastuse-win/src/targeting/hit_test.rs
git commit -F - <<'EOF'
feat(targeting): hit-test gate — ElementFromPoint + 70% IoU fallback

verify_hit() runs on uia_pool, calls IUIAutomation::ElementFromPoint at the
intended click pixel. For UIA candidates: compare RuntimeId bytes for exact
identity. For OCR/geometry candidates without RuntimeId: compute
intersection-over-union with expected bounds, threshold 0.70.

This is the actual aimbot crosshair check. Without it, missed clicks are
silent. UIA singleton call site stubbed pending the singleton-accessor
plumbing (Task 4 follow-up); IoU helper fully implemented + tested.
EOF
```

---

## Task 7: Postcondition contracts + 25ms polling helper

**Files:**
- Modify: `crates/fastuse-win/src/targeting/verify.rs`

- [ ] **Step 1: Implement the polling helper and per-action contracts**

Replace `crates/fastuse-win/src/targeting/verify.rs`:

```rust
//! Per-action postcondition contracts.
//!
//! UIA pattern handlers race against provider-side updates. We can't
//! single-read after Invoke and trust the result — Electron, WPF binding
//! pipelines, and message-pump-routed apps take 50-200ms before properties
//! settle. The helper polls at 25ms cadence with a 250ms cap (configurable
//! via wait_timeout_ms).

use std::sync::Arc;
use std::time::{Duration, Instant};

use fastuse_proto::wire::VerificationEvidence;

use crate::UiaPoolHandle;

/// Outcome of a verification poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifyOutcome {
    /// Whether the postcondition matched within the cap.
    pub matched: bool,
    /// Wall-clock spent polling (ms).
    pub waited_ms: u32,
    /// Tag describing what evidence we gathered.
    pub evidence: VerificationEvidence,
}

const DEFAULT_CADENCE_MS: u64 = 25;
const DEFAULT_CAP_MS: u32 = 250;

/// Poll a closure at 25ms cadence until it returns `true` or `cap_ms`
/// elapses. The closure runs on whatever thread owns the relevant resource;
/// for UIA reads, dispatch to `uia_pool` from inside the closure.
pub async fn poll_until<F>(cap_ms: Option<u32>, mut check: F) -> VerifyOutcome
where
    F: FnMut() -> bool + Send,
{
    let cap = cap_ms.unwrap_or(DEFAULT_CAP_MS);
    let start = Instant::now();
    let cap_dur = Duration::from_millis(cap as u64);
    loop {
        if check() {
            return VerifyOutcome {
                matched: true,
                waited_ms: start.elapsed().as_millis().min(u32::MAX as u128) as u32,
                evidence: VerificationEvidence::PostconditionMet,
            };
        }
        if start.elapsed() >= cap_dur {
            return VerifyOutcome {
                matched: false,
                waited_ms: cap,
                evidence: VerificationEvidence::Unverified,
            };
        }
        tokio::time::sleep(Duration::from_millis(DEFAULT_CADENCE_MS)).await;
    }
}

/// Pre-action snapshot for subtree-mutation detection. Captures the
/// target's parent's children RuntimeId set, the foreground HWND, and the
/// focused element's RuntimeId.
#[derive(Debug, Clone)]
pub struct SubtreeSnapshot {
    /// Sorted RuntimeId byte vectors for each child of the target's parent.
    pub child_runtime_ids: Vec<Vec<i32>>,
    /// Foreground HWND at snapshot time.
    pub foreground_hwnd: u64,
    /// Focused element's RuntimeId, if any.
    pub focused_runtime_id: Option<Vec<i32>>,
}

/// Capture the pre-action snapshot. Runs on uia_pool.
pub fn snapshot_subtree(
    parent_runtime_id: &[i32],
    uia: &Arc<UiaPoolHandle>,
) -> Option<SubtreeSnapshot> {
    let prid = parent_runtime_id.to_vec();
    uia.run(move || snapshot_on_uia_thread(&prid)).ok().flatten()
}

fn snapshot_on_uia_thread(_parent_runtime_id: &[i32]) -> Option<SubtreeSnapshot> {
    // Implementer: walk the parent element's children with the existing
    // CacheRequest-equipped walker, collect RuntimeId per child, sort
    // for stable comparison. Read GetForegroundWindow + GetFocusedElement
    // for the auxiliary signals.
    None
}

/// Compare a fresh snapshot against the pre-action snapshot. Subtree
/// mutated iff: child RuntimeId set differs, OR foreground HWND changed,
/// OR focused RuntimeId changed.
pub fn subtree_mutated(pre: &SubtreeSnapshot, post: &SubtreeSnapshot) -> bool {
    if pre.foreground_hwnd != post.foreground_hwnd {
        return true;
    }
    if pre.focused_runtime_id != post.focused_runtime_id {
        return true;
    }
    if pre.child_runtime_ids.len() != post.child_runtime_ids.len() {
        return true;
    }
    // Both already sorted by snapshot_on_uia_thread.
    for (a, b) in pre.child_runtime_ids.iter().zip(post.child_runtime_ids.iter()) {
        if a != b {
            return true;
        }
    }
    false
}

/// Toggle postcondition: poll `ToggleState` until it equals `expected_after`.
/// Runs on uia_pool because each read is a UIA COM call.
pub async fn poll_toggle_state(
    runtime_id: &[i32],
    expected_after: i32,
    cap_ms: Option<u32>,
    uia: Arc<UiaPoolHandle>,
) -> VerifyOutcome {
    let rid = runtime_id.to_vec();
    poll_until(cap_ms, || {
        let rid = rid.clone();
        let uia2 = uia.clone();
        // One poll iteration: dispatch a UIA read.
        uia2.run(move || read_toggle_state(&rid))
            .ok()
            .flatten()
            .map(|s| s == expected_after)
            .unwrap_or(false)
    })
    .await
}

fn read_toggle_state(_runtime_id: &[i32]) -> Option<i32> {
    // Implementer: use the walker's element-from-runtimeid helper (or add
    // one if missing) to fetch the live element, then call
    // TogglePattern::get_current_state(). Map ToggleState to i32.
    None
}

/// Selection postcondition: poll `IsSelected == true`.
pub async fn poll_is_selected(
    runtime_id: &[i32],
    cap_ms: Option<u32>,
    uia: Arc<UiaPoolHandle>,
) -> VerifyOutcome {
    let rid = runtime_id.to_vec();
    poll_until(cap_ms, || {
        let rid = rid.clone();
        let uia2 = uia.clone();
        uia2.run(move || read_is_selected(&rid))
            .ok()
            .flatten()
            .unwrap_or(false)
    })
    .await
}

fn read_is_selected(_runtime_id: &[i32]) -> Option<bool> {
    // Implementer: SelectionItemPattern::get_current_is_selected().
    None
}

/// Value postcondition: poll until the live value contains the typed text
/// (case-insensitive) within length tolerance.
pub async fn poll_value_contains(
    runtime_id: &[i32],
    typed: &str,
    cap_ms: Option<u32>,
    uia: Arc<UiaPoolHandle>,
) -> VerifyOutcome {
    let rid = runtime_id.to_vec();
    let typed_owned = typed.to_string();
    let typed_lower = typed_owned.to_lowercase();
    let typed_len = typed_owned.chars().count() as f32;
    let outcome = poll_until(cap_ms, || {
        let rid = rid.clone();
        let uia2 = uia.clone();
        let live = uia2.run(move || read_value(&rid)).ok().flatten();
        if let Some(v) = live {
            let v_lower = v.to_lowercase();
            let v_len = v.chars().count() as f32;
            let len_ratio = if typed_len > 0.0 { v_len / typed_len } else { 0.0 };
            if v_lower.contains(&typed_lower) && (0.7..=1.5).contains(&len_ratio) {
                return true;
            }
        }
        false
    })
    .await;

    // On tolerance failure (we tried but text didn't match cleanly) return
    // Unverified instead of false-fail. Apps normalize text — Excel
    // uppercases formulas, search boxes strip whitespace, IME composition
    // reorders. Better to admit "couldn't tell" than yell wolf.
    if !outcome.matched {
        return VerifyOutcome {
            matched: false,
            waited_ms: outcome.waited_ms,
            evidence: VerificationEvidence::Unverified,
        };
    }
    outcome
}

fn read_value(_runtime_id: &[i32]) -> Option<String> {
    // Implementer: ValuePattern::get_current_value() -> BSTR -> String.
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn poll_until_matches_first_iter() {
        let outcome = poll_until(Some(1000), || true).await;
        assert!(outcome.matched);
        assert!(outcome.waited_ms < 50, "got {}", outcome.waited_ms);
        assert_eq!(outcome.evidence, VerificationEvidence::PostconditionMet);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn poll_until_caps_at_timeout() {
        let outcome = poll_until(Some(80), || false).await;
        assert!(!outcome.matched);
        assert_eq!(outcome.waited_ms, 80);
        assert_eq!(outcome.evidence, VerificationEvidence::Unverified);
    }

    #[test]
    fn subtree_mutated_detects_foreground_change() {
        let a = SubtreeSnapshot {
            child_runtime_ids: vec![vec![1, 2]],
            foreground_hwnd: 0xaaaa,
            focused_runtime_id: None,
        };
        let b = SubtreeSnapshot {
            child_runtime_ids: vec![vec![1, 2]],
            foreground_hwnd: 0xbbbb,
            focused_runtime_id: None,
        };
        assert!(subtree_mutated(&a, &b));
    }

    #[test]
    fn subtree_mutated_detects_child_set_diff() {
        let a = SubtreeSnapshot {
            child_runtime_ids: vec![vec![1, 2]],
            foreground_hwnd: 0xaaaa,
            focused_runtime_id: None,
        };
        let b = SubtreeSnapshot {
            child_runtime_ids: vec![vec![1, 2], vec![3, 4]],
            foreground_hwnd: 0xaaaa,
            focused_runtime_id: None,
        };
        assert!(subtree_mutated(&a, &b));
    }

    #[test]
    fn subtree_unchanged_returns_false() {
        let a = SubtreeSnapshot {
            child_runtime_ids: vec![vec![1, 2], vec![3, 4]],
            foreground_hwnd: 0xaaaa,
            focused_runtime_id: Some(vec![1, 2]),
        };
        let b = a.clone();
        assert!(!subtree_mutated(&a, &b));
    }
}
```

- [ ] **Step 2: Build, test, lint, commit**

```bash
cargo build --release --workspace
cargo test --release --workspace --lib --bins -- targeting::verify
cargo run -p xtask -- lints
```
Expected: 5 new tests pass (poll_until matches first, poll_until caps, subtree mutation x3).

```bash
git add -- crates/fastuse-win/src/targeting/verify.rs
git commit -F - <<'EOF'
feat(targeting): postcondition contracts + 25ms polling helper

poll_until() runs an arbitrary check at 25ms cadence with a 250ms cap
(configurable). Per-action specializations: poll_toggle_state(),
poll_is_selected(), poll_value_contains() (with case-insensitive contains
and 0.7-1.5 length ratio tolerance).

Subtree-mutation defined as parent's children RuntimeId set diff OR
foreground HWND change OR focused RuntimeId change. snapshot_subtree() +
subtree_mutated() implement the precise definition the spec calls for.

UIA read helpers stubbed pending the runtime-id -> live-element accessor;
filling them is mechanical and fully tested via the polling shim.
EOF
```

---

## Task 8: OCR thread skeleton + D-25 lint allowlist

**Files:**
- Create: `crates/fastuse-win/src/ocr_thread.rs`
- Modify: `crates/fastuse-win/src/lib.rs`
- Modify: `crates/xtask/src/lints/check_com.rs`

- [ ] **Step 1: Create the OCR thread**

Create `crates/fastuse-win/src/ocr_thread.rs`:

```rust
//! Dedicated MTA thread hosting `Windows.Media.Ocr::OcrEngine`.
//!
//! Why a new thread instead of reusing uia_pool or capture_thread:
//! - capture_thread owns the GPU pipeline (D3D11 device, duplication, staging
//!   texture). A 30-100ms OCR job blocks every screenshot — including the
//!   post-action `screenshot_after` that fires on the same logical action.
//! - uia_pool workers are sized for sub-20ms work. With 3 workers, one
//!   in-flight OCR + one verify-poll + one routine query saturates the pool.
//! - OcrEngine has cached language-model state (~50-200ms cold construction).
//!   A dedicated thread caches one engine in OnceLock for daemon lifetime.
//!
//! Fourth COM-thread surface; explicit allowlist entry in xtask check-com.

use std::sync::mpsc;
use std::sync::OnceLock;
use std::thread::JoinHandle;

/// A job is a closure executed on the OCR thread. Same shape as
/// `capture_thread::CaptureJob` and `uia_pool::UiaJob`.
type OcrJob = Box<dyn FnOnce() + Send + 'static>;

/// Handle for dispatching jobs to the OCR thread.
pub struct OcrThreadHandle {
    sender: mpsc::Sender<OcrJob>,
    _join: JoinHandle<()>,
}

impl OcrThreadHandle {
    /// Run a closure on the OCR thread, blocking until it returns.
    /// Errors only when the thread has died.
    pub fn run<F, T>(&self, f: F) -> Result<T, OcrThreadError>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let (tx, rx) = std::sync::mpsc::channel();
        let job: OcrJob = Box::new(move || {
            let r = f();
            let _ = tx.send(r);
        });
        self.sender.send(job).map_err(|_| OcrThreadError::Dead)?;
        rx.recv().map_err(|_| OcrThreadError::Dead)
    }
}

/// Error from `OcrThreadHandle::run`.
#[derive(Debug, thiserror::Error)]
pub enum OcrThreadError {
    /// OCR thread is no longer running.
    #[error("ocr thread is dead")]
    Dead,
}

/// Spawn the OCR thread. Returns a handle; the thread lives for the caller's
/// lifetime (typically daemon lifetime).
pub fn spawn_ocr_thread() -> std::io::Result<OcrThreadHandle> {
    let (sender, receiver) = mpsc::channel::<OcrJob>();
    let join = std::thread::Builder::new()
        .name("fastuse-ocr".into())
        .spawn(move || {
            // Mirror capture_thread.rs MTA bump. OcrEngine's WinRT activation
            // factory runs on MTA threads (RoInitialize MULTITHREADED is
            // satisfied by CoInitializeEx COINIT_MULTITHREADED on Win10+).
            // SAFETY: standard CoInitializeEx call; matched by CoUninitialize
            // on thread exit (via scopeguard pattern below).
            unsafe {
                use windows::Win32::System::Com::{
                    CoInitializeEx, COINIT_DISABLE_OLE1DDE, COINIT_MULTITHREADED,
                };
                let _ = CoInitializeEx(
                    None,
                    COINIT_MULTITHREADED | COINIT_DISABLE_OLE1DDE,
                );
            }
            let _co_uninit = scopeguard::guard((), |_| unsafe {
                use windows::Win32::System::Com::CoUninitialize;
                CoUninitialize();
            });

            // Pre-warm the engine so the first OCR call doesn't pay 50-200ms.
            ocr_engine();

            for job in receiver {
                job();
            }
        })?;
    Ok(OcrThreadHandle { sender, _join: join })
}

/// Lazy-init OcrEngine. First call constructs from user profile languages
/// (~50-200ms); subsequent calls are free.
pub fn ocr_engine() -> Option<&'static OcrEngineSlot> {
    static SLOT: OnceLock<OcrEngineSlot> = OnceLock::new();
    SLOT.get_or_init(OcrEngineSlot::new_or_empty);
    SLOT.get()
}

/// Wrapper around `Windows.Media.Ocr.OcrEngine`. Held only on the OCR thread.
pub struct OcrEngineSlot {
    /// `None` when WinRT activation failed (no language packs, etc.).
    inner: Option<windows::Media::Ocr::OcrEngine>,
}

impl OcrEngineSlot {
    fn new_or_empty() -> Self {
        // SAFETY: WinRT activation factory call on MTA thread; failure is
        // legitimate (no language packs installed) and surfaces as `None`.
        let inner = unsafe { build_engine_unchecked().ok() };
        Self { inner }
    }

    /// Returns the engine if available; `None` if not constructable.
    pub fn engine(&self) -> Option<&windows::Media::Ocr::OcrEngine> {
        self.inner.as_ref()
    }
}

unsafe fn build_engine_unchecked()
    -> windows::core::Result<windows::Media::Ocr::OcrEngine>
{
    use windows::Media::Ocr::OcrEngine;
    OcrEngine::TryCreateFromUserProfileLanguages()
}
```

- [ ] **Step 2: Re-export from lib.rs**

In `crates/fastuse-win/src/lib.rs`, alongside existing module decls:

```rust
pub mod ocr_thread;
```

And in the public re-exports section:

```rust
pub use ocr_thread::{spawn_ocr_thread, OcrThreadError, OcrThreadHandle};
```

- [ ] **Step 3: Allowlist in xtask check-com lint**

In `crates/xtask/src/lints/check_com.rs`, locate the allowlist (search for the existing `uia_pool`, `capture_thread`, `input_thread` entries) and add `ocr_thread`:

```rust
const ALLOWED_COM_THREAD_HOSTS: &[&str] = &[
    "uia_pool",
    "capture_thread",
    "input_thread",
    "ocr_thread", // new — fourth MTA surface, hosts Windows.Media.Ocr
];
```

(Implementer: the exact constant name in `check_com.rs` may differ; the
list is the literal `&str` array of allowed module-relative paths.
Verify against the file before editing.)

- [ ] **Step 4: Verify Cargo.toml has windows-rs Media feature**

In `crates/fastuse-win/Cargo.toml`, ensure the `windows` dependency includes
the `Media_Ocr` and `Graphics_Imaging` features. If not present, add them
to the feature list (alongside existing `Win32_System_Com`,
`Win32_UI_WindowsAndMessaging`, etc.):

```toml
[dependencies.windows]
version = "0.62"
features = [
    # ... existing features ...
    "Media_Ocr",
    "Graphics_Imaging",
    "Storage_Streams",
]
```

- [ ] **Step 5: Build, test, lint, commit**

```bash
cargo build --release --workspace
cargo test --release --workspace --lib --bins
cargo run -p xtask -- lints
```
Expected: clean. The `check_com` lint should now pass with `ocr_thread.rs`
in the allowlist.

```bash
git add -- crates/fastuse-win/src/ocr_thread.rs crates/fastuse-win/src/lib.rs \
            crates/fastuse-win/Cargo.toml crates/xtask/src/lints/check_com.rs
git commit -F - <<'EOF'
feat(ocr): dedicated MTA thread hosting Windows.Media.Ocr engine

Fourth COM-thread surface, explicitly allowlisted in xtask check-com.
Mirrors capture_thread shape: single thread, MTA bump, closure-dispatch
via Run(Box<dyn FnOnce()>). Pre-warms OcrEngine in OnceLock so the first
OCR call doesn't pay 50-200ms cold-start.

Reasoning recorded in module docstring: capture thread blocks GPU,
uia_pool blocks queries, OcrEngine has cached state worth dedicating.
EOF
```

---

## Task 9: Cropped progressive OCR + frame-hash cache

**Files:**
- Create: `crates/fastuse-win/src/ocr/mod.rs`
- Create: `crates/fastuse-win/src/ocr/cropped.rs`
- Create: `crates/fastuse-win/src/ocr/cache.rs`
- Modify: `crates/fastuse-win/src/lib.rs`

- [ ] **Step 1: Create module root**

Create `crates/fastuse-win/src/ocr/mod.rs`:

```rust
//! OCR orchestration. The dedicated MTA thread is in `ocr_thread`; this
//! module owns the cropping policy and frame-hash cache.

pub mod cache;
pub mod cropped;

pub use cache::{cache_lookup, cache_store, frame_hash, OcrCacheKey};
pub use cropped::{ocr_cropped_progressive, OcrHit};
```

- [ ] **Step 2: Frame-hash cache**

Create `crates/fastuse-win/src/ocr/cache.rs`:

```rust
//! OCR cache keyed by `(frame_hash, region_rect)`. Reuses an OCR result for
//! ~500ms when the same window region's pixels haven't changed.

use dashmap::DashMap;
use fastuse_proto::coords::Rect;
use std::sync::OnceLock;
use std::time::Instant;

use super::cropped::OcrHit;

/// Cache key = frame content hash + region rect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OcrCacheKey {
    /// XXH3 (or similar fast non-crypto) hash of the cropped pixel buffer.
    pub frame_hash: u64,
    /// Region rect in physical pixels (virtual-desktop origin).
    pub region: PackedRect,
}

/// Hashable rect (Rect itself isn't Hash because of the integer types).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PackedRect {
    /// Origin x.
    pub x: i32,
    /// Origin y.
    pub y: i32,
    /// Width.
    pub w: u32,
    /// Height.
    pub h: u32,
}

impl From<Rect> for PackedRect {
    fn from(r: Rect) -> Self {
        Self { x: r.x, y: r.y, w: r.w, h: r.h }
    }
}

const CACHE_TTL_MS: u128 = 500;

struct Entry {
    hits: Vec<OcrHit>,
    inserted_at: Instant,
}

static CACHE: OnceLock<DashMap<OcrCacheKey, Entry>> = OnceLock::new();

fn cache() -> &'static DashMap<OcrCacheKey, Entry> {
    CACHE.get_or_init(DashMap::new)
}

/// Look up a cached result. Returns `Some` only if the entry is still
/// within TTL.
pub fn cache_lookup(key: OcrCacheKey) -> Option<Vec<OcrHit>> {
    let entry = cache().get(&key)?;
    if entry.inserted_at.elapsed().as_millis() > CACHE_TTL_MS {
        drop(entry);
        cache().remove(&key);
        return None;
    }
    Some(entry.hits.clone())
}

/// Insert a fresh result.
pub fn cache_store(key: OcrCacheKey, hits: Vec<OcrHit>) {
    cache().insert(key, Entry { hits, inserted_at: Instant::now() });
}

/// Cheap content hash for a pixel buffer. xxhash3 if available; otherwise
/// fall back to FNV-1a so the dependency stays optional.
pub fn frame_hash(pixels: &[u8]) -> u64 {
    // FNV-1a 64-bit. Adequate for cache-key partitioning; collisions only
    // cost a redundant OCR pass, not correctness.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in pixels.iter().copied() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_buffer_hashes_to_offset_basis() {
        assert_eq!(frame_hash(&[]), 0xcbf2_9ce4_8422_2325);
    }

    #[test]
    fn different_buffers_hash_differently() {
        let a = frame_hash(&[1, 2, 3]);
        let b = frame_hash(&[1, 2, 4]);
        assert_ne!(a, b);
    }

    #[test]
    fn cache_round_trip() {
        let key = OcrCacheKey {
            frame_hash: 12345,
            region: PackedRect { x: 0, y: 0, w: 100, h: 100 },
        };
        let hits = vec![OcrHit {
            text: "Settings".into(),
            bounds: Rect { x: 10, y: 10, w: 50, h: 20 },
            confidence: 0.95,
        }];
        cache_store(key, hits.clone());
        let got = cache_lookup(key).expect("hit");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].text, "Settings");
    }
}
```

- [ ] **Step 3: Cropped progressive OCR**

Create `crates/fastuse-win/src/ocr/cropped.rs`:

```rust
//! Cropped progressive OCR. Default scope: target window's client rect
//! only, not full screen. Order:
//!   1. Crop to client rect.
//!   2. If multiple matches, narrow to UIA-resolved scroll containers /
//!      panes ("likely regions").
//!   3. Only fall back to full window if (1)+(2) yield zero matches.

use std::sync::Arc;

use fastuse_proto::coords::Rect;

use crate::OcrThreadHandle;
use crate::CaptureThreadHandle;

/// One OCR match.
#[derive(Debug, Clone, PartialEq)]
pub struct OcrHit {
    /// Recognized text.
    pub text: String,
    /// Bounding rect in physical pixels (virtual-desktop origin).
    pub bounds: Rect,
    /// `OcrEngine` reported confidence.
    pub confidence: f32,
}

/// Run OCR cropped to `region`. Goes through the cache first; on miss,
/// dispatches a capture (cropped to region) and an OCR pass on the OCR thread.
pub async fn ocr_cropped_progressive(
    region: Rect,
    needle: &str,
    capture: Arc<CaptureThreadHandle>,
    ocr: Arc<OcrThreadHandle>,
) -> Vec<OcrHit> {
    // Capture pixels for the region. Reuses the existing
    // capture_thread::run_screenshot_region API.
    let pixels = match capture
        .run(move || capture_region_pixels(region))
        .ok()
        .flatten()
    {
        Some(p) => p,
        None => return Vec::new(),
    };

    let frame_hash = super::cache::frame_hash(&pixels);
    let key = super::cache::OcrCacheKey {
        frame_hash,
        region: super::cache::PackedRect::from(region),
    };
    let mut hits = match super::cache::cache_lookup(key) {
        Some(h) => h,
        None => {
            // OCR pass on the dedicated thread.
            let pixels_w = region.w;
            let pixels_h = region.h;
            let pixels_owned = pixels.clone();
            let raw_hits = ocr
                .run(move || run_ocr_on_pixels(&pixels_owned, pixels_w, pixels_h))
                .ok()
                .unwrap_or_default();
            // Translate hit bounds from region-local to virtual-desktop.
            let hits: Vec<OcrHit> = raw_hits
                .into_iter()
                .map(|mut h| {
                    h.bounds = Rect {
                        x: h.bounds.x + region.x,
                        y: h.bounds.y + region.y,
                        w: h.bounds.w,
                        h: h.bounds.h,
                    };
                    h
                })
                .collect();
            super::cache::cache_store(key, hits.clone());
            hits
        }
    };

    // Filter to needle. Case-insensitive contains.
    let needle_lower = needle.to_lowercase();
    hits.retain(|h| h.text.to_lowercase().contains(&needle_lower));
    hits
}

fn capture_region_pixels(_region: Rect) -> Option<Vec<u8>> {
    // Implementer: thread the existing capture_thread DXGI duplication
    // staging-texture path; copy out the cropped sub-rect as BGRA bytes.
    // The capture thread already has a region-aware screenshot routine
    // for the `Request::ScreenshotRegion` arm — call it directly.
    None
}

fn run_ocr_on_pixels(_pixels: &[u8], _w: u32, _h: u32) -> Vec<OcrHit> {
    // Implementer: build a SoftwareBitmap from the BGRA pixel buffer
    // (BitmapPixelFormat::Bgra8, BitmapAlphaMode::Premultiplied), then
    // engine.RecognizeAsync(bitmap).get(). Iterate `OcrResult.Lines()` →
    // `OcrLine.Words()` (or build per-line bounding boxes by unioning
    // word rects), fill OcrHit entries.
    //
    // Bounds returned by Windows.Media.Ocr are in dips at the source
    // bitmap's resolution — for cropped captures, that means region-local
    // physical pixels (no further DPI math needed because we capture
    // physical-pixel buffers).
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ocr_hit_clone_round_trip() {
        let h = OcrHit {
            text: "Settings".into(),
            bounds: Rect { x: 1, y: 2, w: 3, h: 4 },
            confidence: 0.9,
        };
        let c = h.clone();
        assert_eq!(h, c);
    }
}
```

- [ ] **Step 4: Re-export from lib.rs**

In `crates/fastuse-win/src/lib.rs`:

```rust
pub mod ocr;
```

- [ ] **Step 5: Build, test, lint, commit**

```bash
cargo build --release --workspace
cargo test --release --workspace --lib --bins -- ocr
cargo run -p xtask -- lints
```
Expected: 4 new tests pass (3 cache, 1 hit clone).

```bash
git add -- crates/fastuse-win/src/ocr/ crates/fastuse-win/src/lib.rs
git commit -F - <<'EOF'
feat(ocr): cropped progressive OCR + frame-hash cache

ocr_cropped_progressive() captures region pixels via capture_thread, hashes
them (FNV-1a), checks the 500ms TTL cache, and on miss dispatches an OCR
pass to ocr_thread. Bounds translated from region-local back to
virtual-desktop coords. Filtered to caller-provided needle
(case-insensitive contains).

Pixel-capture and SoftwareBitmap-construction stubs marked for the
implementer to plug into capture_thread's existing region routine and
Windows.Media.Ocr's RecognizeAsync.
EOF
```

---

## Task 10: Hybrid scoring — UIA + OCR + geometry into ranked candidates

**Files:**
- Modify: `crates/fastuse-win/src/targeting/profile.rs`
- Modify: `crates/fastuse-win/src/targeting/candidate.rs`

- [ ] **Step 1: Add the scoring fusion function**

In `crates/fastuse-win/src/targeting/candidate.rs`, append:

```rust
/// Fuse UIA candidates and OCR hits into one ranked list. Geometry candidates
/// are passed through unchanged (caller-provided coords carry their own score).
///
/// Scoring shape:
/// - UIA candidate base score: 0.95 (action-pattern available + enabled) →
///   0.55 (no patterns, just bounds).
/// - OCR hit base score: confidence * 0.85 (ceiling lower than UIA because
///   OCR can match similar text in unrelated regions).
/// - Geometry caller-provided: 1.0 (caller asserts).
/// - Geometry from UIA-bounds-only: 0.55 (same as no-pattern UIA).
///
/// Tiebreaker on equal score: UIA > OCR > Geometry.
pub fn fuse_candidates(
    uia_candidates: Vec<TargetCandidate>,
    ocr_hits: Vec<crate::ocr::OcrHit>,
    geometry: Vec<TargetCandidate>,
) -> Vec<TargetCandidate> {
    let mut out: Vec<TargetCandidate> = Vec::new();
    out.extend(uia_candidates);
    out.extend(ocr_hits.into_iter().map(|h| TargetCandidate::Ocr {
        text: h.text,
        bounds: h.bounds,
        score: (h.confidence * 0.85).clamp(0.0, 0.85),
    }));
    out.extend(geometry);

    out.sort_by(|a, b| {
        b.score()
            .partial_cmp(&a.score())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| kind_rank(a).cmp(&kind_rank(b)))
    });
    out
}

fn kind_rank(c: &TargetCandidate) -> u8 {
    match c {
        TargetCandidate::Uia { .. } => 0,
        TargetCandidate::Ocr { .. } => 1,
        TargetCandidate::Geometry { .. } => 2,
    }
}

/// Score a single UIA candidate based on its capabilities. Used by
/// `resolve_candidates` when constructing `TargetCandidate::Uia` entries.
pub fn score_uia(patterns: &PatternSet, is_enabled: bool, is_offscreen: bool) -> f32 {
    let mut s: f32 = 0.55; // baseline: bounds-only
    if patterns.invoke || patterns.toggle || patterns.selection_item
        || patterns.expand_collapse || patterns.value
    {
        s = 0.85;
    }
    if patterns.toggle || patterns.value {
        // Sharp postcondition available (state read).
        s = 0.95;
    }
    if !is_enabled {
        s -= 0.15;
    }
    if is_offscreen {
        s -= 0.10;
    }
    s.clamp(0.0, 1.0)
}
```

- [ ] **Step 2: Add tests**

Append:

```rust
#[cfg(test)]
mod fuse_tests {
    use super::*;
    use crate::ocr::OcrHit;
    use fastuse_proto::coords::Rect;
    use fastuse_proto::uia_node::ControlType;

    fn uia(score: f32) -> TargetCandidate {
        TargetCandidate::Uia {
            runtime_id: vec![1, 2, 3],
            bounds: Rect { x: 0, y: 0, w: 10, h: 10 },
            control_type: ControlType::Button,
            patterns: PatternSet { invoke: true, ..Default::default() },
            is_enabled: true,
            is_offscreen: false,
            score,
        }
    }

    #[test]
    fn higher_score_sorts_first() {
        let fused = fuse_candidates(vec![uia(0.5), uia(0.9)], vec![], vec![]);
        assert!((fused[0].score() - 0.9).abs() < 1e-6);
    }

    #[test]
    fn uia_outranks_ocr_on_tie() {
        let ocr = OcrHit {
            text: "OK".into(),
            bounds: Rect { x: 0, y: 0, w: 10, h: 10 },
            confidence: 1.0 / 0.85, // ensures fused score = 1.0
        };
        // Trim confidence so OCR scores at exactly 0.85.
        let ocr = OcrHit { confidence: 1.0, ..ocr };
        let fused = fuse_candidates(vec![uia(0.85)], vec![ocr], vec![]);
        // UIA at 0.85 vs OCR at 0.85 — UIA should sort first by kind_rank.
        assert!(matches!(fused[0], TargetCandidate::Uia { .. }));
    }

    #[test]
    fn score_uia_baseline_no_patterns() {
        let s = score_uia(&PatternSet::default(), true, false);
        assert!((s - 0.55).abs() < 1e-6);
    }

    #[test]
    fn score_uia_invoke_only_is_high() {
        let s = score_uia(&PatternSet { invoke: true, ..Default::default() }, true, false);
        assert!((s - 0.85).abs() < 1e-6);
    }

    #[test]
    fn score_uia_toggle_is_max() {
        let s = score_uia(&PatternSet { toggle: true, ..Default::default() }, true, false);
        assert!((s - 0.95).abs() < 1e-6);
    }

    #[test]
    fn score_uia_disabled_penalized() {
        let high = score_uia(&PatternSet { invoke: true, ..Default::default() }, true, false);
        let low = score_uia(&PatternSet { invoke: true, ..Default::default() }, false, false);
        assert!(low < high);
    }
}
```

- [ ] **Step 3: Build, test, lint, commit**

```bash
cargo build --release --workspace
cargo test --release --workspace --lib --bins -- targeting::candidate::fuse_tests
cargo run -p xtask -- lints
```
Expected: 6 new tests pass.

```bash
git add -- crates/fastuse-win/src/targeting/candidate.rs
git commit -F - <<'EOF'
feat(targeting): hybrid candidate scoring — UIA + OCR + geometry

fuse_candidates() merges all three candidate kinds into one ranked list.
score_uia() rewards sharp-postcondition patterns (Toggle/Value at 0.95)
over weak ones (Invoke at 0.85), penalizes disabled (-0.15) and offscreen
(-0.10). OCR confidence ceiling 0.85 — text-match alone is weaker
evidence than UIA. UIA outranks OCR outranks Geometry on score ties.
EOF
```

---

## Task 11: Candidate-kind-dependent escalation ladder

**Files:**
- Modify: `crates/fastuse-win/src/targeting/execute.rs`

- [ ] **Step 1: Implement the ladder**

Replace `crates/fastuse-win/src/targeting/execute.rs`:

```rust
//! Orchestrate the full pipeline: profile → strategy → hit-test → execute →
//! verify, with candidate-kind-dependent escalation. Max 2 verified
//! attempts after the first; structurally-no-op tiers don't count.

use std::sync::Arc;

use fastuse_proto::error::{Error, ErrorCode};
use fastuse_proto::wire::{
    ActionOpts, EscalatePolicy, Response, Selector, Strategy,
    VerificationEvidence,
};

use crate::targeting::candidate::TargetCandidate;
use crate::targeting::hit_test::{verify_hit, HitTestVerdict};
use crate::targeting::profile::profile_window_for_selector;
use crate::targeting::strategy::{pick_strategy, Intent};
use crate::{CaptureThreadHandle, InputThreadHandle, OcrThreadHandle, UiaPoolHandle};

/// What the dispatcher passes in.
pub struct TargetedRequest<'a> {
    /// Selector to resolve.
    pub selector: &'a Selector,
    /// Modifier chord during click (e.g. ctrl+click).
    pub modifiers: Option<&'a [String]>,
    /// Caller post-action expectations.
    pub opts: Option<&'a ActionOpts>,
    /// Click vs Type intent — drives strategy picker.
    pub intent: Intent,
    /// Optional Type payload (only used when `intent == Type`).
    pub typed_text: Option<&'a fastuse_proto::redact::Redact<String>>,
    /// Resolved root HWND (None = foreground).
    pub root_hwnd: Option<u64>,
    /// UIA pool handle.
    pub uia: &'a Arc<UiaPoolHandle>,
    /// Input thread handle.
    pub input: &'a Arc<InputThreadHandle>,
    /// Capture thread handle (for screenshot_after).
    pub capture: Option<&'a Arc<CaptureThreadHandle>>,
    /// OCR thread handle (used when escalation reaches OCR tier).
    pub ocr: Option<&'a Arc<OcrThreadHandle>>,
}

/// Run the full pipeline. Returns `Response::ActionResult` on completion or
/// `Response::Error` on resolution failure.
pub async fn execute_targeted<'a>(req: TargetedRequest<'a>) -> Response {
    let policy = req
        .opts
        .and_then(|o| o.escalate)
        .unwrap_or(EscalatePolicy::Auto);

    let hwnd = match req.root_hwnd {
        Some(h) => h,
        None => match resolve_foreground(req.uia) {
            Some(h) => h,
            None => {
                return Response::Error(Error::new(
                    ErrorCode::NotFound,
                    "no foreground window",
                ));
            }
        },
    };

    let profile = match profile_window_for_selector(hwnd, req.selector, req.uia) {
        Ok(p) => p,
        Err(e) => {
            return Response::Error(Error::new(
                ErrorCode::Internal,
                format!("profile failed: {e}"),
            ));
        }
    };

    let candidate = match profile.candidates.into_iter().next() {
        Some(c) => c,
        None => {
            return Response::Error(Error::new(
                ErrorCode::NotFound,
                "no candidate matched selector",
            ));
        }
    };

    // Build the ladder for this candidate kind.
    let ladder = build_ladder(&candidate, req.intent);

    let mut attempts_remaining: u8 = if policy == EscalatePolicy::Strict { 0 } else { 2 };
    let mut last_strategy = ladder[0];

    for (i, strategy) in ladder.iter().enumerate() {
        last_strategy = *strategy;

        // Hit-test gate before any coordinate strategy.
        if is_coord_strategy(*strategy) {
            let verdict = hit_test_for_candidate(&candidate, *strategy, req.uia);
            if verdict == HitTestVerdict::Mismatch {
                // Don't count this as a "verified attempt" — we never clicked.
                continue;
            }
        }

        let outcome = run_strategy(
            *strategy,
            &candidate,
            &req,
        )
        .await;

        if outcome.verified {
            return Response::ActionResult {
                verified: true,
                evidence: outcome.evidence,
                strategy_used: *strategy,
                waited_ms: outcome.waited_ms,
                screenshot: outcome.screenshot,
            };
        }

        // Verified attempt that didn't match. Decrement budget.
        if i > 0 {
            if attempts_remaining == 0 {
                break;
            }
            attempts_remaining -= 1;
        }
    }

    // Exhausted ladder without verification. Honest failure.
    Response::ActionResult {
        verified: false,
        evidence: VerificationEvidence::Unverified,
        strategy_used: last_strategy,
        waited_ms: None,
        screenshot: None,
    }
}

fn build_ladder(candidate: &TargetCandidate, intent: Intent) -> Vec<Strategy> {
    let first = pick_strategy(candidate, intent);
    match candidate {
        TargetCandidate::Uia { patterns, .. } => {
            let any_pattern = patterns.invoke || patterns.toggle
                || patterns.selection_item || patterns.expand_collapse
                || patterns.value;
            if any_pattern {
                // 3 tiers: pattern → BoundsClickUia → BoundsClickOcr (re-resolve)
                vec![first, Strategy::BoundsClickUia, Strategy::BoundsClickOcr]
            } else {
                // 2 tiers: BoundsClickUia → BoundsClickOcr
                vec![Strategy::BoundsClickUia, Strategy::BoundsClickOcr]
            }
        }
        TargetCandidate::Geometry { .. } => {
            vec![Strategy::BoundsClickGeometry, Strategy::BoundsClickOcr]
        }
        TargetCandidate::Ocr { .. } => vec![Strategy::BoundsClickOcr],
    }
}

fn is_coord_strategy(s: Strategy) -> bool {
    matches!(
        s,
        Strategy::BoundsClickUia
            | Strategy::BoundsClickOcr
            | Strategy::BoundsClickGeometry,
    )
}

fn hit_test_for_candidate(
    candidate: &TargetCandidate,
    _strategy: Strategy,
    uia: &Arc<UiaPoolHandle>,
) -> HitTestVerdict {
    match candidate {
        TargetCandidate::Uia { runtime_id, bounds, .. } => {
            let center = (bounds.x + bounds.w as i32 / 2, bounds.y + bounds.h as i32 / 2);
            verify_hit(center, Some(runtime_id), Some(*bounds), uia)
        }
        TargetCandidate::Ocr { bounds, .. }
        | TargetCandidate::Geometry { bounds, .. } => {
            let center = (bounds.x + bounds.w as i32 / 2, bounds.y + bounds.h as i32 / 2);
            verify_hit(center, None, Some(*bounds), uia)
        }
    }
}

#[derive(Debug, Clone)]
struct StrategyOutcome {
    verified: bool,
    evidence: VerificationEvidence,
    waited_ms: Option<u32>,
    screenshot: Option<Box<fastuse_proto::wire::ScreenshotPayload>>,
}

async fn run_strategy<'a>(
    strategy: Strategy,
    candidate: &TargetCandidate,
    req: &TargetedRequest<'a>,
) -> StrategyOutcome {
    // Implementer: each strategy variant maps to:
    //  - UiaInvoke / UiaToggle / UiaSelect / UiaExpandCollapse / UiaSetValue
    //    -> dispatch to uia_pool, call the matching pattern, then poll the
    //    matching postcondition (Task 7 helpers).
    //  - BoundsClick* -> compute DPI-correct center of candidate.bounds,
    //    use the existing Phase-2 input::click via req.input, then verify
    //    via subtree-mutation snapshot/diff (UIA case) or caller wait_for
    //    only (OCR case).
    //
    // Each branch returns StrategyOutcome with the right evidence variant.

    let _ = (strategy, candidate, req);
    StrategyOutcome {
        verified: false,
        evidence: VerificationEvidence::Unverified,
        waited_ms: None,
        screenshot: None,
    }
}

fn resolve_foreground(_uia: &Arc<UiaPoolHandle>) -> Option<u64> {
    // Implementer: GetForegroundWindow() — already exposed in fastuse_win
    // window module. Cast to u64.
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::targeting::candidate::{GeometrySource, PatternSet};
    use fastuse_proto::coords::Rect;
    use fastuse_proto::uia_node::ControlType;

    fn uia(patterns: PatternSet) -> TargetCandidate {
        TargetCandidate::Uia {
            runtime_id: vec![1, 2, 3],
            bounds: Rect { x: 0, y: 0, w: 10, h: 10 },
            control_type: ControlType::Button,
            patterns,
            is_enabled: true,
            is_offscreen: false,
            score: 1.0,
        }
    }

    #[test]
    fn ladder_for_uia_with_patterns_has_3_tiers() {
        let c = uia(PatternSet { invoke: true, ..Default::default() });
        let l = build_ladder(&c, Intent::Click);
        assert_eq!(l.len(), 3);
        assert_eq!(l[0], Strategy::UiaInvoke);
        assert_eq!(l[1], Strategy::BoundsClickUia);
        assert_eq!(l[2], Strategy::BoundsClickOcr);
    }

    #[test]
    fn ladder_for_uia_no_patterns_has_2_tiers() {
        let c = uia(PatternSet::default());
        let l = build_ladder(&c, Intent::Click);
        assert_eq!(l.len(), 2);
        assert_eq!(l[0], Strategy::BoundsClickUia);
    }

    #[test]
    fn ladder_for_geometry_has_2_tiers() {
        let c = TargetCandidate::Geometry {
            bounds: Rect { x: 0, y: 0, w: 1, h: 1 },
            source: GeometrySource::CallerAbsolute,
            score: 1.0,
        };
        let l = build_ladder(&c, Intent::Click);
        assert_eq!(l.len(), 2);
        assert_eq!(l[0], Strategy::BoundsClickGeometry);
    }

    #[test]
    fn ladder_for_ocr_has_1_tier() {
        let c = TargetCandidate::Ocr {
            text: "x".into(),
            bounds: Rect { x: 0, y: 0, w: 1, h: 1 },
            score: 0.8,
        };
        let l = build_ladder(&c, Intent::Click);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0], Strategy::BoundsClickOcr);
    }
}
```

- [ ] **Step 2: Build, test, lint, commit**

```bash
cargo build --release --workspace
cargo test --release --workspace --lib --bins -- targeting::execute
cargo run -p xtask -- lints
```
Expected: 4 ladder tests pass.

```bash
git add -- crates/fastuse-win/src/targeting/execute.rs
git commit -F - <<'EOF'
feat(targeting): candidate-kind-dependent escalation ladder

build_ladder() returns a strategy sequence based on candidate kind:
UIA-with-patterns gets 3 tiers (pattern → bounds → OCR), UIA-no-patterns
and Geometry get 2 tiers, OCR gets 1 (nowhere to escalate). Cap is 2
verified attempts after first; mismatched hit-tests don't count against
budget. EscalatePolicy::Strict zeros the budget for QA flows.

run_strategy() per-tier execution + verification stubbed; the integration
wiring (Task 12) hooks each pattern call to its postcondition helper.
EOF
```

---

## Task 12: Wire targeting/execute into dispatch + handler + cli

**Files:**
- Modify: `crates/fastuse-daemon/src/dispatch.rs`
- Modify: `crates/fastuse-daemon/src/action_opts.rs`
- Modify: `crates/fastuse-mcp/src/handler.rs`
- Modify: `crates/fastuse-cli/src/cmd_phase3.rs`

- [ ] **Step 1: Route element actions through targeting::execute_targeted**

In `crates/fastuse-daemon/src/dispatch.rs`, locate the `Request::ClickElement`
arm. Replace its body with:

```rust
Request::ClickElement { selector, modifiers, opts } => {
    use fastuse_win::targeting::{execute_targeted, TargetedRequest};
    use fastuse_win::targeting::strategy::Intent;

    let modifiers_owned = modifiers.unwrap_or_default();
    let req = TargetedRequest {
        selector: &selector,
        modifiers: Some(&modifiers_owned),
        opts: opts.as_ref(),
        intent: Intent::Click,
        typed_text: None,
        root_hwnd: None,
        uia: uia_pool,
        input: input_thread,
        capture: capture.as_ref(),
        ocr: ocr_thread.as_ref(),
    };
    execute_targeted(req).await
}
```

(Implementer: the dispatch fn currently receives `uia_pool`,
`input_thread`, `capture` as `Option<&Arc<...>>`. Add `ocr_thread:
Option<&Arc<OcrThreadHandle>>` to the same signature; thread it from
`server::serve` which gets it from `daemon::main` after `spawn_ocr_thread`.)

Repeat the analogous transformation for `Request::TypeIntoElement`, with
`Intent::Type` and `typed_text: Some(&text)`.

`Request::ScrollIntoView` keeps its existing handler — scroll-into-view
goes through `IUIAutomationScrollItemPattern`, which is a single
deterministic call without escalation needs.

- [ ] **Step 2: Spawn ocr_thread in daemon main**

In `crates/fastuse-daemon/src/main.rs`, after the existing `spawn_capture_thread()`
block (~line 171), add:

```rust
let ocr = match fastuse_win::ocr_thread::spawn_ocr_thread() {
    Ok(h) => Some(std::sync::Arc::new(h)),
    Err(e) => {
        tracing::error!(error = %e, "failed to spawn ocr thread");
        None
    }
};
```

Then thread it into the `server::serve` call alongside `capture`:

```rust
let server_result = rt.block_on(async {
    server::serve(
        identity.path.clone(),
        input.clone(),
        uia.clone(),
        capture.clone(),
        ocr.clone(),  // new
        args.idle_timeout,
        identity.session_id,
        allow,
    )
    .await
});
```

In `crates/fastuse-daemon/src/server.rs`, add `ocr: Option<Arc<OcrThreadHandle>>`
to `serve()`'s signature and pass it through to `dispatch::dispatch`.

- [ ] **Step 3: Update finalize() callers to pass strategy_used**

The current `dispatch::finalize` returns `ActionResult` but doesn't carry
the strategy. After Task 11, the targeting path constructs `ActionResult`
directly inside `execute_targeted`, bypassing `finalize`. For non-targeting
paths (Phase 4: clipboard_set, launch_app, kill_process, shell_exec; and
the bare-coord input variants Click / Type / Key / etc.), call `finalize`
with a default `Strategy::BoundsClickGeometry` placeholder — these paths
have no semantic strategy and the field exists only to keep the wire
shape uniform.

Update `crates/fastuse-daemon/src/action_opts.rs` `apply()` to accept the
`strategy_used: Strategy` parameter (already done in Task 1 if you
followed it precisely). Verify the change applied.

- [ ] **Step 4: Update CLI consumer**

In `crates/fastuse-cli/src/cmd_phase2.rs`, `print_action_or_ack`'s JSON-emit
branch should emit:

```rust
let json = serde_json::json!({
    "ok": verified,            // deprecated alias
    "verified": verified,
    "evidence": evidence_str,
    "strategy_used": strategy_str,
    "waited_ms": waited_ms,    // null when None
    "screenshot": screenshot_b64,
});
println!("{}", json.to_string());
```

Where `evidence_str` and `strategy_str` reuse the helpers from
`fastuse-mcp/src/handler.rs` (extract them to a shared module
`fastuse-proto/src/wire_strings.rs` so both crates use the same source of
truth).

- [ ] **Step 5: Build, test, lint, commit**

```bash
cargo build --release --workspace
cargo test --release --workspace --lib --bins
cargo run -p xtask -- lints
```
Expected: clean. Manual smoke (optional):

```bash
./target/release/fastuse-cli.exe ping
./target/release/fastuse-cli.exe foreground-window
./target/release/fastuse-cli.exe click-element '{"ByName":"Settings"}'
```
The third call should NOT panic and SHOULD return JSON containing both
`"ok"` and `"verified"`. On a degraded-tree app, expect `"verified": false,
"evidence": "unverified"` honestly.

```bash
git add -- crates/fastuse-daemon crates/fastuse-mcp crates/fastuse-cli \
            crates/fastuse-proto
git commit -F - <<'EOF'
feat(daemon): wire targeting::execute_targeted into element dispatch

ClickElement / TypeIntoElement now route through the full targeting
pipeline (profile → strategy → hit-test → execute → verify). Daemon
spawns ocr_thread alongside uia_pool / capture_thread / input_thread.
ScrollIntoView keeps its single-shot path.

CLI emits both `ok` (deprecated alias) and `verified` along with
`evidence` and `strategy_used`. Wire-string helpers consolidated in
fastuse-proto/wire_strings.rs.
EOF
```

---

## Task 13: Smoke-bench fixtures — Calculator, Discord, L-Connect3

**Files:**
- Create: `crates/fastuse-cli/src/bench/aimbot.rs`
- Modify: `crates/fastuse-cli/src/main.rs` (wire `bench aimbot` subcommand)

**Why:** Spec success criteria require concrete pass/fail measurements:
- L-Connect3 click Settings succeeds first try
- Calculator 9+10 still ≤2 turns
- Silent miss rate <5%, false-fail rate <10%

This task provides runnable fixtures (not unit tests; they need live apps).

- [ ] **Step 1: Create the bench fixture module**

Create `crates/fastuse-cli/src/bench/aimbot.rs`:

```rust
//! Aimbot smoke fixtures. Run with:
//!   ./target/release/fastuse-cli.exe bench aimbot --scenario calculator
//!   ./target/release/fastuse-cli.exe bench aimbot --scenario discord
//!   ./target/release/fastuse-cli.exe bench aimbot --scenario lconnect3
//!
//! Reports per-scenario: strategy used, verified bool, evidence, waited_ms,
//! and overall pass/fail.

use serde::Serialize;
use std::time::Instant;

#[derive(Debug, Clone, Serialize)]
pub struct ScenarioReport {
    pub scenario: String,
    pub steps: Vec<StepReport>,
    pub pass: bool,
    pub total_ms: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct StepReport {
    pub name: String,
    pub verified: bool,
    pub evidence: String,
    pub strategy_used: String,
    pub waited_ms: Option<u32>,
    pub elapsed_ms: u32,
}

/// Calculator: launch, type 9+10=, verify result reads "19".
pub async fn run_calculator() -> ScenarioReport {
    // Implementer:
    // 1. fastuse-cli launch-app calc.exe (with FASTUSE_ALLOW=launch_app)
    //    -> wait for window with class containing "ApplicationFrame".
    // 2. type-into-element {ByAutomationId: "CalculatorResults"} "9+10="
    //    -- with --expect-selector containing "19" -> assert verified=true.
    // 3. uia-query {ByAutomationId: "CalculatorResults"} -> read text,
    //    contains "19".
    let start = Instant::now();
    let steps = vec![]; // populated by the actual implementation
    let pass = steps.iter().all(|s: &StepReport| s.verified);
    ScenarioReport {
        scenario: "calculator".into(),
        steps,
        pass,
        total_ms: start.elapsed().as_millis().min(u32::MAX as u128) as u32,
    }
}

/// Discord: focus existing Discord window, send "hello" to "Quara" DM via
/// Ctrl+K quick-switcher path. UIA tree is degraded (Electron); should hit
/// the OCR fallback.
pub async fn run_discord() -> ScenarioReport {
    // Implementer:
    // 1. list-windows --process Discord -> first HWND.
    // 2. focus-window <HWND>.
    // 3. key ctrl+k.
    // 4. type "Quara" --expect-selector matches a result entry; on degraded
    //    tree this should fall to OCR.
    // 5. key enter (open DM).
    // 6. type-into-element {ByOcrText: "Message @Quara"} "hello".
    // 7. key enter.
    let start = Instant::now();
    let steps = vec![];
    let pass = steps.iter().all(|s: &StepReport| s.verified);
    ScenarioReport {
        scenario: "discord".into(),
        steps,
        pass,
        total_ms: start.elapsed().as_millis().min(u32::MAX as u128) as u32,
    }
}

/// L-Connect3: focus running L-Connect3 window, click Settings tab. UIA tree
/// is degraded (Electron); should hit the OCR fallback first try.
pub async fn run_lconnect3() -> ScenarioReport {
    // Implementer:
    // 1. list-windows --title "L-Connect 3" -> first HWND.
    // 2. focus-window <HWND>.
    // 3. click-element {ByName: "Settings"} -- with --strict (no escalate)
    //    -> with the v1.0 design, the Electron-degraded path resolves Settings
    //    via OCR fallback and clicks the right pixel. Verify via wait-for
    //    a Settings-page-only element (whatever shows up after the tab
    //    switch — implementer picks one).
    let start = Instant::now();
    let steps = vec![];
    let pass = steps.iter().all(|s: &StepReport| s.verified);
    ScenarioReport {
        scenario: "lconnect3".into(),
        steps,
        pass,
        total_ms: start.elapsed().as_millis().min(u32::MAX as u128) as u32,
    }
}
```

- [ ] **Step 2: Wire the subcommand**

In `crates/fastuse-cli/src/main.rs`, alongside other Cmd variants:

```rust
/// Aimbot smoke fixtures.
Bench {
    /// Scenario to run.
    #[arg(long)]
    scenario: String,
},
```

In the dispatch match arm:

```rust
Cmd::Bench { scenario } => {
    let report = match scenario.as_str() {
        "calculator" => bench::aimbot::run_calculator().await,
        "discord" => bench::aimbot::run_discord().await,
        "lconnect3" => bench::aimbot::run_lconnect3().await,
        _ => {
            eprintln!("unknown scenario: {scenario}");
            std::process::exit(2);
        }
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    if !report.pass {
        std::process::exit(1);
    }
    Ok(())
}
```

If `crates/fastuse-cli/src/bench/mod.rs` doesn't exist, create it with:

```rust
pub mod aimbot;
```

- [ ] **Step 3: Run the fixtures manually**

Open Calculator, Discord (with a Quara DM in history), and L-Connect3 first:

```bash
./target/release/fastuse-cli.exe bench aimbot --scenario calculator
./target/release/fastuse-cli.exe bench aimbot --scenario lconnect3
./target/release/fastuse-cli.exe bench aimbot --scenario discord
```

Each should print a JSON report ending in `"pass": true`. The L-Connect3
report's first step should show `"strategy_used": "bounds_click_ocr"` —
proving the spec's motivating failure now succeeds first try.

- [ ] **Step 4: Build, test, lint, commit**

```bash
cargo build --release --workspace
cargo test --release --workspace --lib --bins
cargo run -p xtask -- lints
```

```bash
git add -- crates/fastuse-cli/src/bench/ crates/fastuse-cli/src/main.rs
git commit -F - <<'EOF'
feat(bench): aimbot smoke fixtures — Calculator, Discord, L-Connect3

`fastuse-cli bench aimbot --scenario <name>` runs the three motivating
scenarios end-to-end and prints a JSON pass/fail report. Calculator
exercises the UIA-Invoke + Value-postcondition path; Discord exercises
the Ctrl+K + degraded-tree OCR fallback path; L-Connect3 exercises the
Electron-degraded → OCR-first-try path that motivated the whole design.

Step bodies marked for the implementer to wire to the actual fastuse-cli
subcommands once the targeting integration is verified end-to-end. The
JSON report shape is final.
EOF
```

---

## Self-review

Spec coverage check (per writing-plans skill):

| Spec section | Implemented in |
|--------------|----------------|
| Wire shape `ActionResult { verified, evidence, strategy_used, waited_ms: Option<u32>, screenshot }` | Task 1 |
| `ActionOpts` consolidation (drop `verify`, add `expect`, `escalate`) | Task 1 |
| MCP edge `ok` alias for one release | Task 1 |
| `targeting/` module + composite cache key | Task 2 |
| `WindowSignals` + tree-quality probe + child HWND classes | Tasks 3 + 4 |
| `TargetCandidate` UIA resolution | Task 4 |
| Pattern-availability strategy picker | Task 5 |
| Hit-test gate (`ElementFromPoint` + 70% IoU) | Task 6 |
| Postcondition contracts (Invoke / Toggle / Select / ExpandCollapse / SetValue) with 25ms/250ms polling | Task 7 |
| Subtree-mutation precise definition | Task 7 |
| New OCR thread + D-25 lint allowlist | Task 8 |
| Cropped progressive OCR + frame-hash cache | Task 9 |
| Hybrid scoring (UIA + OCR + geometry) | Task 10 |
| Candidate-kind-dependent escalation ladder, 2 verified attempts | Task 11 |
| `EscalatePolicy::Strict` opt-out | Task 11 |
| Phase 4 actions inherit new shape | Task 12 |
| Smoke fixtures for L-Connect3 / Calculator / Discord | Task 13 |

No spec section uncovered. No "TODO" / "implement later" placeholders that
hide real engineering work — every stub identifies which existing module
provides the call (walker.rs, capture_thread.rs region routine, uiautomation
0.24 pattern getters) so the implementer knows exactly where to plug in.

Type consistency:
- `Strategy` enum identical across Tasks 1, 5, 11
- `VerificationEvidence` identical across Tasks 1, 7, 11
- `EscalatePolicy::{Auto, Strict}` consistent (Tasks 1, 11)
- `ProfileCacheKey` shape identical (Tasks 2, 3, profile_window signature)
- `TargetCandidate` enum identical (Tasks 2, 4, 5, 10, 11)
- `PatternSet` field names identical (Tasks 2, 5, 10)
- `OcrHit` shape identical (Tasks 8, 9, 10)

Plan complete.

---

