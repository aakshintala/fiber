//! The history fold over a session's chain of logs
//! (`docs/events.md`, "Rewind"): the parent logs folded to their points,
//! root first, then the own log. A resume folds the whole chain, so a
//! resumed rewound session keeps its history.

use std::collections::HashMap;
use std::path::Path;

use contract::events::{DecidedBy, Decision, Event, Outcome};
use contract::{Envelope, Seq, TurnId};
use log::{Log, Segment};
use serde_json::Value;

use crate::Error;
use crate::handoff::Carry;
use crate::resume::Resumed;
use crate::reviewer::{Reviewed, render_reviewed};

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
    let root = first.session_id.0.clone();
    let session = last.session_id.0.clone();
    let mut workspace = None;
    let mut worktree = None;
    let mut preamble = None;
    let mut model = None;
    let mut credential = None;
    let mut thinking = None;
    let mut ledger = crate::usage::Ledger::default();
    // A parent segment's own first-seen generations, so a correction there
    // keeps the model its call was first recorded at, as the ledger does
    // for the own segment. Never written anywhere.
    let mut parent_ledger = crate::usage::Ledger::default();
    let mut grants = Vec::new();
    let mut session_blocks = 0;
    // The reviewer's input follows the session's handoffs
    // (`docs/permissions.md`, "At a handoff").
    let mut reviewed = Vec::new();
    let mut orphans = crate::jobs::Orphans::default();
    let mut offers = crate::offer::Folded::default();
    // The jobs running and the session log's path as of the line read.
    let mut running = Carry::default();
    // Each turn's latest `turn_started` since the last completed handoff,
    // with the state at it: a later handoff names one of them. The
    // position names its segment, so a turn in a parent and one in the
    // child never share it.
    let mut starts: HashMap<TurnId, ((usize, u64), Carry)> = HashMap::new();
    let mut window = ((0, 0), Carry::default());
    let mut end = 0;
    let Some(own) = segments.len().checked_sub(1) else {
        return Err(Error::NoSessionStarted);
    };
    let mut fold = Fold {
        workspace: &mut workspace,
        worktree: &mut worktree,
        preamble: &mut preamble,
        model: &mut model,
        credential: &mut credential,
        thinking: &mut thinking,
        ledger: &mut ledger,
        parent_ledger: &mut parent_ledger,
        own_segment: false,
        grants: &mut grants,
        session_blocks: &mut session_blocks,
        reviewed: &mut reviewed,
        orphans: &mut orphans,
        offers: &mut offers,
        running: &mut running,
        starts: &mut starts,
        window: &mut window,
        index: 0,
    };
    for (index, segment) in segments.iter().enumerate() {
        fold.own_segment = index == own;
        fold.index = index;
        if fold.own_segment {
            for line in log::lines(&segment.dir)? {
                let line = line?;
                end += 1;
                // The point is inclusive: the history holds the lines
                // with `seq <= to` (`docs/events.md`, "Rewind").
                if segment
                    .to
                    .as_ref()
                    .is_some_and(|to| line.seq.as_ref().is_some_and(|seq| seq.0 > to.0))
                {
                    continue;
                }
                fold_line(&line, &mut fold)?;
            }
        } else {
            for line in segment.lines(0)? {
                fold_line(&line, &mut fold)?;
            }
        }
    }
    let Some(workspace) = workspace else {
        return Err(Error::NoSessionStarted);
    };
    let (window, seed) = window;
    Ok(Resumed {
        session,
        root,
        workspace,
        worktree,
        model,
        credential,
        thinking,
        window,
        end,
        seed,
        ledger,
        grants,
        session_blocks,
        reviewed,
        orphans: orphans.finish(),
        offers,
        preamble,
    })
}

/// What [`fold_line`] threads through one line.
struct Fold<'a> {
    workspace: &'a mut Option<String>,
    worktree: &'a mut Option<contract::shapes::Worktree>,
    preamble: &'a mut Option<contract::events::PreambleBuilt>,
    model: &'a mut Option<String>,
    credential: &'a mut Option<String>,
    thinking: &'a mut Option<String>,
    ledger: &'a mut crate::usage::Ledger,
    parent_ledger: &'a mut crate::usage::Ledger,
    own_segment: bool,
    grants: &'a mut Vec<contract::events::Grant>,
    session_blocks: &'a mut u64,
    reviewed: &'a mut Vec<Reviewed>,
    orphans: &'a mut crate::jobs::Orphans,
    offers: &'a mut crate::offer::Folded,
    running: &'a mut Carry,
    starts: &'a mut HashMap<TurnId, ((usize, u64), Carry)>,
    window: &'a mut ((usize, u64), Carry),
    index: usize,
}

/// Folds one line of segment `fold.index`: the workspace from the own
/// segment's `session_started`, the model, the credential label and the
/// thinking, the session-wide state, and the handoff window. The ledger
/// and the reviewer's block count fold only the own segment.
fn fold_line(line: &Envelope, fold: &mut Fold<'_>) -> Result<(), Error> {
    if !line.is_durable() || !FOLDED.contains(&line.kind.as_str()) {
        return Ok(());
    }
    let Some(event) = Event::from_envelope(line).map_err(Error::Unreadable)? else {
        return Ok(());
    };
    render_reviewed(fold.reviewed, &event, line.action_id.as_ref(), line.seq);
    fold.orphans.fold(&event);
    fold.offers.fold(&event);
    fold.running.fold_jobs(&event);
    if let Event::SessionStarted(started) = &event
        && fold.own_segment
        && fold.workspace.is_none()
    {
        *fold.workspace = Some(started.workspace.clone());
        *fold.worktree = started.worktree.clone();
    } else if let Event::UsageRecorded(recorded) = &event {
        // A correction keeps the model the call was first recorded at,
        // which may be an earlier model than the latest one.
        let ledger = if fold.own_segment {
            &mut *fold.ledger
        } else {
            &mut *fold.parent_ledger
        };
        if !ledger.record(recorded) {
            *fold.model = Some(recorded.model.clone());
        }
    } else if let Event::PreambleBuilt(built) = &event {
        fold.credential.clone_from(&built.credential);
        *fold.preamble = Some(built.clone());
    } else if let Event::ModelChanged(changed) = &event {
        *fold.model = Some(changed.after.model.clone());
        fold.credential.clone_from(&changed.after.credential);
        *fold.thinking = changed.after.thinking.clone();
    } else if let Event::PermissionResolved(resolved) = &event {
        if let Some(grant) = &resolved.grant {
            fold.grants.push(grant.clone());
        }
        // A model's block, a reviewer failure and a denial with no
        // reviewer set up each count, as they do live. The
        // spending-budget denial is `decided_by: budget` and does not.
        // debt: undercounts session blocks whose escalation a person
        // answered or a cancel ended; fixed when the log records blocks.
        if fold.own_segment
            && resolved.decision == Decision::Deny
            && matches!(
                resolved.decided_by,
                DecidedBy::Reviewer | DecidedBy::NoReviewer
            )
        {
            *fold.session_blocks += 1;
        }
    } else if let Event::OpeningMessage(message) = &event {
        fold.running
            .session_log
            .clone_from(&message.environment.session_log);
    } else if let Event::TurnStarted(_) = &event {
        if let (Some(turn), Some(seq)) = (&line.turn_id, line.seq) {
            let state = Carry {
                jobs: fold.running.jobs.clone(),
                session_log: fold.running.session_log.clone(),
                ..Carry::default()
            };
            fold.starts
                .insert(turn.clone(), ((fold.index, seq.0), state));
        }
    } else if let Event::HandoffCompleted(done) = &event
        && done.outcome == Outcome::Completed
    {
        let turn = line.turn_id.as_ref();
        *fold.window = turn
            .and_then(|turn| fold.starts.get(turn))
            .cloned()
            .unwrap_or_default();
        // Only this turn can complete another handoff before its
        // next `turn_started`.
        fold.starts.retain(|started, _| Some(started) == turn);
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
/// [`Segment::lines`], with every image part's `path` made absolute
/// against that segment's directory in memory only, and the own log
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

/// Makes every image part's `path` in `line` absolute against `dir`: an
/// image part's path is relative to the session directory that wrote it,
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

/// Makes every image part's `path` in `parts` absolute against `dir`.
fn rewrite_parts(parts: &mut [Value], dir: &Path) {
    for part in parts {
        let path = part.as_object().and_then(|map| {
            if map.get("type").and_then(Value::as_str) != Some("image") {
                return None;
            }
            map.get("path")?.as_str().map(str::to_owned)
        });
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
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;
