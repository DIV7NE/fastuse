# fastuse

## What This Is

`fastuse` is a Windows computer-use control plane built for AI coding agents. It exposes screenshot, UI Automation, keyboard, mouse, clipboard, and shell primitives over MCP and a thin native CLI so any agent (Claude Code, opencode, Codex, Cursor, Cline, etc.) can drive the user's Windows desktop. The non-negotiable design constraint is raw speed — every primitive aims for sub-50ms tool round-trip, beating `windows-mcp` by an order of magnitude.

## Core Value

**Latency.** Every other property (coverage, ergonomics, error messages, polish) is negotiable. If a tool call is not measurably faster than `windows-mcp` at the same task, it has failed its purpose.

## Requirements

### Validated

(None yet — ship to validate)

### Active

- [ ] Sub-50ms tool RTT for all primitives that don't require capture (click, type, key, scroll, focus)
- [ ] Sub-150ms screenshot capture (full + region) end-to-end including encode + transport
- [ ] Sub-20ms UIA element query for cached/foreground app
- [ ] MCP server (stdio + optional SSE) compatible with Claude Code, opencode, Codex, any MCP client
- [ ] Native CLI (`fastuse <subcommand>`) producing identical surface as the MCP tools — agents can shell out for power-user paths
- [ ] Persistent local daemon — MCP server and CLI are both thin clients of the daemon, eliminating per-call cold-start cost
- [ ] Hybrid perception model: UIA accessibility tree as default (cheap, fast, semantic), screenshot on demand, OCR only when explicitly requested by the agent
- [ ] Agent-driven strategy selection: tool surface lets the AI decide per-call whether it needs UIA, pixel screenshot, or both
- [ ] Window/process management: list windows, focus window, list running apps, launch apps, kill process
- [ ] Clipboard read/write as first-class fast tools (text + image)
- [ ] Shell exec tool: spawn cmd/powershell/bash, capture stdout/stderr, stream long output
- [ ] Coordinate model: pixel-accurate, DPI-aware, multi-monitor aware
- [ ] Discord E2E benchmark: send a message to a friend in Discord measurably faster than `windows-mcp` end-to-end
- [ ] Notepad E2E benchmark: open notepad, type text, save file — full round trip under a hard latency budget
- [ ] Visual debug mode: each tool call optionally returns a small annotated screenshot for the agent to verify the action landed
- [ ] Single-binary distribution — no Python runtime, no .NET runtime, no node_modules to ship
- [ ] Works in any AI coding terminal that speaks MCP (Claude Code, opencode, Codex, Cursor, Cline, Hermes, etc.)

### Out of Scope

- Cross-platform (macOS/Linux) — Windows-only by design; speed comes from native Win32/UIA/DXGI APIs
- Browser DOM control via CDP — agents already have dedicated browser MCPs (claude-in-chrome); fastuse stays at the OS layer
- Screen recording / video capture in v1 — adds large surface, defer until core stable
- Cloud relay / remote desktop — local machine only; security and latency dictate it
- Built-in vision models / on-device OCR pipelines beyond a thin Tesseract or Windows OCR API wrapper — heavy ML stays out of the hot path
- Bot-detection / CAPTCHA bypass — explicitly forbidden

## Context

- Existing tools and why they're insufficient:
  - `windows-mcp` — Python-based, slow cold-starts, slow per-call latency, broad surface but mediocre on the perf axis
  - Anthropic Claude Desktop's macOS computer-use — high-quality reference but macOS-only and not exposed to coding-CLI agents
  - `pyautogui` / `pywinauto` — building blocks, not tool surfaces; no MCP, no daemon, no agent-friendly contract
- The user runs Windows 11 Pro 26200 on a multi-monitor setup, uses Claude Code, opencode, and other coding terminals daily, and has hit `windows-mcp` performance walls when driving real workflows (Discord, Notepad, dev tooling).
- This is a greenfield Rust project. No prior code exists in this directory.

## Constraints

- **Tech stack**: Rust — chosen for ms-level startup, single-binary deploys, mature `windows-rs` bindings, mature `rmcp` MCP crate, and zero-runtime distribution
- **Platform**: Windows 10 1809+ / Windows 11 — UIAutomation, DXGI desktop duplication, Win32 SendInput required
- **Performance budget**: tool RTT <50ms (no capture), <150ms (screenshot), <20ms (UIA query) — these are the project's reason to exist
- **Distribution**: single-file binary; daemon auto-spawns on first MCP connection
- **Compatibility**: MCP spec compliance (stdio transport mandatory, SSE optional) so any agent can plug in without custom integration
- **Dependencies**: prefer first-party Microsoft APIs (UIAutomation, SendInput, DXGI, Windows.Graphics.Capture) over third-party wrappers — fewer layers = less latency
- **Trust model**: local-only RPC; no auth in v1 (trust boundary is the user's machine), but daemon must refuse non-loopback connections by default

## Key Decisions

| Decision | Rationale | Outcome |
|----------|-----------|---------|
| Rust over C# AOT / Python / Go | Smallest cold-start, smallest binary, best concurrency story for daemon, mature MCP + windows-rs ecosystem | — Pending |
| Persistent daemon over per-call binary | Amortize Windows API init, UIA cache warmup, and MCP framing cost across all calls | — Pending |
| MCP + CLI both backed by daemon | Maximum compatibility (MCP) plus raw-speed escape hatch (CLI) without duplicating logic | — Pending |
| UIA-first hybrid perception | UIA queries are 10-50x faster than OCR and carry semantics; screenshot/OCR only when UIA insufficient | — Pending |
| Agent-chosen perception per call | Different tasks have different needs; let the model decide rather than guess upfront | — Pending |
| Windows-only, no cross-platform | Speed comes from native APIs; abstraction would defeat the entire goal | — Pending |
| Discord + Notepad as E2E benchmarks | One web/Electron app, one native app — covers the perception and input axes | — Pending |
| Daemon refuses non-loopback by default | Local-only trust model; prevents network-exposed accidents | — Pending |

## Evolution

This document evolves at phase transitions and milestone boundaries.

**After each phase transition** (via `/gsd-transition`):
1. Requirements invalidated? → Move to Out of Scope with reason
2. Requirements validated? → Move to Validated with phase reference
3. New requirements emerged? → Add to Active
4. Decisions to log? → Add to Key Decisions
5. "What This Is" still accurate? → Update if drifted

**After each milestone** (via `/gsd-complete-milestone`):
1. Full review of all sections
2. Core Value check — still the right priority?
3. Audit Out of Scope — reasons still valid?
4. Update Context with current state

---
*Last updated: 2026-05-04 after initialization*
