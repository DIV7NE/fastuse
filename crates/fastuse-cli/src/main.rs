//! fastuse-cli — local helper CLI for the daemon.

mod bench;
mod cmd_computer;
mod cmd_phase2;
mod cmd_phase3;
mod cmd_phase4;
mod cmd_ping;
mod cmd_setup_mcp;
mod cmd_start;
mod cmd_status;
mod cmd_stop;
mod proto_io;
mod spawn;

use clap::{Parser, Subcommand};
use fastuse_core::pipe_path_resolve;
use fastuse_win::set_per_monitor_v2_first_call;

#[derive(Parser, Debug)]
#[command(name = "fastuse-cli", version, about = "fastuse local helper CLI")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Round-trip a ping through the daemon.
    Ping {
        /// Run N iterations and report cold/warm separately.
        #[arg(long, default_value_t = 1)]
        bench: u32,
        /// Pretty human-readable output.
        #[arg(long, default_value_t = false)]
        pretty: bool,
    },
    /// Start the daemon (idempotent).
    Start,
    /// Stop the running daemon.
    Stop,
    /// Print daemon status (sentinel + 50ms ping probe).
    Status,

    // ----- Phase 2: input -----
    /// Click at physical-pixel coordinates.
    Click {
        /// Target x in physical pixels (virtual-desktop origin).
        x: i32,
        /// Target y in physical pixels (virtual-desktop origin).
        y: i32,
        /// Mouse button: left | right | middle.
        #[arg(long, default_value = "left")]
        button: String,
        /// Number of click cycles (e.g. 2 = double-click).
        #[arg(long, default_value_t = 1)]
        count: u8,
        /// Comma-separated modifier list, e.g. `ctrl,shift`.
        #[arg(long)]
        mods: Option<String>,
        /// Skip the implicit SetCursorPos pre-move.
        #[arg(long, default_value_t = false)]
        no_cursor: bool,
        #[command(flatten)]
        opts: ActionOptsArgs,
    },
    /// Type literal Unicode text.
    Type {
        /// Text to type. Wrapped in Redact<> end-to-end (D-10).
        text: String,
        #[command(flatten)]
        opts: ActionOptsArgs,
    },
    /// Press a chord (e.g. ctrl+s).
    Key {
        /// Chord to press.
        chord: String,
        /// Number of full DOWN/UP cycles for the primary key.
        #[arg(long, default_value_t = 1)]
        repeat: u32,
        #[command(flatten)]
        opts: ActionOptsArgs,
    },
    /// Press a chord and hold for --ms milliseconds.
    HoldKey {
        /// Chord to press.
        chord: String,
        /// Hold duration.
        #[arg(long)]
        ms: u32,
        #[command(flatten)]
        opts: ActionOptsArgs,
    },
    /// Move the cursor.
    MouseMove {
        /// Target x in physical pixels.
        x: i32,
        /// Target y in physical pixels.
        y: i32,
        #[command(flatten)]
        opts: ActionOptsArgs,
    },
    /// Mouse-button DOWN at the current cursor position.
    MouseDown {
        /// Button: left | right | middle.
        #[arg(long, default_value = "left")]
        button: String,
        #[command(flatten)]
        opts: ActionOptsArgs,
    },
    /// Mouse-button UP at the current cursor position.
    MouseUp {
        /// Button: left | right | middle.
        #[arg(long, default_value = "left")]
        button: String,
        #[command(flatten)]
        opts: ActionOptsArgs,
    },
    /// Click-and-drag from start to end.
    Drag {
        /// Drag start x.
        sx: i32,
        /// Drag start y.
        sy: i32,
        /// Drag end x.
        ex: i32,
        /// Drag end y.
        ey: i32,
        /// Button held during drag.
        #[arg(long, default_value = "left")]
        button: String,
        /// Modifiers held during drag.
        #[arg(long)]
        mods: Option<String>,
        #[command(flatten)]
        opts: ActionOptsArgs,
    },
    /// Scroll at coords.
    Scroll {
        /// Target x in physical pixels.
        x: i32,
        /// Target y in physical pixels.
        y: i32,
        /// Direction: up | down | left | right.
        #[arg(long)]
        dir: String,
        /// Wheel notch count.
        #[arg(long, default_value_t = 3)]
        amount: i32,
        /// Modifiers held during scroll.
        #[arg(long)]
        mods: Option<String>,
        #[command(flatten)]
        opts: ActionOptsArgs,
    },
    /// Server-side sleep.
    Wait {
        /// Sleep duration in milliseconds.
        #[arg(long)]
        ms: u32,
    },

    // ----- Phase 2: window/monitor -----
    /// List display monitors.
    ListMonitors,
    /// Read the current cursor position.
    CursorPosition,
    /// Print the foreground window's WindowInfo.
    ForegroundWindow,
    /// Enumerate top-level windows.
    ListWindows {
        /// Filter by process basename substring (case-insensitive).
        #[arg(long)]
        process: Option<String>,
        /// Filter by title substring (case-insensitive).
        #[arg(long)]
        title: Option<String>,
    },
    /// Bring the given HWND to the foreground.
    FocusWindow {
        /// HWND as decimal or 0x-prefixed hex.
        hwnd: String,
        #[command(flatten)]
        opts: ActionOptsArgs,
    },
    /// Wait for a window matching title/process to appear (polls up to --timeout-ms).
    WaitForWindow {
        /// Substring match against window title.
        #[arg(long)]
        title: Option<String>,
        /// Substring match against owning process name.
        #[arg(long)]
        process: Option<String>,
        /// Poll timeout in milliseconds.
        #[arg(long, default_value_t = 5000)]
        timeout_ms: u32,
    },
    /// Move + resize a window.
    ResizeMoveWindow {
        /// HWND as decimal or 0x-prefixed hex.
        hwnd: String,
        /// New top-left x in physical pixels.
        x: i32,
        /// New top-left y in physical pixels.
        y: i32,
        /// New width in physical pixels.
        w: i32,
        /// New height in physical pixels.
        h: i32,
        #[command(flatten)]
        opts: ActionOptsArgs,
    },

    // ----- Phase 3: capture -----
    /// Take a full-monitor screenshot.
    Screenshot {
        /// Monitor index (0 = primary).
        #[arg(long)]
        monitor: Option<u32>,
        /// jpeg (default) or png.
        #[arg(long)]
        format: Option<String>,
        /// Write encoded bytes to this file (raw, NOT base64).
        #[arg(long)]
        out: Option<std::path::PathBuf>,
    },
    /// Take a sub-rectangle screenshot (reuses cached duplication object).
    ScreenshotRegion {
        /// Top-left x in physical pixels.
        x: i32,
        /// Top-left y in physical pixels.
        y: i32,
        /// Width in physical pixels.
        w: u32,
        /// Height in physical pixels.
        h: u32,
        /// Monitor index (0 = primary).
        #[arg(long)]
        monitor: Option<u32>,
        /// jpeg (default) or png.
        #[arg(long)]
        format: Option<String>,
        /// Write encoded bytes to this file.
        #[arg(long)]
        out: Option<std::path::PathBuf>,
    },

    // ----- Phase 3: UIA -----
    /// Walk the UIA subtree for a window (foreground if --hwnd is unset).
    UiaTree {
        /// HWND as decimal or 0x-hex.
        #[arg(long)]
        hwnd: Option<String>,
        /// Depth bound (None = unlimited).
        #[arg(long)]
        depth: Option<u32>,
        /// content (default) or raw.
        #[arg(long)]
        view: Option<String>,
    },
    /// Selector-driven query. Selector is JSON: {"ByName":"OK"}.
    UiaQuery {
        /// Selector JSON. See fastuse_proto::Selector.
        selector: String,
        /// Optional explicit root HWND (default foreground).
        #[arg(long)]
        root_hwnd: Option<String>,
    },
    /// Inspect the UIA element under a screen point.
    InspectAt {
        /// Probe x in physical pixels.
        x: i32,
        /// Probe y in physical pixels.
        y: i32,
    },
    /// Scroll the matched element into view.
    ScrollIntoView {
        /// Selector JSON.
        selector: String,
        #[command(flatten)]
        opts: ActionOptsArgs,
    },

    // ----- Phase 4: clipboard / shell / launch / processes -----
    /// Read text from the clipboard.
    ClipboardGetText,
    /// Write text to the clipboard.
    ClipboardSetText {
        /// Text to write.
        text: String,
    },
    /// Run a shell command (permission-gated).
    ShellExec {
        /// Command line.
        command: String,
        /// Shell: cmd | powershell | pwsh | bash.
        #[arg(long)]
        shell: Option<String>,
        /// Working directory.
        #[arg(long)]
        cwd: Option<String>,
        /// Timeout in milliseconds (default 30000).
        #[arg(long)]
        timeout_ms: Option<u64>,
    },
    /// Launch an application by query.
    LaunchApp {
        /// Path / PATH binary / URI / app name.
        query: String,
    },
    /// Enumerate running processes.
    ListProcesses {
        /// Substring filter on process name.
        #[arg(long)]
        name: Option<String>,
        /// Restrict to processes with a visible main window.
        #[arg(long)]
        visible_only: bool,
    },
    /// Terminate a process (permission-gated).
    KillProcess {
        /// 'pid:<n>' or 'name:<stem>'.
        selector: String,
        /// Best-effort hard-kill.
        #[arg(long)]
        force: bool,
        /// Kill the entire process tree.
        #[arg(long)]
        process_tree: bool,
    },

    // ----- Warmup -----
    /// Warm every cold path (D3D11, DXGI, UIA root, monitors). Idempotent.
    Warmup,

    // ----- Bench -----
    /// Aimbot smoke fixtures.
    Bench {
        #[command(subcommand)]
        kind: BenchCmd,
    },

    // ----- v2 computer subcommands (native pixel coords, no scale) -----
    /// computer_20251124-compatible actions with native virtual-desktop coords.
    Computer(ComputerArgs),

    // ----- setup -----
    /// Register fastuse-mcp.exe in Claude Code's settings.json (idempotent).
    SetupMcp(SetupMcpArgs),
}

#[derive(Subcommand, Debug)]
enum BenchCmd {
    /// Run an aimbot smoke scenario.
    Aimbot {
        /// Scenario: calculator | discord | lconnect3 | all.
        #[arg(long)]
        scenario: String,
    },
}

/// Top-level wrapper for `computer` subcommands.
#[derive(clap::Args, Debug)]
struct ComputerArgs {
    #[command(subcommand)]
    action: ComputerSubcmd,
}

/// All `computer` sub-actions. Coordinates are native virtual-desktop pixels.
#[derive(Subcommand, Debug)]
enum ComputerSubcmd {
    /// Capture a screenshot of monitor N (default primary).
    Screenshot {
        /// Output file path. Raw encoded bytes (JPEG/PNG), not base64.
        #[arg(long)]
        out: Option<std::path::PathBuf>,
        /// Monitor index (0 = primary).
        #[arg(long)]
        monitor: Option<u32>,
        /// Output format: jpeg (default) or png.
        #[arg(long, default_value = "jpeg")]
        format: String,
    },
    /// Single primary-button click at (X, Y).
    LeftClick {
        /// Target x in native virtual-desktop pixels.
        x: i32,
        /// Target y in native virtual-desktop pixels.
        y: i32,
        /// Modifier chord to hold during the click, e.g. `"ctrl+shift"`.
        #[arg(long)]
        modifiers: Option<String>,
        /// Skip Bezier-curve humanization — click fires immediately.
        #[arg(long)]
        instant: bool,
    },
    /// Single secondary-button click at (X, Y).
    RightClick {
        /// Target x in native virtual-desktop pixels.
        x: i32,
        /// Target y in native virtual-desktop pixels.
        y: i32,
        /// Skip Bezier-curve humanization.
        #[arg(long)]
        instant: bool,
    },
    /// Single middle-button click at (X, Y).
    MiddleClick {
        /// Target x in native virtual-desktop pixels.
        x: i32,
        /// Target y in native virtual-desktop pixels.
        y: i32,
        /// Skip Bezier-curve humanization.
        #[arg(long)]
        instant: bool,
    },
    /// Two primary-button clicks at (X, Y).
    DoubleClick {
        /// Target x in native virtual-desktop pixels.
        x: i32,
        /// Target y in native virtual-desktop pixels.
        y: i32,
        /// Skip Bezier-curve humanization.
        #[arg(long)]
        instant: bool,
    },
    /// Three primary-button clicks at (X, Y) — select-all equivalent.
    TripleClick {
        /// Target x in native virtual-desktop pixels.
        x: i32,
        /// Target y in native virtual-desktop pixels.
        y: i32,
        /// Skip Bezier-curve humanization.
        #[arg(long)]
        instant: bool,
    },
    /// Click-drag from (SX, SY) to (EX, EY) holding left button.
    Drag {
        /// Drag start x in native virtual-desktop pixels.
        sx: i32,
        /// Drag start y in native virtual-desktop pixels.
        sy: i32,
        /// Drag end x in native virtual-desktop pixels.
        ex: i32,
        /// Drag end y in native virtual-desktop pixels.
        ey: i32,
        /// Modifier chord held during the drag, e.g. `"ctrl"`.
        #[arg(long)]
        modifiers: Option<String>,
        /// Skip humanized Bezier motion.
        #[arg(long)]
        instant: bool,
    },
    /// Type literal Unicode text.
    Type {
        /// Text to type (sent via SendInput KEYEVENTF_UNICODE).
        text: String,
        /// Skip per-keystroke timing jitter.
        #[arg(long)]
        instant: bool,
    },
    /// Press a key chord (e.g. `ctrl+s`, `alt+f4`, `enter`).
    Key {
        /// Chord string in xdotool-style syntax.
        chord: String,
    },
    /// Press a chord and hold it for --ms milliseconds.
    HoldKey {
        /// Chord string in xdotool-style syntax.
        chord: String,
        /// Hold duration in milliseconds.
        #[arg(long)]
        ms: u32,
    },
    /// Mouse-wheel scroll at (X, Y).
    Scroll {
        /// Scroll origin x in native virtual-desktop pixels.
        x: i32,
        /// Scroll origin y in native virtual-desktop pixels.
        y: i32,
        /// Direction: up | down | left | right.
        #[arg(long)]
        direction: String,
        /// Number of wheel ticks.
        #[arg(long, default_value_t = 3)]
        amount: i32,
    },
    /// Move the cursor to (X, Y) without clicking.
    MouseMove {
        /// Target x in native virtual-desktop pixels.
        x: i32,
        /// Target y in native virtual-desktop pixels.
        y: i32,
        /// Skip humanized Bezier motion.
        #[arg(long)]
        instant: bool,
    },
    /// Read the current cursor position.
    CursorPosition,
    /// Sleep for MS milliseconds (server-side).
    Wait {
        /// Sleep duration in milliseconds.
        ms: u32,
    },
    /// Zoom/crop around (X, Y) and push a new ScaleSnapshot.
    Zoom {
        /// Center x in native virtual-desktop pixels.
        x: i32,
        /// Center y in native virtual-desktop pixels.
        y: i32,
        /// Zoom factor (e.g. 2.0 = 2×).
        #[arg(long, default_value_t = 2.0)]
        factor: f32,
    },
    /// Press left mouse button down at optional (X, Y).
    LeftMouseDown {
        /// Optional x; defaults to current cursor x when absent.
        #[arg(long)]
        x: Option<i32>,
        /// Optional y; defaults to current cursor y when absent.
        #[arg(long)]
        y: Option<i32>,
    },
    /// Release left mouse button at optional (X, Y).
    LeftMouseUp {
        /// Optional x; defaults to current cursor x when absent.
        #[arg(long)]
        x: Option<i32>,
        /// Optional y; defaults to current cursor y when absent.
        #[arg(long)]
        y: Option<i32>,
    },
}

/// Arguments for `setup-mcp`.
#[derive(clap::Args, Debug)]
struct SetupMcpArgs {
    /// Write to user-level (~/.claude/settings.json). Default when neither flag is given.
    #[arg(long)]
    user: bool,
    /// Write to project-level (.claude/settings.json in cwd).
    #[arg(long)]
    project: bool,
}

#[derive(clap::Args, Debug, Clone, Default)]
struct ActionOptsArgs {
    /// Selector JSON to poll for after the action.
    #[arg(long)]
    wait_for: Option<String>,
    /// Capture a screenshot after the action.
    #[arg(long, default_value_t = false)]
    screenshot_after: bool,
    /// "auto" or "full"; default "auto".
    #[arg(long)]
    screenshot_region: Option<String>,
    /// "jpeg" (default) or "png".
    #[arg(long)]
    screenshot_format: Option<String>,
    /// Wait timeout (ms); default 2000.
    #[arg(long)]
    wait_timeout_ms: Option<u32>,
}

fn build_action_opts(a: ActionOptsArgs) -> anyhow::Result<Option<fastuse_proto::ActionOpts>> {
    if a.wait_for.is_none()
        && !a.screenshot_after
        && a.wait_timeout_ms.is_none()
    {
        return Ok(None);
    }
    let wait_for = a.wait_for.as_deref().map(serde_json::from_str).transpose()?;
    let screenshot_after = a.screenshot_after.then(|| {
        let region = match a.screenshot_region.as_deref() {
            Some("full") => None,
            _ => Some(fastuse_proto::RegionSpec::Auto),
        };
        fastuse_proto::ScreenshotOpts {
            region,
            format: Some(match a.screenshot_format.as_deref() {
                Some("png") => fastuse_proto::ImageFormat::Png,
                _ => fastuse_proto::ImageFormat::Jpeg,
            }),
            quality: None,
        }
    });
    Ok(Some(fastuse_proto::ActionOpts {
        wait_for,
        screenshot_after,
        wait_timeout_ms: a.wait_timeout_ms,
    }))
}

fn parse_hwnd(s: &str) -> anyhow::Result<u64> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Ok(u64::from_str_radix(hex, 16)?)
    } else {
        Ok(s.parse::<u64>()?)
    }
}

fn main() {
    set_per_monitor_v2_first_call();
    let cli = Cli::parse();

    let identity = match pipe_path_resolve() {
        Ok(id) => id,
        Err(e) => {
            eprintln!("fastuse-cli: pipe path resolve failed: {e}");
            std::process::exit(2);
        }
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime build");

    let result: anyhow::Result<()> = rt.block_on(async {
        match cli.command {
            Cmd::Ping { bench, pretty } => cmd_ping::run(&identity.path, bench, pretty).await,
            Cmd::Start => cmd_start::run(&identity.path).await,
            Cmd::Stop => cmd_stop::run(&identity.path).await,
            Cmd::Status => cmd_status::run(&identity.path).await,
            Cmd::Click { x, y, button, count, mods, no_cursor, opts } => {
                let opts = build_action_opts(opts)?;
                cmd_phase2::click(&identity.path, x, y, &button, count, mods.as_deref(), no_cursor, opts).await
            }
            Cmd::Type { text, opts } => {
                let opts = build_action_opts(opts)?;
                cmd_phase2::r#type(&identity.path, text, opts).await
            }
            Cmd::Key { chord, repeat, opts } => {
                let opts = build_action_opts(opts)?;
                cmd_phase2::key(&identity.path, chord, repeat, opts).await
            }
            Cmd::HoldKey { chord, ms, opts } => {
                let opts = build_action_opts(opts)?;
                cmd_phase2::hold_key(&identity.path, chord, ms, opts).await
            }
            Cmd::MouseMove { x, y, opts } => {
                let opts = build_action_opts(opts)?;
                cmd_phase2::mouse_move(&identity.path, x, y, opts).await
            }
            Cmd::MouseDown { button, opts } => {
                let opts = build_action_opts(opts)?;
                cmd_phase2::mouse_down(&identity.path, &button, opts).await
            }
            Cmd::MouseUp { button, opts } => {
                let opts = build_action_opts(opts)?;
                cmd_phase2::mouse_up(&identity.path, &button, opts).await
            }
            Cmd::Drag { sx, sy, ex, ey, button, mods, opts } => {
                let opts = build_action_opts(opts)?;
                cmd_phase2::drag(&identity.path, sx, sy, ex, ey, &button, mods.as_deref(), opts).await
            }
            Cmd::Scroll { x, y, dir, amount, mods, opts } => {
                let opts = build_action_opts(opts)?;
                cmd_phase2::scroll(&identity.path, x, y, &dir, amount, mods.as_deref(), opts).await
            }
            Cmd::Wait { ms } => cmd_phase2::wait(&identity.path, ms).await,
            Cmd::ListMonitors => cmd_phase2::list_monitors(&identity.path).await,
            Cmd::CursorPosition => cmd_phase2::cursor_position(&identity.path).await,
            Cmd::ForegroundWindow => cmd_phase2::foreground_window(&identity.path).await,
            Cmd::ListWindows { process, title } =>
                cmd_phase2::list_windows(&identity.path, process, title).await,
            Cmd::FocusWindow { hwnd, opts } => {
                let h = parse_hwnd(&hwnd)?;
                let opts = build_action_opts(opts)?;
                cmd_phase2::focus_window(&identity.path, h, opts).await
            }
            Cmd::WaitForWindow { title, process, timeout_ms } => {
                use fastuse_proto::{wire::WaitForWindowRequest, Request as Req, Response};
                use crate::proto_io::{read_response, write_request};
                use crate::spawn::connect_or_spawn;
                let req = Req::WaitForWindowV2(WaitForWindowRequest {
                    title_substr: title,
                    process_name: process,
                    timeout_ms,
                });
                let mut pipe = connect_or_spawn(&identity.path).await?;
                write_request(&mut pipe, &Req::Hello {
                    client_kind: "cli".into(),
                    client_version: env!("CARGO_PKG_VERSION").into(),
                    requested_idle_timeout_secs: None,
                }).await?;
                let _ = read_response(&mut pipe).await?;
                write_request(&mut pipe, &req).await?;
                match read_response(&mut pipe).await? {
                    Response::Window(w) => {
                        println!("{}", serde_json::to_string(&w)?);
                        Ok(())
                    }
                    Response::Error(e) => anyhow::bail!("{}", e.message),
                    other => anyhow::bail!("unexpected: {other:?}"),
                }
            }
            Cmd::ResizeMoveWindow { hwnd, x, y, w, h, opts } => {
                let hw = parse_hwnd(&hwnd)?;
                let opts = build_action_opts(opts)?;
                cmd_phase2::resize_move_window(&identity.path, hw, x, y, w, h, opts).await
            }

            // ---- Phase 3 ----
            Cmd::Screenshot { monitor, format, out } => {
                cmd_phase3::screenshot(
                    &identity.path,
                    monitor,
                    format.as_deref(),
                    out.as_deref(),
                )
                .await
            }
            Cmd::ScreenshotRegion {
                x,
                y,
                w,
                h,
                monitor,
                format,
                out,
            } => {
                cmd_phase3::screenshot_region(
                    &identity.path,
                    x,
                    y,
                    w,
                    h,
                    monitor,
                    format.as_deref(),
                    out.as_deref(),
                )
                .await
            }
            Cmd::UiaTree { hwnd, depth, view } => {
                let hwnd = match hwnd {
                    Some(s) => Some(parse_hwnd(&s)?),
                    None => None,
                };
                cmd_phase3::uia_tree(&identity.path, hwnd, depth, view.as_deref()).await
            }
            Cmd::UiaQuery { selector, root_hwnd } => {
                let root = match root_hwnd {
                    Some(s) => Some(parse_hwnd(&s)?),
                    None => None,
                };
                cmd_phase3::uia_query(&identity.path, &selector, root).await
            }
            Cmd::InspectAt { x, y } => cmd_phase3::inspect_at_point(&identity.path, x, y).await,
            Cmd::ScrollIntoView { selector, opts } => {
                let opts = build_action_opts(opts)?;
                cmd_phase3::scroll_into_view(&identity.path, &selector, opts).await
            }

            // ---- Phase 4 ----
            Cmd::ClipboardGetText => cmd_phase4::clipboard_get_text(&identity.path).await,
            Cmd::ClipboardSetText { text } => cmd_phase4::clipboard_set_text(&identity.path, text).await,
            Cmd::ShellExec { command, shell, cwd, timeout_ms } => {
                cmd_phase4::shell_exec(&identity.path, command, shell.as_deref(), cwd, timeout_ms).await
            }
            Cmd::LaunchApp { query } => cmd_phase4::launch_app(&identity.path, query).await,
            Cmd::ListProcesses { name, visible_only } => {
                let v = if visible_only { Some(true) } else { None };
                cmd_phase4::list_processes(&identity.path, name, v).await
            }
            Cmd::KillProcess { selector, force, process_tree } => {
                cmd_phase4::kill_process(
                    &identity.path,
                    selector,
                    if force { Some(true) } else { None },
                    if process_tree { Some(true) } else { None },
                ).await
            }

            // ---- v2 computer subcommands ----
            Cmd::Computer(ComputerArgs { action }) => {
                let path = &identity.path;
                match action {
                    ComputerSubcmd::Screenshot { out, monitor, format } => {
                        cmd_computer::screenshot(path, out.as_deref(), monitor, &format).await
                    }
                    ComputerSubcmd::LeftClick { x, y, modifiers, instant } => {
                        cmd_computer::left_click(path, x, y, modifiers, instant).await
                    }
                    ComputerSubcmd::RightClick { x, y, instant } => {
                        cmd_computer::right_click(path, x, y, instant).await
                    }
                    ComputerSubcmd::MiddleClick { x, y, instant } => {
                        cmd_computer::middle_click(path, x, y, instant).await
                    }
                    ComputerSubcmd::DoubleClick { x, y, instant } => {
                        cmd_computer::double_click(path, x, y, instant).await
                    }
                    ComputerSubcmd::TripleClick { x, y, instant } => {
                        cmd_computer::triple_click(path, x, y, instant).await
                    }
                    ComputerSubcmd::Drag { sx, sy, ex, ey, modifiers, instant } => {
                        cmd_computer::drag(path, sx, sy, ex, ey, modifiers, instant).await
                    }
                    ComputerSubcmd::Type { text, instant } => {
                        cmd_computer::type_text(path, text, instant).await
                    }
                    ComputerSubcmd::Key { chord } => {
                        cmd_computer::key(path, chord).await
                    }
                    ComputerSubcmd::HoldKey { chord, ms } => {
                        cmd_computer::hold_key(path, chord, ms).await
                    }
                    ComputerSubcmd::Scroll { x, y, direction, amount } => {
                        cmd_computer::scroll(path, x, y, &direction, amount).await
                    }
                    ComputerSubcmd::MouseMove { x, y, instant } => {
                        cmd_computer::mouse_move(path, x, y, instant).await
                    }
                    ComputerSubcmd::CursorPosition => {
                        cmd_computer::cursor_position(path).await
                    }
                    ComputerSubcmd::Wait { ms } => {
                        cmd_computer::wait(path, ms).await
                    }
                    ComputerSubcmd::Zoom { x, y, factor } => {
                        cmd_computer::zoom(path, x, y, factor).await
                    }
                    ComputerSubcmd::LeftMouseDown { x, y } => {
                        cmd_computer::left_mouse_down(path, x, y).await
                    }
                    ComputerSubcmd::LeftMouseUp { x, y } => {
                        cmd_computer::left_mouse_up(path, x, y).await
                    }
                }
            }

            // ---- setup-mcp ----
            Cmd::SetupMcp(args) => cmd_setup_mcp::run(args.user, args.project),

            // ---- Bench ----
            Cmd::Bench { kind } => match kind {
                BenchCmd::Aimbot { scenario } => {
                    let reports: Vec<bench::aimbot::ScenarioReport> = match scenario.as_str() {
                        "calculator" => vec![bench::aimbot::run_calculator().await],
                        "discord" => vec![bench::aimbot::run_discord().await],
                        "lconnect3" => vec![bench::aimbot::run_lconnect3().await],
                        "all" => vec![
                            bench::aimbot::run_calculator().await,
                            bench::aimbot::run_discord().await,
                            bench::aimbot::run_lconnect3().await,
                        ],
                        other => {
                            eprintln!("unknown scenario: {other}");
                            std::process::exit(2);
                        }
                    };
                    let all_pass = reports.iter().all(|r| r.pass);
                    let json = if reports.len() == 1 {
                        serde_json::to_string_pretty(&reports[0])?
                    } else {
                        serde_json::to_string_pretty(&reports)?
                    };
                    println!("{json}");
                    if !all_pass {
                        std::process::exit(1);
                    }
                    Ok(())
                }
            },

            // ---- Warmup ----
            Cmd::Warmup => {
                use fastuse_proto::{Request, Response};
                use crate::proto_io::{read_response, write_request};
                use crate::spawn::connect_or_spawn;
                let mut pipe = connect_or_spawn(&identity.path).await?;
                write_request(&mut pipe, &Request::Hello {
                    client_kind: "cli".into(),
                    client_version: env!("CARGO_PKG_VERSION").into(),
                    requested_idle_timeout_secs: None,
                }).await?;
                let _ = read_response(&mut pipe).await?;
                write_request(&mut pipe, &Request::Warmup).await?;
                match read_response(&mut pipe).await? {
                    Response::Warmup { capture_us, uia_us, monitors_us, total_us } => {
                        println!("{}", serde_json::json!({
                            "ok": true,
                            "capture_us": capture_us,
                            "uia_us": uia_us,
                            "monitors_us": monitors_us,
                            "total_us": total_us,
                        }));
                        Ok(())
                    }
                    Response::Error(e) => anyhow::bail!("warmup: {}", e.message),
                    other => anyhow::bail!("unexpected response: {other:?}"),
                }
            }
        }
    });

    if let Err(e) = result {
        let err = serde_json::json!({"ok": false, "error": e.to_string()});
        eprintln!("{}", err);
        std::process::exit(1);
    }
}
