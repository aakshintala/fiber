//! Steps 5 to 7 of a step (`docs/loop.md`, "One step"): every tool call in a
//! reply is checked and decided in order, the approved ones run at once, one
//! thread each, and their results are written in the order the model asked
//! for them (`docs/architecture.md`, "Tool calls in a step").

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::thread;

use contract::events::{
    CallStatus, Event, ToolCallCompleted, ToolCallRequested, ToolCallStarted, ToolReplaced,
};
use contract::provider::ToolDefinition;
use contract::shapes::{ContentPart, DeclaredEffects, Effect, Failure};
use contract::tool::{Bound, Output, Tool};
use contract::{ActionId, ErrorCode, TurnId};
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

    /// Decides every call, then runs the approved ones concurrently and
    /// writes each call's completion in request order.
    pub(crate) fn run_calls(
        &mut self,
        calls: Vec<(ActionId, ToolCallRequested)>,
        turn: &TurnId,
    ) -> Result<(), Error> {
        let decided: Vec<(ActionId, Result<Approved, Box<ToolCallCompleted>>)> = calls
            .into_iter()
            .map(|(id, call)| {
                let decision = self.decide(&call);
                (id, decision)
            })
            .collect();
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
    /// calls that do not run").
    fn decide(&self, call: &ToolCallRequested) -> Result<Approved, Box<ToolCallCompleted>> {
        let Some((_, tool, definition)) = self.tools.get(&call.name) else {
            let names: Vec<String> = self.tools.keys().map(|n| format!("`{n}`")).collect();
            let exist = if names.is_empty() {
                "You have no tools.".to_owned()
            } else {
                format!("The tools are {}.", names.join(", "))
            };
            return Err(Box::new(failed(
                ErrorCode::UnknownTool,
                format!("No tool is named `{}`. {exist}", call.name),
            )));
        };
        let arguments = match &call.repair {
            Some(repair) => Value::Object(repair.repaired.clone()),
            None => call.arguments.clone(),
        };
        let Value::Object(arguments) = arguments else {
            return Err(Box::new(failed(
                ErrorCode::InvalidArguments,
                "The arguments are not a JSON object. Send them as one.".to_owned(),
            )));
        };
        let errors = schema::check(&definition.input_schema, &Value::Object(arguments.clone()));
        if !errors.is_empty() {
            return Err(Box::new(failed(
                ErrorCode::InvalidArguments,
                format!(
                    "The arguments do not match the tool's schema:\n{}",
                    errors.join("\n")
                ),
            )));
        }
        let effects = match tool.effects(&arguments) {
            Ok(effects) => effects,
            Err(e) => return Err(Box::new(failed(ErrorCode::ToolError, e.to_string()))),
        };
        // Only the fast paths are decided; every other call is denied until
        // the permission order (#293) and the reviewer (#294) exist
        // (`docs/permissions.md`, "The order a call is judged in").
        if !fast_path(&effects.declared, &self.workspace) {
            let text = "This call needs a review, and no reviewer is available. \
                        It did not run."
                .to_owned();
            return Err(Box::new(ToolCallCompleted {
                status: CallStatus::Denied,
                reason: Some("not_reviewed".to_owned()),
                ..completed(text, None)
            }));
        }
        Ok((Arc::clone(tool), arguments, effects.declared))
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
fn fast_path(declared: &DeclaredEffects, workspace: &Path) -> bool {
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
fn resolve(path: &Path) -> Option<PathBuf> {
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
