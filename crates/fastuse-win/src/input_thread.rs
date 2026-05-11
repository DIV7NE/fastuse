//! STA input thread (D-26).
//!
//! Owns a hidden message-only HWND and runs `GetMessage`/`DispatchMessage`.
//! All input jobs are dispatched through a [`crate::input::backend::InputBackend`]
//! trait object. The default backend is
//! [`crate::input::sendinput_backend::SendInputBackend`]; alternative backends
//! (e.g. hardware HID, recording mock) can be injected via
//! [`spawn_input_thread_with_backend`].

use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use crate::input::backend::{InputAction, InputBackend};
use crate::input::sendinput_backend::SendInputBackend;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    PostThreadMessageW, RegisterClassExW, TranslateMessage, UnregisterClassW, CW_USEDEFAULT,
    HMENU, MSG, WS_DISABLED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP, WM_DISPLAYCHANGE,
    WM_QUIT, WM_USER, WNDCLASSEXW,
};

/// Job sent to the input thread.
pub enum InputJob {
    /// No-op probe; replies `Ack`.
    Noop,
    /// Phase 2 inline closure-style job. `Box<dyn FnOnce>` so the thread can
    /// run handler-specific logic without re-encoding every Phase 2 variant
    /// here. Reply payload is `serde_json::Value` to keep the channel
    /// mono-typed; callers decode back to their concrete Response variant.
    Run(Box<dyn FnOnce() -> Result<serde_json::Value, fastuse_proto::Error> + Send>),
    /// Flush all currently-held modifiers (panic-hook / connection-drop).
    /// Bypasses the backend entirely — modifier state lives outside the
    /// backend abstraction (modifier_guard recovery path).
    FlushHeldModifiers,
    /// Dispatch a single [`InputAction`] through the thread's
    /// [`InputBackend`]. This is the primary entry point for the daemon's
    /// `Request::Computer(...)` handler (Task 10 onwards).
    Backend(InputAction),
}

impl std::fmt::Debug for InputJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InputJob::Noop => write!(f, "InputJob::Noop"),
            InputJob::Run(_) => write!(f, "InputJob::Run(<fn>)"),
            InputJob::FlushHeldModifiers => write!(f, "InputJob::FlushHeldModifiers"),
            InputJob::Backend(a) => write!(f, "InputJob::Backend({a:?})"),
        }
    }
}

/// Reply payload from the input thread.
pub enum InputReplyPayload {
    /// Result of a Run job carrying back a JSON value (or proto error).
    Run(Result<serde_json::Value, fastuse_proto::Error>),
    /// Plain ack for Noop / FlushHeldModifiers.
    Ack,
}

/// Reply channel paired with each [`InputJob`].
pub type InputReply = mpsc::Sender<InputReplyPayload>;

/// Errors raised by the input thread.
#[derive(Debug, thiserror::Error)]
pub enum InputThreadError {
    /// Thread terminated before reply could be delivered.
    #[error("input thread shut down")]
    ShutDown,
}

/// Handle returned by [`spawn_input_thread`]. Dropping signals shutdown and
/// joins the OS thread.
pub struct InputThreadHandle {
    sender: Option<mpsc::Sender<(InputJob, InputReply)>>,
    thread_id: u32,
    join: Option<JoinHandle<()>>,
}

impl InputThreadHandle {
    /// CR-04: thread id of the input STA worker. Callers (notably the daemon
    /// panic hook) compare against `GetCurrentThreadId` to detect "we are
    /// the input thread" and flush directly instead of going through the
    /// channel — sending to the channel deadlocks when the input thread is
    /// itself mid-panic.
    pub fn thread_id(&self) -> u32 {
        self.thread_id
    }

    /// Send a job to the input thread; blocks for the reply. Phase 1
    /// preserved this signature for the Noop probe and the new
    /// FlushHeldModifiers variant.
    pub fn send(&self, job: InputJob) -> Result<(), InputThreadError> {
        match self.send_payload(job)? {
            InputReplyPayload::Ack => Ok(()),
            InputReplyPayload::Run(_) => Ok(()),
        }
    }

    /// Phase 2: send a Run job and return the payload. Caller decodes.
    pub fn run<F, T>(&self, f: F) -> Result<T, fastuse_proto::Error>
    where
        F: FnOnce() -> Result<T, fastuse_proto::Error> + Send + 'static,
        T: serde::Serialize + serde::de::DeserializeOwned,
    {
        let job = InputJob::Run(Box::new(move || {
            f().and_then(|v| {
                serde_json::to_value(&v).map_err(|e| fastuse_proto::Error::new(
                    fastuse_proto::ErrorCode::Internal,
                    format!("encode reply: {e}"),
                ))
            })
        }));
        match self.send_payload(job).map_err(|e| {
            // WR-10: surface input-thread death loudly. The daemon has no
            // supervisor in v1; every subsequent dispatch will fail with
            // DaemonDead until the user restarts. A Phase-3 follow-up will
            // add a respawn loop.
            tracing::error!(
                error = ?e,
                "input thread died — daemon must be restarted (Phase-3 will add respawn)"
            );
            fastuse_proto::Error::new(
                fastuse_proto::ErrorCode::DaemonDead,
                "input thread shut down".to_string(),
            )
        })? {
            InputReplyPayload::Run(r) => {
                let v = r?;
                serde_json::from_value(v).map_err(|e| fastuse_proto::Error::new(
                    fastuse_proto::ErrorCode::Internal,
                    format!("decode reply: {e}"),
                ))
            }
            InputReplyPayload::Ack => Err(fastuse_proto::Error::new(
                fastuse_proto::ErrorCode::Internal,
                "expected Run reply, got Ack".to_string(),
            )),
        }
    }

    /// Dispatch an [`InputAction`] through the thread's [`InputBackend`].
    /// Blocks until the action completes. Returns the backend error (if any)
    /// wrapped in a [`fastuse_proto::Error`].
    pub fn dispatch(&self, action: InputAction) -> Result<(), fastuse_proto::Error> {
        match self.send_payload(InputJob::Backend(action)).map_err(|e| {
            tracing::error!(
                error = ?e,
                "input thread died — daemon must be restarted (Phase-3 will add respawn)"
            );
            fastuse_proto::Error::new(
                fastuse_proto::ErrorCode::DaemonDead,
                "input thread shut down".to_string(),
            )
        })? {
            InputReplyPayload::Ack => Ok(()),
            InputReplyPayload::Run(r) => r.map(|_| ()),
        }
    }

    fn send_payload(&self, job: InputJob) -> Result<InputReplyPayload, InputThreadError> {
        let (tx, rx) = mpsc::channel();
        self.sender
            .as_ref()
            .ok_or(InputThreadError::ShutDown)?
            .send((job, tx))
            .map_err(|_| InputThreadError::ShutDown)?;
        // SAFETY: posting WM_USER to our owned thread is safe.
        unsafe {
            let _ = PostThreadMessageW(self.thread_id, WM_USER, WPARAM(0), LPARAM(0));
        }
        rx.recv().map_err(|_| InputThreadError::ShutDown)
    }
}

impl Drop for InputThreadHandle {
    fn drop(&mut self) {
        // Drop sender to break the loop, then post WM_QUIT to wake the pump.
        self.sender.take();
        // SAFETY: thread_id was issued by GetCurrentThreadId on the worker; safe to post WM_QUIT.
        unsafe {
            let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn run_job(job: InputJob, backend: &dyn InputBackend) -> InputReplyPayload {
    match job {
        InputJob::Noop => InputReplyPayload::Ack,
        InputJob::Run(f) => InputReplyPayload::Run(f()),
        InputJob::FlushHeldModifiers => {
            crate::input::handlers::flush_held_modifiers();
            InputReplyPayload::Ack
        }
        InputJob::Backend(action) => {
            match backend.dispatch(action) {
                Ok(()) => InputReplyPayload::Ack,
                Err(e) => InputReplyPayload::Run(Err(fastuse_proto::Error::new(
                    fastuse_proto::ErrorCode::Internal,
                    format!("backend dispatch: {e}"),
                ))),
            }
        }
    }
}

extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_DISPLAYCHANGE {
        // The OS broadcasts WM_DISPLAYCHANGE to top-level windows when the
        // monitor configuration changes. Our hidden message-only HWND
        // receives it because we have one. Invalidate the monitor cache so
        // the next list_monitors call re-enumerates with current bounds /
        // DPI. (Plan Task 8.)
        crate::window::monitors::invalidate_cache();
        // Signal the capture thread to drop cached DXGI state on its next
        // capture. Cross-thread atomic flag — STATE is thread-local on the
        // capture thread, so we can't invalidate it from here directly.
        crate::capture::dxgi::signal_display_changed();
    }
    // SAFETY: DefWindowProcW is the Win32 fallback handler; always safe to call.
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

/// Spawn the STA input thread with a custom [`InputBackend`]. Returns a handle
/// that signals shutdown on Drop. Prefer [`spawn_input_thread`] for production
/// use; this entry point is primarily for tests that inject a recording mock.
pub fn spawn_input_thread_with_backend(
    backend: Box<dyn InputBackend>,
) -> std::io::Result<InputThreadHandle> {
    spawn_input_thread_inner(backend)
}

/// Spawn the STA input thread. Returns a handle that signals shutdown on Drop.
/// The default backend is [`SendInputBackend`].
pub fn spawn_input_thread() -> std::io::Result<InputThreadHandle> {
    spawn_input_thread_inner(Box::new(SendInputBackend::new()))
}

fn spawn_input_thread_inner(
    backend: Box<dyn InputBackend>,
) -> std::io::Result<InputThreadHandle> {
    let (tx, rx) = mpsc::channel::<(InputJob, InputReply)>();
    let (id_tx, id_rx) = mpsc::channel::<u32>();

    let join = thread::Builder::new()
        .name("fastuse-input".into())
        .spawn(move || {
            // The backend is owned by the worker thread for its entire lifetime.
            let backend: Box<dyn InputBackend> = backend;

            // SAFETY: STA init for the input thread; matched by CoUninitialize.
            let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };

            // Register a private window class for the hidden message-only window.
            let class_name = w!("FastuseInputMsgClass");
            // SAFETY: GetModuleHandleW(None) returns the current process module.
            let hinstance = unsafe { GetModuleHandleW(PCWSTR::null()) }.unwrap_or_default();
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wnd_proc),
                hInstance: hinstance.into(),
                lpszClassName: class_name,
                ..Default::default()
            };
            // SAFETY: WNDCLASSEXW is fully populated above; class registration is idempotent.
            let _ = unsafe { RegisterClassExW(&wc) };

            // CR-02 fix: must be a *top-level* window (parent = None) so the
            // OS delivers WM_DISPLAYCHANGE — message-only windows (HWND_MESSAGE)
            // do NOT receive system broadcasts and the monitor-cache invalidation
            // hook would otherwise be silently dead. The window is invisible
            // (no WS_VISIBLE), disabled (WS_DISABLED) so it can't receive focus,
            // hidden from the taskbar (WS_EX_TOOLWINDOW), and refuses activation
            // (WS_EX_NOACTIVATE).
            // SAFETY: registered class, valid args; null parent makes a top-level.
            let hwnd = unsafe {
                CreateWindowExW(
                    WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                    class_name,
                    w!("fastuse-input"),
                    WS_POPUP | WS_DISABLED,
                    CW_USEDEFAULT,
                    CW_USEDEFAULT,
                    0,
                    0,
                    None,
                    Some(HMENU::default()),
                    Some(hinstance.into()),
                    None,
                )
            }
            .unwrap_or(HWND::default());

            // SAFETY: GetCurrentThreadId is always safe.
            let tid = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };
            let _ = id_tx.send(tid);

            // Message pump: drain any pending jobs between message dispatches.
            let mut msg = MSG::default();
            loop {
                // Process any queued input jobs first.
                while let Ok((job, reply)) = rx.try_recv() {
                    let payload = run_job(job, &*backend);
                    let _ = reply.send(payload);
                }
                // SAFETY: GetMessageW blocks for the next OS message; HWND nullable.
                let got = unsafe { GetMessageW(&mut msg, Some(HWND::default()), 0, 0) };
                if got.0 <= 0 {
                    break;
                }
                // SAFETY: msg fully populated by GetMessageW.
                unsafe {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                // After waking on WM_USER (job-queued nudge), drain again.
                while let Ok((job, reply)) = rx.try_recv() {
                    let payload = run_job(job, &*backend);
                    let _ = reply.send(payload);
                }
            }

            // Tear down the message-only window and unregister our class so
            // the HWND/atom don't leak past process shutdown (WR-13). Best-
            // effort: failures here are non-fatal because process exit will
            // reclaim regardless.
            if hwnd.0 as isize != 0 {
                // SAFETY: HWND was created above on this thread.
                let _ = unsafe { DestroyWindow(hwnd) };
            }
            // SAFETY: class was registered on this thread above.
            let _ = unsafe { UnregisterClassW(class_name, Some(hinstance.into())) };
            // SAFETY: paired with the CoInitializeEx above.
            unsafe { CoUninitialize() };
        })?;

    let thread_id = id_rx.recv().map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::Other, "input thread failed to start")
    })?;

    Ok(InputThreadHandle {
        sender: Some(tx),
        thread_id,
        join: Some(join),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_thread_acks_noop_and_joins() {
        let h = spawn_input_thread().expect("spawn input thread");
        h.send(InputJob::Noop).expect("noop ack");
        // Drop joins.
        let start = std::time::Instant::now();
        drop(h);
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
    }
}
