# app-ensure — Parallel App State Resolution

**Date:** 2026-05-06
**Status:** Approved

## Problem

When an AI is told to interact with a Windows app (e.g. "open Calculator"), it currently issues 3–4 sequential CLI calls to check process state, find the window, restore if minimized, and focus — each incurring a named-pipe round-trip. Weaker models stall or make incorrect decisions between steps. Total latency: 4–6 AI turns, 200–600ms of round-trips.

## Goal

Collapse app-open into a single call that runs all state reads in parallel, executes the minimal correction actions, and returns a "ready" result with an optional screenshot. AI prompt complexity: two lines.

## Command Interface

```
fastuse-cli app-ensure <name> [OPTIONS]

Arguments:
  <name>    App name, partial title, or process stem (e.g. "Calculator", "calc")

Options:
  --screenshot-after     Capture screenshot once app is ready, return inline
  --timeout-ms <N>       Wait budget for launch + window appearance [default: 5000]
  --instant              Disable humanized mouse movement for the focus step
```

### Success output (JSON stdout)

```json
{
  "status": "ready",
  "hwnd": 394716,
  "pid": 14832,
  "title": "Calculator",
  "launched": false,
  "was_minimized": true,
  "was_background": true,
  "actions_taken": ["restored", "focused"],
  "elapsed_ms": 38,
  "screenshot": null
}
```

### Error output

```json
{ "status": "error", "reason": "launch_timeout" | "permission_required" | "not_found" }
```

### AI prompt addition

> To open any app: `app-ensure <name> --screenshot-after`. Read `status` and `hwnd`. If `ready`, proceed.

## Architecture

### Wire protocol additions (additive — no renumbering)

```rust
// fastuse-proto/src/wire.rs
Request::AppEnsure {
    name: String,
    timeout_ms: u32,
    screenshot_after: Option<ScreenshotOpts>,
}

Response::AppEnsure {
    hwnd: u64,
    pid: u32,
    title: String,
    launched: bool,
    was_minimized: bool,
    was_background: bool,
    actions_taken: Vec<String>,
    elapsed_ms: u64,
    screenshot: Option<ScreenshotPayload>,
}
```

### Daemon state machine

**Phase 1 — Parallel reads** (`tokio::join!`):
- `list_processes(name)` — find running PIDs matching name/stem
- `list_windows(name)` — find HWNDs with state flags (minimized, visible, foreground)

Cost: `max(t_proc, t_win)` instead of `t_proc + t_win`. Typical: ~3ms instead of ~8ms.

**Phase 2 — Sequential corrections** (minimal set):

| State | Action |
|-------|--------|
| Process not running | `launch_app(name)` → `wait_for_window(name, timeout_ms)` |
| Running, no visible window | `ShowWindow(hwnd, SW_RESTORE)` |
| Window minimized | `ShowWindow(hwnd, SW_RESTORE)` |
| Window not foreground | `focus_window(hwnd)` |
| Already foreground | no-op |

**Phase 3 — Optional screenshot** (if `screenshot_after` set): captured after focus settles, returned inline in `Response::AppEnsure`.

### Permission gate

`launch_app` within `app-ensure` is gated identically to `Request::LaunchApp`. If `FASTUSE_SAFE_MODE=1` or the permission is absent, returns `{"status": "error", "reason": "permission_required"}` before any launch attempt. Restore + focus actions are always allowed.

## MCP Tool

```
mcp__fastuse__app_ensure({
  name: "Calculator",
  screenshot_after: true,
  timeout_ms: 5000
})
```

Returns the same JSON structure. When `screenshot_after: true`, response includes `ImageContent` inline — app open + screenshot in a single tool call.

**AI loop before:**
1. `launch_app` → wait
2. `list_windows` → get hwnd
3. `focus_window` → wait
4. `computer screenshot` → see state

**AI loop after:**
1. `app_ensure(name, screenshot_after: true)` → ready + screenshot

### Affected crates

| Crate | Change |
|-------|--------|
| `fastuse-proto` | Add `Request::AppEnsure`, `Response::AppEnsure` |
| `fastuse-daemon` | Add handler: parallel reads + correction state machine |
| `fastuse-cli` | Add `app-ensure` subcommand |
| `fastuse-mcp` | Add `app_ensure` tool |

## Non-goals

- This does not replace individual `focus-window`, `launch-app`, or `list-windows` commands — those remain for cases where the AI needs fine-grained control.
- No fuzzy name matching beyond what `list-windows --title` and `list-processes --name` already do (substring match).
- No multi-monitor window placement logic.
