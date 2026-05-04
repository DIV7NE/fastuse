//! MTA UIA pool (D-26).
//!
//! Spawns N MTA worker threads. Each initializes (lazily) a shared
//! `OnceLock<IUIAutomation>`. Phase 1 only acks `UiaJob::Noop`; Phase 3 adds
//! the real CacheRequest-based queries.

use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};

use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

use uiautomation::UIAutomation;

/// UIA job sent to a worker.
#[derive(Debug)]
pub enum UiaJob {
    /// No-op probe.
    Noop,
}

/// UIA worker reply channel.
pub type UiaReply = Sender<Result<(), UiaPoolError>>;

/// Errors from the UIA pool.
#[derive(Debug, thiserror::Error)]
pub enum UiaPoolError {
    /// Pool was dropped before reply could be delivered.
    #[error("uia pool shut down")]
    ShutDown,
    /// CoCreate of CUIAutomation failed.
    #[error("CUIAutomation init failed: {0}")]
    InitFailed(String),
}

/// Free-threaded UIAutomation singleton (initialized once across the pool).
///
/// `uiautomation::UIAutomation` wraps the COM `IUIAutomation` interface, which
/// is documented as free-threaded. We share it via Arc + OnceLock.
struct UiaSingleton(UIAutomation);
// SAFETY: IUIAutomation is documented free-threaded by Microsoft.
unsafe impl Send for UiaSingleton {}
unsafe impl Sync for UiaSingleton {}

static UIA_SINGLETON: OnceLock<Arc<UiaSingleton>> = OnceLock::new();

fn get_or_init_uia() -> Result<Arc<UiaSingleton>, UiaPoolError> {
    if let Some(s) = UIA_SINGLETON.get() {
        return Ok(Arc::clone(s));
    }
    let auto = UIAutomation::new_direct()
        .map_err(|e| UiaPoolError::InitFailed(format!("{e:?}")))?;
    let arc = Arc::new(UiaSingleton(auto));
    let _ = UIA_SINGLETON.set(Arc::clone(&arc));
    Ok(UIA_SINGLETON.get().cloned().unwrap_or(arc))
}

/// Handle to the UIA pool. Drop signals shutdown of all workers.
pub struct UiaPoolHandle {
    sender: Option<Sender<(UiaJob, UiaReply)>>,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

impl UiaPoolHandle {
    /// Submit a job to any free worker; blocks for the reply.
    pub fn send(&self, job: UiaJob) -> Result<(), UiaPoolError> {
        let (tx, rx) = mpsc::channel();
        self.sender
            .as_ref()
            .ok_or(UiaPoolError::ShutDown)?
            .send((job, tx))
            .map_err(|_| UiaPoolError::ShutDown)?;
        rx.recv().map_err(|_| UiaPoolError::ShutDown)?
    }
}

impl Drop for UiaPoolHandle {
    fn drop(&mut self) {
        // Closing the sender drops the channel; each worker exits its recv loop.
        self.sender.take();
        let mut workers = self.workers.lock().unwrap();
        for j in workers.drain(..) {
            let _ = j.join();
        }
    }
}

/// Spawn `n` MTA UIA workers (default 3).
pub fn spawn_uia_pool(n: usize) -> std::io::Result<UiaPoolHandle> {
    let n = if n == 0 { 3 } else { n };
    let (tx, rx) = mpsc::channel::<(UiaJob, UiaReply)>();
    let rx = Arc::new(Mutex::new(rx));

    let mut workers = Vec::with_capacity(n);
    for i in 0..n {
        let rx = Arc::clone(&rx);
        let join = thread::Builder::new()
            .name(format!("fastuse-uia-{i}"))
            .spawn(move || {
                // SAFETY: MTA per-thread init; matched by CoUninitialize on exit.
                let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };

                // Force-init the singleton on first job (lazy CoCreate).
                loop {
                    let job = {
                        let lock = rx.lock().expect("uia rx mutex poisoned");
                        match lock.recv() {
                            Ok(j) => j,
                            Err(_) => break, // channel closed -> shutdown
                        }
                    };
                    let (job, reply) = job;
                    let res = match job {
                        UiaJob::Noop => match get_or_init_uia() {
                            Ok(_) => Ok(()),
                            Err(e) => Err(e),
                        },
                    };
                    let _ = reply.send(res);
                }

                // SAFETY: paired with CoInitializeEx above.
                unsafe { CoUninitialize() };
            })?;
        workers.push(join);
    }

    Ok(UiaPoolHandle {
        sender: Some(tx),
        workers: Mutex::new(workers),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uia_pool_acks_noop_and_joins() {
        let h = spawn_uia_pool(3).expect("spawn uia pool");
        for _ in 0..5 {
            h.send(UiaJob::Noop).expect("noop ack");
        }
        let start = std::time::Instant::now();
        drop(h);
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
    }
}
