//! The hooks install offer: after the hub refuses a `start` for missing
//! blocking hooks, the terminal offers to install `hooks` before the
//! session starts (`docs/extensions.md`, "Hooks declared in configuration").
//! This module is a child of home's question module, whose closes it owns.
//!
//! The hub is authoritative: only a pending `Kind::Start` refused with
//! `extension_missing` and the exact missing-hooks message opens the
//! offer. The names query and the install both run on background workers
//! through `Configure`, never on the loop thread; their results carry
//! the offer's ticket, so a stale or duplicate result changes nothing.
//! Accepting resends the retained start args unchanged under a fresh
//! command id, and the hub reruns its gate. Declining settles the
//! retained refusal exactly once through the existing rejected paths.

use std::path::PathBuf;

use serde_json::{Value, json};

use super::super::super::{App, Effect, Kind, NoticeLine, Phase, mint};
use super::Prompt;
use crate::configure::ConfigureError;
use crate::home::HooksChoice;
use crate::install_worker::{HooksInstall, HooksJob, HooksQuery};
use crate::keys::Key;
use contract::ErrorCode;

/// The refusal that opens the offer: the hub's gate for declared
/// blocking hooks with no `hooks` extension to run them. Any other
/// missing-extension error settles as today, with no offer.
const MISSING_HOOKS: &str =
    "The `hooks` extension is not installed. Run `fiber extension install hooks`.";

/// The install failure when the link drops mid-install: the connection
/// detail, with the install-command guidance appended by
/// `install_failure_text`.
const INSTALL_CONNECTION_LOST: &str = "Connection lost while installing hooks.";

/// The offer's stage: waiting on the names query, showing the names,
/// or committed to the install. The install commits only from the
/// shown offer, so no installing-without-names state exists.
#[derive(Debug, Clone)]
enum Stage {
    /// The names query is on the worker: the offer shows nothing yet.
    Querying,
    /// The query answered with names: the offer shows them.
    Offered(Vec<String>),
    /// Enter committed the install: keys, clicks, paste and the wheel
    /// cannot start, cancel or alter it.
    Installing(Vec<String>),
}

/// The refused start behind the install offer, until one path settles
/// it: a query answer, a committed install, a decline, or a dropped
/// connection.
#[derive(Debug, Clone)]
pub(in crate::app::home) struct State {
    /// The refused start's command id.
    refused_id: String,
    /// The worker ticket: only results carrying it fold.
    ticket: u64,
    /// The offer's stage: what shows and what input may do.
    stage: Stage,
    /// The original refusal, for the decline and query-error paths.
    code: String,
    message: String,
    /// The refused start's args, read before settlement: the retry
    /// resends them unchanged under a fresh command id.
    args: Value,
}

impl State {
    /// Whether the open offer committed its install: further keys,
    /// paste, close and outside clicks cannot start, cancel or alter
    /// it, and a duplicate or stale result cannot retry it.
    fn installing(&self) -> bool {
        matches!(self.stage, Stage::Installing(_))
    }
}

/// The install failure's text: the install's own error, naming
/// `fiber extension install hooks` when it does not already.
fn install_failure_text(error: &ConfigureError) -> String {
    const COMMAND: &str = "fiber extension install hooks";
    if error.message.contains(COMMAND) {
        return error.message.clone();
    }
    format!(
        "{}. Run `{COMMAND}`.",
        error.message.strip_suffix('.').unwrap_or(&error.message)
    )
}

impl App {
    /// Whether the hooks install offer is open, on home or over the
    /// attached old session: while it is, every key, edit and frame
    /// click reaches it first.
    pub(crate) fn hooks_offer_open(&self) -> bool {
        self.home
            .as_ref()
            .is_some_and(|home| matches!(home.questions.prompt(), Some(Prompt::HooksOffer(_))))
    }

    /// Whether the open offer committed its install: further keys,
    /// paste, close and outside clicks cannot start, cancel or alter
    /// it, and a duplicate or stale result cannot retry it.
    fn hooks_offer_installing(&self) -> bool {
        self.home.as_ref().is_some_and(|home| {
            matches!(
                home.questions.prompt(),
                Some(Prompt::HooksOffer(state)) if state.installing()
            )
        })
    }

    /// The offer's names, once the query answered with a nonempty list:
    /// the draw shows the offer only then. Raw and unsorted here; the
    /// draw sorts and sanitizes for display. The committed install
    /// keeps showing them while it runs.
    pub(crate) fn hooks_offer_names(&self) -> Option<Vec<String>> {
        self.home
            .as_ref()
            .and_then(|home| match home.questions.prompt() {
                Some(Prompt::HooksOffer(state)) => match &state.stage {
                    Stage::Querying => None,
                    Stage::Offered(names) | Stage::Installing(names) => Some(names.clone()),
                },
                _ => None,
            })
    }

    /// Takes the offer's state, closing the prompt: settling the
    /// refused start never reopens it. `None` when no offer is open.
    fn take_hooks_offer(&mut self) -> Option<State> {
        let home = self.home.as_mut()?;
        match home.questions.prompt.take() {
            Some(Prompt::HooksOffer(state)) => Some(state),
            prompt => {
                home.questions.prompt = prompt;
                None
            }
        }
    }

    /// A ticket no live offer waits on: every worker result carrying
    /// it is stale.
    fn hooks_ticket_live(&self, ticket: u64, installing: bool) -> bool {
        self.home.as_ref().is_some_and(|home| {
            matches!(
                home.questions.prompt(),
                Some(Prompt::HooksOffer(state))
                    if state.ticket == ticket && state.installing() == installing
            )
        })
    }

    /// The offer's queued worker job, once: the query the refusal
    /// queued, or the install Enter committed. The loop starts it after
    /// the input.
    pub(crate) fn take_hooks_job(&mut self) -> Option<HooksJob> {
        self.hooks_job.take()
    }

    /// Intercepts a refused `start` for missing blocking hooks: only a
    /// pending start refused with `extension_missing` and the exact
    /// missing-hooks message opens the offer. Anything else settles as
    /// today. True when the refusal is held for the offer's query.
    pub(in crate::app) fn intercept_hooks_offer(
        &mut self,
        id: &str,
        code: &str,
        message: &str,
    ) -> bool {
        if code != "extension_missing" {
            return false;
        }
        if message != MISSING_HOOKS {
            return false;
        }
        if !matches!(self.outbox.pending.get(id), Some((Kind::Start, _))) {
            return false;
        }
        // Another question owns the foot: the refusal settles as today
        // rather than clobbering it.
        if self
            .home
            .as_ref()
            .is_some_and(|home| home.questions.prompt().is_some())
        {
            return false;
        }
        let Some(seam) = self.configure_seam() else {
            return false;
        };
        let Some(line) = self.outbox.start_line(id) else {
            return false;
        };
        let args = serde_json::from_str::<Value>(&line)
            .ok()
            .and_then(|line| line.get("args").cloned())
            .unwrap_or(Value::Null);
        if !args.is_object() {
            return false;
        }
        let workspace = args
            .get("workspace")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let overrides = args
            .get("overrides")
            .and_then(Value::as_array)
            .map(|overrides| {
                overrides
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        self.hooks_ticket = self.hooks_ticket.wrapping_add(1);
        let ticket = self.hooks_ticket;
        self.hooks_job = Some(HooksJob::Query(HooksQuery {
            ticket,
            workspace: PathBuf::from(workspace),
            overrides,
            seam,
        }));
        let state = State {
            refused_id: id.to_owned(),
            ticket,
            stage: Stage::Querying,
            code: code.to_owned(),
            message: message.to_owned(),
            args,
        };
        if let Some(home) = self.home.as_mut() {
            home.questions.prompt = Some(Prompt::HooksOffer(state));
            true
        } else {
            self.hooks_job = None;
            false
        }
    }

    /// A key for the hooks install offer, once open: Enter installs
    /// and retries the refused start, Esc cancels. Anything else holds,
    /// keeping the draft. While the install runs, every key holds.
    pub(in crate::app) fn hooks_offer_key(&mut self, key: &Key) -> Option<Effect> {
        if !self.hooks_offer_open() {
            return None;
        }
        if self.hooks_offer_installing() {
            return Some(Effect::None);
        }
        if matches!(key, Key::Enter) {
            Some(self.commit_hooks_offer())
        } else {
            if matches!(key, Key::Esc) {
                self.decline_hooks_offer();
            }
            Some(Effect::None)
        }
    }

    /// The offer's choice: installing commits the install and the
    /// staged retry, cancelling settles the refused start. A committed
    /// install swallows both, and a closed offer does nothing.
    pub(in crate::app::home) fn hooks_offer_click(&mut self, choice: HooksChoice) -> Effect {
        if !self.hooks_offer_open() {
            return Effect::None;
        }
        if self.hooks_offer_installing() {
            return Effect::None;
        }
        match choice {
            HooksChoice::Install => self.commit_hooks_offer(),
            HooksChoice::Cancel => {
                self.decline_hooks_offer();
                Effect::None
            }
        }
    }

    /// Enter in the hooks install offer: the install runs on a worker,
    /// and its answer retries the refused start. With no seam the
    /// install cannot run, so the offer closes on the failure naming
    /// the install command instead.
    fn commit_hooks_offer(&mut self) -> Effect {
        // Enter commits only what the offer shows: while the names are
        // still on the worker there is nothing to install yet, so it
        // holds like every other key.
        let ticket = match self.home.as_ref().and_then(|home| home.questions.prompt()) {
            Some(Prompt::HooksOffer(state)) if matches!(state.stage, Stage::Offered(_)) => {
                state.ticket
            }
            _ => return Effect::None,
        };
        let Some(seam) = self.configure_seam() else {
            let error = crate::configure::ConfigureError {
                code: ErrorCode::IoFailed,
                message: "Installing is not available in this terminal.".to_owned(),
            };
            self.fail_hooks_install(error);
            return Effect::None;
        };
        // A remote without a hub link fails at once, with no job: the
        // worker would only repeat the hub's own refusal to route.
        if self.remote().is_some() && !self.outbox.is_up() {
            let error = crate::configure::ConfigureError {
                code: ErrorCode::IoFailed,
                message: "The hub is not connected.".to_owned(),
            };
            self.fail_hooks_install(error);
            return Effect::None;
        }
        if let Some(home) = self.home.as_mut()
            && let Some(Prompt::HooksOffer(state)) = home.questions.prompt.as_mut()
            && state.ticket == ticket
        {
            state.stage = match std::mem::replace(&mut state.stage, Stage::Querying) {
                Stage::Offered(names) => Stage::Installing(names),
                stage @ (Stage::Querying | Stage::Installing(_)) => stage,
            };
        }
        self.hooks_job = Some(HooksJob::Install(HooksInstall { ticket, seam }));
        Effect::None
    }

    /// Settles the refused start behind the offer through the existing
    /// rejected paths: the clear-staged start through its clear path,
    /// anything else through the ordinary refusal. Exactly one notice.
    fn settle_refused(&mut self, state: &State) {
        if !self.clear_rejected(&state.refused_id, &state.code, &state.message) {
            self.rejected(&state.refused_id, &state.code, state.message.clone());
        }
    }

    /// Esc, the ✕ or a click outside in the hooks install offer: no
    /// session starts and no hook point runs. The retained refusal
    /// settles exactly once through the existing rejected paths, so an
    /// ordinary start restores its draft and a clear-staged start stays
    /// attached to the old session.
    fn decline_hooks_offer(&mut self) {
        let Some(state) = self.take_hooks_offer() else {
            return;
        };
        self.hooks_job = None;
        self.settle_refused(&state);
    }

    /// Closes the offer on an install that never runs — no seam, or no
    /// hub link for a remote — settling the refused start silently and
    /// presenting the failure naming the install command instead.
    /// A connection loss the worker reports is the install's own
    /// failure, not the link's condition: it folds as the
    /// `extension_failed` event, so the reconnect's clearing of
    /// `connection_failed` notices never erases it, while its text
    /// keeps the connection detail and the install-command guidance.
    /// Either way no start is retried. The pushed notice is stashed
    /// while it stands, so a dropped connection can upgrade a raw
    /// worker error to the connection-loss text.
    fn fail_hooks_install(&mut self, error: ConfigureError) {
        if let Some(state) = self.take_hooks_offer() {
            let code = if matches!(error.code, ErrorCode::ConnectionFailed) {
                "extension_failed".to_owned()
            } else {
                let code = crate::format::code(&error.code);
                if code.is_empty() {
                    "io_failed".to_owned()
                } else {
                    code
                }
            };
            self.settle_hooks_start(&state);
            let text = install_failure_text(&error);
            self.hooks_failed = Some((code.clone(), text.clone()));
            self.push_notice(&code, text);
        }
    }

    /// Settles the refused start behind the offer without its refusal
    /// notice: the install's own failure replaces it. An ordinary
    /// start restores its draft and phase; a clear-staged start stays
    /// attached to the old session.
    fn settle_hooks_start(&mut self, state: &State) {
        if let Some(pending) = self.clear_pending.take() {
            if pending.command_id == state.refused_id {
                self.outbox.answered(&state.refused_id);
                return;
            }
            self.clear_pending = Some(pending);
        }
        self.fail(&state.refused_id);
        self.outbox.answered(&state.refused_id);
    }

    /// Folds the names query's answer: a nonempty list opens the offer,
    /// while an empty list or a query error settles the original
    /// refusal with no offer and no session. A result for a stale
    /// ticket, or one arriving after the install committed, does
    /// nothing.
    pub(crate) fn on_hooks_offer_names(
        &mut self,
        ticket: u64,
        result: Result<Vec<String>, ConfigureError>,
    ) {
        if !self.hooks_ticket_live(ticket, false) {
            return;
        }
        let names = match result {
            Ok(names) if !names.is_empty() => names,
            _ => {
                let Some(state) = self.take_hooks_offer() else {
                    return;
                };
                self.hooks_job = None;
                self.settle_refused(&state);
                return;
            }
        };
        // The query's answer folds once: a duplicate or late result for
        // the live ticket finds the offer past its query and does
        // nothing.
        if let Some(home) = self.home.as_mut()
            && let Some(Prompt::HooksOffer(state)) = home.questions.prompt.as_mut()
            && state.ticket == ticket
            && matches!(state.stage, Stage::Querying)
        {
            state.stage = Stage::Offered(names);
        }
    }

    /// Folds the install's answer: success resends the retained start
    /// args unchanged under a fresh command id, and the hub reruns its
    /// gate; failure replaces the offer with its error, naming
    /// `fiber extension install hooks`. Either way no second result
    /// for the ticket can retry.
    pub(crate) fn on_hooks_installed(
        &mut self,
        ticket: u64,
        result: Result<(), ConfigureError>,
    ) -> Vec<String> {
        if !self.hooks_ticket_live(ticket, true) {
            return Vec::new();
        }
        match result {
            Ok(()) => {
                let Some(state) = self.take_hooks_offer() else {
                    return Vec::new();
                };
                let id = mint();
                let line = json!({"id": id, "command": "start", "args": state.args}).to_string();
                let draft = self
                    .outbox
                    .fail(&state.refused_id)
                    .map(|(_, draft)| draft)
                    .unwrap_or_default();
                if let Some(pending) = self.clear_pending.as_mut() {
                    // The retry is actually sent now: the clear's close
                    // follows only this new start's answer.
                    if pending.command_id == state.refused_id {
                        pending.command_id = id.clone();
                    }
                } else {
                    self.phase = Phase::Pending {
                        command_id: id.clone(),
                    };
                }
                match self.outbox.send(id, Kind::Start, draft, line) {
                    Some(line) => vec![line],
                    None => Vec::new(),
                }
            }
            Err(error) => {
                // The install's own failure replaces the offer: no
                // session starts, and no second result can retry.
                self.fail_hooks_install(error);
                Vec::new()
            }
        }
    }

    /// Drops the offer after a dropped connection: the queued job never
    /// runs and a late worker result finds no live ticket. A committed
    /// install cannot retry, so the failure names the install command
    /// with the connection lost; a refused start still waiting settles
    /// silently when Attached, and through the existing unanswered
    /// settle on reconnect when Starting. A reconnect never replays
    /// the start. When the worker already folded its failure, the
    /// disconnect upgrades that notice to the same connection-loss
    /// text instead of leaving the worker's raw error beside the
    /// link's own notice.
    pub(crate) fn hooks_disconnected(&mut self) {
        if self.hooks_offer_installing() {
            self.hooks_job = None;
            let error = ConfigureError {
                code: ErrorCode::ConnectionFailed,
                message: INSTALL_CONNECTION_LOST.to_owned(),
            };
            self.fail_hooks_install(error);
        } else if let Some(state) = self.take_hooks_offer() {
            self.hooks_job = None;
            if self
                .clear_pending
                .as_ref()
                .is_some_and(|pending| pending.command_id == state.refused_id)
            {
                self.settle_hooks_start(&state);
            }
        }
        self.upgrade_hooks_failure();
    }

    /// Upgrades the stashed install failure to the connection-loss text
    /// while it still stands: a worker result that folded before the
    /// loop noticed the dead link names the worker's raw error, while
    /// the disconnect both orders share names the install command with
    /// the connection lost. A faded or dismissed failure stays gone;
    /// either way the stash clears, so one failure upgrades at most
    /// once and no start is retried.
    fn upgrade_hooks_failure(&mut self) {
        let Some((code, identity)) = self.hooks_failed.take() else {
            return;
        };
        let standing = self.notices().into_iter().any(|line| {
            matches!(
                line,
                NoticeLine::Notice { code: c, text: t, .. } if c == code && t == identity
            )
        });
        if !standing {
            return;
        }
        self.notices.clear_code(&code, &identity);
        let error = ConfigureError {
            code: ErrorCode::ConnectionFailed,
            message: INSTALL_CONNECTION_LOST.to_owned(),
        };
        self.push_notice("extension_failed", install_failure_text(&error));
    }
}

#[cfg(test)]
#[path = "hooks_offer_tests.rs"]
mod tests;

#[cfg(test)]
impl App {
    /// The offer tests' driver: sends a start — typing `text` and
    /// Enter on home, `/clear` over the attached session, whose status
    /// the caller folded — then refuses it for missing blocking hooks
    /// through the real interception path. The names query stays
    /// queued for the caller to take and answer. `None` when no start
    /// went out or another question holds the foot.
    pub(in crate::app) fn refuse_start_for_offer(&mut self, text: &str) -> Option<String> {
        let id = self.refuse_start_for_offer_inner(text);
        self.settle();
        id
    }

    fn refuse_start_for_offer_inner(&mut self, text: &str) -> Option<String> {
        use contract::clock::Clock;
        let now = fakes::clock::FakeClock::new().now();
        if self.session().is_none() {
            for ch in text.chars() {
                self.on_key_inner(crate::keys::Key::Char(ch), now);
            }
            if !matches!(
                self.on_key_inner(crate::keys::Key::Enter, now),
                Effect::Send(_)
            ) {
                return None;
            }
        } else if self.clear_pending.is_none() && !matches!(self.clear(), Effect::Send(_)) {
            return None;
        }
        let id = self
            .outbox
            .pending
            .iter()
            .find_map(|(id, (kind, _))| matches!(kind, Kind::Start).then(|| id.clone()))?;
        let refused = crate::link::Line::Hub(contract::HubLine {
            kind: "command_rejected".to_owned(),
            ts: 0,
            schema_version: contract::SCHEMA_VERSION,
            payload: [
                (
                    "command_id".to_owned(),
                    serde_json::Value::String(id.clone()),
                ),
                (
                    "code".to_owned(),
                    serde_json::Value::String("extension_missing".to_owned()),
                ),
                (
                    "message".to_owned(),
                    serde_json::Value::String(MISSING_HOOKS.to_owned()),
                ),
            ]
            .into_iter()
            .collect(),
        });
        self.on_line_inner(refused);
        self.hooks_offer_open().then_some(id)
    }
}
