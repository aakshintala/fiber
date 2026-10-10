//! `host.ask`'s question and answer checks (`docs/extensions.md`,
//! "Commands and screens"): reading a `kind` with its `spec` into an
//! [`Interaction`], and the registry of open asks on [`Shared`]. A reply is
//! fitted by [`Interaction::fit`], the one rule every asker applies.

use std::collections::HashSet;
use std::sync::Arc;

use contract::events::{
    Answer, Interaction, InteractionRequested, InteractionResolved, ResolvedBy,
};
use contract::inbox::{Ack, Delivery, Rejection};
use contract::shapes::True;
use contract::{ErrorCode, RequestId};

use super::hub::{Hub, Shared};
use crate::host;

/// Reads `kind` with its `spec` table, as JSON, into the [`Interaction`] a
/// `host.ask` raises. A bad `kind` or `spec` is an error in the calling
/// code: the string names the offending key or label, so `pcall` catches it
/// and nothing is registered.
pub(crate) fn interaction(kind: &str, spec: &serde_json::Value) -> Result<Interaction, String> {
    let fail = |why: &str| format!("host.ask: {why}");
    let spec = spec
        .as_object()
        .ok_or_else(|| fail("spec must be a table"))?;
    // The contract reader ignores keys it does not know, so the allowed
    // keys are checked first, one set per kind.
    let allowed: &[&str] = match kind {
        "confirm" | "text_input" => &["prompt"],
        "select" | "multi_select" => &["prompt", "options"],
        "form" => &["fields"],
        _ => {
            return Err(fail(&format!(
                "unknown kind {kind:?}; one of confirm, select, multi_select, text_input, form"
            )));
        }
    };
    for key in spec.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(fail(&format!("{kind} takes no key {key:?}")));
        }
    }
    if matches!(kind, "select" | "multi_select") {
        for (index, option) in options_of(spec)?.iter().enumerate() {
            check_option(option, index)?;
        }
    }
    if kind == "form" {
        for (index, field) in fields_of(spec)?.iter().enumerate() {
            check_field(field, index)?;
        }
    }
    if !matches!(kind, "form") {
        match spec.get("prompt") {
            // a_missing_prompt names the key.
            None => return Err(fail(&format!("{kind} needs \"prompt\""))),
            // a_non_string_prompt names the key.
            Some(prompt) if !prompt.is_string() => {
                return Err(fail("`prompt` must be a string"));
            }
            Some(_) => {}
        }
    }
    // The reader checks the values' shapes and that every required key is
    // there; the key sets above already passed.
    let mut line = serde_json::Map::from_iter([
        (
            "request_id".to_owned(),
            serde_json::Value::String("r_check".to_owned()),
        ),
        (
            "kind".to_owned(),
            serde_json::Value::String(kind.to_owned()),
        ),
    ]);
    for (key, value) in spec {
        line.insert(key.clone(), value.clone());
    }
    let requested: InteractionRequested = serde_json::from_value(serde_json::Value::Object(line))
        .map_err(|e| fail(&format!("{kind} {e}")))?;
    match &requested.interaction {
        Interaction::Select { options, .. } | Interaction::MultiSelect { options, .. } => {
            if options.is_empty() {
                return Err(fail(&format!("{kind} needs at least one option")));
            }
            check_labels(&option_labels(options), kind)?;
        }
        Interaction::Form { fields } => {
            if fields.is_empty() {
                return Err(fail("form needs at least one field"));
            }
            for field in fields {
                check_labels(&option_labels(&field.options), kind)?;
            }
        }
        Interaction::Confirm { .. } | Interaction::TextInput { .. } => {}
    }
    Ok(requested.interaction)
}

/// The `options` of a `select` or `multi_select` spec, as JSON values.
fn options_of(
    spec: &serde_json::Map<String, serde_json::Value>,
) -> Result<&Vec<serde_json::Value>, String> {
    match spec.get("options") {
        Some(serde_json::Value::Array(options)) => Ok(options),
        _ => Err("host.ask: select takes `options`, a list of tables".to_owned()),
    }
}

/// The `fields` of a `form` spec, as JSON values.
fn fields_of(
    spec: &serde_json::Map<String, serde_json::Value>,
) -> Result<&Vec<serde_json::Value>, String> {
    match spec.get("fields") {
        Some(serde_json::Value::Array(fields)) => Ok(fields),
        _ => Err("host.ask: form takes `fields`, a list of tables".to_owned()),
    }
}

/// Checks one option's keys: only `label` and `description`.
fn check_option(option: &serde_json::Value, index: usize) -> Result<(), String> {
    let fail = |why: &str| format!("host.ask: option {} {why}", index + 1);
    let option = option.as_object().ok_or_else(|| fail("must be a table"))?;
    for key in option.keys() {
        if key != "label" && key != "description" {
            return Err(fail(&format!("takes no key {key:?}")));
        }
    }
    Ok(())
}

/// Checks one field's keys: only `header`, `question`, `options` and
/// `multiSelect`, in that casing.
fn check_field(field: &serde_json::Value, index: usize) -> Result<(), String> {
    let fail = |why: &str| format!("host.ask: field {} {why}", index + 1);
    let field = field.as_object().ok_or_else(|| fail("must be a table"))?;
    for key in field.keys() {
        if !["header", "question", "options", "multiSelect"].contains(&key.as_str()) {
            return Err(fail(&format!("takes no key {key:?}")));
        }
    }
    if let Some(options) = field.get("options") {
        let options = options
            .as_array()
            .ok_or_else(|| fail("`options` must be a list of tables"))?;
        for (at, option) in options.iter().enumerate() {
            let option = option
                .as_object()
                .ok_or_else(|| fail(&format!("option {} must be a table", at + 1)))?;
            for key in option.keys() {
                if key != "label" && key != "description" {
                    return Err(fail(&format!("option {} takes no key {key:?}", at + 1)));
                }
            }
        }
    }
    Ok(())
}

/// The labels of `options`, as strings when they are.
fn option_labels(options: &[contract::shapes::Choice]) -> Vec<&str> {
    options.iter().map(|option| option.label.as_str()).collect()
}

/// Rejects two options with the same label: an answer names options by
/// label. The error names the label.
fn check_labels(labels: &[&str], kind: &str) -> Result<(), String> {
    let mut seen = HashSet::new();
    for label in labels {
        if !seen.insert(label) {
            return Err(format!(
                "host.ask: {kind} has two options labelled {label:?}"
            ));
        }
    }
    Ok(())
}

/// An open `host.ask`: what was asked and whose parked call waits for the
/// answer. Plain data: leaving the registry neither sends nor answers.
pub(super) struct PendingAsk {
    /// What the `reply` must fit.
    pub(super) interaction: Interaction,
    /// The parked call the answer resumes.
    pub(super) call: u64,
}

/// A `reply`'s answer does not fit the pending request.
const UNFIT: &str = "That answer does not fit the pending request.";

impl Shared {
    /// Routes an extension delivery to the loop's inbox, or buffers it when
    /// no inbox arrived yet. Returns what could not be sent: after `seal`
    /// or the drop, or the inbox's `SendError` on a disconnected receiver.
    /// The caller drops it only after the hub lock is released: a dropped
    /// `Resolved` answers its driver, which may re-enter this hub.
    pub(super) fn route(&mut self, delivery: Delivery) -> Option<Delivery> {
        if self.disposed || self.sealed {
            return Some(delivery);
        }
        match self.inbox.clone() {
            Some(inbox) => match inbox.send(delivery) {
                Ok(()) => None,
                Err(failed) => Some(failed.0),
            },
            None => {
                self.buffer.push(delivery);
                None
            }
        }
    }

    /// Registers `requested` for the parked call `call`, then routes its
    /// `Interaction`, both under the one hub lock the caller holds.
    /// Returns what could not be sent, for the caller to drop after the
    /// lock is released.
    pub(super) fn raise(&mut self, call: u64, requested: InteractionRequested) -> Option<Delivery> {
        self.asks.insert(
            requested.request_id.clone(),
            PendingAsk {
                interaction: requested.interaction.clone(),
                call,
            },
        );
        self.route(Delivery::Interaction(requested))
    }

    /// Removes `request` when held and routes its decline by `fiber`.
    /// Whoever removes first routes the one `Resolved`: a loser finds
    /// nothing held and routes nothing. Returns what could not be sent,
    /// for the caller to drop after the lock is released.
    pub(super) fn decline(&mut self, request: &RequestId) -> Option<Delivery> {
        // decline_after_a_reply_routes_nothing: first remover wins.
        self.asks.remove(request)?;
        let resolved = InteractionResolved {
            request_id: request.clone(),
            by: ResolvedBy::Fiber,
            answer: Answer::Declined { declined: True },
        };
        self.route(Delivery::Resolved(resolved, noop()))
    }
}

/// A new ask's id: `r_` plus 16 hex digits, as the loop's `mint` does.
pub(super) fn mint() -> RequestId {
    use std::collections::hash_map::RandomState;
    use std::hash::BuildHasher;
    RequestId(format!("r_{:016x}", RandomState::new().hash_one(())))
}

/// An ack that answers nothing: a `fiber` decline resumes no call.
fn noop() -> Ack {
    Ack(Box::new(|_| {}))
}

impl Hub {
    /// Answers a driver's `reply` from the ask it names. A reply for an id
    /// no extension holds, or once sealed or disposed, is handed back for
    /// the loop, which rejects it `stale_request`. A fitting answer removes
    /// the ask and routes one `Resolved` by `person` with a composite ack:
    /// when the loop calls it, the driver's `reply` is answered
    /// `command_accepted`, then the parked call resumes with the answer, so
    /// the callback resumes only after the line is in the log. An unfit
    /// answer keeps the ask held and rejects `invalid_arguments` after the
    /// hub lock is released.
    pub(crate) fn answer(
        self: &Arc<Self>,
        reply: contract::commands::Reply,
        ack: Ack,
    ) -> Option<(contract::commands::Reply, Ack)> {
        let mut shared = self.lock();
        if shared.sealed || shared.disposed {
            // answer_after_seal_hands_back: the loop has gone; the door's
            // dropped ack answers `closing`.
            return Some((reply, ack));
        }
        let Some(held) = shared.asks.remove(&reply.request_id) else {
            // answer_for_an_id_not_held_hands_back.
            return Some((reply, ack));
        };
        let Some(answer) = held.interaction.fit(&reply.answer) else {
            shared.asks.insert(reply.request_id.clone(), held);
            drop(shared);
            (ack.0)(Err(Rejection {
                code: ErrorCode::InvalidArguments,
                message: UNFIT.to_owned(),
            }));
            return None;
        };
        let resolved = InteractionResolved {
            request_id: reply.request_id.clone(),
            by: ResolvedBy::Person,
            answer: answer.clone(),
        };
        let hub = Arc::clone(self);
        let composite = Ack(Box::new(move |_| {
            (ack.0)(Ok(None));
            hub.deliver(held.call, host::Reply::Ask(answer));
        }));
        let unsent = shared.route(Delivery::Resolved(resolved, composite));
        drop(shared);
        drop(unsent);
        None
    }
}

#[cfg(test)]
#[path = "asks_tests.rs"]
mod tests;
