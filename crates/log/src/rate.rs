//! The bytes-to-tokens rate of the last preamble build (`docs/tools.md`,
//! "Seeing the tools"): the preamble's size in bytes, and the tokens the
//! build's first own request wrote to the cache. [`Log`](crate::Log) folds
//! one from every line it appends and every line [`Log::open`](crate::Log)
//! replays.

use std::collections::HashSet;

use contract::Envelope;
use contract::events::{Event, UsageRecorded};
use serde_json::{Map, Value};

/// The bytes-to-tokens rate of the last preamble build (`docs/tools.md`,
/// "Seeing the tools").
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rate {
    /// The last `preamble_built`'s request fields' compact-JSON size in bytes; 0 before one.
    preamble: u64,
    /// Cache-write tokens from the first own request after the build;
    /// None until that request's `usage_recorded`.
    written: Option<u64>,
}

impl Rate {
    /// `bytes` in tokens: floor(bytes * written / preamble) in u128,
    /// saturating at u64::MAX; None before the first own request after the
    /// last build, or when the preamble is 0 bytes.
    pub fn tokens(&self, bytes: u64) -> Option<u64> {
        let written = self.written?;
        if self.preamble == 0 {
            return None;
        }
        let tokens = u128::from(bytes) * u128::from(written) / u128::from(self.preamble);
        Some(u64::try_from(tokens).unwrap_or(u64::MAX))
    }
}

/// What `log` keeps to fold the rate: the rate, and the action ids of the
/// `assistant_message_started` lines since the latest build while its
/// numerator is unset.
#[derive(Debug, Default)]
pub(crate) struct RateFold {
    rate: Rate,
    started: HashSet<String>,
}

impl RateFold {
    /// Folds one appended or replayed line.
    pub(crate) fn fold(&mut self, line: &Envelope) {
        match line.kind.as_str() {
            "preamble_built" => {
                self.rate = Rate {
                    preamble: preamble_size(&line.payload),
                    written: None,
                };
                self.started.clear();
            }
            "assistant_message_started" => {
                if self.rate.written.is_none()
                    && let Some(action) = &line.action_id
                {
                    self.started.insert(action.0.clone());
                }
            }
            "usage_recorded" => {
                if self.rate.written.is_none()
                    && line
                        .action_id
                        .as_ref()
                        .is_some_and(|action| self.started.contains(&action.0))
                    && let Ok(Some(Event::UsageRecorded(recorded))) = Event::from_envelope(line)
                    && recorded.extension.is_none()
                    && recorded.origin_session_id.is_none()
                {
                    self.rate.written = Some(cache_written(&recorded));
                    self.started.clear();
                }
            }
            _ => {}
        }
    }

    /// The rate folded so far.
    pub(crate) fn rate(&self) -> Rate {
        self.rate
    }
}

/// The size in bytes of the request a `preamble_built` payload describes:
/// the compact JSON size of `preamble_built`'s request fields.
pub(crate) fn preamble_size(payload: &Map<String, Value>) -> u64 {
    let mut request = Map::new();
    for key in [
        "cache_lifetime",
        "credential",
        "model",
        "system_prompt",
        "thinking",
        "tool_choice",
    ] {
        if let Some(value) = payload.get(key) {
            request.insert(key.to_owned(), value.clone());
        }
    }
    let tools: Vec<Value> =
        payload
            .get("tools")
            .and_then(Value::as_array)
            .map_or(Vec::new(), |tools| {
                tools
                    .iter()
                    .filter_map(|tool| tool.get("definition").cloned())
                    .collect()
            });
    request.insert("tools".to_owned(), Value::Array(tools));
    serde_json::to_vec(&request).map_or(0, |bytes| u64::try_from(bytes.len()).unwrap_or(u64::MAX))
}

/// The tokens `recorded` wrote to the cache, summed across every lifetime.
pub(crate) fn cache_written(recorded: &UsageRecorded) -> u64 {
    recorded
        .tokens
        .cache_write
        .values()
        .fold(0, |sum, written| sum.saturating_add(*written))
}

#[cfg(test)]
#[path = "rate_tests.rs"]
mod tests;
