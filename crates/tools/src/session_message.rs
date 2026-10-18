//! `session_message` (`docs/tools.md`, "Messaging other sessions"): sends a
//! plain-text message to one running session, answered `delivered` once it
//! is in the target's inbox.

use std::sync::Arc;

use contract::emit::Emit;
use contract::provider::ToolDefinition;
use contract::sessions::{MessageError, Sessions};
use contract::shapes::Effect;
use contract::tool::{Cancel, Effects, EffectsError, Output, Tool};
use contract::{ErrorCode, SessionId};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::tool_util::{effects, failed, text_output};

/// The call's arguments, checked against the schema before the call runs.
#[derive(Debug, Deserialize)]
struct Args {
    id: String,
    text: String,
}

/// Sends a message to one running session, addressed by its full id.
pub struct SessionMessage {
    sessions: Arc<dyn Sessions>,
    own: SessionId,
}

impl SessionMessage {
    /// Uses the socket client and the calling session's id.
    pub fn new(sessions: Arc<dyn Sessions>, own: SessionId) -> Self {
        Self { sessions, own }
    }
}

impl Tool for SessionMessage {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "session_message".to_owned(),
            description: "Sends a plain-text message to one running Fiber session by its full id from session_list. It arrives there as steering during a turn, or starts a turn. Answers \"delivered\" once it is in that session's inbox; fails unreachable when no running session has the id, and closing when the target is closing.".into(),
            input_schema: json!({"type":"object","properties":{"id":{"type":"string","description":"The target session's full id, as session_list gives it."},"text":{"type":"string","description":"The message, plain text."}},"required":["id","text"],"additionalProperties":false}),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        // A socket path is never declared: declaring `run/<id>` would take
        // the workspace fast path when Fiber home sits inside the workspace
        // and outside `.fiber/`, and the call must reach the reviewer.
        let _args: Args =
            crate::tool_util::arguments(arguments).map_err(EffectsError::Arguments)?;
        Ok(effects(
            vec![Effect::Writes],
            false,
            None,
            Some(String::new()),
            None,
        ))
    }

    fn run(&self, arguments: &Map<String, Value>, cancel: &dyn Cancel, _emit: &dyn Emit) -> Output {
        let args: Args = match crate::tool_util::arguments(arguments) {
            Ok(args) => args,
            Err(message) => return failed(ErrorCode::InvalidArguments, message),
        };
        let to = SessionId(args.id.clone());
        let result = self.sessions.message(&to, &self.own, &args.text, cancel);
        // The call claims nothing about delivery when it is cancelled: the
        // target may hold the message while the call reports the cancel.
        if cancel.is_cancelled() {
            return text_output("Cancelled before it finished.".into());
        }
        match result {
            Ok(()) => text_output("delivered".into()),
            Err(MessageError::Unreachable) => failed(
                ErrorCode::Unreachable,
                format!("No running session has the id {}.", to.0),
            ),
            Err(MessageError::Rejected(failure)) => failed(failure.code, failure.message),
            Err(MessageError::Io { message, .. }) => failed(
                ErrorCode::ToolError,
                format!("Could not message session {}: {message}", to.0),
            ),
            Err(MessageError::Cancelled) => text_output("Cancelled before it finished.".into()),
        }
    }
}

#[cfg(test)]
#[path = "session_message_tests.rs"]
mod tests;
