//! Resolving `fiber:` references, building the child's command and watching
//! its socket (`docs/delegates.md`, "Choosing a model" and "Lifetime").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use crate::test_support::{install_extension, load};
use fakes::MUST_SUCCEED_WITHIN;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use contract::ErrorCode;
use contract::SessionId;
use serde_json::json;

use super::{launcher, quota_text, resolver, watcher};
use crate::switch::quota::{LabelQuota, QuotaView};
use extensions::{Quota, QuotaEntry, Window, WindowUse};
use fakes::Deadline;

/// Installs a provider `fake` with a leveled model `m` and a plain model
/// `plain` (unless `install` is false), writes `{"roles": <roles>}` to
/// the test home's configuration, and returns the loaded registry with
/// that configuration.
fn rig_with(roles: serde_json::Value, install: bool) -> (extensions::Providers, config::Config) {
    let root = fakes::TempDir::new("fd");
    rig_loaded(&root, roles, install, &[], None)
}

/// Loads a rig in `root`: the fake provider reads its key from the
/// environment, so `default` is always a label it has, and `stored` are
/// stored as `credentials/fake/<label>` on disk. `repo_roles` writes a
/// repository-layer `{"roles": ...}` into the workspace. Callers needing
/// the stored labels or the repository layer keep `root` alive: the label
/// set is read from disk at call time.
fn rig_loaded(
    root: &fakes::TempDir,
    roles: serde_json::Value,
    install: bool,
    stored: &[&str],
    repo_roles: Option<&serde_json::Value>,
) -> (extensions::Providers, config::Config) {
    let home = root.path().join("home");
    if install {
        install_extension(
            &home,
            "extensions/fake",
            serde_json::from_str(
                r#"{"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}"#,
            )
            .unwrap(),
            &[(
                "fake",
                serde_json::from_str(
                    r#"{"name": "fake", "credential": {"env": "FIBER_TEST_FAKE_KEY"}, "models": [
            {"id": "m", "protocol": "openai-responses", "base_url": "http://127.0.0.1:9/v1",
             "context_window": 1000, "thinking_levels": ["low", "high"], "thinking_default": "low"},
            {"id": "plain", "protocol": "openai-responses", "base_url": "http://127.0.0.1:9/v1",
             "context_window": 1000}
        ]}"#,
                )
                .unwrap(),
            )],
        );
    } else {
        std::fs::create_dir_all(&home).unwrap();
    }
    for label in stored {
        config::store_credential(
            &home,
            "fake",
            label,
            &config::Secret::new(format!("{label}-key")),
        )
        .unwrap();
    }
    std::fs::write(
        home.join("config.json"),
        json!({"roles": roles}).to_string(),
    )
    .unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    if let Some(repo_roles) = repo_roles {
        let dir = workspace.join(".fiber");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.json"),
            json!({"roles": repo_roles}).to_string(),
        )
        .unwrap();
    }
    let config = load(&home, &workspace, "test", Vec::<String>::new());
    let (providers, _notices) = extensions::Providers::load(&home).unwrap();
    // `root` is dropped here; the registry and configuration were read.
    (providers, config)
}

/// A rig with `stored` labels on disk for the fake provider, whose temp
/// dir stays alive under the caller: the label set is read from disk at
/// call time.
fn stored_rig(
    roles: serde_json::Value,
    stored: &[&str],
) -> (fakes::TempDir, extensions::Providers, config::Config) {
    let root = fakes::TempDir::new("fd-label");
    let (providers, config) = rig_loaded(&root, roles, true, stored, None);
    (root, providers, config)
}

/// Installs the `fake` provider and returns the loaded registry with
/// `roles` configured.
fn rig(roles: serde_json::Value) -> (extensions::Providers, config::Config) {
    rig_with(roles, true)
}

#[test]
fn a_full_reference_resolves_to_its_provider_model_and_level() {
    let (providers, config) = rig(json!({}));
    let resolve = resolver(&providers, &config);
    assert_eq!(
        (resolve.lookup)("fiber:fake/m:high").unwrap(),
        "fake/m:high"
    );
}

#[test]
fn a_reference_without_a_level_resolves_to_its_provider_model() {
    let (providers, config) = rig(json!({}));
    let resolve = resolver(&providers, &config);
    assert_eq!((resolve.lookup)("fiber:fake/m").unwrap(), "fake/m");
}

#[test]
fn a_level_the_model_does_not_take_fails_with_the_thinking_message() {
    let (providers, config) = rig(json!({}));
    let resolve = resolver(&providers, &config);
    let Err(reason) = (resolve.lookup)("fiber:fake/plain:high") else {
        panic!("a level the model does not take resolves");
    };
    assert!(
        reason.contains("not one model `fake/plain` takes"),
        "{reason}"
    );
}

#[test]
fn an_unknown_model_lists_the_valid_references() {
    let (providers, config) = rig(json!({}));
    let resolve = resolver(&providers, &config);
    assert_eq!(
        (resolve.lookup)("fiber:fake/nope").unwrap_err(),
        "Unknown delegate model \"fiber:fake/nope\"."
    );
}

#[test]
fn a_reference_without_the_fiber_prefix_is_refused() {
    let (providers, config) = rig(json!({}));
    let resolve = resolver(&providers, &config);
    // `claude:opus` names another harness, which this ticket does not build.
    assert_eq!(
        (resolve.lookup)("claude:opus").unwrap_err(),
        "Unknown delegate model \"claude:opus\"."
    );
    // `fake/m` would resolve through the plain lookup, but only full
    // `fiber:` references are accepted.
    assert_eq!(
        (resolve.lookup)("fake/m").unwrap_err(),
        "Unknown delegate model \"fake/m\"."
    );
}

#[test]
fn a_bare_id_without_a_provider_is_refused() {
    let (providers, config) = rig(json!({}));
    let resolve = resolver(&providers, &config);
    assert_eq!(
        (resolve.lookup)("fiber:m").unwrap_err(),
        "Unknown delegate model \"fiber:m\"."
    );
}

#[test]
fn odd_references_list_the_valid_references() {
    let (providers, config) = rig(json!({}));
    let resolve = resolver(&providers, &config);
    for reference in ["fiber:", "fiber:/", "fiber:fake/m:", "fiber:fake/"] {
        assert_eq!(
            (resolve.lookup)(reference).unwrap_err(),
            format!("Unknown delegate model {reference:?}."),
            "{reference}"
        );
    }
}

#[test]
fn a_role_resolves_to_its_reference_and_level() {
    let (providers, config) = rig(json!({"deep": "fiber:fake/m:high"}));
    let resolve = resolver(&providers, &config);
    assert_eq!((resolve.lookup)("deep").unwrap(), "fake/m:high");
}

#[test]
fn a_role_object_resolves_through_its_model() {
    let (providers, config) =
        rig(json!({"fast": {"model": "fiber:fake/plain", "credential": "work"}}));
    let resolve = resolver(&providers, &config);
    assert_eq!((resolve.lookup)("fast").unwrap(), "fake/plain");
}

#[test]
fn a_role_to_another_harness_or_unknown_model_is_refused() {
    let (providers, config) = rig(json!({
        "gone": "claude:opus",
        "bare": "fake/m",
        "missing": "fiber:fake/nope",
    }));
    let resolve = resolver(&providers, &config);
    for (name, target) in [
        ("gone", "claude:opus"),
        ("bare", "fake/m"),
        ("missing", "fiber:fake/nope"),
    ] {
        assert_eq!(
            (resolve.lookup)(name).unwrap_err(),
            format!("Role {name:?} maps to {target:?}: Unknown delegate model {target:?}.")
        );
    }
}

#[test]
fn a_role_with_a_level_its_model_does_not_take_is_refused() {
    let (providers, config) = rig(json!({"bad": "fiber:fake/plain:high"}));
    let resolve = resolver(&providers, &config);
    let reason = (resolve.lookup)("bad").unwrap_err();
    assert!(
        reason.starts_with("Role \"bad\" maps to \"fiber:fake/plain:high\": "),
        "{reason}"
    );
    assert!(
        reason.contains("not one model `fake/plain` takes"),
        "{reason}"
    );
}

#[test]
fn roles_do_not_chain() {
    let (providers, config) = rig(json!({"a": "b", "b": "fiber:fake/m"}));
    let resolve = resolver(&providers, &config);
    // `a` maps to `b`, which is resolved as a reference only: no role
    // lookup follows.
    assert_eq!(
        (resolve.lookup)("a").unwrap_err(),
        "Role \"a\" maps to \"b\": Unknown delegate model \"b\"."
    );
    assert_eq!((resolve.lookup)("b").unwrap(), "fake/m");
}

#[test]
fn the_catalogue_lists_roles_then_references_sorted() {
    let (providers, config) = rig(json!({
        "gone": "claude:opus",
        "deep": "fiber:fake/m:high",
        "fast": {"model": "fiber:fake/plain"},
    }));
    let resolve = resolver(&providers, &config);
    assert_eq!(
        resolve.catalogue.as_ref(),
        "Roles:\ndeep: fiber:fake/m:high\nfast: fiber:fake/plain\ngone: claude:opus (unavailable)\nReferences:\nfiber:fake/m (thinking: low, high)\nfiber:fake/plain"
    );
}

#[test]
fn an_empty_catalogue_reads_none() {
    let (providers, config) = rig_with(json!({}), false);
    let resolve = resolver(&providers, &config);
    assert_eq!(resolve.catalogue.as_ref(), "Roles: none\nReferences: none");
}

#[test]
fn lookup_and_catalogue_agree_for_every_role() {
    let (providers, config) = rig(json!({
        "gone": "claude:opus",
        "deep": "fiber:fake/m:high",
        "fast": {"model": "fiber:fake/plain"},
    }));
    let resolve = resolver(&providers, &config);
    let catalogue = resolve.catalogue.as_ref().to_owned();
    let mut sections = catalogue.split("\nReferences:\n");
    let roles = sections
        .next()
        .expect("a roles section")
        .strip_prefix("Roles:\n")
        .expect("the roles header");
    let references = sections.next().expect("a references section");
    // At least one role on each side, so the test asserts a positive
    // fact about each branch.
    let mut usable = 0;
    let mut unavailable = 0;
    for line in roles.split('\n') {
        let (name, target) = line.split_once(": ").expect("a role line");
        let (target, marked) = match target.strip_suffix(" (unavailable)") {
            Some(target) => (target, true),
            None => (target, false),
        };
        match (resolve.lookup)(name) {
            Ok(resolved) => {
                assert!(!marked, "{name} is marked but resolves");
                assert_eq!(format!("fiber:{resolved}"), target);
                usable += 1;
            }
            Err(_) => {
                assert!(marked, "{name} resolves nothing but is unmarked");
                unavailable += 1;
            }
        }
    }
    assert!(usable > 0 && unavailable > 0);
    // Every listed reference looks up `Ok`.
    let mut seen = 0;
    for line in references.split('\n') {
        let reference = line.split_once(' ').map(|(head, _)| head).unwrap_or(line);
        assert!((resolve.lookup)(reference).is_ok(), "{reference}");
        seen += 1;
    }
    assert!(seen > 0);
}

/// The launch the runner receives for one delegate.
fn launched() -> jobs::Launched {
    jobs::Launched {
        depth: 1,
        session_id: SessionId("s_child".into()),
        job_id: contract::JobId("j_1".into()),
        parent: SessionId("s_parent".into()),
        prompt: "scan the tree".to_owned(),
        workspace: std::path::PathBuf::from("/w"),
        start: jobs::Start::Fresh(jobs::Fresh::Spawn {
            model: "fake/m:high".to_owned(),
            credential: None,
        }),
    }
}

#[test]
fn the_launch_runs_the_child_session_with_its_ids_model_and_prompt() {
    let launched = launched();
    let command = launcher(std::path::Path::new("/bin/fiber"))(&launched);
    assert_eq!(command.get_program(), "/bin/fiber");
    let argv: Vec<_> = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        argv,
        [
            "session",
            "--id=s_child",
            "--workspace=/w",
            "--model=fake/m:high",
            "--prompt=scan the tree",
            "--parent=s_parent",
            "--delegate-id=j_1",
            "--depth=1",
        ]
    );
}

#[test]
fn launch_carries_the_childs_depth() {
    let mut launched = launched();
    launched.depth = 2;
    let command = launcher(std::path::Path::new("/bin/fiber"))(&launched);
    let argv: Vec<_> = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert!(
        argv.windows(2)
            .any(|args| args == ["--delegate-id=j_1", "--depth=2"])
    );
}

fn tool_depth() -> u32 {
    let dir = fakes::TempDir::new("fiber-delegate-depth");
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let jobs = jobs::Registry::new(
        dir.path().join("artifacts"),
        Arc::clone(&clock),
        Arc::new(fakes::Recorder::default()),
    );
    super::Delegates::new(
        "/bin/fiber".into(),
        dir.path().into(),
        SessionId("s_parent".into()),
        dir.path().into(),
        dir.path().join("sessions"),
        jobs,
        clock,
        jobs::Resolve {
            lookup: Arc::new(|_| Err("unused".into())),
            catalogue: Arc::from("unused"),
            credential: Arc::new(|_| Ok(None)),
        },
    )
    .tool()
    .depth
}

#[test]
fn top_level_depth_defaults_to_zero() {
    assert_eq!(super::depth(), 0);
    assert_eq!(tool_depth(), 0);
}

#[test]
fn the_first_session_depth_reaches_its_tool() {
    super::set_depth(1);
    super::set_depth(2);
    assert_eq!(super::depth(), 1);
    assert_eq!(tool_depth(), 1);
}

#[test]
fn a_delegate_of_a_login_shell_session_carries_the_flag() {
    r#loop::set_variables_source(contract::events::VariablesSource::LoginShell);
    let launched = launched();
    let argv: Vec<_> = launcher(std::path::Path::new("/bin/fiber"))(&launched)
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert_eq!(argv.last().map(String::as_str), Some("--login-shell"));
}

/// Serves one subscription on the socket `home/run/<id>`: acknowledges it,
/// then sends one `fiber_exited` line per `seq`, each carrying it, and
/// closes. The thread owns the listener and ends once it served the one
/// connection.
fn serve(home: &std::path::Path, id: &str, seqs: Vec<Option<u64>>) -> std::thread::JoinHandle<()> {
    let run = home.join("run");
    std::fs::create_dir_all(&run).unwrap();
    // Bound before returning, so the socket file exists when the watch
    // connects; only the accept runs on the thread.
    let listener = UnixListener::bind(run.join(id)).unwrap();
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let command: serde_json::Value = serde_json::from_str(&line).unwrap();
        let accepted = json!({
            "kind": "command_accepted",
            "payload": {"command_id": command["id"]},
            "schema_version": contract::SCHEMA_VERSION,
            "ts": 0,
        });
        writeln!(writer, "{accepted}").unwrap();
        for seq in seqs {
            let line = match seq {
                Some(seq) => json!({
                    "kind": "fiber_exited",
                    "session_id": "s_test",
                    "ts": 0,
                    "schema_version": contract::SCHEMA_VERSION,
                    "seq": seq,
                    "payload": {},
                }),
                None => json!({
                    "kind": "fiber_exited",
                    "session_id": "s_test",
                    "ts": 0,
                    "schema_version": contract::SCHEMA_VERSION,
                    "payload": {},
                }),
            };
            writeln!(writer, "{line}").unwrap();
        }
    })
}

#[test]
fn the_watch_maps_the_socket_exit_to_the_runner_exit() {
    let root = fakes::TempDir::new("dw");
    let home = root.path().join("home");
    let _served = serve(&home, "s_exited", vec![None]);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let watched = watch_bounded(&watcher(&home), "s_exited", None, &seen).unwrap();
    assert!(matches!(watched, jobs::Watched::Exited));
    assert_eq!(*seen.lock().unwrap(), ["fiber_exited"]);
}

#[test]
fn the_watch_maps_a_close_before_the_exit_to_the_runner_close() {
    let root = fakes::TempDir::new("dw");
    let home = root.path().join("home");
    let _served = serve(&home, "s_closed", vec![]);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let watched = watch_bounded(&watcher(&home), "s_closed", None, &seen).unwrap();
    assert!(matches!(watched, jobs::Watched::Closed));
}

#[test]
fn a_refused_watch_is_an_error() {
    let root = fakes::TempDir::new("dw");
    let home = root.path().join("home");
    std::fs::create_dir_all(home.join("run")).unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    assert!(watch_bounded(&watcher(&home), "s_missing", None, &seen).is_err());
}

/// Runs the watch on a thread and receives its outcome within the shared must-succeed bound: every
/// blocking socket read happens there, so a watch that never returns
/// fails the test instead of hanging it. Seen envelope kinds arrive over
/// the shared record.
#[track_caller]
fn watch_bounded(
    watch: &jobs::Watch,
    id: &str,
    after: Option<u64>,
    seen: &Arc<Mutex<Vec<String>>>,
) -> std::io::Result<jobs::Watched> {
    let watch = Arc::clone(watch);
    let id = SessionId(id.to_owned());
    let seen = Arc::clone(seen);
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut on_line = |envelope: &contract::Envelope| {
            seen.lock().unwrap().push(envelope.kind.clone());
        };
        done.send(watch(&id, after, &mut on_line)).unwrap_or(());
    });
    Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&finished, "the watch to return")
}

fn resumed() -> jobs::Launched {
    jobs::Launched {
        depth: 1,
        session_id: SessionId("s_child".into()),
        job_id: contract::JobId("j_1".into()),
        parent: SessionId("s_parent".into()),
        prompt: "- also check Y".to_owned(),
        workspace: std::path::PathBuf::from("/w"),
        start: jobs::Start::Resume,
    }
}

#[test]
fn a_resume_builds_the_resume_argv_with_the_message_whole_and_no_model() {
    let command = launcher(std::path::Path::new("/bin/fiber"))(&resumed());
    assert_eq!(command.get_program(), "/bin/fiber");
    let argv: Vec<_> = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    // One `--prompt=<message>` argument, so a message starting with `-`
    // reaches the child whole; no `--model`.
    assert_eq!(
        argv,
        [
            "session",
            "--id=s_child",
            "--workspace=/w",
            "--resume",
            "--prompt=- also check Y",
            "--parent=s_parent",
            "--delegate-id=j_1",
            "--depth=1",
        ]
    );
    assert!(!argv.iter().any(|arg| arg.starts_with("--model")));
}

#[test]
fn a_fork_builds_the_fork_argv_with_no_model_or_credential() {
    let fork = jobs::Launched {
        start: jobs::Start::Fresh(jobs::Fresh::Fork {
            from: jobs::Forking {
                point: contract::shapes::Point {
                    session_id: SessionId("s_parent".into()),
                    seq: contract::Seq(41),
                },
                model: "fiber:parent-model".to_owned(),
            },
        }),
        prompt: "-go".to_owned(),
        ..launched()
    };
    let command = launcher(std::path::Path::new("/bin/fiber"))(&fork);
    assert_eq!(command.get_program(), "/bin/fiber");
    let argv: Vec<_> = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    // One `--forked-from` and one `--forked-seq` in `to_argv` order,
    // with the delegate's ids and no `--model`, `-c` or `--credential`:
    // a fork continues its point on the parent's build.
    assert_eq!(
        argv,
        [
            "session",
            "--id=s_child",
            "--workspace=/w",
            "--prompt=-go",
            "--forked-from=s_parent",
            "--forked-seq=41",
            "--parent=s_parent",
            "--delegate-id=j_1",
            "--depth=1",
        ]
    );
    // Side by side, the same launch as a spawn carries the model, the
    // credential pair and the label, none of which the fork emits.
    let spawned = jobs::Launched {
        start: jobs::Start::Fresh(jobs::Fresh::Spawn {
            model: "fake/m:high".to_owned(),
            credential: Some("work".to_owned()),
        }),
        prompt: "scan the tree".to_owned(),
        ..launched()
    };
    let argv: Vec<_> = launcher(std::path::Path::new("/bin/fiber"))(&spawned)
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert!(
        argv.iter().any(|arg| arg.starts_with("--model")),
        "{argv:?}"
    );
    assert!(argv.iter().any(|arg| arg == "-c"), "{argv:?}");
    assert!(
        argv.iter()
            .any(|arg| arg == "providers.fake.credential=\"work\""),
        "{argv:?}"
    );
    // And the fork argv carries none of those shapes.
    let fork_argv: Vec<_> = launcher(std::path::Path::new("/bin/fiber"))(&fork)
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert!(
        !fork_argv.iter().any(|arg| arg.starts_with("--model")),
        "{fork_argv:?}"
    );
    assert!(!fork_argv.iter().any(|arg| arg == "-c"), "{fork_argv:?}");
    assert!(
        !fork_argv.iter().any(|arg| arg.starts_with("--credential")),
        "{fork_argv:?}"
    );
}

#[test]
fn a_resume_of_a_login_shell_session_carries_the_flag_last() {
    r#loop::set_variables_source(contract::events::VariablesSource::LoginShell);
    let resumed = resumed();
    let argv: Vec<_> = launcher(std::path::Path::new("/bin/fiber"))(&resumed)
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert_eq!(argv.last().map(String::as_str), Some("--login-shell"));
    // A new delegate's argv is unchanged.
    let launched = launched();
    let argv: Vec<_> = launcher(std::path::Path::new("/bin/fiber"))(&launched)
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert_eq!(argv.last().map(String::as_str), Some("--login-shell"));
}

#[test]
fn the_watch_skips_a_replayed_exit_at_or_before_the_boundary() {
    let root = fakes::TempDir::new("dw");
    let home = root.path().join("home");
    let _served = serve(&home, "s_replay", vec![Some(5)]);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let watched = watch_bounded(&watcher(&home), "s_replay", Some(5), &seen).unwrap();
    assert!(matches!(watched, jobs::Watched::Closed));
    assert!(seen.lock().unwrap().is_empty(), "no line passes");
}

#[test]
fn the_watch_without_a_boundary_takes_the_replayed_exit() {
    let root = fakes::TempDir::new("dw");
    let home = root.path().join("home");
    let _served = serve(&home, "s_replay", vec![Some(5)]);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let watched = watch_bounded(&watcher(&home), "s_replay", None, &seen).unwrap();
    assert!(matches!(watched, jobs::Watched::Exited));
    assert_eq!(*seen.lock().unwrap(), ["fiber_exited"]);
}

/// Serves one steer on the socket `home/run/<id>`: answers the subscribe
/// with this build's schema, reads the steer line and the tools probe,
/// answers the probe, then answers the steer. Bound before returning, so
/// the socket exists when the seam connects; the thread owns the listener
/// and serves the one connection. The parsed steer line comes back over
/// the channel: `None` when the client closed after the subscribe, so no
/// byte of the steer line was written.
fn serve_steer(
    home: &std::path::Path,
    id: &str,
    reject: bool,
) -> (
    std::thread::JoinHandle<()>,
    std::sync::mpsc::Receiver<Option<serde_json::Value>>,
) {
    let run = home.join("run");
    std::fs::create_dir_all(&run).unwrap();
    // Bound before returning, so the socket file exists when the seam
    // connects; only the accept runs on the thread.
    let listener = UnixListener::bind(run.join(id)).unwrap();
    let (seen, received) = std::sync::mpsc::channel();
    let served = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let sub: serde_json::Value = serde_json::from_str(&line).unwrap();
        let accepted = json!({
            "kind": "command_accepted",
            "payload": {"command_id": sub["id"]},
            "schema_version": contract::SCHEMA_VERSION,
            "ts": 0,
        });
        writeln!(writer, "{accepted}").unwrap();
        // The steer line, unless the commit refused it and the client
        // closed instead.
        line.clear();
        if reader.read_line(&mut line).unwrap() == 0 {
            seen.send(None).unwrap_or(());
            return;
        }
        let steered: serde_json::Value = serde_json::from_str(&line).unwrap();
        // The tools probe follows the steer line on the same connection.
        line.clear();
        reader.read_line(&mut line).unwrap();
        let probe: serde_json::Value = serde_json::from_str(&line).unwrap();
        // The probe's answer proves the message is queued, so it goes
        // first; the steer's answer ends the attempt.
        let probe_ok = json!({
            "kind": "command_accepted",
            "payload": {"command_id": probe["id"]},
            "schema_version": contract::SCHEMA_VERSION,
            "ts": 0,
        });
        writeln!(writer, "{probe_ok}").unwrap();
        let answer = if reject {
            json!({
                "kind": "command_rejected",
                "payload": {
                    "command_id": steered["id"],
                    "code": "message_refused",
                    "message": "no thanks",
                },
                "schema_version": contract::SCHEMA_VERSION,
                "ts": 0,
            })
        } else {
            json!({
                "kind": "command_accepted",
                "payload": {"command_id": steered["id"]},
                "schema_version": contract::SCHEMA_VERSION,
                "ts": 0,
            })
        };
        writeln!(writer, "{answer}").unwrap();
        seen.send(Some(steered)).unwrap_or(());
    });
    (served, received)
}

/// Runs the steer seam on its own thread and receives its outcome within the
/// shared must-succeed bound: every blocking socket read happens there, so a steer that never
/// returns fails the test instead of hanging it. `commit` and `on_queued`
/// count their calls in the returned counters.
#[track_caller]
fn steer_bounded(
    steer: &jobs::Steer,
    id: &SessionId,
    command: &str,
    text: &str,
    commit: bool,
) -> (
    jobs::Steered,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let steer = Arc::clone(steer);
    let id = id.clone();
    let command = command.to_owned();
    let text = text.to_owned();
    let commits = Arc::new(AtomicUsize::new(0));
    let queued = Arc::new(AtomicUsize::new(0));
    let (done, finished) = std::sync::mpsc::channel();
    let counted = Arc::clone(&commits);
    let probed = Arc::clone(&queued);
    std::thread::spawn(move || {
        let mut commit_once = || {
            counted.fetch_add(1, Ordering::SeqCst);
            commit
        };
        let mut on_queued = || {
            probed.fetch_add(1, Ordering::SeqCst);
        };
        done.send(steer(
            &id,
            &command,
            &text,
            &mut commit_once,
            &mut on_queued,
        ))
        .unwrap_or(());
    });
    let outcome =
        Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&finished, "the steer to return");
    (outcome, commits, queued)
}

#[test]
fn the_steer_seam_maps_an_accepted_steer() {
    use std::sync::atomic::Ordering;
    let root = fakes::TempDir::new("dm-steer");
    let home = root.path().join("home");
    let id = SessionId("s_steer_ok".into());
    let (_served, seen) = serve_steer(&home, &id.0, false);
    let (outcome, commits, queued) =
        steer_bounded(&super::steerer(&home), &id, "c_1", "check the lint", true);
    assert!(matches!(outcome, jobs::Steered::Accepted), "{outcome:?}");
    assert_eq!(commits.load(Ordering::SeqCst), 1);
    assert_eq!(queued.load(Ordering::SeqCst), 1);
    let line = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv_or_fail(&seen, "the server to observe the steer")
        .expect("the client wrote the steer line");
    assert_eq!(line["id"], "c_1");
    assert_eq!(line["command"], "steer");
    assert_eq!(
        line["args"]["content"],
        json!([{"type": "text", "text": "check the lint"}])
    );
}

#[test]
fn the_steer_seam_maps_a_rejected_steer() {
    use std::sync::atomic::Ordering;
    let root = fakes::TempDir::new("dm-steer");
    let home = root.path().join("home");
    let id = SessionId("s_steer_no".into());
    let (_served, seen) = serve_steer(&home, &id.0, true);
    let (outcome, commits, queued) =
        steer_bounded(&super::steerer(&home), &id, "c_2", "check the lint", true);
    let jobs::Steered::Rejected(failure) = outcome else {
        panic!("a refused steer is not rejected: {outcome:?}");
    };
    assert_eq!(failure.code, contract::ErrorCode::MessageRefused);
    assert_eq!(failure.message, "no thanks");
    assert_eq!(commits.load(Ordering::SeqCst), 1);
    assert_eq!(queued.load(Ordering::SeqCst), 1);
    let line = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv_or_fail(&seen, "the server to observe the steer")
        .expect("the client wrote the steer line");
    assert_eq!(line["id"], "c_2");
}

#[test]
fn the_steer_seam_maps_a_missing_socket_to_gone() {
    use std::sync::atomic::Ordering;
    let root = fakes::TempDir::new("dm-steer");
    let home = root.path().join("home");
    std::fs::create_dir_all(home.join("run")).unwrap();
    let id = SessionId("s_steer_gone".into());
    let (outcome, commits, _queued) =
        steer_bounded(&super::steerer(&home), &id, "c_3", "check the lint", true);
    assert!(matches!(outcome, jobs::Steered::Gone), "{outcome:?}");
    assert_eq!(commits.load(Ordering::SeqCst), 0);
}

#[test]
fn the_steer_seam_maps_a_refused_commit_to_withheld() {
    let root = fakes::TempDir::new("dm-steer");
    let home = root.path().join("home");
    let id = SessionId("s_steer_hold".into());
    let (_served, seen) = serve_steer(&home, &id.0, false);
    let (outcome, _, _) =
        steer_bounded(&super::steerer(&home), &id, "c_4", "check the lint", false);
    assert!(matches!(outcome, jobs::Steered::Withheld), "{outcome:?}");
    // The server saw the subscribe, then end of file: no steer line.
    let line =
        Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&seen, "the server to observe the close");
    assert!(line.is_none(), "a refused commit wrote: {line:?}");
}

/// Writes `lines` as the log `<sessions>/<sid>/events.jsonl`, each with a
/// trailing newline.
fn write_log(sessions: &std::path::Path, sid: &str, lines: &[serde_json::Value]) {
    let dir = sessions.join(sid);
    std::fs::create_dir_all(&dir).unwrap();
    let mut text = String::new();
    for line in lines {
        text.push_str(&line.to_string());
        text.push('\n');
    }
    std::fs::write(dir.join("events.jsonl"), text).unwrap();
}

/// One envelope line carrying `seq`, in the served shape.
fn envelope(seq: u64) -> serde_json::Value {
    json!({
        "kind": "fiber_exited",
        "session_id": "s_test",
        "ts": 0,
        "schema_version": contract::SCHEMA_VERSION,
        "seq": seq,
        "payload": {},
    })
}

#[test]
fn the_last_seq_seam_reads_the_last_complete_lines_seq() {
    let root = fakes::TempDir::new("dm-seq");
    let sessions = root.path().join("sessions");
    write_log(&sessions, "s_seq", &[envelope(1), envelope(7)]);
    let last = super::last_seq_reader(&sessions)(&SessionId("s_seq".into()));
    assert_eq!(last, Ok(Some(7)));
}

#[test]
fn the_last_seq_seam_reads_an_empty_log_as_none() {
    let root = fakes::TempDir::new("dm-seq");
    let sessions = root.path().join("sessions");
    write_log(&sessions, "s_empty", &[]);
    let last = super::last_seq_reader(&sessions)(&SessionId("s_empty".into()));
    assert_eq!(last, Ok(None));
}

#[test]
fn the_last_seq_seam_names_a_missing_log_in_its_reason() {
    let root = fakes::TempDir::new("dm-seq");
    let sessions = root.path().join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let Err(reason) = super::last_seq_reader(&sessions)(&SessionId("s_missing".into())) else {
        panic!("a missing log reads a boundary");
    };
    assert!(
        reason.contains("sessions/s_missing/events.jsonl"),
        "{reason}"
    );
}

/// One session's delegates with its registry held back, for the
/// `message()` wiring cases.
fn message_rig() -> (super::Delegates, Arc<jobs::Registry>) {
    let dir = fakes::TempDir::new("fiber-delegate-message");
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let jobs = jobs::Registry::new(
        dir.path().join("artifacts"),
        Arc::clone(&clock),
        Arc::new(fakes::Recorder::default()),
    );
    let delegates = super::Delegates::new(
        dir.path().join("fiber-stub"),
        dir.path().into(),
        SessionId("s_parent".into()),
        dir.path().into(),
        dir.path().join("sessions"),
        Arc::clone(&jobs),
        clock,
        jobs::Resolve {
            lookup: Arc::new(|_| Err("unused".into())),
            catalogue: Arc::from("unused"),
            credential: Arc::new(|_| Ok(None)),
        },
    );
    (delegates, jobs)
}

#[test]
fn the_message_tool_shares_the_spawn_tools_registry_launch_watch_and_depth() {
    use contract::tool::Tool as _;
    let (delegates, jobs) = message_rig();
    let message = delegates.message();
    let spawned = delegates.tool();
    assert_eq!(message.depth, spawned.depth);
    assert_eq!(message.depth, 0);
    assert!(Arc::ptr_eq(&message.registry, &jobs));
    assert!(Arc::ptr_eq(&message.launch, &spawned.launch));
    assert!(Arc::ptr_eq(&message.watch, &spawned.watch));
    assert!(Arc::ptr_eq(&message.steer, &delegates.steer));
    assert!(Arc::ptr_eq(&message.last_seq, &delegates.last_seq));
    assert_eq!(message.parent, SessionId("s_parent".into()));
    assert_eq!(message.bound, doors::SHUTDOWN_BOUND);
    assert_eq!(message.cap, super::OUTPUT_CAP);
    assert_eq!(message.definition().name, "delegate_message");
}

#[test]
fn the_session_depth_reaches_the_message_tool() {
    super::set_depth(1);
    super::set_depth(2);
    assert_eq!(super::depth(), 1);
    let (delegates, _jobs) = message_rig();
    assert_eq!(delegates.message().depth, 1);
}

#[test]
fn a_role_object_with_a_stored_label_resolves_to_that_label() {
    let (_root, providers, config) = stored_rig(
        json!({"fast": {"model": "fiber:fake/plain", "credential": "work"}}),
        &["work"],
    );
    let resolve = resolver(&providers, &config);
    assert_eq!((resolve.lookup)("fast").unwrap(), "fake/plain");
    assert_eq!((resolve.credential)("fast"), Ok(Some("work".to_owned())));
}

#[test]
fn a_role_naming_a_label_the_provider_lacks_is_refused_naming_the_labels() {
    let (_root, providers, config) = stored_rig(
        json!({"fast": {"model": "fiber:fake/plain", "credential": "gone"}}),
        &["work"],
    );
    let resolve = resolver(&providers, &config);
    assert_eq!((resolve.lookup)("fast").unwrap(), "fake/plain");
    // The refusal names the label asked for and every label the provider
    // has, sorted; the model sees only role names, never labels, so the
    // catalogue stays without it.
    assert_eq!(
        (resolve.credential)("fast"),
        Err(
            "`fake` has no credential label `gone`. The labels for `fake` are: default, work"
                .to_owned()
        )
    );
}

#[test]
fn a_scripted_provider_role_with_a_stored_label_is_refused_with_credential_missing() {
    let root = fakes::TempDir::new("fd-scripted-label");
    let (mut providers, config) = rig_loaded(
        &root,
        json!({"s": {"model": "fiber:scripted/a.json", "credential": "work"}}),
        true,
        &["work"],
        None,
    );
    // The label is stored for the scripted provider too: membership alone
    // must not let it through.
    config::store_credential(
        &root.path().join("home"),
        "scripted",
        "work",
        &config::Secret::new("work-key".to_owned()),
    )
    .unwrap();
    providers.add_scripted("scripted/a.json");
    let resolve = resolver(&providers, &config);
    assert_eq!((resolve.lookup)("s").unwrap(), "scripted/a.json");
    assert_eq!(
        (resolve.credential)("s"),
        Err(
            "`scripted` has no credential label `work`. The labels for `scripted` are: none"
                .to_owned()
        )
    );
}

#[test]
fn a_full_reference_resolves_with_no_label() {
    let (providers, config) = rig(json!({}));
    let resolve = resolver(&providers, &config);
    // A delegate started with a full reference uses the provider's
    // configured label, so no override travels.
    assert_eq!((resolve.credential)("fiber:fake/m"), Ok(None));
}

#[test]
fn a_string_role_resolves_with_no_label() {
    let (providers, config) = rig(json!({"deep": "fiber:fake/m:high"}));
    let resolve = resolver(&providers, &config);
    assert_eq!((resolve.credential)("deep"), Ok(None));
}

#[test]
fn a_repository_layer_role_credential_is_ignored() {
    let root = fakes::TempDir::new("fd-repo");
    let (providers, config) = rig_loaded(
        &root,
        json!({}),
        true,
        &["work"],
        Some(&json!({
            "fast": {"model": "fiber:fake/plain", "credential": "work"},
            "odd": {"model": "fiber:fake/plain", "credential": 5},
        })),
    );
    let resolve = resolver(&providers, &config);
    // A repository's role cannot name a credential: the label is ignored,
    // and a non-string `credential` reads as no label too. The load raises
    // one ignored-key notice per role carrying one, naming the key.
    let notices = config.notices();
    assert_eq!(notices.len(), 2, "{notices:?}");
    assert!(
        notices
            .iter()
            .all(|notice| notice.code == ErrorCode::ConfigKeyIgnored),
        "{notices:?}"
    );
    let file = root.path().join("workspace/.fiber/config.json");
    for name in ["fast", "odd"] {
        assert!(
            notices.iter().any(|notice| notice
                .message
                .contains(&format!("roles.\"{name}\".credential"))
                && notice.message.contains(&file.display().to_string())),
            "{notices:?}"
        );
    }
    assert_eq!((resolve.credential)("fast"), Ok(None));
    assert_eq!((resolve.credential)("odd"), Ok(None));
    // The launch then passes no `-c`: the child reads the provider's
    // configured label.
    let mut launch = launched();
    let jobs::Start::Fresh(jobs::Fresh::Spawn { credential, .. }) = &mut launch.start else {
        panic!("a new launch is a fresh spawn");
    };
    *credential = (resolve.credential)("fast").unwrap();
    let argv: Vec<_> = launcher(std::path::Path::new("/bin/fiber"))(&launch)
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert!(!argv.iter().any(|arg| arg == "-c"), "{argv:?}");
}

#[test]
fn the_catalogue_never_prints_a_role_label() {
    let object = rig(json!({"fast": {"model": "fiber:fake/plain", "credential": "work"}}));
    let string = rig(json!({"fast": "fiber:fake/plain"}));
    let labelled = resolver(&object.0, &object.1).catalogue;
    let plain = resolver(&string.0, &string.1).catalogue;
    assert_eq!(labelled.as_ref(), plain.as_ref());
    assert!(!labelled.contains("work"), "{labelled}");
}

#[test]
fn a_new_launch_with_a_credential_passes_it_as_a_run_override() {
    let mut launch = launched();
    let jobs::Start::Fresh(jobs::Fresh::Spawn { credential, .. }) = &mut launch.start else {
        panic!("a new launch is a fresh spawn");
    };
    *credential = Some("work".to_owned());
    let argv: Vec<_> = launcher(std::path::Path::new("/bin/fiber"))(&launch)
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    // The override travels as one adjacent `-c` pair.
    assert!(
        argv.windows(2)
            .any(|pair| pair == ["-c", "providers.fake.credential=\"work\"",]),
        "{argv:?}"
    );
}

#[test]
fn a_label_that_parses_as_json_is_still_a_string() {
    let mut launch = launched();
    let jobs::Start::Fresh(jobs::Fresh::Spawn { credential, .. }) = &mut launch.start else {
        panic!("a new launch is a fresh spawn");
    };
    *credential = Some("123".to_owned());
    let argv: Vec<_> = launcher(std::path::Path::new("/bin/fiber"))(&launch)
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert!(
        argv.windows(2)
            .any(|pair| pair == ["-c", "providers.fake.credential=\"123\"",]),
        "{argv:?}"
    );
}

#[test]
fn a_resume_launch_passes_no_credential_override() {
    // A resume carries no credential: the type makes one
    // unrepresentable, so the recorded session keeps its label and no
    // `-c` may travel.
    let launch = resumed();
    let command = launcher(std::path::Path::new("/bin/fiber"))(&launch);
    let argv: Vec<_> = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    // A resume continues the recorded session: no `-c` may change its label.
    assert!(!argv.iter().any(|arg| arg == "-c"), "{argv:?}");
}

fn quota_row(provider: &str, credential: &str, entry: QuotaEntry, pending: bool) -> LabelQuota {
    LabelQuota {
        provider: provider.to_owned(),
        credential: credential.to_owned(),
        entry,
        pending,
    }
}

fn window(name: &str, used: f64, resets_at: Option<u64>) -> Window {
    Window {
        name: name.to_owned(),
        used: WindowUse::Percent {
            used,
            resets_at: resets_at.map(|secs| UNIX_EPOCH + Duration::from_secs(secs)),
        },
    }
}

#[test]
fn quota_text_of_a_loading_view_is_quota_loading() {
    let view = QuotaView::Loading;
    // The loading state renders before any per-label entry.
    assert_eq!(quota_text(&view), "Quota: loading");
}

#[test]
fn quota_text_of_nothing_loaded_is_quota_none() {
    let view = QuotaView::Loaded(Vec::new());
    assert_eq!(quota_text(&view), "Quota: none");
}

#[test]
fn a_windows_row_prints_percent_used_and_a_reset_window_prints_reset() {
    let view = QuotaView::Loaded(vec![
        quota_row(
            "p",
            "a",
            QuotaEntry::Reported(Quota::Windows(vec![
                window("alpha", 42.4, None),
                Window {
                    name: "beta".to_owned(),
                    used: WindowUse::Reset,
                },
            ])),
            false,
        ),
        quota_row(
            "p",
            "b",
            QuotaEntry::Reported(Quota::Windows(vec![window("w", 42.5, None)])),
            false,
        ),
        quota_row(
            "p",
            "c",
            QuotaEntry::Reported(Quota::Windows(vec![window("zero", 0.0, None)])),
            false,
        ),
        quota_row(
            "p",
            "d",
            QuotaEntry::Reported(Quota::Windows(vec![window("full", 100.0, None)])),
            false,
        ),
    ]);
    assert_eq!(
        quota_text(&view),
        "Quota:\np/a: alpha 42% used, beta reset\np/b: w 43% used\np/c: zero 0% used\np/d: full 100% used"
    );
}

#[test]
fn a_window_with_a_reset_time_prints_it_in_utc() {
    let view = QuotaView::Loaded(vec![quota_row(
        "p",
        "a",
        QuotaEntry::Reported(Quota::Windows(vec![
            window("w", 10.0, Some(90_061)),
            window("plain", 5.0, None),
        ])),
        false,
    )]);
    assert_eq!(
        quota_text(&view),
        "Quota:\np/a: w 10% used (resets 1970-01-02T01:01:01Z), plain 5% used"
    );
}

#[test]
fn a_credit_row_prints_remaining_and_limit_or_remaining_alone() {
    let view = QuotaView::Loaded(vec![
        quota_row(
            "p",
            "a",
            QuotaEntry::Reported(Quota::Credit {
                remaining: 4.2,
                limit: Some(10.0),
            }),
            false,
        ),
        quota_row(
            "p",
            "b",
            QuotaEntry::Reported(Quota::Credit {
                remaining: 4.2,
                limit: None,
            }),
            false,
        ),
    ]);
    assert_eq!(
        quota_text(&view),
        "Quota:\np/a: credit 4.2 remaining of 10\np/b: credit 4.2 remaining"
    );
}

#[test]
fn a_no_quota_row_prints_no_quota_reported() {
    let view = QuotaView::Loaded(vec![quota_row("p", "a", QuotaEntry::NoQuota, false)]);
    assert_eq!(quota_text(&view), "Quota:\np/a: no quota reported");
}

#[test]
fn an_empty_windows_report_prints_no_quota_reported() {
    let view = QuotaView::Loaded(vec![quota_row(
        "p",
        "a",
        QuotaEntry::Reported(Quota::Windows(Vec::new())),
        false,
    )]);
    assert_eq!(quota_text(&view), "Quota:\np/a: no quota reported");
}

#[test]
fn an_unreachable_row_prints_last_value_and_age_or_nothing() {
    let last = |age: Duration| QuotaEntry::Unreachable {
        last: Some((Quota::Windows(vec![window("primary", 80.0, None)]), age)),
    };
    let view = QuotaView::Loaded(vec![
        quota_row("p", "a", last(Duration::from_secs(59)), false),
        quota_row("p", "b", last(Duration::from_secs(60)), false),
        quota_row("p", "c", last(Duration::from_secs(3599)), false),
        quota_row("p", "d", last(Duration::from_secs(3600)), false),
        quota_row("p", "e", last(Duration::from_secs(86399)), false),
        quota_row("p", "f", last(Duration::from_secs(86400)), false),
        quota_row("p", "g", QuotaEntry::Unreachable { last: None }, false),
    ]);
    assert_eq!(
        quota_text(&view),
        "Quota:\np/a: unreachable (last: primary 80% used, 59s ago)\n\
         p/b: unreachable (last: primary 80% used, 1m ago)\n\
         p/c: unreachable (last: primary 80% used, 59m ago)\n\
         p/d: unreachable (last: primary 80% used, 1h ago)\n\
         p/e: unreachable (last: primary 80% used, 23h ago)\n\
         p/f: unreachable (last: primary 80% used, 1d ago)\n\
         p/g: unreachable"
    );
}

#[test]
fn a_pending_label_prints_no_value_yet_and_a_failed_first_fetch_prints_unreachable() {
    let view = QuotaView::Loaded(vec![
        quota_row("p", "a", QuotaEntry::Unreachable { last: None }, true),
        quota_row("p", "b", QuotaEntry::Unreachable { last: None }, false),
    ]);
    assert_eq!(
        quota_text(&view),
        "Quota:\np/a: no value yet\np/b: unreachable"
    );
}

#[test]
fn quota_rows_sort_by_provider_then_credential() {
    let view = QuotaView::Loaded(vec![
        quota_row("z", "b", QuotaEntry::NoQuota, false),
        quota_row("a", "z", QuotaEntry::NoQuota, false),
        quota_row("a", "a", QuotaEntry::NoQuota, false),
    ]);
    assert_eq!(
        quota_text(&view),
        "Quota:\na/a: no quota reported\na/z: no quota reported\nz/b: no quota reported"
    );
}

/// One session's delegates with `catalogue`, for the models wiring tests.
fn models_rig(catalogue: &str) -> super::Delegates {
    let dir = fakes::TempDir::new("fiber-delegate-models");
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let jobs = jobs::Registry::new(
        dir.path().join("artifacts"),
        Arc::clone(&clock),
        Arc::new(fakes::Recorder::default()),
    );
    super::Delegates::new(
        dir.path().join("fiber-stub"),
        dir.path().into(),
        SessionId("s_parent".into()),
        dir.path().into(),
        dir.path().join("sessions"),
        jobs,
        clock,
        jobs::Resolve {
            lookup: Arc::new(|_| Err("unused".into())),
            catalogue: Arc::from(catalogue.to_owned()),
            credential: Arc::new(|_| Ok(None)),
        },
    )
}

fn models_text(models: &jobs::DelegateModels) -> String {
    use contract::tool::Tool as _;
    let output = models.run(
        &serde_json::Map::new(),
        &fakes::CancelToken::new(),
        &fakes::Recorder::default(),
    );
    let [contract::shapes::ContentPart::Text { text }] = &output.content[..] else {
        panic!("one text part: {:?}", output.content);
    };
    text.clone()
}

#[test]
fn models_without_a_reader_says_quota_none() {
    let text = models_text(&models_rig("CAT").models());
    assert_eq!(text, "CAT\nQuota: none");
}

#[test]
fn models_renders_the_reader_each_call() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    let reader: crate::switch::quota::ModelQuotaRead = Arc::new(move || {
        let call = counted.fetch_add(1, Ordering::SeqCst);
        // Alternate the quota section, so equal texts would prove caching.
        if call == 0 {
            QuotaView::Loading
        } else {
            QuotaView::Loaded(Vec::new())
        }
    });
    let models = models_rig("CAT").with_quota(reader).models();
    assert_eq!(models_text(&models), "CAT\nQuota: loading");
    assert_eq!(models_text(&models), "CAT\nQuota: none");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn models_says_quota_loading_while_the_reader_is_held_then_lists_the_label() {
    let (release, released) = std::sync::mpsc::channel::<()>();
    let released = Arc::new(Mutex::new(released));
    let reader: crate::switch::quota::ModelQuotaRead = Arc::new(move || {
        if released.lock().unwrap().try_recv().is_ok() {
            QuotaView::Loaded(vec![quota_row(
                "p",
                "c",
                QuotaEntry::Reported(Quota::Windows(vec![window("primary", 42.5, None)])),
                false,
            )])
        } else {
            QuotaView::Loading
        }
    });
    let models = models_rig("CAT").with_quota(reader).models();
    // The first call returns while the fake is still held: `run` never
    // waits on the reader.
    let (found, got) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let _sent = found.send(models_text(&models));
        });
        let first = fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&got)
            .expect("the models call never returned");
        assert_eq!(first, "CAT\nQuota: loading");
    });
    // After the release the next call lists the label's entry.
    release.send(()).unwrap();
    assert_eq!(models_text(&models), "CAT\nQuota:\np/c: primary 43% used");
}

/// A session's delegate service with the jobs rig's long-running child:
/// the launcher sleeps, the registry delivers to `inbox`, and `resolve`
/// maps `deep` to the fake model. `launch` is replaced after `new`,
/// because the session's own launcher would re-execute the binary.
struct ServiceRig {
    _dir: fakes::TempDir,
    delegates: super::Delegates,
    inbox: std::sync::mpsc::Receiver<contract::inbox::Delivery>,
}

/// The rig's last child, read from its pidfile, armed as its watchdog so
/// a failing test leaves no child behind: `exec` replaces the shell, so
/// a command-line match would never see the rig's directory.
#[track_caller]
fn child_guard(rig: &ServiceRig) -> fakes::Watchdog {
    let pidfile = rig._dir.path().join("child.pid");
    let pid: u32 = fakes::within(
        "the child writes its pid",
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || loop {
            let Ok(text) = std::fs::read_to_string(&pidfile) else {
                std::thread::yield_now();
                continue;
            };
            let Ok(pid) = text.trim().parse() else {
                std::thread::yield_now();
                continue;
            };
            return pid;
        },
    );
    fakes::Watchdog::group(pid)
}

fn service_rig(depth: u32) -> ServiceRig {
    let dir = fakes::TempDir::new("fiber-delegates-service");
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let artifacts = dir.path().join("artifacts");
    std::fs::create_dir(&artifacts).unwrap();
    let sessions = dir.path().join("sessions");
    std::fs::create_dir(&sessions).unwrap();
    let registry = jobs::Registry::new(
        artifacts,
        Arc::clone(&clock),
        Arc::new(fakes::Recorder::default()),
    );
    let (inbox_tx, inbox_rx) = std::sync::mpsc::channel();
    contract::jobs::Jobs::deliver_to(registry.as_ref(), inbox_tx);
    let pid = dir.path().join("child.pid");
    let launch: jobs::Launch = Arc::new(move |launched: &jobs::Launched| {
        let _ = launched;
        let mut command = std::process::Command::new("sh");
        command.args([
            "-c",
            &format!("echo $$ > '{}'; exec sleep 60", pid.display()),
        ]);
        command
    });
    let mut delegates = super::Delegates::new(
        "/bin/fiber".into(),
        dir.path().into(),
        SessionId("s_parent".into()),
        dir.path().into(),
        sessions,
        registry,
        clock,
        jobs::Resolve {
            lookup: Arc::new(|model: &str| match model {
                "deep" => Ok("fake/m:high".to_owned()),
                _ => Err(format!("Unknown delegate model {model:?}.")),
            }),
            catalogue: Arc::from("Roles:\ndeep: fake/m:high"),
            credential: Arc::new(|_| Ok(None)),
        },
    );
    delegates.launch = launch;
    delegates.depth = depth;
    ServiceRig {
        _dir: dir,
        delegates,
        inbox: inbox_rx,
    }
}

/// Receives the started job's `ExtensionJob`: its send registers the job
/// as running under the same lock, so a later `retain` finds it.
#[track_caller]
fn await_started(rig: &ServiceRig, id: &contract::JobId) {
    let contract::inbox::Delivery::ExtensionJob(started, _) = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&rig.inbox)
        .expect("the start is delivered")
    else {
        panic!("the start precedes the end");
    };
    assert_eq!(&started.job_id, id);
}

/// Waits until the registry lists no running job.
fn await_idle(rig: &ServiceRig) {
    let jobs = Arc::clone(&rig.delegates.jobs);
    fakes::within(
        "the job ends",
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || {
            loop {
                if contract::jobs::Jobs::running(jobs.as_ref()).is_empty() {
                    return;
                }
                std::thread::yield_now();
            }
        },
    );
}

fn arguments() -> serde_json::Map<String, serde_json::Value> {
    serde_json::json!({
        "description": "scan",
        "prompt": "hi",
        "model": "deep",
    })
    .as_object()
    .unwrap()
    .clone()
}

/// `start` from a `Delegates` whose depth is 2 fails `depth_exceeded`
/// without calling `bind`: the guard in `tool()` fires first.
#[test]
fn a_start_at_depth_two_fails_depth_exceeded_without_binding() {
    use contract::extension::HostDelegate;
    let rig = service_rig(2);
    let mut bound = false;
    let error = HostDelegate::start(
        &rig.delegates,
        "fiber.test/x",
        &arguments(),
        &mut |_: &contract::JobId| bound = true,
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::DepthExceeded);
    assert!(!bound, "no binding before the refusal");
}

/// `start` at depth 0 calls `bind` once with the returned id: the
/// registry's `running()` then holds that id, and its inbox gets
/// `Delivery::ExtensionJob` with the extension and no tool.
#[test]
fn a_start_at_depth_zero_binds_and_lists_its_job() {
    use contract::extension::HostDelegate;
    let rig = service_rig(0);
    let mut bound = Vec::new();
    let id = HostDelegate::start(&rig.delegates, "fiber.test/x", &arguments(), &mut |id| {
        bound.push(id.clone())
    })
    .unwrap();
    assert_eq!(bound, vec![id.clone()], "bind runs once with the id");
    assert!(
        contract::jobs::Jobs::running(rig.delegates.jobs.as_ref()).contains(&id),
        "the registry lists the started job"
    );
    let contract::inbox::Delivery::ExtensionJob(started, delegate) =
        Deadline::after(MUST_SUCCEED_WITHIN)
            .recv(&rig.inbox)
            .expect("the start is delivered")
    else {
        panic!("the start precedes the end");
    };
    assert_eq!(started.job_id, id);
    assert_eq!(delegate.job_id, id);
    assert_eq!(started.extension.as_deref(), Some("fiber.test/x"));
    assert_eq!(started.tool, None);
    let _guard = child_guard(&rig);
    assert_eq!(HostDelegate::cancel(&rig.delegates, &[id]), 1);
    await_idle(&rig);
}

/// `retain(&[])` stops the running child and returns 1; `cancel` with
/// that id returns 0 once it has ended.
#[test]
fn retain_stops_what_is_no_longer_loaded_and_cancel_counts_true_stops() {
    use contract::extension::HostDelegate;
    let rig = service_rig(0);
    let mut bound = Vec::new();
    let id = HostDelegate::start(&rig.delegates, "fiber.test/x", &arguments(), &mut |id| {
        bound.push(id.clone())
    })
    .unwrap();
    await_started(&rig, &id);
    assert_eq!(HostDelegate::retain(&rig.delegates, &[]), 1);
    await_idle(&rig);
    assert_eq!(HostDelegate::cancel(&rig.delegates, &[id]), 0);
}

/// One envelope line of a parent's own log for the fork-point reader:
/// `kind` with `seq`, carrying `action_id` when `action` names one and
/// `payload` as its body.
fn point_line(
    kind: &str,
    seq: u64,
    action: Option<&str>,
    payload: serde_json::Value,
) -> serde_json::Value {
    let mut line = json!({
        "kind": kind,
        "session_id": "s_parent",
        "ts": 0,
        "schema_version": contract::SCHEMA_VERSION,
        "seq": seq,
        "payload": payload,
    });
    if let Some(action) = action {
        line["action_id"] = json!(action);
    }
    line
}

/// A `tool_call_requested` line for `name` with `seq` and `action`,
/// carrying `ran_by` when the call ran inside something else.
fn call_line(
    seq: u64,
    action: &str,
    name: &str,
    ran_by: Option<serde_json::Value>,
) -> serde_json::Value {
    let mut payload = json!({"name": name, "arguments": {}});
    if let Some(ran_by) = ran_by {
        payload["ran_by"] = ran_by;
    }
    point_line("tool_call_requested", seq, Some(action), payload)
}

/// A model call's `usage_recorded` for message `action`: its `model` is
/// what the reader reports for the calling message.
fn usage_line(seq: u64, action: &str, model: &str) -> serde_json::Value {
    point_line(
        "usage_recorded",
        seq,
        Some(action),
        json!({
            "generation_id": "g_1",
            "model": model,
            "tokens": {"input": 1, "output": 2},
            "input_bytes": 10,
        }),
    )
}

/// Writes `lines` as the parent's own log for the fork-point reader.
fn point_log(sessions: &std::path::Path, lines: &[serde_json::Value]) {
    write_log(sessions, "s_parent", lines);
}

/// The reader over `sessions`, whose parent is `s_parent`.
fn reader(sessions: &std::path::Path) -> jobs::ForkPoint {
    super::fork_point_reader(sessions, &SessionId("s_parent".into()))
}

/// Reads the point for `action` from the log in `sessions`.
fn read_for(
    sessions: &std::path::Path,
    action: Option<&str>,
) -> Result<jobs::Forking, contract::shapes::Failure> {
    let action = action.map(|action| contract::ActionId(action.to_owned()));
    reader(sessions)(action.as_ref())
}

/// A parent log whose first reply calls `delegate_fork`: the call `a_call`
/// at seq 8, its message `a_msg` at seq 7, and a later message `a_next`
/// at seq 12 that must not move the point.
fn fork_call_log() -> Vec<serde_json::Value> {
    vec![
        point_line("session_started", 0, None, json!({})),
        point_line("fiber_started", 1, None, json!({})),
        point_line("extensions_loaded", 2, None, json!({})),
        point_line("preamble_built", 3, None, json!({})),
        point_line("opening_message", 4, None, json!({})),
        point_line("turn_started", 5, None, json!({})),
        point_line("step_started", 6, None, json!({})),
        point_line("assistant_message_started", 7, Some("a_msg"), json!({})),
        call_line(8, "a_call", "delegate_fork", None),
        usage_line(9, "a_msg", "fiber:pa/m"),
        point_line("assistant_message_completed", 10, Some("a_msg"), json!({})),
        point_line("step_started", 11, None, json!({})),
        point_line("assistant_message_started", 12, Some("a_next"), json!({})),
        point_line("text_completed", 13, Some("a_next"), json!({"text": "hi"})),
        usage_line(14, "a_next", "fiber:pa/m"),
        point_line("assistant_message_completed", 15, Some("a_next"), json!({})),
    ]
}

/// The point is `S - 1`, where `S` is the latest
/// `assistant_message_started` before the call's own
/// `tool_call_requested`: the later message leaves it alone, and the
/// model is the calling message's `usage_recorded.model`.
#[test]
fn the_reader_points_just_before_the_calling_message_with_its_model() {
    let root = fakes::TempDir::new("dm-fork-point");
    let sessions = root.path().join("sessions");
    point_log(&sessions, &fork_call_log());
    let forking = read_for(&sessions, Some("a_call")).unwrap();
    assert_eq!(
        forking.point,
        contract::shapes::Point {
            session_id: SessionId("s_parent".into()),
            seq: contract::Seq(6),
        }
    );
    assert_eq!(forking.model, "fiber:pa/m");
}

/// A parent log where the fork call runs inside an outer model call: the
/// `outer_action_id` chain resolves through `a_outer` to a root with no
/// `ran_by`, so the parent keeps its point and its message's model.
#[test]
fn the_reader_walks_the_outer_chain_to_its_root_call() {
    let root = fakes::TempDir::new("dm-fork-outer");
    let sessions = root.path().join("sessions");
    point_log(
        &sessions,
        &[
            point_line("assistant_message_started", 7, Some("a_first"), json!({})),
            call_line(8, "a_outer", "shell", None),
            usage_line(9, "a_first", "fiber:pa/m"),
            point_line("assistant_message_started", 12, Some("a_msg"), json!({})),
            call_line(
                13,
                "a_call",
                "delegate_fork",
                Some(json!({"extension": "e", "outer_action_id": "a_outer"})),
            ),
            usage_line(14, "a_msg", "fiber:pa/m"),
        ],
    );
    let forking = read_for(&sessions, Some("a_call")).unwrap();
    assert_eq!(forking.point.seq, contract::Seq(11));
    assert_eq!(forking.model, "fiber:pa/m");
}

/// A point whose root is a command (`ran_by.command_id`) is refused with
/// `invalid_arguments`: directly on the call, and through an outer link.
#[test]
fn a_command_root_is_refused_as_invalid_arguments() {
    let root = fakes::TempDir::new("dm-fork-command");
    let sessions = root.path().join("sessions");
    point_log(
        &sessions,
        &[
            point_line("assistant_message_started", 7, Some("a_msg"), json!({})),
            call_line(
                8,
                "a_direct",
                "delegate_fork",
                Some(json!({"extension": "e", "command_id": "c_1"})),
            ),
            usage_line(9, "a_msg", "fiber:pa/m"),
            point_line("assistant_message_started", 12, Some("a_later"), json!({})),
            call_line(
                13,
                "a_nested",
                "delegate_fork",
                Some(json!({"extension": "e", "outer_action_id": "a_outer"})),
            ),
            call_line(
                10,
                "a_outer",
                "shell",
                Some(json!({"extension": "e", "command_id": "c_2"})),
            ),
            usage_line(14, "a_later", "fiber:pa/m"),
        ],
    );
    for action in ["a_direct", "a_nested"] {
        let error = read_for(&sessions, Some(action)).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArguments);
    }
}

/// An outer link the log does not hold, and a cycle back to the call,
/// keep the point and fall back to the calling message's
/// `usage_recorded.model`: the parent never fails on its ancestry.
#[test]
fn an_unreadable_ancestry_keeps_the_point_and_falls_back() {
    let root = fakes::TempDir::new("dm-fork-ancestry");
    let sessions = root.path().join("sessions");
    point_log(
        &sessions,
        &[
            point_line("assistant_message_started", 7, Some("a_msg"), json!({})),
            call_line(
                8,
                "a_dangling",
                "delegate_fork",
                Some(json!({"extension": "e", "outer_action_id": "a_gone"})),
            ),
            usage_line(9, "a_msg", "fiber:pa/m"),
            point_line("assistant_message_started", 12, Some("a_loop"), json!({})),
            call_line(
                13,
                "a_cycle",
                "delegate_fork",
                Some(json!({"extension": "e", "outer_action_id": "a_cycle"})),
            ),
            usage_line(14, "a_loop", "fiber:pa/m"),
        ],
    );
    for action in ["a_dangling", "a_cycle"] {
        let forking = read_for(&sessions, Some(action)).unwrap();
        let expected = if action == "a_dangling" { 6 } else { 11 };
        assert_eq!(forking.point.seq, contract::Seq(expected));
        assert_eq!(forking.model, "fiber:pa/m");
    }
}

/// A failure that reads only the parent's own log is a `tool_error`: a
/// missing log, a line that does not parse, a call the log does not
/// hold, a call with no `seq`, a call with no message before it, and a
/// calling message with no `usage_recorded`.
#[test]
fn own_log_failures_are_tool_errors() {
    let root = fakes::TempDir::new("dm-fork-own-log");
    let sessions = root.path().join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    // A missing log.
    let error = read_for(&sessions, Some("a_call")).unwrap_err();
    assert_eq!(error.code, ErrorCode::ToolError);
    // A line that does not parse.
    point_log(
        &sessions,
        &[point_line("session_started", 0, None, json!({}))],
    );
    std::fs::write(sessions.join("s_parent").join("events.jsonl"), "{nope\n").unwrap();
    let error = read_for(&sessions, Some("a_call")).unwrap_err();
    assert_eq!(error.code, ErrorCode::ToolError);
    // A call the log does not hold.
    point_log(&sessions, &fork_call_log());
    let error = read_for(&sessions, Some("a_gone")).unwrap_err();
    assert_eq!(error.code, ErrorCode::ToolError);
    // A call with no `seq` reads as absent.
    let mut no_seq = fork_call_log();
    no_seq[8].as_object_mut().unwrap().remove("seq");
    point_log(&sessions, &no_seq);
    let error = read_for(&sessions, Some("a_call")).unwrap_err();
    assert_eq!(error.code, ErrorCode::ToolError);
    // A call with no message before it.
    point_log(&sessions, &[call_line(8, "a_call", "delegate_fork", None)]);
    let error = read_for(&sessions, Some("a_call")).unwrap_err();
    assert_eq!(error.code, ErrorCode::ToolError);
    // A calling message with no `usage_recorded`.
    point_log(
        &sessions,
        &[
            point_line("assistant_message_started", 7, Some("a_msg"), json!({})),
            call_line(8, "a_call", "delegate_fork", None),
        ],
    );
    let error = read_for(&sessions, Some("a_call")).unwrap_err();
    assert_eq!(error.code, ErrorCode::ToolError);
}

/// With no calling action, `S` is the latest `assistant_message_started`
/// in the parent's own log; an empty log is a `tool_error`.
#[test]
fn no_calling_action_reads_the_latest_message_in_the_log() {
    let root = fakes::TempDir::new("dm-fork-no-action");
    let sessions = root.path().join("sessions");
    point_log(&sessions, &fork_call_log());
    let forking = read_for(&sessions, None).unwrap();
    assert_eq!(forking.point.seq, contract::Seq(11));
    assert_eq!(forking.model, "fiber:pa/m");
    point_log(&sessions, &[]);
    let error = read_for(&sessions, None).unwrap_err();
    assert_eq!(error.code, ErrorCode::ToolError);
}

/// The fork tool shares the spawn tool's wiring: depth, registry,
/// parent, workspace, sessions, clock, bound, cap, launch and watch.
/// It carries a point reader, not a model resolver.
#[test]
fn the_fork_tool_shares_the_spawn_tools_wiring() {
    use contract::tool::Tool as _;
    let (delegates, jobs) = message_rig();
    let forked = delegates.fork();
    let spawned = delegates.tool();
    assert_eq!(forked.depth, spawned.depth);
    assert!(Arc::ptr_eq(&forked.registry, &jobs));
    assert!(Arc::ptr_eq(&forked.registry, &spawned.registry));
    assert_eq!(forked.parent, spawned.parent);
    assert_eq!(forked.workspace, spawned.workspace);
    assert_eq!(forked.sessions, spawned.sessions);
    assert!(Arc::ptr_eq(&forked.clock, &spawned.clock));
    assert_eq!(forked.bound, spawned.bound);
    assert_eq!(forked.cap, spawned.cap);
    assert!(Arc::ptr_eq(&forked.launch, &spawned.launch));
    assert!(Arc::ptr_eq(&forked.watch, &spawned.watch));
    assert_eq!(forked.definition().name, "delegate_fork");
}

/// N17: with a stored list that lacks `x`, `start` for `x` returns
/// `Ok(id)` and the job is stopped; with `x` listed, it is not.
#[test]
fn a_start_missing_from_the_stored_list_ends_stopped() {
    use contract::extension::HostDelegate;
    let rig = service_rig(0);
    HostDelegate::retain(&rig.delegates, &["fiber.test/kept".to_owned()]);
    let mut bound = Vec::new();
    let id = HostDelegate::start(&rig.delegates, "fiber.test/x", &arguments(), &mut |id| {
        bound.push(id.clone())
    })
    .unwrap();
    assert_eq!(bound, vec![id.clone()]);
    await_started(&rig, &id);
    await_idle(&rig);
    assert!(
        contract::jobs::Jobs::running(rig.delegates.jobs.as_ref()).is_empty(),
        "the unlisted job is stopped"
    );
    HostDelegate::retain(&rig.delegates, &["fiber.test/x".to_owned()]);
    let mut bound = Vec::new();
    let kept = HostDelegate::start(&rig.delegates, "fiber.test/x", &arguments(), &mut |id| {
        bound.push(id.clone())
    })
    .unwrap();
    assert_eq!(bound, vec![kept.clone()]);
    assert!(
        contract::jobs::Jobs::running(rig.delegates.jobs.as_ref()).contains(&kept),
        "the listed job keeps running"
    );
    let _guard = child_guard(&rig);
    assert_eq!(HostDelegate::cancel(&rig.delegates, &[kept]), 1);
    await_idle(&rig);
}
