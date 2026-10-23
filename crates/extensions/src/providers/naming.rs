//! Model-name resolution (`docs/model-routing.md`, "Naming a model").

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
    /// every provider that names it, configured or not. A bare id tries
    /// the full typed id first, so a literal id ending in a thinking level
    /// matches before the suffix strips (`docs/model-routing.md`, "Naming
    /// a model": the exact match comes first, since OpenRouter ids contain
    /// colons); only when nothing names the full id does the stripped id
    /// count. Every failure is the crate's `Error`, so the caller keeps
    /// the typed failure across the crate boundary.
    pub fn resolve_named<'a>(
        &'a self,
        naming: &[(String, String)],
        typed: &str,
    ) -> Result<Model<'a>, crate::Error> {
        let (rest, _) = Self::split_thinking(typed);
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
        if typed != rest
            && let Some(prepared) = resolve_literal(self, naming, typed)
        {
            return prepared;
        }
        let mut matches = named(naming, rest);
        match self.resolve(typed) {
            Ok(model) => {
                let reference = model.reference();
                if !matches.contains(&reference) {
                    matches.push(reference);
                }
                if matches.len() > 1 {
                    return Err(ambiguous(rest, &mut matches));
                }
                Ok(model)
            }
            Err(error) => {
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
                if matches.len() > 1 {
                    return Err(ambiguous(rest, &mut matches));
                }
                Err(error)
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
/// ambiguity. `None` when nothing in the registry names it, and the
/// stripped id counts instead.
fn resolve_literal<'a>(
    registry: &'a Providers,
    naming: &[(String, String)],
    typed: &str,
) -> Option<Result<Model<'a>, crate::Error>> {
    let mut matches = named(naming, typed);
    // Check the exact provider/model first: bare resolution can discard an
    // unconfigured literal for suffix matches. Qualified resolution can
    // strip a suffix too, so require the error to name this exact reference.
    if let [reference] = matches.as_slice()
        && let Err(crate::Error::Unconfigured { message }) = registry.resolve(reference)
        && message.starts_with(&format!("The model `{reference}` "))
    {
        return Some(Err(crate::Error::Unconfigured { message }));
    }
    match registry.resolve(typed) {
        // The registry's own exact-first order tries the full id before
        // the suffix strips, so a model whose id is the full text is a
        // full-id match; anything else resolved through the suffix.
        Ok(model) if model.model.id == typed => {
            let reference = model.reference();
            if !matches.contains(&reference) {
                matches.push(reference);
            }
            if matches.len() > 1 {
                return Some(Err(ambiguous(typed, &mut matches)));
            }
            return Some(Ok(model));
        }
        Err(crate::Error::Ambiguous {
            id,
            matches: theirs,
        }) if id == typed => {
            for reference in &theirs {
                if !matches.contains(reference) {
                    matches.push(reference.clone());
                }
            }
            return Some(Err(ambiguous(typed, &mut matches)));
        }
        Ok(_) | Err(_) => {}
    }
    if matches.len() > 1 {
        return Some(Err(ambiguous(typed, &mut matches)));
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
