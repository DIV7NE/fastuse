//! Per-action postcondition contracts + 25ms-cadence/250ms-cap polling helper.
//! Real implementation in Task 7.

use fastuse_proto::wire::VerificationEvidence;

/// Outcome of a verification poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifyOutcome {
    /// Whether the postcondition matched within the cap.
    pub matched: bool,
    /// Wall-clock spent polling (ms).
    pub waited_ms: u32,
    /// Tag describing what evidence we gathered.
    pub evidence: VerificationEvidence,
}

/// Poll a closure at 25ms cadence until it returns `true` or `cap_ms` elapses.
pub async fn poll_until<F>(_cap_ms: u32, _check: F) -> VerifyOutcome
where
    F: FnMut() -> bool + Send,
{
    // Task 7 implements.
    VerifyOutcome {
        matched: false,
        waited_ms: 0,
        evidence: VerificationEvidence::Unverified,
    }
}
