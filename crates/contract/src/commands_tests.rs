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
        json!({"id": "c", "command": "subscribe", "args": {"level": "full"}}),
        json!({"id": "c", "command": "subscribe", "args": {"level": "summary"}}),
        json!({"id": "c", "command": "prompt", "args": {"content": content}}),
        json!({"id": "c", "command": "steer", "args": {"content": content}}),
        json!({"id": "c", "command": "steer_drop", "args": {"command_id": "c0"}}),
        json!({"id": "c", "command": "message", "args": {"from_session_id": "s", "text": "t"}}),
        json!({"id": "c", "command": "cancel"}),
        json!({"id": "c", "command": "reply",
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
        json!({"id": "c", "command": "history", "args": {"from_seq": 0, "to_seq": 10}}),
        json!({"id": "c", "command": "history", "args": {"from_seq": 0}}),
        json!({"id": "c", "command": "model",
            "args": {"model": "opus", "thinking": "high"}}),
        json!({"id": "c", "command": "model", "args": {"model": "opus"}}),
        json!({"id": "c", "command": "credential", "args": {"label": "work"}}),
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
fn a_missing_args_is_read_as_an_empty_object() {
    let line = parse(r#"{"id":"c","command":"handoff"}"#).unwrap();
    assert_eq!(
        line.command,
        Command::Handoff(Handoff { instructions: None })
    );
    assert_eq!(
        line.command,
        parse(r#"{"id":"c","command":"handoff","args":{}}"#)
            .unwrap()
            .command
    );
    let line = parse(r#"{"id":"c","command":"rewind"}"#).unwrap();
    assert_eq!(
        line.command,
        parse(r#"{"id":"c","command":"rewind","args":{}}"#)
            .unwrap()
            .command
    );
    assert_eq!(
        parse(r#"{"id":"c","command":"cancel"}"#).unwrap().command,
        Command::Cancel
    );
}

/// `command`, which takes no `args`, reads as `expected` with `args` missing
/// or empty, and still refuses a key in them.
fn empty_args_read_as_missing(command: &str, expected: Command) {
    let missing = format!(r#"{{"id":"c","command":"{command}"}}"#);
    let empty = format!(r#"{{"id":"c","command":"{command}","args":{{}}}}"#);
    let extra = format!(r#"{{"id":"c","command":"{command}","args":{{"future":1}}}}"#);
    assert_eq!(parse(&missing).unwrap().command, expected);
    assert_eq!(
        parse(&empty)
            .unwrap_or_else(|e| panic!("{empty}: {e}"))
            .command,
        expected
    );
    assert!(parse(&extra).is_err(), "{extra}");
}

#[test]
fn cancel_reads_an_empty_args_as_a_missing_one() {
    empty_args_read_as_missing("cancel", Command::Cancel);
}

#[test]
fn background_reads_an_empty_args_as_a_missing_one() {
    empty_args_read_as_missing("background", Command::Background);
}

#[test]
fn reload_reads_an_empty_args_as_a_missing_one() {
    empty_args_read_as_missing("reload", Command::Reload);
}

#[test]
fn tools_reads_an_empty_args_as_a_missing_one() {
    empty_args_read_as_missing("tools", Command::Tools);
}

#[test]
fn close_reads_an_empty_args_as_a_missing_one() {
    empty_args_read_as_missing("close", Command::Close);
}

#[test]
fn args_that_are_present_are_never_replaced_by_an_empty_object() {
    // Every key of `handoff` is optional, so only a present `args` that does
    // not fit can be refused.
    assert!(parse(r#"{"id":"c","command":"handoff","args":{"future":1}}"#).is_err());
    assert!(parse(r#"{"id":"c","command":"handoff","args":"x"}"#).is_err());
    assert!(parse(r#"{"id":"c","command":"rewind","args":{"seq":"7"}}"#).is_err());
    assert!(parse(r#"{"id":"c","args":{}}"#).is_err());
}

#[test]
fn a_missing_args_on_a_command_with_a_required_key_is_refused() {
    for command in [
        "subscribe",
        "prompt",
        "steer",
        "steer_drop",
        "history",
        "model",
        "name",
        "shell",
    ] {
        let line = format!(r#"{{"id":"c","command":"{command}"}}"#);
        assert!(parse(&line).is_err(), "{command} without args");
    }
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
fn subscribe_args_that_do_not_fit_are_invalid_arguments() {
    // docs/invocation.md, "The command line": a missing key, a key of the
    // wrong type or a key the command does not take is invalid_arguments.
    assert!(parse(r#"{"id":"c","command":"subscribe","args":{}}"#).is_err());
    assert!(parse(r#"{"id":"c","command":"subscribe","args":{"level":"partial"}}"#).is_err());
    assert!(
        parse(r#"{"id":"c","command":"subscribe","args":{"level":"full","extra":true}}"#).is_err()
    );
}

#[test]
fn history_args_that_do_not_fit_are_invalid_arguments() {
    assert!(parse(r#"{"id":"c","command":"history","args":{}}"#).is_err());
    assert!(parse(r#"{"id":"c","command":"history","args":{"from_seq":"0"}}"#).is_err());
    assert!(parse(r#"{"id":"c","command":"history","args":{"from_seq":0,"extra":true}}"#).is_err());
}

#[test]
fn an_optional_arg_set_to_null_is_invalid_arguments() {
    assert!(
        parse(r#"{"id":"c","command":"history","args":{"from_seq":0,"to_seq":null}}"#).is_err()
    );
    assert!(
        parse(r#"{"id":"c","command":"model","args":{"model":"opus","thinking":null}}"#).is_err()
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

#[test]
fn a_form_answer_with_a_key_it_does_not_take_is_refused() {
    for answer in [
        r#"{"skipped":true,"future":1}"#,
        r#"{"labels":[],"future":1}"#,
    ] {
        let line = format!(
            r#"{{"id":"c","command":"reply","args":{{"request_id":"r","answers":[{answer}]}}}}"#
        );
        assert!(parse(&line).is_err(), "{answer}");
    }
}

#[test]
fn declined_and_skipped_are_only_ever_true() {
    assert!(
        parse(r#"{"id":"c","command":"reply","args":{"request_id":"r","declined":false}}"#)
            .is_err()
    );
    assert!(
        parse(r#"{"id":"c","command":"reply","args":{"request_id":"r","answers":[{"skipped":false}]}}"#)
            .is_err()
    );
}

#[test]
fn a_key_not_in_the_command_line_is_refused() {
    assert!(parse(r#"{"id":"c","command":"cancel","extra":1}"#).is_err());
    assert!(parse(r#"{"id":"c","command":"cancel","session_id":"s_1"}"#).is_err());
}
