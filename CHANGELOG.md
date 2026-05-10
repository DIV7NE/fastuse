# Changelog

## 2.1.0 — 2026-05-10

### Changed (behaviour)
- `click-element` now returns structured JSON errors instead of free-form
  anyhow messages:
  - Zero matches → `{"ok":false,"error":"NO_ELEMENT_MATCHED",...}` (exit 1)
  - Multiple matches without `--first` →
    `{"ok":false,"error":"AMBIGUOUS_MATCH","matches":N,...}` (exit 1)
  - UIA degraded with no matches → `{"ok":false,"error":"UIA_DEGRADED",...}`
- Previously the first match was silently taken for multi-match selectors;
  callers must now opt in with `--first` or tighten the selector.

### Added
- `--first` flag on `click-element` to opt into pick-first behaviour for
  ambiguous selectors.
- `ErrorCode::NoElementMatched` (`NO_ELEMENT_MATCHED`) and
  `ErrorCode::AmbiguousMatch` (`AMBIGUOUS_MATCH`) — appended; existing
  variants unchanged.

## 2.0.2 — 2026-05-10

### Fixed
- Daemon auto-recovery on stale pipe. `connect_or_spawn` now sweeps dead
  sentinels (`daemon.pid` pointing at a non-fastuse-daemon PID) *before*
  invoking the UAC spawn, eliminating the manual
  `taskkill /F /IM fastuse-daemon.exe` + sentinel-deletion recipe after a
  daemon crash or upgrade. Live-zombie eviction still runs as a fallback
  after spawn failure.
- `DaemonSpawnFailed` error hint now reports what was tried
  (`stale_sentinel_evicted`, `zombie_killed`, `last_pipe_error`) so callers
  can distinguish UAC decline from a stuck spawn.

## 2.0.1 — 2026-05-10

### Added
- `fastuse-cli screenshot-window <HWND>` — capture a specific window's client
  area. Returns the image plus `monitor_offset` and `dpi_scale` so window-local
  pixel coordinates can be translated to monitor-absolute coords without a
  separate `list-windows` / `foreground-window` round-trip. Reuses the cached
  duplication object — no extra capture surface created.
- `Request::ScreenshotWindow` / `Response::ScreenshotWindow` wire variants
  (appended; existing variant indices unchanged).

## 2.0.0 — 2026-05-05

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
