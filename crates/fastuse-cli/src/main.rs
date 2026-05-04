//! fastuse-cli — local helper CLI for the daemon.

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
    },
    /// Type literal Unicode text.
    Type {
        /// Text to type. Wrapped in Redact<> end-to-end (D-10).
        text: String,
    },
    /// Press a chord (e.g. ctrl+s).
    Key {
        /// Chord to press.
        chord: String,
        /// Number of full DOWN/UP cycles for the primary key.
        #[arg(long, default_value_t = 1)]
        repeat: u32,
    },
    /// Press a chord and hold for --ms milliseconds.
    HoldKey {
        /// Chord to press.
        chord: String,
        /// Hold duration.
        #[arg(long)]
        ms: u32,
    },
    /// Move the cursor.
    MouseMove {
        /// Target x in physical pixels.
        x: i32,
        /// Target y in physical pixels.
        y: i32,
    },
    /// Mouse-button DOWN at the current cursor position.
    MouseDown {
        /// Button: left | right | middle.
        #[arg(long, default_value = "left")]
        button: String,
    },
    /// Mouse-button UP at the current cursor position.
    MouseUp {
        /// Button: left | right | middle.
        #[arg(long, default_value = "left")]
        button: String,
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
    },
    /// Find an element via selector, focus it, type text.
    TypeIntoElement {
        /// Selector JSON.
        selector: String,
        /// Text to type (Redact<>-wrapped end-to-end).
        text: String,
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
            Cmd::Click { x, y, button, count, mods, no_cursor } =>
                cmd_phase2::click(&identity.path, x, y, &button, count, mods.as_deref(), no_cursor).await,
            Cmd::Type { text } => cmd_phase2::r#type(&identity.path, text).await,
            Cmd::Key { chord, repeat } => cmd_phase2::key(&identity.path, chord, repeat).await,
            Cmd::HoldKey { chord, ms } => cmd_phase2::hold_key(&identity.path, chord, ms).await,
            Cmd::MouseMove { x, y } => cmd_phase2::mouse_move(&identity.path, x, y).await,
            Cmd::MouseDown { button } => cmd_phase2::mouse_down(&identity.path, &button).await,
            Cmd::MouseUp { button } => cmd_phase2::mouse_up(&identity.path, &button).await,
            Cmd::Drag { sx, sy, ex, ey, button, mods } =>
                cmd_phase2::drag(&identity.path, sx, sy, ex, ey, &button, mods.as_deref()).await,
            Cmd::Scroll { x, y, dir, amount, mods } =>
                cmd_phase2::scroll(&identity.path, x, y, &dir, amount, mods.as_deref()).await,
            Cmd::Wait { ms } => cmd_phase2::wait(&identity.path, ms).await,
            Cmd::ListMonitors => cmd_phase2::list_monitors(&identity.path).await,
            Cmd::CursorPosition => cmd_phase2::cursor_position(&identity.path).await,
            Cmd::ForegroundWindow => cmd_phase2::foreground_window(&identity.path).await,
            Cmd::ListWindows { process, title } =>
                cmd_phase2::list_windows(&identity.path, process, title).await,
            Cmd::FocusWindow { hwnd } => {
                let h = parse_hwnd(&hwnd)?;
                cmd_phase2::focus_window(&identity.path, h).await
            }
            Cmd::ResizeMoveWindow { hwnd, x, y, w, h } => {
                let hw = parse_hwnd(&hwnd)?;
                cmd_phase2::resize_move_window(&identity.path, hw, x, y, w, h).await
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
            Cmd::ClickElement { selector, mods } => {
                cmd_phase3::click_element(&identity.path, &selector, mods.as_deref()).await
            }
            Cmd::TypeIntoElement { selector, text } => {
                cmd_phase3::type_into_element(&identity.path, &selector, text).await
            }
            Cmd::WaitForElement { selector, timeout_ms } => {
                cmd_phase3::wait_for_element(&identity.path, &selector, timeout_ms).await
            }
            Cmd::ScrollIntoView { selector } => {
                cmd_phase3::scroll_into_view(&identity.path, &selector).await
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
        }
    });

    if let Err(e) = result {
        let err = serde_json::json!({"ok": false, "error": e.to_string()});
        eprintln!("{}", err);
        std::process::exit(1);
    }
}
