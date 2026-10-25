//! `fiber sessions` against an in-process fake hub: the scope it asks for,
//! the lines it skips, its failures, and the rows it prints as text and
//! JSON.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use contract::shapes::Failure;
use contract::{ErrorCode, HubLine};
use serde_json::{Map, Value, json};

use super::{latest_listed, list, resolve_listed, run};
use crate::CliError;
use fakes::Deadline;

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

    fn connect(&mut self) -> Result<doors::hub::Hub, doors::Error> {
        let (client, hub) = UnixStream::pair().map_err(doors::Error::Local)?;
        hub.set_read_timeout(Some(HUB_DEADLINE))
            .map_err(doors::Error::Local)?;
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
    #[track_caller]
    fn sent(&self) -> Value {
        let line = Deadline::after(HUB_DEADLINE)
            .recv(self.got.as_ref().unwrap())
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
        "started": 0, "calls": 0, "since": 1, "delegates": 0, "jobs": 0, "clients": 0,
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

/// A project at `path`, inside a repository or not.
fn project_at(path: &str, in_repository: bool) -> doors::Project {
    doors::Project {
        path: path.into(),
        in_repository,
    }
}

/// Runs the list against `answers` in `project`: the outcome, what it
/// printed, and the command the hub read.
#[track_caller]
fn listing(
    project: &doors::Project,
    all: bool,
    json: bool,
    answers: &[Value],
) -> (Result<(), CliError>, String, Value) {
    let project = project.clone();
    let answers = answers.to_owned();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut hub = FakeHub::new(&answers);
        let mut out = Vec::new();
        let ran = run(&project, all, json, &mut out, &mut || hub.connect());
        let text = String::from_utf8(out).unwrap();
        let sent = hub.sent();
        tx.send((ran, text, sent)).unwrap_or(());
    });
    Deadline::after(HUB_DEADLINE).recv(&rx).unwrap()
}

/// The JSON Lines `fiber sessions --json` prints for `result`.
#[track_caller]
fn json_rows(result: Value) -> Vec<Value> {
    let (ran, out, _) = listing(&project_at("/w", false), false, true, &[accepted(result)]);
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
        &project_at("/r/.git", true),
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
    for (identity, in_repository, all) in [("/r/.git", true, true), ("/w", false, false)] {
        let (ran, _, sent) = listing(
            &project_at(identity, in_repository),
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
        &project_at("/w", false),
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
        &project_at("/w", false),
        false,
        false,
        &[hub_line(
            "command_rejected",
            json!({"command_id": "c_sessions", "code": "invalid_arguments", "message": "no"}),
        )],
    );
    let failure = ran.unwrap_err();
    assert!(
        matches!(
            &failure,
            CliError::Rejected {
                code: ErrorCode::InvalidArguments,
                ..
            }
        ),
        "{failure:?}"
    );
    assert_eq!(failure.code(), ErrorCode::InvalidArguments);
    assert_eq!(failure.to_string(), "no");
    assert_eq!(doors::exit_code(&Failure::from(failure)), 1);
    assert!(out.is_empty());
}

#[test]
fn an_end_before_the_answer_is_io_failed() {
    let (ran, _, _) = listing(&project_at("/w", false), false, false, &[]);
    let failure = ran.unwrap_err();
    assert!(
        matches!(&failure, CliError::HubClosed { command } if command == "sessions"),
        "{failure:?}"
    );
    assert_eq!(failure.code(), ErrorCode::IoFailed);
    assert!(
        failure.to_string().contains("`sessions`"),
        "{}",
        failure.to_string()
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
            live("s_6", status("f", waiting_on("offer", "run repository code"), None, 0.0)),
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
            &json!("offer: run repository code"),
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
    let (ran, out, _) = listing(&project_at("/w", false), false, false, &[accepted(result)]);
    ran.unwrap();
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[1].ends_with("  -"), "{}", lines[1]);
    assert!(lines[4].ends_with("  -"), "{}", lines[4]);
}

#[test]
fn json_keys_come_in_the_documented_order() {
    let (ran, out, _) = listing(
        &project_at("/w", false),
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
        &project_at("/w", false),
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
fn a_multiline_name_is_one_row_in_text_and_whole_in_json() {
    let name = "say hi\nthen bye\r\nor\rafter";
    let result = json!({"live": [live("s_1", idle(name))], "exited": []});
    let rows = json_rows(result.clone());
    assert_eq!(rows[0]["name"], json!(name));
    let (ran, out, _) = listing(&project_at("/w", false), false, false, &[accepted(result)]);
    ran.unwrap();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "{out}");
    assert!(lines[1].contains("say hi then bye or after"), "{out}");
}

#[test]
fn a_tab_and_an_escape_in_a_name_print_as_spaces_in_text() {
    let name = "fix\tthe\u{1b}[31mtest";
    let result = json!({"live": [live("s_1", idle(name))], "exited": []});
    let rows = json_rows(result.clone());
    assert_eq!(rows[0]["name"], json!(name));
    let (ran, out, _) = listing(&project_at("/w", false), false, false, &[accepted(result)]);
    ran.unwrap();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "{out}");
    assert!(!out.contains('\u{1b}'), "{out:?}");
    assert!(lines[1].contains("fix the [31mtest"), "{out}");
}

#[test]
fn no_rows_prints_the_header_in_text_and_nothing_in_json() {
    let (ran, out, _) = listing(&project_at("/w", false), false, false, &[accepted(empty())]);
    ran.unwrap();
    assert_eq!(out, "id  state  spend  waits on  name\n");
    assert!(json_rows(empty()).is_empty());
}

/// The child runs `list` and exits with its code, so the parent can read
/// the exit code while the list's own stdout goes nowhere.
const CHILD: &str = "FIBER_CLI_SESSIONS_LIST_CHILD";

#[test]
fn list_exits_zero_on_success_and_fail_code_on_hub_failure() {
    if let Ok(case) = std::env::var(CHILD) {
        let code = match case.as_str() {
            "connect-error" => list(false, false, &mut || {
                Err(doors::Error::Local(io::Error::other("hub down")))
            }),
            "rejected" => {
                let mut hub = FakeHub::new(&[hub_line(
                    "command_rejected",
                    json!({"command_id": "c_sessions", "code": "invalid_arguments", "message": "no"}),
                )]);
                let code = list(false, false, &mut || hub.connect());
                assert_eq!(hub.sent()["command"], json!("sessions"));
                code
            }
            _ => {
                let mut hub = FakeHub::new(&[accepted(empty())]);
                let code = list(false, false, &mut || hub.connect());
                assert_eq!(hub.sent()["command"], json!("sessions"));
                code
            }
        };
        std::process::exit(code);
    }
    let root = fakes::TempDir::new("cli-sessions-list");
    let workspace = root.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    let workspace = fs::canonicalize(&workspace).unwrap();
    let io_failed = crate::fail(doors::failure(ErrorCode::IoFailed, "probe"));
    let refused = crate::fail(doors::failure(ErrorCode::InvalidArguments, "probe"));
    assert_eq!(io_failed, 1);
    assert_eq!(refused, 1);
    let name = module_path!().split_once("::").unwrap().1;
    for (case, code) in [
        ("success", 0),
        ("connect-error", io_failed),
        ("rejected", refused),
    ] {
        let output = fakes::rerun_prepared(
            &format!("{name}::list_exits_zero_on_success_and_fail_code_on_hub_failure"),
            &[(CHILD, case)],
            Deadline::after(HUB_DEADLINE),
            |command| {
                command.current_dir(&workspace);
            },
        );
        assert_eq!(
            output.status.code(),
            Some(code),
            "{case}: {}",
            output.status
        );
    }
}

/// Resolves `selector` on hub `box` against `result` answered to one
/// `sessions` command: the outcome and the command the hub read, the
/// outcome received under the hub deadline.
#[track_caller]
fn resolving(selector: &str, result: Value) -> (Result<contract::SessionId, CliError>, Value) {
    let selector = selector.to_owned();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut hub = FakeHub::new(&[accepted(result)]);
        let resolved = resolve_listed(&selector, "box", &mut || hub.connect());
        let sent = hub.sent();
        tx.send((resolved, sent)).unwrap_or(());
    });
    Deadline::after(HUB_DEADLINE).recv(&rx).unwrap()
}

#[test]
fn an_exact_id_wins_over_a_longer_id_it_prefixes() {
    let (resolved, sent) = resolving(
        "s_1",
        json!({"live": [live("s_1", idle("a")), live("s_10", idle("b"))], "exited": []}),
    );
    assert_eq!(resolved.unwrap().0, "s_1");
    assert_eq!(
        sent,
        json!({"id": "c_sessions", "command": "sessions", "args": {}})
    );
}

#[test]
fn a_unique_prefix_resolves_live_or_exited() {
    for (selector, expected) in [("s_abc", "s_abcdef"), ("s_ex", "s_exit1")] {
        let (resolved, _) = resolving(
            selector,
            json!({
                "live": [live("s_abcdef", idle("a"))],
                "exited": [exited("s_exit1", "n", "exited", None)],
            }),
        );
        assert_eq!(resolved.unwrap().0, expected, "{selector}");
    }
}

#[test]
fn a_prefix_matching_two_rows_is_a_usage_error_naming_both_sorted() {
    for result in [
        json!({"live": [live("s_b", idle("a")), live("s_a", idle("b"))], "exited": []}),
        json!({
            "live": [live("s_b", idle("a"))],
            "exited": [exited("s_a", "n", "exited", None)],
        }),
    ] {
        let (resolved, _) = resolving("s_", result);
        let failure = resolved.unwrap_err();
        assert!(
            matches!(
                &failure,
                CliError::Log(log::Error::Ambiguous { matches, .. })
                    if matches == &["s_a".to_owned(), "s_b".to_owned()]
            ),
            "{failure:?}"
        );
        assert_eq!(failure.code(), ErrorCode::Usage);
        assert_eq!(
            failure.to_string(),
            "\"s_\" matches more than one session: s_a, s_b"
        );
    }
}

#[test]
fn no_match_is_session_not_found_naming_the_hub() {
    let (resolved, _) = resolving("s_nope", empty());
    let failure = resolved.unwrap_err();
    assert!(
        matches!(
            &failure,
            CliError::SessionNotFoundOnHub { selector, hub }
                if selector == "s_nope" && hub == "box"
        ),
        "{failure:?}"
    );
    assert_eq!(failure.code(), ErrorCode::SessionNotFound);
    assert_eq!(
        failure.to_string(),
        "no session matches \"s_nope\" on hub `box`"
    );
}

#[test]
fn an_empty_selector_matches_nothing_without_connecting() {
    let mut connect = || -> Result<doors::hub::Hub, doors::Error> {
        panic!("an empty selector must not connect")
    };
    let failure = resolve_listed("", "box", &mut connect).unwrap_err();
    assert!(
        matches!(
            &failure,
            CliError::SessionNotFoundOnHub { selector, hub }
                if selector.is_empty() && hub == "box"
        ),
        "{failure:?}"
    );
    assert_eq!(failure.code(), ErrorCode::SessionNotFound);
    assert_eq!(failure.to_string(), "no session matches \"\" on hub `box`");
}

#[test]
fn a_rejection_and_an_early_end_are_hub_failures() {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut hub = FakeHub::new(&[hub_line(
            "command_rejected",
            json!({"command_id": "c_sessions", "code": "invalid_arguments", "message": "no"}),
        )]);
        let resolved = resolve_listed("s_1", "box", &mut || hub.connect());
        tx.send(resolved).unwrap_or(());
    });
    let resolved: Result<contract::SessionId, CliError> =
        Deadline::after(HUB_DEADLINE).recv(&rx).unwrap();
    let failure = resolved.unwrap_err();
    assert!(
        matches!(
            &failure,
            CliError::Rejected {
                code: ErrorCode::InvalidArguments,
                ..
            }
        ),
        "{failure:?}"
    );
    assert_eq!(failure.code(), ErrorCode::InvalidArguments);
    assert_eq!(failure.to_string(), "no");

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut hub = FakeHub::new(&[]);
        let resolved = resolve_listed("s_1", "box", &mut || hub.connect());
        tx.send(resolved).unwrap_or(());
    });
    let resolved: Result<contract::SessionId, CliError> =
        Deadline::after(HUB_DEADLINE).recv(&rx).unwrap();
    let failure = resolved.unwrap_err();
    assert!(
        matches!(&failure, CliError::HubClosed { command } if command == "sessions"),
        "{failure:?}"
    );
    assert_eq!(failure.code(), ErrorCode::IoFailed);
    assert!(
        failure.to_string().contains("`sessions`"),
        "{}",
        failure.to_string()
    );

    let failure = resolve_listed("s_1", "box", &mut || {
        Err(doors::Error::Local(io::Error::other("hub down")))
    })
    .unwrap_err();
    assert!(
        matches!(&failure, CliError::Hub(doors::Error::Local(_))),
        "{failure:?}"
    );
    assert_eq!(failure.code(), ErrorCode::IoFailed);
    assert_eq!(failure.to_string(), "the hub: hub down");
}

#[test]
fn a_wrong_shaped_result_is_a_sessions_answer_error() {
    let (ran, _, _) = listing(
        &project_at("/w", false),
        false,
        false,
        &[accepted(json!(7))],
    );
    let failure = ran.unwrap_err();
    assert!(
        matches!(&failure, CliError::SessionsAnswer(_)),
        "{failure:?}"
    );
    assert_eq!(failure.code(), ErrorCode::IoFailed);

    let (resolved, _) = resolving("s_1", json!(7));
    let failure = resolved.unwrap_err();
    assert!(
        matches!(&failure, CliError::SessionsAnswer(_)),
        "{failure:?}"
    );
    assert_eq!(failure.code(), ErrorCode::IoFailed);
}

/// A live row with `active`: the row's `status` plus its activity.
fn live_active(id: &str, name: &str, active: Value) -> Value {
    let mut row = live(id, idle(name));
    row["active"] = active;
    row
}

/// An exited row with `active`: the row's fields plus its activity.
fn exited_active(id: &str, active: Value) -> Value {
    let mut row = exited(id, "n", "exited", None);
    row["active"] = active;
    row
}

/// Runs `latest_listed` against `result` answered to one `sessions`
/// command: the outcome and the command the hub read, the outcome
/// received under the hub deadline.
#[track_caller]
fn latest(result: Value) -> (Result<Option<contract::SessionId>, CliError>, Value) {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut hub = FakeHub::new(&[accepted(result)]);
        let picked = latest_listed(&mut || hub.connect());
        let sent = hub.sent();
        tx.send((picked, sent)).unwrap_or(());
    });
    Deadline::after(HUB_DEADLINE).recv(&rx).unwrap()
}

/// The picked id, if any.
fn picked(result: Value) -> Option<String> {
    latest(result).0.unwrap().map(|id| id.0)
}

#[test]
fn latest_requests_activity_without_a_project() {
    let (_, sent) = latest(empty());
    assert_eq!(
        sent,
        json!({"id": "c_sessions", "command": "sessions", "args": {"active": true}})
    );
}

#[test]
fn latest_prefers_the_greatest_activity_live_or_exited() {
    // The exited row wrote last, so it wins over the live one: the pick
    // follows the last line, never the row's `ts` or `since`
    // (docs/invocation.md:115: `continue --hub` opens the entry with the
    // greatest (`active`, `id`)).
    let result = json!({
        "live": [live_active("s_live", "a", json!(100))],
        "exited": [exited_active("s_exited", json!(300))],
    });
    assert_eq!(picked(result), Some("s_exited".to_owned()));
    let result = json!({
        "live": [live_active("s_live", "a", json!(300))],
        "exited": [exited_active("s_exited", json!(100))],
    });
    assert_eq!(picked(result), Some("s_live".to_owned()));
}

#[test]
fn latest_breaks_activity_ties_by_greater_id() {
    // Just above wins, just below loses, and an exact tie falls back to
    // the greater id.
    let tied = |first: &str, second: &str| {
        json!({
            "live": [live_active(first, "a", json!(100)), live_active(second, "b", json!(100))],
            "exited": [],
        })
    };
    assert_eq!(picked(tied("s_a", "s_b")), Some("s_b".to_owned()));
    assert_eq!(picked(tied("s_b", "s_a")), Some("s_b".to_owned()));
    let above = json!({
        "live": [live_active("s_a", "a", json!(101))],
        "exited": [exited_active("s_b", json!(100))],
    });
    assert_eq!(picked(above), Some("s_a".to_owned()));
    let below = json!({
        "live": [live_active("s_a", "a", json!(99))],
        "exited": [exited_active("s_b", json!(100))],
    });
    assert_eq!(picked(below), Some("s_b".to_owned()));
}

#[test]
fn latest_without_an_eligible_entry_is_none() {
    assert_eq!(picked(empty()), None);
    let result = json!({
        "live": [live_active("s_live", "a", Value::Null)],
        "exited": [exited_active("s_exited", Value::Null)],
    });
    assert_eq!(picked(result), None);
}

#[test]
fn latest_skips_null_activity() {
    let result = json!({
        "live": [live_active("s_live", "a", Value::Null)],
        "exited": [exited_active("s_exited", json!(7))],
    });
    assert_eq!(picked(result), Some("s_exited".to_owned()));
}

#[test]
fn latest_with_malformed_activity_is_a_sessions_answer_error() {
    let result = json!({
        "live": [live_active("s_live", "a", json!("soon"))],
        "exited": [],
    });
    let (got, _) = latest(result);
    let failure = got.unwrap_err();
    assert!(
        matches!(&failure, CliError::SessionsAnswer(_)),
        "{failure:?}"
    );
    assert_eq!(failure.code(), ErrorCode::IoFailed);
}
