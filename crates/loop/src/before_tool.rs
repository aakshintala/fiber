//! Before a call runs (`docs/tools.md`, "Before a call runs"): the
//! existence and schema check, the effects function, the `before_tool`
//! hooks and the re-check of a rewrite. The permission decision judges the
//! call this prepares, not the one the model asked for.

use std::sync::Arc;

use contract::events::{AskStep, Event, PermissionRequested, ToolCallCompleted, ToolCallRequested};
use contract::hook::{BeforeToolCall, BeforeToolOutcome, CheckOutcome, StateLine};
use contract::tool::{Effects, Tool};
use contract::{ErrorCode, TurnId};
use serde_json::{Map, Value};

use crate::calls::Decided;
use crate::{Error, Loop};

/// A call that names a tool and fits its schema: the tool, the arguments it
/// runs with, and the effects it declared.
pub(crate) type Checked = (Arc<dyn Tool>, Map<String, Value>, Effects);

/// What a started call runs again with on resume: the arguments its
/// `tool_call_started` logged and the extensions that rewrote them.
pub(crate) type Started = (Map<String, Value>, Vec<String>);

/// A call ready to be judged: the tool, the arguments it runs with, the
/// effects function's answer for those arguments, and who rewrote them.
/// `rewritten` is `Some(changed_by)` exactly when `arguments` are a hook's,
/// and `effects` are then the effects function's answer for those
/// arguments.
///
/// Public as [`crate::ReviewPrepared`] for the `review` jig's replay,
/// which rebuilds one from a source log's recorded declared effects
/// (`docs/testing.md`, "Jigs"); the loop itself builds one per call it
/// judges.
#[derive(Clone)]
pub struct Prepared {
    /// The tool the call runs on.
    pub tool: Arc<dyn Tool>,
    /// The arguments the call runs with.
    pub arguments: Map<String, Value>,
    /// The effects function's answer for `arguments`.
    pub effects: Effects,
    /// The extensions that rewrote the arguments, in run order.
    pub rewritten: Option<Vec<String>>,
}

impl From<Checked> for Prepared {
    fn from((tool, arguments, effects): Checked) -> Self {
        Self {
            tool,
            arguments,
            effects,
            rewritten: None,
        }
    }
}

impl Prepared {
    /// The logged rewrite: the judged arguments with who rewrote them, or
    /// neither when no hook rewrote the call.
    pub(crate) fn rewrite(&self) -> (Option<Map<String, Value>>, Option<Vec<String>>) {
        match &self.rewritten {
            Some(changed_by) => (Some(self.arguments.clone()), Some(changed_by.clone())),
            None => (None, None),
        }
    }
}

/// Why the schema-and-effects check failed, before it is worded.
enum ClassifyFailure {
    /// The arguments do not fit the schema: one line per error.
    Schema(Vec<String>),
    /// The effects function failed: its message.
    Effects(String),
}

/// What preparing a suspended approval found (`docs/events.md`, "Resume").
#[allow(
    clippy::large_enum_variant,
    reason = "a short-lived resume value; boxing its one large arm would allocate on every resume"
)]
pub(crate) enum Again {
    /// The model's own call no longer checks: the recorded request is
    /// raised again, and an allow completes with the check's failure.
    Unchecked(Box<ToolCallCompleted>),
    /// A hook refused or failed, or the rewrite failed its check: the call
    /// completes with that, and the request is not raised again.
    Settled(Box<ToolCallCompleted>),
    /// The prepared call, and the request to raise again: the recorded
    /// `request_id` and step kind, with `declared` and any rule offer
    /// rebuilt from the prepared call.
    Ready(Prepared, PermissionRequested, Vec<StateLine>),
}

impl Loop {
    /// The tool, arguments and effects of `call`, once it names a tool and
    /// its arguments fit that tool's schema; otherwise the failed
    /// completion it gets (`docs/loop.md`, "Tool calls that do not run").
    pub(crate) fn checked(
        &self,
        call: &ToolCallRequested,
    ) -> Result<Checked, Box<ToolCallCompleted>> {
        let Some((_, tool, _)) = self.tools.get(&call.name) else {
            return Err(unknown_tool(self, &call.name));
        };
        let tool = Arc::clone(tool);
        let arguments = match &call.repair {
            Some(repair) => Value::Object(repair.repaired.clone()),
            None => call.arguments.clone(),
        };
        let Value::Object(arguments) = arguments else {
            // The failure quotes what the model sent, so the model still
            // sees what it wrote: a raw string as sent, any other value in
            // its JSON form.
            let seen = match &call.arguments {
                Value::String(raw) => raw.clone(),
                Value::Null
                | Value::Bool(_)
                | Value::Number(_)
                | Value::Array(_)
                | Value::Object(_) => call.arguments.to_string(),
            };
            return Err(Box::new(crate::completion::failed(
                ErrorCode::InvalidArguments,
                format!("The arguments are not a JSON object. Send them as one.\n{seen}"),
            )));
        };
        match classify(&tool, &arguments) {
            Ok(effects) => {
                if let Some(tap) = &self.status_tap {
                    tap.subject(call, effects.subject.as_deref());
                }
                Ok((tool, arguments, effects))
            }
            Err(ClassifyFailure::Schema(errors)) => Err(Box::new(crate::completion::failed(
                ErrorCode::InvalidArguments,
                match suggestion(self, call) {
                    Some(name) => format!(
                        "The arguments do not match the tool's schema:\n{}\nThese arguments fit `{name}`.",
                        errors.join("\n")
                    ),
                    None => format!(
                        "The arguments do not match the tool's schema:\n{}",
                        errors.join("\n")
                    ),
                },
            ))),
            Err(ClassifyFailure::Effects(message)) => Err(Box::new(crate::completion::failed(
                ErrorCode::ToolError,
                message,
            ))),
        }
    }

    /// Checks `call`, runs the `before_tool` hooks and re-checks a rewrite,
    /// then runs the `check` hooks on what will run (`docs/permissions.md`,
    /// "The order a call is judged in"). Never writes a permission line and
    /// never touches the reviewer's history. With no hooks it is `checked`
    /// converted, `rewritten: None`.
    pub(crate) fn prepared(
        &mut self,
        call: &ToolCallRequested,
        turn: Option<&TurnId>,
    ) -> Result<(Decided, Vec<StateLine>), Error> {
        let checked = match self.checked(call) {
            Ok(checked) => checked,
            Err(failed) => return Ok((Err(failed), Vec::new())),
        };
        let Some(hooks) = self.hooks.clone() else {
            return Ok((Ok(Prepared::from(checked)), Vec::new()));
        };
        self.hooked(&call.name, checked, hooks, turn)
    }

    /// Runs the `before_tool` hooks on a checked call and re-checks a
    /// rewrite, then runs the `check` hooks on what will run. Writes each
    /// stage's notices under the turn as it answers. Never writes a
    /// permission line.
    fn hooked(
        &mut self,
        tool_name: &str,
        checked: Checked,
        hooks: Arc<dyn contract::hook::Hooks>,
        turn: Option<&TurnId>,
    ) -> Result<(Decided, Vec<StateLine>), Error> {
        let mut state = Vec::new();
        let decided = (|| -> Result<Decided, Error> {
            let (tool, arguments, effects) = checked;
            if self.shutting_down() {
                return Ok(Err(self.cancelled_before_ran()));
            }
            let answer = {
                // One actual `before_tool` dispatch: the guard closes before
                // the notices, so classification and cap work stay outside.
                // `tool_name` names the registered tool `checked` admitted.
                let _dispatch = self
                    .diag
                    .timed_tool(contract::diag::Op::ToolHooks, tool_name);
                hooks.before_tool(&BeforeToolCall {
                    tool: tool_name,
                    arguments: &arguments,
                    declared: &effects.declared,
                })
            };
            state = hooks.take_change_state();
            self.drain_state(&hooks, turn)?;
            for notice in answer.notices {
                self.write(&Event::Notice(notice), turn, None)?;
            }
            let (arguments, effects, rewritten) = match answer.outcome {
                BeforeToolOutcome::Unchanged => (arguments, effects, None),
                BeforeToolOutcome::Rewritten {
                    arguments: rewritten_arguments,
                    changed_by,
                } => {
                    let names = changed_by.join(", ");
                    match classify(&tool, &rewritten_arguments) {
                        Ok(rewritten_effects) => {
                            (rewritten_arguments, rewritten_effects, Some(changed_by))
                        }
                        Err(ClassifyFailure::Schema(errors)) => {
                            return Ok(Err(Box::new(crate::completion::failed(
                                ErrorCode::InvalidArguments,
                                format!(
                                    "Extension {names} rewrote this call's arguments, and they do not match the tool's schema:\n{}",
                                    errors.join("\n")
                                ),
                            ))));
                        }
                        Err(ClassifyFailure::Effects(message)) => {
                            return Ok(Err(Box::new(crate::completion::failed(
                                ErrorCode::ToolError,
                                format!(
                                    "Extension {names} rewrote this call's arguments, and the tool could not classify them: {message}"
                                ),
                            ))));
                        }
                    }
                }
                BeforeToolOutcome::Refused(refusal) => {
                    return Ok(Err(crate::completion::denied(
                        "hook",
                        format!(
                            "Extension {} refused this call: {}. It did not run.",
                            refusal.extension, refusal.reason
                        ),
                    )));
                }
                BeforeToolOutcome::Failed(failure) => {
                    return Ok(Err(Box::new(crate::completion::failed(
                        ErrorCode::HookFailed,
                        format!(
                            "The `before_tool` hook of extension {} failed, so the call did not run: {}",
                            failure.extension, failure.message
                        ),
                    ))));
                }
            };
            if self.shutting_down() {
                return Ok(Err(self.cancelled_before_ran()));
            }
            let check = {
                // One actual `check` dispatch: the guard closes before the
                // notices, as the `before_tool` one does.
                let _dispatch = self
                    .diag
                    .timed_tool(contract::diag::Op::ToolHooks, tool_name);
                hooks.check_tool(&BeforeToolCall {
                    tool: tool_name,
                    arguments: &arguments,
                    declared: &effects.declared,
                })
            };
            self.drain_state(&hooks, turn)?;
            for notice in check.notices {
                self.write(&Event::Notice(notice), turn, None)?;
            }
            match check.outcome {
                CheckOutcome::Passed => Ok(Ok(Prepared {
                    tool,
                    arguments,
                    effects,
                    rewritten,
                })),
                CheckOutcome::Refused(refusal) => Ok(Err(crate::completion::denied(
                    "hook",
                    format!(
                        "Extension {} refused this call: {}. It did not run.",
                        refusal.extension, refusal.reason
                    ),
                ))),
                CheckOutcome::Failed(failure) => Ok(Err(Box::new(crate::completion::failed(
                    ErrorCode::HookFailed,
                    format!(
                        "The `before_tool` hook of extension {} failed, so the call did not run: {}",
                        failure.extension, failure.message
                    ),
                )))),
            }
        })()?;
        if decided.is_err() {
            state.clear();
        }
        Ok((decided, state))
    }
}

/// `Ready` for `prepared`: the recorded `request_id` and step kind, with
/// `declared` and any rule offer rebuilt from the prepared call, so the
/// person judges the call that will run.
fn ready(prepared: Prepared, recorded: &PermissionRequested, state: Vec<StateLine>) -> Again {
    let step = match &recorded.step {
        AskStep::StandingAsk { standing_rule } => AskStep::StandingAsk {
            standing_rule: standing_rule.clone(),
        },
        AskStep::Review { escalation, .. } => AskStep::Review {
            escalation: escalation.clone(),
            rule: crate::reviewer::rule_offer(&prepared.effects),
        },
    };
    let request = PermissionRequested {
        always_reviewed: prepared.effects.always_reviewed,
        request_id: recorded.request_id.clone(),
        declared: prepared.effects.declared.clone(),
        step,
    };
    Again::Ready(prepared, request, state)
}

/// The one other tool whose schema the model's sent arguments fit, when
/// exactly one qualifies (`docs/tools.md`, "Before a call runs"). An
/// extension's inner call answers the extension, not the model, so it
/// carries no suggestion. With no preamble no request declared any tool
/// in full. Hosted tools ride the request as a type and name only, and
/// deferred tools are not declared in full either, so neither counts.
fn suggestion(looped: &Loop, call: &ToolCallRequested) -> Option<String> {
    if call.ran_by.is_some() {
        return None;
    }
    let Value::Object(sent) = &call.arguments else {
        return None;
    };
    let preamble = looped.preamble.as_ref()?;
    let deferred = preamble.deferred.as_ref();
    let mut fitted = None;
    for definition in &preamble.tools {
        if definition.name == call.name || definition.hosted.is_some() {
            continue;
        }
        if deferred.is_some_and(|deferred| deferred.contains(&definition.name)) {
            continue;
        }
        let Some(properties) = definition
            .input_schema
            .get("properties")
            .and_then(Value::as_object)
        else {
            continue;
        };
        if !sent.keys().all(|key| properties.contains_key(key)) {
            continue;
        }
        let requires_one = definition
            .input_schema
            .get("required")
            .and_then(Value::as_array)
            .is_some_and(|required| {
                required
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|name| sent.contains_key(name))
            });
        if !requires_one {
            continue;
        }
        if !crate::schema::check(&definition.input_schema, &call.arguments).is_empty() {
            continue;
        }
        if fitted.is_some() {
            return None;
        }
        fitted = Some(definition.name.clone());
    }
    fitted
}

/// The failed completion of a call naming no registered tool, as
/// [`Loop::checked`] words it.
fn unknown_tool(looped: &Loop, name: &str) -> Box<ToolCallCompleted> {
    let names: Vec<String> = looped.tools.keys().map(|n| format!("`{n}`")).collect();
    let exist = if names.is_empty() {
        "You have no tools.".to_owned()
    } else {
        format!("The tools are {}.", names.join(", "))
    };
    Box::new(crate::completion::failed(
        ErrorCode::UnknownTool,
        format!("No tool is named `{name}`. {exist}"),
    ))
}

/// The effects function's answer for `arguments` of `tool`, once they fit
/// its schema; otherwise why they do not, before it is worded.
fn classify(
    tool: &Arc<dyn Tool>,
    arguments: &Map<String, Value>,
) -> Result<Effects, ClassifyFailure> {
    let definition = tool.definition();
    let errors = crate::schema::check(&definition.input_schema, &Value::Object(arguments.clone()));
    if !errors.is_empty() {
        return Err(ClassifyFailure::Schema(errors));
    }
    match tool.effects(arguments) {
        Ok(effects) => Ok(effects),
        Err(error) => Err(ClassifyFailure::Effects(error.to_string())),
    }
}

impl Loop {
    /// Runs a suspended approval's call through the hooks once, before its
    /// request is raised again (`docs/events.md`, "Resume"). `Unchecked`:
    /// the model's own call no longer checks, and no hook runs. `Settled`:
    /// a hook refused or failed, or the rewrite failed its check. `Ready`:
    /// the prepared call with the request to raise again, rebuilt from it.
    pub(crate) fn prepared_suspended(
        &mut self,
        call: &ToolCallRequested,
        turn: &TurnId,
        recorded: &PermissionRequested,
    ) -> Result<Again, Error> {
        let checked = match self.checked(call) {
            Ok(checked) => checked,
            Err(failed) => return Ok(Again::Unchecked(failed)),
        };
        let Some(hooks) = self.hooks.clone() else {
            return Ok(ready(Prepared::from(checked), recorded, Vec::new()));
        };
        let (decided, state) = self.hooked(&call.name, checked, hooks, Some(turn))?;
        match decided {
            Ok(prepared) => Ok(ready(prepared, recorded, state)),
            Err(settled) => Ok(Again::Settled(settled)),
        }
    }

    /// Decides a call that already started, on resume, with the arguments
    /// its `tool_call_started` logged (`docs/events.md`, "Resume").
    /// `None` checks the model's call as ever. `Some` runs no hook: the
    /// started arguments are checked against the schema and the effects
    /// function, without repair, failing with the rewrite texts when they
    /// no longer fit.
    pub(crate) fn started_again(
        &self,
        call: &ToolCallRequested,
        started: Option<Started>,
    ) -> Decided {
        let Some((arguments, changed_by)) = started else {
            return self.checked(call).map(Prepared::from);
        };
        let Some((_, tool, _)) = self.tools.get(&call.name) else {
            return Err(unknown_tool(self, &call.name));
        };
        let tool = Arc::clone(tool);
        let names = changed_by.join(", ");
        match classify(&tool, &arguments) {
            Ok(effects) => Ok(Prepared {
                tool,
                arguments,
                effects,
                rewritten: Some(changed_by),
            }),
            Err(ClassifyFailure::Schema(errors)) => Err(Box::new(crate::completion::failed(
                ErrorCode::InvalidArguments,
                format!(
                    "Extension {names} rewrote this call's arguments, and they do not match the tool's schema:\n{}",
                    errors.join("\n")
                ),
            ))),
            Err(ClassifyFailure::Effects(message)) => Err(Box::new(crate::completion::failed(
                ErrorCode::ToolError,
                format!(
                    "Extension {names} rewrote this call's arguments, and the tool could not classify them: {message}"
                ),
            ))),
        }
    }
}

#[cfg(test)]
#[path = "before_tool_tests.rs"]
mod tests;
