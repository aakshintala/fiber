//! Unit tests for the models.dev generator: one small inline catalog
//! per rule.

use fakes::MUST_SUCCEED_WITHIN;

use serde_json::json;

use fakes::{Deadline, TempDir};

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

/// A catalog holding each source's models.
fn catalogs(sources: &[(&str, &[(&str, Value)])]) -> Value {
    let mut catalog = Map::new();
    for (source, models) in sources {
        let mut map = Map::new();
        for (id, model) in *models {
            map.insert((*id).to_owned(), model.clone());
        }
        let mut provider = Map::new();
        provider.insert("models".to_owned(), Value::Object(map));
        catalog.insert((*source).to_owned(), Value::Object(provider));
    }
    Value::Object(catalog)
}

/// A catalog holding `models` under `source`.
fn catalog(source: &str, models: &[(&str, Value)]) -> Value {
    catalogs(&[(source, models)])
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
    let (output, left_out, _) = generate_corrected(package, models, &[]);
    (output, left_out)
}

/// The error generating one package's file gives.
fn generate_err(package: &Package, input: &Value) -> String {
    match generate(input, std::slice::from_ref(package), &[]) {
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

/// Thinking rows for the `old` fixture id on gateway probes: a gateway
/// model needs a row, so the probe names one.
static OLD_THINKING: [(&str, &[&str], Option<&str>); 1] = [("old", &["low"], None)];

/// `package` with its identity kept but its reviewer, subset and stale
/// entries cleared, so one fixture model generates cleanly. A gateway
/// probe keeps a left-out model only when it names no thinking row: a
/// row for a left-out id is a stale table entry.
fn probe_package(
    package: &Package,
    thinking: &'static [(&'static str, &'static [&'static str], Option<&'static str>)],
) -> Package {
    Package {
        path: package.path,
        source: package.source,
        provider: r#"{"name":"t"}"#,
        protocol: match package.protocol {
            ProtocolRule::Fixed(protocol) => ProtocolRule::Fixed(protocol),
            ProtocolRule::ByNpm => ProtocolRule::ByNpm,
        },
        base_url: package.base_url,
        drop_deprecated: package.drop_deprecated,
        skip: package.skip,
        only: &[],
        protocol_overrides: &[],
        every_model: package.every_model,
        by_protocol: package.by_protocol,
        by_model: &[],
        thinking,
    }
}

#[test]
fn deprecated_is_dropped_in_every_package() {
    for package in &PACKAGES {
        assert!(
            package.drop_deprecated,
            "{} leaves out deprecated models",
            package.path
        );
        let probe = probe_package(package, &[]);
        let (output, left_out) = generate_one(
            &probe,
            &[("old", tool_model(json!({"status": "deprecated"})))],
        );
        assert_eq!(output.models, 0, "{}", package.path);
        assert_eq!(
            left_out,
            [format!(
                "models-dev: {}/old is deprecated in models.dev; left out",
                stem(package)
            )],
            "{}",
            package.path
        );
        let probe = probe_package(
            package,
            if is_gateway(package) {
                &OLD_THINKING
            } else {
                &[]
            },
        );
        for status in [json!({"status": "stable"}), json!({"status": null})] {
            let (output, _) = generate_one(&probe, &[("old", tool_model(status))]);
            assert_eq!(output.models, 1, "{}", package.path);
        }
        let (output, _) = generate_one(&probe, &[("old", tool_model(json!({})))]);
        assert_eq!(output.models, 1, "{}", package.path);
    }
}

/// Parses `text` as the corrections file, or panics.
fn corrections(text: &str) -> Vec<Correction> {
    parse_corrections("corrections", text).unwrap()
}

/// Generates one package's file with `corrections`, returning the
/// output, the left-out lines and the surviving corrections.
fn generate_corrected(
    package: &Package,
    models: &[(&str, Value)],
    corrections: &[Correction],
) -> (Output, Vec<String>, Vec<Correction>) {
    let input = catalog(package.source, models);
    let ((mut outputs, left_out), survivors) =
        generate(&input, std::slice::from_ref(package), corrections).unwrap();
    assert_eq!(outputs.len(), 1);
    (outputs.pop().unwrap(), left_out, survivors)
}

/// The error generating one package's file with `corrections` gives.
fn generate_corrected_err(package: &Package, input: &Value, corrections: &[Correction]) -> String {
    match generate(input, std::slice::from_ref(package), corrections) {
        Err(error) => error,
        Ok(_) => panic!("generate unexpectedly succeeded"),
    }
}

/// One correction entry overriding every derived field of `m`.
fn full_correction() -> Vec<Correction> {
    corrections(
        "[[correction]]\n\
         package = \"t\"\n\
         model = \"m\"\n\
         source = \"https://vendor.example/m\"\n\
         upstream = \"https://github.com/models-dev/models.dev/pull/1\"\n\
         [correction.fields]\n\
         name = \"Fixed\"\n\
         protocol = \"openai-responses\"\n\
         context_window = 2000\n\
         max_output_tokens = 50\n\
         input = [\"text\", \"image\"]\n\
         cost = {input = 5, output = 6}\n",
    )
}

#[test]
fn a_correction_overrides_derived_fields_after_the_table() {
    let mut package = fixed_package("s", "openai-completions");
    package.every_model = r#"{"context_window":5}"#;
    let (output, _, survivors) = generate_corrected(
        &package,
        &[("m", tool_model(json!({})))],
        &full_correction(),
    );
    let model = generated_model(&output, "m").unwrap();
    assert_eq!(model["name"], "Fixed");
    assert_eq!(model["protocol"], "openai-responses");
    assert_eq!(model["context_window"], 2000);
    assert_eq!(model["max_output_tokens"], 50);
    assert_eq!(model["input"], json!(["text", "image"]));
    assert_eq!(model["cost"], json!({"input": 5, "output": 6}));
    assert_eq!(survivors.len(), 1);
}

#[test]
fn a_correction_matching_derived_values_is_pruned() {
    let package = fixed_package("s", "openai-completions");
    let matching = corrections(
        "[[correction]]\n\
         package = \"t\"\n\
         model = \"m\"\n\
         source = \"https://vendor.example/m\"\n\
         upstream = \"https://github.com/models-dev/models.dev/pull/1\"\n\
         [correction.fields]\n\
         protocol = \"openai-completions\"\n\
         context_window = 1000\n\
         max_output_tokens = 100\n\
         input = [\"text\"]\n\
         cost = {input = 1, output = 2}\n",
    );
    let (output, _, survivors) =
        generate_corrected(&package, &[("m", tool_model(json!({})))], &matching);
    assert!(survivors.is_empty());
    let model = generated_model(&output, "m").unwrap();
    assert_eq!(model["context_window"], 1000);
}

#[test]
fn a_correction_differing_in_one_field_is_kept() {
    let package = fixed_package("s", "openai-completions");
    let mut partial = full_correction();
    partial[0]
        .fields
        .insert("context_window".to_owned(), json!(1000));
    partial[0]
        .fields
        .insert("max_output_tokens".to_owned(), json!(100));
    partial[0]
        .fields
        .insert("protocol".to_owned(), json!("openai-completions"));
    partial[0].fields.insert("name".to_owned(), json!("M"));
    partial[0].fields.remove("input");
    partial[0].fields.remove("cost");
    let (output, _, survivors) = generate_corrected(
        &package,
        &[("m", tool_model(json!({"name": "Other"})))],
        &partial,
    );
    assert_eq!(survivors.len(), 1);
    assert_eq!(generated_model(&output, "m").unwrap()["name"], "M");
}

#[test]
fn a_correction_missing_upstream_is_rejected() {
    let missing = "[[correction]]\n\
                   package = \"t\"\n\
                   model = \"m\"\n\
                   source = \"https://vendor.example/m\"\n\
                   [correction.fields]\n\
                   name = \"Fixed\"\n";
    assert_eq!(
        parse_corrections("corrections", missing).unwrap_err(),
        "corrections: correction 1 names no `upstream`"
    );
    let empty = missing.replace(
        "source = \"https://vendor.example/m\"",
        "source = \"https://vendor.example/m\"\nupstream = \"\"",
    );
    assert_eq!(
        parse_corrections("corrections", &empty).unwrap_err(),
        "corrections: correction 1 names no `upstream`"
    );
}

#[test]
fn a_correction_for_an_unknown_model_is_an_error() {
    let package = fixed_package("s", "openai-completions");
    let input = catalog(package.source, &[("m", tool_model(json!({})))]);
    let mut ghost = full_correction();
    ghost[0].model = "ghost".to_owned();
    assert_eq!(
        generate_corrected_err(&package, &input, &ghost),
        "t: correction for `ghost` matches no generated model"
    );
}

#[test]
fn a_correction_for_a_left_out_model_is_an_error() {
    let package = fixed_package("s", "openai-completions");
    let input = catalog(
        package.source,
        &[("off", tool_model(json!({"tool_call": false})))],
    );
    let mut left_out = full_correction();
    left_out[0].model = "off".to_owned();
    assert_eq!(
        generate_corrected_err(&package, &input, &left_out),
        "t: correction for `off` matches no generated model"
    );
}

#[test]
fn a_correction_overriding_a_table_only_field_is_rejected() {
    let package = fixed_package("s", "openai-completions");
    let input = catalog(package.source, &[("m", tool_model(json!({})))]);
    for field in [
        "id",
        "thinking_levels",
        "thinking_default",
        "compat",
        "base_url",
        "web_search",
    ] {
        let mut entry = full_correction();
        entry[0].fields = Map::from_iter([(field.to_owned(), json!(1))]);
        assert_eq!(
            generate_corrected_err(&package, &input, &entry),
            format!(
                "providers/models-corrections.toml: correction for `t/m` overrides table-only field `{field}`"
            )
        );
    }
    let mut unknown = full_correction();
    unknown[0].fields = Map::from_iter([("bogus".to_owned(), json!(1))]);
    assert_eq!(
        generate_corrected_err(&package, &input, &unknown),
        "providers/models-corrections.toml: correction for `t/m` names unknown field `bogus`"
    );
}

#[test]
fn a_correction_for_codex_context_window_is_rejected() {
    let package = fixed_package("s", "openai-completions");
    let input = catalog(package.source, &[("m", tool_model(json!({})))]);
    let mut package = package;
    package.path = "providers/codex/providers/codex.json";
    let mut pin = full_correction();
    pin[0].package = "codex".to_owned();
    pin[0].fields = Map::from_iter([("context_window".to_owned(), json!(1))]);
    assert_eq!(
        generate_corrected_err(&package, &input, &pin),
        "providers/models-corrections.toml: correction for `codex/m` overrides the table-pinned `context_window`"
    );
}

#[test]
fn a_correction_for_an_unknown_package_is_rejected() {
    let package = fixed_package("s", "openai-completions");
    let input = catalog(package.source, &[("m", tool_model(json!({})))]);
    let mut ghost = full_correction();
    ghost[0].package = "ghost".to_owned();
    assert_eq!(
        generate_corrected_err(&package, &input, &ghost),
        "providers/models-corrections.toml: correction for `ghost/m` names unknown package `ghost`"
    );
}

#[test]
fn a_second_correction_for_one_model_is_rejected() {
    let package = fixed_package("s", "openai-completions");
    let input = catalog(package.source, &[("m", tool_model(json!({})))]);
    let mut twice = full_correction();
    twice.push(twice[0].clone());
    assert_eq!(
        generate_corrected_err(&package, &input, &twice),
        "providers/models-corrections.toml: correction for `t/m` appears twice"
    );
}

#[test]
fn corrections_with_wrong_types_are_rejected() {
    assert_eq!(
        parse_corrections("corrections", "[[correction]]\npackage = 3\n").unwrap_err(),
        "corrections: correction 1 names a non-string `package`"
    );
    assert_eq!(
        parse_corrections(
            "corrections",
            "[[correction]]\n\
             package = \"t\"\n\
             model = \"m\"\n\
             source = \"https://vendor.example/m\"\n\
             upstream = \"https://github.com/models-dev/models.dev/pull/1\"\n\
             fields = \"x\"\n"
        )
        .unwrap_err(),
        "corrections: correction 1 names a non-table `fields`"
    );
    let package = fixed_package("s", "openai-completions");
    let input = catalog(package.source, &[("m", tool_model(json!({})))]);
    let mut entry = full_correction();
    entry[0].fields = Map::from_iter([("context_window".to_owned(), json!("big"))]);
    assert_eq!(
        generate_corrected_err(&package, &input, &entry),
        "providers/models-corrections.toml: correction for `t/m` sets `context_window` to a non-integer"
    );
}

#[test]
fn the_committed_corrections_file_parses_and_renders_to_itself() {
    // The empty file writes the header alone, so a prune rewrite keeps
    // the file's shape byte for byte.
    let committed = include_str!("../../providers/models-corrections.toml");
    let corrections = parse_corrections("corrections", committed).unwrap();
    assert!(corrections.is_empty());
    assert_eq!(render_corrections(&corrections), committed);
}

#[test]
fn corrections_parse_both_fields_forms_and_reject_stray_tables() {
    let inline = corrections(
        "[[correction]]\n\
         package = \"t\"\n\
         model = \"m\"\n\
         source = \"https://vendor.example/m\"\n\
         upstream = \"https://github.com/models-dev/models.dev/pull/1\"\n\
         fields = {name = \"Fixed\"}\n",
    );
    assert_eq!(inline[0].fields["name"], "Fixed");
    assert_eq!(
        parse_corrections("corrections", "key = 1\n").unwrap_err(),
        "corrections: names a key outside any `[[correction]]`"
    );
    assert_eq!(
        parse_corrections("corrections", "[other]\n").unwrap_err(),
        "corrections: names unknown table `other`"
    );
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

/// A gateway package writing `providers/opencode/providers/opencode-go.json`:
/// its path is what marks it as a gateway, so its models inherit a vendor
/// package's thinking row by exact model id.
fn gateway_package(source: &'static str) -> Package {
    Package {
        path: "providers/opencode/providers/opencode-go.json",
        ..fixed_package(source, "openai-responses")
    }
}

/// A direct-vendor package: its path is what marks its thinking rows as
/// inheritable by gateways. A `codex` path never is.
fn vendor_package(source: &'static str, path: &'static str) -> Package {
    Package {
        path,
        ..fixed_package(source, "openai-responses")
    }
}

/// The generated output for `file` in `outputs`, such as the gateway's.
fn output_for<'a>(outputs: &'a [Output], file: &str) -> &'a Output {
    outputs
        .iter()
        .find(|output| output.path.ends_with(file))
        .unwrap()
}

#[test]
fn a_gateway_model_with_no_row_at_all_is_an_error_naming_both() {
    let gateway = gateway_package("g");
    let input = catalog("g", &[("m", tool_model(json!({})))]);
    assert_eq!(
        generate_err(&gateway, &input),
        "opencode-go: `m` has neither an inherited nor its own thinking row"
    );
    // A non-gateway package with the same content still declares none.
    let plain = fixed_package("g", "openai-responses");
    let (output, _) = generate_one(&plain, &[("m", tool_model(json!({})))]);
    assert!(
        generated_model(&output, "m")
            .unwrap()
            .get("thinking_levels")
            .is_none()
    );
}

#[test]
fn a_gateway_model_inherits_the_vendor_row_for_its_exact_id() {
    let mut vendor = vendor_package("v", "providers/openai/providers/openai.json");
    vendor.thinking = &[("m", &["low", "high"], Some("low"))];
    let gateway = gateway_package("g");
    let model = tool_model(json!({}));
    let input = catalogs(&[("v", &[("m", model.clone())]), ("g", &[("m", model)])]);
    let ((outputs, _), _) = generate(&input, &[vendor, gateway], &[]).unwrap();
    let model = generated_model(output_for(&outputs, "opencode-go.json"), "m").unwrap();
    assert_eq!(model["thinking_levels"], json!(["low", "high"]));
    assert_eq!(model["thinking_default"], "low");
}

#[test]
fn inheritance_matches_the_whole_id_only() {
    let mut vendor = vendor_package("v", "providers/openai/providers/openai.json");
    vendor.thinking = &[("m", &["low"], None)];
    let gateway = gateway_package("g");
    let model = tool_model(json!({}));
    let input = catalogs(&[("v", &[("m", model.clone())]), ("g", &[("m-extra", model)])]);
    assert_eq!(
        match generate(&input, &[vendor, gateway], &[]) {
            Err(error) => error,
            Ok(_) => panic!("generate unexpectedly succeeded"),
        },
        "opencode-go: `m-extra` has neither an inherited nor its own thinking row"
    );
}

#[test]
fn a_gateway_own_row_beats_the_vendor_row() {
    let mut vendor = vendor_package("v", "providers/openai/providers/openai.json");
    vendor.thinking = &[("m", &["low"], Some("low"))];
    let mut gateway = gateway_package("g");
    gateway.thinking = &[("m", &["high"], None)];
    let model = tool_model(json!({}));
    let input = catalogs(&[("v", &[("m", model.clone())]), ("g", &[("m", model)])]);
    let ((outputs, _), _) = generate(&input, &[vendor, gateway], &[]).unwrap();
    let model = generated_model(output_for(&outputs, "opencode-go.json"), "m").unwrap();
    assert_eq!(model["thinking_levels"], json!(["high"]));
    assert!(model.get("thinking_default").is_none());
}

#[test]
fn codex_rows_never_reach_a_gateway() {
    let mut codex = vendor_package("c", "providers/codex/providers/codex.json");
    codex.thinking = &[("m", &["low"], Some("low"))];
    let mut openai = vendor_package("v", "providers/openai/providers/openai.json");
    openai.thinking = &[("m", &["low", "high"], None)];
    let gateway = gateway_package("g");
    let model = tool_model(json!({}));
    let input = catalogs(&[
        ("c", &[("m", model.clone())]),
        ("v", &[("m", model.clone())]),
        ("g", &[("m", model)]),
    ]);
    let ((outputs, _), _) = generate(&input, &[codex, openai, gateway], &[]).unwrap();
    let model = generated_model(output_for(&outputs, "opencode-go.json"), "m").unwrap();
    assert_eq!(model["thinking_levels"], json!(["low", "high"]));
    assert!(model.get("thinking_default").is_none());
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
    let snapshot = include_str!("../tests/models-dev.json");
    let snapshot: Value = serde_json::from_str(snapshot).unwrap();
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
            if levels.is_empty() {
                // An explicitly empty row means the model takes no level,
                // so it names no default either.
                assert!(default.is_none(), "{id} lists no level but names a default");
                continue;
            }
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
fn an_explicitly_empty_row_writes_empty_levels_with_no_default() {
    let mut package = fixed_package("s", "openai-responses");
    package.thinking = &[("m", &[], None)];
    let (output, _) = generate_one(&package, &[("m", tool_model(json!({})))]);
    let model = generated_model(&output, "m").unwrap();
    assert_eq!(model["thinking_levels"], json!([]));
    assert!(model.get("thinking_default").is_none());
}

#[test]
fn a_legacy_claude_compat_layer_keeps_the_gateway_header() {
    // A gateway Claude model inheriting a legacy budget-tier row takes
    // adaptive thinking off via `compat.thinking_budget`; table layers
    // replace by key rather than deep-merging, so its model-specific
    // layer repeats the gateway `cache_key_header` or auth breaks.
    let mut gateway = Package {
        protocol: ProtocolRule::Fixed("anthropic-messages"),
        ..gateway_package("g")
    };
    gateway.by_protocol = &[(
        "anthropic-messages",
        r#"{"compat":{"cache_key_header":"x-opencode-session"}}"#,
    )];
    gateway.by_model = &[(
        "m",
        r#"{"compat":{"cache_key_header":"x-opencode-session","thinking_budget":true}}"#,
    )];
    gateway.thinking = &[("m", &["off", "minimal", "low", "medium", "high"], None)];
    let (output, _) = generate_one(&gateway, &[("m", tool_model(json!({})))]);
    let model = generated_model(&output, "m").unwrap();
    assert_eq!(
        model["compat"],
        json!({"cache_key_header": "x-opencode-session", "thinking_budget": true})
    );
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
    let ((outputs, _), _) = generate(&input, std::slice::from_ref(&package), &[]).unwrap();
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
    let first = generate(&catalog, std::slice::from_ref(&package), &[])
        .unwrap()
        .0
        .0;
    let second = generate(&catalog, std::slice::from_ref(&package), &[])
        .unwrap()
        .0
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
                "name": "Acme Model",
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
        assert_eq!(model["name"], "Acme Model");
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
fn a_non_empty_string_name_is_written_verbatim() {
    let package = fixed_package("s", "openai-responses");
    let (output, _) = generate_one(
        &package,
        &[("m", tool_model(json!({"name": "Acme Model"})))],
    );
    let model = generated_model(&output, "m").unwrap();
    assert_eq!(model["name"], "Acme Model");
    let keys: Vec<&str> = model
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted);
}

#[test]
fn a_missing_non_string_or_empty_name_is_left_out() {
    let package = fixed_package("s", "openai-responses");
    let (output, _) = generate_one(
        &package,
        &[
            ("missing", tool_model(json!({}))),
            ("number", tool_model(json!({"name": 7}))),
            ("null", tool_model(json!({"name": null}))),
            ("empty", tool_model(json!({"name": ""}))),
        ],
    );
    assert_eq!(output.models, 4);
    for id in ["missing", "number", "null", "empty"] {
        assert!(
            generated_model(&output, id).unwrap().get("name").is_none(),
            "{id} names no name"
        );
    }
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

/// `fetch` on a thread, received under `MUST_SUCCEED_WITHIN`.
fn fetched(url: &str) -> Result<Vec<u8>, String> {
    let url = url.to_owned();
    fakes::within(
        "models-dev fetch",
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || fetch(&url),
    )
}

/// A `file://` URL for `dir` joining `name`, which no test writes.
fn missing_url(dir: &TempDir, name: &str) -> String {
    format!("file://{}", dir.path().join(name).display())
}

#[test]
fn fetch_reads_a_file_url() {
    assert_curl_argv("https://models.dev/api.json");
    let dir = TempDir::new("xtask-models-dev-fetch-read");
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
    let dir = TempDir::new("xtask-models-dev-fetch-fail");
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
    let openai = || tool_model(json!({"provider": {"npm": "@ai-sdk/openai"}}));
    let anthropic = || tool_model(json!({"provider": {"npm": "@ai-sdk/anthropic"}}));
    // Focused fixtures: the vendor rows the gateways inherit, and each
    // gateway's own rows for the asserted models. The drift test in
    // xtask/tests/models_dev.rs already checks the real tables for stale
    // entries across packages.
    let mut gemini = vendor_package("v-gemini", "providers/gemini/providers/gemini.json");
    gemini.thinking = &[("gemini-3.8-flash", &["low", "medium", "high"], None)];
    let mut vendor = vendor_package("v-openai", "providers/openai/providers/openai.json");
    vendor.thinking = &[
        ("gpt-6-luna", &["low"], None),
        ("gpt-6.1-sol", &["low"], None),
    ];
    let mut zen = gateway_package("z");
    zen.path = "providers/opencode/providers/opencode-zen.json";
    zen.protocol = ProtocolRule::ByNpm;
    zen.thinking = &[
        ("muse-spark-1.3", &["low"], None),
        ("minimax-m2.7", &[], None),
    ];
    let mut go = gateway_package("g");
    go.protocol = ProtocolRule::ByNpm;
    go.thinking = &[
        ("muse-spark-1.3-contributor", &["low"], None),
        ("minimax-m2.7", &["low"], None),
    ];
    let input = catalogs(&[
        ("v-gemini", &[("gemini-3.8-flash", google())]),
        (
            "v-openai",
            &[("gpt-6-luna", openai()), ("gpt-6.1-sol", openai())],
        ),
        (
            "z",
            &[
                ("gemini-3.8-flash", google()),
                ("gpt-6-luna", openai()),
                ("muse-spark-1.3", openai()),
                ("gpt-6.1-sol", openai()),
                ("minimax-m2.7", anthropic()),
            ],
        ),
        (
            "g",
            &[
                ("gemini-3.8-flash", google()),
                ("muse-spark-1.3-contributor", openai()),
                ("minimax-m2.7", anthropic()),
            ],
        ),
    ]);
    let ((outputs, left_out), _) = generate(&input, &[gemini, vendor, zen, go], &[]).unwrap();
    let output = output_for(&outputs, "opencode-zen.json");
    let gemini = generated_model(output, "gemini-3.8-flash").unwrap();
    assert_eq!(gemini["protocol"], "google-generative-ai");
    assert_eq!(gemini["thinking_levels"], json!(["low", "medium", "high"]));
    for id in ["gpt-6-luna", "muse-spark-1.3", "gpt-6.1-sol"] {
        assert!(generated_model(output, id).is_some(), "{id}");
    }
    assert!(
        !left_out
            .iter()
            .any(|line| line.contains("gemini-3.8-flash")),
        "{left_out:?}"
    );
    let output = output_for(&outputs, "opencode-go.json");
    assert!(generated_model(output, "gemini-3.8-flash").is_some());
    for id in ["muse-spark-1.3-contributor", "minimax-m2.7"] {
        assert!(generated_model(output, id).is_some(), "{id}");
    }
}

#[test]
fn openai_search_marks_match_the_october_probe() {
    // The October 16, 2026 hosted-search probe
    // (research/web-tools/hosted-search.md) accepted `web_search` on
    // `gpt-4o` and refused it on `gpt-4`, so the generated OpenAI file
    // marks the former and leaves the latter unmarked.
    let package = PACKAGES
        .iter()
        .find(|package| package.path == "providers/openai/providers/openai.json")
        .unwrap();
    let mut ids: Vec<&str> = package.by_model.iter().map(|(id, _)| *id).collect();
    for (id, _, _) in package.thinking {
        if !ids.contains(id) {
            ids.push(*id);
        }
    }
    for id in ["gpt-4o", "gpt-4"] {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    let models: Vec<(&str, Value)> = ids.iter().map(|id| (*id, tool_model(json!({})))).collect();
    let (output, _) = generate_one(package, &models);
    assert_eq!(
        generated_model(&output, "gpt-4o").unwrap()["web_search"],
        "web_search"
    );
    assert!(
        generated_model(&output, "gpt-4")
            .unwrap()
            .get("web_search")
            .is_none()
    );
}

#[test]
fn opencode_go_marks_store_false_where_the_protocol_accepts_it() {
    // The generated opencode-go file, compiled in: every openai-responses
    // and openai-completions model carries compat.store false, since
    // those protocols read it, while anthropic-messages and
    // google-generative-ai models carry no store key their protocol
    // never reads.
    let file: Value = serde_json::from_str(include_str!(
        "../../providers/opencode/providers/opencode-go.json"
    ))
    .unwrap();
    let models = file["models"].as_array().unwrap();
    assert!(!models.is_empty());
    for model in models {
        let protocol = model["protocol"].as_str().unwrap();
        match protocol {
            "openai-responses" | "openai-completions" => assert_eq!(
                model["compat"]["store"],
                Value::Bool(false),
                "{} on {protocol} carries no store:false",
                model["id"]
            ),
            _ => assert!(
                model["compat"].get("store").is_none(),
                "{} on {protocol} carries a store flag its protocol never reads",
                model["id"]
            ),
        }
    }
}

#[test]
fn databricks_keys_mirror_the_lua_match_key() {
    // Each arm of Lua `model_key`, pinned against the same services the
    // extension's `models-model-key-strips-prefixes` case covers, plus the
    // `/` tail, the three droppable suffixes and case-folding.
    for (raw, key) in [
        ("model-services/system.ai.alpha-one", "alpha-one"),
        ("model-services/system.ai.system.ai.beta-two", "beta-two"),
        (
            "model-services/system.ai.databricks-gamma-three",
            "gamma-three",
        ),
        ("model-services/system.ai.fw-delta-four", "delta-four"),
        ("model-services/system.ai.meta-epsilon-five", "epsilon-five"),
        ("model-services/system.ai.us.zeta-six", "zeta-six"),
        ("model-services/system.ai.anthropic.eta-seven", "eta-seven"),
        ("model-services/system.ai.theta/acme-eight", "acme-eight"),
        ("model-services/system.ai.iota__nine", "iota-nine"),
        ("model-services/system.ai.kappa-ten-preview", "kappa-ten"),
        ("model-services/system.ai.kappa-ten-maas", "kappa-ten"),
        ("model-services/system.ai.kappa-ten-it", "kappa-ten"),
        ("model-services/system.ai.nu-thirteen-", "nu-thirteen"),
        ("model-services/system.ai.GPT-6-Sol", "gpt-6-sol"),
        ("claude-opus-5-20251101", "claude-opus-5-20251101"),
        ("muse-spark-1.3", "muse-spark-1-3"),
        ("eu.openai.gpt-6-sol", "gpt-6-sol"),
    ] {
        assert_eq!(databricks_model_key(raw), key, "{raw}");
    }
}

#[test]
fn databricks_keys_keep_lookalike_suffixes() {
    // The stem drops only `maas`, `preview` and `it`: a trailing number,
    // an unknown word and a bare `-` stay, and a dropped stem still trims
    // the `-` it leaves behind.
    for (raw, key) in [
        (
            "model-services/system.ai.claude-opus-5-20251101",
            "claude-opus-5-20251101",
        ),
        (
            "model-services/system.ai.model-preview-x",
            "model-preview-x",
        ),
        ("model-services/system.ai.model-", "model"),
        ("-maas", ""),
    ] {
        assert_eq!(databricks_model_key(raw), key, "{raw}");
    }
}

#[test]
fn databricks_thinking_keeps_vendor_rows_and_their_defaults() {
    let mut vendor = vendor_package("v", "providers/openai/providers/openai.json");
    vendor.thinking = &[
        (
            "gpt-6-sol",
            &["off", "low", "medium", "high", "xhigh", "max"],
            None,
        ),
        ("synth", &["low", "medium"], Some("medium")),
    ];
    let output = merge_databricks_thinking(std::slice::from_ref(&vendor), &[]).unwrap();
    assert_eq!(output.path, DATABRICKS_THINKING);
    assert_eq!(output.models, 2);
    assert!(
        output.text.contains(
            "  [\"synth\"] = { levels = { \"low\", \"medium\" }, default = \"medium\" },\n"
        ),
        "{}",
        output.text
    );
}

#[test]
fn databricks_thinking_inherits_known_vendor_ids() {
    // The real tables: the Claude and GPT rows the extension case reads,
    // with the `openai` row for the id `codex` also lists. The overlay
    // adds `gpt-6-sol`'s default and overrides `claude-haiku-5-5`'s row.
    let output = databricks_thinking(&PACKAGES).unwrap();
    assert!(
        output.text.contains(
            "  [\"claude-opus-5\"] = { levels = { \"low\", \"medium\", \"high\", \"xhigh\", \"max\" } },\n"
        ),
        "{}",
        output.text
    );
    assert!(
        output.text.contains(
            "  [\"gpt-6-sol\"] = { levels = { \"off\", \"low\", \"medium\", \"high\", \"xhigh\", \"max\" }, default = \"medium\" },\n"
        ),
        "{}",
        output.text
    );
    assert!(
        output.text.contains(
            "  [\"claude-haiku-5-5\"] = { levels = { \"off\", \"low\", \"medium\", \"high\" }, default = \"medium\" },\n"
        ),
        "{}",
        output.text
    );
}

#[test]
fn databricks_thinking_ignores_codex_rows() {
    // `codex` carries ChatGPT-backend defaults that do not hold on a
    // gateway route: its `medium` default for `gpt-6-sol` never reaches
    // the table, while the `openai` row for the same id does.
    let mut codex = vendor_package("c", "providers/codex/providers/codex.json");
    codex.thinking = &[("gpt-6-sol", &["low"], Some("low"))];
    let alone = merge_databricks_thinking(std::slice::from_ref(&codex), &[]).unwrap();
    assert_eq!(alone.models, 0);
    assert!(!alone.text.contains("gpt-6-sol"), "{}", alone.text);
    let mut openai = vendor_package("v", "providers/openai/providers/openai.json");
    openai.thinking = &[("gpt-6-sol", &["low", "high"], None)];
    let input = [codex, openai];
    let output = merge_databricks_thinking(&input, &[]).unwrap();
    assert_eq!(output.models, 1);
    assert!(
        output
            .text
            .contains("  [\"gpt-6-sol\"] = { levels = { \"low\", \"high\" } },\n"),
        "{}",
        output.text
    );
}

#[test]
fn databricks_thinking_rejects_a_shared_key_with_differing_rows() {
    // `M` and `m` share the match key `m`: differing rows name both ids,
    // while identical rows keep the first without an error.
    let mut anthropic = vendor_package("a", "providers/anthropic/providers/anthropic.json");
    anthropic.thinking = &[("m", &["low"], None)];
    let mut openai = vendor_package("v", "providers/openai/providers/openai.json");
    openai.thinking = &[("M", &["high"], None)];
    let input = [anthropic, openai];
    match databricks_thinking(&input) {
        Err(error) => assert_eq!(
            error,
            "databricks thinking: `m` and `M` share a match key with differing thinking rows"
        ),
        Ok(_) => panic!("databricks_thinking unexpectedly succeeded"),
    };
    let mut anthropic = vendor_package("a", "providers/anthropic/providers/anthropic.json");
    anthropic.thinking = &[("m", &["low"], None)];
    let mut openai = vendor_package("v", "providers/openai/providers/openai.json");
    openai.thinking = &[("M", &["low"], None)];
    let input = [anthropic, openai];
    let output = merge_databricks_thinking(&input, &[]).unwrap();
    assert_eq!(output.models, 1);
    assert!(
        output
            .text
            .contains("  [\"m\"] = { levels = { \"low\" } },\n"),
        "{}",
        output.text
    );
}

#[test]
fn databricks_thinking_overlay_wins_on_a_shared_key() {
    // A vendor row for an overlay-covered key loses: the overlay's
    // `gpt-oss-120b` row replaces the vendor's differing one, with no
    // shared-key error.
    let mut vendor = vendor_package("v", "providers/openai/providers/openai.json");
    vendor.thinking = &[("gpt-oss-120b", &["low"], Some("low"))];
    let output = databricks_thinking(std::slice::from_ref(&vendor)).unwrap();
    assert!(
        output.text.contains(
            "  [\"gpt-oss-120b\"] = { levels = { \"low\", \"medium\", \"high\" }, default = \"medium\" },\n"
        ),
        "{}",
        output.text
    );
    assert!(
        !output
            .text
            .contains("\"gpt-oss-120b\"] = { levels = { \"low\" }"),
        "{}",
        output.text
    );
}

#[test]
fn databricks_thinking_overlay_adds_an_uncovered_key() {
    // No vendor package names `gpt-oss-20b`, yet the overlay adds it with
    // its default.
    let output = databricks_thinking(&[]).unwrap();
    assert!(
        output.text.contains(
            "  [\"gpt-oss-20b\"] = { levels = { \"low\", \"medium\", \"high\" }, default = \"medium\" },\n"
        ),
        "{}",
        output.text
    );
}

#[test]
fn thinking_overlay_rejects_an_unknown_level() {
    match parse_thinking_overlay("overlay.json", r#"{"m": {"levels": ["low", "ultra"]}}"#) {
        Err(error) => assert_eq!(
            error,
            "databricks thinking overlay: `m` names unknown level `ultra`"
        ),
        Ok(_) => panic!("overlay with an unknown level unexpectedly succeeded"),
    };
}

#[test]
fn thinking_overlay_rejects_a_default_outside_its_levels() {
    match parse_thinking_overlay(
        "overlay.json",
        r#"{"m": {"levels": ["low", "high"], "default": "max"}}"#,
    ) {
        Err(error) => assert_eq!(
            error,
            "databricks thinking overlay: `m` names default `max` outside its levels"
        ),
        Ok(_) => panic!("overlay with a default outside its levels unexpectedly succeeded"),
    };
    // A default inside its levels parses, and a null default reads as none.
    let overlay = parse_thinking_overlay(
        "overlay.json",
        r#"{"m": {"levels": ["low"], "default": null}}"#,
    )
    .unwrap();
    assert_eq!(overlay.len(), 1);
    assert_eq!(overlay[0].key, "m");
    assert!(overlay[0].default.is_none());
}

#[test]
fn thinking_overlay_rejects_empty_levels() {
    match parse_thinking_overlay("overlay.json", r#"{"m": {"levels": []}}"#) {
        Err(error) => assert_eq!(error, "databricks thinking overlay: `m` names no levels"),
        Ok(_) => panic!("overlay with empty levels unexpectedly succeeded"),
    };
}

#[test]
fn thinking_overlay_rejects_a_non_object_row() {
    // A top level that is no object, a row with no levels array, a level
    // that is no string and a default that is no string each fail.
    match parse_thinking_overlay("overlay.json", r#"[]"#) {
        Err(error) => assert_eq!(
            error,
            "databricks thinking overlay overlay.json: top level must be an object"
        ),
        Ok(_) => panic!("overlay with a non-object top level unexpectedly succeeded"),
    };
    match parse_thinking_overlay("overlay.json", r#"{"m": {"default": "low"}}"#) {
        Err(error) => assert_eq!(
            error,
            "databricks thinking overlay: `m` names no levels array"
        ),
        Ok(_) => panic!("overlay with no levels array unexpectedly succeeded"),
    };
    match parse_thinking_overlay("overlay.json", r#"{"m": {"levels": ["low", 7]}}"#) {
        Err(error) => assert_eq!(
            error,
            "databricks thinking overlay: `m` names a level that is no string"
        ),
        Ok(_) => panic!("overlay with a non-string level unexpectedly succeeded"),
    };
    match parse_thinking_overlay(
        "overlay.json",
        r#"{"m": {"levels": ["low"], "default": 7}}"#,
    ) {
        Err(error) => assert_eq!(
            error,
            "databricks thinking overlay: `m` names a default that is no string"
        ),
        Ok(_) => panic!("overlay with a non-string default unexpectedly succeeded"),
    };
}

#[test]
fn thinking_overlay_rejects_a_normalized_key_collision() {
    // `M` and `m` share the match key `m`: differing rows name both file
    // keys, while identical rows keep the first with no error.
    match parse_thinking_overlay(
        "overlay.json",
        r#"{"m": {"levels": ["low"]}, "M": {"levels": ["high"]}}"#,
    ) {
        Err(error) => assert_eq!(
            error,
            "databricks thinking overlay: `M` and `m` share a match key with differing thinking rows"
        ),
        Ok(_) => panic!("overlay with a normalized-key collision unexpectedly succeeded"),
    };
    match parse_thinking_overlay(
        "overlay.json",
        r#"{"m": {"levels": ["low"], "default": "low"}, "M": {"levels": ["low"]}}"#,
    ) {
        Err(error) => assert_eq!(
            error,
            "databricks thinking overlay: `M` and `m` share a match key with differing thinking rows"
        ),
        Ok(_) => panic!("overlay with a default-only collision unexpectedly succeeded"),
    };
    let overlay = parse_thinking_overlay(
        "overlay.json",
        r#"{"m": {"levels": ["low"]}, "M": {"levels": ["low"]}}"#,
    )
    .unwrap();
    assert_eq!(overlay.len(), 1);
    assert_eq!(overlay[0].key, "M");
}

#[test]
fn thinking_overlay_rejects_a_gpt_5_row() {
    // The gpt-5.x family stays out by name, on the bare key and on a
    // suffixed one; a lookalike without the dash still parses.
    for raw in ["gpt-5", "gpt-5-6-sol"] {
        let text = format!(r#"{{"{raw}": {{"levels": ["low"]}}}}"#);
        match parse_thinking_overlay("overlay.json", &text) {
            Err(error) => assert_eq!(
                error,
                format!(
                    "databricks thinking overlay: `{raw}` names a gpt-5.x model, which keeps the openai package's row"
                )
            ),
            Ok(_) => panic!("overlay with a gpt-5.x row unexpectedly succeeded"),
        };
    }
    let overlay =
        parse_thinking_overlay("overlay.json", r#"{"gpt-50": {"levels": ["low"]}}"#).unwrap();
    assert_eq!(overlay.len(), 1);
    assert_eq!(overlay[0].key, "gpt-50");
}

#[test]
fn thinking_overlay_malformed_json_names_the_file_and_location() {
    match parse_thinking_overlay("overlay.json", r#"{"m": "#) {
        Err(error) => assert!(
            error.starts_with("databricks thinking overlay overlay.json: "),
            "{error}"
        ),
        Ok(_) => panic!("malformed overlay unexpectedly succeeded"),
    };
}
