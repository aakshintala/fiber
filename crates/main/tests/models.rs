//! Binary-level tests of `fiber models` (`docs/testing.md`, "Levels";
//! `docs/invocation.md`, "Commands and flags"): the built `fiber` runs in
//! its own process group with its own `FIBER_HOME` holding one data
//! provider. Every run carries a wall-clock deadline. Each test runs
//! `fiber` through a hard link in its own temporary root, so a child it
//! re-runs from `current_exe()` carries that unique path on its command
//! line and no other test run's process ever matches it.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use support::repo_provider::{declare_repo_provider, git_init};
use support::write_record;

use fakes::Watchdog;
use serde_json::{Value, json};
use support::{Deadline, KillGroup, Output, group_alive, spawn_watched};

/// A temporary root holding Fiber home and the workspace, removed on drop.
struct Setup {
    root: fakes::TempDir,
    /// `<CARGO_TARGET_TMPDIR>/<root's name>`, holding the `fiber` link.
    bin: PathBuf,
    deadline: Deadline,
}

impl Drop for Setup {
    fn drop(&mut self) {
        match fs::remove_dir_all(&self.bin) {
            Ok(()) | Err(_) => {}
        }
    }
}

impl Setup {
    fn new() -> Self {
        let deadline = Deadline::start();
        let root = fakes::TempDir::new("fm");
        fs::create_dir_all(root.path().join("h")).unwrap();
        fs::create_dir_all(root.path().join("w")).unwrap();
        // A hard link, not a copy: the bytes and inode are the built
        // binary's, so nothing new is executed (`docs/testing.md`, "Waits
        // and timeouts"), but the path is this test's own. It lives in
        // Cargo's per-target temporary directory, which is on the target
        // directory's filesystem, in a directory named after the root.
        let name = root.path().file_name().unwrap();
        let bin = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
        fs::create_dir(&bin).unwrap();
        let exe = bin.join("fiber");
        if let Err(err) = fs::hard_link(env!("CARGO_BIN_EXE_fiber"), &exe) {
            panic!("hard-linking `fiber` to {}: {err}", exe.display());
        }
        Self {
            deadline,
            root,
            bin,
        }
    }

    /// This test's own path to the built `fiber`.
    fn exe(&self) -> PathBuf {
        self.bin.join("fiber")
    }

    /// The refresh child's command line: this test's `fiber` re-run as
    /// `refresh-model-lists`. The path is unique to this test, so no other
    /// test run's process, and no stub with the same arguments, matches.
    fn refresh_pattern(&self) -> String {
        format!("{} refresh-model-lists", self.exe().display())
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("h")
    }

    /// Installs the provider `acme` serving `big` with a context window and
    /// prices, and makes `acme/big` the configured model.
    fn provider(&self) {
        let dir = self.home().join("extensions").join("acme");
        fs::create_dir_all(dir.join("providers")).unwrap();
        fs::write(
            dir.join("extension.json"),
            json!({"name": "acme", "version": "v0.0.0", "fiber": "0.0.0", "api": 1,
                   "providers": {"acme": ["http://127.0.0.1:9/v1"]}})
            .to_string(),
        )
        .unwrap();
        write_record(&dir);
        fs::write(
            dir.join("providers").join("acme.json"),
            json!({
                "name": "acme",
                "models": [{
                    "id": "big",
                    "protocol": "openai-responses",
                    "base_url": "http://127.0.0.1:9/v1",
                    "context_window": 200000,
                    "cost": {"input": 3.0, "output": 15.0},
                }],
            })
            .to_string(),
        )
        .unwrap();
        write(
            &self.home().join("config.json"),
            &json!({"model": "acme/big"}),
        );
    }

    /// Runs `fiber` with `args` and waits for it under the test's
    /// [`Deadline`].
    #[track_caller]
    fn fiber(&self, args: &[&str]) -> Output {
        let mut command = Command::new(self.exe());
        command
            .args(args)
            .current_dir(self.root.path().join("w"))
            .env_clear()
            .envs(fakes::check_run())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path())
            .env("FIBER_HOME", self.home())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (child, watchdog) = spawn_watched(&mut command);
        let group = child.id();
        let guard = KillGroup(group);
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait_with_output()).unwrap());
        let output = match self.deadline.recv(&finished) {
            Ok(output) => output.unwrap(),
            Err(_) => support::expired(
                self.deadline,
                group,
                &finished,
                &format!("`fiber {}` to exit", args.join(" ")),
            ),
        };
        assert!(
            !group_alive(self.deadline, group),
            "`fiber` left a process behind"
        );
        std::mem::forget(guard);
        watchdog.stand_down(self.deadline.cleanup_phase());
        Output::from(output)
    }
}

fn write(file: &PathBuf, value: &Value) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, value.to_string()).unwrap();
}

#[test]
fn models_lists_the_installed_models_as_text() {
    let setup = Setup::new();
    setup.provider();
    let run = setup.fiber(&["models"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.stderr, "");
    assert_eq!(
        run.stdout,
        "  model     context  in $/M  out $/M\n\
         * acme/big  200000   3       15\n"
    );
}

#[test]
fn models_json_prints_one_object_per_row() {
    let setup = Setup::new();
    setup.provider();
    let run = setup.fiber(&["models", "--json"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.stderr, "");
    assert_eq!(
        run.stdout,
        "{\"model\":\"acme/big\",\"context_window\":200000,\
         \"input\":3.0,\"output\":15.0,\"default\":true}\n"
    );
}

#[test]
fn models_search_filters_by_substring() {
    let setup = Setup::new();
    setup.provider();
    let run = setup.fiber(&["models", "acme"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(run.stdout.contains("acme/big"), "stdout: {}", run.stdout);
    let run = setup.fiber(&["models", "nothing-matches-this"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.stderr, "");
    assert_eq!(run.stdout, "");
}

#[test]
fn help_models_matches_models_help() {
    let setup = Setup::new();
    setup.provider();
    let via_help = setup.fiber(&["help", "models"]);
    let via_flag = setup.fiber(&["models", "--help"]);
    assert_eq!(via_help.code, Some(0), "stderr: {}", via_help.stderr);
    assert_eq!(via_flag.code, Some(0), "stderr: {}", via_flag.stderr);
    assert_eq!(via_help.stderr, "");
    assert_eq!(via_flag.stderr, "");
    assert_eq!(via_help.stdout, via_flag.stdout);
    assert!(
        via_flag
            .stdout
            .lines()
            .any(|line| line.starts_with("Usage: fiber models")),
        "stdout: {}",
        via_flag.stdout
    );
}

#[test]
fn models_runs_a_lua_providers_models_with_no_cached_copy() {
    let setup = Setup::new();
    let server = fakes::ProviderServer::start([fakes::Response::status(
        200,
        json!({"data": [{"id": "m1", "context_length": 1000}]}).to_string(),
    )])
    .unwrap();
    extensions::plan(
        &setup.home(),
        &extensions::Request::Path(fakes::lua_fixture()),
        "0.1.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    {
        let dir = setup
            .home()
            .join("extensions")
            .join("fiber.test-lua-fixture");
        let mut installed: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("extension.json")).unwrap())
                .unwrap();
        installed["providers"] = json!({"fixture": [format!("{}/v1", server.url())]});
        write(&dir.join("extension.json"), &installed);
    }
    config::store_secret(
        &setup.home(),
        "fixture.url",
        &config::Secret::new(server.url()),
    )
    .unwrap();
    config::store_secret(
        &setup.home(),
        "fixture.api_key",
        &config::Secret::new("k1".into()),
    )
    .unwrap();
    let run = setup.fiber(&["models"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.stderr, "");
    assert!(run.stdout.contains("fixture/m1"), "stdout: {}", run.stdout);
}

/// Installs the copied OpenRouter package with `https://openrouter.ai`
/// rewritten to the fake server, and stores its credential:
/// `Setup::fiber` clears the environment, so no env key reaches the child.
fn openrouter(setup: &Setup, server: &fakes::ProviderServer) {
    let copied = setup.root.path().join("pkg-openrouter");
    support::package::copy_package(
        "openrouter",
        &copied,
        "https://openrouter.ai",
        &server.url(),
    );
    extensions::plan(
        &setup.home(),
        &extensions::Request::Path(copied),
        "0.1.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    config::store_credential(
        &setup.home(),
        "openrouter",
        "default",
        &config::Secret::new("k-openrouter".into()),
    )
    .unwrap();
}

/// The `openrouter/<id>` references `fiber models --json` prints, in order.
fn listed_ids(run: &Output) -> Vec<String> {
    run.stdout
        .lines()
        .map(|line| {
            serde_json::from_str::<Value>(line)
                .unwrap()
                .get("model")
                .and_then(Value::as_str)
                .unwrap()
                .to_owned()
        })
        .collect()
}

#[test]
fn models_lists_openrouter_models_from_a_refresh_once_cached() {
    let setup = Setup::new();
    let server = fakes::ProviderServer::start([fakes::Response::status(
        200,
        support::package::openrouter_listing(),
    )])
    .unwrap();
    openrouter(&setup, &server);
    let first = setup.fiber(&["models", "--json"]);
    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
    assert_eq!(first.stderr, "");
    assert_eq!(
        listed_ids(&first),
        [
            "openrouter/anthropic/claude-sonnet-5.5",
            "openrouter/z-ai/glm-5.3-flash",
            "openrouter/openrouter/free",
            "openrouter/qwen/qwen3-max",
            "openrouter/openrouter/auto",
        ]
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    let only = requests.first().unwrap();
    assert_eq!(only.method.as_str(), "GET");
    assert_eq!(only.path.as_str(), "/api/v1/models");
    assert_eq!(only.header("authorization"), None);
    assert!(setup.home().join("cache/models/openrouter.json").is_file());

    let second = setup.fiber(&["models", "--json"]);
    assert_eq!(second.code, Some(0), "stderr: {}", second.stderr);
    assert_eq!(second.stdout, first.stdout);
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn a_failed_openrouter_refresh_keeps_the_cached_list() {
    let setup = Setup::new();
    // Declared after `setup`, so it drops first: a failing run kills this
    // test's refresh child before its directory is removed.
    let refresh_guard = Watchdog::matching(&setup.refresh_pattern());
    let server = fakes::ProviderServer::start([
        fakes::Response::status(200, support::package::openrouter_listing()),
        fakes::Response::status(500, "{}"),
    ])
    .unwrap();
    openrouter(&setup, &server);
    let first = setup.fiber(&["models", "--json"]);
    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
    // A stale cached copy: an ancient mtime is older than any
    // `model_lists.refresh_after`, without reading the clock.
    let file = setup.home().join("cache/models/openrouter.json");
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1))
        .unwrap();
    let cached = std::fs::read(&file).unwrap();

    let second = setup.fiber(&["models", "--json"]);
    assert_eq!(second.code, Some(0), "stderr: {}", second.stderr);
    assert_eq!(second.stdout, first.stdout);

    // The second run returns without waiting: the failed refresh leaves
    // the cache as it was, once the detached child has exited.
    await_refresh_exit(setup.deadline, &setup.refresh_pattern());
    assert_eq!(std::fs::read(&file).unwrap(), cached);
    assert_eq!(server.requests().len(), 2);

    let third = setup.fiber(&["models", "--json"]);
    assert_eq!(third.code, Some(0), "stderr: {}", third.stderr);
    assert_eq!(third.stdout, first.stdout);
    await_refresh_exit(setup.deadline, &setup.refresh_pattern());
    refresh_guard.stand_down(setup.deadline.cleanup_phase());
}

#[test]
fn models_lists_a_thousand_openrouter_models_within_the_memory_cap() {
    let setup = Setup::new();
    let body: Value = serde_json::from_slice(&support::package::openrouter_listing()).unwrap();
    let template = body["data"][0].clone();
    let data: Vec<Value> = (0..1000)
        .map(|i| {
            let mut model = template.clone();
            model["id"] = json!(format!("anthropic/m{i}"));
            model
        })
        .collect();
    let server = fakes::ProviderServer::start([fakes::Response::status(
        200,
        json!({"data": data}).to_string(),
    )])
    .unwrap();
    openrouter(&setup, &server);
    let run = setup.fiber(&["models", "--json"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.stderr, "");
    let ids = listed_ids(&run);
    assert_eq!(ids.len(), 1000);
    assert_eq!(
        ids.first().map(String::as_str),
        Some("openrouter/anthropic/m0")
    );
    assert_eq!(
        ids.last().map(String::as_str),
        Some("openrouter/anthropic/m999")
    );
}

/// Every first-party model loads: `fiber models --json` lists each
/// `provider/id` the six files hold, and none is dropped as
/// `model_invalid`.
#[test]
fn models_lists_every_first_party_model_and_drops_none() {
    let setup = Setup::new();
    for package in ["anthropic", "gemini", "muse", "openai", "opencode"] {
        let dest = setup.home().join("extensions").join(package);
        support::package::copy_package(package, &dest, "", "");
        write_record(&dest);
    }
    let run = setup.fiber(&["models", "--json"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.stderr, "");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../providers");
    let mut expected = Vec::new();
    for package in ["anthropic", "gemini", "muse", "openai", "opencode"] {
        for provider in config::read_providers(&root.join(package)).unwrap() {
            for model in &provider.models {
                expected.push(format!("{}/{}", provider.name, model.id));
            }
        }
    }
    expected.sort_by_key(|reference| reference.split('/').next().unwrap_or("").to_owned());
    assert_eq!(listed_ids(&run), expected);
}

/// Waits until this test's refresh child is gone, under the test's
/// [`Deadline`]: the detached child exits after it rewrites the cache, and
/// a rewritten cache alone never proves it did. On expiry it kills the
/// child and its group, checks they are gone before the cleanup deadline,
/// and fails.
#[track_caller]
fn await_refresh_exit(deadline: Deadline, pattern: &str) {
    if fakes::matching_exits(pattern, deadline) {
        return;
    }
    support::kill_matching(deadline, pattern).unwrap();
    fakes::try_matching_exits(pattern, deadline.cleanup_phase()).unwrap_or_else(|err| {
        panic!("the refresh child outlived SIGKILL past the cleanup deadline: {err}")
    });
    panic!("waited until the deadline for the refresh child to exit");
}

#[test]
fn models_prints_a_stale_list_at_once_and_it_is_fresh_on_the_next_run() {
    let setup = Setup::new();
    // Declared after `setup`, so it drops first: a failing run kills this
    // test's refresh child before its directory is removed.
    let refresh_guard = Watchdog::matching(&setup.refresh_pattern());
    let server = fakes::ProviderServer::start([fakes::Response::status(
        200,
        json!({"data": [{"id": "m1", "context_length": 1000}]}).to_string(),
    )])
    .unwrap();
    extensions::plan(
        &setup.home(),
        &extensions::Request::Path(fakes::lua_fixture()),
        "0.1.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    {
        let dir = setup
            .home()
            .join("extensions")
            .join("fiber.test-lua-fixture");
        let mut installed: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("extension.json")).unwrap())
                .unwrap();
        installed["providers"] =
            json!({"fixture": [format!("{}/v1", server.url()), "http://127.0.0.1:9/v1"]});
        write(&dir.join("extension.json"), &installed);
    }
    config::store_secret(
        &setup.home(),
        "fixture.url",
        &config::Secret::new(server.url()),
    )
    .unwrap();
    config::store_secret(
        &setup.home(),
        "fixture.api_key",
        &config::Secret::new("k1".into()),
    )
    .unwrap();
    // A stale cached copy: an ancient mtime is older than any
    // `model_lists.refresh_after`, without reading the clock.
    let file = setup.home().join("cache/models/fixture.json");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    // The stale copy's URL is declared beside the server's, so the first
    // run lists it with no notice.
    std::fs::write(
        &file,
        json!([{ "id": "old", "protocol": "openai-responses",
                 "base_url": "http://127.0.0.1:9/v1", "context_window": 1000 }])
        .to_string(),
    )
    .unwrap();
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1))
        .unwrap();
    let old = std::fs::read(&file).unwrap();

    let first = setup.fiber(&["models"]);
    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
    assert_eq!(first.stderr, "");
    assert!(
        first.stdout.contains("fixture/old"),
        "stdout: {}",
        first.stdout
    );

    // The first run returns without waiting: the list is fresh on the
    // next run, once the detached child has rewritten the cache and exited.
    await_refresh_exit(setup.deadline, &setup.refresh_pattern());
    assert!(
        std::fs::read(&file).unwrap() != old,
        "the refresh child exited without rewriting {}",
        file.display()
    );
    let second = setup.fiber(&["models"]);
    assert_eq!(second.code, Some(0), "stderr: {}", second.stderr);
    assert_eq!(second.stderr, "");
    assert!(
        second.stdout.contains("fixture/m1"),
        "stdout: {}",
        second.stdout
    );
    assert!(
        !second.stdout.contains("fixture/old"),
        "stdout: {}",
        second.stdout
    );
    // The rewritten cache never proves the detached child exited: wait
    // for it under the deadline, then stand its watchdog down. Dropping
    // the guard without an exit would kill it instead of checking it,
    // and a timeout above drops it, so no run leaves one behind.
    await_refresh_exit(setup.deadline, &setup.refresh_pattern());
    refresh_guard.stand_down(setup.deadline.cleanup_phase());
}

fn install_data(home: &std::path::Path, name: &str, providers: &Value, datas: &[(&str, Value)]) {
    let dir = home.join("extensions").join(name);
    std::fs::create_dir_all(dir.join("providers")).unwrap();
    std::fs::write(
        dir.join("extension.json"),
        json!({"name": name, "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "providers": providers}).to_string(),
    )
    .unwrap();
    write_record(&dir);
    for (pname, data) in datas {
        std::fs::write(
            dir.join("providers").join(format!("{pname}.json")),
            data.to_string(),
        )
        .unwrap();
    }
}

fn data_with(name: &str, models: Value) -> Value {
    json!({"name": name, "models": models})
}

fn model_at(id: &str, base: &str) -> Value {
    json!({"id": id, "protocol": "openai-responses",
        "base_url": base, "context_window": 1000,
        "cost": {"input": 1.0, "output": 2.0}})
}

#[test]
fn models_skips_an_extension_whose_data_names_an_undeclared_provider() {
    let setup = Setup::new();
    install_data(
        &setup.home(),
        "a-ext",
        &json!({"a1": ["http://127.0.0.1:9/v1"]}),
        &[
            (
                "a1",
                data_with("a1", json!([model_at("m", "http://127.0.0.1:9/v1")])),
            ),
            (
                "a2",
                data_with("a2", json!([model_at("m", "http://127.0.0.1:9/v1")])),
            ),
        ],
    );
    install_data(
        &setup.home(),
        "b-ext",
        &json!({"b": ["http://127.0.0.1:9/v1"]}),
        &[(
            "b",
            data_with("b", json!([model_at("m", "http://127.0.0.1:9/v1")])),
        )],
    );
    let run = setup.fiber(&["models"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(run.stdout.contains("b/m"), "stdout: {}", run.stdout);
    assert!(
        !run.stdout.contains("a1/m") && !run.stdout.contains("a2/m"),
        "stdout: {}",
        run.stdout
    );
    assert_eq!(
        run.stderr.matches("a2").count(),
        1,
        "stderr: {}",
        run.stderr
    );
}

#[test]
fn models_skips_an_extension_whose_lua_registers_an_undeclared_provider() {
    let setup = Setup::new();
    install_data(
        &setup.home(),
        "c-ext",
        &json!({"c": ["http://127.0.0.1:9/v1"]}),
        &[(
            "c",
            data_with("c", json!([model_at("m", "http://127.0.0.1:9/v1")])),
        )],
    );
    let src = setup.root.path().join("src-lua");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("extension.json"),
        json!({"name": "c-ext", "version": "v0.0.0", "fiber": "0.0.0", "api": 1,
               "providers": {"c": ["http://127.0.0.1:9/v1"]}})
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        { std::fs::create_dir_all((src.join("providers/rogue.lua")).parent().unwrap()).unwrap(); src.join("providers/rogue.lua") },
        "fiber.provider(\"rogue\", { credential = { timeout = 1000, run = function() return { token = \"t\", expires_at = 4102444800 } end } })\n",
    )
    .unwrap();
    // Install over the data extension dir is not supported; install as a second copy is
    // skipped: instead rewrite the installed init.lua in place.
    std::fs::write(
        { std::fs::create_dir_all((setup.home().join("extensions").join("c-ext").join("providers/rogue.lua")).parent().unwrap()).unwrap(); setup.home().join("extensions").join("c-ext").join("providers/rogue.lua") },
        "fiber.provider(\"rogue\", { credential = { timeout = 1000, run = function() return { token = \"t\", expires_at = 4102444800 } end } })\n",
    )
    .unwrap();
    let run = setup.fiber(&["models"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(!run.stdout.contains("c/m"), "stdout: {}", run.stdout);
    assert_eq!(
        run.stderr.matches("rogue").count(),
        1,
        "stderr: {}",
        run.stderr
    );
}

#[test]
fn models_leaves_out_a_model_whose_base_url_is_not_listed() {
    let setup = Setup::new();
    install_data(
        &setup.home(),
        "fake-ext",
        &json!({"fake": ["http://127.0.0.1:9/v1"]}),
        &[(
            "fake",
            data_with(
                "fake",
                json!([
                    model_at("m", "http://127.0.0.1:9/v1"),
                    model_at("n", "http://127.0.0.1:9/v1/"),
                ]),
            ),
        )],
    );
    let run = setup.fiber(&["models"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(run.stdout.contains("fake/m"), "stdout: {}", run.stdout);
    assert!(!run.stdout.contains("fake/n"), "stdout: {}", run.stdout);
    assert!(run.stderr.contains("fake/n"), "stderr: {}", run.stderr);
}

/// Runs `fiber` with `args` in `dir`: [`Setup::fiber`] anywhere but the
/// workspace, under the test's [`Deadline`].
#[track_caller]
fn fiber_in(setup: &Setup, dir: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(setup.exe());
    command
        .args(args)
        .current_dir(dir)
        .env_clear()
        .envs(fakes::check_run())
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", setup.root.path())
        .env("FIBER_HOME", setup.home())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let (child, watchdog) = spawn_watched(&mut command);
    let group = child.id();
    let guard = KillGroup(group);
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait_with_output()).unwrap());
    let output = match setup.deadline.recv(&finished) {
        Ok(output) => output.unwrap(),
        Err(_) => support::expired(
            setup.deadline,
            group,
            &finished,
            &format!("`fiber {}` to exit", args.join(" ")),
        ),
    }
    .into();
    assert!(
        !group_alive(setup.deadline, group),
        "`fiber` left a process behind"
    );
    std::mem::forget(guard);
    watchdog.stand_down(setup.deadline.cleanup_phase());
    output
}

#[test]
fn models_lists_an_approved_repository_providers_models() {
    let setup = Setup::new();
    let workspace = setup.root.path().join("w");
    git_init(&workspace);
    declare_repo_provider(
        &workspace,
        "fiber.test/acme",
        "acme",
        &json!([{"id": "big", "protocol": "openai-responses",
                 "base_url": "http://127.0.0.1:9/v1", "context_window": 1000}]),
    );
    let approved = setup.fiber(&["approve", "--yes"]);
    assert_eq!(approved.code, Some(0), "stderr: {}", approved.stderr);
    let run = setup.fiber(&["models"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(run.stdout.contains("acme/big"), "stdout: {}", run.stdout);
}

#[test]
fn models_outside_the_project_lists_no_repository_models() {
    let setup = Setup::new();
    let workspace = setup.root.path().join("w");
    git_init(&workspace);
    declare_repo_provider(
        &workspace,
        "fiber.test/acme",
        "acme",
        &json!([{"id": "big", "protocol": "openai-responses",
                 "base_url": "http://127.0.0.1:9/v1", "context_window": 1000}]),
    );
    let approved = setup.fiber(&["approve", "--yes"]);
    assert_eq!(approved.code, Some(0), "stderr: {}", approved.stderr);
    let elsewhere = fakes::TempDir::new("fm-outside");
    let run = fiber_in(&setup, elsewhere.path(), &["models"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(!run.stdout.contains("acme/big"), "stdout: {}", run.stdout);
}

#[test]
fn models_never_runs_an_unapproved_repository_providers_code() {
    let setup = Setup::new();
    let server = fakes::ProviderServer::start([fakes::Response::status(
        200,
        json!({"data": [{"id": "m1", "context_length": 1000}]}).to_string(),
    )])
    .unwrap();
    config::store_secret(
        &setup.home(),
        "fixture.url",
        &config::Secret::new(server.url()),
    )
    .unwrap();
    config::store_secret(
        &setup.home(),
        "fixture.api_key",
        &config::Secret::new("k".to_owned()),
    )
    .unwrap();
    // A Lua-only provider: its models list exists only if its code runs.
    // Nothing is approved, so nothing may run it.
    let workspace = setup.root.path().join("w");
    git_init(&workspace);
    let dir = workspace.join("repo");
    fs::create_dir_all(dir.join("providers")).unwrap();
    fs::write(
        dir.join("extension.json"),
        json!({"name": "fiber.test/lua", "version": "v0.0.0", "fiber": "0.0.0", "api": 1,
               "providers": {"fixture": [format!("{}/v1", server.url())]}})
        .to_string(),
    )
    .unwrap();
    fs::copy(
        fakes::lua_fixture().join("providers/fixture.lua"),
        dir.join("providers/fixture.lua"),
    )
    .unwrap();
    write(
        &workspace.join(".fiber/config.json"),
        &json!({"repository_extensions": [{"path": "repo"}]}),
    );
    let run = setup.fiber(&["models"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(!run.stdout.contains("fixture"), "stdout: {}", run.stdout);
    assert!(server.requests().is_empty(), "its code ran");
}
