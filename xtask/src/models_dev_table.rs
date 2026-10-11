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
    pub(crate) thinking: &'static [(&'static str, &'static [&'static str], Option<&'static str>)],
}

/// The level sets the thinking tables share, each in vocabulary order.
const OFF_LOW_MEDIUM_HIGH_MAX: &[&str] = &["off", "low", "medium", "high", "max"];
const OFF_LOW_MEDIUM_HIGH_XHIGH_MAX: &[&str] = &["off", "low", "medium", "high", "xhigh", "max"];
const OFF_LOW_MEDIUM_HIGH: &[&str] = &["off", "low", "medium", "high"];
const OFF_LOW_MEDIUM_HIGH_XHIGH: &[&str] = &["off", "low", "medium", "high", "xhigh"];
const MINIMAL_LOW_MEDIUM_HIGH: &[&str] = &["minimal", "low", "medium", "high"];
const MINIMAL_LOW_MEDIUM_HIGH_XHIGH: &[&str] = &["minimal", "low", "medium", "high", "xhigh"];
const MINIMAL_HIGH: &[&str] = &["minimal", "high"];
const LOW_MEDIUM_HIGH: &[&str] = &["low", "medium", "high"];
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
    every_model: r#"{"compat":{"cache_key_header":"session_id","store":false},"subscription":true}"#,
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
    by_model: &[
        (
            "muse-spark-1.3",
            r#"{"thinking_levels":["minimal","low","medium","high","xhigh","max"]}"#,
        ),
        (
            "muse-spark-1.3-contributor",
            r#"{"thinking_levels":["minimal","low","medium","high","xhigh","max"]}"#,
        ),
    ],
    thinking: &[],
};

/// The cache-key header every OpenCode model sends.
const OPENCODE_SESSION: &str = r#"{"compat":{"cache_key_header":"x-opencode-session"}}"#;
/// The same, plus the token-limit field for `openai-completions` models.
const OPENCODE_COMPLETIONS: &str =
    r#"{"compat":{"cache_key_header":"x-opencode-session","max_tokens":true}}"#;

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
    protocol_overrides: &[("minimax-m2.7", "openai-completions")],
    every_model: r#"{"subscription":true}"#,
    by_protocol: &[
        // provisional: x-api-key-only auth is unprobed on OpenCode;
        // research/opencode-probe/probe.py sent Authorization and x-api-key together.
        ("anthropic-messages", OPENCODE_SESSION),
        ("openai-completions", OPENCODE_COMPLETIONS),
        ("openai-responses", OPENCODE_SESSION),
        ("google-generative-ai", OPENCODE_SESSION),
    ],
    by_model: &[(
        "muse-spark-1.3-contributor",
        r#"{"thinking_levels":["minimal","low","medium","high","xhigh","max"]}"#,
    )],
    thinking: &[],
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
            "muse-spark-1.3",
            r#"{"thinking_levels":["minimal","low","medium","high","xhigh","max"]}"#,
        ),
        (
            "gpt-6.1-sol",
            r#"{"extra_body":{"include":["reasoning.encrypted_content","web_search_call.action.sources"]},"web_search":"web_search"}"#,
        ),
    ],
    thinking: &[],
};

/// The seven first-party provider files generated from models.dev.
pub(crate) const PACKAGES: [Package; 7] =
    [ANTHROPIC, GEMINI, OPENAI, CODEX, MUSE, OPENCODE_GO, OPENCODE_ZEN];
