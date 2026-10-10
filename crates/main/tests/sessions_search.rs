//! Binary-level tests of `fiber sessions search` (`docs/invocation.md`,
//! "Commands and flags"; `docs/testing.md`, "Levels"): the built `fiber`
//! searches fixture sessions in its own process group with its own
//! `FIBER_HOME`, from a git repository with a linked worktree and from a
//! plain directory.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::time::Duration;

use contract::events::Event;
use contract::session_search::{LIMIT, Query, Scan};
use contract::{Envelope, SessionId};
use fakes::clock::FakeClock;
use serde_json::{Value, json};
use support::{Deadline, QUERY, Setup, run_to_exit};

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

/// Runs `git` with `args` in `dir`, waiting under the test's deadline.
fn git(deadline: Deadline, dir: &Path, args: &[&str]) {
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let output = run_to_exit(deadline, &format!("git {}", args.join(" ")), command);
    assert_eq!(
        output.status.code(),
        Some(0),
        "git {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The fixture layout: a repository `r` with a linked worktree `rw`, and a
/// plain directory `o`. `s_main` and `s_wt` are the repository's project,
/// recorded in `r` and `rw`; `s_out` is another project, recorded in `o`.
struct Layout {
    home: PathBuf,
    r: PathBuf,
    rw: PathBuf,
    o: PathBuf,
}

impl Layout {
    fn write(setup: &Setup) -> Self {
        let root = setup.root.path();
        let r = root.join("r");
        fs::create_dir_all(&r).unwrap();
        git(setup.deadline, &r, &["init", "-q"]);
        git(
            setup.deadline,
            &r,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "--allow-empty",
                "-q",
                "-m",
                "x",
            ],
        );
        git(
            setup.deadline,
            root,
            &["-C", "r", "worktree", "add", "-q", "../rw"],
        );
        let o = root.join("o");
        fs::create_dir_all(&o).unwrap();
        // Symlinks resolved, as the command sees the workspace: the launch
        // directory's project is git's common dir, canonicalized.
        let r = fs::canonicalize(&r).unwrap();
        let rw = fs::canonicalize(root.join("rw")).unwrap();
        let o = fs::canonicalize(&o).unwrap();
        let home = setup.home();
        let sessions = log::sessions_dir(&home, &r.join(".git"));
        let past = session(
            &sessions,
            "s_main",
            &r,
            &[
                event(
                    "session_named",
                    json!({"name": "retry work", "by": "person"}),
                ),
                event(
                    "text_completed",
                    json!({"text": "We keep the Retry Budget at 3"}),
                ),
                event(
                    "tool_call_requested",
                    json!({"name": "shell", "arguments": {"command": "grep -n \"retry budget\""}}),
                ),
                event(
                    "tool_call_completed",
                    json!({"status": "completed", "content": [{"type": "text", "text": "retry budget (cut)"}], "artifact": "artifacts/call_1.txt"}),
                ),
            ],
        );
        fs::write(
            past.join("artifacts").join("call_1.txt"),
            "full output: RETRY BUDGET exhausted\n",
        )
        .unwrap();
        session(
            &sessions,
            "s_wt",
            &rw,
            &[event("text_completed", json!({"text": "wt retry budget"}))],
        );
        session(
            &log::sessions_dir(&home, &o),
            "s_out",
            &o,
            &[event("text_completed", json!({"text": "out retry budget"}))],
        );
        Self { home, r, rw, o }
    }
}

/// Runs `fiber sessions search` with `args` in `dir`.
fn search_in(setup: &Setup, dir: &Path, args: &[&str]) -> Output {
    let mut argv = vec!["sessions", "search"];
    argv.extend(args);
    let mut command = setup.fiber(&argv);
    command.current_dir(dir);
    run_to_exit(setup.deadline, "fiber sessions search", command)
}

/// The session id of every JSON Lines hit, in order.
fn hit_ids(output: &Output) -> Vec<String> {
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
    String::from_utf8(output.stdout.clone())
        .unwrap()
        .lines()
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
    let setup = Setup::new();
    let layout = Layout::write(&setup);
    // From the worktree, the repository's project: both its sessions.
    assert_eq!(
        hit_ids(&search_in(&setup, &layout.rw, &["--json", QUERY])),
        ["s_main", "s_main", "s_wt", "s_main"]
    );
    // With `--all`, every project anywhere.
    assert_eq!(
        hit_ids(&search_in(&setup, &layout.r, &["--all", "--json", QUERY])),
        ["s_main", "s_main", "s_out", "s_wt", "s_main"]
    );
    // Outside a repository, every project.
    assert_eq!(
        hit_ids(&search_in(&setup, &layout.o, &["--json", QUERY])),
        ["s_main", "s_main", "s_out", "s_wt", "s_main"]
    );
}

#[test]
fn the_command_calls_the_shared_scan() {
    let setup = Setup::new();
    let layout = Layout::write(&setup);
    let output = search_in(&setup, &layout.r, &["--all", "--json", QUERY]);
    let lines: Vec<Value> = String::from_utf8(output.stdout.clone())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let scan = log::SessionScan::new(&layout.home, &layout.r, Arc::new(doors::project));
    let found = scan.scan(
        &Query {
            text: QUERY.to_owned(),
            all_projects: true,
            limit: LIMIT,
        },
        &fakes::CancelToken::new(),
    );
    let expected: Vec<Value> = found
        .hits
        .iter()
        .map(|hit| {
            json!({
                "session_id": hit.session_id.0,
                "name": hit.name,
                "seq": hit.seq.0,
                "ts": hit.ts,
                "label": match hit.label {
                    contract::session_search::Label::Message => "message",
                    contract::session_search::Label::ToolInput => "tool_input",
                    contract::session_search::Label::ToolOutput => "tool_output",
                },
                "snippet": hit.snippet,
                "log": hit.log.display().to_string(),
                "artifact": hit.artifact.as_ref().map(|path| path.display().to_string()),
            })
        })
        .collect();
    assert_eq!(lines, expected);
}

#[test]
fn text_prints_a_header_then_one_block_per_hit() {
    let setup = Setup::new();
    let layout = Layout::write(&setup);
    let output = search_in(&setup, &layout.r, &[QUERY]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
    let out = String::from_utf8(output.stdout).unwrap();
    let mut blocks = out.split("  ");
    assert_eq!(
        blocks.next().unwrap(),
        "4 of 4 hits for \"retry budget\", best first.\n1. tool_input, session s_main \"retry work\", seq 3, 2023-11-14T22:13:23Z\n",
    );
    assert_eq!(blocks.count(), 5);
    assert_eq!(out.lines().count(), 1 + 2 + 2 + 2 + 3);
}

#[test]
fn a_relative_fiber_home_is_a_usage_error_naming_it() {
    let setup = Setup::new();
    let layout = Layout::write(&setup);
    let mut command = setup.fiber(&["sessions", "search", QUERY]);
    command
        .current_dir(&layout.r)
        .env("FIBER_HOME", "relative/home");
    let output = run_to_exit(setup.deadline, "fiber sessions search", command);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(stderr.contains("FIBER_HOME"), "{stderr}");
}

#[test]
fn a_missing_text_is_a_usage_error_naming_it() {
    let setup = Setup::new();
    let layout = Layout::write(&setup);
    let output = search_in(&setup, &layout.r, &[]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(stderr.contains("<text>"), "{stderr}");
    let output = search_in(&setup, &layout.r, &[""]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "");
    assert_eq!(String::from_utf8(output.stderr).unwrap().lines().count(), 1);
}

#[test]
fn no_hits_exits_zero_and_json_prints_nothing() {
    let setup = Setup::new();
    let layout = Layout::write(&setup);
    let output = search_in(&setup, &layout.r, &["zzz no match"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .starts_with("No hits for \"zzz no match\"")
    );
    let output = search_in(&setup, &layout.r, &["--json", "zzz no match"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "");
    assert_eq!(String::from_utf8(output.stderr).unwrap(), "");
}

/// Every file under `dir`, by path, with its bytes.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        for entry in fs::read_dir(&next).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() && !path.is_symlink() {
                stack.push(path);
            } else {
                files.insert(path.clone(), fs::read(&path).unwrap());
            }
        }
    }
    files
}

#[test]
fn a_search_writes_nothing() {
    let setup = Setup::new();
    let layout = Layout::write(&setup);
    let before = snapshot(&layout.home);
    let output = search_in(&setup, &layout.r, &["--all", QUERY]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(snapshot(&layout.home), before);
}
