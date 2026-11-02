//! The person's messages that still bind survive a handoff in the agent's
//! context (`docs/handoff.md`, "What the model sees after a handoff"), through
//! the built `fiber`: a session on a fake vendor server, reviewed by a scripted
//! model that keeps two reply/message pairs and reloads a skill.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;

use fakes::ProviderServer;
use serde_json::{Value, json};
use support::{Run, Setup, function_call, run_to_exit, text_reply, write_json};

/// `fiber <args>` run from the workspace to its exit.
fn fiber(setup: &Setup, args: &[&str]) -> Run {
    let mut command = setup.fiber(args);
    command.current_dir(setup.workspace());
    let output = run_to_exit(setup.deadline, "fiber", command);
    let stdout = String::from_utf8(output.stdout).unwrap();
    Run {
        code: output.status.code(),
        lines: stdout
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect(),
        stderr: String::from_utf8(output.stderr).unwrap(),
    }
}

#[test]
fn a_handoff_carries_two_reply_pairs_a_loaded_skill_and_labelled_new_input() {
    let setup = Setup::new();
    let skill_path = setup.workspace().join(".agents/skills/review/SKILL.md");
    fs::create_dir_all(skill_path.parent().unwrap()).unwrap();
    fs::write(
        &skill_path,
        "---\nname: review\ndescription: Review this work.\n---\nUse the current review skill instructions.\n",
    )
    .unwrap();
    let server = ProviderServer::start([
        text_reply("Which color should I use?"),
        text_reply("Which number should I use?"),
        stream_call("load_review", "skill", &json!({"name": "review"})),
        text_reply("The skill is loaded."),
        stream_call(
            "handoff_now",
            "handoff",
            &json!({"note": "The handoff note."}),
        ),
        text_reply("The work continues."),
    ])
    .unwrap();
    setup.provider(&server);
    write_json(
        &setup.workspace().join("r.json"),
        &json!({"steps": [{"text": "2, 3"}]}),
    );
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "reviewer": {"model": "scripted/r.json"}}),
    );

    let first = fiber(&setup, &["ask", "Which color?"]);
    assert_eq!(first.code, Some(0), "{}", first.stderr);
    let id = first.session_id().to_owned();
    let mut lines = first.lines;

    let second = fiber(&setup, &["ask", "--resume", &id, "Blue."]);
    assert_eq!(second.code, Some(0), "{}", second.stderr);
    lines.extend(second.lines);
    let third = fiber(&setup, &["ask", "--resume", &id, "Seven."]);
    assert_eq!(third.code, Some(0), "{}", third.stderr);
    lines.extend(third.lines);
    let fourth = fiber(&setup, &["ask", "--resume", &id, "Please hand off now."]);
    assert_eq!(fourth.code, Some(0), "{}", fourth.stderr);
    lines.extend(fourth.lines);

    // Confirm the reviewer, skill tool and handoff all ran before inspecting
    // the request bytes, so a broken setup cannot masquerade as the regression.
    let color_seq = turn_started_seq(&lines, "Which color?");
    let blue_seq = turn_started_seq(&lines, "Blue.");
    let seven_seq = turn_started_seq(&lines, "Seven.");
    let kept = lines
        .iter()
        .find(|line| line["kind"] == "reviewer_kept")
        .expect("the reviewer selected messages");
    assert_eq!(
        kept["payload"]["kept"],
        json!([
            {"seq": blue_seq, "item": 0},
            {"seq": seven_seq, "item": 0}
        ]),
        "the selection keeps the two answers, not the questions"
    );
    assert_ne!(color_seq, blue_seq);
    let loaded = lines
        .iter()
        .find(|line| {
            line["kind"] == "tool_call_completed"
                && line["payload"]["control"]["skill"]["name"] == "review"
        })
        .expect("the real skill tool loaded review");
    assert_eq!(loaded["payload"]["status"], "completed");
    let resent = lines
        .iter()
        .find(|line| line["kind"] == "skills_resent")
        .expect("the loaded skill was resent after handoff");
    assert_eq!(resent["payload"]["skills"].as_array().unwrap().len(), 1);
    let completed = lines
        .iter()
        .find(|line| line["kind"] == "handoff_completed")
        .expect("the handoff completed");
    assert_eq!(completed["payload"]["outcome"], "completed");

    // The built OpenAI Responses serializer writes the four earlier turns,
    // the note request, then the first request after handoff.
    let requests = server.requests();
    assert_eq!(requests.len(), 6);
    let before = requests
        .iter()
        .take(5)
        .map(|request| user_messages(&request.body).join(""))
        .collect::<String>();
    assert!(before.contains("Which color?"));
    assert!(before.contains("Blue."));
    assert!(before.contains("Seven."));
    assert!(before.contains("Please hand off now."));
    assert!(
        !before.contains("The person's earlier message:")
            && !before.contains("The person's new message:")
            && !before.contains("Skills reloaded from the previous context:"),
        "handoff-only framing must not leak into ordinary requests"
    );
    let skill_result: Value = serde_json::from_slice(&requests[3].body).unwrap();
    assert!(
        skill_result["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| {
                item["type"] == "function_call_output"
                    && item["output"].as_str().is_some_and(|text| {
                        text.contains("Use the current review skill instructions.")
                    })
            }),
        "the ordinary skill result is present before handoff"
    );

    let messages = user_messages(&requests[5].body);
    let earlier: Vec<&String> = messages
        .iter()
        .filter(|text| text.contains("The person's earlier message:"))
        .collect();
    assert_eq!(earlier.len(), 2, "each kept answer has its own block");
    let pair_one =
        "The person's earlier message:\nThe agent: Which color should I use?\nThe person: Blue.";
    let pair_two =
        "The person's earlier message:\nThe agent: Which number should I use?\nThe person: Seven.";
    assert_eq!(earlier[0].as_str(), pair_one);
    assert_eq!(earlier[1].as_str(), pair_two);
    let current = "The person's new message:\nPlease hand off now.";
    let note = "The handoff note.";
    let skill = &resent["payload"]["skills"][0];
    let skill_block = format!(
        "Skills reloaded from the previous context:\n{} — {}\n\n{}",
        skill["name"].as_str().unwrap(),
        skill["path"].as_str().unwrap(),
        skill["content"].as_str().unwrap(),
    );
    assert_order(
        &messages,
        &[pair_one, pair_two, current, note, &skill_block],
        "docs/handoff.md, What the model sees after a handoff, items 4–8",
    );
    let flattened = messages.join("");
    for expected in [pair_one, pair_two, current, note, skill_block.as_str()] {
        assert!(
            flattened.contains(expected),
            "flattened request lost {expected:?}"
        );
    }
}

/// A completed Responses function call to a built-in tool.
fn stream_call(call_id: &str, name: &str, arguments: &Value) -> fakes::Response {
    support::stream(&[function_call(call_id, name, arguments)])
}

/// The `seq` of the turn whose input contains `text`.
fn turn_started_seq(lines: &[Value], text: &str) -> u64 {
    lines
        .iter()
        .find(|line| {
            line["kind"] == "turn_started"
                && line["payload"]["input"].as_array().is_some_and(|items| {
                    items.iter().any(|item| {
                        item["content"]
                            .as_array()
                            .is_some_and(|parts| parts.iter().any(|part| part["text"] == text))
                    })
                })
        })
        .and_then(|line| line["seq"].as_u64())
        .expect("turn_started with the requested text and seq")
}

/// User messages decoded from one captured OpenAI Responses request.
fn user_messages(body: &[u8]) -> Vec<String> {
    let request: Value = serde_json::from_slice(body).unwrap();
    request["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["role"] == "user")
        .map(|item| {
            if let Some(text) = item["content"].as_str() {
                text.to_owned()
            } else {
                item["content"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|part| part["text"].as_str())
                    .collect::<String>()
            }
        })
        .collect()
}

/// Asserts the order the handoff contract promises, with the promise beside
/// the assertion (`docs/testing.md`, "Event streams").
fn assert_order(messages: &[String], expected: &[&str], promise: &str) {
    let positions: Vec<usize> = expected
        .iter()
        .map(|text| {
            messages
                .iter()
                .position(|message| message == text)
                .unwrap_or_else(|| panic!("{promise}: missing block {text:?}"))
        })
        .collect();
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "{promise}: block positions were {positions:?}"
    );
}
