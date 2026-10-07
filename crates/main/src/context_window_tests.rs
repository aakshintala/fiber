//! The declared context window (`docs/configuration.md`, "Keys"): a model
//! without a positive window never starts a session.

use super::context_window as resolve;
use config::ModelData;
use contract::ErrorCode;
use serde_json::json;

fn model_with(window: serde_json::Value) -> ModelData {
    let mut data = json!({
        "id": "m",
        "protocol": "openai-responses",
        "base_url": "https://x/v1",
    });
    if !window.is_null() {
        data["context_window"] = window;
    }
    serde_json::from_value(data).unwrap()
}

#[test]
fn a_missing_window_is_no_model() {
    let model = model_with(json!(null));
    let Err(failure) = resolve(&model, "fake/m") else {
        panic!("a missing window resolves");
    };
    assert_eq!(failure.code, ErrorCode::NoModel);
    assert_eq!(
        failure.message,
        "The model `fake/m` declares no `context_window`."
    );
}

#[test]
fn a_zero_window_is_no_model() {
    let model = model_with(json!(0));
    let Err(failure) = resolve(&model, "fake/m") else {
        panic!("a zero window resolves");
    };
    assert_eq!(failure.code, ErrorCode::NoModel);
    assert_eq!(
        failure.message,
        "The model `fake/m` declares no `context_window`."
    );
}

#[test]
fn a_positive_window_resolves() {
    let model = model_with(json!(1_000));
    assert_eq!(resolve(&model, "fake/m").unwrap(), 1_000);
}
