# AGENTS.md

Instructions for any AI coding agent working in this project — Cursor, Cline,
aider, or anything DeepSeek/GPT/Gemini-based that does not auto-load CLAUDE.md.

Before working, read and follow the global spec at
`C:\Users\Uporabnik\.claude\CLAUDE.md`. It defines the working discipline for all
projects on this machine: understand → act → verify, root-cause fixes, minimal
diffs after full reading, building the whole feature (wiring, migrations, error
states, and affected callers — not just the centerpiece), security-first defaults
(server-side enforcement, fail closed, no secrets in code), research against live
sources instead of memory, and a when-stuck escalation protocol (never retry the
same idea twice — go read the library source, the version-exact docs, or search
the exact error, or stop and report honestly). A portable copy lives at
`C:\Users\Uporabnik\Desktop\CLAUDE.md`.

Project-specific notes: see `CLAUDE.md` in this folder.
