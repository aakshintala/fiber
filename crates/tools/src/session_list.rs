//! `session_list` (`docs/tools.md`, "Messaging other sessions"): prints
//! summaries supplied by the injected socket client, holding no session state.

use std::sync::Arc;

use contract::SessionId;
use contract::emit::Emit;
use contract::provider::ToolDefinition;
use contract::sessions::{ListError, Sessions};
use contract::shapes::Effect;
use contract::tool::{Cancel, Effects, EffectsError, Output, Tool};
use serde_json::{Map, Value, json};

use crate::tool_util::{effects, errored, text_output};

/// Lists the account's running sessions, including the calling session.
pub struct SessionList {
    sessions: Arc<dyn Sessions>,
    own: SessionId,
}

impl SessionList {
    /// Uses the socket client and the calling session's id.
    pub fn new(sessions: Arc<dyn Sessions>, own: SessionId) -> Self {
        Self { sessions, own }
    }
}

impl Tool for SessionList {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "session_list".to_owned(),
            description: "Lists every running Fiber session of this account: this one, other projects' sessions, delegates and their siblings. Each entry gives the session's id, name and workspace, and its parent's id when it is a delegate; this session's entry has \"self\": true. A running session that did not report in time is listed by id with \"answering\": false.".into(),
            input_schema: json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, _arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Ok(effects(
            vec![Effect::Reads],
            true,
            Some(vec![self.sessions.scope().to_string_lossy().into_owned()]),
            Some(String::new()),
            None,
        ))
    }

    fn run(
        &self,
        _arguments: &Map<String, Value>,
        cancel: &dyn Cancel,
        _emit: &dyn Emit,
    ) -> Output {
        let result = self.sessions.list(cancel);
        if cancel.is_cancelled() {
            return text_output("Cancelled before it finished.".into());
        }
        let running = match result {
            Ok(running) => running,
            Err(ListError::Cancelled) => {
                return text_output("Cancelled before it finished.".into());
            }
            Err(ListError::Io { path, message }) => {
                return errored(crate::error::SessionListError::Io { path, message });
            }
        };
        let entries: Vec<_> = running
            .into_iter()
            .map(|running| {
                let mut entry = Map::from_iter([
                    ("id".into(), json!(running.id)),
                    ("answering".into(), running.status.is_some().into()),
                ]);
                if let Some(status) = running.status {
                    entry.insert("name".into(), status.name.into());
                    entry.insert("workspace".into(), status.workspace.into());
                    if let Some(parent) = status.parent {
                        entry.insert("parent".into(), json!(parent));
                    }
                }
                if running.id == self.own {
                    entry.insert("self".into(), true.into());
                }
                Value::Object(entry)
            })
            .collect();
        text_output(Value::Array(entries).to_string())
    }
}

#[cfg(test)]
#[path = "session_list_tests.rs"]
mod tests;
