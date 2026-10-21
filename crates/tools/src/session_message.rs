//! `session_message` (`docs/tools.md`, "Messaging other sessions"): sends a
//! plain-text message to one running session, answered `delivered` once the
//! target takes it, after its `before_message` hook ran.

use std::sync::Arc;

use contract::SessionId;
use contract::emit::Emit;
use contract::provider::ToolDefinition;
use contract::sessions::{MessageError, Sessions};
use contract::shapes::Effect;
use contract::tool::{Cancel, Effects, EffectsError, Output, Tool};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::error::SessionMessageError;
use crate::tool_util::{effects, errored, text_output};

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
    parent_of: Arc<dyn Fn() -> Option<SessionId> + Send + Sync>,
}

impl SessionMessage {
    /// Uses the socket client and the calling session's id; `parent_of`
    /// reads the calling session's parent for the target's hook.
    pub fn new(
        sessions: Arc<dyn Sessions>,
        own: SessionId,
        parent_of: Arc<dyn Fn() -> Option<SessionId> + Send + Sync>,
    ) -> Self {
        Self {
            sessions,
            own,
            parent_of,
        }
    }
}

impl Tool for SessionMessage {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "session_message".to_owned(),
            description: "Sends a plain-text message to one running Fiber session by its full id from session_list. It arrives there as steering during a turn, or starts a turn. Answers \"delivered\" once the target takes it; fails unreachable, closing, message_refused, or hook_failed.".into(),
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
        let args: Args = match crate::tool_util::parsed(arguments) {
            Ok(args) => args,
            Err(error) => return errored(SessionMessageError::from(error)),
        };
        let to = SessionId(args.id.clone());
        let parent = (self.parent_of)();
        let result = self
            .sessions
            .message(&to, &self.own, parent.as_ref(), &args.text, cancel);
        // The call claims nothing about delivery when it is cancelled: the
        // target may hold the message while the call reports the cancel.
        if cancel.is_cancelled() {
            return text_output("Cancelled before it finished.".into());
        }
        match result {
            Ok(()) => text_output("delivered".into()),
            Err(MessageError::Unreachable) => {
                errored(SessionMessageError::Unreachable { id: to.clone() })
            }
            Err(MessageError::Rejected(failure)) => errored(SessionMessageError::Rejected(failure)),
            Err(MessageError::Io { message, .. }) => errored(SessionMessageError::Io {
                id: to.clone(),
                message,
            }),
            Err(MessageError::Cancelled) => text_output("Cancelled before it finished.".into()),
        }
    }
}

#[cfg(test)]
#[path = "session_message_tests.rs"]
mod tests;
