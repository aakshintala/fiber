//! Steps 5 to 7 of a step (`docs/loop.md`, "One step"): every tool call in a
//! reply is checked and decided in order, the approved ones run at once, one
//! thread each, and their results are written in the order the model asked
//! for them (`docs/architecture.md`, "Tool calls in a step").

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::thread;

use contract::events::{
    AskStep, CallStatus, DecidedBy, Decision, Event, PermissionRequested, PermissionResolved,
    ToolCallCompleted, ToolCallRequested, ToolCallStarted, ToolReplaced,
};
use contract::inbox::Delivery;
use contract::provider::ToolDefinition;
use contract::shapes::{ContentPart, DeclaredEffects, Effect, Failure};
use contract::tool::{Bound, Output, Tool};
use contract::{ActionId, ErrorCode, RequestId, TurnId};
use serde_json::{Map, Value};

use crate::{Error, Loop, schema};

/// A call that runs: its tool, the arguments it runs with, and the effects
/// it declared.
type Approved = (Arc<dyn Tool>, Map<String, Value>, DeclaredEffects);

/// A registered tool: who registered it (`builtin`, or the extension or MCP
/// server), the tool, and its definition.
pub(crate) type Registered = (String, Arc<dyn Tool>, ToolDefinition);

/// `tools`, each paired with who registered it, by name. A later tool of a
/// name already taken replaces the earlier one, and each replacement is
/// returned (`docs/architecture.md`, "Tool seam").
pub(crate) fn register(
    tools: Vec<(String, Arc<dyn Tool>)>,
) -> (BTreeMap<String, Registered>, Vec<ToolReplaced>) {
    let mut registered = BTreeMap::new();
    let mut replaced = Vec::new();
    for (by, tool) in tools {
        let definition = tool.definition();
        let name = definition.name.clone();
        if let Some((from, _, _)) = registered.insert(name.clone(), (by.clone(), tool, definition))
        {
            replaced.push(ToolReplaced { name, from, to: by });
        }
    }
    (registered, replaced)
}

impl Loop {
    /// `call` with the repair its tool's schema allows, if any
    /// (`docs/tools.md`, "Before a call runs").
    pub(crate) fn repaired(&self, mut call: ToolCallRequested) -> ToolCallRequested {
        if let Some((_, _, definition)) = self.tools.get(&call.name) {
            call.repair = schema::repair(&definition.input_schema, &call.arguments);
        }
        call
    }

    /// Decides every call in order, then runs the approved ones concurrently and
    /// writes each call's completion in request order. Deciding waits for a
    /// person's reply, so every decision line is written before any
    /// `tool_call_started`.
    pub(crate) fn run_calls(
        &mut self,
        calls: Vec<(ActionId, ToolCallRequested)>,
        turn: &TurnId,
    ) -> Result<(), Error> {
        let mut decided = Vec::with_capacity(calls.len());
        for (id, call) in calls {
            let decision = self.decide(&call, &id, turn)?;
            decided.push((id, decision));
        }
        thread::scope(|scope| {
            let mut running = Vec::new();
            for (id, decision) in decided {
                let ran = match decision {
                    Err(completed) => Ok(completed),
                    Ok((tool, arguments, declared)) => {
                        self.append(
                            &Event::ToolCallStarted(ToolCallStarted {
                                declared,
                                arguments: None,
                                changed_by: None,
                            }),
                            turn,
                            Some(&id),
                        )?;
                        let bound = tool.bound();
                        Err((bound, scope.spawn(move || tool.run(&arguments))))
                    }
                };
                running.push((id, ran));
            }
            for (id, ran) in running {
                let completed = match ran {
                    Ok(completed) => *completed,
                    Err((bound, handle)) => {
                        // A panic aborts the process (`docs/code-quality.md`,
                        // "Panics"), so a join never fails.
                        let output = handle.join().unwrap_or_default();
                        self.finish(output, bound, &id)
                    }
                };
                self.append(&Event::ToolCallCompleted(completed), turn, Some(&id))?;
            }
            Ok(())
        })
    }

    /// Checks `call` and decides whether it runs (`docs/loop.md`, "Tool
    /// calls that do not run", and `docs/permissions.md`, "The order a call
    /// is judged in").
    fn decide(
        &mut self,
        call: &ToolCallRequested,
        id: &ActionId,
        turn: &TurnId,
    ) -> Result<Result<Approved, Box<ToolCallCompleted>>, Error> {
        let Some((_, tool, definition)) = self.tools.get(&call.name) else {
            let names: Vec<String> = self.tools.keys().map(|n| format!("`{n}`")).collect();
            let exist = if names.is_empty() {
                "You have no tools.".to_owned()
            } else {
                format!("The tools are {}.", names.join(", "))
            };
            return Ok(Err(Box::new(failed(
                ErrorCode::UnknownTool,
                format!("No tool is named `{}`. {exist}", call.name),
            ))));
        };
        let arguments = match &call.repair {
            Some(repair) => Value::Object(repair.repaired.clone()),
            None => call.arguments.clone(),
        };
        let Value::Object(arguments) = arguments else {
            return Ok(Err(Box::new(failed(
                ErrorCode::InvalidArguments,
                "The arguments are not a JSON object. Send them as one.".to_owned(),
            ))));
        };
        let errors = schema::check(&definition.input_schema, &Value::Object(arguments.clone()));
        if !errors.is_empty() {
            return Ok(Err(Box::new(failed(
                ErrorCode::InvalidArguments,
                format!(
                    "The arguments do not match the tool's schema:\n{}",
                    errors.join("\n")
                ),
            ))));
        }
        let effects = match tool.effects(&arguments) {
            Ok(effects) => effects,
            Err(e) => {
                return Ok(Err(Box::new(failed(ErrorCode::ToolError, e.to_string()))));
            }
        };
        let tool = Arc::clone(tool);
        // Step 1 first, before the rules are read: a call the credential
        // deny refuses never touches them.
        if let Some(why) =
            super::permission::credential_why(&effects.declared, &self.workspace, &self.credentials)
        {
            self.decided(
                id,
                turn,
                PermissionResolved {
                    request_id: None,
                    decision: Decision::Deny,
                    decided_by: DecidedBy::CredentialDeny,
                    reason: Some(why.clone()),
                    feedback: None,
                    grant: None,
                    rule: None,
                    reviewer: None,
                },
            )?;
            return Ok(Err(denied("credentials", format!("{why} It did not run."))));
        }
        // The rules are read for every call that reaches step 2, including
        // ones that later fast-path (`docs/permissions.md`, "Scope").
        let rules = self.rules.read();
        match super::permission::judge(
            &call.name,
            &effects,
            &rules,
            &self.grants,
            &self.workspace,
            &self.credentials,
        ) {
            super::permission::Verdict::Deny { by, reason, why } => {
                self.decided(
                    id,
                    turn,
                    PermissionResolved {
                        request_id: None,
                        decision: Decision::Deny,
                        decided_by: by,
                        reason: Some(why.clone()),
                        feedback: None,
                        grant: None,
                        rule: None,
                        reviewer: None,
                    },
                )?;
                Ok(Err(denied(reason, format!("{why} It did not run."))))
            }
            super::permission::Verdict::Ask(rule) => {
                self.ask(id, turn, &effects.declared, rule).map(|answered| {
                    answered.map(|()| (Arc::clone(&tool), arguments, effects.declared))
                })
            }
            super::permission::Verdict::Allow(decided) => {
                if let Some(by) = decided {
                    self.decided(
                        id,
                        turn,
                        PermissionResolved {
                            request_id: None,
                            decision: Decision::Allow,
                            decided_by: by,
                            reason: None,
                            feedback: None,
                            grant: None,
                            rule: None,
                            reviewer: None,
                        },
                    )?;
                }
                Ok(Ok((tool, arguments, effects.declared)))
            }
            // Step 7 is the reviewer (#294): until then every other call is
            // denied.
            super::permission::Verdict::Review => {
                let text = "This call needs a review, and no reviewer is available. \
                            It did not run."
                    .to_owned();
                Ok(Err(Box::new(ToolCallCompleted {
                    status: CallStatus::Denied,
                    reason: Some("not_reviewed".to_owned()),
                    ..completed(text, None)
                })))
            }
        }
    }

    /// Writes a `permission_resolved` line for `id`.
    fn decided(
        &mut self,
        id: &ActionId,
        turn: &TurnId,
        resolved: PermissionResolved,
    ) -> Result<(), Error> {
        self.append(&Event::PermissionResolved(resolved), turn, Some(id))
    }

    /// Asks a person about a call a standing ask matched, waiting for their
    /// reply (`docs/permissions.md`, "What the log records"). Messages
    /// that arrive meanwhile are held for the next step boundary; a reply
    /// that names another request, or does not fit, is dropped and the loop
    /// keeps waiting.
    fn ask(
        &mut self,
        id: &ActionId,
        turn: &TurnId,
        declared: &DeclaredEffects,
        rule: contract::events::StandingRule,
    ) -> Result<Result<(), Box<ToolCallCompleted>>, Error> {
        if !self.answerable {
            let reason = "No person can answer an approval in this session.";
            self.decided(
                id,
                turn,
                PermissionResolved {
                    request_id: None,
                    decision: Decision::Deny,
                    decided_by: DecidedBy::StandingRule,
                    reason: Some(reason.to_owned()),
                    feedback: None,
                    grant: None,
                    rule: None,
                    reviewer: None,
                },
            )?;
            return Ok(Err(denied(
                "no_person",
                format!("{reason} It did not run."),
            )));
        }
        let request_id = RequestId(super::mint("r_"));
        self.append(
            &Event::PermissionRequested(PermissionRequested {
                request_id: request_id.clone(),
                declared: declared.clone(),
                step: AskStep::StandingAsk {
                    standing_rule: rule,
                },
            }),
            turn,
            Some(id),
        )?;
        loop {
            match self.inbox.recv() {
                // Every sender is gone, so no answer can come.
                Err(_) => {
                    let reason = "The session ended while waiting for an answer.";
                    self.decided(
                        id,
                        turn,
                        PermissionResolved {
                            request_id: Some(request_id),
                            decision: Decision::Deny,
                            decided_by: DecidedBy::StandingRule,
                            reason: Some(reason.to_owned()),
                            feedback: None,
                            grant: None,
                            rule: None,
                            reviewer: None,
                        },
                    )?;
                    return Ok(Err(denied(
                        "no_person",
                        format!("{reason} It did not run."),
                    )));
                }
                Ok(Delivery::Message(message)) => self.held.push_back(message),
                Ok(Delivery::Reply(reply)) => {
                    if reply.request_id != request_id {
                        continue;
                    }
                    // A standing ask offers no rule to remember, so a reply
                    // that remembers never fits.
                    let Some(answer) = super::permission::answer(None, &reply.answer) else {
                        continue;
                    };
                    match answer.decision {
                        Decision::Deny => {
                            self.decided(
                                id,
                                turn,
                                PermissionResolved {
                                    request_id: Some(request_id),
                                    decision: Decision::Deny,
                                    decided_by: DecidedBy::Person,
                                    reason: None,
                                    feedback: answer.feedback.clone(),
                                    grant: None,
                                    rule: None,
                                    reviewer: None,
                                },
                            )?;
                            let text = match answer.feedback {
                                Some(feedback) => format!(
                                    "A person refused this call: {feedback}. It did not run."
                                ),
                                None => "A person refused this call. It did not run.".to_owned(),
                            };
                            return Ok(Err(denied("person", text)));
                        }
                        Decision::Allow => {
                            debug_assert!(answer.remember.is_none());
                            self.decided(
                                id,
                                turn,
                                PermissionResolved {
                                    request_id: Some(request_id),
                                    decision: Decision::Allow,
                                    decided_by: DecidedBy::Person,
                                    reason: None,
                                    feedback: None,
                                    grant: None,
                                    rule: None,
                                    reviewer: None,
                                },
                            )?;
                            return Ok(Ok(()));
                        }
                    }
                }
            }
        }
    }

    /// The completion of a call that ran and returned `output`, its text cut
    /// to `bound` (`docs/tools.md`, "Bounded results").
    fn finish(&self, output: Output, bound: Bound, id: &ActionId) -> ToolCallCompleted {
        let Output {
            content,
            error,
            process,
            details,
            changes,
            control,
        } = output;
        let (text, images): (Vec<ContentPart>, Vec<ContentPart>) = content
            .into_iter()
            .partition(|part| matches!(part, ContentPart::Text { .. }));
        let full = crate::conversation::text(&text);
        let cap = bound.start.saturating_add(bound.end);
        let (content, artifact) = if full.len() > cap {
            let (kept, artifact) = self.cut(&full, bound, id);
            (vec![ContentPart::Text { text: kept }], artifact)
        } else {
            (text, None)
        };
        ToolCallCompleted {
            status: if error.is_some() {
                CallStatus::Failed
            } else {
                CallStatus::Completed
            },
            error,
            process,
            details,
            changes,
            control,
            artifact,
            content: content.into_iter().chain(images).collect(),
            ..completed(String::new(), None)
        }
    }

    /// `full` cut to `bound`, with a notice of how many bytes were cut and
    /// where the whole text is, and the artifact's path relative to the
    /// session directory.
    fn cut(&self, full: &str, bound: Bound, id: &ActionId) -> (String, Option<String>) {
        let head = full.floor_char_boundary(bound.start);
        let tail = full.ceil_char_boundary(full.len().saturating_sub(bound.end).max(head));
        let removed = tail.saturating_sub(head);
        let (notice, artifact) = match self
            .log
            .write_artifact(&format!("{}.txt", id.0), full.as_bytes())
        {
            Ok((relative, path)) => (
                format!(
                    "[{removed} bytes cut. The full output is in {}; read it with `read`.]",
                    path.display()
                ),
                Some(relative),
            ),
            Err(e) => (
                format!("[{removed} bytes cut. The full output could not be saved: {e}.]"),
                None,
            ),
        };
        let mut kept = full.get(..head).unwrap_or_default().to_owned();
        kept.push('\n');
        kept.push_str(&notice);
        if tail < full.len() {
            kept.push('\n');
            kept.push_str(full.get(tail..).unwrap_or_default());
        }
        (kept, artifact)
    }
}

/// How a call the reply cut off completes (`docs/loop.md`, "A reply cut off
/// by the output limit").
pub(crate) fn truncated() -> ToolCallCompleted {
    failed(
        ErrorCode::OutputTruncated,
        "Your reply reached its output limit, so this call did not run and its \
         arguments may be incomplete. Make the call again, split into smaller \
         calls if it was large."
            .to_owned(),
    )
}

/// A call that was denied with `reason`, the model told `text`.
fn denied(reason: &str, text: String) -> Box<ToolCallCompleted> {
    Box::new(ToolCallCompleted {
        status: CallStatus::Denied,
        reason: Some(reason.to_owned()),
        ..completed(text, None)
    })
}

/// A call that failed with `code` before it ran, the model told `message`.
fn failed(code: ErrorCode, message: String) -> ToolCallCompleted {
    ToolCallCompleted {
        status: CallStatus::Failed,
        ..completed(
            message.clone(),
            Some(Failure {
                code,
                message,
                retry_after: None,
                provider: None,
            }),
        )
    }
}

fn completed(text: String, error: Option<Failure>) -> ToolCallCompleted {
    ToolCallCompleted {
        status: CallStatus::Completed,
        reason: None,
        error,
        process: None,
        content: if text.is_empty() {
            Vec::new()
        } else {
            vec![ContentPart::Text { text }]
        },
        details: None,
        artifact: None,
        changes: None,
        control: None,
        changed_by: None,
    }
}

/// Whether a call with `declared` effects takes a fast path
/// (`docs/permissions.md`, "Fast paths"): it only reads, or it writes only
/// inside `workspace` and outside `.git/` and `.fiber/`.
pub(crate) fn fast_path(declared: &DeclaredEffects, workspace: &Path) -> bool {
    let only = |allowed: &[Effect]| declared.effects.iter().all(|e| allowed.contains(e));
    if only(&[Effect::Reads]) {
        return true;
    }
    only(&[Effect::Reads, Effect::Writes])
        && declared.paths.as_ref().is_some_and(|paths| {
            !paths.is_empty() && paths.iter().all(|p| inside(&workspace.join(p), workspace))
        })
}

/// Whether `path`, symlinks resolved, sits in `workspace` and under no
/// `.git` or `.fiber` directory.
fn inside(path: &Path, workspace: &Path) -> bool {
    resolve(path)
        .as_deref()
        .and_then(|p| p.strip_prefix(workspace).ok())
        .is_some_and(|rest| {
            !rest
                .components()
                .any(|c| c.as_os_str() == ".git" || c.as_os_str() == ".fiber")
        })
}

/// `path` with its longest existing ancestor canonicalised. `None` when the
/// part that does not exist yet holds `.` or `..`, which only the file
/// system can resolve.
pub(crate) fn resolve(path: &Path) -> Option<PathBuf> {
    let parts: Vec<Component<'_>> = path.components().collect();
    (0..=parts.len()).rev().find_map(|split| {
        let (existing, rest) = parts.split_at(split);
        let real = existing.iter().collect::<PathBuf>().canonicalize().ok()?;
        Some(rest.iter().try_fold(real, |p, c| match c {
            Component::Normal(name) => Some(p.join(name)),
            Component::Prefix(_)
            | Component::RootDir
            | Component::CurDir
            | Component::ParentDir => None,
        }))
    })?
}

#[cfg(test)]
#[path = "calls_tests.rs"]
mod tests;
