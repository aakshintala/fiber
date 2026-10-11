//! Switching the session's model, thinking level or credential label at the
//! next turn boundary (`docs/prompt-cache.md`, "Switching model").
//!
//! `main`'s closure composes the same calls `parts_in` makes for the
//! startup model. It may read the new model's credential, or start its Lua
//! provider, on a thread of its own; the loop waits for it, and shutdown
//! ends that wait (`docs/configuration.md`, "Secrets"). Every cross-command
//! rule lives here in `loop` (`docs/architecture.md`, "The call rules").

use std::path::PathBuf;
use std::sync::Arc;

use contract::events::{Event, ModelChanged, ModelSettings, Notice, SwitchSource};
use contract::inbox::{Ack, Rejection};
use contract::tool::Tool;
use contract::{ErrorCode, ThinkingLevel};

use crate::inbox::{CLOSING, accept, reject};
use crate::{Error, Loop};

/// What a prepared switch carries: everything applying it replaces.
pub struct Prepared {
    /// The new model's provider.
    pub provider: Arc<dyn contract::provider::Provider>,
    /// The new model, and how its calls are priced.
    pub model: crate::Model,
    /// The level sent.
    pub thinking: Option<ThinkingLevel>,
    /// The session's explicit choice after this switch.
    pub chosen: Option<ThinkingLevel>,
    /// The credential label every request uses.
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
///
/// `label` is `Some` for a `credential` command: the label to switch to on
/// `args.model`'s provider. `None` keeps the provider's selected label.
pub type Prepare = Arc<
    dyn Fn(
            &contract::commands::ModelArgs,
            Option<&str>,
            Option<ThinkingLevel>,
        ) -> Result<Prepared, Rejection>
        + Send
        + Sync,
>;

/// What a switch command asked for: a `model` command's arguments, or a
/// `credential` command's label, switched on the model queued before it.
pub(crate) enum Asked {
    /// A `model` command's arguments.
    Model(contract::commands::ModelArgs),
    /// A `credential` command's label.
    Credential(contract::commands::CredentialArgs),
}

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

    /// Admits a `model` or `credential` command. `idle` is whether the loop
    /// is waiting for a turn: an idle switch applies at once, a switch admitted during a
    /// turn waits in `pending` for the next turn boundary. `closing` is
    /// checked before `prepare`, the loop's own checks after it, and the
    /// acknowledgement answers only once the switch is accepted or
    /// rejected. A rejection changes nothing but the credential deny, which
    /// only grows.
    pub(crate) fn take_switch(&mut self, asked: Asked, ack: Ack, idle: bool) -> Result<(), Error> {
        if self.closing {
            reject(ack, ErrorCode::Closing, CLOSING);
            return Ok(());
        }
        let Some((prepare, _)) = self.switcher.as_ref() else {
            reject(ack, ErrorCode::InvalidArguments, NO_SWITCH);
            return Ok(());
        };
        let prepare = Arc::clone(prepare);
        // A credential command switches the queued model's provider, not
        // the current one, so a label is checked against the provider it
        // switches (`docs/model-routing.md`, "Which credential a session uses").
        let (args, label) = match asked {
            Asked::Model(args) => (args, None),
            Asked::Credential(credential) => {
                let model = self
                    .pending
                    .last()
                    .map(|queued| queued.model.reference.clone())
                    .unwrap_or_else(|| self.model.reference.clone());
                (
                    contract::commands::ModelArgs {
                        model,
                        thinking: None,
                    },
                    Some(credential.label),
                )
            }
        };
        let chosen = self
            .pending
            .last()
            .map(|queued| queued.chosen)
            .unwrap_or(self.chosen);
        let mut prepared = match prepare(&args, label.as_deref(), chosen) {
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

    /// Called once by `Loop::resume`: writes `model_changed` with `before`
    /// = `recorded` when `recorded` is `Some`, its `credential` is `Some`,
    /// and that label differs from the session's label. A rewound loop
    /// passes `None` and writes nothing (`docs/events.md`,
    /// "`model_changed`").
    pub(crate) fn resumed_label(&mut self, recorded: Option<ModelSettings>) -> Result<(), Error> {
        let Some(recorded) = recorded else {
            return Ok(());
        };
        // A `preamble_built` with no `credential` field folds to settings
        // with `credential: None`, and records no label to switch from; the
        // same label as the session's writes nothing either.
        if recorded.credential.is_none() || recorded.credential == self.prompt.credential {
            return Ok(());
        }
        let after = self.settings();
        self.write(
            &Event::ModelChanged(ModelChanged {
                before: recorded,
                after,
                source: SwitchSource::Driver,
            }),
            None,
            None,
        )?;
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
            self.write(
                &Event::ModelChanged(contract::events::ModelChanged {
                    before,
                    after,
                    source: SwitchSource::Driver,
                }),
                None,
                None,
            )?;
            if let Some(notice) = prepared.notice {
                self.write(&Event::Notice(notice), None, None)?;
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
            self.warming.stop(self.log.clock().now());
        }
        Ok(())
    }
}

/// Adds each of `read` to the credential deny `denied`, unless it is
/// already there. Each path is the canonical file the credential read
/// actually read, so it joins as given: resolving it again could follow
/// a symlink swapped in after the read and protect a file never read
/// instead. The deny only grows.
fn deny_also(denied: &mut Vec<PathBuf>, read: Vec<PathBuf>) {
    for path in read {
        if !denied.contains(&path) {
            denied.push(path);
        }
    }
}

#[cfg(test)]
#[path = "switch_tests.rs"]
mod tests;
