//! The bytes-to-tokens rate of the last preamble build (`docs/tools.md`,
//! "Seeing the tools"): every input token the build's first own request
//! reported in `usage_recorded`, whether uncached, read from the cache or
//! written to it, over that request's input size in bytes. [`Log`](crate::Log)
//! folds one from every line it appends and every line
//! [`Log::open`](crate::Log) replays.

use std::collections::HashSet;

use contract::Envelope;
use contract::events::{Event, UsageRecorded};

/// The bytes-to-tokens rate of the last preamble build (`docs/tools.md`,
/// "Seeing the tools").
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rate {
    /// Every input token of the first own request after the build;
    /// None until that request's `usage_recorded`.
    input_tokens: Option<u64>,
    /// That request's input size in bytes; 0 before one.
    input_bytes: u64,
}

impl Rate {
    /// `bytes` in tokens: floor(bytes * input_tokens / input_bytes) in u128,
    /// saturating at u64::MAX; None before the first own request after the
    /// last build, or when that request's input size is 0 bytes.
    pub fn tokens(&self, bytes: u64) -> Option<u64> {
        let input_tokens = self.input_tokens?;
        if self.input_bytes == 0 {
            return None;
        }
        let tokens = u128::from(bytes) * u128::from(input_tokens) / u128::from(self.input_bytes);
        Some(u64::try_from(tokens).unwrap_or(u64::MAX))
    }
}

/// What `log` keeps to fold the rate: the rate, the last build's model, and
/// the action ids of the `assistant_message_started` lines since the latest
/// build while its numerator is unset.
#[derive(Debug, Default)]
pub(crate) struct RateFold {
    rate: Rate,
    model: Option<String>,
    started: HashSet<String>,
}

impl RateFold {
    /// Folds one appended or replayed line.
    pub(crate) fn fold(&mut self, line: &Envelope) {
        match line.kind.as_str() {
            "preamble_built" => {
                self.rate = Rate::default();
                self.model = line
                    .payload
                    .get("model")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned);
                self.started.clear();
            }
            "assistant_message_started" => {
                if self.rate.input_tokens.is_none()
                    && let Some(action) = &line.action_id
                {
                    self.started.insert(action.0.clone());
                }
            }
            "usage_recorded" => {
                if self.rate.input_tokens.is_none()
                    && line
                        .action_id
                        .as_ref()
                        .is_some_and(|action| self.started.contains(&action.0))
                    && let Ok(Some(Event::UsageRecorded(recorded))) = Event::from_envelope(line)
                    && recorded.extension.is_none()
                    && recorded.origin_session_id.is_none()
                    && self.model.as_deref() == Some(recorded.model.as_str())
                    && recorded.input_media != Some(true)
                    && recorded.input_bytes > 0
                {
                    self.rate = Rate {
                        input_tokens: Some(input_tokens(&recorded)),
                        input_bytes: recorded.input_bytes,
                    };
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

/// Every input token `recorded` reports: uncached, read from the cache and
/// written to it, each addition saturating.
pub(crate) fn input_tokens(recorded: &UsageRecorded) -> u64 {
    recorded.tokens.cache_write.values().fold(
        recorded
            .tokens
            .input
            .saturating_add(recorded.tokens.cache_read),
        |sum, written| sum.saturating_add(*written),
    )
}

#[cfg(test)]
#[path = "rate_tests.rs"]
mod tests;
