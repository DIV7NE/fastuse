//! MTA UIA pool (D-26).
//!
//! Spawns N MTA worker threads. Each initializes (lazily) a shared
//! `OnceLock<UIAutomation>` (Phase 3 Task 08 promotes this to the canonical
//! singleton accessor). Phase 3 promotes the surface from the Phase 1
//! `Noop`-only enum to a closure-injection shape so UIA handlers (walk,
//! query, inspect, click_element, type_into_element, scroll_into_view,
//! wait_for_element) can run arbitrary closures with `&UIAutomation`
//! injected, without proliferating enum variants.

use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};

use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

use uiautomation::UIAutomation;

/// UIA job sent to a worker.
///
/// `Run` carries a closure that receives `&UIAutomation` (the singleton
/// IUIAutomation wrapper). This is the canonical Phase 3 dispatch surface.
/// Reply payload is `serde_json::Value` to keep the channel mono-typed.
pub enum UiaJob {
    /// No-op probe (Phase 1 liveness).
    Noop,
    /// Phase 3 closure-dispatch slot — receives the UIA singleton.
    Run(
        Box<
            dyn FnOnce(&UIAutomation) -> Result<serde_json::Value, fastuse_proto::Error>
                + Send,
        >,
    ),
}

impl std::fmt::Debug for UiaJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UiaJob::Noop => write!(f, "UiaJob::Noop"),
            UiaJob::Run(_) => write!(f, "UiaJob::Run(<fn>)"),
        }
    }
}

/// Reply payload from the UIA pool.
pub enum UiaReplyPayload {
    /// Plain ack (Noop).
    Ack(Result<(), UiaPoolError>),
    /// Result of a `Run` closure.
    Run(Result<serde_json::Value, fastuse_proto::Error>),
}

/// UIA worker reply channel.
pub type UiaReply = Sender<UiaReplyPayload>;

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
    /// Submit a Noop job to any free worker; blocks for the reply
    /// (Phase 1 surface).
    pub fn send(&self, job: UiaJob) -> Result<(), UiaPoolError> {
        let (tx, rx) = mpsc::channel();
        self.sender
            .as_ref()
            .ok_or(UiaPoolError::ShutDown)?
            .send((job, tx))
            .map_err(|_| UiaPoolError::ShutDown)?;
        match rx.recv().map_err(|_| UiaPoolError::ShutDown)? {
            UiaReplyPayload::Ack(r) => r,
            UiaReplyPayload::Run(_) => Ok(()),
        }
    }

    /// Phase 3: dispatch a closure onto a UIA pool worker. The closure
    /// receives `&UIAutomation` (the singleton). Reply is decoded from
    /// JSON back to `T`. Errors propagate as `fastuse_proto::Error`.
    pub fn run<F, T>(&self, f: F) -> Result<T, fastuse_proto::Error>
    where
        F: FnOnce(&UIAutomation) -> Result<T, fastuse_proto::Error> + Send + 'static,
        T: serde::Serialize + serde::de::DeserializeOwned,
    {
        let job = UiaJob::Run(Box::new(move |a: &UIAutomation| {
            f(a).and_then(|v| {
                serde_json::to_value(&v).map_err(|e| {
                    fastuse_proto::Error::new(
                        fastuse_proto::ErrorCode::Internal,
                        format!("encode uia reply: {e}"),
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
                    "uia pool shut down".to_string(),
                )
            })?
            .send((job, tx))
            .map_err(|_| {
                fastuse_proto::Error::new(
                    fastuse_proto::ErrorCode::DaemonDead,
                    "uia pool shut down".to_string(),
                )
            })?;
        let reply = rx.recv().map_err(|_| {
            fastuse_proto::Error::new(
                fastuse_proto::ErrorCode::DaemonDead,
                "uia pool shut down".to_string(),
            )
        })?;
        match reply {
            UiaReplyPayload::Run(r) => {
                let v = r?;
                serde_json::from_value(v).map_err(|e| {
                    fastuse_proto::Error::new(
                        fastuse_proto::ErrorCode::Internal,
                        format!("decode uia reply: {e}"),
                    )
                })
            }
            UiaReplyPayload::Ack(_) => Err(fastuse_proto::Error::new(
                fastuse_proto::ErrorCode::Internal,
                "expected Run reply, got Ack".to_string(),
            )),
        }
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

                loop {
                    let job = {
                        let lock = rx.lock().expect("uia rx mutex poisoned");
                        match lock.recv() {
                            Ok(j) => j,
                            Err(_) => break, // channel closed -> shutdown
                        }
                    };
                    let (job, reply) = job;
                    let payload = match job {
                        UiaJob::Noop => match get_or_init_uia() {
                            Ok(_) => UiaReplyPayload::Ack(Ok(())),
                            Err(e) => UiaReplyPayload::Ack(Err(e)),
                        },
                        UiaJob::Run(f) => match get_or_init_uia() {
                            Ok(uia) => UiaReplyPayload::Run(f(&uia.0)),
                            Err(e) => UiaReplyPayload::Run(Err(fastuse_proto::Error::new(
                                fastuse_proto::ErrorCode::Internal,
                                format!("uia init failed: {e}"),
                            ))),
                        },
                    };
                    let _ = reply.send(payload);
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

    #[test]
    fn uia_pool_runs_closure_with_singleton() {
        let h = spawn_uia_pool(2).expect("spawn uia pool");
        // Ensure the closure receives a usable `&UIAutomation`. We don't
        // touch UIA tree (which would require a real desktop session); just
        // confirm the singleton is provided and the closure round-trips a
        // simple value.
        let v: u64 = h
            .run(|_uia: &UIAutomation| Ok::<u64, fastuse_proto::Error>(7))
            .expect("closure runs");
        assert_eq!(v, 7);
    }
}
