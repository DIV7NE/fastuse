# Migrating fastuse scripts from v1 to v2

v2 deletes the entire UIA+OCR targeting layer. The fast-path "click an
element by name" workflow becomes "screenshot, look, click coordinates."

## CLI subcommand changes

| v1 | v2 equivalent |
|---|---|
| `click-element '{"ByName":"OK"}'` | `computer screenshot` then `computer left-click X Y` (Claude reads the screenshot and decides X,Y) |
| `type-into-element '{"ByAutomationId":"input"}' "hello"` | `click-element` analog: focus via screenshot+click, then `computer type "hello"` |
| `wait-for-element '{...}' --timeout-ms N` | `wait-for-window --title <substr> --timeout-ms N` (window-level only) |

UIA inspection still works as read-only:

| v1 | v2 |
|---|---|
| `uia-query '{...}'` | unchanged (read-only) |
| `uia-tree --hwnd N` | unchanged (read-only) |
| `inspect-at X Y` | unchanged (read-only) |

## Wire protocol changes

Wire types removed: `Strategy`, `VerificationEvidence`, `ExpectClause`,
`EscalatePolicy`, `Response::ActionResult`. Replaced by `ComputerAction`
enum + `ComputerResult` (carries optional `ImagePayload` and `ScaleInfo`).

If you wrote scripts that parsed `ActionResult` JSON: they need to read
`ComputerResult` instead.

## Tag v1.0-pre-pivot

v1 binaries preserved at git tag `v1.0-pre-pivot` (pushed to origin).
Roll back with `git checkout v1.0-pre-pivot && cargo build --release`.

## What stays

- Daemon lifecycle (auto-spawn elevated, named pipes, idle timeout, sentinel)
- Window/process/clipboard/launch/shell helpers
- UIA pool + capture thread infrastructure
- Latency budgets and locked invariants (D-10, D-21, D-24, D-25, D-26)
