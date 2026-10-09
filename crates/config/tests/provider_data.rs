//! `docs/configuration.md`, "An extension's manifest" and "A provider's
//! data": both files are strict JSON in the documented shape, and a file that
//! does not fit is `config_invalid`.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]

mod common;

use common::Setup;
use config::{CredentialSource, Login, Protocol, read_manifest, read_package_text, read_providers};
use contract::ErrorCode;

const DATABRICKS: &str = r#"{
  "name": "databricks",
  "credential": { "env": "DATABRICKS_TOKEN" },
  "headers": { "x-databricks-client": "fiber" },
  "placeholders": { "workspace": { "env": "DATABRICKS_HOST" } },
  "models": [
    {
      "id": "databricks-claude-opus-5",
      "protocol": "anthropic-messages",
      "base_url": "https://{workspace}/ai-gateway/anthropic",
      "compat": { "store": false },
      "deferred_tools": true,
      "extra_body": {},
      "context_window": 1000000,
      "max_output_tokens": 128000,
      "input": ["text", "image"],
      "cost": { "input": 5.0, "output": 25.0, "cache_read": 0.5, "cache_write": 6.25 }
    },
    { "id": "gpt", "protocol": "openai-responses", "base_url": "https://x/v1", "context_window": 1000, "subscription": true }
  ]
}"#;

#[test]
fn prompt_addendum_reads_and_defaults_to_none() {
    let setup = Setup::new();
    let dir = setup.root().join("ext");
    setup.write(
        &dir.join("providers/p.json"),
        r#"{"name":"p","models":[
            {"id":"with","protocol":"anthropic-messages","base_url":"u", "context_window": 1000,"prompt_addendum":"prompts/with.md"},
            {"id":"without","protocol":"anthropic-messages","base_url":"u", "context_window": 1000}]}"#,
    );
    let models = &read_providers(&dir).unwrap()[0].models;
    assert_eq!(
        models[0].prompt_addendum.as_deref(),
        Some("prompts/with.md")
    );
    assert_eq!(models[1].prompt_addendum, None);
}

#[test]
fn read_package_text_returns_the_files_text() {
    let setup = Setup::new();
    let dir = setup.root().join("ext");
    setup.write(&dir.join("prompts/m.md"), "Be brief.\n");
    assert_eq!(
        read_package_text(&dir, "prompts/m.md", "prompt_addendum").unwrap(),
        "Be brief.\n"
    );
}

#[test]
fn read_package_text_rejects_a_missing_file() {
    let setup = Setup::new();
    let dir = setup.root().join("ext");
    let err = read_package_text(&dir, "prompts/gone.md", "prompt_addendum").unwrap_err();
    assert_eq!(err.code(), ErrorCode::ConfigInvalid);
    let msg = err.to_string();
    assert!(msg.contains("prompt_addendum"), "{msg}");
    assert!(msg.contains("gone.md"), "{msg}");
}

#[test]
fn read_package_text_rejects_paths_outside_the_package() {
    let setup = Setup::new();
    let dir = setup.root().join("ext");
    for path in [
        "../up.md",
        "/abs.md",
        "a/../../up.md",
        "a/./b.md",
        "",
        "a\\\\b.md",
        "C:x.md",
    ] {
        let err = read_package_text(&dir, path, "prompt").unwrap_err();
        assert_eq!(err.code(), ErrorCode::ConfigInvalid, "{path}");
        let msg = err.to_string();
        assert!(msg.contains("prompt"), "{path}: {msg}");
    }
}

#[test]
#[cfg(unix)]
fn read_package_text_refuses_a_file_symlink_pointing_outside() {
    let setup = Setup::new();
    let dir = setup.root().join("ext");
    setup.write(&setup.root().join("outside/evil.md"), "evil\n");
    let link = dir.join("prompts/m.md");
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(setup.root().join("outside/evil.md"), &link).unwrap();
    let err = read_package_text(&dir, "prompts/m.md", "prompt_addendum").unwrap_err();
    assert_eq!(err.code(), ErrorCode::ConfigInvalid);
    let msg = err.to_string();
    assert!(msg.contains("prompt_addendum"), "{msg}");
    assert!(msg.contains("m.md"), "{msg}");
}

#[test]
#[cfg(unix)]
fn read_package_text_refuses_a_symlinked_parent_pointing_outside() {
    let setup = Setup::new();
    let dir = setup.root().join("ext");
    setup.write(&setup.root().join("outside/m.md"), "evil\n");
    std::fs::create_dir_all(&dir).unwrap();
    std::os::unix::fs::symlink(setup.root().join("outside"), dir.join("prompts")).unwrap();
    let err = read_package_text(&dir, "prompts/m.md", "prompt_addendum").unwrap_err();
    assert_eq!(err.code(), ErrorCode::ConfigInvalid);
    let msg = err.to_string();
    assert!(msg.contains("prompt_addendum"), "{msg}");
    assert!(msg.contains("m.md"), "{msg}");
}

#[test]
#[cfg(unix)]
fn read_package_text_reads_a_symlink_staying_inside_the_package() {
    let setup = Setup::new();
    let dir = setup.root().join("ext");
    setup.write(&dir.join("real/m.md"), "Be brief.\n");
    let link = dir.join("prompts/m.md");
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(dir.join("real/m.md"), &link).unwrap();
    assert_eq!(
        read_package_text(&dir, "prompts/m.md", "prompt_addendum").unwrap(),
        "Be brief.\n"
    );
}

#[test]
fn read_package_text_rejects_non_utf8_bytes() {
    let setup = Setup::new();
    let dir = setup.root().join("ext");
    let file = dir.join("prompts/bin.md");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, [0xff, 0xfe]).unwrap();
    let err = read_package_text(&dir, "prompts/bin.md", "prompt").unwrap_err();
    assert_eq!(err.code(), ErrorCode::ConfigInvalid);
    let msg = err.to_string();
    assert!(msg.contains("prompt"), "{msg}");
    assert!(msg.contains("bin.md"), "{msg}");
}

#[test]
fn reviewer_model_reads_from_provider_data_and_is_none_when_absent() {
    let setup = Setup::new();
    let dir = setup.root().join("ext");
    setup.write(
        &dir.join("providers/p.json"),
        r#"{"name":"p","reviewer_model":"fast","models":[]}"#,
    );
    assert_eq!(
        read_providers(&dir).unwrap()[0].reviewer_model,
        Some("fast".into())
    );
    setup.write(&dir.join("providers/p.json"), r#"{"name":"p","models":[]}"#);
    assert_eq!(read_providers(&dir).unwrap()[0].reviewer_model, None);
}

#[test]
fn the_documented_provider_data_reads() {
    let setup = Setup::new();
    let dir = setup.root().join("ext");
    setup.write(&dir.join("providers/databricks.json"), DATABRICKS);
    setup.write(&dir.join("providers/notes.txt"), "ignored");
    let providers = read_providers(&dir).unwrap();
    let [data] = providers.as_slice() else {
        panic!("{providers:?}");
    };
    assert_eq!(data.name, "databricks");
    assert_eq!(
        data.credential,
        Some(CredentialSource::Env("DATABRICKS_TOKEN".into()))
    );
    assert_eq!(data.headers["x-databricks-client"], "fiber");
    let [opus, gpt] = data.models.as_slice() else {
        panic!("{:?}", data.models);
    };
    assert_eq!(opus.protocol, Protocol::AnthropicMessages);
    assert_eq!(opus.compat["store"], false);
    assert!(opus.deferred_tools && !opus.subscription);
    assert_eq!(opus.context_window, Some(1_000_000));
    assert_eq!(opus.max_output_tokens, Some(128_000));
    assert_eq!(opus.input, ["text", "image"]);
    assert_eq!(opus.cost.as_ref().unwrap().cache_write, Some(6.25));
    assert!(opus.cost.as_ref().unwrap().tiers.is_empty());
    assert_eq!(gpt.protocol, Protocol::OpenaiResponses);
    assert!(gpt.subscription && !gpt.deferred_tools);
    assert_eq!(gpt.cost, None);
    assert_eq!(
        data.placeholders["workspace"].env.as_deref(),
        Some("DATABRICKS_HOST")
    );
    assert_eq!(opus.base_url, "https://{workspace}/ai-gateway/anthropic");
}

#[test]
fn placeholders_default_to_none_and_env_is_optional() {
    let setup = Setup::new();
    let dir = setup.root().join("ext");
    setup.write(&dir.join("providers/p.json"), r#"{"name":"p","models":[]}"#);
    assert!(read_providers(&dir).unwrap()[0].placeholders.is_empty());
    setup.write(
        &dir.join("providers/p.json"),
        r#"{"name":"p","placeholders":{"region":{}},"models":[]}"#,
    );
    let data = &read_providers(&dir).unwrap()[0];
    assert_eq!(data.placeholders["region"].env, None);
}

#[test]
fn cost_tiers_read_from_provider_data() {
    let setup = Setup::new();
    let dir = setup.root().join("ext");
    setup.write(
        &dir.join("providers/p.json"),
        r#"{
  "name": "p",
  "models": [
    {
      "id": "tiered",
      "protocol": "openai-responses",
      "base_url": "https://x/v1", "context_window": 1000,
      "cost": {
        "input": 2.0,
        "output": 10.0,
        "cache_read": 0.1,
        "cache_write": 2.5,
        "tiers": [
          {
            "input_tokens_above": 272000,
            "input": 4.0,
            "output": 15.0,
            "cache_read": 0.2,
            "cache_write": 5.0
          }
        ]
      }
    },
    {
      "id": "flat",
      "protocol": "openai-responses",
      "base_url": "https://x/v1", "context_window": 1000,
      "cost": { "input": 1.0, "output": 2.0 }
    }
  ]
}"#,
    );
    let models = &read_providers(&dir).unwrap()[0].models;
    let [tiered, flat] = models.as_slice() else {
        panic!("{models:?}");
    };
    let tiered_cost = tiered.cost.as_ref().unwrap();
    let [tier] = tiered_cost.tiers.as_slice() else {
        panic!("{tiered_cost:?}");
    };
    assert_eq!(tier.input_tokens_above, 272_000);
    assert_eq!(tier.input, 4.0);
    assert_eq!(tier.output, 15.0);
    assert_eq!(tier.cache_read, 0.2);
    assert_eq!(tier.cache_write, 5.0);
    assert!(flat.cost.as_ref().unwrap().tiers.is_empty());
}

#[test]
fn a_tier_missing_a_price_is_invalid() {
    let setup = Setup::new();
    let dir = setup.root().join("ext");
    let full = r#""input_tokens_above":1,"input":1,"output":1,"cache_read":1,"cache_write":1"#;
    for missing in ["input", "output", "cache_read", "cache_write"] {
        let tier: Vec<&str> = full
            .split(',')
            .filter(|kv| !kv.starts_with(&format!("\"{missing}\"")))
            .collect();
        setup.write(
            &dir.join("providers/p.json"),
            &format!(
                r#"{{"name":"p","models":[{{"id":"m","protocol":"openai-responses","base_url":"u", "context_window": 1000,"cost":{{"input":1,"output":1,"tiers":[{{{}}}]}}}}]}}"#,
                tier.join(",")
            ),
        );
        let err = read_providers(&dir).unwrap_err();
        assert_eq!(err.code(), ErrorCode::ConfigInvalid, "{missing}");
    }
}

#[test]
fn every_protocol_name_reads_and_an_unknown_one_is_invalid() {
    let setup = Setup::new();
    let dir = setup.root().join("ext");
    for (name, protocol) in [
        ("anthropic-messages", Protocol::AnthropicMessages),
        ("openai-completions", Protocol::OpenaiCompletions),
        ("openai-responses", Protocol::OpenaiResponses),
        ("google-generative-ai", Protocol::GoogleGenerativeAi),
        ("bedrock-converse", Protocol::BedrockConverse),
    ] {
        setup.write(
            &dir.join("providers/p.json"),
            &format!(
                r#"{{"name":"p","models":[{{"id":"m","protocol":"{name}","base_url":"u", "context_window": 1000}}]}}"#
            ),
        );
        assert_eq!(
            read_providers(&dir).unwrap()[0].models[0].protocol,
            protocol
        );
    }
    setup.write(
        &dir.join("providers/p.json"),
        r#"{"name":"p","models":[{"id":"m","protocol":"smoke-signals","base_url":"u", "context_window": 1000}]}"#,
    );
    let err = read_providers(&dir).unwrap_err();
    assert_eq!(err.code(), ErrorCode::ConfigInvalid);
    assert!(!err.to_string().contains("smoke-signals"), "{err}");
}

/// `scripted` is built into the binary (`docs/model-routing.md`, "The
/// scripted provider"): no package's data may declare it. It fails exactly
/// as an unknown protocol name of the same length does, at the `protocol`
/// value.
#[test]
fn a_model_declaring_the_scripted_protocol_is_invalid_as_an_unknown_one() {
    let setup = Setup::new();
    let dir = setup.root().join("ext");
    let data = |protocol: &str| {
        format!(
            r#"{{"name":"p","models":[{{"id":"m","protocol":"{protocol}","base_url":"u", "context_window": 1000}}]}}"#
        )
    };
    setup.write(&dir.join("providers/p.json"), &data("scripted"));
    let err = read_providers(&dir).unwrap_err();
    assert_eq!(err.code(), ErrorCode::ConfigInvalid);
    setup.write(&dir.join("providers/p.json"), &data("unknownx"));
    assert_eq!(
        err.to_string(),
        read_providers(&dir).unwrap_err().to_string()
    );
    setup.write(
        &dir.join("providers/p.json"),
        r#"{"name":"p","models":[{"id":"m","protocol":"openai-responses","base_url":"u", "context_window": 1000}]}"#,
    );
    assert_eq!(
        read_providers(&dir).unwrap()[0].models[0].protocol,
        Protocol::OpenaiResponses
    );
}

#[test]
fn a_provider_file_named_for_another_provider_is_invalid() {
    let setup = Setup::new();
    let dir = setup.root().join("ext");
    setup.write(
        &dir.join("providers/openrouter.json"),
        r#"{"name":"evil","models":[]}"#,
    );
    let err = read_providers(&dir).unwrap_err();
    assert_eq!(err.code(), ErrorCode::ConfigInvalid);
}

#[test]
fn providers_read_in_file_name_order_and_none_is_empty() {
    let setup = Setup::new();
    let dir = setup.root().join("ext");
    assert!(read_providers(&dir).unwrap().is_empty());
    for name in ["b", "a", "c"] {
        setup.write(
            &dir.join(format!("providers/{name}.json")),
            &format!(r#"{{"name":"{name}","models":[]}}"#),
        );
    }
    let names: Vec<String> = read_providers(&dir)
        .unwrap()
        .into_iter()
        .map(|p| p.name)
        .collect();
    assert_eq!(names, ["a", "b", "c"]);
}

#[test]
fn the_documented_manifest_reads() {
    let setup = Setup::new();
    let dir = setup.root().join("ext");
    setup.write(
        &dir.join("extension.json"),
        r#"{"name": "github.com/acme/fiber-acme", "version": "v1.4.0", "fiber": "0.3.0",
            "api": 1, "depends": {"github.com/acme/oauth-helper": "v1.2.0"},
            "repo_settings": ["workspace_url"], "prompt": "prompt.md", "memory_mib": 8,
            "opening": {"machine": ["index.md"], "project": ["notes.md"], "budget_bytes": 25000}}"#,
    );
    let manifest = read_manifest(&dir).unwrap();
    assert_eq!(manifest.name, "github.com/acme/fiber-acme");
    assert_eq!(manifest.version, "v1.4.0");
    assert_eq!(manifest.fiber, "0.3.0");
    assert_eq!(manifest.api, 1);
    assert_eq!(manifest.depends["github.com/acme/oauth-helper"], "v1.2.0");
    assert_eq!(manifest.memory_mib, Some(8));
    let opening = manifest.opening.unwrap();
    assert_eq!(opening.machine, ["index.md"]);
    assert_eq!(opening.project, ["notes.md"]);
    assert_eq!(opening.budget_bytes, Some(25_000));
}

#[test]
fn memory_mib_absent_or_null_is_none_and_a_positive_value_reads() {
    let setup = Setup::new();
    let dir = setup.root().join("ext");
    let base = r#"{"name": "a", "version": "v1.0.0", "fiber": "0.1.0", "api": 1}"#;
    setup.write(&dir.join("extension.json"), base);
    assert_eq!(read_manifest(&dir).unwrap().memory_mib, None);
    setup.write(
        &dir.join("extension.json"),
        r#"{"name": "a", "version": "v1.0.0", "fiber": "0.1.0", "api": 1, "memory_mib": null}"#,
    );
    assert_eq!(read_manifest(&dir).unwrap().memory_mib, None);
    setup.write(
        &dir.join("extension.json"),
        r#"{"name": "a", "version": "v1.0.0", "fiber": "0.1.0", "api": 1, "memory_mib": 8}"#,
    );
    assert_eq!(read_manifest(&dir).unwrap().memory_mib, Some(8));
}

#[test]
fn replaces_lists_the_built_ins_and_defaults_to_none() {
    let setup = Setup::new();
    let dir = setup.root().join("ext");
    let base = r#"{"name": "a", "version": "v1.0.0", "fiber": "0.1.0", "api": 1"#;
    setup.write(&dir.join("extension.json"), &format!("{base}}}"));
    assert!(read_manifest(&dir).unwrap().replaces.is_empty());
    setup.write(
        &dir.join("extension.json"),
        &format!(r#"{base}, "replaces": ["web_search", "shell"]}}"#),
    );
    assert_eq!(
        read_manifest(&dir).unwrap().replaces,
        ["web_search", "shell"]
    );
}

#[test]
fn memory_mib_zero_or_overflowing_bytes_is_invalid() {
    let setup = Setup::new();
    let dir = setup.root().join("ext");
    for memory_mib in [0_u64, 17_592_186_044_416] {
        setup.write(
            &dir.join("extension.json"),
            &format!(
                r#"{{"name": "a", "version": "v1.0.0", "fiber": "0.1.0", "api": 1, "memory_mib": {memory_mib}}}"#
            ),
        );
        let err = read_manifest(&dir).unwrap_err();
        assert_eq!(err.code(), ErrorCode::ConfigInvalid);
        let msg = err.to_string();
        assert!(msg.contains("memory_mib"), "{msg}");
        assert!(msg.contains("extension.json"), "{msg}");
    }
}

#[test]
fn a_manifest_without_depends_has_none() {
    let setup = Setup::new();
    let dir = setup.root().join("ext");
    setup.write(
        &dir.join("extension.json"),
        r#"{"name": "a", "version": "v1.0.0", "fiber": "0.1.0", "api": 1}"#,
    );
    assert!(read_manifest(&dir).unwrap().depends.is_empty());
}

#[test]
fn a_manifest_that_is_not_json_or_lacks_a_field_is_invalid() {
    let setup = Setup::new();
    let dir = setup.root().join("ext");
    for text in [
        r#"{"name": "a", "fiber": "0.1.0", "api": 1,}"#,
        r#"{"name": "a", "api": 1}"#,
        r#"{"name": "a", "fiber": "0.1.0", "api": 1}"#,
        r#"{"name": "a", "version": "v1", "fiber": "0.1.0", "api": 1, "depends": ["b"]}"#,
        r#"{"name": "a", "fiber": "0.1.0", "api": "1"}"#,
    ] {
        setup.write(&dir.join("extension.json"), text);
        let err = read_manifest(&dir).unwrap_err();
        assert_eq!(err.code(), ErrorCode::ConfigInvalid, "{text}");
        assert!(err.to_string().contains("extension.json"), "{err}");
    }
    let err = read_manifest(&setup.root().join("nowhere")).unwrap_err();
    assert_eq!(err.code(), ErrorCode::IoFailed);
}

#[test]
fn web_search_reads_and_defaults_to_none() {
    let setup = Setup::new();
    let dir = setup.root().join("ext");
    setup.write(
        &dir.join("providers/p.json"),
        r#"{"name":"p","models":[
            {"id":"s","protocol":"anthropic-messages","base_url":"u", "context_window": 1000,"web_search":"web_search_20250305"},
            {"id":"plain","protocol":"anthropic-messages","base_url":"u", "context_window": 1000}]}"#,
    );
    let models = &read_providers(&dir).unwrap()[0].models;
    assert_eq!(models[0].web_search.as_deref(), Some("web_search_20250305"));
    assert_eq!(models[1].web_search, None);
}

#[test]
fn reserved_body_fields_match_the_documented_table() {
    assert_eq!(
        Protocol::AnthropicMessages.reserved_body_fields(),
        [
            "model",
            "system",
            "messages",
            "tools",
            "tool_choice",
            "stream"
        ]
    );
    assert_eq!(
        Protocol::OpenaiCompletions.reserved_body_fields(),
        ["model", "messages", "tools", "tool_choice", "stream"]
    );
    assert_eq!(
        Protocol::OpenaiResponses.reserved_body_fields(),
        [
            "model",
            "instructions",
            "input",
            "tools",
            "tool_choice",
            "stream"
        ]
    );
    assert_eq!(
        Protocol::GoogleGenerativeAi.reserved_body_fields(),
        ["systemInstruction", "contents", "tools", "toolConfig"]
    );
    assert_eq!(
        Protocol::BedrockConverse.reserved_body_fields(),
        ["system", "messages", "toolConfig"]
    );
    assert!(Protocol::Scripted.reserved_body_fields().is_empty());
}

#[test]
fn reads_web_search_reads_one_type_per_protocol() {
    let protocols = [
        Protocol::AnthropicMessages,
        Protocol::OpenaiCompletions,
        Protocol::OpenaiResponses,
        Protocol::GoogleGenerativeAi,
        Protocol::BedrockConverse,
        Protocol::Scripted,
    ];
    let kinds = [
        "web_search_20250305",
        "web_search",
        "google_search",
        "web_search_preview",
        "",
    ];
    for protocol in protocols {
        for kind in kinds {
            let expected = matches!(
                (protocol, kind),
                (Protocol::AnthropicMessages, "web_search_20250305")
                    | (Protocol::OpenaiResponses, "web_search")
            );
            assert_eq!(
                protocol.reads_web_search(kind),
                expected,
                "{protocol:?} reads {kind:?}"
            );
        }
    }
}

#[test]
fn opening_absent_is_none_and_present_reads_all_three_fields() {
    let setup = Setup::new();
    let dir = setup.root().join("ext");
    setup.write(
        &dir.join("extension.json"),
        r#"{"name": "a", "version": "v1.0.0", "fiber": "0.1.0", "api": 1}"#,
    );
    assert!(read_manifest(&dir).unwrap().opening.is_none());
    setup.write(
        &dir.join("extension.json"),
        r#"{"name": "a", "version": "v1.0.0", "fiber": "0.1.0", "api": 1,
            "opening": {"machine": ["a/b.md"], "project": ["n.md"], "budget_bytes": 10}}"#,
    );
    let opening = read_manifest(&dir).unwrap().opening.unwrap();
    assert_eq!(opening.machine, ["a/b.md"]);
    assert_eq!(opening.project, ["n.md"]);
    assert_eq!(opening.budget_bytes, Some(10));
}

#[test]
fn opening_rejects_paths_outside_the_data_directory() {
    let setup = Setup::new();
    let dir = setup.root().join("ext");
    for path in ["/abs.md", "../up.md", "a/../../up.md", ".", "", "a/./b.md"] {
        for key in ["machine", "project"] {
            setup.write(
                &dir.join("extension.json"),
                &format!(
                    r#"{{"name": "a", "version": "v1.0.0", "fiber": "0.1.0", "api": 1, "opening": {{"{key}": ["{path}"]}}}}"#
                ),
            );
            let err = read_manifest(&dir).unwrap_err();
            assert_eq!(err.code(), ErrorCode::ConfigInvalid, "{key} {path}");
            let msg = err.to_string();
            assert!(msg.contains("opening"), "{msg}");
            assert!(
                msg.contains("paths relative to the data directory and inside it"),
                "{msg}"
            );
        }
    }
}

#[test]
fn thinking_declaration_parses_and_an_unknown_level_fails_the_parse() {
    use contract::ThinkingLevel::{High, Low};
    let setup = Setup::new();
    let dir = setup.root().join("ext");
    setup.write(
        &dir.join("providers/p.json"),
        r#"{"name":"p","models":[
            {"id":"m","protocol":"anthropic-messages","base_url":"u", "context_window": 1000,
             "thinking_levels": ["low", "high"], "thinking_default": "high"}]}"#,
    );
    let models = &read_providers(&dir).unwrap()[0].models;
    assert_eq!(models[0].thinking_levels, vec![Low, High]);
    assert_eq!(models[0].thinking_default, Some(High));
    setup.write(
        &dir.join("providers/p.json"),
        r#"{"name":"p","models":[
            {"id":"m","protocol":"anthropic-messages","base_url":"u", "context_window": 1000,
             "thinking_levels": ["turbo"]}]}"#,
    );
    let err = read_providers(&dir).unwrap_err();
    assert_eq!(err.code(), ErrorCode::ConfigInvalid);
}

const BASE: &str = r#"{"name": "a", "version": "v1.0.0", "fiber": "0.1.0", "api": 1"#;

#[test]
fn secrets_default_to_none() {
    let setup = Setup::new();
    let dir = setup.root().join("ext");
    setup.write(&dir.join("extension.json"), &format!("{BASE}}}"));
    assert!(read_manifest(&dir).unwrap().secrets.is_empty());
}

#[test]
fn secrets_read_in_order_and_a_repeated_name_is_kept() {
    let setup = Setup::new();
    let dir = setup.root().join("ext");
    setup.write(
        &dir.join("extension.json"),
        &format!(r#"{BASE}, "secrets": ["acme.api_key", "acme.url", "acme.api_key"]}}"#),
    );
    assert_eq!(
        read_manifest(&dir).unwrap().secrets,
        ["acme.api_key", "acme.url", "acme.api_key"]
    );
}

#[test]
fn a_secret_name_with_dots_reads() {
    let setup = Setup::new();
    let dir = setup.root().join("ext");
    setup.write(
        &dir.join("extension.json"),
        &format!(r#"{BASE}, "secrets": [".hidden", "mcp.server.KEY"]}}"#),
    );
    assert_eq!(
        read_manifest(&dir).unwrap().secrets,
        [".hidden", "mcp.server.KEY"]
    );
}

#[test]
fn login_reads_browser_and_defaults_to_none() {
    let setup = Setup::new();
    let dir = setup.root().join("ext");
    setup.write(
        &dir.join("providers/p.json"),
        r#"{"name":"p","login":"browser","models":[]}"#,
    );
    assert_eq!(read_providers(&dir).unwrap()[0].login, Some(Login::Browser));
    setup.write(&dir.join("providers/p.json"), r#"{"name":"p","models":[]}"#);
    assert_eq!(read_providers(&dir).unwrap()[0].login, None);
}

#[test]
fn an_unknown_login_is_a_provider_data_error() {
    let setup = Setup::new();
    let dir = setup.root().join("ext");
    setup.write(
        &dir.join("providers/p.json"),
        r#"{"name":"p","login":"key","models":[]}"#,
    );
    let err = read_providers(&dir).unwrap_err();
    assert_eq!(err.code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("a provider's data"), "{err}");
}

#[test]
fn a_secret_name_that_is_not_one_file_name_is_invalid() {
    let setup = Setup::new();
    let dir = setup.root().join("ext");
    // Each entry is JSON string text, so the NUL is spelled as JSON escapes it.
    for name in ["", ".", "..", "a/b", "/abs", r"a\u0000b"] {
        setup.write(
            &dir.join("extension.json"),
            &format!(r#"{BASE}, "secrets": ["ok", "{name}"]}}"#),
        );
        let err = read_manifest(&dir).unwrap_err();
        assert!(
            matches!(&err, config::ConfigError::WrongType { key, .. } if key == "secrets"),
            "{name}: {err:?}"
        );
        assert_eq!(err.code(), ErrorCode::ConfigInvalid, "{name}");
        assert!(err.to_string().contains("extension.json"), "{err}");
    }
}
