//! MTA capture thread skeleton (D-26 / D-28).
//!
//! Phase 1 only acks `CaptureJob::Noop` — Phase 3 plugs in DXGI Desktop
//! Duplication and Windows.Graphics.Capture. The thread is MTA so that future
//! D3D11 device + IDXGIOutputDuplication can be acquired once and reused for
//! the daemon's lifetime.

use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};

use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

/// Capture job sent to the thread.
#[derive(Debug)]
pub enum CaptureJob {
    /// No-op probe.
    Noop,
}

/// Capture worker reply channel.
pub type CaptureReply = Sender<Result<(), CaptureError>>;

/// Errors from the capture thread.
#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    /// Thread shut down before reply.
    #[error("capture thread shut down")]
    ShutDown,
}

/// Handle to the capture thread; Drop signals shutdown.
pub struct CaptureThreadHandle {
    sender: Option<Sender<(CaptureJob, CaptureReply)>>,
    join: Option<JoinHandle<()>>,
}

impl CaptureThreadHandle {
    /// Submit a capture job; blocks for the reply.
    pub fn send(&self, job: CaptureJob) -> Result<(), CaptureError> {
        let (tx, rx) = mpsc::channel();
        self.sender
            .as_ref()
            .ok_or(CaptureError::ShutDown)?
            .send((job, tx))
            .map_err(|_| CaptureError::ShutDown)?;
        rx.recv().map_err(|_| CaptureError::ShutDown)?
    }
}

impl Drop for CaptureThreadHandle {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Spawn the single MTA capture thread.
pub fn spawn_capture_thread() -> std::io::Result<CaptureThreadHandle> {
    let (tx, rx) = mpsc::channel::<(CaptureJob, CaptureReply)>();
    let join = thread::Builder::new()
        .name("fastuse-capture".into())
        .spawn(move || {
            // SAFETY: MTA per-thread init; matched by CoUninitialize on exit.
            let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };

            // Phase 3 will pre-acquire ID3D11Device + IDXGIOutputDuplication here.
            // Phase 1: just service jobs.
            while let Ok((job, reply)) = rx.recv() {
                let res = match job {
                    CaptureJob::Noop => Ok(()),
                };
                let _ = reply.send(res);
            }

            // SAFETY: paired with CoInitializeEx above.
            unsafe { CoUninitialize() };
        })?;

    Ok(CaptureThreadHandle {
        sender: Some(tx),
        join: Some(join),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_thread_acks_noop_and_joins() {
        let h = spawn_capture_thread().expect("spawn capture thread");
        h.send(CaptureJob::Noop).expect("noop ack");
        let start = std::time::Instant::now();
        drop(h);
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
    }
}
