//! Binary-level test of `/reviewer-setup` (`docs/permissions.md`, "Setting
//! up the notes"; #2481): a person walks the six topics on a pty with the
//! `fiber` binary, flips a layer with Tab, approves the diff and finds the
//! reviewer notes saved byte for byte, prose and untouched bullets
//! included. A second run that cancels on the diff leaves the files as they
//! were. The fake provider serves the first turn and one scripted reply per
//! topic but the last, which asks no model.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::path::Path;

use fakes::ProviderServer;
use serde_json::{Value, json};
use support::pty::{Grid, Run};
use support::{Setup, function_call, git, hello, stream};

/// Esc as the kitty keyboard protocol reports it: a lone `ESC` byte waits
/// for the rest of a sequence.
const ESC: &[u8] = b"\x1b[27u";

/// The global notes the run starts from: prose above, between and below two
/// earlier blocks, one bullet ending `\r\n` and one written `-  x`.
const START: &str = "Notes for the reviewer.\n\n### Source control\r\n- github.com/acme/* are ours\r\n\nProse between the blocks.\n\n### Routine actions\n-  x\n- merge my own PRs after CI\n\nProse below.\n";

/// What the run saves: the confirmed line added after the last line of the
/// `### Routine actions` block, and every other byte as it was.
const SAVED: &str = "Notes for the reviewer.\n\n### Source control\r\n- github.com/acme/* are ours\r\n\nProse between the blocks.\n\n### Routine actions\n-  x\n- merge my own PRs after CI\n- squash-merging my own pull requests once CI passes\n\nProse below.\n";

/// An `openai-responses` stream answering `text`.
fn text_reply(text: &str) -> fakes::Response {
    stream(&[json!({"type": "response.output_item.done", "item": {
        "type": "message", "content": [{"type": "output_text", "text": text}]
    }})])
}

/// A reply that proposes nothing.
fn nothing() -> fakes::Response {
    text_reply("nothing to add")
}

/// The reply that cites the one evidence item, the session counts.
fn lines(lines: &[&str]) -> fakes::Response {
    let text: Vec<String> = lines.iter().map(|line| format!("1 | {line}")).collect();
    text_reply(&text.join("\n"))
}

/// Another project's session holding two escalations answered allow: the
/// one evidence item every topic gathers, since the workspace is a
/// repository with no remote and no CI files.
fn seed_other_project(setup: &Setup) {
    let other = setup.root.path().join("other");
    fs::create_dir_all(&other).unwrap();
    git(setup.deadline, &other, &["init", "--quiet"]);
    let seed = setup
        .home()
        .join("projects")
        .join("seed")
        .join("sessions")
        .join("s_seed00000001");
    fs::create_dir_all(&seed).unwrap();
    let mut log = json!({"kind": "session_started",
        "payload": {"workspace": other.to_str().unwrap()}})
    .to_string();
    log.push('\n');
    let mut seq = 0u64;
    for number in ["r1", "r2"] {
        for (kind, payload) in [
            (
                "permission_requested",
                json!({"request_id": number, "step": "review",
                    "escalation": {"cause": "consecutive_blocks", "reason": "blocked"},
                    "rule": {"subject": "gh pr merge", "prefix": "gh pr merge"}}),
            ),
            (
                "permission_resolved",
                json!({"request_id": number, "decision": "allow", "decided_by": "person"}),
            ),
        ] {
            seq += 1;
            log.push_str(
                &json!({"kind": kind, "session_id": "s_seed00000001",
                    "ts": seq, "schema_version": 1, "seq": seq, "payload": payload})
                .to_string(),
            );
            log.push('\n');
        }
    }
    fs::write(seed.join("events.jsonl"), log).unwrap();
}

/// The text of `reviewer.context` in the global `config.json`.
fn context(setup: &Setup) -> String {
    let file = fs::read_to_string(setup.home().join("config.json")).unwrap();
    let config: Value = serde_json::from_str(&file).unwrap();
    config["reviewer"]["context"].as_str().unwrap().to_owned()
}

/// Whether any project under fiber home has a `config.json` of its own.
fn project_config_exists(setup: &Setup) -> bool {
    let projects = setup.home().join("projects");
    fs::read_dir(projects)
        .unwrap()
        .filter_map(Result::ok)
        .any(|entry| Path::new(&entry.path()).join("config.json").exists())
}

/// Types `/reviewer-setup` and waits for topic 1's question.
fn open_setup(run: &mut Run) {
    run.write(b"/reviewer-setup\r");
    run.wait_screen("topic 1's question", |grid| {
        grid.contents.contains("Topic 1 of 6 · Source control")
    });
}

/// Enter on a question, then Enter on a lines screen that has nothing to
/// decide: `topic`'s question is on screen.
fn pass_empty_topic(run: &mut Run, topic: &str) {
    run.wait_screen(topic, |grid| grid.contents.contains(topic));
    run.write(b"\r");
    run.wait_screen("a topic with no lines", |grid| {
        grid.contents.contains("No lines for this topic")
    });
    run.write(b"\r");
}

/// Enter on a question whose topic has earlier lines only; Enter keeps the
/// focused earlier line.
fn keep_earlier(run: &mut Run, topic: &str) {
    run.wait_screen(topic, |grid| grid.contents.contains(topic));
    run.write(b"\r");
    run.wait_screen("the earlier lines", |grid| {
        grid.contents.contains("Earlier lines")
    });
    run.write(b"\r");
}

#[test]
fn the_setup_saves_the_notes_byte_for_byte_and_a_cancel_leaves_them() {
    let setup = Setup::new();
    let server = ProviderServer::start([
        hello(),
        // Run one: one reply for each topic.
        nothing(),
        nothing(),
        nothing(),
        lines(&[
            "ignored by a strike, flipped first",
            "squash-merging my own pull requests once CI passes",
        ]),
        nothing(),
        // Run two, which cancels.
        lines(&["an extra line the cancel must not keep"]),
        nothing(),
        nothing(),
        nothing(),
        nothing(),
    ])
    .unwrap();
    setup.provider(&server);
    support::write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "hub": {"idle_exit_ms": 1000},
            "reviewer": {"context": START}}),
    );
    // The workspace is a repository with no remote.
    git(setup.deadline, &setup.workspace(), &["init", "--quiet"]);
    seed_other_project(&setup);

    let mut run = Run::spawn(
        &setup,
        120,
        40,
        &[],
        &[
            ("FIBER_TEST_FAKE_KEY", "sk-test"),
            ("TERM", "xterm-256color"),
        ],
    );
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    let from = run.ready();
    run.write(b"hello\r");
    run.turn_finished(from);

    // Run one: topics 1 to 6.
    open_setup(&mut run);
    keep_earlier(&mut run, "Topic 1 of 6 · Source control");
    pass_empty_topic(&mut run, "Topic 2 of 6 · Internal hosts");
    pass_empty_topic(&mut run, "Topic 3 of 6 · Internal tools");
    run.wait_screen("topic 4's question", |grid| {
        grid.contents.contains("Topic 4 of 6 · Routine actions")
    });
    run.write(b"\r");
    run.wait_screen("topic 4's proposals", |grid| {
        grid.contents.contains("New proposals")
            && grid
                .contents
                .contains("squash-merging my own pull requests")
    });
    // Tab reaches the setup through the real key path: the session count
    // goes to the global notes, so the flip moves the first line to the
    // project's, which the next stroke then strikes.
    run.write(b"\t");
    run.wait_screen("the flipped layer", |grid| {
        grid.contents.contains("was global · Tab flipped it")
    });
    run.write(b"x");
    run.write(b"\r");
    run.wait_screen("topic 5's question", |grid| {
        grid.contents.contains("Topic 5 of 6")
    });
    run.write(b"\r");
    run.wait_screen("a topic with no lines", |grid| {
        grid.contents.contains("No lines for this topic")
    });
    run.write(b"\r");
    // Topic 6 asks no question: the seeded sessions hold no reviewer
    // allows, so its lines are empty and one Enter reaches the diff.
    pass_read_only_topic(&mut run);
    let diff: Grid = run.wait_screen("the diff", |grid| {
        grid.contents.contains("Approving then offers")
    });
    assert!(
        diff.contents
            .contains("squash-merging my own pull requests"),
        "{}",
        diff.contents
    );
    assert!(
        !diff.contents.contains("ignored by a strike"),
        "{}",
        diff.contents
    );
    run.write(b"\r");
    run.wait_screen("the saved screen", |grid| {
        grid.contents.contains("reload now")
    });
    run.write(b"n");
    run.wait_screen("the setup closed", |grid| {
        !grid.contents.contains("Reviewer setup")
    });
    assert_eq!(context(&setup), SAVED);
    assert!(
        !project_config_exists(&setup),
        "the project file stays absent"
    );
    let saved = fs::read(setup.home().join("config.json")).unwrap();

    // Run two: the same terminal goes to the diff and cancels.
    open_setup(&mut run);
    run.write(b"\r");
    run.wait_screen("topic 1's proposal", |grid| {
        grid.contents
            .contains("an extra line the cancel must not keep")
    });
    run.write(b"\r");
    pass_earlier_or_empty(&mut run, "Topic 2 of 6");
    pass_earlier_or_empty(&mut run, "Topic 3 of 6");
    pass_earlier_or_empty(&mut run, "Topic 4 of 6");
    pass_earlier_or_empty(&mut run, "Topic 5 of 6");
    pass_read_only_topic(&mut run);
    let diff: Grid = run.wait_screen("the second diff", |grid| {
        grid.contents.contains("Approving then offers")
    });
    assert!(
        diff.contents
            .contains("an extra line the cancel must not keep"),
        "{}",
        diff.contents
    );
    run.write(ESC);
    run.wait_screen("the setup closed", |grid| {
        !grid.contents.contains("Reviewer setup")
    });
    assert_eq!(fs::read(setup.home().join("config.json")).unwrap(), saved);
    assert!(!project_config_exists(&setup));
    run.write(b"/quit\r");
    run.wait();
}

/// The read-only topic's lines screen, which asks no question: Enter on
/// its empty lines leaves for the diff. The topic proposes at once, so
/// the wait names the lines screen itself: the topic number alone also
/// matches the wait before it, and the second Enter then lands on the
/// diff and saves what the cancel must not save.
fn pass_read_only_topic(run: &mut Run) {
    run.wait_screen("topic 6's lines", |grid| {
        grid.contents.contains("Topic 6 of 6") && grid.contents.contains("No lines for this topic")
    });
    run.write(b"\r");
}

/// A topic whose lines screen shows earlier lines or none: Enter proposes,
/// Enter leaves.
fn pass_earlier_or_empty(run: &mut Run, topic: &str) {
    run.wait_screen(topic, |grid| grid.contents.contains(topic));
    run.write(b"\r");
    run.wait_screen("the topic's lines", |grid| {
        grid.contents.contains("No lines for this topic") || grid.contents.contains("Earlier lines")
    });
    run.write(b"\r");
}

/// The global layer after the run: the notes byte for byte, with the one
/// confirmed entry merged beside them.
const SAVED_WITH_ENTRY: &str = "{\n  \"hub\": {\n    \"idle_exit_ms\": 1000\n  },\n  \"model\": \"fake/m\",\n  \"reviewer\": {\n    \"context\": \"Notes.\"\n  },\n  \"shell\": {\n    \"read_only\": {\n      \"acme-cli query\": {\n        \"flags\": [\n          \"--json\"\n        ]\n      }\n    }\n  }\n}\n";

/// One log line with its action: the join key for one shell call.
fn seed_line(kind: &str, action: &str, seq: u64, payload: &Value) -> String {
    json!({
        "kind": kind,
        "session_id": "s_seed00000002",
        "action_id": action,
        "ts": seq,
        "schema_version": 1,
        "seq": seq,
        "payload": payload,
    })
    .to_string()
        + "\n"
}

/// Allowed shell calls: the reviewer allows each action.
fn seed_allows(actions: &[&str], command: &str, first_seq: u64) -> String {
    let mut body = String::new();
    for (at, action) in actions.iter().enumerate() {
        let seq = first_seq + u64::try_from(at).unwrap() * 2;
        body.push_str(&seed_line(
            "tool_call_requested",
            action,
            seq,
            &json!({"name": "shell", "arguments": {"command": command}}),
        ));
        body.push_str(&seed_line(
            "permission_resolved",
            action,
            seq + 1,
            &json!({"decision": "allow", "decided_by": "reviewer"}),
        ));
    }
    body
}

/// One denied shell call: requested, then the reviewer's denial.
fn seed_deny(action: &str, seq: u64, command: &str) -> String {
    seed_line(
        "tool_call_requested",
        action,
        seq,
        &json!({"name": "shell", "arguments": {"command": command}}),
    ) + &seed_line(
        "permission_resolved",
        action,
        seq + 1,
        &json!({"decision": "deny", "decided_by": "reviewer"}),
    )
}

/// One blocked shell call: the request escalated, a person allowed it.
fn seed_block(action: &str, seq: u64, command: &str) -> String {
    seed_line(
        "tool_call_requested",
        action,
        seq,
        &json!({"name": "shell", "arguments": {"command": command}}),
    ) + &seed_line(
        "permission_requested",
        action,
        seq + 1,
        &json!({"request_id": "r_1", "step": "review",
            "escalation": {"cause": "consecutive_blocks", "reason": "blocked"}}),
    ) + &seed_line(
        "permission_resolved",
        action,
        seq + 2,
        &json!({"request_id": "r_1", "decision": "allow", "decided_by": "person"}),
    )
}

/// Sessions holding the ticket's read-only spread: five reviewer allows of
/// one shell subject, one of another, a deny case, a `--output` case, a
/// one-word case, a cross-subject block, and five allows each of two
/// built-in-classified commands. Every session routes away from the
/// workspace, so a confirmed entry lands in the global layer.
fn seed_read_only_sessions(setup: &Setup, other: &Path) {
    let sessions = [
        (
            "s_query",
            seed_allows(&["q1", "q2", "q3", "q4", "q5"], "acme-cli query --json", 1),
        ),
        ("s_push", seed_allows(&["p1"], "acme-cli push --json", 1)),
        (
            "s_deny",
            seed_allows(&["d1", "d2", "d3", "d4", "d5"], "deploy-cli run --json", 1)
                + &seed_deny("d6", 11, "deploy-cli run --json"),
        ),
        (
            "s_output",
            seed_allows(
                &["o1", "o2", "o3", "o4", "o5"],
                "acme-cli query --output out.txt",
                1,
            ),
        ),
        (
            "s_oneword",
            seed_allows(&["w1", "w2", "w3", "w4", "w5"], "acme-cli --json", 1),
        ),
        (
            "s_block",
            seed_allows(&["f1", "f2", "f3", "f4", "f5"], "acme-cli fetch --json", 1)
                + &seed_block("f6", 11, "acme-cli fetch --json"),
        ),
        (
            "s_ls",
            seed_allows(&["l1", "l2", "l3", "l4", "l5"], "ls --json", 1),
        ),
        (
            "s_git",
            seed_allows(&["g1", "g2", "g3", "g4", "g5"], "git push", 1),
        ),
    ];
    for (id, body) in sessions {
        let dir = setup
            .home()
            .join("projects")
            .join("seed")
            .join("sessions")
            .join(id);
        fs::create_dir_all(&dir).unwrap();
        let log = json!({"kind": "session_started",
            "payload": {"workspace": other.to_str().unwrap()}})
        .to_string()
            + "\n"
            + &body;
        fs::write(dir.join("events.jsonl"), log).unwrap();
    }
}

#[test]
fn the_setup_proposes_and_saves_a_confirmed_entry() {
    let setup = Setup::new();
    let server = ProviderServer::start([
        hello(),
        nothing(),
        nothing(),
        nothing(),
        nothing(),
        nothing(),
    ])
    .unwrap();
    setup.provider(&server);
    support::write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "hub": {"idle_exit_ms": 1000},
            "reviewer": {"context": "Notes."}}),
    );
    // The workspace is a repository with no remote.
    git(setup.deadline, &setup.workspace(), &["init", "--quiet"]);
    let other = setup.root.path().join("other");
    fs::create_dir_all(&other).unwrap();
    git(setup.deadline, &other, &["init", "--quiet"]);
    seed_read_only_sessions(&setup, &other);

    let mut run = Run::spawn(
        &setup,
        120,
        40,
        &[],
        &[
            ("FIBER_TEST_FAKE_KEY", "sk-test"),
            ("TERM", "xterm-256color"),
        ],
    );
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    let from = run.ready();
    run.write(b"hello\r");
    run.turn_finished(from);

    // Topics 1 to 5 propose nothing; topic 6 proposes the one entry.
    open_setup(&mut run);
    pass_empty_topic(&mut run, "Topic 1 of 6");
    pass_empty_topic(&mut run, "Topic 2 of 6");
    pass_empty_topic(&mut run, "Topic 3 of 6");
    pass_empty_topic(&mut run, "Topic 4 of 6");
    pass_empty_topic(&mut run, "Topic 5 of 6");
    let topic6: Grid = run.wait_screen("topic 6's lines", |grid| {
        grid.contents.contains("Topic 6 of 6") && grid.contents.contains("acme-cli query")
    });
    // The built-in-owned subjects stay silent: listed nowhere, proposed
    // nowhere. The unknown `acme-cli query` is the one proposal.
    assert!(
        topic6.contents.contains("acme-cli query"),
        "{}",
        topic6.contents
    );
    assert!(
        !topic6.contents.contains("ls --json"),
        "{}",
        topic6.contents
    );
    assert!(!topic6.contents.contains("git push"), "{}", topic6.contents);
    run.write(b"\r");
    let diff: Grid = run.wait_screen("the diff", |grid| {
        grid.contents.contains("Approving then offers")
    });
    assert!(
        diff.contents.contains("acme-cli query"),
        "{}",
        diff.contents
    );
    assert!(!diff.contents.contains("ls --json"), "{}", diff.contents);
    assert!(!diff.contents.contains("git push"), "{}", diff.contents);
    run.write(b"\r");
    run.wait_screen("the saved screen", |grid| {
        grid.contents.contains("reload now")
    });
    run.write(b"n");
    run.wait_screen("the setup closed", |grid| {
        !grid.contents.contains("Reviewer setup")
    });
    let file = fs::read_to_string(setup.home().join("config.json")).unwrap();
    assert_eq!(file, SAVED_WITH_ENTRY);

    run.write(b"/quit\r");
    run.wait();
}

/// One finished headless `ask` turn: stdout's JSON lines parsed, sharing
/// the support run shape.
fn headless_ask(setup: &Setup, message: &str) -> support::Run {
    let mut command = setup.fiber(&["ask", message]);
    // The call runs in the workspace: anywhere else trips the credential
    // guard, which denies before the reviewer ever judges.
    command.current_dir(setup.workspace());
    let (child, watchdog) = support::spawn_watched(&mut command);
    let group = child.id();
    let guard = support::KillGroup(group);
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || done.send(child.wait_with_output()).unwrap());
    let output = match setup.deadline.recv(&finished) {
        Ok(output) => output.unwrap(),
        Err(_) => support::expired(setup.deadline, group, &finished, "`fiber ask` to exit"),
    };
    assert!(
        !fakes::group_alive(group),
        "`fiber` left a process in its group behind"
    );
    std::mem::forget(guard);
    watchdog.stand_down(setup.deadline.cleanup_phase());
    let lines = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or(Value::Null))
        .collect();
    support::Run {
        code: output.status.code(),
        lines,
        stderr: String::from_utf8(output.stderr).unwrap(),
    }
}

#[test]
fn a_saved_entry_covers_later_calls_without_asking() {
    let setup = Setup::new();
    let server = ProviderServer::start([
        stream(&[function_call(
            "call_query",
            "shell",
            &json!({"command": "acme-cli query --json"}),
        )]),
        text_reply("done"),
    ])
    .unwrap();
    setup.provider(&server);
    // The exact bytes the setup saves: the entry beside untouched notes.
    fs::write(setup.home().join("config.json"), SAVED_WITH_ENTRY).unwrap();
    git(setup.deadline, &setup.workspace(), &["init", "--quiet"]);

    // A fresh session runs the covered call: it declares reads, and the
    // reviewer allows it with no permission request.
    let run = headless_ask(&setup, "run acme-cli query --json");
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let requested: Vec<&Value> = run
        .lines
        .iter()
        .filter(|line| {
            line["kind"] == "tool_call_requested"
                && line["payload"]["name"] == "shell"
                && line["payload"]["arguments"]["command"] == "acme-cli query --json"
        })
        .collect();
    assert_eq!(requested.len(), 1, "{:?}", run.lines);
    let action = &requested[0]["action_id"];
    let reads: Vec<&Value> = run
        .lines
        .iter()
        .filter(|line| {
            line["kind"] == "tool_call_started"
                && line["action_id"] == *action
                && line["payload"]["effects"] == json!(["reads"])
        })
        .collect();
    assert_eq!(reads.len(), 1, "{:?}", run.lines);
    // No permission request goes out and no resolution is recorded: the
    // read-only fast path allows the call outright, skipping review.
    assert!(
        !run.lines
            .iter()
            .any(|line| (line["kind"] == "permission_requested"
                || line["kind"] == "permission_resolved")
                && line["action_id"] == *action),
        "{:?}",
        run.lines
    );
}
