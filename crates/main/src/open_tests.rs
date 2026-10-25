//! Tests for `fiber resume` and `fiber continue`: the tty refusal, the
//! latest-session pick, and the memoized project test.

use std::cell::RefCell;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::{
    hub_entries, in_project, listed_latest, notice_lines, route, select_latest, target_in,
    tty_refusal,
};
use contract::SessionId;

/// The refusal sentence, as `fiber` without a tty prints it.
const SENTENCE: &str =
    "The terminal needs a tty; run `fiber ask \"<prompt>\"`. Run `fiber --help` for usage.";

#[test]
fn the_terminal_refuses_unless_both_standard_input_and_output_are_ttys() {
    assert_eq!(tty_refusal(true, true), None);
    assert_eq!(tty_refusal(false, false), Some(SENTENCE));
    assert_eq!(tty_refusal(false, true), Some(SENTENCE));
    assert_eq!(tty_refusal(true, false), Some(SENTENCE));
}

/// Session `id` in `sessions` with a first line at `first` and a last
/// line at `last`.
fn logged(sessions: &Path, id: &str, first: u64, last: u64) {
    let dir = sessions.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let started = serde_json::json!({"kind": "session_started", "session_id": id,
        "ts": first, "schema_version": 1,
        "payload": {"workspace": "/w",
            "variables": {"path": "/usr/bin", "names": [], "source": "inherited"}}});
    let last = serde_json::json!({"kind": "fiber_exited", "session_id": id,
        "ts": last, "schema_version": 1, "payload": {}});
    std::fs::write(dir.join("events.jsonl"), format!("{started}\n{last}\n")).unwrap();
}

#[test]
fn select_latest_returns_the_session_with_the_newest_last_line() {
    let root = fakes::TempDir::new("fiber-open");
    let sessions = root.path().join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    logged(&sessions, "s_old", 200, 200);
    logged(&sessions, "s_new", 100, 300);

    assert_eq!(select_latest(&sessions, &|_| true).unwrap().0, "s_new");
}

#[test]
fn select_latest_without_a_session_is_a_usage_error_naming_fiber() {
    let root = fakes::TempDir::new("fiber-open-empty");
    let sessions = root.path().join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();

    let Err(e) = select_latest(&sessions, &|_| true) else {
        panic!("expected no session");
    };
    assert_eq!(e.code, contract::ErrorCode::Usage);
    assert_eq!(
        e.message,
        "No session in this project to continue; run `fiber` to start one. Run `fiber --help` for usage."
    );
}

#[test]
fn in_project_runs_the_identity_once_per_distinct_workspace() {
    let project = Path::new("/a/b");
    let calls = RefCell::new(Vec::new());
    let identity = |path: &Path| {
        calls.borrow_mut().push(path.to_owned());
        if path == Path::new("/w") {
            PathBuf::from("/a/b")
        } else {
            path.to_owned()
        }
    };
    let accept = in_project(project, &identity);

    assert!(accept("/w"));
    assert!(accept("/w"));
    assert!(!accept("/x"));
    assert!(!accept("/x"));
    assert!(accept("/w"));
    assert_eq!(
        *calls.borrow(),
        vec![PathBuf::from("/w"), PathBuf::from("/x")]
    );
}

/// A Fiber home whose global `config.json` sets `hub.default` and the
/// `hubs` entries `hubs` names, beside a launch directory: the shape
/// `target_in` reads, without process state.
fn hub_home(default: Option<&str>, hubs: &[(&str, &str)]) -> (fakes::TempDir, PathBuf) {
    let dir = fakes::TempDir::new("fiber-open-target");
    let mut global = serde_json::Map::new();
    if let Some(default) = default {
        global.insert("hub".to_owned(), serde_json::json!({"default": default}));
    }
    if !hubs.is_empty() {
        let entries: serde_json::Map<String, serde_json::Value> = hubs
            .iter()
            .map(|(name, address)| ((*name).to_owned(), serde_json::json!({"address": address})))
            .collect();
        global.insert("hubs".to_owned(), serde_json::Value::Object(entries));
    }
    std::fs::write(
        dir.path().join("config.json"),
        serde_json::Value::Object(global).to_string(),
    )
    .unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    (dir, workspace)
}

/// The hub `target` names: `None` for the local hub.
fn listed_name(target: &doors::hub::Target) -> Option<&str> {
    match target {
        doors::hub::Target::Local => None,
        doors::hub::Target::Listed { name, .. } => Some(name),
    }
}

/// The configuration `target_in` loads for `home` and `workspace`, so
/// the notice test reads the same file the target test does.
fn loaded_config(home: &Path, workspace: &Path) -> config::Config {
    let project = config::ProjectKey::new(log::project_key(&doors::project(workspace))).unwrap();
    config::Config::load(config::Sources {
        home: home.to_path_buf(),
        workspace: workspace.to_path_buf(),
        project,
        overrides: Vec::new(),
    })
    .unwrap()
}

#[test]
fn hub_entries_keeps_only_hub_and_hubs_keys() {
    assert_eq!(
        hub_entries(vec![
            "hub.default=box".to_owned(),
            "model=x".to_owned(),
            "hubs.\"box\".address=ws://h".to_owned(),
            "hubbub=1".to_owned(),
            "hub_x=1".to_owned(),
            "a.hub=1".to_owned(),
            "hub".to_owned(),
        ]),
        vec![
            "hub.default=box".to_owned(),
            "hubs.\"box\".address=ws://h".to_owned(),
        ]
    );
    // A quoted segment may hold a dot, and a value may hold an `=`:
    // the whole entry rides. A key the path parser refuses rides
    // nowhere, as does an entry with no `=`.
    assert_eq!(
        hub_entries(vec![
            "hubs.\"a.b\".address=unix:/x".to_owned(),
            "hub.default=a=b".to_owned(),
            "hub..default=x".to_owned(),
            "hubs.\"unclosed=x".to_owned(),
        ]),
        vec![
            "hubs.\"a.b\".address=unix:/x".to_owned(),
            "hub.default=a=b".to_owned(),
        ]
    );
}

#[test]
fn target_reads_the_flag_else_the_default_else_the_local_hub() {
    let (dir, workspace) = hub_home(
        Some("a"),
        &[
            ("a", "unix:/tmp/a"),
            ("box", "unix:/tmp/box"),
            ("c", "unix:/tmp/c"),
        ],
    );
    let home = dir.path();
    assert_eq!(
        listed_name(&target_in(home, &workspace, None, Vec::new()).unwrap()),
        Some("a")
    );
    assert_eq!(
        listed_name(
            &target_in(home, &workspace, None, vec!["hub.default=box".to_owned()]).unwrap()
        ),
        Some("box")
    );
    assert_eq!(
        listed_name(
            &target_in(
                home,
                &workspace,
                Some("c"),
                vec!["hub.default=box".to_owned()]
            )
            .unwrap()
        ),
        Some("c")
    );
}

#[test]
fn target_without_a_flag_or_default_is_the_local_hub() {
    let (dir, workspace) = hub_home(None, &[("a", "unix:/tmp/a")]);
    assert!(matches!(
        target_in(dir.path(), &workspace, None, Vec::new()).unwrap(),
        doors::hub::Target::Local
    ));
    // A layer with no hub entry loads with no overrides and does not
    // fail: the unrelated key stays with the terminal run.
    assert!(matches!(
        target_in(dir.path(), &workspace, None, vec!["model=nope".to_owned()]).unwrap(),
        doors::hub::Target::Local
    ));
}

#[test]
fn target_with_an_unknown_hub_is_a_usage_error() {
    let (dir, workspace) = hub_home(Some("a"), &[("a", "unix:/tmp/a")]);
    let error = target_in(dir.path(), &workspace, Some("nope"), Vec::new()).unwrap_err();
    assert_eq!(error.code, contract::ErrorCode::Usage);
}

#[test]
fn a_repository_hub_default_is_ignored_with_a_notice() {
    let (dir, workspace) = hub_home(Some("a"), &[("a", "unix:/tmp/a"), ("b", "unix:/tmp/b")]);
    std::fs::create_dir_all(workspace.join(".fiber")).unwrap();
    std::fs::write(
        workspace.join(".fiber/config.json"),
        r#"{"hub": {"default": "b"}}"#,
    )
    .unwrap();
    // The repository's person-only key cannot redirect the target.
    assert_eq!(
        listed_name(&target_in(dir.path(), &workspace, None, Vec::new()).unwrap()),
        Some("a")
    );
    let lines = notice_lines(loaded_config(dir.path(), &workspace).notices());
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].starts_with("fiber: "), "{}", lines[0]);
    assert!(lines[0].contains("ignored `hub.default`"), "{}", lines[0]);
    assert!(lines[0].contains(".fiber/config.json"), "{}", lines[0]);
}

#[test]
fn notice_lines_is_empty_without_an_ignored_key() {
    let (dir, workspace) = hub_home(Some("a"), &[("a", "unix:/tmp/a")]);
    assert!(notice_lines(loaded_config(dir.path(), &workspace).notices()).is_empty());
}

/// One named deadline for the fake hub's read and the test's receive.
const HUB_DEADLINE: Duration = Duration::from_secs(10);

/// An in-process hub answering one `sessions` command with `answer`: a
/// thread on the hub end reads the one command line, then writes it.
struct FakeHub {
    answer: String,
}

impl FakeHub {
    fn connect(&mut self) -> Result<doors::hub::Hub, doors::Error> {
        let (client, hub) = UnixStream::pair().map_err(doors::Error::Local)?;
        hub.set_read_timeout(Some(HUB_DEADLINE))
            .map_err(doors::Error::Local)?;
        let answer = self.answer.clone();
        thread::spawn(move || {
            let mut line = String::new();
            BufReader::new(&hub).read_line(&mut line).unwrap();
            (&hub).write_all(answer.as_bytes()).unwrap();
        });
        let hello = contract::HubLine {
            kind: "hub_hello".to_owned(),
            ts: 1,
            schema_version: 1,
            payload: serde_json::Map::new(),
        };
        Ok((client, hello))
    }
}

/// One `sessions` answer line for `result`, as the hub writes it.
fn sessions_answer(result: serde_json::Value) -> String {
    let line = serde_json::json!({"kind": "command_accepted", "ts": 1, "schema_version": 1,
        "payload": {"command_id": "c_sessions", "result": result}});
    format!("{line}\n")
}

/// Runs `listed_latest` for `name` against a hub answering `answer`, on a
/// worker thread: the outcome, received under the hub deadline so a hung
/// read fails the test instead of hanging it.
fn listed(name: &str, answer: String) -> Result<SessionId, contract::shapes::Failure> {
    let name = name.to_owned();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut hub = FakeHub { answer };
        let got = listed_latest(&name, &mut || hub.connect());
        tx.send(got).unwrap_or(());
    });
    fakes::Deadline::after(HUB_DEADLINE).recv(&rx).unwrap()
}

#[test]
fn a_listed_route_opens_the_hub_session_without_reading_local_logs() {
    let listed_target = doors::hub::Target::Listed {
        name: "box".to_owned(),
        address: doors::hub::Address::parse("unix:/x").unwrap(),
        token: None,
    };
    let mut local = || -> Result<SessionId, contract::shapes::Failure> {
        panic!("a listed continue never reads local logs")
    };
    let mut listed = |name: &str| {
        assert_eq!(name, "box");
        Ok::<_, contract::shapes::Failure>(SessionId("s_hub".to_owned()))
    };
    let id = route(&listed_target, &mut local, &mut listed).unwrap();
    assert_eq!(id.0, "s_hub");
}

#[test]
fn continue_without_a_flag_uses_the_configured_hub_and_a_flag_overrides_it() {
    let (dir, workspace) = hub_home(
        Some("box"),
        &[("box", "unix:/tmp/box"), ("other", "unix:/tmp/other")],
    );
    let home = dir.path();
    assert_eq!(
        listed_name(&target_in(home, &workspace, None, Vec::new()).unwrap()),
        Some("box")
    );
    assert_eq!(
        listed_name(&target_in(home, &workspace, Some("other"), Vec::new()).unwrap()),
        Some("other")
    );
}

#[test]
fn listed_continue_without_an_eligible_session_names_fiber() {
    for result in [
        serde_json::json!({"live": [], "exited": []}),
        serde_json::json!({"live": [], "exited": [
            {"session_id": "s_1", "name": "n", "how": "exited", "active": null},
        ]}),
    ] {
        let error = listed("box", sessions_answer(result)).unwrap_err();
        assert_eq!(error.code, contract::ErrorCode::Usage);
        assert_eq!(
            error.message,
            "No session on hub `box` to continue; run `fiber` to start one. Run `fiber --help` for usage."
        );
    }
}

#[test]
fn route_runs_only_the_taken_side() {
    let listed_target = doors::hub::Target::Listed {
        name: "box".to_owned(),
        address: doors::hub::Address::parse("unix:/x").unwrap(),
        token: None,
    };
    let mut local_calls = 0;
    let mut listed_calls = 0;
    let mut listed_name_seen: Option<String> = None;
    let local_target = doors::hub::Target::Local;
    let mut local = || {
        local_calls += 1;
        Ok::<_, contract::shapes::Failure>(SessionId("s_1".to_owned()))
    };
    let mut listed = |name: &str| {
        listed_calls += 1;
        listed_name_seen = Some(name.to_owned());
        Ok::<_, contract::shapes::Failure>(SessionId("s_2".to_owned()))
    };
    let first = route(&local_target, &mut local, &mut listed).unwrap();
    let second = route(&listed_target, &mut local, &mut listed).unwrap();
    drop((local, listed));
    assert_eq!(first.0, "s_1");
    assert_eq!(second.0, "s_2");
    assert_eq!((local_calls, listed_calls), (1, 1));
    assert_eq!(listed_name_seen.as_deref(), Some("box"));
}
