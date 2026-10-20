//! Tests for `render_loaded::render`: the On/Off matrix over empty and
//! non-empty D and logged lists, plus the length and identity pair.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use std::collections::BTreeSet;

use contract::provider::{Input, NO_MATCH, loaded_text};

use super::render;

fn action(id: &str) -> contract::ActionId {
    contract::ActionId(id.into())
}

fn search(id: &str, loaded: &[&str]) -> Input {
    Input::ToolSearchResult {
        action_id: action(id),
        loaded: loaded.iter().map(|name| (*name).to_owned()).collect(),
    }
}

fn on(names: &[&str]) -> Option<BTreeSet<String>> {
    Some(names.iter().map(|name| (*name).to_owned()).collect())
}

fn result_text(input: &Input) -> &str {
    match input {
        Input::ToolResult { text, .. } => text,
        other @ (Input::User { .. }
        | Input::Assistant { .. }
        | Input::Reasoning { .. }
        | Input::ToolCall { .. }
        | Input::ToolSearchResult { .. }) => panic!("expected a ToolResult, got {other:?}"),
    }
}

fn result_is_error(input: &Input) -> bool {
    match input {
        Input::ToolResult { is_error, .. } => *is_error,
        other @ (Input::User { .. }
        | Input::Assistant { .. }
        | Input::Reasoning { .. }
        | Input::ToolCall { .. }
        | Input::ToolSearchResult { .. }) => panic!("expected a ToolResult, got {other:?}"),
    }
}

fn search_loaded(input: &Input) -> &[String] {
    match input {
        Input::ToolSearchResult { loaded, .. } => loaded,
        other @ (Input::User { .. }
        | Input::Assistant { .. }
        | Input::Reasoning { .. }
        | Input::ToolCall { .. }
        | Input::ToolResult { .. }) => panic!("expected a ToolSearchResult, got {other:?}"),
    }
}

#[test]
fn on_with_both_deferred_keeps_logged_order() {
    let out = render(vec![search("a_1", &["b", "a"])], on(&["a", "b"]).as_ref());
    assert_eq!(out.len(), 1);
    assert_eq!(search_loaded(&out[0]), &["b".to_owned(), "a".to_owned()]);
}

#[test]
fn on_with_one_deferred_keeps_only_it() {
    let out = render(vec![search("a_1", &["a", "b"])], on(&["a"]).as_ref());
    assert_eq!(out.len(), 1);
    assert_eq!(search_loaded(&out[0]), &["a".to_owned()]);
}

#[test]
fn on_with_none_deferred_sends_no_match() {
    let out = render(vec![search("a_1", &["a"])], on(&["c"]).as_ref());
    assert_eq!(out.len(), 1);
    assert_eq!(result_text(&out[0]), NO_MATCH);
    assert!(!result_is_error(&out[0]));
}

#[test]
fn on_with_empty_d_sends_no_match() {
    let out = render(vec![search("a_1", &["a"])], on(&[]).as_ref());
    assert_eq!(out.len(), 1);
    assert_eq!(result_text(&out[0]), NO_MATCH);
    assert!(!result_is_error(&out[0]));
}

#[test]
fn on_with_empty_logged_sends_no_match() {
    let out = render(vec![search("a_1", &[])], on(&["a"]).as_ref());
    assert_eq!(out.len(), 1);
    assert_eq!(result_text(&out[0]), NO_MATCH);
    assert!(!result_is_error(&out[0]));
}

#[test]
fn off_renders_the_logged_list_as_text() {
    let out = render(vec![search("a_1", &["a", "b"])], None);
    assert_eq!(out.len(), 1);
    assert_eq!(
        result_text(&out[0]),
        loaded_text(&["a".to_owned(), "b".to_owned()])
    );
    assert_eq!(result_text(&out[0]), "Loaded: a, b.");
    assert!(!result_is_error(&out[0]));
}

#[test]
fn off_with_empty_logged_sends_no_match() {
    let out = render(vec![search("a_1", &[])], None);
    assert_eq!(out.len(), 1);
    assert_eq!(result_text(&out[0]), NO_MATCH);
    assert!(!result_is_error(&out[0]));
}

#[test]
fn other_inputs_cross_unchanged_and_length_holds() {
    let user = Input::User {
        text: "hi".into(),
        images: Vec::new(),
    };
    let result = Input::ToolResult {
        action_id: action("a_9"),
        text: "done".into(),
        is_error: true,
        images: Vec::new(),
        pdfs: Vec::new(),
    };
    let out = render(
        vec![user.clone(), search("a_1", &["a", "b"]), result.clone()],
        on(&["a"]).as_ref(),
    );
    assert_eq!(out.len(), 3);
    assert_eq!(out[0], user);
    assert_eq!(search_loaded(&out[1]), &["a".to_owned()]);
    assert_eq!(out[2], result);
}
