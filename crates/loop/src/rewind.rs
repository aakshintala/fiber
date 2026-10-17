//! Starting a session that continues another from a step boundary
//! (`docs/events.md`, "Rewind"): the inherited preamble, the note, and the
//! old session's worktree.

use std::collections::HashMap;
use std::path::Path;

use contract::events::{Event, Rewind, SessionNamed, SessionStarted, ToolCallRequested};
use contract::provider::{Input, ToolDefinition};
use contract::shapes::{Effect, Point, Worktree};
use contract::{ActionId, Seq};
use serde_json::Value;

use crate::prompt::{fill, message};
use crate::{Error, Loop, Preamble, Session, variables};

pub(crate) mod command;
pub(crate) mod stop;

/// What the new session starts from: the point, the note, and the old
/// session's worktree.
pub struct Rewound {
    /// The point the new session continues from.
    pub from: Point,
    /// Fiber's note: files written, shell calls and jobs since the point.
    pub note: String,
    /// The old session's worktree, when it runs in one: the new session
    /// runs there too and never removes it (`docs/events.md`, "Rewind").
    pub worktree: Option<Worktree>,
    /// The rewound session's latest `session_named`, from
    /// [`latest_name`]: the new session writes a copy after
    /// `session_started`, so the name and a pin carry over.
    pub name: Option<SessionNamed>,
}

impl Loop {
    /// Starts a session that continues another from `start.from`, whose
    /// history is the old log folded to the point: `session_started`
    /// carries `forked_from`, the note and the old session's worktree, with
    /// no `parent` and no summary. The chain
    /// now includes this line, so the fold gives the new session the old
    /// log's history to the point. The new session starts with no jobs, so
    /// it clears its orphans, then builds through [`Loop::resume`], sending
    /// the fold's latest `preamble_built` verbatim as its first request.
    /// It writes no `preamble_built`: nothing was built. Everything a new
    /// loop runs on rides [`crate::Session`].
    pub fn rewound(session: Session, start: Rewound) -> Result<Self, Error> {
        session.log.append(
            &Event::SessionStarted(SessionStarted {
                workspace: session.permissions.workspace.clone(),
                variables: variables(),
                parent: None,
                forked_from: Some(start.from),
                rewind: Some(Rewind {
                    summary: None,
                    note: start.note,
                }),
                worktree: start.worktree,
            }),
            None,
            None,
        )?;
        // The name goes with the conversation: a copy of the parent's
        // latest `session_named`, `by` included, so a pin holds
        // (`docs/events.md`, "Rewind").
        if let Some(named) = start.name {
            session
                .log
                .append(&Event::SessionNamed(named), None, None)?;
        }
        let mut folded = crate::resume::resumed(session.log.dir())?;
        let logged = folded.preamble.clone();
        // The new session starts with no jobs: the jobs the folded history
        // holds open were stopped by the rewind in the old log, not orphaned.
        // The handoff carry drops them too, so a handoff started in the new
        // session names no job. It continues the logged build verbatim, so it
        // writes no `model_changed` for a label the log changed after the
        // point (`docs/events.md`, "Rewind").
        folded.orphans = Vec::new();
        folded.settings = None;
        let mut rewound = Loop::resume(session, folded)?;
        rewound.handoff.carry.jobs.clear();
        // A rewound session starts for its own reason, whatever the
        // preamble it adopted was built for.
        rewound.lifecycle.reason = contract::hook::StartReason::Rewind;
        if let Some(built) = logged {
            let tools = rewound
                .tools
                .values()
                .map(|(_, _, definition)| definition.clone())
                .collect();
            rewound.preamble = Some(Preamble::logged(&built, tools));
            rewound.chosen = built.thinking.as_ref().and_then(|level| level.parse().ok());
            rewound.set_trigger();
        }
        Ok(rewound)
    }
}

/// The latest `session_named` in the log in `dir`, read from the end
/// backwards; `None` when the session never named itself or its log
/// cannot be read.
pub fn latest_name(dir: &Path) -> Option<SessionNamed> {
    log::last_line_where(dir, |line| {
        let event: Value = serde_json::from_slice(line).ok()?;
        if event.get("kind")?.as_str()? != "session_named" {
            return None;
        }
        serde_json::from_value(event.get("payload")?.clone()).ok()
    })
}

impl Preamble {
    /// The fold's latest `preamble_built` as the preamble: the system
    /// prompt, the tool choice, the cache lifetime and the thinking level
    /// as logged, and the logged definitions as what the first request
    /// sends. `tools` is the session's own registry definitions, for
    /// executing calls only.
    fn logged(built: &contract::events::PreambleBuilt, tools: Vec<ToolDefinition>) -> Self {
        Self {
            system_prompt: built.system_prompt.clone(),
            tools,
            sent_tools: Some(
                built
                    .tools
                    .iter()
                    .map(|tool| tool.definition.clone())
                    .collect(),
            ),
            tool_choice: built.tool_choice.clone(),
            cache_lifetime: built.cache_lifetime,
            thinking: built.thinking.as_ref().and_then(|level| level.parse().ok()),
        }
    }
}

/// What the conversation renders for `session_started.rewind`: the note as
/// one user message, so a live start and a resume render it the same way.
pub(crate) fn note_input(rewind: &Rewind) -> Input {
    Input::User {
        text: rewind.note.clone(),
        images: Vec::new(),
    }
}

/// Collects what the session did after the point `point` in the log in
/// `dir`: the paths its tools wrote and the shell calls that may have
/// changed files, each in first-seen order, from the effects each
/// `tool_call_started` declares. The text comes only from logged fields,
/// so it is the same bytes whenever it is rendered.
fn changes(dir: &Path, point: Seq) -> Result<(Vec<String>, Vec<String>), Error> {
    let lines = log::read(dir)?;
    let mut requested: HashMap<ActionId, ToolCallRequested> = HashMap::new();
    for line in &lines {
        if line.kind != "tool_call_requested" {
            continue;
        }
        if let Some(Event::ToolCallRequested(call)) =
            Event::from_envelope(line).map_err(Error::Unreadable)?
            && let Some(action) = &line.action_id
        {
            requested.insert(action.clone(), call);
        }
    }
    let mut paths: Vec<String> = Vec::new();
    let mut calls: Vec<String> = Vec::new();
    for line in &lines {
        if line.kind != "tool_call_started" {
            continue;
        }
        if !line.seq.as_ref().is_some_and(|seq| seq.0 > point.0) {
            continue;
        }
        let Some(Event::ToolCallStarted(started)) =
            Event::from_envelope(line).map_err(Error::Unreadable)?
        else {
            continue;
        };
        let Some(action) = &line.action_id else {
            continue;
        };
        let Some(call) = requested.get(action) else {
            continue;
        };
        // A hook may have rewritten the arguments: what ran wins.
        let arguments = started
            .arguments
            .clone()
            .map(Value::Object)
            .unwrap_or_else(|| call.arguments.clone());
        let ran = format!("- {}: {}", call.name, arguments);
        if started.declared.effects.contains(&Effect::Writes) {
            match &started.declared.paths {
                // A `writes` call that names its paths lists them; one
                // with none is listed with the commands, so nothing it
                // wrote is left out.
                Some(declared) if !declared.is_empty() => {
                    for path in declared {
                        // Duplicates are kept once, in first-seen order.
                        if !paths.iter().any(|held| held == path) {
                            paths.push(path.clone());
                        }
                    }
                }
                _ => calls.push(ran.clone()),
            }
        }
        if started.declared.effects.contains(&Effect::Executes) {
            calls.push(ran);
        }
    }
    Ok((paths, calls))
}

/// Builds the note for the point `point` in the log in `dir`: the files
/// written and the shell calls run after the point, from [`changes`]
/// (`docs/events.md`, "Rewind"). The text comes only from logged fields,
/// so it is the same bytes whenever it is rendered.
///
/// debt: `pub` only for the integration tests in `crates/loop/tests/`,
/// ceiling one re-export; move the callers in `rewind.rs` and
/// `approved_code.rs` there to [`rewind_note_with_jobs`], after which
/// `rewind_note` goes crate-private.
pub fn rewind_note(dir: &Path, point: Seq) -> Result<String, Error> {
    let (paths, calls) = changes(dir, point)?;
    Ok(note(&paths, &calls, &[]))
}

/// The note with the jobs the rewind stopped, read from the rewound
/// session's own log (`docs/events.md`, "Rewind"): `old` is that log's
/// directory, `dir` the point's. With no stopped job the output equals
/// `rewind_note` byte for byte.
pub fn rewind_note_with_jobs(old: &Path, dir: &Path, point: Seq) -> Result<String, Error> {
    let stopped = stop::stopped_jobs(old)?;
    let (paths, calls) = changes(dir, point)?;
    let jobs = stopped
        .iter()
        .map(|job| match &job.description {
            Some(description) => format!("- {}: {description}", job.job_id.0),
            None => format!("- {}", job.job_id.0),
        })
        .collect::<Vec<_>>();
    Ok(note(&paths, &calls, &jobs))
}

/// The note text over `paths` written, `calls` run after the point, and
/// `jobs` stopped by the rewind: the jobs section follows the changes,
/// joined with a blank line, only when at least one job was stopped.
fn note(paths: &[String], calls: &[String], jobs: &[String]) -> String {
    let changes = match (paths.is_empty(), calls.is_empty()) {
        (true, true) => message("rewind-unchanged").to_owned(),
        _ => {
            let mut parts = Vec::new();
            if !paths.is_empty() {
                let listed = paths
                    .iter()
                    .map(|path| format!("- {path}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                parts.push(fill(message("rewind-written"), &[("paths", &listed)]));
            }
            if !calls.is_empty() {
                parts.push(fill(message("rewind-ran"), &[("calls", &calls.join("\n"))]));
            }
            parts.join("\n\n")
        }
    };
    let noted = fill(message("rewind-note"), &[("changes", &changes)]);
    if jobs.is_empty() {
        noted
    } else {
        format!(
            "{noted}\n\n{}",
            fill(message("rewind-jobs"), &[("stopped", &jobs.join("\n"))])
        )
    }
}

#[cfg(test)]
#[path = "rewind_tests.rs"]
mod tests;
