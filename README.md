# fastuse — Windows computer-use control plane for AI agents

Vision-first Rust daemon + CLI + MCP server for Claude Code (and any
MCP-compatible agent). Drives the Windows desktop like a human: see, click,
type, scroll. Sub-50ms primitive RTT. Works on cooperative apps (Calculator,
browsers, IDEs) and adversarial apps (Electron, custom-rendered WPF, Direct2D).

## Install

```powershell
git clone https://github.com/DIV7NE/fastuse
cd fastuse
cargo build --release --workspace
.\target\release\fastuse-cli.exe setup-mcp --user
# Restart Claude Code. Done.
```

## Quick start

```powershell
# Sanity check
.\target\release\fastuse-cli.exe ping

# Take a screenshot
.\target\release\fastuse-cli.exe computer screenshot --out screen.jpg

# Click somewhere (native coords)
.\target\release\fastuse-cli.exe computer left-click 100 200

# In Claude Code (after setup-mcp): just ask Claude to do things.
# "Take a screenshot and tell me what's on screen."
# "Open Notepad and type 'hello world'."
```

## Architecture

See [`docs/architecture.md`](docs/architecture.md). Three Rust binaries from
one workspace, talking to one daemon over Windows named pipes.

## Configuration

See [`docs/configuration.md`](docs/configuration.md). Optional `config.toml`
for permissions, input humanization, screenshot scaling target.

## Permissions

See [`docs/permissions.md`](docs/permissions.md). Default open;
`FASTUSE_SAFE_MODE=1` gates destructive tools.

## v1 -> v2 migration

See [`docs/migration-from-v1.md`](docs/migration-from-v1.md). v1 used
UIA+OCR targeting (`click-element {ByName: "OK"}`). v2 is vision-first
(`computer({action: "left_click", coordinate: [x, y]})`). Old binaries
preserved at tag `v1.0-pre-pivot`.

## Spec & plan

- Design: [`docs/superpowers/specs/2026-05-05-fastuse-v2-vision-first-design.md`](docs/superpowers/specs/2026-05-05-fastuse-v2-vision-first-design.md)
- Plan: [`docs/superpowers/plans/2026-05-05-fastuse-v2-vision-first.md`](docs/superpowers/plans/2026-05-05-fastuse-v2-vision-first.md)
