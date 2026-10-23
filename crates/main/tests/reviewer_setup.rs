//! Binary-level test of `/reviewer-setup` (`docs/permissions.md`, "Setting
//! up the notes"; #2481): a person walks the five topics on a pty with the
//! `fiber` binary, flips a layer with Tab, approves the diff and finds the
//! reviewer notes saved byte for byte, prose and untouched bullets
//! included. A second run that cancels on the diff leaves the files as they
//! were. The fake provider serves the first turn and one scripted reply per
//! topic.
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
use support::{Setup, git, hello, stream};

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
        grid.contents.contains("Topic 1 of 5 · Source control")
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

    // Run one: topics 1 to 5.
    open_setup(&mut run);
    keep_earlier(&mut run, "Topic 1 of 5 · Source control");
    pass_empty_topic(&mut run, "Topic 2 of 5 · Internal hosts");
    pass_empty_topic(&mut run, "Topic 3 of 5 · Internal tools");
    run.wait_screen("topic 4's question", |grid| {
        grid.contents.contains("Topic 4 of 5 · Routine actions")
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
        grid.contents.contains("Topic 5 of 5")
    });
    run.write(b"\r");
    run.wait_screen("a topic with no lines", |grid| {
        grid.contents.contains("No lines for this topic")
    });
    run.write(b"\r");
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
    pass_earlier_or_empty(&mut run, "Topic 2 of 5");
    pass_earlier_or_empty(&mut run, "Topic 3 of 5");
    pass_earlier_or_empty(&mut run, "Topic 4 of 5");
    pass_earlier_or_empty(&mut run, "Topic 5 of 5");
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
