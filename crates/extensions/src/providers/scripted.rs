//! The built-in `scripted` provider in a session's registry
//! (`docs/model-routing.md`, "The scripted provider"). It is added only for
//! a reference that names it, never by [`Providers::load`], so the model
//! picker and `fiber models` never list it.

use std::collections::BTreeMap;

use config::{ModelData, Protocol, ProviderData};
use serde_json::Map;

use super::Providers;

#[cfg(test)]
#[path = "scripted_tests.rs"]
mod tests;

/// The built-in provider's name.
pub(super) const SCRIPTED: &str = "scripted";

/// A scripted model's context window, in tokens: large enough that no size
/// notice or handoff fires on a script.
pub(crate) const SCRIPTED_CONTEXT_WINDOW: u64 = 1_000_000;

impl Providers {
    /// Adds the model `typed` names when it is `scripted/<path>`, with or
    /// without a `:<level>` suffix: a model whose id is the script's path,
    /// under a provider named `scripted` with no credential, quota, cost or
    /// prompt cache. A reference to another provider, or one naming no
    /// path, adds nothing; a path already added is added once.
    pub fn add_scripted(&mut self, typed: &str) {
        let (rest, _) = Self::split_thinking(typed);
        let Some(id) = rest.strip_prefix("scripted/") else {
            return;
        };
        if id.is_empty() {
            return;
        }
        let data = self
            .by_name
            .entry(SCRIPTED.to_owned())
            .or_insert_with(|| ProviderData {
                name: SCRIPTED.to_owned(),
                credential: None,
                credential_name: None,
                headers: BTreeMap::new(),
                placeholders: BTreeMap::new(),
                models: Vec::new(),
                reviewer_model: None,
            });
        if data.models.iter().any(|model| model.id == id) {
            return;
        }
        data.models.push(ModelData {
            id: id.to_owned(),
            protocol: Protocol::Scripted,
            base_url: String::new(),
            compat: Map::new(),
            deferred_tools: false,
            extra_body: Map::new(),
            context_window: Some(SCRIPTED_CONTEXT_WINDOW),
            max_output_tokens: None,
            input: Vec::new(),
            cost: None,
            subscription: false,
            web_search: None,
            thinking_levels: Vec::new(),
            thinking_default: None,
            prompt_addendum: None,
        });
    }
}
