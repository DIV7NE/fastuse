# Architecture

fastuse is three Rust binaries talking to one long-running daemon over Windows
named pipes. The agent (Claude Code or any MCP client) never touches Win32
directly — all desktop interaction goes through the daemon.

## System diagram

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

## How the pieces fit

### fastuse-daemon

The only process that touches Win32 hardware. Runs elevated (UAC `runas` on
first spawn). Persists for 5 minutes of idle, then self-terminates. On the next
call the CLI or MCP server re-spawns it automatically.

Internally the daemon has four worker contexts:

- **Capture thread** — DXGI Desktop Duplication on a dedicated MTA thread.
  Frames arrive in GPU-shared textures; the thread copies, scales, and JPEG-
  encodes in under 50ms at 1080p. First capture amortizes ~20ms WinRT init cost.
- **UIA pool (3 workers)** — COM MTA apartment. Handles `inspect_at`,
  `uia_query`, and `uia_tree`. Three workers because UIA COM calls can block;
  parallel requests from the MCP server don't queue behind each other.
- **Input thread** — COM STA apartment. All `SendInput` calls land here.
  STA is required because some accessibility hooks expect input on the UI
  thread. Humanized mouse/keyboard dispatch goes through `InputBackend`.
- **Window helpers** — synchronous Win32 calls (`EnumWindows`,
  `SetForegroundWindow`, clipboard, process list). No dedicated thread;
  dispatched inline on the pipe-server tokio runtime.

Named pipes: `\\.\pipe\fastuse-{session}-{user}`. Session ID is the daemon's
pid; user is the Windows username. Two surfaces (MCP server, CLI) share one
daemon — same source of truth, no duplicate state.

### fastuse-mcp

Long-lived process managed by Claude Code as an MCP stdio server. Registered
once via `fastuse-cli setup-mcp --user`; Claude Code spawns it on startup and
keeps it alive for the session.

`fastuse-mcp` is the primary consumer surface in v2. It implements:

- The `computer` tool (Anthropic `computer_20251124` schema)
- Windows-specific helper tools (`list_windows`, `focus_window`, etc.)
- UIA read-only inspection tools (`inspect_at`, `uia_query`, `uia_tree`)
- Daemon meta tools (`ping`, `status`, `stop`, `warmup`)

For each MCP tool call it opens a named-pipe connection to the daemon, sends
a request, reads the response, and returns MCP `ToolResult`. The pipe round-
trip is the dominant latency: ~30–80μs for non-capture calls, <150ms for
screenshot (encode-dominated).

### fastuse-cli

Short-lived process; one call = one request = one response = exit. Used for
debugging, scripting, and one-shot automation tasks where spinning up an MCP
session is overkill.

CLI subcommands mirror MCP tools 1:1 for `computer` actions and Windows
helpers. Output is JSON to stdout; exit code 0 on success, 1 on error.

CLI uses **native virtual-desktop pixels** (no scaling). MCP uses **scaled
image-pixel space** (~1024-wide). Keep this distinction in mind when switching
between surfaces.

## Crate breakdown

The workspace has seven crates:

| Crate | Role |
|---|---|
| `fastuse-proto` | Wire types. `ComputerAction` enum, `ComputerResult`, Windows helper request/response types. Shared by daemon, MCP server, and CLI. |
| `fastuse-core` | Local-app-data paths, config loading, redaction utilities, error enum. No Win32 deps. |
| `fastuse-win` | All Windows work: DXGI capture, UIA pool, `InputBackend` trait + humanized impl, scaling, permissions gate. Linked only into the daemon. |
| `fastuse-daemon` | Pipe server, request dispatch, lifecycle management (spawn/idle/shutdown). Thin layer over `fastuse-win`. |
| `fastuse-cli` | clap derive CLI. Thin daemon-RPC client. Parses args, serializes request, pretty-prints response. |
| `fastuse-mcp` | rmcp stdio MCP server. Registers tools, translates MCP call → daemon request, returns MCP result. The primary agent surface. |
| `xtask` | Cargo build helper. Lint runner (`cargo run -p xtask -- lints`). |

`fastuse-win` is the performance-critical crate. Everything else is
coordination glue.

## Thread model

The daemon is a multi-threaded tokio runtime hosting three non-async threads:

- Capture thread parks on `AcquireNextFrame` (DXGI). Returns immediately when
  the desktop hasn't changed — zero CPU at idle.
- UIA pool workers block on COM calls. Three workers means three concurrent
  UIA queries without serialization.
- Input thread processes a channel of `InputEvent`s. Humanized mode adds
  Bezier-curved mouse interpolation and per-keystroke timing jitter before
  calling `SendInput`.

The tokio runtime handles pipe I/O, request routing, and lifecycle timers.
Worker threads communicate back via `oneshot` channels.

## Daemon lifecycle

Auto-spawn elevated via UAC `runas` on first CLI or MCP call. If spawning
fails (`DAEMON_SPAWN_FAILED`):

1. Kill stale daemon: `taskkill /F /IM fastuse-daemon.exe`
2. Remove sentinel: `del %LOCALAPPDATA%\fastuse\daemon.pid`
3. Retry the original call.

Idle timeout: 5 minutes of no requests. The daemon writes `daemon.pid` on
start and deletes it on clean shutdown. On startup it checks for a stale pid
and kills any zombie process before binding the pipe.

Logs: `%LOCALAPPDATA%\fastuse\logs\daemon.YYYY-MM-DD.log`. Structured JSON
via `tracing-subscriber`. Spans record per-request latency.

## Coordinate spaces

Two spaces; never mix them:

- **Scaled image space** — MCP `screenshot` returns a JPEG scaled so the
  longest side is ~1024px. All `computer` coordinates Claude sends back in MCP
  calls are in this space. The daemon's `scaling.rs` converts to native pixels
  before dispatch.
- **Native virtual-desktop space** — CLI coordinates, `foreground_window`
  bounds, `inspect_at` arguments, and `list_windows` bounds are all in native
  virtual-desktop pixels (DPI-unscaled). Multi-monitor setups use the combined
  virtual desktop; left-of-primary monitors have negative X coordinates.

## IPC transport

Named pipes at `\\.\pipe\fastuse-{session}-{user}`.

Chosen over TCP loopback because:
- ~30–80μs local RTT vs ~200–500μs for TCP
- No TCP/IP stack traversal, no Nagle algorithm, no SYN/ACK
- No firewall prompts
- Standard Windows IPC

Frame format: length-prefixed JSON. Each request and response is a 4-byte
little-endian length header followed by a serde_json-encoded payload.
`fastuse-proto` defines all types; both sides share the same crate.
