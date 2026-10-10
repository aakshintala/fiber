//! Searching past sessions through the public scan (`docs/tools.md`,
//! "Searching past sessions"), over a temporary Fiber home.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod common;

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::event;
use common::line;
use contract::ActionId;
use contract::SessionId;
use contract::session_search::{Label, Query, Scan};
use fakes::clock::FakeClock;
use log::{Identity, Log, SessionScan, project_key, sessions_dir};
use serde_json::json;

/// Workspaces under `/other` are another project; every other is `/proj`.
fn identity() -> Identity {
    Arc::new(|path: &Path| {
        if path.starts_with("/other") {
            PathBuf::from("/other")
        } else {
            PathBuf::from("/proj")
        }
    })
}

fn search(scan: &SessionScan, text: &str, all_projects: bool) -> contract::session_search::Found {
    let query = Query {
        text: text.into(),
        all_projects,
        limit: 20,
    };
    scan.scan(&query, &fakes::CancelToken::new())
}

#[test]
fn a_search_finds_a_message_an_input_and_an_artifact_of_its_own_project() {
    let home = fakes::TempDir::new("log-search-public");
    let clock = FakeClock::new();
    let sessions = sessions_dir(home.path(), Path::new("/proj"));
    let log = Log::create(&sessions, SessionId("s_past".into()), clock.clone()).unwrap();
    let started = |workspace: &str| {
        event(
            "session_started",
            json!({"workspace": workspace, "variables": {"path": "/bin", "names": [], "source": "inherited"}}),
        )
    };
    log.append(&started("/proj/sub"), None, None).unwrap();
    clock.advance(Duration::from_secs(1));
    log.append(
        &event(
            "session_named",
            json!({"name": "retry work", "by": "person"}),
        ),
        None,
        None,
    )
    .unwrap();
    clock.advance(Duration::from_secs(1));
    let prompt = json!({"input": [{"type": "message", "content": [{"type": "text", "text": "We keep the Retry Budget at 3"}], "source": "driver"}]});
    log.append(&event("turn_started", prompt), None, None)
        .unwrap();
    clock.advance(Duration::from_secs(1));
    let call = json!({"name": "shell", "arguments": {"command": "grep -n \"retry budget\""}});
    log.append(&event("tool_call_requested", call), None, None)
        .unwrap();
    clock.advance(Duration::from_secs(1));
    let dir = sessions.join("s_past");
    let artifact = dir.join("artifacts").join("call_1.txt");
    fs::write(&artifact, "full output: RETRY BUDGET exhausted\n").unwrap();
    let completed = json!({"status": "completed", "content": [{"type": "text", "text": "retry budget (cut)"}], "artifact": "artifacts/call_1.txt"});
    log.append(&event("tool_call_completed", completed), None, None)
        .unwrap();
    drop(log);

    // Same key, another project: skipped without `all_projects`.
    let other = Log::create(&sessions, SessionId("s_other".into()), clock.clone()).unwrap();
    other.append(&started("/other/w"), None, None).unwrap();
    clock.advance(Duration::from_secs(1));
    other
        .append(
            &event("text_completed", json!({"text": "retry budget"})),
            None,
            None,
        )
        .unwrap();
    drop(other);

    let scan = SessionScan::new(home.path(), Path::new("/proj"), identity());
    let found = search(&scan, "retry budget", false);
    let rows: Vec<(Label, u64, &str)> = found
        .hits
        .iter()
        .map(|hit| (hit.label, hit.seq.0, hit.snippet.as_str()))
        .collect();
    assert_eq!(
        rows,
        [
            (Label::ToolInput, 3, "grep -n \"retry budget\""),
            (Label::Message, 2, "We keep the Retry Budget at 3"),
            (Label::ToolOutput, 4, "full output: RETRY BUDGET exhausted"),
        ]
    );
    assert_eq!(found.total, 3);
    for hit in &found.hits {
        assert_eq!(hit.name, "retry work");
        assert_eq!(hit.log, dir.join("events.jsonl"));
    }
    assert_eq!(found.hits[2].artifact.as_deref(), Some(artifact.as_path()));
    assert!(found.problems.is_empty(), "{:?}", found.problems);

    let found = search(&scan, "retry budget", true);
    assert_eq!(found.total, 4);
    assert_eq!(found.hits[0].session_id, SessionId("s_other".into()));
    let key = project_key(Path::new("/proj"));
    assert_eq!(
        scan.scope(false),
        PathBuf::from(format!("{}/projects/{key}/", home.path().display()))
    );
    // The project key makes every slash a dash, and sessions live under
    // it in Fiber home: the deleted `writing` tests' expectations.
    assert_eq!(
        project_key(Path::new("/Users/alice/work/fiber/.git")),
        "-Users-alice-work-fiber-.git"
    );
    assert_eq!(project_key(Path::new("a/b//c/")), "a-b--c-");
    assert_eq!(
        sessions_dir(home.path(), Path::new("/proj")),
        home.path().join("projects/-proj/sessions")
    );
}

#[test]
fn a_search_never_returns_session_search_own_calls_or_results() {
    let home = fakes::TempDir::new("log-search-self");
    let clock = FakeClock::new();
    let sessions = sessions_dir(home.path(), Path::new("/proj"));
    let log = Log::create(&sessions, SessionId("s_mixed".into()), clock.clone()).unwrap();
    log.append(
        &event(
            "session_started",
            json!({"workspace": "/proj", "variables": {"path": "/bin", "names": [], "source": "inherited"}}),
        ),
        None,
        None,
    )
    .unwrap();
    clock.advance(Duration::from_secs(1));
    // The tool's own call: its arguments, its output and its artifact all
    // hold the query, and none gives a hit.
    log.append(
        &event(
            "tool_call_requested",
            json!({"name": "session_search", "arguments": {"text": "retry budget"}}),
        ),
        None,
        Some(ActionId("a_1".into())),
    )
    .unwrap();
    clock.advance(Duration::from_secs(1));
    let dir = sessions.join("s_mixed");
    fs::write(
        dir.join("artifacts").join("self_1.txt"),
        "full output: RETRY BUDGET exhausted\n",
    )
    .unwrap();
    log.append(
        &event(
            "tool_call_completed",
            json!({"status": "completed", "content": [{"type": "text", "text": "retry budget results"}], "artifact": "artifacts/self_1.txt"}),
        ),
        None,
        Some(ActionId("a_1".into())),
    )
    .unwrap();
    clock.advance(Duration::from_secs(1));
    // Another tool's call with the same text still hits.
    log.append(
        &event(
            "tool_call_requested",
            json!({"name": "shell", "arguments": {"command": "grep -n \"retry budget\""}}),
        ),
        None,
        Some(ActionId("a_2".into())),
    )
    .unwrap();
    clock.advance(Duration::from_secs(1));
    log.append(
        &event(
            "tool_call_completed",
            json!({"status": "completed", "content": [{"type": "text", "text": "found retry budget"}]}),
        ),
        None,
        Some(ActionId("a_2".into())),
    )
    .unwrap();
    drop(log);
    let scan = SessionScan::new(home.path(), Path::new("/proj"), identity());
    let found = search(&scan, "retry budget", false);
    let rows: Vec<(Label, u64, &str)> = found
        .hits
        .iter()
        .map(|hit| (hit.label, hit.seq.0, hit.snippet.as_str()))
        .collect();
    assert_eq!(
        rows,
        [
            (Label::ToolInput, 3, "grep -n \"retry budget\""),
            (Label::ToolOutput, 4, "found retry budget"),
        ]
    );
    assert_eq!(found.total, 2);
    assert!(found.hits.iter().all(|hit| hit.artifact.is_none()));
    assert!(found.problems.is_empty(), "{:?}", found.problems);
}

/// Session `id` under project key `key`, started in `workspace`, then
/// `lines` appended raw. Its directory.
fn session(home: &Path, key: &str, id: &str, workspace: &str, lines: &[String]) -> PathBuf {
    let sessions = home.join("projects").join(key).join("sessions");
    let log = Log::create(&sessions, SessionId(id.into()), FakeClock::new()).unwrap();
    log.append(
        &event(
            "session_started",
            json!({"workspace": workspace, "variables": {"path": "/bin", "names": [], "source": "inherited"}}),
        ),
        None,
        None,
    )
    .unwrap();
    drop(log);
    let dir = sessions.join(id);
    let mut log = fs::read_to_string(dir.join("events.jsonl")).unwrap();
    log.extend(lines.iter().map(String::as_str));
    fs::write(dir.join("events.jsonl"), log).unwrap();
    dir
}

/// A line that holds the query and is not JSON.
const BAD: &str = "needle not json\n";

fn text(id: &str, ts: u64, seq: u64, text: &str) -> String {
    line("text_completed", id, ts, seq, &json!({"text": text}))
}

fn named(id: &str, seq: u64, name: &str) -> String {
    line(
        "session_named",
        id,
        1,
        seq,
        &json!({"name": name, "by": "person"}),
    )
}

/// The expected hit of the session in `dir`.
fn want(
    dir: &Path,
    name: &str,
    (seq, ts): (u64, u64),
    label: Label,
    snippet: &str,
) -> contract::session_search::Hit {
    contract::session_search::Hit {
        session_id: SessionId(dir.file_name().unwrap().to_string_lossy().into_owned()),
        name: name.into(),
        seq: contract::Seq(seq),
        ts,
        label,
        snippet: snippet.into(),
        log: dir.join("events.jsonl"),
        artifact: None,
    }
}

/// The problems of a session's [`BAD`] lines, `lines` numbered in its log.
fn bad(dir: &Path, lines: std::ops::RangeInclusive<u64>) -> Vec<String> {
    lines
        .map(|n| {
            format!(
                "Could not read: {}, line {n}: expected ident at line 1 column 2",
                dir.join("events.jsonl").display()
            )
        })
        .collect()
}

#[test]
fn a_search_over_many_projects_gives_the_one_pass_answer() {
    let home = fakes::TempDir::new("log-search-many");
    let home = home.path();
    let projects = home.join("projects");
    let bads = vec![BAD.to_owned(); 8];
    let with_bads = |first: String| [vec![first], bads.clone()].concat();
    // `-alpha`: a named session, one with eight bad lines, one of another
    // project; `s_a2`, `s_a3` and `s_p1` hit at the same ts.
    let a1 = session(
        home,
        "-alpha",
        "s_a1",
        "/proj/x",
        &[
            text("s_a1", 1000, 1, "needle one"),
            named("s_a1", 2, "alpha one"),
        ],
    );
    let a2 = session(
        home,
        "-alpha",
        "s_a2",
        "/proj/x",
        &with_bads(text("s_a2", 2000, 1, "needle two")),
    );
    let a3 = session(
        home,
        "-alpha",
        "s_a3",
        "/other/y",
        &[line(
            "tool_call_requested",
            "s_a3",
            2000,
            1,
            &json!({"name": "shell", "arguments": {"command": "grep needle"}}),
        )],
    );
    // `-beta` is a link.
    symlink(projects.join("-alpha"), projects.join("-beta")).unwrap();
    // `-gamma`: a matched artifact and eight bad lines, a linked session
    // directory, a session named by its first prompt, one with no hit.
    let completed = json!({"status": "completed", "content": [{"type": "text", "text": "cut"}], "artifact": "artifacts/out.txt"});
    let g1 = session(
        home,
        "-gamma",
        "s_g1",
        "/proj/x",
        &with_bads(line("tool_call_completed", "s_g1", 3000, 1, &completed)),
    );
    let artifact = g1.join("artifacts").join("out.txt");
    fs::write(&artifact, "needle in artifact\n").unwrap();
    let gamma = projects.join("-gamma").join("sessions");
    symlink(&a1, gamma.join("s_g2")).unwrap();
    let prompt = json!({"input": [{"type": "message", "content": [{"type": "text", "text": "find the needle"}], "source": "driver"}]});
    let g3 = session(
        home,
        "-gamma",
        "s_g3",
        "/proj/x",
        &[line("turn_started", "s_g3", 1500, 1, &prompt)],
    );
    session(
        home,
        "-gamma",
        "s_g4",
        "/proj/x",
        &[text("s_g4", 1500, 1, "nothing here")],
    );
    // `-proj`, the own key: a named session with two hits, one with eight
    // bad lines, a linked log, another project's session, and an input with
    // its output.
    let p1 = session(
        home,
        "-proj",
        "s_p1",
        "/proj/main",
        &[
            text("s_p1", 2000, 1, "needle p1"),
            text("s_p1", 2000, 2, "needle p1 again"),
            named("s_p1", 3, "own one"),
        ],
    );
    let p2 = session(
        home,
        "-proj",
        "s_p2",
        "/proj/w",
        &with_bads(text("s_p2", 500, 1, "needle p2")),
    );
    let p3 = session(home, "-proj", "s_p3", "/proj/main", &[]);
    fs::remove_file(p3.join("events.jsonl")).unwrap();
    symlink(p1.join("events.jsonl"), p3.join("events.jsonl")).unwrap();
    let p4 = session(
        home,
        "-proj",
        "s_p4",
        "/other/z",
        &[text("s_p4", 4000, 1, "needle other")],
    );
    let p5 = session(
        home,
        "-proj",
        "s_p5",
        "/proj/main",
        &[
            line(
                "tool_call_requested",
                "s_p5",
                3000,
                1,
                &json!({"name": "shell", "arguments": {"command": "needle input"}}),
            ),
            line(
                "tool_call_completed",
                "s_p5",
                3000,
                2,
                &json!({"status": "completed", "content": [{"type": "text", "text": "needle output"}]}),
            ),
        ],
    );

    let link = |path: PathBuf| format!("{} is a link", path.display());
    let mut g1_output = want(&g1, "", (1, 3000), Label::ToolOutput, "needle in artifact");
    g1_output.artifact = Some(artifact);
    let all_hits = vec![
        want(&p4, "", (1, 4000), Label::Message, "needle other"),
        want(&p5, "", (1, 3000), Label::ToolInput, "needle input"),
        want(&a2, "", (1, 2000), Label::Message, "needle two"),
        want(&a3, "", (1, 2000), Label::ToolInput, "grep needle"),
        want(&p1, "own one", (2, 2000), Label::Message, "needle p1 again"),
        want(&p1, "own one", (1, 2000), Label::Message, "needle p1"),
        want(
            &g3,
            "find the needle",
            (1, 1500),
            Label::Message,
            "find the needle",
        ),
        want(&a1, "alpha one", (1, 1000), Label::Message, "needle one"),
        want(&p2, "", (1, 500), Label::Message, "needle p2"),
        g1_output,
        want(&p5, "", (2, 3000), Label::ToolOutput, "needle output"),
    ];
    let all_problems = [
        bad(&a2, 3..=10),
        vec![link(projects.join("-beta"))],
        bad(&g1, 3..=10),
        vec![link(gamma.join("s_g2"))],
        bad(&p2, 3..=4),
    ]
    .concat();
    let own_hits: Vec<_> = all_hits
        .iter()
        .filter(|hit| ["s_p1", "s_p2", "s_p5"].contains(&hit.session_id.0.as_str()))
        .cloned()
        .collect();
    let own_problems = [bad(&p2, 3..=10), vec![link(p3.join("events.jsonl"))]].concat();

    let scan = SessionScan::new(home, Path::new("/proj/main"), identity());
    for limit in [0, 3, 1000] {
        let query = |all_projects| Query {
            text: "needle".into(),
            all_projects,
            limit,
        };
        let found = scan.scan(&query(true), &fakes::CancelToken::new());
        let want_all = contract::session_search::Found {
            hits: all_hits.iter().take(limit).cloned().collect(),
            total: 11,
            problems: all_problems.clone(),
            more_problems: 7,
        };
        assert_eq!(found, want_all, "all projects, limit {limit}");
        let found = scan.scan(&query(false), &fakes::CancelToken::new());
        let want_own = contract::session_search::Found {
            hits: own_hits.iter().take(limit).cloned().collect(),
            total: 5,
            problems: own_problems.clone(),
            more_problems: 0,
        };
        assert_eq!(found, want_own, "own project, limit {limit}");
    }
}
