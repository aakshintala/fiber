//! `fiber sessions` against an in-process fake hub: the scope it asks for,
//! the lines it skips, its failures, and the rows it prints as text and
//! JSON.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use contract::{ErrorCode, HubLine};
use serde_json::{Map, Value, json};

use super::run;

/// One named deadline per wait on the fake hub.
const HUB_DEADLINE: Duration = Duration::from_secs(10);

/// An in-process hub: `connect` hands out one end of a pair, and a thread
/// on the other reads the one command line, then writes `answers`.
struct FakeHub {
    answers: Vec<String>,
    got: Option<mpsc::Receiver<String>>,
}

impl FakeHub {
    fn new(answers: &[Value]) -> Self {
        Self {
            answers: answers.iter().map(|line| format!("{line}\n")).collect(),
            got: None,
        }
    }

    fn connect(&mut self) -> io::Result<doors::hub::Hub> {
        let (client, hub) = UnixStream::pair()?;
        hub.set_read_timeout(Some(HUB_DEADLINE))?;
        let (tx, rx) = mpsc::channel();
        let answers = self.answers.clone();
        thread::spawn(move || {
            let mut line = String::new();
            BufReader::new(&hub).read_line(&mut line).unwrap();
            tx.send(line).unwrap();
            for answer in answers {
                (&hub).write_all(answer.as_bytes()).unwrap();
            }
        });
        self.got = Some(rx);
        let hello = HubLine {
            kind: "hub_hello".to_owned(),
            ts: 1,
            schema_version: 1,
            payload: Map::new(),
        };
        Ok((client, hello))
    }

    /// The command line the hub read, as JSON.
    fn sent(&self) -> Value {
        let line = self
            .got
            .as_ref()
            .unwrap()
            .recv_timeout(HUB_DEADLINE)
            .unwrap();
        serde_json::from_str(&line).unwrap()
    }
}

fn hub_line(kind: &str, payload: Value) -> Value {
    json!({"kind": kind, "ts": 1, "schema_version": 1, "payload": payload})
}

fn accepted(result: Value) -> Value {
    hub_line(
        "command_accepted",
        json!({"command_id": "c_sessions", "result": result}),
    )
}

/// A `session_status` payload: `state` with its keys merged in, and
/// `cost` (`None` leaves it `null`) and `subscription_cost`.
fn status(name: &str, state: Value, cost: Option<f64>, subscription: f64) -> Value {
    let mut payload = json!({
        "name": name, "workspace": "/w", "project": "-w", "model": "p/m",
        "since": 1, "delegates": 0, "jobs": 0, "clients": 0,
        "spend": {
            "tokens": {"input": 0, "cache_read": 0, "cache_write": {}, "output": 0},
            "cost": cost, "subscription_cost": subscription,
        },
    });
    for (key, value) in state.as_object().unwrap() {
        payload[key] = value.clone();
    }
    payload
}

fn idle(name: &str) -> Value {
    status(name, json!({"state": "idle"}), Some(0.0), 0.0)
}

fn waiting_on(kind: &str, summary: &str) -> Value {
    json!({"state": "waiting", "waiting": {"request_id": "r_1", "kind": kind, "summary": summary}})
}

fn live(id: &str, status: Value) -> Value {
    json!({"session_id": id, "status": status})
}

fn exited(id: &str, name: &str, how: &str, status: Option<Value>) -> Value {
    let mut row = json!({
        "session_id": id, "ts": 2, "project": "-w", "workspace": "/w", "name": name, "how": how,
    });
    if let Some(status) = status {
        row["status"] = status;
    }
    row
}

/// Runs the list against `answers` in `workspace` with project `identity`:
/// the outcome, what it printed, and the command the hub read.
fn listing(
    workspace: &Path,
    identity: &Path,
    all: bool,
    json: bool,
    answers: &[Value],
) -> (Result<(), contract::shapes::Failure>, String, Value) {
    let mut hub = FakeHub::new(answers);
    let mut out = Vec::new();
    let ran = run(workspace, identity, all, json, &mut out, &mut || {
        hub.connect()
    });
    (ran, String::from_utf8(out).unwrap(), hub.sent())
}

/// The JSON Lines `fiber sessions --json` prints for `result`.
fn json_rows(result: Value) -> Vec<Value> {
    let (ran, out, _) = listing(
        Path::new("/w"),
        Path::new("/w"),
        false,
        true,
        &[accepted(result)],
    );
    ran.unwrap();
    out.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn empty() -> Value {
    json!({"live": [], "exited": []})
}

#[test]
fn inside_a_repository_without_all_the_request_names_the_project() {
    let (ran, _, sent) = listing(
        Path::new("/r/w"),
        Path::new("/r/.git"),
        false,
        false,
        &[accepted(empty())],
    );
    ran.unwrap();
    assert_eq!(
        sent,
        json!({"id": "c_sessions", "command": "sessions", "args": {"project": "-r-.git"}})
    );
}

#[test]
fn with_all_or_outside_a_repository_the_request_names_no_project() {
    for (identity, all) in [("/r/.git", true), ("/w", false)] {
        let (ran, _, sent) = listing(
            Path::new("/w"),
            Path::new(identity),
            all,
            false,
            &[accepted(empty())],
        );
        ran.unwrap();
        assert_eq!(
            sent,
            json!({"id": "c_sessions", "command": "sessions", "args": {}}),
            "{identity} all={all}"
        );
    }
}

#[test]
fn lines_before_the_answer_are_skipped() {
    let (ran, out, _) = listing(
        Path::new("/w"),
        Path::new("/w"),
        false,
        true,
        &[
            hub_line("attention", json!({"session_id": "s_1"})),
            hub_line("command_accepted", json!({"command_id": "c_other"})),
            json!("not a hub line"),
            accepted(json!({"live": [live("s_1", idle("a"))], "exited": []})),
        ],
    );
    ran.unwrap();
    assert_eq!(out.lines().count(), 1, "{out}");
}

#[test]
fn a_rejection_is_a_failure_with_its_code_and_message() {
    let (ran, out, _) = listing(
        Path::new("/w"),
        Path::new("/w"),
        false,
        false,
        &[hub_line(
            "command_rejected",
            json!({"command_id": "c_sessions", "code": "invalid_arguments", "message": "no"}),
        )],
    );
    let failure = ran.unwrap_err();
    assert_eq!(failure.code, ErrorCode::InvalidArguments);
    assert_eq!(failure.message, "no");
    assert_eq!(doors::exit_code(&failure), 1);
    assert!(out.is_empty());
}

#[test]
fn an_end_before_the_answer_is_io_failed() {
    let (ran, _, _) = listing(Path::new("/w"), Path::new("/w"), false, false, &[]);
    let failure = ran.unwrap_err();
    assert_eq!(failure.code, ErrorCode::IoFailed);
    assert!(
        failure.message.contains("`sessions`"),
        "{}",
        failure.message
    );
}

#[test]
fn a_live_state_is_its_status_word_and_an_exited_one_is_how_it_ended() {
    let states = [
        (json!({"state": "streaming"}), "streaming"),
        (json!({"state": "tool", "tool": "shell"}), "tool"),
        (json!({"state": "retrying"}), "retrying"),
        (waiting_on("approval", "run ls"), "waiting"),
        (json!({"state": "jobs"}), "jobs"),
        (json!({"state": "idle"}), "idle"),
    ];
    let live_rows: Vec<Value> = states
        .iter()
        .enumerate()
        .map(|(n, (state, _))| live(&format!("s_{n}"), status("n", state.clone(), None, 0.0)))
        .collect();
    let rows = json_rows(json!({
        "live": live_rows,
        "exited": [exited("s_e", "n", "exited", None), exited("s_c", "n", "crashed", None)],
    }));
    let words: Vec<&str> = rows
        .iter()
        .map(|row| row["state"].as_str().unwrap())
        .collect();
    let mut expected: Vec<&str> = states.iter().map(|(_, word)| *word).collect();
    expected.extend(["exited", "crashed"]);
    assert_eq!(words, expected);
}

#[test]
fn waiting_names_approval_or_question_live_and_exited_alike() {
    let rows = json_rows(json!({
        "live": [
            live("s_1", status("a", waiting_on("approval", "shell ls"), None, 0.0)),
            live("s_2", status("b", waiting_on("question", "2 of 3 answered"), None, 0.0)),
            live("s_3", idle("c")),
        ],
        "exited": [
            exited("s_4", "d", "exited", Some(status("d", waiting_on("question", "pick"), None, 0.0))),
            exited("s_5", "e", "exited", None),
        ],
    }));
    let waits: Vec<&Value> = rows.iter().map(|row| &row["waiting"]).collect();
    assert_eq!(
        waits,
        [
            &json!("approval: shell ls"),
            &json!("question: 2 of 3 answered"),
            &Value::Null,
            &json!("question: pick"),
            &Value::Null,
        ]
    );
}

#[test]
fn spend_adds_cost_and_subscription_cost_and_is_zero_without_a_status() {
    let rows = json_rows(json!({
        "live": [
            live("s_1", status("a", json!({"state": "idle"}), Some(0.25), 0.5)),
            live("s_2", status("b", json!({"state": "idle"}), None, 0.125)),
        ],
        "exited": [
            exited("s_3", "c", "exited", Some(status("c", json!({"state": "idle"}), Some(1.0), 0.0))),
            exited("s_4", "d", "crashed", None),
        ],
    }));
    let spends: Vec<f64> = rows
        .iter()
        .map(|row| row["spend"].as_f64().unwrap())
        .collect();
    assert_eq!(spends, [0.75, 0.125, 1.0, 0.0]);
}

#[test]
fn the_name_is_the_statuss_else_the_rows_and_an_empty_one_is_a_dash_in_text() {
    let result = json!({
        "live": [live("s_1", idle(""))],
        "exited": [
            exited("s_2", "row name", "exited", Some(idle("status name"))),
            exited("s_3", "row name", "exited", Some(idle(""))),
            exited("s_4", "", "crashed", None),
        ],
    });
    let names: Vec<Value> = json_rows(result.clone())
        .iter()
        .map(|row| row["name"].clone())
        .collect();
    assert_eq!(
        names,
        [
            json!(""),
            json!("status name"),
            json!("row name"),
            json!("")
        ]
    );
    let (ran, out, _) = listing(
        Path::new("/w"),
        Path::new("/w"),
        false,
        false,
        &[accepted(result)],
    );
    ran.unwrap();
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[1].ends_with("  -"), "{}", lines[1]);
    assert!(lines[4].ends_with("  -"), "{}", lines[4]);
}

#[test]
fn json_keys_come_in_the_documented_order() {
    let (ran, out, _) = listing(
        Path::new("/w"),
        Path::new("/w"),
        false,
        true,
        &[accepted(json!({
            "live": [
                live("s_1", status("fix it", waiting_on("approval", "run"), Some(0.42), 0.0)),
                live("s_2", idle("a")),
            ],
            "exited": [],
        }))],
    );
    ran.unwrap();
    assert_eq!(
        out,
        "{\"id\":\"s_1\",\"state\":\"waiting\",\"name\":\"fix it\",\"waiting\":\"approval: run\",\"spend\":0.42}\n\
         {\"id\":\"s_2\",\"state\":\"idle\",\"name\":\"a\",\"waiting\":null,\"spend\":0.0}\n"
    );
}

#[test]
fn text_is_a_padded_table_with_a_header_and_two_decimal_spend() {
    let (ran, out, _) = listing(
        Path::new("/w"),
        Path::new("/w"),
        false,
        false,
        &[accepted(json!({
            "live": [live(
                "s_0123456789abcdef",
                status("fix the flaky test", waiting_on("approval", "shell cargo publish --dry-run"), Some(0.42), 0.0),
            )],
            "exited": [exited("s_fedcba9876543210", "", "crashed", None)],
        }))],
    );
    ran.unwrap();
    assert_eq!(
        out,
        "id                  state    spend  waits on                                 name\n\
         s_0123456789abcdef  waiting  $0.42  approval: shell cargo publish --dry-run  fix the flaky test\n\
         s_fedcba9876543210  crashed  $0.00  -                                        -\n"
    );
}

#[test]
fn no_rows_prints_the_header_in_text_and_nothing_in_json() {
    let (ran, out, _) = listing(
        Path::new("/w"),
        Path::new("/w"),
        false,
        false,
        &[accepted(empty())],
    );
    ran.unwrap();
    assert_eq!(out, "id  state  spend  waits on  name\n");
    assert!(json_rows(empty()).is_empty());
}
