//! One first-party provider file and the fields `cargo xtask models-dev`
//! keeps by hand for it, so a rerun reproduces the file exactly
//! (`docs/model-routing.md`, "What a provider extension declares").

/// How a package's models get their protocol.
pub(crate) enum ProtocolRule {
    /// Every model speaks this protocol.
    Fixed(&'static str),
    /// The protocol comes from models.dev's `provider.npm` field.
    ByNpm,
}

/// One first-party provider file and its hand-kept fields
/// (`docs/model-routing.md`, "What a provider extension declares"): the
/// provider-level fields, and the layers merged into each model after the
/// derived ones, with a later key replacing an earlier one.
pub(crate) struct Package {
    /// The file the generator writes, from the repository root.
    pub(crate) path: &'static str,
    /// The models.dev key its models come from.
    pub(crate) source: &'static str,
    /// A JSON object holding every provider-level field but `models`.
    pub(crate) provider: &'static str,
    /// How a model's protocol is decided.
    pub(crate) protocol: ProtocolRule,
    /// The base URL every model declares.
    pub(crate) base_url: &'static str,
    /// Whether a model with `status == "deprecated"` is left out.
    pub(crate) drop_deprecated: bool,
    /// Ids models.dev lists that the vendor rejects.
    pub(crate) skip: &'static [&'static str],
    /// Ids kept from models.dev: empty keeps every model, non-empty
    /// keeps just these ids and drops the rest silently. An id matching
    /// no generated model is an error, like a stale table entry.
    pub(crate) only: &'static [&'static str],
    /// A model's protocol, replacing the rule's, as `(id, protocol)`.
    pub(crate) protocol_overrides: &'static [(&'static str, &'static str)],
    /// A JSON object merged into every model.
    pub(crate) every_model: &'static str,
    /// A JSON object merged into each model on a protocol, as
    /// `(protocol, object)`.
    pub(crate) by_protocol: &'static [(&'static str, &'static str)],
    /// A JSON object merged into one model, as `(id, object)`.
    pub(crate) by_model: &'static [(&'static str, &'static str)],
    /// A model's thinking levels and default, as `(id, levels, default)`:
    /// merged after `by_model`, so it replaces a `by_model` layer naming
    /// the same keys. Every level is a thinking-vocabulary name (`off`,
    /// `minimal`, `low`, `medium`, `high`, `xhigh`, `max`); a `None`
    /// default leaves the vendor default in force by sending no level.
    /// An empty `levels` with no default means the model takes no level.
    /// A gateway (`opencode-go`, `opencode-zen`) model whose id no row
    /// here names takes the row of the direct-vendor package listing the
    /// same id exactly, unless its own table names one.
    pub(crate) thinking: &'static [(&'static str, &'static [&'static str], Option<&'static str>)],
}

/// The level sets the thinking tables share, each in vocabulary order.
const OFF_LOW_MEDIUM_HIGH_MAX: &[&str] = &["off", "low", "medium", "high", "max"];
const OFF_LOW_MEDIUM_HIGH_XHIGH_MAX: &[&str] = &["off", "low", "medium", "high", "xhigh", "max"];
const OFF_LOW_MEDIUM_HIGH: &[&str] = &["off", "low", "medium", "high"];
const OFF_HIGH: &[&str] = &["off", "high"];
/// No thinking level: the model takes none that could be shown, so Fiber
/// never sends a level. Cited by the record that shows why.
const NO_LEVELS: &[&str] = &[];
const OFF_LOW_MEDIUM_HIGH_XHIGH: &[&str] = &["off", "low", "medium", "high", "xhigh"];
const MINIMAL_LOW_MEDIUM_HIGH: &[&str] = &["minimal", "low", "medium", "high"];
const MINIMAL_LOW_MEDIUM_HIGH_XHIGH: &[&str] = &["minimal", "low", "medium", "high", "xhigh"];
const MINIMAL_LOW_MEDIUM_HIGH_XHIGH_MAX: &[&str] =
    &["minimal", "low", "medium", "high", "xhigh", "max"];
const MINIMAL_HIGH: &[&str] = &["minimal", "high"];
const LOW_MEDIUM_HIGH: &[&str] = &["low", "medium", "high"];
const LOW_HIGH_MAX: &[&str] = &["low", "high", "max"];
const LOW_MEDIUM_HIGH_XHIGH: &[&str] = &["low", "medium", "high", "xhigh"];
const LOW_MEDIUM_HIGH_XHIGH_MAX: &[&str] = &["low", "medium", "high", "xhigh", "max"];
const MEDIUM_XHIGH: &[&str] = &["medium", "xhigh"];
const MEDIUM_HIGH_XHIGH: &[&str] = &["medium", "high", "xhigh"];
const HIGH_ONLY: &[&str] = &["high"];
const OFF_MINIMAL_LOW_MEDIUM_HIGH: &[&str] = &["off", "minimal", "low", "medium", "high"];

/// The compat layer of an Anthropic model that takes a token budget.
const THINKING_BUDGET: &str = r#"{"compat":{"thinking_budget":true}}"#;

/// Anthropic's models, all on `anthropic-messages` with its search tool.
const ANTHROPIC: Package = Package {
    path: "providers/anthropic/providers/anthropic.json",
    source: "anthropic",
    provider: r#"{"credential":{"env":"ANTHROPIC_API_KEY"},"name":"anthropic","reviewer_model":"claude-sonnet-5-5"}"#,
    protocol: ProtocolRule::Fixed("anthropic-messages"),
    base_url: "https://api.anthropic.com/v1",
    drop_deprecated: false,
    skip: &[],
    only: &[],
    protocol_overrides: &[],
    every_model: r#"{}"#,
    by_protocol: &[(
        "anthropic-messages",
        r#"{"web_search":"web_search_20250305"}"#,
    )],
    by_model: &[
        ("claude-haiku-4-5", THINKING_BUDGET),
        ("claude-haiku-4-5-20251001", THINKING_BUDGET),
        ("claude-opus-4-5", THINKING_BUDGET),
        ("claude-opus-4-5-20251101", THINKING_BUDGET),
        ("claude-sonnet-4-5", THINKING_BUDGET),
        ("claude-sonnet-4-5-20250929", THINKING_BUDGET),
    ],
    thinking: &[
        ("claude-fable-5", LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        ("claude-fable-5-1", LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        ("claude-opus-4-6", OFF_LOW_MEDIUM_HIGH_MAX, None),
        ("claude-opus-4-7", OFF_LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        ("claude-opus-4-8", OFF_LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        ("claude-opus-5", LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        ("claude-haiku-5-5", LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        ("claude-opus-5-5", LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        ("claude-sonnet-4-6", OFF_LOW_MEDIUM_HIGH_MAX, None),
        ("claude-sonnet-5", OFF_LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        ("claude-sonnet-5-5", LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        ("claude-haiku-4-5", OFF_MINIMAL_LOW_MEDIUM_HIGH, None),
        (
            "claude-haiku-4-5-20251001",
            OFF_MINIMAL_LOW_MEDIUM_HIGH,
            None,
        ),
        ("claude-opus-4-5", OFF_MINIMAL_LOW_MEDIUM_HIGH, None),
        (
            "claude-opus-4-5-20251101",
            OFF_MINIMAL_LOW_MEDIUM_HIGH,
            None,
        ),
        ("claude-sonnet-4-5", OFF_MINIMAL_LOW_MEDIUM_HIGH, None),
        (
            "claude-sonnet-4-5-20250929",
            OFF_MINIMAL_LOW_MEDIUM_HIGH,
            None,
        ),
    ],
};

/// The hosted search the probed Gemini models accept (October 9, 2026); a
/// model not probed declares none.
const GOOGLE_SEARCH: &str = r#"{"web_search":"google_search"}"#;

/// Gemini's models, all on `google-generative-ai`.
const GEMINI: Package = Package {
    path: "providers/gemini/providers/gemini.json",
    source: "google",
    provider: r#"{"credential":{"env":"GEMINI_API_KEY"},"name":"gemini","reviewer_model":"gemini-3.8-flash"}"#,
    protocol: ProtocolRule::Fixed("google-generative-ai"),
    base_url: "https://generativelanguage.googleapis.com/v1beta",
    drop_deprecated: false,
    skip: &[],
    only: &[],
    protocol_overrides: &[],
    every_model: r#"{}"#,
    by_protocol: &[],
    by_model: &[
        ("gemini-3.1-flash-lite", GOOGLE_SEARCH),
        ("gemini-3.1-flash-lite-preview", GOOGLE_SEARCH),
        ("gemini-3.1-pro-preview", GOOGLE_SEARCH),
        ("gemini-3.1-pro-preview-customtools", GOOGLE_SEARCH),
        ("gemini-3.5-flash", GOOGLE_SEARCH),
        ("gemini-3.5-flash-lite", GOOGLE_SEARCH),
        ("gemini-3.6-flash", GOOGLE_SEARCH),
        ("gemini-3.7-flash", GOOGLE_SEARCH),
        ("gemini-3.8-flash", GOOGLE_SEARCH),
        ("gemini-3-flash-preview", GOOGLE_SEARCH),
    ],
    thinking: &[
        ("gemini-3-flash-preview", MINIMAL_LOW_MEDIUM_HIGH, None),
        ("gemini-3.1-flash-lite", MINIMAL_LOW_MEDIUM_HIGH, None),
        ("gemini-3.1-flash-lite-image", MINIMAL_HIGH, None),
        (
            "gemini-3.1-flash-lite-preview",
            MINIMAL_LOW_MEDIUM_HIGH,
            None,
        ),
        (
            "gemini-3.1-flash-live-preview",
            MINIMAL_LOW_MEDIUM_HIGH,
            None,
        ),
        ("gemini-3.1-pro-preview", LOW_MEDIUM_HIGH, None),
        ("gemini-3.1-pro-preview-customtools", LOW_MEDIUM_HIGH, None),
        ("gemini-3.5-flash", MINIMAL_LOW_MEDIUM_HIGH, None),
        ("gemini-3.5-flash-lite", MINIMAL_LOW_MEDIUM_HIGH, None),
        ("gemini-3.6-flash", MINIMAL_LOW_MEDIUM_HIGH, None),
        ("gemini-3.7-flash", LOW_MEDIUM_HIGH, None),
        ("gemini-3.8-flash", LOW_MEDIUM_HIGH, None),
        ("gemini-flash-latest", MINIMAL_LOW_MEDIUM_HIGH, None),
        ("gemini-flash-lite-latest", MINIMAL_LOW_MEDIUM_HIGH, None),
        ("gemma-4-26b-a4b-it", MINIMAL_HIGH, None),
        ("gemma-4-31b-it", MINIMAL_HIGH, None),
    ],
};

/// OpenAI's models, all on `openai-responses` with `store: false`.
const OPENAI: Package = Package {
    path: "providers/openai/providers/openai.json",
    source: "openai",
    provider: r#"{"credential":{"env":"OPENAI_API_KEY"},"name":"openai","reviewer_model":"gpt-6-luna"}"#,
    protocol: ProtocolRule::Fixed("openai-responses"),
    base_url: "https://api.openai.com/v1",
    drop_deprecated: false,
    skip: &["gpt-5.6"],
    only: &[],
    protocol_overrides: &[],
    every_model: r#"{"compat":{"store":false}}"#,
    by_protocol: &[],
    by_model: &[],
    thinking: &[
        ("gpt-5", MINIMAL_LOW_MEDIUM_HIGH, None),
        ("gpt-5-mini", MINIMAL_LOW_MEDIUM_HIGH, None),
        ("gpt-5-nano", MINIMAL_LOW_MEDIUM_HIGH, None),
        ("gpt-5-pro", HIGH_ONLY, None),
        ("gpt-5.1", OFF_LOW_MEDIUM_HIGH, None),
        ("gpt-5.2", OFF_LOW_MEDIUM_HIGH_XHIGH, None),
        ("gpt-5.2-chat-latest", MEDIUM_XHIGH, None),
        ("gpt-5.2-pro", MEDIUM_HIGH_XHIGH, None),
        ("gpt-5.3-codex", OFF_LOW_MEDIUM_HIGH_XHIGH, None),
        ("gpt-5.3-codex-spark", LOW_MEDIUM_HIGH_XHIGH, None),
        ("gpt-5.4", OFF_LOW_MEDIUM_HIGH_XHIGH, None),
        ("gpt-5.4-mini", OFF_LOW_MEDIUM_HIGH_XHIGH, None),
        ("gpt-5.4-nano", OFF_LOW_MEDIUM_HIGH_XHIGH, None),
        ("gpt-5.4-pro", MEDIUM_HIGH_XHIGH, None),
        ("gpt-5.5", OFF_LOW_MEDIUM_HIGH_XHIGH, None),
        ("gpt-5.5-pro", MEDIUM_HIGH_XHIGH, None),
        ("gpt-5.6-luna", OFF_LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        ("gpt-5.6-sol", OFF_LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        ("gpt-5.6-terra", OFF_LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        ("gpt-6-astra", LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        ("gpt-6-luna", OFF_LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        ("gpt-6-sol", OFF_LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        ("gpt-6.1-sol", LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        (
            "gpt-daybreak-blue-latest",
            OFF_LOW_MEDIUM_HIGH_XHIGH_MAX,
            None,
        ),
        (
            "gpt-daybreak-red-latest",
            OFF_LOW_MEDIUM_HIGH_XHIGH_MAX,
            None,
        ),
        ("gpt-realtime-2.1", MINIMAL_LOW_MEDIUM_HIGH_XHIGH, None),
        ("o1", LOW_MEDIUM_HIGH, None),
        ("o1-pro", LOW_MEDIUM_HIGH, None),
        ("o3", LOW_MEDIUM_HIGH, None),
        ("o3-mini", LOW_MEDIUM_HIGH, None),
        ("o3-pro", LOW_MEDIUM_HIGH, None),
        ("o4-mini", LOW_MEDIUM_HIGH, None),
    ],
};

/// Codex's models: the four subscription models models.dev lists under
/// `openai`, all on `openai-responses` with the session cache-key header.
/// The ChatGPT backend serves a 272000-token window, not the 1050000 the
/// OpenAI API lists, so the table pins `context_window`.
const CODEX: Package = Package {
    path: "providers/codex/providers/codex.json",
    source: "openai",
    provider: r#"{"headers":{"originator":"fiber"},"login":"browser","name":"codex","reviewer_model":"gpt-6-luna"}"#,
    protocol: ProtocolRule::Fixed("openai-responses"),
    base_url: "https://chatgpt.com/backend-api/codex",
    drop_deprecated: false,
    skip: &[],
    only: &["gpt-6-astra", "gpt-6-luna", "gpt-6-sol", "gpt-6.1-sol"],
    protocol_overrides: &[],
    every_model: r#"{"compat":{"cache_key_header":"session_id","store":false},"context_window":272000,"subscription":true}"#,
    by_protocol: &[],
    by_model: &[],
    thinking: &[
        ("gpt-6.1-sol", LOW_MEDIUM_HIGH_XHIGH_MAX, Some("low")),
        ("gpt-6-sol", LOW_MEDIUM_HIGH_XHIGH_MAX, Some("medium")),
        ("gpt-6-astra", LOW_MEDIUM_HIGH_XHIGH_MAX, Some("low")),
        ("gpt-6-luna", LOW_MEDIUM_HIGH_XHIGH_MAX, Some("medium")),
    ],
};

/// Meta's models, all on `openai-responses` with its search tool.
const MUSE: Package = Package {
    path: "providers/muse/providers/muse.json",
    source: "meta",
    provider: r#"{"credential":{"env":"META_API_KEY"},"name":"muse","reviewer_model":"muse-spark-1.3"}"#,
    protocol: ProtocolRule::Fixed("openai-responses"),
    base_url: "https://api.meta.ai/v1",
    drop_deprecated: false,
    skip: &[],
    only: &[],
    protocol_overrides: &[],
    every_model: r#"{}"#,
    by_protocol: &[(
        "openai-responses",
        r#"{"extra_body":{"include":["reasoning.encrypted_content","web_search_call.results"]},"web_search":"web_search"}"#,
    )],
    by_model: &[],
    thinking: &[
        ("muse-spark-1.3", MINIMAL_LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        (
            "muse-spark-1.3-contributor",
            MINIMAL_LOW_MEDIUM_HIGH_XHIGH_MAX,
            None,
        ),
    ],
};

/// The cache-key header every OpenCode model sends.
const OPENCODE_SESSION: &str = r#"{"compat":{"cache_key_header":"x-opencode-session"}}"#;
/// The same, plus the token-limit field for `openai-completions` models.
const OPENCODE_COMPLETIONS: &str =
    r#"{"compat":{"cache_key_header":"x-opencode-session","max_tokens":true}}"#;
/// A gateway Claude model on a legacy budget tier: the budget flag beside
/// the gateway cache-key header, since a table layer replaces the whole
/// `compat` object rather than deep-merging it.
const GATEWAY_THINKING_BUDGET: &str =
    r#"{"compat":{"cache_key_header":"x-opencode-session","thinking_budget":true}}"#;

/// OpenCode Go: a subscription, priced at the API rates.
const OPENCODE_GO: Package = Package {
    path: "providers/opencode/providers/opencode-go.json",
    source: "opencode-go",
    provider: r#"{"credential":{"env":"OPENCODE_API_KEY"},"credential_name":"opencode","name":"opencode-go"}"#,
    protocol: ProtocolRule::ByNpm,
    base_url: "https://opencode.ai/zen/go/v1",
    drop_deprecated: true,
    skip: &[],
    only: &[],
    // No overrides: minimax-m2.7 once forced openai-completions, but the
    // route rejects that protocol and speaks anthropic-messages, which
    // ByNpm already gives it (research/opencode-thinking/README.md).
    protocol_overrides: &[],
    every_model: r#"{"subscription":true}"#,
    by_protocol: &[
        // provisional: x-api-key-only auth is unprobed on OpenCode;
        // research/opencode-probe/probe.py sent Authorization and x-api-key together.
        ("anthropic-messages", OPENCODE_SESSION),
        ("openai-completions", OPENCODE_COMPLETIONS),
        ("openai-responses", OPENCODE_SESSION),
        ("google-generative-ai", OPENCODE_SESSION),
    ],
    by_model: &[],
    thinking: &[
        // DeepSeek's legacy names are served by DeepSeek-V4.1-Flash
        // (https://api-docs.deepseek.com/); the record below probed it.
        ("deepseek-v4-flash", OFF_LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        // research/opencode-thinking/raw/go-deepseek-v4-flash-vision-exp.json
        (
            "deepseek-v4-flash-vision-exp",
            OFF_LOW_MEDIUM_HIGH_XHIGH_MAX,
            None,
        ),
        // No level could be shown: both routes block this model, so Fiber
        // sends none (research/opencode-thinking/raw/go-deepseek-v4-pro.json
        // and raw/zen-deepseek-v4-pro.json).
        ("deepseek-v4-pro", NO_LEVELS, None),
        // research/opencode-thinking/raw/zen-deepseek-v4.1-flash.json: the
        // Go route needs Global regions, so the Go row cites the Zen probe
        // of the same model.
        ("deepseek-v4.1-flash", OFF_LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        // research/opencode-thinking/raw/go-glm-5.2.json (`off` is
        // rejected: a thinking-only model).
        ("glm-5.2", MINIMAL_LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        // research/opencode-thinking/raw/go-glm-5.3.json (reasoning tokens
        // move with the level).
        ("glm-5.3", MINIMAL_LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        // research/opencode-thinking/raw/go-glm-5.3-flash.json
        ("glm-5.3-flash", OFF_LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        // https://docs.x.ai/developers/model-capabilities/text/reasoning:
        // `reasoning.effort` takes low, medium, high and xhigh (4.6 and
        // later); reasoning cannot be disabled, so no `off`.
        ("grok-4.6", LOW_MEDIUM_HIGH_XHIGH, None),
        ("grok-4.7", LOW_MEDIUM_HIGH_XHIGH, None),
        // research/opencode-thinking/raw/go-hy3.json
        ("hy3", OFF_LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        // research/opencode-thinking/raw/go-hy4-preview.json
        ("hy4-preview", OFF_LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        // research/opencode-thinking/raw/go-kimi-k2.7-code.json
        ("kimi-k2.7-code", MINIMAL_LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        // https://platform.moonshot.ai/docs/guide/use-reasoning-effort:
        // K3 configures reasoning effort with top-level `reasoning_effort`,
        // supporting low, high and max with a max default.
        ("kimi-k3", LOW_HIGH_MAX, None),
        // research/opencode-thinking/raw/go-longcat-2.0.json
        ("longcat-2.0", OFF_LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        // research/opencode-thinking/raw/go-longcat-2.5-preview-free.json
        (
            "longcat-2.5-preview-free",
            OFF_LOW_MEDIUM_HIGH_XHIGH_MAX,
            None,
        ),
        // research/opencode-thinking/raw/go-mimo-v2.5.json
        ("mimo-v2.5", OFF_LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        // research/opencode-thinking/raw/go-mimo-v2.5-pro.json: upstream
        // enumerates none, low, medium, high (plus max, accepted).
        ("mimo-v2.5-pro", OFF_LOW_MEDIUM_HIGH_MAX, None),
        // research/opencode-thinking/raw/go-mimo-v2.6-flash.json
        ("mimo-v2.6-flash", OFF_LOW_MEDIUM_HIGH_MAX, None),
        // research/opencode-thinking/raw/go-mimo-v2.6-pro.json
        ("mimo-v2.6-pro", OFF_LOW_MEDIUM_HIGH_MAX, None),
        // research/opencode-thinking/raw/go-minimax-m2.7.json (messages;
        // completions is unsupported there).
        ("minimax-m2.7", OFF_LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        // research/opencode-thinking/raw/go-minimax-m3.json
        ("minimax-m3", OFF_LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        // research/opencode-thinking/raw/go-muse-spark-1.2-contributor.json:
        // the route enumerates minimal through xhigh.
        (
            "muse-spark-1.2-contributor",
            MINIMAL_LOW_MEDIUM_HIGH_XHIGH,
            None,
        ),
        // Normalized from `by_model`: the vendor's own row, kept verbatim.
        (
            "muse-spark-1.3-contributor",
            MINIMAL_LOW_MEDIUM_HIGH_XHIGH_MAX,
            None,
        ),
        // research/opencode-thinking/raw/go-qwen3.7-plus.json
        ("qwen3.7-plus", OFF_LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        // research/opencode-thinking/raw/go-qwen3.8-flash.json
        ("qwen3.8-flash", OFF_LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        // research/opencode-thinking/raw/go-qwen3.8-max.json
        ("qwen3.8-max", OFF_LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        // research/opencode-thinking/raw/go-space-bunny.json
        ("space-bunny", MINIMAL_LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        // research/opencode-thinking/raw/go-step-5-preview-free.json
        (
            "step-5-preview-free",
            MINIMAL_LOW_MEDIUM_HIGH_XHIGH_MAX,
            None,
        ),
    ],
};

/// OpenCode Zen: billed per token. Its Gemini models are kept: one request
/// to each of two answered 200 (research/opencode-zen-gemini-probe).
const OPENCODE_ZEN: Package = Package {
    path: "providers/opencode/providers/opencode-zen.json",
    source: "opencode",
    provider: r#"{"credential":{"env":"OPENCODE_API_KEY"},"credential_name":"opencode","name":"opencode-zen","reviewer_model":"muse-spark-1.3"}"#,
    protocol: ProtocolRule::ByNpm,
    base_url: "https://opencode.ai/zen/v1",
    drop_deprecated: true,
    skip: &[],
    only: &[],
    protocol_overrides: &[],
    every_model: r#"{}"#,
    by_protocol: &[
        // provisional: x-api-key-only auth is unprobed on OpenCode;
        // research/opencode-probe/probe.py sent Authorization and x-api-key together.
        ("anthropic-messages", OPENCODE_SESSION),
        ("openai-completions", OPENCODE_COMPLETIONS),
        ("openai-responses", OPENCODE_SESSION),
        ("google-generative-ai", OPENCODE_SESSION),
    ],
    by_model: &[
        (
            "gpt-6.1-sol",
            r#"{"extra_body":{"include":["reasoning.encrypted_content","web_search_call.action.sources"]},"web_search":"web_search"}"#,
        ),
        // Each inherits a legacy Claude budget-tier row (off, minimal, low,
        // medium, high), so it takes the token budget instead of adaptive
        // thinking (`docs/model-routing.md`, "Thinking").
        ("claude-haiku-4-5", GATEWAY_THINKING_BUDGET),
        ("claude-opus-4-5", GATEWAY_THINKING_BUDGET),
        ("claude-sonnet-4", GATEWAY_THINKING_BUDGET),
        ("claude-sonnet-4-5", GATEWAY_THINKING_BUDGET),
    ],
    thinking: &[
        // No level could be shown: the free tier answers only inside
        // OpenCode (research/opencode-thinking/raw/zen-big-pickle.json).
        ("big-pickle", NO_LEVELS, None),
        // https://docs.anthropic.com/en/docs/build-with-claude/extended-thinking:
        // Claude Sonnet 4 is an earlier Claude 4 model, so it keeps manual
        // budget thinking; the compat layer above carries the budget flag.
        ("claude-sonnet-4", OFF_MINIMAL_LOW_MEDIUM_HIGH, None),
        // DeepSeek's legacy names are served by DeepSeek-V4.1-Flash
        // (https://api-docs.deepseek.com/); the record below probed it.
        ("deepseek-v4-flash", OFF_LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        // research/opencode-thinking/raw/go-deepseek-v4-flash-vision-exp.json:
        // Zen keeps this model disabled, so the row cites the Go probe of
        // the same model.
        (
            "deepseek-v4-flash-vision-exp",
            OFF_LOW_MEDIUM_HIGH_XHIGH_MAX,
            None,
        ),
        // No level could be shown: both routes block this model, so Fiber
        // sends none (research/opencode-thinking/raw/zen-deepseek-v4-pro.json
        // and raw/go-deepseek-v4-pro.json).
        ("deepseek-v4-pro", NO_LEVELS, None),
        // research/opencode-thinking/raw/zen-deepseek-v4.1-flash.json
        ("deepseek-v4.1-flash", OFF_LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        // No level could be shown: the route retired this model
        // (research/opencode-thinking/raw/zen-exo-free.json).
        ("exo-free", NO_LEVELS, None),
        // https://ai.google.dev/gemini-api/docs/thinking lists these levels
        // for the preview of the same model; the GA model stays disabled on
        // Zen (research/opencode-thinking/raw/zen-gemini-3-flash.json).
        ("gemini-3-flash", MINIMAL_LOW_MEDIUM_HIGH, None),
        // https://ai.google.dev/gemini-api/docs/thinking lists these levels
        // for the preview of the same model; the GA model stays disabled on
        // Zen (research/opencode-thinking/raw/zen-gemini-3.1-pro.json).
        ("gemini-3.1-pro", LOW_MEDIUM_HIGH, None),
        // The Zen account keeps GLM-5 and 5.1 disabled, so these rows mirror
        // the Go-route probes of the same-backend 5.2 and 5.3, beside the
        // Zhipu thinking-mode docs
        // (https://docs.z.ai/guides/capabilities/thinking-mode).
        ("glm-5", MINIMAL_LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        ("glm-5.1", MINIMAL_LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        // research/opencode-thinking/raw/go-glm-5.2.json: Zen keeps this
        // model disabled, so the row cites the Go probe of the same model.
        ("glm-5.2", MINIMAL_LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        // research/opencode-thinking/raw/go-glm-5.3.json: Zen keeps this
        // model disabled, so the row cites the Go probe of the same model.
        ("glm-5.3", MINIMAL_LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        // research/opencode-thinking/raw/zen-glm-5.3-flash.json
        ("glm-5.3-flash", MINIMAL_LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        // No level could be shown: Zen keeps these models disabled, and no
        // vendor docs settle their levels
        // (research/opencode-thinking/raw/zen-gpt-5-codex.json and the
        // matching records).
        ("gpt-5-codex", NO_LEVELS, None),
        ("gpt-5.1-codex", NO_LEVELS, None),
        ("gpt-5.1-codex-max", NO_LEVELS, None),
        ("gpt-5.1-codex-mini", NO_LEVELS, None),
        ("gpt-5.2-codex", NO_LEVELS, None),
        // No level could be shown: Zen keeps this model disabled, and xAI's
        // docs name no such model
        // (research/opencode-thinking/raw/zen-grok-build-0.1.json).
        ("grok-build-0.1", NO_LEVELS, None),
        // https://docs.x.ai/developers/model-capabilities/text/reasoning:
        // `reasoning.effort` takes low, medium and high on 4.5, plus xhigh
        // on 4.6 and later; reasoning cannot be disabled, so no `off`.
        ("grok-4.5", LOW_MEDIUM_HIGH, None),
        ("grok-4.6", LOW_MEDIUM_HIGH_XHIGH, None),
        ("grok-4.7", LOW_MEDIUM_HIGH_XHIGH, None),
        // The Zen account keeps Kimi K2.5 and K2.6 disabled, so these rows
        // mirror the Go-route probe of the same-backend k2.7-code, beside
        // the Moonshot thinking-models docs
        // (https://platform.moonshot.ai/docs/guide/use-thinking-models).
        ("kimi-k2.5", MINIMAL_LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        ("kimi-k2.6", MINIMAL_LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        // research/opencode-thinking/raw/go-kimi-k2.7-code.json: Zen keeps
        // this model disabled, so the row cites the Go probe of the same
        // model.
        ("kimi-k2.7-code", MINIMAL_LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        // https://platform.moonshot.ai/docs/guide/use-reasoning-effort:
        // K3 configures reasoning effort with top-level `reasoning_effort`,
        // supporting low, high and max with a max default.
        ("kimi-k3", LOW_HIGH_MAX, None),
        // No level could be shown: upstream reports these models unavailable
        // (research/opencode-thinking/raw/zen-ling-3.0-flash-fin-free.json
        // and raw/zen-ling-3.1-flash-free.json).
        ("ling-3.0-flash-fin-free", NO_LEVELS, None),
        ("ling-3.1-flash-free", NO_LEVELS, None),
        // research/opencode-thinking/raw/go-longcat-2.5-preview-free.json:
        // the free tier answers only inside OpenCode on Zen, so the row
        // cites the Go probe of the same model.
        (
            "longcat-2.5-preview-free",
            OFF_LOW_MEDIUM_HIGH_XHIGH_MAX,
            None,
        ),
        // research/opencode-thinking/raw/go-mimo-v2.6-flash.json: the free
        // tier answers only inside OpenCode on Zen, so the row cites the Go
        // probe of the paid variant.
        ("mimo-v2.6-flash-free", OFF_LOW_MEDIUM_HIGH_MAX, None),
        // No level could be shown: Zen keeps these MiniMax models disabled,
        // and nothing probes their completions route
        // (research/opencode-thinking/raw/zen-minimax-m2.5.json and the
        // matching records).
        ("minimax-m2.5", NO_LEVELS, None),
        ("minimax-m2.7", NO_LEVELS, None),
        ("minimax-m3", NO_LEVELS, None),
        // research/opencode-thinking/raw/zen-mistral-large-4.json (the
        // `high` reply carries a thinking chunk) and
        // https://docs.mistral.ai/capabilities/reasoning.
        ("mistral-large-4", OFF_HIGH, None),
        // research/opencode-thinking/raw/go-muse-spark-1.2-contributor.json:
        // Zen keeps 1.2 disabled, so the row cites the Go probe of the same
        // generation's contributor tier.
        ("muse-spark-1.2", MINIMAL_LOW_MEDIUM_HIGH_XHIGH, None),
        // Normalized from `by_model`: the vendor's own row, kept verbatim.
        ("muse-spark-1.3", MINIMAL_LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        // The 1.3 family takes minimal through max on both routes; the free
        // tier answers only inside OpenCode on Zen
        // (research/opencode-thinking/raw/zen-muse-spark-1.3-contributor-free.json).
        (
            "muse-spark-1.3-contributor-free",
            MINIMAL_LOW_MEDIUM_HIGH_XHIGH_MAX,
            None,
        ),
        // No level could be shown: the free tier answers only inside
        // OpenCode (research/opencode-thinking/raw/zen-nemotron-3-ultra-free.json
        // and raw/zen-nemotron-3.5-lightning-free.json).
        ("nemotron-3-ultra-free", NO_LEVELS, None),
        ("nemotron-3.5-lightning-free", NO_LEVELS, None),
        // The Zen account keeps Qwen 3.5 and 3.6 Plus disabled, so these
        // rows mirror the probed Qwen siblings on both routes.
        ("qwen3.5-plus", OFF_LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        ("qwen3.6-plus", OFF_LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        // research/opencode-thinking/raw/zen-qwen3.8-flash.json
        ("qwen3.8-flash", OFF_LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        // research/opencode-thinking/raw/zen-qwen3.8-max.json
        ("qwen3.8-max", OFF_LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        // research/opencode-thinking/raw/go-space-bunny.json: the free tier
        // answers only inside OpenCode on Zen, so the row cites the Go
        // probe of the paid tier.
        ("space-bunny-free", MINIMAL_LOW_MEDIUM_HIGH_XHIGH_MAX, None),
        // research/opencode-thinking/raw/go-step-5-preview-free.json: the
        // free tier answers only inside OpenCode on Zen, so the row cites
        // the Go probe of the same model.
        (
            "step-5-preview-free",
            MINIMAL_LOW_MEDIUM_HIGH_XHIGH_MAX,
            None,
        ),
    ],
};

/// The seven first-party provider files generated from models.dev.
pub(crate) const PACKAGES: [Package; 7] = [
    ANTHROPIC,
    GEMINI,
    OPENAI,
    CODEX,
    MUSE,
    OPENCODE_GO,
    OPENCODE_ZEN,
];
