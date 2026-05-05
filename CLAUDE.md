<!-- GSD:project-start source:PROJECT.md -->
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
<!-- GSD:project-end -->

<!-- GSD:stack-start source:research/STACK.md -->
## Technology Stack

## Recommended Stack
### Core Technologies
| Technology | Version | Purpose | Why Recommended |
|------------|---------|---------|-----------------|
| **rustc (stable)** | 1.83+ | Compiler | Required by `windows 0.62` and `tokio 1.49`. Use stable; AOT/zero-runtime is the entire reason we're not in C#/Python. |
| **tokio** | 1.49 | Async runtime | The only runtime `rmcp 1.6` accepts (its `Cargo.toml` pins `tokio ^1`). All Windows async I/O (named pipes via `tokio::net::windows::named_pipe`) lives in tokio. Mature, multi-threaded scheduler, lowest p99 jitter for local IPC at our scale. |
| **rmcp** | 1.6.0 | MCP server/client SDK | Official Rust SDK published by `modelcontextprotocol/rust-sdk` (tag `rmcp-v1.6.0`, 2026-05-01). Provides stdio (`stdio()` server, `TokioChildProcess` client), Streamable HTTP, and SSE (via optional `sse-stream 0.2` dep). `IntoTransport` trait lets us layer a custom in-process transport for the CLI ↔ daemon path if we want zero serialization. |
| **windows** | 0.62.2 | Win32 / WinRT bindings | First-party Microsoft `windows-rs`. Exposes `Win32::UI::Input::KeyboardAndMouse::SendInput`, `Win32::UI::WindowsAndMessaging::*` (EnumWindows, GetForegroundWindow, SetForegroundWindow), `Win32::System::DataExchange` (clipboard), `Win32::Graphics::Dxgi` (Desktop Duplication), and the WinRT `Windows::Graphics::Capture` namespace under one crate. Single binding source = zero version skew. |
| **uiautomation** | 0.24.4 | IUIAutomation COM wrapper | Idiomatic Rust over the raw COM `IUIAutomation` interface. Internally pinned to `windows ^0.62.2` and `windows-core ^0.62.2` — version-aligned with our root `windows`. Supports `UIAutomationCacheRequest` (the *only* way to hit <20ms element queries; uncached COM round-trips routinely cost 5–50ms each). |
| **windows-capture** | 2.0.0 | Screen capture | Wraps **Windows.Graphics.Capture** (WGC) with optional **DXGI Desktop Duplication** fallback. Cross-GPU safe, async-friendly, GPU-shared textures. v2.0 is current. See "DXGI vs WGC" below for which path we use when. |
| **interprocess** | 2.4.2 | Daemon IPC (named pipes) | First-class async Windows named pipes with the `tokio` feature. Pipes win over loopback TCP on Windows: no TCP/IP stack traversal, no Nagle, no SYN/ACK handshake — typical local RTT 30–80μs vs 200–500μs for TCP loopback. |
| **serde** / **serde_json** | 1.0 / 1.0.149 | JSON-RPC payload | Required by rmcp; default in the ecosystem. |
| **clap** | 4.6.1 | CLI argparse | Derive macros, single static parser table, ~1ms parse overhead. The CLI is a thin daemon-RPC client, so startup *is* the latency budget. `argh` is ~30% faster to compile but worse UX; `bpaf` is elegant but smaller community. clap is the safe pick. |
| **tracing** + **tracing-subscriber** | 0.1.44 / 0.3 | Structured logging | Spans give us per-tool-call latency timing for free, and a JSON subscriber emits machine-readable daemon logs. `log+env_logger` lacks the span model and can't time async work. |
| **anyhow** + **thiserror** | 1.0 / 2.0 | Errors | `thiserror` for the daemon's typed error enum exported over MCP; `anyhow` inside binaries for wiring. Standard pairing. |
| **bytes** | 1.10 | Buffer plumbing | Zero-copy frame buffers between capture → encode → IPC. |
### Supporting Libraries
| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| **image** | 0.25.10 | PNG/WebP encode (general) | Default screenshot encoder. Adequate for the <150ms budget at typical resolutions; pure-Rust, no C deps. |
| **mtpng** | 0.4.1 | Multi-threaded PNG encode | Drop-in replacement when 4K-monitor PNG encoding blows the 150ms budget. Splits the image across cores; 2–4x faster than `image::png` on multi-core boxes. Add behind a feature flag, swap in if profiling demands it. |
| **turbojpeg** | 1.4.0 | JPEG encode | If clients accept JPEG — encodes a 4K frame in ~5–10ms vs ~30–60ms for PNG. **Cost:** requires libjpeg-turbo C library + NASM at build time, complicates "single binary" story. Recommend statically linking with `vendor` feature; only enable if MCP clients negotiate `image/jpeg`. |
| **base64** | 0.22 | MCP image payload encoding | MCP returns images as base64-encoded `ImageContent`. SIMD-accelerated `base64-simd` is an option if encoding becomes hot. |
| **windows-core** | 0.62.2 | COM core types | Pulled in transitively by `windows` and `uiautomation`. Pin explicitly to avoid duplicate-versions bloat. |
| **dashmap** | 6.1 | Concurrent caches | UIA element cache, window-handle cache. Lock-free reads — critical for the <20ms query path. |
| **parking_lot** | 0.12 | Faster mutexes | Replaces `std::sync::Mutex` where contention matters. ~2x faster, better poisoning semantics. |
| **once_cell** / **std::sync::OnceLock** | std (1.83) | Lazy globals | UIA root element, COM apartment init — initialize once at daemon start. |
| **rmcp-macros** | 1.6.0 | `#[tool]` derive | Companion to `rmcp` for ergonomic tool registration. |
| **schemars** | 1.0 | Tool input JSON Schema | rmcp uses schemars to publish each tool's input schema to clients. |
| **futures** | 0.3 | Stream/Sink helpers | rmcp uses Sink/Stream pairs in transports; we'll need adapters. |
| **windows-sys** | 0.62 | Raw FFI fallback | If a hot path can't tolerate the `windows` crate's COM-safety wrappers, drop to `windows-sys` for direct FFI — same metadata source, no ref-counting overhead. Use sparingly. |
### Optional / Conditional
| Library | Version | Purpose | Trigger |
|---------|---------|---------|---------|
| **windows-ocr** (via `windows::Media::Ocr`) | bundled in `windows 0.62` | OCR fallback | When agent explicitly requests OCR. Use **Windows.Media.Ocr** (built-in to Win10+, no shipping cost, 60+ languages, GPU-assisted). Avoid `tesseract`/`leptess` — adds 30+ MB native deps and slower than the OS API. **Confidence: MEDIUM** (low priority surface; revisit at OCR phase). |
| **axum** | 0.8 | SSE / HTTP transport | Only if we expose remote MCP over SSE. rmcp's `StreamableHttpService` already wraps a tower service, so axum is the natural host. Defer until v2 — stdio covers Claude Code, opencode, Codex, Cursor. |
| **windows-service** | 0.7 | Windows service wrapper | If we promote the daemon from user-launched to a Windows Service. Defer until distribution phase. |
### Development Tools
| Tool | Purpose | Notes |
|------|---------|-------|
| **cargo-nextest** | Test runner | 2–3x faster than `cargo test`, parallel, better failure UI. |
| **criterion** | Microbenchmarks | Per-primitive latency benches (SendInput RTT, UIA cached query, screenshot encode). The project's reason to exist demands continuous benchmarks. |
| **divan** | Alternative bench harness | Faster compile times than criterion if bench iteration speed matters. |
| **flamegraph** / **samply** | Profiling | `samply` is the modern choice on Windows — produces Firefox Profiler traces, no perf stack required. |
| **cargo-llvm-lines** | Compile-time auditing | Catch monomorphization bloat in `windows` and `uiautomation` (huge crates). |
| **cross** / **cargo-xwin** | N/A | Not needed — Windows-only project, build native. |
| **wix** + **cargo-wix** | MSI installer | If we ever ship via MSI. v1 ships a portable `.exe`. |
| **signtool** (Windows SDK) | Code signing | EV cert recommended to avoid SmartScreen friction. Manual step, scripted in CI. |
## Cargo.toml — Concrete Pins
# MCP
# Async + IPC
# Windows
# Image / encoding
# Serde
# CLI + logging + errors
# Concurrency
## Capture-API Decision: WGC vs DXGI vs BitBlt
### Reasoning
| API | Latency | Pros | Cons |
|-----|---------|------|------|
| **Windows.Graphics.Capture (WGC)** | ~3–8ms per frame at 1080p, GPU-shared texture | Modern, supported by Microsoft, cross-GPU safe, captures specific HWNDs (not just monitors), no DRM weirdness, no UAC issues, async event-driven via `Direct3D11CaptureFramePool` | WinRT init overhead at first capture (~20–50ms one-time, amortized by daemon) |
| **DXGI Desktop Duplication** | ~1–4ms per frame at 1080p | Lowest documented full-screen latency; `AcquireNextFrame` returns nothing when the screen hasn't changed (free idle) | Same-GPU constraint — fails on multi-GPU laptops where the foreground app is on the dGPU; full-monitor only (no per-window); no cursor by default |
| **BitBlt / PrintWindow** | 30–100ms+ at 1080p | Works everywhere | CPU-bound; 10–30x slower; **disqualified** |
- [OBS Forum — WGC vs DXGI Desktop Duplication](https://obsproject.com/forum/threads/windows-graphics-capture-vs-dxgi-desktop-duplication.149320/)
- [Win32CaptureSample — Desktop duplication vs WGC discussion](https://github.com/robmikh/Win32CaptureSample/issues/24)
- [windows-capture 2.0 docs](https://docs.rs/windows-capture/latest/windows_capture/)
## IPC Decision: Named Pipes
| Transport | Local RTT (typical) | Notes |
|-----------|--------------------:|-------|
| **Windows named pipes** | 30–80 μs | Kernel-mediated, no TCP stack, 64 KB default buffer. The standard Windows IPC. |
| Unix domain sockets (Win10 1803+) | 80–150 μs | Works, but tooling (`tokio::net::UnixStream` on Windows) is less mature; mostly a portability convenience. |
| Localhost TCP | 200–500 μs | Goes through full TCP/IP stack. Wastes our budget. |
## Alternatives Considered
| Recommended | Alternative | When to Use Alternative |
|-------------|-------------|-------------------------|
| **rmcp 1.6** | `mcp-rs` / `mcp_sdk` (community crates) | Don't. The official SDK has Anthropic backing, full transport coverage, and active maintenance through May 2026. Community alternatives are 6+ months stale on protocol revisions. |
| **tokio 1.49** | `smol` / `async-std` | If we wanted single-threaded tail-latency optimization for embedded. We don't. rmcp pins tokio. |
| **windows 0.62 (windows-rs)** | `winapi 0.3` | `winapi` is unmaintained since 2021 and lacks WinRT. Hard pass. Use `windows` exclusively. |
| **windows 0.62** | `winsafe` | Pure-Rust safe wrapper, but tiny coverage compared to `windows-rs` and no WinRT. Niche. |
| **windows-capture 2.0** | `win_desktop_duplication 0.10` | If we need raw DXGI control without WGC abstraction (e.g. predictable AcquireNextFrame semantics for game capture). Keep as a known-good fallback we can swap to module-by-module. |
| **uiautomation 0.24** | Hand-rolled COM via `windows::UI::Accessibility` | Only if profiling proves the wrapper costs us >2ms per query. Direct COM is more painful and we lose the cache request abstraction. |
| **clap 4.6** | `argh`, `bpaf`, `lexopt` | `lexopt` parses in ~50μs vs clap's ~1ms — meaningful only if the CLI startup budget shrinks to <5ms total. If yes, switch to `lexopt` (within ~10% of clap UX-wise → flag as Key Decision to revisit at perf phase). |
| **interprocess named pipes** | Raw `tokio::net::windows::named_pipe` | Tokio's stdlib offers this directly with no extra dep. `interprocess` adds Unix socket parity and a unified API. If we want zero deps, drop it. **Within ~10% → revisit decision.** |
| **tracing 0.1** | `slog`, `log + env_logger` | Only if we drop async — `tracing` was built for async span propagation, which is what we need for per-tool-call timing. |
| **image 0.25** | `mtpng 0.4` | When 4K screenshots blow the budget (always benchmark first). |
| **image PNG** | `turbojpeg 1.4` JPEG | When MCP client negotiates `image/jpeg`. ~5x faster encode but lossy and adds C deps. |
| **Windows.Media.Ocr** | `tesseract` / `leptess` | Only if we need OCR on Win7/Win8 (we don't) or non-Windows (we don't). |
## What NOT to Use
| Avoid | Why | Use Instead |
|-------|-----|-------------|
| **Python** (any binding, even via PyO3) | The whole project's reason to exist is escaping `windows-mcp`'s Python latency. Adding a Python runtime defeats the goal. | Pure Rust. |
| **.NET / C# AOT** | Single-binary stories exist (NativeAOT) but cold-start is 50–150ms even AOT'd; UIAutomation is primary-platform API, but we want zero runtime. | `windows` + `uiautomation` crates. |
| **`winapi` crate** | Unmaintained since 2021. No WinRT. No metadata-driven generation. | `windows` 0.62. |
| **BitBlt screen capture** | 10–30x slower than WGC/DXGI. CPU-bound. Doesn't capture hardware-accelerated content (Chrome, games) correctly. | `windows-capture` (WGC), DXGI fallback. |
| **`PrintWindow` for screenshots** | Same as BitBlt; some windows render black; legacy GDI path. | WGC HWND capture. |
| **Localhost TCP for daemon IPC** | 200–500μs per RTT, full TCP stack overhead, firewall warnings. | Named pipes via `interprocess` 2.4. |
| **Raw `std::process::Command` for shell tool** | No async stdout streaming, blocks the runtime. | `tokio::process::Command` with line-buffered streaming. |
| **`log + env_logger`** | No spans, no async-aware timing, no structured fields. We need per-tool-call latency tracing. | `tracing` + `tracing-subscriber`. |
| **`pyautogui` / `pywinauto` model** (synthetic UI events without UIA awareness) | Brittle, slow, no semantic targeting. | UIA-first targeting + `SendInput` for the actual keystroke/click. |
| **`tesseract` / `leptess`** | 30+ MB C deps, slower than the OS-built-in API on Windows. | `windows::Media::Ocr`. |
| **MSVCRT dynamic linking** | "Single binary" promise breaks if user lacks the redist. | `RUSTFLAGS="-C target-feature=+crt-static"` for fully static CRT (MSVC target). |
| **Anything that spawns a window** during daemon startup | Perception is "instant"; we can't have a console flash. | `#[windows_subsystem = "windows"]` for the daemon binary; CLI stays console. |
## Stack Patterns by Variant
- rmcp stdio transport
- Daemon-spawn-on-first-connect; CLI also speaks named-pipe RPC
- No HTTP, no TLS, no auth
- Add `axum 0.8` + rmcp `StreamableHttpService`
- Bind to `127.0.0.1` only by default; explicit `--listen 0.0.0.0` flag with mandatory bearer-token auth
- Reuse the same daemon RPC underneath
- Switch from `image::png` to `mtpng` behind the `fast-png` feature
- Optionally negotiate `image/jpeg` with the client and use `turbojpeg`
- Aggressively use `UIAutomationCacheRequest` to fetch parent/children/properties in one COM call
- Cache `IUIAutomationElement` for the foreground HWND, invalidate on focus change
- As escalation: drop to raw COM via `windows::UI::Accessibility` for hot paths
- Use WGC's built-in cursor inclusion flag (`IncludeCursor`)
- Avoid the DXGI cursor-compositing dance
## Version Compatibility
| Package | Compatible With | Notes |
|---------|-----------------|-------|
| `rmcp 1.6` | `tokio ^1` | Hard requirement; rmcp won't compile against `smol`/`async-std`. |
| `uiautomation 0.24.4` | `windows ^0.62.2`, `windows-core ^0.62.2` | Pin `windows` to `0.62.2` at workspace level to avoid the dependency tree pulling two copies of `windows` (huge crate, doubles compile time). |
| `windows-capture 2.0` | `windows-rs` internal | Verify against our pinned `windows 0.62.2` — if it pulls a different version, force-resolve via `[patch.crates-io]`. |
| `interprocess 2.4` (tokio feature) | `tokio ^1` | Standard. |
| `clap 4.6` | `rustc 1.74+` | Non-issue; we require 1.83+. |
| `image 0.25` | pure Rust | No C deps; clean static link. |
| `turbojpeg 1.4` (vendor) | NASM + CMake + C compiler at build time | Build-server burden. Acceptable for CI; document in README. |
| **MSRV** | rustc 1.83 | Driven by `windows 0.62` and recent tokio/serde minor bumps. |
| **CRT linking** | MSVC target only | Linux/MinGW unsupported by goal. `RUSTFLAGS="-C target-feature=+crt-static"` in `.cargo/config.toml`. |
## Sources
- [docs.rs/rmcp — 1.6.0](https://docs.rs/rmcp/latest/rmcp/) — verified version, transports, tokio dependency. **HIGH**
- [github.com/modelcontextprotocol/rust-sdk — rmcp-v1.6.0 tag, 2026-05-01](https://github.com/modelcontextprotocol/rust-sdk) — official SDK confirmation, version. **HIGH**
- [docs.rs/windows — 0.62.2](https://docs.rs/windows/latest/windows/) — current windows-rs version. **HIGH**
- [docs.rs/uiautomation — 0.24.4](https://docs.rs/uiautomation/latest/uiautomation/) — UIA wrapper, windows-rs alignment. **HIGH**
- [docs.rs/windows-capture — 2.0.0](https://docs.rs/windows-capture/latest/windows_capture/) — WGC + DXGI wrapper, current version. **HIGH** (version), **MEDIUM** (perf claims)
- [docs.rs/tokio — 1.49](https://docs.rs/tokio/latest/tokio/) — runtime version. **HIGH**
- [docs.rs/interprocess — 2.4.2](https://docs.rs/interprocess/latest/interprocess/) — named pipes + tokio feature. **HIGH**
- [docs.rs/clap — 4.6.1](https://docs.rs/clap/latest/clap/) — current version. **HIGH**
- [docs.rs/tracing — 0.1.44](https://docs.rs/tracing/latest/tracing/) — current. **HIGH**
- [docs.rs/serde_json — 1.0.149](https://docs.rs/serde_json/latest/serde_json/) — current. **HIGH**
- [docs.rs/image — 0.25.10](https://docs.rs/image/latest/image/) — current. **HIGH**
- [docs.rs/mtpng — 0.4.1](https://docs.rs/mtpng/latest/mtpng/) — current. **HIGH**
- [docs.rs/turbojpeg — 1.4.0](https://docs.rs/turbojpeg/latest/turbojpeg/) — current; note 1.4.0 build broken on docs.rs, last green 1.3.3. **MEDIUM**
- [lib.rs/win_desktop_duplication — 0.10.11](https://lib.rs/crates/win_desktop_duplication) — DXGI fallback option. **HIGH**
- [OBS Forum — WGC vs DXGI Desktop Duplication](https://obsproject.com/forum/threads/windows-graphics-capture-vs-dxgi-desktop-duplication.149320/) — qualitative perf comparison. **MEDIUM**
- [Win32CaptureSample issue #24 — capture API tradeoffs](https://github.com/robmikh/Win32CaptureSample/issues/24) — Microsoft sample maintainer's view. **MEDIUM**
- [TM Dev Lab — MCP Server Performance Benchmark](https://www.tmdevlab.com/mcp-server-performance-benchmark.html) — Rust MCP at 4,845 RPS, 10.9 MB RAM, 4–7ms p50. Validates the language choice. **MEDIUM**
<!-- GSD:stack-end -->

<!-- GSD:conventions-start source:CONVENTIONS.md -->
## Conventions

Conventions not yet established. Will populate as patterns emerge during development.
<!-- GSD:conventions-end -->

<!-- GSD:architecture-start source:ARCHITECTURE.md -->
## Architecture

Architecture not yet mapped. Follow existing patterns found in the codebase.
<!-- GSD:architecture-end -->

<!-- GSD:skills-start source:skills/ -->
## Project Skills

No project skills found. Add skills to any of: `.claude/skills/`, `.agents/skills/`, `.cursor/skills/`, `.github/skills/`, or `.codex/skills/` with a `SKILL.md` index file.
<!-- GSD:skills-end -->

<!-- GSD:workflow-start source:GSD defaults -->
## GSD Workflow Enforcement

Before using Edit, Write, or other file-changing tools, start work through a GSD command so planning artifacts and execution context stay in sync.

Use these entry points:
- `/gsd-quick` for small fixes, doc updates, and ad-hoc tasks
- `/gsd-debug` for investigation and bug fixing
- `/gsd-execute-phase` for planned phase work

Do not make direct repo edits outside a GSD workflow unless the user explicitly asks to bypass it.
<!-- GSD:workflow-end -->



<!-- GSD:profile-start -->
## Developer Profile

> Profile not yet configured. Run `/gsd-profile-user` to generate your developer profile.
> This section is managed by `generate-claude-profile` -- do not edit manually.
<!-- GSD:profile-end -->
