# fastuse v2 — vision-first computer-use design

**Date:** 2026-05-05
**Status:** Design (pre-implementation)
**Predecessor:** `v1.0-pre-pivot` tag (architecture rebuild, not iteration)

## Why

v1's targeting layer (UIA tree → OCR fallback → click) is brittle on adversarial apps (Electron, custom-rendered WPF, Direct2D). Three hours of debugging on L-Connect3 demonstrated this in production. Research across the field (Anthropic's `computer_20251124`, Microsoft OmniParser, ByteDance UI-TARS, OpenAI Codex Computer Use, OSWorld benchmark) converges on **vision-first agents with model-side targeting**. Claude Opus 4.7 hits 78% on OSWorld with the vision-only `computer` tool — well past anything UIA-first reaches.

v2 pivots fastuse from "targeting brain on the daemon" to "fast Windows primitives + Claude does targeting." The valuable work (sub-50ms RTT, MTA-isolated COM, DXGI capture, named-pipe IPC, elevated daemon, input-thread serialization) stays. The brittle layers (OCR pipeline, candidate scoring, escalation ladder, hit-test gates) get deleted.

## Locked decisions

| | Choice | Rationale |
|---|---|---|
| Primary consumer | **Claude Code** | Other agents (OpenRouter routes, Cursor, Codex) work via MCP if compatible, but design favors Claude Code |
| Surface shape | **MCP-primary, CLI as ops/scripting fallback** | MCP returns `ImageContent` inline (saves a turn vs file-read on Bash). On Windows, MCP is also marginally faster than CLI in steady state because Bash spawns a process per call (~15-30ms cold) while MCP holds an open stdio connection |
| Tool surface | **Anthropic `computer_20251124` schema fidelity + Windows-specific helpers as separate MCP tools** | Matches Claude's RL training; Windows helpers are the moat over Anthropic's Linux-Docker reference |
| Screenshot strategy | **Default scaled XGA, `zoom` action for fidelity escape hatch** | Anthropic's reference already converged here; matches Claude's training |
| Multi-monitor | **Foreground-monitor by default, opt-in `monitor: N`** | Mirrors how a human focuses; avoids fidelity collapse on multi-mon setups |
| Permission model | **Open by default, opt-in safe-mode** | "Like a human" — humans don't need permission to launch Notepad. Safe-mode gates destructive tools when needed |
| Targeting layer | **Deleted entirely; UIA kept as read-only inspection** | One action surface (vision+coords); no parallel targeting brain. UIA tools become "DevTools-style" structure inspection |
| Migration | **Hard cut on `v1.0` branch** | Solo project; `v1.0-pre-pivot` tag preserves rollback |
| Action batching | **Not built** | Drag covered by atomic `left_click_drag`; multi-step batches lose self-verification benefit |
| Anti-cheat | **Out of scope** | No bypass, no awareness, no kernel driver |
| Input humanization | **Tier 1 (Bezier curves + timing jitter) on by default** | Defeats web bot detection (Google Play Console scenario). Opt-out via `--instant` / `humanize: false` |
| Hardware HID injection | **Architecturally provisioned (`InputBackend` trait), firmware deferred** | Future v3 project; not v2 scope |

## Goals

A Windows desktop control plane that lets Claude Code drive any Windows GUI application like a human would: see the screen, move the mouse, click, type, scroll, drag, switch windows, read text. Universal across cooperative apps (Calculator, browsers, IDEs) and adversarial apps (Electron, Direct2D, custom-rendered WPF) because the substrate is vision, not accessibility-tree introspection.

## Non-goals

- Linux / macOS support
- Cloud-hosted deployment (Operator-style remote VM)
- Multi-user concurrent access
- Anti-cheat circumvention or stealth from kernel-mode security software
- Hardware HID firmware (architecture supports it post-v2)
- Replacing accessibility-tree-first tools for users who specifically want them

## Success criteria

Tiered evaluation. v2 ships when:

- **Tier 1 — Cooperative-app fluency.** Claude reliably drives Calculator, Notepad, Chrome (basic browsing), Discord (send a message), VS Code (open file, edit, save). Pass rate ≥95% across automated runs.
- **Tier 2 — Adversarial-app fluency.** L-Connect3 (motivating case), Photoshop or GIMP, Steam launcher, a custom Tauri/Iced/egui app. Pass rate ≥80%.
- **Tier 3 — Multi-step task chains.** "Open Discord → DM user → screenshot response → paste in OneNote → save." Mixed-app workflows on tasks not seen during development. Pass rate ≥60%.
- **Specific use case — GUI testing of compiled apps.** Claude Code compiles a project, launches the .exe, drives the GUI to test features. First-class workflow.
- **Specific use case — web automation.** Driving Chrome (e.g., Google Play Console) with Tier 1 humanization defeating standard web bot detection.

## Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│                       Claude Code (agent)                         │
│  ┌──────────────────────────┐    ┌──────────────────────────┐   │
│  │  MCP tool calls          │    │  Bash tool calls          │   │
│  │  (primary surface)       │    │  (manual / scripting)     │   │
│  └────────────┬─────────────┘    └────────────┬─────────────┘   │
└───────────────┼───────────────────────────────┼─────────────────┘
                │ stdio                         │ exec
                ▼                               ▼
     ┌──────────────────────┐        ┌──────────────────────┐
     │   fastuse-mcp        │        │   fastuse-cli        │
     │   (long-running      │        │   (short-lived       │
     │    MCP server)       │        │    process)          │
     └──────────┬───────────┘        └──────────┬───────────┘
                │ named pipe                     │ named pipe
                ▼                               ▼
     ┌──────────────────────────────────────────────────────────┐
     │             fastuse-daemon (single instance)              │
     │  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌──────────┐    │
     │  │ Capture  │ │ UIA pool │ │ Input    │ │ Window   │    │
     │  │ thread   │ │ (3 work) │ │ thread   │ │ helpers  │    │
     │  │ (DXGI)   │ │ (MTA COM)│ │ (STA)    │ │ (sync)   │    │
     │  └──────────┘ └──────────┘ └──────────┘ └──────────┘    │
     └──────────────────────────────────────────────────────────┘
```

### Crates after the rebuild

- **`fastuse-proto`** — wire types. Major rewrite: drop targeting types (`ActionResult`, `Strategy`, `VerificationEvidence`, `ExpectClause`, `EscalatePolicy`); add `computer` action enum + Windows helper request/response types.
- **`fastuse-core`** — local-app-data paths, redaction, error enum. Lightly touched.
- **`fastuse-win`** — Windows work. **Major restructuring.** Targeting/OCR modules deleted. New modules: `scaling.rs`, `input/humanize.rs`, `permissions.rs`. `InputBackend` trait introduced.
- **`fastuse-daemon`** — pipe server, dispatch, lifecycle. Lightly touched (dispatch handlers rewritten for new wire types).
- **`fastuse-cli`** — clap derive structure rewritten. New subcommand tree mirroring `computer` actions + Windows helpers + ops tools.
- **`fastuse-mcp`** — **expanded significantly.** Currently a thin shim; v2 it's the primary consumer surface. Implements `computer_20251124` as one MCP tool plus separate Windows helper tools and read-only UIA inspection tools.
- **`xtask`** — lints unchanged.

### Threads after the rebuild

- Capture thread (DXGI, single MTA worker) — unchanged
- UIA pool (3 MTA workers) — unchanged
- Input thread (STA) — unchanged threading; new dispatch interface via `InputBackend` trait
- **OCR thread retired** — deleted with the OCR module

### Daemon lifecycle

Unchanged from v1.0-pre-pivot:
- Auto-spawn elevated via UAC `runas` on first CLI/MCP call
- Idle-timeout after 5 minutes
- Sentinel + zombie eviction at `%LOCALAPPDATA%\fastuse\daemon.pid`
- Named-pipe IPC at `\\.\pipe\fastuse-{session}-{user}`

### MCP server hosting

`fastuse-mcp` registers with Claude Code via `.claude/settings.json` (or user-level config). Claude Code spawns it as a long-lived stdio child; it stays connected to the same daemon as the CLI uses. Two surfaces, one daemon, one source of truth.

## Tool surface

Three categories on the MCP side. CLI mirrors action tools 1:1; ops/inspection-only on the CLI for the others.

### The `computer` tool (Anthropic schema fidelity)

One MCP tool named `computer`, matching Anthropic's `computer_20251124` schema verbatim. Action enum:

| action | Required fields | Notes |
|---|---|---|
| `screenshot` | — | Returns scaled image (XGA-ish), foreground monitor by default |
| `left_click` | `coordinate: [x, y]` | Optionally `text: "ctrl+shift"` for modifier-click |
| `right_click` | `coordinate` | |
| `middle_click` | `coordinate` | |
| `double_click` | `coordinate` | |
| `triple_click` | `coordinate` | |
| `left_click_drag` | `start_coordinate, coordinate` | Atomic drag — covers ~95% of drag needs |
| `left_mouse_down` | `coordinate` (optional) | Composable for modifier drags |
| `left_mouse_up` | `coordinate` (optional) | |
| `mouse_move` | `coordinate` | |
| `cursor_position` | — | Returns current cursor `[x, y]` |
| `type` | `text: string` | Whole string at once; humanized intervals between keys |
| `key` | `text: "ctrl+l"` or `"enter"` | xdotool-style chord syntax |
| `hold_key` | `text, duration: ms` | |
| `scroll` | `coordinate, scroll_direction, scroll_amount` | |
| `wait` | `duration: ms` | |
| `zoom` | `coordinate, zoom_factor` (or `region` variant) | New in `computer_20251124` — crops + upscales region to standard XGA. Fidelity escape hatch. |

Coordinates always in **scaled image-pixel space** that Claude sees in the screenshot. Daemon scales back to native virtual-desktop pixels before dispatch.

Extension fields (additive, beyond the published schema):
- `humanize: bool` (default true) — opt-out of humanized motion/typing for `*_click`, `mouse_move`, `*_click_drag`, `type`, `scroll`
- `monitor: u32` (for `screenshot` only) — explicit monitor selection

### Windows-specific helper tools

These don't exist in Anthropic's schema. The Windows-native moat.

| Tool | Purpose |
|---|---|
| `list_windows` | Filter by title substring or process name; returns hwnd, bounds, title, process |
| `focus_window` | Bring HWND to foreground; handles foreground-lockout |
| `foreground_window` | Current foreground HWND + metadata |
| `wait_for_window` | Poll for a window matching predicate; up to timeout |
| `list_processes` | All processes or filtered by name; visible-only flag |
| `kill_process` | By pid or name (gated when safe_mode on) |
| `launch_app` | Start a process by name/path/URI (gated when safe_mode on) |
| `shell_exec` | Run a shell command (gated when safe_mode on) |
| `clipboard_get_text` | Read clipboard text |
| `clipboard_set_text` | Write clipboard text (gated when safe_mode on) |

### UIA read-only inspection

Pure information tools. No action dispatch. Claude uses these like a developer uses DevTools.

| Tool | Purpose |
|---|---|
| `inspect_at` | UIA element under a pixel: `control_type, name, automation_id, bounds`. Read-only. |
| `uia_query` | Selector-driven element lookup (existing JSON selector grammar: `ByName`/`ByControlType`/`ByClass`/`ByAutomationId`/`And`/`Or`/`Not`). Returns matching elements with bounds. Read-only. |
| `uia_tree` | Full UIA tree dump for a window. Debugging. Read-only. |

**No `click_element`, no `type_into_element`, no `wait_for_element`.** The action surface is exclusively `computer` with coordinates.

### Daemon meta tools

| Tool | Purpose |
|---|---|
| `ping` | Liveness, returns RTT, daemon_pid, session_id |
| `status` | Daemon health, uptime, idle countdown |
| `stop` | Graceful shutdown |
| `warmup` | Touch DXGI + UIA + monitors to amortize cold-start |

### CLI mapping

CLI mirrors `computer` actions and Windows helpers as subcommands.

```
fastuse-cli computer screenshot --out file.jpg [--monitor N]
fastuse-cli computer left-click X Y [--instant]
fastuse-cli computer right-click X Y
fastuse-cli computer type "text" [--instant]
fastuse-cli computer key ctrl+s
fastuse-cli computer scroll X Y --direction down --amount 3
fastuse-cli computer drag SX SY EX EY [--instant]
fastuse-cli computer zoom X Y --factor 2.5
fastuse-cli list-windows --title "Discord"
fastuse-cli focus-window HWND
fastuse-cli foreground-window
fastuse-cli wait-for-window --title "L-Connect" --timeout-ms 5000
fastuse-cli inspect-at X Y
fastuse-cli uia-query '{"ByName":"OK"}'
fastuse-cli list-processes [--name <substr>] [--visible-only]
fastuse-cli kill-process pid:1234
fastuse-cli launch-app calc.exe
fastuse-cli shell-exec "cmd"
fastuse-cli clipboard-get-text
fastuse-cli clipboard-set-text "text"
fastuse-cli ping | status | stop | warmup
fastuse-cli setup-mcp [--user | --project]
```

CLI subcommands take **native virtual-desktop coordinates** (no scaling). MCP applies scaling because Claude sees a scaled image.

Total tool count: ~18 MCP tools (1 `computer` + 10 Windows helpers + 3 UIA inspection + 4 meta).

## Coordinate scaling

### The problem

Claude sees a scaled screenshot (~1024 longest side). Click coordinates Claude returns are in *that* scaled image's space. Daemon must reverse the scaling to native virtual-desktop pixels.

### Math: scale-to-fit, preserve aspect

```
native_w, native_h = monitor dimensions (after DPI scaling, physical pixels per Per-Monitor V2)
target_max = 1024 (configurable via env / config)
ratio = max(native_w, native_h) / target_max
scaled_w = native_w / ratio
scaled_h = native_h / ratio
```

Single uniform ratio for x and y. Aspect preserved. No letterboxing.

### Forward scale (native → scaled, on screenshot output)

```rust
fn scale_to_image(native: Rect, ratio: f64) -> Rect {
    Rect {
        x: (native.x / ratio) as i32,
        y: (native.y / ratio) as i32,
        w: (native.w / ratio) as i32,
        h: (native.h / ratio) as i32,
    }
}
```

### Inverse scale (scaled coord → native, on click input)

```rust
fn scale_to_native(scaled: Point, ratio: f64, monitor_origin: Point) -> Point {
    Point {
        x: (scaled.x as f64 * ratio).round() as i32 + monitor_origin.x,
        y: (scaled.y as f64 * ratio).round() as i32 + monitor_origin.y,
    }
}
```

### State management — last-screenshot-wins, scoped per session

Daemon tracks the most recent screenshot's scale state per session:

```rust
struct ScaleContext {
    ratio: f64,
    monitor_origin: Point,
    scaled_dims: (u32, u32),
    captured_at: Instant,
}
```

Click coords interpreted in *that* screenshot's space. Each new screenshot updates the state. Stack-based: `zoom` pushes new context; next `screenshot` pops back to fullscreen.

Failure mode: click without recent screenshot → daemon returns error `"no scale context — call screenshot first"`. Click out of bounds of last-screenshot → daemon returns error with the actual bounds. Recoverable.

### Multi-monitor

`screenshot` without monitor argument: daemon reads foreground HWND, finds containing monitor via `MonitorFromWindow`, captures that monitor only. Scale state records the monitor's virtual-desktop origin.

`screenshot { monitor: N }`: capture monitor N explicitly.

Foreground spans monitors: pick the monitor with larger overlap.

### `zoom` action

Takes a region in scaled space, crops native pixels for that region, upscales to standard target dimensions. Returns image + new scale state. Click coords against zoomed image translate via the zoom's scale state. Stack-based: each `zoom` pushes context; `screenshot` pops back to fullscreen.

### High-DPI

Per-Monitor V2 awareness already enforced (D-24 invariant). Native pixels = physical pixels regardless of system scaling. Math is unaffected.

### Performance

- Scaling math: microseconds
- Image resampling (1440p → 1024-wide): ~5-15ms on a single core
- Acceptable inside the 150ms screenshot budget
- If 4K becomes hot: swap to `mtpng` or move to GPU compute shader (post-v2)

## Permission model

### Default: wide open

All tools available without gating. Rationale: "like a human." A human doesn't get prompted before launching Notepad.

### Safe mode: opt-in lockdown

Triggered three ways, in priority order:

1. **Per-session env var:** `FASTUSE_SAFE_MODE=1` set in daemon's spawn environment. One-shot.
2. **Per-call env var (CLI only):** `FASTUSE_SAFE_MODE=1 fastuse-cli launch-app calc.exe` honored per-request.
3. **Persistent config:** `%LOCALAPPDATA%\fastuse\config.toml`:
   ```toml
   [permissions]
   safe_mode = true
   gated_tools = ["launch_app", "kill_process", "shell_exec", "clipboard_set_text", "clipboard_set_image"]
   ```

Gated tools return `Error::PermissionRequired` with hint:
```json
{
  "error": {
    "code": "PERMISSION_REQUIRED",
    "message": "tool 'kill_process' is gated in safe mode",
    "hint": "set FASTUSE_SAFE_MODE=0 to disable, or remove from gated_tools in config.toml"
  }
}
```

### Default gated set (safe_mode=true)

- `launch_app` — could spawn arbitrary executables
- `kill_process` — can kill user's work or system processes
- `shell_exec` — arbitrary command execution
- `clipboard_set_text` — overwrites user's clipboard contents
- `clipboard_set_image` — same

**Not gated even in safe_mode:** `computer` (visible to user), window operations, UIA inspection, process listing, clipboard read.

### Explicitly not implemented

- No per-action confirmation prompts (breaks agent loop)
- No per-app permissions (use VM or separate user account if needed)
- No anti-cheat awareness (out of scope per locked decision)

## Input pipeline (humanization)

### `InputBackend` trait

```rust
pub trait InputBackend: Send + Sync {
    fn dispatch(&self, action: InputAction) -> Result<(), InputError>;
    fn capabilities(&self) -> Capabilities;
    fn name(&self) -> &'static str;
}

pub enum InputAction {
    MouseMove { from: Point, to: Point, profile: MotionProfile },
    MouseClick { button: MouseButton, count: u8, modifiers: Modifiers },
    MouseDown { button: MouseButton },
    MouseUp { button: MouseButton },
    Drag { from: Point, to: Point, profile: MotionProfile, modifiers: Modifiers },
    KeyType { text: String, profile: TypingProfile },
    KeyChord { keys: Vec<VirtualKey>, hold_ms: Option<u32> },
    Scroll { at: Point, direction: ScrollDirection, amount: i32 },
}

pub struct MotionProfile {
    pub humanize: bool,
    pub duration_ms: Option<u32>,
    pub jitter: f32,
}

pub struct TypingProfile {
    pub humanize: bool,
    pub mean_interval_ms: u32,    // default ~80ms
    pub interval_stddev_ms: u32,  // default ~30ms
}
```

### Default `SendInputBackend` with Tier 1 humanization

**Mouse motion.** Bezier curve interpolation, not teleport:
- Cubic Bezier from `from` to `to`, two control points offset perpendicular to path (natural curvature)
- Sample at ~60Hz over motion duration
- Duration scales with distance: ~150ms for short moves, up to ~500ms for cross-screen
- Per-sample velocity follows a bell curve (slow start → fast middle → slow end) — matches biomechanical motor output
- Each waypoint dispatched as `SendInput(MOUSEEVENTF_MOVE)` with ±1-2px sample-level jitter

**Click timing.** ±50ms jitter on click duration; mouse-down to mouse-up gap ~30-100ms.

**Keystroke timing.** Inter-key intervals from normal distribution (mean 80ms, stddev 30ms). Occasional natural pauses every 8-15 keys (~200-400ms). No simulated typos/backspaces.

**Drag motion.** Same Bezier as `MouseMove`, with `MouseDown` at start and `MouseUp` at end. Path decelerates near target.

**Scroll.** Discrete wheel ticks (not smooth-scroll), small random delays between ticks, ±10% jitter on amount.

### Opt-out: instant mode

Per-call override. CLI flag `--instant`; MCP `computer` action accepts `humanize: false`.

```bash
fastuse-cli computer left-click 100 200 --instant
```

```json
{ "action": "left_click", "coordinate": [100, 200], "humanize": false }
```

Single `SendInput` dispatch, no interpolation. Sub-millisecond. For driving your own apps, automated tests, internal tooling.

### Future `HardwareHidBackend` (deferred)

Stub trait impl. Same `InputBackend` contract. Discovers Pico via USB VID/PID, talks via CDC serial, sends high-level commands; firmware does interpolation. v3 follow-up project with own spec.

### Explicitly not implemented

- Tier 3 behavioral fingerprint matching (gaze, session-level rhythm)
- Per-app input policies
- Randomized typo / backspace correction

## Migration: what survives, what dies

Hard cut on `v1.0` branch. `v1.0-pre-pivot` tag preserves rollback.

### Deleted entirely

- `crates/fastuse-win/src/targeting/` — execute, profile, candidate, strategy, hit_test, verify (~3000 lines)
- `crates/fastuse-win/src/ocr/` — cropped, cache (~600 lines)
- `crates/fastuse-win/src/ocr_thread.rs` (~150 lines)
- Wire targeting types in `fastuse-proto/src/wire.rs` — `ActionResult`, `Strategy`, `VerificationEvidence`, `ExpectClause`, `EscalatePolicy` (~400 lines)
- CLI subcommands: `click-element`, `type-into-element`, `wait-for-element`
- Lint rules policing the targeting module
- Hybrid scoring system

Total: ~40-50% of `fastuse-win` lines.

### Surviving with light touch

- `fastuse-win/src/capture/` — DXGI capture. Add scaling on output.
- `fastuse-win/src/capture_thread.rs` — no changes
- `fastuse-win/src/uia/` — automation root, cache, query. Becomes read-only inspection.
- `fastuse-win/src/uia_pool.rs` — no changes
- `fastuse-win/src/input/` — refactored to implement `InputBackend` trait; humanization layer added
- `fastuse-win/src/input_thread.rs` — no changes to threading; new dispatch interface
- `fastuse-win/src/window/` — no changes
- `fastuse-win/src/{clipboard,process,launch,shell}/` — no changes (still gated when safe_mode active)
- `fastuse-daemon/` — dispatch handlers rewritten for new wire types
- `fastuse-cli/` — clap structure rewritten

### New work

- `fastuse-mcp/` — currently a thin shim. Becomes primary consumer surface. Implement `computer` tool + Windows helpers + UIA inspection + meta tools. `ImageContent` return. Permission gate enforcement.
- `fastuse-win/src/scaling.rs` — stateful scale context per session; forward + inverse math.
- `fastuse-win/src/input/humanize.rs` — Bezier curve generation, motion profiles, typing profiles.
- `fastuse-win/src/input/mod.rs` — `InputBackend` trait. `SendInputBackend` impl. `HardwareHidBackend` stub.
- `fastuse-win/src/permissions.rs` — gating logic, env + config resolution.
- `fastuse-core/src/config.rs` (or similar) — load `%LOCALAPPDATA%\fastuse\config.toml`. Sections: `[permissions]`, `[input]`, `[scaling]`. Optional file; defaults if absent.

### Preserved invariants

D-10 (Redact wrapping), D-21 (pipe paths), D-24 (PerMonitorV2 first call), D-25 (no `windows::*` on tokio threads), D-26 (DXGI on capture thread only). Lint enforcement via xtask continues.

### Implementation sequencing (preview — refined by writing-plans)

1. Branch context: `v1.0` working branch; `v1.0-pre-pivot` tag covers rollback
2. Demolition commit: rip out `targeting/`, `ocr/`, related CLI subcommands, related wire types
3. `InputBackend` trait + refactor existing input dispatch behind it (no humanization yet)
4. Humanization layer; default-on for `MouseMove` and `KeyType`; CLI `--instant` flag
5. Coordinate scaling state machine; hooks into screenshot output and click input
6. Rewrite wire types around `computer` action enum + Windows helpers
7. Rewrite daemon dispatch handlers
8. Rewrite CLI subcommand tree
9. `fastuse-mcp` rewrite: `computer` tool with all actions, Windows helpers, UIA inspection, meta tools, permission gating, image-content return
10. Config system
11. Tier 1/2/3 evaluation suites
12. CLAUDE.md rewrite, README, docs
13. Tag `v2.0`

Roughly 10-13 atomic commits.

## Testing strategy

### Layer 1 — Unit tests

Pure-logic tests via `cargo test --release --workspace --lib --bins`. Inheriting ~80-100 surviving tests after demolition (down from 176+ baseline because targeting tests die with the modules). New tests for:
- `scaling.rs` — forward/inverse math, round-trip identity, multi-monitor offset
- `humanize.rs` — Bezier curve monotonic timestamps, typing interval distribution, motion sample counts
- `permissions.rs` — config parse, env precedence, default gated set
- Wire types — serde round-trip for `computer` action enum, Windows helper requests/responses
- `InputBackend` trait — mock backend records calls; verify routing

### Layer 2 — Integration tests

Drive daemon via wire protocol. Test:
- Pipe handshake
- Each MCP tool's request/response round-trip
- Coordinate scaling state persists across calls within a session
- Safe-mode gates fire correctly
- Daemon lifecycle (auto-spawn, idle-timeout, zombie eviction)

Run via `cargo test --release` with feature flag `integration` (existing pattern).

### Layer 3 — End-to-end evaluation suite

Where the design's "universal, like a human" claim gets validated.

**Tier 1 — Cooperative-app fluency** (deterministic, automated, nightly):
- Calculator: arithmetic, memory operations, mode switching
- Notepad: open file, edit, save, close
- Chrome: open URL, fill form, click button, verify navigation
- VS Code: open file via command palette, edit, save, run task
- Discord: focus app, switch channel, type message, send

10 runs each. Pass rate target ≥95%.

**Tier 2 — Adversarial-app fluency** (semi-automated, weekly):
- L-Connect3: Settings tab, system info button, fan curve adjustment
- Photoshop / GIMP: open image, basic edit, save
- Steam launcher: search game, click play
- Custom Tauri/Iced/egui app

Pass rate target ≥80%. Failures captured with screenshot + trace for triage.

**Tier 3 — Multi-step task chains** (manual, monthly review):
- "Open Discord, find user X, send message Y, screenshot the response, paste in OneNote, save"
- "I just compiled my Tauri app — open it, verify the new search feature works, report back"
- "Take a screenshot of the Google Play Console dashboard and tell me what's restricted"

Pass rate target ≥60%. Recorded as session transcripts.

Tooling: `crates/fastuse-eval` binary orchestrates Claude Code via headless invocation, captures transcript, runs success check, records pass/fail.

### Layer 4 — Performance regression (criterion benches)

Targets (see Performance section):
- `screenshot` <150ms p95
- `computer left_click` instant <50ms p95
- `computer left_click` humanized 200-500ms (intentional; budget ≤600ms)
- `uia_query` <20ms p95
- `ping` RTT <500µs

Existing criterion setup; new benches for scaling math and humanization curve generation.

### Layer 5 — CI matrix

GitHub Actions on each PR:
- Build all crates
- `cargo test` (Layer 1 + 2)
- `cargo run -p xtask -- lints` (D-invariants enforcement)
- `cargo build --release` (smoke)

Layer 3 nightly on self-hosted Windows runner. Layer 4 weekly with regression alerts.

### Out of scope

- End-to-end test of MCP transport (trust `rmcp` 1.6 test suite)
- Browser bot-detection regression suite (moving target; spot-check)
- Anti-cheat regression suite (out of scope per locked decision)

## Performance budgets

### Tool latency targets (p95, no contention)

| Tool | Target | Notes |
|---|---|---|
| `ping` | <500µs | Daemon RTT only |
| `computer screenshot` (scaled) | <150ms | Capture + scale + JPEG encode + base64 |
| `computer screenshot` (region/zoom) | <100ms | Smaller payload |
| `computer left_click` (instant) | <30ms | Single SendInput |
| `computer left_click` (humanized) | 200-500ms | Bezier motion duration (intentional) |
| `computer type "hello world"` (instant) | <20ms | Burst SendInput |
| `computer type "hello world"` (humanized) | ~900ms | 11 chars × ~80ms (intentional) |
| `computer key ctrl+s` | <30ms | Always instant |
| `computer scroll` | <50ms | A few wheel ticks |
| `computer left_click_drag` | 300-800ms | Humanized drag (intentional) |
| `list_windows` | <30ms | EnumWindows + filter |
| `focus_window` | <50ms | SetForegroundWindow + AttachThreadInput |
| `wait_for_window` | depends on `timeout_ms` | 50ms poll cadence |
| `inspect_at` | <20ms | UIA element-from-point with cache |
| `uia_query` | <50ms | Cache-backed |
| `clipboard_get_text` | <10ms | OpenClipboard fast path |

### Cold-start budgets

- First call after daemon spawn: <130ms (DXGI init, UIA root, monitor enum, MTA pump warmup amortized)
- First screenshot specifically: <300ms (one-time DXGI duplication acquisition)
- Subsequent screenshots: <150ms
- MCP server startup: <500ms cold (handshake + tool list announcement)

### Hard latency bounds (regression alert)

- `computer screenshot` >500ms p99
- `computer` instant actions >100ms p99
- Daemon RTT (`ping`) >5ms p99

### Memory budgets

- Daemon RSS at idle: <50MB
- Daemon RSS during active use: <120MB
- MCP server RSS: <30MB

### CPU budgets

- Idle daemon: <0.1% CPU
- Active screenshot capture: ~5-15% on one core for ~80ms
- Active humanized motion: <2% CPU for motion duration

### Distribution targets

- Total release binaries: <30MB (fastuse-cli, fastuse-daemon, fastuse-mcp combined)
- Clean release build: <2 minutes

## Distribution & install

### Three binaries from one workspace

```
target/release/
├── fastuse-cli.exe
├── fastuse-daemon.exe
└── fastuse-mcp.exe
```

All three from `cargo build --release --workspace`.

### Claude Code MCP registration

**Path A — manual one-time setup.** User edits `~/.claude/settings.json` (or project-level `.claude/settings.json`):

```json
{
  "mcp": {
    "servers": {
      "fastuse": {
        "command": "C:\\path\\to\\fastuse-mcp.exe",
        "args": [],
        "env": {}
      }
    }
  }
}
```

Restart Claude Code. Tools appear under namespace `mcp__fastuse__*`.

**Path B — bootstrap subcommand.** `fastuse-cli setup-mcp [--user | --project]`:
- Resolves `fastuse-mcp.exe` absolute path
- Reads existing `settings.json` (creates if missing)
- Adds `mcp.servers.fastuse` entry idempotently
- Prints "Done. Restart Claude Code to pick up the change."

### Binary installation

1. Build from source (today): `cargo build --release --workspace`
2. GitHub Release binaries (when sharing): pre-built `fastuse-windows-x64.zip`
3. scoop / winget package: post-v2 follow-up

### CLAUDE.md rewrite

Major change. Reverses v1's CLI-first structure to MCP-primary.

```markdown
## fastuse v2 — Windows computer-use for Claude Code

Drives the Windows desktop like a human: see, click, type, scroll. 
Vision-first; no accessibility-tree dependence. Works on cooperative apps 
(Calculator, browsers, IDEs) and adversarial ones (Electron, custom-rendered apps).

### How to use

**Primary path: MCP tools.** The `mcp__fastuse__computer` tool exposes the 
Anthropic computer_20251124 schema. Call it like:
  - `mcp__fastuse__computer({action: "screenshot"})` — returns ImageContent inline
  - `mcp__fastuse__computer({action: "left_click", coordinate: [x, y]})`
  - `mcp__fastuse__computer({action: "type", text: "hello"})`

Plus Windows helpers as separate MCP tools: `mcp__fastuse__list_windows`, 
`mcp__fastuse__focus_window`, `mcp__fastuse__inspect_at`, etc.

**Secondary path: CLI via Bash** for one-shot actions, debugging, scripting:
  - `fastuse-cli computer screenshot --out file.jpg`
  - `fastuse-cli list-windows --title "Discord"`

### Coordinate system

Screenshots are scaled to ~1024-wide image space. Click coordinates Claude 
returns are in *that scaled space*; the daemon translates to native pixels. 
Always take a screenshot before clicking unless you're confident the layout 
is the same as the last screenshot.

### Permission model

Default: all tools available. Set FASTUSE_SAFE_MODE=1 to gate destructive 
ones (kill_process, shell_exec, launch_app, clipboard_set_*).

### Daemon lifecycle

Auto-spawns elevated via UAC on first call. Idle-timeout 5 min. 
On DAEMON_SPAWN_FAILED: kill stale daemon, remove pid sentinel, retry.
```

Full content drafted in implementation.

### Documentation deliverables in v2

- `README.md` — overview, install, quick start
- `CLAUDE.md` — agent-facing reference (skeleton above)
- `docs/architecture.md` — daemon, MCP, CLI fit-together
- `docs/configuration.md` — config.toml reference, env vars
- `docs/permissions.md` — safe_mode, gated tools, env precedence
- `docs/migration-from-v1.md` — what changed, what's gone, how to adapt scripts

### Out of scope

- Auto-updater
- Code signing (EV cert; post-v2)
- Telemetry / usage analytics
- Remote crash reporting

## Open questions tracked for follow-up

- **Hardware HID firmware** — separate v3 spec; RP2040 target; firmware in own repo
- **GPU-accelerated scaling** — only if 4K screenshot scaling becomes bottleneck post-v2
- **Multi-session daemon** — current design assumes single user session; revisit if shared-machine usage emerges
- **OpenRouter / non-Claude agent compatibility** — works via MCP if agent supports it; explicit testing deferred until Claude Code path is rock-solid
