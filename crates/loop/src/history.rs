//! The history fold over a session's chain of logs
//! (`docs/events.md`, "Rewind"): the parent logs folded to their points,
//! root first, then the own log. A resume folds the whole chain, so a
//! resumed rewound session keeps its history.

use std::collections::HashMap;
use std::path::Path;

use contract::events::{DecidedBy, Decision, Event, ModelSettings, Outcome};
use contract::{Envelope, Seq, TurnId};
use log::{Log, Segment};
use serde_json::Value;

use crate::Error;
use crate::handoff::Carry;
use crate::resume::Resumed;
use crate::reviewer::render_reviewed;

/// The kinds the fold reads a payload of. Every other line's envelope is
/// read and its payload never parsed.
const FOLDED: &[&str] = &[
    "session_started",
    "usage_recorded",
    "preamble_built",
    "model_changed",
    "permission_resolved",
    "turn_started",
    "steering_applied",
    "tool_call_requested",
    "job_started",
    "job_completed",
    "rewound",
    "opening_message",
    "handoff_completed",
    "reviewer_kept",
    "repository_code_offered",
    "repository_code_resolved",
];

/// Folds `segments`, root first, in one pass, one line at a time, holding
/// no parsed vector of them: the session, the root, the workspace, the
/// model, the credential label, the session-wide state, and the window a
/// resume reads. Each parent segment folds only to its point; the own
/// segment folds whole. The usage ledger and the reviewer's session block
/// count fold only the own segment (`docs/events.md`, "Rewind"): one
/// session's spend and blocks are its own. A chain with no segment, or an
/// own log with no `session_started`, is corrupt, as is a folded line
/// whose payload does not read as its kind.
pub(crate) fn fold(segments: &[Segment]) -> Result<Resumed, Error> {
    let first = segments.first().ok_or(Error::NoSessionStarted)?;
    let last = segments.last().ok_or(Error::NoSessionStarted)?;
    let mut resumed = Resumed {
        session: last.session_id.0.clone(),
        root: first.session_id.0.clone(),
        // A placeholder until the own `session_started` below writes
        // the real one: the error returns first, so it never leaves.
        workspace: String::new(),
        worktree: None,
        model: None,
        credential: None,
        thinking: None,
        settings: None,
        window: (0, 0),
        end: 0,
        seed: Carry::default(),
        ledger: crate::usage::Ledger::default(),
        grants: Vec::new(),
        session_blocks: 0,
        // The reviewer's input follows the session's handoffs
        // (`docs/permissions.md`, "At a handoff").
        reviewed: Vec::new(),
        orphans: Vec::new(),
        offers: crate::offer::Folded::default(),
        preamble: None,
    };
    let mut pass = Pass {
        own_segment: false,
        index: 0,
        workspace: None,
        // A parent segment's own first-seen generations, so a correction
        // there keeps the model its call was first recorded at, as the
        // ledger does for the own segment. Never written anywhere.
        parent_ledger: crate::usage::Ledger::default(),
        // The jobs running and the session log's path as of the line read.
        running: Carry::default(),
        // Each turn's latest `turn_started` since the last completed
        // handoff, with the state at it: a later handoff names one of
        // them. The position names its segment, so a turn in a parent
        // and one in the child never share it.
        starts: HashMap::new(),
        orphans: crate::jobs::Orphans::default(),
    };
    let mut end = 0;
    let Some(own) = segments.len().checked_sub(1) else {
        return Err(Error::NoSessionStarted);
    };
    for (index, segment) in segments.iter().enumerate() {
        pass.own_segment = index == own;
        pass.index = index;
        if pass.own_segment {
            for line in log::lines(&segment.dir)? {
                let line = line?;
                end += 1;
                let seq = line.seq.as_ref().map(|seq| seq.0);
                // Past the point the log is unread (`docs/events.md`,
                // "Resume"): breaking here leaves those lines unparsed,
                // however corrupt they are.
                if segment
                    .to
                    .as_ref()
                    .is_some_and(|to| seq.is_some_and(|seq| seq > to.0))
                {
                    break;
                }
                fold_line(&line, &mut resumed, &mut pass)?;
                // The point is inclusive (`docs/events.md`, "Rewind"):
                // its own line is the last one read.
                if segment
                    .to
                    .as_ref()
                    .is_some_and(|to| seq.is_some_and(|seq| seq == to.0))
                {
                    break;
                }
            }
        } else {
            for line in segment.lines(0)? {
                fold_line(&line, &mut resumed, &mut pass)?;
            }
        }
    }
    let Some(workspace) = pass.workspace.take() else {
        return Err(Error::NoSessionStarted);
    };
    resumed.workspace = workspace;
    resumed.orphans = pass.orphans.finish();
    resumed.end = end;
    Ok(resumed)
}

/// What [`fold_line`] carries through the pass besides the [`Resumed`] it
/// builds: the own segment's workspace until it moves, and the state
/// that only exists mid-pass. Every field is owned.
struct Pass {
    own_segment: bool,
    index: usize,
    workspace: Option<String>,
    parent_ledger: crate::usage::Ledger,
    running: Carry,
    starts: HashMap<TurnId, ((usize, u64), Carry)>,
    orphans: crate::jobs::Orphans,
}

/// Folds one line of segment `pass.index` into `resumed`: the workspace
/// from the own segment's `session_started`, the model, the credential
/// label and the thinking, the session-wide state, and the handoff
/// window. The ledger and the reviewer's block count fold only the own
/// segment.
fn fold_line(line: &Envelope, resumed: &mut Resumed, pass: &mut Pass) -> Result<(), Error> {
    if !line.is_durable() || !FOLDED.contains(&line.kind.as_str()) {
        return Ok(());
    }
    let Some(event) = Event::from_envelope(line).map_err(Error::Unreadable)? else {
        return Ok(());
    };
    render_reviewed(
        &mut resumed.reviewed,
        &event,
        line.action_id.as_ref(),
        line.seq,
    );
    pass.orphans.fold(&event);
    resumed.offers.fold(&event);
    pass.running.fold_jobs(&event);
    if let Event::SessionStarted(started) = &event
        && pass.own_segment
        && pass.workspace.is_none()
    {
        pass.workspace = Some(started.workspace.clone());
        resumed.worktree = started.worktree.clone();
    } else if let Event::UsageRecorded(recorded) = &event {
        // The call's model is not the session's: the line may be a
        // delegate's copy, a reviewer's call or an extension's.
        let ledger = if pass.own_segment {
            &mut resumed.ledger
        } else {
            &mut pass.parent_ledger
        };
        ledger.record(recorded);
    } else if let Event::PreambleBuilt(built) = &event {
        resumed.model = Some(built.model.clone());
        resumed.credential.clone_from(&built.credential);
        resumed.preamble = Some(built.clone());
        // The settings the log last recorded, for a resume that switches
        // the credential label (`docs/model-routing.md`, "Which credential
        // a session uses").
        resumed.settings = Some(ModelSettings {
            model: built.model.clone(),
            thinking: built.thinking.clone(),
            cache_lifetime: built.cache_lifetime,
            credential: built.credential.clone(),
        });
    } else if let Event::ModelChanged(changed) = &event {
        resumed.model = Some(changed.after.model.clone());
        resumed.credential.clone_from(&changed.after.credential);
        resumed.thinking = changed.after.thinking.clone();
        resumed.settings = Some(changed.after.clone());
    } else if let Event::PermissionResolved(resolved) = &event {
        if let Some(grant) = &resolved.grant {
            resumed.grants.push(grant.clone());
        }
        // A model's block, a reviewer failure and a denial with no
        // reviewer set up each count, as they do live. The
        // spending-budget denial is `decided_by: budget` and does not.
        // debt: undercounts session blocks whose escalation a person
        // answered or a cancel ended; fixed when the log records blocks.
        if pass.own_segment
            && resolved.decision == Decision::Deny
            && matches!(
                resolved.decided_by,
                DecidedBy::Reviewer | DecidedBy::NoReviewer
            )
        {
            resumed.session_blocks += 1;
        }
    } else if let Event::OpeningMessage(message) = &event {
        pass.running
            .session_log
            .clone_from(&message.environment.session_log);
    } else if let Event::TurnStarted(_) = &event {
        if let (Some(turn), Some(seq)) = (&line.turn_id, line.seq) {
            let state = Carry {
                jobs: pass.running.jobs.clone(),
                session_log: pass.running.session_log.clone(),
                ..Carry::default()
            };
            pass.starts
                .insert(turn.clone(), ((pass.index, seq.0), state));
        }
    } else if let Event::HandoffCompleted(done) = &event
        && done.outcome == Outcome::Completed
    {
        let turn = line.turn_id.as_ref();
        let (window, seed) = turn
            .and_then(|turn| pass.starts.get(turn))
            .cloned()
            .unwrap_or_default();
        resumed.window = window;
        resumed.seed = seed;
        // Only this turn can complete another handoff before its
        // next `turn_started`.
        pass.starts.retain(|started, _| Some(started) == turn);
    }
    Ok(())
}

/// Folds `dir`'s chain to `point`: the history of any session on the
/// chain, the old session or an ancestor, continued from `point`. The
/// model, credential and thinking are the parent's latest build at or
/// before the point, when there is one (`docs/events.md`, "Rewind"): an
/// explicitly chosen model or thinking level is kept even before the old
/// session's first request.
pub fn forked(dir: &Path, point: Seq) -> Result<Resumed, Error> {
    let mut folded = fold(&log::history_to(dir, point)?)?;
    if let Some(built) = &folded.preamble {
        folded.model = Some(built.model.clone());
        folded.credential.clone_from(&built.credential);
        folded.thinking = built.thinking.clone();
    }
    Ok(folded)
}

/// The window's lines across the chain: parent segments through
/// [`Segment::lines`], with every image and PDF part's `path` (and each
/// PDF's pages) made absolute against that segment's directory in memory
/// only, and the own log
/// through [`Log::range`] as a plain resume reads it. The body carries an
/// image's bytes, not its path, so byte identity holds.
pub(crate) fn read_window(
    log: &Log,
    window: (usize, u64),
    end: u64,
) -> Result<Vec<Envelope>, Error> {
    let segments = log::history(log.dir())?;
    let Some(own) = segments.len().checked_sub(1) else {
        return Err(Error::NoSessionStarted);
    };
    let mut lines = Vec::new();
    for (index, segment) in segments.iter().enumerate() {
        if index < window.0 {
            continue;
        }
        let from = if index == window.0 { window.1 } else { 0 };
        if index == own {
            let max = if index == window.0 {
                end.saturating_sub(from)
            } else {
                end
            };
            lines.extend(log.range(from, usize::try_from(max).unwrap_or(usize::MAX))?);
        } else {
            for mut line in segment.lines(from)? {
                rewrite_images(&mut line, &segment.dir);
                lines.push(line);
            }
        }
    }
    Ok(lines)
}

/// Makes every image and PDF part's `path` in `line` absolute against
/// `dir`: an image or PDF part's path is relative to the session directory
/// that wrote it,
/// and a request reads it under its own. Only the content parts a
/// protocol reads as images: a person's message, a steering message and
/// a tool result. Tool-call arguments are never rewritten, even when
/// they are shaped like an image: the history replays the call byte for
/// byte. In memory only; nothing is written or copied.
fn rewrite_images(line: &mut Envelope, dir: &Path) {
    match line.kind.as_str() {
        "turn_started" => {
            if let Some(items) = line.payload.get_mut("input").and_then(Value::as_array_mut) {
                for item in items {
                    if let Some(content) = item.get_mut("content").and_then(Value::as_array_mut) {
                        rewrite_parts(content, dir);
                    }
                }
            }
        }
        "steering_applied" | "tool_call_completed" => {
            if let Some(content) = line
                .payload
                .get_mut("content")
                .and_then(Value::as_array_mut)
            {
                rewrite_parts(content, dir);
            }
        }
        _ => {}
    }
}

/// Makes every image and PDF part's `path` in `parts` absolute against
/// `dir`, including each PDF part's rendered pages.
fn rewrite_parts(parts: &mut [Value], dir: &Path) {
    for part in parts {
        let kind = part
            .as_object()
            .and_then(|map| map.get("type").and_then(Value::as_str))
            .unwrap_or("")
            .to_owned();
        if kind != "image" && kind != "pdf" {
            continue;
        }
        make_absolute(part, dir);
        if kind == "pdf"
            && let Some(pages) = part
                .as_object_mut()
                .and_then(|map| map.get_mut("pages").and_then(Value::as_array_mut))
        {
            for page in pages.iter_mut() {
                make_absolute(page, dir);
            }
        }
    }
}

/// Makes the part's own `path` absolute against `dir` when it is relative.
fn make_absolute(part: &mut Value, dir: &Path) {
    let path = part
        .as_object()
        .and_then(|map| map.get("path")?.as_str().map(str::to_owned));
    if let Some(path) = path
        && std::path::Path::new(&path).is_relative()
        && let Some(map) = part.as_object_mut()
    {
        map.insert(
            "path".to_owned(),
            Value::String(dir.join(path).display().to_string()),
        );
    }
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;
