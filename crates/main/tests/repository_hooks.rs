//! Binary pinning of repository hooks: approved entries run from their
//! pinned copies, unapproved entries are withheld, and copy failures fail
//! closed for required hooks.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use fakes::ProviderServer;
use serde_json::{Value, json};
use support::Run;

fn write(file: &Path, text: impl AsRef<[u8]>) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, text).unwrap();
}

struct Setup {
    root: fakes::TempDir,
    deadline: support::Deadline,
    #[allow(dead_code, reason = "held only for its drop")]
    guard: fakes::Watchdog,
}

impl Setup {
    fn new() -> Self {
        let deadline = support::Deadline::start();
        let root = fakes::TempDir::new("fiber-repo-hooks");
        let guard = fakes::Watchdog::matching(&root.path().display().to_string());
        fs::create_dir_all(root.path().join("h")).unwrap();
        fs::create_dir_all(root.path().join("w")).unwrap();
        Self {
            deadline,
            root,
            guard,
        }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("h")
    }

    fn workspace(&self) -> PathBuf {
        self.root.path().join("w")
    }

    fn write_repo(&self, rel: &str, text: &str) {
        write(&self.workspace().join(rel), text);
    }

    fn provider(&self, server: &ProviderServer) {
        let source = self.root.path().join("src");
        write(
            &source.join("extension.json"),
            json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "providers": {"fake": [format!("{}/v1", server.url())]}}).to_string(),
        );
        write(
            &source.join("providers/fake.json"),
            json!({
                "name": "fake",
                "credential": {"env": "FIBER_TEST_FAKE_KEY"},
                "models": [{"id": "m", "protocol": "openai-responses", "base_url": format!("{}/v1", server.url()), "context_window": 100000}]
            }).to_string(),
        );
        extensions::plan(
            &self.home(),
            &extensions::Request::Path(source),
            "0.0.0",
            &extensions::Origin::github(),
            &*fakes::clock::FakeClock::new(),
        )
        .unwrap()
        .commit()
        .unwrap();
        write(
            &self.home().join("config.json"),
            json!({"model": "fake/m"}).to_string(),
        );
    }

    fn install_hooks(&self) {
        extensions::plan(
            &self.home(),
            &extensions::Request::Path(
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../extensions/hooks"),
            ),
            "0.0.0",
            &extensions::Origin::github(),
            &*fakes::clock::FakeClock::new(),
        )
        .unwrap()
        .commit()
        .unwrap();
    }

    fn git_init(&self) {
        // Every blocking wait runs under the test's deadline
        // (`docs/testing.md`, "Waits and timeouts").
        let mut git = Command::new("git");
        git.args(["init", "-q"])
            .arg(self.workspace())
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let output = support::run_to_exit(self.deadline, "git init", git);
        assert!(output.status.success(), "{output:?}");
    }

    fn declare_hooks(&self, hooks: &Value) {
        write(
            &self.workspace().join(".fiber/config/hooks.json"),
            json!({"hooks": hooks}).to_string(),
        );
    }

    fn approve(&self) {
        // The approve run waits under the test's deadline, like `run_ask`
        // below (`docs/testing.md`, "Waits and timeouts").
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(["approve", "--yes"])
            .current_dir(self.workspace())
            .env_clear()
            .envs(fakes::check_run())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path())
            .env("FIBER_HOME", self.home())
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let output = support::run_to_exit(self.deadline, "`fiber approve` to exit", command);
        assert!(output.status.success(), "{output:?}");
    }

    fn run_ask(&self) -> Run {
        use std::sync::mpsc;
        use std::thread;
        use support::{KillGroup, spawn_watched};
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(["ask", "hi"])
            .current_dir(self.workspace())
            .env_clear()
            .envs(fakes::check_run())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path())
            .env("FIBER_HOME", self.home())
            .env("FIBER_TEST_FAKE_KEY", "sk-test")
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
            Err(_) => support::expired(self.deadline, group, &finished, "`fiber ask` to exit"),
        };
        std::mem::forget(guard);
        watchdog.stand_down(self.deadline.cleanup_phase());
        let stdout = String::from_utf8(output.stdout).unwrap();
        let lines: Vec<Value> = stdout
            .lines()
            .filter(|l| !support::is_status(l))
            .map(|line| serde_json::from_str(line).unwrap_or(Value::Null))
            .collect();
        Run {
            code: output.status.code(),
            lines,
            stderr: String::from_utf8(output.stderr).unwrap(),
        }
    }
}

/// A running `fiber session` with a `full` client attached: the binary
/// gate for an offer the client answers `skip`. Every wait takes the
/// test's deadline (`docs/testing.md`, "Waits and timeouts").
struct Interactive {
    child: std::process::Child,
    guard: support::KillGroup,
    watchdog: fakes::Watchdog,
    reader: std::thread::JoinHandle<()>,
    client: support::Socket,
}

impl Interactive {
    fn start(setup: &Setup, id: &str) -> Self {
        use std::io::{BufRead, BufReader};
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args([
                "session",
                "--id",
                id,
                "--workspace",
                setup.workspace().to_str().unwrap(),
            ])
            .current_dir(setup.workspace())
            .env_clear()
            .envs(fakes::check_run())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", setup.root.path())
            .env("FIBER_HOME", setup.home())
            .env("FIBER_TEST_FAKE_KEY", "sk-test")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (mut child, watchdog) = support::spawn_watched(&mut command);
        let guard = support::KillGroup(child.id());
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        support::first_line(setup.deadline, &lines);
        let client = support::Socket::connect(setup.deadline, &setup.home().join("run").join(id));
        client.send(r#"{"id":"sub","command":"subscribe","args":{"level":"full"}}"#);
        // `clients` is written when the `full` connection attaches, with no
        // order against the startup events: the wait returns once both
        // `clients` and `extensions_loaded` have arrived.
        let mut seen_clients = false;
        let mut seen_extensions = false;
        support::until(&client, "startup", |line| {
            if line["kind"] == "clients" {
                seen_clients = true;
            }
            if line["kind"] == "extensions_loaded" {
                seen_extensions = true;
            }
            seen_clients && seen_extensions
        });
        Self {
            child,
            guard,
            watchdog,
            reader,
            client,
        }
    }

    fn wait(&self, what: &str, kind: &str) -> Value {
        support::until(&self.client, what, |line| line["kind"] == kind)
            .last()
            .unwrap()
            .clone()
    }

    fn finish(self, setup: &Setup) -> bool {
        let Self {
            mut child,
            guard,
            watchdog,
            reader,
            client,
        } = self;
        client.send(r#"{"id":"close","command":"close"}"#);
        support::until_close(&client);
        let group = child.id();
        let (tx, done) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            tx.send(child.wait()).unwrap();
        });
        let status = setup.deadline.recv(&done).unwrap().unwrap();
        assert!(!fakes::group_alive(group));
        std::mem::forget(guard);
        watchdog.stand_down(setup.deadline.cleanup_phase());
        reader.join().unwrap();
        status.success()
    }
}

fn sh_entry(script: &str, on_failure: &str) -> Value {
    json!({
        "point": "session_start",
        "command": "/bin/sh",
        "args": [script],
        "timeout": 10000,
        "on_failure": on_failure,
    })
}

fn notice(run: &Run, code: &str, name: &str) -> bool {
    run.lines.iter().any(|line| {
        line.get("kind").and_then(Value::as_str) == Some("notice")
            && line
                .get("payload")
                .and_then(|p| p.get("code"))
                .and_then(Value::as_str)
                == Some(code)
            && line
                .get("payload")
                .and_then(|p| p.get("message"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .contains(name)
    })
}

#[test]
fn person_entry_runs_while_unapproved_same_name_is_withheld() {
    let setup = Setup::new();
    let server = ProviderServer::start([support::hello()]).unwrap();
    setup.provider(&server);
    setup.install_hooks();
    setup.git_init();
    write(
        &setup.home().join("config/hooks.json"),
        json!({"hooks": {"audit": sh_entry("person.sh", "non-blocking")}}).to_string(),
    );
    write(
        &setup.workspace().join("person.sh"),
        "#!/bin/sh\necho person >> ran.txt\n",
    );
    setup.write_repo("scripts/repo.sh", "#!/bin/sh\necho repo >> ran.txt\n");
    setup.declare_hooks(&json!({"audit": {
        "point": "session_start",
        "command": "/bin/sh",
        "args": ["scripts/repo.sh"],
        "timeout": 10000,
        "on_failure": "non-blocking",
    }}));
    let run = setup.run_ask();
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let ran = fs::read_to_string(setup.workspace().join("ran.txt")).unwrap_or_default();
    assert!(ran.contains("person"), "the person's hook ran: {ran:?}");
    assert!(
        !ran.contains("repo"),
        "the repository hook did not run: {ran:?}"
    );
    assert!(
        notice(&run, "repository_code_skipped", "audit"),
        "the unapproved entry is skipped with a notice"
    );
}

#[test]
fn unapproved_blocking_and_non_blocking_are_withheld_at_startup() {
    for (name, on_failure) in [("guard", "blocking"), ("note", "non-blocking")] {
        let setup = Setup::new();
        let server = ProviderServer::start([support::hello()]).unwrap();
        setup.provider(&server);
        setup.install_hooks();
        setup.git_init();
        setup.write_repo("scripts/repo.sh", "#!/bin/sh\necho hi\n");
        setup.declare_hooks(&json!({name: {
            "point": "session_start",
            "command": "/bin/sh",
            "args": ["scripts/repo.sh"],
            "timeout": 10000,
            "on_failure": on_failure,
        }}));
        let run = setup.run_ask();
        assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
        assert!(
            notice(&run, "repository_code_skipped", name),
            "unapproved {on_failure} is withheld with a skip notice"
        );
    }
}

#[test]
fn approved_hook_runs_from_its_pinned_copy_in_the_workspace() {
    let setup = Setup::new();
    let server = ProviderServer::start([support::hello()]).unwrap();
    setup.provider(&server);
    setup.install_hooks();
    setup.git_init();
    setup.write_repo(
        "scripts/run.sh",
        "#!/bin/sh\nprintf '%s\\n' \"$0\" > argv0.txt\npwd > cwd.txt\n",
    );
    setup.declare_hooks(&json!({"fmt": sh_entry("scripts/run.sh", "non-blocking")}));
    setup.approve();
    let run = setup.run_ask();
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let argv0 = fs::read_to_string(setup.workspace().join("argv0.txt")).unwrap();
    assert!(argv0.contains("pinned"), "runs the pinned copy: {argv0:?}");
    assert!(
        !argv0.contains(&setup.workspace().display().to_string()),
        "never the repository's files: {argv0:?}"
    );
    let cwd = fs::read_to_string(setup.workspace().join("cwd.txt")).unwrap();
    assert_eq!(
        cwd.trim(),
        fs::canonicalize(setup.workspace())
            .unwrap()
            .display()
            .to_string()
            .as_str()
    );
}

#[test]
fn pinned_script_reads_live_workspace_data_and_misses_its_sibling() {
    let setup = Setup::new();
    let server = ProviderServer::start([support::hello()]).unwrap();
    setup.provider(&server);
    setup.install_hooks();
    setup.git_init();
    setup.write_repo("data/current.txt", "v1\n");
    setup.write_repo("lib.sh", "helper\n");
    setup.write_repo(
        "scripts/read.sh",
        "#!/bin/sh\ncat data/current.txt > data.txt\nif [ -f \"$(dirname \"$0\")/lib.sh\" ]; then echo found > sibling.txt; else echo missing > sibling.txt; fi\n",
    );
    setup.declare_hooks(&json!({"fmt": sh_entry("scripts/read.sh", "non-blocking")}));
    setup.approve();
    setup.run_ask();
    assert_eq!(
        fs::read_to_string(setup.workspace().join("data.txt")).unwrap(),
        "v1\n"
    );
    assert_eq!(
        fs::read_to_string(setup.workspace().join("sibling.txt"))
            .unwrap()
            .trim(),
        "missing",
        "only named files are pinned"
    );
    setup.write_repo("data/current.txt", "v2\n");
    setup.run_ask();
    assert_eq!(
        fs::read_to_string(setup.workspace().join("data.txt")).unwrap(),
        "v2\n",
        "the pinned script reads the live file"
    );
}

fn break_copy_source(setup: &Setup, rel: &str) {
    use std::os::unix::fs::PermissionsExt;
    // Remove the finished pinned copy and its ready marker, keeping the
    // approval record: the next start must rebuild from the source.
    // The fixture declares exactly one hook, so exactly one approval file
    // holds its hash (`docs/extensions.md`, "Pinning": approving copies
    // those files into Fiber home).
    let approvals: Vec<String> = fs::read_dir(setup.home().join("projects"))
        .unwrap()
        .flat_map(|e| fs::read_dir(e.unwrap().path().join("approvals")).unwrap())
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(approvals.len(), 1);
    let hash = approvals.first().unwrap();
    fs::remove_dir_all(setup.home().join("pinned").join(hash)).unwrap();
    fs::remove_file(setup.home().join("pinned").join(format!("{hash}.ready"))).unwrap();
    // Deny the test user's open without changing size or mtime: the index
    // trusts its cached digest, so startup reaches `ensure_copy` where the
    // read fails.
    let file = setup.workspace().join(rel);
    fs::set_permissions(&file, fs::Permissions::from_mode(0o000)).unwrap();
    assert!(
        fs::File::open(&file).is_err(),
        "the copy source cannot be read"
    );
}

#[test]
fn approved_required_copy_failure_fails_start_with_hook_failed() {
    let setup = Setup::new();
    let server = ProviderServer::start([support::hello()]).unwrap();
    setup.provider(&server);
    setup.install_hooks();
    setup.git_init();
    setup.write_repo("scripts/run.sh", "#!/bin/sh\necho hi\n");
    setup.declare_hooks(&json!({"fmt": {
        "point": "session_start",
        "command": "/bin/sh",
        "args": ["scripts/run.sh"],
        "timeout": 10000,
        "on_failure": "non-blocking",
        "required": true,
    }}));
    setup.approve();
    break_copy_source(&setup, "scripts/run.sh");
    let run = setup.run_ask();
    assert_eq!(run.code, Some(1), "stderr: {}", run.stderr);
    let exited = run
        .lines
        .iter()
        .find(|l| l["kind"] == "fiber_exited")
        .expect("fiber_exited");
    assert_eq!(exited["payload"]["error"]["code"], "hook_failed");
    assert!(
        exited["payload"]["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("`fmt`"),
        "exit 1 names the hook: {exited:?}"
    );
}

#[test]
fn approved_required_copy_failure_fails_without_the_hooks_extension() {
    // The required gate consumes classified copy failures, never loaded
    // extension names (`docs/extensions.md`, "Hooks declared in configuration":
    // a `required` copy failure stops the start). Without the `hooks`
    // extension installed, the same failure still exits 1 with `hook_failed`
    // naming the hook. Removing the gate's failure branch lets this start and
    // fails this test.
    let setup = Setup::new();
    let server = ProviderServer::start([support::hello()]).unwrap();
    setup.provider(&server);
    setup.git_init();
    setup.write_repo("scripts/run.sh", "#!/bin/sh\necho hi\n");
    setup.declare_hooks(&json!({"fmt": {
        "point": "session_start",
        "command": "/bin/sh",
        "args": ["scripts/run.sh"],
        "timeout": 10000,
        "on_failure": "non-blocking",
        "required": true,
    }}));
    setup.approve();
    break_copy_source(&setup, "scripts/run.sh");
    let run = setup.run_ask();
    assert_eq!(run.code, Some(1), "stderr: {}", run.stderr);
    let exited = run
        .lines
        .iter()
        .find(|l| l["kind"] == "fiber_exited")
        .expect("fiber_exited");
    assert_eq!(exited["payload"]["error"]["code"], "hook_failed");
    assert!(
        exited["payload"]["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("`fmt`"),
        "exit 1 names the hook without the extension: {exited:?}"
    );
}

#[test]
fn approved_optional_copy_failure_skips_with_a_hook_failed_notice() {
    let setup = Setup::new();
    let server = ProviderServer::start([support::hello()]).unwrap();
    setup.provider(&server);
    setup.install_hooks();
    setup.git_init();
    setup.write_repo("scripts/run.sh", "#!/bin/sh\necho hi\n");
    setup.declare_hooks(&json!({"fmt": sh_entry("scripts/run.sh", "non-blocking")}));
    setup.approve();
    break_copy_source(&setup, "scripts/run.sh");
    let run = setup.run_ask();
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(
        notice(&run, "hook_failed", "fmt"),
        "a non-required copy failure is skipped with a notice"
    );
}

#[test]
fn deliberately_skipped_required_does_not_fail() {
    use contract::events::OfferedKind;
    use std::collections::BTreeSet;
    let deadline = support::Deadline::start();
    let tmp = fakes::TempDir::new("fiber-repo-hooks-skip");
    let home = tmp.path().join("h");
    let workspace = tmp.path().join("w");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&workspace).unwrap();
    // Every blocking wait runs under the test's deadline
    // (`docs/testing.md`, "Waits and timeouts").
    let mut git = Command::new("git");
    git.args(["init", "-q"])
        .arg(&workspace)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    assert!(
        support::run_to_exit(deadline, "git init", git)
            .status
            .success()
    );
    let write_repo = |rel: &str, text: &str| {
        let path = workspace.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    };
    write_repo("scripts/v1.sh", "v1\n");
    write_repo(
        ".fiber/config/hooks.json",
        json!({"hooks": {"fmt": {
            "point": "session_start",
            "command": "/bin/sh",
            "args": ["scripts/v1.sh"],
            "timeout": 10000,
            "on_failure": "non-blocking",
            "required": true,
        }}})
        .to_string()
        .as_str(),
    );
    let (_, project) = cli::project_of(&home, &workspace).unwrap();
    let clock = fakes::clock::FakeClock::new();
    let items = extensions::declared_items(&workspace, &*clock).unwrap();
    let v1 = items
        .into_iter()
        .find(|i| i.kind == OfferedKind::Hook)
        .unwrap();
    let v1_hash = extensions::hash(&mut extensions::Index::load(&home), &v1).unwrap();
    extensions::Store::new(&home, &project, fakes::clock::FakeClock::new())
        .approve(&v1, &v1_hash)
        .unwrap();
    write_repo("scripts/v2.sh", "v2\n");
    write_repo(
        ".fiber/config/hooks.json",
        json!({"hooks": {"fmt": {
            "point": "session_start",
            "command": "/bin/sh",
            "args": ["scripts/v2.sh"],
            "timeout": 10000,
            "on_failure": "non-blocking",
            "required": true,
        }}})
        .to_string()
        .as_str(),
    );
    let items = extensions::declared_items(&workspace, &*clock).unwrap();
    let v2 = items
        .into_iter()
        .find(|i| i.kind == OfferedKind::Hook)
        .unwrap();
    let v2_hash = extensions::hash(&mut extensions::Index::load(&home), &v2).unwrap();
    let mut skipped = BTreeSet::new();
    skipped.insert(("fmt".to_owned(), v2_hash));
    let config = config::Config::load(config::Sources {
        home: home.clone(),
        workspace: workspace.clone(),
        project: project.clone(),
        overrides: vec![],
    })
    .unwrap();
    let set1 = extensions::RepositorySet::load(
        &home,
        &project,
        &workspace,
        &BTreeSet::new(),
        extensions::Fetch::new(
            extensions::Origin::github(),
            "0.0.0",
            fakes::clock::FakeClock::new(),
        ),
    );
    let session = extensions::SessionExtensions::load_repository(
        &home,
        &config,
        fakes::clock::FakeClock::new(),
        std::sync::Arc::new(fakes::FakeLock::new()),
        None,
        extensions::StateSeed::new(),
        set1,
    );
    let set2 = extensions::RepositorySet::load(
        &home,
        &project,
        &workspace,
        &skipped,
        extensions::Fetch::new(
            extensions::Origin::github(),
            "0.0.0",
            fakes::clock::FakeClock::new(),
        ),
    );
    let reloaded = session.reload_repository(
        &home,
        &config,
        fakes::clock::FakeClock::new(),
        std::sync::Arc::new(fakes::FakeLock::new()),
        set2,
    );
    // A deliberately skipped required replacement is not a copy failure:
    // the required gate sees no failure (`docs/extensions.md`, "Hooks
    // declared in configuration").
    assert!(reloaded.first_hook_required_failure().is_none());
    assert!(
        !reloaded
            .hooks_config(&config)
            .extensions()
            .merged(&config::full_name("hooks"), &["hooks"])
            .unwrap()
            .0
            .as_object()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn deliberately_skipped_required_starts_at_the_binary_gate() {
    // The session-state case above proves the required gate sees no
    // failure for a skipped required replacement; this case proves the
    // binary starts through the same offer at its own gate
    // (`docs/extensions.md`, "Hooks declared in configuration": a skipped
    // replacement never fails the run). Every wait takes the test's
    // deadline (`docs/testing.md`, "Waits and timeouts").
    let setup = Setup::new();
    let server = ProviderServer::start([support::hello()]).unwrap();
    setup.provider(&server);
    setup.install_hooks();
    setup.git_init();
    setup.write_repo("scripts/v1.sh", "#!/bin/sh\necho v1\n");
    setup.declare_hooks(&json!({"fmt": {
        "point": "session_start",
        "command": "/bin/sh",
        "args": ["scripts/v1.sh"],
        "timeout": 10000,
        "on_failure": "non-blocking",
        "required": true,
    }}));
    setup.approve();
    setup.write_repo("scripts/v2.sh", "#!/bin/sh\necho v2\n");
    setup.declare_hooks(&json!({"fmt": {
        "point": "session_start",
        "command": "/bin/sh",
        "args": ["scripts/v2.sh"],
        "timeout": 10000,
        "on_failure": "non-blocking",
        "required": true,
    }}));
    let id = doors::mint("s_");
    let session = Interactive::start(&setup, &id);
    session.client.send(
        r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"hi"}]}}"#,
    );
    let offered = session.wait("the offer", "repository_code_offered");
    let items = offered["payload"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{offered:?}");
    assert_eq!(items[0]["name"], "fmt", "{offered:?}");
    assert_eq!(items[0]["kind"], "hook", "{offered:?}");
    let request = offered["payload"]["request_id"]
        .as_str()
        .unwrap()
        .to_owned();
    session.client.send(
        &json!({"id":"c_reply","command":"reply","args":{"request_id":request,"decisions":["skip"]}}).to_string(),
    );
    let resolved = session.wait("the resolution", "repository_code_resolved");
    assert_eq!(resolved["payload"]["decisions"], json!(["skip"]));
    session.wait("the turn", "turn_completed");
    assert!(session.finish(&setup), "the skipped required hook starts");
}
