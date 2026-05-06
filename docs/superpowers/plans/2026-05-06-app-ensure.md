# app-ensure Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `app-ensure <name>` — a single CLI/MCP call that checks if an app is running, launches it if not, restores + focuses it if minimized/backgrounded, and returns a ready state with optional screenshot.

**Architecture:** Four-layer change: proto (wire types), daemon (async handler with parallel reads + correction state machine), CLI (subcommand + handler), MCP (tool). The daemon uses `tokio::task::spawn_blocking` to run `list_processes` and `list_windows` in parallel, then calls `focus_window` (which already handles SW_RESTORE) on the result.

**Tech Stack:** Rust 1.83+, tokio 1.49, postcard (wire encoding), clap 4.6, rmcp 1.6, windows-rs 0.62 (Win32_UI_WindowsAndMessaging already in daemon deps).

---

## File Map

| File | Change |
|------|--------|
| `crates/fastuse-proto/src/wire.rs` | Add `AppEnsureResp` struct, `Request::AppEnsure` variant, `Response::AppEnsure` variant |
| `crates/fastuse-daemon/src/dispatch.rs` | Add match arm for `Request::AppEnsure` with parallel reads + correction state machine |
| `crates/fastuse-cli/src/main.rs` | Add `Cmd::AppEnsure` variant + dispatch arm |
| `crates/fastuse-cli/src/cmd_phase4.rs` | Add `app_ensure()` async handler function |
| `crates/fastuse-mcp/src/handler.rs` | Add `AppEnsureArgs`, `AppEnsureOutput`, and `#[tool] app_ensure` method |

---

## Task 1: Proto — wire types

**Files:**
- Modify: `crates/fastuse-proto/src/wire.rs`

- [ ] **Step 1: Add `AppEnsureResp` struct**

Insert after the `KillProcess` struct (around line 603, before `/// All responses`):

```rust
/// `app_ensure` response payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppEnsureResp {
    /// HWND of the ready window.
    pub hwnd: u64,
    /// PID of the owning process.
    pub pid: u32,
    /// Window title at the moment of readiness.
    pub title: String,
    /// True if the app was launched from scratch.
    pub launched: bool,
    /// True if the window was minimized when first found.
    pub was_minimized: bool,
    /// True if the window was not in the foreground when first found.
    pub was_background: bool,
    /// Human-readable list of corrective actions taken (e.g. "launched", "focused").
    pub actions_taken: Vec<String>,
    /// Wall-clock milliseconds from request receipt to ready.
    pub elapsed_ms: u64,
    /// Optional post-ready screenshot.
    pub screenshot: Option<ScreenshotPayload>,
}
```

- [ ] **Step 2: Add `Request::AppEnsure` variant**

In the `Request` enum, append after the `KillProcess` variant (around line 530, after the `}` that closes `KillProcess`):

```rust
    /// Ensure an app is running, visible, and in the foreground.
    /// Launches, restores, and focuses as needed in a single round-trip.
    AppEnsure {
        /// App name, process stem, or title substring (e.g. "Calculator", "calc").
        name: String,
        /// Total budget for launch + window-appear polling, in milliseconds.
        timeout_ms: u32,
        /// If set, take a screenshot once the app is ready and embed it in the response.
        screenshot_after: Option<ScreenshotOpts>,
    },
```

- [ ] **Step 3: Add `Response::AppEnsure` variant**

In the `Response` enum, append after `Response::Computer(ComputerResult)` (line 727, last variant):

```rust
    // --- app-ensure (Task app-ensure) ---
    /// Result of `AppEnsure`.
    AppEnsure(AppEnsureResp),
```

- [ ] **Step 4: Write round-trip serialization test**

Add at the bottom of `crates/fastuse-proto/src/wire.rs` (inside or after the existing `#[cfg(test)]` block if one exists, otherwise create new):

```rust
#[cfg(test)]
mod app_ensure_tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn app_ensure_request_round_trips() {
        let req = Request::AppEnsure {
            name: "Calculator".into(),
            timeout_ms: 5000,
            screenshot_after: None,
        };
        let bytes = encode_frame(&req).unwrap();
        let decoded: Request = decode_frame(&mut Cursor::new(bytes)).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn app_ensure_response_round_trips() {
        let resp = Response::AppEnsure(AppEnsureResp {
            hwnd: 12345,
            pid: 678,
            title: "Calculator".into(),
            launched: true,
            was_minimized: false,
            was_background: true,
            actions_taken: vec!["launched".into(), "focused".into()],
            elapsed_ms: 42,
            screenshot: None,
        });
        let bytes = encode_frame(&resp).unwrap();
        let decoded: Response = decode_frame(&mut Cursor::new(bytes)).unwrap();
        assert_eq!(resp, decoded);
    }
}
```

- [ ] **Step 5: Run tests**

```
cd crates/fastuse-proto
cargo test app_ensure
```

Expected: `test app_ensure_request_round_trips ... ok` and `test app_ensure_response_round_trips ... ok`

- [ ] **Step 6: Commit**

```
git add crates/fastuse-proto/src/wire.rs
git commit -m "feat(proto): add AppEnsure request/response wire types"
```

---

## Task 2: Daemon — AppEnsure handler

**Files:**
- Modify: `crates/fastuse-daemon/src/dispatch.rs`

- [ ] **Step 1: Add imports**

At the top of `dispatch.rs`, add to the existing `use fastuse_proto::` import block:

```rust
use fastuse_proto::{
    // existing imports stay...
    AppEnsureResp, ProcFilter, ListProcesses, ScreenshotOpts,
    wire::WaitForWindowRequest,
};
```

Add to the existing `use fastuse_win::window::` import block:

```rust
use fastuse_win::window::wait_for_window::wait_for_window as wait_for_window_fn;
```

Add a new `use` line for the Win32 window-state functions:

```rust
use windows::Win32::Foundation::HWND as Win32Hwnd;
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, IsIconic};
```

- [ ] **Step 2: Add the match arm**

Inside `pub async fn handle`, append a new arm to the `match req { ... }` block, just before the closing brace (after the `Request::WaitForWindowV2` arm):

```rust
        Request::AppEnsure { name, timeout_ms, screenshot_after } => {
            let t_start = Instant::now();
            let mut actions_taken: Vec<String> = Vec::new();

            // ── Phase 1: parallel reads ──────────────────────────────────
            let name_p = name.clone();
            let name_w = name.clone();
            let procs_handle = tokio::task::spawn_blocking(move || {
                fastuse_win::process::list_processes(Some(ProcFilter {
                    name_contains: Some(name_p),
                    visible_only: None,
                }))
            });
            let wins_handle = tokio::task::spawn_blocking(move || {
                list_windows(Some(&name_w), None, Some(true))
            });
            let (procs_result, wins_result) = tokio::join!(procs_handle, wins_handle);
            let procs = procs_result.unwrap_or_default();
            let windows_list = wins_result
                .unwrap_or_else(|_| Ok(vec![]))
                .unwrap_or_default();

            // ── Phase 2: launch if needed ────────────────────────────────
            let win = if windows_list.is_empty() {
                if procs.is_empty() {
                    // Gated: requires launch_app permission.
                    if let Err(resp) = safe_mode_gate(ctx, "launch_app") {
                        return DispatchResult {
                            response: resp,
                            daemon_dispatch_us: t_start.elapsed().as_micros() as i64,
                            win32_work_us: 0,
                        };
                    }
                    let la = fastuse_proto::LaunchApp { query: name.clone() };
                    match fastuse_win::launch::launch_app(la) {
                        Err(e) => {
                            return DispatchResult {
                                response: Response::Error(e.to_wire()),
                                daemon_dispatch_us: t_start.elapsed().as_micros() as i64,
                                win32_work_us: 0,
                            };
                        }
                        Ok(_) => {
                            actions_taken.push("launched".into());
                        }
                    }
                }
                // Wait for window to appear (blocking — must be spawn_blocking).
                let wait_name = name.clone();
                let wait_result = tokio::task::spawn_blocking(move || {
                    wait_for_window_fn(&WaitForWindowRequest {
                        title_substr: None,
                        process_name: Some(wait_name),
                        timeout_ms,
                    })
                })
                .await
                .unwrap_or(None);

                match wait_result {
                    None => {
                        return DispatchResult {
                            response: Response::Error(
                                Error::new(
                                    ErrorCode::WindowNotFound,
                                    format!(
                                        "app '{}' window did not appear within {}ms",
                                        name, timeout_ms
                                    ),
                                )
                                .with_hint("check the app name/process stem, or increase timeout_ms"),
                            ),
                            daemon_dispatch_us: t_start.elapsed().as_micros() as i64,
                            win32_work_us: 0,
                        };
                    }
                    Some(w) => w,
                }
            } else {
                windows_list.into_iter().next().unwrap()
            };

            // ── Phase 3: restore + focus if needed ──────────────────────
            let hwnd_val = Win32Hwnd(win.hwnd as *mut _);
            // SAFETY: IsIconic / GetForegroundWindow are always safe.
            let was_minimized = unsafe { IsIconic(hwnd_val) }.as_bool();
            let fg = unsafe { GetForegroundWindow() };
            let was_background = fg.0 != hwnd_val.0 || was_minimized;

            if was_background {
                // focus_window internally calls SW_RESTORE if iconic.
                let hwnd_copy = win.hwnd;
                let focus_resp = run_unit(ctx, &mut win32_us, move || focus_window(hwnd_copy));
                if let Response::Error(e) = focus_resp {
                    return DispatchResult {
                        response: Response::Error(e),
                        daemon_dispatch_us: t_start.elapsed().as_micros() as i64,
                        win32_work_us: win32_us,
                    };
                }
                actions_taken.push("focused".into());
            }

            // ── Phase 4: optional screenshot ─────────────────────────────
            let screenshot = if let Some(ss_opts) = screenshot_after {
                ctx.capture.as_ref().and_then(|capture| {
                    handle_screenshot(capture, None, ss_opts.format)
                        .ok()
                        .and_then(|r| {
                            if let Response::Screenshot { bytes, mime, width, height } = r {
                                Some(fastuse_proto::ScreenshotPayload { bytes, mime, width, height })
                            } else {
                                None
                            }
                        })
                })
            } else {
                None
            };

            Response::AppEnsure(AppEnsureResp {
                hwnd: win.hwnd,
                pid: win.pid,
                title: win.title,
                launched: actions_taken.contains(&"launched".to_string()),
                was_minimized,
                was_background,
                actions_taken,
                elapsed_ms: t_start.elapsed().as_millis() as u64,
                screenshot,
            })
        }
```

- [ ] **Step 3: Build daemon to verify**

```
cargo build -p fastuse-daemon 2>&1
```

Expected: compiles without errors. Fix any import resolution issues (unused import warnings are OK).

- [ ] **Step 4: Commit**

```
git add crates/fastuse-daemon/src/dispatch.rs
git commit -m "feat(daemon): AppEnsure handler — parallel reads + correction state machine"
```

---

## Task 3: CLI — subcommand + handler

**Files:**
- Modify: `crates/fastuse-cli/src/main.rs`
- Modify: `crates/fastuse-cli/src/cmd_phase4.rs`

- [ ] **Step 1: Add `Cmd::AppEnsure` to the enum in `main.rs`**

In the `Cmd` enum, after the `KillProcess` variant (around line 359) and before the `// ----- Warmup -----` comment, add:

```rust
    /// Ensure an app is running, focused, and ready.
    /// Launches it if not running, restores if minimized, focuses if backgrounded.
    /// Returns JSON with `status`, `hwnd`, and optional screenshot. Permission-gated
    /// for launch (same as `launch-app`).
    AppEnsure {
        /// App name, process stem, or title substring (e.g. "Calculator", "calc").
        name: String,
        /// Budget for launch + window-appear polling, in milliseconds.
        #[arg(long, default_value_t = 5000)]
        timeout_ms: u32,
        /// Capture a screenshot once the app is ready and print it as a base64-encoded
        /// JPEG in the JSON output.
        #[arg(long)]
        screenshot_after: bool,
    },
```

- [ ] **Step 2: Add dispatch arm in `main.rs`**

In the `match cmd { ... }` block, after the `Cmd::KillProcess { ... }` arm (around line 828), add:

```rust
            Cmd::AppEnsure { name, timeout_ms, screenshot_after } => {
                cmd_phase4::app_ensure(&identity.path, name, timeout_ms, screenshot_after).await
            }
```

- [ ] **Step 3: Add `app_ensure` handler in `cmd_phase4.rs`**

Add the following function at the bottom of `crates/fastuse-cli/src/cmd_phase4.rs`, before the `fn print_err` function:

```rust
pub async fn app_ensure(
    pipe_path: &str,
    name: String,
    timeout_ms: u32,
    screenshot_after: bool,
) -> anyhow::Result<()> {
    use fastuse_proto::{ImageFormat, ScreenshotOpts};
    let ss = if screenshot_after {
        Some(ScreenshotOpts { region: None, format: Some(ImageFormat::Jpeg), quality: None })
    } else {
        None
    };
    let req = Request::AppEnsure { name, timeout_ms, screenshot_after: ss };
    match one_call(pipe_path, req).await? {
        Response::AppEnsure(r) => {
            use base64::Engine;
            let screenshot_b64 = r.screenshot.map(|s| {
                base64::engine::general_purpose::STANDARD.encode(s.bytes.into_inner())
            });
            println!(
                "{}",
                json!({
                    "status": "ready",
                    "hwnd": r.hwnd,
                    "pid": r.pid,
                    "title": r.title,
                    "launched": r.launched,
                    "was_minimized": r.was_minimized,
                    "was_background": r.was_background,
                    "actions_taken": r.actions_taken,
                    "elapsed_ms": r.elapsed_ms,
                    "screenshot_b64": screenshot_b64,
                })
            );
            Ok(())
        }
        Response::Error(e) => print_err(e),
        other => Ok(println!("{}", json!({"unexpected": format!("{other:?}")}))),
    }
}
```

- [ ] **Step 4: Build CLI**

```
cargo build -p fastuse-cli 2>&1
```

Expected: compiles cleanly. Fix any issues.

- [ ] **Step 5: Manual smoke test**

```
FASTUSE_ALLOW=launch_app ./target/release/fastuse-cli.exe app-ensure "Calculator" --screenshot-after
```

Expected output (with Calculator opening or already open):
```json
{"status":"ready","hwnd":394716,"pid":14832,"title":"Calculator","launched":true,"was_minimized":false,"was_background":true,"actions_taken":["launched","focused"],"elapsed_ms":1234,"screenshot_b64":"..."}
```

Run a second time (Calculator already open and focused):
```json
{"status":"ready",...,"launched":false,"was_background":false,"actions_taken":[],"elapsed_ms":8}
```

- [ ] **Step 6: Commit**

```
git add crates/fastuse-cli/src/main.rs crates/fastuse-cli/src/cmd_phase4.rs
git commit -m "feat(cli): app-ensure subcommand"
```

---

## Task 4: MCP — app_ensure tool

**Files:**
- Modify: `crates/fastuse-mcp/src/handler.rs`

- [ ] **Step 1: Add `AppEnsureArgs` input schema**

In `handler.rs`, after the `WaitForWindowArgs` struct (around line 538), add:

```rust
#[derive(Deserialize, schemars::JsonSchema)]
pub struct AppEnsureArgs {
    /// App name, process stem, or title substring (e.g. "Calculator", "calc").
    pub name: String,
    /// Budget for launch + window-appear polling, in milliseconds. Defaults to 5000.
    #[serde(default = "default_app_ensure_timeout")]
    pub timeout_ms: u32,
    /// If true, capture a screenshot once the app is ready and include it as
    /// ImageContent in the response. Strongly recommended — saves a separate
    /// screenshot call.
    #[serde(default)]
    pub screenshot_after: bool,
}

fn default_app_ensure_timeout() -> u32 { 5000 }
```

- [ ] **Step 2: Add `AppEnsureOutput` output schema**

In `handler.rs`, after the `WarmupOutput` struct (around line 578), add:

```rust
#[derive(Serialize, schemars::JsonSchema)]
pub struct AppEnsureOutput {
    pub status: String,
    pub hwnd: u64,
    pub pid: u32,
    pub title: String,
    pub launched: bool,
    pub was_minimized: bool,
    pub was_background: bool,
    pub actions_taken: Vec<String>,
    pub elapsed_ms: u64,
}
```

- [ ] **Step 3: Add `#[tool] app_ensure` method**

Inside the `#[tool_router(server_handler)] impl Fastuse` block, after the `wait_for_window` tool method, add:

```rust
    #[tool(
        name = "app_ensure",
        description = "Ensure an app is running, visible, and in the foreground — in a single call. \
Launches the app if not running (permission-gated like launch_app), restores it if minimized, \
focuses it if in the background. Returns status=ready with hwnd and pid when done. \
Set screenshot_after=true to get a screenshot of the ready app in the same call."
    )]
    async fn app_ensure(
        &self,
        Parameters(args): Parameters<AppEnsureArgs>,
    ) -> Result<CallToolResult, McpError> {
        use fastuse_proto::{ImageFormat, ScreenshotOpts};
        let ss = if args.screenshot_after {
            Some(ScreenshotOpts { region: None, format: Some(ImageFormat::Jpeg), quality: None })
        } else {
            None
        };
        let req = Request::AppEnsure {
            name: args.name,
            timeout_ms: args.timeout_ms,
            screenshot_after: ss,
        };
        let resp = self.call(req).await?;
        match resp {
            Response::AppEnsure(r) => {
                use rmcp::model::{Content, ImageContent, TextContent};
                let output = AppEnsureOutput {
                    status: "ready".into(),
                    hwnd: r.hwnd,
                    pid: r.pid,
                    title: r.title.clone(),
                    launched: r.launched,
                    was_minimized: r.was_minimized,
                    was_background: r.was_background,
                    actions_taken: r.actions_taken,
                    elapsed_ms: r.elapsed_ms,
                };
                let mut content = vec![Content::Text(TextContent {
                    text: serde_json::to_string(&output)
                        .unwrap_or_else(|_| r"{}".into()),
                    annotations: None,
                })];
                if let Some(ss) = r.screenshot {
                    use base64::Engine;
                    content.push(Content::Image(ImageContent {
                        data: base64::engine::general_purpose::STANDARD
                            .encode(ss.bytes.into_inner()),
                        mime_type: ss.mime,
                        annotations: None,
                    }));
                }
                Ok(CallToolResult { content, is_error: Some(false) })
            }
            Response::Error(e) => Err(Self::err_from_proto(e)),
            other => Err(McpError::internal_error(format!("unexpected: {other:?}"), None)),
        }
    }
```

- [ ] **Step 4: Build MCP server**

```
cargo build -p fastuse-mcp 2>&1
```

Expected: compiles cleanly.

- [ ] **Step 5: Verify tool appears in MCP tool list**

Restart Claude Code (or run `fastuse-cli setup-mcp --user` again if needed), then verify `app_ensure` appears when Claude lists available fastuse tools.

- [ ] **Step 6: Commit**

```
git add crates/fastuse-mcp/src/handler.rs
git commit -m "feat(mcp): app_ensure tool — open + focus any app in one call"
```

---

## Task 5: Final build + integration verification

- [ ] **Step 1: Full workspace build**

```
cargo build --workspace 2>&1
```

Expected: all crates compile without errors.

- [ ] **Step 2: Run proto tests**

```
cargo test -p fastuse-proto app_ensure 2>&1
```

Expected: 2 tests pass.

- [ ] **Step 3: End-to-end CLI test — cold start**

Close Calculator if open, then:

```
FASTUSE_ALLOW=launch_app ./target/release/fastuse-cli.exe app-ensure "Calculator" --screenshot-after
```

Expected: `"launched": true`, `"actions_taken": ["launched","focused"]`, non-null `screenshot_b64`.

- [ ] **Step 4: End-to-end CLI test — already running, minimized**

Minimize Calculator, then:

```
./target/release/fastuse-cli.exe app-ensure "Calculator"
```

Expected: `"launched": false`, `"was_minimized": true`, `"actions_taken": ["focused"]`.

- [ ] **Step 5: End-to-end CLI test — already in foreground**

With Calculator already focused:

```
./target/release/fastuse-cli.exe app-ensure "Calculator"
```

Expected: `"launched": false`, `"was_background": false`, `"actions_taken": []`, `"elapsed_ms"` < 50.

- [ ] **Step 6: Final commit**

```
git add -A
git commit -m "feat: app-ensure complete — proto + daemon + cli + mcp"
```
