//! Extension-state folds over a session log and its fork boundaries.

use std::collections::BTreeMap;

use contract::Envelope;
use contract::events::{Event, OnFork};
use serde_json::Value;

/// The seed handed to extensions before their first call.
pub use contract::hook::StateSeed;

/// Values and their last set's fork rules. An unset retains its rule.
pub type Ruled = BTreeMap<String, BTreeMap<String, (Option<Value>, OnFork)>>;

/// Whether to resume the whole log or inherit through a fork point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fold {
    /// The latest surviving value for each key.
    Resume,
    /// Each key's last rule at this inclusive sequence number.
    At {
        /// The last parent sequence number the child inherits.
        point: u64,
    },
}

/// Folds a single log, without inherited state.
pub fn fold<'a>(lines: impl IntoIterator<Item = &'a Envelope>, mode: Fold) -> StateSeed {
    seed(fold_from(Ruled::new(), lines, mode))
}

/// Folds a segment over inherited state, retaining rules through unsets.
pub fn fold_from<'a>(
    mut carry: Ruled,
    lines: impl IntoIterator<Item = &'a Envelope>,
    mode: Fold,
) -> Ruled {
    match mode {
        Fold::Resume => {
            for line in lines {
                let Ok(Some(event)) = Event::from_envelope(line) else {
                    continue;
                };
                if let Event::ExtensionStateSet(set) = event {
                    carry
                        .entry(set.extension)
                        .or_default()
                        .insert(set.key, (Some(set.value), set.on_fork));
                } else if let Event::ExtensionStateUnset(unset) = event
                    && let Some((value, _)) = carry
                        .get_mut(&unset.extension)
                        .and_then(|keys| keys.get_mut(&unset.key))
                {
                    *value = None;
                }
            }
            carry
        }
        Fold::At { point } => {
            let lines: Vec<_> = lines.into_iter().collect();
            fold_boundary(carry, lines.iter().copied(), lines.iter().copied(), point)
        }
    }
}

/// Applies the rules at a parent's point, reading `latest` from its full log.
/// `point_lines` may be bounded by the log reader; later lines are excluded.
pub fn fold_boundary<'a>(
    carry: Ruled,
    point_lines: impl IntoIterator<Item = &'a Envelope>,
    full_lines: impl IntoIterator<Item = &'a Envelope>,
    point: u64,
) -> Ruled {
    let at = fold_from(
        carry.clone(),
        point_lines
            .into_iter()
            .filter(|line| line.seq.is_some_and(|seq| seq.0 <= point)),
        Fold::Resume,
    );
    let end = fold_from(carry, full_lines, Fold::Resume);
    at.into_iter()
        .filter_map(|(extension, keys)| {
            let keys: BTreeMap<_, _> = keys
                .into_iter()
                .filter_map(|(key, (value, rule))| {
                    let value = match rule {
                        OnFork::AtPoint => value,
                        OnFork::Latest => end
                            .get(&extension)
                            .and_then(|keys| keys.get(&key))
                            .and_then(|(value, _)| value.clone()),
                        OnFork::Fresh => return None,
                    };
                    Some((key, (value, rule)))
                })
                .collect();
            (!keys.is_empty()).then_some((extension, keys))
        })
        .collect()
}

/// Removes value-less keys only when producing an extension's starting view.
pub fn seed(ruled: Ruled) -> StateSeed {
    ruled
        .into_iter()
        .filter_map(|(extension, keys)| {
            let keys: BTreeMap<_, _> = keys
                .into_iter()
                .filter_map(|(key, (value, rule))| value.map(|value| (key, (value, rule))))
                .collect();
            (!keys.is_empty()).then_some((extension, keys))
        })
        .collect()
}

#[cfg(test)]
#[path = "state_fold_tests.rs"]
mod tests;
