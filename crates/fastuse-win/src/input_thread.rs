//! STA input thread skeleton (D-26).
//!
//! Owns a hidden message-only HWND and runs `GetMessage`/`DispatchMessage`.
//! Phase 1 only acks `InputJob::Noop`; Phase 2 will plumb real input via
//! `SendInput`. The hidden HWND is needed because some Win32 input messages
//! (mouse hover, raw input, foreground tracking) require a window owner.

use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    PostMessageW, PostThreadMessageW, RegisterClassExW, TranslateMessage, UnregisterClassW,
    CW_USEDEFAULT, HMENU, HWND_MESSAGE, MSG, WINDOW_EX_STYLE, WINDOW_STYLE, WM_QUIT, WM_USER,
    WNDCLASSEXW,
};

/// Job sent to the input thread.
#[derive(Debug)]
pub enum InputJob {
    /// No-op probe; replies `Ok(())`.
    Noop,
}

/// Reply channel paired with each [`InputJob`].
pub type InputReply = mpsc::Sender<Result<(), InputThreadError>>;

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
    /// Send a job to the input thread; blocks for the reply.
    pub fn send(&self, job: InputJob) -> Result<(), InputThreadError> {
        let (tx, rx) = mpsc::channel();
        self.sender
            .as_ref()
            .ok_or(InputThreadError::ShutDown)?
            .send((job, tx))
            .map_err(|_| InputThreadError::ShutDown)?;
        // Wake the message pump so it polls the channel.
        // SAFETY: posting WM_USER to our owned thread is safe.
        unsafe {
            let _ = PostThreadMessageW(self.thread_id, WM_USER, WPARAM(0), LPARAM(0));
        }
        rx.recv().map_err(|_| InputThreadError::ShutDown)?
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

extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // SAFETY: DefWindowProcW is the Win32 fallback handler; always safe to call.
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

/// Spawn the STA input thread. Returns a handle that signals shutdown on Drop.
pub fn spawn_input_thread() -> std::io::Result<InputThreadHandle> {
    let (tx, rx) = mpsc::channel::<(InputJob, InputReply)>();
    let (id_tx, id_rx) = mpsc::channel::<u32>();

    let join = thread::Builder::new()
        .name("fastuse-input".into())
        .spawn(move || {
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

            // SAFETY: Creating a HWND_MESSAGE window with our registered class.
            let hwnd = unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    class_name,
                    w!("fastuse-input"),
                    WINDOW_STYLE(0),
                    CW_USEDEFAULT,
                    CW_USEDEFAULT,
                    0,
                    0,
                    Some(HWND_MESSAGE),
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
                    let res = match job {
                        InputJob::Noop => Ok(()),
                    };
                    let _ = reply.send(res);
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
                    let res = match job {
                        InputJob::Noop => Ok(()),
                    };
                    let _ = reply.send(res);
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
