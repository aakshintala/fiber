//! Steps 5 to 7 of a step (`docs/loop.md`, "One step"): every tool call in a
//! reply is checked and decided in order, the approved ones run at once, one
//! thread each, and their results are written in the order the model asked
//! for them (`docs/architecture.md`, "Tool calls in a step").

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Weak};
use std::thread;

use contract::clock::Wake;
use contract::commands::{RememberScope, ReplyAnswer};
use contract::events::{
    AskStep, CallStatus, DecidedBy, Decision, Event, Grant, PermissionRequested,
    PermissionResolved, RuleOffer, ToolCallCompleted, ToolCallRequested, ToolCallStarted,
    ToolReplaced,
};
use contract::provider::ToolDefinition;
use contract::shapes::{ContentPart, DeclaredEffects, Effect, Failure};
use contract::tool::{Bound, Cancel, Output, Tool};
use contract::{ActionId, ErrorCode, RequestId, SessionId, TurnId};
use serde_json::{Map, Value};

use crate::inbox::{self, Waited};
use crate::{Error, Loop, schema};

/// A call that runs: its tool, the arguments it runs with, and the effects
/// it declared.
type Approved = (Arc<dyn Tool>, Map<String, Value>, DeclaredEffects);

/// What a person's answer carries onto its `permission_resolved` line.
struct Answered {
    /// Allow or deny.
    decision: Decision,
    /// With a denial, what the person typed.
    feedback: Option<String>,
    /// On an allow that added a session grant.
    grant: Option<Grant>,
    /// On an allow that added a standing rule to the project's rules file.
    rule: Option<Grant>,
    /// Why a remembered rule is missing: saving it failed.
    reason: Option<String>,
}

impl Answered {
    /// The `permission_resolved` line for a person's allow of the call
    /// `request_id` answered: a remembered session grant rides `grant`, a
    /// remembered project rule rides `rule`, and a rule that could not be
    /// saved rides `reason`.
    fn allow(self, request_id: RequestId) -> PermissionResolved {
        PermissionResolved {
            grant: self.grant,
            rule: self.rule,
            ..resolved(
                Some(request_id),
                Decision::Allow,
                DecidedBy::Person,
                self.reason,
                None,
            )
        }
    }
}

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
                        Err((
                            bound,
                            scope.spawn(move || tool.run(&arguments, &NeverCancel)),
                        ))
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
        // The rules are read once for every call, including ones that later
        // fast-path, so a revoked rule applies to the next call judged
        // (`docs/tui.md`, "/rules"). `judge` runs the credential deny before
        // it looks at them, so an unreadable rules file never stops that
        // deny.
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
                let text = format!("{why} It did not run.");
                self.decided(
                    id,
                    turn,
                    resolved(None, Decision::Deny, by, Some(why), None),
                )?;
                Ok(Err(denied(reason, text)))
            }
            super::permission::Verdict::Ask(rule) => self
                .ask(id, turn, &call.name, &effects.declared, rule)
                .map(|answered| {
                    answered.map(|()| (Arc::clone(&tool), arguments, effects.declared))
                }),
            super::permission::Verdict::Allow(decided) => {
                if let Some(by) = decided {
                    self.decided(id, turn, resolved(None, Decision::Allow, by, None, None))?;
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
    /// reply (`docs/permissions.md`, "What the log records"). Other
    /// deliveries are admitted as at any drain. A reply that does not fit
    /// is rejected and the wait goes on; one that names another request is
    /// rejected `stale_request`.
    fn ask(
        &mut self,
        id: &ActionId,
        turn: &TurnId,
        tool: &str,
        declared: &DeclaredEffects,
        rule: contract::events::StandingRule,
    ) -> Result<Result<(), Box<ToolCallCompleted>>, Error> {
        if !self.answerable {
            return self.unanswerable(id, turn, None);
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
            let delivery = match self.inbox.recv() {
                Ok(delivery) => delivery,
                // Every sender is gone, so no answer can come.
                Err(_) => {
                    let reason = "The session ended while waiting for an answer.";
                    self.decided(
                        id,
                        turn,
                        resolved(
                            Some(request_id),
                            Decision::Deny,
                            DecidedBy::StandingRule,
                            Some(reason.to_owned()),
                            None,
                        ),
                    )?;
                    return Ok(Err(denied(
                        "no_person",
                        format!("{reason} It did not run."),
                    )));
                }
            };
            match self.take_while_waiting(&request_id, delivery) {
                Waited::Again => {}
                Waited::Closed => return self.unanswerable(id, turn, Some(request_id)),
                Waited::Reply(reply, ack) => {
                    let Some(answered) = self.answered(tool, None, &reply.answer) else {
                        inbox::reject(ack, ErrorCode::InvalidArguments, inbox::UNFIT_REPLY);
                        continue;
                    };
                    inbox::accept(ack);
                    match answered.decision {
                        Decision::Deny => {
                            let text = match &answered.feedback {
                                Some(feedback) => format!(
                                    "A person refused this call: {feedback}. It did not run."
                                ),
                                None => "A person refused this call. It did not run.".to_owned(),
                            };
                            self.decided(
                                id,
                                turn,
                                resolved(
                                    Some(request_id),
                                    Decision::Deny,
                                    DecidedBy::Person,
                                    None,
                                    answered.feedback,
                                ),
                            )?;
                            return Ok(Err(denied("person", text)));
                        }
                        Decision::Allow => {
                            self.decided(id, turn, answered.allow(request_id))?;
                            return Ok(Ok(()));
                        }
                    }
                }
            }
        }
    }

    /// Denies a call no person can answer (`docs/permissions.md`,
    /// "Headless"): a session started by `fiber ask`, or one `close` has
    /// been taken. `request_id` is set when the request was already raised.
    fn unanswerable(
        &mut self,
        id: &ActionId,
        turn: &TurnId,
        request_id: Option<RequestId>,
    ) -> Result<Result<(), Box<ToolCallCompleted>>, Error> {
        let reason = "No person can answer an approval in this session.";
        self.decided(
            id,
            turn,
            resolved(
                request_id,
                Decision::Deny,
                DecidedBy::StandingRule,
                Some(reason.to_owned()),
                None,
            ),
        )?;
        Ok(Err(denied(
            "no_person",
            format!("{reason} It did not run."),
        )))
    }

    /// Checks `reply` against `offer`, the request's rule offer (`None` on a
    /// standing ask, which offers nothing to remember), and applies what an
    /// allow remembers (`docs/permissions.md`, "Remembering a decision"): a
    /// session grant goes onto `grants` before the next call is judged, and a
    /// project rule is appended to the project's rules file. A project rule
    /// that cannot be saved still allows the call, with a reason saying so.
    /// `None`: the reply does not fit, so the caller rejects it and the loop
    /// keeps waiting.
    fn answered(
        &mut self,
        tool: &str,
        offer: Option<&RuleOffer>,
        reply: &ReplyAnswer,
    ) -> Option<Answered> {
        let super::permission::Answer {
            decision,
            feedback,
            remember,
        } = super::permission::answer(offer, reply)?;
        let mut answered = Answered {
            decision,
            feedback,
            grant: None,
            rule: None,
            reason: None,
        };
        let Some(remembered) = remember else {
            return Some(answered);
        };
        match remembered.scope {
            RememberScope::Session => {
                let grant = remembered.grant(tool);
                self.grants.push(grant.clone());
                answered.grant = Some(grant);
            }
            RememberScope::Project => {
                let session = SessionId(self.cache_key.clone());
                match self.rules.remember(tool, &remembered.prefix, &session) {
                    Ok(()) => {
                        answered.rule = Some(remembered.grant(tool));
                    }
                    Err(error) => {
                        answered.reason = Some(format!("The rule could not be saved: {error}."));
                    }
                }
            }
        }
        Some(answered)
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

/// A cancel signal that never fires.
///
/// debt: a call is never cancelled, #301 keeps a token, fires it, and then writes `cancelled`
struct NeverCancel;

impl Cancel for NeverCancel {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn subscribe(&self, _waker: Weak<dyn Wake>) {}
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

/// A `permission_resolved` line carrying `request_id`, `decision`,
/// `decided_by`, `reason` and `feedback`: every other key is absent. A call
/// whose answer remembered something sets `grant` or `rule` on it.
fn resolved(
    request_id: Option<RequestId>,
    decision: Decision,
    decided_by: DecidedBy,
    reason: Option<String>,
    feedback: Option<String>,
) -> PermissionResolved {
    PermissionResolved {
        request_id,
        decision,
        decided_by,
        reason,
        feedback,
        grant: None,
        rule: None,
        reviewer: None,
    }
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
