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
    /// Protocols left out until a probe covers them.
    pub(crate) drop_protocols: &'static [&'static str],
    /// Ids models.dev lists that the vendor rejects.
    pub(crate) skip: &'static [&'static str],
    /// A model's protocol, replacing the rule's, as `(id, protocol)`.
    pub(crate) protocol_overrides: &'static [(&'static str, &'static str)],
    /// A JSON object merged into every model.
    pub(crate) every_model: &'static str,
    /// A JSON object merged into each model on a protocol, as
    /// `(protocol, object)`.
    pub(crate) by_protocol: &'static [(&'static str, &'static str)],
    /// A JSON object merged into one model, as `(id, object)`.
    pub(crate) by_model: &'static [(&'static str, &'static str)],
}

/// Anthropic's models, all on `anthropic-messages` with its search tool.
const ANTHROPIC: Package = Package {
    path: "providers/anthropic/providers/anthropic.json",
    source: "anthropic",
    provider: r#"{"credential":{"env":"ANTHROPIC_API_KEY"},"name":"anthropic","reviewer_model":"claude-sonnet-5-5"}"#,
    protocol: ProtocolRule::Fixed("anthropic-messages"),
    base_url: "https://api.anthropic.com/v1",
    drop_deprecated: false,
    drop_protocols: &[],
    skip: &[],
    protocol_overrides: &[],
    every_model: r#"{}"#,
    by_protocol: &[(
        "anthropic-messages",
        r#"{"web_search":"web_search_20250305"}"#,
    )],
    by_model: &[],
};

/// Gemini's models, all on `google-generative-ai`.
const GEMINI: Package = Package {
    path: "providers/gemini/providers/gemini.json",
    source: "google",
    provider: r#"{"credential":{"env":"GEMINI_API_KEY"},"name":"gemini","reviewer_model":"gemini-3.8-flash"}"#,
    protocol: ProtocolRule::Fixed("google-generative-ai"),
    base_url: "https://generativelanguage.googleapis.com/v1beta",
    drop_deprecated: false,
    drop_protocols: &[],
    skip: &[],
    protocol_overrides: &[],
    every_model: r#"{}"#,
    by_protocol: &[],
    by_model: &[],
};

/// OpenAI's models, all on `openai-responses` with `store: false`.
const OPENAI: Package = Package {
    path: "providers/openai/providers/openai.json",
    source: "openai",
    provider: r#"{"credential":{"env":"OPENAI_API_KEY"},"name":"openai","reviewer_model":"gpt-6-luna"}"#,
    protocol: ProtocolRule::Fixed("openai-responses"),
    base_url: "https://api.openai.com/v1",
    drop_deprecated: false,
    drop_protocols: &[],
    skip: &["gpt-5.6"],
    protocol_overrides: &[],
    every_model: r#"{"compat":{"store":false}}"#,
    by_protocol: &[],
    by_model: &[],
};

/// Meta's models, all on `openai-responses` with its search tool.
const MUSE: Package = Package {
    path: "providers/muse/providers/muse.json",
    source: "meta",
    provider: r#"{"credential":{"env":"META_API_KEY"},"name":"muse","reviewer_model":"muse-spark-1.3"}"#,
    protocol: ProtocolRule::Fixed("openai-responses"),
    base_url: "https://api.meta.ai/v1",
    drop_deprecated: false,
    drop_protocols: &[],
    skip: &[],
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
    drop_protocols: &[],
    skip: &[],
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
};

/// OpenCode Zen: billed per token, leaving out its Gemini models until
/// that route is probed.
const OPENCODE_ZEN: Package = Package {
    path: "providers/opencode/providers/opencode-zen.json",
    source: "opencode",
    provider: r#"{"credential":{"env":"OPENCODE_API_KEY"},"credential_name":"opencode","name":"opencode-zen","reviewer_model":"muse-spark-1.3"}"#,
    protocol: ProtocolRule::ByNpm,
    base_url: "https://opencode.ai/zen/v1",
    drop_deprecated: true,
    drop_protocols: &["google-generative-ai"],
    skip: &[],
    protocol_overrides: &[],
    every_model: r#"{}"#,
    by_protocol: &[
        // provisional: x-api-key-only auth is unprobed on OpenCode;
        // research/opencode-probe/probe.py sent Authorization and x-api-key together.
        ("anthropic-messages", OPENCODE_SESSION),
        ("openai-completions", OPENCODE_COMPLETIONS),
        ("openai-responses", OPENCODE_SESSION),
    ],
    by_model: &[
        (
            "muse-spark-1.3",
            r#"{"thinking_levels":["minimal","low","medium","high","xhigh","max"]}"#,
        ),
        ("gpt-6.1-sol", r#"{"web_search":"web_search"}"#),
    ],
};

/// The six first-party provider files generated from models.dev.
pub(crate) const PACKAGES: [Package; 6] =
    [ANTHROPIC, GEMINI, OPENAI, MUSE, OPENCODE_GO, OPENCODE_ZEN];
