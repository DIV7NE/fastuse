//! Daemon-side half of `drag_files`.
//!
//! The daemon owns the mouse and the recovery; the de-elevated helper owns
//! the OLE object graph. Splitting it this way is forced by integrity levels
//! (see `deelevate.rs`), but it also puts the dangerous half — a held mouse
//! button — in the process that cannot crash without taking the daemon with
//! it.

use std::io::{BufRead, BufReader, Write};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use fastuse_proto::{
    DragResult, Error as ProtoError, ErrorCode, MonitorInfo, MouseButton, Rect, WindowInfo,
};
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_MOVE, MOUSEINPUT,
};
use windows::Win32::UI::WindowsAndMessaging::{GetAncestor, WindowFromPoint, GA_ROOT};

use crate::files::deelevate::{spawn_medium_il, MediumIlChild};
use crate::input::sendinput::{mouse_absolute, mouse_button_flags, send};
use crate::input_thread::InputThreadHandle;

/// Hard ceiling on a single drag, matching the helper's own deadline.
///
/// One budget covers everything after the ready line: our cursor walk *and*
/// the drag. The walk costs `MOVE_STEPS * STEP_SLEEP_MS` plus two input-thread
/// round trips — under 300ms — so almost all of this is left for the target.
const DRAG_DEADLINE_MS: u32 = 10_000;

/// Steps the cursor takes between start and target. Enough that the drop
/// target sees genuine `DragOver` traffic rather than a teleport.
const MOVE_STEPS: u32 = 20;

/// Pause between cursor steps. The whole walk is one input-thread job, so
/// this sleeps the input thread, the way `hold_key` already does.
const STEP_SLEEP_MS: u64 = 8;

/// How long to wait for `{"ready":true}`. Generous because
/// `CreateProcessWithTokenW` goes through the Secondary Logon service, which
/// is slow and occasionally cold-starts.
const READY_TIMEOUT_MS: u64 = 5_000;

/// Slack on top of the helper's own deadline when waiting for the outcome
/// line. We must outlast the helper's `QueryContinueDrag` cancel, or we
/// report our timeout instead of its far more specific reason.
const OUTCOME_GRACE_MS: u64 = 2_000;

/// The helper's readiness line, matched exactly. Written by
/// `drag_helper::drag` as a single `writeln!` of this literal.
const READY_LINE: &str = r#"{"ready":true}"#;

/// Inset of the default start point from the monitor edge. A raw corner is
/// the Start button or the notification area; 64px in is empty desktop on
/// every normal layout.
const START_MARGIN_PX: i32 = 64;

/// One drag job, daemon → helper over stdin.
///
/// Mirrors `fastuse_daemon::drag_helper::DragJob`. It cannot be shared: the
/// daemon depends on this crate, not the other way round. The field names are
/// the wire contract and are pinned by `job_line_matches_what_the_helper_parses`.
#[derive(serde::Serialize)]
struct DragJob {
    paths: Vec<String>,
    start_x: i32,
    start_y: i32,
    deadline_ms: u32,
}

/// Outcome, helper → daemon over stdout. Mirrors
/// `fastuse_daemon::drag_helper::DragOutcome`.
#[derive(serde::Deserialize)]
struct DragOutcome {
    dropped: bool,
    effect: u32,
    error: Option<String>,
}

/// Drop `paths` onto `(x, y)` with a real OLE drag-and-drop.
///
/// For drop zones that expose no file input and refuse a paste. `start_x` /
/// `start_y` default to a point on the target's own monitor, far enough away
/// that the button-down cannot land on the drop zone itself.
///
/// A target that refuses the drop is `Ok(DragResult { dropped: false, .. })`,
/// not an error: refusal is information the caller needs, not a malfunction.
pub fn drag_files(
    input: &InputThreadHandle,
    paths: Vec<String>,
    x: i32,
    y: i32,
    start_x: Option<i32>,
    start_y: Option<i32>,
) -> Result<DragResult, ProtoError> {
    // Before anything touches the mouse: a bad path must fail with the cursor
    // still where the user left it.
    let resolved: Vec<String> = crate::files::resolve_paths(&paths)?
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();

    let (sx, sy) = match (start_x, start_y) {
        (Some(a), Some(b)) => (a, b),
        _ => {
            let (dx, dy) =
                default_start_point(x, y, &crate::window::monitors::list_monitors()?)?;
            (start_x.unwrap_or(dx), start_y.unwrap_or(dy))
        }
    };

    let spawned = Instant::now();
    let exe = std::env::current_exe().map_err(|e| {
        ProtoError::new(
            ErrorCode::HelperSpawnFailed,
            format!("cannot locate our own executable to re-exec as the drag helper: {e}"),
        )
    })?;
    // Guarded from here on: an early return past this point must not leave a
    // helper holding a 1x1 window and an OLE modal loop on the user's desktop.
    let mut helper = HelperGuard(spawn_medium_il(&exe, &["--drag-helper".to_string()])?);
    tracing::info!(
        path_count = resolved.len(),
        helper_pid = helper.0.pid(),
        start_x = sx,
        start_y = sy,
        x,
        y,
        "drag_files: helper spawned"
    );

    // The helper's stdout is a blocking anonymous pipe with no read timeout,
    // so it is drained on its own thread and delivered over a channel we can
    // wait on with a deadline. EOF ends the thread, which surfaces here as
    // `Disconnected` — that is how a dead helper is detected.
    let (tx, rx) = mpsc::channel::<String>();
    let pipe = helper.0.stdout.try_clone().map_err(|e| {
        ProtoError::new(
            ErrorCode::HelperSpawnFailed,
            format!("cannot duplicate the drag helper's stdout: {e}"),
        )
    })?;
    std::thread::spawn(move || {
        for line in BufReader::new(pipe).lines() {
            match line {
                Ok(l) => {
                    if tx.send(l).is_err() {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
    });

    let job = DragJob {
        paths: resolved,
        start_x: sx,
        start_y: sy,
        deadline_ms: DRAG_DEADLINE_MS,
    };
    let line = serde_json::to_string(&job)
        .map_err(|e| ProtoError::new(ErrorCode::Internal, format!("encoding the drag job: {e}")))?;
    // The newline is mandatory: the helper's `read_line` blocks without one.
    writeln!(helper.0.stdin, "{line}")
        .and_then(|()| helper.0.stdin.flush())
        .map_err(|e| {
            ProtoError::new(
                ErrorCode::HelperSpawnFailed,
                format!("writing the drag job to the helper failed: {e}"),
            )
        })?;

    // Until this line arrives the 1x1 source window does not exist, and a
    // button-down would land on whatever the user had under the cursor.
    match rx.recv_timeout(Duration::from_millis(READY_TIMEOUT_MS)) {
        // Exact, not a substring: a helper *error* string that happens to
        // contain the literal would otherwise be read as readiness, and the
        // daemon answers readiness with a real click on the user's desktop.
        Ok(l) if is_ready(&l) => {}
        // The helper prints exactly one outcome line on every failure path,
        // so anything else here is that line: it failed before it was ready.
        Ok(l) => return Err(early_failure(&l)),
        Err(RecvTimeoutError::Disconnected) => {
            return Err(ProtoError::new(
                ErrorCode::HelperSpawnFailed,
                "the drag helper exited before reporting ready".to_string(),
            ))
        }
        Err(RecvTimeoutError::Timeout) => {
            return Err(ProtoError::new(
                ErrorCode::HelperSpawnFailed,
                format!("the drag helper did not report ready within {READY_TIMEOUT_MS}ms"),
            ))
        }
    }

    let started = Instant::now();
    let drop_hwnd;
    tracing::debug!(ready_wait_ms = started.duration_since(spawned).as_millis() as u64, "drag_files: helper ready");
    {
        // A guard, not a line at the bottom of the happy path: written as a
        // trailing statement it stops running the first time someone adds an
        // early `?` above it, and the failure mode is the user's physical
        // mouse button stuck down. The helper's `QueryContinueDrag` deadline
        // only protects a wedge *inside* the helper; it does nothing if the
        // helper is killed. So the daemon injects the down, and the button-up
        // is owned by scope exit — success, error, or panic.
        let _up = ReleaseGuard(|| {
            if let Err(e) = input.run(|| {
                release_left_button();
                Ok(())
            }) {
                // D-26 forbids injecting from this thread, so there is no
                // fallback — but the user's mouse button is now physically
                // down with no other trace of why.
                tracing::error!(error = ?e, "drag_files: the left-button release was NOT injected");
            }
        });
        press_at(input, sx, sy)?;
        walk_to(input, sx, sy, x, y)?;
        // Sampled here, with the button still down: the drop activates the
        // window that receives it, so probing after the release reports
        // whichever window the drop brought forward. Only the HWND is taken
        // now — resolving its title costs a cross-process WM_GETTEXT, and
        // nothing may run for 100ms with the user's mouse button held.
        drop_hwnd = window_at(x, y);
    }

    let drop_target = drop_hwnd.and_then(|h| crate::window::build_window_info(h).ok());

    let injected = started.elapsed();
    tracing::debug!(walk_ms = injected.as_millis() as u64, "drag_files: cursor walk done, button released");

    let budget = u64::from(remaining_ms(DRAG_DEADLINE_MS, injected)) + OUTCOME_GRACE_MS;
    let line = match rx.recv_timeout(Duration::from_millis(budget)) {
        Ok(l) => l,
        Err(RecvTimeoutError::Disconnected) => {
            return Err(ProtoError::new(
                ErrorCode::DragFailed,
                "the drag helper exited without reporting an outcome".to_string(),
            ))
        }
        Err(RecvTimeoutError::Timeout) => {
            return Err(ProtoError::new(
                ErrorCode::DragFailed,
                format!("the drag helper reported no outcome within {budget}ms"),
            ))
        }
    };
    let outcome: DragOutcome = serde_json::from_str(&line).map_err(|e| {
        ProtoError::new(
            ErrorCode::DragFailed,
            format!("the drag helper's outcome is not valid JSON: {e}"),
        )
    })?;
    let result = match map_outcome(outcome, drop_target) {
        Ok(r) => r,
        Err(e) => {
            // The error also travels back to the caller, but nothing on the
            // daemon side logs a Response::Error, and this is the one line a
            // post-mortem of a failed drag has to go on. Every message the
            // helper can put here is a fixed literal or an HRESULT — no paths.
            tracing::error!(error = %e.message, "drag_files: the helper reported a failure");
            return Err(e);
        }
    };
    tracing::info!(
        dropped = result.dropped,
        effect = result.effect,
        total_ms = started.elapsed().as_millis() as u64,
        drop_target_hwnd = result.drop_target.as_ref().map(|w| w.hwnd),
        drop_target_process = result.drop_target.as_ref().map(|w| w.process_name.as_str()),
        "drag_files: outcome"
    );
    Ok(result)
}

/// Kills the helper on every exit path. A helper that outlives the call keeps
/// a visible (if 1x1) window and, mid-drag, mouse capture.
///
/// This kill is also the only thing that unblocks the stdout reader thread on
/// the timeout paths: it is parked in `read`, and nothing but the child's exit
/// closes the write end. Making this a no-op — on the assumption that a helper
/// which has printed its outcome exits by itself — leaks a thread per timed-out
/// drag.
struct HelperGuard(MediumIlChild);

impl Drop for HelperGuard {
    fn drop(&mut self) {
        self.0.kill();
    }
}

/// Runs `F` on scope exit. Generic over the closure so the button-up property
/// can be tested without an input thread.
struct ReleaseGuard<F: FnMut()>(F);

impl<F: FnMut()> Drop for ReleaseGuard<F> {
    fn drop(&mut self) {
        (self.0)()
    }
}

/// Emit a bare left-button-up at the current cursor position.
///
/// Deliberately not `handlers::mouse_up`: that calls
/// `check_foreground_integrity` first and can return `Err` *before* sending
/// anything. This is the one path that must never decline to run.
fn release_left_button() {
    let ev = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: 0,
                dwFlags: mouse_button_flags(MouseButton::Left, true),
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let _ = send(&[ev]);
}

/// Move to `(x, y)` and press the left button there.
fn press_at(input: &InputThreadHandle, x: i32, y: i32) -> Result<(), ProtoError> {
    input.run(move || {
        send(&[mouse_absolute(x, y, MOUSEEVENTF_MOVE)]).map_err(ProtoError::from)?;
        // Let the helper's pump see the hover before the click; it is looking
        // for WM_LBUTTONDOWN on a window that has only just appeared.
        std::thread::sleep(Duration::from_millis(STEP_SLEEP_MS));
        send(&[mouse_absolute(
            x,
            y,
            MOUSEEVENTF_MOVE | mouse_button_flags(MouseButton::Left, false),
        )])
        .map_err(ProtoError::from)?;
        Ok(())
    })
}

/// Walk the cursor from `(sx, sy)` to `(ex, ey)` in [`MOVE_STEPS`] injected
/// moves. Injected `MOUSEEVENTF_MOVE` events, not `SetCursorPos`: the drop
/// target's `DragOver` traffic comes from the real input stream.
fn walk_to(
    input: &InputThreadHandle,
    sx: i32,
    sy: i32,
    ex: i32,
    ey: i32,
) -> Result<(), ProtoError> {
    input.run(move || {
        for step in 1..=MOVE_STEPS {
            let (px, py) = lerp(sx, sy, ex, ey, step, MOVE_STEPS);
            send(&[mouse_absolute(px, py, MOUSEEVENTF_MOVE)]).map_err(ProtoError::from)?;
            std::thread::sleep(Duration::from_millis(STEP_SLEEP_MS));
        }
        Ok(())
    })
}

/// Point `step` of `steps` along the segment, ending exactly on the target.
fn lerp(sx: i32, sy: i32, ex: i32, ey: i32, step: u32, steps: u32) -> (i32, i32) {
    let t = f64::from(step) / f64::from(steps.max(1));
    (
        sx + ((f64::from(ex - sx)) * t).round() as i32,
        sy + ((f64::from(ey - sy)) * t).round() as i32,
    )
}

/// Turn the helper's report into a result.
///
/// `DoDragDrop` returns `DRAGDROP_S_DROP` even when the target refuses — the
/// refusal shows up as `DROPEFFECT_NONE`. So a zero effect is a refusal, and
/// it is `Ok`: the caller needs to know the gesture reached a target that said
/// no, which is a different problem from the drag failing.
///
/// An error string is the opposite case and must not share that shape: the
/// helper only sets it when the drag could not be performed (no
/// `WM_LBUTTONDOWN` before its deadline, `DRAGDROP_S_CANCEL`, a `DoDragDrop`
/// HRESULT). Reporting those as a refusal would tell the agent — which the
/// tool description instructs not to retry a refusal — that the target simply
/// said no, and would throw away the only account of what went wrong.
fn map_outcome(o: DragOutcome, drop_target: Option<WindowInfo>) -> Result<DragResult, ProtoError> {
    if let Some(why) = o.error {
        return Err(ProtoError::new(
            ErrorCode::DragFailed,
            format!("the drag helper reported: {why}"),
        ));
    }
    Ok(DragResult {
        dropped: o.dropped && o.effect != 0,
        effect: o.effect,
        drop_target,
    })
}

/// The top-level window under `(x, y)`, or `None` if the point is bare.
///
/// `GetAncestor(GA_ROOT)` because `WindowFromPoint` returns the deepest child
/// — a render surface whose class and title say nothing about which
/// application received the drop.
fn window_at(x: i32, y: i32) -> Option<HWND> {
    // SAFETY: WindowFromPoint accepts any POINT and returns a null HWND when
    // the point is over no window.
    let hwnd = unsafe { WindowFromPoint(POINT { x, y }) };
    if hwnd.0.is_null() {
        return None;
    }
    // SAFETY: GetAncestor accepts any live HWND; returns it unchanged when it
    // is already top-level.
    Some(unsafe { GetAncestor(hwnd, GA_ROOT) })
}

/// Is `line` the helper's readiness line?
fn is_ready(line: &str) -> bool {
    line.trim() == READY_LINE
}

/// The helper printed its outcome line instead of `{"ready":true}`.
fn early_failure(line: &str) -> ProtoError {
    let why = serde_json::from_str::<DragOutcome>(line)
        .ok()
        .and_then(|o| o.error)
        .unwrap_or_else(|| line.trim().to_string());
    ProtoError::new(
        ErrorCode::DragFailed,
        format!("the drag helper failed before it was ready: {why}"),
    )
}

/// Milliseconds left of `total_ms` after `spent`. Saturates at zero.
fn remaining_ms(total_ms: u32, spent: Duration) -> u32 {
    let spent_ms = u32::try_from(spent.as_millis()).unwrap_or(u32::MAX);
    total_ms.saturating_sub(spent_ms)
}

/// A start point on the same monitor as `(x, y)`, as far from it as the
/// monitor allows.
fn default_start_point(
    x: i32,
    y: i32,
    monitors: &[MonitorInfo],
) -> Result<(i32, i32), ProtoError> {
    let m = monitors
        .iter()
        .find(|m| contains(&m.bounds, x, y))
        .or_else(|| monitors.iter().find(|m| m.is_primary))
        .or_else(|| monitors.first())
        .ok_or_else(|| {
            ProtoError::new(
                ErrorCode::Internal,
                "no monitors enumerated, so there is nowhere to start the drag".to_string(),
            )
        })?;
    Ok(farthest_inset_corner(x, y, &m.bounds))
}

fn contains(b: &Rect, x: i32, y: i32) -> bool {
    x >= b.x && x < b.x + b.w && y >= b.y && y < b.y + b.h
}

/// The corner of `b` diagonally opposite `(x, y)`, pulled in by
/// [`START_MARGIN_PX`] (less on a monitor too small to afford it).
fn farthest_inset_corner(x: i32, y: i32, b: &Rect) -> (i32, i32) {
    let margin = START_MARGIN_PX
        .min((b.w - 1).max(0) / 2)
        .min((b.h - 1).max(0) / 2);
    let sx = if x - b.x > b.w / 2 {
        b.x + margin
    } else {
        b.x + b.w - 1 - margin
    };
    let sy = if y - b.y > b.h / 2 {
        b.y + margin
    } else {
        b.y + b.h - 1 - margin
    };
    (sx, sy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn mon(x: i32, y: i32, w: i32, h: i32, primary: bool) -> MonitorInfo {
        MonitorInfo {
            id: 1,
            name: "\\\\.\\DISPLAY1".to_string(),
            bounds: Rect { x, y, w, h },
            dpi_scale: 1.0,
            is_primary: primary,
        }
    }

    #[test]
    fn the_start_point_is_on_the_targets_own_monitor_and_far_from_it() {
        let mons = vec![
            mon(0, 0, 1920, 1080, true),
            mon(1920, 0, 2560, 1440, false),
        ];
        // A target on the *second* monitor must not start on the first.
        let (sx, sy) = default_start_point(3200, 700, &mons).unwrap();
        let b = &mons[1].bounds;
        assert!(contains(b, sx, sy), "start ({sx},{sy}) left monitor {b:?}");
        let d = (((sx - 3200) as f64).powi(2) + ((sy - 700) as f64).powi(2)).sqrt();
        assert!(d > 400.0, "start point only {d:.0}px from the drop target");

        // And a target on the primary starts on the primary.
        let (sx, sy) = default_start_point(960, 540, &mons).unwrap();
        assert!(contains(&mons[0].bounds, sx, sy), "got ({sx},{sy})");
    }

    #[test]
    fn a_point_outside_every_monitor_falls_back_to_the_primary() {
        let mons = vec![mon(0, 0, 1920, 1080, false), mon(0, 2000, 800, 600, true)];
        let (sx, sy) = default_start_point(-9000, -9000, &mons).unwrap();
        assert!(contains(&mons[1].bounds, sx, sy), "got ({sx},{sy})");
        assert!(default_start_point(0, 0, &[]).is_err());
    }

    #[test]
    fn the_inset_never_pushes_the_start_point_off_a_small_monitor() {
        // Smaller than twice the margin: the inset has to shrink, not wrap.
        let b = Rect { x: 10, y: 20, w: 60, h: 40 };
        for (x, y) in [(10, 20), (39, 39), (69, 59)] {
            let (sx, sy) = farthest_inset_corner(x, y, &b);
            assert!(contains(&b, sx, sy), "({x},{y}) -> ({sx},{sy}) is outside {b:?}");
        }
    }

    #[test]
    fn the_deadline_shrinks_by_what_injection_spent_and_saturates_at_zero() {
        assert_eq!(remaining_ms(10_000, Duration::from_millis(200)), 9_800);
        assert_eq!(remaining_ms(10_000, Duration::ZERO), 10_000);
        assert_eq!(remaining_ms(10_000, Duration::from_secs(30)), 0);
        // The walk must not be able to eat the budget on its own.
        let walk = u64::from(MOVE_STEPS + 1) * STEP_SLEEP_MS;
        assert!(
            remaining_ms(DRAG_DEADLINE_MS, Duration::from_millis(walk)) > DRAG_DEADLINE_MS / 2,
            "the cursor walk spends more than half the drag budget"
        );
    }

    #[test]
    fn a_refused_drop_is_a_result_not_a_failure() {
        // DoDragDrop reports DRAGDROP_S_DROP with DROPEFFECT_NONE when the
        // target says no. That is `dropped: false`, and it is not an error.
        let refused = map_outcome(DragOutcome { dropped: true, effect: 0, error: None }, None).unwrap();
        assert!(!refused.dropped);
        assert_eq!(refused.effect, 0);

        let accepted = map_outcome(DragOutcome { dropped: true, effect: 1, error: None }, None).unwrap();
        assert!(accepted.dropped);
        assert_eq!(accepted.effect, 1);

        // An error string from the helper is DragFailed, and it names the
        // helper's own reason rather than replacing it.
        let e = early_failure(r#"{"dropped":false,"effect":0,"error":"OleInitialize failed"}"#);
        assert_eq!(e.code, ErrorCode::DragFailed);
        assert!(e.message.contains("OleInitialize failed"), "{}", e.message);
    }

    /// The drop target must survive into the result, because it is the only
    /// thing that makes `dropped: true` falsifiable. A drag lands on whatever
    /// window is above the point, and a browser reports the same
    /// `DROPEFFECT_COPY` for "opened it in a tab" that a folder reports for a
    /// real copy — so a result without this field cannot be checked at all.
    #[test]
    fn the_window_that_received_the_drop_is_reported_alongside_the_effect() {
        let target = WindowInfo {
            hwnd: 42,
            title: "fu_probe - File Explorer".to_string(),
            class: "CabinetWClass".to_string(),
            process_name: "explorer.exe".to_string(),
            pid: 7,
            bounds: Rect { x: 0, y: 0, w: 800, h: 600 },
        };
        let r = map_outcome(
            DragOutcome { dropped: true, effect: 1, error: None },
            Some(target.clone()),
        )
        .unwrap();
        assert!(r.dropped && r.effect == 1);
        assert_eq!(r.drop_target.as_ref(), Some(&target));

        // And a point over nothing reports nothing rather than inventing a
        // window: `None` here is what tells the caller it cannot check.
        let bare = map_outcome(
            DragOutcome { dropped: true, effect: 1, error: None },
            None,
        )
        .unwrap();
        assert!(bare.drop_target.is_none());
    }

    /// `WindowFromPoint` returns the deepest child; the caller needs the
    /// top-level window to compare against what `list_windows` handed it.
    #[test]
    fn window_at_reports_a_top_level_window() {
        use windows::Win32::UI::WindowsAndMessaging::{GetDesktopWindow, IsWindow};
        // SAFETY: both take a plain HWND / no arguments.
        let desktop = unsafe { GetDesktopWindow() };
        let mut r = windows::Win32::Foundation::RECT::default();
        // SAFETY: `r` is a live local; `desktop` is always a valid HWND.
        unsafe { windows::Win32::UI::WindowsAndMessaging::GetWindowRect(desktop, &mut r) }.unwrap();
        let (cx, cy) = ((r.left + r.right) / 2, (r.top + r.bottom) / 2);
        // Not `if let`: the desktop always covers its own centre (Progman at
        // minimum), so a `None` here is `window_at` being broken, and a test
        // that skips itself on the failure it exists to catch catches nothing.
        let h = window_at(cx, cy).expect("nothing under the centre of the desktop");
        // SAFETY: `h` came from the window manager moments ago.
        assert!(unsafe { IsWindow(Some(h)) }.as_bool());
        // GA_ROOT is idempotent, so a second climb must not move.
        assert_eq!(window_at(cx, cy).map(|x| x.0), Some(h.0));
        // SAFETY: `h` is live.
        let parent = unsafe { GetAncestor(h, GA_ROOT) };
        assert_eq!(parent.0, h.0, "window_at returned a child, not a root");
    }

    #[test]
    fn a_helper_error_after_ready_is_a_failure_and_never_a_refusal() {
        // Every failure the helper hits *after* {"ready":true} — no
        // WM_LBUTTONDOWN, DRAGDROP_S_CANCEL, a DoDragDrop HRESULT — arrives in
        // exactly this shape. Mapping it to Ok(dropped: false) would read as a
        // refusal, which the tool description tells the agent not to retry,
        // and would discard the helper's reason. This test is what stops that.
        let e = map_outcome(DragOutcome {
            dropped: false,
            effect: 0,
            error: Some("no WM_LBUTTONDOWN arrived before the deadline".to_string()),
        }, None)
        .expect_err("a helper error must not become Ok(dropped: false)");
        assert_eq!(e.code, ErrorCode::DragFailed);
        assert!(e.message.contains("no WM_LBUTTONDOWN"), "{}", e.message);
    }

    #[test]
    fn the_release_guard_fires_on_an_early_question_mark() {
        // The property the whole task turns on: the button-up runs even when
        // the walk bails out through `?` long before the happy path ends.
        static FIRED: AtomicU32 = AtomicU32::new(0);
        fn walk_that_fails() -> Result<(), ProtoError> {
            let _up = ReleaseGuard(|| {
                FIRED.fetch_add(1, Ordering::SeqCst);
            });
            Err(ProtoError::new(ErrorCode::Internal, "cursor move failed".to_string()))?;
            unreachable!("the early return is the point of this test");
        }
        assert!(walk_that_fails().is_err());
        assert_eq!(FIRED.load(Ordering::SeqCst), 1, "the button-up never ran");
    }

    #[test]
    fn only_the_exact_ready_line_is_readiness() {
        // Exactly what drag_helper::drag writes, minus the newline lines()
        // already stripped.
        assert!(is_ready(r#"{"ready":true}"#));
        assert!(is_ready("{\"ready\":true}\r"));
        // A helper error that merely mentions the literal must not be read as
        // readiness: the daemon answers readiness with a real mouse click.
        assert!(!is_ready(
            r#"{"dropped":false,"effect":0,"error":"no {\"ready\":true} was sent"}"#
        ));
        assert!(!is_ready(r#"{"ready":false}"#));
    }

    #[test]
    fn job_line_matches_what_the_helper_parses() {
        // Pinned against the literal in fastuse-daemon's
        // `a_job_line_is_accepted_in_the_form_the_daemon_writes_it`. These two
        // structs are mirrored across a process boundary; this is what keeps
        // them from drifting apart silently.
        let line = serde_json::to_string(&DragJob {
            paths: vec![r"C:\Windows\win.ini".to_string()],
            start_x: 400,
            start_y: 400,
            deadline_ms: 10_000,
        })
        .unwrap();
        assert_eq!(
            line,
            r#"{"paths":["C:\\Windows\\win.ini"],"start_x":400,"start_y":400,"deadline_ms":10000}"#
        );
    }
}
