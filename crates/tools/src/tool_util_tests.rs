//! Tests for [`crate::tool_util`].

use std::path::Path;

use contract::ErrorCode;
use contract::shapes::{ContentPart, Effect};
use serde::Deserialize;
use serde_json::json;

use super::{Act, arguments, cancelled_before, effects, failed, no_effects, recheck};

#[test]
fn the_failed_output_carries_the_code_and_the_message_with_a_newline() {
    let output = failed(ErrorCode::ToolError, "boom".to_owned());
    assert_eq!(
        output.content,
        vec![ContentPart::Text {
            text: "boom\n".to_owned()
        }]
    );
    let error = output.error.expect("failed carries an error");
    assert_eq!(error.code, ErrorCode::ToolError);
    assert_eq!(error.message, "boom");
    assert_eq!(error.retry_after_ms, None);
    assert_eq!(error.provider, None);
}

#[test]
fn cancelled_before_says_so_with_no_error() {
    let output = cancelled_before();
    assert_eq!(
        output.content,
        vec![ContentPart::Text {
            text: "Cancelled before it started.\n".to_owned()
        }]
    );
    assert_eq!(output.error, None);
}

#[test]
fn no_effects_is_reversible_with_an_empty_subject() {
    let got = no_effects();
    assert!(got.declared.effects.is_empty());
    assert!(got.declared.reversible);
    assert_eq!(got.declared.paths, None);
    assert_eq!(got.subject, Some(String::new()));
    assert_eq!(got.prefix, None);
    assert!(!got.always_reviewed);
}

#[test]
fn effects_carries_each_field() {
    let got = effects(
        vec![Effect::Reads],
        false,
        Some(vec!["a".to_owned()]),
        Some("subject".to_owned()),
        Some("prefix".to_owned()),
    );
    assert_eq!(got.declared.effects, vec![Effect::Reads]);
    assert!(!got.declared.reversible);
    assert_eq!(got.declared.paths, Some(vec!["a".to_owned()]));
    assert_eq!(got.subject, Some("subject".to_owned()));
    assert_eq!(got.prefix, Some("prefix".to_owned()));
    assert!(!got.always_reviewed);
}

#[test]
fn recheck_fails_when_the_held_path_differs() {
    let output = recheck(
        "a.txt",
        Path::new("/work/b.txt"),
        Some(Path::new("/work/a.txt")),
        None,
        Act::Write,
    )
    .expect_err("a moved path fails");
    assert_eq!(
        output.content,
        vec![ContentPart::Text {
            text:
                "`a.txt` changed between the permission check and the write. Nothing was written.\n"
                    .to_owned()
        }]
    );
    assert_eq!(
        output.error.map(|error| error.code),
        Some(ErrorCode::PathChanged)
    );
}

#[test]
fn recheck_fails_when_the_judged_path_differs() {
    let output = recheck(
        "a.txt",
        Path::new("/work/a.txt"),
        Some(Path::new("/work/a.txt")),
        Some(Path::new("/work/other.txt")),
        Act::Write,
    )
    .expect_err("a retargeted path fails");
    assert!(matches!(
        output.error.map(|error| error.code),
        Some(ErrorCode::PathChanged)
    ));
}

#[test]
fn recheck_passes_when_both_paths_agree() {
    recheck(
        "a.txt",
        Path::new("/work/a.txt"),
        Some(Path::new("/work/a.txt")),
        Some(Path::new("/work/a.txt")),
        Act::Write,
    )
    .expect("agreeing paths pass");
}

#[test]
fn recheck_passes_without_a_held_or_judged_path() {
    recheck(
        Path::new("a.txt").as_os_str().to_str().unwrap_or("a.txt"),
        Path::new("/work/a.txt"),
        None,
        None,
        Act::Read,
    )
    .expect("a read with no lock passes");
}

#[test]
fn recheck_names_the_read() {
    let output = recheck(
        "a.txt",
        Path::new("/work/b.txt"),
        Some(Path::new("/work/a.txt")),
        None,
        Act::Read,
    )
    .expect_err("a moved read fails");
    assert_eq!(
        output.content,
        vec![ContentPart::Text {
            text: "`a.txt` changed between the permission check and the read. Nothing was read.\n"
                .to_owned()
        }]
    );
}

#[derive(Debug, PartialEq, Deserialize)]
struct Args {
    path: String,
}

#[test]
fn arguments_names_a_missing_field() {
    let empty = serde_json::Map::new();
    let error = arguments::<Args>(&empty).expect_err("a missing path fails");
    assert!(
        error.starts_with("The arguments do not fit the schema:"),
        "unexpected message: {error}"
    );
    assert!(
        error.contains("missing field `path`"),
        "unexpected message: {error}"
    );
    let map = json!({"path": "a.txt"}).as_object().unwrap().clone();
    let args = arguments::<Args>(&map).expect("a well-formed map parses");
    assert_eq!(args.path, "a.txt");
}
