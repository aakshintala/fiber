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
        let status = Command::new("git")
            .args(["init", "-q"])
            .arg(self.workspace())
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn declare_hooks(&self, hooks: &Value) {
        write(
            &self.workspace().join(".fiber/config/hooks.json"),
            json!({"hooks": hooks}).to_string(),
        );
    }

    fn approve(&self) {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(["approve", "--yes"])
            .current_dir(self.workspace())
            .env_clear()
            .envs(fakes::check_run())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path())
            .env("FIBER_HOME", self.home())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let output = command.output().unwrap();
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
    let tmp = fakes::TempDir::new("fiber-repo-hooks-skip");
    let home = tmp.path().join("h");
    let workspace = tmp.path().join("w");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&workspace).unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .arg(&workspace)
            .status()
            .unwrap()
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
    assert!(reloaded.hook_required_failures().is_empty());
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
