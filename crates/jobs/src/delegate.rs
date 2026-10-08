//! A Fiber delegate: spawning the child session, watching it to its end,
//! and folding how it ended (`docs/delegates.md`).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use contract::emit::Emit;
use contract::events::DelegateStarted;
use contract::jobs::JobRecord;
use contract::provider::ToolDefinition;
use contract::shapes::{ContentPart, DeclaredEffects, Effect, Failure};
use contract::tool::{Cancel, Effects, EffectsError, Output, Tool};
use contract::{ErrorCode, SessionId};
use serde_json::{Map, Value, json};

pub(crate) mod group;
pub(crate) mod outcome;
pub(crate) mod run;

pub use group::kill_every_group;
pub use run::{Launch, Launched, Resolve, Watch, Watched};

use run::Runner;

use crate::registry::{Registry, mint_job_id};

/// Starts a Fiber delegate: a child `fiber session` that runs the prompt
/// as a job and exits when its run ends (`docs/delegates.md`, "The
/// tools"). The call returns as soon as the child has spawned and the job
/// is recorded; the runner thread owns everything after that.
///
/// The fields are wired by `main`: delegates of `parent` share `workspace`
/// and log under `sessions`; `bound` is the stop bound and `cap` the
/// output cap.
pub struct DelegateSpawn {
    /// The session's jobs.
    pub registry: Arc<Registry>,
    /// The parent session: the child's `--parent`.
    pub parent: SessionId,
    /// The parent's workspace, which the delegate shares.
    pub workspace: PathBuf,
    /// Where delegates log: each child's `events.jsonl` lives under
    /// `<sessions>/<session id>`.
    pub sessions: PathBuf,
    /// The session clock.
    pub clock: Arc<dyn contract::clock::Clock>,
    /// How long a stop waits before SIGKILL.
    pub bound: Duration,
    /// The output cap, in bytes of the delegate's `events.jsonl`.
    pub cap: u64,
    /// Resolves a `fiber:` reference, or lists the valid ones.
    pub resolve: Resolve,
    /// Builds the child's command.
    pub launch: Launch,
    /// Watches the child's socket.
    pub watch: Watch,
}

/// A `delegate_spawn` call's arguments, checked.
struct Arguments {
    description: String,
    prompt: String,
    model: String,
}

/// Reads the call's arguments: each of `description`, `prompt` and `model`
/// is required, and each is a string.
fn check_arguments(arguments: &Map<String, Value>) -> Result<Arguments, String> {
    let mut missing = Vec::new();
    for name in ["description", "prompt", "model"] {
        if !arguments.contains_key(name) {
            missing.push(name);
        }
    }
    if !missing.is_empty() {
        return Err(format!(
            "Give {} as {}.",
            missing.join(", "),
            if missing.len() == 1 {
                "a string"
            } else {
                "strings"
            }
        ));
    }
    let mut wrong = Vec::new();
    for name in ["description", "prompt", "model"] {
        if !arguments[name].is_string() {
            wrong.push(name);
        }
    }
    if !wrong.is_empty() {
        return Err(format!(
            "{} must be {}.",
            wrong.join(", "),
            if wrong.len() == 1 {
                "a string"
            } else {
                "strings"
            }
        ));
    }
    Ok(Arguments {
        description: arguments["description"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        prompt: arguments["prompt"].as_str().unwrap_or_default().to_owned(),
        model: arguments["model"].as_str().unwrap_or_default().to_owned(),
    })
}

impl Tool for DelegateSpawn {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "delegate_spawn".to_owned(),
            description: "Starts a Fiber delegate: a child session that runs \
                 `prompt` as a background job and exits when its run ends. \
                 Returns at once with the delegate's job id and log path; \
                 wait for it with `jobs wait`. `model` is a `fiber:` model \
                 reference."
                .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "description": {
                        "type": "string",
                        "description": "What the delegate does, shown in `jobs list`."
                    },
                    "prompt": {
                        "type": "string",
                        "description": "What the child session runs."
                    },
                    "model": {
                        "type": "string",
                        "description": "The delegate's model, as a `fiber:` reference."
                    }
                },
                "required": ["description", "prompt", "model"],
                "additionalProperties": false
            }),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        check_arguments(arguments).map_err(EffectsError::Arguments)?;
        Ok(Effects {
            declared: DeclaredEffects {
                effects: vec![Effect::Executes],
                reversible: false,
                paths: None,
            },
            // No subject: a rule matches this tool by name
            // (`docs/tools.md`, "What a tool declares").
            subject: Some(String::new()),
            prefix: None,
            // Always judged by the parent's reviewer: no fast path,
            // session grant or standing allow skips it
            // (`docs/delegates.md`, "Permissions").
            always_reviewed: true,
        })
    }

    fn run(
        &self,
        arguments: &Map<String, Value>,
        _cancel: &dyn Cancel,
        _emit: &dyn Emit,
    ) -> Output {
        let call = match check_arguments(arguments) {
            Ok(call) => call,
            Err(message) => return failed(ErrorCode::InvalidArguments, message),
        };
        // Checked when the call is made, before any process starts: an
        // invalid value fails and lists the valid references.
        let resolved = match (self.resolve)(&call.model) {
            Ok(resolved) => resolved,
            Err(valid) => {
                return failed(
                    ErrorCode::InvalidArguments,
                    format!(
                        "Unknown delegate model {:?}. Valid models:\n{}",
                        call.model,
                        valid.join("\n")
                    ),
                );
            }
        };
        let job_id = mint_job_id();
        let session_id = run::mint_session_id();
        let output_path = self
            .sessions
            .join(&session_id.0)
            .join("events.jsonl")
            .to_string_lossy()
            .into_owned();
        let launched = Launched {
            session_id: session_id.clone(),
            job_id: job_id.clone(),
            parent: self.parent.clone(),
            model: resolved,
            prompt: call.prompt,
            workspace: self.workspace.clone(),
        };
        let (runner, stop) = Runner::new(
            job_id.clone(),
            session_id.clone(),
            PathBuf::from(&output_path),
            Arc::clone(&self.clock),
            self.bound,
            self.cap,
            Arc::clone(&self.watch),
        );
        let child = match runner.spawn(&self.launch, &launched) {
            Ok(child) => child,
            Err(source) => {
                return failed(
                    ErrorCode::IoFailed,
                    format!("Starting the delegate failed: {source}."),
                );
            }
        };
        let (started, finish) = self.registry.open_started(
            job_id.clone(),
            "delegate_spawn".to_owned(),
            call.description,
            output_path.clone(),
            stop,
        );
        std::thread::spawn(move || runner.drive(child, finish));
        let delegate = DelegateStarted {
            job_id: job_id.clone(),
            delegate_session_id: session_id.clone(),
            harness: "fiber".to_owned(),
            model: call.model,
            workspace: self.workspace.to_string_lossy().into_owned(),
            worktree: None,
            forked_from: None,
        };
        Output {
            content: vec![ContentPart::Text {
                text: format!("Started delegate {}. Its log is {}.", job_id.0, output_path),
            }],
            jobs: vec![
                JobRecord::Started(started),
                JobRecord::DelegateStarted(delegate),
            ],
            ..Output::default()
        }
    }
}

fn failed(code: ErrorCode, message: String) -> Output {
    Output {
        content: vec![ContentPart::Text {
            text: message.clone(),
        }],
        error: Some(Failure {
            code,
            message,
            retry_after_ms: None,
            provider: None,
        }),
        ..Output::default()
    }
}

#[cfg(test)]
#[path = "delegate/delegate_tests.rs"]
mod tests;
