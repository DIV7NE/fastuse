# Configuration

fastuse reads `%LOCALAPPDATA%\fastuse\config.toml` if present. All sections
optional; sensible defaults if absent.

## `[permissions]`

```toml
[permissions]
safe_mode = false
gated_tools = []  # if non-empty, replaces the default gated set
```

`safe_mode = true` activates gating. Default gated set when no custom list:
`launch_app, kill_process, shell_exec, clipboard_set_text, clipboard_set_image`.

Override at runtime: `FASTUSE_SAFE_MODE=1` (or `0` to force off) takes precedence
over config.toml.

See [`docs/permissions.md`](permissions.md) for the full permissions matrix.

## `[input]`

```toml
[input]
default_humanize = true
typing_mean_ms = 80
typing_stddev_ms = 30
```

`default_humanize` controls whether mouse and keyboard actions use humanized
timing by default. When `true`, mouse moves follow Bezier curves and keystrokes
have random per-key delays drawn from a normal distribution parameterized by
`typing_mean_ms` and `typing_stddev_ms`.

Per-call override: pass `humanize: false` in any MCP `computer` action, or
`--instant` on the CLI.

## `[scaling]`

```toml
[scaling]
target_max = 1024
```

Longest side of scaled screenshots sent to the agent. Anthropic's
`computer_20251124` schema recommends 1024. Larger values increase fidelity at
the cost of token usage and encode time. 768 is the practical minimum; values
above 1280 rarely improve agent accuracy on 1080p screens.

## `[logging]`

```toml
[logging]
level = "info"
```

Valid levels: `trace`, `debug`, `info`, `warn`, `error`. Log files written to
`%LOCALAPPDATA%\fastuse\logs\daemon.YYYY-MM-DD.log`. Older logs are not
automatically rotated; prune manually if disk usage matters.

## Environment variable overrides

All env vars are read at daemon startup (or per-CLI-call for gated checks).

| Variable | Effect |
|---|---|
| `FASTUSE_SAFE_MODE=1` | Activate safe mode (overrides config.toml) |
| `FASTUSE_SAFE_MODE=0` | Force safe mode off (overrides config.toml) |
| `FASTUSE_LOG=<level>` | Override log level |

## Config file location

`%LOCALAPPDATA%\fastuse\config.toml` expands to
`C:\Users\<you>\AppData\Local\fastuse\config.toml`. The daemon creates the
directory on first run; the config file itself is optional.
