# fastuse reference

## Tool surface

**Vision + input**
`computer` (screenshot, left_click, right_click, middle_click, double_click,
triple_click, left_click_drag, type, key, scroll, zoom, mouse_move,
left_mouse_down / left_mouse_up) - Anthropic `computer_20251124` schema,
humanized by default, coordinates in scaled image space.

Discrete equivalents in native pixels: `click`, `drag`, `scroll`, `mouse_move`,
`mouse_down`, `mouse_up`, `type`, `key`, `hold_key`, `screenshot`,
`screenshot_region`.

**Synchronisation** `wait`, `wait_for_idle`, `wait_for_window`, `warmup`.

**Windows** `list_windows`, `foreground_window`, `focus_window`,
`resize_move_window`, `list_monitors`, `cursor_position`.

**Grounding (read-only)** `inspect_at`, `uia_query`, `uia_tree`,
`scroll_into_view`.

**Processes** `launch_app`, `list_processes`, `kill_process`, `shell_exec`.

**Data** `clipboard_get_text`, `clipboard_set_text`, `clipboard_get_image`,
`clipboard_set_image`, `tail_file`.

**Composite** `batch`.

**Meta** `ping`, `status`, `stop`.

## Coordinate spaces

| Path | Space |
|---|---|
| MCP `computer` | scaled, ~1024 wide; daemon maps back to native |
| MCP discrete tools, CLI | native virtual-desktop pixels |

A screenshot resets the scale context; `zoom` pushes a new one. Multi-monitor
coordinates share one virtual desktop origin, so a second monitor left of the
primary has negative x.

## Reading logs

`tail_file({path, offset, max_lines, contains})` returns `lines`,
`next_offset`, `size`, `truncated`, `rotated`. Reads are capped at 256 KiB per
call. `contains` is a case-insensitive substring, not a regex. A file that
shrank below `offset` reports `rotated` and is re-read from the start.

## Daemon

Auto-spawns elevated on first call. `fastuse-cli install-autostart` registers a
scheduled task that starts it at logon with no UAC prompt - the recommended
setup. Release builds run without a console window.

On `DAEMON_SPAWN_FAILED`: kill any stale `fastuse-daemon.exe`, delete
`%LOCALAPPDATA%\fastuse\daemon.pid`, retry. Clients retry `ERROR_PIPE_BUSY`
automatically, so a momentary collision between concurrent sessions is not a
failure.

Logs: `%LOCALAPPDATA%\fastuse\logs\`.

## Sharp edges

- **Text corruption on modern controls.** The bulk `type` path loses characters
  on RichEditD2DPT / WinUI. Use `rate_ms: 30`. Measured, not theoretical.
- **CLI `zoom` cannot be driven standalone.** Scale context lives per
  connection, so a fresh CLI process has none and the call fails with
  "no scale context". Zoom through MCP, where the session holds one.
- **Zoom output is always JPEG.** The Anthropic-shaped action carries no format
  field. `screenshot` honours `format: "png"`.
- **OCR is not wired in yet.** Windows.Media.Ocr reads a 300-900px region well
  on Electron and Qt but fails on a full 1920x1080 frame - measured. If a
  `find_text` tool appears, it will be region-scoped for that reason.
- **Proto changes break mixed builds.** The wire uses postcard, which is not
  self-describing, so an old daemon rejects a newer client with "Hit the end of
  buffer". Rebuild daemon and clients together.
- **Permissions.** Default open. `FASTUSE_SAFE_MODE=1` or
  `%LOCALAPPDATA%\fastuse\config.toml` gates `kill_process`, `shell_exec`,
  `launch_app` and clipboard writes.

## Driving the desktop responsibly

fastuse owns the real cursor and keyboard. Before a run that takes more than a
few seconds, say so. Do not type into a window the user is working in - check
`foreground_window` and window titles first, and prefer a scratch file or a
window you opened yourself over one that already holds their content.
