//! `add_lua` fills per-account host placeholders
//! (`docs/model-routing.md`, "A per-account host").

#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]

use crate::test_support::{Rig, install_extension, load};
use fakes::MUST_SUCCEED_WITHIN;
use std::io::{BufRead, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use config::Config;
use contract::ErrorCode;
use contract::signing::SignRequest;
use extensions::Providers;
use serde_json::json;
use tools::PathLocks;

use super::{add_lua, session_credential};
use fakes::Deadline;

fn setup_home(name: &str) -> (fakes::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let rig = Rig::new(name);
    (rig.root, rig.home, rig.workspace)
}

fn install_acme(home: &std::path::Path) {
    install_acme_with_placeholders(home, json!({"workspace": {}}));
}

fn install_acme_with_placeholders(home: &std::path::Path, placeholders: serde_json::Value) {
    install_extension(
        home,
        "extensions/acme",
        json!({"name": "acme", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}),
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

#[test]
fn an_unconfigured_model_is_left_out_and_a_setting_fills_it() {
    let (_root, home, workspace) = setup_home("fiber-lua-placeholders");
    install_acme(&home);
    let config = load_config(&home, &workspace, &[]);
    let (mut providers, _) = Providers::load(&home).unwrap();
    let extensions = extensions::SessionExtensions::load(
        &home,
        &config,
        fakes::clock::FakeClock::new(),
        Arc::new(PathLocks::new()),
        None,
    );
    let (naming, _) = add_lua(
        &extensions,
        &mut providers,
        &config,
        &fakes::children::grouped_output,
        &home,
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap();
    assert!(
        naming.contains(&("acme".to_owned(), "m".to_owned())),
        "the naming list keeps the unconfigured model: {naming:?}"
    );
    assert_eq!(
        providers.resolve("acme/m").unwrap_err().code(),
        ErrorCode::ModelUnconfigured
    );

    let config = load_config(
        &home,
        &workspace,
        &["extensions.\"acme\".settings.workspace=adb-1.example"],
    );
    let (mut providers, _) = Providers::load(&home).unwrap();
    let extensions = extensions::SessionExtensions::load(
        &home,
        &config,
        fakes::clock::FakeClock::new(),
        Arc::new(PathLocks::new()),
        None,
    );
    let _ = add_lua(
        &extensions,
        &mut providers,
        &config,
        &fakes::children::grouped_output,
        &home,
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap();
    assert_eq!(
        providers.resolve("acme/m").unwrap().model.base_url,
        "https://adb-1.example/v1"
    );
}

#[test]
fn a_settings_file_that_cannot_be_read_fails_with_config_invalid() {
    let (_root, home, workspace) = setup_home("fiber-lua-placeholders-bad");
    install_acme(&home);
    std::fs::create_dir_all(home.join("config")).unwrap();
    std::fs::write(home.join("config/acme.json"), "not json").unwrap();
    let config = load_config(&home, &workspace, &[]);
    let (mut providers, _) = Providers::load(&home).unwrap();
    let extensions = extensions::SessionExtensions::load(
        &home,
        &config,
        fakes::clock::FakeClock::new(),
        Arc::new(PathLocks::new()),
        None,
    );
    let err = add_lua(
        &extensions,
        &mut providers,
        &config,
        &fakes::children::grouped_output,
        &home,
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::ConfigInvalid);
}

#[test]
fn the_session_label_and_shared_credential_name_reach_credential() {
    let (_root, home, workspace) = setup_home("fiber-lua-credential-pair");
    let src = home.join("src").join("acme");
    install_extension(
        &home,
        "src/acme",
        json!({"name": "acme", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}),
        &[(
            "acme",
            json!({
                "name": "acme",
                "credential_name": "shared",
                "models": [{"id": "m", "protocol": "openai-responses",
                            "base_url": "https://x.example/v1"}],
            }),
        )],
    );
    std::fs::write(
        { std::fs::create_dir_all((src.join("providers/acme.lua")).parent().unwrap()).unwrap(); src.join("providers/acme.lua") },
        "fiber.provider(\"acme\", { credential = { timeout = 60000, run = function(who)\
          return { token = who.credential .. \"/\" .. who.label, expires_at = 4102444800 } end } })\n",
    )
    .unwrap();
    extensions::plan(
        &home,
        &extensions::Request::Path(src),
        "0.1.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    let config = load_config(&home, &workspace, &[]);
    let (mut providers, _) = Providers::load(&home).unwrap();
    let extensions = extensions::SessionExtensions::load(
        &home,
        &config,
        fakes::clock::FakeClock::new(),
        Arc::new(PathLocks::new()),
        None,
    );
    let _ = add_lua(
        &extensions,
        &mut providers,
        &config,
        &fakes::children::grouped_output,
        &home,
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap();
    let data = providers.data("acme");
    // The lookup blocks on Lua, so it runs on its own thread under a
    // deadline instead of hanging the test.
    let (done, looked_up) = mpsc::channel();
    std::thread::spawn(move || {
        drop(done.send(session_credential(
            providers.lua("acme"),
            &data,
            "work",
            || panic!("no key is read when credential() is registered"),
        )))
    });
    let (key, signer) = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&looked_up)
        .expect("the lookup ended in time")
        .unwrap();
    assert!(key.is_none(), "no key file is needed past credential()");
    let signer = signer.unwrap();
    let headers = signer
        .sign(&SignRequest {
            method: "POST",
            url: "https://x.example/v1",
            headers: &[],
            body: b"{}",
        })
        .unwrap();
    assert_eq!(
        headers,
        [("authorization".to_owned(), "Bearer shared/work".to_owned())]
    );
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
    let _ = add_lua(
        &extensions,
        &mut providers,
        &config,
        &fakes::children::grouped_output,
        &home,
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap();
    let model = providers.resolve("acme/m").unwrap();
    std::fs::write(
        home.join("snapshot-record"),
        format!("base_url={}\n", model.model.base_url),
    )
    .unwrap();
}
