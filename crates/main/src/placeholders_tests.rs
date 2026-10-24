//! `placeholders::lookup` keeps a hub-spawned session's startup snapshot
//! (`docs/invocation.md`, "A session's environment"): the session resolves
//! from the fixed process environment and sends the hub no placeholder
//! query.

#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]

use crate::test_support::{Rig, install_extension, load};
use std::io::{BufRead, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use config::Config;
use extensions::Providers;
use serde_json::json;
use tools::PathLocks;

use fakes::Deadline;

fn setup_home(name: &str) -> (fakes::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let rig = Rig::new(name);
    (rig.root, rig.home, rig.workspace)
}

fn install_acme_with_placeholders(home: &std::path::Path, placeholders: serde_json::Value) {
    install_extension(
        home,
        "extensions/acme",
        json!({"name": "acme", "version": "v0.0.0", "fiber": "v0.0.0", "api": 1}),
        &[(
            "acme",
            json!({
                "name": "acme",
                "placeholders": placeholders,
                "models": [{"id": "m", "protocol": "openai-responses",
                            "base_url": "https://{workspace}/v1", "context_window": 1000}],
            }),
        )],
    );
}

fn load_config(home: &std::path::Path, workspace: &std::path::Path, overrides: &[&str]) -> Config {
    load(home, workspace, "p", overrides)
}
/// A hub-spawned session keeps its startup snapshot: the session process
/// holds snapshot A with the hub's marker, while the hub answers snapshot
/// B, as after a `fiber hub refresh` between spawn and placeholder lookup.
/// `add_lua` resolves from A and sends the hub no query at all. The marker
/// is process state, so the marked lookup runs in a re-run child; the
/// parent serves the hub side and asserts the child resolved A and the hub
/// saw no request.
const SNAPSHOT_CHILD: &str = "FIBER_MAIN_LUA_SNAPSHOT_CHILD";
const SNAPSHOT_HOME: &str = "FIBER_MAIN_LUA_SNAPSHOT_HOME";
const SNAPSHOT_WORKSPACE: &str = "FIBER_MAIN_LUA_SNAPSHOT_WORKSPACE";
/// A placeholder variable name no test process sets: the snapshot test
/// reads its absence first and sets it only in the re-run child.
const SNAPSHOT_HOST: &str = "FIBER_TEST_LUA_SNAPSHOT_HOST";
/// Snapshot A, the session's fixed startup environment, and snapshot B,
/// what the hub answers after the refresh between spawn and lookup.
const SNAPSHOT_A: &str = "snapshot-a.example.test";
const SNAPSHOT_B: &str = "snapshot-b.example.test";
/// The child's bound: Lua loads once and `add_lua` answers from the
/// process environment without touching the hub, so the child exits at
/// once; the wait stays inside half of nextest's per-test timeout
/// (`.config/nextest.toml`, `docs/testing.md`, "Waits and timeouts").
const SNAPSHOT_WITHIN: Duration = Duration::from_secs(30);

/// This test's full path, for `fakes::rerun_within`: the crate name
/// `module_path!` carries is not part of a test's path.
fn snapshot_path(test: &str) -> String {
    let module = module_path!();
    let module = module.split_once("::").map_or(module, |(_, rest)| rest);
    format!("{module}::{test}")
}

#[test]
fn a_hub_spawned_session_keeps_its_startup_snapshot_across_a_refresh() {
    if std::env::var(SNAPSHOT_CHILD).is_ok() {
        snapshot_child();
        return;
    }
    assert!(
        std::env::var_os(SNAPSHOT_HOST).is_none(),
        "the test process lacks {SNAPSHOT_HOST}"
    );
    let (_root, home, workspace) = setup_home("fiber-lua-snapshot");
    install_acme_with_placeholders(&home, json!({"workspace": {"env": SNAPSHOT_HOST}}));
    std::fs::create_dir_all(home.join("run")).unwrap();
    let listener = UnixListener::bind(home.join("run/hub")).unwrap();
    // The hub answers snapshot B and reports the session's request, if
    // one arrives; a marked session sends none.
    let (sent, got) = mpsc::channel();
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let hello = serde_json::json!({
            "kind": "hub_hello", "ts": 1,
            "schema_version": contract::SCHEMA_VERSION, "payload": {},
        });
        if stream.write_all(format!("{hello}\n").as_bytes()).is_err() {
            return;
        }
        let mut line = String::new();
        if std::io::BufReader::new(&stream)
            .read_line(&mut line)
            .unwrap_or(0)
            == 0
        {
            // The parent's dummy connect after the assertions: no
            // request, nothing to answer.
            return;
        }
        sent.send(line).unwrap_or(());
        let answer = serde_json::json!({
            "kind": "command_accepted", "ts": 1,
            "schema_version": contract::SCHEMA_VERSION,
            "payload": {"command_id": "c_env",
                "result": {"values": {SNAPSHOT_HOST: SNAPSHOT_B}}},
        });
        if stream.write_all(format!("{answer}\n").as_bytes()).is_err() {
            return;
        }
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap_or(());
        let mut rest = String::new();
        std::io::BufReader::new(stream)
            .read_line(&mut rest)
            .unwrap_or(0);
    });
    let home_str = home.to_string_lossy().into_owned();
    let workspace_str = workspace.to_string_lossy().into_owned();
    let out = fakes::rerun_within(
        &snapshot_path("a_hub_spawned_session_keeps_its_startup_snapshot_across_a_refresh"),
        &[
            (SNAPSHOT_CHILD, "1"),
            (hub::HUB_SESSION_MARKER, "1"),
            (SNAPSHOT_HOST, SNAPSHOT_A),
            (SNAPSHOT_HOME, home_str.as_str()),
            (SNAPSHOT_WORKSPACE, workspace_str.as_str()),
        ],
        Deadline::after(SNAPSHOT_WITHIN),
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "the marked child resolves its snapshot: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let record = std::fs::read_to_string(home.join("snapshot-record")).unwrap();
    assert!(
        record.contains(SNAPSHOT_A),
        "the session resolves snapshot A: {record:?}"
    );
    assert!(
        !record.contains(SNAPSHOT_B),
        "the post-refresh answer never reaches the session: {record:?}"
    );
    assert!(
        got.try_recv().is_err(),
        "a hub-spawned session sends the hub no placeholder query"
    );
    // Releases the server thread's accept, which then sees the EOF and
    // exits; the run's leak scan sees no leftover.
    let _dummy = UnixStream::connect(home.join("run/hub"));
}

/// The re-run child: the session process holding snapshot A with the hub's
/// marker, resolving its placeholders through `add_lua` and recording the
/// chosen base URL for the parent.
fn snapshot_child() {
    let home = std::path::PathBuf::from(std::env::var(SNAPSHOT_HOME).unwrap());
    let workspace = std::path::PathBuf::from(std::env::var(SNAPSHOT_WORKSPACE).unwrap());
    let config = load_config(&home, &workspace, &[]);
    let (mut providers, _) = Providers::load(&home).unwrap();
    let extensions = extensions::SessionExtensions::load(
        &home,
        &config,
        fakes::clock::FakeClock::new(),
        Arc::new(PathLocks::new()),
        None,
    );
    let lookup = super::lookup(&home, &providers, &*fakes::clock::FakeClock::new());
    let _ = crate::lua_providers::add_lua(
        &extensions,
        &mut providers,
        &config,
        &fakes::children::grouped_output,
        &lookup,
    )
    .unwrap();
    let model = providers.resolve("acme/m").unwrap();
    std::fs::write(
        home.join("snapshot-record"),
        format!("base_url={}\n", model.model.base_url),
    )
    .unwrap();
}
