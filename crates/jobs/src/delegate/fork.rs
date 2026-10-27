//! A Fiber fork as a tool: `delegate_fork` (`docs/delegates.md`, "Forks").
//! The schema is `description`, `prompt`, `isolation` and `timeout_ms`: a fork
//! takes no model or thinking level. The run order is the
//! depth check, then the argument check, then the point read, then the
//! start it shares with spawn.

use contract::ActionId;
use contract::emit::Emit;
use contract::provider::ToolDefinition;
use contract::shapes::{DeclaredEffects, Effect};
use contract::tool::{Ask, Cancel, Effects, EffectsError, Output, Tool};
use serde_json::{Map, Value, json};

use super::{DelegateFork, check_effects_arguments, receipt};

impl Tool for DelegateFork {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "delegate_fork".to_owned(),
            description: "Starts a Fiber fork: a child session that continues \
                 a point in another session's log as a background job and exits \
                 when its run ends. Returns at once with the fork's job id and \
                 log path; wait for it with `jobs wait`. The point is read \
                 from the calling action when the call is made. Optional \
                 `timeout_ms` stops the fork at its deadline, failed with code \
                 `timeout`; there is no default."
                .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "description": {
                        "type": "string",
                        "description": "What the fork does, shown in `jobs list`."
                    },
                    "prompt": {
                        "type": "string",
                        "description": "What the child session runs."
                    },
                    "isolation": { "type": "string", "enum": ["none", "worktree"] },
                    "timeout_ms": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Stops the fork after this many milliseconds; it then ends failed with code timeout. Optional, with no default."
                    }
                },
                "required": ["description", "prompt"],
                "additionalProperties": false
            }),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        check_effects_arguments(arguments)
            .map_err(|error| EffectsError::Arguments(error.to_string()))?;
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
            hosts: None,
        })
    }

    fn run(
        &self,
        arguments: &Map<String, Value>,
        _cancel: &dyn Cancel,
        _emit: &dyn Emit,
    ) -> Output {
        self.run_inner(arguments, None)
    }

    /// Runs the call with its action, so the point read sees the call
    /// that made it. Any other path runs without one.
    fn run_asking(
        &self,
        arguments: &Map<String, Value>,
        _cancel: &dyn Cancel,
        _emit: &dyn Emit,
        ask: &dyn Ask,
    ) -> Output {
        self.run_inner(arguments, Some(ask.action()))
    }
}

impl DelegateFork {
    /// Runs the fork from its checked arguments and calling action, then
    /// answers with the start's receipt, as a spawn does.
    fn run_inner(&self, arguments: &Map<String, Value>, action: Option<ActionId>) -> Output {
        match self.start(arguments, action) {
            Ok(start) => receipt(start),
            Err(error) => error.output(false),
        }
    }
}

#[cfg(test)]
#[path = "fork_tests.rs"]
mod tests;
