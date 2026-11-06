//! Model-name resolution (`docs/model-routing.md`, "Naming a model").

use std::collections::BTreeSet;

use super::{Model, Providers};

#[cfg(test)]
#[path = "naming_tests.rs"]
mod tests;

impl Providers {
    /// Every model of every provider as `(provider, id)` pairs, taken
    /// after `add_lua` and before placeholders are filled, so a model left
    /// out with `model_unconfigured` still counts toward ambiguity: every
    /// `by_name` model plus every `unconfigured` entry. The two never hold
    /// the same id, so the set is the same before or after the fill.
    pub fn naming(&self) -> Vec<(String, String)> {
        let mut naming: Vec<(String, String)> = self
            .names()
            .flat_map(|name| {
                self.get(name)
                    .map(|data| {
                        data.models
                            .iter()
                            .map(|model| (name.to_owned(), model.id.clone()))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            })
            .collect();
        naming.extend(self.unconfigured.iter().flat_map(|(provider, left)| {
            left.iter().map(|(id, _)| (provider.clone(), id.clone()))
        }));
        naming
    }

    /// The typed model against the registry and the naming list: an exact
    /// `provider/model` resolves as at startup, while a bare id counts
    /// only switched-on providers, configured or not. A bare id tries
    /// the full typed id first, so a literal id ending in a thinking level
    /// matches before the suffix strips (`docs/model-routing.md`, "Naming
    /// a model": the exact match comes first, since OpenRouter ids contain
    /// colons); only when no installed provider names the full id does
    /// the stripped id count. The inventory stays installed-wide:
    /// `resolve`, `naming` and `delegate_model` are unchanged, and only
    /// bare-id candidate selection filters on credential presence.
    /// Every failure is the crate's `Error`, so the caller keeps
    /// the typed failure across the crate boundary.
    pub fn resolve_named<'a>(
        &'a self,
        naming: &[(String, String)],
        config: &config::Config,
        typed: &str,
    ) -> Result<Model<'a>, crate::Error> {
        let (rest, thinking) = Self::split_thinking(typed);
        if rest.contains('/') {
            // A scripted model is added to the registry only at start.
            return self.resolve(typed).map_err(|error| {
                if rest.starts_with("scripted/") {
                    crate::Error::ScriptedStartOnly
                } else {
                    error
                }
            });
        }
        // The configuration is the pinned startup configuration, but
        // credential presence is live (`has_credential` never memoizes, so
        // each call rereads credential files and the environment): a login
        // or logout flips the next read with no configuration change.
        // Within this one call every bare-id candidate list below filters
        // on this one set, so neither contributor leaks.
        let on: BTreeSet<String> = self.switched_on(config).map(str::to_owned).collect();
        if typed != rest
            && let Some(prepared) = resolve_literal(self, naming, &on, typed)
        {
            return prepared;
        }
        let matches = named(naming, rest);
        match self.resolve(typed) {
            Ok(model) => {
                let mut counted = switched_only(&matches, &on);
                let reference = model.reference();
                if on.contains(owner(&reference)) && !counted.contains(&reference) {
                    counted.push(reference.clone());
                }
                match counted.as_slice() {
                    // The one installed holder is logged out: name it
                    // instead of resolving.
                    [] => Err(missing_credential(
                        rest,
                        &logged_out_holders(&matches, Some(&reference), self, &on),
                    )),
                    [single] => self.resolve(single).map(|mut made| {
                        made.thinking = thinking;
                        made
                    }),
                    [..] => Err(ambiguous(rest, &mut counted)),
                }
            }
            Err(error) => {
                let mut matches = matches;
                if let crate::Error::Ambiguous {
                    matches: theirs, ..
                } = &error
                {
                    for reference in theirs {
                        if !matches.contains(reference) {
                            matches.push(reference.clone());
                        }
                    }
                }
                let mut counted = switched_only(&matches, &on);
                match counted.as_slice() {
                    [] => {
                        let holders = logged_out_holders(&matches, None, self, &on);
                        if holders.is_empty() {
                            Err(error)
                        } else {
                            Err(missing_credential(rest, &holders))
                        }
                    }
                    [single] => self.resolve(single).map(|mut made| {
                        made.thinking = thinking;
                        made
                    }),
                    [..] => Err(ambiguous(rest, &mut counted)),
                }
            }
        }
    }

    /// A full `fiber:<provider>/<model>[:<level>]` reference as the child
    /// is started with: `Some` when it names an installed model, `None`
    /// for anything else. Without the prefix the reference is refused even
    /// when the plain lookup would take it, and a bare id without a
    /// provider is refused too.
    pub fn delegate_model(&self, reference: &str) -> Option<Model<'_>> {
        let rest = reference.strip_prefix("fiber:")?;
        if !rest.contains('/') {
            return None;
        }
        self.resolve(rest).ok()
    }
}

/// The provider a `provider/model` reference names.
fn owner(reference: &str) -> &str {
    reference
        .split_once('/')
        .map_or(reference, |(provider, _)| provider)
}

/// Installed-wide bare-id candidates whose provider is switched on, in
/// first-seen order: a logged-out holder counts for nothing, and a
/// naming-list entry no installed provider owns never counts.
fn switched_only(matches: &[String], on: &BTreeSet<String>) -> Vec<String> {
    matches
        .iter()
        .filter(|reference| on.contains(owner(reference)))
        .cloned()
        .collect()
}

/// Installed-wide bare-id candidates whose provider is installed but
/// switched off, sorted, each once, with `extra` counted too: every
/// logged-out holder of the id, named. A naming-list entry no installed
/// provider owns is dropped, never counted.
fn logged_out_holders(
    matches: &[String],
    extra: Option<&str>,
    registry: &Providers,
    on: &BTreeSet<String>,
) -> Vec<String> {
    let mut candidates: Vec<&str> = matches.iter().map(String::as_str).collect();
    if let Some(reference) = extra {
        candidates.push(reference);
    }
    let mut holders: Vec<String> = candidates
        .into_iter()
        .filter(|reference| {
            registry.get(owner(reference)).is_some() && !on.contains(owner(reference))
        })
        .map(str::to_owned)
        .collect();
    holders.sort();
    holders.dedup();
    holders
}

/// The missing-credential error for a bare id no switched-on provider
/// has: the first logged-out holder, sorted, with `why` naming every
/// logged-out holder as not logged in, each with its `fiber login`
/// command, in the shape of the session's own missing-credential error.
#[allow(
    clippy::indexing_slicing,
    reason = "every caller passes a non-empty holder list"
)]
fn missing_credential(id: &str, holders: &[String]) -> crate::Error {
    let mut owners: Vec<&str> = holders.iter().map(|reference| owner(reference)).collect();
    owners.sort();
    owners.dedup();
    let provider = owners[0].to_owned();
    let why = if owners.len() == 1 {
        format!("`{}` has `{id}` but is not logged in", owners[0])
    } else {
        let names = owners
            .iter()
            .map(|name| format!("`{name}`"))
            .collect::<Vec<_>>()
            .join(", ");
        let logins = owners
            .iter()
            .map(|name| format!("`fiber login {name}`"))
            .collect::<Vec<_>>()
            .join(", ");
        format!("{names} have `{id}` but are not logged in; run {logins}")
    };
    crate::Error::Config(config::ConfigError::CredentialMissing { provider, why })
}

/// The `provider/model` references the naming list holds for the bare id
/// `text`.
fn named(naming: &[(String, String)], text: &str) -> Vec<String> {
    naming
        .iter()
        .filter(|(_, id)| id == text)
        .map(|(provider, id)| format!("{provider}/{id}"))
        .collect()
}

/// A bare id that is also a literal model id ending in a thinking level:
/// the full typed id against the registry and the naming list, before the
/// suffix strips. `Some` when the full id names anything: the match or its
/// ambiguity. `None` when no installed provider names the full id, and the
/// stripped id counts instead: when installed providers name the full id
/// but none is switched on, their missing credential reports before any
/// suffix strips.
fn resolve_literal<'a>(
    registry: &'a Providers,
    naming: &[(String, String)],
    on: &BTreeSet<String>,
    typed: &str,
) -> Option<Result<Model<'a>, crate::Error>> {
    let matches = named(naming, typed);
    let mut counted = switched_only(&matches, on);
    // Check the exact provider/model first: bare resolution can discard an
    // unconfigured literal for suffix matches. Qualified resolution can
    // strip a suffix too, so require the error to name this exact reference.
    if let [reference] = counted.as_slice()
        && let Err(crate::Error::Unconfigured { message }) = registry.resolve(reference)
        && message.starts_with(&format!("The model `{reference}` "))
    {
        return Some(Err(crate::Error::Unconfigured {
            message: message.clone(),
        }));
    }
    match registry.resolve(typed) {
        // The registry's own exact-first order tries the full id before
        // the suffix strips, so a model whose id is the full text is a
        // full-id match; anything else resolved through the suffix.
        Ok(model) if model.model.id == typed => {
            let reference = model.reference();
            if !on.contains(owner(&reference)) {
                // The one installed holder of the full id is logged out.
                return Some(Err(missing_credential(
                    typed,
                    &logged_out_holders(&matches, Some(&reference), registry, on),
                )));
            }
            if !counted.contains(&reference) {
                counted.push(reference);
            }
            if counted.len() > 1 {
                return Some(Err(ambiguous(typed, &mut counted)));
            }
            return Some(Ok(model));
        }
        Err(crate::Error::Ambiguous {
            id,
            matches: theirs,
        }) if id == typed => {
            let mut matches = matches;
            for reference in &theirs {
                if !matches.contains(reference) {
                    matches.push(reference.clone());
                }
            }
            let mut counted = switched_only(&matches, on);
            match counted.as_slice() {
                [] => {
                    let holders = logged_out_holders(&matches, None, registry, on);
                    if holders.is_empty() {
                        return None;
                    }
                    return Some(Err(missing_credential(typed, &holders)));
                }
                [single] => return Some(registry.resolve(single)),
                [..] => return Some(Err(ambiguous(typed, &mut counted))),
            }
        }
        Ok(_) | Err(_) => {}
    }
    if counted.len() > 1 {
        return Some(Err(ambiguous(typed, &mut counted)));
    }
    if counted.is_empty() {
        let holders = logged_out_holders(&matches, None, registry, on);
        if !holders.is_empty() {
            return Some(Err(missing_credential(typed, &holders)));
        }
    }
    None
}

/// A bare id more than one provider names, in the shape `resolve_named`
/// names it.
fn ambiguous(id: &str, matches: &mut Vec<String>) -> crate::Error {
    matches.sort();
    matches.dedup();
    crate::Error::Ambiguous {
        id: id.to_owned(),
        matches: matches.clone(),
    }
}
