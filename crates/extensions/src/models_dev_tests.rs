use serde_json::json;

// This pins parity with the whole-document serde_json parse the streaming path replaced.
use super::{
    CatalogMeta, ProviderVisitor, retained_rows, url_with_override, valid_catalog, validate_catalog,
};

#[test]
fn models_dev_url_override_defaults_to_the_public_catalog() {
    assert_eq!(
        url_with_override(None, super::URL),
        "https://models.dev/api.json"
    );
}

#[test]
fn models_dev_url_override_uses_the_configured_catalog() {
    assert_eq!(
        url_with_override(
            Some("http://127.0.0.1:1234/api.json".to_owned()),
            super::URL
        ),
        "http://127.0.0.1:1234/api.json"
    );
}

#[test]
fn streaming_projection_keeps_duplicate_keys_last_wins_and_mistyped_siblings() {
    let models: Vec<config::ModelData> = serde_json::from_value(json!([
        {"id": "x", "protocol": "openai-responses", "base_url": "https://example.test"}
    ]))
    .unwrap();
    let meta = CatalogMeta {
        sources: vec!["provider".into()],
        ..Default::default()
    };
    let input = br#"{"provider":{"x":{"status":"active"}},"provider":{"x":{"name":5,"status":"deprecated"}},"other":{"large":"ignored"}}"#;
    let result = retained_rows(&input[..], &models, &meta).unwrap();
    assert_eq!(
        result,
        json!({"provider":{"x":{"name":5,"status":"deprecated"}}})
    );
}

#[test]
fn streaming_catalog_parsers_reject_trailing_bytes() {
    let models: Vec<config::ModelData> = serde_json::from_value(json!([
        {"id": "x", "protocol": "openai-responses", "base_url": "https://example.test"}
    ]))
    .unwrap();
    let meta = CatalogMeta {
        sources: vec!["provider".into()],
        ..Default::default()
    };
    assert!(retained_rows(&b"{} trailing"[..], &models, &meta).is_err());
    assert!(!valid_catalog(&b"{} trailing"[..]));
    assert!(!valid_catalog(&b"[]"[..]));
    assert!(valid_catalog(&b"{}"[..]));
}

#[test]
fn streaming_visitors_report_expected_object_shapes_and_ignore_non_map_providers() {
    let models: Vec<config::ModelData> = serde_json::from_value(json!([
        {"id": "x", "protocol": "openai-responses", "base_url": "https://example.test"}
    ]))
    .unwrap();
    let meta = CatalogMeta {
        sources: vec!["provider".into()],
        ..Default::default()
    };
    for value in [
        "[]",
        "true",
        "1",
        "18446744073709551615",
        "1.5",
        "\"text\"",
        "null",
    ] {
        let input = format!("{{\"provider\":{value}}}");
        assert_eq!(
            retained_rows(input.as_bytes(), &models, &meta).unwrap(),
            json!({"provider":{}}),
            "{value}"
        );
    }
}

#[test]
fn provider_visitor_retains_negative_integer_as_an_empty_object() {
    let models: Vec<config::ModelData> = serde_json::from_value(json!([
        {"id": "x", "protocol": "openai-responses", "base_url": "https://example.test"}
    ]))
    .unwrap();
    let meta = CatalogMeta {
        sources: vec!["provider".into()],
        ..Default::default()
    };
    let retained = retained_rows(br#"{"provider":-1}"#.as_slice(), &models, &meta).unwrap();
    assert_eq!(retained, json!({"provider": {}}));
}

#[test]
fn provider_visitor_retains_escaped_string_as_an_empty_object() {
    let models: Vec<config::ModelData> = serde_json::from_value(json!([
        {"id": "x", "protocol": "openai-responses", "base_url": "https://example.test"}
    ]))
    .unwrap();
    let meta = CatalogMeta {
        sources: vec!["provider".into()],
        ..Default::default()
    };
    // This escaped string still reaches visit_str through serde_json's from_reader deserializer.
    let retained =
        retained_rows(br#"{"provider":"te\u0078t"}"#.as_slice(), &models, &meta).unwrap();
    assert_eq!(retained, json!({"provider": {}}));
}

#[test]
fn catalog_visitor_error_names_expected_json_object() {
    let models: Vec<config::ModelData> = serde_json::from_value(json!([
        {"id": "x", "protocol": "openai-responses", "base_url": "https://example.test"}
    ]))
    .unwrap();
    let meta = CatalogMeta {
        sources: vec!["provider".into()],
        ..Default::default()
    };
    let error = retained_rows(&b"[]"[..], &models, &meta).unwrap_err();
    assert!(error.to_string().contains("a JSON object"), "{error}");
}

#[test]
fn provider_visitor_error_names_expected_provider_object() {
    let ids = std::collections::BTreeSet::new();
    let error = <serde_json::Error as serde::de::Error>::invalid_type(
        serde::de::Unexpected::Bool(true),
        &ProviderVisitor(&ids),
    );
    assert!(error.to_string().contains("a provider object"), "{error}");
}

#[test]
fn valid_catalog_error_names_expected_json_object() {
    let error = validate_catalog(&b"[]"[..]).unwrap_err();
    assert!(error.to_string().contains("a JSON object"), "{error}");
}

#[test]
fn retained_rows_keeps_only_resolved_catalog_entries() {
    let catalog = json!({
        "provider": {
            "served": {"status": "active"},
            "fallback": {"status": "deprecated"},
            "irrelevant": {"status": "active"}
        },
        "other": {
            "unserved": {"status": "active"}
        }
    });
    let models: Vec<config::ModelData> = serde_json::from_value(json!([
        {"id": "served", "protocol": "openai-responses", "base_url": "https://example.test"},
        {"id": "fallback", "protocol": "openai-responses", "base_url": "https://example.test"}
    ]))
    .unwrap();
    let meta = CatalogMeta {
        source: None,
        pins: Default::default(),
        strip_prefix: None,
        split_source: false,
        sources: vec!["provider".into()],
        source_aliases: Default::default(),
    };

    let bytes = serde_json::to_vec(&catalog).unwrap();
    assert_eq!(
        retained_rows(&bytes[..], &models, &meta).unwrap(),
        json!({
            "provider": {
                "served": {"status": "active"},
                "fallback": {"status": "deprecated"}
            }
        })
    );
}
