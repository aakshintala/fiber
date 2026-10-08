//! The first stage's request shape (`docs/permissions.md`, "How it runs"):
//! its output ceiling, and the thinking level every reviewer request asks
//! for.

use contract::ThinkingLevel;

/// The first stage's output ceiling: a runaway guard, not the answer's
/// length (`docs/permissions.md`, "How it runs"). A model that answers with
/// one word stops on its own; a model that reasons first is billed for its
/// reasoning anyway, so cutting at the answer's length saves little and
/// loses the answer. Without a ceiling a model that ignores "one word" can
/// write up to its own output limit (131,072 tokens on Muse).
///
/// Measured on 2026-10-08 at `api.meta.ai/v1/responses`, at `minimal` with
/// no cap: `muse-spark-1.3` wrote 123-319 tokens (2.4-5.7 s) and
/// `muse-spark-1.3-contributor` wrote 195-293 tokens (2.5-6.9 s). With no
/// level sent, they wrote 533-1,315. `claude-sonnet-5-5` (and OpenRouter's
/// `anthropic/claude-sonnet-5.5`) take 3-4 tokens; `gpt-6-luna` and
/// `gemini-3.8-flash` reason for up to 50 and 82 tokens before the word.
pub(super) const FIRST_STAGE_OUTPUT_TOKENS: u64 = 4096;

/// The lowest of `levels` in `ThinkingLevel::ALL` order; `None` for none.
/// Array order is ignored: what bounds the stage's cost is the model's
/// lowest declared level (`docs/model-routing.md`, "Thinking").
pub(super) fn lowest(levels: &[ThinkingLevel]) -> Option<ThinkingLevel> {
    ThinkingLevel::ALL
        .into_iter()
        .find(|level| levels.contains(level))
}

#[cfg(test)]
#[path = "request_tests.rs"]
mod tests;
