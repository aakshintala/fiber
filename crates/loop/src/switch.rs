//! Switching the session's model or thinking level at the next turn
//! boundary (`docs/prompt-cache.md`, "Switching model").
//!
//! Preparation reads nothing and runs nothing: `main`'s closure composes
//! the same calls `parts_in` makes for the startup model, and every
//! cross-command rule lives here in `loop` (`docs/architecture.md`, "The
//! call rules").

use std::path::PathBuf;
use std::sync::Arc;

use contract::events::{Event, ModelSettings, Notice, SwitchSource};
use contract::inbox::{Ack, Rejection};
use contract::tool::Tool;
use contract::{ErrorCode, ThinkingLevel};

use crate::inbox::{CLOSING, accept, reject};
use crate::{Error, Loop};

/// What a prepared switch carries: everything applying it replaces.
pub struct Prepared {
    /// The new model's provider.
    /// debt: only the session's and the reviewer's providers (#1094);
    /// #1094 lifts it after #649 merges.
    pub provider: Arc<dyn contract::provider::Provider>,
    /// The new model, and how its calls are priced.
    pub model: crate::Model,
    /// The level sent.
    pub thinking: Option<ThinkingLevel>,
    /// The session's explicit choice after this switch.
    pub chosen: Option<ThinkingLevel>,
    /// The credential label every request uses.
    /// debt: only a credential read at startup; no command runs for a
    /// switch (#1094). #1094 lifts it after #649 merges.
    pub credential: Option<String>,
    /// The prompt-cache lifetime.
    pub cache_lifetime: contract::events::CacheLifetime,
    /// The model's context window, in tokens.
    pub context_window: u64,
    /// The model's addendum.
    pub addendum: Option<String>,
    /// Automatic handoff's settings for the new model.
    pub handoff: crate::HandoffSettings,
    /// Who judges step 7's calls under the new model.
    pub reviewer: Result<crate::Reviewer, contract::shapes::Failure>,
    /// What applying the switch does to the hosted search tool
    /// (`docs/tools.md`, "Hosted by the provider").
    pub web_search: Hosted,
    /// A configured thinking level the new model lacks.
    pub notice: Option<Notice>,
    /// Runs once, when the switch applies; never for a rejected switch or
    /// one that changes no setting.
    pub applied: Option<Box<dyn FnOnce() + Send>>,
    /// Canonical paths of the `file` credential sources this preparation
    /// read: the loop adds each to its credential deny when `prepare`
    /// returns (`docs/permissions.md`, "Credentials").
    pub credential_files: Vec<PathBuf>,
}

/// The hosted search tool after a switch applies. The tool set changes
/// only when the preamble is built, so the switch's rebuild declares it
/// (`docs/tools.md`, "Which tools the model sees").
pub enum Hosted {
    /// The tools stay as they are.
    Keep,
    /// Registers the tool by `builtin` under its own name, replacing any
    /// tool of that name.
    Declare(Arc<dyn Tool>),
    /// Removes the tool of this name.
    Withdraw(String),
}

/// What the session started with: the session's own thinking choice.
#[derive(Debug, Clone)]
pub struct Switchable {
    /// The session's own explicit choice at start.
    pub chosen: Option<ThinkingLevel>,
}

/// Prepares a switch without changing any state: resolves the typed
/// reference, thinking, credential, reviewer and hosted search from what
/// startup built. A rejection leaves nothing changed.
pub type Prepare = Arc<
    dyn Fn(&contract::commands::ModelArgs, Option<ThinkingLevel>) -> Result<Prepared, Rejection>
        + Send
        + Sync,
>;

/// Why a switch is rejected when no switcher was set.
pub const NO_SWITCH: &str = "This session cannot switch model.";

impl Loop {
    /// Prepares switches with `prepare`: what startup composed for a
    /// second model. `at_start` is the session's own choice at start.
    pub fn switcher(mut self, prepare: Prepare, at_start: Switchable) -> Self {
        // A resumed `chosen` stands when the switcher names none: the
        // fold already seeded it from the last `model_changed`.
        if at_start.chosen.is_some() {
            self.chosen = at_start.chosen;
        }
        self.switcher = Some((prepare, at_start));
        self
    }

    /// The session's current model settings, as `model_changed` records
    /// them.
    fn settings(&self) -> ModelSettings {
        ModelSettings {
            model: self.model.reference.clone(),
            thinking: self.prompt.thinking.map(|level| level.as_str().to_owned()),
            cache_lifetime: self.prompt.cache_lifetime,
            credential: self.prompt.credential.clone(),
        }
    }

    /// Admits a `model` command. `idle` is whether the loop is waiting for
    /// a turn: an idle switch applies at once, a switch admitted during a
    /// turn waits in `pending` for the next turn boundary. `closing` is
    /// checked before `prepare`, the loop's own checks after it, and the
    /// acknowledgement answers only once the switch is accepted or
    /// rejected. A rejection changes nothing but the credential deny, which
    /// only grows.
    pub(crate) fn take_switch(
        &mut self,
        args: contract::commands::ModelArgs,
        ack: Ack,
        idle: bool,
    ) -> Result<(), Error> {
        if self.closing {
            reject(ack, ErrorCode::Closing, CLOSING);
            return Ok(());
        }
        let Some((prepare, _)) = self.switcher.as_ref() else {
            reject(ack, ErrorCode::InvalidArguments, NO_SWITCH);
            return Ok(());
        };
        let prepare = Arc::clone(prepare);
        let chosen = self
            .pending
            .last()
            .map(|queued| queued.chosen)
            .unwrap_or(self.chosen);
        let mut prepared = match prepare(&args, chosen) {
            Ok(prepared) => prepared,
            Err(rejection) => {
                reject(ack, rejection.code, &rejection.message);
                return Ok(());
            }
        };
        // The file is denied from the moment it was read: a call later in
        // this turn, before the switch applies, is refused too.
        deny_also(
            &mut self.credential_files,
            std::mem::take(&mut prepared.credential_files),
        );
        if let Ok(reviewer) = &prepared.reviewer
            && reviewer.model.reference == prepared.model.reference
        {
            reject(
                ack,
                ErrorCode::InvalidArguments,
                &format!(
                    "`{}` is this session's reviewer model; set `reviewer.model` to another model first.",
                    prepared.model.reference
                ),
            );
            return Ok(());
        }
        accept(ack);
        self.pending.push(prepared);
        if idle {
            self.apply_switches()?;
        }
        Ok(())
    }

    /// Applies every queued switch in order, each with its own
    /// `model_changed`. At most one `preamble_built` follows, at the next
    /// turn's build. A switch that changes no setting writes nothing but
    /// still updates the session's choice.
    pub(crate) fn apply_switches(&mut self) -> Result<(), Error> {
        for prepared in std::mem::take(&mut self.pending) {
            let before = self.settings();
            let after = ModelSettings {
                model: prepared.model.reference.clone(),
                thinking: prepared.thinking.map(|level| level.as_str().to_owned()),
                cache_lifetime: prepared.cache_lifetime,
                credential: prepared.credential.clone(),
            };
            if before == after {
                self.chosen = prepared.chosen;
                continue;
            }
            crate::util::write(
                &self.log,
                &mut self.conversation,
                &mut self.reviewed,
                &self.model.reference,
                &Event::ModelChanged(contract::events::ModelChanged {
                    before,
                    after,
                    source: SwitchSource::Driver,
                }),
                None,
                None,
                &mut self.changes.had,
                &mut self.handoff.carry,
            )?;
            if let Some(notice) = prepared.notice {
                crate::util::write(
                    &self.log,
                    &mut self.conversation,
                    &mut self.reviewed,
                    &self.model.reference,
                    &Event::Notice(notice),
                    None,
                    None,
                    &mut self.changes.had,
                    &mut self.handoff.carry,
                )?;
            }
            self.provider = prepared.provider;
            self.model = prepared.model;
            self.prompt.thinking = prepared.thinking;
            self.prompt.credential = prepared.credential;
            self.prompt.cache_lifetime = prepared.cache_lifetime;
            self.prompt.context_window = prepared.context_window;
            self.prompt.addendum = prepared.addendum;
            self.handoff.settings = prepared.handoff;
            self.reviewer = prepared.reviewer;
            self.reviewer_sent = None;
            match prepared.web_search {
                Hosted::Keep => {}
                Hosted::Declare(tool) => {
                    let definition = tool.definition();
                    self.tools.insert(
                        definition.name.clone(),
                        ("builtin".to_owned(), tool, definition),
                    );
                }
                Hosted::Withdraw(name) => {
                    self.tools.remove(&name);
                }
            }
            if let Some(applied) = prepared.applied {
                applied();
            }
            self.chosen = prepared.chosen;
            self.preamble = None;
            self.preamble_reason = contract::events::PreambleReason::Switch;
            if self.last_request.is_some() {
                self.last_request = None;
                self.warm_stopped = Some(self.log.clock().now());
            }
        }
        Ok(())
    }
}

/// Adds each of `read` to the credential deny `denied`, resolved as a
/// configured source is at start, unless it is already there. The deny
/// only grows.
fn deny_also(denied: &mut Vec<PathBuf>, read: Vec<PathBuf>) {
    for path in read {
        let path = crate::permission::resolved(path);
        if !denied.contains(&path) {
            denied.push(path);
        }
    }
}

#[cfg(test)]
#[path = "switch_tests.rs"]
mod tests;
