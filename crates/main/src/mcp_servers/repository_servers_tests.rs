//! Startup and the post-offer closure, without starting a process: the
//! partition `specs` makes, the pinned spec, and what the load starts,
//! reports or refuses.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::sync::{Arc, Mutex};

use config::ProjectKey;
use contract::ErrorCode;
use contract::events::{Event, OfferedKind};
use fakes::clock::FakeClock;
use serde_json::{Value, json};

use super::{Later, Waiting, pinned_spec, prepare};
use crate::mcp_servers::{RepositorySpec, specs};

/// The stdio half of `spec`: an exhaustive match, so a new transport
/// breaks every site that assumes stdio.
fn stdio(spec: &mcp::ServerSpec) -> &mcp::StdioSpec {
    match &spec.transport {
        mcp::Transport::Stdio(stdio) => stdio,
        mcp::Transport::Http { .. } => panic!("a stdio-only test met a remote spec"),
    }
}

struct Setup {
    rig: crate::test_support::Rig,
    project: ProjectKey,
    clock: Arc<dyn contract::clock::Clock>,
}

impl Setup {
    fn new() -> Self {
        Self {
            rig: crate::test_support::Rig::new("fiber-repo-servers"),
            project: ProjectKey::new("test").unwrap(),
            clock: FakeClock::new(),
        }
    }

    fn home(&self) -> &std::path::Path {
        &self.rig.home
    }

    fn workspace(&self) -> &std::path::Path {
        &self.rig.workspace
    }

    fn write_repo(&self, servers: &Value) {
        std::fs::create_dir_all(self.workspace().join(".fiber")).unwrap();
        std::fs::write(
            self.workspace().join(".fiber/config.json"),
            json!({"mcp": {"servers": servers}}).to_string(),
        )
        .unwrap();
    }

    fn config(&self) -> config::Config {
        self.rig.config(Vec::<String>::new())
    }

    fn repository(&self) -> Vec<RepositorySpec> {
        specs(&self.config()).expect("the specs read").repository
    }

    /// The gathered item and its current hash.
    fn gathered(&self, name: &str) -> (extensions::RepoItem, String) {
        let item = extensions::declared_items(self.workspace(), self.clock.as_ref())
            .unwrap()
            .into_iter()
            .find(|item| item.kind == OfferedKind::McpServer && item.name == name)
            .unwrap();
        let mut index = extensions::Index::load(self.home());
        let hash = extensions::hash(&mut index, &item).unwrap();
        (item, hash)
    }

    fn approve(&self, name: &str) -> String {
        let (item, hash) = self.gathered(name);
        extensions::Store::new(self.home(), &self.project, Arc::clone(&self.clock))
            .approve(&item, &hash)
            .unwrap();
        hash
    }

    fn prepare(
        &self,
        skipped: &[r#loop::OfferKey],
        repository: Vec<RepositorySpec>,
    ) -> super::Prepared {
        prepare(
            self.home(),
            &self.project,
            self.workspace(),
            &self.clock,
            skipped,
            repository,
        )
    }

    fn key(&self, name: &str, hash: &str) -> r#loop::OfferKey {
        r#loop::OfferKey {
            kind: OfferedKind::McpServer,
            name: name.to_owned(),
            hash: hash.to_owned(),
        }
    }

    fn images(&self) -> Arc<dyn contract::images::Images> {
        Arc::new(tools::ImageChild::new(
            std::path::PathBuf::new(),
            std::path::PathBuf::new(),
        ))
    }

    fn later(&self, waiting: Vec<RepositorySpec>) -> Later {
        let cache = self.home().join("cache").join("mcp");
        let servers = mcp::start(
            Vec::new(),
            self.workspace(),
            &self.home().join("artifacts"),
            Arc::clone(&self.images()),
            &cache,
            &self.clock,
            "0.0.0",
            None,
        )
        .servers;
        Later {
            home: self.home().to_path_buf(),
            project: self.project.clone(),
            workspace: self.workspace().to_path_buf(),
            artifacts: self.home().join("artifacts"),
            images: self.images(),
            cache,
            clock: Arc::clone(&self.clock),
            waiting,
            servers: Arc::new(servers),
        }
    }
}

fn outcome(
    skipped: Vec<r#loop::OfferKey>,
    reported: Vec<r#loop::OfferKey>,
) -> r#loop::OfferOutcome {
    r#loop::OfferOutcome {
        skipped,
        reported,
        held: Vec::new(),
    }
}

#[test]
fn prepare_starts_an_approved_server_from_its_copy() {
    let setup = Setup::new();
    fs::create_dir_all(setup.workspace().join("srv")).unwrap();
    fs::write(setup.workspace().join("srv/run.sh"), "echo\n").unwrap();
    setup.write_repo(&json!({"db": {
        "command": "./srv/run.sh",
        "args": ["srv/run.sh"],
        "env": {"MODE": "fast"},
        "timeout_ms": 2000,
        "required": true,
        "tools": {"echo": {"hints": {"readOnlyHint": true}}},
    }}));
    let hash = setup.approve("db");
    let prepared = setup.prepare(&[], setup.repository());
    assert!(prepared.failed.is_empty());
    assert!(prepared.waiting.is_empty());
    let [spec] = prepared.ready.as_slice() else {
        panic!("one ready spec");
    };
    // The declaration stays the cache-key input; the program runs the copy.
    assert_eq!(stdio(spec).command, "./srv/run.sh");
    assert_eq!(stdio(spec).args, vec!["srv/run.sh".to_owned()]);
    assert_eq!(
        stdio(spec).env.get("MODE").map(String::as_str),
        Some("fast")
    );
    assert_eq!(spec.call_timeout, std::time::Duration::from_millis(2000));
    assert!(spec.required);
    assert!(
        spec.hints.is_empty(),
        "a repository's hints never reach the spec"
    );
    let run = stdio(spec).run.as_ref().expect("a program");
    let copy = setup.home().join("pinned").join(&hash).join("srv/run.sh");
    assert_eq!(run.command, copy.display().to_string());
    assert_eq!(run.args, vec![copy.display().to_string()]);
    let (command, args) = stdio(spec).program();
    assert_eq!(command, run.command.as_str());
    assert_eq!(args, run.args.as_slice());
    // The built spec's guard passes, and holds the copy while it lives.
    (run.guard)().expect("the guard passes");
    let marker = setup.home().join("pinned").join(format!("{hash}.ready"));
    let probe = fs::File::options().read(true).open(&marker).unwrap();
    assert!(probe.try_lock().is_err());
    drop(prepared);
    let probe = fs::File::options().read(true).open(&marker).unwrap();
    assert!(probe.try_lock().is_ok(), "the hold releases with the spec");
}

#[test]
fn pinned_spec_maps_each_string_through_the_frozen_resolutions() {
    let setup = Setup::new();
    fs::create_dir_all(setup.workspace().join("srv")).unwrap();
    fs::write(setup.workspace().join("srv/run.sh"), "echo\n").unwrap();
    let outside = setup.rig.root.path().join("outside.sh");
    fs::write(&outside, "echo\n").unwrap();
    setup.write_repo(&json!({"db": {
        "command": "/bin/bash",
        "args": ["srv/run.sh", outside.to_str().unwrap(), "--port", "bare"],
    }}));
    setup.approve("db");
    let repository = setup.repository();
    let [candidate] = repository.as_slice() else {
        panic!("one repository server");
    };
    let approved = extensions::repository_servers(
        setup.home(),
        &setup.project,
        setup.workspace(),
        Arc::clone(&setup.clock),
        &BTreeSet::new(),
    )
    .unwrap()
    .remove("db")
    .unwrap()
    .unwrap();
    let extensions::ServerApproval::Approved(approved) = approved else {
        panic!("db is approved");
    };
    let spec = pinned_spec(candidate, &approved).unwrap();
    let copy = setup
        .home()
        .join("pinned")
        .join(&approved.hash)
        .join("srv/run.sh");
    // An outside command is its frozen target (`/bin` links to `/usr/bin` on
    // some hosts) and a bare word stays as written; the inside path becomes
    // its copy.
    let bash = std::path::Path::new("/bin/bash")
        .canonicalize()
        .unwrap()
        .display()
        .to_string();
    let run = stdio(&spec).run.as_ref().expect("a program");
    assert_eq!(run.command, bash);
    assert_eq!(
        run.args,
        vec![
            copy.display().to_string(),
            outside.canonicalize().unwrap().display().to_string(),
            "--port".to_owned(),
            "bare".to_owned(),
        ]
    );
    assert_eq!(stdio(&spec).command, "/bin/bash");
}

#[test]
fn prepare_waits_on_an_unapproved_server_with_its_hash() {
    for required in [false, true] {
        let setup = Setup::new();
        fs::create_dir_all(setup.workspace().join("srv")).unwrap();
        fs::write(setup.workspace().join("srv/run.sh"), "echo\n").unwrap();
        setup.write_repo(&json!({"db": {"command": "./srv/run.sh", "required": required}}));
        let (_, hash) = setup.gathered("db");
        let prepared = setup.prepare(&[], setup.repository());
        assert!(prepared.ready.is_empty());
        assert!(prepared.failed.is_empty());
        let [
            (
                spec,
                Waiting::Unapproved {
                    hash: seen,
                    required: seen_required,
                },
            ),
        ] = prepared.waiting.as_slice()
        else {
            panic!("one waiting server");
        };
        assert_eq!(spec.spec.name, "db");
        assert_eq!(*seen, hash);
        assert_eq!(*seen_required, required);
    }
}

#[test]
fn prepare_skips_without_making_or_holding_a_copy() {
    let setup = Setup::new();
    fs::create_dir_all(setup.workspace().join("srv")).unwrap();
    fs::write(setup.workspace().join("srv/run.sh"), "echo\n").unwrap();
    setup.write_repo(&json!({"db": {"command": "./srv/run.sh"}}));
    let hash = setup.approve("db");
    // Prune the copy: the skip must not build it again.
    fs::remove_dir_all(setup.home().join("pinned").join(&hash)).unwrap();
    fs::remove_file(setup.home().join("pinned").join(format!("{hash}.ready"))).unwrap();
    let skipped = vec![setup.key("db", &hash)];
    let prepared = setup.prepare(&skipped, setup.repository());
    assert!(prepared.ready.is_empty());
    assert!(prepared.failed.is_empty());
    assert!(
        matches!(prepared.waiting.as_slice(), [(spec, Waiting::Skipped)] if spec.spec.name == "db")
    );
    assert!(
        !setup.home().join("pinned").join(&hash).exists(),
        "no copy is made for a skip"
    );
}

#[test]
fn prepare_fails_a_copy_it_cannot_make() {
    for required in [false, true] {
        let setup = Setup::new();
        fs::create_dir_all(setup.workspace().join("srv")).unwrap();
        fs::write(setup.workspace().join("srv/run.sh"), "echo\n").unwrap();
        setup.write_repo(&json!({"db": {"command": "./srv/run.sh", "required": required}}));
        let hash = setup.approve("db");
        fs::remove_dir_all(setup.home().join("pinned").join(&hash)).unwrap();
        fs::remove_file(setup.home().join("pinned").join(format!("{hash}.ready"))).unwrap();
        fs::create_dir(setup.home().join("pinned").join(format!("{hash}.ready"))).unwrap();
        let prepared = setup.prepare(&[], setup.repository());
        assert!(prepared.ready.is_empty());
        if required {
            let failure = prepared.required_failed.as_ref().expect("required fails");
            assert_eq!(failure.server, "db");
            assert_eq!(failure.error.code, ErrorCode::McpServerUnavailable);
            assert!(
                failure.error.message.contains("pinned copy"),
                "{}",
                failure.error.message
            );
            assert!(prepared.failed.is_empty());
        } else {
            assert!(prepared.required_failed.is_none());
            let [failure] = prepared.failed.as_slice() else {
                panic!("one failure");
            };
            assert_eq!(failure.server, "db");
            assert!(
                failure.error.message.contains("pinned copy"),
                "{}",
                failure.error.message
            );
        }
    }
}

#[test]
fn prepare_fails_every_server_when_gathering_fails() {
    let setup = Setup::new();
    fs::create_dir_all(setup.workspace().join("srv")).unwrap();
    fs::write(setup.workspace().join("srv/run.sh"), "echo\n").unwrap();
    setup.write_repo(&json!({"db": {"command": "./srv/run.sh"}}));
    let repository = setup.repository();
    assert_eq!(repository.len(), 1);
    let prepared = prepare(
        setup.home(),
        &setup.project,
        &setup.workspace().join("missing"),
        &setup.clock,
        &[],
        repository,
    );
    assert!(prepared.ready.is_empty());
    assert!(prepared.waiting.is_empty());
    let [failure] = prepared.failed.as_slice() else {
        panic!("every server failed");
    };
    assert_eq!(failure.server, "db");
    assert!(
        failure.error.message.contains("could not be read"),
        "{}",
        failure.error.message
    );
}

#[test]
fn a_changed_command_args_or_env_needs_a_new_approval() {
    for (field, first, second) in [
        ("command", json!("./srv/run.sh"), json!("./srv/other.sh")),
        ("args", json!(["srv/run.sh"]), json!(["srv/other.sh"])),
        ("env", json!({"MODE": "slow"}), json!({"MODE": "fast"})),
    ] {
        let setup = Setup::new();
        fs::create_dir_all(setup.workspace().join("srv")).unwrap();
        fs::write(setup.workspace().join("srv/run.sh"), "echo\n").unwrap();
        fs::write(setup.workspace().join("srv/other.sh"), "echo\n").unwrap();
        let mut entry = BTreeMap::from([
            ("command".to_owned(), json!("./srv/run.sh")),
            ("args".to_owned(), json!(["srv/run.sh"])),
            ("env".to_owned(), json!({"MODE": "slow"})),
        ]);
        entry.insert(field.to_owned(), first);
        setup.write_repo(&json!({"db": Value::Object(entry.clone().into_iter().collect())}));
        let repository = setup.repository();
        // The approval covers the second declaration; the session read the
        // first.
        entry.insert(field.to_owned(), second);
        setup.write_repo(&json!({"db": Value::Object(entry.into_iter().collect())}));
        setup.approve("db");
        let prepared = setup.prepare(&[], repository);
        assert!(prepared.ready.is_empty(), "{field}");
        assert!(prepared.waiting.is_empty(), "{field}");
        let [failure] = prepared.failed.as_slice() else {
            panic!("{field}: the drift fails the server");
        };
        assert_eq!(failure.server, "db");
        assert!(
            failure.error.message.contains(
                "the repository's declaration of it changed after this session read its configuration"
            ),
            "{field}: {}",
            failure.error.message
        );
    }
}

#[test]
fn a_persons_layer_never_drifts() {
    let setup = Setup::new();
    fs::create_dir_all(setup.workspace().join("srv")).unwrap();
    fs::write(setup.workspace().join("srv/run.sh"), "echo\n").unwrap();
    setup.write_repo(&json!({"db": {"command": "./srv/run.sh", "env": {"MODE": "fast"}}}));
    setup.approve("db");
    // The person's own layers add an `env` key and override nothing the
    // repository supplied: nothing needs to match.
    setup
        .rig
        .write_global(&json!({"mcp": {"servers": {"db": {"env": {"SAFE": "1"}}}}}));
    let prepared = setup.prepare(&[], setup.repository());
    assert!(prepared.failed.is_empty());
    assert_eq!(prepared.ready.len(), 1);
    let [spec] = prepared.ready.as_slice() else {
        panic!("one ready spec");
    };
    assert_eq!(stdio(spec).env.get("SAFE").map(String::as_str), Some("1"));
}

#[test]
fn the_guard_recovers_when_the_new_file_goes_away() {
    let setup = Setup::new();
    fs::create_dir_all(setup.workspace().join("srv")).unwrap();
    fs::write(setup.workspace().join("srv/run.sh"), "echo\n").unwrap();
    setup
        .write_repo(&json!({"db": {"command": "./srv/run.sh", "args": ["srv/run.sh", "late.sh"]}}));
    setup.approve("db");
    let prepared = setup.prepare(&[], setup.repository());
    let [spec] = prepared.ready.as_slice() else {
        panic!("one ready spec");
    };
    let guard = stdio(spec).run.as_ref().expect("a program").guard.clone();
    assert!(guard().is_ok());
    fs::write(setup.workspace().join("late.sh"), "late\n").unwrap();
    let error = guard().expect_err("the new repository file is refused");
    assert!(error.contains("late.sh"), "{error}");
    assert!(error.contains("this approval does not cover"), "{error}");
    fs::remove_file(setup.workspace().join("late.sh")).unwrap();
    assert!(guard().is_ok(), "the guard re-resolves on every call");
}

/// What the `declare` closure records: each declared tool's name and info.
type Declared = Arc<Mutex<Vec<(String, Option<contract::events::ToolInfo>)>>>;

fn declare_recorder() -> (doors::Declare, Declared) {
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&recorded);
    let declare: doors::Declare = Arc::new(move |name: &str, info| {
        seen.lock().unwrap().push((name.to_owned(), info));
    });
    (declare, recorded)
}

/// The cache key of the declaration the control server keeps: its command,
/// arguments, environment and secret references, never the pinned paths. Recompute with:
/// `python3 -c "import hashlib, json; d={'command': './srv/run.sh',
/// 'args': ['srv/run.sh'], 'env': {}, 'secrets': {}}; b=json.dumps(d, separators=
/// (',', ':'), sort_keys=True).encode(); print(hashlib.sha256(b).
/// hexdigest())"`.
const CONTROL_KEY: &str = "c8006cb33c5dd250d5260106e564fdb95eb4836614213fee7585f71cb434553c";

fn write_cache(setup: &Setup, name: &str, key: &str) {
    let dir = setup.home().join("cache").join("mcp");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(format!("{name}.json")),
        json!({"version": 1, "key": key, "tools": [{"name": "echo"}], "prompts": []}).to_string(),
    )
    .unwrap();
}

fn offer_ready(name: &str) -> Setup {
    let setup = Setup::new();
    fs::create_dir_all(setup.workspace().join("srv")).unwrap();
    fs::write(setup.workspace().join("srv/run.sh"), "echo\n").unwrap();
    setup.write_repo(&json!({name: {"command": "./srv/run.sh", "args": ["srv/run.sh"]}}));
    setup
}

#[test]
fn content_changed_after_the_offer_is_noticed_or_fails() {
    for required in [false, true] {
        let setup = Setup::new();
        fs::create_dir_all(setup.workspace().join("srv")).unwrap();
        fs::write(setup.workspace().join("srv/run.sh"), "echo\n").unwrap();
        setup.write_repo(&json!({"db": {"command": "./srv/run.sh", "required": required}}));
        // What the offer's last check saw: gather and approve it.
        let (_, hash) = setup.gathered("db");
        setup.approve("db");
        let waiting = setup.repository();
        // The injection point between the offer's last check and the load:
        // the declaration's file changes, so the content is new.
        fs::write(setup.workspace().join("srv/run.sh"), "changed\n").unwrap();
        let (declare, recorded) = declare_recorder();
        let load = setup.later(waiting).into_load(declare, BTreeMap::new());
        let current = setup.key("db", &hash);
        let result = load(&outcome(Vec::new(), vec![current]));
        assert!(recorded.lock().unwrap().is_empty(), "required: {required}");
        assert!(
            !setup.home().join("cache").exists(),
            "required: {required}: no server starts"
        );
        if required {
            let failure = match result {
                Ok(_) => panic!("a required change fails the run"),
                Err(failure) => failure,
            };
            assert_eq!(failure.code, ErrorCode::McpServerUnapproved);
            assert!(failure.message.contains("`db`"), "{}", failure.message);
        } else {
            let approved = result.expect("a change is noticed");
            assert!(approved.tools.is_empty());
            let [Event::Notice(notice)] = approved.events.as_slice() else {
                panic!("required: {required}: one notice");
            };
            assert_eq!(notice.code, ErrorCode::RepositoryCodeSkipped);
            assert!(notice.message.contains("`db`"), "{}", notice.message);
        }
    }
}

#[test]
fn unchanged_content_approved_in_the_offer_starts_without_a_line() {
    let setup = offer_ready("db");
    let hash = setup.approve("db");
    // The tool-list cache holds the declaration, so the start declares
    // lazily and spawns nothing.
    write_cache(&setup, "db", CONTROL_KEY);
    let waiting = setup.repository();
    let (declare, recorded) = declare_recorder();
    let load = setup.later(waiting).into_load(declare, BTreeMap::new());
    let current = setup.key("db", &hash);
    // Control: the same steps without the rewrite.
    let approved = load(&outcome(Vec::new(), vec![current])).expect("no notice, no failure");
    assert!(approved.events.is_empty());
    assert_eq!(approved.tools.len(), 1);
    let (registered_by, tool) = &approved.tools[0];
    assert_eq!(registered_by, "db");
    assert_eq!(tool.definition().name, "mcp__db__echo");
    let recorded = recorded.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].0, "mcp__db__echo");
}

#[test]
fn a_skip_holds_even_when_approved_meanwhile() {
    let setup = offer_ready("db");
    let hash = setup.approve("db");
    let waiting = setup.repository();
    let (declare, recorded) = declare_recorder();
    let load = setup.later(waiting).into_load(declare, BTreeMap::new());
    // Another session's (or `fiber approve`'s) approval lands after the
    // skip: the content stays skipped for this session.
    let key = setup.key("db", &hash);
    let approved =
        load(&outcome(vec![key], vec![setup.key("db", &hash)])).expect("a skip fails nothing");
    assert!(approved.tools.is_empty());
    assert!(approved.events.is_empty());
    assert!(recorded.lock().unwrap().is_empty());
    // `prepare` with the skip puts the approved server in waiting, not ready.
    let prepared = setup.prepare(&[setup.key("db", &hash)], setup.repository());
    assert!(prepared.ready.is_empty());
    assert!(
        matches!(prepared.waiting.as_slice(), [(spec, Waiting::Skipped)] if spec.spec.name == "db")
    );
}

#[test]
fn already_reported_content_starts_nothing_with_no_line() {
    let setup = offer_ready("db");
    // Never approved: a never, or a headless notice, already wrote its line.
    let (_, hash) = setup.gathered("db");
    let waiting = setup.repository();
    let (declare, recorded) = declare_recorder();
    let load = setup.later(waiting).into_load(declare, BTreeMap::new());
    let approved = load(&outcome(Vec::new(), vec![setup.key("db", &hash)]))
        .expect("reported content fails nothing");
    assert!(approved.tools.is_empty());
    assert!(approved.events.is_empty());
    assert!(recorded.lock().unwrap().is_empty());
}

#[test]
fn the_load_from_nothing_waiting_touches_nothing_and_runs_once() {
    let home = fakes::TempDir::new("fiber-later-empty");
    let setup = Setup::new();
    let empty = Later {
        home: home.path().to_path_buf(),
        project: setup.project.clone(),
        workspace: setup.workspace().to_path_buf(),
        artifacts: setup.home().join("artifacts"),
        images: setup.images(),
        cache: home.path().join("cache").join("mcp"),
        clock: Arc::clone(&setup.clock),
        waiting: Vec::new(),
        servers: Arc::new(
            mcp::start(
                Vec::new(),
                setup.workspace(),
                &setup.home().join("artifacts"),
                setup.images(),
                &home.path().join("cache").join("mcp"),
                &setup.clock,
                "0.0.0",
                None,
            )
            .servers,
        ),
    };
    let (declare, _) = declare_recorder();
    let load = empty.into_load(declare, BTreeMap::new());
    let first = load(&outcome(Vec::new(), Vec::new())).expect("empty");
    assert!(first.tools.is_empty());
    assert!(first.events.is_empty());
    assert!(
        fs::read_dir(home.path()).unwrap().next().is_none(),
        "nothing in the home is touched"
    );
    let second = load(&outcome(Vec::new(), Vec::new())).expect("once");
    assert!(second.tools.is_empty());
    assert!(second.events.is_empty());
}

#[test]
fn a_second_load_call_returns_an_empty_approved() {
    let setup = offer_ready("db");
    let hash = setup.approve("db");
    fs::write(setup.workspace().join("srv/run.sh"), "changed\n").unwrap();
    let waiting = setup.repository();
    let (declare, _) = declare_recorder();
    let load = setup.later(waiting).into_load(declare, BTreeMap::new());
    let current = setup.key("db", &hash);
    let first = load(&outcome(Vec::new(), vec![current])).expect("noticed");
    assert_eq!(first.events.len(), 1);
    let second = load(&outcome(Vec::new(), Vec::new())).expect("once");
    assert!(second.tools.is_empty());
    assert!(second.events.is_empty());
}

#[test]
fn the_load_reports_a_drift_it_refuses_without_starting() {
    let setup = Setup::new();
    fs::create_dir_all(setup.workspace().join("srv")).unwrap();
    fs::write(setup.workspace().join("srv/run.sh"), "echo\n").unwrap();
    setup.write_repo(&json!({"db": {"command": "./srv/run.sh", "args": ["srv/run.sh"]}}));
    let waiting = setup.repository();
    // The approval covers the new declaration; the session read the old one.
    setup.write_repo(&json!({"db": {"command": "./srv/run.sh", "args": ["srv/other.sh"]}}));
    fs::write(setup.workspace().join("srv/other.sh"), "echo\n").unwrap();
    setup.approve("db");
    let (declare, recorded) = declare_recorder();
    let load = setup.later(waiting).into_load(declare, BTreeMap::new());
    let approved =
        load(&outcome(Vec::new(), Vec::new())).expect("a drift is an event, not a failure");
    assert!(approved.tools.is_empty());
    assert!(recorded.lock().unwrap().is_empty());
    let [Event::McpServerFailed(failure)] = approved.events.as_slice() else {
        panic!("one start failure");
    };
    assert_eq!(failure.server, "db");
    assert!(
        failure.error.message.contains(
            "the repository's declaration of it changed after this session read its configuration"
        ),
        "{}",
        failure.error.message
    );
    assert!(!setup.home().join("cache").exists(), "no server starts");
}

#[test]
fn prepare_starts_an_approved_remote_server_as_declared() {
    let setup = Setup::new();
    setup.write_repo(&json!({"db": {"url": "https://example.com/mcp"}}));
    let repository = setup.repository();
    setup.approve("db");
    let prepared = setup.prepare(&[], repository);
    assert!(prepared.failed.is_empty());
    assert!(prepared.waiting.is_empty());
    let [spec] = prepared.ready.as_slice() else {
        panic!("one ready spec");
    };
    // No files to pin and no `run`: the declaration runs as declared.
    let mcp::Transport::Http { url, .. } = &spec.transport else {
        panic!("a remote transport");
    };
    assert_eq!(url, "https://example.com/mcp");
}

#[test]
fn a_changed_url_after_approval_fails_the_server() {
    let setup = Setup::new();
    setup.write_repo(&json!({"db": {"url": "https://example.com/first"}}));
    let repository = setup.repository();
    // The approval covers the second declaration; the session read the first.
    setup.write_repo(&json!({"db": {"url": "https://example.com/second"}}));
    setup.approve("db");
    let prepared = setup.prepare(&[], repository);
    assert!(prepared.ready.is_empty());
    assert!(prepared.waiting.is_empty());
    let [failure] = prepared.failed.as_slice() else {
        panic!("the drift fails the server");
    };
    assert_eq!(failure.server, "db");
    assert!(
        failure.error.message.contains(
            "the repository's declaration of it changed after this session read its configuration"
        ),
        "{}",
        failure.error.message
    );
}

/// The cache of the control server offering resources: the same JSON as
/// `write_cache`, plus `"resources": true`, so the load declares lazily
/// and spawns nothing.
fn write_cache_offering_resources(setup: &Setup, name: &str, key: &str) {
    let dir = setup.home().join("cache").join("mcp");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(format!("{name}.json")),
        json!({"version": 1, "key": key, "tools": [{"name": "echo"}], "prompts": [], "resources": true}).to_string(),
    )
    .unwrap();
}

#[test]
fn the_post_offer_load_skips_mcp_resources_when_the_session_declares_it() {
    let setup = offer_ready("db");
    let hash = setup.approve("db");
    write_cache_offering_resources(&setup, "db", CONTROL_KEY);
    let waiting = setup.repository();
    // The session already declares `mcp_resources`: the load never offers
    // a second registration for replacement.
    let later = setup.later(waiting);
    later.servers.mark_resources_declared();
    let (declare, recorded) = declare_recorder();
    let load = later.into_load(declare, BTreeMap::new());
    let current = setup.key("db", &hash);
    let approved = load(&outcome(Vec::new(), vec![current])).expect("no notice, no failure");
    assert!(approved.events.is_empty());
    let names: Vec<(String, String)> = approved
        .tools
        .iter()
        .map(|(registered_by, tool)| (registered_by.clone(), tool.definition().name))
        .collect();
    assert_eq!(names, [("db".to_owned(), "mcp__db__echo".to_owned())]);
    let recorded = recorded.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].0, "mcp__db__echo");
}

#[test]
fn the_post_offer_load_declares_mcp_resources_when_the_session_has_none() {
    let setup = offer_ready("db");
    let hash = setup.approve("db");
    write_cache_offering_resources(&setup, "db", CONTROL_KEY);
    let waiting = setup.repository();
    let (declare, recorded) = declare_recorder();
    let load = setup.later(waiting).into_load(declare, BTreeMap::new());
    let current = setup.key("db", &hash);
    let approved = load(&outcome(Vec::new(), vec![current])).expect("no notice, no failure");
    assert!(approved.events.is_empty());
    let names: Vec<(String, String)> = approved
        .tools
        .iter()
        .map(|(registered_by, tool)| (registered_by.clone(), tool.definition().name))
        .collect();
    assert_eq!(
        names,
        [
            ("db".to_owned(), "mcp__db__echo".to_owned()),
            ("builtin".to_owned(), "mcp_resources".to_owned()),
        ]
    );
    let recorded = recorded.lock().unwrap();
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[0].0, "mcp__db__echo");
    assert_eq!(recorded[1].0, "mcp_resources");
}
