//! The `rewind` driver command (`docs/events.md`, "Rewind"): its refusals,
//! the point in this session's log or an ancestor's, and `rewound` as the
//! last line.

use std::collections::HashMap;
use std::path::PathBuf;

use contract::commands::RewindArgs;
use contract::events::{CommandResult, Event, Rewound};
use contract::inbox::{Ack, Delivery, Rejection};
use contract::{ActionId, Envelope, ErrorCode, Seq, SessionId};

use crate::inbox::{TurnInput, reject};
use crate::{Error, Loop};

/// `rewind` refused while a turn runs or one is about to start: rewind
/// once it ends.
pub(crate) const TURN_RUNNING: &str = "A turn is running; rewind once it ends.";

/// Every delivery refused once `rewound` is set: the session takes no more
/// commands.
pub(crate) const REWOUND_CLOSING: &str = "The session was rewound and takes no more commands.";

/// `rewind` on a delegate: its parent forks again instead.
const DELEGATE: &str = "A delegate cannot be rewound.";

/// `rewind` asking for a summary: no summary is built in this Fiber yet.
const NO_SUMMARY: &str = "Summaries are not built in this Fiber yet.";

/// `rewind` while jobs run: stop them before rewinding.
const JOBS_RUNNING: &str = "Jobs are running; stop them before rewinding.";

/// `rewind` with no turn to rewind to.
const NO_TURN: &str = "This session has no turn to rewind to.";

/// The log whose lines the point counts in: this session's own log, or an
/// ancestor's on its `forked_from` chain.
pub(crate) struct Target {
    /// The session whose log `seq` counts in.
    pub(crate) session: SessionId,
    /// That session's directory.
    pub(crate) dir: PathBuf,
    /// The last line of that log this session's history holds; `None`
    /// holds all of it.
    pub(crate) bound: Option<u64>,
}

/// Whether `text` is a session id: `s_` followed by 16 lowercase hex
/// digits. Checked before any path is built from it, so a path never
/// leaves `sessions/`.
fn valid_session_id(text: &str) -> bool {
    let hex = text.strip_prefix("s_").unwrap_or("");
    hex.len() == 16
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl Loop {
    /// Takes `rewind` while idle (`docs/events.md`, "Rewind"): the refusals
    /// in order, then `rewound` as the last line, naming the minted new
    /// session and the point, and the new session's id as the answer. Every
    /// refusal leaves the log byte for byte as it was, mints nothing and
    /// starts nothing. On success the loop stops late-cost lookups and
    /// writes what settled, appends `rewound`, sets `rewound`, and only
    /// then accepts, so the acknowledgement is sent after `rewound` is
    /// written.
    pub(crate) fn take_rewind(
        &mut self,
        args: RewindArgs,
        ack: Ack,
        input: &TurnInput,
    ) -> Result<(), Error> {
        if self.closing {
            reject(ack, ErrorCode::Closing, crate::inbox::CLOSING);
            return Ok(());
        }
        // A turn about to start: the drain already admitted a prompt or a
        // steer.
        if !input.pieces.is_empty() {
            reject(ack, ErrorCode::Busy, TURN_RUNNING);
            return Ok(());
        }
        let own_lines = log::read(self.log.dir())?;
        let Some(first) = own_lines.first() else {
            return Err(Error::NoSessionStarted);
        };
        if first.kind != "session_started" {
            return Err(Error::NoSessionStarted);
        }
        let Some(Event::SessionStarted(started)) =
            Event::from_envelope(first).map_err(Error::Unreadable)?
        else {
            return Err(Error::NoSessionStarted);
        };
        if started.parent.is_some() {
            reject(ack, ErrorCode::DelegateSession, DELEGATE);
            return Ok(());
        }
        let own = first.session_id.clone();
        let target = match self.target(&args, &own)? {
            Ok(target) => target,
            Err(rejection) => {
                (ack.0)(Err(rejection));
                return Ok(());
            }
        };
        if args.summarise {
            reject(ack, ErrorCode::SummaryFailed, NO_SUMMARY);
            return Ok(());
        }
        let running = self.running();
        if !running.is_empty() {
            reject(ack, ErrorCode::Busy, JOBS_RUNNING);
            return Ok(());
        }
        if !args.adopt.is_empty() {
            let adopted = args
                .adopt
                .iter()
                .map(|job| job.0.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            reject(
                ack,
                ErrorCode::StaleRequest,
                &format!("The adopted jobs are not running: {adopted}."),
            );
            return Ok(());
        }
        let lines = log::read(&target.dir)?;
        let point = match point_for(&lines, &target, args.seq) {
            Ok(point) => point,
            Err(rejection) => {
                (ack.0)(Err(rejection));
                return Ok(());
            }
        };
        self.write_last_settled()?;
        let new_session_id = SessionId(crate::mint("s_"));
        // `rewound` names the point's session only for an ancestor point:
        // for this session's own point the key is absent.
        let from_session_id = (target.session != own).then(|| target.session.clone());
        self.log.append(
            &Event::Rewound(Rewound {
                new_session_id: new_session_id.clone(),
                seq: Seq(point),
                from_session_id,
                jobs: Vec::new(),
            }),
            None,
            None,
        )?;
        self.rewound = true;
        (ack.0)(Ok(Some(CommandResult::Rewind { new_session_id })));
        Ok(())
    }

    /// The log the point counts in: this session's own log, or the ancestor
    /// `from_session_id` names on its `forked_from` chain. A session off
    /// the chain is refused before it is opened, locked or read.
    fn target(
        &self,
        args: &RewindArgs,
        own: &SessionId,
    ) -> Result<Result<Target, Rejection>, Error> {
        // Absent, or this session's own id: the point is in this
        // session's own log.
        let from = args.from_session_id.as_ref().filter(|from| *from != own);
        match from {
            None => Ok(Ok(Target {
                session: own.clone(),
                dir: self.log.dir().to_path_buf(),
                bound: None,
            })),
            Some(from) => {
                if !valid_session_id(&from.0) {
                    return Ok(Err(Rejection {
                        code: ErrorCode::InvalidArguments,
                        message: format!(
                            "{} is not a session id: one is `s_` followed by 16 lowercase hex digits.",
                            from.0
                        ),
                    }));
                }
                let chain = log::history(self.log.dir())?;
                // The own log is the last segment: only an ancestor names
                // the point's log.
                let found = chain
                    .iter()
                    .rev()
                    .skip(1)
                    .find(|segment| segment.session_id == *from);
                match found {
                    Some(segment) => Ok(Ok(Target {
                        session: segment.session_id.clone(),
                        dir: segment.dir.clone(),
                        bound: segment.to.map(|to| to.0),
                    })),
                    None => Ok(Err(Rejection {
                        code: ErrorCode::InvalidArguments,
                        message: format!(
                            "Session {} is neither this session nor one it continues. To rewind it, send `rewind` for it through the hub.",
                            from.0
                        ),
                    })),
                }
            }
        }
    }
}

/// Refuses `delivery` once `rewound` is set: every acknowledgement is
/// answered `closing`, everything else is dropped, and nothing is written.
pub(crate) fn refuse_after_rewound(delivery: Delivery) {
    match delivery {
        Delivery::Prompt(_, ack)
        | Delivery::Steer(_, ack)
        | Delivery::SteerDrop(_, ack)
        | Delivery::Handoff(_, _, ack)
        | Delivery::Model(_, ack)
        | Delivery::Reply(_, ack)
        | Delivery::Close(ack)
        | Delivery::Resolved(_, ack)
        | Delivery::Rewind(_, ack) => reject(ack, ErrorCode::Closing, REWOUND_CLOSING),
        Delivery::Job(_)
        | Delivery::JobLine(_)
        | Delivery::ExtensionExec(_)
        | Delivery::Interaction(_)
        | Delivery::ExtensionLog(_)
        | Delivery::Cancelled => {}
    }
}

/// The point `requested` names in the point log's `lines`, or the default
/// point: the latest `turn_started` at or before the bound, minus one. The
/// point is a step boundary: the log holds a `turn_started` or a
/// `step_started` just after it, and no tool call before it is answered
/// after it (`docs/events.md`, "Rewind"). A point past the bound is not in
/// this session's history.
pub(crate) fn point_for(
    lines: &[Envelope],
    target: &Target,
    requested: Option<Seq>,
) -> Result<u64, Rejection> {
    match requested {
        None => {
            let latest = lines
                .iter()
                .filter(|line| {
                    line.kind == "turn_started"
                        && target
                            .bound
                            .is_none_or(|to| line.seq.is_some_and(|seq| seq.0 <= to))
                })
                .filter_map(|line| line.seq.map(|seq| seq.0))
                .max();
            match latest.and_then(|start| start.checked_sub(1)) {
                Some(point) => Ok(point),
                None => Err(Rejection {
                    code: ErrorCode::InvalidArguments,
                    message: NO_TURN.to_owned(),
                }),
            }
        }
        Some(seq) => {
            if target.bound.is_some_and(|to| seq.0 > to) {
                return Err(Rejection {
                    code: ErrorCode::NotStepBoundary,
                    message: format!(
                        "Line {} of session {} is not in this session's history.",
                        seq.0, target.session.0
                    ),
                });
            }
            boundary(lines, seq.0)
        }
    }
}

/// Whether `point` is a step boundary in `lines`: the log holds a
/// `turn_started` or a `step_started` just after it. A point with no
/// representable successor has no next line, so it is none.
fn boundary(lines: &[Envelope], point: u64) -> Result<u64, Rejection> {
    let refused = || Rejection {
        code: ErrorCode::NotStepBoundary,
        message: format!(
            "Line {point} is not a step boundary: the start of a turn, just after the person's input, or just after a batch of tool results."
        ),
    };
    let next = point.checked_add(1).and_then(|after| {
        lines
            .iter()
            .find(|line| line.seq.is_some_and(|seq| seq.0 == after))
    });
    match next {
        Some(line) if line.kind == "turn_started" || line.kind == "step_started" => {}
        _ => return Err(refused()),
    }
    // When each call was requested: a completion after the point answers
    // a call at or before it only when its request is there too.
    let mut requested_at: HashMap<&ActionId, u64> = HashMap::new();
    for line in lines {
        if line.kind == "tool_call_requested"
            && let (Some(action), Some(seq)) = (&line.action_id, line.seq)
        {
            requested_at.insert(action, seq.0);
        }
    }
    for line in lines {
        if line.kind == "tool_call_completed"
            && line.seq.is_some_and(|seq| seq.0 > point)
            && let Some(action) = &line.action_id
            && requested_at.get(action).is_some_and(|at| *at <= point)
        {
            return Err(refused());
        }
    }
    Ok(point)
}

#[cfg(test)]
#[path = "command_tests.rs"]
mod tests;
