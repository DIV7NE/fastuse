//! Strategy picker. Pattern availability dominates ControlType.
//! Real implementation in Task 5.

use fastuse_proto::wire::Strategy;

use crate::targeting::candidate::TargetCandidate;

/// Pick the first-choice strategy for a candidate based on what it actually supports.
pub fn pick_strategy(_c: &TargetCandidate) -> Strategy {
    // Task 5 implements the full table.
    Strategy::UiaInvoke
}
