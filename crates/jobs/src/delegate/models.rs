//! The delegate model catalogue (`docs/delegates.md`, "Choosing a model"):
//! the configured roles, what each maps to now, and the full references
//! available. The catalogue is built once per session from the same
//! providers and configuration snapshot the resolver reads, so a refusal
//! and `delegate_models` never disagree within a session. The tool
//! definition never lists roles or references: they change when
//! configuration changes, when a provider withdraws a model, or when model
//! discovery refreshes, and a definition that changes during a session
//! misses the whole prompt cache.

use std::sync::Arc;

use contract::emit::Emit;
use contract::provider::ToolDefinition;
use contract::shapes::{ContentPart, DeclaredEffects, Effect};
use contract::tool::{Cancel, Effects, EffectsError, Output, Tool};
use serde_json::{Map, Value, json};

/// Resolves a `delegate_spawn` `model` argument to the `provider/model[:level]`
/// the child is started with, or refuses it with one reason sentence, and
/// carries the catalogue text every refusal lists. Whoever builds a
/// `Resolve` builds `lookup` and `catalogue` from one immutable snapshot,
/// so every role the catalogue lists without ` (unavailable)` looks up
/// `Ok` to its listed target, every role marked ` (unavailable)` looks up
/// `Err`, and every listed reference looks up `Ok`. `lookup` captures only
/// owned, cloned data, so a later configuration write cannot change it.
#[derive(Clone)]
pub struct Resolve {
    /// Resolves the argument to `provider/model[:level]`, or refuses it
    /// with one reason sentence, without the catalogue.
    #[allow(
        clippy::type_complexity,
        reason = "the resolver seam: one closure type shared by the session resolver and every test stub"
    )]
    pub lookup: Arc<dyn Fn(&str) -> Result<String, String> + Send + Sync>,
    /// The full text `delegate_models` returns and every model refusal
    /// lists.
    pub catalogue: Arc<str>,
    /// Resolves the `credential` label a role names for the same `model`
    /// argument `lookup` took, after `lookup` was `Ok`: `Ok(Some(label))`
    /// when the role names a label the provider has, `Ok(None)` for a
    /// full reference or a role without one, and `Err` with the full
    /// `credential_missing` message when the label names no credential.
    /// The label set is read at call time, so a `fiber login` before the
    /// spawn changes the answer.
    #[allow(
        clippy::type_complexity,
        reason = "the resolver seam: one closure type shared by the session resolver and every test stub"
    )]
    pub credential: Arc<dyn Fn(&str) -> Result<Option<String>, String> + Send + Sync>,
}

/// Returns the catalogue with the quota appended on top: the configured
/// roles, what each maps to now, the full references available, and one
/// quota entry per provider credential label (`docs/delegates.md`,
/// "The tools"). A refusal carries the catalogue alone; only
/// `delegate_models` appends the quota. The quota is read on every call,
/// so a `delegate_models` call never answers a stale figure.
pub struct DelegateModels {
    /// The catalogue text, built with the session's resolver.
    pub catalogue: Arc<str>,
    /// Renders the quota section, read on every call.
    pub quota: Arc<dyn Fn() -> String + Send + Sync>,
}

impl Tool for DelegateModels {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "delegate_models".to_owned(),
            description: "Lists the configured delegate roles, what each maps to now, \
                 and the full model references available, for `delegate_spawn`'s `model`."
                .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": [],
                "additionalProperties": false
            }),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, _arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Ok(Effects {
            declared: DeclaredEffects {
                effects: vec![Effect::Reads],
                reversible: true,
                paths: None,
                confined: false,
            },
            subject: Some(String::new()),
            prefix: None,
            // Reading the catalogue changes nothing, so the call takes
            // the fast path and never reaches a reviewer
            // (`docs/permissions.md`, "Fast paths").
            always_reviewed: false,
            hosts: None,
        })
    }

    fn run(
        &self,
        _arguments: &Map<String, Value>,
        _cancel: &dyn Cancel,
        _emit: &dyn Emit,
    ) -> Output {
        Output {
            content: vec![ContentPart::Text {
                text: format!("{}\n{}", self.catalogue, (self.quota)()),
            }],
            ..Output::default()
        }
    }
}

#[cfg(test)]
#[path = "models_tests.rs"]
mod tests;
