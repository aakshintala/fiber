//! Tests for the rewind note: what the session's tools did after the point,
//! read from the logged effects only.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::path::Path;

use contract::events::{Event, ToolCallRequested, ToolCallStarted};
use contract::shapes::{DeclaredEffects, Effect};
use contract::{ActionId, Envelope, SCHEMA_VERSION, Seq, SessionId};
use serde_json::{Value, json};

use super::*;

fn requested(name: &str, arguments: Value) -> Event {
    Event::ToolCallRequested(ToolCallRequested {
        name: name.to_owned(),
        arguments,
        provider_id: None,
        repair: None,
        ran_by: None,
        provider_item: None,
    })
}

fn started(effects: Vec<Effect>, paths: Option<Vec<&str>>, arguments: Option<Value>) -> Event {
    Event::ToolCallStarted(ToolCallStarted {
        declared: DeclaredEffects {
            effects,
            reversible: true,
            paths: paths.map(|paths| paths.into_iter().map(str::to_owned).collect()),
        },
        arguments: arguments.and_then(|arguments| match arguments {
            Value::Object(map) => Some(map),
            Value::Null
            | Value::Bool(_)
            | Value::Number(_)
            | Value::String(_)
            | Value::Array(_) => None,
        }),
        changed_by: None,
    })
}

fn envelope(seq: u64, action: &str, event: &Event) -> Envelope {
    Envelope {
        kind: event.kind().to_owned(),
        session_id: SessionId("s_1".to_owned()),
        ts: 1_759_150_000_000 + seq,
        schema_version: SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(ActionId(action.to_owned())),
        seq: Some(Seq(seq)),
        payload: event.payload().unwrap(),
    }
}

fn write_log(dir: &Path, lines: &[Envelope]) {
    fs::create_dir_all(dir).unwrap();
    let mut text = String::new();
    for line in lines {
        text.push_str(&serde_json::to_string(line).unwrap());
        text.push('\n');
    }
    fs::write(dir.join("events.jsonl"), text).unwrap();
}

/// One call after the point: its action, the tool's name, the requested
/// arguments, its effects, its paths, and the arguments that ran, when a
/// hook rewrote them.
type Call<'a> = (
    &'a str,
    &'a str,
    Value,
    Vec<Effect>,
    Option<Vec<&'a str>>,
    Option<Value>,
);

/// `tool_call_requested` lines by action, with `started` lines after the
/// point in `calls`.
fn log_with(dir: &Path, calls: &[Call<'_>]) {
    let mut lines = vec![envelope(
        0,
        "a_0",
        &Event::SessionStarted(contract::events::SessionStarted {
            workspace: "/w".to_owned(),
            variables: contract::events::Variables {
                path: "/usr/bin".to_owned(),
                names: Vec::new(),
                source: contract::events::VariablesSource::Inherited,
            },
            parent: None,
            forked_from: None,
            rewind: None,
            worktree: None,
        }),
    )];
    for (index, (action, name, arguments, effects, paths, ran)) in calls.iter().enumerate() {
        let seq = u64::try_from(index).unwrap() + 1;
        lines.push(envelope(seq, action, &requested(name, arguments.clone())));
        lines.push(envelope(
            seq + 100,
            action,
            &started(effects.clone(), paths.clone(), ran.clone()),
        ));
    }
    write_log(dir, &lines);
}

fn note_for(calls: &[Call<'_>]) -> String {
    let home = fakes::TempDir::new("loop-rewind-note");
    let dir = home.path().join("s_1");
    log_with(&dir, calls);
    rewind_note(&dir, Seq(100)).unwrap()
}

#[test]
fn no_tool_call_after_the_point_names_nothing() {
    let note = note_for(&[(
        "a_1",
        "read",
        json!({"city": "Paris"}),
        vec![Effect::Reads],
        None,
        None,
    )]);
    assert!(note.contains("No file was written and no command was run after this point."));
}

#[test]
fn paths_written_after_the_point_are_listed() {
    let note = note_for(&[(
        "a_1",
        "write",
        json!({"city": "Paris"}),
        vec![Effect::Writes],
        Some(vec!["/w/a.txt"]),
        None,
    )]);
    assert!(note.contains("Files written after this point:\n- /w/a.txt"));
    assert!(!note.contains("Commands run"));
}

#[test]
fn commands_run_after_the_point_are_listed_with_their_arguments() {
    let note = note_for(&[(
        "a_1",
        "shell",
        json!({"command": "make"}),
        vec![Effect::Executes],
        None,
        None,
    )]);
    assert!(note.contains("Commands run after this point that may have changed files:"));
    assert!(note.contains(r#"- shell: {"command":"make"}"#));
    assert!(!note.contains("Files written"));
}

#[test]
fn written_paths_and_commands_join_with_a_blank_line() {
    let note = note_for(&[
        (
            "a_1",
            "write",
            json!({"city": "Paris"}),
            vec![Effect::Writes],
            Some(vec!["/w/a.txt"]),
            None,
        ),
        (
            "a_2",
            "shell",
            json!({"command": "make"}),
            vec![Effect::Executes],
            None,
            None,
        ),
    ]);
    assert!(note.contains("- /w/a.txt\n\nCommands run after this point"));
}

#[test]
fn duplicate_paths_are_kept_once_in_first_seen_order() {
    let note = note_for(&[
        (
            "a_1",
            "write",
            json!({"city": "Paris"}),
            vec![Effect::Writes],
            Some(vec!["/w/b.txt", "/w/a.txt"]),
            None,
        ),
        (
            "a_2",
            "write",
            json!({"city": "Paris"}),
            vec![Effect::Writes],
            Some(vec!["/w/a.txt", "/w/c.txt"]),
            None,
        ),
    ]);
    assert!(note.contains("- /w/b.txt\n- /w/a.txt\n- /w/c.txt"));
}

#[test]
fn a_writes_call_with_no_paths_is_listed_with_the_commands() {
    let note = note_for(&[(
        "a_1",
        "write",
        json!({"city": "Paris"}),
        vec![Effect::Writes],
        None,
        None,
    )]);
    assert!(note.contains(r#"- write: {"city":"Paris"}"#));
    assert!(!note.contains("Files written"));
}

#[test]
fn hook_rewritten_arguments_win_over_the_requested_ones() {
    let note = note_for(&[(
        "a_1",
        "shell",
        json!({"command": "make check"}),
        vec![Effect::Executes],
        None,
        Some(json!({"command": "make"})),
    )]);
    assert!(note.contains(r#"- shell: {"command":"make"}"#));
    assert!(!note.contains("make check"));
}

#[test]
fn a_call_before_the_point_is_not_listed() {
    let home = fakes::TempDir::new("loop-rewind-note-point");
    let dir = home.path().join("s_1");
    log_with(
        &dir,
        &[(
            "a_1",
            "shell",
            json!({"command": "make"}),
            vec![Effect::Executes],
            None,
            None,
        )],
    );
    let note = rewind_note(&dir, Seq(101)).unwrap();
    assert!(note.contains("No file was written and no command was run after this point."));
}
