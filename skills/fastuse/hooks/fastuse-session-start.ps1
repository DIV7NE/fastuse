# fastuse SessionStart hook.
#
# 1. Makes sure the daemon is up, using the pre-authorised scheduled task so no
#    UAC prompt appears. Silent when the task is absent or already running.
# 2. Injects a short primer so the session knows the tools exist. Detail lives
#    in the fastuse skill, not here - this stays small because it is paid for
#    on every session.

$ErrorActionPreference = 'SilentlyContinue'

$running = $null -ne (Get-Process -Name 'fastuse-daemon' -ErrorAction SilentlyContinue)
$started = $false

if (-not $running) {
    # Only via the scheduled task: a runas spawn here would pop UAC at session
    # start, which is exactly the annoyance this hook exists to avoid.
    $task = schtasks /Query /TN 'fastuse-daemon' 2>$null
    if ($LASTEXITCODE -eq 0) {
        # A dead sentinel makes the next client call take the recovery path.
        $sentinel = Join-Path $env:LOCALAPPDATA 'fastuse\daemon.pid'
        if (Test-Path $sentinel) { Remove-Item $sentinel -Force -ErrorAction SilentlyContinue }
        schtasks /Run /TN 'fastuse-daemon' 2>$null | Out-Null
        $started = $LASTEXITCODE -eq 0
    }
}

$state = if ($running) { 'already running' } elseif ($started) { 'started via scheduled task' } else { 'not running; it will auto-spawn on first call' }

$primer = @"
fastuse (Windows computer use) is available - daemon $state.

Drive the desktop with mcp__fastuse__computer (screenshot, then click the
coordinates you see) plus batch, tail_file, wait_for_idle, launch_app and the
window/process tools. It takes the real cursor, so the user cannot use the
machine while it acts - keep runs short and say when you are driving.

Load the 'fastuse' skill before using these tools. It carries the rules that
decide whether a task works: screenshot before every click, rate_ms 30 when
typing into Electron/WinUI controls, wait_for_idle between click and type, and
batch to collapse a multi-step sequence into one call.
"@

@{
    hookSpecificOutput = @{
        hookEventName     = 'SessionStart'
        additionalContext = $primer
    }
} | ConvertTo-Json -Depth 5 -Compress
