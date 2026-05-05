//! Orchestrate profile → strategy → hit-test → execute → verify pipeline.
//! Real implementation in Tasks 11+12.

use fastuse_proto::selector::Selector;
use fastuse_proto::wire::Response;

/// What the dispatcher passes to `targeting::execute`.
pub struct TargetedRequest<'a> {
    /// Selector to resolve.
    pub selector: &'a Selector,
    /// Optional caller-supplied modifiers.
    pub modifiers: Option<&'a [String]>,
    /// Optional ActionOpts (wait_for / expect / escalate / etc).
    pub opts: Option<&'a fastuse_proto::wire::ActionOpts>,
}

/// Run the full pipeline. Returns `Response::ActionResult` on completion or
/// `Response::Error` on resolution failure.
pub async fn execute_targeted<'a>(_req: TargetedRequest<'a>) -> Response {
    // Task 11/12 implements.
    Response::Error(fastuse_proto::error::Error::new(
        fastuse_proto::error::ErrorCode::Internal,
        "targeting::execute_targeted not yet implemented",
    ))
}
