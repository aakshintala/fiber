//! What a session's `command_accepted` and `command_rejected` lines settle:
//! the kept command each one answers, and the replayed acknowledgements the
//! relay replaces with an opening boundary at full.

use contract::SessionId;
use serde::Deserialize;
use serde::de::{Deserializer, MapAccess, Visitor};
use serde_json::{Map, Value};
use std::fmt;

use super::{Kept, KeptState, Replayed, valid_session_id};
use crate::connection::{Hub, lock};

/// Whether `kept` still holds `id`: an acknowledgement for one it does
/// not was passed on, and is answered where it went.
pub(super) fn kept_has(kept: &Kept, id: &str) -> bool {
    lock(kept).iter().any(|(kept, _, _)| kept == id)
}

/// What settling an acknowledged command decides: either the command is
/// routed again, an accepted `rewind` or `change_workspace` starts the
/// session it names, or an accepted `subscribe` becomes the kept
/// subscription.
pub(super) enum Settled {
    Reroute(String, Map<String, Value>),
    Rewound(SessionId),
    Subscribed(Map<String, Value>),
}

/// Settles a kept command `line` acknowledges: it is no longer kept. A
/// `closing` rejection of it returns it, to be routed again, once this
/// thread has seen the exited window, whether in this answer
/// (`crate::resume::exited`) or an earlier one (`exiting`). An accepted
/// `subscribe` returns its line, to become the kept subscription.
pub(super) fn settle(
    ack: Option<&Ack>,
    kept: &Kept,
    hub: &Hub,
    session: &str,
    exiting: bool,
) -> Option<Settled> {
    let ack = ack?;
    let (id, command, state) = {
        let mut kept = lock(kept);
        if kept.is_empty() {
            return None;
        }
        let at = kept.iter().position(|(kept, _, _)| *kept == ack.id)?;
        let (_, command, state) = kept.remove(at);
        (ack.id.clone(), command, state)
    };
    // The kept lock is released before the caller takes the relays lock:
    // `route` takes relays and then kept, so the reverse order deadlocks.
    match &ack.verdict {
        Verdict::Closing
            if exiting || crate::resume::exited(&hub.home, &SessionId(session.to_owned())) =>
        {
            // A command the hub only partly wrote may have run: its own
            // closing answer reaches the client, and the command is never
            // routed again.
            if state == KeptState::Partial {
                None
            } else {
                Some(Settled::Reroute(id, command))
            }
        }
        Verdict::Accepted
            if command.get("command").and_then(Value::as_str) == Some("subscribe") =>
        {
            Some(Settled::Subscribed(command))
        }
        Verdict::Accepted
            if matches!(
                command.get("command").and_then(Value::as_str),
                Some("rewind" | "change_workspace")
            ) =>
        {
            ack.rewound.clone().map(Settled::Rewound)
        }
        Verdict::Accepted | Verdict::Rejected | Verdict::Closing => None,
    }
}

/// The `command_id` of a session's `command_accepted` or
/// `command_rejected`, and what the acknowledgement settles.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    Accepted,
    Rejected,
    Closing,
}

/// What one parse of a relayed session line settles: the acknowledged
/// command's id and verdict, and the session an accepted `rewind` or
/// `change_workspace` starts (`docs/events.md`:1308):
/// `result.new_session_id` with the shape the hub mints. `None` for an
/// acknowledgement that names none or names one of another shape, which
/// starts nothing. `settle` still starts one only for a kept `rewind` or
/// `change_workspace`, so a `new_session_id` value on any other command
/// is ignored.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Ack {
    pub(crate) id: String,
    pub(crate) verdict: Verdict,
    pub(crate) rewound: Option<SessionId>,
    pub(crate) last_seq: Option<u64>,
}

pub(crate) fn acknowledgement(line: &[u8]) -> Option<Ack> {
    parse_typed(line)
}

/// One map-only typed pass over the relayed line's bytes into `Ack`,
/// with no `Value` tree. A custom visitor reads the acknowledgement's
/// keys and skips every other value in place, so a wrong-typed optional
/// reads as absent instead of losing the whole `Ack`, while a malformed
/// value still rejects the line.
pub(crate) fn parse_typed(line: &[u8]) -> Option<Ack> {
    #[cfg(test)]
    ACK_PARSES.with(|count| count.set(count.get().saturating_add(1)));
    serde_json::from_slice::<RawAck>(line).ok()?.into_ack()
}

/// The acknowledgement's keys as the line carries them: every optional
/// reads leniently, so a wrong-typed value is absent, never an error.
struct RawAck {
    kind: LenientString,
    payload: LenientPayload,
}

struct RawPayload {
    command_id: LenientString,
    code: LenientString,
    result: Option<RawResult>,
}

struct RawResult {
    last_seq: Option<LenientU64>,
    new_session_id: LenientString,
}

#[derive(Default)]
struct LenientPayload(Option<RawPayload>);

/// A string the line may carry under another type: present only when it
/// is one.
#[derive(Clone, Default)]
struct LenientString(Option<String>);

/// A sequence number the line may carry under another type: present only
/// when it is one.
#[derive(Clone, Copy, Default)]
struct LenientU64(Option<u64>);

impl<'de> Deserialize<'de> for RawAck {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct AckVisitor;
        impl<'de> Visitor<'de> for AckVisitor {
            type Value = RawAck;
            // Exempt: serde_json never calls `expecting` through the
            // `Option`-returning parse, so its text observes nothing and
            // its mutants change no behaviour.
            #[cfg_attr(false, mutants::skip)]
            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a relayed line object")
            }
            fn visit_map<A>(self, mut map: A) -> Result<RawAck, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut kind = LenientString::default();
                let mut payload = LenientPayload::default();
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "kind" => kind = map.next_value()?,
                        "payload" => payload = map.next_value()?,
                        _ => {
                            map.next_value::<Validate>()?;
                        }
                    }
                }
                Ok(RawAck { kind, payload })
            }
        }
        deserializer.deserialize_map(AckVisitor)
    }
}

impl<'de> Deserialize<'de> for RawPayload {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct PayloadVisitor;
        impl<'de> Visitor<'de> for PayloadVisitor {
            type Value = RawPayload;
            // Exempt: serde_json never calls `expecting` through the
            // `Option`-returning parse, so its text observes nothing and
            // its mutants change no behaviour.
            #[cfg_attr(false, mutants::skip)]
            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("an acknowledgement payload object")
            }
            fn visit_map<A>(self, mut map: A) -> Result<RawPayload, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut command_id = LenientString::default();
                let mut code = LenientString::default();
                let mut result = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "command_id" => command_id = map.next_value()?,
                        "code" => code = map.next_value()?,
                        "result" => result = map.next_value()?,
                        _ => {
                            map.next_value::<Validate>()?;
                        }
                    }
                }
                Ok(RawPayload {
                    command_id,
                    code,
                    result,
                })
            }
        }
        deserializer.deserialize_map(PayloadVisitor)
    }
}

impl<'de> Deserialize<'de> for LenientPayload {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct PayloadVisitor;
        impl<'de> Visitor<'de> for PayloadVisitor {
            type Value = LenientPayload;
            // Exempt: serde_json never calls `expecting` through the
            // `Option`-returning parse, so its text observes nothing and
            // its mutants change no behaviour.
            #[cfg_attr(false, mutants::skip)]
            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("an acknowledgement payload object")
            }
            fn visit_map<A>(self, map: A) -> Result<LenientPayload, A::Error>
            where
                A: MapAccess<'de>,
            {
                let payload =
                    RawPayload::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
                Ok(LenientPayload(Some(payload)))
            }
            // Consume and validate this payload so a malformed earlier
            // duplicate cannot hide a later valid payload.
            fn visit_seq<A>(self, mut seq: A) -> Result<LenientPayload, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                while seq.next_element::<Validate>()?.is_some() {}
                Ok(LenientPayload(None))
            }
            // Exempt: a non-map payload carries no data, matching `Default`.
            #[cfg_attr(false, mutants::skip)]
            fn visit_bool<E>(self, _: bool) -> Result<LenientPayload, E>
            where
                E: serde::de::Error,
            {
                Ok(LenientPayload(None))
            }
            // Exempt: a non-map payload carries no data, matching `Default`.
            #[cfg_attr(false, mutants::skip)]
            fn visit_i64<E>(self, _: i64) -> Result<LenientPayload, E>
            where
                E: serde::de::Error,
            {
                Ok(LenientPayload(None))
            }
            // Exempt: a non-map payload carries no data, matching `Default`.
            #[cfg_attr(false, mutants::skip)]
            fn visit_u64<E>(self, _: u64) -> Result<LenientPayload, E>
            where
                E: serde::de::Error,
            {
                Ok(LenientPayload(None))
            }
            // Exempt: a non-map payload carries no data, matching `Default`.
            #[cfg_attr(false, mutants::skip)]
            fn visit_f64<E>(self, _: f64) -> Result<LenientPayload, E>
            where
                E: serde::de::Error,
            {
                Ok(LenientPayload(None))
            }
            // Exempt: a non-map payload carries no data, matching `Default`.
            #[cfg_attr(false, mutants::skip)]
            fn visit_str<E>(self, _: &str) -> Result<LenientPayload, E>
            where
                E: serde::de::Error,
            {
                Ok(LenientPayload(None))
            }
            // Exempt: a non-map payload carries no data, matching `Default`.
            #[cfg_attr(false, mutants::skip)]
            fn visit_string<E>(self, _: String) -> Result<LenientPayload, E>
            where
                E: serde::de::Error,
            {
                Ok(LenientPayload(None))
            }
            // Exempt: a non-map payload carries no data, matching `Default`.
            #[cfg_attr(false, mutants::skip)]
            fn visit_unit<E>(self) -> Result<LenientPayload, E>
            where
                E: serde::de::Error,
            {
                Ok(LenientPayload(None))
            }
        }
        deserializer.deserialize_any(PayloadVisitor)
    }
}

impl<'de> Deserialize<'de> for RawResult {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ResultVisitor;
        impl<'de> Visitor<'de> for ResultVisitor {
            type Value = RawResult;
            // Exempt: serde_json never calls `expecting` through the
            // `Option`-returning parse, so its text observes nothing and
            // its mutants change no behaviour.
            #[cfg_attr(false, mutants::skip)]
            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("an acknowledgement result object")
            }
            fn visit_map<A>(self, mut map: A) -> Result<RawResult, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut last_seq = None;
                let mut new_session_id = LenientString::default();
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "last_seq" => last_seq = map.next_value()?,
                        "new_session_id" => new_session_id = map.next_value()?,
                        _ => {
                            map.next_value::<Validate>()?;
                        }
                    }
                }
                Ok(RawResult {
                    last_seq,
                    new_session_id,
                })
            }
            // A result under any other type carries no sequence and
            // starts nothing.
            fn visit_unit<E>(self) -> Result<RawResult, E>
            where
                E: serde::de::Error,
            {
                Ok(RawResult::empty())
            }
            fn visit_bool<E>(self, _: bool) -> Result<RawResult, E>
            where
                E: serde::de::Error,
            {
                Ok(RawResult::empty())
            }
            fn visit_i64<E>(self, _: i64) -> Result<RawResult, E>
            where
                E: serde::de::Error,
            {
                Ok(RawResult::empty())
            }
            fn visit_u64<E>(self, _: u64) -> Result<RawResult, E>
            where
                E: serde::de::Error,
            {
                Ok(RawResult::empty())
            }
            fn visit_f64<E>(self, _: f64) -> Result<RawResult, E>
            where
                E: serde::de::Error,
            {
                Ok(RawResult::empty())
            }
            fn visit_str<E>(self, _: &str) -> Result<RawResult, E>
            where
                E: serde::de::Error,
            {
                Ok(RawResult::empty())
            }
            fn visit_string<E>(self, _: String) -> Result<RawResult, E>
            where
                E: serde::de::Error,
            {
                Ok(RawResult::empty())
            }
            fn visit_seq<A>(self, mut seq: A) -> Result<RawResult, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                while seq.next_element::<Validate>()?.is_some() {}
                Ok(RawResult::empty())
            }
        }
        deserializer.deserialize_any(ResultVisitor)
    }
}

impl RawResult {
    fn empty() -> Self {
        RawResult {
            last_seq: None,
            new_session_id: LenientString::default(),
        }
    }
}

impl<'de> Deserialize<'de> for LenientString {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct StringVisitor;
        impl<'de> Visitor<'de> for StringVisitor {
            type Value = LenientString;
            // Exempt: serde_json never calls `expecting` through the
            // `Option`-returning parse, so its text observes nothing and
            // its mutants change no behaviour.
            #[cfg_attr(false, mutants::skip)]
            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a string")
            }
            fn visit_str<E>(self, value: &str) -> Result<LenientString, E>
            where
                E: serde::de::Error,
            {
                Ok(LenientString(Some(value.to_owned())))
            }
            // No `visit_string`: `from_slice` borrows plain strings and
            // hands escaped ones from its scratch buffer through
            // `visit_str`, so serde's default `visit_string`, which
            // forwards to `visit_str`, already decodes both.
            // Exempt: a wrong-typed string reads as absent, so the arm
            // already returns the default and its mutant is identical.
            #[cfg_attr(false, mutants::skip)]
            fn visit_unit<E>(self) -> Result<LenientString, E>
            where
                E: serde::de::Error,
            {
                Ok(LenientString::default())
            }
            // Exempt: a wrong-typed string reads as absent, so the arm
            // already returns the default and its mutant is identical.
            #[cfg_attr(false, mutants::skip)]
            fn visit_bool<E>(self, _: bool) -> Result<LenientString, E>
            where
                E: serde::de::Error,
            {
                Ok(LenientString::default())
            }
            // Exempt: a wrong-typed string reads as absent, so the arm
            // already returns the default and its mutant is identical.
            #[cfg_attr(false, mutants::skip)]
            fn visit_i64<E>(self, _: i64) -> Result<LenientString, E>
            where
                E: serde::de::Error,
            {
                Ok(LenientString::default())
            }
            // Exempt: a wrong-typed string reads as absent, so the arm
            // already returns the default and its mutant is identical.
            #[cfg_attr(false, mutants::skip)]
            fn visit_u64<E>(self, _: u64) -> Result<LenientString, E>
            where
                E: serde::de::Error,
            {
                Ok(LenientString::default())
            }
            // Exempt: a wrong-typed string reads as absent, so the arm
            // already returns the default and its mutant is identical.
            #[cfg_attr(false, mutants::skip)]
            fn visit_f64<E>(self, _: f64) -> Result<LenientString, E>
            where
                E: serde::de::Error,
            {
                Ok(LenientString::default())
            }
            fn visit_seq<A>(self, mut seq: A) -> Result<LenientString, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                while seq.next_element::<Validate>()?.is_some() {}
                Ok(LenientString::default())
            }
            fn visit_map<A>(self, mut map: A) -> Result<LenientString, A::Error>
            where
                A: MapAccess<'de>,
            {
                while map.next_key::<String>()?.is_some() {
                    map.next_value::<Validate>()?;
                }
                Ok(LenientString::default())
            }
        }
        deserializer.deserialize_any(StringVisitor)
    }
}

impl<'de> Deserialize<'de> for LenientU64 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct SeqVisitor;
        impl<'de> Visitor<'de> for SeqVisitor {
            type Value = LenientU64;
            // Exempt: serde_json never calls `expecting` through the
            // `Option`-returning parse, so its text observes nothing and
            // its mutants change no behaviour.
            #[cfg_attr(false, mutants::skip)]
            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a sequence number")
            }
            fn visit_u64<E>(self, value: u64) -> Result<LenientU64, E>
            where
                E: serde::de::Error,
            {
                Ok(LenientU64(Some(value)))
            }
            // Exempt: a wrong-typed sequence reads as absent, so the arm
            // already returns the default and its mutant is identical.
            #[cfg_attr(false, mutants::skip)]
            fn visit_bool<E>(self, _: bool) -> Result<LenientU64, E>
            where
                E: serde::de::Error,
            {
                Ok(LenientU64::default())
            }
            // Exempt: a wrong-typed sequence reads as absent, so the arm
            // already returns the default and its mutant is identical.
            #[cfg_attr(false, mutants::skip)]
            fn visit_i64<E>(self, _: i64) -> Result<LenientU64, E>
            where
                E: serde::de::Error,
            {
                Ok(LenientU64::default())
            }
            // Exempt: a wrong-typed sequence reads as absent, so the arm
            // already returns the default and its mutant is identical.
            #[cfg_attr(false, mutants::skip)]
            fn visit_f64<E>(self, _: f64) -> Result<LenientU64, E>
            where
                E: serde::de::Error,
            {
                Ok(LenientU64::default())
            }
            // Exempt: a wrong-typed sequence reads as absent, so the arm
            // already returns the default and its mutant is identical.
            #[cfg_attr(false, mutants::skip)]
            fn visit_str<E>(self, _: &str) -> Result<LenientU64, E>
            where
                E: serde::de::Error,
            {
                Ok(LenientU64::default())
            }
            // No `visit_string`: `from_slice` borrows plain strings and
            // hands escaped ones from its scratch buffer through
            // `visit_str`, so serde's default `visit_string`, which
            // forwards to `visit_str`, already reads both as absent.
            // Exempt: a wrong-typed sequence reads as absent, so the arm
            // already returns the default and its mutant is identical.
            #[cfg_attr(false, mutants::skip)]
            fn visit_unit<E>(self) -> Result<LenientU64, E>
            where
                E: serde::de::Error,
            {
                Ok(LenientU64::default())
            }
            fn visit_seq<A>(self, mut seq: A) -> Result<LenientU64, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                while seq.next_element::<Validate>()?.is_some() {}
                Ok(LenientU64::default())
            }
            fn visit_map<A>(self, mut map: A) -> Result<LenientU64, A::Error>
            where
                A: MapAccess<'de>,
            {
                while map.next_key::<String>()?.is_some() {
                    map.next_value::<Validate>()?;
                }
                Ok(LenientU64::default())
            }
        }
        deserializer.deserialize_any(SeqVisitor)
    }
}

/// An unknown value the line carries: read for real and dropped, so a
/// malformed value rejects the line exactly where the full parse did. A
/// skipping ignore would not validate escapes or number range.
struct Validate;

impl<'de> Deserialize<'de> for Validate {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ValidateVisitor;
        impl<'de> Visitor<'de> for ValidateVisitor {
            type Value = Validate;
            // Exempt: serde_json never calls `expecting` through the
            // `Option`-returning parse, so its text observes nothing and
            // its mutants change no behaviour.
            #[cfg_attr(false, mutants::skip)]
            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("any value")
            }
            fn visit_bool<E>(self, _: bool) -> Result<Validate, E>
            where
                E: serde::de::Error,
            {
                Ok(Validate)
            }
            fn visit_i64<E>(self, _: i64) -> Result<Validate, E>
            where
                E: serde::de::Error,
            {
                Ok(Validate)
            }
            fn visit_u64<E>(self, _: u64) -> Result<Validate, E>
            where
                E: serde::de::Error,
            {
                Ok(Validate)
            }
            fn visit_f64<E>(self, _: f64) -> Result<Validate, E>
            where
                E: serde::de::Error,
            {
                Ok(Validate)
            }
            fn visit_str<E>(self, _: &str) -> Result<Validate, E>
            where
                E: serde::de::Error,
            {
                Ok(Validate)
            }
            fn visit_string<E>(self, _: String) -> Result<Validate, E>
            where
                E: serde::de::Error,
            {
                Ok(Validate)
            }
            fn visit_unit<E>(self) -> Result<Validate, E>
            where
                E: serde::de::Error,
            {
                Ok(Validate)
            }
            fn visit_seq<A>(self, mut seq: A) -> Result<Validate, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                while seq.next_element::<Validate>()?.is_some() {}
                Ok(Validate)
            }
            fn visit_map<A>(self, mut map: A) -> Result<Validate, A::Error>
            where
                A: MapAccess<'de>,
            {
                while map.next_key::<String>()?.is_some() {
                    map.next_value::<Validate>()?;
                }
                Ok(Validate)
            }
        }
        deserializer.deserialize_any(ValidateVisitor)
    }
}

impl RawAck {
    fn into_ack(self) -> Option<Ack> {
        let kind = self.kind.0?;
        if !matches!(kind.as_str(), "command_accepted" | "command_rejected") {
            return None;
        }
        let payload = self.payload.0?;
        let id = payload.command_id.0?;
        let verdict = if kind == "command_accepted" {
            Verdict::Accepted
        } else if payload.code.0.as_deref() == Some("closing") {
            Verdict::Closing
        } else {
            Verdict::Rejected
        };
        let rewound = if kind == "command_accepted" {
            payload
                .result
                .as_ref()
                .and_then(|result| result.new_session_id.0.clone())
                .filter(|next| valid_session_id(next))
                .map(SessionId)
        } else {
            None
        };
        let last_seq = match verdict {
            Verdict::Accepted => payload
                .result
                .as_ref()
                .and_then(|result| result.last_seq)
                .and_then(|lenient| lenient.0),
            Verdict::Rejected | Verdict::Closing => None,
        };
        Some(Ack {
            id,
            verdict,
            rewound,
            last_seq,
        })
    }
}

// Counts the typed passes over relayed lines, on the calling thread: the
// parse's result is consumed inside the relay thread, and no client-visible
// line differs between one parse and five, so nothing outside can count it.
// Test-only; non-test builds never call it.
#[cfg(test)]
thread_local! {
    static ACK_PARSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many typed passes over relayed lines the calling thread has made
/// (`docs/testing.md`, "a test of how much work something does counts the
/// work"). The counted-work test reads it on the relay thread.
#[cfg(test)]
pub(crate) fn ack_parses() -> usize {
    ACK_PARSES.with(|count| count.get())
}

/// Whether `ack` is a subscription the hub sent again on this relay:
/// its id is dropped from the shared list once, so a transferred
/// level never leaks its acknowledgement to the client, while a later
/// transfer for the same relay still drops its own.
pub(super) fn muted(ack: Option<&Ack>, replayed: &Replayed) -> bool {
    let Some(ack) = ack else {
        return false;
    };
    let mut replayed = lock(replayed);
    if let Some(at) = replayed.iter().position(|muted| *muted == ack.id) {
        replayed.remove(at);
        true
    } else {
        false
    }
}
