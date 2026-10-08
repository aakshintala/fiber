//! Starting a session that continues another from a step boundary
//! (`docs/events.md`, "Rewind"): the inherited preamble, the note, and the
//! old session's worktree.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::Receiver;

use contract::events::{Event, Rewind, SessionStarted, ToolCallRequested};
use contract::provider::{Input, ToolDefinition};
use contract::shapes::{Effect, Point, Worktree};
use contract::{ActionId, Seq};
use log::Log;
use serde_json::Value;

use crate::conversation::MESSAGES_MD;
use crate::prompt::{body, fill};
use crate::{Error, Loop, Model, Permissions, Preamble, PromptInputs, variables};

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
}

impl Loop {
    /// Starts a session that continues another from `start.from`, whose
    /// history is the old log folded to the point: `session_started`
    /// carries `forked_from`, the note and the jobs handed over, and the
    /// old session's worktree, with no `parent` and no summary. The chain
    /// now includes this line, so the fold gives the new session the old
    /// log's history to the point. The new session adopts no job, so it
    /// clears its orphans, then builds through [`Loop::resume`], sending
    /// the fold's latest `preamble_built` verbatim as its first request.
    /// It writes no `preamble_built`: nothing was built.
    /// `Loop::start`'s arguments otherwise.
    #[allow(
        clippy::too_many_arguments,
        reason = "the session's whole start: what it continues from rides first"
    )]
    pub fn rewound(
        log: Arc<Log>,
        start: Rewound,
        provider: Arc<dyn contract::provider::Provider>,
        model: Model,
        prompt: PromptInputs,
        inbox: Receiver<contract::inbox::Delivery>,
        tools: Vec<(String, Arc<dyn contract::tool::Tool>)>,
        permissions: Permissions,
    ) -> Result<Self, Error> {
        log.append(
            &Event::SessionStarted(SessionStarted {
                workspace: permissions.workspace.clone(),
                variables: variables(),
                parent: None,
                forked_from: Some(start.from),
                rewind: Some(Rewind {
                    summary: None,
                    note: start.note,
                    jobs: Vec::new(),
                }),
                worktree: start.worktree,
            }),
            None,
            None,
        )?;
        let mut folded = crate::resume::resumed(log.dir())?;
        let logged = folded.preamble.clone();
        // The new session adopts no job: nothing it folds can orphan.
        folded.orphans = Vec::new();
        let mut rewound = Loop::resume(
            log,
            folded,
            provider,
            model,
            prompt,
            inbox,
            tools,
            permissions,
        )?;
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

/// Builds the note for the point `point` in the log in `dir`: the files
/// the session's tools wrote after the point and the shell calls after it
/// that may have changed files, from the effects on each
/// `tool_call_started` after the point (`docs/events.md`, "Rewind"). The
/// text comes only from logged fields, so it is the same bytes whenever it
/// is rendered.
pub fn rewind_note(dir: &Path, point: Seq) -> Result<String, Error> {
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
    Ok(note(&paths, &calls))
}

/// The note text over `paths` written and `calls` run after the point.
fn note(paths: &[String], calls: &[String]) -> String {
    let changes = match (paths.is_empty(), calls.is_empty()) {
        (true, true) => body(MESSAGES_MD, "rewind-unchanged"),
        _ => {
            let mut parts = Vec::new();
            if !paths.is_empty() {
                let listed = paths
                    .iter()
                    .map(|path| format!("- {path}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                parts.push(fill(
                    &body(MESSAGES_MD, "rewind-written"),
                    &[("paths", &listed)],
                ));
            }
            if !calls.is_empty() {
                parts.push(fill(
                    &body(MESSAGES_MD, "rewind-ran"),
                    &[("calls", &calls.join("\n"))],
                ));
            }
            parts.join("\n\n")
        }
    };
    fill(&body(MESSAGES_MD, "rewind-note"), &[("changes", &changes)])
}

#[cfg(test)]
#[path = "rewind_tests.rs"]
mod tests;
