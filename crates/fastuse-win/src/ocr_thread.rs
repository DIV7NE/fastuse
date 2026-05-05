//! Dedicated MTA thread hosting `Windows.Media.Ocr::OcrEngine`.
//!
//! Why a new thread instead of reusing uia_pool or capture_thread:
//! - capture_thread owns the GPU pipeline (D3D11 device, duplication, staging
//!   texture). A 30-100ms OCR job blocks every screenshot — including the
//!   post-action `screenshot_after` that fires on the same logical action.
//! - uia_pool workers are sized for sub-20ms work. With 3 workers, one
//!   in-flight OCR + one verify-poll + one routine query saturates the pool.
//! - `OcrEngine` has cached language-model state (~50-200ms cold construction).
//!   A dedicated thread caches one engine in `OnceLock` for daemon lifetime.
//!
//! Fourth COM-thread surface (joins `input_thread`, `uia_pool`,
//! `capture_thread`). Lives in `fastuse-win`, which is exempt from the
//! `check-com` lint by crate-level filter; D-25 invariant upheld by
//! construction (the only callers are the daemon dispatcher routing through
//! this handle).

use std::sync::mpsc::{self, Sender};
use std::sync::OnceLock;
use std::thread::{self, JoinHandle};

use windows::Win32::System::Com::{
    CoInitializeEx, CoUninitialize, COINIT_DISABLE_OLE1DDE, COINIT_MULTITHREADED,
};

/// A job is a closure executed on the OCR thread. Same shape as
/// `capture_thread::CaptureJob::Run` and `uia_pool` worker jobs.
type OcrJob = Box<dyn FnOnce() + Send + 'static>;

/// Handle for dispatching jobs to the OCR thread.
pub struct OcrThreadHandle {
    sender: Option<Sender<OcrJob>>,
    join: Option<JoinHandle<()>>,
}

impl OcrThreadHandle {
    /// Run a closure on the OCR thread, blocking until it returns.
    /// Errors only when the thread has died.
    pub fn run<F, T>(&self, f: F) -> Result<T, OcrThreadError>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let (tx, rx) = mpsc::channel();
        let job: OcrJob = Box::new(move || {
            let r = f();
            let _ = tx.send(r);
        });
        self.sender
            .as_ref()
            .ok_or(OcrThreadError::Dead)?
            .send(job)
            .map_err(|_| OcrThreadError::Dead)?;
        rx.recv().map_err(|_| OcrThreadError::Dead)
    }
}

impl Drop for OcrThreadHandle {
    fn drop(&mut self) {
        // Closing the sender lets the OCR thread loop fall out of `recv`.
        self.sender.take();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Error from [`OcrThreadHandle::run`].
#[derive(Debug, thiserror::Error)]
pub enum OcrThreadError {
    /// OCR thread is no longer running.
    #[error("ocr thread is dead")]
    Dead,
}

/// Spawn the OCR thread. Returns a handle; the thread lives until the handle
/// is dropped (typically daemon lifetime).
pub fn spawn_ocr_thread() -> std::io::Result<OcrThreadHandle> {
    let (sender, receiver) = mpsc::channel::<OcrJob>();
    let join = thread::Builder::new()
        .name("fastuse-ocr".into())
        .spawn(move || {
            // SAFETY: MTA per-thread init; matched by `CoUninitialize` after
            // the loop. `OcrEngine`'s WinRT activation factory runs on MTA
            // threads (RoInitialize MULTITHREADED is satisfied by
            // CoInitializeEx COINIT_MULTITHREADED on Win10+).
            let _ = unsafe {
                CoInitializeEx(None, COINIT_MULTITHREADED | COINIT_DISABLE_OLE1DDE)
            };

            // Pre-warm the engine so the first OCR call doesn't pay 50-200ms.
            let _ = ocr_engine();

            while let Ok(job) = receiver.recv() {
                job();
            }

            // SAFETY: paired with `CoInitializeEx` above.
            unsafe { CoUninitialize() };
        })?;
    Ok(OcrThreadHandle {
        sender: Some(sender),
        join: Some(join),
    })
}

/// Lazy-init `OcrEngine`. First call constructs from user profile languages
/// (~50-200ms); subsequent calls are free. Held only on the OCR thread.
pub fn ocr_engine() -> Option<&'static OcrEngineSlot> {
    static SLOT: OnceLock<OcrEngineSlot> = OnceLock::new();
    Some(SLOT.get_or_init(OcrEngineSlot::new_or_empty))
}

/// Wrapper around `Windows.Media.Ocr.OcrEngine`. Held only on the OCR thread.
pub struct OcrEngineSlot {
    /// `None` when WinRT activation failed (no language packs, etc.).
    inner: Option<windows::Media::Ocr::OcrEngine>,
}

impl OcrEngineSlot {
    fn new_or_empty() -> Self {
        // SAFETY: WinRT activation factory call on MTA thread; failure is
        // legitimate (no language packs installed) and surfaces as `None`.
        let inner = unsafe { build_engine_unchecked().ok() };
        Self { inner }
    }

    /// Returns the engine if available; `None` if not constructable.
    pub fn engine(&self) -> Option<&windows::Media::Ocr::OcrEngine> {
        self.inner.as_ref()
    }
}

unsafe fn build_engine_unchecked() -> windows::core::Result<windows::Media::Ocr::OcrEngine> {
    use windows::Media::Ocr::OcrEngine;
    OcrEngine::TryCreateFromUserProfileLanguages()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ocr_thread_dispatches_closure_and_joins() {
        let h = spawn_ocr_thread().expect("spawn ocr thread");
        let v: i32 = h.run(|| 42).expect("closure runs");
        assert_eq!(v, 42);
        let start = std::time::Instant::now();
        drop(h);
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
    }

    #[test]
    fn ocr_thread_run_is_serial() {
        let h = spawn_ocr_thread().expect("spawn ocr thread");
        let a = h.run(|| 1).expect("a");
        let b = h.run(move || a + 1).expect("b");
        assert_eq!(b, 2);
    }
}
