//! `fiber sessions search` against fixture Fiber homes: the scope it
//! searches, the hits it prints as text and JSON, the problems it lists,
//! its failures, UTC, and the exit codes of the public command.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::os::unix::fs::symlink;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use contract::events::Event;
use contract::session_search::LIMIT;
use contract::{Envelope, ErrorCode, SessionId};
use fakes::clock::FakeClock;
use serde_json::{Value, json};

use super::{run, utc};

/// One named deadline per wait on a child.
const CHILD_DEADLINE: Duration = Duration::from_secs(10);

/// How long a killed child may take to be reaped.
const REAP_DEADLINE: Duration = Duration::from_secs(10);

/// The query every fixture matches.
const QUERY: &str = "retry budget";

/// An event from its kind and JSON payload.
fn event(kind: &str, payload: Value) -> Event {
    let Value::Object(payload) = payload else {
        panic!("a payload is an object");
    };
    let line = Envelope {
        kind: kind.to_owned(),
        session_id: SessionId("s".to_owned()),
        ts: 0,
        schema_version: 1,
        turn_id: None,
        action_id: None,
        seq: None,
        payload,
    };
    Event::from_envelope(&line).unwrap().unwrap()
}

/// A `session_started` recorded in `workspace`.
fn started(workspace: &Path) -> Event {
    event(
        "session_started",
        json!({
            "workspace": workspace.display().to_string(),
            "variables": {"path": "/bin", "names": [], "source": "inherited"}
        }),
    )
}

/// A message hit.
fn message(text: &str) -> Event {
    event("text_completed", json!({"text": text}))
}

/// A tool-input hit.
fn tool_input(command: &str) -> Event {
    event(
        "tool_call_requested",
        json!({"name": "shell", "arguments": {"command": command}}),
    )
}

/// A tool-output hit in `artifacts/call_1.txt`, holding `artifact_text`.
/// Returns the event and the artifact's bytes.
fn tool_output() -> (Event, Vec<u8>) {
    (
        event(
            "tool_call_completed",
            json!({"status": "completed", "content": [{"type": "text", "text": "retry budget (cut)"}], "artifact": "artifacts/call_1.txt"}),
        ),
        b"full output: RETRY BUDGET exhausted\n".to_vec(),
    )
}

/// Writes session `id` under `sessions`, one line a second from the fake
/// clock's wall time (2023-11-14T22:13:20Z): `session_started` in
/// `workspace`, then `events`.
fn session(sessions: &Path, id: &str, workspace: &Path, events: &[Event]) -> PathBuf {
    let clock = FakeClock::new();
    let clock_dyn: Arc<dyn contract::clock::Clock> = clock.clone();
    let log = log::Log::create(sessions, SessionId(id.to_owned()), clock_dyn).unwrap();
    log.append(&started(workspace), None, None).unwrap();
    for event in events {
        clock.advance(Duration::from_secs(1));
        log.append(event, None, None).unwrap();
    }
    sessions.join(id)
}

/// Writes `artifacts/call_1.txt` under the session `dir`.
fn artifact(dir: &Path, bytes: &[u8]) {
    fs::write(dir.join("artifacts").join("call_1.txt"), bytes).unwrap();
}

/// The identity the scan resolves workspaces with: `main` and `wt` belong
/// to `common`, every other workspace is its own project. No git runs.
fn identity(main: &Path, wt: &Path, common: &Path) -> log::Identity {
    let main = main.to_path_buf();
    let wt = wt.to_path_buf();
    let common = common.to_path_buf();
    Arc::new(move |path: &Path| {
        if path == main || path == wt {
            common.clone()
        } else {
            path.to_path_buf()
        }
    })
}

/// A Fiber home with four matching sessions: `s_main` (a message, a tool
/// input and a tool-output artifact hit) and `s_wt` recorded in the own
/// project, `s_same` under the same key recorded in another project, and
/// `s_far` under another key.
struct Fixtures {
    #[allow(dead_code, reason = "the home is removed when the fixtures drop")]
    root: fakes::TempDir,
    home: PathBuf,
    main: PathBuf,
    wt: PathBuf,
    out: PathBuf,
    identity: log::Identity,
}

impl Fixtures {
    fn write() -> Self {
        let root = fakes::TempDir::new("cli-sessions-search");
        let home = root.path().join("h");
        let main = root.path().join("r");
        let wt = root.path().join("rw");
        let out = root.path().join("o");
        let common = root.path().join("r.git");
        let identity = identity(&main, &wt, &common);
        let own_sessions = log::sessions_dir(&home, &common);
        let (completed, bytes) = tool_output();
        let past = session(
            &own_sessions,
            "s_main",
            &main,
            &[
                event(
                    "session_named",
                    json!({"name": "retry work", "by": "person"}),
                ),
                message("We keep the Retry Budget at 3"),
                tool_input("grep -n \"retry budget\""),
                completed,
            ],
        );
        artifact(&past, &bytes);
        session(&own_sessions, "s_wt", &wt, &[message("wt retry budget")]);
        session(
            &own_sessions,
            "s_same",
            &out,
            &[message("same retry budget")],
        );
        let far = PathBuf::from("/elsewhere/far");
        session(
            &log::sessions_dir(&home, &far),
            "s_far",
            &far,
            &[message("far retry budget")],
        );
        Self {
            root,
            home,
            main,
            wt,
            out,
            identity,
        }
    }
}

/// Runs the search over `fixtures` from `workspace`: the outcome, what it
/// printed on stdout, and what it printed on stderr.
fn searching(
    fixtures: &Fixtures,
    workspace: &Path,
    in_repository: bool,
    all: bool,
    json: bool,
) -> (Result<(), contract::shapes::Failure>, String, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let ran = run(
        &fixtures.home,
        workspace,
        Arc::clone(&fixtures.identity),
        in_repository,
        QUERY,
        all,
        json,
        &mut out,
        &mut err,
    );
    (
        ran,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

/// The session ids of JSON Lines hits.
fn ids(out: &str) -> BTreeSet<String> {
    out.lines()
        .map(|line| {
            serde_json::from_str::<Value>(line).unwrap()["session_id"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

#[test]
fn the_scope_is_the_terminal_session_list_scope() {
    let fixtures = Fixtures::write();
    for (workspace, in_repository, all, expected) in [
        (&fixtures.main, true, false, ["s_main", "s_wt"].as_slice()),
        (&fixtures.wt, true, false, ["s_main", "s_wt"].as_slice()),
        (
            &fixtures.main,
            true,
            true,
            ["s_main", "s_wt", "s_same", "s_far"].as_slice(),
        ),
        (
            &fixtures.out,
            false,
            false,
            ["s_main", "s_wt", "s_same", "s_far"].as_slice(),
        ),
        (
            &fixtures.out,
            false,
            true,
            ["s_main", "s_wt", "s_same", "s_far"].as_slice(),
        ),
    ] {
        let (ran, out, err) = searching(&fixtures, workspace, in_repository, all, true);
        ran.unwrap();
        assert_eq!(err, "", "{workspace:?} all={all}");
        assert_eq!(
            ids(&out),
            expected.iter().map(|id| (*id).to_owned()).collect(),
            "{workspace:?} all={all}"
        );
    }
}

/// A Fiber home with one matching session `s_main`: a message, a tool
/// input and a tool-output artifact hit, named "retry work".
struct Single {
    #[allow(dead_code, reason = "the home is removed when the fixtures drop")]
    root: fakes::TempDir,
    home: PathBuf,
    workspace: PathBuf,
    identity: log::Identity,
    past: PathBuf,
}

impl Single {
    fn write() -> Self {
        let root = fakes::TempDir::new("cli-sessions-search-single");
        let home = root.path().join("h");
        let workspace = root.path().join("r");
        let common = root.path().join("r.git");
        let identity = identity(&workspace, &root.path().join("rw"), &common);
        let sessions = log::sessions_dir(&home, &common);
        let (completed, bytes) = tool_output();
        let past = session(
            &sessions,
            "s_main",
            &workspace,
            &[
                event(
                    "session_named",
                    json!({"name": "retry work", "by": "person"}),
                ),
                message("We keep the Retry Budget at 3"),
                tool_input("grep -n \"retry budget\""),
                completed,
            ],
        );
        artifact(&past, &bytes);
        Self {
            root,
            home,
            workspace,
            identity,
            past,
        }
    }
}

#[test]
fn text_prints_a_header_then_one_block_per_hit_best_first() {
    let single = Single::write();
    let mut out = Vec::new();
    let mut err = Vec::new();
    run(
        &single.home,
        &single.workspace,
        Arc::clone(&single.identity),
        true,
        QUERY,
        false,
        false,
        &mut out,
        &mut err,
    )
    .unwrap();
    assert_eq!(String::from_utf8(err).unwrap(), "");
    let out = String::from_utf8(out).unwrap();
    let artifact = single.past.join("artifacts").join("call_1.txt");
    assert_eq!(
        out,
        format!(
            "3 of 3 hits for \"retry budget\", best first.\n\
             1. tool_input, session s_main \"retry work\", seq 3, 2023-11-14T22:13:23Z\n\
             \x20  grep -n \"retry budget\"\n\
             2. message, session s_main \"retry work\", seq 2, 2023-11-14T22:13:22Z\n\
             \x20  We keep the Retry Budget at 3\n\
             3. tool_output, session s_main \"retry work\", seq 4, 2023-11-14T22:13:24Z\n\
             \x20  artifact {artifact}\n\
             \x20  full output: RETRY BUDGET exhausted\n",
            artifact = artifact.display(),
        )
    );
}

#[test]
fn no_hits_names_the_scope_and_json_prints_nothing() {
    let root = fakes::TempDir::new("cli-sessions-search-empty");
    let home = root.path().join("h");
    let workspace = root.path().join("r");
    let common = root.path().join("r.git");
    let identity = identity(&workspace, &root.path().join("rw"), &common);
    let own_sessions = log::sessions_dir(&home, &common);
    session(
        &own_sessions,
        "s_old",
        &workspace,
        &[message("nothing matching here")],
    );
    for (in_repository, all, scope) in [
        (true, false, "this project's"),
        (true, true, "any project's"),
        (false, false, "any project's"),
    ] {
        let mut out = Vec::new();
        let mut err = Vec::new();
        run(
            &home,
            &workspace,
            Arc::clone(&identity),
            in_repository,
            "zzz no match",
            all,
            false,
            &mut out,
            &mut err,
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!("No hits for \"zzz no match\" in {scope} sessions.\n"),
            "in_repository={in_repository} all={all}"
        );
        assert_eq!(String::from_utf8(err).unwrap(), "");
        let mut out = Vec::new();
        let mut err = Vec::new();
        run(
            &home,
            &workspace,
            Arc::clone(&identity),
            in_repository,
            "zzz no match",
            all,
            true,
            &mut out,
            &mut err,
        )
        .unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "");
        assert_eq!(String::from_utf8(err).unwrap(), "");
    }
}

#[test]
fn the_header_counts_hits_past_the_shared_limit() {
    let root = fakes::TempDir::new("cli-sessions-search-limit");
    let home = root.path().join("h");
    let workspace = root.path().join("r");
    let common = root.path().join("r.git");
    let identity = identity(&workspace, &root.path().join("rw"), &common);
    let sessions = log::sessions_dir(&home, &common);
    let events: Vec<Event> = (0..LIMIT + 5)
        .map(|n| message(&format!("retry budget {n}")))
        .collect();
    session(&sessions, "s_many", &workspace, &events);
    let mut out = Vec::new();
    let mut err = Vec::new();
    run(
        &home, &workspace, identity, true, QUERY, false, false, &mut out, &mut err,
    )
    .unwrap();
    let out = String::from_utf8(out).unwrap();
    assert_eq!(String::from_utf8(err).unwrap(), "");
    let total = LIMIT + 5;
    assert!(
        out.starts_with(&format!(
            "{LIMIT} of {total} hits for \"retry budget\", best first.\n"
        )),
        "{out}"
    );
    assert_eq!(out.lines().count(), 1 + LIMIT * 2, "{out}");
}

#[test]
fn json_lines_carry_every_key_in_order_with_original_strings() {
    let single = Single::write();
    let mut out = Vec::new();
    let mut err = Vec::new();
    run(
        &single.home,
        &single.workspace,
        Arc::clone(&single.identity),
        true,
        QUERY,
        false,
        true,
        &mut out,
        &mut err,
    )
    .unwrap();
    assert_eq!(String::from_utf8(err).unwrap(), "");
    let out = String::from_utf8(out).unwrap();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 3, "{out}");
    let rows: Vec<Value> = lines
        .iter()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        rows.iter()
            .map(|row| row["label"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["tool_input", "message", "tool_output"]
    );
    for (line, row) in lines.iter().zip(&rows) {
        let keys = [
            "session_id",
            "name",
            "seq",
            "ts",
            "label",
            "snippet",
            "log",
            "artifact",
        ]
        .map(|key| format!("\"{key}\":"));
        let at: Vec<usize> = keys
            .iter()
            .map(|key| line.find(key.as_str()).unwrap())
            .collect();
        assert!(at.is_sorted(), "{line}");
        assert_eq!(row.as_object().unwrap().len(), 8, "{line}");
    }
    assert_eq!(rows[0]["session_id"], json!("s_main"));
    assert_eq!(rows[0]["name"], json!("retry work"));
    assert_eq!(rows[0]["seq"], json!(3u64));
    assert_eq!(rows[0]["ts"], json!(1_700_000_003_000u64));
    assert_eq!(rows[0]["snippet"], json!("grep -n \"retry budget\""));
    assert_eq!(
        rows[0]["log"],
        json!(single.past.join("events.jsonl").display().to_string())
    );
    assert_eq!(rows[0]["artifact"], Value::Null);
    assert_eq!(
        rows[2]["artifact"],
        json!(
            single
                .past
                .join("artifacts")
                .join("call_1.txt")
                .display()
                .to_string()
        )
    );
}

#[test]
fn json_keeps_control_characters_the_text_maps() {
    let root = fakes::TempDir::new("cli-sessions-search-controls");
    let home = root.path().join("h");
    let workspace = root.path().join("r");
    let common = root.path().join("r.git");
    let identity = identity(&workspace, &root.path().join("rw"), &common);
    let sessions = log::sessions_dir(&home, &common);
    session(
        &sessions,
        "s_ctl",
        &workspace,
        &[
            event(
                "session_named",
                json!({"name": "two\nlines\there", "by": "person"}),
            ),
            message("a\nb\tc retry budget"),
        ],
    );
    let mut text = Vec::new();
    let mut err = Vec::new();
    run(
        &home,
        &workspace,
        Arc::clone(&identity),
        true,
        QUERY,
        false,
        false,
        &mut text,
        &mut err,
    )
    .unwrap();
    let text = String::from_utf8(text).unwrap();
    assert_eq!(
        text,
        "1 of 1 hits for \"retry budget\", best first.\n\
         1. message, session s_ctl \"two lines here\", seq 2, 2023-11-14T22:13:22Z\n\
         \x20  a b c retry budget\n"
    );
    let mut out = Vec::new();
    let mut err = Vec::new();
    run(
        &home, &workspace, identity, true, QUERY, false, true, &mut out, &mut err,
    )
    .unwrap();
    let row: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(row["name"], json!("two\nlines\there"));
    assert_eq!(row["snippet"], json!("a\nb\tc retry budget"));
}

#[test]
fn control_characters_in_paths_stay_on_their_lines() {
    let root = fakes::TempDir::new("cli-sessions-search-paths");
    let home = root.path().join("ho\nme");
    let workspace = root.path().join("r");
    let common = root.path().join("r.git");
    let identity = identity(&workspace, &root.path().join("rw"), &common);
    let sessions = log::sessions_dir(&home, &common);
    let dir = session(
        &sessions,
        "s_\nctl\u{1b}",
        &workspace,
        &[message("retry budget")],
    );
    let (completed, bytes) = tool_output();
    {
        let clock = FakeClock::new();
        let clock_dyn: Arc<dyn contract::clock::Clock> = clock.clone();
        let log =
            log::Log::open(&sessions, SessionId("s_\nctl\u{1b}".to_owned()), clock_dyn).unwrap();
        clock.advance(Duration::from_secs(2));
        log.append(&completed, None, None).unwrap();
    }
    artifact(&dir, &bytes);
    let mut out = Vec::new();
    let mut err = Vec::new();
    run(
        &home, &workspace, identity, true, QUERY, false, false, &mut out, &mut err,
    )
    .unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(!out.contains('\u{1b}'), "{out:?}");
    // Every block is exactly its lines: the header, then a two-line
    // message block and a three-line artifact block.
    assert_eq!(out.lines().count(), 1 + 2 + 3, "{out:?}");
    assert!(out.contains("session s_ ctl  "), "{out:?}");
}

fn linked_home(links: usize) -> (fakes::TempDir, PathBuf) {
    let root = fakes::TempDir::new("cli-sessions-search-links");
    let home = root.path().join("h");
    let workspace = root.path().join("r");
    let sessions = log::sessions_dir(&home, &workspace);
    fs::create_dir_all(&sessions).unwrap();
    for n in 0..links {
        symlink(
            Path::new("/elsewhere"),
            sessions.join(format!("s_link_{n:02}")),
        )
        .unwrap();
    }
    (root, home)
}

#[test]
fn links_are_problems_on_stderr_and_never_change_the_exit() {
    let (_root, home) = linked_home(1);
    let outside = _root.path().join("o");
    let identity: log::Identity = Arc::new(move |path: &Path| path.to_path_buf());
    let mut out = Vec::new();
    let mut err = Vec::new();
    run(
        &home, &outside, identity, false, QUERY, false, false, &mut out, &mut err,
    )
    .unwrap();
    // Outside a repository every project is searched, so the link is met.
    let err = String::from_utf8(err).unwrap();
    assert_eq!(err.lines().count(), 1, "{err:?}");
    assert!(err.contains("is a link"), "{err:?}");
    assert!(
        String::from_utf8(out).unwrap().starts_with("No hits for "),
        "{err:?}"
    );
}

#[test]
fn twenty_problems_list_all_and_twenty_one_counts_the_rest() {
    for (links, tail) in [(20, None), (21, Some("And 1 more problems."))] {
        let (_root, home) = linked_home(links);
        let outside = _root.path().join("o");
        let identity: log::Identity = Arc::new(move |path: &Path| path.to_path_buf());
        let mut out = Vec::new();
        let mut err = Vec::new();
        run(
            &home, &outside, identity, false, QUERY, false, false, &mut out, &mut err,
        )
        .unwrap();
        let err = String::from_utf8(err).unwrap();
        assert_eq!(err.lines().count(), links.min(20) + tail.is_some() as usize);
        match tail {
            Some(last) => assert!(err.ends_with(&format!("{last}\n"))),
            None => assert!(!err.contains("more problems")),
        }
    }
}

/// A writer that always fails.
struct FailWrite;

impl io::Write for FailWrite {
    fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("closed"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_failed_stdout_write_is_io_failed() {
    let fixtures = Fixtures::write();
    let mut err = Vec::new();
    let failure = run(
        &fixtures.home,
        &fixtures.main,
        Arc::clone(&fixtures.identity),
        true,
        QUERY,
        false,
        false,
        &mut FailWrite,
        &mut err,
    )
    .unwrap_err();
    assert_eq!(failure.code, ErrorCode::IoFailed);
    assert!(
        failure.message.starts_with("standard output:"),
        "{}",
        failure.message
    );
}

#[test]
fn utc_prints_seconds_since_the_epoch_as_a_utc_time() {
    for (seconds, printed) in [
        (0, "1970-01-01T00:00:00Z"),
        (951_868_799, "2000-02-29T23:59:59Z"),
        (946_684_799, "1999-12-31T23:59:59Z"),
        (946_684_800, "2000-01-01T00:00:00Z"),
        (1_791_367_203, "2026-10-07T10:00:03Z"),
        (4_107_542_400, "2100-03-01T00:00:00Z"),
    ] {
        assert_eq!(utc(seconds), printed, "{seconds}");
    }
}

/// The child runs `search` and exits with its code, so the parent can read
/// the exit code while the search's own stdout goes nowhere.
const CHILD: &str = "FIBER_CLI_SESSIONS_SEARCH_CHILD";

#[test]
fn search_exits_zero_on_success_and_usage_on_a_bad_home() {
    if std::env::var(CHILD).is_ok() {
        std::process::exit(super::search(QUERY, false, false));
    }
    let root = fakes::TempDir::new("cli-sessions-search-child");
    let home = root.path().join("h");
    fs::create_dir_all(&home).unwrap();
    let workspace = root.path().join("w");
    fs::create_dir_all(&workspace).unwrap();
    let workspace = fs::canonicalize(&workspace).unwrap();
    let name = module_path!().split_once("::").unwrap().1;
    for (case, home_value, code) in [
        ("success", home.to_str().unwrap().to_owned(), 0),
        ("bad-home", "relative/home".to_owned(), 2),
        ("empty-home", String::new(), 2),
    ] {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!("{name}::search_exits_zero_on_success_and_usage_on_a_bad_home"),
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD, case)
            .env("FIBER_HOME", &home_value)
            .current_dir(&workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let group = child.id();
        let watchdog = fakes::Watchdog::group(group);
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || tx.send(child.wait()));
        let Ok(status) = rx.recv_timeout(CHILD_DEADLINE) else {
            assert!(fakes::kill_group(group, "KILL").unwrap());
            rx.recv_timeout(REAP_DEADLINE)
                .expect("the killed search child must be reaped")
                .unwrap();
            panic!("waited {CHILD_DEADLINE:?} for `fiber sessions search` ({case}) to exit");
        };
        let status = status.unwrap();
        assert!(!fakes::kill_group(group, "0").unwrap(), "a child remains");
        watchdog.stand_down(REAP_DEADLINE);
        assert_eq!(status.code(), Some(code), "{case}: {status}");
    }
}
