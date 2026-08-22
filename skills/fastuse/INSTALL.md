# Installing the fastuse skill and hook

Both are optional. fastuse works without them; they remove the two things a
session otherwise has to rediscover - that the tools exist, and that the daemon
needs to be up.

## Skill

Copy this directory to `~/.claude/skills/fastuse/` (Windows:
`%USERPROFILE%\.claude\skills\fastuse\`). The agent loads it by name.

## SessionStart hook

Copy `hooks/fastuse-session-start.ps1` to `~/.claude/hooks/`, then add it to
the `SessionStart` array in `~/.claude/settings.json`, alongside any hook
already there:

```json
{
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "pwsh -NoProfile -File \"C:\Users\<you>\.claude\hooks\fastuse-session-start.ps1\"",
            "timeout": 15,
            "statusMessage": "Checking fastuse daemon"
          }
        ]
      }
    ]
  }
}
```

The hook starts the daemon only through the `install-autostart` scheduled task,
so it never raises a UAC prompt at session start. With no task registered it
does nothing and the daemon auto-spawns on first call as usual.

Run `fastuse-cli install-autostart` once to register that task.
