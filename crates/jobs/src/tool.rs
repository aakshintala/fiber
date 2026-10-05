//! The `jobs` tool: `list`, `wait` and `stop`
//! (`docs/tools.md`, "Background jobs").

use std::sync::Arc;

use contract::ErrorCode;
use contract::emit::Emit;
use contract::jobs::JobRecord;
use contract::provider::ToolDefinition;
use contract::shapes::{ContentPart, DeclaredEffects, Effect, Failure};
use contract::tool::{Cancel, Effects, EffectsError, Output, Tool};
use serde_json::{Map, Value, json};

use crate::registry::{self, Registry, StopError};

/// Lists, waits on and stops the jobs the calling session started.
pub struct JobsTool {
    registry: Arc<Registry>,
}

impl JobsTool {
    /// The tool over `registry`.
    pub fn new(registry: Arc<Registry>) -> Self {
        Self { registry }
    }
}

impl Tool for JobsTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "jobs".to_owned(),
            description: "Lists, waits on and stops this session's background jobs. `list` shows \
                 each job. `wait` blocks until a job ends or `timeout_ms` passes; cancelling the \
                 wait leaves the job running. `stop` stops one job."
                .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["list", "wait", "stop"],
                        "description": "`list` shows the session's jobs, `wait` blocks until a job ends or the timeout, and `stop` asks a job to stop."
                    },
                    "job_id": {
                        "type": "string",
                        "description": "The job. Required by `wait` and `stop`."
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "How long `wait` blocks, in milliseconds. Required by `wait`."
                    }
                },
                "required": ["action"],
                "additionalProperties": false
            }),
            deferred: false,
        }
    }

    fn effects(&self, arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        let action = action(arguments).map_err(EffectsError::Arguments)?;
        Ok(effects_of(action))
    }

    fn run(&self, arguments: &Map<String, Value>, cancel: &dyn Cancel, _emit: &dyn Emit) -> Output {
        let action = match action(arguments) {
            Ok(action) => action,
            Err(message) => return failed(message),
        };
        match action {
            Action::List => self.list(arguments),
            Action::Wait => self.wait(arguments, cancel),
            Action::Stop => self.stop(arguments, cancel),
        }
    }
}

enum Action {
    List,
    Wait,
    Stop,
}

fn action(arguments: &Map<String, Value>) -> Result<Action, String> {
    match arguments.get("action").and_then(Value::as_str) {
        Some("list") => Ok(Action::List),
        Some("wait") => Ok(Action::Wait),
        Some("stop") => Ok(Action::Stop),
        Some(_) | None => Err("`action` must be `list`, `wait` or `stop`.".to_owned()),
    }
}

fn effects_of(action: Action) -> Effects {
    let (effects, reversible) = match action {
        Action::List | Action::Wait => (vec![Effect::Reads], true),
        Action::Stop => (Vec::new(), false),
    };
    Effects {
        declared: DeclaredEffects {
            effects,
            reversible,
            paths: None,
        },
        subject: Some(String::new()),
        prefix: None,
    }
}

impl JobsTool {
    fn list(&self, arguments: &Map<String, Value>) -> Output {
        if let Some(message) = unknown_if_named(&self.registry, arguments) {
            return failed(message);
        }
        text_only(self.registry.list_text())
    }

    fn wait(&self, arguments: &Map<String, Value>, cancel: &dyn Cancel) -> Output {
        let id = match job_id(arguments) {
            Ok(id) => id,
            Err(message) => return failed(message),
        };
        let timeout_ms = match timeout_ms(arguments) {
            Ok(timeout_ms) => timeout_ms,
            Err(message) => return failed(message),
        };
        match self.registry.wait(&id, timeout_ms, cancel) {
            Ok(answer) => answered(answer.text, answer.record),
            Err(()) => failed(unknown_message(&id)),
        }
    }

    fn stop(&self, arguments: &Map<String, Value>, cancel: &dyn Cancel) -> Output {
        let id = match job_id(arguments) {
            Ok(id) => id,
            Err(message) => return failed(message),
        };
        match self.registry.stop(&id, cancel) {
            Ok(answer) => answered(answer.text, answer.record),
            Err(StopError::Unknown) => failed(unknown_message(&id)),
            Err(StopError::Ended(status)) => failed(format!(
                "Job `{id}` has ended: {}.",
                registry::status_word_of(status)
            )),
        }
    }
}

/// `list` fails when it names a job the session does not have. With no
/// `job_id` it lists every job.
fn unknown_if_named(registry: &Registry, arguments: &Map<String, Value>) -> Option<String> {
    match arguments.get("job_id") {
        None => None,
        Some(Value::String(id)) if registry.contains(id) => None,
        Some(Value::String(id)) => Some(unknown_message(id)),
        Some(_) => Some("`job_id` must be a string.".to_owned()),
    }
}

fn job_id(arguments: &Map<String, Value>) -> Result<String, String> {
    match arguments.get("job_id") {
        Some(Value::String(id)) => Ok(id.clone()),
        Some(_) => Err("`job_id` must be a string.".to_owned()),
        None => Err("Give the job as `job_id`.".to_owned()),
    }
}

fn timeout_ms(arguments: &Map<String, Value>) -> Result<u64, String> {
    match arguments.get("timeout_ms") {
        None => Err("Give how long to wait as `timeout_ms`.".to_owned()),
        Some(Value::Number(number)) => number
            .as_u64()
            .ok_or_else(|| "`timeout_ms` must be an integer of 0 or more.".to_owned()),
        Some(_) => Err("`timeout_ms` must be an integer of 0 or more.".to_owned()),
    }
}

fn unknown_message(id: &str) -> String {
    format!("No job has id `{id}`.")
}

fn text_only(text: String) -> Output {
    Output {
        content: vec![ContentPart::Text { text }],
        ..Output::default()
    }
}

fn answered(text: String, record: Option<JobRecord>) -> Output {
    Output {
        content: vec![ContentPart::Text { text }],
        jobs: record.into_iter().collect(),
        ..Output::default()
    }
}

fn failed(message: String) -> Output {
    Output {
        content: vec![ContentPart::Text {
            text: format!("{message}\n"),
        }],
        error: Some(Failure {
            code: ErrorCode::InvalidArguments,
            message,
            retry_after: None,
            provider: None,
        }),
        ..Output::default()
    }
}

#[cfg(test)]
#[path = "tool_tests.rs"]
mod tests;
