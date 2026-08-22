---
name: fastuse
description: Drive the Windows desktop through the fastuse MCP server - screenshot, click, type, scroll, read window and process state, and follow application logs. Use when a task needs the Windows GUI rather than the shell: testing or debugging a desktop app you are building, driving Electron or custom-rendered apps (Discord, VS Code, games, emulators), filling native dialogs, or reading what a launched app reports on stdout. Use also when the user mentions fastuse, computer use, controlling the desktop, clicking, or automating a Windows application.
---

# fastuse

Vision-first Windows control. You see the screen, then click coordinates. UIA
tools are for grounding only - they read structure, they never click.

fastuse takes the real cursor. It is not a background agent: while it acts, the
user cannot use the machine. Keep runs short and say when you are driving.

## Quick start

```
mcp__fastuse__computer({action: "screenshot"})
mcp__fastuse__computer({action: "left_click", coordinate: [x, y]})
```

**Always screenshot before clicking.** Click coordinates are in the scaled
(~1024-wide) space of the most recent screenshot; without one the daemon has no
scale context and rejects the click. The CLI (`fastuse-cli`) uses native pixels
instead - do not mix the two.

## The three rules that break tasks

1. **Typing is rate-limited by default, and should stay that way.** `type`
   defaults to `rate_ms: 30`, which is what survives RichEditD2DPT, WinUI and
   Modern Notepad. `rate_ms: 0` selects the bulk path: roughly 7ms instead of
   540ms for a short string, but it drops characters on those controls -
   measured, `batch verified 42` came back as `h verified 42`. Only opt out
   when the target is known to tolerate it.

2. **Wait for the window, do not sleep.** Use `wait_for_idle` between a click
   and a follow-up `type`. It returns `focus_settled`, which is what tells you
   typing will land.

3. **`focus_window` is enough to type into an app.** It gives the inner edit
   control keyboard focus - verified against Notepad's RichEditD2DPT. You do
   not need a click into the text area first.

## Batch multi-step work

One `batch` call runs a whole sequence with no model turn between steps, which
is the difference between one round trip and ten:

```
mcp__fastuse__batch({steps: [
  {action: "focus_window", hwnd: 12345},
  {action: "wait_for_idle", timeout_ms: 1500},
  {action: "type", text: "hello"},
  {action: "key", chord: "ctrl+s"},
  {action: "screenshot"}
]})
```

Steps stop at the first failure unless `continue_on_error`. End with a
`screenshot` step to see the result. Type steps default to `rate_ms: 30`.

## Seeing what an app reports, not just what it draws

A screenshot shows a frozen frame; the exception behind it is in the log.

- App writes its own log (Unity, Unreal, servers): `tail_file({path, contains})`.
- App writes only to stdout: `launch_app({query, capture_output: true})` returns
  `log_path`, then `tail_file` it.
- Poll incrementally: pass the previous `next_offset` back as `offset` so you
  get only new lines instead of the whole log.

## Grounding a click

`inspect_at({x, y})` reads the UIA element under a pixel without clicking. When
it returns pixel-only, the surface is custom-rendered - trust the screenshot,
not the tree. `uia_query` / `uia_tree` work on cooperative apps and return
nothing useful on Electron or game canvases.

See [REFERENCE.md](REFERENCE.md) for the full tool list, coordinate spaces,
daemon lifecycle, and known sharp edges.
