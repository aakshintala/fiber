//! Changing the effective bindings from the `/keys` screen (`docs/tui.md`,
//! "Bindings"): one transaction per capture, swap, reset or unbind, and the
//! edits one transaction saves.

use crate::bindings::{BINDINGS, Binding};
use crate::configure::KeyEdit;
use crate::stroke::Stroke;

use super::{Keyset, Row, ctrl_c, variant_name};

/// Why [`Keyset::set`] refused new keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refused {
    /// `clear_then_quit`, any Ctrl+C, or an unknown id: never rebound,
    /// because the second Ctrl+C always quits (`docs/tui.md`, "Input and
    /// focus").
    Fixed,
    /// One key at two places in the new list.
    Repeat(Stroke),
    /// Another action holding one of the new keys in a shared context.
    Clash {
        /// The first such action in table order.
        other: &'static str,
        /// The new key it holds.
        stroke: Stroke,
    },
}

/// The first key the list holds twice, if any.
fn repeated(keys: &[Stroke]) -> Option<Stroke> {
    for (at, first) in keys.iter().enumerate() {
        if keys
            .iter()
            .skip(at.saturating_add(1))
            .any(|second| second == first)
        {
            return Some(*first);
        }
    }
    None
}

/// The capturing action's current first key for the variant the captured
/// key at `got` takes: the key at the first slot with that variant index.
/// `None` when the action holds no key there, as when it is unbound.
fn giveback(rows: &[Row], at: usize, variants: usize, got: usize) -> Option<Stroke> {
    let variants = variants.max(1);
    rows.get(at)?
        .keys
        .iter()
        .enumerate()
        .find_map(|(place, key)| (place % variants == got % variants).then_some(*key))
}

/// Whether `back` may go to the other action in the captured key's place:
/// the capture holds none of it, the other action holds none of it, and no
/// third action sharing a context with the other one holds it, so a
/// give-back never starts a clash of its own.
fn giveable(
    rows: &[Row],
    at: usize,
    other_at: usize,
    other: &Binding,
    keys: &[Stroke],
    back: Stroke,
) -> bool {
    !keys.contains(&back)
        && rows
            .get(other_at)
            .is_some_and(|row| !row.keys.contains(&back))
        && BINDINGS
            .iter()
            .zip(rows.iter())
            .enumerate()
            .all(|(third_at, (third, row))| {
                third_at == at
                    || third_at == other_at
                    || !third.contexts.overlaps(other.contexts)
                    || !row.keys.contains(&back)
            })
}

/// One swap pass over the rows being built: the capturing action and
/// its captured keys, steady while each partner in turn loses them.
struct Trade<'a> {
    /// The rows taking the swap.
    rows: &'a mut [Row],
    /// The capturing action's table index.
    at: usize,
    /// The capturing action's variants.
    acted_variants: usize,
    /// The captured keys, in order.
    keys: &'a [Stroke],
}

/// Swaps one captured key the other action holds at `place`: the give-back
/// takes its place when one may go there, else the action loses the whole
/// group holding it.
fn swap_key(trade: &mut Trade<'_>, other_at: usize, other: &Binding, got: usize, key: Stroke) {
    let place = trade
        .rows
        .get(other_at)
        .and_then(|row| row.keys.iter().position(|held| *held == key));
    let Some(place) = place else {
        return;
    };
    let back = giveback(trade.rows, trade.at, trade.acted_variants, got)
        .filter(|back| giveable(trade.rows, trade.at, other_at, other, trade.keys, *back));
    if let Some(back) = back {
        if let Some(row) = trade.rows.get_mut(other_at)
            && let Some(slot) = row.keys.get_mut(place)
        {
            *slot = back;
        }
        return;
    }
    let variants = other.events.len().max(1);
    if let Some(row) = trade.rows.get_mut(other_at) {
        let start = place / variants * variants;
        let end = start.saturating_add(variants).min(row.keys.len());
        row.keys.drain(start..end);
    }
}

impl Keyset {
    /// The row for `id`: its table index and its keys.
    fn row_of(&self, id: &str) -> Option<(usize, &Row)> {
        BINDINGS
            .iter()
            .zip(self.rows.iter())
            .enumerate()
            .find_map(|(at, (binding, row))| (binding.id == id).then_some((at, row)))
    }

    /// The action's current keys; empty for an unknown id.
    pub(crate) fn current(&self, id: &str) -> &[Stroke] {
        self.row_of(id)
            .map_or_default(|(_, row)| row.keys.as_slice())
    }

    /// The action's default keys; empty for an unknown id.
    pub(crate) fn defaults_of(&self, id: &str) -> &[Stroke] {
        self.row_of(id)
            .map_or_default(|(_, row)| row.defaults.as_slice())
    }

    /// The current keys' labels, the keys in a group joined by a space
    /// and the groups by ", "; `unbound` for none.
    pub(crate) fn labels(&self, id: &str) -> String {
        let keys = self.current(id);
        if keys.is_empty() {
            return "unbound".to_owned();
        }
        let variants = BINDINGS
            .iter()
            .find(|binding| binding.id == id)
            .map_or(1, |binding| binding.events.len().max(1));
        keys.chunks(variants)
            .map(|group| {
                group
                    .iter()
                    .map(Stroke::label)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The variant's name for a variant action (`left`, `next`, `3`);
    /// `None` for a one-variant action.
    pub(crate) fn variant(id: &str, slot: usize) -> Option<String> {
        let variants = BINDINGS
            .iter()
            .find(|binding| binding.id == id)?
            .events
            .len();
        (variants > 1).then(|| variant_name(id, slot))
    }

    /// Binds `id` to `keys`, swapping with `swap_with` first: for each of
    /// those actions, in order, every key of `keys` it still holds is
    /// swapped away, then `id` takes `keys`. One transaction: either the
    /// whole swap lands or nothing does, so no half-swapped keyset is ever
    /// returned or stored. An action is person-set exactly when its keys
    /// differ from its defaults.
    pub(crate) fn set(
        &self,
        id: &str,
        keys: Vec<Stroke>,
        swap_with: &[&str],
    ) -> Result<Keyset, Refused> {
        let Some((at, _)) = self.row_of(id) else {
            return Err(Refused::Fixed);
        };
        if id == "clear_then_quit" || keys.iter().any(|key| *key == ctrl_c()) {
            return Err(Refused::Fixed);
        }
        if let Some(repeat) = repeated(&keys) {
            return Err(Refused::Repeat(repeat));
        }
        let Some(acted) = BINDINGS.get(at) else {
            return Err(Refused::Fixed);
        };
        let acted_variants = acted.events.len().max(1);
        let mut rows: Vec<Row> = self.rows.to_vec();
        let mut trade = Trade {
            rows: rows.as_mut_slice(),
            at,
            acted_variants,
            keys: &keys,
        };
        for other in swap_with {
            if *other == id {
                continue;
            }
            let Some((other_at, _)) = self.row_of(other) else {
                continue;
            };
            let Some(held) = BINDINGS.get(other_at) else {
                continue;
            };
            for (got, key) in keys.iter().enumerate() {
                swap_key(&mut trade, other_at, held, got, *key);
            }
        }
        let mut rows = trade.rows.to_vec();
        if let Some(row) = rows.get_mut(at) {
            row.keys = keys.clone();
        }
        for key in &keys {
            for (other_at, other) in BINDINGS.iter().enumerate() {
                if other_at == at || !other.contexts.overlaps(acted.contexts) {
                    continue;
                }
                let clash = rows.get(other_at).is_some_and(|row| row.keys.contains(key));
                if clash {
                    return Err(Refused::Clash {
                        other: other.id,
                        stroke: *key,
                    });
                }
            }
        }
        for row in rows.iter_mut() {
            row.person = row.keys != row.defaults;
        }
        Ok(Keyset { rows })
    }

    /// One [`KeyEdit`] per action whose keys differ from `before`'s, in
    /// table order: `None` when the keys equal the defaults, else the
    /// written names.
    pub(crate) fn edits(&self, before: &Keyset) -> Vec<KeyEdit> {
        BINDINGS
            .iter()
            .zip(self.rows.iter())
            .zip(before.rows.iter())
            .filter_map(|((binding, row), old)| {
                if row.keys == old.keys {
                    return None;
                }
                if row.keys == row.defaults {
                    return Some(KeyEdit {
                        id: binding.id.to_owned(),
                        keys: None,
                    });
                }
                Some(KeyEdit {
                    id: binding.id.to_owned(),
                    keys: Some(row.keys.iter().map(Stroke::name).collect()),
                })
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "edit_tests.rs"]
mod tests;
