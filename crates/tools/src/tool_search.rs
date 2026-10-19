//! The `tool_search` tool (`docs/tools.md`, "Tool search"): BM25 over the
//! deferred tools' names, descriptions and parameter names, loading the
//! best matches so the model can call them.

use std::sync::Arc;

use contract::emit::Emit;
use contract::provider::{NO_MATCH, TOOL_SEARCH, ToolDefinition, loaded_text};
use contract::shapes::ContentPart;
use contract::tool::{Cancel, Effects, EffectsError, Output, Tool};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::tool_util::{errored, no_effects};

/// The base description, sent when no source names a deferred tool.
const BASE_DESCRIPTION: &str = "Searches the deferred tools by name, description and parameter names, and loads the best matches so you can call them.";

/// The default cap: how many tools a call without `limit` loads.
const DEFAULT_LIMIT: usize = 8;

/// BM25's term saturation.
const K1: f64 = 1.2;
/// BM25's length normalization.
const B: f64 = 0.75;

/// Finds deferred tools by name, description and parameter names. It holds
/// its catalogue immutably, so concurrent calls in one step share no
/// mutable state.
pub struct ToolSearch {
    catalogue: Arc<Vec<ToolDefinition>>,
    description: String,
}

impl ToolSearch {
    /// Searches `catalogue`, naming `sources` in its description: the
    /// sorted, de-duplicated `registered_by` of the deferred tools, leaving
    /// out `builtin`.
    pub fn new(catalogue: Vec<ToolDefinition>, sources: Vec<String>) -> Self {
        let mut sources = sources;
        sources.sort();
        sources.dedup();
        let mut description = BASE_DESCRIPTION.to_owned();
        if !sources.is_empty() {
            description.push_str(&format!(
                " Deferred tools come from: {}.",
                sources.join(", ")
            ));
        }
        Self {
            catalogue: Arc::new(catalogue),
            description,
        }
    }
}

/// The call's arguments, checked against the schema before the call runs.
#[derive(Debug)]
struct Args {
    query: String,
    limit: Limit,
}

/// How many tools to load: the default, an explicit cap, or every match.
#[derive(Debug)]
enum Limit {
    /// No `limit` was passed: load at most 8 tools.
    Default,
    /// Load at most this many tools.
    AtMost(u64),
    /// A `limit` the JSON reader cannot hold as a `u64`: load every match.
    Every,
}

impl<'de> Deserialize<'de> for Args {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Raw {
            query: String,
            #[serde(default)]
            limit: Option<Value>,
        }
        let raw = Raw::deserialize(deserializer)?;
        if raw.query.is_empty() {
            return Err(serde::de::Error::custom("query must not be empty"));
        }
        let limit = match raw.limit {
            None => Limit::Default,
            Some(Value::Number(count)) if count.is_u64() => {
                let capped = count.as_u64().unwrap_or(u64::MAX);
                if capped < 1 {
                    return Err(serde::de::Error::custom("limit must be at least 1"));
                }
                Limit::AtMost(capped)
            }
            Some(Value::Number(count)) if count.is_i64() => {
                return Err(serde::de::Error::custom("limit must be at least 1"));
            }
            Some(Value::Number(count)) => {
                let past_u64 = count
                    .as_f64()
                    .is_some_and(|float| float.is_finite() && float >= 18446744073709551616.0);
                if past_u64 {
                    Limit::Every
                } else {
                    return Err(serde::de::Error::custom(
                        "limit must be an integer of at least 1",
                    ));
                }
            }
            Some(_) => {
                return Err(serde::de::Error::custom(
                    "limit must be an integer of at least 1",
                ));
            }
        };
        Ok(Args {
            query: raw.query,
            limit,
        })
    }
}

/// A tool's document: the tokens of its name, its description and the
/// keys of its schema's top-level `properties`.
fn document_tokens(tool: &ToolDefinition) -> Vec<String> {
    let mut tokens = tokenize(&tool.name);
    tokens.extend(tokenize(&tool.description));
    if let Some(properties) = tool
        .input_schema
        .get("properties")
        .and_then(Value::as_object)
    {
        for key in properties.keys() {
            tokens.extend(tokenize(key));
        }
    }
    tokens
}

/// Splits text into maximal runs of Unicode alphanumerics, lowercased, so
/// `mcp__fx__echo` gives `mcp`, `fx`, `echo`.
fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for character in text.chars() {
        if character.is_alphanumeric() {
            current.extend(character.to_lowercase());
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

impl ToolSearch {
    /// The loaded names for `query`, best first, ties by name bytes.
    fn search(&self, query: &str, limit: Limit) -> Vec<String> {
        let mut terms = tokenize(query);
        terms.sort();
        terms.dedup();
        if terms.is_empty() {
            return Vec::new();
        }
        let documents: Vec<Vec<String>> = self.catalogue.iter().map(document_tokens).collect();
        if documents.is_empty() {
            return Vec::new();
        }
        let count = documents.len() as f64;
        let average = documents.iter().map(Vec::len).sum::<usize>() as f64 / count;
        let inverse: Vec<f64> = terms
            .iter()
            .map(|term| {
                let holding = documents
                    .iter()
                    .filter(|tokens| tokens.iter().any(|token| token == term))
                    .count() as f64;
                (1.0 + (count - holding + 0.5) / (holding + 0.5)).ln()
            })
            .collect();
        let mut ranked: Vec<(&str, f64)> = Vec::new();
        for (tool, tokens) in self.catalogue.iter().zip(documents.iter()) {
            let mut score = 0.0;
            let mut matched = false;
            for (term, idf) in terms.iter().zip(inverse.iter()) {
                let hits = tokens.iter().filter(|token| *token == term).count() as f64;
                if hits == 0.0 {
                    continue;
                }
                matched = true;
                let length = tokens.len() as f64;
                score += idf * (hits * (K1 + 1.0)) / (hits + K1 * (1.0 - B + B * length / average));
            }
            if matched {
                ranked.push((tool.name.as_str(), score));
            }
        }
        ranked.sort_by(|left, right| {
            right
                .1
                .total_cmp(&left.1)
                .then_with(|| left.0.as_bytes().cmp(right.0.as_bytes()))
        });
        let cap = match limit {
            Limit::Default => DEFAULT_LIMIT,
            Limit::AtMost(capped) => usize::try_from(capped).unwrap_or(usize::MAX),
            Limit::Every => usize::MAX,
        };
        ranked
            .into_iter()
            .take(cap)
            .map(|(name, _)| name.to_owned())
            .collect()
    }
}

impl Tool for ToolSearch {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: TOOL_SEARCH.to_owned(),
            description: self.description.clone(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "minLength": 1,
                        "description": "Words describing the tool you need."
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "The most tools to load; 8 when absent."
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        let _args: Args =
            crate::tool_util::arguments(arguments).map_err(EffectsError::Arguments)?;
        let mut effects = no_effects();
        effects.hosts = Some(vec![]);
        Ok(effects)
    }

    fn run(
        &self,
        arguments: &Map<String, Value>,
        _cancel: &dyn Cancel,
        _emit: &dyn Emit,
    ) -> Output {
        match crate::tool_util::parsed::<Args>(arguments) {
            Ok(args) => {
                let names = self.search(&args.query, args.limit);
                if names.is_empty() {
                    Output {
                        content: vec![ContentPart::Text {
                            text: NO_MATCH.to_owned(),
                        }],
                        loaded: Some(Vec::new()),
                        ..Output::default()
                    }
                } else {
                    Output {
                        content: vec![ContentPart::Text {
                            text: loaded_text(&names),
                        }],
                        loaded: Some(names),
                        ..Output::default()
                    }
                }
            }
            Err(error) => errored(error),
        }
    }

    fn guidelines(&self) -> Option<String> {
        crate::guidelines::of("tool_search")
    }
}

#[cfg(test)]
#[path = "tool_search_tests.rs"]
mod tests;
