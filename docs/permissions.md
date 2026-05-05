# Permissions and safe mode

Default: all tools available. Rationale: Claude controls the computer like a
human would; humans don't need permission to open Notepad.

Safe mode: opt-in lockdown for unattended use or sharing the daemon with
untrusted agents.

## Activate safe mode

Three ways, in priority order:

1. **Per-session env**: `FASTUSE_SAFE_MODE=1` set when the daemon spawns.
2. **Per-call env (CLI only)**: `FASTUSE_SAFE_MODE=1 fastuse-cli launch-app calc.exe`.
3. **Persistent**: `%LOCALAPPDATA%\fastuse\config.toml` `[permissions] safe_mode = true`.

## Default gated tools (when safe_mode is on)

- `launch_app` — could spawn arbitrary executables
- `kill_process` — can kill user's work or system processes
- `shell_exec` — arbitrary command execution
- `clipboard_set_text` — overwrites user's clipboard
- `clipboard_set_image` — same

Override the default list with `[permissions] gated_tools = ["..."]` in config.

## Always allowed

`computer` actions (visible to user, reversible), window listing/focus/wait,
process listing, clipboard *read*, UIA inspection, daemon meta tools.

## Failure mode

Gated tool when safe_mode active returns:

```json
{
  "error": {
    "code": "PERMISSION_REQUIRED",
    "message": "tool 'kill_process' is gated in safe mode",
    "hint": "set FASTUSE_SAFE_MODE=0 or remove from gated_tools in config.toml"
  }
}
```

CLI exit code 1; MCP returns `isError: true` with the message.
