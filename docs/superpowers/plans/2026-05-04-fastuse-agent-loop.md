# fastuse Agent Loop Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire Phase 4 system surface into MCP+CLI, add post-action perception parameters (`wait_for`, `screenshot_after`, `verify`) to every action variant, and ship `warmup` so the first agent call after daemon spawn is hot.

**Architecture:** Phase 4 dispatchers already exist in `fastuse-daemon/src/dispatch.rs` — only the MCP `#[tool]` methods and CLI subcommands are missing. Smart parameters add an `ActionOpts` struct to `wire.rs` consumed by every action `Request` variant. The dispatcher invokes a single `apply_action_opts(opts, ctx) -> Response` helper after the inner handler returns Ack/Element. Warmup is a fan-out call to existing per-subsystem warm paths.

**Tech Stack:** Rust 1.83+, rmcp 1.6, clap 4.6, windows 0.62, uiautomation 0.24, image 0.25, base64 0.22.

**Spec:** `docs/superpowers/specs/2026-05-04-fastuse-agent-loop-design.md` (commit `4147fbc`).

---

## Task 1: Wire Phase 4 simple tools into MCP

Adds `clipboard_get_text`, `clipboard_set_text`, `shell_exec`, `launch_app`, `list_processes`, `kill_process` to the MCP handler. Image clipboard is Task 3.

**Files:**
- Modify: `crates/fastuse-mcp/src/handler.rs` (extend with new `#[tool]` methods + arg/output schemas)

- [ ] **Step 1: Add input schemas to `handler.rs` after the existing schemas section (~line 254)**

```rust
// ---------- Phase 4 input schemas ----------

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ClipboardGetTextArgs {}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ClipboardSetTextArgs {
    pub text: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ShellExecArgs {
    pub command: String,
    /// "cmd" (default) | "powershell" | "pwsh" | "bash"
    pub shell: Option<String>,
    /// Working directory.
    pub cwd: Option<String>,
    /// Timeout in milliseconds (default 30000).
    pub timeout_ms: Option<u64>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct LaunchAppArgs {
    pub query: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ListProcessesArgs {
    pub name_contains: Option<String>,
    pub visible_only: Option<bool>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct KillProcessArgs {
    /// Either "pid:1234" or "name:notepad".
    pub selector: String,
    pub force: Option<bool>,
    pub process_tree: Option<bool>,
}

// ---------- Phase 4 output schemas ----------

#[derive(Serialize, schemars::JsonSchema)]
pub struct ClipboardTextOutput {
    pub present: bool,
    pub text: Option<String>,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct ShellExecOutput {
    pub status: i32,
    pub truncated: bool,
    pub duration_ms: u64,
    pub stdout_b64: String,
    pub stderr_b64: String,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct LaunchAppOutput {
    pub pid: u32,
    pub hwnd: Option<isize>,
    pub title: Option<String>,
    pub class: Option<String>,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct KillProcessOutput {
    pub terminated: u32,
}

fn parse_shell_kind(s: Option<&str>) -> Option<fastuse_proto::ShellKind> {
    use fastuse_proto::ShellKind;
    Some(match s.map(str::to_lowercase).as_deref() {
        Some("powershell") => ShellKind::Powershell,
        Some("pwsh") => ShellKind::Pwsh,
        Some("bash") => ShellKind::Bash,
        Some("cmd") | None => ShellKind::Cmd,
        _ => return None,
    })
}

fn parse_proc_selector(s: &str) -> Result<fastuse_proto::ProcessSelector, McpError> {
    use fastuse_proto::ProcessSelector;
    if let Some(pid) = s.strip_prefix("pid:") {
        let pid: u32 = pid.parse().map_err(|e| {
            McpError::invalid_params(format!("pid: {e}"), None)
        })?;
        Ok(ProcessSelector::Pid(pid))
    } else if let Some(name) = s.strip_prefix("name:") {
        Ok(ProcessSelector::Name(name.to_string()))
    } else {
        Err(McpError::invalid_params(
            "selector must be 'pid:<n>' or 'name:<stem>'".to_string(),
            None,
        ))
    }
}
```

- [ ] **Step 2: Add `#[tool]` methods inside the `tool_router` impl block, before the closing `}` (~line 566)**

```rust
    // ---- Phase 4: clipboard ----
    #[tool(name = "clipboard_get_text", description = "Read text from the clipboard. Returns present=false if empty or non-text.")]
    async fn clipboard_get_text(&self, Parameters(_): Parameters<ClipboardGetTextArgs>) -> Result<Json<ClipboardTextOutput>, McpError> {
        let req = Request::ClipboardGet(fastuse_proto::ClipboardGet {
            format: Some(fastuse_proto::ClipFormat::Text),
        });
        match self.call(req).await? {
            Response::ClipboardGet(fastuse_proto::ClipboardGetResp::Text { text }) => {
                Ok(Json(ClipboardTextOutput { present: true, text: Some(text.into_inner()) }))
            }
            Response::ClipboardGet(_) => Ok(Json(ClipboardTextOutput { present: false, text: None })),
            Response::Error(e) => Err(Self::err_from_proto(e)),
            other => Err(McpError::internal_error(format!("unexpected: {other:?}"), None)),
        }
    }

    #[tool(name = "clipboard_set_text", description = "Write text to the clipboard. Payload is wrapped in Redact<> end-to-end.")]
    async fn clipboard_set_text(&self, Parameters(args): Parameters<ClipboardSetTextArgs>) -> Result<Json<AckOutput>, McpError> {
        let req = Request::ClipboardSet(fastuse_proto::ClipboardSet::Text(Redact::new(args.text)));
        match self.call(req).await? {
            Response::ClipboardSet => Ok(Json(AckOutput { ok: true, slept_us: None })),
            Response::Error(e) => Err(Self::err_from_proto(e)),
            other => Err(McpError::internal_error(format!("unexpected: {other:?}"), None)),
        }
    }

    // ---- Phase 4: shell ----
    #[tool(name = "shell_exec", description = "Run a shell command. Permission-gated (Confirmed tier). stdout/stderr returned base64.")]
    async fn shell_exec(&self, Parameters(args): Parameters<ShellExecArgs>) -> Result<Json<ShellExecOutput>, McpError> {
        let shell = parse_shell_kind(args.shell.as_deref())
            .ok_or_else(|| McpError::invalid_params("unknown shell".to_string(), None))?;
        let req = Request::ShellExec(fastuse_proto::ShellExec {
            command: Redact::new(args.command),
            shell: Some(shell),
            env: None,
            cwd: args.cwd,
            timeout_ms: args.timeout_ms,
            stream_chunk_size: None,
        });
        match self.call(req).await? {
            Response::ShellExec(r) => {
                use base64::Engine;
                Ok(Json(ShellExecOutput {
                    status: r.status,
                    truncated: r.truncated,
                    duration_ms: r.duration_ms,
                    stdout_b64: base64::engine::general_purpose::STANDARD.encode(r.stdout.into_inner()),
                    stderr_b64: base64::engine::general_purpose::STANDARD.encode(r.stderr.into_inner()),
                }))
            }
            Response::Error(e) => Err(Self::err_from_proto(e)),
            other => Err(McpError::internal_error(format!("unexpected: {other:?}"), None)),
        }
    }

    // ---- Phase 4: launch / process ----
    #[tool(name = "launch_app", description = "Launch an application by PATH binary, absolute path, or known URI scheme. Returns spawned PID and main HWND if visible within 3s.")]
    async fn launch_app(&self, Parameters(args): Parameters<LaunchAppArgs>) -> Result<Json<LaunchAppOutput>, McpError> {
        let req = Request::LaunchApp(fastuse_proto::LaunchApp { query: args.query });
        match self.call(req).await? {
            Response::LaunchApp(r) => Ok(Json(LaunchAppOutput {
                pid: r.pid,
                hwnd: r.hwnd,
                title: r.title,
                class: r.class,
            })),
            Response::Error(e) => Err(Self::err_from_proto(e)),
            other => Err(McpError::internal_error(format!("unexpected: {other:?}"), None)),
        }
    }

    #[tool(name = "list_processes", description = "Enumerate running processes with optional name substring filter and visible-window filter.")]
    async fn list_processes(&self, Parameters(args): Parameters<ListProcessesArgs>) -> Result<Json<Vec<fastuse_proto::ProcessInfo>>, McpError> {
        let filter = if args.name_contains.is_some() || args.visible_only.is_some() {
            Some(fastuse_proto::ProcFilter {
                name_contains: args.name_contains,
                visible_only: args.visible_only,
            })
        } else {
            None
        };
        let req = Request::ListProcesses(fastuse_proto::ListProcesses { filter });
        match self.call(req).await? {
            Response::ListProcesses(v) => Ok(Json(v)),
            Response::Error(e) => Err(Self::err_from_proto(e)),
            other => Err(McpError::internal_error(format!("unexpected: {other:?}"), None)),
        }
    }

    #[tool(name = "kill_process", description = "Terminate a process by 'pid:<n>' or 'name:<stem>'. Permission-gated (Confirmed tier).")]
    async fn kill_process(&self, Parameters(args): Parameters<KillProcessArgs>) -> Result<Json<KillProcessOutput>, McpError> {
        let selector = parse_proc_selector(&args.selector)?;
        let req = Request::KillProcess(fastuse_proto::KillProcess {
            selector,
            force: args.force,
            process_tree: args.process_tree,
        });
        match self.call(req).await? {
            Response::KillProcess { terminated } => Ok(Json(KillProcessOutput { terminated })),
            Response::Error(e) => Err(Self::err_from_proto(e)),
            other => Err(McpError::internal_error(format!("unexpected: {other:?}"), None)),
        }
    }
```

- [ ] **Step 3: Build & verify**

Run: `cargo build -p fastuse-mcp --release`
Expected: clean build.

- [ ] **Step 4: Commit**

```bash
git add crates/fastuse-mcp/src/handler.rs
git commit -m "feat(mcp): wire phase4 clipboard text / shell / launch / processes"
```

---

## Task 2: Wire Phase 4 simple tools into CLI

**Files:**
- Create: `crates/fastuse-cli/src/cmd_phase4.rs`
- Modify: `crates/fastuse-cli/src/main.rs` (add module + Cmd variants + dispatch arms)

- [ ] **Step 1: Create `cmd_phase4.rs`**

```rust
//! Phase 4 system-surface CLI subcommands.
//!
//! Pattern matches `cmd_phase3.rs`: one_call + handshake helpers, JSON output.

use fastuse_proto::{
    ClipFormat, ClipboardGet, ClipboardGetResp, ClipboardSet, KillProcess, LaunchApp,
    ListProcesses, ProcFilter, ProcessSelector, Redact, Request, Response, ShellExec, ShellKind,
};
use serde_json::json;
use tokio::net::windows::named_pipe::NamedPipeClient;

use crate::proto_io::{read_response, write_request};
use crate::spawn::connect_or_spawn;

async fn one_call(pipe_path: &str, req: Request) -> anyhow::Result<Response> {
    let mut pipe = connect_or_spawn(pipe_path).await?;
    handshake(&mut pipe).await?;
    write_request(&mut pipe, &req).await?;
    Ok(read_response(&mut pipe).await?)
}

async fn handshake(pipe: &mut NamedPipeClient) -> anyhow::Result<()> {
    let hello = Request::Hello {
        client_kind: "cli".into(),
        client_version: env!("CARGO_PKG_VERSION").into(),
        requested_idle_timeout_secs: None,
    };
    write_request(pipe, &hello).await?;
    let _ = read_response(pipe).await?;
    Ok(())
}

fn parse_shell(s: Option<&str>) -> anyhow::Result<ShellKind> {
    Ok(match s.map(str::to_lowercase).as_deref() {
        Some("powershell") => ShellKind::Powershell,
        Some("pwsh") => ShellKind::Pwsh,
        Some("bash") => ShellKind::Bash,
        Some("cmd") | None => ShellKind::Cmd,
        Some(other) => anyhow::bail!("unknown shell: {other}"),
    })
}

fn parse_proc_selector(s: &str) -> anyhow::Result<ProcessSelector> {
    if let Some(pid) = s.strip_prefix("pid:") {
        Ok(ProcessSelector::Pid(pid.parse()?))
    } else if let Some(name) = s.strip_prefix("name:") {
        Ok(ProcessSelector::Name(name.to_string()))
    } else {
        anyhow::bail!("selector must be 'pid:<n>' or 'name:<stem>'")
    }
}

pub async fn clipboard_get_text(pipe_path: &str) -> anyhow::Result<()> {
    let req = Request::ClipboardGet(ClipboardGet { format: Some(ClipFormat::Text) });
    match one_call(pipe_path, req).await? {
        Response::ClipboardGet(ClipboardGetResp::Text { text }) => {
            println!("{}", json!({"ok": true, "present": true, "text": text.into_inner()}));
            Ok(())
        }
        Response::ClipboardGet(_) => {
            println!("{}", json!({"ok": true, "present": false}));
            Ok(())
        }
        Response::Error(e) => print_err(e),
        other => Ok(println!("{}", json!({"unexpected": format!("{other:?}")}))),
    }
}

pub async fn clipboard_set_text(pipe_path: &str, text: String) -> anyhow::Result<()> {
    let req = Request::ClipboardSet(ClipboardSet::Text(Redact::new(text)));
    match one_call(pipe_path, req).await? {
        Response::ClipboardSet => {
            println!("{}", json!({"ok": true}));
            Ok(())
        }
        Response::Error(e) => print_err(e),
        other => Ok(println!("{}", json!({"unexpected": format!("{other:?}")}))),
    }
}

pub async fn shell_exec(
    pipe_path: &str,
    command: String,
    shell: Option<&str>,
    cwd: Option<String>,
    timeout_ms: Option<u64>,
) -> anyhow::Result<()> {
    let req = Request::ShellExec(ShellExec {
        command: Redact::new(command),
        shell: Some(parse_shell(shell)?),
        env: None,
        cwd,
        timeout_ms,
        stream_chunk_size: None,
    });
    match one_call(pipe_path, req).await? {
        Response::ShellExec(r) => {
            use base64::Engine;
            println!(
                "{}",
                json!({
                    "ok": true,
                    "status": r.status,
                    "truncated": r.truncated,
                    "duration_ms": r.duration_ms,
                    "stdout_b64": base64::engine::general_purpose::STANDARD.encode(r.stdout.into_inner()),
                    "stderr_b64": base64::engine::general_purpose::STANDARD.encode(r.stderr.into_inner()),
                })
            );
            Ok(())
        }
        Response::Error(e) => print_err(e),
        other => Ok(println!("{}", json!({"unexpected": format!("{other:?}")}))),
    }
}

pub async fn launch_app(pipe_path: &str, query: String) -> anyhow::Result<()> {
    let req = Request::LaunchApp(LaunchApp { query });
    match one_call(pipe_path, req).await? {
        Response::LaunchApp(r) => {
            println!(
                "{}",
                json!({"ok": true, "pid": r.pid, "hwnd": r.hwnd, "title": r.title, "class": r.class})
            );
            Ok(())
        }
        Response::Error(e) => print_err(e),
        other => Ok(println!("{}", json!({"unexpected": format!("{other:?}")}))),
    }
}

pub async fn list_processes(
    pipe_path: &str,
    name_contains: Option<String>,
    visible_only: Option<bool>,
) -> anyhow::Result<()> {
    let filter = (name_contains.is_some() || visible_only.is_some()).then(|| ProcFilter {
        name_contains,
        visible_only,
    });
    let req = Request::ListProcesses(ListProcesses { filter });
    match one_call(pipe_path, req).await? {
        Response::ListProcesses(v) => {
            println!("{}", json!({"ok": true, "processes": v}));
            Ok(())
        }
        Response::Error(e) => print_err(e),
        other => Ok(println!("{}", json!({"unexpected": format!("{other:?}")}))),
    }
}

pub async fn kill_process(
    pipe_path: &str,
    selector: String,
    force: Option<bool>,
    process_tree: Option<bool>,
) -> anyhow::Result<()> {
    let req = Request::KillProcess(KillProcess {
        selector: parse_proc_selector(&selector)?,
        force,
        process_tree,
    });
    match one_call(pipe_path, req).await? {
        Response::KillProcess { terminated } => {
            println!("{}", json!({"ok": true, "terminated": terminated}));
            Ok(())
        }
        Response::Error(e) => print_err(e),
        other => Ok(println!("{}", json!({"unexpected": format!("{other:?}")}))),
    }
}

fn print_err(e: fastuse_proto::Error) -> anyhow::Result<()> {
    let body = json!({
        "ok": false,
        "error": { "code": e.code.as_str(), "message": e.message, "hint": e.hint }
    });
    eprintln!("{body}");
    std::process::exit(1);
}
```

- [ ] **Step 2: Register the module in `main.rs` line ~3 (after `mod cmd_phase3;`)**

Add line: `mod cmd_phase4;`

- [ ] **Step 3: Add Cmd variants in `main.rs` after the Phase 3 block (~line 265)**

```rust
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
```

- [ ] **Step 4: Add dispatch arms in `main.rs` before the closing brace of the match (~line 385)**

```rust
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
```

- [ ] **Step 5: Build & verify**

Run: `cargo build -p fastuse-cli --release`
Expected: clean build.

- [ ] **Step 6: Smoke test**

Run: `./target/release/fastuse-cli.exe launch-app calc.exe`
Expected: JSON with `ok: true` and a non-zero `pid`. (Calc opens.)

- [ ] **Step 7: Commit**

```bash
git add crates/fastuse-cli/src/cmd_phase4.rs crates/fastuse-cli/src/main.rs
git commit -m "feat(cli): wire phase4 clipboard text / shell / launch / processes"
```

---

## Task 3: Clipboard image round-trip codec

The daemon's `fastuse_win::clipboard::clipboard_get` and `clipboard_set` already exist but the image arms return `None` / `Internal`. This task wires the codec.

**Files:**
- Modify: `crates/fastuse-win/src/clipboard/mod.rs` (implement image arms)
- Modify: `crates/fastuse-mcp/src/handler.rs` (add `clipboard_get_image` / `clipboard_set_image` tools)
- Modify: `crates/fastuse-cli/src/cmd_phase4.rs` (add image variants)
- Modify: `crates/fastuse-cli/src/main.rs` (add Cmd variants)

- [ ] **Step 1: Read current clipboard module to confirm shape**

Run: `wc -l crates/fastuse-win/src/clipboard/mod.rs && grep -n "ClipboardSet::Image\|ClipboardGetResp::Image" crates/fastuse-win/src/clipboard/mod.rs`
Expected: 238 lines, image arms present but unimplemented.

- [ ] **Step 2: Implement DIBV5 → PNG decode and PNG/JPEG → DIBV5 encode in `crates/fastuse-win/src/clipboard/mod.rs`**

Replace the current image arms with full implementations using `image` 0.25:
- `clipboard_get` Image branch: open clipboard, read `CF_DIBV5`, parse `BITMAPV5HEADER`, build `image::RgbaImage`, encode as PNG via `image::codecs::png::PngEncoder`, return `ClipboardGetResp::Image { mime: "image/png", base64: <encoded>, w, h }`. Wrap encoded bytes via `Redact::new` after base64.
- `clipboard_set` Image branch: decode incoming bytes via `image::load_from_memory_with_format`, convert to `RgbaImage`, build a `BITMAPV5HEADER` (V5_USE_ALPHA, sRGB color space), `OpenClipboard` + `EmptyClipboard` + `SetClipboardData(CF_DIBV5, GMEM_MOVEABLE)`. Use `windows::Win32::Graphics::Gdi::BITMAPV5HEADER` and `windows::Win32::System::DataExchange`.

Detailed sub-steps:

```rust
// inside clipboard_get, replace the unimplemented image arm:
ClipFormat::Image => {
    use image::{ImageEncoder, codecs::png::PngEncoder};
    use base64::Engine;
    let dib = read_cf_dibv5()?; // existing helper or new one returning DibImage { rgba: Vec<u8>, w: u32, h: u32 }
    let mut png = Vec::with_capacity(dib.rgba.len() / 2);
    PngEncoder::new(&mut png)
        .write_image(&dib.rgba, dib.w, dib.h, image::ExtendedColorType::Rgba8)
        .map_err(|e| Error::new(ErrorCode::Internal, format!("png encode: {e}")))?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
    Ok(ClipboardGetResp::Image {
        mime: "image/png".into(),
        base64: Redact::new(b64),
        w: dib.w,
        h: dib.h,
    })
}
```

```rust
// inside clipboard_set, replace the unimplemented image arm:
ClipboardSet::Image { mime, bytes, w, h } => {
    let format = match mime.as_str() {
        "image/png" => image::ImageFormat::Png,
        "image/jpeg" | "image/jpg" => image::ImageFormat::Jpeg,
        other => return Err(Error::new(ErrorCode::InvalidParams, format!("unsupported mime: {other}"))),
    };
    let img = image::load_from_memory_with_format(&bytes.into_inner(), format)
        .map_err(|e| Error::new(ErrorCode::InvalidParams, format!("decode: {e}")))?
        .to_rgba8();
    if img.width() != w || img.height() != h {
        return Err(Error::new(ErrorCode::InvalidParams, "w/h mismatch".into()));
    }
    write_cf_dibv5(&img.into_raw(), w, h)?;
    Ok(())
}
```

- [ ] **Step 3: Implement `read_cf_dibv5` and `write_cf_dibv5` helpers (in same file, private)**

```rust
struct DibImage {
    rgba: Vec<u8>,
    w: u32,
    h: u32,
}

fn read_cf_dibv5() -> Result<DibImage, Error> {
    use windows::Win32::System::DataExchange::{OpenClipboard, CloseClipboard, GetClipboardData};
    use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock, GlobalSize};
    use windows::Win32::Graphics::Gdi::BITMAPV5HEADER;

    unsafe {
        OpenClipboard(None).map_err(|e| Error::new(ErrorCode::Internal, format!("OpenClipboard: {e}")))?;
        let result = (|| -> Result<DibImage, Error> {
            let h = GetClipboardData(17 /* CF_DIBV5 */)
                .map_err(|e| Error::new(ErrorCode::Internal, format!("GetClipboardData: {e}")))?;
            let size = GlobalSize(windows::Win32::Foundation::HGLOBAL(h.0 as _));
            if size < std::mem::size_of::<BITMAPV5HEADER>() {
                return Err(Error::new(ErrorCode::Internal, "DIBV5 too small".into()));
            }
            let ptr = GlobalLock(windows::Win32::Foundation::HGLOBAL(h.0 as _)) as *const u8;
            if ptr.is_null() {
                return Err(Error::new(ErrorCode::Internal, "GlobalLock null".into()));
            }
            let header: &BITMAPV5HEADER = &*(ptr as *const BITMAPV5HEADER);
            let w = header.bV5Width as u32;
            let h_abs = header.bV5Height.unsigned_abs();
            let bottom_up = header.bV5Height > 0;
            let bpp = header.bV5BitCount as u32;
            if bpp != 32 {
                let _ = GlobalUnlock(windows::Win32::Foundation::HGLOBAL((ptr as *mut u8) as _));
                return Err(Error::new(ErrorCode::Internal, format!("unsupported bpp: {bpp}")));
            }
            let pixel_offset = std::mem::size_of::<BITMAPV5HEADER>();
            let stride = (w * 4) as usize;
            let mut rgba = vec![0u8; (h_abs as usize) * stride];
            let src = ptr.add(pixel_offset);
            for row in 0..h_abs as usize {
                let src_row = if bottom_up { (h_abs as usize - 1 - row) * stride } else { row * stride };
                for col in 0..w as usize {
                    let s = src.add(src_row + col * 4);
                    let d = rgba.as_mut_ptr().add(row * stride + col * 4);
                    *d.add(0) = *s.add(2); // B->R
                    *d.add(1) = *s.add(1); // G
                    *d.add(2) = *s.add(0); // R->B
                    *d.add(3) = *s.add(3); // A
                }
            }
            let _ = GlobalUnlock(windows::Win32::Foundation::HGLOBAL((ptr as *mut u8) as _));
            Ok(DibImage { rgba, w, h: h_abs })
        })();
        let _ = CloseClipboard();
        result
    }
}

fn write_cf_dibv5(rgba: &[u8], w: u32, h: u32) -> Result<(), Error> {
    use windows::Win32::System::DataExchange::{OpenClipboard, CloseClipboard, EmptyClipboard, SetClipboardData};
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    use windows::Win32::Graphics::Gdi::{BITMAPV5HEADER, LCS_sRGB};

    let header_size = std::mem::size_of::<BITMAPV5HEADER>();
    let pixel_size = (w * h * 4) as usize;
    let total = header_size + pixel_size;

    unsafe {
        let hmem = GlobalAlloc(GMEM_MOVEABLE, total)
            .map_err(|e| Error::new(ErrorCode::Internal, format!("GlobalAlloc: {e}")))?;
        let dst = GlobalLock(hmem) as *mut u8;
        if dst.is_null() {
            return Err(Error::new(ErrorCode::Internal, "GlobalLock null".into()));
        }
        let header = &mut *(dst as *mut BITMAPV5HEADER);
        *header = std::mem::zeroed();
        header.bV5Size = header_size as u32;
        header.bV5Width = w as i32;
        header.bV5Height = -(h as i32); // top-down
        header.bV5Planes = 1;
        header.bV5BitCount = 32;
        header.bV5Compression = 3 /* BI_BITFIELDS */;
        header.bV5SizeImage = pixel_size as u32;
        header.bV5RedMask = 0x00FF0000;
        header.bV5GreenMask = 0x0000FF00;
        header.bV5BlueMask = 0x000000FF;
        header.bV5AlphaMask = 0xFF000000;
        header.bV5CSType = LCS_sRGB.0 as u32;

        let pixels = dst.add(header_size);
        for row in 0..h as usize {
            for col in 0..w as usize {
                let s = rgba.as_ptr().add(row * (w * 4) as usize + col * 4);
                let d = pixels.add(row * (w * 4) as usize + col * 4);
                *d.add(0) = *s.add(2); // R->B
                *d.add(1) = *s.add(1); // G
                *d.add(2) = *s.add(0); // B->R
                *d.add(3) = *s.add(3); // A
            }
        }
        let _ = GlobalUnlock(hmem);

        OpenClipboard(None).map_err(|e| Error::new(ErrorCode::Internal, format!("OpenClipboard: {e}")))?;
        let _ = EmptyClipboard();
        SetClipboardData(17 /* CF_DIBV5 */, Some(windows::Win32::Foundation::HANDLE(hmem.0)))
            .map_err(|e| Error::new(ErrorCode::Internal, format!("SetClipboardData: {e}")))?;
        let _ = CloseClipboard();
    }
    Ok(())
}
```

- [ ] **Step 4: Add `clipboard_get_image` / `clipboard_set_image` MCP tools**

In `handler.rs`, after `clipboard_set_text`:

```rust
    #[tool(name = "clipboard_get_image", description = "Read image from the clipboard. Returns base64 PNG.")]
    async fn clipboard_get_image(&self) -> Result<Json<ScreenshotOutput>, McpError> {
        let req = Request::ClipboardGet(fastuse_proto::ClipboardGet {
            format: Some(fastuse_proto::ClipFormat::Image),
        });
        match self.call(req).await? {
            Response::ClipboardGet(fastuse_proto::ClipboardGetResp::Image { mime, base64, w, h }) => {
                Ok(Json(ScreenshotOutput { mime, width: w, height: h, data_b64: base64.into_inner() }))
            }
            Response::ClipboardGet(_) => Err(McpError::internal_error("no image on clipboard".into(), None)),
            Response::Error(e) => Err(Self::err_from_proto(e)),
            other => Err(McpError::internal_error(format!("unexpected: {other:?}"), None)),
        }
    }

    #[tool(name = "clipboard_set_image", description = "Write a base64-encoded PNG/JPEG to the clipboard.")]
    async fn clipboard_set_image(&self, Parameters(args): Parameters<ClipboardSetImageArgs>) -> Result<Json<AckOutput>, McpError> {
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD.decode(&args.data_b64)
            .map_err(|e| McpError::invalid_params(format!("data_b64: {e}"), None))?;
        let req = Request::ClipboardSet(fastuse_proto::ClipboardSet::Image {
            mime: args.mime,
            bytes: Redact::new(bytes),
            w: args.width,
            h: args.height,
        });
        match self.call(req).await? {
            Response::ClipboardSet => Ok(Json(AckOutput { ok: true, slept_us: None })),
            Response::Error(e) => Err(Self::err_from_proto(e)),
            other => Err(McpError::internal_error(format!("unexpected: {other:?}"), None)),
        }
    }
```

And add the input schema near other Phase 4 schemas:

```rust
#[derive(Deserialize, schemars::JsonSchema)]
pub struct ClipboardSetImageArgs {
    /// MIME type: image/png or image/jpeg.
    pub mime: String,
    pub width: u32,
    pub height: u32,
    /// Base64-encoded image bytes.
    pub data_b64: String,
}
```

- [ ] **Step 5: Build & verify**

Run: `cargo build --release --workspace`
Expected: clean.

- [ ] **Step 6: Round-trip test (manual)**

Run:
```bash
./target/release/fastuse-cli.exe screenshot --out /tmp/sc.png
# pipe sc.png base64 into clipboard_set_image then clipboard_get_image, compare bytes
```
Expected: same w/h, perceptually identical image.

- [ ] **Step 7: Commit**

```bash
git add crates/fastuse-win/src/clipboard/mod.rs crates/fastuse-mcp/src/handler.rs
git commit -m "feat(clipboard): DIBV5 image round-trip via PNG"
```

---

## Task 4: Add `ActionOpts` to wire protocol

The shared post-action perception envelope. Every action variant grows an optional `opts` field of this type. Old wire decoders treat absent `opts` as `None` (postcard `Option<T>` is forward-compatible — appending optional fields keeps old clients working).

**Files:**
- Modify: `crates/fastuse-proto/src/wire.rs`

- [ ] **Step 1: Write the failing round-trip test**

Add to the `tests` module of `wire.rs` (~line 870 area):

```rust
#[test]
fn click_with_action_opts_round_trips() {
    use crate::selector::Selector;
    use crate::uia_node::ControlType;
    let req = Request::Click {
        x: 100,
        y: 200,
        button: MouseButton::Left,
        count: 1,
        modifiers: vec![],
        skip_set_cursor_pos: false,
        opts: Some(ActionOpts {
            wait_for: Some(Selector::ByName("Saved".into())),
            screenshot_after: Some(ScreenshotOpts {
                region: Some(RegionSpec::Auto),
                format: Some(ImageFormat::Jpeg),
                quality: Some(85),
            }),
            verify: Some(Selector::ByControlType(ControlType::Window)),
            wait_timeout_ms: Some(2000),
        }),
    };
    let bytes = encode_frame(&req).unwrap();
    let mut cur = std::io::Cursor::new(bytes);
    let decoded: Request = decode_frame(&mut cur).unwrap();
    assert_eq!(req, decoded);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p fastuse-proto click_with_action_opts_round_trips`
Expected: compile errors (`ActionOpts`, `ScreenshotOpts`, `RegionSpec` not found; `opts` field not in Click variant).

- [ ] **Step 3: Add the new types to `wire.rs` (above the `Request` enum, ~line 19)**

```rust
/// Region specifier for `ScreenshotOpts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RegionSpec {
    /// Capture the foreground window's client rect at the moment the
    /// post-action snapshot fires.
    Auto,
    /// Explicit rectangle in physical-pixel virtual-desktop coordinates.
    Rect {
        /// Top-left x.
        x: i32,
        /// Top-left y.
        y: i32,
        /// Width in physical pixels.
        w: u32,
        /// Height in physical pixels.
        h: u32,
    },
}

/// Post-action screenshot options bundled into `ActionOpts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenshotOpts {
    /// Region to capture; `None` = whole primary monitor.
    pub region: Option<RegionSpec>,
    /// Output format; `None` = JPEG (default).
    pub format: Option<ImageFormat>,
    /// JPEG quality (0..=100); ignored for PNG. `None` = 85.
    pub quality: Option<u8>,
}

/// Optional post-action perception bundle — collapses perceive→act→perceive
/// into a single tool call. Absent on legacy clients (decoded as `None`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionOpts {
    /// Poll UIA after the action until this selector matches or
    /// `wait_timeout_ms` elapses. The action's response includes
    /// `waited_ms` and `wait_matched` so the agent can branch.
    pub wait_for: Option<Selector>,
    /// Capture a screenshot after the action (and after `wait_for` if set).
    pub screenshot_after: Option<ScreenshotOpts>,
    /// Like `wait_for`, but timeout = action failed.
    pub verify: Option<Selector>,
    /// Timeout shared by `wait_for` and `verify`. Default 2000ms when unset.
    pub wait_timeout_ms: Option<u32>,
}
```

- [ ] **Step 4: Add `opts: Option<ActionOpts>` field to every action variant in `Request`**

Variants to amend (these are the ones that perform an action and benefit from post-action perception):

```
Click, MouseMove, MouseDown, MouseUp, Drag, Scroll, Type, Key, HoldKey,
FocusWindow, ResizeMoveWindow,
ClickElement, TypeIntoElement, ScrollIntoView,
ClipboardSet, LaunchApp
```

Pure-read variants (`ListMonitors`, `CursorPosition`, `ForegroundWindow`, `ListWindows`, `Screenshot`, `ScreenshotRegion`, `UiaTree`, `UiaQuery`, `InspectAtPoint`, `WaitForElement`, `ClipboardGet`, `ListProcesses`) and lifecycle (`Hello`, `Ping`, `Shutdown`, `Wait`) do NOT get `opts` — they're already idempotent perception or have no act-side effect to follow up on. `KillProcess` also skips: post-kill the target window is gone.

`ShellExec` skips: it returns its own structured result and `wait_for` against UIA after a shell command is meaningless to a typical agent flow.

For each named variant, add the field at the end of the brace-style payload:

```rust
Request::Click {
    x: i32,
    y: i32,
    button: MouseButton,
    count: u8,
    modifiers: Vec<String>,
    skip_set_cursor_pos: bool,
    /// Optional post-action perception bundle (Phase 5 agent-loop wins).
    opts: Option<ActionOpts>,  // <-- new field
},
```

For tuple-style payloads (`Type { text: Redact<String> }` etc.), add the field as a sibling.

- [ ] **Step 5: Add new `Response::ActionResult` variant capturing post-action perception**

After the existing `Response::Element` variant (~line 615):

```rust
    /// Action result with optional post-action perception. Used when the
    /// caller passed `ActionOpts`. Without `opts`, dispatchers continue to
    /// return `Ack` / `Element` as before.
    ActionResult {
        /// True if `verify` was None or matched within timeout.
        ok: bool,
        /// `wait_for` outcome: `Some(true)` matched, `Some(false)` timed out,
        /// `None` not requested.
        wait_matched: Option<bool>,
        /// Wall-clock time spent in `wait_for` polling.
        waited_ms: u32,
        /// Optional post-action screenshot.
        screenshot: Option<Box<ScreenshotPayload>>,
    },
```

And add the helper struct near `ScreenshotOpts`:

```rust
/// Screenshot payload re-used by `ActionResult` and `Response::Screenshot`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScreenshotPayload {
    pub bytes: Redact<Vec<u8>>,
    pub mime: String,
    pub width: u32,
    pub height: u32,
}
```

- [ ] **Step 6: Run round-trip test, expect PASS**

Run: `cargo test -p fastuse-proto click_with_action_opts_round_trips`
Expected: PASS. (Other tests will fail to compile until call sites are updated — that's Task 5.)

- [ ] **Step 7: Update existing in-tree call sites that construct Request variants**

Search for every constructor call:

Run: `cargo build -p fastuse-proto && cargo build -p fastuse-cli 2>&1 | head -100`
Expected: compile errors at construction sites in `cmd_phase2.rs` and `cmd_phase3.rs`.

For each error, add `opts: None` to the struct literal. Same for `cmd_phase4.rs` `ClipboardSet` / `LaunchApp` arms (Task 2 already wrote them — they need updating now).

Also update `wire.rs`'s own internal tests (search for `Request::Click {`, `Request::Drag {`, etc. in the `tests` module — every existing literal needs `opts: None`).

- [ ] **Step 8: Build & verify whole workspace compiles**

Run: `cargo build --workspace`
Expected: clean. Tests don't need to pass yet — Task 5 implements server side.

- [ ] **Step 9: Commit**

```bash
git add crates/fastuse-proto/src/wire.rs crates/fastuse-cli/
git commit -m "feat(proto): add ActionOpts envelope for post-action perception"
```

---

## Task 5: Implement `apply_action_opts` helper in daemon

Single helper used by every action-arm in `dispatch.rs`. Takes the inner result and `Option<ActionOpts>`; if `opts` is `None`, returns the original `Ack`/`Element` response. If `Some`, runs `wait_for` / `verify` / `screenshot_after` and returns `ActionResult`.

**Files:**
- Create: `crates/fastuse-daemon/src/action_opts.rs`
- Modify: `crates/fastuse-daemon/src/main.rs` (declare module)
- Modify: `crates/fastuse-daemon/src/dispatch.rs` (use helper from one variant for now)

- [ ] **Step 1: Create the helper**

```rust
//! Post-action perception helper (`ActionOpts`).
//!
//! Exactly one entry point: `apply` runs `wait_for`, `verify`, and
//! `screenshot_after` against the existing UIA pool + capture thread,
//! returning a `Response::ActionResult` that bundles them. Inner action
//! must have already executed before `apply` is called.

use std::time::{Duration, Instant};

use fastuse_proto::{
    coords::Rect, ActionOpts, Error, ErrorCode, ImageFormat, RegionSpec, Response,
    ScreenshotPayload, Selector,
};
use fastuse_win::capture::{handle_screenshot, handle_screenshot_region};
use fastuse_win::capture_thread::CaptureThreadHandle;
use fastuse_win::uia::{
    automation::foreground_hwnd, cache::get_or_fetch, find::find_first,
};
use fastuse_win::uia_pool::UiaPoolHandle;

const DEFAULT_WAIT_TIMEOUT_MS: u32 = 2000;
const POLL_CADENCE: Duration = Duration::from_millis(50);

pub struct OptsCtx<'a> {
    pub uia: Option<&'a UiaPoolHandle>,
    pub capture: Option<&'a CaptureThreadHandle>,
}

/// Apply post-action perception. `inner_ok` reports whether the action
/// itself succeeded; if false we still try to gather perception (best-effort)
/// but mark `ok: false`.
pub fn apply(opts: ActionOpts, inner_ok: bool, ctx: OptsCtx<'_>) -> Response {
    let timeout_ms = opts.wait_timeout_ms.unwrap_or(DEFAULT_WAIT_TIMEOUT_MS);

    let (wait_matched, waited_ms) = match opts.wait_for.as_ref() {
        None => (None, 0u32),
        Some(sel) => {
            let start = Instant::now();
            let m = poll_selector(ctx.uia, sel.clone(), timeout_ms);
            (Some(m), start.elapsed().as_millis() as u32)
        }
    };

    let verify_ok = match opts.verify.as_ref() {
        None => true,
        Some(sel) => poll_selector(ctx.uia, sel.clone(), timeout_ms),
    };

    let screenshot = opts
        .screenshot_after
        .and_then(|so| capture_after(ctx.capture, so).ok());

    Response::ActionResult {
        ok: inner_ok && verify_ok,
        wait_matched,
        waited_ms,
        screenshot: screenshot.map(Box::new),
    }
}

fn poll_selector(uia: Option<&UiaPoolHandle>, sel: Selector, timeout_ms: u32) -> bool {
    let Some(pool) = uia else { return false };
    let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64);
    loop {
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        let s = sel.clone();
        let hwnd = match foreground_hwnd() {
            Some(h) => h,
            None => {
                std::thread::sleep(POLL_CADENCE);
                continue;
            }
        };
        let probe: Result<Option<fastuse_proto::UIANode>, Error> = pool.run(move |uia| {
            let root = get_or_fetch(uia, hwnd).map_err(|_| {
                Error::new(ErrorCode::WindowNotFound, "foreground HWND vanished".to_string())
            })?;
            find_first(uia, &root, &s)
        });
        if matches!(probe, Ok(Some(_))) {
            return true;
        }
        let remaining = POLL_CADENCE.saturating_sub(now.elapsed());
        if !remaining.is_zero() {
            std::thread::sleep(remaining);
        }
    }
}

fn capture_after(
    capture: Option<&CaptureThreadHandle>,
    opts: fastuse_proto::ScreenshotOpts,
) -> Result<ScreenshotPayload, ()> {
    let cap = capture.ok_or(())?;
    let format = opts.format.unwrap_or(ImageFormat::Jpeg);
    let resp = match opts.region {
        None => handle_screenshot(cap, None, Some(format)).map_err(|_| ())?,
        Some(RegionSpec::Auto) => {
            let (x, y, w, h) = foreground_rect().ok_or(())?;
            handle_screenshot_region(cap, Rect { x, y, w: w as i32, h: h as i32 }, None, Some(format))
                .map_err(|_| ())?
        }
        Some(RegionSpec::Rect { x, y, w, h }) => {
            handle_screenshot_region(cap, Rect { x, y, w: w as i32, h: h as i32 }, None, Some(format))
                .map_err(|_| ())?
        }
    };
    match resp {
        Response::Screenshot { bytes, mime, width, height } => {
            Ok(ScreenshotPayload { bytes, mime, width, height })
        }
        _ => Err(()),
    }
}

fn foreground_rect() -> Option<(i32, i32, u32, u32)> {
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowRect};
    unsafe {
        let hwnd: HWND = GetForegroundWindow();
        if hwnd.0 == 0 {
            return None;
        }
        let mut r = RECT::default();
        if GetWindowRect(hwnd, &mut r).is_err() {
            return None;
        }
        let w = (r.right - r.left) as u32;
        let h = (r.bottom - r.top) as u32;
        Some((r.left, r.top, w, h))
    }
}
```

- [ ] **Step 2: Register module in `crates/fastuse-daemon/src/main.rs`**

Add line near other `mod` declarations:
```rust
mod action_opts;
```

- [ ] **Step 3: Wire `Click` arm in `dispatch.rs` to use `apply_action_opts` (reference implementation)**

Replace the existing `Request::Click { ... }` arm with:

```rust
Request::Click {
    x, y, button, count, modifiers, skip_set_cursor_pos, opts,
} => {
    let inner = run_unit(ctx, &mut win32_us, move || {
        ih::click(x, y, button, count, &modifiers, skip_set_cursor_pos)
    });
    finalize(inner, opts, ctx)
}
```

And add `finalize` helper at the bottom of `dispatch.rs`:

```rust
fn finalize(inner: Response, opts: Option<fastuse_proto::ActionOpts>, ctx: &DispatchCtx) -> Response {
    let Some(opts) = opts else { return inner };
    let inner_ok = matches!(inner, Response::Ack { .. } | Response::Element { matched: true });
    crate::action_opts::apply(
        opts,
        inner_ok,
        crate::action_opts::OptsCtx {
            uia: ctx.uia.as_deref(),
            capture: ctx.capture.as_deref(),
        },
    )
}
```

(`Arc<T>::as_deref()` doesn't exist; use `ctx.uia.as_ref().map(|a| a.as_ref())` if needed — the `OptsCtx` field types in Task 5/Step 1 are `Option<&UiaPoolHandle>` so use `.as_ref().map(|a| &**a)` or change `OptsCtx` to take `Option<&Arc<...>>`. Pick the option with no allocation; if compiler complains, change `OptsCtx` to:
```rust
pub struct OptsCtx<'a> {
    pub uia: Option<&'a std::sync::Arc<UiaPoolHandle>>,
    pub capture: Option<&'a std::sync::Arc<CaptureThreadHandle>>,
}
```
and pass `ctx.uia.as_ref()` directly.)

- [ ] **Step 4: Build & smoke**

Run: `cargo build --release --workspace && ./target/release/fastuse-cli.exe ping`
Expected: clean build, ping succeeds.

- [ ] **Step 5: Commit**

```bash
git add crates/fastuse-daemon/src/action_opts.rs crates/fastuse-daemon/src/main.rs crates/fastuse-daemon/src/dispatch.rs
git commit -m "feat(daemon): apply_action_opts helper + Click reference wiring"
```

---

## Task 6: Wire `ActionOpts` into remaining action variants

Apply the same `finalize(inner, opts, ctx)` pattern to every remaining action arm in `dispatch.rs`.

**Files:**
- Modify: `crates/fastuse-daemon/src/dispatch.rs`

- [ ] **Step 1: Pull `opts` out of every relevant `Request` arm and pipe through `finalize`**

For each arm in this list, change the destructuring to bind `opts` and replace the trailing expression with `finalize(inner, opts, ctx)`:

`MouseMove`, `MouseDown`, `MouseUp`, `Drag`, `Scroll`, `Type`, `Key`, `HoldKey`, `FocusWindow`, `ResizeMoveWindow`, `ClickElement`, `TypeIntoElement`, `ScrollIntoView`.

For `ClipboardSet` and `LaunchApp` (which use `gate_then`), restructure as:

```rust
Request::ClipboardSet(s) => {
    let opts = std::mem::take(&mut /* opts is on the variant */);
    let inner = /* existing gate_then -> Response */;
    finalize(inner, opts, ctx)
}
```

Note: `ClipboardSet` and `LaunchApp` payloads are tuple-wrapping a struct. The `opts` field lives on the *variant*, not the inner struct. So variant should be:
```rust
ClipboardSet(ClipboardSet, Option<ActionOpts>)
```
NOT inside the struct. Adjust Task 4 if needed: prefer struct-style variants for `ClipboardSet`/`LaunchApp` to keep `opts` on the same level as the rest:
```rust
ClipboardSet { req: ClipboardSet, opts: Option<ActionOpts> }
LaunchApp    { req: LaunchApp,    opts: Option<ActionOpts> }
```

Update CLI/MCP construction sites to match (`Request::ClipboardSet { req: ..., opts: None }`).

- [ ] **Step 2: Build & verify**

Run: `cargo build --release --workspace`
Expected: clean.

- [ ] **Step 3: Run lints**

Run: `cargo run -p xtask -- lints`
Expected: clean (firstcall + com + redact + cacherequest).

- [ ] **Step 4: Run all tests**

Run: `cargo test --release --workspace --lib --bins`
Expected: all 176+ tests pass. New round-trip tests in Task 4 also pass.

- [ ] **Step 5: Commit**

```bash
git add crates/fastuse-daemon/src/dispatch.rs crates/fastuse-proto/src/wire.rs crates/fastuse-cli/ crates/fastuse-mcp/
git commit -m "feat(daemon): wire ActionOpts through every action variant"
```

---

## Task 7: Expose `ActionOpts` in MCP handler

Each `#[tool]` method that performs an action grows an optional `wait_for` / `screenshot_after` / `verify` set of arguments. To avoid 13× of repetition, define one shared input fragment via flatten.

**Files:**
- Modify: `crates/fastuse-mcp/src/handler.rs`

- [ ] **Step 1: Add shared `ActionOptsArgs` schema and converter**

Near other input schemas:

```rust
#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct ActionOptsArgs {
    /// Optional UIA selector (JSON) to wait for after the action.
    #[serde(default)]
    pub wait_for: Option<serde_json::Value>,
    /// Optional UIA selector (JSON) to verify; failure = action failed.
    #[serde(default)]
    pub verify: Option<serde_json::Value>,
    /// Capture a screenshot after the action.
    #[serde(default)]
    pub screenshot_after: bool,
    /// "auto" (foreground window) or "full" (whole monitor). Default "auto".
    #[serde(default)]
    pub screenshot_region: Option<String>,
    /// "jpeg" (default) or "png".
    #[serde(default)]
    pub screenshot_format: Option<String>,
    /// JPEG quality 0..=100; default 85.
    #[serde(default)]
    pub screenshot_quality: Option<u8>,
    /// Wait timeout (ms) shared by wait_for and verify. Default 2000.
    #[serde(default)]
    pub wait_timeout_ms: Option<u32>,
}

fn build_action_opts(a: ActionOptsArgs) -> Result<Option<fastuse_proto::ActionOpts>, McpError> {
    let any = a.wait_for.is_some() || a.verify.is_some() || a.screenshot_after;
    if !any {
        return Ok(None);
    }
    let wait_for = a.wait_for.map(parse_selector).transpose()?;
    let verify = a.verify.map(parse_selector).transpose()?;
    let screenshot_after = a.screenshot_after.then(|| {
        let region = match a.screenshot_region.as_deref() {
            Some("full") => None,
            _ => Some(fastuse_proto::RegionSpec::Auto),
        };
        fastuse_proto::ScreenshotOpts {
            region,
            format: Some(parse_image_format(a.screenshot_format.as_deref())),
            quality: a.screenshot_quality,
        }
    });
    Ok(Some(fastuse_proto::ActionOpts {
        wait_for,
        screenshot_after,
        verify,
        wait_timeout_ms: a.wait_timeout_ms,
    }))
}
```

- [ ] **Step 2: Add output schema for `ActionResult`**

```rust
#[derive(Serialize, schemars::JsonSchema)]
pub struct ActionResultOutput {
    pub ok: bool,
    pub wait_matched: Option<bool>,
    pub waited_ms: u32,
    /// Present iff screenshot_after was requested and capture succeeded.
    pub screenshot: Option<ScreenshotOutput>,
}

fn action_result_response(res: Response) -> Result<Json<ActionResultOutput>, McpError> {
    use base64::Engine;
    match res {
        Response::ActionResult { ok, wait_matched, waited_ms, screenshot } => {
            let screenshot = screenshot.map(|s| {
                let s = *s;
                ScreenshotOutput {
                    mime: s.mime,
                    width: s.width,
                    height: s.height,
                    data_b64: base64::engine::general_purpose::STANDARD.encode(s.bytes.into_inner()),
                }
            });
            Ok(Json(ActionResultOutput { ok, wait_matched, waited_ms, screenshot }))
        }
        Response::Error(e) => Err(Fastuse::err_from_proto(e)),
        other => Err(McpError::internal_error(format!("unexpected: {other:?}"), None)),
    }
}
```

- [ ] **Step 3: Update each action `#[tool]` to flatten `ActionOptsArgs`**

For `ClickArgs`, `XYArgs`, `ButtonArgs`, `DragArgs`, `ScrollArgs`, `TypeArgs`, `KeyArgs`, `HoldKeyArgs`, `HwndArgs`, `ResizeMoveArgs`, `ClickElementArgs`, `TypeIntoElementArgs`, `SelectorArgs` (used by scroll_into_view): add a flattened field

```rust
    #[serde(flatten, default)]
    pub opts: ActionOptsArgs,
```

Then update each tool method to:
1. Compute `let opts = build_action_opts(args.opts)?;`
2. Pass `opts` into the `Request::*` constructor.
3. Branch on opts:
   - If `opts.is_none()`: keep existing `ack(...)` / `element_response(...)` returning `AckOutput` / `ElementMatchOutput`.
   - If `opts.is_some()`: change return type to `Result<Json<ActionResultOutput>, McpError>` and use `action_result_response(...)`.

To keep MCP tool surface stable: split each tool into two: when `opts` is non-empty the response shape changes. **Simpler approach:** always return `ActionResultOutput` when any opts field is set, otherwise keep returning the legacy shape. rmcp `#[tool]` requires a fixed return type per method — so split the return into a tagged enum:

```rust
#[derive(Serialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum ActionOrAck {
    Ack(AckOutput),
    Result(ActionResultOutput),
}
```

Each `#[tool]` action returns `Json<ActionOrAck>`.

- [ ] **Step 4: Build & verify**

Run: `cargo build -p fastuse-mcp --release`
Expected: clean.

- [ ] **Step 5: Smoke test**

Run via stdio MCP client (or with a unit test that constructs a `Request::Click` with `ActionOpts { screenshot_after: Some(...), .. }` and verifies the daemon round-trip).

- [ ] **Step 6: Commit**

```bash
git add crates/fastuse-mcp/src/handler.rs
git commit -m "feat(mcp): expose ActionOpts (wait_for / screenshot_after / verify)"
```

---

## Task 8: Expose `ActionOpts` in CLI

The CLI already supports per-call action commands. Add `--wait-for <SELECTOR_JSON>`, `--verify <SELECTOR_JSON>`, `--screenshot-after`, `--screenshot-region <auto|full>`, `--screenshot-format <jpeg|png>`, `--wait-timeout-ms <N>` to every action subcommand.

**Files:**
- Modify: `crates/fastuse-cli/src/main.rs`
- Modify: `crates/fastuse-cli/src/cmd_phase2.rs`
- Modify: `crates/fastuse-cli/src/cmd_phase3.rs`

- [ ] **Step 1: Add a shared `ActionOptsArgs` clap struct**

In `main.rs`, near other helpers:

```rust
#[derive(clap::Args, Debug, Clone, Default)]
struct ActionOptsArgs {
    /// Selector JSON to poll for after the action.
    #[arg(long)]
    wait_for: Option<String>,
    /// Selector JSON to verify; if it doesn't match, action is reported failed.
    #[arg(long)]
    verify: Option<String>,
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
    if a.wait_for.is_none() && a.verify.is_none() && !a.screenshot_after {
        return Ok(None);
    }
    let wait_for = a.wait_for.as_deref().map(serde_json::from_str).transpose()?;
    let verify = a.verify.as_deref().map(serde_json::from_str).transpose()?;
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
        verify,
        wait_timeout_ms: a.wait_timeout_ms,
    }))
}
```

- [ ] **Step 2: Flatten `ActionOptsArgs` into each action `Cmd` variant**

Example for `Click`:

```rust
Click {
    x: i32,
    y: i32,
    #[arg(long, default_value = "left")]
    button: String,
    #[arg(long, default_value_t = 1)]
    count: u8,
    #[arg(long)]
    mods: Option<String>,
    #[arg(long, default_value_t = false)]
    no_cursor: bool,
    #[command(flatten)]
    opts: ActionOptsArgs,
},
```

Apply to: `Click`, `Type`, `Key`, `HoldKey`, `MouseMove`, `MouseDown`, `MouseUp`, `Drag`, `Scroll`, `FocusWindow`, `ResizeMoveWindow`, `ClickElement`, `TypeIntoElement`, `ScrollIntoView`, `ClipboardSetText`, `LaunchApp`.

- [ ] **Step 3: Update dispatch arms in `main.rs` to thread `opts` through**

```rust
Cmd::Click { x, y, button, count, mods, no_cursor, opts } => {
    let opts = build_action_opts(opts)?;
    cmd_phase2::click(&identity.path, x, y, &button, count, mods.as_deref(), no_cursor, opts).await
}
```

- [ ] **Step 4: Update `cmd_phase2.rs` and `cmd_phase3.rs` to accept `Option<ActionOpts>` and print the new response shape**

Add a shared helper:

```rust
fn print_action_or_ack(res: Response) -> anyhow::Result<()> {
    match res {
        Response::Ack { slept_us } => {
            println!("{}", json!({"ok": true, "slept_us": slept_us}));
            Ok(())
        }
        Response::Element { matched } => {
            println!("{}", json!({"ok": true, "matched": matched}));
            Ok(())
        }
        Response::ActionResult { ok, wait_matched, waited_ms, screenshot } => {
            use base64::Engine;
            let screenshot = screenshot.map(|s| {
                let s = *s;
                json!({
                    "mime": s.mime,
                    "width": s.width,
                    "height": s.height,
                    "data_b64": base64::engine::general_purpose::STANDARD.encode(s.bytes.into_inner()),
                })
            });
            println!("{}", json!({
                "ok": ok,
                "wait_matched": wait_matched,
                "waited_ms": waited_ms,
                "screenshot": screenshot,
            }));
            Ok(())
        }
        Response::Error(e) => print_err(e),
        other => Ok(println!("{}", json!({"unexpected": format!("{other:?}")}))),
    }
}
```

Update each `pub async fn` to thread `opts: Option<ActionOpts>` into the request and use `print_action_or_ack` instead of `print_match` / inline ack.

- [ ] **Step 5: Build & smoke test**

Run:
```bash
cargo build --release --workspace
./target/release/fastuse-cli.exe launch-app calc.exe --screenshot-after --wait-for '{"ByName":"Display is 0"}'
```
Expected: JSON output with `ok: true`, `wait_matched: true`, and a base64 screenshot of Calculator.

- [ ] **Step 6: Commit**

```bash
git add crates/fastuse-cli/
git commit -m "feat(cli): expose ActionOpts on every action subcommand"
```

---

## Task 9: `warmup` tool + auto-warmup on daemon spawn

Single tool that touches every cold path so the first real call is hot.

**Files:**
- Modify: `crates/fastuse-proto/src/wire.rs` (add `Request::Warmup`, `Response::Warmup`)
- Modify: `crates/fastuse-daemon/src/dispatch.rs` (handler)
- Create: `crates/fastuse-daemon/src/warmup.rs` (the actual warm fan-out)
- Modify: `crates/fastuse-daemon/src/main.rs` (run warmup on startup, register module)
- Modify: `crates/fastuse-mcp/src/handler.rs` (`warmup` tool)
- Modify: `crates/fastuse-cli/src/main.rs` (`warmup` subcommand)

- [ ] **Step 1: Add wire variants**

In `wire.rs` `Request` enum:
```rust
    /// Warm every cold path: D3D11 device, DXGI duplication, UIA root,
    /// foreground HWND cache, monitor enum, COM apartments. No side effects.
    Warmup,
```

In `Response` enum:
```rust
    /// `Warmup` reply with per-subsystem timings.
    Warmup {
        /// Time spent warming the capture path.
        capture_us: u64,
        /// Time spent warming UIA root + foreground.
        uia_us: u64,
        /// Time spent warming monitor enumeration.
        monitors_us: u64,
        /// Total wall-clock.
        total_us: u64,
    },
```

Add a round-trip test:
```rust
#[test]
fn warmup_round_trips() {
    let r = Request::Warmup;
    let bytes = encode_frame(&r).unwrap();
    let mut cur = std::io::Cursor::new(bytes);
    assert_eq!(r, decode_frame::<Request, _>(&mut cur).unwrap());
}
```

Run: `cargo test -p fastuse-proto warmup_round_trips`
Expected: PASS.

- [ ] **Step 2: Create `crates/fastuse-daemon/src/warmup.rs`**

```rust
//! Warmup fan-out: touches D3D11, DXGI dup, UIA root, foreground cache,
//! monitor enum. Idempotent — running twice is cheap (subsequent calls
//! hit the warm caches).

use std::time::Instant;

use fastuse_proto::Response;
use fastuse_win::capture_thread::CaptureThreadHandle;
use fastuse_win::uia_pool::UiaPoolHandle;

pub fn run(uia: Option<&UiaPoolHandle>, capture: Option<&CaptureThreadHandle>) -> Response {
    let total = Instant::now();

    // Monitors
    let m_start = Instant::now();
    let _ = fastuse_win::window::monitors::list_monitors();
    let monitors_us = m_start.elapsed().as_micros() as u64;

    // Capture
    let c_start = Instant::now();
    if let Some(cap) = capture {
        let _ = fastuse_win::capture::handle_screenshot(
            cap,
            None,
            Some(fastuse_proto::ImageFormat::Jpeg),
        );
    }
    let capture_us = c_start.elapsed().as_micros() as u64;

    // UIA root + foreground
    let u_start = Instant::now();
    if let Some(pool) = uia {
        let _ = pool.run(|uia| {
            let root = fastuse_win::uia::automation::root_element(uia)?;
            Ok::<_, fastuse_proto::Error>(root)
        });
    }
    let uia_us = u_start.elapsed().as_micros() as u64;

    Response::Warmup {
        capture_us,
        uia_us,
        monitors_us,
        total_us: total.elapsed().as_micros() as u64,
    }
}
```

(If `root_element` doesn't exist as a public helper, expose `automation::foreground_hwnd()` and call `get_or_fetch(uia, hwnd)` instead.)

- [ ] **Step 3: Register module + wire dispatch arm**

In `crates/fastuse-daemon/src/main.rs`:
```rust
mod warmup;
```

In `dispatch.rs`, add to the `match req` table (next to other lifecycle arms):
```rust
        Request::Warmup => {
            let w_start = Instant::now();
            let resp = crate::warmup::run(ctx.uia.as_deref(), ctx.capture.as_deref());
            win32_us = w_start.elapsed().as_micros() as i64;
            resp
        }
```

(Use `.as_ref().map(|a| &**a)` if `as_deref` doesn't compile against `Arc<T>`.)

- [ ] **Step 4: Auto-warmup on daemon spawn**

In `main.rs` after the dispatch context is built and the pipe server is listening, add a one-shot:
```rust
// Best-effort warmup; no failure path.
let _ = warmup::run(uia.as_deref(), capture.as_deref());
```
Place it after the UIA pool and capture thread are up but before `accept` loop.

- [ ] **Step 5: Add MCP tool**

```rust
#[derive(Serialize, schemars::JsonSchema)]
pub struct WarmupOutput {
    pub capture_us: u64,
    pub uia_us: u64,
    pub monitors_us: u64,
    pub total_us: u64,
}

// inside #[tool_router] impl:
#[tool(name = "warmup", description = "Warm every cold path (D3D11, DXGI, UIA root, monitors). Idempotent. Daemon also auto-warms on spawn.")]
async fn warmup(&self) -> Result<Json<WarmupOutput>, McpError> {
    match self.call(Request::Warmup).await? {
        Response::Warmup { capture_us, uia_us, monitors_us, total_us } => {
            Ok(Json(WarmupOutput { capture_us, uia_us, monitors_us, total_us }))
        }
        Response::Error(e) => Err(Self::err_from_proto(e)),
        other => Err(McpError::internal_error(format!("unexpected: {other:?}"), None)),
    }
}
```

- [ ] **Step 6: Add CLI subcommand**

In `main.rs` `Cmd` enum:
```rust
    /// Warm every cold path; idempotent.
    Warmup,
```

Dispatch:
```rust
Cmd::Warmup => {
    let mut pipe = spawn::connect_or_spawn(&identity.path).await?;
    /* handshake */
    proto_io::write_request(&mut pipe, &Request::Hello { ... }).await?;
    let _ = proto_io::read_response(&mut pipe).await?;
    proto_io::write_request(&mut pipe, &Request::Warmup).await?;
    match proto_io::read_response(&mut pipe).await? {
        Response::Warmup { capture_us, uia_us, monitors_us, total_us } => {
            println!("{}", serde_json::json!({
                "ok": true,
                "capture_us": capture_us,
                "uia_us": uia_us,
                "monitors_us": monitors_us,
                "total_us": total_us,
            }));
        }
        other => println!("{}", serde_json::json!({"unexpected": format!("{other:?}")})),
    }
    Ok(())
}
```

(Or factor into a `cmd_warmup.rs` mirroring `cmd_status.rs`.)

- [ ] **Step 7: Verify cold first call after daemon spawn is <500ms**

Run:
```bash
./target/release/fastuse-cli.exe stop
./target/release/fastuse-cli.exe screenshot --out /tmp/sc.png  # cold first call
```
Time the second command end-to-end. Expected: <500ms wall-clock total.

- [ ] **Step 8: Run full verification**

Run:
```bash
cargo build --release --workspace
cargo test --release --workspace --lib --bins
cargo run -p xtask -- lints
```
Expected: clean across the board.

- [ ] **Step 9: Commit**

```bash
git add crates/fastuse-proto/src/wire.rs crates/fastuse-daemon/ crates/fastuse-mcp/src/handler.rs crates/fastuse-cli/
git commit -m "feat(perf): warmup tool + auto-warmup on daemon spawn"
```

---

## Self-review checklist

After all tasks complete:

- [ ] **Calculator end-to-end test:**
```bash
./target/release/fastuse-cli.exe launch-app calc.exe --wait-for '{"ByName":"Display is 0"}'
./target/release/fastuse-cli.exe type "23+45=" --wait-for '{"ByControlType":"Text"}'
```
Verify: `wait_matched: true` on both, total wall-clock under 3s.

- [ ] **Spec coverage:** every "Components" section in the design doc maps to one of Tasks 1–9.

- [ ] **Backwards compat:** old MCP clients that send `Request::Click` without `opts` still get `Response::Ack` (because `opts: None` flows through `finalize` which returns `inner` unchanged).

- [ ] **Lints:** `cargo run -p xtask -- lints` clean.

- [ ] **Tests:** `cargo test --release --workspace` clean.

## Out of scope for this plan (separate plan)

- `doctor` and `logs` CLI subcommands (spec item 5)
- Criterion microbenches per primitive (item 6)
- `fastuse bench windows-mcp` comparator (item 7)
- `cargo xtask dist` portable zip (item 8)
- `fastuse install --client claude-code` config patcher (item 9)

These are observability and distribution work; the agent-loop wins are independent and ship first.
