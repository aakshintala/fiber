//! `fiber models`: the text table, `--json` lines, the substring search,
//! the marked default and the no-provider sentence, against a temporary
//! Fiber home.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code; a failure is the test's"
)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use contract::clock::Clock;
use contract::shapes::Failure;
use serde_json::{Value, json};

use super::{refresh_named, refresh_run, run};

/// `fiber models` never writes a file, so its lock runs every call straight
/// through.
struct NoLock;

impl contract::files::PathLock for NoLock {
    fn hold(&self, _path: &std::path::Path, run: &mut dyn FnMut()) {
        run();
    }

    fn hold_all(&self, _paths: &[PathBuf], run: &mut dyn FnMut()) {
        run();
    }
}

/// Fiber home and a workspace in a temporary directory, removed on drop.
struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        let setup = Self {
            root: fakes::TempDir::new("fiber-models"),
        };
        fs::create_dir_all(setup.home()).unwrap();
        fs::create_dir_all(setup.root.path().join("workspace")).unwrap();
        setup
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn workspace(&self) -> PathBuf {
        self.root.path().join("workspace")
    }

    /// Installs the extension `extension` registering the provider `name`
    /// with these models.
    fn install(&self, extension: &str, name: &str, models: &Value) {
        let dir = self.home().join("extensions").join(extension);
        let data = json!({
            "name": name,
            "models": models,
        });
        fs::create_dir_all(dir.join("providers")).unwrap();
        fs::write(
            dir.join("extension.json"),
            json!({
                "name": extension,
                "version": "v0.0.0",
                "fiber": "0.0.0",
                "api": extensions::API,
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            dir.join("providers").join(format!("{name}.json")),
            data.to_string(),
        )
        .unwrap();
    }

    fn write_config(&self, value: &Value) {
        fs::write(self.home().join("config.json"), value.to_string()).unwrap();
    }

    /// Runs the command with in-memory writers.
    fn run(
        &self,
        search: Option<&str>,
        json: bool,
        spawn: &dyn Fn(Vec<String>) -> std::io::Result<()>,
        clock: std::sync::Arc<dyn contract::clock::Clock>,
    ) -> (Result<(), Failure>, String, String) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let home = self.home();
        let result = run(
            &self.home(),
            &self.workspace(),
            search,
            json,
            &mut out,
            &mut err,
            &|config: &config::Config| {
                let locks: std::sync::Arc<dyn contract::files::PathLock> =
                    std::sync::Arc::new(NoLock);
                extensions::SessionExtensions::load(
                    &home,
                    config,
                    std::sync::Arc::clone(&clock),
                    locks,
                )
            },
            spawn,
            clock.as_ref(),
        );
        (
            result,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    /// Installs the Lua extension `extension` registering the provider
    /// `name` whose `models()` returns the Lua list `models`.
    fn install_lua(&self, extension: &str, name: &str, models: &str) {
        let src = self.root.path().join("src").join(extension);
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("extension.json"),
            json!({
                "name": extension,
                "version": "v0.0.0",
                "fiber": "0.0.0",
                "api": extensions::API,
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            src.join("init.lua"),
            format!(
                "fiber.provider(\"{name}\", {{ credential = {{ timeout = 1000, \
                 run = function() return {{ token = \"t\", expires_at = 1893456000 }} end }}, \
                 models = {{ timeout = 1000, \
                 run = function() return {models} end }} }})\n"
            ),
        )
        .unwrap();
        extensions::plan(
            &self.home(),
            &extensions::Request::Path(src),
            "0.0.0",
            &extensions::Origin::github(),
            &*fakes::clock::FakeClock::new(),
        )
        .unwrap()
        .commit()
        .unwrap();
    }
}

/// The two-provider home the text and JSON tests share: `acme/big` with a
/// context window and prices, `localhost/tiny` with neither.
fn two_provider_setup() -> Setup {
    let setup = Setup::new();
    setup.install(
        "acme",
        "acme",
        &json!([{
            "id": "big",
            "protocol": "openai-responses",
            "base_url": "http://127.0.0.1:1/v1",
            "context_window": 200000,
            "cost": {"input": 3.0, "output": 15.0},
        }]),
    );
    setup.install(
        "localhost",
        "localhost",
        &json!([{
            "id": "tiny",
            "protocol": "openai-responses",
            "base_url": "http://127.0.0.1:1/v1",
        }]),
    );
    setup.write_config(&json!({"model": "acme/big"}));
    setup
}

#[test]
fn the_text_table_marks_exactly_the_default_row() {
    let setup = two_provider_setup();
    let (result, out, err) = setup.run(None, false, &no_spawn, fakes::clock::FakeClock::new());
    result.unwrap();
    assert_eq!(err, "");
    assert_eq!(
        out,
        "  model           context  in $/M  out $/M\n\
         * acme/big        200000   3       15\n\
         \x20 localhost/tiny  -        -       -\n"
    );
}

#[test]
fn json_prints_the_same_rows_in_the_same_order() {
    let setup = two_provider_setup();
    let (result, out, err) = setup.run(None, true, &no_spawn, fakes::clock::FakeClock::new());
    result.unwrap();
    assert_eq!(err, "");
    assert_eq!(
        out,
        "{\"model\":\"acme/big\",\"context_window\":200000,\
         \"input\":3.0,\"output\":15.0,\"default\":true}\n\
         {\"model\":\"localhost/tiny\",\"context_window\":null,\
         \"input\":null,\"output\":null,\"default\":false}\n"
    );
}

#[test]
fn the_search_matches_provider_slash_model_without_regard_to_case() {
    let setup = two_provider_setup();
    for search in ["acme", "ACME", "Big", "acme/b", "localhost/t"] {
        let (result, out, err) = setup.run(
            Some(search),
            false,
            &no_spawn,
            fakes::clock::FakeClock::new(),
        );
        result.unwrap();
        assert_eq!(err, "", "{search}");
        assert_eq!(out.lines().count(), 2, "{search}: {out:?}");
        assert!(
            out.lines()
                .all(|line| line.contains(&search.to_ascii_lowercase())
                    || line.starts_with("  model")),
            "{search}: {out:?}"
        );
    }
    let (result, out, err) = setup.run(
        Some("acme"),
        true,
        &no_spawn,
        fakes::clock::FakeClock::new(),
    );
    result.unwrap();
    assert_eq!(err, "");
    assert_eq!(out.lines().count(), 1);
    assert!(out.contains("\"acme/big\""), "{out:?}");
}

#[test]
fn a_search_with_no_match_prints_nothing() {
    let setup = two_provider_setup();
    for json in [false, true] {
        let (result, out, err) = setup.run(
            Some("nothing-matches-this"),
            json,
            &no_spawn,
            fakes::clock::FakeClock::new(),
        );
        result.unwrap();
        assert_eq!(out, "", "json={json}");
        assert_eq!(err, "", "json={json}");
    }
}

#[test]
fn a_bare_id_as_the_default_marks_the_providers_row() {
    let setup = two_provider_setup();
    setup.write_config(&json!({"model": "big"}));
    let (result, out, err) = setup.run(None, false, &no_spawn, fakes::clock::FakeClock::new());
    result.unwrap();
    assert_eq!(err, "");
    assert!(
        out.lines()
            .any(|line| line == "* acme/big        200000   3       15"),
        "{out:?}"
    );
    let (result, out, _) = setup.run(None, true, &no_spawn, fakes::clock::FakeClock::new());
    result.unwrap();
    assert!(out.contains("\"default\":true"), "{out:?}");
}

#[test]
fn a_default_that_does_not_resolve_marks_no_row_and_is_not_an_error() {
    for model in [json!({"model": "acme/missing"}), json!({})] {
        let setup = two_provider_setup();
        setup.write_config(&model);
        let (result, out, err) = setup.run(None, false, &no_spawn, fakes::clock::FakeClock::new());
        result.unwrap();
        assert_eq!(err, "");
        assert!(!out.lines().any(|line| line.starts_with('*')), "{out:?}");
        assert_eq!(out.lines().count(), 3, "{out:?}");
        let (result, out, _) = setup.run(None, true, &no_spawn, fakes::clock::FakeClock::new());
        result.unwrap();
        assert!(!out.contains("\"default\":true"), "{out:?}");
    }
}

#[test]
fn with_no_provider_it_names_extension_install_on_stderr() {
    let setup = Setup::new();
    for json in [false, true] {
        let (result, out, err) = setup.run(None, json, &no_spawn, fakes::clock::FakeClock::new());
        result.unwrap();
        assert_eq!(out, "", "json={json}");
        assert_eq!(
            err, "No provider is installed. Run `fiber extension install <name>` to install one.\n",
            "json={json}"
        );
    }
}

#[test]
fn a_tiered_model_shows_its_base_prices() {
    let setup = Setup::new();
    setup.install(
        "acme",
        "acme",
        &json!([{
            "id": "big",
            "protocol": "openai-responses",
            "base_url": "http://127.0.0.1:1/v1",
            "context_window": 1000,
            "cost": {
                "input": 1.25,
                "output": 0.3,
                "tiers": [{
                    "input_tokens_above": 100000,
                    "input": 50.0,
                    "output": 60.0,
                    "cache_read": 5.0,
                    "cache_write": 6.0,
                }],
            },
        }]),
    );
    setup.write_config(&json!({"model": "acme/big"}));
    let (result, out, err) = setup.run(None, false, &no_spawn, fakes::clock::FakeClock::new());
    result.unwrap();
    assert_eq!(err, "");
    assert_eq!(
        out,
        "  model     context  in $/M  out $/M\n\
         * acme/big  1000     1.25    0.3\n"
    );
    let (result, out, _) = setup.run(None, true, &no_spawn, fakes::clock::FakeClock::new());
    result.unwrap();
    assert_eq!(
        out,
        "{\"model\":\"acme/big\",\"context_window\":1000,\
         \"input\":1.25,\"output\":0.3,\"default\":true}\n"
    );
}

#[test]
fn a_model_listed_twice_prints_two_rows_and_marks_only_the_first() {
    let setup = Setup::new();
    setup.install(
        "acme",
        "acme",
        &json!([
            {
                "id": "big",
                "protocol": "openai-responses",
                "base_url": "http://127.0.0.1:1/v1",
                "context_window": 7,
                "cost": {"input": 1.0, "output": 2.0},
            },
            {
                "id": "big",
                "protocol": "openai-responses",
                "base_url": "http://127.0.0.1:1/v1",
                "context_window": 7,
                "cost": {"input": 1.0, "output": 2.0},
            },
        ]),
    );
    setup.write_config(&json!({"model": "acme/big"}));
    let (result, out, err) = setup.run(None, false, &no_spawn, fakes::clock::FakeClock::new());
    result.unwrap();
    assert_eq!(err, "");
    assert_eq!(
        out,
        "  model     context  in $/M  out $/M\n\
         * acme/big  7        1       2\n\
         \x20 acme/big  7        1       2\n"
    );
    let (result, out, _) = setup.run(None, true, &no_spawn, fakes::clock::FakeClock::new());
    result.unwrap();
    assert_eq!(
        out,
        "{\"model\":\"acme/big\",\"context_window\":7,\
         \"input\":1.0,\"output\":2.0,\"default\":true}\n\
         {\"model\":\"acme/big\",\"context_window\":7,\
         \"input\":1.0,\"output\":2.0,\"default\":false}\n"
    );
}

/// How long a refresh worker may run before the test fails: the refresh
/// joins its threads on a worker and receives completion under this one
/// named deadline.
const REFRESH_DEADLINE: Duration = Duration::from_secs(60);

/// How long the spawn stub may take to record its arguments: a fresh
/// executable can stall on macOS, so the crate's usual child deadline.
#[cfg(unix)]
const SPAWN_DEADLINE: Duration = Duration::from_secs(60);

/// The child's marker: set, the test runs `models` and exits with its code.
const CHILD: &str = "FIBER_CLI_TEST_CHILD";

/// How long the child may run before the test kills it and fails.
const CHILD_DEADLINE: Duration = Duration::from_secs(60);

#[test]
fn models_exits_zero_and_prints_the_row() {
    // Runs in a child with a Fiber home holding one provider, so the exit
    // code is `models`' own. The parent checks the code and the row: a code
    // alone would not catch a `models` that returns 0 without printing.
    if std::env::var_os(CHILD).is_some() {
        std::process::exit(super::models(
            None,
            false,
            fakes::clock::FakeClock::new(),
            std::sync::Arc::new(NoLock),
        ));
    }
    let setup = Setup::new();
    setup.install(
        "acme",
        "acme",
        &json!([{
            "id": "big",
            "protocol": "openai-responses",
            "base_url": "http://127.0.0.1:1/v1",
        }]),
    );
    let name = module_path!().split_once("::").unwrap().1;
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("{name}::models_exits_zero_and_prints_the_row"),
            "--nocapture",
            "--test-threads=1",
        ])
        .env("FIBER_HOME", setup.home())
        .env(CHILD, "1")
        .current_dir(setup.workspace())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || tx.send(child.wait_with_output().unwrap()));
    let Ok(output) = rx.recv_timeout(CHILD_DEADLINE) else {
        fakes::kill_pid(pid, "KILL").unwrap();
        panic!("waited {CHILD_DEADLINE:?} for `fiber models` to exit");
    };
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("acme/big"), "{stdout:?}");
}

#[test]
fn models_with_a_relative_fiber_home_is_a_usage_failure() {
    // `FIBER_HOME` must be an absolute path, so a relative one fails before
    // anything is read. It runs in a child so the process's own `FIBER_HOME`
    // cannot change the outcome.
    if std::env::var_os(CHILD).is_some() {
        std::process::exit(super::models(
            None,
            false,
            fakes::clock::FakeClock::new(),
            std::sync::Arc::new(NoLock),
        ));
    }
    let setup = Setup::new();
    let name = module_path!().split_once("::").unwrap().1;
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("{name}::models_with_a_relative_fiber_home_is_a_usage_failure"),
            "--nocapture",
            "--test-threads=1",
        ])
        .env("FIBER_HOME", "relative")
        .env(CHILD, "1")
        .current_dir(setup.workspace())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || tx.send(child.wait().unwrap()));
    let Ok(status) = rx.recv_timeout(CHILD_DEADLINE) else {
        fakes::kill_pid(pid, "KILL").unwrap();
        panic!("waited {CHILD_DEADLINE:?} for `fiber models` to exit");
    };
    // `FIBER_HOME must be an absolute path` is a usage error.
    assert_eq!(status.code(), Some(2), "{status:?}");
}

#[test]
fn padding_counts_characters_not_bytes() {
    let setup = Setup::new();
    setup.install(
        "acme",
        "acme",
        &json!([
            {
                "id": "café",
                "protocol": "openai-responses",
                "base_url": "http://127.0.0.1:1/v1",
            },
            {
                "id": "big",
                "protocol": "openai-responses",
                "base_url": "http://127.0.0.1:1/v1",
            },
        ]),
    );
    setup.write_config(&json!({"model": "acme/big"}));
    let (result, out, err) = setup.run(None, false, &no_spawn, fakes::clock::FakeClock::new());
    result.unwrap();
    assert_eq!(err, "");
    assert_eq!(
        out,
        "  model      context  in $/M  out $/M\n\
         \x20 acme/café  -        -       -\n\
         * acme/big   -        -       -\n"
    );
}

#[test]
fn a_lua_providers_discovered_models_are_listed_with_no_cached_copy() {
    let setup = Setup::new();
    setup.install_lua(
        "acme-lua",
        "acme",
        "{ { id = \"m\", protocol = \"openai-responses\", \
         base_url = \"http://127.0.0.1:1/v1\", context_window = 8000 } }",
    );
    let (result, out, err) = setup.run(None, false, &no_spawn, fakes::clock::FakeClock::new());
    result.unwrap();
    assert_eq!(err, "");
    assert!(
        out.lines()
            .any(|line| line.contains("acme/m") && line.contains("8000")),
        "{out:?}"
    );
    let (result, out, err) = setup.run(
        Some("acme"),
        true,
        &no_spawn,
        fakes::clock::FakeClock::new(),
    );
    result.unwrap();
    assert_eq!(err, "");
    assert!(out.contains("\"model\":\"acme/m\""), "{out:?}");
}

/// A spawner that records its calls and fails when told to.
struct Recorder {
    calls: std::sync::Mutex<Vec<Vec<String>>>,
    fail: bool,
}

impl Recorder {
    fn fresh() -> Self {
        Self {
            calls: std::sync::Mutex::new(Vec::new()),
            fail: false,
        }
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().unwrap().clone()
    }
}

impl Recorder {
    fn spawn(&self, providers: Vec<String>) -> std::io::Result<()> {
        self.calls.lock().unwrap().push(providers);
        if self.fail {
            Err(std::io::Error::other("cannot start the refresh child"))
        } else {
            Ok(())
        }
    }
}

/// A spawner for the tests that never refresh: none of them starts one,
/// so any call is a failure.
fn no_spawn(providers: Vec<String>) -> std::io::Result<()> {
    Err(std::io::Error::other(format!(
        "must not refresh: {providers:?}"
    )))
}

/// Writes the cached list of `name` and backdates it, so its age reads
/// against `now`.
fn write_stale_cache(home: &std::path::Path, name: &str, list: &Value, now: std::time::SystemTime) {
    config::write_model_cache(home, name, list).unwrap();
    let file = home.join("cache/models").join(format!("{name}.json"));
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(now - std::time::Duration::from_secs(25 * 60 * 60))
        .unwrap();
}

fn lua_list(id: &str) -> String {
    format!(
        "{{ {{ id = \"{id}\", protocol = \"openai-responses\", \
         base_url = \"http://127.0.0.1:1/v1\" }} }}"
    )
}

#[test]
fn a_stale_list_prints_at_once_and_spawns_its_refresh() {
    let setup = Setup::new();
    setup.install_lua("acme-lua", "acme", &lua_list("new"));
    let clock = fakes::clock::FakeClock::new();
    write_stale_cache(
        &setup.home(),
        "acme",
        &json!([{"id": "old", "protocol": "openai-responses",
                 "base_url": "http://127.0.0.1:1/v1"}]),
        clock.wall(),
    );
    let spawner = Recorder::fresh();
    let (result, out, err) = setup.run(None, false, &|providers| spawner.spawn(providers), clock);
    result.unwrap();
    assert_eq!(err, "");
    assert!(out.contains("acme/old"), "{out:?}");
    assert!(!out.contains("acme/new"), "{out:?}");
    assert_eq!(spawner.calls(), [vec!["acme".to_owned()]]);
}

#[test]
fn a_fresh_list_prints_at_once_and_spawns_nothing() {
    let setup = Setup::new();
    setup.install_lua("acme-lua", "acme", &lua_list("new"));
    let clock = fakes::clock::FakeClock::new();
    config::write_model_cache(
        &setup.home(),
        "acme",
        &json!([{"id": "old", "protocol": "openai-responses",
                 "base_url": "http://127.0.0.1:1/v1"}]),
    )
    .unwrap();
    let spawner = Recorder::fresh();
    let (result, out, err) = setup.run(None, false, &|providers| spawner.spawn(providers), clock);
    result.unwrap();
    assert_eq!(err, "");
    assert!(out.contains("acme/old"), "{out:?}");
    assert!(spawner.calls().is_empty());
}

#[test]
fn a_spawn_that_fails_still_prints_from_the_cache() {
    let setup = Setup::new();
    setup.install_lua("acme-lua", "acme", &lua_list("new"));
    let clock = fakes::clock::FakeClock::new();
    write_stale_cache(
        &setup.home(),
        "acme",
        &json!([{"id": "old", "protocol": "openai-responses",
                 "base_url": "http://127.0.0.1:1/v1"}]),
        clock.wall(),
    );
    let spawner = Recorder {
        calls: std::sync::Mutex::new(Vec::new()),
        fail: true,
    };
    let (result, out, err) = setup.run(None, false, &|providers| spawner.spawn(providers), clock);
    result.unwrap();
    assert_eq!(err, "");
    assert!(out.contains("acme/old"), "{out:?}");
    assert_eq!(spawner.calls(), [vec!["acme".to_owned()]]);
}

#[test]
fn the_refresh_child_leaves_an_unnamed_uncached_provider_alone() {
    // A named stale provider beside an unrelated credentialed provider
    // with no cache: only the named one refreshes, and nothing runs the
    // other's `models()` synchronously outside the refresh lock. Its list
    // would land in the cache if it ran, so no cache file means no call.
    let setup = Setup::new();
    setup.install_lua("stale-ext", "stale", &lua_list("new"));
    setup.install_lua("other-ext", "other", &lua_list("other-model"));
    let clock = fakes::clock::FakeClock::new();
    write_stale_cache(
        &setup.home(),
        "stale",
        &json!([{"id": "old", "protocol": "openai-responses",
                 "base_url": "http://127.0.0.1:1/v1"}]),
        clock.wall(),
    );
    let home = setup.home();
    let (_, project) = crate::project_of(&home, &setup.workspace()).unwrap();
    let config = config::Config::load(config::Sources {
        home: home.clone(),
        workspace: setup.workspace(),
        project,
        overrides: Vec::new(),
    })
    .unwrap();
    let (providers, _) = extensions::Providers::load(&home).unwrap();
    let loaded = extensions::SessionExtensions::load(
        &home,
        &config,
        clock.clone(),
        std::sync::Arc::new(NoLock),
    );
    // `refresh_named` joins its refresh threads: run it on a worker and
    // receive completion under one named deadline.
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        refresh_named(&["stale".to_owned()], &providers, &loaded, &config);
        done.send(()).unwrap();
    });
    assert!(
        finished.recv_timeout(REFRESH_DEADLINE).is_ok(),
        "waited {REFRESH_DEADLINE:?} for the refresh child to finish"
    );
    let stale: Vec<config::ModelData> = config::read_model_cache(&home, "stale").unwrap().unwrap();
    assert_eq!(
        stale.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        ["new"]
    );
    assert!(
        config::read_model_cache(&home, "other").unwrap().is_none(),
        "the unnamed provider's `models()` must not run"
    );
}

#[test]
fn a_list_exactly_refresh_after_old_spawns_nothing() {
    // The boundary `stale_lists` pins: a list whose age equals `max_age`
    // exactly is not stale. The default `refresh_after` is one day, and
    // the fake clock's wall time is whole seconds, so the mtime below
    // reads exactly one day old.
    let setup = Setup::new();
    setup.install_lua("acme-lua", "acme", &lua_list("new"));
    let clock = fakes::clock::FakeClock::new();
    let now = clock.wall();
    config::write_model_cache(
        &setup.home(),
        "acme",
        &json!([{"id": "old", "protocol": "openai-responses",
                 "base_url": "http://127.0.0.1:1/v1"}]),
    )
    .unwrap();
    let file = setup.home().join("cache/models/acme.json");
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(now - Duration::from_secs(24 * 60 * 60))
        .unwrap();
    let spawner = Recorder::fresh();
    let (result, out, err) = setup.run(None, false, &|providers| spawner.spawn(providers), clock);
    result.unwrap();
    assert_eq!(err, "");
    assert!(out.contains("acme/old"), "{out:?}");
    assert!(
        spawner.calls().is_empty(),
        "a list exactly `refresh_after` old is not stale"
    );
}

#[test]
#[cfg(unix)]
fn spawn_refresh_runs_the_stub_with_the_refresh_arguments() {
    // The subject is direct execution, so the stub stays an executable
    // file (`docs/testing.md`, "Waits and timeouts"): a shell script
    // recording its arguments, run by its shebang.
    use std::os::unix::fs::PermissionsExt;
    let dir = fakes::TempDir::new("fiber-spawn-refresh");
    let record = dir.path().join("args");
    let stub = dir.path().join("stub.sh");
    std::fs::write(
        &stub,
        format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\n", record.display()),
    )
    .unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut child = super::spawn_refresh(&stub, vec!["acme".to_owned()]).unwrap();
    // The stub writes its arguments and exits, so its exit is the proof it
    // ran; waiting on it fails fast where polling the record would not.
    let pid = child.id();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait().unwrap()));
    let Ok(status) = finished.recv_timeout(SPAWN_DEADLINE) else {
        fakes::kill_pid(pid, "KILL").unwrap();
        panic!("waited {SPAWN_DEADLINE:?} for the refresh child to exit");
    };
    assert!(status.success(), "{status}");
    let args = std::fs::read_to_string(&record).unwrap();
    assert!(
        args.contains("refresh-model-lists") && args.contains("acme"),
        "{args:?}"
    );
}

#[test]
fn the_refresh_entry_refreshes_a_stale_list() {
    // `refresh_run` is the testable body behind the hidden child: a stale
    // Lua provider's cache holds the new list afterwards. A body replaced
    // with `()` refreshes nothing, so the assertion fails. It joins its
    // refresh threads, so it runs on a worker under one named deadline.
    let setup = Setup::new();
    setup.install_lua("stale-ext", "stale", &lua_list("new"));
    let clock = fakes::clock::FakeClock::new();
    write_stale_cache(
        &setup.home(),
        "stale",
        &json!([{"id": "old", "protocol": "openai-responses",
                 "base_url": "http://127.0.0.1:1/v1"}]),
        clock.wall(),
    );
    let home = setup.home();
    let workspace = setup.workspace();
    let names = vec!["stale".to_owned()];
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        refresh_run(
            &home,
            &workspace,
            &names,
            clock,
            std::sync::Arc::new(NoLock),
        );
        done.send(()).unwrap();
    });
    assert!(
        finished.recv_timeout(REFRESH_DEADLINE).is_ok(),
        "waited {REFRESH_DEADLINE:?} for the refresh entry to finish"
    );
    let stale: Vec<config::ModelData> = config::read_model_cache(&setup.home(), "stale")
        .unwrap()
        .unwrap();
    assert_eq!(
        stale.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        ["new"]
    );
}

#[test]
fn the_hidden_refresh_child_refreshes_a_stale_list() {
    // The public entry, run in a child whose `FIBER_HOME` and directory
    // hold a stale Lua provider: the cache holds the new list after it
    // exits. A body replaced with `()` leaves the old list.
    if std::env::var_os(CHILD).is_some() {
        super::refresh_model_lists(
            &["stale".to_owned()],
            fakes::clock::FakeClock::new(),
            std::sync::Arc::new(NoLock),
        );
        std::process::exit(0);
    }
    let setup = Setup::new();
    setup.install_lua("stale-ext", "stale", &lua_list("new"));
    write_stale_cache(
        &setup.home(),
        "stale",
        &json!([{"id": "old", "protocol": "openai-responses",
                 "base_url": "http://127.0.0.1:1/v1"}]),
        fakes::clock::FakeClock::new().wall(),
    );
    let name = module_path!().split_once("::").unwrap().1;
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("{name}::the_hidden_refresh_child_refreshes_a_stale_list"),
            "--nocapture",
            "--test-threads=1",
        ])
        .env("FIBER_HOME", setup.home())
        .env(CHILD, "1")
        .current_dir(setup.workspace())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || tx.send(child.wait().unwrap()));
    let Ok(status) = rx.recv_timeout(CHILD_DEADLINE) else {
        fakes::kill_pid(pid, "KILL").unwrap();
        panic!("waited {CHILD_DEADLINE:?} for the refresh child to exit");
    };
    assert!(status.success(), "{status}");
    let refreshed: Vec<config::ModelData> = config::read_model_cache(&setup.home(), "stale")
        .unwrap()
        .unwrap();
    assert_eq!(
        refreshed.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        ["new"]
    );
}
