//! Resuming a session (`docs/events.md`, "Resume"): rebuilding the loop's
//! state from the log and its configuration, nothing else. A suspended turn
//! (`fiber_exited` with `suspended_on`) re-raises its pending approval under
//! the same `request_id`, refuses it as headless, finishes the turn, and
//! then runs the prompt as the next turn.

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;

use contract::events::{DecidedBy, Decision, Event, TurnOutcome};
use contract::inbox::Delivery;
use contract::provider::Provider;
use contract::shapes::Failure;
use contract::tool::Tool;
use contract::{ActionId, Envelope, ErrorCode, TurnId};
use log::Log;

use crate::calls;
use crate::retry::Retry;
use crate::reviewer::{BlockLimits, NO_MODEL_MESSAGE, render_reviewed};
use crate::{Error, Loop, Model, Permissions, Step};

/// What a resume folds back: the session, the workspace the first
/// `session_started` recorded, and the model the last `usage_recorded`
/// names, if any.
pub struct Resumed {
    /// The session's id, from the first `session_started` line: the cache
    /// keys are built from it (`docs/prompt-cache.md`, "Rules for other
    /// areas").
    pub session: String,
    /// The first `session_started`'s workspace: a resumed session keeps it,
    /// wherever the resume runs (`docs/state.md`, "Sessions and resume").
    pub workspace: String,
    /// The last `usage_recorded`'s model: the session's model, beating
    /// `--model` (`docs/model-routing.md`, "Choosing the model"). `None`
    /// when the log holds none.
    pub model: Option<String>,
}

/// Folds `lines` back to the session, the workspace and the model. A log
/// with no `session_started` is corrupt.
pub fn resumed(lines: &[Envelope]) -> Result<Resumed, Error> {
    let mut folded: Option<Resumed> = None;
    let mut model = None;
    for line in lines {
        let event = Event::from_envelope(line).map_err(Error::Unreadable)?;
        if let Some(Event::SessionStarted(started)) = &event
            && folded.is_none()
        {
            folded = Some(Resumed {
                session: line.session_id.0.clone(),
                workspace: started.workspace.clone(),
                model: None,
            });
        }
        if let Some(Event::UsageRecorded(recorded)) = &event {
            model = Some(recorded.model.clone());
        }
    }
    match folded {
        Some(mut folded) => {
            folded.model = model;
            Ok(folded)
        }
        None => Err(Error::NoSessionStarted),
    }
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
    /// request order: the batch the finishing turn completes.
    pub(crate) batch: Vec<ActionId>,
}

/// The suspended turn `lines` describe, if any: the last line is a
/// `fiber_exited` with `suspended_on` naming a `permission_requested` that
/// has no `permission_resolved`, whose turn has no `turn_completed`, and
/// whose action is in the suspended batch. Anything else resumes as a
/// cut-short turn. Only approvals re-raise: no tool raises an
/// `interaction_requested` yet, so a `suspended_on` naming one resumes as
/// cut short.
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
        if line.kind != "tool_call_requested" {
            continue;
        }
        if let Some(id) = &line.action_id
            && !completed.contains(id)
            && !batch.contains(id)
        {
            batch.push(id.clone());
        }
    }
    if !batch.contains(&action) {
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
    /// Resumes the session `lines` describe on `log`, which already holds
    /// the lock: the conversation is rebuilt with the fixed results for the
    /// calls a crash left without one, and the folds a new loop builds from
    /// nothing are restored. The only lines written are one `job_completed`
    /// per job a crash left running, marked `orphaned`; `session_started` is
    /// the first line's, and `seq` carries on. Every other argument is
    /// [`Loop::start`]'s. `lines` is the whole log, read under the lock
    /// `Log::open` took.
    #[allow(
        clippy::too_many_arguments,
        reason = "#302's resume interface: the log's lines ride with Loop::start's arguments"
    )]
    pub fn resume(
        log: Arc<Log>,
        lines: &[Envelope],
        provider: Arc<dyn Provider>,
        model: Model,
        prompt: crate::prompt::PromptInputs,
        inbox: Receiver<Delivery>,
        tools: Vec<(String, Arc<dyn Tool>)>,
        permissions: Permissions,
    ) -> Result<Self, Error> {
        // Fails `log_corrupt` on a log with no `session_started`.
        let folded = resumed(lines)?;
        // The suspended turn, if any: its batch stays open, so the rebuild
        // writes no fixed result for it; the finishing turn completes it.
        let halted = suspended(lines)?;
        let open: HashSet<ActionId> = halted
            .as_ref()
            .map(|halted| halted.batch.iter().cloned().collect())
            .unwrap_or_default();
        // The conversation, with the fixed results, and its length at the
        // last `assistant_message_started`: the previous request's end, for
        // the cache markers. Notices the log holds behind the open batch
        // are released after its results, as the finishing turn writes them.
        let (conversation, sent, held) =
            crate::conversation::rebuild_and_sent(lines, &model.reference, &open)?;
        let mut reviewed = Vec::new();
        let mut grants = Vec::new();
        let mut session_blocks = 0;
        let mut ledger = crate::usage::Ledger::default();
        for line in lines.iter().filter(|l| l.is_durable()) {
            let event = Event::from_envelope(line).map_err(Error::Unreadable)?;
            if let Some(event) = &event {
                render_reviewed(&mut reviewed, event, line.action_id.as_ref());
            }
            if let Some(Event::PermissionResolved(resolved)) = &event {
                if let Some(grant) = &resolved.grant {
                    grants.push(grant.clone());
                }
                // A model's block: the spending-budget denial and
                // a reviewer failure carry no `reviewer` object
                // and are not counted. Escalated blocks a person
                // answered are not distinguished in the log.
                // debt: undercounts session blocks that a person
                // answered or a reviewer failure caused; fixed
                // when the log records blocks.
                if resolved.decision == Decision::Deny
                    && resolved.decided_by == DecidedBy::Reviewer
                    && resolved.reviewer.is_some()
                {
                    session_blocks += 1;
                }
            }
            if let Some(Event::UsageRecorded(recorded)) = &event {
                ledger.record(recorded);
            }
        }
        let (tools, replaced) = calls::register(tools);
        let workspace = PathBuf::from(&permissions.workspace);
        let workspace = workspace.canonicalize().unwrap_or(workspace);
        let credentials =
            calls::resolve(&permissions.credentials).unwrap_or(permissions.credentials);
        // A log that already holds an opening message keeps it: the
        // conversation rebuild renders it from the log, so the first turn
        // writes none. A log with none gets one at its first turn.
        let opened = lines.iter().any(|line| line.kind == "opening_message");
        // The tracked state the log's lines describe, when the log holds
        // an opening message; without one the first turn writes it fresh
        // and rebuilds the state from it.
        let changes = if opened {
            crate::changes::State::resumed(lines, &workspace, &prompt.home)?
        } else {
            crate::changes::State::empty(&prompt.home)
        };
        // debt: copies `Loop::start`'s literal apart from five fields; a
        // shared constructor once a third constructor needs the same fields.
        let mut resumed = Self {
            log,
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
            cache_key: folded.session.clone(),
            reviewer_key: format!("{}:reviewer", folded.session),
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
            rules: permissions.rules,
            grants,
            reviewer: Err(Failure {
                code: ErrorCode::NoModel,
                message: NO_MODEL_MESSAGE.to_owned(),
                retry_after: None,
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
            opened,
            changes,
            cut_off: false,
            ledger,
            budget: None,
            retry: Retry::default(),
            idle_exit: None,
            idle_left: false,
            hooks: None,
        };
        resumed.mark_orphans(lines)?;
        Ok(resumed)
    }
}

impl Loop {
    /// Finishes a turn cut short on a pending approval, then runs the
    /// prompt as the next turn (`docs/invocation.md`, "Lifecycle"): the
    /// request is raised again under the same `request_id`, refused as
    /// headless, every call of the batch is completed in request order,
    /// and the turn takes its next step as after any completed batch. The
    /// preamble runs first, as before any turn; the finishing turn is not
    /// a turn start, so it runs no instruction-file/date check (the next
    /// turn, the prompt's, does it at its own start). `turn_started` is
    /// not written again. Deliveries already
    /// waiting are held aside in `deferred`, so the finishing turn's
    /// drains do not reject the prompt `busy`.
    pub(crate) fn finish_suspended(
        &mut self,
        suspended: Suspended,
    ) -> Result<Option<TurnOutcome>, Error> {
        self.deferred.extend(self.inbox.try_iter());
        self.ensure_preamble()?;
        self.cut_off = false;
        self.cancel.arm();
        let turn = suspended.turn.clone();
        self.append(
            &Event::PermissionRequested(suspended.request.clone()),
            &turn,
            Some(&suspended.action),
        )?;
        // debt: refuses a suspended request whatever answerable says; the
        // hub's resume raises it and waits for a reply (docs/invocation.md
        // "Lifecycle").
        let denied = self.unanswerable(
            &suspended.action,
            &turn,
            Some(suspended.request.request_id.clone()),
        )?;
        for id in &suspended.batch {
            let completed = if *id == suspended.action {
                denied.clone()
            } else {
                crate::cancel::never_ran()
            };
            self.append(&Event::ToolCallCompleted(*completed), &turn, Some(id))?;
        }
        // Orphan notices the resume logged behind the open batch.
        self.conversation.append(&mut self.held);
        self.run_steps(&turn)
    }

    /// The step loop and the `turn_completed` tail every turn ends with:
    /// shared by a new turn and the finishing one, so the two cannot drift.
    pub(crate) fn run_steps(&mut self, turn: &TurnId) -> Result<Option<TurnOutcome>, Error> {
        let mut completed = loop {
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
        if self.idle_left {
            self.cancel.disarm();
            return Ok(None);
        }
        self.disarm_cancel(&mut completed);
        let outcome = completed.outcome;
        self.append(&Event::TurnCompleted(completed), turn, None)?;
        Ok(Some(outcome))
    }
}
