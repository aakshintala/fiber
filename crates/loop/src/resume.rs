//! Resuming a session (`docs/events.md`, "Resume"): rebuilding the loop's
//! state from the log and its configuration, nothing else. A suspended turn
//! (`fiber_exited` with `suspended_on`) resumes as any cut-short turn until
//! #302's third criterion lands: its calls with no result get the fixed
//! result and the prompt starts a new turn.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;

use contract::events::{DecidedBy, Decision, Event};
use contract::inbox::Delivery;
use contract::provider::Provider;
use contract::shapes::Failure;
use contract::tool::Tool;
use contract::{Envelope, ErrorCode};
use log::Log;

use crate::calls;
use crate::retry::Retry;
use crate::reviewer::{BlockLimits, NO_MODEL_MESSAGE, render_reviewed};
use crate::{Error, Loop, Model, Permissions};

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

impl Loop {
    /// Resumes the session `lines` describe on `log`, which already holds
    /// the lock: the conversation is rebuilt with the fixed results for the
    /// calls a crash left without one, and the folds a new loop builds from
    /// nothing are restored. Nothing is written: `session_started` is the
    /// first line's, and `seq` carries on. Every other argument is
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
        system_prompt: String,
        inbox: Receiver<Delivery>,
        tools: Vec<(String, Arc<dyn Tool>)>,
        permissions: Permissions,
    ) -> Result<Self, Error> {
        // Fails `log_corrupt` on a log with no `session_started`.
        let folded = resumed(lines)?;
        // The conversation, with the fixed results, and its length at the
        // last `assistant_message_started`: the previous request's end, for
        // the cache markers.
        let (conversation, sent) = crate::conversation::rebuild_and_sent(lines, &model.reference)?;
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
        // debt: copies `Loop::start`'s literal apart from five fields; a
        // shared constructor once a third constructor needs the same fields.
        Ok(Self {
            log,
            provider,
            model,
            system_prompt,
            inbox,
            cancel: std::sync::Arc::new(crate::TurnCancel::default()),
            // The session's own id, as `Loop::start` sets it: no parent or
            // fork exists yet (`docs/prompt-cache.md`, "Rules for other
            // areas").
            cache_key: folded.session.clone(),
            reviewer_key: format!("{}:reviewer", folded.session),
            queued: VecDeque::new(),
            closing: false,
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
            cut_off: false,
            ledger,
            budget: None,
            retry: Retry::default(),
        })
    }
}
