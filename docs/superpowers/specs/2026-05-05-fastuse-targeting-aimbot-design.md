# fastuse v1.0 — Aimbot-Grade Targeting Design

**Date:** 2026-05-05
**Branch:** v1.0
**Status:** Approved, pending implementation plan
**Supersedes:** click-handling sections of `2026-05-04-fastuse-agent-loop-design.md`

## Goal

Make `click_element`, `type_into_element`, `wait_for_element`, and `scroll_into_view` reliable across **unknown Windows apps** — including degraded-UIA cases (Electron, Qt, custom renderers, canvas-only UIs). The system must:

1. Detect what kind of app it is *before* the first action and pick the right strategy on first try.
2. Never silently report success when the action did not produce its intended effect.

## Motivating failure

An agent ran `click_element({ByName: "Settings"})` against L-Connect3 (Lian Li peripheral software, Electron-based). UIA tree was degraded — anonymous Pane elements with no AutomationIds. The agent fell back to eyeballing pixel coords from a screenshot, clicked, missed. The daemon returned `ok: true`. The agent burned three turns guessing root causes (UAC integrity, elevation, focus-stealing) instead of seeing "the click didn't land."

Two failures stacked:
- No semantic targeting fallback when UIA degraded
- No verification — the daemon couldn't tell whether the click had any effect

Both must be fixed in this design.

## Architecture

A new module `crates/fastuse-win/src/targeting/` owns the smart parts of element interaction:

```
targeting/
  profile.rs    — fingerprint target window + element, produce signals
  strategy.rs   — score candidates across UIA / OCR / geometry; pick top
  verify.rs     — per-action postcondition contracts
  execute.rs    — run picked strategy + verify + return rich result
```

The existing per-pattern modules (`uia/`, `input/`, `capture/`, `clipboard/`) stay as primitives. `targeting/` orchestrates.

## TargetProfile — replaces deterministic decision table

Profiling produces a **ranked list of candidates with confidence**, not a single strategy. Multiple signals fuse into the score.

```rust
pub struct TargetProfile {
    pub window: WindowSignals,
    pub candidates: Vec<TargetCandidate>,  // sorted by confidence desc
}

pub struct WindowSignals {
    pub framework_id: Option<String>,        // hint, not authority
    pub window_class: String,                // hard backstop
    pub child_classes: Vec<String>,          // Chrome_RenderWidgetHostHWND, WebView2, Qt*, etc.
    pub uia_tree_quality: TreeQuality,       // Healthy | Mixed | Degraded
    pub integrity_level: IntegrityLevel,     // for UIPI gate
    pub cloaked: bool,
    pub minimized: bool,
    pub occluded: bool,
    pub dwm_extended_frame_bounds: Rect,
}

pub enum TargetCandidate {
    Uia { element_id: RuntimeId, bounds: Rect, patterns: PatternSet, score: f32 },
    Ocr { text: String, bounds: Rect, score: f32 },
    Geometry { bounds: Rect, source: GeometrySource, score: f32 },
}
```

**Cache key is composite, not just HWND.** HWND reuse is real on long sessions:
```rust
pub struct ProfileCacheKey {
    hwnd: HWND,
    pid: u32,
    process_start_time: FILETIME,
    generation: u64,  // bumped on detected window-rect / DPI / topology change
}
```

Invalidated on: process exit, window-destroy, foreground/location/DPI/display-topology change, provider error.

## Strategy selection — pattern availability dominates ControlType

The decision is driven by **what the candidate actually supports**, not by ControlType label:

| Candidate state | First-choice strategy |
|-----------------|------------------------|
| UIA + InvokePattern available + IsEnabled | Invoke (no coords) |
| UIA + TogglePattern available | Toggle |
| UIA + SelectionItemPattern available | Select |
| UIA + ExpandCollapsePattern available | Expand/Collapse per intent |
| UIA + ValuePattern available (for type) | SetValue |
| UIA + only LegacyIAccessible | DoDefaultAction *(deferred to v1.1)* |
| UIA bounds, no usable pattern | Hit-tested BoundsClick |
| OCR-derived bounds | Hit-tested BoundsClick |
| Geometry-only candidate (caller-provided) | Hit-tested BoundsClick |

### Escalation ladder is candidate-kind dependent

The ladder length depends on the chosen candidate, not a fixed cap:

| Candidate kind | Ladder |
|----------------|--------|
| `Uia` with patterns | pattern → BoundsClick(uia_rect) → OCR re-resolve (3 tiers) |
| `Uia` without patterns | BoundsClick(uia_rect) → OCR re-resolve (2 tiers) |
| `Geometry` (caller-provided) | BoundsClick(rect) → OCR re-resolve (2 tiers) |
| `Ocr`-derived | BoundsClick(ocr_rect) only (1 tier; nowhere to escalate) |

The cap is **max 2 verified attempts after the first attempt** — a structurally-no-op first tier (e.g. pattern attempt on a no-pattern candidate) doesn't count against the budget. Each tier is gated by hit-test + postcondition verification before counting as "attempted".

`ActionOpts.escalate: Option<EscalatePolicy>` lets callers opt out:
- `EscalatePolicy::Auto` (default) — full ladder for the candidate kind
- `EscalatePolicy::Strict` — single-shot, no escalation (for QA flows where a miss must surface as a failure, not a recovery)

## Hit-test gate — mandatory before any coordinate click

Before SendInput at any point — UIA-bounds, OCR-bounds, or caller-provided coords:

1. Compute DPI-correct point (already handled per D-24).
2. Call `IUIAutomation::ElementFromPoint` at that exact point.
3. Verify the hit-test result matches the intended target candidate (same RuntimeId for UIA candidates; bounding-box overlap > 70% for geometry candidates).
4. If mismatch: abort, re-resolve, do not click.

This is the actual aimbot crosshair check. Without it, "click missed" is unobservable.

## Per-action postcondition contracts — the load-bearing piece

The honesty mechanism. Each action type carries its own verification.

**Property-state postconditions race against provider-side handlers.** UIA pattern handlers are *not* always synchronous — Electron, WPF binding pipelines, and message-pump-routed apps can take 50–200ms before the property updates. A bare "read after Invoke" is insufficient.

**The verification helper polls** at 25ms cadence with a 250ms cap (configurable via `wait_timeout_ms`). Cheap when fast (one read), bounded when slow. Same shape as existing `wait_for` — reuse the helper.

| Action | Postcondition | Implementation |
|--------|---------------|----------------|
| Invoke | Subtree mutation OR caller `wait_for` matched | UIA snapshot pre + poll-post (parent's children RuntimeId set diff OR foreground HWND change OR focused element change) |
| Toggle | `ToggleState` matches intent | Poll pattern state, 25ms cadence, 250ms cap |
| Select | `SelectionItemPattern.IsSelected == true` | Poll pattern state, 25ms cadence, 250ms cap |
| ExpandCollapse | `ExpandCollapseState` matches intent | Read after — set synchronously by provider in the common case; fall to poll on mismatch |
| SetValue (type) | `ValuePattern.Value.to_lowercase().contains(typed.to_lowercase())` AND `len_ratio in 0.7..=1.5` | Poll value, 25ms cadence, 250ms cap. On tolerance failure, return `Unverified`, not `verified: false` (apps normalize text — Excel uppercases formulas, search boxes strip whitespace, IME composition reorders) |
| BoundsClick (UIA) | Subtree mutation OR caller `wait_for` matched | Same as Invoke — uses parent's children RuntimeId set diff |
| BoundsClick (OCR) | Caller `wait_for` matched (no other honest verifier without semantic context) | Falls back to caller intent |

### "Subtree mutation" — precise definition

Required because a naive child-count diff blows the false-fail budget on any app with a tooltip:

> **Subtree mutation =** RuntimeId set of the *target's parent's* children differs from pre-snapshot, OR foreground HWND changed, OR focused element changed.

The pre-action probe must retain a cached reference to the target's parent so the post-snapshot can re-walk just that one level. Adds one cached element to the pre-action probe; no extra UIA walk on the action path.

Generic verification (screenshot diff, raw UIA-root mutation) is **explicitly rejected** as unreliable.

### Response shape

```rust
pub struct ActionResult {
    pub verified: bool,            // postcondition matched
    pub evidence: VerificationEvidence,
    pub strategy_used: Strategy,
    pub waited_ms: Option<u32>,    // None when no wait_for / no postcondition polled
    pub screenshot: Option<ScreenshotPayload>,
}

pub enum VerificationEvidence {
    PostconditionMet,    // toggle flipped, value set, selection changed, etc.
    WaitForMatched,      // caller-supplied wait_for / expect matched
    HitTestOnly,         // we hit the right pixel; no postcondition available
    Unverified,          // attempted but no contract applicable, or contract failed tolerance check
}
```

`attempted` is *not* a field — `Response::ActionResult` is only emitted after the strategy actually ran. Inner failures (input thread unavailable, etc.) return `Response::Error`, never `ActionResult`. Encoding `attempted: bool` would name a state that can't exist.

`waited_ms: Option<u32>` replaces the old `u32` — `None` is the honest encoding for "no wait_for requested and no postcondition polled".

`Unverified` is the new honest state. Better an honest "couldn't tell" than a noisy false-fail.

## OCR strategy — cropped progressive

Default scope on degraded UIA: target window only, not full screen. Order:

1. Crop to target window's client rect (excludes title bar, borders).
2. If multiple OCR matches, narrow to UIA-reported scroll containers / panes that *did* resolve.
3. Only fall back to full window if (1)+(2) yield zero matches.

Cache by `frame_hash + region_rect` — when the same window region is OCR'd within ~500ms, reuse the result. Cache invalidated on capture-thread frame-change notification (already exists in `capture_thread`).

Latency target: warm OCR ~30ms (cached), cold OCR <100ms (cropped). Full-window OCR (~150ms cold) only when other strategies fail.

## Wire format changes

`fastuse-proto/src/wire.rs` — clean break:

```rust
Response::ActionResult {
    verified: bool,
    evidence: VerificationEvidence,
    strategy_used: Strategy,
    waited_ms: Option<u32>,
    screenshot: Option<ScreenshotPayload>,
}
```

`ok` is removed from the wire — it conflated "attempted" and "verified" and that conflation was the L-Connect3 bug. `verified: bool` is the contract callers should branch on.

**MCP edge compat (one release):** `fastuse-mcp/src/handler.rs` `ActionResultOutput` emits **both** `ok` (= `verified`) and `verified` for one release. Schema description marks `ok` deprecated. Removed in v1.1. The CLI's `print_action_or_ack` does the same. ~6 LOC of compat cost; keeps every existing agent prompt and CLAUDE.md trigger working.

**`ActionOpts` consolidation:** the existing `verify: Option<Selector>` and the new `expect` overlap. Fold `verify` into `expect::SelectorMatches` and remove the `verify` field — one concept on the wire:

```rust
pub struct ActionOpts {
    pub wait_for: Option<Selector>,        // unchanged — non-failing wait
    pub expect: Option<ExpectClause>,      // replaces verify; failing wait + richer postconditions
    pub screenshot_after: Option<ScreenshotOpts>,
    pub wait_timeout_ms: Option<u32>,
    pub escalate: Option<EscalatePolicy>,  // new — Auto (default) | Strict
}

pub enum ExpectClause {
    DialogOpens,
    WindowTitleMatches(String),
    ForegroundChangesTo { class: Option<String>, title: Option<String> },
    SelectorMatches(Selector),
}

pub enum EscalatePolicy {
    Auto,    // full ladder for the candidate kind (default)
    Strict,  // single-shot, no escalation (QA flows)
}
```

**Phase 4 actions also use the new `ActionResult` shape.** `launch_app`, `clipboard_set_text`, `clipboard_set_image`, `kill_process`, `shell_exec` all currently route through `dispatch::finalize()` and therefore emit `ActionResult`. The wire change applies workspace-wide, not just to element-targeting. This is intentional — the same honesty rules apply ("did the launch actually start the process?", "did the clipboard set?", etc.). v1.1 may grow per-action postcondition contracts for the Phase 4 cohort; v1.0 returns `Unverified` honestly.

## Signals added beyond the original proposal

Per Codex review:
- Cloaked state (UWP windows hidden by OS)
- Minimized / occluded state
- Z-order / topmost overlay detection (defer detailed handling to v1.1)
- Cursor position before click (for restoration after, and for hit-test sanity)
- DWM extended frame bounds (≠ window rect for modern apps)
- Child HWND class names: `Chrome_RenderWidgetHostHWND`, `WebView2`, `Qt*`, `Windows.UI.Core.CoreWindow`
- Bounding rectangle sanity checks (zero-size, offscreen, stale)
- Foreground eligibility / last-input-time lock state

`FrameworkId` stays as a **hint, not a decision key**. Mixed trees are common — Electron with WebView2 child has different providers per layer.

## What's in v1 (this spec)

- `targeting/` module skeleton: `profile.rs`, `strategy.rs`, `verify.rs`, `execute.rs`
- Composite cache key (HWND + PID + process_start + generation)
- `TargetProfile` with ranked candidates + signals
- Pre-click hit-test gate for every coordinate click
- Postcondition contracts for the 5 cheap pattern cases (Invoke, Toggle, Select, ExpandCollapse, SetValue)
- New response shape with `verified: bool` + `VerificationEvidence`
- Cropped progressive OCR with frame-hash cache
- Hybrid scoring fusing UIA candidates + OCR + geometry
- Two-tier verified escalation (pattern → bounds → OCR re-resolve), max two attempts
- `ExpectClause` on `ActionOpts`

## What's deferred to v1.1

- Stable-frame detection / animation handling
- Overlay/loading-state detection ("blocked by spinner")
- Image template matching (caller-provided reference image)
- `LegacyIAccessible.DoDefaultAction` strategy
- Z-order / topmost overlay handling beyond detection
- Per-action wait policies (debounce, virtualized list awareness)

## Non-goals

- Generic screenshot-diff verification — explicitly rejected as unreliable
- Generic UIA-tree-mutation verification — explicitly rejected, too noisy on degraded trees
- LLM-in-the-loop targeting — defeats the latency goal
- Cross-platform — Windows only

## Success criteria

| Metric | Target |
|--------|--------|
| L-Connect3 "click Settings" succeeds first try | Yes (via OCR strategy + post-action wait_for or hit-test+expect) |
| Calculator "9+10=" still ≤2 turns | Yes (UIA Invoke + ToggleState/Value postcondition) |
| Silent miss rate (`verified: false` while UI changed) | <5% across bench corpus |
| False-fail rate (`verified: false` while target was hit) | <10% on bench corpus, with `expect` provided |
| Tool RTT, no perception, healthy UIA | p50 <50ms |
| Tool RTT, degraded UIA + OCR | p50 <200ms warm |

## Implementation order

1. New wire shape — `ActionResult { verified, evidence, strategy_used, waited_ms: Option<u32>, screenshot }`; `ActionOpts` consolidation (drop `verify`, add `expect: ExpectClause`, add `escalate: EscalatePolicy`); MCP edge `ok` alias for one release
2. `targeting/` module skeleton + composite cache key (`{hwnd, pid, process_start, generation}`)
3. `WindowSignals` + tree-quality probe + child-HWND class collection (incl. cloaked, occluded, foreground eligibility, DWM extended frame bounds)
4. `TargetCandidate` resolution from UIA (refactor existing walker)
5. Pattern-availability-first strategy picker
6. Hit-test gate (`ElementFromPoint` + RuntimeId / 70% bbox-overlap match) before any coordinate click
7. Postcondition contracts for the 5 cheap pattern cases — Invoke, Toggle, Select, ExpandCollapse, SetValue — with 25ms-cadence/250ms-cap polling helper. Subtree-mutation defined as parent's children RuntimeId set diff OR foreground HWND change OR focused element change
8. New OCR thread (`fastuse-win/src/ocr_thread.rs`) — MTA, closure-dispatch, cached `OcrEngine`. Add to D-25 lint allowlist explicitly
9. Cropped progressive OCR (window → likely regions → full) + frame-hash cache
10. Hybrid scoring (UIA + OCR + geometry → ranked candidates)
11. Candidate-kind-dependent escalation ladder, max 2 verified attempts after first; respect `EscalatePolicy::Strict`
12. Bench fixture: L-Connect3 settings click; Discord DM send via OCR fallback; Calculator round-trip with postcondition verification

Wire change (item 1) goes first because every other step touches the response shape. Phase 4 actions inherit the new shape automatically via `dispatch::finalize()` — they emit `verified: false, evidence: Unverified` honestly until v1.1 grows per-action contracts for them.

## Open invariants

- D-24 (PerMonitorV2 first-call): unchanged
- D-25 (no `windows::*` on tokio threads): targeting/ runs on UIA pool + capture thread; OCR runs on **a new dedicated MTA thread** (fourth COM-thread surface, explicitly added to D-25 lint allowlist). Reasoning:
  - Capture thread owns the GPU pipeline (D3D11 device, duplication, staging texture). Routing 30–100ms OCR jobs onto it serializes against every screenshot, multiplying screenshot p50 by 10–30× — including the post-action `screenshot_after` that fires on the same logical action.
  - UIA pool workers are sized for sub-20ms work. With only 3 workers, one in-flight OCR + one verify-poll + one routine query saturates the pool and blows UIA p99.
  - `Windows.Media.Ocr::OcrEngine` has cached language-model state (~50–200ms cold construction). A dedicated thread caches one `OcrEngine` in a `OnceLock` for daemon lifetime without polluting the UIA singleton.
  - Implementation: copy-paste `capture_thread.rs` shape — single MTA thread, closure-dispatch via `Run(Box<dyn FnOnce()>)`, `OcrThreadHandle::run<F,T>` mirroring `UiaPoolHandle::run`. ~150 LOC.
- D-10 / D-19 (Redact wrapping): all OCR results are `Redact<String>` — text on screen is sensitive
- New invariant: **no action returns `verified: true` without a postcondition contract or caller-provided `wait_for` / `expect` matching.** Lint-enforced via xtask check.
