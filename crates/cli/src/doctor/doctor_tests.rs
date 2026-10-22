//! The whole report through `run`: literals, hub states, environment
//! rendering, secrets-adjacent notices and exit codes.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::{Hub, Inputs, doctor, run};
use crate::CliError;
use config::{Secret, store_credential};
use contract::ErrorCode;
use contract::clock::Clock;
use fakes::Deadline;
use serde_json::{Value, json};

/// The version line the literals pin.
const VERSION: &str = "0.0.0 (abc1234)";

/// Set in the child that runs `doctor` itself.
const CHILD: &str = "FIBER_DOCTOR_CHILD";

/// Set in the child that deletes its working directory before running
/// `doctor` itself: it prints the bare directory-read error on stdout.
const DIR_CHILD: &str = "FIBER_DOCTOR_DIR_CHILD";

/// How long a re-run child may take.
const CHILD_DEADLINE: Duration = Duration::from_secs(50);

/// `has_userinfo` for the report tests: what a `@` in the URL means.
/// The token methods never run: doctor starts no MCP server.
struct Remote;

impl contract::McpRemote for Remote {
    fn has_userinfo(&self, url: &str) -> bool {
        url.contains('@')
    }

    fn token_key(&self, _url: &str) -> Result<String, contract::shapes::Failure> {
        unreachable!("doctor reads no MCP token")
    }

    fn login(
        &self,
        _files: &dyn contract::mcp_tokens::TokenFiles,
        _url: &str,
        _present: &dyn Fn(&str),
        _clock: &dyn contract::clock::Clock,
    ) -> Result<(), contract::shapes::Failure> {
        unreachable!("doctor logs in to no MCP server")
    }
}

/// A home with a workspace under it.
struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        let root = fakes::TempDir::under(Path::new("/tmp"), "fiber-doctor-report");
        fs::create_dir_all(root.path().join("home")).unwrap();
        fs::create_dir_all(root.path().join("workspace")).unwrap();
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn workspace(&self) -> PathBuf {
        self.root.path().join("workspace")
    }

    /// Places extension `fake` with model `m` at `base_url` and this
    /// credential source.
    fn place_fake(&self, base_url: &str, credential: Value) {
        let dir = self.home().join("extensions").join("fake");
        fs::create_dir_all(dir.join("providers")).unwrap();
        fs::write(
            dir.join("extension.json"),
            json!({"name": "fake", "version": "v1.0.0", "fiber": "0.1.0", "api": 1,
                   "providers": {"fake": [base_url]}})
            .to_string(),
        )
        .unwrap();
        fs::write(
            dir.join("providers").join("fake.json"),
            json!({"name": "fake", "credential": credential, "models": [
                {"id": "m", "protocol": "openai-responses",
                 "base_url": base_url, "context_window": 1000},
            ]})
            .to_string(),
        )
        .unwrap();
        fakes::write_record(&dir);
    }

    /// The ready report's provider: the default key in the environment,
    /// `work` stored.
    fn place_ready_provider(&self) {
        self.place_fake(
            "http://127.0.0.1:1/v1",
            json!({"env": "FIBER_TEST_FAKE_KEY"}),
        );
        self.write_config(&json!({"model": "fake/m"}));
        self.store("fake", "work", "k");
    }

    fn write_config(&self, value: &Value) {
        fs::write(self.home().join("config.json"), value.to_string()).unwrap();
    }

    fn write_repo(&self, value: &Value) {
        let dir = self.workspace().join(".fiber");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("config.json"), value.to_string()).unwrap();
    }

    fn store(&self, name: &str, label: &str, text: &str) {
        store_credential(&self.home(), name, label, &Secret::new(text.into())).unwrap();
    }

    /// Runs the report with this environment and no hub, and returns
    /// readiness with the whole stdout and stderr.
    fn report(&self, env: &HashMap<String, String>) -> (bool, String, String) {
        self.report_with(env, Hub::NotRunning)
    }

    /// Runs the report with this environment and hub state, and returns
    /// readiness with the whole stdout and stderr.
    fn report_with(&self, env: &HashMap<String, String>, hub: Hub) -> (bool, String, String) {
        let clock = fakes::clock::FakeClock::new();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let ready = run(
            &Inputs {
                home: &self.home(),
                workspace: &self.workspace(),
                version: VERSION,
                now: clock.wall(),
                env: &|name| env.get(name).cloned(),
                has_userinfo: &|url| url.contains('@'),
                hub: &|_, _| hub.clone(),
                clock: &*clock,
            },
            &mut out,
            &mut err,
        )
        .unwrap();
        (
            ready,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    /// Every path under the home with its size and mtime, in order: the
    /// tree `run` must leave identical.
    fn snapshot(&self) -> Vec<(String, u64, SystemTime)> {
        let mut found = Vec::new();
        let mut dirs = vec![self.home()];
        while let Some(dir) = dirs.pop() {
            let mut entries: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap()).collect();
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let path = entry.path();
                let meta = fs::symlink_metadata(&path).unwrap();
                if meta.is_dir() {
                    dirs.push(path);
                } else {
                    found.push((
                        path.strip_prefix(self.home())
                            .unwrap()
                            .to_string_lossy()
                            .into_owned(),
                        meta.len(),
                        meta.modified().unwrap(),
                    ));
                }
            }
        }
        found.sort();
        found
    }
}

fn env_of(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

/// The ready home the literals pin: two startable MCP servers, `docs/`
/// installed, one log error two days old and one crash three hours old.
fn ready_home() -> Setup {
    let setup = Setup::new();
    setup.place_ready_provider();
    setup.write_config(&json!({
        "model": "fake/m",
        "mcp": {"servers": {
            "a": {"command": "run-a"},
            "b": {"url": "https://example.invalid/mcp"},
        }},
    }));
    fs::create_dir_all(setup.home().join("docs")).unwrap();
    let logs = setup.home().join("logs");
    fs::create_dir_all(&logs).unwrap();
    let ts = (1_700_000_000u64 - 2 * 86_400) * 1_000;
    fs::write(
        logs.join("hub.log"),
        format!(
            "{{\"ts\":{ts},\"level\":\"error\",\"code\":\"config_invalid\",\"message\":\"The file config.json is not valid JSON.\"}}\n"
        ),
    )
    .unwrap();
    let crashes = setup.home().join("crashes");
    fs::create_dir_all(&crashes).unwrap();
    let crash = crashes.join("s_0123456789abcdef-1699989200000.txt");
    fs::write(&crash, "traceback").unwrap();
    fs::File::options()
        .write(true)
        .open(&crash)
        .unwrap()
        .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 - 3 * 3_600))
        .unwrap();
    setup
}

#[test]
fn the_ready_report_equals_the_literal() {
    let setup = ready_home();
    let (ready, out, err) = setup.report(&env_of(&[("FIBER_TEST_FAKE_KEY", "k")]));
    assert!(ready);
    assert_eq!(err, "");
    let home = setup.home();
    assert_eq!(
        out,
        format!(
            "fiber {VERSION}\nmodel: fake/m (provider fake)\nfake:\n  * default  environment variable FIBER_TEST_FAKE_KEY\n    work     stored in credentials/fake/work\npermissions: auto\nMCP servers: 2\nhub: not running\nlogs: {logs}\nnewest error: hub.log, 2d old, config_invalid: The file config.json is not valid JSON.\nnewest crash: {crash}, 3h old\n",
            logs = home.join("logs").display(),
            crash = home
                .join("crashes/s_0123456789abcdef-1699989200000.txt")
                .display(),
        )
    );
}

#[test]
fn no_key_blocks_directly_after_its_block() {
    let setup = ready_home();
    let (ready, out, err) = setup.report(&env_of(&[]));
    assert!(!ready);
    assert_eq!(err, "");
    let home = setup.home();
    assert_eq!(
        out,
        format!(
            "fiber {VERSION}\nmodel: fake/m (provider fake)\nfake:\n  * default  environment variable FIBER_TEST_FAKE_KEY (no key)\n    work     stored in credentials/fake/work\nno key for fake: run `fiber login fake`\npermissions: auto\nMCP servers: 2\nhub: not running\nlogs: {logs}\nnewest error: hub.log, 2d old, config_invalid: The file config.json is not valid JSON.\nnewest crash: {crash}, 3h old\n",
            logs = home.join("logs").display(),
            crash = home
                .join("crashes/s_0123456789abcdef-1699989200000.txt")
                .display(),
        )
    );
}

#[test]
fn an_empty_missing_home_gives_the_empty_report_and_stays_missing() {
    let setup = Setup::new();
    let home = setup.home().join("missing");
    let clock = fakes::clock::FakeClock::new();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let ready = run(
        &Inputs {
            home: &home,
            workspace: &setup.workspace(),
            version: VERSION,
            now: clock.wall(),
            env: &|_| None,
            has_userinfo: &|url| url.contains('@'),
            hub: &|_, _| Hub::NotRunning,
            clock: &*clock,
        },
        &mut out,
        &mut err,
    )
    .unwrap();
    assert!(!ready);
    assert_eq!(String::from_utf8(err).unwrap(), "");
    assert_eq!(
        String::from_utf8(out).unwrap(),
        format!(
            "fiber {VERSION}\nNo model was chosen. Pass `--model provider/model`, or set `model` in configuration.\npermissions: auto\nMCP servers: 0\nhub: not running\nlogs: {logs}\nnewest error: none\nnewest crash: none\nuser guide not installed: run `fiber update`\n",
            logs = home.join("logs").display(),
        )
    );
    assert!(!home.exists(), "a missing home stays missing");
}

#[test]
fn the_report_leaves_an_existing_home_identical() {
    let setup = ready_home();
    let before = setup.snapshot();
    assert!(!before.is_empty());
    setup.report(&env_of(&[("FIBER_TEST_FAKE_KEY", "k")]));
    assert_eq!(setup.snapshot(), before);
}

#[test]
fn docs_missing_or_a_file_names_the_update_line() {
    let setup = Setup::new();
    setup.place_ready_provider();
    let env = env_of(&[("FIBER_TEST_FAKE_KEY", "k")]);
    let (_, out, _) = setup.report(&env);
    assert!(
        out.contains("\nuser guide not installed: run `fiber update`\n"),
        "{out}"
    );
    fs::write(setup.home().join("docs"), "not a directory").unwrap();
    let (_, out, _) = setup.report(&env);
    assert!(
        out.contains("\nuser guide not installed: run `fiber update`\n"),
        "{out}"
    );
    fs::remove_file(setup.home().join("docs")).unwrap();
    fs::create_dir(setup.home().join("docs")).unwrap();
    let (_, out, _) = setup.report(&env);
    assert!(!out.contains("user guide not installed"), "{out}");
}

#[test]
fn mcp_counts_zero_and_two() {
    let setup = Setup::new();
    setup.place_ready_provider();
    let env = env_of(&[("FIBER_TEST_FAKE_KEY", "k")]);
    let (ready, out, _) = setup.report(&env);
    assert!(ready);
    assert!(out.contains("\nMCP servers: 0\n"), "{out}");
    setup.write_config(&json!({
        "model": "fake/m",
        "mcp": {"servers": {
            "a": {"command": "run-a"},
            "b": {"url": "https://example.invalid/mcp"},
        }},
    }));
    let (ready, out, _) = setup.report(&env);
    assert!(ready);
    assert!(out.contains("\nMCP servers: 2\n"), "{out}");
}

#[test]
fn a_non_object_mcp_servers_is_a_configuration_blocker() {
    // Base config validation rejects a non-object `mcp.servers` before
    // doctor's count runs, as it does for a session: the report carries
    // the configuration blocker, not a count.
    let setup = Setup::new();
    setup.place_ready_provider();
    let env = env_of(&[("FIBER_TEST_FAKE_KEY", "k")]);
    setup.write_config(&json!({"model": "fake/m", "mcp": {"servers": [1, 2]}}));
    let (ready, out, _) = setup.report(&env);
    assert!(!ready);
    assert!(out.contains("configuration does not load:"), "{out}");
    assert!(!out.contains("MCP servers:"), "{out}");
}

#[test]
fn an_unknown_repository_key_is_one_stderr_line() {
    let setup = ready_home();
    setup.write_repo(&json!({"zzz_unknown": 1}));
    let (ready, out, err) = setup.report(&env_of(&[("FIBER_TEST_FAKE_KEY", "k")]));
    assert!(ready);
    let mut lines = err.lines();
    let only = lines.next().unwrap();
    assert!(lines.next().is_none(), "{err:?}");
    assert!(only.starts_with("fiber: "), "{only:?}");
    assert!(only.contains("zzz_unknown"), "{only:?}");
    assert!(out.contains("\nMCP servers: 2\n"), "{out}");
}

#[test]
fn an_extension_for_another_api_is_a_stderr_notice() {
    let setup = ready_home();
    let dir = setup.home().join("extensions").join("old");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("extension.json"),
        json!({"name": "old", "version": "v1.0.0", "fiber": "0.1.0", "api": 999,
               "providers": {}})
        .to_string(),
    )
    .unwrap();
    fakes::write_record(&dir);
    let (ready, out, err) = setup.report(&env_of(&[("FIBER_TEST_FAKE_KEY", "k")]));
    assert!(ready);
    assert!(out.contains("model: fake/m (provider fake)"), "{out}");
    let mut lines = err.lines();
    let only = lines.next().unwrap();
    assert!(lines.next().is_none(), "{err:?}");
    assert!(only.starts_with("fiber: "), "{only:?}");
    assert!(
        only.contains("was written for extension API 999"),
        "{only:?}"
    );
}

#[test]
fn no_request_reaches_the_provider_and_no_command_runs() {
    let setup = Setup::new();
    let server = fakes::ProviderServer::start(Vec::<fakes::Response>::new()).unwrap();
    setup.place_fake(
        &server.url(),
        json!({"command": ["touch", setup.home().join("marker").to_str().unwrap()]}),
    );
    setup.write_config(&json!({"model": "fake/m"}));
    let (ready, _, _) = setup.report(&env_of(&[]));
    assert!(ready);
    assert!(server.requests().is_empty());
    assert!(!setup.home().join("marker").exists());
}

/// An `out` that fails every write, recording what it was asked for.
struct FailingWriter {
    attempted: Vec<String>,
}

impl std::io::Write for FailingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.attempted
            .push(String::from_utf8_lossy(buf).into_owned());
        Err(std::io::Error::other("boom"))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn an_unwritable_report_is_doctor_report() {
    let setup = Setup::new();
    let clock = fakes::clock::FakeClock::new();
    let mut out = FailingWriter {
        attempted: Vec::new(),
    };
    let mut err = Vec::new();
    let error = run(
        &Inputs {
            home: &setup.home(),
            workspace: &setup.workspace(),
            version: VERSION,
            now: clock.wall(),
            env: &|_| None,
            has_userinfo: &|_| false,
            hub: &|_, _| Hub::NotRunning,
            clock: &*clock,
        },
        &mut out,
        &mut err,
    )
    .unwrap_err();
    assert!(
        matches!(error, CliError::DoctorReport(_)),
        "expected DoctorReport, got {error:?}"
    );
    assert_eq!(error.code(), ErrorCode::IoFailed);
    // The report stops where it starts: the version line is the only
    // line it tried to write. `writeln!` writes the line before its
    // line feed, and that first write already fails.
    assert_eq!(out.attempted, vec![format!("fiber {VERSION}")]);
}

/// Runs the test `test` of this module in a child with `FIBER_HOME` set
/// to `home` and `extra` in its environment, and returns how it exited.
/// The key the fixtures read is always removed first, so the parent's
/// environment cannot leak into the child's. The child is killed, and
/// the test fails, at `CHILD_DEADLINE`.
#[track_caller]
fn child(test: &str, home: &Path, extra: &[(&str, &str)]) -> std::process::ExitStatus {
    child_output(test, home, extra).status
}

/// Like [`child`], but returns the whole run so the parent can read
/// stderr off it. Built on the same environment setup.
#[track_caller]
fn child_output(test: &str, home: &Path, extra: &[(&str, &str)]) -> std::process::Output {
    let module = module_path!().split_once("::").unwrap().1;
    fakes::rerun_prepared(
        &format!("{module}::{test}"),
        &[(CHILD, "1")],
        Deadline::after(CHILD_DEADLINE),
        |command| {
            command.env("FIBER_HOME", home);
            command.env_remove("FIBER_TEST_FAKE_KEY");
            for (key, value) in extra {
                command.env(key, value);
            }
        },
    )
}

#[test]
fn doctor_exits_0_for_a_ready_home() {
    if std::env::var_os(CHILD).is_some() {
        let clock = fakes::clock::FakeClock::new();
        std::process::exit(doctor(VERSION, &*clock, &Remote));
    }
    let setup = ready_home();
    let status = child(
        "doctor_exits_0_for_a_ready_home",
        &setup.home(),
        &[("FIBER_TEST_FAKE_KEY", "k")],
    );
    assert_eq!(status.code(), Some(0), "{status:?}");
}

#[test]
fn doctor_exits_1_for_a_blocked_home() {
    if std::env::var_os(CHILD).is_some() {
        let clock = fakes::clock::FakeClock::new();
        std::process::exit(doctor(VERSION, &*clock, &Remote));
    }
    let setup = Setup::new();
    setup.place_ready_provider();
    let status = child("doctor_exits_1_for_a_blocked_home", &setup.home(), &[]);
    assert_eq!(status.code(), Some(1), "{status:?}");
}

#[test]
fn doctor_exits_1_for_a_missing_home_and_leaves_it_missing() {
    if std::env::var_os(CHILD).is_some() {
        let clock = fakes::clock::FakeClock::new();
        std::process::exit(doctor(VERSION, &*clock, &Remote));
    }
    let setup = Setup::new();
    let missing = setup.home().join("missing");
    let status = child(
        "doctor_exits_1_for_a_missing_home_and_leaves_it_missing",
        &missing,
        &[],
    );
    assert_eq!(status.code(), Some(1), "{status:?}");
    assert!(!missing.exists(), "a missing home stays missing");
}

#[test]
fn doctor_exits_2_for_a_relative_home() {
    if std::env::var_os(CHILD).is_some() {
        let clock = fakes::clock::FakeClock::new();
        std::process::exit(doctor(VERSION, &*clock, &Remote));
    }
    let status = child(
        "doctor_exits_2_for_a_relative_home",
        Path::new("relative"),
        &[],
    );
    assert_eq!(status.code(), Some(2), "{status:?}");
}

#[test]
#[allow(clippy::print_stdout, reason = "the parent test reads this line")]
fn doctor_exits_1_for_a_deleted_directory() {
    if std::env::var_os(DIR_CHILD).is_some() {
        let dir = std::env::temp_dir().join(format!("fiber-doctor-cwd-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        std::env::set_current_dir(&dir).unwrap();
        fs::remove_dir_all(&dir).unwrap();
        // The bare text the parent compares stderr against, read here
        // in the child: the parent never leaves its own directory.
        println!("{}", std::env::current_dir().unwrap_err());
        let clock = fakes::clock::FakeClock::new();
        std::process::exit(doctor(VERSION, &*clock, &Remote));
    }
    let setup = Setup::new();
    let output = child_output(
        "doctor_exits_1_for_a_deleted_directory",
        &setup.home(),
        &[(DIR_CHILD, "1")],
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let module = module_path!().split_once("::").unwrap().1;
    let bare = stdout
        .find("running 1 test\n")
        .map(|at| &stdout[at + "running 1 test\n".len()..])
        .and_then(|rest| {
            rest.strip_prefix(&format!(
                "test {module}::doctor_exits_1_for_a_deleted_directory ... "
            ))
        })
        .unwrap_or_else(|| panic!("the child wrote no bare error line: {stdout:?}"));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert_eq!(
        stderr,
        format!("fiber: doctor could not read the current directory: {bare}")
    );
}

/// The hub lines for a running hub, by environment: the tests inject the
/// exchange's state, so no socket runs.
fn running(environment: Result<contract::events::Variables, String>) -> Hub {
    Hub::Running { environment }
}

fn login_shell() -> contract::events::Variables {
    contract::events::Variables {
        path: "/hub/bin:/usr/bin".to_owned(),
        names: vec!["EDITOR".to_owned(), "FIBER_HOME".to_owned()],
        source: contract::events::VariablesSource::LoginShell,
    }
}

#[test]
fn a_running_hub_names_the_login_shell_and_its_variables() {
    let setup = ready_home();
    let (_, out, _) = setup.report_with(
        &env_of(&[("FIBER_TEST_FAKE_KEY", "k")]),
        running(Ok(login_shell())),
    );
    assert!(
        out.contains(
            "hub: running\nhub environment: login shell\nhub PATH: /hub/bin:/usr/bin\nhub variables: EDITOR, FIBER_HOME\n"
        ),
        "{out}"
    );
}

#[test]
fn an_inherited_hub_environment_says_the_capture_failed() {
    let setup = ready_home();
    let variables = contract::events::Variables {
        source: contract::events::VariablesSource::Inherited,
        ..login_shell()
    };
    let (_, out, _) = setup.report_with(
        &env_of(&[("FIBER_TEST_FAKE_KEY", "k")]),
        running(Ok(variables)),
    );
    assert!(
        out.contains(
            "hub: running\nhub environment: inherited, because the login-shell capture failed\nhub PATH: /hub/bin:/usr/bin\nhub variables: EDITOR, FIBER_HOME\n"
        ),
        "{out}"
    );
}

#[test]
fn an_unknown_hub_environment_names_its_reason() {
    let setup = ready_home();
    let (_, out, _) = setup.report_with(
        &env_of(&[("FIBER_TEST_FAKE_KEY", "k")]),
        running(Err(
            "the hub closed the connection before answering status".to_owned()
        )),
    );
    assert!(
        out.contains(
            "hub: running\nhub environment: unknown (the hub closed the connection before answering status)\n"
        ),
        "{out}"
    );
}

#[test]
fn an_unknown_hub_names_its_reason() {
    let setup = ready_home();
    let (_, out, _) = setup.report_with(
        &env_of(&[("FIBER_TEST_FAKE_KEY", "k")]),
        Hub::Unknown("the hub: connection refused".to_owned()),
    );
    assert!(
        out.contains("hub: unknown (the hub: connection refused)\n"),
        "{out}"
    );
}

#[test]
fn escapes_in_hub_variables_print_on_one_line() {
    let setup = ready_home();
    let variables = contract::events::Variables {
        path: "/hu\nb/bin".to_owned(),
        names: vec!["ED\x1bITOR".to_owned()],
        ..login_shell()
    };
    let (_, out, _) = setup.report_with(
        &env_of(&[("FIBER_TEST_FAKE_KEY", "k")]),
        running(Ok(variables)),
    );
    assert!(
        out.contains("hub PATH: /hu b/bin\nhub variables: ED ITOR\n"),
        "{out}"
    );
}

#[test]
fn a_regular_file_at_run_hub_is_unknown_not_a_hang() {
    let setup = Setup::new();
    let run = setup.home().join("run");
    fs::create_dir_all(&run).unwrap();
    fs::write(run.join("hub"), b"not a socket").unwrap();
    let clock = fakes::clock::FakeClock::new();
    let origin = clock.origin();
    // A connect that fails fast moves no clock: the probe returns.
    let Hub::Unknown(reason) = super::probe(&setup.home(), &*clock) else {
        panic!("a regular file at run/hub is unknown");
    };
    assert!(reason.starts_with("the hub: "), "{reason}");
    assert_eq!(clock.now(), origin);
}

impl Setup {
    /// Hand-places the extension `name` declaring these secrets and no
    /// providers: the record marks it installed.
    fn place_secrets(&self, name: &str, secrets: &[&str]) {
        let dir = self.home().join("extensions").join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("extension.json"),
            json!({"name": name, "version": "v1.0.0", "fiber": "0.1.0", "api": 1,
                   "secrets": secrets})
            .to_string(),
        )
        .unwrap();
        fakes::write_record(&dir);
    }

    /// Writes the per-project configuration for this workspace's project.
    fn write_project(&self, value: &Value) {
        let (_, project) = crate::project_of(&self.home(), &self.workspace()).unwrap();
        let dir = self.home().join("projects").join(project.as_str());
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("config.json"), value.to_string()).unwrap();
    }
}

#[test]
fn extension_secrets_list_the_missing_ones_in_order() {
    let setup = Setup::new();
    setup.place_ready_provider();
    setup.place_secrets("acme", &["acme.api_key", "acme.region", "fake"]);
    setup.place_secrets("off", &["off.key"]);
    setup.place_secrets("scoped", &["scoped.key"]);
    setup.write_config(&json!({
        "model": "fake/m",
        "extensions": {"off": {"enabled": false}, "scoped": {"enabled": false}},
    }));
    setup.write_project(&json!({"extensions": {"scoped": {"enabled": true}}}));
    config::store_secret(&setup.home(), "acme.region", &Secret::new("region".into())).unwrap();
    let (ready, out, _) = setup.report(&env_of(&[("FIBER_TEST_FAKE_KEY", "k")]));
    assert!(ready, "{out}");
    let api = out
        .find("secret acme.api_key (from acme) is not stored: run `fiber login acme.api_key`")
        .unwrap();
    let scoped = out
        .find("secret scoped.key (from scoped) is not stored: run `fiber login scoped.key`")
        .unwrap();
    assert!(api < scoped, "declared_secrets order: {out}");
    // The stored region, the provider-named secret and the disabled
    // extension's secret print nothing.
    for silent in ["secret acme.region", "secret fake", "secret off.key"] {
        assert!(!out.contains(silent), "{silent} is silent: {out}");
    }
}

#[test]
fn a_symlinked_secret_prints_the_cannot_be_read_line() {
    let setup = Setup::new();
    setup.place_ready_provider();
    setup.place_secrets("acme", &["acme.api_key"]);
    setup.write_config(&json!({"model": "fake/m"}));
    let credentials = setup.home().join("credentials");
    fs::create_dir_all(&credentials).unwrap();
    let target = credentials.join("target");
    fs::write(&target, "linked").unwrap();
    std::os::unix::fs::symlink(&target, credentials.join("acme.api_key")).unwrap();
    let (ready, out, _) = setup.report(&env_of(&[("FIBER_TEST_FAKE_KEY", "k")]));
    assert!(ready, "{out}");
    assert!(
        out.contains("secret acme.api_key (from acme) cannot be read:"),
        "{out}"
    );
}
