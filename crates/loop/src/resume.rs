//! Resuming a session (`docs/events.md`, "Resume"): rebuilding the loop's
//! state from the log and its configuration, nothing else. A suspended turn
//! (`fiber_exited` with `suspended_on`) re-raises its pending approval under
//! the same `request_id`, waits for a person's answer (or refuses it as
//! headless when no person can answer), finishes the turn, and then runs
//! the prompt as the next turn.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::Receiver;

use contract::events::{
    DecidedBy, Decision, Event, Grant, JobCompleted, Outcome, ToolCallRequested, TurnCompleted,
    TurnOutcome,
};
use contract::inbox::Delivery;
use contract::provider::Provider;
use contract::shapes::Failure;
use contract::tool::Tool;
use contract::{ActionId, Envelope, ErrorCode, TurnId};
use log::Log;

use crate::calls::{self, Asked, Decided};
use crate::cancel::Commit;
use crate::handoff::Carry;
use crate::retry::Retry;
use crate::reviewer::{BlockLimits, NO_MODEL_MESSAGE, Reviewed, render_reviewed};
use crate::{Error, Loop, Model, Permissions, Step};

/// What a resume folds back in one streaming pass over the log: the
/// session, the workspace the first `session_started` recorded, the model
/// the last `usage_recorded` names, if any, and the session-wide state a
/// resumed loop restores. The pass also finds the window a resume reads:
/// the lines from the last completed handoff on (`docs/handoff.md`,
/// "Resume").
pub struct Resumed {
    /// The session's id, from the first `session_started` line: the cache
    /// keys are built from it (`docs/prompt-cache.md`, "Rules for other
    /// areas").
    pub session: String,
    /// The first `session_started`'s workspace: a resumed session keeps it,
    /// wherever the resume runs (`docs/state.md`, "Sessions and resume").
    pub workspace: String,
    /// The model of the last call recorded, or of the last `model_changed`
    /// when it came later; a late cost's second record of a call is not a
    /// new call. The session's model, beating `--model`
    /// (`docs/model-routing.md`, "Choosing the model"). `None` when the log
    /// holds none.
    pub model: Option<String>,
    /// The last `preamble_built`'s credential label: the label a resumed
    /// session keeps, beating the configured one (`docs/model-routing.md`,
    /// "Which credential a session uses"). `None` when the log holds none.
    pub credential: Option<String>,
    /// The last `model_changed`'s `after.thinking`: a resumed session
    /// treats it as its own choice (`docs/model-routing.md`, "Thinking").
    /// `None` when the log holds none.
    pub thinking: Option<String>,
    /// The `seq` the window starts at: the latest `turn_started` of the
    /// latest completed handoff's turn, before that handoff; 0 with no
    /// completed handoff, or none of its turn's `turn_started` lines.
    pub(crate) window: u64,
    /// How many lines the pass read: the window ends there, before any line
    /// written after the pass.
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
}

/// The kinds the pass reads a payload of. Every other line's envelope is
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

/// Folds the log in the session directory `dir` in one pass, one line at a
/// time, holding no parsed vector of it (`docs/events.md`, "Resume"): the
/// session, the workspace, the model, the credential label, the
/// session-wide state, and the window a resume reads. A log with no
/// `session_started` is corrupt, as is a line that is not an envelope or a
/// folded line whose payload does not read as its kind.
pub fn resumed(dir: &Path) -> Result<Resumed, Error> {
    let mut first: Option<(String, String)> = None;
    let mut model = None;
    let mut credential = None;
    let mut thinking = None;
    let mut ledger = crate::usage::Ledger::default();
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
    // with the state at it: a later handoff names one of them.
    let mut starts: HashMap<TurnId, (u64, Carry)> = HashMap::new();
    let mut window = (0, Carry::default());
    let mut end = 0;
    for line in log::lines(dir)? {
        let line = line?;
        end += 1;
        if !line.is_durable() || !FOLDED.contains(&line.kind.as_str()) {
            continue;
        }
        let Some(event) = Event::from_envelope(&line).map_err(Error::Unreadable)? else {
            continue;
        };
        render_reviewed(&mut reviewed, &event, line.action_id.as_ref(), line.seq);
        orphans.fold(&event);
        offers.fold(&event);
        running.fold_jobs(&event);
        if let Event::SessionStarted(started) = &event
            && first.is_none()
        {
            first = Some((line.session_id.0.clone(), started.workspace.clone()));
        } else if let Event::UsageRecorded(recorded) = &event {
            // A correction keeps the model the call was first recorded at,
            // which may be an earlier model than the latest one.
            if !ledger.record(recorded) {
                model = Some(recorded.model.clone());
            }
        } else if let Event::PreambleBuilt(built) = &event {
            credential.clone_from(&built.credential);
        } else if let Event::ModelChanged(changed) = &event {
            model = Some(changed.after.model.clone());
            credential = changed.after.credential.clone();
            thinking = changed.after.thinking.clone();
        } else if let Event::PermissionResolved(resolved) = &event {
            if let Some(grant) = &resolved.grant {
                grants.push(grant.clone());
            }
            // A model's block, a reviewer failure and a denial with no
            // reviewer set up each count, as they do live. The
            // spending-budget denial is `decided_by: budget` and does not.
            // debt: undercounts session blocks whose escalation a person
            // answered or a cancel ended; fixed when the log records blocks.
            if resolved.decision == Decision::Deny
                && matches!(
                    resolved.decided_by,
                    DecidedBy::Reviewer | DecidedBy::NoReviewer
                )
            {
                session_blocks += 1;
            }
        } else if let Event::OpeningMessage(message) = &event {
            running
                .session_log
                .clone_from(&message.environment.session_log);
        } else if let Event::TurnStarted(_) = &event {
            if let (Some(turn), Some(seq)) = (&line.turn_id, line.seq) {
                let state = Carry {
                    jobs: running.jobs.clone(),
                    session_log: running.session_log.clone(),
                    ..Carry::default()
                };
                starts.insert(turn.clone(), (seq.0, state));
            }
        } else if let Event::HandoffCompleted(done) = &event
            && done.outcome == Outcome::Completed
        {
            let turn = line.turn_id.as_ref();
            window = turn
                .and_then(|turn| starts.get(turn))
                .cloned()
                .unwrap_or_default();
            // Only this turn can complete another handoff before its
            // next `turn_started`.
            starts.retain(|started, _| Some(started) == turn);
        }
    }
    let Some((session, workspace)) = first else {
        return Err(Error::NoSessionStarted);
    };
    let (window, seed) = window;
    Ok(Resumed {
        session,
        workspace,
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
    })
}

/// A turn cut short on a pending approval: what the finishing turn writes
/// back under the same turn id (`docs/invocation.md`, "Lifecycle").
pub(crate) struct Suspended {
    /// The cut-short turn, which the finishing turn completes.
    pub(crate) turn: TurnId,
    /// The pending request, re-raised with the same `request_id`.
    pub(crate) request: contract::events::PermissionRequested,
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
/// has no `permission_resolved`, whose turn has no `turn_completed`, and
/// whose action is in the suspended batch. Anything else resumes as a
/// cut-short turn. Only approvals re-raise: no tool raises an
/// `interaction_requested` yet, so a `suspended_on` naming one resumes as
/// cut short. A `suspended_on` naming a repository offer resumes no turn:
/// the offer is raised again by the offer step.
// debt: re-raises approvals only; an interaction_requested joins when a tool raises one (ask_user, docs/tools.md "Asking the person").
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
    let Some((request, turn, action)) = found else {
        return Ok(None);
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
        request,
        action,
        batch,
    }))
}

impl Loop {
    /// Resumes the session `resumed` folded on `log`, which already holds
    /// the lock: the conversation is rebuilt from the window `resumed`
    /// found, with the fixed results for the calls a crash left without
    /// one, and the folds a new loop builds from nothing are restored. The
    /// window is the only part of the log read here, up to the line count
    /// the pass saw, so lines written since (`fiber_started` and the
    /// like) are not in it. The only lines written are one `job_completed`
    /// per job a crash left running, marked `orphaned`; `session_started`
    /// is the first line's, and `seq` carries on. Every other argument is
    /// [`Loop::start`]'s.
    #[allow(
        clippy::too_many_arguments,
        reason = "#302's resume interface: the folded log rides with Loop::start's arguments"
    )]
    pub fn resume(
        log: Arc<Log>,
        resumed: Resumed,
        provider: Arc<dyn Provider>,
        model: Model,
        prompt: crate::prompt::PromptInputs,
        inbox: Receiver<Delivery>,
        tools: Vec<(String, Arc<dyn Tool>)>,
        permissions: Permissions,
    ) -> Result<Self, Error> {
        let Resumed {
            session,
            window,
            end,
            seed,
            ledger,
            grants,
            session_blocks,
            reviewed,
            orphans,
            thinking,
            offers,
            ..
        } = resumed;
        let lines = log.range(
            window,
            usize::try_from(end.saturating_sub(window)).unwrap_or(usize::MAX),
        )?;
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
            crate::conversation::rebuild_and_sent(&lines, &model.reference, &open, seed)?;
        let (tools, replaced) = calls::register(tools);
        let workspace = PathBuf::from(&permissions.workspace);
        let workspace = workspace.canonicalize().unwrap_or(workspace);
        let credentials = crate::permission::resolved(permissions.credentials);
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
        // The tracked state the log's lines describe, when the log holds
        // an opening message; without one the first turn writes it fresh
        // and rebuilds the state from it.
        // Only the current context's lines: a completed handoff starts the
        // tracked state afresh, as it does live.
        let changes = if opened {
            let context = lines
                .iter()
                .rposition(|line| line.kind == "opening_message")
                .unwrap_or(0);
            let context = lines.get(context..).unwrap_or_default();
            crate::changes::State::resumed(context, &workspace, &prompt)?
        } else {
            crate::changes::State::empty(&prompt.home)
        };
        // debt: copies `Loop::start`'s literal apart from five fields; a
        // shared constructor once a third constructor needs the same fields.
        let diag = crate::diag::SessionDiag::new(
            &prompt.home,
            contract::SessionId(session.clone()),
            Arc::clone(log.clock()),
        );
        let mut resumed = Self {
            log,
            diag,
            provider,
            model,
            prompt,
            preamble_reason: contract::events::PreambleReason::Resume,
            preamble: None,
            inbox,
            cancel: std::sync::Arc::new(crate::TurnCancel::default()),
            // The session's own id, as `Loop::start` sets it: no parent or
            // fork exists yet (`docs/prompt-cache.md`, "Rules for other
            // areas").
            reviewer_key: format!("{session}:reviewer"),
            cache_key: session,
            queued: VecDeque::new(),
            closing: false,
            suspended: halted,
            deferred: VecDeque::new(),
            held,
            conversation,
            sent,
            tools,
            replaced,
            workspace,
            credentials,
            credential_files: (permissions.credential_files.into_iter())
                .map(crate::permission::resolved)
                .collect(),
            rules: permissions.rules,
            grants,
            reviewer: Err(Failure {
                code: ErrorCode::NoModel,
                message: NO_MODEL_MESSAGE.to_owned(),
                retry_after_ms: None,
                provider: None,
            }),
            limits: BlockLimits::default(),
            reviewed,
            reviewer_sent: None,
            consecutive: 0,
            session_blocks,
            no_model_noticed: false,
            turn_blocked: None,
            workspace_label: permissions.workspace,
            answerable: true,
            repository: crate::offer::State::resumed(offers),
            opened,
            changes,
            cut_off: false,
            ledger,
            budget: None,
            retry: Retry::default(),
            idle_exit: None,
            idle_left: false,
            hooks: None,
            handoff: crate::handoff::State::new(carry),
            ending: crate::jobs::Ending::default(),
            // The log does not hold requests: a resumed loop warms only
            // after its own first step.
            warm: None,
            last_request: None,
            switcher: None,
            pending: Vec::new(),
            chosen: thinking.and_then(|level| level.parse().ok()),
            warm_stopped: None,
            late_cost: crate::late_cost::LateCost::default(),
        };
        resumed.mark_orphans(orphans)?;
        Ok(resumed)
    }
}

impl Loop {
    /// Writes the suspended approval's `permission_requested` again, under
    /// its turn and action, as its re-raise does.
    fn keep_suspended(&mut self, suspended: &Suspended) -> Result<(), Error> {
        self.append(
            &Event::PermissionRequested(suspended.request.clone()),
            &suspended.turn,
            Some(&suspended.action),
        )
    }

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
        let offered = self.offer(Some(&suspended.request.request_id));
        if !matches!(offered, Ok(true)) {
            self.keep_suspended(&suspended)?;
            return offered.map(|_| None);
        }
        self.ensure_preamble()?;
        self.cut_off = false;
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
                        Some(suspended.request.request_id.clone()),
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
            let mut decision: Option<Decided> =
                Some(Err(crate::completion::denied("credentials", text)));
            let mut before = true;
            let mut calls = Vec::with_capacity(suspended.batch.len());
            for (id, call) in suspended.batch {
                let already = if id == suspended.action {
                    before = false;
                    decision.take()
                } else if before {
                    Some(Err(self.cancelled_before_ran()))
                } else {
                    None
                };
                calls.push((id, call, already));
            }
            let cancelled = self.run_batch(calls, &turn)?;
            // A later call's approval reached the idle delay: nothing more
            // is written, as in any step. The turn resumes later, so a
            // queued switch is dropped.
            if self.idle_left {
                self.pending.clear();
                self.cancel.disarm();
                return Ok(None);
            }
            // Orphan notices the resume logged behind the open batch.
            self.conversation.append(&mut self.held);
            // A cancel that ended the batch ends the turn `interrupted` at
            // the next step's start, as a step's cancel does. A spent
            // headless block budget ends the turn `failed` `blocked`
            // before the batch's questions are processed, as a step's
            // does.
            if !cancelled {
                if let Some(blocked) = self.take_blocked_end() {
                    return self.end_turn(&turn, blocked);
                }
                if let Some(completed) = self.after_calls(&turn)? {
                    return self.end_turn(&turn, completed);
                }
            }
            return self.run_steps(&turn);
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
                &Event::PermissionRequested(suspended.request.clone()),
                &turn,
                Some(&suspended.action),
            )
        });
        let Some(raised) = raised else {
            return Ok(None);
        };
        raised?;
        if self.answerable {
            return self.answer_suspended(suspended);
        }
        let denied = self.unanswerable(
            &suspended.action,
            &turn,
            Some(suspended.request.request_id.clone()),
        )?;
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
    fn answer_suspended(&mut self, suspended: Suspended) -> Result<Option<TurnOutcome>, Error> {
        let Suspended {
            turn,
            request,
            action,
            batch,
        } = suspended;
        // `suspended` only returns a batch that holds the action.
        let Some((_, call)) = batch.iter().find(|(id, _)| *id == action) else {
            return self.run_steps(&turn);
        };
        let decision: Decided = match self.await_answer(&action, &turn, &call.name, &request)? {
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
            Asked::Closed(request_id) => {
                Err(self.unanswerable(&action, &turn, Some(request_id))?)
            }
        };
        let mut decision = Some(decision);
        let mut before = true;
        let mut calls = Vec::with_capacity(batch.len());
        for (id, call) in batch {
            let already = if id == action {
                before = false;
                decision.take()
            } else if before {
                Some(Err(self.cancelled_before_ran()))
            } else {
                None
            };
            calls.push((id, call, already));
        }
        let cancelled = self.run_batch(calls, &turn)?;
        // A later call's approval reached the idle delay: nothing more is
        // written, as in any step. The turn resumes later, so a queued
        // switch is dropped.
        if self.idle_left {
            self.pending.clear();
            self.cancel.disarm();
            return Ok(None);
        }
        // Orphan notices the resume logged behind the open batch.
        self.conversation.append(&mut self.held);
        // A cancel that ended the batch ends the turn `interrupted` at the
        // next step's start, as a step's cancel does. A spent headless
        // block budget ends the turn `failed` `blocked` before the batch's
        // questions are processed, as a step's does.
        if !cancelled {
            if let Some(blocked) = self.take_blocked_end() {
                return self.end_turn(&turn, blocked);
            }
            if let Some(completed) = self.after_calls(&turn)? {
                return self.end_turn(&turn, completed);
            }
        }
        self.run_steps(&turn)
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
    fn end_turn(
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
