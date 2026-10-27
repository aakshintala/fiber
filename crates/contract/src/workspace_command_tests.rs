//! Tests for the `change_workspace` driver command
//! (`docs/invocation.md`, "Driver commands"): its shape, its path input
//! classes, and the accepted result it answers with.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use serde_json::json;

use super::{ChangeWorkspaceArgs, Command, CommandLine};
use crate::events::CommandResult;

fn parse(line: &str) -> Result<CommandLine, serde_json::Error> {
    serde_json::from_str(line)
}

#[test]
fn change_workspace_round_trips_with_its_path() {
    for path in ["../other", "/work/other", "with space", "~/other", "~"] {
        let line =
            format!(r#"{{"id":"c_cd","command":"change_workspace","args":{{"path":{path:?}}}}}"#);
        let parsed = parse(&line).unwrap_or_else(|e| panic!("{line}: {e}"));
        assert_eq!(
            parsed.command,
            Command::ChangeWorkspace(ChangeWorkspaceArgs { path: path.into() }),
            "{line}"
        );
        assert_eq!(serde_json::to_string(&parsed).unwrap(), line, "{line}");
    }
}

#[test]
fn change_workspace_needs_its_path_as_a_string() {
    for args in [
        json!({}),
        json!({"path": null}),
        json!({"path": 7}),
        json!({"path": ["../other"]}),
        json!({"path": "../other", "future": true}),
    ] {
        let line = json!({"id": "c_cd", "command": "change_workspace", "args": args});
        assert!(parse(&line.to_string()).is_err(), "{line}");
    }
    assert!(parse(r#"{"id":"c_cd","command":"change_workspace"}"#).is_err());
}

#[test]
fn the_accepted_result_answers_with_the_new_session_id() {
    let result = CommandResult::Rewind {
        new_session_id: crate::SessionId("s_bbbbbbbbbbbbbbbb".into()),
    };
    assert_eq!(
        serde_json::to_value(&result).unwrap(),
        json!({"new_session_id": "s_bbbbbbbbbbbbbbbb"}),
    );
    let back: CommandResult =
        serde_json::from_value(json!({"new_session_id": "s_bbbbbbbbbbbbbbbb"})).unwrap();
    assert_eq!(back, result);
}
