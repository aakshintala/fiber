//! The explicit callback context every admitted host call carries, shared
//! by the Lua and process adapters (`docs/extensions.md`, "Host calls").
//!
//! A call made while answering a request runs under that request's context,
//! with every rule a Lua call in that callback follows: which calls it may
//! make, what `ran_by` names, the timeout it counts against, whether a
//! state write goes with a hook's change, and what is cancelled when the
//! callback ends. A call with no `request_id` before `ready` runs while the
//! entry script runs; one after `ready` runs from a timer with no timeout
//! of its own.

use std::time::Instant;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use contract::{ErrorCode, RequestId};

/// Where an admitted call runs.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(crate) enum CallbackKind {
    /// While the entry script runs, before `ready`.
    Entry,
    /// Inside the callback answering `request_id`.
    Request {
        /// The request answered.
        request_id: RequestId,
        /// What the callback runs as, naming the call's attribution.
        ran_by: String,
    },
    /// From a timer, after `ready` with no `request_id`: no timeout of its
    /// own.
    Timer,
}

/// The explicit context every admitted host call carries, whatever backend
/// runs the callback. No registry, state or emitter lock is held over pipe
/// I/O or callback waits; the context owns the deadline and the cancel
/// signal the wait reads instead.
#[derive(Clone)]
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(crate) struct CallbackContext {
    /// The extension making the call.
    pub extension: String,
    /// Where the call runs.
    pub kind: CallbackKind,
    /// The callback's end on the wall clock, when it has one.
    pub deadline: Option<Instant>,
    /// Fired when the callback ends for any other reason: the request is
    /// answered, times out, is cancelled, or its generation retires.
    pub cancel: support::cancel::Token,
    /// The hook state transaction the call's writes join, when the call
    /// runs inside a hook callback.
    pub state_call: Option<u64>,
}

#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
impl CallbackContext {
    /// A call made while the entry script runs, before `ready`.
    pub(crate) fn entry(extension: &str) -> Self {
        Self {
            extension: extension.to_owned(),
            kind: CallbackKind::Entry,
            deadline: None,
            cancel: support::cancel::Token::new(),
            state_call: None,
        }
    }

    /// A call made from a timer, after `ready` with no `request_id`.
    pub(crate) fn timer(extension: &str) -> Self {
        Self {
            extension: extension.to_owned(),
            kind: CallbackKind::Timer,
            deadline: None,
            cancel: support::cancel::Token::new(),
            state_call: None,
        }
    }

    /// A call made while answering `request_id` as `ran_by`, ending at
    /// `deadline` and joining hook transaction `state_call`, when the
    /// request is a hook delivery.
    pub(crate) fn request(
        extension: &str,
        request_id: RequestId,
        ran_by: String,
        deadline: Option<Instant>,
        cancel: support::cancel::Token,
        state_call: Option<u64>,
    ) -> Self {
        Self {
            extension: extension.to_owned(),
            kind: CallbackKind::Request { request_id, ran_by },
            deadline,
            cancel,
            state_call,
        }
    }

    /// Whether a registration call may run here: only while the entry
    /// script runs, as in Lua.
    pub(crate) fn registration_allowed(&self) -> bool {
        matches!(self.kind, CallbackKind::Entry)
    }

    /// The request answered, when the call runs inside a callback.
    pub(crate) fn request_id(&self) -> Option<&RequestId> {
        match &self.kind {
            CallbackKind::Request { request_id, .. } => Some(request_id),
            CallbackKind::Entry | CallbackKind::Timer => None,
        }
    }

    /// What the callback runs as, when it answers a request.
    pub(crate) fn ran_by(&self) -> Option<&str> {
        match &self.kind {
            CallbackKind::Request { ran_by, .. } => Some(ran_by),
            CallbackKind::Entry | CallbackKind::Timer => None,
        }
    }
}

#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
impl std::fmt::Debug for CallbackContext {
    /// Names the extension, the kind, the deadline and the hook
    /// transaction: the cancel signal has no `Debug`, and neither prints.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallbackContext")
            .field("extension", &self.extension)
            .field("kind", &self.kind)
            .field("deadline", &self.deadline)
            .field("state_call", &self.state_call)
            .finish_non_exhaustive()
    }
}

/// Decodes a bytes position carried as standard base64, both ways. Invalid
/// base64 is rejected `invalid_arguments`, naming the call and the
/// argument.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(crate) fn decode_bytes(
    call: &str,
    arg: &str,
    value: &str,
) -> Result<Vec<u8>, (ErrorCode, String)> {
    STANDARD.decode(value).map_err(|_| {
        (
            ErrorCode::InvalidArguments,
            format!("{call}: `{arg}` is not valid base64"),
        )
    })
}

/// Encodes bytes for a base64 position, standard base64.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(crate) fn encode_bytes(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

/// Whether `len` bytes fit under the framing cap: a result that does not
/// is answered `command_rejected` `too_large` in place of the result.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(crate) fn fits_framing(len: usize, max: usize) -> bool {
    len <= max
}
