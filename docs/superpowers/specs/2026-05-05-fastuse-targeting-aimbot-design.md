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

Escalation is **two-tier max**, gated by verification:
- pattern → BoundsClick on same UIA bounds → OCR re-resolve

After two failed verified attempts, return rich diagnostic; do not thrash.

## Hit-test gate — mandatory before any coordinate click

Before SendInput at any point — UIA-bounds, OCR-bounds, or caller-provided coords:

1. Compute DPI-correct point (already handled per D-24).
2. Call `IUIAutomation::ElementFromPoint` at that exact point.
3. Verify the hit-test result matches the intended target candidate (same RuntimeId for UIA candidates; bounding-box overlap > 70% for geometry candidates).
4. If mismatch: abort, re-resolve, do not click.

This is the actual aimbot crosshair check. Without it, "click missed" is unobservable.

## Per-action postcondition contracts — the load-bearing piece

The honesty mechanism. Each action type carries its own verification:

| Action | Postcondition | Implementation |
|--------|---------------|----------------|
| Invoke | Foreground/HWND change OR target subtree mutated OR caller `wait_for` matched | One UIA snapshot pre + post |
| Toggle | `ToggleState` flipped (or matches intent) | Read pattern state before+after |
| Select | `SelectionItemPattern.IsSelected == true` after | Read pattern state after |
| ExpandCollapse | `ExpandCollapseState` matches intent | Read pattern state after |
| SetValue (type) | `ValuePattern.Value` contains typed text | Read pattern state after |
| BoundsClick (UIA) | Subtree mutation in target's parent OR caller `wait_for` matched | UIA snapshot pre + post |
| BoundsClick (OCR) | Caller `wait_for` matched (no other honest verifier without semantic context) | Falls back to caller intent |

Generic verification (screenshot diff, raw UIA-root mutation) is **explicitly rejected** as unreliable. Without an applicable postcondition, the response is honest:

```rust
pub struct ActionResult {
    pub attempted: bool,           // strategy ran
    pub verified: bool,            // postcondition matched
    pub evidence: VerificationEvidence,
    pub strategy_used: Strategy,
    pub screenshot: Option<ScreenshotPayload>,  // populated on miss for agent recovery
}

pub enum VerificationEvidence {
    PostconditionMet,    // toggle flipped, value set, selection changed, etc.
    WaitForMatched,      // caller-supplied wait_for selector hit
    HitTestOnly,         // we hit the right pixel; no postcondition available
    Unverified,          // attempted but no contract applicable and no caller hint
}
```

`Unverified` is the new honest state. The existing `ActionOpts.wait_for` / `verify` remain — they become one evidence source among several.

## OCR strategy — cropped progressive

Default scope on degraded UIA: target window only, not full screen. Order:

1. Crop to target window's client rect (excludes title bar, borders).
2. If multiple OCR matches, narrow to UIA-reported scroll containers / panes that *did* resolve.
3. Only fall back to full window if (1)+(2) yield zero matches.

Cache by `frame_hash + region_rect` — when the same window region is OCR'd within ~500ms, reuse the result. Cache invalidated on capture-thread frame-change notification (already exists in `capture_thread`).

Latency target: warm OCR ~30ms (cached), cold OCR <100ms (cropped). Full-window OCR (~150ms cold) only when other strategies fail.

## Wire format changes

`fastuse-proto/src/wire.rs`:

```rust
// Existing Response::ActionResult { ok, wait_matched, waited_ms, screenshot }
// becomes:

Response::ActionResult {
    attempted: bool,
    verified: bool,
    evidence: VerificationEvidence,
    strategy_used: Strategy,
    waited_ms: Option<u32>,
    screenshot: Option<ScreenshotPayload>,
}
```

`ok` is removed — it conflated "attempted" and "verified" and that conflation was the bug. `verified: bool` is the contract callers should branch on.

`Request::ClickElement` etc. add an optional `expect` field for cases where the caller has stronger semantic knowledge than the daemon:

```rust
pub struct ActionOpts {
    pub wait_for: Option<Selector>,
    pub verify: Option<Selector>,
    pub expect: Option<ExpectClause>,   // new
    pub screenshot_after: Option<ScreenshotOpts>,
    pub wait_timeout_ms: Option<u32>,
}

pub enum ExpectClause {
    DialogOpens,
    WindowTitleMatches(String),
    ForegroundChangesTo { class: Option<String>, title: Option<String> },
    SelectorMatches(Selector),     // alias of wait_for, kept for ergonomics
}
```

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

1. `targeting/` module skeleton + composite cache key
2. `WindowSignals` + tree-quality probe + child-HWND class collection
3. `TargetCandidate` resolution from UIA (existing walker, refactored)
4. Pattern-availability-first strategy picker (replaces existing `click_element` core)
5. Postcondition contracts for the 5 cheap pattern cases
6. Hit-test gate before any coordinate click
7. New `ActionResult` wire shape + `ExpectClause`
8. Cropped progressive OCR + frame-hash cache (`fastuse-win/src/ocr/`)
9. Hybrid scoring (UIA + OCR + geometry → ranked candidates)
10. Two-tier verified escalation
11. Bench fixture: L-Connect3 settings click; Discord DM send via `ByText`; Calculator round-trip

## Open invariants

- D-24 (PerMonitorV2 first-call): unchanged
- D-25 (no `windows::*` on tokio threads): targeting/ runs on UIA pool + capture thread; OCR runs on dedicated WinRT thread (new) since `Windows.Media.Ocr` is async-COM and shouldn't share the UIA MTA pool
- D-10 / D-19 (Redact wrapping): all OCR results are `Redact<String>` — text on screen is sensitive
- New invariant: **no action returns `verified: true` without a postcondition contract or caller-provided `wait_for` / `expect` matching.** Lint-enforced via xtask check.
