//! `docs/configuration.md`, "An extension's manifest" and "A provider's
//! data": both files are strict JSON in the documented shape, and a file that
//! does not fit is `config_invalid`.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]

mod common;

use common::Setup;
use config::{CredentialSource, Protocol, read_manifest, read_providers};
use contract::ErrorCode;

const DATABRICKS: &str = r#"{
  "name": "databricks",
  "credential": { "env": "DATABRICKS_TOKEN" },
  "headers": { "x-databricks-client": "fiber" },
  "models": [
    {
      "id": "databricks-claude-opus-5",
      "protocol": "anthropic-messages",
      "base_url": "https://example.cloud.databricks.com/ai-gateway/anthropic",
      "compat": { "store": false },
      "deferred_tools": true,
      "extra_body": {},
      "context_window": 1000000,
      "max_output_tokens": 128000,
      "input": ["text", "image"],
      "cost": { "input": 5.0, "output": 25.0, "cache_read": 0.5, "cache_write": 6.25 }
    },
    { "id": "gpt", "protocol": "openai-responses", "base_url": "https://x/v1", "subscription": true }
  ]
}"#;

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
      "base_url": "https://x/v1",
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
      "base_url": "https://x/v1",
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
    assert_eq!(tier.cache_read, Some(0.2));
    assert_eq!(tier.cache_write, Some(5.0));
    assert!(flat.cost.as_ref().unwrap().tiers.is_empty());
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
                r#"{{"name":"p","models":[{{"id":"m","protocol":"{name}","base_url":"u"}}]}}"#
            ),
        );
        assert_eq!(
            read_providers(&dir).unwrap()[0].models[0].protocol,
            protocol
        );
    }
    setup.write(
        &dir.join("providers/p.json"),
        r#"{"name":"p","models":[{"id":"m","protocol":"smoke-signals","base_url":"u"}]}"#,
    );
    let err = read_providers(&dir).unwrap_err();
    assert_eq!(err.code(), ErrorCode::ConfigInvalid);
    assert!(!err.to_string().contains("smoke-signals"), "{err}");
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
            "repo_settings": ["workspace_url"], "prompt": "prompt.md"}"#,
    );
    let manifest = read_manifest(&dir).unwrap();
    assert_eq!(manifest.name, "github.com/acme/fiber-acme");
    assert_eq!(manifest.version, "v1.4.0");
    assert_eq!(manifest.fiber, "0.3.0");
    assert_eq!(manifest.api, 1);
    assert_eq!(manifest.depends["github.com/acme/oauth-helper"], "v1.2.0");
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
