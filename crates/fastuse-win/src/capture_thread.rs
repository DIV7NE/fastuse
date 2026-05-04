//! MTA capture thread (D-26 / D-28).
//!
//! Phase 1 only acked `CaptureJob::Noop`; Phase 3 promotes this to a
//! closure-dispatch surface so DXGI/WGC handlers can inject arbitrary work
//! onto the single MTA capture thread without per-feature enum bloat. Phase
//! 1's `Noop` remains for the existing liveness test.
//!
//! The capture thread is the sole COM-MTA thread permitted to touch
//! `ID3D11Device`, `IDXGIOutputDuplication`, and the cached staging
//! `ID3D11Texture2D` (D-26: tokio workers NEVER call into the Windows API
//! surface; everything routes through this thread).

use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};

use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

/// Job sent to the capture thread.
///
/// `Run` carries an arbitrary `FnOnce` so handlers (Task 04 DXGI capture,
/// Task 07 screenshot/region) can do all their D3D11 work on this thread
/// without proliferating enum variants. Reply payload is
/// `serde_json::Value` so the channel stays mono-typed; callers decode
/// back to their concrete response shape.
pub enum CaptureJob {
    /// No-op probe (Phase 1 liveness).
    Noop,
    /// Phase 3 closure-dispatch slot.
    Run(Box<dyn FnOnce() -> Result<serde_json::Value, fastuse_proto::Error> + Send>),
}

impl std::fmt::Debug for CaptureJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CaptureJob::Noop => write!(f, "CaptureJob::Noop"),
            CaptureJob::Run(_) => write!(f, "CaptureJob::Run(<fn>)"),
        }
    }
}

/// Reply payload from the capture thread.
pub enum CaptureReplyPayload {
    /// Plain ack (Noop).
    Ack,
    /// Result of a `Run` closure.
    Run(Result<serde_json::Value, fastuse_proto::Error>),
}

/// Reply channel paired with each [`CaptureJob`].
pub type CaptureReply = Sender<CaptureReplyPayload>;

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
    /// Submit a Noop probe; blocks for the reply (Phase 1 surface).
    pub fn send(&self, job: CaptureJob) -> Result<(), CaptureError> {
        let (tx, rx) = mpsc::channel();
        self.sender
            .as_ref()
            .ok_or(CaptureError::ShutDown)?
            .send((job, tx))
            .map_err(|_| CaptureError::ShutDown)?;
        match rx.recv().map_err(|_| CaptureError::ShutDown)? {
            CaptureReplyPayload::Ack => Ok(()),
            CaptureReplyPayload::Run(_) => Ok(()),
        }
    }

    /// Phase 3: dispatch a closure onto the capture thread and decode the
    /// JSON-shaped reply back to `T`. The closure runs on the MTA capture
    /// thread; reply travels back through the oneshot. Errors propagate as
    /// `fastuse_proto::Error`.
    pub fn run<F, T>(&self, f: F) -> Result<T, fastuse_proto::Error>
    where
        F: FnOnce() -> Result<T, fastuse_proto::Error> + Send + 'static,
        T: serde::Serialize + serde::de::DeserializeOwned,
    {
        let job = CaptureJob::Run(Box::new(move || {
            f().and_then(|v| {
                serde_json::to_value(&v).map_err(|e| {
                    fastuse_proto::Error::new(
                        fastuse_proto::ErrorCode::Internal,
                        format!("encode capture reply: {e}"),
                    )
                })
            })
        }));
        let (tx, rx) = mpsc::channel();
        self.sender
            .as_ref()
            .ok_or_else(|| {
                fastuse_proto::Error::new(
                    fastuse_proto::ErrorCode::DaemonDead,
                    "capture thread shut down".to_string(),
                )
            })?
            .send((job, tx))
            .map_err(|_| {
                fastuse_proto::Error::new(
                    fastuse_proto::ErrorCode::DaemonDead,
                    "capture thread shut down".to_string(),
                )
            })?;
        let reply = rx.recv().map_err(|_| {
            fastuse_proto::Error::new(
                fastuse_proto::ErrorCode::DaemonDead,
                "capture thread shut down".to_string(),
            )
        })?;
        match reply {
            CaptureReplyPayload::Run(r) => {
                let v = r?;
                serde_json::from_value(v).map_err(|e| {
                    fastuse_proto::Error::new(
                        fastuse_proto::ErrorCode::Internal,
                        format!("decode capture reply: {e}"),
                    )
                })
            }
            CaptureReplyPayload::Ack => Err(fastuse_proto::Error::new(
                fastuse_proto::ErrorCode::Internal,
                "expected Run reply, got Ack".to_string(),
            )),
        }
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

            // Phase 3 will pre-acquire ID3D11Device + IDXGIOutputDuplication
            // here once Task 04 lands the DXGI backend. For now, the loop
            // services Noop probes and Run closures.
            while let Ok((job, reply)) = rx.recv() {
                let payload = match job {
                    CaptureJob::Noop => CaptureReplyPayload::Ack,
                    CaptureJob::Run(f) => CaptureReplyPayload::Run(f()),
                };
                let _ = reply.send(payload);
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

    #[test]
    fn capture_thread_dispatches_closure() {
        let h = spawn_capture_thread().expect("spawn capture thread");
        let v: i32 = h
            .run(|| Ok::<i32, fastuse_proto::Error>(42))
            .expect("closure runs");
        assert_eq!(v, 42);
    }

    #[test]
    fn capture_thread_propagates_closure_error() {
        let h = spawn_capture_thread().expect("spawn capture thread");
        let r: Result<i32, fastuse_proto::Error> = h.run(|| {
            Err(fastuse_proto::Error::new(
                fastuse_proto::ErrorCode::Internal,
                "boom",
            ))
        });
        assert!(r.is_err());
    }
}
