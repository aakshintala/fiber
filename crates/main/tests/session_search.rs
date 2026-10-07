//! Binary-level tests of the `session_search` tool (`docs/tools.md`,
//! "Searching past sessions"; `docs/testing.md`, "Levels"): the built
//! `fiber` runs `fiber ask` in its own process group with its own
//! `FIBER_HOME`, holding fixture sessions and an ordinary provider whose
//! base URL is the fake server.

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
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use contract::events::Event;
use contract::{Envelope, SessionId};
use fakes::ProviderServer;
use fakes::clock::FakeClock;
use serde_json::{Value, json};
use support::{Setup, hello, run_to_exit, stream};

/// The query every test searches for.
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

/// The fixture Fiber home: `s_past` in the workspace's project, `s_other`
/// under the same key from a colliding workspace that is another project,
/// and `s_far` under another key.
struct Fixtures {
    /// The workspace's canonical path, its project identity outside a
    /// repository.
    workspace: PathBuf,
    /// `projects/<key>/sessions/` of the workspace.
    sessions: PathBuf,
    past: PathBuf,
    other: PathBuf,
    far: PathBuf,
}

impl Fixtures {
    fn write(setup: &Setup) -> Self {
        let workspace = fs::canonicalize(setup.root.path()).unwrap();
        let sessions = log::sessions_dir(&setup.home(), &workspace);
        let past = session(
            &sessions,
            "s_past",
            &workspace,
            &[
                event(
                    "session_named",
                    json!({"name": "retry work", "by": "person"}),
                ),
                event(
                    "turn_started",
                    json!({"input": [{"type": "message", "content": [{"type": "text", "text": "We keep the Retry Budget at 3"}], "source": "driver"}]}),
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
        // `<parent>-<root>` beside the root's parent has the root's key
        // and is another project.
        let parent = workspace.parent().unwrap();
        let colliding = parent.parent().unwrap().join(format!(
            "{}-{}",
            parent.file_name().unwrap().to_string_lossy(),
            workspace.file_name().unwrap().to_string_lossy()
        ));
        assert_eq!(log::project_key(&colliding), log::project_key(&workspace));
        let other = session(
            &sessions,
            "s_other",
            &colliding,
            &[event(
                "text_completed",
                json!({"text": "other retry budget"}),
            )],
        );
        let far_workspace = Path::new("/elsewhere/far");
        let far = session(
            &log::sessions_dir(&setup.home(), far_workspace),
            "s_far",
            far_workspace,
            &[event("text_completed", json!({"text": "far retry budget"}))],
        );
        Self {
            workspace,
            sessions,
            past,
            other,
            far,
        }
    }

    /// The scope the tool declares for its own project:
    /// `<home>/projects/<key>/`.
    fn own_scope(&self) -> String {
        format!("{}/", self.sessions.parent().unwrap().display())
    }
}

/// Every file under `dir`, by path, with its bytes.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        for entry in fs::read_dir(&next).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                files.insert(path.clone(), fs::read(&path).unwrap());
            }
        }
    }
    files
}

/// A finished `function_call` for `session_search` with `arguments`.
fn search_call(arguments: &Value) -> Value {
    json!({"type": "response.output_item.done", "item": {
        "type": "function_call",
        "id": "fc_call_search",
        "call_id": "call_search",
        "name": "session_search",
        "arguments": arguments.to_string()
    }})
}

/// Runs `fiber ask` with a model that calls `session_search` with
/// `arguments`, then answers `Hello.`, and returns stdout's lines.
fn ask(setup: &Setup, arguments: &Value) -> Vec<Value> {
    ask_with(setup, "look", arguments)
}

/// [`ask`] with `prompt` as the person's message.
fn ask_with(setup: &Setup, prompt: &str, arguments: &Value) -> Vec<Value> {
    let server = ProviderServer::start([stream(&[search_call(arguments)]), hello()]).unwrap();
    setup.provider(&server);
    ask_configured(setup, prompt)
}

/// `fiber ask` with `prompt`, the provider and config already in place.
fn ask_configured(setup: &Setup, prompt: &str) -> Vec<Value> {
    let output = run_to_exit(setup.deadline, "fiber ask", setup.fiber(&["ask", prompt]));
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or(Value::Null))
        .collect()
}

/// The one line of `kind`.
fn line<'a>(lines: &'a [Value], kind: &str) -> &'a Value {
    let mut found = lines.iter().filter(|line| line["kind"] == kind);
    let first = found.next().unwrap_or_else(|| panic!("no {kind} line"));
    assert!(found.next().is_none(), "more than one {kind} line");
    first
}

/// The result text of the call.
fn result(lines: &[Value]) -> String {
    line(lines, "tool_call_completed")["payload"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// The expected block of `s_past`'s three hits, numbered from `first`.
fn past_hits(past: &Path, first: usize) -> String {
    let log = past.join("events.jsonl");
    let log = log.display();
    let artifact = past.join("artifacts").join("call_1.txt");
    format!(
        "{a}. tool_input, session s_past \"retry work\", seq 3, 2023-11-14T22:13:23Z\n\
         \x20  read {log} from offset 4\n\
         \x20  grep -n \"retry budget\"\n\
         {b}. message, session s_past \"retry work\", seq 2, 2023-11-14T22:13:22Z\n\
         \x20  read {log} from offset 3\n\
         \x20  We keep the Retry Budget at 3\n\
         {c}. tool_output, session s_past \"retry work\", seq 4, 2023-11-14T22:13:24Z\n\
         \x20  read {log} from offset 5\n\
         \x20  artifact {artifact}\n\
         \x20  full output: RETRY BUDGET exhausted\n",
        a = first,
        b = first + 1,
        c = first + 2,
        artifact = artifact.display(),
    )
}

#[test]
fn a_search_finds_this_projects_past_session_and_never_its_own_call() {
    let setup = Setup::new();
    let fixtures = Fixtures::write(&setup);
    let before = snapshot(&setup.home().join("projects"));

    let lines = ask_with(&setup, "find the retry budget", &json!({"text": QUERY}));

    // The running session's prompt is a hit, the newest; its own call
    // holds the query too and gives none.
    let shown = result(&lines);
    let running = lines[0]["session_id"].as_str().unwrap();
    let prompt = line(&lines, "turn_started");
    let seq = prompt["seq"].as_u64().unwrap();
    let ts = prompt["ts"].as_u64().unwrap();
    assert!(ts > 1_700_000_004_000, "the run is newer than the fixtures");
    // The run's time is the real clock's; the fixtures' are fixed.
    let heading = format!("1. message, session {running} \"find the retry budget\", seq {seq}, ");
    let at = shown.find(&heading).unwrap() + heading.len();
    let time = &shown[at..at + 20];
    assert!(time.starts_with("20") && time.ends_with('Z'), "{shown}");
    assert_eq!(
        shown,
        format!(
            "4 of 4 hits for \"retry budget\", best first.\n\
             {heading}{time}\n\
             \x20  read {log} from offset {offset}\n\
             \x20  find the retry budget\n\
             {}",
            past_hits(&fixtures.past, 2),
            log = fixtures
                .sessions
                .join(running)
                .join("events.jsonl")
                .display(),
            offset = seq + 1,
        )
    );
    assert!(!shown.contains("s_other"), "{shown}");
    assert!(!shown.contains("s_far"), "{shown}");
    let declared = &line(&lines, "tool_call_started")["payload"];
    assert_eq!(declared["effects"], json!(["reads"]));
    assert_eq!(declared["paths"], json!([fixtures.own_scope()]));
    assert!(
        lines
            .iter()
            .all(|line| line["kind"] != "permission_requested"),
        "a session search is never reviewed"
    );
    // Nothing in the fixture sessions changed: the search writes nothing.
    let mut after = snapshot(&setup.home().join("projects"));
    after.retain(|path, _| !path.starts_with(fixtures.sessions.join(running)));
    assert_eq!(after, before);
    assert!(fixtures.workspace.is_dir());
}

#[test]
fn every_project_is_searched_with_all_projects() {
    let setup = Setup::new();
    let fixtures = Fixtures::write(&setup);

    let lines = ask(&setup, &json!({"text": QUERY, "all_projects": true}));

    let shown = result(&lines);
    let past = past_hits(&fixtures.past, 1);
    let (class_0, class_1) = past.split_at(past.find("3. tool_output").unwrap());
    // Messages and inputs first, newer first, then by session id at equal
    // times; tool outputs last.
    assert_eq!(
        shown,
        format!(
            "5 of 5 hits for \"retry budget\", best first.\n\
             {class_0}\
             3. message, session s_far \"\", seq 1, 2023-11-14T22:13:21Z\n\
             \x20  read {far} from offset 2\n\
             \x20  far retry budget\n\
             4. message, session s_other \"\", seq 1, 2023-11-14T22:13:21Z\n\
             \x20  read {other} from offset 2\n\
             \x20  other retry budget\n\
             {}",
            class_1.replacen("3. ", "5. ", 1),
            far = fixtures.far.join("events.jsonl").display(),
            other = fixtures.other.join("events.jsonl").display(),
        )
    );
    let declared = &line(&lines, "tool_call_started")["payload"];
    assert_eq!(declared["effects"], json!(["reads"]));
    assert_eq!(
        declared["paths"],
        json!([format!("{}/", setup.home().join("projects").display())])
    );
}

#[test]
fn a_link_below_projects_is_listed_and_never_followed() {
    let setup = Setup::new();
    let fixtures = Fixtures::write(&setup);
    let outside = setup.root.path().join("outside");
    let projects = setup.home().join("projects");
    // A project directory that links outside Fiber home.
    session(
        &outside.join("proj").join("sessions"),
        "s_linked_project",
        Path::new("/linked"),
        &[event("text_completed", json!({"text": "retry budget"}))],
    );
    let linked_project = projects.join("-linked");
    symlink(outside.join("proj"), &linked_project).unwrap();
    // A session whose log links to a matching log outside.
    let outside_log = session(
        &outside.join("logs"),
        "s_outside",
        Path::new("/elsewhere/far"),
        &[event("text_completed", json!({"text": "retry budget"}))],
    );
    let far_sessions = fixtures.far.parent().unwrap();
    let linked_log = far_sessions.join("s_linked_log");
    fs::create_dir(&linked_log).unwrap();
    symlink(
        outside_log.join("events.jsonl"),
        linked_log.join("events.jsonl"),
    )
    .unwrap();
    // A session whose `artifacts/` links to a directory with a matching
    // file its log names.
    let linked_artifacts = session(
        far_sessions,
        "s_linked_artifacts",
        Path::new("/elsewhere/far"),
        &[event(
            "tool_call_completed",
            json!({"status": "completed", "content": [{"type": "text", "text": "cut"}], "artifact": "artifacts/x.txt"}),
        )],
    );
    fs::create_dir_all(outside.join("art")).unwrap();
    fs::write(outside.join("art").join("x.txt"), "retry budget\n").unwrap();
    fs::remove_dir(linked_artifacts.join("artifacts")).unwrap();
    symlink(outside.join("art"), linked_artifacts.join("artifacts")).unwrap();

    let lines = ask(&setup, &json!({"text": QUERY, "all_projects": true}));

    let shown = result(&lines);
    assert!(
        shown.starts_with("5 of 5 hits for \"retry budget\", best first.\n"),
        "{shown}"
    );
    for id in [
        "s_linked_project",
        "s_outside",
        "s_linked_log",
        "s_linked_artifacts",
    ] {
        assert!(!shown.contains(&format!("session {id} ")), "{id}: {shown}");
    }
    for link in [
        linked_project,
        linked_log.join("events.jsonl"),
        linked_artifacts.join("artifacts"),
    ] {
        assert!(
            shown.contains(&format!("\n{} is a link\n", link.display())),
            "{}: {shown}",
            link.display()
        );
    }
}

#[test]
fn the_limit_caps_the_hits_and_the_result_is_cut_like_any_other() {
    let setup = Setup::new();
    let fixtures = Fixtures::write(&setup);

    let server = ProviderServer::start([
        stream(&[search_call(&json!({"text": QUERY, "limit": 1}))]),
        hello(),
        stream(&[search_call(&json!({"text": QUERY}))]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);

    let lines = ask_configured(&setup, "look");

    let shown = result(&lines);
    let log = fixtures.past.join("events.jsonl");
    assert_eq!(
        shown,
        format!(
            "1 of 3 hits for \"retry budget\", best first.\n\
             1. tool_input, session s_past \"retry work\", seq 3, 2023-11-14T22:13:23Z\n\
             \x20  read {} from offset 4\n\
             \x20  grep -n \"retry budget\"\n",
            log.display()
        )
    );

    // The first run's own call and result are never hits.
    fs::write(
        setup.home().join("config.json"),
        json!({"model": "fake/m", "tools": {"session_search": {"max_result_bytes": 300}}})
            .to_string(),
    )
    .unwrap();
    let lines = ask_configured(&setup, "look");

    let completed = line(&lines, "tool_call_completed");
    let artifact = completed["payload"]["artifact"].as_str().unwrap();
    assert!(artifact.starts_with("artifacts/"), "{artifact}");
    assert!(
        result(&lines).contains("bytes cut. The full output is in"),
        "{}",
        result(&lines)
    );
}
