# Tier 3 — multi-step task chains

These are end-to-end workflows that exercise mixed apps. They're not
automated; you ask Claude in a Claude Code conversation, watch it work, then
record pass/fail in `docs/eval-tier3-results.csv`.

Pass criterion: the task finished as you'd want a competent human to do it,
within reasonable time, with no irreversible mistakes.

## T3.1 — Discord-to-OneNote relay

> Open Discord, find the user **<your Discord friend's name>**, send them
> the message "fastuse v2 ping". Take a screenshot of their reply. Open
> OneNote and paste the screenshot into the **fastuse-eval** notebook.
> Save.

## T3.2 — Test a compiled app

> I just compiled my Tauri app at `$env:FASTUSE_TEST_APP_PATH`. Launch it,
> verify the new search feature works (try searching "hello"), report
> back what you see.

## T3.3 — Google Play Console dashboard

> Take a screenshot of the Google Play Console dashboard and tell me which
> of my apps are restricted, suspended, or have policy issues.

## T3.4 — Mixed-app research summary

> Open Chrome, search for "OSWorld benchmark 2026 latest results", read the
> top 3 results, then summarize what you found in a new Notepad document.
> Save the document as %TEMP%\fastuse-eval-research.txt.

## T3.5 — File reorganization

> Open File Explorer, navigate to %TEMP%, find all *.txt files created in
> the last hour, and move them into a new subfolder called
> `fastuse-eval-archive`.

## Recording results

After each run, append a row to `docs/eval-tier3-results.csv`:

```
date,task,pass,duration_min,notes
2026-05-15,T3.1,true,4,one retry on Discord focus
```
