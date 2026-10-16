//! Borrowing layer views and the `get` fast path (`super::view`,
//! `super::Config::get_segments`): what the clone count is made of, and
//! that the fast path reads what `merged` reads.

use std::borrow::Cow;

use serde_json::json;

use super::view;

#[test]
fn view_borrows_without_a_model() {
    let layer = json!({"model": "a/b", "models": {"a/b": {"model": "c/d"}}});
    assert!(matches!(view(&layer, None), Cow::Borrowed(_)));
}

#[test]
fn view_borrows_a_layer_with_no_entry_for_the_model() {
    let layer = json!({"model": "a/b", "models": {"x/y": {"model": "c/d"}}});
    let seen = view(&layer, Some("a/b"));
    assert!(matches!(seen, Cow::Borrowed(_)));
    assert_eq!(seen["model"], json!("a/b"));
}

#[test]
fn view_owns_a_layer_with_an_entry_for_the_model() {
    let layer = json!({"model": "a/b", "models": {"a/b": {"model": "c/d"}}});
    let seen = view(&layer, Some("a/b"));
    assert!(matches!(seen, Cow::Owned(_)));
    assert_eq!(seen["model"], json!("c/d"));
}
