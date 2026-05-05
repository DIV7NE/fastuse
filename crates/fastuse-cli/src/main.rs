//! fastuse-cli — local helper CLI for the daemon.

mod bench;
mod cmd_phase2;
mod cmd_phase3;
mod cmd_phase4;
mod cmd_ping;
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
    /// Find an element via selector and click its centroid.
    ClickElement {
        /// Selector JSON.
        selector: String,
        /// Modifier list, e.g. ctrl,shift.
        #[arg(long)]
        mods: Option<String>,
        #[command(flatten)]
        opts: ActionOptsArgs,
    },
    /// Find an element via selector, focus it, type text.
    TypeIntoElement {
        /// Selector JSON.
        selector: String,
        /// Text to type (Redact<>-wrapped end-to-end).
        text: String,
        #[command(flatten)]
        opts: ActionOptsArgs,
    },
    /// Poll for an element until it appears or timeout.
    WaitForElement {
        /// Selector JSON.
        selector: String,
        /// Timeout in milliseconds (0 = default 5000).
        #[arg(long, default_value_t = 0)]
        timeout_ms: u32,
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

#[derive(clap::Args, Debug, Clone, Default)]
struct ActionOptsArgs {
    /// Selector JSON to poll for after the action.
    #[arg(long)]
    wait_for: Option<String>,
    /// Selector JSON that MUST match within timeout for `verified: true`.
    /// Maps to `ExpectClause::SelectorMatches`.
    #[arg(long)]
    expect_selector: Option<String>,
    /// DEPRECATED: alias for `--expect-selector`. Removed in v1.1.
    #[arg(long, hide = true)]
    verify: Option<String>,
    /// Disable internal strategy escalation on verification miss
    /// (`EscalatePolicy::Strict`).
    #[arg(long, default_value_t = false)]
    strict: bool,
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
    use fastuse_proto::wire::{EscalatePolicy, ExpectClause};
    if a.wait_for.is_none()
        && a.expect_selector.is_none()
        && a.verify.is_none()
        && !a.screenshot_after
        && !a.strict
        && a.wait_timeout_ms.is_none()
    {
        return Ok(None);
    }
    let wait_for = a.wait_for.as_deref().map(serde_json::from_str).transpose()?;
    // expect_selector takes precedence over the deprecated --verify alias.
    let expect = match (a.expect_selector.as_deref(), a.verify.as_deref()) {
        (Some(s), _) | (None, Some(s)) => {
            let sel: fastuse_proto::Selector = serde_json::from_str(s)?;
            Some(ExpectClause::SelectorMatches(sel))
        }
        (None, None) => None,
    };
    let escalate = if a.strict { Some(EscalatePolicy::Strict) } else { None };
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
        expect,
        screenshot_after,
        wait_timeout_ms: a.wait_timeout_ms,
        escalate,
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
            Cmd::ClickElement { selector, mods, opts } => {
                let opts = build_action_opts(opts)?;
                cmd_phase3::click_element(&identity.path, &selector, mods.as_deref(), opts).await
            }
            Cmd::TypeIntoElement { selector, text, opts } => {
                let opts = build_action_opts(opts)?;
                cmd_phase3::type_into_element(&identity.path, &selector, text, opts).await
            }
            Cmd::WaitForElement { selector, timeout_ms } => {
                cmd_phase3::wait_for_element(&identity.path, &selector, timeout_ms).await
            }
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
