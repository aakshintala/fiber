//! Tests for one session's pass: labels, decoding, the torn tail, artifacts
//! and their claiming, and the session's name.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use contract::clock::Wake;
use contract::events::Event;
use contract::session_search::{Found, Label};
use contract::tool::Cancel;
use contract::{ActionId, Envelope, SessionId};
use fakes::clock::FakeClock;
use fakes::{CancelToken, TempDir};
use serde_json::{Value, json};

use super::*;
use crate::Log;

/// The fake clock's wall time at its origin, in milliseconds.
const ORIGIN_MS: u64 = 1_700_000_000_000;

/// A Fiber session directory written with [`Log`].
struct Fixture {
    _home: TempDir,
    sessions: PathBuf,
    clock: Arc<FakeClock>,
}

impl Fixture {
    fn new() -> Self {
        let home = TempDir::new("log-search-session");
        let sessions = home.path().join("sessions");
        Self {
            _home: home,
            sessions,
            clock: FakeClock::new(),
        }
    }

    /// A new session `id`, its `session_started` written at seq 0.
    fn log(&self, id: &str) -> Log {
        let log = Log::create(&self.sessions, SessionId(id.into()), self.clock.clone()).unwrap();
        let started = json!({
            "workspace": "/w",
            "variables": {"path": "/bin", "names": [], "source": "inherited"},
        });
        log.append(&event("session_started", started), None, None)
            .unwrap();
        log
    }

    /// Writes one event a second after the last, returning its seq.
    fn add(&self, log: &Log, kind: &str, payload: Value) -> u64 {
        self.clock.advance(Duration::from_secs(1));
        let line = log.append(&event(kind, payload), None, None).unwrap();
        line.seq.unwrap().0
    }

    /// Writes one event for the tool call `action`, returning its seq.
    fn add_action(&self, log: &Log, kind: &str, payload: Value, action: &str) -> u64 {
        self.clock.advance(Duration::from_secs(1));
        let line = log
            .append(&event(kind, payload), None, Some(ActionId(action.into())))
            .unwrap();
        line.seq.unwrap().0
    }

    fn dir(&self, id: &str) -> PathBuf {
        self.sessions.join(id)
    }
}

fn event(kind: &str, payload: Value) -> Event {
    let line = Envelope {
        kind: kind.into(),
        session_id: SessionId("x".into()),
        ts: 0,
        schema_version: 1,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().unwrap().clone(),
    };
    Event::from_envelope(&line).unwrap().unwrap()
}

fn text(t: &str) -> Value {
    json!({"type": "text", "text": t})
}

fn prompt(t: &str) -> Value {
    json!({"input": [{"type": "message", "content": [text(t)], "source": "driver"}]})
}

fn requested(arguments: Value) -> Value {
    json!({"name": "shell", "arguments": arguments})
}

fn completed(content: &str, artifact: Option<&str>) -> Value {
    let mut payload = json!({"status": "completed", "content": [text(content)]});
    if let Some(artifact) = artifact {
        payload["artifact"] = json!(artifact);
    }
    payload
}

fn shell(command: &str, output: &str, artifact: &str) -> Value {
    json!({
        "command": command,
        "output": output,
        "artifact": artifact,
        "process": {"exit_code": 0, "timed_out": false},
    })
}

fn named(name: Option<&str>) -> Value {
    json!({"name": name, "by": "person"})
}

/// Appends raw bytes to `dir`'s log.
fn raw(dir: &Path, bytes: &[u8]) {
    let mut log = OpenOptions::new()
        .append(true)
        .open(dir.join("events.jsonl"))
        .unwrap();
    log.write_all(bytes).unwrap();
}

fn artifact(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join("artifacts").join(name);
    fs::write(&path, bytes).unwrap();
    path
}

fn run_with(query: &str, dir: &Path, cancel: &dyn Cancel) -> Found {
    let text = Text::new(query).unwrap();
    let log = File::open(dir.join("events.jsonl")).unwrap();
    let id = SessionId(dir.file_name().unwrap().to_string_lossy().into_owned());
    let mut out = Collect::new(100);
    let session = Session {
        id: &id,
        dir,
        log: &log,
    };
    search(&text, &session, cancel, &mut out);
    out.found()
}

fn run(query: &str, dir: &Path) -> Found {
    run_with(query, dir, &CancelToken::new())
}

/// Each hit as (label, seq, snippet, artifact).
fn hits(found: &Found) -> Vec<(Label, u64, String, Option<PathBuf>)> {
    found
        .hits
        .iter()
        .map(|hit| {
            (
                hit.label,
                hit.seq.0,
                hit.snippet.clone(),
                hit.artifact.clone(),
            )
        })
        .collect()
}

/// A [`Cancel`] that turns true after `after` checks.
struct After {
    checks: AtomicUsize,
    after: usize,
}

impl After {
    fn new(after: usize) -> Self {
        Self {
            checks: AtomicUsize::new(0),
            after,
        }
    }
}

impl Cancel for After {
    fn is_cancelled(&self) -> bool {
        self.checks.fetch_add(1, Ordering::SeqCst) >= self.after
    }

    fn subscribe(&self, _: Weak<dyn Wake>) {}
}

#[test]
fn each_label_gives_a_hit_with_its_line_seq_and_time() {
    let fx = Fixture::new();
    let log = fx.log("s_1");
    let message = fx.add(&log, "turn_started", prompt("keep the Needle here"));
    let input = fx.add(
        &log,
        "tool_call_requested",
        requested(json!({"command": "grep needle"})),
    );
    let output = fx.add(&log, "tool_call_completed", completed("found NEEDLE", None));
    fx.add(&log, "text_completed", json!({"text": "nothing"}));
    let found = run("needle", &fx.dir("s_1"));
    assert_eq!(
        hits(&found),
        [
            (Label::ToolInput, input, "grep needle".into(), None),
            (Label::Message, message, "keep the Needle here".into(), None),
            (Label::ToolOutput, output, "found NEEDLE".into(), None),
        ]
    );
    assert_eq!(found.total, 3);
    assert!(found.problems.is_empty(), "{:?}", found.problems);
    let first = &found.hits[0];
    assert_eq!(first.ts, ORIGIN_MS + 2_000);
    assert_eq!(first.session_id, SessionId("s_1".into()));
    assert_eq!(first.log, fx.dir("s_1").join("events.jsonl"));
    assert_eq!(found.hits[1].ts, ORIGIN_MS + 1_000);
}

#[test]
fn a_session_search_call_gives_no_hit_while_another_tools_does() {
    let fx = Fixture::new();
    let log = fx.log("s_1");
    let dir = fx.dir("s_1");
    // The tool's own call: its arguments, its rewritten arguments, its
    // output and its artifact all hold the query, and none gives a hit.
    artifact(&dir, "self_1.txt", b"full output: NEEDLE exhausted\n");
    fx.add_action(
        &log,
        "tool_call_requested",
        json!({"name": "session_search", "arguments": {"text": "find the needle"}}),
        "a_1",
    );
    fx.add_action(
        &log,
        "tool_call_started",
        json!({"effects": ["reads"], "reversible": true, "arguments": {"text": "needle again"}}),
        "a_1",
    );
    fx.add_action(
        &log,
        "tool_call_completed",
        completed("needle results", Some("artifacts/self_1.txt")),
        "a_1",
    );
    // An earlier call for another query: its arguments do not hold this
    // query, so only the raw pass's tool-name literal selects its request
    // line, yet its output still gives no hit.
    fx.add_action(
        &log,
        "tool_call_requested",
        json!({"name": "session_search", "arguments": {"text": "older search"}}),
        "a_2",
    );
    fx.add_action(
        &log,
        "tool_call_completed",
        completed("a needle from before", None),
        "a_2",
    );
    // Another tool's call with the same text still hits.
    let input = fx.add_action(
        &log,
        "tool_call_requested",
        requested(json!({"command": "grep needle"})),
        "a_9",
    );
    let output = fx.add_action(
        &log,
        "tool_call_completed",
        completed("found NEEDLE", None),
        "a_9",
    );
    let found = run("needle", &dir);
    assert_eq!(
        hits(&found),
        [
            (Label::ToolInput, input, "grep needle".into(), None),
            (Label::ToolOutput, output, "found NEEDLE".into(), None),
        ]
    );
    assert_eq!(found.total, 2);
    assert!(found.problems.is_empty(), "{:?}", found.problems);
}

#[test]
fn a_shell_command_gives_an_input_and_an_output_hit() {
    let fx = Fixture::new();
    let log = fx.log("s_1");
    let seq = fx.add(
        &log,
        "shell_command",
        shell("echo needle", "needle", "artifacts/none"),
    );
    let found = run("needle", &fx.dir("s_1"));
    assert_eq!(
        hits(&found),
        [
            (Label::ToolInput, seq, "echo needle".into(), None),
            (Label::ToolOutput, seq, "needle".into(), None),
        ]
    );
}

#[test]
fn escaped_quotes_and_newlines_match_decoded() {
    let fx = Fixture::new();
    let log = fx.log("s_1");
    let seq = fx.add(&log, "text_completed", json!({"text": "say \"hi\"\nthere"}));
    for query in ["\"HI\"\nthe", "\"hi\"", "\nthere"] {
        let found = run(query, &fx.dir("s_1"));
        assert_eq!(
            hits(&found),
            [(Label::Message, seq, "say \"hi\"\nthere".into(), None)],
            "{query:?}"
        );
    }
}

#[test]
fn a_matching_torn_tail_gives_nothing() {
    let fx = Fixture::new();
    let log = fx.log("s_1");
    fx.add(&log, "text_completed", json!({"text": "plain"}));
    let dir = fx.dir("s_1");
    raw(
        &dir,
        br#"{"kind":"text_completed","session_id":"s_1","ts":1,"schema_version":1,"seq":2,"payload":{"text":"needle"}}"#,
    );
    let found = run("needle", &dir);
    assert_eq!(found, Found::default());
}

#[test]
fn a_matching_line_that_does_not_parse_is_listed_with_its_number() {
    let fx = Fixture::new();
    let log = fx.log("s_1");
    fx.add(&log, "text_completed", json!({"text": "plain"}));
    let dir = fx.dir("s_1");
    raw(&dir, b"{\"kind\":\"text_completed\" needle\n");
    let found = run("needle", &dir);
    assert!(found.hits.is_empty());
    assert_eq!(found.problems.len(), 1, "{:?}", found.problems);
    let want = format!(
        "Could not read: {}, line 3: ",
        dir.join("events.jsonl").display()
    );
    assert!(found.problems[0].starts_with(&want), "{:?}", found.problems);
}

#[test]
fn a_line_without_seq_gives_no_hit() {
    let fx = Fixture::new();
    let log = fx.log("s_1");
    let dir = fx.dir("s_1");
    raw(
        &dir,
        b"{\"kind\":\"text_completed\",\"session_id\":\"s_1\",\"ts\":1,\"schema_version\":1,\"payload\":{\"text\":\"needle\"}}\n",
    );
    let seq = fx.add(&log, "text_completed", json!({"text": "needle"}));
    let found = run("needle", &dir);
    assert_eq!(hits(&found), [(Label::Message, seq, "needle".into(), None)]);
}

#[test]
fn an_artifact_hit_takes_the_naming_line_and_replaces_its_cut_output() {
    let fx = Fixture::new();
    let log = fx.log("s_1");
    let dir = fx.dir("s_1");
    let call = artifact(
        &dir,
        "call_1.txt",
        b"first\nfull output: NEEDLE exhausted\n",
    );
    let sh = artifact(&dir, "sh_1.txt", b"sh needle all\n");
    let completed_seq = fx.add(
        &log,
        "tool_call_completed",
        completed("needle cut", Some("artifacts/call_1.txt")),
    );
    let shell_seq = fx.add(
        &log,
        "shell_command",
        shell("echo needle", "needle out", "artifacts/sh_1.txt"),
    );
    let found = run("needle", &dir);
    assert_eq!(
        hits(&found),
        [
            (Label::ToolInput, shell_seq, "echo needle".into(), None),
            (
                Label::ToolOutput,
                shell_seq,
                "sh needle all".into(),
                Some(sh)
            ),
            (
                Label::ToolOutput,
                completed_seq,
                "full output: NEEDLE exhausted".into(),
                Some(call)
            ),
        ]
    );
    assert_eq!(found.total, 3);
}

#[test]
fn an_artifact_matches_even_when_the_cut_output_does_not() {
    let fx = Fixture::new();
    let log = fx.log("s_1");
    let dir = fx.dir("s_1");
    let call = artifact(&dir, "call_1.txt", b"deep needle\n");
    let seq = fx.add(
        &log,
        "tool_call_completed",
        completed("head only", Some("artifacts/call_1.txt")),
    );
    let found = run("needle", &dir);
    assert_eq!(
        hits(&found),
        [(Label::ToolOutput, seq, "deep needle".into(), Some(call))]
    );
}

#[test]
fn an_artifact_named_by_two_lines_takes_the_lower_seq() {
    let fx = Fixture::new();
    let log = fx.log("s_1");
    let dir = fx.dir("s_1");
    let job = artifact(&dir, "j1.out", b"job needle\n");
    let started = fx.add(
        &log,
        "job_started",
        json!({"job_id": "j1", "description": "d", "output_path": "artifacts/j1.out"}),
    );
    let later = fx.add(
        &log,
        "delegate_finished",
        json!({
            "job_id": "j1", "text": "needle final", "artifact": "artifacts/j1.out",
            "usage": {
                "tokens": {"input": 0, "cache_read": 0, "cache_write": {}, "output": 0},
                "cost": 0.0,
                "subscription_cost": 0.0,
            },
        }),
    );
    let found = run("needle", &dir);
    assert_eq!(
        hits(&found),
        [
            (Label::ToolOutput, later, "needle final".into(), None),
            (Label::ToolOutput, started, "job needle".into(), Some(job)),
        ]
    );
}

#[test]
fn artifacts_that_are_not_named_text_give_no_hit() {
    let fx = Fixture::new();
    let log = fx.log("s_1");
    let dir = fx.dir("s_1");
    let outside = fx.sessions.join("outside.txt");
    fs::write(&outside, "needle outside").unwrap();
    artifact(&dir, "unnamed.txt", b"needle\n");
    artifact(&dir, "shot.PNG", b"needle\n");
    artifact(&dir, "nul.txt", b"needle\n\0\n");
    let mut late = b"needle first\n".to_vec();
    late.extend(std::iter::repeat_n(b'a', 1 << 21));
    late.extend(b"\n\0\n");
    artifact(&dir, "late.txt", &late);
    artifact(&dir, "elsewhere.txt", b"needle\n");
    symlink(&outside, dir.join("artifacts").join("link.txt")).unwrap();
    for name in ["shot.PNG", "nul.txt", "late.txt", "link.txt"] {
        fx.add(
            &log,
            "tool_call_completed",
            completed("cut", Some(&format!("artifacts/{name}"))),
        );
    }
    for value in [
        "artifacts/../artifacts/elsewhere.txt",
        "elsewhere.txt",
        "./artifacts/elsewhere.txt",
    ] {
        fx.add(&log, "tool_call_completed", completed("cut", Some(value)));
    }
    let found = run("needle", &dir);
    assert!(found.hits.is_empty(), "{:?}", hits(&found));
    let link = dir.join("artifacts").join("link.txt");
    assert_eq!(found.problems, [format!("{} is a link", link.display())]);
}

#[test]
fn a_linked_artifacts_directory_gives_no_hit_and_a_problem() {
    let fx = Fixture::new();
    let log = fx.log("s_1");
    let dir = fx.dir("s_1");
    let outside = fx.sessions.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("a.txt"), "needle").unwrap();
    fs::remove_dir(dir.join("artifacts")).unwrap();
    symlink(&outside, dir.join("artifacts")).unwrap();
    fx.add(
        &log,
        "tool_call_completed",
        completed("cut", Some("artifacts/a.txt")),
    );
    let found = run("needle", &dir);
    assert!(found.hits.is_empty());
    assert_eq!(
        found.problems,
        [format!("{} is a link", dir.join("artifacts").display())]
    );
}

#[test]
fn a_newline_query_matches_across_artifact_lines() {
    let fx = Fixture::new();
    let log = fx.log("s_1");
    let dir = fx.dir("s_1");
    let file = artifact(&dir, "a.txt", b"zero\nends with ONE\nTWO starts\nthree\n");
    let seq = fx.add(
        &log,
        "tool_call_completed",
        completed("cut", Some("artifacts/a.txt")),
    );
    let found = run("one\ntwo", &dir);
    assert_eq!(
        hits(&found),
        [(
            Label::ToolOutput,
            seq,
            "ends with ONE\nTWO starts".into(),
            Some(file)
        )]
    );
}

#[test]
fn a_newline_query_skips_an_artifact_with_a_late_nul() {
    let fx = Fixture::new();
    let log = fx.log("s_1");
    let dir = fx.dir("s_1");
    let mut late = b"one\ntwo\n".to_vec();
    late.extend(std::iter::repeat_n(b'a', 1 << 17));
    late.extend(b"\0");
    artifact(&dir, "a.txt", &late);
    fx.add(
        &log,
        "tool_call_completed",
        completed("cut", Some("artifacts/a.txt")),
    );
    assert!(run("one\ntwo", &dir).hits.is_empty());
}

#[test]
fn an_artifact_search_stopped_before_the_end_admits_nothing() {
    let dir = TempDir::new("log-search-first");
    let path = dir.path().join("late.txt");
    // Two matches, then a NUL past the searcher's first buffer.
    let mut late = b"needle first\nneedle second\n".to_vec();
    late.extend(std::iter::repeat_n(b'a', 1 << 21));
    late.extend(b"\n\0\n");
    fs::write(&path, &late).unwrap();
    let text = Text::new("needle").unwrap();
    // A cancel at each check in turn: one of them is the second match's
    // check, which stops the search after the first match was kept and
    // before the NUL is read, and none admits a hit.
    let mut stopped = 0;
    for after in 0..16 {
        match first_match(&text, File::open(&path).unwrap(), &After::new(after)) {
            Ok(Some(snippet)) => panic!("cancel after {after} checks admitted {snippet:?}"),
            Ok(None) => stopped += 1,
            Err(_) => {}
        }
    }
    assert!(stopped > 0, "no cancel stopped the search at the match");
    // Without the NUL, a search that runs to the end admits the hit.
    fs::write(&path, &late[..late.len() - 2]).unwrap();
    let got = first_match(&text, File::open(&path).unwrap(), &CancelToken::new()).unwrap();
    assert_eq!(got.as_deref(), Some("needle first"));
}

#[test]
fn the_name_is_the_latest_named_else_the_first_prompt() {
    let fx = Fixture::new();
    let log = fx.log("s_1");
    fx.add(&log, "turn_started", prompt("first needle prompt"));
    fx.add(&log, "turn_started", prompt("second prompt"));
    fx.add(&log, "session_named", named(Some("old")));
    fx.add(&log, "session_named", named(Some("new name")));
    let found = run("needle", &fx.dir("s_1"));
    assert_eq!(found.hits[0].name, "new name");

    // A cleared name falls back to the first turn's prompt.
    fx.add(&log, "session_named", named(None));
    let found = run("needle", &fx.dir("s_1"));
    assert_eq!(found.hits[0].name, "first needle prompt");

    // No name and no turn: empty.
    let other = fx.log("s_2");
    fx.add(&other, "text_completed", json!({"text": "needle"}));
    let found = run("needle", &fx.dir("s_2"));
    assert_eq!(found.hits[0].name, "");

    // The first message of the first turn, its text parts joined; a part
    // of another type holds no text even with a `text` key.
    fx.log("s_4");
    raw(
        &fx.dir("s_4"),
        concat!(
            r#"{"kind":"turn_started","session_id":"s_4","ts":1,"schema_version":1,"seq":1,"payload":{"input":["#,
            r#"{"type":"shell_command","seq":0},"#,
            r#"{"type":"message","source":"driver","content":[{"type":"text","text":"one "},{"type":"unknown","text":"x"},{"type":"text","text":"needle"}]}"#,
            "]}}\n",
        )
        .as_bytes(),
    );
    let found = run("needle", &fx.dir("s_4"));
    assert_eq!(found.hits[0].name, "one needle");

    // A first turn with no message gives an empty name, never a later
    // turn's.
    let third = fx.log("s_3");
    fx.add(
        &third,
        "turn_started",
        json!({"input": [{"type": "shell_command", "seq": 0}]}),
    );
    fx.add(&third, "turn_started", prompt("later needle"));
    let found = run("needle", &fx.dir("s_3"));
    assert_eq!(found.hits[0].name, "");
}

#[test]
fn the_first_matching_string_of_a_label_gives_its_snippet() {
    let fx = Fixture::new();
    let log = fx.log("s_1");
    let input = json!({"input": [{"type": "message", "content": [text("plain"), text("first needle"), text("second needle")], "source": "driver"}]});
    let seq = fx.add(&log, "turn_started", input);
    let found = run("needle", &fx.dir("s_1"));
    assert_eq!(
        hits(&found),
        [(Label::Message, seq, "first needle".into(), None)]
    );
}

#[test]
fn a_missing_or_odd_artifacts_directory_lists_no_problem() {
    let fx = Fixture::new();
    let log = fx.log("s_1");
    let seq = fx.add(&log, "text_completed", json!({"text": "needle"}));
    let dir = fx.dir("s_1");
    fs::remove_dir(dir.join("artifacts")).unwrap();
    let want = [(Label::Message, seq, "needle".to_owned(), None)];
    let found = run("needle", &dir);
    assert_eq!((hits(&found), found.problems), (want.to_vec(), vec![]));
    // `artifacts` that is a file.
    fs::write(dir.join("artifacts"), "needle").unwrap();
    let found = run("needle", &dir);
    assert_eq!((hits(&found), found.problems), (want.to_vec(), vec![]));
    // A directory inside `artifacts/`, named by a line.
    fs::remove_file(dir.join("artifacts")).unwrap();
    fs::create_dir_all(dir.join("artifacts").join("sub")).unwrap();
    fx.add(
        &log,
        "tool_call_completed",
        completed("cut", Some("artifacts/sub")),
    );
    let found = run("needle", &dir);
    assert_eq!((hits(&found), found.problems), (want.to_vec(), vec![]));
}

#[test]
fn a_cancel_is_seen_at_a_match_across_lines() {
    let dir = TempDir::new("log-search-multi");
    let path = dir.path().join("a.txt");
    fs::write(&path, "one\ntwo\n").unwrap();
    let text = Text::new("one\ntwo").unwrap();
    let counted = After::new(usize::MAX);
    let got = first_match(&text, File::open(&path).unwrap(), &counted).unwrap();
    assert_eq!(got.as_deref(), Some("one\ntwo"));
    // The last check is the match's own: the read is done, and the
    // cancelled search keeps nothing.
    let checks = counted.checks.load(Ordering::SeqCst);
    let got = first_match(&text, File::open(&path).unwrap(), &After::new(checks - 1));
    assert_eq!(got.unwrap(), None);
}

#[test]
fn a_cancel_is_seen_at_each_selected_line() {
    let fx = Fixture::new();
    let log = fx.log("s_1");
    let first = fx.add(&log, "text_completed", json!({"text": "needle 1"}));
    fx.add(&log, "text_completed", json!({"text": "needle 2"}));
    let dir = fx.dir("s_1");
    let counted = After::new(usize::MAX);
    assert_eq!(run_with("needle", &dir, &counted).total, 2);
    // The checks end with the last line's and the read of the end: a cancel
    // at the last line's check keeps the line before it.
    let checks = counted.checks.load(Ordering::SeqCst);
    let found = run_with("needle", &dir, &After::new(checks - 2));
    assert_eq!(
        hits(&found),
        [(Label::Message, first, "needle 1".into(), None)]
    );
}
