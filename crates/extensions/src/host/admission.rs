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

use crate::lua::CredentialFor;

/// Where an admitted call runs.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
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
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
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
    /// What a `host.oauth.refresh` in this callback refreshes: a stored
    /// pair's file, or a login's result slot. Only the dispatcher that
    /// runs a provider's `credential()` sets it, carrying the session's
    /// pair; every other callback holds no provider credential.
    pub credential: Option<CredentialFor>,
}

#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
impl CallbackContext {
    /// A call made while the entry script runs, before `ready`.
    pub(crate) fn entry(extension: &str) -> Self {
        Self {
            extension: extension.to_owned(),
            kind: CallbackKind::Entry,
            deadline: None,
            cancel: support::cancel::Token::new(),
            state_call: None,
            credential: None,
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
            credential: None,
        }
    }

    /// A call made while answering `request_id` as `ran_by`, ending at
    /// `deadline` and joining hook transaction `state_call`, when the
    /// request is a hook delivery. `credential` is what a
    /// `host.oauth.refresh` in this callback refreshes, set only by the
    /// dispatcher that runs a provider's `credential()`.
    pub(crate) fn request(
        extension: &str,
        request_id: RequestId,
        ran_by: String,
        deadline: Option<Instant>,
        cancel: support::cancel::Token,
        state_call: Option<u64>,
        credential: Option<CredentialFor>,
    ) -> Self {
        Self {
            extension: extension.to_owned(),
            kind: CallbackKind::Request { request_id, ran_by },
            deadline,
            cancel,
            state_call,
            credential,
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

    /// The inner call `host.tool` may run under here, if any.
    pub(crate) fn tool_anchor(&self) -> Option<ToolAnchor> {
        tool_anchor(self.ran_by())
    }

    /// Whether `host.model` may run in this callback.
    pub(crate) fn model_permitted(&self) -> bool {
        model_permitted(self.ran_by())
    }

    /// Whether `host.delegate` may run in this callback.
    pub(crate) fn delegate_permitted(&self) -> bool {
        delegate_permitted(self.ran_by())
    }

    /// Whether a finished `host.exec` run here is logged.
    pub(crate) fn exec_logged(&self) -> bool {
        exec_logged(self.ran_by())
    }
}

#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
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

/// What `host.tool` may run under: a tool or command callback anchors
/// an inner call. Every other callback, and no callback at all, refuses
/// `host.tool` with the Lua sentence, starting no tool work.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(crate) enum ToolAnchor {
    /// Inside a tool request, judged by that tool's `executes`.
    Tool(String),
    /// Inside a command request, which anchors an inner call as a
    /// tool's does.
    Command(String),
}

/// The inner call `host.tool` may run under: a tool or command request
/// anchors it. The `ran_by` is the request's, or `None` outside one.
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(crate) fn tool_anchor(ran_by: Option<&str>) -> Option<ToolAnchor> {
    let ran_by = ran_by?;
    if let Some(name) = ran_by.strip_prefix("tool ") {
        // "tool {name} effects" is the effects function, not the run:
        // only a bare tool name anchors.
        if !name.contains(' ') {
            return Some(ToolAnchor::Tool(name.to_owned()));
        }
        return None;
    }
    if let Some(name) = ran_by.strip_prefix("command ") {
        if !name.contains(' ') {
            return Some(ToolAnchor::Command(name.to_owned()));
        }
        return None;
    }
    None
}

/// Whether `host.model` may run under `ran_by`: everywhere but an
/// effects, provider or search-backend request, as in Lua. `None` is a
/// timer, which may call the session's model.
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(crate) fn model_permitted(ran_by: Option<&str>) -> bool {
    let Some(ran_by) = ran_by else {
        return true;
    };
    if ran_by.ends_with(" effects") {
        return false;
    }
    if ran_by.starts_with("provider ") || ran_by.starts_with("search ") {
        return false;
    }
    true
}

/// Whether `host.delegate` may run under `ran_by`: everywhere but a
/// provider, watcher, search-backend or effects request, as in Lua.
/// `None` is a timer, which may start a delegate.
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(crate) fn delegate_permitted(ran_by: Option<&str>) -> bool {
    let Some(ran_by) = ran_by else {
        return true;
    };
    if ran_by.ends_with(" effects") {
        return false;
    }
    if ran_by.starts_with("provider ")
        || ran_by.starts_with("watcher ")
        || ran_by.starts_with("search ")
    {
        return false;
    }
    true
}

/// Whether a finished `host.exec` run is logged as `extension_exec`:
/// every run but one inside a tool request, whose effects the tool's
/// own run already judged.
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(crate) fn exec_logged(ran_by: Option<&str>) -> bool {
    !matches!(tool_anchor(ran_by), Some(ToolAnchor::Tool(_)))
}

/// The entry-script refusal for `call`: the sentence and code the Lua
/// half raises before it yields. `None` runs in the entry script as it
/// does anywhere else.
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(crate) fn entry_refusal(call: &str) -> Option<(ErrorCode, String)> {
    let string = |message: &str| Some((ErrorCode::InvalidArguments, message.to_owned()));
    let closing = |message: &str| Some((ErrorCode::Closing, message.to_owned()));
    match call {
        "host.tool" => string("host.tool: not available while init.lua runs"),
        "host.exec" => string("host.exec: not available while init.lua runs"),
        "host.model" => closing("host.model: not available while init.lua runs"),
        "host.delegate" => closing("host.delegate: not available while init.lua runs"),
        "host.drive" => closing("host.drive: not available while init.lua runs"),
        "host.ask" => string("host.ask: not available while init.lua runs"),
        "host.oauth.callback" => {
            string("host.oauth.callback: needs a running callback, not the entry script")
        }
        "host.oauth.poll" => {
            string("host.oauth.poll: needs a running callback, not the entry script")
        }
        "host.oauth.refresh" => {
            string("host.oauth.refresh: needs a running callback, not the entry script")
        }
        _ => None,
    }
}

/// The lock failure `host.oauth.refresh` raises without a provider
/// credential: the sentence names the callback, as in Lua. `None` is a
/// timer, which holds no credential either.
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(crate) fn lock_arg_error(call: &str, ran_by: Option<&str>) -> String {
    let kind = match ran_by {
        None => "a timer",
        Some(ran_by) => {
            if ran_by.starts_with("tool ") {
                "a tool"
            } else if ran_by.starts_with("command ") {
                "a command"
            } else if ran_by.starts_with("hook ") {
                "a hook"
            } else if ran_by.starts_with("watcher ") {
                "a watcher"
            } else if ran_by.starts_with("search ") {
                "a search backend"
            } else if ran_by.starts_with("harness ") {
                "a harness function"
            } else if let Some(pair) = ran_by.strip_prefix("provider ") {
                // A provider function names its own pair, as in Lua.
                return format!(
                    "{call}: {pair} has no credential to refresh; only credential() refreshes"
                );
            } else {
                "a delegate run"
            }
        }
    };
    format!("{call}: {kind} has no provider credential to refresh")
}

/// Decodes a bytes position carried as standard base64, both ways. Invalid
/// base64 is rejected `invalid_arguments`, naming the call and the
/// argument.
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
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
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(crate) fn encode_bytes(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}
