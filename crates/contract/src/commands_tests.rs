use std::collections::BTreeSet;

use serde_json::{Value, json};

use super::*;
use crate::events::OfferDecision;

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
    assert_eq!(examples.len(), 3);
    for line in examples {
        assert_eq!(serde_json::to_string(&parse(line).unwrap()).unwrap(), line);
    }
}

#[test]
fn the_doc_reply_example_reads_as_an_approval() {
    let line = parse(doc_examples()[2]).unwrap();
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
        json!({"id": "c", "command": "subscribe", "args": {"level": "full", "credential": "work"}}),
        json!({"id": "c", "command": "subscribe", "args": {"level": "summary", "credential": "work"}}),
        json!({"id": "c", "command": "prompt", "args": {"content": content}}),
        json!({"id": "c", "command": "steer", "args": {"content": content}}),
        json!({"id": "c", "command": "steer_drop", "args": {"command_id": "c0"}}),
        json!({"id": "c", "command": "message", "args": {"from_session_id": "s", "text": "t"}}),
        json!({"id": "c", "command": "message", "args": {"from_session_id": "s", "parent_session_id": "s_0123456789abcdef", "text": "t"}}),
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
        json!({"id": "c", "command": "reply", "args": {"request_id": "r",
            "decisions": ["approve", "skip", "never"]}}),
        json!({"id": "c", "command": "call", "args": {"call": "fiber.tool",
            "args": ["echo", {"description": "Echo"}]}}),
        json!({"id": "c", "command": "call", "args": {"call": "host.tool",
            "args": ["read", {"path": "example.txt"}], "request_id": "r_A"}}),
        json!({"id": "c", "command": "ready"}),
        json!({"id": "c", "command": "job_stop", "args": {"job_id": "j"}}),
        json!({"id": "c", "command": "job_input", "args": {"job_id": "j", "text": "t"}}),
        json!({"id": "c", "command": "offered_file", "args": {"request_id": "r", "path": "tools/db.sh"}}),
        json!({"id": "c", "command": "background"}),
        json!({"id": "c", "command": "reload"}),
        json!({"id": "c", "command": "tools"}),
        json!({"id": "c", "command": "hooks"}),
        json!({"id": "c", "command": "mcp_servers"}),
        json!({"id": "c", "command": "mcp_login", "args": {"server": "linear"}}),
        json!({"id": "c", "command": "mcp_reconnect", "args": {"server": "linear"}}),
        json!({"id": "c", "command": "commands"}),
        json!({"id": "c", "command": "skills"}),
        json!({"id": "c", "command": "quota", "args": {"fetch": false}}),
        json!({"id": "c", "command": "credentials"}),
        json!({"id": "c", "command": "history", "args": {"from_seq": 0, "to_seq": 10}}),
        json!({"id": "c", "command": "history", "args": {"from_seq": 0}}),
        json!({"id": "c", "command": "history", "args": {}}),
        json!({"id": "c", "command": "history", "args": {"to_seq": 10}}),
        json!({"id": "c", "command": "history", "args": {"session_id": "s_0123456789abcdef", "to_seq": 10}}),
        json!({"id": "c", "command": "history", "args": {"session_id": "s_0123456789abcdef", "from_seq": 0, "to_seq": 10}}),
        json!({"id": "c", "command": "artifact_page", "args": {"path": "artifacts/a_3.txt", "offset": 0}}),
        json!({"id": "c", "command": "artifact_page", "args": {"path": "artifacts/a_3.txt", "offset": 4096}}),
        json!({"id": "c", "command": "model",
            "args": {"model": "opus", "thinking": "high"}}),
        json!({"id": "c", "command": "model", "args": {"model": "opus"}}),
        json!({"id": "c", "command": "credential", "args": {"label": "work"}}),
        json!({"id": "c", "command": "name", "args": {"text": ""}}),
        json!({"id": "c", "command": "handoff", "args": {"instructions": "i"}}),
        json!({"id": "c", "command": "handoff", "args": {}}),
        json!({"id": "c", "command": "rewind", "args": {"from_session_id": "s", "seq": 7,
            "summarise": true}}),
        json!({"id": "c", "command": "change_workspace", "args": {"path": "../other"}}),
        json!({"id": "c", "command": "shell", "args": {"command": "git status", "send": true}}),
        json!({"id": "c", "command": "propose_notes", "args": {"topic": "routine_actions"}}),
        json!({"id": "c", "command": "propose_notes", "args": {"topic": "routine_actions",
            "answer": "I merge my own PRs", "earlier": ["github.com/acme/* are ours"]}}),
        json!({"id": "c", "command": "command", "args": {"name": "review", "text": "all"}}),
        json!({"id": "c", "command": "close", "args": {"now": true}}),
        json!({"id": "c", "command": "close", "args": {"now": false}}),
        json!({"id": "c", "command": "import", "args": {}}),
        json!({"id": "c", "command": "import", "args": {"source": "codex"}}),
        json!({"id": "c", "command": "import", "args": {"source": "claude-code"}}),
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
    let line = parse(r#"{"id":"c","command":"artifact_page","args":{"path":"artifacts/a_3.txt"}}"#)
        .unwrap();
    assert_eq!(
        line.command,
        Command::ArtifactPage(ArtifactPageArgs {
            path: "artifacts/a_3.txt".into(),
            offset: 0,
        })
    );
    let line = parse(r#"{"id":"c","command":"rewind","args":{}}"#).unwrap();
    assert_eq!(
        line.command,
        Command::Rewind(RewindArgs::Rewind {
            from_session_id: None,
            seq: None,
            summarise: false,
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

/// `AtEnd` refuses any company: `seq`, `from_session_id` or `summarise`,
/// even an explicit `summarise: false` or an explicit `null`, since only
/// the session's own end is a fork point. Absence and `null` differ: an
/// explicit `null` is a wrong type, never absence.
#[test]
fn rewind_at_end_is_exclusive_and_omitted_when_false() {
    let line = parse(r#"{"id":"c","command":"rewind","args":{"at_end":true}}"#).unwrap();
    assert_eq!(line.command, Command::Rewind(RewindArgs::AtEnd));
    for args in [
        json!({"seq": 12, "at_end": true}),
        json!({"from_session_id": "s_0123456789abcdef", "at_end": true}),
        json!({"at_end": true, "summarise": true}),
        json!({"at_end": true, "summarise": false}),
        // Presence counts even when the value is `null`.
        json!({"seq": null, "at_end": true}),
        json!({"from_session_id": null, "at_end": true}),
        json!({"at_end": true, "summarise": null}),
    ] {
        let line = json!({"id": "c", "command": "rewind", "args": args});
        assert!(parse(&line.to_string()).is_err(), "{line}");
        assert!(
            serde_json::from_value::<RewindArgs>(args.clone()).is_err(),
            "{line}"
        );
    }
    // Without `at_end`, the old keys still read.
    let line = parse(r#"{"id":"c","command":"rewind","args":{"seq":12}}"#).unwrap();
    assert_eq!(
        line.command,
        Command::Rewind(RewindArgs::Rewind {
            from_session_id: None,
            seq: Some(Seq(12)),
            summarise: false,
        })
    );
    // A `Rewind` with defaults omits nothing it needs on the wire, so old
    // lines read unchanged.
    let line = CommandLine {
        id: CommandId("c".into()),
        command: Command::Rewind(RewindArgs::Rewind {
            from_session_id: None,
            seq: None,
            summarise: false,
        }),
    };
    assert_eq!(
        serde_json::to_string(&line).unwrap(),
        r#"{"id":"c","command":"rewind","args":{"summarise":false}}"#
    );
    // `AtEnd` is written to the wire as only `at_end` (`/fork` sends it).
    let line = CommandLine {
        id: CommandId("c_rewind".into()),
        command: Command::Rewind(RewindArgs::AtEnd),
    };
    assert_eq!(
        serde_json::to_string(&line).unwrap(),
        r#"{"id":"c_rewind","command":"rewind","args":{"at_end":true}}"#
    );
    // An unknown key still fails to read.
    let line = json!({"id": "c", "command": "rewind", "args": {"at_end": true, "future": 1}});
    assert!(parse(&line.to_string()).is_err(), "{line}");
    // An explicit `null` is never absence, on either mode.
    for args in [
        json!({"seq": null}),
        json!({"from_session_id": null}),
        json!({"summarise": null}),
        json!({"at_end": null}),
        json!({"seq": null, "at_end": false}),
    ] {
        let line = json!({"id": "c", "command": "rewind", "args": args});
        assert!(parse(&line.to_string()).is_err(), "{line}");
        assert!(
            serde_json::from_value::<RewindArgs>(args.clone()).is_err(),
            "{line}"
        );
    }
    // A wrong type is refused on every key, with or without `at_end`.
    for args in [
        json!({"seq": "7"}),
        json!({"seq": 1.5}),
        json!({"from_session_id": 7}),
        json!({"summarise": "yes"}),
        json!({"summarise": 1}),
        json!({"at_end": "yes"}),
        json!({"at_end": 1}),
        json!({"seq": 12, "at_end": "yes"}),
    ] {
        let line = json!({"id": "c", "command": "rewind", "args": args});
        assert!(parse(&line.to_string()).is_err(), "{line}");
        assert!(
            serde_json::from_value::<RewindArgs>(args.clone()).is_err(),
            "{line}"
        );
    }
    // `at_end: false` reads as the default mode, without its own key.
    let line = parse(r#"{"id":"c","command":"rewind","args":{"seq":12,"at_end":false}}"#).unwrap();
    assert_eq!(
        line.command,
        Command::Rewind(RewindArgs::Rewind {
            from_session_id: None,
            seq: Some(Seq(12)),
            summarise: false,
        })
    );
}

#[test]
fn artifact_page_args_that_do_not_fit_are_invalid_arguments() {
    for args in [
        json!({}),
        json!({"offset": 0}),
        json!({"path": "p", "offset": -1}),
        json!({"path": "p", "offset": "0"}),
        json!({"path": "p", "offset": null}),
        json!({"path": 1}),
        json!({"path": "p", "extra": true}),
        json!({"path": "p", "offset": 1.5}),
    ] {
        let line = json!({"id": "c", "command": "artifact_page", "args": args});
        assert!(parse(&line.to_string()).is_err(), "{line}");
    }
    for offset in [0, 1, u64::MAX] {
        let line = json!({"id": "c", "command": "artifact_page",
            "args": {"path": "p", "offset": offset}});
        assert_eq!(
            parse(&line.to_string()).unwrap().command,
            Command::ArtifactPage(ArtifactPageArgs {
                path: "p".into(),
                offset
            })
        );
    }
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
    let line = parse(r#"{"id":"c","command":"history"}"#).unwrap();
    assert_eq!(
        line.command,
        parse(r#"{"id":"c","command":"history","args":{}}"#)
            .unwrap()
            .command
    );
}

/// A command that takes no `args` reads with `args` missing or empty as
/// without it, and still refuses a key in them: the missing-or-empty retry
/// in `CommandLine::deserialize`.
#[test]
fn commands_without_args_read_missing_and_empty_as_missing() {
    for (name, expected) in [
        ("cancel", Command::Cancel),
        ("background", Command::Background),
        ("reload", Command::Reload),
        ("tools", Command::Tools),
        ("hooks", Command::Hooks),
        ("mcp_servers", Command::McpServers),
        ("commands", Command::Commands),
        ("skills", Command::Skills),
        ("credentials", Command::Credentials),
        ("close", Command::Close(CloseArgs { now: false })),
    ] {
        let missing = format!(r#"{{"id":"c","command":"{name}"}}"#);
        let empty = format!(r#"{{"id":"c","command":"{name}","args":{{}}}}"#);
        let extra = format!(r#"{{"id":"c","command":"{name}","args":{{"future":1}}}}"#);
        assert_eq!(parse(&missing).unwrap().command, expected, "{name}");
        assert_eq!(
            parse(&empty)
                .unwrap_or_else(|e| panic!("{empty}: {e}"))
                .command,
            expected,
            "{name}"
        );
        assert!(parse(&extra).is_err(), "{extra}");
    }
    for (name, command) in [
        ("commands", Command::Commands),
        ("skills", Command::Skills),
        ("mcp_servers", Command::McpServers),
        ("credentials", Command::Credentials),
        ("hooks", Command::Hooks),
        ("tools", Command::Tools),
    ] {
        let line = CommandLine {
            id: CommandId("c_1".into()),
            command,
        };
        assert_eq!(
            serde_json::to_string(&line).unwrap(),
            format!(r#"{{"id":"c_1","command":"{name}"}}"#),
            "{name}"
        );
    }
}

/// `quota` takes an all-optional `fetch` key: a missing or empty `args`
/// reads as `fetch: false` through the missing-or-empty retry in
/// `CommandLine::deserialize`.
#[test]
fn quota_fetch_reads_missing_and_empty_as_false() {
    for (args, expected) in [
        (None, Ok(false)),
        (Some(r#"{}"#), Ok(false)),
        (Some(r#"{"fetch":false}"#), Ok(false)),
        (Some(r#"{"fetch":true}"#), Ok(true)),
        (Some(r#"{"fetch":null}"#), Err(())),
        (Some(r#"{"fetch":"yes"}"#), Err(())),
        (Some(r#"{"fetch":1}"#), Err(())),
        (Some(r#"{"fetch":true,"future":1}"#), Err(())),
        (Some(r#"{"future":1}"#), Err(())),
    ] {
        let line = match args {
            None => r#"{"id":"c","command":"quota"}"#.to_owned(),
            Some(args) => format!(r#"{{"id":"c","command":"quota","args":{args}}}"#),
        };
        match expected {
            Ok(fetch) => assert_eq!(
                parse(&line)
                    .unwrap_or_else(|e| panic!("{line}: {e}"))
                    .command,
                Command::Quota(QuotaArgs { fetch }),
                "{line}"
            ),
            Err(()) => assert!(parse(&line).is_err(), "{line}"),
        }
    }
    for fetch in [false, true] {
        let line = CommandLine {
            id: CommandId("c_1".into()),
            command: Command::Quota(QuotaArgs { fetch }),
        };
        assert_eq!(
            serde_json::to_string(&line).unwrap(),
            format!(r#"{{"id":"c_1","command":"quota","args":{{"fetch":{fetch}}}}}"#),
            "fetch {fetch}"
        );
    }
}

#[test]
fn close_now_reads_a_boolean_and_refuses_anything_else() {
    for (args, expected) in [
        (r#"{"now":true}"#, Ok(true)),
        (r#"{"now":false}"#, Ok(false)),
        (r#"{}"#, Ok(false)),
        (r#"{"now":null}"#, Err(())),
        (r#"{"now":"yes"}"#, Err(())),
        (r#"{"now":1}"#, Err(())),
        (r#"{"now":true,"future":1}"#, Err(())),
    ] {
        let line = format!(r#"{{"id":"c","command":"close","args":{args}}}"#);
        match expected {
            Ok(now) => assert_eq!(
                parse(&line)
                    .unwrap_or_else(|e| panic!("{line}: {e}"))
                    .command,
                Command::Close(CloseArgs { now }),
                "{line}"
            ),
            Err(()) => assert!(parse(&line).is_err(), "{line}"),
        }
    }
    assert_eq!(
        parse(r#"{"id":"c","command":"close"}"#).unwrap().command,
        Command::Close(CloseArgs { now: false })
    );
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
fn job_input_reads_and_writes_back_unchanged() {
    let line =
        parse(r#"{"id":"c","command":"job_input","args":{"job_id":"j","text":"hi\u001b[A"}}"#)
            .unwrap();
    assert_eq!(
        line.command,
        Command::JobInput(JobInput {
            job_id: JobId("j".into()),
            // The keys as typed, escape sequences included.
            text: "hi\x1b[A".into(),
        })
    );
    assert_eq!(
        serde_json::to_string(&line).unwrap(),
        r#"{"id":"c","command":"job_input","args":{"job_id":"j","text":"hi\u001b[A"}}"#
    );
}

#[test]
fn job_input_refuses_a_key_it_does_not_take_or_a_missing_one() {
    assert!(
        parse(r#"{"id":"c","command":"job_input","args":{"job_id":"j","text":"t","force":true}}"#)
            .is_err()
    );
    assert!(parse(r#"{"id":"c","command":"job_input","args":{"job_id":"j"}}"#).is_err());
    assert!(parse(r#"{"id":"c","command":"job_input"}"#).is_err());
}

#[test]
fn a_missing_args_on_a_command_with_a_required_key_is_refused() {
    for command in [
        "subscribe",
        "prompt",
        "steer",
        "steer_drop",
        "artifact_page",
        "mcp_login",
        "mcp_reconnect",
        "offered_file",
        "model",
        "name",
        "shell",
        "propose_notes",
    ] {
        let line = format!(r#"{{"id":"c","command":"{command}"}}"#);
        assert!(parse(&line).is_err(), "{command} without args");
    }
}

#[test]
fn mcp_login_and_reconnect_read_their_server_and_refuse_the_rest() {
    for command in ["mcp_login", "mcp_reconnect"] {
        let line = format!(r#"{{"id":"c","command":"{command}","args":{{"server":"linear"}}}}"#);
        let parsed = parse(&line).unwrap();
        let expected = McpServerArgs {
            server: "linear".into(),
        };
        assert_eq!(
            parsed.command,
            if command == "mcp_login" {
                Command::McpLogin(expected)
            } else {
                Command::McpReconnect(expected)
            },
            "{command}"
        );
        assert_eq!(serde_json::to_string(&parsed).unwrap(), line);
        let extra = format!(
            r#"{{"id":"c","command":"{command}","args":{{"server":"linear","future":1}}}}"#
        );
        assert!(parse(&extra).is_err(), "{extra}");
        let missing = format!(r#"{{"id":"c","command":"{command}","args":{{}}}}"#);
        assert!(parse(&missing).is_err(), "{missing}");
    }
}

/// `propose_notes` takes a topic and two optional keys: anything else is
/// refused, and a missing or null `topic` with it.
#[test]
fn propose_notes_reads_its_topic_and_refuses_the_rest() {
    let line = parse(
        r#"{"id":"c","command":"propose_notes","args":{"topic":"routine_actions","answer":"I merge my own PRs","earlier":["github.com/acme/* are ours"]}}"#,
    )
    .unwrap();
    assert_eq!(
        line.command,
        Command::ProposeNotes(crate::notes::ProposeNotesArgs {
            topic: crate::notes::Topic::RoutineActions,
            answer: Some("I merge my own PRs".into()),
            earlier: vec!["github.com/acme/* are ours".into()],
        })
    );
    assert_eq!(
        serde_json::to_string(&line).unwrap(),
        r#"{"id":"c","command":"propose_notes","args":{"topic":"routine_actions","answer":"I merge my own PRs","earlier":["github.com/acme/* are ours"]}}"#
    );
    let line = parse(r#"{"id":"c","command":"propose_notes","args":{"topic":"never"}}"#).unwrap();
    assert_eq!(
        line.command,
        Command::ProposeNotes(crate::notes::ProposeNotesArgs {
            topic: crate::notes::Topic::Never,
            answer: None,
            earlier: Vec::new(),
        })
    );
    assert_eq!(
        serde_json::to_string(&line).unwrap(),
        r#"{"id":"c","command":"propose_notes","args":{"topic":"never"}}"#
    );
    for args in [
        // An unknown topic, a missing one, a key the command does not
        // take, a null in place of a key, and missing `args` outright.
        json!({"topic": "budgets"}),
        json!({"answer": "yes"}),
        json!({}),
        json!({"topic": "never", "future": 1}),
        json!({"topic": null}),
        json!({"topic": "never", "answer": null}),
        json!({"topic": "never", "earlier": null}),
        json!({"topic": "never", "earlier": ["kept", null]}),
    ] {
        let line = json!({"id": "c", "command": "propose_notes", "args": args});
        assert!(parse(&line.to_string()).is_err(), "{line}");
    }
    assert!(parse(r#"{"id":"c","command":"propose_notes"}"#).is_err());
}

#[test]
fn offered_file_requires_exactly_its_arguments() {
    for args in [
        json!({"request_id": "r"}),
        json!({"path": "p"}),
        json!({"request_id": "r", "path": "p", "extra": true}),
    ] {
        assert!(
            serde_json::from_value::<CommandLine>(
                json!({"id": "c", "command": "offered_file", "args": args})
            )
            .is_err()
        );
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
    assert!(
        parse(r#"{"id":"c","command":"subscribe","args":{"level":"full","credential":"work","extra":true}}"#)
            .is_err()
    );
    assert!(
        parse(r#"{"id":"c","command":"subscribe","args":{"level":"full","credential":1}}"#)
            .is_err()
    );
    assert!(
        parse(r#"{"id":"c","command":"subscribe","args":{"level":"full","credential":{"label":"work"}}}"#)
            .is_err()
    );
    assert!(
        parse(r#"{"id":"c","command":"subscribe","args":{"level":1,"credential":"work"}}"#)
            .is_err()
    );
}

#[test]
fn subscribe_credential_is_an_optional_label() {
    let line =
        parse(r#"{"id":"c","command":"subscribe","args":{"level":"full","credential":"work"}}"#)
            .unwrap();
    assert_eq!(
        line.command,
        Command::Subscribe(SubscribeArgs {
            level: SubscribeLevel::Full,
            credential: Some("work".into()),
        })
    );
    let line = parse(r#"{"id":"c","command":"subscribe","args":{"level":"summary"}}"#).unwrap();
    assert_eq!(
        line.command,
        Command::Subscribe(SubscribeArgs {
            level: SubscribeLevel::Summary,
            credential: None,
        })
    );
    // An explicit null is never an absent optional key.
    assert!(
        parse(r#"{"id":"c","command":"subscribe","args":{"level":"full","credential":null}}"#)
            .is_err()
    );
}

#[test]
fn history_args_that_do_not_fit_are_invalid_arguments() {
    let line = parse(r#"{"id":"c","command":"history","args":{}}"#).unwrap();
    assert_eq!(
        line.command,
        Command::History(HistoryArgs {
            session_id: None,
            from_seq: None,
            to_seq: None,
        })
    );
    assert!(parse(r#"{"id":"c","command":"history","args":{"from_seq":"0"}}"#).is_err());
    assert!(parse(r#"{"id":"c","command":"history","args":{"from_seq":0,"extra":true}}"#).is_err());
    for session_id in [json!(5), json!(["s"]), json!({})] {
        let line = json!({"id":"c","command":"history","args":{"session_id":session_id}});
        assert!(parse(&line.to_string()).is_err(), "{line}");
    }
    assert!(parse(r#"{"id":"c","command":"history","args":{"to_seq":"5"}}"#).is_err());
    assert!(parse(r#"{"id":"c","command":"history","args":{"to_seq":5,"extra":true}}"#).is_err());
}

#[test]
fn an_optional_arg_set_to_null_is_invalid_arguments() {
    assert!(parse(r#"{"id":"c","command":"history","args":{"session_id":null}}"#).is_err());
    assert!(
        parse(r#"{"id":"c","command":"history","args":{"from_seq":0,"to_seq":null}}"#).is_err()
    );
    assert!(parse(r#"{"id":"c","command":"history","args":{"from_seq":null}}"#).is_err());
    assert!(
        parse(r#"{"id":"c","command":"model","args":{"model":"opus","thinking":null}}"#).is_err()
    );
}

#[test]
fn an_unknown_command_reads_as_unknown() {
    assert_eq!(
        parse(r#"{"id":"c","command":"teleport"}"#).unwrap().command,
        Command::Unknown
    );
}

#[test]
fn an_unknown_name_reads_as_unknown_and_refuses_args() {
    // A bare unknown name reads as `Unknown`; with args the line does not
    // read as a command line at all, and the driver re-reads the name
    // alone to answer `unknown_command`.
    assert_eq!(
        parse(r#"{"id":"c","command":"frob"}"#).unwrap().command,
        Command::Unknown,
        "bare frob"
    );
    assert!(
        parse(r#"{"id":"c","command":"frob","args":{"x":1}}"#).is_err(),
        "frob with args"
    );
    // The built-in `reload` reads as itself beside `Unknown`.
    assert_eq!(
        parse(r#"{"id":"c","command":"reload"}"#).unwrap().command,
        Command::Reload
    );
}

#[test]
fn unknown_is_never_serialised() {
    let line = CommandLine {
        id: CommandId("c".into()),
        command: Command::Unknown,
    };
    assert!(serde_json::to_string(&line).is_err());
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

/// `BUILT_IN_COMMANDS` names the commands in `docs/tui.md`'s "Slash commands" table.
#[test]
fn built_in_commands_match_the_tui_doc_table() {
    let doc = include_str!("../../../docs/tui.md");
    let table = doc.split("### Slash commands").nth(1).unwrap();
    let mut names = Vec::new();
    for line in table.lines().skip_while(|l| !l.starts_with("| `")) {
        if !line.starts_with("| `") {
            break;
        }
        let cell = line.split('|').nth(1).unwrap_or("");
        for part in cell.split(",") {
            let part = part.trim().trim_matches('`');
            // `/handoff [instructions]`, `/?`.
            let name = part
                .trim_start_matches('/')
                .split_whitespace()
                .next()
                .unwrap_or("");
            if !name.is_empty() {
                names.push(name.to_owned());
            }
        }
    }
    let listed: Vec<&str> = BUILT_IN_COMMANDS.to_vec();
    for name in &names {
        assert!(
            listed.contains(&name.as_str()),
            "`{name}` from docs/tui.md not in BUILT_IN_COMMANDS"
        );
    }
    for name in listed {
        assert!(
            names.iter().any(|n| n == name),
            "`{name}` in BUILT_IN_COMMANDS not in docs/tui.md"
        );
    }
}

/// `/init` is a first-party prompt template, not a built-in command.
#[test]
fn init_is_not_a_built_in_command() {
    assert!(BUILT_IN_COMMANDS.contains(&"reload"));
    assert!(!BUILT_IN_COMMANDS.contains(&"init"));
}

#[test]
fn decisions_read_in_order_as_an_offers_answer() {
    let line =
        parse(r#"{"id":"c","command":"reply","args":{"request_id":"r_1","decisions":["approve","skip","never"]}}"#)
            .unwrap();
    let Command::Reply(reply) = line.command else {
        panic!("not a reply");
    };
    assert_eq!(reply.request_id, RequestId("r_1".into()));
    assert_eq!(
        reply.answer,
        ReplyAnswer::Decisions {
            decisions: vec![
                OfferDecision::Approve,
                OfferDecision::Skip,
                OfferDecision::Never
            ],
        }
    );
    let empty = parse(r#"{"id":"c","command":"reply","args":{"request_id":"r_1","decisions":[]}}"#)
        .unwrap();
    let Command::Reply(reply) = empty.command else {
        panic!("not a reply");
    };
    assert_eq!(reply.answer, ReplyAnswer::Decisions { decisions: vec![] });
}

#[test]
fn decisions_outside_the_closed_set_or_beside_another_answer_are_refused() {
    assert!(
        parse(r#"{"id":"c","command":"reply","args":{"request_id":"r","decisions":["maybe"]}}"#)
            .is_err()
    );
    assert!(
        parse(r#"{"id":"c","command":"reply","args":{"request_id":"r","decisions":["approve"],"decision":"allow"}}"#)
            .is_err()
    );
}

#[test]
fn a_rewind_with_an_unknown_key_is_refused() {
    assert!(parse(r#"{"id":"c","command":"rewind","args":{"adopt":["j"]}}"#).is_err());
}

/// `import` takes an all-optional `source` naming the harness: a missing
/// or empty `args` reads as both sources, through the missing-or-empty
/// retry in `CommandLine::deserialize`.
#[test]
fn message_parent_round_trip_missing_key_parses_and_none_omits_the_key() {
    let with_parent = parse(
        r#"{"id":"c","command":"message","args":{"from_session_id":"s","parent_session_id":"s_0123456789abcdef","text":"t"}}"#,
    )
    .unwrap();
    assert_eq!(
        with_parent.command,
        Command::Message(Message {
            from_session_id: SessionId("s".into()),
            parent_session_id: Some(SessionId("s_0123456789abcdef".into())),
            text: "t".into(),
        })
    );
    assert_eq!(
        serde_json::to_string(&with_parent).unwrap(),
        r#"{"id":"c","command":"message","args":{"from_session_id":"s","parent_session_id":"s_0123456789abcdef","text":"t"}}"#
    );
    let without_parent =
        parse(r#"{"id":"c","command":"message","args":{"from_session_id":"s","text":"t"}}"#)
            .unwrap();
    assert_eq!(
        without_parent.command,
        Command::Message(Message {
            from_session_id: SessionId("s".into()),
            parent_session_id: None,
            text: "t".into(),
        })
    );
    assert_eq!(
        serde_json::to_string(&without_parent).unwrap(),
        r#"{"id":"c","command":"message","args":{"from_session_id":"s","text":"t"}}"#
    );
    assert!(
        parse(
            r#"{"id":"c","command":"message","args":{"from_session_id":"s","parent_session_id":null,"text":"t"}}"#
        )
        .is_err()
    );
}

#[test]
fn import_source_reads_missing_and_empty_as_both() {
    for line in [
        r#"{"id":"c","command":"import"}"#.to_owned(),
        r#"{"id":"c","command":"import","args":{}}"#.to_owned(),
    ] {
        assert_eq!(
            parse(&line)
                .unwrap_or_else(|e| panic!("{line}: {e}"))
                .command,
            Command::Import(ImportArgs { source: None }),
            "{line}"
        );
    }
    for source in ["claude-code", "codex"] {
        let line = format!(r#"{{"id":"c","command":"import","args":{{"source":"{source}"}}}}"#);
        assert_eq!(
            parse(&line)
                .unwrap_or_else(|e| panic!("{line}: {e}"))
                .command,
            Command::Import(ImportArgs {
                source: Some(source.into())
            }),
            "{line}"
        );
    }
    // An absent source serialises as empty `args`, which reads back as
    // absent: the round trip holds in both directions.
    let line = CommandLine {
        id: CommandId("c".into()),
        command: Command::Import(ImportArgs { source: None }),
    };
    assert_eq!(
        parse(&serde_json::to_string(&line).unwrap())
            .unwrap()
            .command,
        Command::Import(ImportArgs { source: None })
    );
}

#[test]
fn import_refuses_anything_but_its_two_source_names() {
    for args in [
        json!({"source": 1}),
        json!({"source": null}),
        json!({"source": ["codex"]}),
        json!({"source": "codex", "future": 1}),
        json!({"future": 1}),
        json!({"source": "claude-code", "source2": "codex"}),
    ] {
        let line = json!({"id": "c", "command": "import", "args": args});
        assert!(parse(&line.to_string()).is_err(), "{line}");
    }
}
