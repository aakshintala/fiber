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
use contract::{Envelope, ErrorCode, SCHEMA_VERSION, SessionId};
use log::Log;
use serde_json::Map;

use crate::calls;
use crate::reviewer::{BlockLimits, NO_MODEL_MESSAGE, render_reviewed};
use crate::{Error, Loop, Model, Permissions};

/// What a resume folds back: the workspace the first `session_started`
/// recorded, and the model the last `usage_recorded` names, if any.
pub struct Resumed {
    /// The first `session_started`'s workspace: a resumed session keeps it,
    /// wherever the resume runs (`docs/state.md`, "Sessions and resume").
    pub workspace: String,
    /// The last `usage_recorded`'s model: the session's model, beating
    /// `--model` (`docs/model-routing.md`, "Choosing the model"). `None`
    /// when the log holds none.
    pub model: Option<String>,
}

/// Folds `lines` back to the workspace and the model. A log with no
/// `session_started` is corrupt.
pub fn resumed(lines: &[Envelope]) -> Result<Resumed, Error> {
    let mut workspace = None;
    let mut model = None;
    for line in lines {
        let event = Event::from_envelope(line).map_err(Error::Unreadable)?;
        if let Some(Event::SessionStarted(started)) = &event {
            workspace.get_or_insert_with(|| started.workspace.clone());
        }
        if let Some(Event::UsageRecorded(recorded)) = &event {
            model = Some(recorded.model.clone());
        }
    }
    match workspace {
        Some(workspace) => Ok(Resumed { workspace, model }),
        None => Err(no_session_started(
            lines.first().map(|line| &line.session_id),
        )),
    }
}

/// `log_corrupt` for a log with no `session_started`: reading an empty
/// payload as one names the missing line. `loop` takes no `serde`
/// dependency for `Error::custom` (`docs/dependencies.md`), so the error
/// comes from a real parse.
fn no_session_started(session: Option<&SessionId>) -> Error {
    let line = Envelope {
        kind: "session_started".to_owned(),
        session_id: session.cloned().unwrap_or(SessionId(String::new())),
        ts: 0,
        schema_version: SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: Map::new(),
    };
    match Event::from_envelope(&line) {
        Err(error) => Error::Unreadable(error),
        // An empty payload never reads as `session_started`: `workspace`
        // and `variables` are required. Without this arm the parse above
        // would need `serde` for `Error::custom`; with it, the fallback
        // below never runs.
        Ok(_) => corrupt(),
    }
}

/// A `log_corrupt` failure from a text that never parses. The `Ok` arm
/// parses it again, so it never returns either.
fn corrupt() -> Error {
    match serde_json::from_str("the log has no session_started") {
        Ok(()) => corrupt(),
        Err(error) => Error::Unreadable(error),
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
        resumed(lines)?;
        let completed = crate::conversation::completed_actions(lines)?;
        // The conversation, with the fixed results, and its length at the
        // last `assistant_message_started`: the previous request's end, for
        // the cache markers.
        let (conversation, sent) =
            crate::conversation::rebuild_and_sent(lines, &model.reference, &completed)?;
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
        let session = folded_session(lines)?;
        Ok(Self {
            log,
            provider,
            model,
            system_prompt,
            inbox,
            // The session's own id, as `Loop::start` sets it: no parent or
            // fork exists yet (`docs/prompt-cache.md`, "Rules for other
            // areas").
            cache_key: session.clone(),
            reviewer_key: format!("{session}:reviewer"),
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
        })
    }
}

/// The session's id: the first `session_started` line's. `resumed` already
/// showed one exists.
fn folded_session(lines: &[Envelope]) -> Result<String, Error> {
    for line in lines {
        if matches!(
            Event::from_envelope(line).map_err(Error::Unreadable)?,
            Some(Event::SessionStarted(_))
        ) {
            return Ok(line.session_id.0.clone());
        }
    }
    Err(no_session_started(
        lines.first().map(|line| &line.session_id),
    ))
}
