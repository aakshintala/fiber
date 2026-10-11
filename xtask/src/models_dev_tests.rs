//! Unit tests for the models.dev generator: one small inline catalog
//! per rule.

use std::time::Duration;

use serde_json::json;

use crate::test_dir::TestDir;

use super::*;

/// A source model that passes every filter, with `extra` merged over the
/// defaults.
fn tool_model(extra: Value) -> Value {
    let mut model = json!({
        "tool_call": true,
        "limit": {"context": 1000, "output": 100},
        "modalities": {"input": ["text"], "output": ["text"]},
        "cost": {"input": 1, "output": 2},
    });
    for (key, value) in extra.as_object().unwrap() {
        model[key.as_str()] = value.clone();
    }
    model
}

/// A catalog holding `models` under `source`.
fn catalog(source: &str, models: &[(&str, Value)]) -> Value {
    let mut map = Map::new();
    for (id, model) in models {
        map.insert((*id).to_owned(), model.clone());
    }
    let mut provider = Map::new();
    provider.insert("models".to_owned(), Value::Object(map));
    let mut catalog = Map::new();
    catalog.insert(source.to_owned(), Value::Object(provider));
    Value::Object(catalog)
}

/// A package on a fixed protocol, writing `providers/t/providers/t.json`.
fn fixed_package(source: &'static str, protocol: &'static str) -> Package {
    Package {
        path: "providers/t/providers/t.json",
        source,
        provider: r#"{"name":"t"}"#,
        protocol: ProtocolRule::Fixed(protocol),
        base_url: "https://t/v1",
        drop_deprecated: false,
        skip: &[],
        only: &[],
        protocol_overrides: &[],
        every_model: r#"{}"#,
        by_protocol: &[],
        by_model: &[],
        thinking: &[],
    }
}

/// A package mapping protocols from models.dev's `provider.npm`.
fn npm_package(source: &'static str) -> Package {
    Package {
        protocol: ProtocolRule::ByNpm,
        ..fixed_package(source, "openai-completions")
    }
}

/// The generated models of `output`, in file order.
fn generated(output: &Output) -> Vec<Value> {
    let file: Value = serde_json::from_str(&output.text).unwrap();
    file["models"].as_array().unwrap().clone()
}

/// The generated model `id` in `output`, if it lists one.
fn generated_model(output: &Output, id: &str) -> Option<Value> {
    generated(output)
        .into_iter()
        .find(|model| model["id"] == id)
}

/// Generates one package's file from one source's models.
fn generate_one(package: &Package, models: &[(&str, Value)]) -> (Output, Vec<String>) {
    let input = catalog(package.source, models);
    let (mut outputs, left_out) = generate(&input, std::slice::from_ref(package)).unwrap();
    assert_eq!(outputs.len(), 1);
    (outputs.pop().unwrap(), left_out)
}

/// The error generating one package's file gives.
fn generate_err(package: &Package, input: &Value) -> String {
    match generate(input, std::slice::from_ref(package)) {
        Err(error) => error,
        Ok(_) => panic!("generate unexpectedly succeeded"),
    }
}

#[test]
fn tool_call_false_and_absent_are_dropped() {
    let package = fixed_package("s", "openai-responses");
    let mut missing = tool_model(json!({"tool_call": false}));
    missing.as_object_mut().unwrap().remove("tool_call");
    let (output, left_out) = generate_one(
        &package,
        &[
            ("off", tool_model(json!({"tool_call": false}))),
            ("missing", missing),
            ("on", tool_model(json!({}))),
        ],
    );
    assert_eq!(output.models, 1);
    assert!(generated_model(&output, "on").is_some());
    assert_eq!(left_out.len(), 2);
}

#[test]
fn tool_call_as_a_non_bool_is_dropped() {
    let package = fixed_package("s", "openai-responses");
    let (output, _) = generate_one(
        &package,
        &[("odd", tool_model(json!({"tool_call": "yes"})))],
    );
    assert_eq!(output.models, 0);
}

/// A source model with no `modalities` key at all.
fn bare_modalities() -> Value {
    let mut model = tool_model(json!({}));
    model.as_object_mut().unwrap().remove("modalities");
    model
}

#[test]
fn output_without_text_is_dropped() {
    let package = fixed_package("s", "openai-responses");
    let (output, left_out) = generate_one(
        &package,
        &[
            (
                "images",
                tool_model(json!({"modalities": {"input": ["text"], "output": ["image"]}})),
            ),
            (
                "absent",
                tool_model(json!({"modalities": {"input": ["text"]}})),
            ),
            ("none", bare_modalities()),
            (
                "voice",
                tool_model(json!({"modalities": {"input": ["text"], "output": ["text", "audio"]}})),
            ),
        ],
    );
    assert!(generated_model(&output, "voice").is_some());
    assert_eq!(output.models, 1);
    assert_eq!(left_out.len(), 3);
}

#[test]
fn only_keeps_exactly_its_ids_silently() {
    let mut package = fixed_package("s", "openai-responses");
    package.only = &["b"];
    let (output, left_out) = generate_one(
        &package,
        &[
            ("a", tool_model(json!({}))),
            ("b", tool_model(json!({}))),
            ("c", tool_model(json!({}))),
        ],
    );
    assert!(generated_model(&output, "b").is_some());
    assert_eq!(output.models, 1);
    assert!(left_out.is_empty(), "{left_out:?}");
}

#[test]
fn empty_only_keeps_every_passing_model() {
    let package = fixed_package("s", "openai-responses");
    let (output, left_out) = generate_one(
        &package,
        &[("a", tool_model(json!({}))), ("b", tool_model(json!({})))],
    );
    assert!(generated_model(&output, "a").is_some());
    assert!(generated_model(&output, "b").is_some());
    assert_eq!(output.models, 2);
    assert!(left_out.is_empty(), "{left_out:?}");
}

#[test]
fn an_only_id_absent_from_the_catalog_is_an_error() {
    let mut package = fixed_package("s", "openai-responses");
    package.only = &["ghost"];
    let input = catalog(package.source, &[("m", tool_model(json!({})))]);
    assert_eq!(
        generate_err(&package, &input),
        "t: only entry for `ghost` matches no generated model"
    );
}

#[test]
fn an_only_id_filtered_out_is_an_error() {
    let mut package = fixed_package("s", "openai-responses");
    package.only = &["off"];
    let input = catalog(
        package.source,
        &[("off", tool_model(json!({"tool_call": false})))],
    );
    assert_eq!(
        generate_err(&package, &input),
        "t: only entry for `off` matches no generated model"
    );
}

#[test]
fn deprecated_is_dropped_only_where_flagged() {
    let mut flagged = npm_package("s");
    flagged.drop_deprecated = true;
    let deprecated = tool_model(json!({"status": "deprecated"}));
    let (output, _) = generate_one(&flagged, &[("old", deprecated.clone())]);
    assert_eq!(output.models, 0);
    let plain = npm_package("s");
    let (output, _) = generate_one(&plain, &[("old", deprecated)]);
    assert_eq!(output.models, 1);
}

#[test]
fn the_skip_list_drops_only_its_ids() {
    let mut package = fixed_package("s", "openai-responses");
    package.skip = &["gpt-5.6"];
    let (output, _) = generate_one(
        &package,
        &[
            ("gpt-5.6", tool_model(json!({}))),
            ("gpt-5.6-sol", tool_model(json!({}))),
        ],
    );
    assert!(generated_model(&output, "gpt-5.6-sol").is_some());
    assert_eq!(output.models, 1);
}

#[test]
fn pre_3_gemini_is_dropped_on_google_generative_ai_only() {
    let google = fixed_package("s", "google-generative-ai");
    let (output, _) = generate_one(
        &google,
        &[
            ("gemini-2.5-pro", tool_model(json!({}))),
            (
                "gemini-2.5-computer-use-preview-10-2025",
                tool_model(json!({})),
            ),
            ("gemini-3-flash-preview", tool_model(json!({}))),
            ("gemini-3.1-pro", tool_model(json!({}))),
            ("gemini-flash-latest", tool_model(json!({}))),
            ("gemma-4-31b-it", tool_model(json!({}))),
        ],
    );
    assert_eq!(output.models, 4);
    for id in [
        "gemini-3-flash-preview",
        "gemini-3.1-pro",
        "gemini-flash-latest",
        "gemma-4-31b-it",
    ] {
        assert!(generated_model(&output, id).is_some(), "{id}");
    }
    let npm = npm_package("s");
    let (output, _) = generate_one(&npm, &[("gemini-2.5-flash", tool_model(json!({})))]);
    let model = generated_model(&output, "gemini-2.5-flash").unwrap();
    assert_eq!(model["protocol"], "openai-completions");
}

#[test]
fn npm_maps_each_arm() {
    let package = npm_package("s");
    let npm = |npm: Value| tool_model(json!({"provider": {"npm": npm}}));
    let (output, _) = generate_one(
        &package,
        &[
            ("a", npm(json!("@ai-sdk/openai"))),
            ("b", npm(json!("@ai-sdk/anthropic"))),
            ("c", npm(json!("@ai-sdk/google"))),
            ("d", npm(json!("@ai-sdk/mistral"))),
            ("e", npm(json!(null))),
            ("f", tool_model(json!({"no_provider": true}))),
        ],
    );
    for (id, protocol) in [
        ("a", "openai-responses"),
        ("b", "anthropic-messages"),
        ("c", "google-generative-ai"),
        ("d", "openai-completions"),
        ("e", "openai-completions"),
        ("f", "openai-completions"),
    ] {
        assert_eq!(generated_model(&output, id).unwrap()["protocol"], protocol);
    }
}

#[test]
fn a_protocol_override_applies_on_its_package_only() {
    let mut over = npm_package("s");
    over.protocol_overrides = &[("minimax-m2.7", "openai-completions")];
    let anthropic = tool_model(json!({"provider": {"npm": "@ai-sdk/anthropic"}}));
    let (output, _) = generate_one(&over, &[("minimax-m2.7", anthropic.clone())]);
    assert_eq!(
        generated_model(&output, "minimax-m2.7").unwrap()["protocol"],
        "openai-completions"
    );
    let plain = npm_package("s");
    let (output, _) = generate_one(&plain, &[("minimax-m2.7", anthropic)]);
    assert_eq!(
        generated_model(&output, "minimax-m2.7").unwrap()["protocol"],
        "anthropic-messages"
    );
}

#[test]
fn context_absent_and_zero_are_left_out_with_a_line() {
    let package = fixed_package("s", "openai-responses");
    let (output, left_out) = generate_one(
        &package,
        &[
            ("absent", tool_model(json!({"limit": {"output": 10}}))),
            (
                "zero",
                tool_model(json!({"limit": {"context": 0, "output": 10}})),
            ),
            (
                "one",
                tool_model(json!({"limit": {"context": 1, "output": 10}})),
            ),
        ],
    );
    assert!(generated_model(&output, "one").is_some());
    assert_eq!(output.models, 1);
    assert_eq!(
        left_out,
        [
            "models-dev: t/absent has no context window in models.dev; left out",
            "models-dev: t/zero has no context window in models.dev; left out",
        ]
    );
}

#[test]
fn output_absent_and_zero_give_no_max_output_tokens() {
    let package = fixed_package("s", "openai-responses");
    let (output, _) = generate_one(
        &package,
        &[
            ("absent", tool_model(json!({"limit": {"context": 5}}))),
            (
                "zero",
                tool_model(json!({"limit": {"context": 5, "output": 0}})),
            ),
            (
                "capped",
                tool_model(json!({"limit": {"context": 5, "output": 7}})),
            ),
        ],
    );
    assert!(generated_model(&output, "absent").unwrap()["max_output_tokens"].is_null());
    assert!(generated_model(&output, "zero").unwrap()["max_output_tokens"].is_null());
    assert_eq!(
        generated_model(&output, "capped").unwrap()["max_output_tokens"],
        7
    );
}

#[test]
fn input_lists_image_only_when_listed() {
    let package = fixed_package("s", "openai-responses");
    let (output, _) = generate_one(
        &package,
        &[
            (
                "seeing",
                tool_model(json!({"modalities": {"input": ["text", "image"], "output": ["text"]}})),
            ),
            ("plain", tool_model(json!({}))),
        ],
    );
    assert_eq!(
        generated_model(&output, "seeing").unwrap()["input"],
        json!(["text", "image"])
    );
    assert_eq!(
        generated_model(&output, "plain").unwrap()["input"],
        json!(["text"])
    );
}

#[test]
fn cost_absent_and_empty_give_none() {
    let package = fixed_package("s", "openai-responses");
    let bare = json!({
        "tool_call": true,
        "limit": {"context": 1000, "output": 100},
        "modalities": {"input": ["text"], "output": ["text"]},
    });
    let mut empty = bare.clone();
    empty["cost"] = json!({});
    let (output, _) = generate_one(&package, &[("absent", bare), ("empty", empty)]);
    for id in ["absent", "empty"] {
        assert!(
            generated_model(&output, id).unwrap()["cost"].is_null(),
            "{id}"
        );
    }
}

#[test]
fn cost_with_only_input_gives_output_zero() {
    let package = fixed_package("s", "openai-responses");
    let (output, _) = generate_one(
        &package,
        &[("half", tool_model(json!({"cost": {"input": 2}})))],
    );
    assert_eq!(
        generated_model(&output, "half").unwrap()["cost"],
        json!({"input": 2, "output": 0})
    );
}

#[test]
fn cache_prices_are_written_only_when_listed() {
    let package = fixed_package("s", "openai-responses");
    let (output, _) = generate_one(
        &package,
        &[(
            "cached",
            tool_model(json!({"cost": {"input": 1, "output": 2, "cache_read": 0.1}})),
        )],
    );
    assert_eq!(
        generated_model(&output, "cached").unwrap()["cost"],
        json!({"input": 1, "output": 2, "cache_read": 0.1})
    );
}

#[test]
fn only_context_tiers_with_a_size_are_kept() {
    let package = fixed_package("s", "openai-responses");
    let (output, _) = generate_one(
        &package,
        &[(
            "tiered",
            tool_model(json!({"cost": {
                "input": 4, "output": 15,
                "tiers": [
                    {"input": 1, "tier": {"type": "input", "size": 5}},
                    {"input": 2, "tier": {"type": "context"}},
                    {"input": 3, "tier": {"type": "context", "size": 10}},
                ],
            }})),
        )],
    );
    assert_eq!(
        generated_model(&output, "tiered").unwrap()["cost"],
        json!({
            "input": 4, "output": 15,
            "tiers": [
                {"input": 3, "output": 15, "cache_read": 0, "cache_write": 0,
                 "input_tokens_above": 10},
            ],
        })
    );
}

#[test]
fn a_missing_tier_rate_falls_back_to_base_then_zero() {
    let package = fixed_package("s", "openai-responses");
    let (output, _) = generate_one(
        &package,
        &[
            (
                "fallback",
                tool_model(json!({"cost": {
                    "input": 4, "output": 15, "cache_read": 0.2,
                    "tiers": [{"tier": {"type": "context", "size": 1}}],
                }})),
            ),
            (
                "skipped",
                tool_model(json!({"cost": {
                    "input": 4, "output": 15,
                    "tiers": [{"input": 1, "tier": {"type": "input", "size": 1}}],
                }})),
            ),
        ],
    );
    assert_eq!(
        generated_model(&output, "fallback").unwrap()["cost"]["tiers"],
        json!([{
            "input": 4, "output": 15, "cache_read": 0.2, "cache_write": 0,
            "input_tokens_above": 1,
        }])
    );
    assert!(
        generated_model(&output, "skipped").unwrap()["cost"]
            .get("tiers")
            .is_none()
    );
}

#[test]
fn thinking_entries_name_snapshot_models_and_known_levels() {
    let snapshot = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/models-dev.json"
    ))
    .unwrap();
    let snapshot: Value = serde_json::from_str(&snapshot).unwrap();
    const VOCABULARY: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];
    let mut listed = 0;
    for package in &PACKAGES {
        let models = &snapshot[package.source]["models"];
        for (id, levels, default) in package.thinking {
            listed += 1;
            assert!(
                models.get(*id).is_some(),
                "{id} is not a models.dev model under `{}`",
                package.source
            );
            assert!(!levels.is_empty(), "{id} lists no thinking level");
            for level in *levels {
                assert!(
                    VOCABULARY.contains(level),
                    "{id} lists `{level}`, outside the thinking vocabulary"
                );
            }
            if let Some(default) = default {
                assert!(
                    levels.contains(default),
                    "{id} defaults to `{default}`, outside its listed levels"
                );
            }
        }
    }
    assert!(listed > 0, "no package lists a thinking entry");
}

#[test]
fn the_thinking_layer_merges_levels_and_its_default() {
    let mut package = fixed_package("s", "openai-responses");
    package.thinking = &[("m", &["low", "high"], Some("low")), ("n", &["high"], None)];
    let (output, _) = generate_one(
        &package,
        &[("m", tool_model(json!({}))), ("n", tool_model(json!({})))],
    );
    let model = generated_model(&output, "m").unwrap();
    assert_eq!(model["thinking_levels"], json!(["low", "high"]));
    assert_eq!(model["thinking_default"], "low");
    let model = generated_model(&output, "n").unwrap();
    assert_eq!(model["thinking_levels"], json!(["high"]));
    assert!(model.get("thinking_default").is_none());
}

#[test]
fn a_model_without_a_thinking_entry_declares_none() {
    let mut package = fixed_package("s", "openai-responses");
    package.thinking = &[("m", &["low"], None)];
    let (output, _) = generate_one(
        &package,
        &[
            ("m", tool_model(json!({}))),
            ("plain", tool_model(json!({}))),
        ],
    );
    let plain = generated_model(&output, "plain").unwrap();
    assert!(plain.get("thinking_levels").is_none());
    assert!(plain.get("thinking_default").is_none());
}

#[test]
fn the_thinking_layer_replaces_a_by_model_one() {
    let mut package = fixed_package("s", "openai-responses");
    package.by_model = &[("m", r#"{"thinking_levels":["low"]}"#)];
    package.thinking = &[("m", &["low", "high"], None)];
    let (output, _) = generate_one(&package, &[("m", tool_model(json!({})))]);
    assert_eq!(
        generated_model(&output, "m").unwrap()["thinking_levels"],
        json!(["low", "high"])
    );
}

#[test]
fn later_table_layers_replace_earlier_keys() {
    let mut package = fixed_package("s", "p");
    package.every_model = r#"{"keep":1,"mid":1,"top":0}"#;
    package.by_protocol = &[("p", r#"{"mid":2,"high":2}"#)];
    package.by_model = &[("m", r#"{"high":3}"#)];
    let (output, _) = generate_one(&package, &[("m", tool_model(json!({})))]);
    let model = generated_model(&output, "m").unwrap();
    assert_eq!(model["keep"], 1);
    assert_eq!(model["mid"], 2);
    assert_eq!(model["high"], 3);
    assert_eq!(model["top"], 0);
}

#[test]
fn a_by_protocol_entry_no_model_uses_is_allowed() {
    let mut package = fixed_package("s", "p");
    package.by_protocol = &[("q", r#"{"never":true}"#)];
    let (output, _) = generate_one(&package, &[("m", tool_model(json!({})))]);
    assert_eq!(output.models, 1);
}

#[test]
fn a_stale_table_entry_is_an_error_naming_both() {
    let mut package = fixed_package("s", "p");
    package.by_model = &[("ghost", r#"{"x":1}"#)];
    let input = catalog(package.source, &[("m", tool_model(json!({})))]);
    assert_eq!(
        generate_err(&package, &input),
        "t: table entry for `ghost` matches no generated model"
    );
    let mut package = fixed_package("s", "p");
    package.protocol_overrides = &[("ghost", "openai-responses")];
    let input = catalog(package.source, &[("m", tool_model(json!({})))]);
    assert_eq!(
        generate_err(&package, &input),
        "t: table entry for `ghost` matches no generated model"
    );
    let mut package = fixed_package("s", "p");
    package.thinking = &[("ghost", &["low"], None)];
    let input = catalog(package.source, &[("m", tool_model(json!({})))]);
    assert_eq!(
        generate_err(&package, &input),
        "t: table entry for `ghost` matches no generated model"
    );
}

#[test]
fn a_reviewer_model_outside_the_list_is_an_error() {
    let mut package = fixed_package("s", "p");
    package.provider = r#"{"name":"t","reviewer_model":"ghost"}"#;
    let input = catalog(package.source, &[("m", tool_model(json!({})))]);
    assert_eq!(
        generate_err(&package, &input),
        "t: reviewer_model `ghost` is not a generated model"
    );
}

#[test]
fn a_reviewer_model_inside_the_list_passes() {
    let mut package = fixed_package("s", "p");
    package.provider = r#"{"name":"t","reviewer_model":"m"}"#;
    let input = catalog(package.source, &[("m", tool_model(json!({})))]);
    let (outputs, _) = generate(&input, std::slice::from_ref(&package)).unwrap();
    assert_eq!(outputs.len(), 1);
}

#[test]
fn a_missing_source_key_is_an_error() {
    let package = fixed_package("s", "p");
    let input = json!({});
    assert_eq!(
        generate_err(&package, &input),
        "models.dev has no `s` provider"
    );
    assert_eq!(
        trim(&input).unwrap_err(),
        "models.dev has no `anthropic` provider"
    );
}

#[test]
fn models_come_out_sorted_by_id() {
    let package = fixed_package("s", "openai-responses");
    let (output, _) = generate_one(
        &package,
        &[("b", tool_model(json!({}))), ("a", tool_model(json!({})))],
    );
    let ids: Vec<String> = generated(&output)
        .iter()
        .map(|model| model["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ids, ["a", "b"]);
}

#[test]
fn two_runs_give_byte_identical_output() {
    let package = fixed_package("s", "openai-responses");
    let catalog = catalog(
        package.source,
        &[("b", tool_model(json!({}))), ("a", tool_model(json!({})))],
    );
    let first = generate(&catalog, std::slice::from_ref(&package))
        .unwrap()
        .0;
    let second = generate(&catalog, std::slice::from_ref(&package))
        .unwrap()
        .0;
    assert_eq!(first[0].text, second[0].text);
}

#[test]
fn numbers_keep_their_spelling() {
    let package = fixed_package("s", "openai-responses");
    let (output, _) = generate_one(
        &package,
        &[(
            "m",
            tool_model(json!({"cost": {"input": 5, "output": 0.075}})),
        )],
    );
    assert!(output.text.contains("\"input\": 5"), "{}", output.text);
    assert!(output.text.contains("\"output\": 0.075"), "{}", output.text);
}

#[test]
fn trim_keeps_every_model_and_only_the_fields_read() {
    let mut sources = Map::new();
    for package in &PACKAGES {
        let mut models = Map::new();
        models.insert(
            "m".to_owned(),
            json!({
                "tool_call": false,
                "status": "deprecated",
                "limit": {"context": 1, "output": 2, "input": 3},
                "cost": {
                    "input": 1, "output": 2, "cache_read": 0.1, "cache_write": 0.2,
                    "context_over_200k": {"input": 2},
                    "tiers": [{"input": 2, "tier": {"type": "context", "size": 4}, "extra": true}],
                },
                "modalities": {"input": ["text"], "output": ["text"], "extra": true},
                "provider": {"npm": "@ai-sdk/openai", "extra": true},
                "name": "dropped",
                "reasoning_options": [],
            }),
        );
        let mut provider = Map::new();
        provider.insert("models".to_owned(), Value::Object(models));
        provider.insert("extra".to_owned(), Value::from(true));
        sources.insert(package.source.to_owned(), Value::Object(provider));
    }
    sources.insert("groq".to_owned(), json!({"models": {}}));
    let catalog = Value::Object(sources);
    let trimmed = trim(&catalog).unwrap();
    assert!(trimmed.get("groq").is_none());
    for package in &PACKAGES {
        let model = &trimmed[package.source]["models"]["m"];
        assert_eq!(model["tool_call"], false);
        assert_eq!(model["status"], "deprecated");
        assert!(model.get("name").is_none());
        assert!(model.get("reasoning_options").is_none());
        assert!(model["limit"].get("input").is_none());
        assert!(model["cost"].get("context_over_200k").is_none());
        assert!(model["cost"]["tiers"][0].get("extra").is_none());
        assert_eq!(model["provider"], json!({"npm": "@ai-sdk/openai"}));
    }
    let twice = trim(&trimmed).unwrap();
    assert_eq!(trimmed, twice);
}

#[test]
fn trim_keeps_only_the_modality_keys_read() {
    let mut sources = Map::new();
    for package in &PACKAGES {
        let mut models = Map::new();
        models.insert(
            "m".to_owned(),
            json!({
                "modalities": {"input": ["text"], "output": ["text"], "audio_only": true},
            }),
        );
        let mut provider = Map::new();
        provider.insert("models".to_owned(), Value::Object(models));
        sources.insert(package.source.to_owned(), Value::Object(provider));
    }
    let trimmed = trim(&Value::Object(sources)).unwrap();
    for package in &PACKAGES {
        assert_eq!(
            trimmed[package.source]["models"]["m"]["modalities"],
            json!({"input": ["text"], "output": ["text"]}),
        );
    }
}

/// Production `curl`'s argv for `url`: asserting every flag keeps `fetch`
/// honest, so adding, dropping or reordering one fails both cases below.
fn assert_curl_argv(url: &str) {
    let command = curl(url);
    let argv: Vec<String> = [command.get_program()]
        .into_iter()
        .chain(command.get_args())
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert_eq!(argv, ["curl", "-fsSL", "--max-time", "120", url]);
}

/// One deadline for every blocking `fetch` below (`docs/testing.md`,
/// "Waits and timeouts"): a mutant that hangs `fetch` fails here,
/// not on nextest's kill.
const FETCH_WITHIN: Duration = Duration::from_secs(4);

/// `fetch` on a thread, received under `FETCH_WITHIN`.
fn fetched(url: &str) -> Result<Vec<u8>, String> {
    let url = url.to_owned();
    fakes::within("models-dev fetch", FETCH_WITHIN, move || fetch(&url))
}

/// A `file://` URL for `dir` joining `name`, which no test writes.
fn missing_url(dir: &TestDir, name: &str) -> String {
    format!("file://{}", dir.path().join(name).display())
}

#[test]
fn fetch_reads_a_file_url() {
    assert_curl_argv("https://models.dev/api.json");
    let dir = TestDir::new("models-dev-fetch-read");
    dir.write("catalog.json", "{\"anthropic\":{\"models\":{}}}");
    let file = dir.path().join("catalog.json");
    let url = format!("file://{}", file.display());
    assert_eq!(fetched(&url).unwrap(), std::fs::read(&file).unwrap());
    // The other half of the contract, so this case also fails when the
    // exit-status check flips: a missing file is an error, not bytes.
    let missing = missing_url(&dir, "missing.json");
    assert!(fetched(&missing).is_err(), "{missing}");
}

#[test]
fn fetch_names_a_failed_curl() {
    assert_curl_argv("https://models.dev/api.json");
    let dir = TestDir::new("models-dev-fetch-fail");
    let missing = missing_url(&dir, "missing.json");
    let error = fetched(&missing).unwrap_err();
    assert!(error.contains("exit status"), "{error}");
    assert!(error.contains(&missing), "{error}");
    // And reading a file still works, so this case also fails when the
    // argv above changes to flags that cannot read one.
    dir.write("catalog.json", "{}");
    let file = dir.path().join("catalog.json");
    let url = format!("file://{}", file.display());
    assert_eq!(fetched(&url).unwrap(), std::fs::read(&file).unwrap());
}

#[test]
fn zen_keeps_its_google_generative_ai_models() {
    let google = || tool_model(json!({"provider": {"npm": "@ai-sdk/google"}}));
    let other = || tool_model(json!({"provider": {"npm": "@ai-sdk/openai"}}));
    let zen = PACKAGES
        .iter()
        .find(|package| package.path.ends_with("opencode-zen.json"))
        .unwrap();
    let (output, left_out) = generate_one(
        zen,
        &[
            ("gemini-3.8-flash", google()),
            ("gpt-6-luna", other()),
            ("muse-spark-1.3", other()),
            ("gpt-6.1-sol", other()),
        ],
    );
    assert!(generated_model(&output, "gemini-3.8-flash").is_some());
    for id in ["gpt-6-luna", "muse-spark-1.3", "gpt-6.1-sol"] {
        assert!(generated_model(&output, id).is_some(), "{id}");
    }
    assert!(
        !left_out
            .iter()
            .any(|line| line.contains("gemini-3.8-flash")),
        "{left_out:?}"
    );
    let go = PACKAGES
        .iter()
        .find(|package| package.path.ends_with("opencode-go.json"))
        .unwrap();
    let (output, _) = generate_one(
        go,
        &[
            ("gemini-3.8-flash", google()),
            ("muse-spark-1.3-contributor", other()),
            (
                "minimax-m2.7",
                tool_model(json!({"provider": {"npm": "@ai-sdk/anthropic"}})),
            ),
        ],
    );
    assert!(generated_model(&output, "gemini-3.8-flash").is_some());
    let gemini = PACKAGES
        .iter()
        .find(|package| package.path.ends_with("providers/gemini.json"))
        .unwrap();
    let models: Vec<(&str, Value)> = gemini
        .by_model
        .iter()
        .map(|(id, _)| *id)
        .chain(gemini.thinking.iter().map(|(id, _, _)| *id))
        .map(|id| (id, tool_model(json!({}))))
        .collect();
    let (output, _) = generate_one(gemini, &models);
    assert!(generated_model(&output, "gemini-3.8-flash").is_some());
}
