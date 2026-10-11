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
/// `plain`, and returns the loaded registry with an empty configuration.
fn rig() -> (extensions::Providers, config::Config) {
    let root = fakes::TempDir::new("fd");
    let home = root.path().join("home");
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
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let config = load(&home, &workspace, "test", Vec::<String>::new());
    let (providers, _notices) = extensions::Providers::load(&home).unwrap();
    // `root` is dropped here; the registry and configuration were read.
    (providers, config)
}

/// The valid references the resolver lists, in order.
fn valid() -> Vec<String> {
    vec!["fiber:fake/m".to_owned(), "fiber:fake/plain".to_owned()]
}

#[test]
fn a_full_reference_resolves_to_its_provider_model_and_level() {
    let (providers, config) = rig();
    let resolve = resolver(&providers, &config);
    assert_eq!(resolve("fiber:fake/m:high").unwrap(), "fake/m:high");
}

#[test]
fn a_reference_without_a_level_resolves_to_its_provider_model() {
    let (providers, config) = rig();
    let resolve = resolver(&providers, &config);
    assert_eq!(resolve("fiber:fake/m").unwrap(), "fake/m");
}

#[test]
fn a_level_the_model_does_not_take_fails_with_the_thinking_message() {
    let (providers, config) = rig();
    let resolve = resolver(&providers, &config);
    let Err(valid) = resolve("fiber:fake/plain:high") else {
        panic!("a level the model does not take resolves");
    };
    assert_eq!(valid.len(), 1);
    assert!(
        valid[0].contains("not one model `fake/plain` takes"),
        "{}",
        valid[0]
    );
}

#[test]
fn an_unknown_model_lists_the_valid_references() {
    let (providers, config) = rig();
    let resolve = resolver(&providers, &config);
    assert_eq!(resolve("fiber:fake/nope").unwrap_err(), valid());
}

#[test]
fn a_reference_without_the_fiber_prefix_is_refused() {
    let (providers, config) = rig();
    let resolve = resolver(&providers, &config);
    // `claude:opus` names another harness, which this ticket does not build.
    assert_eq!(resolve("claude:opus").unwrap_err(), valid());
    // `fake/m` would resolve through the plain lookup, but only full
    // `fiber:` references are accepted.
    assert_eq!(resolve("fake/m").unwrap_err(), valid());
}

#[test]
fn a_bare_id_without_a_provider_is_refused() {
    let (providers, config) = rig();
    let resolve = resolver(&providers, &config);
    assert_eq!(resolve("fiber:m").unwrap_err(), valid());
}

#[test]
fn odd_references_list_the_valid_references() {
    let (providers, config) = rig();
    let resolve = resolver(&providers, &config);
    for reference in ["fiber:", "fiber:/", "fiber:fake/m:", "fiber:fake/"] {
        assert_eq!(resolve(reference).unwrap_err(), valid(), "{reference}");
    }
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

/// Serves one subscription on the socket `home/run/<id>`: acknowledges it,
/// then either sends one `fiber_exited` line or closes without one. The
/// thread owns the listener and ends once it served the one connection.
fn serve(home: &std::path::Path, id: &str, exited: bool) -> std::thread::JoinHandle<()> {
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
        if exited {
            let line = json!({
                "kind": "fiber_exited",
                "session_id": "s_test",
                "ts": 0,
                "schema_version": contract::SCHEMA_VERSION,
                "payload": {},
            });
            writeln!(writer, "{line}").unwrap();
        }
    })
}

#[test]
fn the_watch_maps_the_socket_exit_to_the_runner_exit() {
    let root = fakes::TempDir::new("dw");
    let home = root.path().join("home");
    let _served = serve(&home, "s_exited", true);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let watched = watch_bounded(&watcher(&home), "s_exited", &seen).unwrap();
    assert!(matches!(watched, jobs::Watched::Exited));
    assert_eq!(*seen.lock().unwrap(), ["fiber_exited"]);
}

#[test]
fn the_watch_maps_a_close_before_the_exit_to_the_runner_close() {
    let root = fakes::TempDir::new("dw");
    let home = root.path().join("home");
    let _served = serve(&home, "s_closed", false);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let watched = watch_bounded(&watcher(&home), "s_closed", &seen).unwrap();
    assert!(matches!(watched, jobs::Watched::Closed));
}

#[test]
fn a_refused_watch_is_an_error() {
    let root = fakes::TempDir::new("dw");
    let home = root.path().join("home");
    std::fs::create_dir_all(home.join("run")).unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    assert!(watch_bounded(&watcher(&home), "s_missing", &seen).is_err());
}

/// Runs the watch on a thread and receives its outcome within 5 s: every
/// blocking socket read happens there, so a watch that never returns
/// fails the test instead of hanging it. Seen envelope kinds arrive over
/// the shared record.
#[track_caller]
fn watch_bounded(
    watch: &jobs::Watch,
    id: &str,
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
        done.send(watch(&id, &mut on_line)).unwrap_or(());
    });
    Deadline::after(std::time::Duration::from_secs(5))
        .recv(&finished)
        .unwrap_or_else(|_| panic!("the watch returned within 5 s"))
}
