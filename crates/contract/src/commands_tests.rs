use std::collections::BTreeSet;

use serde_json::{Value, json};

use super::*;

const DOC: &str = include_str!("../../../docs/invocation.md");

/// Every line of every `json` block in `docs/invocation.md`.
fn doc_examples() -> Vec<&'static str> {
    DOC.split("```json\n")
        .skip(1)
        .flat_map(|block| block.split("```").next().unwrap().lines())
        .collect()
}

/// The names in the first column of the `| Command | `args` |` table.
fn doc_commands() -> BTreeSet<&'static str> {
    let table = DOC.split("| Command | `args` |").nth(1).unwrap();
    table
        .lines()
        .skip(2)
        .take_while(|line| line.starts_with('|'))
        .map(|line| line.split('`').nth(1).unwrap())
        .collect()
}

fn parse(line: &str) -> Result<CommandLine, serde_json::Error> {
    serde_json::from_str(line)
}

#[test]
fn the_doc_examples_round_trip_byte_for_byte() {
    let examples = doc_examples();
    assert_eq!(examples.len(), 2);
    for line in examples {
        assert_eq!(serde_json::to_string(&parse(line).unwrap()).unwrap(), line);
    }
}

#[test]
fn the_doc_reply_example_reads_as_an_approval() {
    let line = parse(doc_examples()[1]).unwrap();
    assert_eq!(line.id, CommandId("c_91be".into()));
    let Command::Reply(reply) = line.command else {
        panic!("not a reply");
    };
    assert_eq!(reply.request_id, RequestId("r_2c01".into()));
    assert_eq!(
        reply.answer,
        ReplyAnswer::Approval {
            decision: Decision::Allow,
            feedback: None,
            remember: Some(Remember {
                scope: RememberScope::Project,
                prefix: "npm test".into()
            }),
        }
    );
}

/// One line per command, with every key it takes.
fn samples() -> Vec<Value> {
    let content = json!([{"type": "text", "text": "t"},
        {"type": "image", "data": "aGk=", "mime_type": "image/png"}]);
    vec![
        json!({"id": "c", "command": "prompt", "args": {"content": content}}),
        json!({"id": "c", "command": "steer", "session_id": "d", "args": {"content": content}}),
        json!({"id": "c", "command": "steer_amend",
            "args": {"command_id": "c0", "content": content}}),
        json!({"id": "c", "command": "steer_drop", "args": {"command_id": "c0"}}),
        json!({"id": "c", "command": "message", "args": {"from_session_id": "s", "text": "t"}}),
        json!({"id": "c", "command": "cancel"}),
        json!({"id": "c", "command": "reply", "session_id": "d",
            "args": {"request_id": "r", "declined": true}}),
        json!({"id": "c", "command": "reply", "args": {"request_id": "r", "confirmed": true}}),
        json!({"id": "c", "command": "reply", "args": {"request_id": "r", "labels": ["a"]}}),
        json!({"id": "c", "command": "reply", "args": {"request_id": "r", "text": "t"}}),
        json!({"id": "c", "command": "reply", "args": {"request_id": "r",
            "answers": [{"skipped": true}, {"labels": [], "text": "t"}], "note": "n"}}),
        json!({"id": "c", "command": "reply", "args": {"request_id": "r", "decision": "deny",
            "feedback": "no"}}),
        json!({"id": "c", "command": "reply", "args": {"request_id": "r", "decision": "allow",
            "remember": {"scope": "session", "prefix": "ls"}}}),
        json!({"id": "c", "command": "job_stop", "args": {"job_id": "j"}}),
        json!({"id": "c", "command": "background"}),
        json!({"id": "c", "command": "reload"}),
        json!({"id": "c", "command": "tools"}),
        json!({"id": "c", "command": "model",
            "args": {"model": "opus", "effort": "high", "thinking": "on"}}),
        json!({"id": "c", "command": "model", "args": {"model": "opus"}}),
        json!({"id": "c", "command": "mode", "args": {"mode": "readonly"}}),
        json!({"id": "c", "command": "name", "args": {"text": ""}}),
        json!({"id": "c", "command": "handoff", "args": {"instructions": "i"}}),
        json!({"id": "c", "command": "handoff", "args": {}}),
        json!({"id": "c", "command": "rewind", "args": {"from_session_id": "s", "seq": 7,
            "summarise": true, "adopt": ["j"]}}),
        json!({"id": "c", "command": "shell", "args": {"command": "git status", "send": true}}),
        json!({"id": "c", "command": "command", "args": {"name": "review", "text": "all"}}),
        json!({"id": "c", "command": "close"}),
    ]
}

#[test]
fn every_sample_reads_and_writes_back_unchanged() {
    for sample in samples() {
        let line: CommandLine =
            serde_json::from_value(sample.clone()).unwrap_or_else(|e| panic!("{sample}: {e}"));
        assert_eq!(serde_json::to_value(&line).unwrap(), sample);
    }
}

#[test]
fn the_samples_cover_every_command_in_the_doc() {
    let named: BTreeSet<String> = samples()
        .iter()
        .map(|s| s["command"].as_str().unwrap().to_owned())
        .collect();
    let doc: BTreeSet<String> = doc_commands().into_iter().map(str::to_owned).collect();
    assert_eq!(named, doc);
}

#[test]
fn defaults_fill_what_a_client_leaves_out() {
    let line = parse(r#"{"id":"c","command":"rewind","args":{}}"#).unwrap();
    assert_eq!(
        line.command,
        Command::Rewind(RewindArgs {
            from_session_id: None,
            seq: None,
            summarise: false,
            adopt: vec![],
        })
    );
    let line = parse(r#"{"id":"c","command":"shell","args":{"command":"ls"}}"#).unwrap();
    assert_eq!(
        line.command,
        Command::Shell(Shell {
            command: "ls".into(),
            send: false
        })
    );
}

#[test]
fn a_key_the_line_does_not_take_is_refused() {
    assert!(parse(r#"{"id":"c","command":"cancel","future":1}"#).is_err());
}

#[test]
fn a_key_the_command_does_not_take_is_refused() {
    assert!(
        parse(r#"{"id":"c","command":"job_stop","args":{"job_id":"j","force":true}}"#).is_err()
    );
    assert!(
        parse(r#"{"id":"c","command":"reply","args":{"request_id":"r","confirmed":true,"x":1}}"#)
            .is_err()
    );
    assert!(
        parse(r#"{"id":"c","command":"prompt","args":{"content":[{"type":"text","text":"t","path":"p"}]}}"#)
            .is_err()
    );
}

#[test]
fn an_unknown_command_is_refused() {
    assert!(parse(r#"{"id":"c","command":"teleport"}"#).is_err());
}

#[test]
fn a_line_without_an_id_is_refused() {
    assert!(parse(r#"{"command":"cancel"}"#).is_err());
}
