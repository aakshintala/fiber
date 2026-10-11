//! The one `strict` walker: the tools that fit the vendor's strict subset
//! carry only strict-shape schema keywords, with every property required.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use std::collections::BTreeSet;
use std::path::Path;

use contract::tool::Tool;
use fakes::clock::FakeClock;
use serde_json::Value;

use super::{Files, Handoff, WebFetch};

const KEYWORDS: &[&str] = &[
    "type",
    "properties",
    "required",
    "additionalProperties",
    "items",
    "enum",
    "description",
];

fn strict(schema: &Value) {
    let Some(map) = schema.as_object() else {
        panic!("schema node is an object");
    };
    for key in map.keys() {
        assert!(KEYWORDS.contains(&key.as_str()), "{key}");
    }
    match map.get("type").and_then(Value::as_str) {
        Some("object") => {
            let properties = map.get("properties").unwrap().as_object().unwrap();
            let required: BTreeSet<_> = map
                .get("required")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap())
                .collect();
            let names: BTreeSet<_> = properties.keys().map(String::as_str).collect();
            assert_eq!(required, names);
            assert_eq!(map.get("additionalProperties"), Some(&Value::Bool(false)));
            for property in properties.values() {
                strict(property);
            }
        }
        Some("array") => strict(map.get("items").unwrap()),
        Some("string" | "number" | "integer" | "boolean" | "null") => {}
        _ => panic!("schema type"),
    }
}

#[test]
fn strict_tools_fit_the_strict_shape() {
    let files = Files::new(Path::new("/ws").to_path_buf());
    strict(&files.write().definition().input_schema);
    strict(&files.edit().definition().input_schema);
    strict(&Handoff.definition().input_schema);
    let fetch = WebFetch::new(Path::new("/ws").join("artifacts"), FakeClock::new());
    strict(&fetch.definition().input_schema);
}
