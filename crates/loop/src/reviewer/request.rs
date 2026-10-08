//! The first stage's request shape (`docs/permissions.md`, "How it runs"):
//! its output ceiling, and the thinking level every reviewer request asks
//! for.

use contract::ThinkingLevel;

/// The first stage's output limit. A reviewer model that reasons before it
/// answers spends the limit on that reasoning, so the limit covers it as well
/// as the word. Measured against the models a first-party provider names for
/// review (`docs/model-routing.md`): `claude-sonnet-5-5` (and OpenRouter's
/// `anthropic/claude-sonnet-5.5`) takes 4 tokens for `allow` and 3 for `check`; `gpt-6-luna` and `gemini-3.8-flash` reasoned
/// for up to 50 and 82 tokens before the one-token word.
pub(super) const FIRST_STAGE_OUTPUT_TOKENS: u64 = 128;

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
