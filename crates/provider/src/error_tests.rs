//! The code a quota, billing, rate-limit or unknown-model reply maps to:
//! the documented quota shapes (`research/provider-errors/quota.md`, one
//! fixture each in `research/provider-errors/documented/`), the recorded
//! replies (`research/retry-signals`, `research/provider-errors`), and the
//! ChatGPT/codex usage-limit shape (`research/codex-responses-probe`,
//! "The usage-limit error body").

use contract::ErrorCode;
use contract::events::CacheLifetime;
use serde_json::Value;

use crate::Error;
use crate::redact::Secrets;

/// Every recorded reply under `research/*/raw/`, compiled in: reading
/// them at run time would read the repository at test time (`docs/ci.md`,
/// "Selection"). Each entry names the recording, then holds its text.
const RECORDED: &[(&str, &str)] = &[
    (
        "research/anthropic-messages-probe/raw/beta.json",
        include_str!("../../../research/anthropic-messages-probe/raw/beta.json"),
    ),
    (
        "research/anthropic-messages-probe/raw/cache.json",
        include_str!("../../../research/anthropic-messages-probe/raw/cache.json"),
    ),
    (
        "research/anthropic-messages-probe/raw/caller.json",
        include_str!("../../../research/anthropic-messages-probe/raw/caller.json"),
    ),
    (
        "research/anthropic-messages-probe/raw/choice.json",
        include_str!("../../../research/anthropic-messages-probe/raw/choice.json"),
    ),
    (
        "research/anthropic-messages-probe/raw/ptc.json",
        include_str!("../../../research/anthropic-messages-probe/raw/ptc.json"),
    ),
    (
        "research/anthropic-messages-probe/raw/stream.json",
        include_str!("../../../research/anthropic-messages-probe/raw/stream.json"),
    ),
    (
        "research/anthropic-messages-probe/raw/thinking-enabled.json",
        include_str!("../../../research/anthropic-messages-probe/raw/thinking-enabled.json"),
    ),
    (
        "research/bedrock-invoke-probe/raw/adaptive.json",
        include_str!("../../../research/bedrock-invoke-probe/raw/adaptive.json"),
    ),
    (
        "research/bedrock-invoke-probe/raw/bad-bearer.json",
        include_str!("../../../research/bedrock-invoke-probe/raw/bad-bearer.json"),
    ),
    (
        "research/bedrock-invoke-probe/raw/bad-key-id.json",
        include_str!("../../../research/bedrock-invoke-probe/raw/bad-key-id.json"),
    ),
    (
        "research/bedrock-invoke-probe/raw/bad-model.json",
        include_str!("../../../research/bedrock-invoke-probe/raw/bad-model.json"),
    ),
    (
        "research/bedrock-invoke-probe/raw/bad-signature.json",
        include_str!("../../../research/bedrock-invoke-probe/raw/bad-signature.json"),
    ),
    (
        "research/bedrock-invoke-probe/raw/cache-read.json",
        include_str!("../../../research/bedrock-invoke-probe/raw/cache-read.json"),
    ),
    (
        "research/bedrock-invoke-probe/raw/cache-write.json",
        include_str!("../../../research/bedrock-invoke-probe/raw/cache-write.json"),
    ),
    (
        "research/bedrock-invoke-probe/raw/plain.json",
        include_str!("../../../research/bedrock-invoke-probe/raw/plain.json"),
    ),
    (
        "research/bedrock-invoke-probe/raw/thinking.json",
        include_str!("../../../research/bedrock-invoke-probe/raw/thinking.json"),
    ),
    (
        "research/bedrock-invoke-probe/raw/tool.json",
        include_str!("../../../research/bedrock-invoke-probe/raw/tool.json"),
    ),
    (
        "research/bedrock-invoke-probe/raw/ttl-1h.json",
        include_str!("../../../research/bedrock-invoke-probe/raw/ttl-1h.json"),
    ),
    (
        "research/bedrock-invoke-probe/raw/validation.json",
        include_str!("../../../research/bedrock-invoke-probe/raw/validation.json"),
    ),
    (
        "research/codex-responses-probe/raw/beta-other.json",
        include_str!("../../../research/codex-responses-probe/raw/beta-other.json"),
    ),
    (
        "research/codex-responses-probe/raw/beta-with.json",
        include_str!("../../../research/codex-responses-probe/raw/beta-with.json"),
    ),
    (
        "research/codex-responses-probe/raw/beta-without-nosid.json",
        include_str!("../../../research/codex-responses-probe/raw/beta-without-nosid.json"),
    ),
    (
        "research/codex-responses-probe/raw/beta-without.json",
        include_str!("../../../research/codex-responses-probe/raw/beta-without.json"),
    ),
    (
        "research/codex-responses-probe/raw/cache-1-fresh-1.json",
        include_str!("../../../research/codex-responses-probe/raw/cache-1-fresh-1.json"),
    ),
    (
        "research/codex-responses-probe/raw/cache-1-fresh-2.json",
        include_str!("../../../research/codex-responses-probe/raw/cache-1-fresh-2.json"),
    ),
    (
        "research/codex-responses-probe/raw/cache-1-stable-1.json",
        include_str!("../../../research/codex-responses-probe/raw/cache-1-stable-1.json"),
    ),
    (
        "research/codex-responses-probe/raw/cache-1-stable-2.json",
        include_str!("../../../research/codex-responses-probe/raw/cache-1-stable-2.json"),
    ),
    (
        "research/codex-responses-probe/raw/cache-2-fresh-1.json",
        include_str!("../../../research/codex-responses-probe/raw/cache-2-fresh-1.json"),
    ),
    (
        "research/codex-responses-probe/raw/cache-2-fresh-2.json",
        include_str!("../../../research/codex-responses-probe/raw/cache-2-fresh-2.json"),
    ),
    (
        "research/codex-responses-probe/raw/cache-2-stable-1.json",
        include_str!("../../../research/codex-responses-probe/raw/cache-2-stable-1.json"),
    ),
    (
        "research/codex-responses-probe/raw/cache-2-stable-2.json",
        include_str!("../../../research/codex-responses-probe/raw/cache-2-stable-2.json"),
    ),
    (
        "research/codex-responses-probe/raw/cache-3-fresh-1.json",
        include_str!("../../../research/codex-responses-probe/raw/cache-3-fresh-1.json"),
    ),
    (
        "research/codex-responses-probe/raw/cache-3-fresh-2.json",
        include_str!("../../../research/codex-responses-probe/raw/cache-3-fresh-2.json"),
    ),
    (
        "research/codex-responses-probe/raw/cache-3-stable-1.json",
        include_str!("../../../research/codex-responses-probe/raw/cache-3-stable-1.json"),
    ),
    (
        "research/codex-responses-probe/raw/cache-3-stable-2.json",
        include_str!("../../../research/codex-responses-probe/raw/cache-3-stable-2.json"),
    ),
    (
        "research/codex-responses-probe/raw/cachehdr-session-id-hyphen-1.json",
        include_str!(
            "../../../research/codex-responses-probe/raw/cachehdr-session-id-hyphen-1.json"
        ),
    ),
    (
        "research/codex-responses-probe/raw/cachehdr-session-id-hyphen-2.json",
        include_str!(
            "../../../research/codex-responses-probe/raw/cachehdr-session-id-hyphen-2.json"
        ),
    ),
    (
        "research/codex-responses-probe/raw/cachehdr-sessionid-underscore-1.json",
        include_str!(
            "../../../research/codex-responses-probe/raw/cachehdr-sessionid-underscore-1.json"
        ),
    ),
    (
        "research/codex-responses-probe/raw/cachehdr-sessionid-underscore-2.json",
        include_str!(
            "../../../research/codex-responses-probe/raw/cachehdr-sessionid-underscore-2.json"
        ),
    ),
    (
        "research/codex-responses-probe/raw/cachehdr-xreq-only-1.json",
        include_str!("../../../research/codex-responses-probe/raw/cachehdr-xreq-only-1.json"),
    ),
    (
        "research/codex-responses-probe/raw/cachehdr-xreq-only-2.json",
        include_str!("../../../research/codex-responses-probe/raw/cachehdr-xreq-only-2.json"),
    ),
    (
        "research/codex-responses-probe/raw/cachehdronly-1.json",
        include_str!("../../../research/codex-responses-probe/raw/cachehdronly-1.json"),
    ),
    (
        "research/codex-responses-probe/raw/cachehdronly-2.json",
        include_str!("../../../research/codex-responses-probe/raw/cachehdronly-2.json"),
    ),
    (
        "research/codex-responses-probe/raw/cachekeyonly-1.json",
        include_str!("../../../research/codex-responses-probe/raw/cachekeyonly-1.json"),
    ),
    (
        "research/codex-responses-probe/raw/cachekeyonly-2.json",
        include_str!("../../../research/codex-responses-probe/raw/cachekeyonly-2.json"),
    ),
    (
        "research/codex-responses-probe/raw/cachenone-1.json",
        include_str!("../../../research/codex-responses-probe/raw/cachenone-1.json"),
    ),
    (
        "research/codex-responses-probe/raw/cachenone-2.json",
        include_str!("../../../research/codex-responses-probe/raw/cachenone-2.json"),
    ),
    (
        "research/codex-responses-probe/raw/cachesplit-1.json",
        include_str!("../../../research/codex-responses-probe/raw/cachesplit-1.json"),
    ),
    (
        "research/codex-responses-probe/raw/cachesplit-2.json",
        include_str!("../../../research/codex-responses-probe/raw/cachesplit-2.json"),
    ),
    (
        "research/codex-responses-probe/raw/ev-text.json",
        include_str!("../../../research/codex-responses-probe/raw/ev-text.json"),
    ),
    (
        "research/codex-responses-probe/raw/ev-text2.json",
        include_str!("../../../research/codex-responses-probe/raw/ev-text2.json"),
    ),
    (
        "research/codex-responses-probe/raw/ev-tool.json",
        include_str!("../../../research/codex-responses-probe/raw/ev-tool.json"),
    ),
    (
        "research/codex-responses-probe/raw/ev-tool2.json",
        include_str!("../../../research/codex-responses-probe/raw/ev-tool2.json"),
    ),
    (
        "research/codex-responses-probe/raw/f-baseline.json",
        include_str!("../../../research/codex-responses-probe/raw/f-baseline.json"),
    ),
    (
        "research/codex-responses-probe/raw/f-parallel-false.json",
        include_str!("../../../research/codex-responses-probe/raw/f-parallel-false.json"),
    ),
    (
        "research/codex-responses-probe/raw/f-parallel-omitted.json",
        include_str!("../../../research/codex-responses-probe/raw/f-parallel-omitted.json"),
    ),
    (
        "research/codex-responses-probe/raw/f-parallel-true.json",
        include_str!("../../../research/codex-responses-probe/raw/f-parallel-true.json"),
    ),
    (
        "research/codex-responses-probe/raw/f-temp.json",
        include_str!("../../../research/codex-responses-probe/raw/f-temp.json"),
    ),
    (
        "research/codex-responses-probe/raw/f-temp0.json",
        include_str!("../../../research/codex-responses-probe/raw/f-temp0.json"),
    ),
    (
        "research/codex-responses-probe/raw/f-tier-auto.json",
        include_str!("../../../research/codex-responses-probe/raw/f-tier-auto.json"),
    ),
    (
        "research/codex-responses-probe/raw/f-tier-default.json",
        include_str!("../../../research/codex-responses-probe/raw/f-tier-default.json"),
    ),
    (
        "research/codex-responses-probe/raw/f-tier-flex.json",
        include_str!("../../../research/codex-responses-probe/raw/f-tier-flex.json"),
    ),
    (
        "research/codex-responses-probe/raw/f-tier-priority.json",
        include_str!("../../../research/codex-responses-probe/raw/f-tier-priority.json"),
    ),
    (
        "research/codex-responses-probe/raw/f-toolchoice-auto.json",
        include_str!("../../../research/codex-responses-probe/raw/f-toolchoice-auto.json"),
    ),
    (
        "research/codex-responses-probe/raw/f-toolchoice-required.json",
        include_str!("../../../research/codex-responses-probe/raw/f-toolchoice-required.json"),
    ),
    (
        "research/codex-responses-probe/raw/f-verb-high.json",
        include_str!("../../../research/codex-responses-probe/raw/f-verb-high.json"),
    ),
    (
        "research/codex-responses-probe/raw/f-verb-low.json",
        include_str!("../../../research/codex-responses-probe/raw/f-verb-low.json"),
    ),
    (
        "research/codex-responses-probe/raw/f-verb-omitted.json",
        include_str!("../../../research/codex-responses-probe/raw/f-verb-omitted.json"),
    ),
    (
        "research/google-generative-ai-probe/raw/auth-badheader.json",
        include_str!("../../../research/google-generative-ai-probe/raw/auth-badheader.json"),
    ),
    (
        "research/google-generative-ai-probe/raw/auth-both-badheader-goodquery.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/auth-both-badheader-goodquery.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/auth-both.json",
        include_str!("../../../research/google-generative-ai-probe/raw/auth-both.json"),
    ),
    (
        "research/google-generative-ai-probe/raw/auth-header.json",
        include_str!("../../../research/google-generative-ai-probe/raw/auth-header.json"),
    ),
    (
        "research/google-generative-ai-probe/raw/auth-none.json",
        include_str!("../../../research/google-generative-ai-probe/raw/auth-none.json"),
    ),
    (
        "research/google-generative-ai-probe/raw/auth-query.json",
        include_str!("../../../research/google-generative-ai-probe/raw/auth-query.json"),
    ),
    (
        "research/google-generative-ai-probe/raw/auth2-badheader-goodquery.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/auth2-badheader-goodquery.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/auth2-header-gemini-3.1-flash-lite.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/auth2-header-gemini-3.1-flash-lite.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/auth2-query-gemini-3.1-flash-lite.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/auth2-query-gemini-3.1-flash-lite.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/auth3-header-gemini-2.5-flash-image.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/auth3-header-gemini-2.5-flash-image.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/auth3-query-gemini-2.5-flash-image.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/auth3-query-gemini-2.5-flash-image.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/id-bad-space-slash-gemini-2.5-flash.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/id-bad-space-slash-gemini-2.5-flash.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/id-bad-space-slash-gemini-3.1-flash-lite.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/id-bad-space-slash-gemini-3.1-flash-lite.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/id-emitted-gemini-2.5-flash.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/id-emitted-gemini-2.5-flash.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/id-emitted-gemini-3.1-flash-lite.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/id-emitted-gemini-3.1-flash-lite.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/id-good-gemini-2.5-flash.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/id-good-gemini-2.5-flash.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/id-good-gemini-3.1-flash-lite.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/id-good-gemini-3.1-flash-lite.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/id-long65-gemini-2.5-flash.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/id-long65-gemini-2.5-flash.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/id-long65-gemini-3.1-flash-lite.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/id-long65-gemini-3.1-flash-lite.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/id-none-gemini-2.5-flash.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/id-none-gemini-2.5-flash.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/id-none-gemini-3.1-flash-lite.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/id-none-gemini-3.1-flash-lite.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/id2-bad-space-slash-gemini-3.1-flash-lite.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/id2-bad-space-slash-gemini-3.1-flash-lite.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/id2-emitted-gemini-3.1-flash-lite.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/id2-emitted-gemini-3.1-flash-lite.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/id2-good-gemini-3.1-flash-lite.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/id2-good-gemini-3.1-flash-lite.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/id2-long65-gemini-3.1-flash-lite.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/id2-long65-gemini-3.1-flash-lite.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/id2-mismatch-gemini-3.1-flash-lite.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/id2-mismatch-gemini-3.1-flash-lite.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/img-plain-user-part-gemini-2.5-flash.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/img-plain-user-part-gemini-2.5-flash.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/imgparts-gemini-2.5-flash.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/imgparts-gemini-2.5-flash.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/imgparts-gemini-3.1-flash-lite.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/imgparts-gemini-3.1-flash-lite.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/imgparts2-blue-gemini-3.1-flash-lite.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/imgparts2-blue-gemini-3.1-flash-lite.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/imgparts2-none-gemini-3.1-flash-lite.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/imgparts2-none-gemini-3.1-flash-lite.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/lite-404-gemini-2.5-flash-lite.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/lite-404-gemini-2.5-flash-lite.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/mode-ANY-gemini-2.5-flash.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/mode-ANY-gemini-2.5-flash.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/mode-ANY-gemini-3.1-flash-lite.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/mode-ANY-gemini-3.1-flash-lite.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/mode-AUTO-gemini-2.5-flash.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/mode-AUTO-gemini-2.5-flash.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/mode-AUTO-gemini-3.1-flash-lite.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/mode-AUTO-gemini-3.1-flash-lite.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/mode-lowercase-validated.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/mode-lowercase-validated.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/mode-NONE-gemini-2.5-flash.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/mode-NONE-gemini-2.5-flash.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/mode-NONE-gemini-3.1-flash-lite.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/mode-NONE-gemini-3.1-flash-lite.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/mode-tempt-AUTO-gemini-2.5-flash.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/mode-tempt-AUTO-gemini-2.5-flash.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/mode-tempt-AUTO-gemini-3.1-flash-lite.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/mode-tempt-AUTO-gemini-3.1-flash-lite.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/mode-tempt-VALIDATED-gemini-2.5-flash.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/mode-tempt-VALIDATED-gemini-2.5-flash.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/mode-tempt-VALIDATED-gemini-3.1-flash-lite.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/mode-tempt-VALIDATED-gemini-3.1-flash-lite.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/mode-VALIDATED-gemini-2.5-flash.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/mode-VALIDATED-gemini-2.5-flash.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/mode-VALIDATED-gemini-3.1-flash-lite.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/mode-VALIDATED-gemini-3.1-flash-lite.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/schema-anyof-only-in-parameters-gemini-2.5-flash.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/schema-anyof-only-in-parameters-gemini-2.5-flash.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/schema-json-schema-in-parameters-gemini-2.5-flash.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/schema-json-schema-in-parameters-gemini-2.5-flash.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/schema-json-schema-in-parameters-gemini-3.1-flash-lite.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/schema-json-schema-in-parameters-gemini-3.1-flash-lite.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/schema-json-schema-in-parametersJsonSchema-gemini-2.5-flash.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/schema-json-schema-in-parametersJsonSchema-gemini-2.5-flash.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/schema-json-schema-in-parametersJsonSchema-gemini-3.1-flash-lite.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/schema-json-schema-in-parametersJsonSchema-gemini-3.1-flash-lite.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/schema-openapi-subset-in-parameters-gemini-2.5-flash.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/schema-openapi-subset-in-parameters-gemini-2.5-flash.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/schema-openapi-subset-in-parameters-gemini-3.1-flash-lite.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/schema-openapi-subset-in-parameters-gemini-3.1-flash-lite.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/sse-bad-mime.json",
        include_str!("../../../research/google-generative-ai-probe/raw/sse-bad-mime.json"),
    ),
    (
        "research/google-generative-ai-probe/raw/sse-empty-contents.json",
        include_str!("../../../research/google-generative-ai-probe/raw/sse-empty-contents.json"),
    ),
    (
        "research/google-generative-ai-probe/raw/sse-huge-maxoutput.json",
        include_str!("../../../research/google-generative-ai-probe/raw/sse-huge-maxoutput.json"),
    ),
    (
        "research/google-generative-ai-probe/raw/sse-long-gen-cut.json",
        include_str!("../../../research/google-generative-ai-probe/raw/sse-long-gen-cut.json"),
    ),
    (
        "research/google-generative-ai-probe/raw/sse-toolresponse-noname.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/sse-toolresponse-noname.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/sse2-huge-maxoutput.json",
        include_str!("../../../research/google-generative-ai-probe/raw/sse2-huge-maxoutput.json"),
    ),
    (
        "research/google-generative-ai-probe/raw/sse2-long-gen.json",
        include_str!("../../../research/google-generative-ai-probe/raw/sse2-long-gen.json"),
    ),
    (
        "research/google-generative-ai-probe/raw/sse2-stream-ok.json",
        include_str!("../../../research/google-generative-ai-probe/raw/sse2-stream-ok.json"),
    ),
    (
        "research/google-generative-ai-probe/raw/sysrole-model-gemini-2.5-flash.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/sysrole-model-gemini-2.5-flash.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/sysrole-model-gemini-3.1-flash-lite.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/sysrole-model-gemini-3.1-flash-lite.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/sysrole-None-gemini-2.5-flash.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/sysrole-None-gemini-2.5-flash.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/sysrole-None-gemini-3.1-flash-lite.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/sysrole-None-gemini-3.1-flash-lite.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/sysrole-user-gemini-2.5-flash.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/sysrole-user-gemini-2.5-flash.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/sysrole-user-gemini-3.1-flash-lite.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/sysrole-user-gemini-3.1-flash-lite.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/sysrole2-model-gemini-2.5-flash-image.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/sysrole2-model-gemini-2.5-flash-image.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/sysrole2-None-gemini-2.5-flash-image.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/sysrole2-None-gemini-2.5-flash-image.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/sysrole2-user-gemini-2.5-flash-image.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/sysrole2-user-gemini-2.5-flash-image.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/thought-signature-missing.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/thought-signature-missing.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/val-allowed-names.json",
        include_str!("../../../research/google-generative-ai-probe/raw/val-allowed-names.json"),
    ),
    (
        "research/google-generative-ai-probe/raw/val-ANY-0.json",
        include_str!("../../../research/google-generative-ai-probe/raw/val-ANY-0.json"),
    ),
    (
        "research/google-generative-ai-probe/raw/val-ANY-1.json",
        include_str!("../../../research/google-generative-ai-probe/raw/val-ANY-1.json"),
    ),
    (
        "research/google-generative-ai-probe/raw/val-ANY-2.json",
        include_str!("../../../research/google-generative-ai-probe/raw/val-ANY-2.json"),
    ),
    (
        "research/google-generative-ai-probe/raw/val-ANY-3.json",
        include_str!("../../../research/google-generative-ai-probe/raw/val-ANY-3.json"),
    ),
    (
        "research/google-generative-ai-probe/raw/val-AUTO-0.json",
        include_str!("../../../research/google-generative-ai-probe/raw/val-AUTO-0.json"),
    ),
    (
        "research/google-generative-ai-probe/raw/val-AUTO-1.json",
        include_str!("../../../research/google-generative-ai-probe/raw/val-AUTO-1.json"),
    ),
    (
        "research/google-generative-ai-probe/raw/val-AUTO-2.json",
        include_str!("../../../research/google-generative-ai-probe/raw/val-AUTO-2.json"),
    ),
    (
        "research/google-generative-ai-probe/raw/val-AUTO-3.json",
        include_str!("../../../research/google-generative-ai-probe/raw/val-AUTO-3.json"),
    ),
    (
        "research/google-generative-ai-probe/raw/val-VALIDATED-0.json",
        include_str!("../../../research/google-generative-ai-probe/raw/val-VALIDATED-0.json"),
    ),
    (
        "research/google-generative-ai-probe/raw/val-VALIDATED-1.json",
        include_str!("../../../research/google-generative-ai-probe/raw/val-VALIDATED-1.json"),
    ),
    (
        "research/google-generative-ai-probe/raw/val-VALIDATED-2.json",
        include_str!("../../../research/google-generative-ai-probe/raw/val-VALIDATED-2.json"),
    ),
    (
        "research/google-generative-ai-probe/raw/val-VALIDATED-3.json",
        include_str!("../../../research/google-generative-ai-probe/raw/val-VALIDATED-3.json"),
    ),
    (
        "research/google-generative-ai-probe/raw/val2-AUTO-0.json",
        include_str!("../../../research/google-generative-ai-probe/raw/val2-AUTO-0.json"),
    ),
    (
        "research/google-generative-ai-probe/raw/val2-AUTO-1.json",
        include_str!("../../../research/google-generative-ai-probe/raw/val2-AUTO-1.json"),
    ),
    (
        "research/google-generative-ai-probe/raw/val2-AUTO-2.json",
        include_str!("../../../research/google-generative-ai-probe/raw/val2-AUTO-2.json"),
    ),
    (
        "research/google-generative-ai-probe/raw/val2-AUTO-3.json",
        include_str!("../../../research/google-generative-ai-probe/raw/val2-AUTO-3.json"),
    ),
    (
        "research/google-generative-ai-probe/raw/val2-AUTO-4.json",
        include_str!("../../../research/google-generative-ai-probe/raw/val2-AUTO-4.json"),
    ),
    (
        "research/google-generative-ai-probe/raw/val2-AUTO-5.json",
        include_str!("../../../research/google-generative-ai-probe/raw/val2-AUTO-5.json"),
    ),
    (
        "research/google-generative-ai-probe/raw/val2-VALIDATED-0.json",
        include_str!("../../../research/google-generative-ai-probe/raw/val2-VALIDATED-0.json"),
    ),
    (
        "research/google-generative-ai-probe/raw/val2-VALIDATED-1.json",
        include_str!("../../../research/google-generative-ai-probe/raw/val2-VALIDATED-1.json"),
    ),
    (
        "research/google-generative-ai-probe/raw/val2-VALIDATED-2.json",
        include_str!("../../../research/google-generative-ai-probe/raw/val2-VALIDATED-2.json"),
    ),
    (
        "research/google-generative-ai-probe/raw/val2-VALIDATED-3.json",
        include_str!("../../../research/google-generative-ai-probe/raw/val2-VALIDATED-3.json"),
    ),
    (
        "research/google-generative-ai-probe/raw/val2-VALIDATED-4.json",
        include_str!("../../../research/google-generative-ai-probe/raw/val2-VALIDATED-4.json"),
    ),
    (
        "research/google-generative-ai-probe/raw/val2-VALIDATED-5.json",
        include_str!("../../../research/google-generative-ai-probe/raw/val2-VALIDATED-5.json"),
    ),
    (
        "research/google-generative-ai-probe/raw/x2-avail-gemini-2.5-computer-use-preview-10-2025.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/x2-avail-gemini-2.5-computer-use-preview-10-2025.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/x2-avail-gemini-2.5-flash-image.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/x2-avail-gemini-2.5-flash-image.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/x2-avail-gemini-2.5-flash-preview-tts.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/x2-avail-gemini-2.5-flash-preview-tts.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/x2-avail-gemini-2.5-pro.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/x2-avail-gemini-2.5-pro.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/x2-flash-image-imgparts.json",
        include_str!(
            "../../../research/google-generative-ai-probe/raw/x2-flash-image-imgparts.json"
        ),
    ),
    (
        "research/google-generative-ai-probe/raw/x2-flash-image-tool.json",
        include_str!("../../../research/google-generative-ai-probe/raw/x2-flash-image-tool.json"),
    ),
    (
        "research/image-limits/raw/anthropic-flat-8000x6000.json",
        include_str!("../../../research/image-limits/raw/anthropic-flat-8000x6000.json"),
    ),
    (
        "research/image-limits/raw/anthropic-flat-9000x9000.json",
        include_str!("../../../research/image-limits/raw/anthropic-flat-9000x9000.json"),
    ),
    (
        "research/image-limits/raw/anthropic-noise-2400.json",
        include_str!("../../../research/image-limits/raw/anthropic-noise-2400.json"),
    ),
    (
        "research/image-limits/raw/anthropic-small-gif.json",
        include_str!("../../../research/image-limits/raw/anthropic-small-gif.json"),
    ),
    (
        "research/image-limits/raw/google-2.5-flash-lite-404.json",
        include_str!("../../../research/image-limits/raw/google-2.5-flash-lite-404.json"),
    ),
    (
        "research/image-limits/raw/google-flat-8000x6000.json",
        include_str!("../../../research/image-limits/raw/google-flat-8000x6000.json"),
    ),
    (
        "research/image-limits/raw/google-flat-9000x9000.json",
        include_str!("../../../research/image-limits/raw/google-flat-9000x9000.json"),
    ),
    (
        "research/image-limits/raw/google-noise-2400.json",
        include_str!("../../../research/image-limits/raw/google-noise-2400.json"),
    ),
    (
        "research/image-limits/raw/google-small-gif.json",
        include_str!("../../../research/image-limits/raw/google-small-gif.json"),
    ),
    (
        "research/image-limits/raw/openai-chat-flat-8000x6000.json",
        include_str!("../../../research/image-limits/raw/openai-chat-flat-8000x6000.json"),
    ),
    (
        "research/image-limits/raw/openai-chat-flat-9000x9000.json",
        include_str!("../../../research/image-limits/raw/openai-chat-flat-9000x9000.json"),
    ),
    (
        "research/image-limits/raw/openai-chat-noise-2400.json",
        include_str!("../../../research/image-limits/raw/openai-chat-noise-2400.json"),
    ),
    (
        "research/image-limits/raw/openai-flat-8000x6000.json",
        include_str!("../../../research/image-limits/raw/openai-flat-8000x6000.json"),
    ),
    (
        "research/image-limits/raw/openai-flat-9000x9000.json",
        include_str!("../../../research/image-limits/raw/openai-flat-9000x9000.json"),
    ),
    (
        "research/image-limits/raw/openai-noise-2400.json",
        include_str!("../../../research/image-limits/raw/openai-noise-2400.json"),
    ),
    (
        "research/image-limits/raw/openai-small-gif.json",
        include_str!("../../../research/image-limits/raw/openai-small-gif.json"),
    ),
    (
        "research/mcp-tool-names/raw/anthropic.json",
        include_str!("../../../research/mcp-tool-names/raw/anthropic.json"),
    ),
    (
        "research/mcp-tool-names/raw/codex.json",
        include_str!("../../../research/mcp-tool-names/raw/codex.json"),
    ),
    (
        "research/mcp-tool-names/raw/go-completions.json",
        include_str!("../../../research/mcp-tool-names/raw/go-completions.json"),
    ),
    (
        "research/mcp-tool-names/raw/google.json",
        include_str!("../../../research/mcp-tool-names/raw/google.json"),
    ),
    (
        "research/mcp-tool-names/raw/openai-responses.json",
        include_str!("../../../research/mcp-tool-names/raw/openai-responses.json"),
    ),
    (
        "research/mcp-tool-names/raw/zen-responses.json",
        include_str!("../../../research/mcp-tool-names/raw/zen-responses.json"),
    ),
    (
        "research/openai-completions-probe/raw/openai-cache_control-on-openai.json",
        include_str!(
            "../../../research/openai-completions-probe/raw/openai-cache_control-on-openai.json"
        ),
    ),
    (
        "research/openai-completions-probe/raw/openai-mt-max_completion_tokens.json",
        include_str!(
            "../../../research/openai-completions-probe/raw/openai-mt-max_completion_tokens.json"
        ),
    ),
    (
        "research/openai-completions-probe/raw/openai-mt-max_tokens.json",
        include_str!("../../../research/openai-completions-probe/raw/openai-mt-max_tokens.json"),
    ),
    (
        "research/openai-completions-probe/raw/openai-stream-include_usage.json",
        include_str!(
            "../../../research/openai-completions-probe/raw/openai-stream-include_usage.json"
        ),
    ),
    (
        "research/openai-completions-probe/raw/openai-stream-length.json",
        include_str!("../../../research/openai-completions-probe/raw/openai-stream-length.json"),
    ),
    (
        "research/openai-completions-probe/raw/openai-stream-no_include_usage.json",
        include_str!(
            "../../../research/openai-completions-probe/raw/openai-stream-no_include_usage.json"
        ),
    ),
    (
        "research/openai-completions-probe/raw/openai-stream-reasoning-low.json",
        include_str!(
            "../../../research/openai-completions-probe/raw/openai-stream-reasoning-low.json"
        ),
    ),
    (
        "research/openai-completions-probe/raw/openai-stream-tool.json",
        include_str!("../../../research/openai-completions-probe/raw/openai-stream-tool.json"),
    ),
    (
        "research/openai-completions-probe/raw/openrouter-anthropic-cache/last_message.json",
        include_str!(
            "../../../research/openai-completions-probe/raw/openrouter-anthropic-cache/last_message.json"
        ),
    ),
    (
        "research/openai-completions-probe/raw/openrouter-anthropic-cache/none.json",
        include_str!(
            "../../../research/openai-completions-probe/raw/openrouter-anthropic-cache/none.json"
        ),
    ),
    (
        "research/openai-completions-probe/raw/openrouter-anthropic-cache/system_ttl_1h.json",
        include_str!(
            "../../../research/openai-completions-probe/raw/openrouter-anthropic-cache/system_ttl_1h.json"
        ),
    ),
    (
        "research/openai-completions-probe/raw/openrouter-anthropic-cache/system.json",
        include_str!(
            "../../../research/openai-completions-probe/raw/openrouter-anthropic-cache/system.json"
        ),
    ),
    (
        "research/openai-completions-probe/raw/openrouter-anthropic-cache/tool.json",
        include_str!(
            "../../../research/openai-completions-probe/raw/openrouter-anthropic-cache/tool.json"
        ),
    ),
    (
        "research/openai-completions-probe/raw/openrouter-cc-bogus-type.json",
        include_str!(
            "../../../research/openai-completions-probe/raw/openrouter-cc-bogus-type.json"
        ),
    ),
    (
        "research/openai-completions-probe/raw/openrouter-cc-last-message.json",
        include_str!(
            "../../../research/openai-completions-probe/raw/openrouter-cc-last-message.json"
        ),
    ),
    (
        "research/openai-completions-probe/raw/openrouter-cc-last-tool-in-function.json",
        include_str!(
            "../../../research/openai-completions-probe/raw/openrouter-cc-last-tool-in-function.json"
        ),
    ),
    (
        "research/openai-completions-probe/raw/openrouter-cc-last-tool-top.json",
        include_str!(
            "../../../research/openai-completions-probe/raw/openrouter-cc-last-tool-top.json"
        ),
    ),
    (
        "research/openai-completions-probe/raw/openrouter-cc-system-ttl1h.json",
        include_str!(
            "../../../research/openai-completions-probe/raw/openrouter-cc-system-ttl1h.json"
        ),
    ),
    (
        "research/openai-completions-probe/raw/openrouter-cc-system-ttl5m.json",
        include_str!(
            "../../../research/openai-completions-probe/raw/openrouter-cc-system-ttl5m.json"
        ),
    ),
    (
        "research/openai-completions-probe/raw/openrouter-cc-system.json",
        include_str!("../../../research/openai-completions-probe/raw/openrouter-cc-system.json"),
    ),
    (
        "research/openai-completions-probe/raw/openrouter-effort-none-400.json",
        include_str!(
            "../../../research/openai-completions-probe/raw/openrouter-effort-none-400.json"
        ),
    ),
    (
        "research/openai-completions-probe/raw/openrouter-max_completion_tokens.json",
        include_str!(
            "../../../research/openai-completions-probe/raw/openrouter-max_completion_tokens.json"
        ),
    ),
    (
        "research/openai-completions-probe/raw/openrouter-max_tokens.json",
        include_str!("../../../research/openai-completions-probe/raw/openrouter-max_tokens.json"),
    ),
    (
        "research/openai-completions-probe/raw/openrouter-pinned-replay-none.json",
        include_str!(
            "../../../research/openai-completions-probe/raw/openrouter-pinned-replay-none.json"
        ),
    ),
    (
        "research/openai-completions-probe/raw/openrouter-pinned-replay-reasoning_content.json",
        include_str!(
            "../../../research/openai-completions-probe/raw/openrouter-pinned-replay-reasoning_content.json"
        ),
    ),
    (
        "research/openai-completions-probe/raw/openrouter-pinned-replay-reasoning_details.json",
        include_str!(
            "../../../research/openai-completions-probe/raw/openrouter-pinned-replay-reasoning_details.json"
        ),
    ),
    (
        "research/openai-completions-probe/raw/openrouter-pinned-replay-reasoning.json",
        include_str!(
            "../../../research/openai-completions-probe/raw/openrouter-pinned-replay-reasoning.json"
        ),
    ),
    (
        "research/openai-completions-probe/raw/openrouter-replay-none.json",
        include_str!("../../../research/openai-completions-probe/raw/openrouter-replay-none.json"),
    ),
    (
        "research/openai-completions-probe/raw/openrouter-replay-reasoning_content.json",
        include_str!(
            "../../../research/openai-completions-probe/raw/openrouter-replay-reasoning_content.json"
        ),
    ),
    (
        "research/openai-completions-probe/raw/openrouter-replay-reasoning_details.json",
        include_str!(
            "../../../research/openai-completions-probe/raw/openrouter-replay-reasoning_details.json"
        ),
    ),
    (
        "research/openai-completions-probe/raw/openrouter-replay-reasoning.json",
        include_str!(
            "../../../research/openai-completions-probe/raw/openrouter-replay-reasoning.json"
        ),
    ),
    (
        "research/openai-completions-probe/raw/openrouter-stream-include_usage.json",
        include_str!(
            "../../../research/openai-completions-probe/raw/openrouter-stream-include_usage.json"
        ),
    ),
    (
        "research/openai-completions-probe/raw/openrouter-stream-length.json",
        include_str!(
            "../../../research/openai-completions-probe/raw/openrouter-stream-length.json"
        ),
    ),
    (
        "research/openai-completions-probe/raw/openrouter-stream-no_include_usage.json",
        include_str!(
            "../../../research/openai-completions-probe/raw/openrouter-stream-no_include_usage.json"
        ),
    ),
    (
        "research/openai-completions-probe/raw/openrouter-stream-reasoning.json",
        include_str!(
            "../../../research/openai-completions-probe/raw/openrouter-stream-reasoning.json"
        ),
    ),
    (
        "research/openai-completions-probe/raw/openrouter-stream-tool.json",
        include_str!("../../../research/openai-completions-probe/raw/openrouter-stream-tool.json"),
    ),
    (
        "research/openai-responses-probe/raw/probe.json",
        include_str!("../../../research/openai-responses-probe/raw/probe.json"),
    ),
    (
        "research/opencode-probe/raw/go_protocols.json",
        include_str!("../../../research/opencode-probe/raw/go_protocols.json"),
    ),
    (
        "research/opencode-probe/raw/go_stream_tools.json",
        include_str!("../../../research/opencode-probe/raw/go_stream_tools.json"),
    ),
    (
        "research/opencode-probe/raw/models.json",
        include_str!("../../../research/opencode-probe/raw/models.json"),
    ),
    (
        "research/opencode-probe/raw/usage.json",
        include_str!("../../../research/opencode-probe/raw/usage.json"),
    ),
    (
        "research/opencode-probe/raw/zen.json",
        include_str!("../../../research/opencode-probe/raw/zen.json"),
    ),
    (
        "research/opencode-thinking/raw/go-deepseek-v4-flash-vision-exp.json",
        include_str!(
            "../../../research/opencode-thinking/raw/go-deepseek-v4-flash-vision-exp.json"
        ),
    ),
    (
        "research/opencode-thinking/raw/go-deepseek-v4-flash.json",
        include_str!("../../../research/opencode-thinking/raw/go-deepseek-v4-flash.json"),
    ),
    (
        "research/opencode-thinking/raw/go-deepseek-v4-pro.json",
        include_str!("../../../research/opencode-thinking/raw/go-deepseek-v4-pro.json"),
    ),
    (
        "research/opencode-thinking/raw/go-deepseek-v4.1-flash.json",
        include_str!("../../../research/opencode-thinking/raw/go-deepseek-v4.1-flash.json"),
    ),
    (
        "research/opencode-thinking/raw/go-glm-5.1.json",
        include_str!("../../../research/opencode-thinking/raw/go-glm-5.1.json"),
    ),
    (
        "research/opencode-thinking/raw/go-glm-5.2.json",
        include_str!("../../../research/opencode-thinking/raw/go-glm-5.2.json"),
    ),
    (
        "research/opencode-thinking/raw/go-glm-5.3-flash.json",
        include_str!("../../../research/opencode-thinking/raw/go-glm-5.3-flash.json"),
    ),
    (
        "research/opencode-thinking/raw/go-glm-5.3.json",
        include_str!("../../../research/opencode-thinking/raw/go-glm-5.3.json"),
    ),
    (
        "research/opencode-thinking/raw/go-glm-5.json",
        include_str!("../../../research/opencode-thinking/raw/go-glm-5.json"),
    ),
    (
        "research/opencode-thinking/raw/go-hy3.json",
        include_str!("../../../research/opencode-thinking/raw/go-hy3.json"),
    ),
    (
        "research/opencode-thinking/raw/go-hy4-preview.json",
        include_str!("../../../research/opencode-thinking/raw/go-hy4-preview.json"),
    ),
    (
        "research/opencode-thinking/raw/go-kimi-k2.5.json",
        include_str!("../../../research/opencode-thinking/raw/go-kimi-k2.5.json"),
    ),
    (
        "research/opencode-thinking/raw/go-kimi-k2.6.json",
        include_str!("../../../research/opencode-thinking/raw/go-kimi-k2.6.json"),
    ),
    (
        "research/opencode-thinking/raw/go-kimi-k2.7-code.json",
        include_str!("../../../research/opencode-thinking/raw/go-kimi-k2.7-code.json"),
    ),
    (
        "research/opencode-thinking/raw/go-longcat-2.0.json",
        include_str!("../../../research/opencode-thinking/raw/go-longcat-2.0.json"),
    ),
    (
        "research/opencode-thinking/raw/go-longcat-2.5-preview-free.json",
        include_str!("../../../research/opencode-thinking/raw/go-longcat-2.5-preview-free.json"),
    ),
    (
        "research/opencode-thinking/raw/go-mimo-v2.5-pro.json",
        include_str!("../../../research/opencode-thinking/raw/go-mimo-v2.5-pro.json"),
    ),
    (
        "research/opencode-thinking/raw/go-mimo-v2.5.json",
        include_str!("../../../research/opencode-thinking/raw/go-mimo-v2.5.json"),
    ),
    (
        "research/opencode-thinking/raw/go-mimo-v2.6-flash.json",
        include_str!("../../../research/opencode-thinking/raw/go-mimo-v2.6-flash.json"),
    ),
    (
        "research/opencode-thinking/raw/go-mimo-v2.6-pro.json",
        include_str!("../../../research/opencode-thinking/raw/go-mimo-v2.6-pro.json"),
    ),
    (
        "research/opencode-thinking/raw/go-minimax-m2.7-completions.json",
        include_str!("../../../research/opencode-thinking/raw/go-minimax-m2.7-completions.json"),
    ),
    (
        "research/opencode-thinking/raw/go-minimax-m2.7.json",
        include_str!("../../../research/opencode-thinking/raw/go-minimax-m2.7.json"),
    ),
    (
        "research/opencode-thinking/raw/go-minimax-m3.json",
        include_str!("../../../research/opencode-thinking/raw/go-minimax-m3.json"),
    ),
    (
        "research/opencode-thinking/raw/go-muse-spark-1.2-contributor.json",
        include_str!("../../../research/opencode-thinking/raw/go-muse-spark-1.2-contributor.json"),
    ),
    (
        "research/opencode-thinking/raw/go-qwen3.5-plus.json",
        include_str!("../../../research/opencode-thinking/raw/go-qwen3.5-plus.json"),
    ),
    (
        "research/opencode-thinking/raw/go-qwen3.6-plus.json",
        include_str!("../../../research/opencode-thinking/raw/go-qwen3.6-plus.json"),
    ),
    (
        "research/opencode-thinking/raw/go-qwen3.7-plus.json",
        include_str!("../../../research/opencode-thinking/raw/go-qwen3.7-plus.json"),
    ),
    (
        "research/opencode-thinking/raw/go-qwen3.8-flash.json",
        include_str!("../../../research/opencode-thinking/raw/go-qwen3.8-flash.json"),
    ),
    (
        "research/opencode-thinking/raw/go-qwen3.8-max.json",
        include_str!("../../../research/opencode-thinking/raw/go-qwen3.8-max.json"),
    ),
    (
        "research/opencode-thinking/raw/go-space-bunny.json",
        include_str!("../../../research/opencode-thinking/raw/go-space-bunny.json"),
    ),
    (
        "research/opencode-thinking/raw/go-step-5-preview-free.json",
        include_str!("../../../research/opencode-thinking/raw/go-step-5-preview-free.json"),
    ),
    (
        "research/opencode-thinking/raw/zen-big-pickle.json",
        include_str!("../../../research/opencode-thinking/raw/zen-big-pickle.json"),
    ),
    (
        "research/opencode-thinking/raw/zen-deepseek-v4-flash-vision-exp.json",
        include_str!(
            "../../../research/opencode-thinking/raw/zen-deepseek-v4-flash-vision-exp.json"
        ),
    ),
    (
        "research/opencode-thinking/raw/zen-deepseek-v4-flash.json",
        include_str!("../../../research/opencode-thinking/raw/zen-deepseek-v4-flash.json"),
    ),
    (
        "research/opencode-thinking/raw/zen-deepseek-v4-pro.json",
        include_str!("../../../research/opencode-thinking/raw/zen-deepseek-v4-pro.json"),
    ),
    (
        "research/opencode-thinking/raw/zen-deepseek-v4.1-flash.json",
        include_str!("../../../research/opencode-thinking/raw/zen-deepseek-v4.1-flash.json"),
    ),
    (
        "research/opencode-thinking/raw/zen-exo-free.json",
        include_str!("../../../research/opencode-thinking/raw/zen-exo-free.json"),
    ),
    (
        "research/opencode-thinking/raw/zen-gemini-3-flash.json",
        include_str!("../../../research/opencode-thinking/raw/zen-gemini-3-flash.json"),
    ),
    (
        "research/opencode-thinking/raw/zen-gemini-3.1-pro.json",
        include_str!("../../../research/opencode-thinking/raw/zen-gemini-3.1-pro.json"),
    ),
    (
        "research/opencode-thinking/raw/zen-glm-5.1.json",
        include_str!("../../../research/opencode-thinking/raw/zen-glm-5.1.json"),
    ),
    (
        "research/opencode-thinking/raw/zen-glm-5.2.json",
        include_str!("../../../research/opencode-thinking/raw/zen-glm-5.2.json"),
    ),
    (
        "research/opencode-thinking/raw/zen-glm-5.3-flash.json",
        include_str!("../../../research/opencode-thinking/raw/zen-glm-5.3-flash.json"),
    ),
    (
        "research/opencode-thinking/raw/zen-glm-5.3.json",
        include_str!("../../../research/opencode-thinking/raw/zen-glm-5.3.json"),
    ),
    (
        "research/opencode-thinking/raw/zen-glm-5.json",
        include_str!("../../../research/opencode-thinking/raw/zen-glm-5.json"),
    ),
    (
        "research/opencode-thinking/raw/zen-gpt-5-codex.json",
        include_str!("../../../research/opencode-thinking/raw/zen-gpt-5-codex.json"),
    ),
    (
        "research/opencode-thinking/raw/zen-gpt-5.1-codex-max.json",
        include_str!("../../../research/opencode-thinking/raw/zen-gpt-5.1-codex-max.json"),
    ),
    (
        "research/opencode-thinking/raw/zen-gpt-5.1-codex-mini.json",
        include_str!("../../../research/opencode-thinking/raw/zen-gpt-5.1-codex-mini.json"),
    ),
    (
        "research/opencode-thinking/raw/zen-gpt-5.1-codex.json",
        include_str!("../../../research/opencode-thinking/raw/zen-gpt-5.1-codex.json"),
    ),
    (
        "research/opencode-thinking/raw/zen-gpt-5.2-codex.json",
        include_str!("../../../research/opencode-thinking/raw/zen-gpt-5.2-codex.json"),
    ),
    (
        "research/opencode-thinking/raw/zen-grok-build-0.1.json",
        include_str!("../../../research/opencode-thinking/raw/zen-grok-build-0.1.json"),
    ),
    (
        "research/opencode-thinking/raw/zen-kimi-k2.5.json",
        include_str!("../../../research/opencode-thinking/raw/zen-kimi-k2.5.json"),
    ),
    (
        "research/opencode-thinking/raw/zen-kimi-k2.6.json",
        include_str!("../../../research/opencode-thinking/raw/zen-kimi-k2.6.json"),
    ),
    (
        "research/opencode-thinking/raw/zen-kimi-k2.7-code.json",
        include_str!("../../../research/opencode-thinking/raw/zen-kimi-k2.7-code.json"),
    ),
    (
        "research/opencode-thinking/raw/zen-ling-3.0-flash-fin-free.json",
        include_str!("../../../research/opencode-thinking/raw/zen-ling-3.0-flash-fin-free.json"),
    ),
    (
        "research/opencode-thinking/raw/zen-ling-3.1-flash-free.json",
        include_str!("../../../research/opencode-thinking/raw/zen-ling-3.1-flash-free.json"),
    ),
    (
        "research/opencode-thinking/raw/zen-longcat-2.5-preview-free.json",
        include_str!("../../../research/opencode-thinking/raw/zen-longcat-2.5-preview-free.json"),
    ),
    (
        "research/opencode-thinking/raw/zen-mimo-v2.6-flash-free.json",
        include_str!("../../../research/opencode-thinking/raw/zen-mimo-v2.6-flash-free.json"),
    ),
    (
        "research/opencode-thinking/raw/zen-minimax-m2.5.json",
        include_str!("../../../research/opencode-thinking/raw/zen-minimax-m2.5.json"),
    ),
    (
        "research/opencode-thinking/raw/zen-minimax-m2.7.json",
        include_str!("../../../research/opencode-thinking/raw/zen-minimax-m2.7.json"),
    ),
    (
        "research/opencode-thinking/raw/zen-minimax-m3.json",
        include_str!("../../../research/opencode-thinking/raw/zen-minimax-m3.json"),
    ),
    (
        "research/opencode-thinking/raw/zen-mistral-large-4.json",
        include_str!("../../../research/opencode-thinking/raw/zen-mistral-large-4.json"),
    ),
    (
        "research/opencode-thinking/raw/zen-muse-spark-1.2.json",
        include_str!("../../../research/opencode-thinking/raw/zen-muse-spark-1.2.json"),
    ),
    (
        "research/opencode-thinking/raw/zen-muse-spark-1.3-contributor-free.json",
        include_str!(
            "../../../research/opencode-thinking/raw/zen-muse-spark-1.3-contributor-free.json"
        ),
    ),
    (
        "research/opencode-thinking/raw/zen-nemotron-3-ultra-free.json",
        include_str!("../../../research/opencode-thinking/raw/zen-nemotron-3-ultra-free.json"),
    ),
    (
        "research/opencode-thinking/raw/zen-nemotron-3.5-lightning-free.json",
        include_str!(
            "../../../research/opencode-thinking/raw/zen-nemotron-3.5-lightning-free.json"
        ),
    ),
    (
        "research/opencode-thinking/raw/zen-qwen3.5-plus.json",
        include_str!("../../../research/opencode-thinking/raw/zen-qwen3.5-plus.json"),
    ),
    (
        "research/opencode-thinking/raw/zen-qwen3.6-plus.json",
        include_str!("../../../research/opencode-thinking/raw/zen-qwen3.6-plus.json"),
    ),
    (
        "research/opencode-thinking/raw/zen-qwen3.8-flash.json",
        include_str!("../../../research/opencode-thinking/raw/zen-qwen3.8-flash.json"),
    ),
    (
        "research/opencode-thinking/raw/zen-qwen3.8-max.json",
        include_str!("../../../research/opencode-thinking/raw/zen-qwen3.8-max.json"),
    ),
    (
        "research/opencode-thinking/raw/zen-space-bunny-free.json",
        include_str!("../../../research/opencode-thinking/raw/zen-space-bunny-free.json"),
    ),
    (
        "research/opencode-thinking/raw/zen-step-5-preview-free.json",
        include_str!("../../../research/opencode-thinking/raw/zen-step-5-preview-free.json"),
    ),
    (
        "research/opencode-zen-gemini-probe/raw/generate-gemini-3.5-flash-lite.json",
        include_str!(
            "../../../research/opencode-zen-gemini-probe/raw/generate-gemini-3.5-flash-lite.json"
        ),
    ),
    (
        "research/opencode-zen-gemini-probe/raw/generate-gemini-3.8-flash.json",
        include_str!(
            "../../../research/opencode-zen-gemini-probe/raw/generate-gemini-3.8-flash.json"
        ),
    ),
    (
        "research/openrouter-cost/raw/results.json",
        include_str!("../../../research/openrouter-cost/raw/results.json"),
    ),
    (
        "research/pdf-tool-results/raw/anthropic-bio-refusals/anthropic.after-result.scanned.1.json",
        include_str!(
            "../../../research/pdf-tool-results/raw/anthropic-bio-refusals/anthropic.after-result.scanned.1.json"
        ),
    ),
    (
        "research/pdf-tool-results/raw/anthropic-bio-refusals/anthropic.after-result.scanned.2.json",
        include_str!(
            "../../../research/pdf-tool-results/raw/anthropic-bio-refusals/anthropic.after-result.scanned.2.json"
        ),
    ),
    (
        "research/pdf-tool-results/raw/anthropic-bio-refusals/anthropic.after-result.text.1.json",
        include_str!(
            "../../../research/pdf-tool-results/raw/anthropic-bio-refusals/anthropic.after-result.text.1.json"
        ),
    ),
    (
        "research/pdf-tool-results/raw/anthropic-bio-refusals/anthropic.after-result.text.2.json",
        include_str!(
            "../../../research/pdf-tool-results/raw/anthropic-bio-refusals/anthropic.after-result.text.2.json"
        ),
    ),
    (
        "research/pdf-tool-results/raw/anthropic-bio-refusals/anthropic.in-result.scanned.1.json",
        include_str!(
            "../../../research/pdf-tool-results/raw/anthropic-bio-refusals/anthropic.in-result.scanned.1.json"
        ),
    ),
    (
        "research/pdf-tool-results/raw/anthropic-bio-refusals/anthropic.in-result.scanned.2.json",
        include_str!(
            "../../../research/pdf-tool-results/raw/anthropic-bio-refusals/anthropic.in-result.scanned.2.json"
        ),
    ),
    (
        "research/pdf-tool-results/raw/anthropic-bio-refusals/anthropic.in-result.text.1.json",
        include_str!(
            "../../../research/pdf-tool-results/raw/anthropic-bio-refusals/anthropic.in-result.text.1.json"
        ),
    ),
    (
        "research/pdf-tool-results/raw/anthropic-bio-refusals/anthropic.in-result.text.2.json",
        include_str!(
            "../../../research/pdf-tool-results/raw/anthropic-bio-refusals/anthropic.in-result.text.2.json"
        ),
    ),
    (
        "research/pdf-tool-results/raw/anthropic.after-result.scanned.1.json",
        include_str!(
            "../../../research/pdf-tool-results/raw/anthropic.after-result.scanned.1.json"
        ),
    ),
    (
        "research/pdf-tool-results/raw/anthropic.after-result.scanned.2.json",
        include_str!(
            "../../../research/pdf-tool-results/raw/anthropic.after-result.scanned.2.json"
        ),
    ),
    (
        "research/pdf-tool-results/raw/anthropic.after-result.text.1.json",
        include_str!("../../../research/pdf-tool-results/raw/anthropic.after-result.text.1.json"),
    ),
    (
        "research/pdf-tool-results/raw/anthropic.after-result.text.2.json",
        include_str!("../../../research/pdf-tool-results/raw/anthropic.after-result.text.2.json"),
    ),
    (
        "research/pdf-tool-results/raw/anthropic.in-result.scanned.1.json",
        include_str!("../../../research/pdf-tool-results/raw/anthropic.in-result.scanned.1.json"),
    ),
    (
        "research/pdf-tool-results/raw/anthropic.in-result.scanned.2.json",
        include_str!("../../../research/pdf-tool-results/raw/anthropic.in-result.scanned.2.json"),
    ),
    (
        "research/pdf-tool-results/raw/anthropic.in-result.text.1.json",
        include_str!("../../../research/pdf-tool-results/raw/anthropic.in-result.text.1.json"),
    ),
    (
        "research/pdf-tool-results/raw/anthropic.in-result.text.2.json",
        include_str!("../../../research/pdf-tool-results/raw/anthropic.in-result.text.2.json"),
    ),
    (
        "research/pdf-tool-results/raw/completions.after-result.scanned.1.json",
        include_str!(
            "../../../research/pdf-tool-results/raw/completions.after-result.scanned.1.json"
        ),
    ),
    (
        "research/pdf-tool-results/raw/completions.after-result.scanned.2.json",
        include_str!(
            "../../../research/pdf-tool-results/raw/completions.after-result.scanned.2.json"
        ),
    ),
    (
        "research/pdf-tool-results/raw/completions.after-result.text.1.json",
        include_str!("../../../research/pdf-tool-results/raw/completions.after-result.text.1.json"),
    ),
    (
        "research/pdf-tool-results/raw/completions.after-result.text.2.json",
        include_str!("../../../research/pdf-tool-results/raw/completions.after-result.text.2.json"),
    ),
    (
        "research/pdf-tool-results/raw/completions.image-after-result.scanned.1.json",
        include_str!(
            "../../../research/pdf-tool-results/raw/completions.image-after-result.scanned.1.json"
        ),
    ),
    (
        "research/pdf-tool-results/raw/completions.image-in-result.scanned.1.json",
        include_str!(
            "../../../research/pdf-tool-results/raw/completions.image-in-result.scanned.1.json"
        ),
    ),
    (
        "research/pdf-tool-results/raw/completions.image-in-result.scanned.2.json",
        include_str!(
            "../../../research/pdf-tool-results/raw/completions.image-in-result.scanned.2.json"
        ),
    ),
    (
        "research/pdf-tool-results/raw/completions.in-result.scanned.1.json",
        include_str!("../../../research/pdf-tool-results/raw/completions.in-result.scanned.1.json"),
    ),
    (
        "research/pdf-tool-results/raw/completions.in-result.text.1.json",
        include_str!("../../../research/pdf-tool-results/raw/completions.in-result.text.1.json"),
    ),
    (
        "research/pdf-tool-results/raw/completions.reasoning-effort-with-tools.1.json",
        include_str!(
            "../../../research/pdf-tool-results/raw/completions.reasoning-effort-with-tools.1.json"
        ),
    ),
    (
        "research/pdf-tool-results/raw/gemini.404-2.5-flash-lite.json",
        include_str!("../../../research/pdf-tool-results/raw/gemini.404-2.5-flash-lite.json"),
    ),
    (
        "research/pdf-tool-results/raw/gemini.after-result.scanned.1.json",
        include_str!("../../../research/pdf-tool-results/raw/gemini.after-result.scanned.1.json"),
    ),
    (
        "research/pdf-tool-results/raw/gemini.after-result.scanned.2.json",
        include_str!("../../../research/pdf-tool-results/raw/gemini.after-result.scanned.2.json"),
    ),
    (
        "research/pdf-tool-results/raw/gemini.after-result.text.1.json",
        include_str!("../../../research/pdf-tool-results/raw/gemini.after-result.text.1.json"),
    ),
    (
        "research/pdf-tool-results/raw/gemini.after-result.text.2.json",
        include_str!("../../../research/pdf-tool-results/raw/gemini.after-result.text.2.json"),
    ),
    (
        "research/pdf-tool-results/raw/gemini.cache-repeat.in-result-parts.text.1.json",
        include_str!(
            "../../../research/pdf-tool-results/raw/gemini.cache-repeat.in-result-parts.text.1.json"
        ),
    ),
    (
        "research/pdf-tool-results/raw/gemini.cache-repeat.in-result-parts.text.2.json",
        include_str!(
            "../../../research/pdf-tool-results/raw/gemini.cache-repeat.in-result-parts.text.2.json"
        ),
    ),
    (
        "research/pdf-tool-results/raw/gemini.cache-repeat.in-result-parts.text.3.json",
        include_str!(
            "../../../research/pdf-tool-results/raw/gemini.cache-repeat.in-result-parts.text.3.json"
        ),
    ),
    (
        "research/pdf-tool-results/raw/gemini.in-result-parts.scanned.1.json",
        include_str!(
            "../../../research/pdf-tool-results/raw/gemini.in-result-parts.scanned.1.json"
        ),
    ),
    (
        "research/pdf-tool-results/raw/gemini.in-result-parts.scanned.2.json",
        include_str!(
            "../../../research/pdf-tool-results/raw/gemini.in-result-parts.scanned.2.json"
        ),
    ),
    (
        "research/pdf-tool-results/raw/gemini.in-result-parts.text.1.json",
        include_str!("../../../research/pdf-tool-results/raw/gemini.in-result-parts.text.1.json"),
    ),
    (
        "research/pdf-tool-results/raw/gemini.in-result-parts.text.2.json",
        include_str!("../../../research/pdf-tool-results/raw/gemini.in-result-parts.text.2.json"),
    ),
    (
        "research/pdf-tool-results/raw/gemini.in-result-response-field.scanned.1.json",
        include_str!(
            "../../../research/pdf-tool-results/raw/gemini.in-result-response-field.scanned.1.json"
        ),
    ),
    (
        "research/pdf-tool-results/raw/gemini.in-result-response-field.scanned.2.json",
        include_str!(
            "../../../research/pdf-tool-results/raw/gemini.in-result-response-field.scanned.2.json"
        ),
    ),
    (
        "research/pdf-tool-results/raw/gemini.in-result-response-field.text.1.json",
        include_str!(
            "../../../research/pdf-tool-results/raw/gemini.in-result-response-field.text.1.json"
        ),
    ),
    (
        "research/pdf-tool-results/raw/gemini.in-result-response-field.text.2.json",
        include_str!(
            "../../../research/pdf-tool-results/raw/gemini.in-result-response-field.text.2.json"
        ),
    ),
    (
        "research/pdf-tool-results/raw/gemini.no-thought-signature.text.1.json",
        include_str!(
            "../../../research/pdf-tool-results/raw/gemini.no-thought-signature.text.1.json"
        ),
    ),
    (
        "research/pdf-tool-results/raw/responses.after-result.scanned.1.json",
        include_str!(
            "../../../research/pdf-tool-results/raw/responses.after-result.scanned.1.json"
        ),
    ),
    (
        "research/pdf-tool-results/raw/responses.after-result.scanned.2.json",
        include_str!(
            "../../../research/pdf-tool-results/raw/responses.after-result.scanned.2.json"
        ),
    ),
    (
        "research/pdf-tool-results/raw/responses.after-result.text.1.json",
        include_str!("../../../research/pdf-tool-results/raw/responses.after-result.text.1.json"),
    ),
    (
        "research/pdf-tool-results/raw/responses.after-result.text.2.json",
        include_str!("../../../research/pdf-tool-results/raw/responses.after-result.text.2.json"),
    ),
    (
        "research/pdf-tool-results/raw/responses.file_id-with-filename.1.json",
        include_str!(
            "../../../research/pdf-tool-results/raw/responses.file_id-with-filename.1.json"
        ),
    ),
    (
        "research/pdf-tool-results/raw/responses.in-result-file_data.scanned.1.json",
        include_str!(
            "../../../research/pdf-tool-results/raw/responses.in-result-file_data.scanned.1.json"
        ),
    ),
    (
        "research/pdf-tool-results/raw/responses.in-result-file_data.scanned.2.json",
        include_str!(
            "../../../research/pdf-tool-results/raw/responses.in-result-file_data.scanned.2.json"
        ),
    ),
    (
        "research/pdf-tool-results/raw/responses.in-result-file_data.text.1.json",
        include_str!(
            "../../../research/pdf-tool-results/raw/responses.in-result-file_data.text.1.json"
        ),
    ),
    (
        "research/pdf-tool-results/raw/responses.in-result-file_data.text.2.json",
        include_str!(
            "../../../research/pdf-tool-results/raw/responses.in-result-file_data.text.2.json"
        ),
    ),
    (
        "research/pdf-tool-results/raw/responses.in-result-file_id.scanned.1.json",
        include_str!(
            "../../../research/pdf-tool-results/raw/responses.in-result-file_id.scanned.1.json"
        ),
    ),
    (
        "research/pdf-tool-results/raw/responses.in-result-file_id.scanned.2.json",
        include_str!(
            "../../../research/pdf-tool-results/raw/responses.in-result-file_id.scanned.2.json"
        ),
    ),
    (
        "research/pdf-tool-results/raw/responses.in-result-file_id.text.1.json",
        include_str!(
            "../../../research/pdf-tool-results/raw/responses.in-result-file_id.text.1.json"
        ),
    ),
    (
        "research/pdf-tool-results/raw/responses.in-result-file_id.text.2.json",
        include_str!(
            "../../../research/pdf-tool-results/raw/responses.in-result-file_id.text.2.json"
        ),
    ),
    (
        "research/provider-errors/raw/anthropic-messages.unknown-model.json",
        include_str!("../../../research/provider-errors/raw/anthropic-messages.unknown-model.json"),
    ),
    (
        "research/provider-errors/raw/anthropic-messages.wrong-path.json",
        include_str!("../../../research/provider-errors/raw/anthropic-messages.wrong-path.json"),
    ),
    (
        "research/provider-errors/raw/muse-completions.bad-key.json",
        include_str!("../../../research/provider-errors/raw/muse-completions.bad-key.json"),
    ),
    (
        "research/provider-errors/raw/muse-completions.bad-schema-stream.json",
        include_str!(
            "../../../research/provider-errors/raw/muse-completions.bad-schema-stream.json"
        ),
    ),
    (
        "research/provider-errors/raw/muse-completions.bad-schema.json",
        include_str!("../../../research/provider-errors/raw/muse-completions.bad-schema.json"),
    ),
    (
        "research/provider-errors/raw/muse-completions.burst.json",
        include_str!("../../../research/provider-errors/raw/muse-completions.burst.json"),
    ),
    (
        "research/provider-errors/raw/muse-completions.max-tokens-huge.json",
        include_str!("../../../research/provider-errors/raw/muse-completions.max-tokens-huge.json"),
    ),
    (
        "research/provider-errors/raw/muse-completions.missing-key.json",
        include_str!("../../../research/provider-errors/raw/muse-completions.missing-key.json"),
    ),
    (
        "research/provider-errors/raw/muse-completions.ok-stream.json",
        include_str!("../../../research/provider-errors/raw/muse-completions.ok-stream.json"),
    ),
    (
        "research/provider-errors/raw/muse-completions.overflow-stream.json",
        include_str!("../../../research/provider-errors/raw/muse-completions.overflow-stream.json"),
    ),
    (
        "research/provider-errors/raw/muse-completions.overflow.json",
        include_str!("../../../research/provider-errors/raw/muse-completions.overflow.json"),
    ),
    (
        "research/provider-errors/raw/muse-completions.padded-8mb.json",
        include_str!("../../../research/provider-errors/raw/muse-completions.padded-8mb.json"),
    ),
    (
        "research/provider-errors/raw/muse-completions.tool-choice-required.json",
        include_str!(
            "../../../research/provider-errors/raw/muse-completions.tool-choice-required.json"
        ),
    ),
    (
        "research/provider-errors/raw/muse-completions.unknown-model.json",
        include_str!("../../../research/provider-errors/raw/muse-completions.unknown-model.json"),
    ),
    (
        "research/provider-errors/raw/muse-messages.bad-key.json",
        include_str!("../../../research/provider-errors/raw/muse-messages.bad-key.json"),
    ),
    (
        "research/provider-errors/raw/muse-messages.bad-schema-stream.json",
        include_str!("../../../research/provider-errors/raw/muse-messages.bad-schema-stream.json"),
    ),
    (
        "research/provider-errors/raw/muse-messages.bad-schema.json",
        include_str!("../../../research/provider-errors/raw/muse-messages.bad-schema.json"),
    ),
    (
        "research/provider-errors/raw/muse-messages.max-tokens-huge.json",
        include_str!("../../../research/provider-errors/raw/muse-messages.max-tokens-huge.json"),
    ),
    (
        "research/provider-errors/raw/muse-messages.missing-key.json",
        include_str!("../../../research/provider-errors/raw/muse-messages.missing-key.json"),
    ),
    (
        "research/provider-errors/raw/muse-messages.ok-stream.json",
        include_str!("../../../research/provider-errors/raw/muse-messages.ok-stream.json"),
    ),
    (
        "research/provider-errors/raw/muse-messages.overflow-stream.json",
        include_str!("../../../research/provider-errors/raw/muse-messages.overflow-stream.json"),
    ),
    (
        "research/provider-errors/raw/muse-messages.overflow.json",
        include_str!("../../../research/provider-errors/raw/muse-messages.overflow.json"),
    ),
    (
        "research/provider-errors/raw/muse-messages.tool-choice-required.json",
        include_str!(
            "../../../research/provider-errors/raw/muse-messages.tool-choice-required.json"
        ),
    ),
    (
        "research/provider-errors/raw/muse-messages.unknown-model.json",
        include_str!("../../../research/provider-errors/raw/muse-messages.unknown-model.json"),
    ),
    (
        "research/provider-errors/raw/muse-responses.bad-key.json",
        include_str!("../../../research/provider-errors/raw/muse-responses.bad-key.json"),
    ),
    (
        "research/provider-errors/raw/muse-responses.bad-schema-stream.json",
        include_str!("../../../research/provider-errors/raw/muse-responses.bad-schema-stream.json"),
    ),
    (
        "research/provider-errors/raw/muse-responses.bad-schema.json",
        include_str!("../../../research/provider-errors/raw/muse-responses.bad-schema.json"),
    ),
    (
        "research/provider-errors/raw/muse-responses.max-tokens-huge.json",
        include_str!("../../../research/provider-errors/raw/muse-responses.max-tokens-huge.json"),
    ),
    (
        "research/provider-errors/raw/muse-responses.missing-key.json",
        include_str!("../../../research/provider-errors/raw/muse-responses.missing-key.json"),
    ),
    (
        "research/provider-errors/raw/muse-responses.ok-stream.json",
        include_str!("../../../research/provider-errors/raw/muse-responses.ok-stream.json"),
    ),
    (
        "research/provider-errors/raw/muse-responses.overflow-stream.json",
        include_str!("../../../research/provider-errors/raw/muse-responses.overflow-stream.json"),
    ),
    (
        "research/provider-errors/raw/muse-responses.overflow.json",
        include_str!("../../../research/provider-errors/raw/muse-responses.overflow.json"),
    ),
    (
        "research/provider-errors/raw/muse-responses.tool-choice-required.json",
        include_str!(
            "../../../research/provider-errors/raw/muse-responses.tool-choice-required.json"
        ),
    ),
    (
        "research/provider-errors/raw/muse-responses.unknown-model.json",
        include_str!("../../../research/provider-errors/raw/muse-responses.unknown-model.json"),
    ),
    (
        "research/provider-errors/raw/openai-completions.wrong-path.json",
        include_str!("../../../research/provider-errors/raw/openai-completions.wrong-path.json"),
    ),
    (
        "research/provider-errors/raw/or-completions-haiku.bad-key.json",
        include_str!("../../../research/provider-errors/raw/or-completions-haiku.bad-key.json"),
    ),
    (
        "research/provider-errors/raw/or-completions-haiku.bad-schema-stream.json",
        include_str!(
            "../../../research/provider-errors/raw/or-completions-haiku.bad-schema-stream.json"
        ),
    ),
    (
        "research/provider-errors/raw/or-completions-haiku.bad-schema.json",
        include_str!("../../../research/provider-errors/raw/or-completions-haiku.bad-schema.json"),
    ),
    (
        "research/provider-errors/raw/or-completions-haiku.max-tokens-huge.json",
        include_str!(
            "../../../research/provider-errors/raw/or-completions-haiku.max-tokens-huge.json"
        ),
    ),
    (
        "research/provider-errors/raw/or-completions-haiku.missing-key.json",
        include_str!("../../../research/provider-errors/raw/or-completions-haiku.missing-key.json"),
    ),
    (
        "research/provider-errors/raw/or-completions-haiku.ok-stream.json",
        include_str!("../../../research/provider-errors/raw/or-completions-haiku.ok-stream.json"),
    ),
    (
        "research/provider-errors/raw/or-completions-haiku.overflow-dense-amazon-bedrock-stream.json",
        include_str!(
            "../../../research/provider-errors/raw/or-completions-haiku.overflow-dense-amazon-bedrock-stream.json"
        ),
    ),
    (
        "research/provider-errors/raw/or-completions-haiku.overflow-dense-amazon-bedrock.json",
        include_str!(
            "../../../research/provider-errors/raw/or-completions-haiku.overflow-dense-amazon-bedrock.json"
        ),
    ),
    (
        "research/provider-errors/raw/or-completions-haiku.overflow-dense-anthropic-stream.json",
        include_str!(
            "../../../research/provider-errors/raw/or-completions-haiku.overflow-dense-anthropic-stream.json"
        ),
    ),
    (
        "research/provider-errors/raw/or-completions-haiku.overflow-dense-anthropic.json",
        include_str!(
            "../../../research/provider-errors/raw/or-completions-haiku.overflow-dense-anthropic.json"
        ),
    ),
    (
        "research/provider-errors/raw/or-completions-haiku.overflow-stream.json",
        include_str!(
            "../../../research/provider-errors/raw/or-completions-haiku.overflow-stream.json"
        ),
    ),
    (
        "research/provider-errors/raw/or-completions-haiku.overflow.json",
        include_str!("../../../research/provider-errors/raw/or-completions-haiku.overflow.json"),
    ),
    (
        "research/provider-errors/raw/or-completions-haiku.tool-choice-required.json",
        include_str!(
            "../../../research/provider-errors/raw/or-completions-haiku.tool-choice-required.json"
        ),
    ),
    (
        "research/provider-errors/raw/or-completions-haiku.unknown-model.json",
        include_str!(
            "../../../research/provider-errors/raw/or-completions-haiku.unknown-model.json"
        ),
    ),
    (
        "research/provider-errors/raw/or-completions-luna.bad-key.json",
        include_str!("../../../research/provider-errors/raw/or-completions-luna.bad-key.json"),
    ),
    (
        "research/provider-errors/raw/or-completions-luna.bad-schema-stream.json",
        include_str!(
            "../../../research/provider-errors/raw/or-completions-luna.bad-schema-stream.json"
        ),
    ),
    (
        "research/provider-errors/raw/or-completions-luna.bad-schema.json",
        include_str!("../../../research/provider-errors/raw/or-completions-luna.bad-schema.json"),
    ),
    (
        "research/provider-errors/raw/or-completions-luna.max-tokens-huge.json",
        include_str!(
            "../../../research/provider-errors/raw/or-completions-luna.max-tokens-huge.json"
        ),
    ),
    (
        "research/provider-errors/raw/or-completions-luna.missing-key.json",
        include_str!("../../../research/provider-errors/raw/or-completions-luna.missing-key.json"),
    ),
    (
        "research/provider-errors/raw/or-completions-luna.ok-stream.json",
        include_str!("../../../research/provider-errors/raw/or-completions-luna.ok-stream.json"),
    ),
    (
        "research/provider-errors/raw/or-completions-luna.overflow-dense-stream.json",
        include_str!(
            "../../../research/provider-errors/raw/or-completions-luna.overflow-dense-stream.json"
        ),
    ),
    (
        "research/provider-errors/raw/or-completions-luna.overflow-dense.json",
        include_str!(
            "../../../research/provider-errors/raw/or-completions-luna.overflow-dense.json"
        ),
    ),
    (
        "research/provider-errors/raw/or-completions-luna.overflow-stream.json",
        include_str!(
            "../../../research/provider-errors/raw/or-completions-luna.overflow-stream.json"
        ),
    ),
    (
        "research/provider-errors/raw/or-completions-luna.overflow.json",
        include_str!("../../../research/provider-errors/raw/or-completions-luna.overflow.json"),
    ),
    (
        "research/provider-errors/raw/or-completions-luna.tool-choice-required.json",
        include_str!(
            "../../../research/provider-errors/raw/or-completions-luna.tool-choice-required.json"
        ),
    ),
    (
        "research/provider-errors/raw/or-completions-luna.unknown-model.json",
        include_str!(
            "../../../research/provider-errors/raw/or-completions-luna.unknown-model.json"
        ),
    ),
    (
        "research/provider-errors/raw/or-completions.wrong-path.json",
        include_str!("../../../research/provider-errors/raw/or-completions.wrong-path.json"),
    ),
    (
        "research/provider-errors/raw/or-messages-haiku.bad-key.json",
        include_str!("../../../research/provider-errors/raw/or-messages-haiku.bad-key.json"),
    ),
    (
        "research/provider-errors/raw/or-messages-haiku.bad-schema-stream.json",
        include_str!(
            "../../../research/provider-errors/raw/or-messages-haiku.bad-schema-stream.json"
        ),
    ),
    (
        "research/provider-errors/raw/or-messages-haiku.bad-schema.json",
        include_str!("../../../research/provider-errors/raw/or-messages-haiku.bad-schema.json"),
    ),
    (
        "research/provider-errors/raw/or-messages-haiku.max-tokens-huge.json",
        include_str!(
            "../../../research/provider-errors/raw/or-messages-haiku.max-tokens-huge.json"
        ),
    ),
    (
        "research/provider-errors/raw/or-messages-haiku.missing-key.json",
        include_str!("../../../research/provider-errors/raw/or-messages-haiku.missing-key.json"),
    ),
    (
        "research/provider-errors/raw/or-messages-haiku.ok-stream.json",
        include_str!("../../../research/provider-errors/raw/or-messages-haiku.ok-stream.json"),
    ),
    (
        "research/provider-errors/raw/or-messages-haiku.overflow-dense-amazon-bedrock.json",
        include_str!(
            "../../../research/provider-errors/raw/or-messages-haiku.overflow-dense-amazon-bedrock.json"
        ),
    ),
    (
        "research/provider-errors/raw/or-messages-haiku.overflow-dense-anthropic.json",
        include_str!(
            "../../../research/provider-errors/raw/or-messages-haiku.overflow-dense-anthropic.json"
        ),
    ),
    (
        "research/provider-errors/raw/or-messages-haiku.overflow-stream.json",
        include_str!(
            "../../../research/provider-errors/raw/or-messages-haiku.overflow-stream.json"
        ),
    ),
    (
        "research/provider-errors/raw/or-messages-haiku.overflow.json",
        include_str!("../../../research/provider-errors/raw/or-messages-haiku.overflow.json"),
    ),
    (
        "research/provider-errors/raw/or-messages-haiku.tool-choice-required.json",
        include_str!(
            "../../../research/provider-errors/raw/or-messages-haiku.tool-choice-required.json"
        ),
    ),
    (
        "research/provider-errors/raw/or-messages-haiku.unknown-model.json",
        include_str!("../../../research/provider-errors/raw/or-messages-haiku.unknown-model.json"),
    ),
    (
        "research/provider-errors/raw/or-responses-luna.bad-key.json",
        include_str!("../../../research/provider-errors/raw/or-responses-luna.bad-key.json"),
    ),
    (
        "research/provider-errors/raw/or-responses-luna.bad-schema-stream.json",
        include_str!(
            "../../../research/provider-errors/raw/or-responses-luna.bad-schema-stream.json"
        ),
    ),
    (
        "research/provider-errors/raw/or-responses-luna.bad-schema.json",
        include_str!("../../../research/provider-errors/raw/or-responses-luna.bad-schema.json"),
    ),
    (
        "research/provider-errors/raw/or-responses-luna.max-tokens-huge.json",
        include_str!(
            "../../../research/provider-errors/raw/or-responses-luna.max-tokens-huge.json"
        ),
    ),
    (
        "research/provider-errors/raw/or-responses-luna.missing-key.json",
        include_str!("../../../research/provider-errors/raw/or-responses-luna.missing-key.json"),
    ),
    (
        "research/provider-errors/raw/or-responses-luna.ok-stream.json",
        include_str!("../../../research/provider-errors/raw/or-responses-luna.ok-stream.json"),
    ),
    (
        "research/provider-errors/raw/or-responses-luna.overflow-dense-stream.json",
        include_str!(
            "../../../research/provider-errors/raw/or-responses-luna.overflow-dense-stream.json"
        ),
    ),
    (
        "research/provider-errors/raw/or-responses-luna.overflow-dense.json",
        include_str!("../../../research/provider-errors/raw/or-responses-luna.overflow-dense.json"),
    ),
    (
        "research/provider-errors/raw/or-responses-luna.overflow-stream.json",
        include_str!(
            "../../../research/provider-errors/raw/or-responses-luna.overflow-stream.json"
        ),
    ),
    (
        "research/provider-errors/raw/or-responses-luna.overflow.json",
        include_str!("../../../research/provider-errors/raw/or-responses-luna.overflow.json"),
    ),
    (
        "research/provider-errors/raw/or-responses-luna.tool-choice-required.json",
        include_str!(
            "../../../research/provider-errors/raw/or-responses-luna.tool-choice-required.json"
        ),
    ),
    (
        "research/provider-errors/raw/or-responses-luna.unknown-model.json",
        include_str!("../../../research/provider-errors/raw/or-responses-luna.unknown-model.json"),
    ),
    (
        "research/retry-signals/raw/anthropic.bad-key.json",
        include_str!("../../../research/retry-signals/raw/anthropic.bad-key.json"),
    ),
    (
        "research/retry-signals/raw/anthropic.count-tokens-burst-first-429.json",
        include_str!(
            "../../../research/retry-signals/raw/anthropic.count-tokens-burst-first-429.json"
        ),
    ),
    (
        "research/retry-signals/raw/anthropic.count-tokens-burst.json",
        include_str!("../../../research/retry-signals/raw/anthropic.count-tokens-burst.json"),
    ),
    (
        "research/retry-signals/raw/anthropic.count-tokens-ok.json",
        include_str!("../../../research/retry-signals/raw/anthropic.count-tokens-ok.json"),
    ),
    (
        "research/retry-signals/raw/anthropic.malformed-missing-messages.json",
        include_str!(
            "../../../research/retry-signals/raw/anthropic.malformed-missing-messages.json"
        ),
    ),
    (
        "research/retry-signals/raw/anthropic.malformed-not-json.json",
        include_str!("../../../research/retry-signals/raw/anthropic.malformed-not-json.json"),
    ),
    (
        "research/retry-signals/raw/anthropic.max-tokens-huge.json",
        include_str!("../../../research/retry-signals/raw/anthropic.max-tokens-huge.json"),
    ),
    (
        "research/retry-signals/raw/anthropic.ok.json",
        include_str!("../../../research/retry-signals/raw/anthropic.ok.json"),
    ),
    (
        "research/retry-signals/raw/anthropic.unknown-model.json",
        include_str!("../../../research/retry-signals/raw/anthropic.unknown-model.json"),
    ),
    (
        "research/retry-signals/raw/anthropic.wrong-method.json",
        include_str!("../../../research/retry-signals/raw/anthropic.wrong-method.json"),
    ),
    (
        "research/retry-signals/raw/anthropic.wrong-path.json",
        include_str!("../../../research/retry-signals/raw/anthropic.wrong-path.json"),
    ),
    (
        "research/retry-signals/raw/codex.bad-token.json",
        include_str!("../../../research/retry-signals/raw/codex.bad-token.json"),
    ),
    (
        "research/retry-signals/raw/codex.malformed-missing-input.json",
        include_str!("../../../research/retry-signals/raw/codex.malformed-missing-input.json"),
    ),
    (
        "research/retry-signals/raw/codex.ok.json",
        include_str!("../../../research/retry-signals/raw/codex.ok.json"),
    ),
    (
        "research/retry-signals/raw/codex.unknown-model.json",
        include_str!("../../../research/retry-signals/raw/codex.unknown-model.json"),
    ),
    (
        "research/retry-signals/raw/codex.unsupported-verb-trace.json",
        include_str!("../../../research/retry-signals/raw/codex.unsupported-verb-trace.json"),
    ),
    (
        "research/retry-signals/raw/codex.wrong-method-get.json",
        include_str!("../../../research/retry-signals/raw/codex.wrong-method-get.json"),
    ),
    (
        "research/retry-signals/raw/codex.wrong-method-patch.json",
        include_str!("../../../research/retry-signals/raw/codex.wrong-method-patch.json"),
    ),
    (
        "research/retry-signals/raw/codex.wrong-method-put.json",
        include_str!("../../../research/retry-signals/raw/codex.wrong-method-put.json"),
    ),
    (
        "research/retry-signals/raw/codex.wrong-path.json",
        include_str!("../../../research/retry-signals/raw/codex.wrong-path.json"),
    ),
    (
        "research/retry-signals/raw/gemini.bad-key.json",
        include_str!("../../../research/retry-signals/raw/gemini.bad-key.json"),
    ),
    (
        "research/retry-signals/raw/gemini.count-tokens-burst.json",
        include_str!("../../../research/retry-signals/raw/gemini.count-tokens-burst.json"),
    ),
    (
        "research/retry-signals/raw/gemini.count-tokens-ok.json",
        include_str!("../../../research/retry-signals/raw/gemini.count-tokens-ok.json"),
    ),
    (
        "research/retry-signals/raw/gemini.malformed-bad-field.json",
        include_str!("../../../research/retry-signals/raw/gemini.malformed-bad-field.json"),
    ),
    (
        "research/retry-signals/raw/gemini.malformed-not-json.json",
        include_str!("../../../research/retry-signals/raw/gemini.malformed-not-json.json"),
    ),
    (
        "research/retry-signals/raw/gemini.max-tokens-huge.json",
        include_str!("../../../research/retry-signals/raw/gemini.max-tokens-huge.json"),
    ),
    (
        "research/retry-signals/raw/gemini.model-burst.json",
        include_str!("../../../research/retry-signals/raw/gemini.model-burst.json"),
    ),
    (
        "research/retry-signals/raw/gemini.ok.json",
        include_str!("../../../research/retry-signals/raw/gemini.ok.json"),
    ),
    (
        "research/retry-signals/raw/gemini.unknown-model.json",
        include_str!("../../../research/retry-signals/raw/gemini.unknown-model.json"),
    ),
    (
        "research/retry-signals/raw/gemini.wrong-method.json",
        include_str!("../../../research/retry-signals/raw/gemini.wrong-method.json"),
    ),
    (
        "research/retry-signals/raw/gemini.wrong-path.json",
        include_str!("../../../research/retry-signals/raw/gemini.wrong-path.json"),
    ),
    (
        "research/retry-signals/raw/muse.bad-key.json",
        include_str!("../../../research/retry-signals/raw/muse.bad-key.json"),
    ),
    (
        "research/retry-signals/raw/muse.malformed-missing-messages.json",
        include_str!("../../../research/retry-signals/raw/muse.malformed-missing-messages.json"),
    ),
    (
        "research/retry-signals/raw/muse.max-tokens-huge-repeat2.json",
        include_str!("../../../research/retry-signals/raw/muse.max-tokens-huge-repeat2.json"),
    ),
    (
        "research/retry-signals/raw/muse.max-tokens-huge-repeat3.json",
        include_str!("../../../research/retry-signals/raw/muse.max-tokens-huge-repeat3.json"),
    ),
    (
        "research/retry-signals/raw/muse.max-tokens-huge.json",
        include_str!("../../../research/retry-signals/raw/muse.max-tokens-huge.json"),
    ),
    (
        "research/retry-signals/raw/muse.ok.json",
        include_str!("../../../research/retry-signals/raw/muse.ok.json"),
    ),
    (
        "research/retry-signals/raw/muse.unknown-model.json",
        include_str!("../../../research/retry-signals/raw/muse.unknown-model.json"),
    ),
    (
        "research/retry-signals/raw/muse.wrong-method.json",
        include_str!("../../../research/retry-signals/raw/muse.wrong-method.json"),
    ),
    (
        "research/retry-signals/raw/muse.wrong-path.json",
        include_str!("../../../research/retry-signals/raw/muse.wrong-path.json"),
    ),
    (
        "research/retry-signals/raw/openai.bad-key.json",
        include_str!("../../../research/retry-signals/raw/openai.bad-key.json"),
    ),
    (
        "research/retry-signals/raw/openai.malformed-missing-input.json",
        include_str!("../../../research/retry-signals/raw/openai.malformed-missing-input.json"),
    ),
    (
        "research/retry-signals/raw/openai.malformed-not-json.json",
        include_str!("../../../research/retry-signals/raw/openai.malformed-not-json.json"),
    ),
    (
        "research/retry-signals/raw/openai.max-tokens-huge.json",
        include_str!("../../../research/retry-signals/raw/openai.max-tokens-huge.json"),
    ),
    (
        "research/retry-signals/raw/openai.ok.json",
        include_str!("../../../research/retry-signals/raw/openai.ok.json"),
    ),
    (
        "research/retry-signals/raw/openai.unknown-model.json",
        include_str!("../../../research/retry-signals/raw/openai.unknown-model.json"),
    ),
    (
        "research/retry-signals/raw/openai.wrong-method.json",
        include_str!("../../../research/retry-signals/raw/openai.wrong-method.json"),
    ),
    (
        "research/retry-signals/raw/openai.wrong-path.json",
        include_str!("../../../research/retry-signals/raw/openai.wrong-path.json"),
    ),
    (
        "research/retry-signals/raw/openrouter.bad-key.json",
        include_str!("../../../research/retry-signals/raw/openrouter.bad-key.json"),
    ),
    (
        "research/retry-signals/raw/openrouter.malformed-missing-messages.json",
        include_str!(
            "../../../research/retry-signals/raw/openrouter.malformed-missing-messages.json"
        ),
    ),
    (
        "research/retry-signals/raw/openrouter.max-tokens-huge.json",
        include_str!("../../../research/retry-signals/raw/openrouter.max-tokens-huge.json"),
    ),
    (
        "research/retry-signals/raw/openrouter.ok.json",
        include_str!("../../../research/retry-signals/raw/openrouter.ok.json"),
    ),
    (
        "research/retry-signals/raw/openrouter.unknown-model.json",
        include_str!("../../../research/retry-signals/raw/openrouter.unknown-model.json"),
    ),
    (
        "research/retry-signals/raw/openrouter.wrong-method.json",
        include_str!("../../../research/retry-signals/raw/openrouter.wrong-method.json"),
    ),
    (
        "research/retry-signals/raw/openrouter.wrong-path.json",
        include_str!("../../../research/retry-signals/raw/openrouter.wrong-path.json"),
    ),
];

/// The text of a recorded reply under `research/`, compiled in: reading
/// it at run time would read the repository at test time (`docs/ci.md`,
/// "Selection").
fn recorded_case(path: &str) -> &'static str {
    match path {
        "provider-errors/documented/anthropic.billing-402.json" => {
            include_str!("../../../research/provider-errors/documented/anthropic.billing-402.json")
        }
        "provider-errors/documented/anthropic.spend-cap-429.json" => include_str!(
            "../../../research/provider-errors/documented/anthropic.spend-cap-429.json"
        ),
        "provider-errors/documented/anthropic.spend-limit-400.json" => include_str!(
            "../../../research/provider-errors/documented/anthropic.spend-limit-400.json"
        ),
        "provider-errors/documented/anthropic.workspace-spend-limit-400.json" => include_str!(
            "../../../research/provider-errors/documented/anthropic.workspace-spend-limit-400.json"
        ),
        "provider-errors/documented/gemini.billing-disabled-403.json" => include_str!(
            "../../../research/provider-errors/documented/gemini.billing-disabled-403.json"
        ),
        "provider-errors/documented/gemini.prepay-depleted-402.json" => include_str!(
            "../../../research/provider-errors/documented/gemini.prepay-depleted-402.json"
        ),
        "provider-errors/documented/gemini.quota-failure-429.json" => include_str!(
            "../../../research/provider-errors/documented/gemini.quota-failure-429.json"
        ),
        "provider-errors/documented/gemini.rate-limit-exceeded-429.json" => include_str!(
            "../../../research/provider-errors/documented/gemini.rate-limit-exceeded-429.json"
        ),
        "provider-errors/documented/gemini.resource-quota-exceeded-429.json" => include_str!(
            "../../../research/provider-errors/documented/gemini.resource-quota-exceeded-429.json"
        ),
        "provider-errors/documented/openai.credit-balance-exhausted-429.json" => include_str!(
            "../../../research/provider-errors/documented/openai.credit-balance-exhausted-429.json"
        ),
        "provider-errors/documented/openai.insufficient-quota-429.json" => include_str!(
            "../../../research/provider-errors/documented/openai.insufficient-quota-429.json"
        ),
        "provider-errors/documented/openai.insufficient-quota-code-429.json" => include_str!(
            "../../../research/provider-errors/documented/openai.insufficient-quota-code-429.json"
        ),
        "provider-errors/documented/openai.organization-spend-limit-429.json" => include_str!(
            "../../../research/provider-errors/documented/openai.organization-spend-limit-429.json"
        ),
        "provider-errors/documented/openai.organization-usage-limit-429.json" => include_str!(
            "../../../research/provider-errors/documented/openai.organization-usage-limit-429.json"
        ),
        "provider-errors/documented/openai.project-spend-limit-429.json" => include_str!(
            "../../../research/provider-errors/documented/openai.project-spend-limit-429.json"
        ),
        "provider-errors/documented/openai.slow-down-429.json" => {
            include_str!("../../../research/provider-errors/documented/openai.slow-down-429.json")
        }
        "provider-errors/documented/openrouter.in-flight-budget-402.json" => include_str!(
            "../../../research/provider-errors/documented/openrouter.in-flight-budget-402.json"
        ),
        "provider-errors/documented/openrouter.insufficient-credits-402.json" => include_str!(
            "../../../research/provider-errors/documented/openrouter.insufficient-credits-402.json"
        ),
        _ => RECORDED
            .iter()
            .copied()
            .find(|(name, _)| name.strip_prefix("research/") == Some(path))
            .map(|(_, text)| text)
            .unwrap_or_else(|| panic!("unknown recorded reply {path}")),
    }
}

/// The recorded reply: its status, its body, and the `Retry-After` header
/// when it parses as seconds.
fn recorded(path: &str) -> Error {
    let value: Value = serde_json::from_str(recorded_case(path)).unwrap();
    Error::Status {
        status: u16::try_from(value["status"].as_u64().unwrap()).unwrap(),
        body: value["body"].as_str().unwrap().to_owned(),
        retry_after: value
            .pointer("/headers/Retry-After")
            .and_then(Value::as_str)
            .and_then(|wait| wait.parse::<f64>().ok()),
        should_retry: None,
        url: crate::error::target(value["url"].as_str().unwrap_or_default()),
        error_type: value["headers"].as_object().and_then(|headers| {
            headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("x-amzn-errortype"))
                .and_then(|(_, value)| value.as_str()?.split(':').next())
                .map(str::to_owned)
        }),
    }
}

/// The body a `documented/` status fixture holds.
fn fixture_body(path: &str) -> String {
    let value: Value = serde_json::from_str(recorded_case(path)).unwrap();
    value["body"].as_str().unwrap().to_owned()
}

/// The text of a `documented/` stream fixture, compiled in: reading it at
/// run time would read the repository at test time (`docs/ci.md`,
/// "Selection").
fn stream_case(case: &str) -> &'static str {
    match case {
        "anthropic.billing-error-event.json" => include_str!(
            "../../../research/provider-errors/documented/anthropic.billing-error-event.json"
        ),
        "anthropic.spend-cap-event.json" => include_str!(
            "../../../research/provider-errors/documented/anthropic.spend-cap-event.json"
        ),
        "gemini.billing-disabled-chunk.json" => include_str!(
            "../../../research/provider-errors/documented/gemini.billing-disabled-chunk.json"
        ),
        "gemini.payment-required-chunk.json" => include_str!(
            "../../../research/provider-errors/documented/gemini.payment-required-chunk.json"
        ),
        "gemini.resource-exhausted-chunk.json" => include_str!(
            "../../../research/provider-errors/documented/gemini.resource-exhausted-chunk.json"
        ),
        "gemini.resource-quota-exceeded-chunk.json" => include_str!(
            "../../../research/provider-errors/documented/gemini.resource-quota-exceeded-chunk.json"
        ),
        "openrouter.payment-required-chunk.json" => include_str!(
            "../../../research/provider-errors/documented/openrouter.payment-required-chunk.json"
        ),
        _ => panic!("unknown stream fixture {case}"),
    }
}

/// The stream bytes a `documented/` stream fixture holds in `raw_sse`.
fn stream_bytes(case: &str) -> Vec<u8> {
    let value: Value = serde_json::from_str(stream_case(case)).unwrap();
    value["raw_sse"].as_str().unwrap().as_bytes().to_owned()
}

fn status(status: u16, body: &str, retry_after: Option<f64>) -> ErrorCode {
    Error::Status {
        status,
        body: body.to_owned(),
        retry_after,
        should_retry: None,
        url: String::new(),
        error_type: None,
    }
    .code()
}

fn anthropic_stream(case: &str) -> Error {
    crate::anthropic_messages_decode::decode(&stream_bytes(case)[..], &mut |_| {}).unwrap_err()
}

fn completions_stream(case: &str) -> Error {
    crate::openai_completions::decode(
        &stream_bytes(case)[..],
        &CacheLifetime::FiveMinutes,
        &mut |_| {},
    )
    .unwrap_err()
}

fn gemini_stream(case: &str) -> Error {
    crate::google_generative_ai_decode::decode(&stream_bytes(case)[..], &mut |_| {}).unwrap_err()
}

#[test]
fn a_recorded_unknown_model_is_model_not_found() {
    for path in [
        "retry-signals/raw/muse.unknown-model.json",
        "retry-signals/raw/openai.unknown-model.json",
        "retry-signals/raw/anthropic.unknown-model.json",
        "retry-signals/raw/gemini.unknown-model.json",
        "retry-signals/raw/openrouter.unknown-model.json",
        "provider-errors/raw/or-completions-haiku.unknown-model.json",
        "provider-errors/raw/muse-completions.unknown-model.json",
        "provider-errors/raw/muse-responses.unknown-model.json",
        "provider-errors/raw/muse-messages.unknown-model.json",
        "provider-errors/raw/anthropic-messages.unknown-model.json",
    ] {
        assert_eq!(recorded(path).code(), ErrorCode::ModelNotFound, "{path}");
    }
}

#[test]
fn a_recorded_wrong_path_is_invalid_request() {
    for path in [
        "provider-errors/raw/anthropic-messages.wrong-path.json",
        "provider-errors/raw/openai-completions.wrong-path.json",
        "provider-errors/raw/or-completions.wrong-path.json",
        "retry-signals/raw/anthropic.wrong-path.json",
        "retry-signals/raw/muse.wrong-path.json",
    ] {
        assert_eq!(recorded(path).code(), ErrorCode::InvalidRequest, "{path}");
    }
}

#[test]
fn a_wrong_path_failure_names_the_host_and_path_not_the_query() {
    let message = recorded("provider-errors/raw/openai-completions.wrong-path.json")
        .failure("openai", crate::LoginAdvice::Advised, &Secrets::default())
        .message;
    assert_eq!(
        message,
        "openai answered HTTP 404 for api.openai.com/v1/chat/completionz. Check the base URL."
    );
    let wrong_key = Error::Status {
        status: 404,
        body: String::new(),
        retry_after: None,
        should_retry: None,
        url: crate::error::target("https://user:pw@host.test:8080/v1beta/m:gen?key=SECRET#frag"),
        error_type: None,
    };
    let message = wrong_key
        .failure("p", crate::LoginAdvice::Advised, &Secrets::default())
        .message;
    assert_eq!(
        message,
        "p answered HTTP 404 for host.test:8080/v1beta/m:gen. Check the base URL."
    );
    assert_eq!(crate::error::target("host.test"), "host.test");
    assert_eq!(crate::error::target("http://host.test?q=1"), "host.test");
}

#[test]
fn a_404_that_names_the_model_keeps_the_provider_wording() {
    let message = recorded("provider-errors/raw/muse-messages.unknown-model.json")
        .failure("muse", crate::LoginAdvice::Advised, &Secrets::default())
        .message;
    assert_eq!(message, "muse answered HTTP 404.");
    let other = Error::Status {
        status: 400,
        body: String::new(),
        retry_after: None,
        should_retry: None,
        url: "h/p".to_owned(),
        error_type: None,
    };
    assert_eq!(
        other
            .failure("p", crate::LoginAdvice::Advised, &Secrets::default())
            .message,
        "p answered HTTP 400."
    );
}

#[test]
fn only_the_unknown_model_shapes_are_model_not_found() {
    assert_eq!(status(404, "{}", None), ErrorCode::InvalidRequest);
    assert_eq!(status(404, "", None), ErrorCode::InvalidRequest);
    assert_eq!(
        status(
            404,
            r#"{"error":{"type":"not_found_error","message":"Not found"}}"#,
            None
        ),
        ErrorCode::InvalidRequest,
    );
    assert_eq!(
        status(
            404,
            r#"{"error":{"type":"not_found_error","message":"model: x"}}"#,
            None
        ),
        ErrorCode::ModelNotFound,
    );
    assert_eq!(status(400, "{}", None), ErrorCode::InvalidRequest);
    assert_eq!(
        status(400, r#"{"error":{"code":"model_not_found"}}"#, None),
        ErrorCode::ModelNotFound,
    );
    assert_eq!(
        status(
            400,
            r#"{"error":{"type":"not_found_error","message":"Model not found"}}"#,
            None
        ),
        ErrorCode::ModelNotFound,
    );
    for body in [
        r#"{"error":{"status":"NOT_FOUND","message":"models/x is not found"}}"#,
        r#"{"error":{"status":"NOT_FOUND","message":"the page is not found"}}"#,
        r#"{"error":{"status":"INVALID_ARGUMENT","message":"models/x is not found"}}"#,
    ] {
        let expected = if body.contains("NOT_FOUND\",\"message\":\"models/") {
            ErrorCode::ModelNotFound
        } else {
            ErrorCode::InvalidRequest
        };
        assert_eq!(status(404, body, None), expected, "{body}");
    }
    assert_eq!(
        status(400, r#"{"error":{"type":"not_found_error"}}"#, None),
        ErrorCode::InvalidRequest,
    );
    assert_eq!(
        status(
            400,
            r#"{"error":{"message":"x is not a valid model ID"}}"#,
            None
        ),
        ErrorCode::ModelNotFound,
    );
    assert_eq!(
        status(
            400,
            r#"{"error":{"message":"x is a valid model ID"}}"#,
            None
        ),
        ErrorCode::InvalidRequest,
    );
    assert_eq!(
        status(400, r#"{"error":{"type":"invalid_request_error"}}"#, None),
        ErrorCode::InvalidRequest,
    );
}

#[test]
fn a_documented_quota_or_billing_reply_is_quota_exceeded() {
    for case in [
        "provider-errors/documented/anthropic.billing-402.json",
        "provider-errors/documented/anthropic.spend-cap-429.json",
        "provider-errors/documented/anthropic.spend-limit-400.json",
        "provider-errors/documented/anthropic.workspace-spend-limit-400.json",
        "provider-errors/documented/openai.credit-balance-exhausted-429.json",
        "provider-errors/documented/openai.organization-spend-limit-429.json",
        "provider-errors/documented/openai.project-spend-limit-429.json",
        "provider-errors/documented/openai.organization-usage-limit-429.json",
        "provider-errors/documented/openai.insufficient-quota-429.json",
        "provider-errors/documented/openai.insufficient-quota-code-429.json",
        "provider-errors/documented/gemini.prepay-depleted-402.json",
        "provider-errors/documented/gemini.billing-disabled-403.json",
        "provider-errors/documented/gemini.resource-quota-exceeded-429.json",
        "provider-errors/documented/openrouter.insufficient-credits-402.json",
    ] {
        assert_eq!(recorded(case).code(), ErrorCode::QuotaExceeded, "{case}");
    }
}

#[test]
fn a_documented_rate_limit_stays_rate_limited() {
    for case in [
        "provider-errors/documented/openai.slow-down-429.json",
        "provider-errors/documented/gemini.quota-failure-429.json",
        "provider-errors/documented/gemini.rate-limit-exceeded-429.json",
        "provider-errors/documented/openrouter.in-flight-budget-402.json",
    ] {
        assert_eq!(recorded(case).code(), ErrorCode::RateLimited, "{case}");
    }
}

#[test]
fn an_in_flight_budget_402_without_retry_after_is_quota_exceeded() {
    let body = fixture_body("provider-errors/documented/openrouter.in-flight-budget-402.json");
    assert_eq!(status(402, &body, None), ErrorCode::QuotaExceeded);
    // A1 names no `limit_source`, so even a header leaves it a billing error.
    let billing = fixture_body("provider-errors/documented/anthropic.billing-402.json");
    assert_eq!(status(402, &billing, Some(5.0)), ErrorCode::QuotaExceeded);
}

#[test]
fn a_documented_stream_quota_failure_is_quota_exceeded() {
    let mut failures = Vec::new();
    failures.push(anthropic_stream("anthropic.billing-error-event.json"));
    failures.push(anthropic_stream("anthropic.spend-cap-event.json"));
    failures.push(completions_stream("openrouter.payment-required-chunk.json"));
    for case in [
        "gemini.payment-required-chunk.json",
        "gemini.billing-disabled-chunk.json",
        "gemini.resource-quota-exceeded-chunk.json",
    ] {
        failures.push(gemini_stream(case));
    }
    for err in &failures {
        assert_eq!(err.code(), ErrorCode::QuotaExceeded);
        let failure = err.failure("p", crate::LoginAdvice::Advised, &Secrets::default());
        assert_eq!(failure.code, ErrorCode::QuotaExceeded);
        let provider = failure.provider.unwrap();
        assert_eq!(provider.status, Some(200));
    }
}

#[test]
fn a_bare_stream_resource_exhausted_stays_rate_limited() {
    let err = gemini_stream("gemini.resource-exhausted-chunk.json");
    assert_eq!(err.code(), ErrorCode::RateLimited);
}

#[test]
fn only_the_quota_shapes_are_quota_exceeded() {
    // One clause is enough: the code alone, the type alone, A2's shape
    // alone, and a quota `ErrorInfo` with no other clause.
    assert_eq!(
        status(
            429,
            r#"{"error":{"code":"credit_balance_exhausted"}}"#,
            None
        ),
        ErrorCode::QuotaExceeded,
    );
    assert_eq!(
        status(429, r#"{"error":{"type":"insufficient_quota"}}"#, None),
        ErrorCode::QuotaExceeded,
    );
    assert_eq!(
        status(429, r#"{"error":{"code":"insufficient_quota"}}"#, None),
        ErrorCode::QuotaExceeded,
    );
    assert_eq!(
        status(
            429,
            r#"{"error":{"type":"rate_limit_error","details":{"error_code":"enforced_spend_limit_reached"}}}"#,
            None
        ),
        ErrorCode::QuotaExceeded,
    );
    // Near misses keep their old codes.
    assert_eq!(
        status(
            400,
            r#"{"error":{"type":"invalid_request_error","message":"bad field"}}"#,
            None
        ),
        ErrorCode::InvalidRequest,
    );
    assert_eq!(
        status(
            400,
            r#"{"error":{"type":"other","message":"You have reached your specified API usage limits"}}"#,
            None
        ),
        ErrorCode::InvalidRequest,
    );
    assert_eq!(
        status(429, r#"{"error":{"type":"rate_limit_error"}}"#, None),
        ErrorCode::RateLimited,
    );
    assert_eq!(
        status(
            429,
            r#"{"error":{"type":"rate_limit_error","details":{"error_code":"other"}}}"#,
            None
        ),
        ErrorCode::RateLimited,
    );
}

#[test]
fn quota_wins_over_every_code_but_authentication() {
    let billing_disabled = r#"{"error":{"code":403,"message":"billing is disabled","status":"PERMISSION_DENIED","details":[{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"BILLING_DISABLED"}]}}"#;
    assert_eq!(
        status(404, billing_disabled, None),
        ErrorCode::QuotaExceeded
    );
    assert_eq!(
        status(500, billing_disabled, None),
        ErrorCode::QuotaExceeded
    );
    assert_eq!(
        status(
            400,
            r#"{"error":{"type":"invalid_request_error","message":"You have reached your specified API usage limits for this model but the prompt is too long"}}"#,
            None
        ),
        ErrorCode::QuotaExceeded,
    );
    // Authentication still wins.
    assert_eq!(
        status(401, billing_disabled, None),
        ErrorCode::AuthenticationFailed
    );
    assert_eq!(
        status(
            400,
            r#"{"error":{"details":[{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"API_KEY_INVALID"},{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"BILLING_DISABLED"}]}}"#,
            None
        ),
        ErrorCode::AuthenticationFailed,
    );
    // `insufficient_quota` is exact to 429: a 404 with it names no model.
    assert_eq!(
        status(404, r#"{"error":{"type":"insufficient_quota"}}"#, None),
        ErrorCode::InvalidRequest,
    );
    assert_eq!(
        status(404, r#"{"error":{"code":"insufficient_quota"}}"#, None),
        ErrorCode::InvalidRequest,
    );
    assert_eq!(
        status(
            400,
            r#"{"error":{"type":"insufficient_quota","message":"the prompt is too long"}}"#,
            None
        ),
        ErrorCode::ContextOverflow,
    );
}

#[test]
fn no_recorded_reply_is_quota_exceeded() {
    let mut checked = 0;
    for (path, text) in RECORDED.iter().copied() {
        let value: Value = serde_json::from_str(text).unwrap();
        let Some(status) = value.get("status").and_then(Value::as_u64) else {
            continue;
        };
        if (200..300).contains(&status) || value.get("body").is_none_or(|b| !b.is_string()) {
            continue;
        }
        let body = value["body"].as_str().unwrap();
        let code = Error::Status {
            status: u16::try_from(status).unwrap(),
            body: body.to_owned(),
            retry_after: None,
            should_retry: None,
            url: String::new(),
            error_type: None,
        }
        .code();
        assert_ne!(code, ErrorCode::QuotaExceeded, "{path}");
        checked += 1;
    }
    assert!(checked >= 150, "only {checked} recorded replies checked");
}

#[test]
fn an_error_info_reason_needs_its_type() {
    // A `BILLING_DISABLED` reason under another `@type` is not a quota shape.
    assert_eq!(
        status(
            403,
            r#"{"error":{"details":[{"@type":"type.googleapis.com/google.rpc.QuotaFailure","reason":"BILLING_DISABLED"}]}}"#,
            None
        ),
        ErrorCode::InvalidRequest,
    );
}

#[test]
fn a_failed_status_records_the_asked_wait_in_milliseconds_rounded_up() {
    use std::time::Duration;
    let failed = |retry_after: Option<f64>| {
        let value = Error::Status {
            status: 429,
            body: "{}".to_owned(),
            retry_after,
            should_retry: None,
            url: String::new(),
            error_type: None,
        }
        .failure("p", crate::LoginAdvice::Advised, &Secrets::default());
        serde_json::to_value(&value).unwrap()
    };
    for (seconds, ms) in [
        (Some(2.0), Some(2000)),
        (Some(1.5), Some(1500)),
        (Some(0.0), Some(0)),
        (Some(0.0001), Some(1)),
        (Some(1e300), Some(u64::MAX)),
        (Some(0.043), Some(43)),
        (Some(0.043000000000000003), Some(43)),
        (Some(0.0431), Some(44)),
        (
            Some(Duration::from_millis(u64::MAX).as_secs_f64()),
            Some(u64::MAX),
        ),
    ] {
        let value = failed(seconds);
        assert_eq!(
            value.get("retry_after_ms").and_then(|v| v.as_u64()),
            ms,
            "asked {seconds:?}"
        );
        assert!(
            value.get("retry_after").is_none(),
            "no seconds key for {seconds:?}: {value}"
        );
    }
    for seconds in [Some(f64::NAN), Some(f64::INFINITY), Some(-1.0), None] {
        let value = failed(seconds);
        assert!(
            value.get("retry_after_ms").is_none(),
            "absent for {seconds:?}: {value}"
        );
        assert!(value.get("retry_after").is_none(), "{value}");
    }
}

// unprobed: shape from pi `parseErrorResponse` and codex-cli strings,
// `research/codex-responses-probe`, "The usage-limit error body".
fn usage_limit_body(field: &str, code: &str) -> String {
    format!(
        r#"{{"error":{{"{field}":"{code}","message":"The usage limit has been reached","plan_type":"plus","resets_at":1791396000}}}}"#
    )
}

#[test]
fn a_usage_limit_code_is_quota_exceeded_at_any_status() {
    for code in ["usage_limit_reached", "usage_not_included"] {
        for field in ["code", "type"] {
            for http in [400, 401, 403, 429, 500] {
                let body = usage_limit_body(field, code);
                assert_eq!(
                    status(http, &body, None),
                    ErrorCode::QuotaExceeded,
                    "{field}={code} at {http}"
                );
            }
        }
    }
}

#[test]
fn only_the_usage_limit_codes_are_quota_exceeded() {
    // `rate_limit_exceeded` stays a rate limit, as a code or a type.
    for field in ["code", "type"] {
        let body =
            format!(r#"{{"error":{{"{field}":"rate_limit_exceeded","message":"Slow down."}}}}"#);
        assert_eq!(status(429, &body, None), ErrorCode::RateLimited, "{field}");
    }
    // Near misses keep their old codes.
    assert_eq!(
        status(429, r#"{"error":{"code":"usage_limit"}}"#, None),
        ErrorCode::RateLimited,
    );
    assert_eq!(
        status(429, r#"{"error":{"type":"usage_limit_reached "}}"#, None),
        ErrorCode::RateLimited,
    );
    assert_eq!(
        status(429, r#"{"error":{"message":"usage_limit_reached"}}"#, None),
        ErrorCode::RateLimited,
    );
    assert_eq!(
        status(429, r#"{"error":{"code":402}}"#, None),
        ErrorCode::RateLimited,
    );
    assert_eq!(status(429, "not json", None), ErrorCode::RateLimited,);
}

#[test]
fn a_usage_limit_failure_names_the_reset_time() {
    let failed = Error::Status {
        status: 429,
        body: usage_limit_body("type", "usage_limit_reached"),
        retry_after: Some(7200.0),
        should_retry: None,
        url: String::new(),
        error_type: None,
    }
    .failure("codex", crate::LoginAdvice::Advised, &Secrets::default());
    assert_eq!(failed.code, ErrorCode::QuotaExceeded);
    assert_eq!(failed.retry_after_ms, Some(7_200_000));
    assert_eq!(
        failed.message,
        "codex reached its usage limit; it resets at 2026-10-07 18:00 UTC."
    );
    assert_eq!(
        failed.provider.unwrap().message,
        "The usage limit has been reached"
    );
}

#[test]
fn another_quota_shape_keeps_the_generic_status_message() {
    let failed = Error::Status {
        status: 429,
        body: r#"{"error":{"code":"credit_balance_exhausted","message":"No credits left."}}"#
            .to_owned(),
        retry_after: None,
        should_retry: None,
        url: String::new(),
        error_type: None,
    }
    .failure("openai", crate::LoginAdvice::Advised, &Secrets::default());

    assert_eq!(failed.code, ErrorCode::QuotaExceeded);
    assert_eq!(failed.message, "openai answered HTTP 429.");
}

#[test]
fn a_usage_limit_failure_without_a_reset_names_no_time() {
    for body in [
        r#"{"error":{"code":"usage_limit_reached","message":"Limited."}}"#.to_owned(),
        r#"{"error":{"code":"usage_limit_reached","resets_at":"tomorrow"}}"#.to_owned(),
        r#"{"error":{"code":"usage_limit_reached","resets_at":-1}}"#.to_owned(),
        r#"{"error":{"code":"usage_limit_reached","resets_at":1791396000.5}}"#.to_owned(),
    ] {
        let failed = Error::Status {
            status: 400,
            body,
            retry_after: None,
            should_retry: None,
            url: String::new(),
            error_type: None,
        }
        .failure("codex", crate::LoginAdvice::Advised, &Secrets::default());
        assert_eq!(failed.code, ErrorCode::QuotaExceeded);
        assert_eq!(failed.retry_after_ms, None);
        assert_eq!(failed.message, "codex reached its usage limit.");
    }
}

#[test]
fn reset_times_name_the_civil_date_in_utc() {
    // Through the failure message, from `resets_at` alone: no clock is read.
    for (resets_at, day) in [
        (0_u64, "1970-01-01 00:00 UTC"),
        (915_148_800_u64, "1999-01-01 00:00 UTC"),
        (946_598_400_u64, "1999-12-31 00:00 UTC"),
        (946_684_800_u64, "2000-01-01 00:00 UTC"),
        (951_782_400_u64, "2000-02-29 00:00 UTC"),
        (951_868_800_u64, "2000-03-01 00:00 UTC"),
        (1_835_395_200_u64, "2028-02-29 00:00 UTC"),
        (1_798_761_540_u64, "2026-12-31 23:59 UTC"),
        (4_102_444_800_u64, "2100-01-01 00:00 UTC"),
        (4_107_542_400_u64, "2100-03-01 00:00 UTC"),
        (13_574_563_200_u64, "2400-02-29 00:00 UTC"),
    ] {
        let body =
            format!(r#"{{"error":{{"code":"usage_not_included","resets_at":{resets_at}}}}}"#);
        let failed = Error::Status {
            status: 403,
            body,
            retry_after: None,
            should_retry: None,
            url: String::new(),
            error_type: None,
        }
        .failure("codex", crate::LoginAdvice::Advised, &Secrets::default());
        assert_eq!(
            failed.message,
            format!("codex reached its usage limit; it resets at {day}.")
        );
    }
}

/// A status as AWS answers it: the exception named in `X-Amzn-ErrorType`.
fn aws_status(status: u16, error_type: &str, body: &str) -> ErrorCode {
    Error::Status {
        status,
        body: body.to_owned(),
        retry_after: None,
        should_retry: None,
        url: String::new(),
        error_type: Some(error_type.to_owned()),
    }
    .code()
}

#[test]
fn each_recorded_bedrock_error_maps_to_its_code() {
    for (path, code) in [
        (
            "bedrock-invoke-probe/raw/bad-model.json",
            ErrorCode::ModelNotFound,
        ),
        (
            "bedrock-invoke-probe/raw/validation.json",
            ErrorCode::InvalidRequest,
        ),
        (
            "bedrock-invoke-probe/raw/bad-signature.json",
            ErrorCode::AuthenticationFailed,
        ),
        (
            "bedrock-invoke-probe/raw/bad-key-id.json",
            ErrorCode::AuthenticationFailed,
        ),
        (
            "bedrock-invoke-probe/raw/bad-bearer.json",
            ErrorCode::AuthenticationFailed,
        ),
    ] {
        assert_eq!(recorded(path).code(), code, "{path}");
    }
}

#[test]
fn an_aws_error_body_gives_its_message_whatever_the_keys_case() {
    for (path, said) in [
        (
            "bedrock-invoke-probe/raw/bad-model.json",
            "The provided model identifier is invalid.",
        ),
        (
            "bedrock-invoke-probe/raw/bad-bearer.json",
            "Invalid API Key format: Delimiter ':' not found",
        ),
    ] {
        let failure =
            recorded(path).failure("bedrock", crate::LoginAdvice::Advised, &Secrets::default());
        assert_eq!(failure.provider.unwrap().message, said, "{path}");
    }
}

#[test]
fn an_aws_exception_decides_the_code_whatever_its_case() {
    for (status, name, code) in [
        (
            403,
            "AccessDeniedException",
            ErrorCode::AuthenticationFailed,
        ),
        (
            403,
            "UnrecognizedClientException",
            ErrorCode::AuthenticationFailed,
        ),
        (
            403,
            "InvalidSignatureException",
            ErrorCode::AuthenticationFailed,
        ),
        (
            400,
            "IncompleteSignatureException",
            ErrorCode::AuthenticationFailed,
        ),
        (
            403,
            "MissingAuthenticationTokenException",
            ErrorCode::AuthenticationFailed,
        ),
        (
            403,
            "ExpiredTokenException",
            ErrorCode::AuthenticationFailed,
        ),
        (429, "ThrottlingException", ErrorCode::RateLimited),
        (429, "ModelNotReadyException", ErrorCode::RateLimited),
        (
            500,
            "InternalServerException",
            ErrorCode::ProviderUnavailable,
        ),
        (
            503,
            "ServiceUnavailableException",
            ErrorCode::ProviderUnavailable,
        ),
        (
            424,
            "ModelStreamErrorException",
            ErrorCode::ProviderUnavailable,
        ),
        (408, "ModelTimeoutException", ErrorCode::ProviderUnavailable),
        (424, "ModelErrorException", ErrorCode::ProviderUnavailable),
        (404, "ResourceNotFoundException", ErrorCode::ModelNotFound),
        // A name the status and the message decide.
        (400, "ValidationException", ErrorCode::InvalidRequest),
        // Unsettled until a probe reaches it (`docs/errors.md`, "Not
        // settled here"): any other 4xx.
        (
            400,
            "ServiceQuotaExceededException",
            ErrorCode::InvalidRequest,
        ),
    ] {
        let body = r#"{"message":"m"}"#;
        assert_eq!(aws_status(status, name, body), code, "{name}");
        let camel = format!("{}{}", name[..1].to_ascii_lowercase(), &name[1..]);
        assert_eq!(aws_status(status, &camel, body), code, "{camel}");
    }
    // The name decides over the status.
    assert_eq!(
        aws_status(400, "ThrottlingException", "{}"),
        ErrorCode::RateLimited
    );
    assert_eq!(
        aws_status(429, "ModelStreamErrorException", "{}"),
        ErrorCode::ProviderUnavailable
    );
}

#[test]
fn an_aws_validation_message_is_matched_as_every_body_is() {
    assert_eq!(
        aws_status(
            400,
            "ValidationException",
            r#"{"message":"prompt is too long: 250000 tokens > 200000 maximum"}"#
        ),
        ErrorCode::ContextOverflow
    );
    assert_eq!(
        aws_status(
            400,
            "ValidationException",
            r#"{"message":"The provided model identifier is invalid."}"#
        ),
        ErrorCode::ModelNotFound
    );
}

#[test]
fn authentication_and_quota_shapes_win_over_an_aws_exception() {
    // A usage-limit body wins over everything; an authentication exception
    // wins over a quota shape; a quota shape wins over any other exception.
    assert_eq!(
        aws_status(
            403,
            "AccessDeniedException",
            r#"{"error":{"code":"usage_limit_reached"}}"#
        ),
        ErrorCode::QuotaExceeded
    );
    let spend = r#"{"error":{"type":"invalid_request_error","message":"You have reached your specified API usage limits."}}"#;
    assert_eq!(
        aws_status(400, "ExpiredTokenException", spend),
        ErrorCode::AuthenticationFailed
    );
    assert_eq!(
        aws_status(400, "ThrottlingException", spend),
        ErrorCode::QuotaExceeded
    );
    assert_eq!(
        aws_status(401, "ThrottlingException", "{}"),
        ErrorCode::AuthenticationFailed
    );
}

#[test]
fn an_aws_exception_inside_the_stream_maps_to_its_code() {
    for (name, message, code) in [
        (
            "throttlingException",
            "Too many tokens.",
            ErrorCode::RateLimited,
        ),
        (
            "modelStreamErrorException",
            "The model stream failed.",
            ErrorCode::ProviderUnavailable,
        ),
        (
            "internalServerException",
            "Internal error.",
            ErrorCode::ProviderUnavailable,
        ),
        (
            "serviceUnavailableException",
            "Unavailable.",
            ErrorCode::ProviderUnavailable,
        ),
        (
            "modelTimeoutException",
            "Timed out.",
            ErrorCode::ProviderUnavailable,
        ),
        (
            "validationException",
            "Bad field.",
            ErrorCode::InvalidRequest,
        ),
        (
            "validationException",
            "prompt is too long: 250000 tokens > 200000 maximum",
            ErrorCode::ContextOverflow,
        ),
        ("someNewException", "New.", ErrorCode::StreamIncomplete),
    ] {
        let error = Error::ReplyFailed {
            code: Some(name.to_owned()),
            message: message.to_owned(),
        };
        assert_eq!(error.code(), code, "{name}: {message}");
        assert_eq!(error.should_retry(), None, "{name}");
        let failure = error.failure("bedrock", crate::LoginAdvice::Advised, &Secrets::default());
        let said = failure.provider.unwrap();
        assert_eq!(said.status, Some(200), "{name}");
        assert_eq!(said.message, message, "{name}");
    }
}
