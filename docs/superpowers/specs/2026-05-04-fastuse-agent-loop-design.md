# fastuse v1.0 — Agent Loop Design

**Date:** 2026-05-04
**Branch:** v1.0
**Status:** Approved, pending implementation plan

## Goal

Make Claude Code, opencode, Codex, Cursor, Cline, and any other MCP-speaking agent drive Windows in **1–2 turns** for tasks that take 5–10 turns on `windows-mcp` or screenshot-only computer-use frameworks.

End-to-end wall-clock target: "open Calculator, compute 23+45=, read result" in **<3 seconds** from cold daemon (LLM time dominates; tool time is negligible).

## Strategy: collapse turns, not microseconds

Per-call RTT is already won. Warm ping is 34µs p50 / 124µs p99. The remaining win for an agent is reducing **turns per task**, because each turn pays an LLM round-trip (500–2000ms) that dwarfs any IPC.

Three levers:

1. **Semantic UIA targeting** as the default path. A UIA tree node is ~20 LLM tokens; a screenshot is ~1500. UIA-first targeting is ~75x cheaper per agent turn in tokens, and tokens are wall-clock on slower models.
2. **Smart parameters** on every primitive (`wait_for`, `screenshot_after`, `verify`) so a single tool call performs perceive→act→perceive atomically.
3. **Universal screenshot+pixel fallback** auto-attached when UIA resolution fails. The agent never burns a turn asking "what went wrong?"; the failure response already contains the data needed to recover.

## Components

### 1. UIA-first targeting with auto-fallback

- `click_element`, `type_into_element`, `wait_for_element`, `scroll_into_view` resolve via the cached UIA tree (already implemented, sub-20ms).
- On miss (degraded tree, no match, ambiguous): the daemon **auto-captures a screenshot of the foreground window** and returns it alongside the error in the *same response*. Agent recovers in the next turn without an extra perceive call.
- Selector grammar stays compact: `ByName`, `ByAutomationId`, `ByControlType`, `ByRole`, plus an `Or` combinator for fuzzy matching (e.g. name OR aria-label OR partial substring match).
- No LLM-in-the-loop on the daemon side. Selector resolution is deterministic and fast.

### 2. Smart parameters on every action

Every action variant in `fastuse-proto/src/wire.rs` grows three optional fields. The same field schema is reused in v1.1's `batch` tool, so the agent learns the vocabulary once.

| Field | Type | Behavior |
|-------|------|----------|
| `wait_for` | `Option<Selector>` | Daemon polls UIA after the action (default 2000ms, configurable) until the selector matches or times out. Replaces the agent's "did it work?" follow-up turn. |
| `screenshot_after` | `Option<ScreenshotOpts>` | Return a screenshot in the same response. Replaces the agent's "what's on screen now?" follow-up turn. |
| `verify` | `Option<Selector>` | Like `wait_for` but if the selector fails to match within the timeout, the action is reported as failed. For QA flows where "the dialog opened" is part of the contract. |

`ScreenshotOpts` is `{ region?: Rect | "auto", format?: "webp" | "png" | "jpeg", quality?: u8 }`. `region: "auto"` captures the foreground window's rect, cutting typical payload size 4–10x.

### 3. Phase-4 wiring + `launch_app` + `warmup`

- Wire `clipboard_get/set`, `shell_exec`, `launch_app`, `list_processes`, `kill_process` into `fastuse-mcp/src/handler.rs` and `fastuse-cli/src/cmd_phase4.rs`. The daemon already dispatches these with permission-tier gating.
- `launch_app` resolves: `.exe` on PATH, `ms-*:` URI schemes, AUMID for UWP, Start Menu app name. Returns the spawned PID and the first foreground HWND that matches the launched process. `.lnk` shortcuts and arbitrary AUMID resolution deferred to v1.1.
- `warmup` tool + auto-warmup on daemon spawn: touches the D3D11 device, DXGI Desktop Duplication object, UIA root element, foreground HWND cache, and monitor enumeration. First agent call after a cold daemon is already hot.
- `clipboard_get_image` / `clipboard_set_image` codec wiring (uses `image` 0.25, ~50 LOC).

### 4. Screenshot strategy

- Default format: **WebP** (lossy quality 90). ~2x smaller payload than PNG, ~3x faster encode at typical UI resolutions, visually indistinguishable for screenshot use. Fall back to PNG if the MCP client does not negotiate WebP support.
- Capture API: DXGI Desktop Duplication (already implemented, ~1–4ms per frame). Per-window WGC capture deferred to v1.1.
- New `region: "auto"` mode = the current foreground window's bounds. Cuts payload size 4–10x for typical single-app screenshots.

## v1.1 Hooks (designed-compatible)

These are deferred but the v1.0 design does not foreclose them.

- **`batch` tool** — takes `Vec<Step>` where each `Step` reuses the *same* `wait_for` / `screenshot_after` / `verify` schema. Zero re-learning for the agent. Enables one-turn QA scripts: `batch([launch("myapp"), wait_for("File menu"), click("File"), wait_for("Open"), click("Open"), screenshot])`.
- **Element handles** — `uia_query` returns a stable opaque handle; `click_element` accepts `handle | selector`. Skips re-resolution on repeated interactions with the same element.
- **Per-window WGC capture** — for capturing a specific HWND without compositing the whole desktop.

## Non-goals for v1.0

- No fuzzy / natural-language selectors beyond the `Or` combinator. LLM-in-the-loop selector resolution defeats the latency goal.
- No remote / HTTP MCP transport. stdio covers Claude Code, opencode, Codex, Cursor, Cline.
- No code signing, no MSI installer. Portable zip + documented SmartScreen workaround.
- No `.lnk` or arbitrary UWP AUMID resolution in `launch_app`. Returns `AppNotFound` for those; v1.1.
- No recipe library exposed as MCP tools. Recipes live as bench fixtures in `crates/fastuse-cli/src/bench/`.
- No multi-client auto-installer. README copy-paste config snippets for v1.0.

## Success criteria

| Metric | Target |
|--------|--------|
| "Open Calculator, type 23+45=, read result" | ≤3 agent turns from cold daemon |
| "QA: open Rust app, File→Open, verify dialog appears, cancel" | ≤4 agent turns |
| Tool RTT, no perception | p50 <50ms |
| Tool RTT, with `screenshot_after` | p50 <150ms |
| Cold first-call after daemon spawn | <500ms (warmup-amortized) |
| vs `windows-mcp` on same scenarios | ≥5x faster wall-clock |

Bench harness reports p50 and p99 only. `mean` is lint-blocked — averages hide the tail latency that matters for agent UX.

## Calculator example, end-to-end

Naive flow on `windows-mcp` or screenshot-only computer-use: 6–8 turns.

With fastuse v1.0:

1. **Turn 1.** `launch_app("calc", wait_for={ByName:"Display is 0"})` → returns when Calculator is open and ready.
2. **Turn 2.** `type("23+45=", wait_for={ByControlType:"Text", contains:"68"})` → returns when the answer is rendered.
3. **Turn 3 (optional).** Agent reads the answer node via `uia_query` if it needs the value as data.

3 turns vs 6–8. At ~800ms LLM time per turn: ~2.4s vs ~6s wall-clock. That is the felt speedup.

## Implementation order

The existing v1.0 roadmap (in `CLAUDE.md`) already orders this work correctly. The remaining items are:

1. Wire Phase 4 dispatchers (clipboard, shell, launch_app, processes) into MCP + CLI.
2. Clipboard image round-trip codec.
3. `wait_for` / `screenshot_after` / `verify` parameters on every action variant.
4. `warmup` tool + auto-warmup on daemon spawn.
5. `doctor` and `logs` CLI subcommands.
6. Criterion microbenches per primitive (cold / p50 / p99).
7. `fastuse bench windows-mcp` comparator harness.
8. `cargo xtask dist` portable zip.
9. `fastuse install --client claude-code` config patcher.

Items 1, 3, 4 deliver the agent-loop wins this design is about. Items 5–9 are distribution and validation.
