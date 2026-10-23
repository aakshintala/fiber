//! `host.exec` `env` reaches the program (`docs/extensions.md`, "Host calls").

mod common;

use fakes::MUST_SUCCEED_WITHIN;
use std::sync::{Arc, mpsc};

use common::{Setup, write};
use config::{Config, ProjectKey, Sources};
use extensions::LuaExtension;
use fakes::clock::FakeClock;
use fakes::{Deadline, NoLock};

/// Writes `init` as the entry script of `fiber.test/<short>` and returns a
/// `LuaExtension` started in a session on the setup's workspace.
#[allow(clippy::unwrap_used, reason = "a test helper; a failure is the test's")]
fn exec_extension(
    setup: &Setup,
    short: &str,
    init: &str,
    clock: Arc<FakeClock>,
) -> Arc<LuaExtension> {
    let dir = setup.home().join(short);
    write(&dir.join("init.lua"), init);
    let config = Config::load(Sources {
        home: setup.home(),
        workspace: setup.workspace(),
        project: ProjectKey::new("p").unwrap(),
        overrides: Vec::new(),
    })
    .unwrap();
    Arc::new(
        LuaExtension::new(format!("fiber.test/{short}"), dir, setup.home(), clock).with_session(
            extensions::Session {
                config,
                repo_settings: Vec::new(),
                locks: Arc::new(NoLock),
            },
        ),
    )
}

/// Runs `command` on its own thread under `MUST_SUCCEED_WITHIN`, so a callback the runtime
/// fails to stop fails the test instead of hanging it.
#[allow(clippy::panic, reason = "a test helper; a hang is the test's failure")]
#[track_caller]
fn exec_call(ext: &Arc<LuaExtension>, command: &str) -> Result<String, extensions::Error> {
    let (tx, rx) = mpsc::channel();
    let ext = Arc::clone(ext);
    let name = command.to_owned();
    std::thread::spawn(move || tx.send(ext.command(&name, "")));
    match Deadline::after(MUST_SUCCEED_WITHIN).recv(&rx) {
        Ok(result) => result,
        Err(_) => panic!("`{command}` did not return within {MUST_SUCCEED_WITHIN:?}"),
    }
}

const ENV_INIT: &str = r#"
fiber.command("probe", { timeout = 5000, run = function()
  return json.encode(host.exec("sh", {"-c", 'printf %s "$FIBER_EXEC_ENV_PROBE"'}, {env = {FIBER_EXEC_ENV_PROBE = "from-env"}}))
end })
fiber.command("home", { timeout = 5000, run = function()
  return json.encode(host.exec("sh", {"-c", 'printf %s "$HOME"'}, {env = {FIBER_EXEC_ENV_PROBE = "from-env"}}))
end })
"#;

#[test]
fn env_reaches_the_program_from_lua() {
    let setup = Setup::new();
    let ext = exec_extension(&setup, "exec-env", ENV_INIT, FakeClock::new());
    let body: serde_json::Value =
        serde_json::from_str(&exec_call(&ext, "probe").expect("the probe command runs")).unwrap();
    assert_eq!(body["exit_code"], 0);
    assert_eq!(body["stdout"], "from-env");
}

#[test]
fn env_is_added_to_the_inherited_environment() {
    let setup = Setup::new();
    let ext = exec_extension(&setup, "exec-env", ENV_INIT, FakeClock::new());
    let body: serde_json::Value =
        serde_json::from_str(&exec_call(&ext, "home").expect("the home command runs")).unwrap();
    assert_eq!(body["exit_code"], 0);
    assert_eq!(body["stdout"], std::env::var("HOME").unwrap());
}
