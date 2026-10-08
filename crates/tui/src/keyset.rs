//! The effective bindings: the table's defaults overlaid with the
//! person's `keys` (`docs/tui.md`, "Bindings"). A hand-edited entry
//! replaces its action's defaults, `[]` unbinds it, and an invalid or
//! clashing entry gives one notice and keeps the defaults.

use serde_json::{Map, Value};

use crate::bindings::BINDINGS;
use crate::keys::{Edit, Key, default_event};
use crate::stroke::Stroke;

/// When a stroke arrives: exactly one of five contexts, mirroring
/// `route_key`'s handler order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Context {
    /// The input box has the keyboard.
    Input,
    /// A steering row is selected.
    Steering,
    /// Focus is in the conversation.
    Conversation,
    /// Conversation search is open.
    Search,
    /// An overlay has the keyboard.
    Overlay,
}

/// The contexts an action acts in, as a set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Contexts(u8);

impl Contexts {
    /// The input box.
    pub(crate) const INPUT: Contexts = Contexts(0b00001);
    /// A steering row selected.
    pub(crate) const STEERING: Contexts = Contexts(0b00010);
    /// The conversation focused.
    pub(crate) const CONVERSATION: Contexts = Contexts(0b00100);
    /// Search open.
    pub(crate) const SEARCH: Contexts = Contexts(0b01000);
    /// An overlay open.
    pub(crate) const OVERLAY: Contexts = Contexts(0b10000);
    /// Every context. A literal, not a combination of the others, so no
    /// operator is left for a mutation to swap.
    pub(crate) const ALL: Contexts = Contexts(0b11111);
    /// The input box and steering.
    pub(crate) const INPUT_STEERING: Contexts = Contexts(0b00011);
    /// The input box, steering and the conversation.
    pub(crate) const INPUT_STEERING_CONVERSATION: Contexts = Contexts(0b00111);

    /// Whether `context` is in the set.
    pub(crate) fn contains(self, context: Context) -> bool {
        let bit = match context {
            Context::Input => Contexts::INPUT.0,
            Context::Steering => Contexts::STEERING.0,
            Context::Conversation => Contexts::CONVERSATION.0,
            Context::Search => Contexts::SEARCH.0,
            Context::Overlay => Contexts::OVERLAY.0,
        };
        self.0 & bit != 0
    }

    /// Whether the two sets share a context.
    pub(crate) fn overlaps(self, other: Contexts) -> bool {
        self.0 & other.0 != 0
    }
}

/// What a binding resolves a stroke to, before the person's `keys`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Canon {
    /// A key the app matches.
    Key(Key),
    /// A key that edits the draft.
    Edit(Edit),
    /// An action with no key of its own: `new_session` and `go_home`.
    Action,
    /// Nothing yet: `paste_image` and `model_picker`, until their tickets
    /// give them keys.
    None,
}

/// What a stroke means in a context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Resolved {
    /// A key the app matches.
    Key(Key),
    /// A key that edits the draft.
    Edit(Edit),
    /// An action's id.
    Action(&'static str),
    /// No action: the stroke does nothing.
    Nothing,
}

/// One action's keys: its defaults parsed, its current keys, and whether
/// the person set them. An entry equal to the defaults counts as not set.
struct Row {
    /// The action's default keys, parsed.
    defaults: Vec<Stroke>,
    /// The action's current keys.
    keys: Vec<Stroke>,
    /// Whether the person set a value different from the defaults.
    person: bool,
}

/// The effective bindings, in the table's order.
pub(crate) struct Keyset {
    rows: Vec<Row>,
}

/// The defaults parsed: every default key name parses, so an entry that
/// fails to parse is the person's, never a default's.
fn parsed(names: &[&str]) -> Vec<Stroke> {
    names
        .iter()
        .filter_map(|name| Stroke::parse(name).ok())
        .collect()
}

impl Default for Keyset {
    fn default() -> Self {
        // No person entries: the defaults, with no notices.
        let (keyset, _) = load(&Map::new());
        keyset
    }
}

/// How a variant action's list reads: the count word and the variant
/// names, as the length notice names them.
fn takes(id: &str) -> (&'static str, &'static str) {
    match id {
        "rail_row_n" => ("nines", "1 to 9"),
        "focus_next_prev" | "search_next_prev" => ("twos", "next, prev"),
        "line_start_end" => ("twos", "start, end"),
        "select_steering" => ("twos", "up, down"),
        _ => ("twos", "left, right"),
    }
}

/// A variant's name, as the two-variants notice names it: the variant at
/// `slot`, of the action's variants in order. Rail rows are numbered.
fn variant_name(id: &str, slot: usize) -> String {
    if id == "rail_row_n" {
        return (slot.saturating_add(1)).to_string();
    }
    let names = match id {
        "line_start_end" => &["start", "end"],
        "focus_next_prev" | "search_next_prev" => &["next", "prev"],
        "select_steering" => &["up", "down"],
        _ => &["left", "right"],
    };
    names.get(slot).unwrap_or(&"?").to_string()
}

/// The stroke Ctrl+C: no action may take it.
fn ctrl_c() -> Stroke {
    Stroke {
        code: crate::stroke::Code::Char('c'),
        mods: crate::stroke::Mods::CTRL,
    }
}

/// Validates one person's entry, returning its keys when they replace the
/// defaults. `None` keeps the defaults, with one notice pushed.
fn entry(
    id: &'static str,
    value: &Value,
    defaults: &[Stroke],
    variants: usize,
    notices: &mut Vec<String>,
) -> Option<Vec<Stroke>> {
    // `clear_then_quit` cannot be rebound at all.
    if id == "clear_then_quit" {
        notices.push(format!(
            "keys.{id}: Ctrl+C always clears, then quits, and cannot be rebound; it is ignored."
        ));
        return None;
    }
    let texts: Vec<&str> = match value {
        Value::String(text) => vec![text],
        Value::Array(items) if items.iter().all(Value::is_string) => {
            items.iter().filter_map(Value::as_str).collect()
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => {
            notices.push(format!(
                "keys.{id}: {value} is not a key or a list of keys; {id} keeps its default."
            ));
            return None;
        }
    };
    let mut keys = Vec::with_capacity(texts.len());
    for text in texts {
        match Stroke::parse(text) {
            Ok(stroke) => keys.push(stroke),
            Err(_) => {
                notices.push(format!(
                    "keys.{id}: {text:?} is not a key name; {id} keeps its default."
                ));
                return None;
            }
        }
    }
    // A list holds keys in groups, one key per variant in order; `[]`
    // unbinds.
    if keys.len() % variants != 0 {
        let (count, names) = takes(id);
        notices.push(format!(
            "keys.{id}: takes keys in {count} ({names}); {id} keeps its default."
        ));
        return None;
    }
    // A key in two variants makes the entry invalid; a group identical
    // to an earlier group is dropped whole.
    for (at, first) in keys.iter().enumerate() {
        for (later, second) in keys.iter().enumerate().skip(at.saturating_add(1)) {
            if first == second && at % variants != later % variants {
                let held = variant_name(id, at % variants);
                let also = variant_name(id, later % variants);
                notices.push(format!(
                    "keys.{id}: {} is both {held} and {also}; {id} keeps its default.",
                    first.name()
                ));
                return None;
            }
        }
    }
    let mut kept: Vec<Stroke> = Vec::with_capacity(keys.len());
    for group in keys.chunks(variants.max(1)) {
        if !kept.chunks(variants.max(1)).any(|seen| seen == group) {
            kept.extend_from_slice(group);
        }
    }
    // Ctrl+C always clears, then quits.
    if kept.contains(&ctrl_c()) {
        notices.push(format!(
            "keys.{id}: Ctrl+C always clears, then quits; {id} keeps its default."
        ));
        return None;
    }
    // An entry equal to the defaults counts as not set.
    if kept == defaults {
        return None;
    }
    Some(kept)
}

/// The first key both actions bind, if any: two actions clash whenever
/// they share a key in overlapping contexts, whatever slot each key sits
/// at, since the earlier row resolves the stroke first and the other's
/// key goes dead there.
fn shared_key(lower_keys: &[Stroke], higher_keys: &[Stroke]) -> Option<Stroke> {
    lower_keys
        .iter()
        .find(|first| higher_keys.contains(first))
        .copied()
}

/// Loads the keyset from the person's `keys`, with its notices: first
/// every invalid entry, each keeping its defaults, then the clash passes,
/// each reverting the person-set sides to their defaults until one pass
/// finds no clash. Every keyset returned has no clash.
pub(crate) fn load(user: &Map<String, Value>) -> (Keyset, Vec<String>) {
    let mut notices: Vec<String> = Vec::new();
    for name in user.keys() {
        if !BINDINGS.iter().any(|binding| binding.id == name) {
            notices.push(format!(
                "keys.{name}: Fiber has no action {name}; it is ignored."
            ));
        }
    }
    let mut rows: Vec<Row> = Vec::with_capacity(BINDINGS.len());
    for binding in BINDINGS {
        let defaults = parsed(binding.defaults);
        let keys = match user.get(binding.id) {
            None => defaults.clone(),
            Some(value) => {
                let variants = binding.events.len().max(1);
                match entry(binding.id, value, &defaults, variants, &mut notices) {
                    Some(kept) => kept,
                    None => defaults.clone(),
                }
            }
        };
        let person = keys != defaults;
        rows.push(Row {
            defaults,
            keys,
            person,
        });
    }
    // Each pass reverts at least one person-set entry, and a reverted
    // entry is never person-set again, so the passes are at most the
    // number of person-set entries plus one.
    let bound = rows
        .iter()
        .filter(|row| row.person)
        .count()
        .saturating_add(1);
    for _ in 0..bound {
        let mut pairs: Vec<(usize, usize, Stroke)> = Vec::new();
        for lower in 0..BINDINGS.len() {
            for higher in lower.saturating_add(1)..BINDINGS.len() {
                let (Some(first), Some(second)) = (BINDINGS.get(lower), BINDINGS.get(higher))
                else {
                    continue;
                };
                if !first.contexts.overlaps(second.contexts) {
                    continue;
                }
                let (Some(near), Some(far)) = (rows.get(lower), rows.get(higher)) else {
                    continue;
                };
                if !near.person && !far.person {
                    continue;
                }
                if let Some(stroke) = shared_key(&near.keys, &far.keys) {
                    pairs.push((lower, higher, stroke));
                }
            }
        }
        if pairs.is_empty() {
            break;
        }
        let mut done: Vec<(usize, usize, String)> = Vec::with_capacity(pairs.len());
        for (lower, higher, stroke) in pairs {
            let (Some(first), Some(second)) = (BINDINGS.get(lower), BINDINGS.get(higher)) else {
                continue;
            };
            let (Some(near), Some(far)) = (rows.get(lower), rows.get(higher)) else {
                continue;
            };
            let label = stroke.label();
            if near.person && far.person {
                let (one, other) = if first.id < second.id {
                    (first, second)
                } else {
                    (second, first)
                };
                done.push((
                    lower,
                    higher,
                    format!(
                        "keys.{} and keys.{} both bind {label}; both keep their defaults.",
                        one.id, other.id
                    ),
                ));
            } else if near.person || far.person {
                let (set, kept) = if near.person {
                    (first, second)
                } else {
                    (second, first)
                };
                done.push((
                    lower,
                    higher,
                    format!(
                        "keys.{}: {label} is also {} ({}); {} keeps its default.",
                        set.id, kept.description, kept.id, set.id
                    ),
                ));
            }
        }
        for (lower, higher, notice) in done {
            notices.push(notice);
            for at in [lower, higher] {
                if let Some(row) = rows.get_mut(at)
                    && row.person
                {
                    row.keys = row.defaults.clone();
                    row.person = false;
                }
            }
        }
    }
    (Keyset { rows }, notices)
}

/// Maps `default_event`'s answer onto what a stroke means.
fn resolved(event: Option<crate::keys::Event>) -> Resolved {
    match event {
        Some(crate::keys::Event::Key(key)) => Resolved::Key(key),
        Some(crate::keys::Event::Edit(edit)) => Resolved::Edit(edit),
        Some(crate::keys::Event::Stroke(_))
        | Some(crate::keys::Event::Mouse(_))
        | Some(crate::keys::Event::Reply(_))
        | None => Resolved::Nothing,
    }
}

impl Keyset {
    /// What `stroke` means in `context`: (a) the action acting there whose
    /// keys hold it, through its variant's canonical event, except at its
    /// own default's place, which keeps the stroke's own event; (b) else
    /// nothing when an action acting there moved off that default; (c)
    /// else the stroke's own event. A person's key wins over a default
    /// holding the same stroke at another variant slot.
    pub(crate) fn resolve(&self, stroke: &Stroke, context: Context) -> Resolved {
        let mut shadowed: Option<Resolved> = None;
        for (binding, row) in BINDINGS.iter().zip(self.rows.iter()) {
            if !binding.contexts.contains(context) {
                continue;
            }
            let Some(at) = row.keys.iter().position(|key| key == stroke) else {
                continue;
            };
            let variants = binding.events.len().max(1);
            let canonical = binding.events.get(at % variants);
            if matches!(canonical, Some(Canon::Action)) {
                return Resolved::Action(binding.id);
            }
            let home = row
                .defaults
                .iter()
                .enumerate()
                .any(|(place, default)| default == stroke && place % variants == at % variants);
            let answer = if !row.person || home {
                resolved(default_event(stroke))
            } else {
                match canonical {
                    Some(Canon::Key(key)) => Resolved::Key(key.clone()),
                    Some(Canon::Edit(edit)) => Resolved::Edit(edit.clone()),
                    Some(Canon::Action) | Some(Canon::None) | None => Resolved::Nothing,
                }
            };
            if row.person {
                return answer;
            }
            if shadowed.is_none() {
                shadowed = Some(answer);
            }
        }
        if let Some(answer) = shadowed {
            return answer;
        }
        let moved = BINDINGS.iter().zip(self.rows.iter()).any(|(binding, row)| {
            binding.contexts.contains(context)
                && row.defaults.contains(stroke)
                && !row.keys.contains(stroke)
        });
        if moved {
            return Resolved::Nothing;
        }
        resolved(default_event(stroke))
    }

    /// How the key map shows `binding`'s row: the doc's Key cell while the
    /// person did not set it, `unbound` for `[]`, else its keys' labels,
    /// the keys in a group joined by a space and the groups by ", ".
    pub(crate) fn shown(&self, binding: &crate::bindings::Binding) -> String {
        let row = BINDINGS
            .iter()
            .zip(self.rows.iter())
            .find(|(known, _)| known.id == binding.id)
            .map(|(_, row)| row);
        let Some(row) = row else {
            return binding.keys.to_owned();
        };
        if !row.person {
            return binding.keys.to_owned();
        }
        if row.keys.is_empty() {
            return "unbound".to_owned();
        }
        let variants = binding.events.len().max(1);
        row.keys
            .chunks(variants)
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
}

/// The person's `keys` (`docs/configuration.md`, "Keys"): each action's id
/// to a key name or a list of key names, `[]` leaving the action unbound.
#[derive(Debug, Default)]
pub struct KeysSetup {
    /// The merged `keys` object, as written.
    pub user: Map<String, Value>,
}

#[cfg(test)]
#[path = "keyset_tests.rs"]
mod tests;
