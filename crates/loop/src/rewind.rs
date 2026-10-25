//! Starting a rewind or fork from a step boundary (`docs/events.md`,
//! "Rewind" and `docs/delegates.md`, "Forks"): the inherited preamble,
//! history and worktree.

use std::collections::HashMap;
use std::path::Path;

use contract::events::{Event, Parent, Rewind, SessionNamed, SessionStarted, ToolCallRequested};
use contract::provider::{Input, ToolDefinition};
use contract::shapes::{Effect, Point, Worktree};
use contract::{ActionId, Envelope, Seq, SessionId};
use serde_json::Value;

use crate::prompt::{fill, message};
use crate::{Error, Loop, Preamble, Session, ToolSearchFactory, variables};

pub(crate) mod command;
pub(crate) mod stop;
pub(crate) mod summary;

/// What a rewind or fork starts from: a point and its inherited worktree.
pub struct Rewound {
    /// The point the new session continues from.
    pub from: Point,
    /// The continued session's worktree, when it runs in one. The new
    /// session runs there too and never removes it.
    pub worktree: Option<Worktree>,
    /// Whether this is a person's rewind or a delegate's fork.
    pub how: Continuation,
    /// Builds the `tool_search` tool the first request declares when the
    /// logged preamble deferred tools (`docs/tools.md`, "Tool search"):
    /// `None` renders logged searches as text.
    pub factory: Option<ToolSearchFactory>,
}

/// How a session continues another's history from a point.
pub enum Continuation {
    /// A person's rewind (`docs/events.md`, "Rewind").
    Rewind {
        /// Fiber's note: files written, shell calls and jobs since the point.
        note: String,
        /// The old session's summary of the path after the point, when requested.
        summary: Option<String>,
        /// The latest name and pin, copied after `session_started`.
        name: Option<SessionNamed>,
    },
    /// A delegate's fork (`docs/delegates.md`, "Forks").
    Fork {
        /// The parent session and its delegate job.
        parent: Parent,
    },
}

impl Loop {
    /// Starts a rewind or fork with the old log folded through `start.from`.
    /// A rewind carries its note, summary and name; a fork carries its parent.
    /// Both inherit the worktree, start without jobs and send the latest
    /// logged preamble verbatim, without writing a new `preamble_built`.
    /// Everything a new loop runs on rides [`crate::Session`].
    pub fn rewound(session: Session, start: Rewound) -> Result<Self, Error> {
        let (parent, rewind, name, reason) = match start.how {
            Continuation::Rewind {
                note,
                summary,
                name,
            } => (
                None,
                Some(Rewind { summary, note }),
                name,
                contract::hook::StartReason::Rewind,
            ),
            Continuation::Fork { parent } => {
                (Some(parent), None, None, contract::hook::StartReason::Fork)
            }
        };
        session.log.append(
            &Event::SessionStarted(SessionStarted {
                workspace: session.permissions.workspace.clone(),
                variables: variables(),
                parent,
                forked_from: Some(start.from),
                rewind,
                worktree: start.worktree,
                // A rewind or fork starts a new session: it is never a
                // supervised resume of the old one.
                supervised: false,
            }),
            None,
            None,
        )?;
        // The name goes with the conversation: a copy of the parent's
        // latest `session_named`, `by` included, so a pin holds
        // (`docs/events.md`, "Rewind").
        if let Some(named) = name {
            session
                .log
                .append(&Event::SessionNamed(named), None, None)?;
        }
        let mut folded = crate::resume::resumed(session.log.dir())?;
        let logged = folded.preamble.clone();
        // The new session owns none of the jobs the folded history holds:
        // a rewind stopped them, and a fork leaves them with its parent.
        // The handoff carry drops them too, so a handoff started in the new
        // session names no job. It continues the logged build verbatim, so it
        // writes no `model_changed` for a label the log changed after the
        // point (`docs/events.md`, "Rewind").
        folded.orphans = Vec::new();
        folded.settings = None;
        let mut rewound = Loop::resume(session, folded)?;
        rewound.handoff.carry.jobs.clear();
        // A rewind or fork starts for its own reason, whatever the
        // preamble it adopted was built for.
        rewound.lifecycle.reason = reason;
        if let Some(built) = logged {
            let tools = rewound
                .tools
                .values()
                .map(|(_, _, definition)| definition.clone())
                .collect();
            rewound.preamble = Some(Preamble::logged(&built, tools));
            // Without a factory, or on a model without deferral, no
            // request defers: the logged D is dropped and the first
            // request renders logged searches as text. With one and a
            // non-empty logged D, the rewind path builds the
            // search tool from the logged-D catalogue and registers it
            // before the first request, so an unchanged tool set sends its
            // bytes (`docs/tools.md`, "Tool search").
            if start.factory.is_none() || !rewound.provider.defers() {
                if let Some(preamble) = rewound.preamble.as_mut() {
                    preamble.deferred = None;
                }
            } else if let Some(deferred) = rewound
                .preamble
                .as_ref()
                .and_then(|preamble| preamble.deferred.clone())
                .filter(|deferred| !deferred.is_empty())
            {
                let (catalogue, sources) =
                    crate::prompt::catalogue_and_sources(&rewound.tools, &deferred);
                if let Some(factory) = start.factory.as_ref() {
                    let search = factory(&catalogue, &sources);
                    let definition = search.definition();
                    let name = definition.name.clone();
                    if !rewound.tools.contains_key(&name) {
                        rewound.tools.insert(
                            name.clone(),
                            ("builtin".to_owned(), search, definition.clone()),
                        );
                        // The insert rides the loop until the next build
                        // drops it, as a build's own insert does.
                        rewound.inserted_search = Some(name);
                        // The rewound preamble wires the rebuilt search in
                        // name order, so the first request declares it as
                        // built from the logged D.
                        if let Some(preamble) = rewound.preamble.as_mut() {
                            // The logged tools arrive in name order, so the
                            // pushed search sorts into place; no manual
                            // index to misplace.
                            preamble.tools.push(definition);
                            preamble.tools.sort_by(|a, b| a.name.cmp(&b.name));
                        }
                    }
                }
            }
            rewound.chosen = built.thinking.as_ref().and_then(|level| level.parse().ok());
            rewound.set_trigger();
        }
        Ok(rewound)
    }
}

/// The first turn's task for a fork, with its parent's identity and depth.
/// Workspace isolation is supplied separately; the inherited workspace adds no text.
pub fn fork_task(parent: &contract::SessionId, depth: u32, prompt: &str) -> String {
    fill(
        message("fork-task"),
        &[
            ("parent_session_id", &parent.0),
            ("depth", &depth.to_string()),
            ("workspace", ""),
            ("unattended", message("unattended")),
            ("prompt", prompt),
        ],
    )
}

/// The point `new` continues from, from the old log's last line: `seq`
/// counts in the old session's log, or in `from_session_id`'s when an
/// ancestor point names one. Anything else is [`Error::NotRewound`].
pub fn rewound_from(
    last: Option<Envelope>,
    new: &SessionId,
    old: &SessionId,
) -> Result<(Point, Option<String>), Error> {
    let not_rewound = || Error::NotRewound {
        old: old.clone(),
        new: new.clone(),
    };
    let Some(last) = last else {
        return Err(not_rewound());
    };
    if last.kind != "rewound" {
        return Err(not_rewound());
    }
    let rewound: contract::events::Rewound =
        serde_json::from_value(serde_json::Value::Object(last.payload))
            .map_err(|_| not_rewound())?;
    if rewound.new_session_id != *new {
        return Err(not_rewound());
    }
    Ok((
        Point {
            session_id: rewound.from_session_id.unwrap_or_else(|| old.clone()),
            seq: rewound.seq,
        },
        rewound.summary,
    ))
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
            deferred: Some(
                built
                    .tools
                    .iter()
                    .filter(|tool| tool.deferred)
                    .map(|tool| tool.name.clone())
                    .collect(),
            ),
            // The logged preamble carries no insert of its own: the rewind
            // path records the search it builds on the loop when due.
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

/// The summary as one user message between the history and Fiber's note.
pub(crate) fn summary_input(rewind: &Rewind) -> Option<Input> {
    rewind.summary.as_ref().map(|summary| Input::User {
        text: summary.clone(),
        images: Vec::new(),
    })
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
