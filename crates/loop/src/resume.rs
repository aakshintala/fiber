//! Resuming a session (`docs/events.md`, "Resume"): rebuilding the loop's
//! state from the log and its configuration, nothing else. A suspended turn
//! (`fiber_exited` with `suspended_on`) re-raises its pending approval or
//! question under the same `request_id`, waits for a person's answer (or
//! refuses it as headless when no person can answer), finishes the turn,
//! and then runs the prompt as the next turn.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use contract::events::{
    DecidedBy, Decision, Event, Grant, JobCompleted, ModelSettings, ToolCallRequested,
    TurnCompleted, TurnOutcome,
};
use contract::{ActionId, Envelope, TurnId};

use crate::calls::{self, Asked, Decided};
use crate::cancel::Commit;
use crate::completion::resolved;
use crate::handoff::Carry;
use crate::reviewer::Reviewed;
use crate::suspend::Pending;
use crate::{Error, Loop, Session, Step};

/// The denial's reason for a request re-raised in a session nobody can
/// answer (`docs/events.md`, `permission_resolved`).
const NOBODY_TO_ANSWER: &str = "The session was resumed with nobody to answer.";

/// What a resume folds back over the session's chain of logs, root
/// first (`docs/events.md`, "Rewind"): the session, the root the cache
/// keys are built from, the workspace the own log's first
/// `session_started` recorded, the model the last `preamble_built` or
/// `model_changed` names, if any, and the session-wide state a resumed loop restores. The pass
/// also finds the window a resume reads: the lines from the last completed
/// handoff on (`docs/handoff.md`, "Resume").
pub struct Resumed {
    /// The own session's id, from its directory name: the diagnostic log
    /// and the reviewer key are built from it (`docs/prompt-cache.md`,
    /// "Rules for other areas").
    pub session: String,
    /// The root session's id, from its directory name: the cache keys are
    /// built from it (`docs/prompt-cache.md`, "Cache markers and keys").
    pub root: String,
    /// The first `session_started`'s workspace: a resumed session keeps it,
    /// wherever the resume runs (`docs/state.md`, "Sessions and resume").
    /// The own log's, when the session continues another.
    pub workspace: String,
    /// The model of the last `preamble_built` or `model_changed`; a
    /// `usage_recorded` names its call's model, which may be a delegate's
    /// or a reviewer's. The session's model, beating `--model`
    /// (`docs/model-routing.md`, "Choosing the model"). `None` when the log
    /// holds none.
    pub model: Option<String>,
    /// The last `preamble_built`'s credential label, or the last
    /// `model_changed`'s `after` one when it came later: the label a resumed
    /// session keeps, beating the configured one (`docs/model-routing.md`,
    /// "Which credential a session uses"). `None` when the log holds none.
    pub credential: Option<String>,
    /// The last `model_changed`'s `after.thinking`: a resumed session
    /// treats it as its own choice (`docs/model-routing.md`, "Thinking").
    /// `None` when the log holds none.
    pub thinking: Option<String>,
    /// The settings the log last recorded: the last `preamble_built`'s, or
    /// the last `model_changed`'s `after` when it came later. `None` when
    /// the log holds neither.
    pub(crate) settings: Option<ModelSettings>,
    /// The window's start, as a position in the chain: the segment and
    /// the `seq` the window starts at: the latest `turn_started` of the
    /// latest completed handoff's turn, before that handoff; the chain's
    /// start with no completed handoff, or none of its turn's
    /// `turn_started` lines.
    pub(crate) window: (usize, u64),
    /// How many lines the own log holds: the window ends there, before any
    /// line written after the pass.
    pub(crate) end: u64,
    /// The render state at the window start that the window's own lines do
    /// not rebuild: the jobs running and the session log's path.
    pub(crate) seed: Carry,
    /// Every `usage_recorded`, the whole session's.
    pub(crate) ledger: crate::usage::Ledger,
    /// Every grant a `permission_resolved` recorded.
    pub(crate) grants: Vec<Grant>,
    /// The reviewer's blocks this session.
    pub(crate) session_blocks: u64,
    /// What the reviewer is shown: the latest `reviewer_kept`'s messages,
    /// then what followed (`docs/permissions.md`, "At a handoff").
    pub(crate) reviewed: Vec<Reviewed>,
    /// The jobs started and never ended, each as its `orphaned` completion.
    pub(crate) orphans: Vec<JobCompleted>,
    /// The repository's offers: the session's skips and the pending offer.
    pub(crate) offers: crate::offer::Folded,
    /// The latest `preamble_built` in the chain: a rewound session sends
    /// it verbatim as its first request (`docs/events.md`, "Rewind").
    /// `None` when the chain holds none yet.
    pub(crate) preamble: Option<contract::events::PreambleBuilt>,
    /// The own log's `session_started.worktree`: the worktree the session
    /// runs in, when Fiber created one for it or for the session it
    /// continues (`docs/events.md`, `session_started`).
    pub worktree: Option<contract::shapes::Worktree>,
}

/// Folds the log in the session directory `dir` over its chain, root
/// first (`docs/events.md`, "Rewind"): the session, the root, the
/// workspace, the model, the credential label, the session-wide state,
/// and the window a resume reads. A log with no `session_started` is
/// corrupt, as is a line that is not an envelope or a folded line whose
/// payload does not read as its kind.
pub fn resumed(dir: &Path) -> Result<Resumed, Error> {
    crate::history::fold(&log::history(dir)?)
}

/// A turn cut short on a pending approval or question: what the finishing
/// turn writes back under the same turn id (`docs/invocation.md`,
/// "Lifecycle").
pub(crate) struct Suspended {
    /// The cut-short turn, which the finishing turn completes.
    pub(crate) turn: TurnId,
    /// The pending request, re-raised with the same `request_id`.
    pub(crate) pending: Pending,
    /// The call the request was raised for.
    pub(crate) action: ActionId,
    /// The calls with `tool_call_requested` after the log's last
    /// `assistant_message_started` and no `tool_call_completed`, in
    /// request order, each with its request: the batch the finishing turn
    /// completes.
    pub(crate) batch: Vec<(ActionId, ToolCallRequested)>,
}

/// The suspended turn `lines` describe, if any: the last line is a
/// `fiber_exited` with `suspended_on` naming a `permission_requested` that
/// has no `permission_resolved`, or an `interaction_requested` a resume may
/// raise again ([`crate::suspend::interaction_pending`]), whose turn has no
/// `turn_completed`, and whose action is in the suspended batch. Anything
/// else resumes as a cut-short turn: an interaction not logged
/// `resumes: true` is never raised again (`docs/events.md`, "Resume"). A
/// `suspended_on` naming a repository offer resumes no turn: the offer is
/// raised again by the offer step.
pub(crate) fn suspended(lines: &[Envelope]) -> Result<Option<Suspended>, Error> {
    let Some(last) = lines.last() else {
        return Ok(None);
    };
    let Some(event) = Event::from_envelope(last).map_err(Error::Unreadable)? else {
        return Ok(None);
    };
    let Event::FiberExited(exited) = event else {
        return Ok(None);
    };
    let Some(pending) = exited.suspended_on else {
        return Ok(None);
    };
    let mut found = None;
    for line in lines.iter().filter(|l| l.is_durable()) {
        let Some(event) = Event::from_envelope(line).map_err(Error::Unreadable)? else {
            continue;
        };
        if let Event::PermissionRequested(request) = &event
            && request.request_id == pending
        {
            let (Some(turn), Some(action)) = (line.turn_id.clone(), line.action_id.clone()) else {
                continue;
            };
            found = Some((request.clone(), turn, action));
        }
        if let Event::PermissionResolved(resolved) = &event
            && resolved.request_id.as_ref() == Some(&pending)
        {
            return Ok(None);
        }
    }
    let (request, turn, action) = match found {
        Some((request, turn, action)) => (Pending::Approval(request), turn, action),
        None => match crate::suspend::interaction_pending(lines, &pending)? {
            Some((asked, turn, action)) => (Pending::Interaction(asked), turn, action),
            None => return Ok(None),
        },
    };
    if lines
        .iter()
        .any(|l| l.is_durable() && l.kind == "turn_completed" && l.turn_id.as_ref() == Some(&turn))
    {
        return Ok(None);
    }
    let completed = crate::conversation::completed_actions(lines)?;
    // Starting at the message line itself is safe: it is an
    // `assistant_message_started`, never a `tool_call_requested`, so the
    // scan below skips it either way.
    let start = lines
        .iter()
        .rposition(|l| l.kind == "assistant_message_started")
        .unwrap_or(0);
    let mut batch = Vec::new();
    for line in lines.iter().skip(start) {
        // A call the provider ran is never given a result.
        if line.kind != "tool_call_requested" || line.payload.contains_key("provider_item") {
            continue;
        }
        if let Some(id) = &line.action_id
            && !completed.contains(id)
            && !batch.iter().any(|(queued, _)| queued == id)
            && let Some(Event::ToolCallRequested(call)) =
                Event::from_envelope(line).map_err(Error::Unreadable)?
        {
            batch.push((id.clone(), call));
        }
    }
    if !batch.iter().any(|(id, _)| *id == action) {
        return Ok(None);
    }
    Ok(Some(Suspended {
        turn,
        pending: request,
        action,
        batch,
    }))
}

impl Loop {
    /// Resumes the session `resumed` folded on `log`, which already holds
    /// the lock: the conversation is rebuilt from the window `resumed`
    /// found, across the chain it folds, with the fixed results for the calls a crash left without
    /// one, and the folds a new loop builds from nothing are restored. The
    /// window is the only part of the log read here, up to the line count
    /// the pass saw, so lines written since (`fiber_started` and the
    /// like) are not in it. The only lines written are one `job_completed`
    /// per job a crash left running, marked `orphaned`; `session_started`
    /// is the first line's, and `seq` carries on. Everything a new loop
    /// runs on rides [`crate::Session`].
    pub fn resume(session: Session, resumed: Resumed) -> Result<Self, Error> {
        let mut session = session;
        let Resumed {
            session: session_id,
            root,
            window,
            end,
            seed: carry,
            ledger,
            grants,
            session_blocks,
            reviewed,
            orphans,
            thinking,
            offers,
            settings,
            ..
        } = resumed;
        let mut lines = crate::history::read_window(&session.log, window, end)?;
        // The suspended turn, if any: its batch stays open, so the rebuild
        // writes no fixed result for it; the finishing turn completes it.
        let halted = suspended(&lines)?;
        let open: HashSet<ActionId> = halted
            .as_ref()
            .map(|halted| halted.batch.iter().map(|(id, _)| id.clone()).collect())
            .unwrap_or_default();
        // The conversation, with the fixed results, and its length at the
        // last `assistant_message_started`: the previous request's end, for
        // the cache markers. Notices the log holds behind the open batch
        // are released after its results, as the finishing turn writes them.
        let (conversation, sent, held, carry) =
            crate::conversation::rebuild_and_sent(&lines, &session.model.reference, &open, carry)?;
        // Registered where a new loop registers: a `Tool::definition()`
        // may read the log.
        let (tools, replaced) = calls::register(std::mem::take(&mut session.tools));
        let workspace = PathBuf::from(&session.permissions.workspace);
        let workspace = workspace.canonicalize().unwrap_or(workspace);
        let credentials = crate::permission::resolved(session.permissions.credentials.clone());
        // A log that already holds an opening message keeps it: the
        // conversation rebuild renders it from the log, so the first turn
        // writes none. A log with none gets one at its first turn.
        // A completed handoff starts a new context, which needs a new
        // opening message: one the crash cut off is written at the next turn.
        let mut opened = false;
        for line in &lines {
            if line.kind == "opening_message" {
                opened = true;
            } else if line.kind == "handoff_completed"
                && line.payload.get("outcome").and_then(|v| v.as_str()) == Some("completed")
            {
                opened = false;
            }
        }
        // Only the current context's lines: a completed handoff starts the
        // tracked state afresh, as it does live.
        let context = if opened {
            let start = lines
                .iter()
                .rposition(|line| line.kind == "opening_message")
                .unwrap_or(0);
            Some(lines.split_off(start))
        } else {
            None
        };
        let mut resumed = Self::new(
            session,
            crate::Seed {
                reason: contract::events::PreambleReason::Resume,
                session: session_id,
                // The root session's id: every session on a rewind chain
                // shares the root's cache key (`docs/prompt-cache.md`,
                // "Cache markers and keys").
                root,
                suspended: halted,
                held,
                conversation,
                sent,
                tools,
                replaced,
                workspace,
                credentials,
                grants,
                reviewed,
                session_blocks,
                repository: crate::offer::State::resumed(offers),
                context,
                ledger,
                carry,
                chosen: thinking.and_then(|level| level.parse().ok()),
            },
        )?;
        resumed.mark_orphans(orphans)?;
        // After any orphaned `job_completed` lines, before the first
        // `preamble_built`: a resume that switches the credential label
        // records it as a switch does (`docs/events.md`,
        // "`model_changed`").
        resumed.resumed_label(settings)?;
        Ok(resumed)
    }
}

impl Loop {
    /// Finishes a turn cut short on a pending approval, then runs the
    /// prompt as the next turn (`docs/invocation.md`, "Lifecycle"): the
    /// request is raised again under the same `request_id` and answered,
    /// every call of the batch is completed in request order, and the turn
    /// takes its next step as after any completed batch. With no person to
    /// answer, the request is refused as headless. With one, the loop waits
    /// for the answer as any approval does: the calls before the action
    /// complete `cancelled` without running, as an answered request's calls
    /// do across a shutdown (`docs/invocation.md`, "Shutdown"), the action
    /// follows the answer, and the calls after it are judged and run with
    /// it. The preamble runs first, as before any turn; the finishing turn
    /// is not a turn start, so it runs no instruction-file/date check (the
    /// next turn, the prompt's, does it at its own start). `turn_started`
    /// is not written again. Deliveries already waiting are held aside in
    /// `deferred`, so the finishing turn's drains do not reject the prompt
    /// `busy`; a reply to the request among them answers it.
    pub(crate) fn finish_suspended(
        &mut self,
        suspended: Suspended,
    ) -> Result<Option<TurnOutcome>, Error> {
        self.deferred.extend(self.inbox.try_iter());
        // The repository's offer resolves before the first request. A
        // process that ends on it writes the suspended approval again, so it
        // exits naming the approval and the next resume finishes the turn.
        let offered = self.offer(Some(suspended.pending.request_id()));
        if !matches!(offered, Ok(true)) {
            self.keep_suspended(&suspended)?;
            return offered.map(|_| None);
        }
        self.ensure_preamble()?;
        self.cut_off = false;
        let Pending::Approval(request) = suspended.pending.clone() else {
            return self.finish_suspended_form(suspended);
        };
        let turn = suspended.turn.clone();
        // The credential deny applies to every call, including one
        // suspended before its path became a configured credential file:
        // it is refused without asking, as `judge` refuses it before any
        // ask. A call that cannot be checked waits for its answer as ever.
        // The refusal answers the previously raised request, and the batch
        // completes through the run phase every step uses: the calls
        // before the action complete `cancelled` without running, as an
        // answered request's calls do, and the calls after it are judged
        // and run with it (`docs/loop.md`, "Tool calls that do not run").
        // Armed with the refusal: the calls after it run cancellably, as
        // after the re-raise below. A shutdown before it writes nothing
        // more, so the next resume refuses again.
        if let Some((_, call)) = suspended
            .batch
            .iter()
            .find(|(id, _)| *id == suspended.action)
            && let Ok((_, _, effects)) = self.checked(call)
            && let Some(why) = crate::permission::credential_why(
                &effects.declared,
                &self.workspace,
                &self.credentials,
                &self.credential_files,
            )
        {
            let text = format!("{why} It did not run.");
            let cancel = Arc::clone(&self.cancel);
            let refused = cancel.commit(Commit::Arm, || {
                self.decided(
                    &suspended.action,
                    &turn,
                    crate::completion::resolved(
                        Some(request.request_id.clone()),
                        Decision::Deny,
                        DecidedBy::CredentialDeny,
                        Some(why),
                        None,
                    ),
                )
            });
            let Some(refused) = refused else {
                return Ok(None);
            };
            refused?;
            // The refusal stands as decided: it is passed as is, never
            // re-checked into an allow.
            let decision: Decided = Err(crate::completion::denied("credentials", text));
            let calls = crate::suspend::form_batch(
                suspended.batch,
                &suspended.action,
                || Err(self.cancelled_before_ran()),
                |_| decision,
            );
            let cancelled = self.run_batch(calls, &turn, None)?;
            return self.finish_batch(&turn, cancelled);
        }
        // Armed with the re-raise: a shutdown before it leaves the request
        // pending, so the next resume raises it again (`docs/invocation.md`,
        // "Shutdown"). Once raised, a headless request's answer is the
        // refusal below, already in hand: it stands, and a shutdown after
        // the re-raise ends the turn `interrupted`. A shutdown while
        // waiting for a person leaves the request pending again.
        let cancel = Arc::clone(&self.cancel);
        let raised = cancel.commit(Commit::Arm, || {
            self.append(
                &Event::PermissionRequested(request.clone()),
                &turn,
                Some(&suspended.action),
            )
        });
        let Some(raised) = raised else {
            return Ok(None);
        };
        raised?;
        if self.answerable {
            return self.answer_suspended(suspended, &request);
        }
        // Nobody decided the re-raised request, review or standing ask: the
        // resume denies it by `cancel` (`docs/events.md`, `permission_resolved`).
        self.decided(
            &suspended.action,
            &turn,
            resolved(
                Some(request.request_id.clone()),
                Decision::Deny,
                DecidedBy::Cancel,
                Some(NOBODY_TO_ANSWER.to_owned()),
                None,
            ),
        )?;
        let denied = calls::no_person();
        for (id, _) in &suspended.batch {
            let completed = if *id == suspended.action {
                denied.clone()
            } else {
                self.cancelled_before_ran()
            };
            self.append(&Event::ToolCallCompleted(*completed), &turn, Some(id))?;
        }
        // Orphan notices the resume logged behind the open batch.
        self.conversation.append(&mut self.held);
        self.run_steps(&turn)
    }

    /// The finishing turn once its request is raised again and a person can
    /// answer it: waits for the answer, then completes the batch through
    /// the run phase every step uses. The idle delay passing while waiting
    /// writes nothing more: the session exits suspended on the same
    /// request, and the next resume raises it again.
    fn answer_suspended(
        &mut self,
        suspended: Suspended,
        request: &contract::events::PermissionRequested,
    ) -> Result<Option<TurnOutcome>, Error> {
        let Suspended {
            turn,
            action,
            batch,
            ..
        } = suspended;
        // `suspended` only returns a batch that holds the action.
        let Some((_, call)) = batch.iter().find(|(id, _)| *id == action) else {
            return self.run_steps(&turn);
        };
        let decision: Decided = match self.await_answer(&action, &turn, &call.name, request)? {
            // The answer allows the call the request was raised for, as
            // the model asked for it: checked again, it runs.
            Asked::Allow => self
                .checked(call)
                .map(|(tool, arguments, effects)| (tool, arguments, effects.declared)),
            Asked::Deny(completed) | Asked::Gone(completed) => Err(completed),
            Asked::Cancelled => Err(self.cancelled_before_ran()),
            Asked::Idle => {
                self.pending.clear();
                self.cancel.disarm();
                return Ok(None);
            }
            // The close already denied the re-raised request by `cancel`:
            // the call completes as one no person can answer
            // (`docs/events.md`, `permission_resolved`).
            Asked::Closed => Err(crate::calls::no_person()),
        };
        let calls = crate::suspend::form_batch(
            batch,
            &action,
            || Err(self.cancelled_before_ran()),
            |_| decision,
        );
        let cancelled = self.run_batch(calls, &turn, None)?;
        self.finish_batch(&turn, cancelled)
    }

    /// The step loop and the `turn_completed` tail every turn ends with:
    /// shared by a new turn and the finishing one, so the two cannot drift.
    pub(crate) fn run_steps(&mut self, turn: &TurnId) -> Result<Option<TurnOutcome>, Error> {
        let completed = loop {
            match self.step(turn)? {
                Step::Next => {}
                // Anything waiting continues the turn (`docs/loop.md`,
                // "Ending a turn"). A steer taken here waits for the next
                // step; a prompt is rejected.
                Step::Replied => {
                    self.drain(turn)?;
                    if self.queued.is_empty() {
                        break crate::ended(TurnOutcome::Completed, None);
                    }
                }
                Step::Ended(completed) => break completed,
            }
        };
        self.end_turn(turn, completed)
    }

    /// `run_steps`'s tail: the idle check, `disarm_cancel`, `turn_completed`.
    pub(crate) fn end_turn(
        &mut self,
        turn: &TurnId,
        mut completed: TurnCompleted,
    ) -> Result<Option<TurnOutcome>, Error> {
        if self.idle_left {
            self.pending.clear();
            self.cancel.disarm();
            return Ok(None);
        }
        self.disarm_cancel(&mut completed);
        let outcome = completed.outcome;
        self.append(&Event::TurnCompleted(completed), turn, None)?;
        Ok(Some(outcome))
    }
}
