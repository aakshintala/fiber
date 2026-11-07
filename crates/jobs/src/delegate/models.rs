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

use contract::ErrorCode;
use contract::emit::Emit;
use contract::provider::ToolDefinition;
use contract::shapes::{ContentPart, DeclaredEffects, Effect, Failure};
use contract::tool::{Cancel, Effects, EffectsError, Output, Tool};
use serde_json::{Map, Value, json};

/// Resolves a `delegate_spawn` `model` argument to the `provider/model[:level]`
/// the child is started with, or refuses it with one reason sentence, and
/// carries the short shape every refusal lists: the roles line and, for a
/// model string that resolves to nothing, the closest references. Whoever
/// builds a `Resolve` builds `lookup`, `catalogue`, `roles` and `closest`
/// from one immutable snapshot, so every role the catalogue lists without
/// ` (unavailable)` looks up `Ok` to its listed target, every role marked
/// ` (unavailable)` looks up `Err`, and every listed reference looks up
/// `Ok`. `lookup` captures only owned, cloned data, so a later
/// configuration write cannot change it.
#[derive(Clone)]
pub struct Resolve {
    /// Resolves the argument to `provider/model[:level]`, or refuses it
    /// with one reason sentence, without the catalogue.
    #[allow(
        clippy::type_complexity,
        reason = "the resolver seam: one closure type shared by the session resolver and every test stub"
    )]
    pub lookup: Arc<dyn Fn(&str) -> Result<String, String> + Send + Sync>,
    /// The full text `delegate_models` returns. A refusal never lists it
    /// whole: it carries `roles` and, for a model string that resolves to
    /// nothing, the closest references instead.
    pub catalogue: Arc<str>,
    /// The one-line roles listing every model refusal carries, built with
    /// the resolver from the same snapshot as `lookup` and `catalogue`.
    pub roles: Arc<str>,
    /// The closest references for a model string, nearest first, built
    /// with the resolver from the same snapshot as `lookup`: a missing or
    /// non-string model has nothing to compare, so those refusals never
    /// call it.
    #[allow(
        clippy::type_complexity,
        reason = "the resolver seam: one closure type shared by the session resolver and every test stub"
    )]
    pub closest: Arc<dyn Fn(&str) -> Vec<String> + Send + Sync>,
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
/// "The tools"). A refusal carries the roles line and closest references,
/// never the catalogue; only `delegate_models` appends the quota. The
/// quota is read on every call, so a `delegate_models` call never answers
/// a stale figure.
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
                "properties": {
                    "filter": {
                        "type": "string",
                        "description": "Keep only references containing this string."
                    }
                },
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
            review_when_confined: false,
            hosts: None,
        })
    }

    fn run(
        &self,
        arguments: &Map<String, Value>,
        _cancel: &dyn Cancel,
        _emit: &dyn Emit,
    ) -> Output {
        let filter = match arguments.get("filter") {
            None | Some(Value::Null) => None,
            Some(Value::String(filter)) => Some(filter.clone()),
            Some(_) => {
                return Output {
                    error: Some(Failure {
                        code: ErrorCode::InvalidArguments,
                        message: "`filter` must be a string.".to_owned(),
                        retry_after_ms: None,
                        provider: None,
                    }),
                    content: Vec::new(),
                    ..Output::default()
                };
            }
        };
        let catalogue = match filter {
            None => self.catalogue.to_string(),
            Some(filter) => apply_filter(&self.catalogue, &filter),
        };
        Output {
            content: vec![ContentPart::Text {
                text: format!("{catalogue}\n{}", (self.quota)()),
            }],
            ..Output::default()
        }
    }
}

/// Keeps only the References-section entries whose reference token
/// contains `filter`, case-sensitively: the token is the entry before
/// any ` (…)` annotation, so a filter never matches the annotation.
/// Group headers survive only with at least one kept entry, with their
/// counts rewritten; no kept entry renders `References: none`. The
/// session line and the roles pass through untouched.
fn apply_filter(catalogue: &str, filter: &str) -> String {
    let mut head: Vec<&str> = Vec::new();
    let mut entries: Vec<&str> = Vec::new();
    let mut in_references = false;
    for line in catalogue.split('\n') {
        if !in_references {
            if line == "References:" || line == "References: none" {
                in_references = true;
            } else {
                head.push(line);
            }
        } else {
            entries.push(line);
        }
    }
    if !in_references {
        return catalogue.to_owned();
    }
    let mut rendered: Vec<String> = Vec::new();
    let mut group: Option<String> = None;
    let mut kept: Vec<String> = Vec::new();
    let flush = |rendered: &mut Vec<String>, group: &mut Option<String>, kept: &mut Vec<String>| {
        if let Some(name) = group.take() {
            if !kept.is_empty() {
                rendered.push(format!("{name} ({}):", kept.len()));
                rendered.append(kept);
            }
        } else {
            rendered.append(kept);
        }
    };
    for line in entries {
        if let Some(name) = group_header(line) {
            flush(&mut rendered, &mut group, &mut kept);
            group = Some(name);
        } else if reference_token(line).contains(filter) {
            kept.push(line.to_owned());
        }
    }
    flush(&mut rendered, &mut group, &mut kept);
    let mut text = head.join("\n");
    text.push_str("\nReferences:");
    if rendered.is_empty() {
        text.push_str(" none");
    } else {
        for line in &rendered {
            text.push('\n');
            text.push_str(line);
        }
    }
    text
}

/// The provider name of a `{provider} ({count}):` group header; `None`
/// for a reference entry, which never ends with a colon.
fn group_header(line: &str) -> Option<String> {
    let inner = line.strip_suffix(':')?;
    let (name, count) = inner.rsplit_once(" (")?;
    count
        .strip_suffix(')')
        .filter(|count| count.parse::<usize>().is_ok())?;
    Some(name.to_owned())
}

/// The reference token of a catalogue entry: the entry before any
/// ` (…)` annotation.
fn reference_token(line: &str) -> &str {
    line.split_once(' ').map(|(head, _)| head).unwrap_or(line)
}

#[cfg(test)]
#[path = "models_tests.rs"]
mod tests;
