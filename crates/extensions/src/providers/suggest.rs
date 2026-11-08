//! Closest-reference suggestions for an unknown delegate model
//! (`docs/delegates.md`, "Choosing a model"): the shared edit count the
//! CLI's own closest-model hint ranks with, the gated candidate pool plus
//! ranking both delegate callers use, and the one-line roles helper every
//! model refusal carries.

use super::Providers;

/// Edits between two references, in characters, ranking the closest matches.
pub fn distance(left: &str, right: &str) -> usize {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    let mut prev: Vec<usize> = (0..=right.len()).collect();
    for (i, l) in left.iter().enumerate() {
        let mut current = Vec::with_capacity(right.len() + 1);
        current.push(i + 1);
        for (j, r) in right.iter().enumerate() {
            let cost = usize::from(l != r);
            let above = prev.get(j + 1).map_or(usize::MAX, |v| v.saturating_add(1));
            let beside = prev.get(j).map_or(usize::MAX, |v| v.saturating_add(cost));
            let side = current.get(j).map_or(usize::MAX, |v| v.saturating_add(1));
            current.push(above.min(beside).min(side));
        }
        prev = current;
    }
    prev.get(right.len()).copied().unwrap_or(usize::MAX)
}

/// Up to 3 `fiber:` references nearest the typed text, nearest first with
/// ties in sorted reference order. The pool holds every switched-on
/// provider's models as bare references plus one suffixed variant per
/// thinking level the model takes, so a leveled typo keeps its suffixes
/// verbatim; the typed string compares whole, never stripped. Logged-out
/// providers never contribute, however near their models read.
pub fn closest_refs(providers: &Providers, config: &config::Config, typed: &str) -> Vec<String> {
    let mut candidates: Vec<String> = Vec::new();
    for name in providers.switched_on(config) {
        if let Some(provider) = providers.get(name) {
            for model in &provider.models {
                let reference = format!("fiber:{name}/{}", model.id);
                candidates.push(reference.clone());
                for level in &model.thinking_levels {
                    candidates.push(format!("{reference}:{level}"));
                }
            }
        }
    }
    candidates.sort();
    candidates.sort_by_key(|candidate| distance(typed, candidate));
    candidates.truncate(3);
    candidates
}

/// The one-line roles listing every model refusal carries: all configured
/// role names sorted, or `none` when no role is configured. Roles whose
/// targets resolve to nothing count by name only: they are valid attempts
/// with a reasoned refusal.
pub fn roles_line(config: &config::Config) -> String {
    let mut names: Vec<String> = config
        .merged(None)
        .get("roles")
        .and_then(|roles| roles.as_object())
        .map(|roles| roles.keys().cloned().collect())
        .unwrap_or_default();
    names.sort();
    if names.is_empty() {
        String::from("Roles: none")
    } else {
        format!("Roles: {}", names.join(", "))
    }
}

#[cfg(test)]
#[path = "suggest_tests.rs"]
mod tests;
