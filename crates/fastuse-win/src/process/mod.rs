//! Process enumeration and termination.
//!
//! `list_processes` joins `EnumProcesses` results to top-level windows so
//! agents can correlate Phase 2's `list_windows()` PIDs with `ProcessInfo`.
//!
//! `kill_process` enforces the daemon-self-PID protection (refuses to
//! terminate the daemon itself) and the deny-list (refuses to kill 2FA /
//! banking / wallet apps). The protection is documented prominently here
//! per the Phase 4 CONTEXT decision.

pub mod enum_proc;
pub mod kill;

pub use enum_proc::list_processes;
pub use kill::kill_process;
