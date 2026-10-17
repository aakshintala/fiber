//! The session's routing and credentials behind `host.model`
//! (`docs/extensions.md`, "Host calls"): one model request through the
//! session's provider with the session's retry policy, serialized per
//! session by a budget gate. Red: the provider call is not wired yet; every
//! test fails here until the green commit lands it.

use std::path::PathBuf;
use std::sync::Arc;

use contract::clock::Clock;
use contract::model::{Bound, HostModel, HostModelReply, HostModelRequest};
use contract::provider::Provider;
use contract::shapes::Failure;
use contract::{ErrorCode, SessionId};

/// One model request through the session's routing and credentials, made by
/// an extension's `host.model` call.
pub(crate) struct ModelHost {
    /// The session's connected provider, with its credential already read.
    provider: Arc<dyn Provider>,
    /// The session's model: its reference names the session's provider.
    model: r#loop::Model,
    /// `budget.usd`, or none when the key is absent or not a number.
    budget: Option<f64>,
    /// How a failed model call is retried.
    retry: r#loop::Retry,
    /// The session directory, whose log the spend folds.
    dir: PathBuf,
    /// The session's id, naming the request's cache key.
    id: SessionId,
    /// The session clock the waits read.
    clock: Arc<dyn Clock>,
}

impl ModelHost {
    /// Builds the host from the session's parts, where `drive_to` stands.
    #[allow(
        clippy::too_many_arguments,
        reason = "one construction from the session's parts, beside drive_to"
    )]
    pub(crate) fn new(
        provider: Arc<dyn Provider>,
        model: r#loop::Model,
        budget: Option<f64>,
        retry: r#loop::Retry,
        dir: PathBuf,
        id: SessionId,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            provider,
            model,
            budget,
            retry,
            dir,
            id,
            clock,
        }
    }
}

impl HostModel for ModelHost {
    fn call(
        &self,
        _extension: &str,
        _request: HostModelRequest,
        _bound: Bound,
    ) -> Result<HostModelReply, Failure> {
        // Red: Task 4 wires the provider call here.
        Err(Failure {
            code: ErrorCode::Other("model_host_stub".to_owned()),
            message: "Task 4 red: the provider call is not wired yet".to_owned(),
            retry_after_ms: None,
            provider: None,
        })
    }
}
