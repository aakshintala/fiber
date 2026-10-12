//! Resolving `fiber:` references, building the child's command and watching
//! its socket (`docs/delegates.md`, "Choosing a model" and "Lifetime").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use crate::test_support::{install_extension, load};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::sync::{Arc, Mutex};

use contract::SessionId;
use serde_json::json;

use super::{launcher, resolver, watcher};
use fakes::Deadline;

/// Installs a provider `fake` with a leveled model `m` and a plain model
/// `plain` (unless `install` is false), writes `{"roles": <roles>}` to
/// the test home's configuration, and returns the loaded registry with
/// that configuration.
fn rig_with(roles: serde_json::Value, install: bool) -> (extensions::Providers, config::Config) {
    let root = fakes::TempDir::new("fd");
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
                    r#"{"name": "fake", "models": [
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
    std::fs::write(
        home.join("config.json"),
        json!({"roles": roles}).to_string(),
    )
    .unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let config = load(&home, &workspace, "test", Vec::<String>::new());
    let (providers, _notices) = extensions::Providers::load(&home).unwrap();
    // `root` is dropped here; the registry and configuration were read.
    (providers, config)
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
        session_id: SessionId("s_child".into()),
        job_id: contract::JobId("j_1".into()),
        parent: SessionId("s_parent".into()),
        model: "fake/m:high".to_owned(),
        prompt: "scan the tree".to_owned(),
        workspace: std::path::PathBuf::from("/w"),
        resume: false,
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
            "--id",
            "s_child",
            "--workspace",
            "/w",
            "--model",
            "fake/m:high",
            "--prompt",
            "scan the tree",
            "--parent",
            "s_parent",
            "--delegate-id",
            "j_1",
        ]
    );
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
fn serve(
    home: &std::path::Path,
    id: &str,
    seqs: Vec<Option<u64>>,
) -> std::thread::JoinHandle<()> {
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

/// Runs the watch on a thread and receives its outcome within 5 s: every
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
    Deadline::after(std::time::Duration::from_secs(5))
        .recv_or_fail(&finished, "the watch to return")
}

fn resumed() -> jobs::Launched {
    jobs::Launched {
        session_id: SessionId("s_child".into()),
        job_id: contract::JobId("j_1".into()),
        parent: SessionId("s_parent".into()),
        model: "fake/m:high".to_owned(),
        prompt: "- also check Y".to_owned(),
        workspace: std::path::PathBuf::from("/w"),
        resume: true,
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
            "--id",
            "s_child",
            "--workspace",
            "/w",
            "--resume",
            "--prompt=- also check Y",
            "--parent",
            "s_parent",
            "--delegate-id",
            "j_1",
        ]
    );
    assert!(!argv.iter().any(|arg| arg == "--model"));
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
