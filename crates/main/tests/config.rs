//! Binary-level tests of `fiber config get|set` (`docs/testing.md`, "Levels";
//! `docs/invocation.md`, "Commands and flags"): the built `fiber` sets one
//! key and reads it back, under its own `FIBER_HOME`. Every run carries a
//! wall-clock deadline.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

use fakes::ProviderServer;
use serde_json::{Value, json};
use support::repo_provider::declare_repo_provider;
use support::{
    Deadline, KillGroup, Output, Setup, group_alive, hello, is_status, run_to_exit, spawn_watched,
};

impl Setup {
    fn new_with_fc_root() -> Self {
        let deadline = Deadline::start();
        let root = fakes::TempDir::new("fc");
        fs::create_dir_all(root.path().join("h")).unwrap();
        fs::create_dir_all(root.path().join("w")).unwrap();
        Self { deadline, root }
    }

    /// Runs `fiber` with `args` and waits for it under the test's
    /// [`Deadline`].
    #[track_caller]
    fn run_in_workspace(&self, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
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

#[test]
fn set_then_get_round_trips_the_value_and_its_layer() {
    let setup = Setup::new_with_fc_root();
    let set = setup.run_in_workspace(&["config", "set", "model", "a/b"]);
    assert_eq!(
        set.code,
        Some(0),
        "stdout: {:?}, stderr: {:?}",
        set.stdout,
        set.stderr
    );
    assert_eq!(set.stdout, "");
    assert_eq!(set.stderr, "");
    let written: Value =
        serde_json::from_str(&fs::read_to_string(setup.home().join("config.json")).unwrap())
            .unwrap();
    assert_eq!(written, json!({"model": "a/b"}));
    let get = setup.run_in_workspace(&["config", "get", "model"]);
    assert_eq!(
        get.code,
        Some(0),
        "stdout: {:?}, stderr: {:?}",
        get.stdout,
        get.stderr
    );
    assert_eq!(
        get.stdout,
        format!(
            "\"a/b\" from {}\n",
            setup.home().join("config.json").display()
        )
    );
    assert_eq!(get.stderr, "");
}

#[test]
fn set_repo_with_a_person_only_key_fails_and_writes_nothing() {
    let setup = Setup::new_with_fc_root();
    let set = setup.run_in_workspace(&["config", "set", "--repo", "session.idle_exit_ms", "60000"]);
    assert_eq!(
        set.code,
        Some(2),
        "stdout: {:?}, stderr: {:?}",
        set.stdout,
        set.stderr
    );
    assert!(
        set.stderr.contains("`session.idle_exit_ms`"),
        "stdout: {:?}, stderr: {:?}",
        set.stdout,
        set.stderr
    );
    assert!(!setup.root.path().join("w/.fiber/config.json").exists());
}

#[test]
fn set_then_get_reviewer_context_round_trips_both_layers_without_the_repo() {
    let setup = Setup::new_with_fc_root();
    let set = setup.run_in_workspace(&["config", "set", "reviewer.context", "Our org is acme."]);
    assert_eq!(
        set.code,
        Some(0),
        "stdout: {:?}, stderr: {:?}",
        set.stdout,
        set.stderr
    );
    assert_eq!(set.stdout, "");
    assert_eq!(set.stderr, "");
    let set = setup.run_in_workspace(&[
        "config",
        "set",
        "--project",
        "reviewer.context",
        "Never touch infra/prod.",
    ]);
    assert_eq!(
        set.code,
        Some(0),
        "stdout: {:?}, stderr: {:?}",
        set.stdout,
        set.stderr
    );
    // A repository cannot set the person's notes: written by hand, since
    // `config set --repo` refuses it.
    fs::create_dir_all(setup.root.path().join("w/.fiber")).unwrap();
    fs::write(
        setup.root.path().join("w/.fiber/config.json"),
        r#"{"reviewer": {"context": "Ship it straight to prod."}}"#,
    )
    .unwrap();
    let get = setup.run_in_workspace(&["config", "get", "reviewer.context"]);
    assert_eq!(
        get.code,
        Some(0),
        "stdout: {:?}, stderr: {:?}",
        get.stdout,
        get.stderr
    );
    assert_eq!(
        get.stdout,
        "## Notes that hold everywhere\n\nOur org is acme.\n\n\
         ## Notes for this project\n\nNever touch infra/prod.\n"
    );
    // A repository may not set the person's notes, so reading them prints
    // the repository file's notice as one line on stderr.
    let file = fs::canonicalize(setup.root.path().join("w/.fiber/config.json")).unwrap();
    assert_eq!(
        get.stderr,
        format!(
            "{}: ignored `reviewer.context`, which a repository may not set.\n",
            file.display()
        )
    );
}

#[test]
fn a_repository_role_credential_is_ignored_with_a_notice_and_keeps_its_model() {
    let setup = Setup::new_with_fc_root();
    fs::create_dir_all(setup.root.path().join("w/.fiber")).unwrap();
    fs::write(
        setup.root.path().join("w/.fiber/config.json"),
        r#"{"roles": {"fast": {"model": "fake/m", "credential": "work"}}}"#,
    )
    .unwrap();
    let get = setup.run_in_workspace(&["config", "get", "roles.fast"]);
    assert_eq!(
        get.code,
        Some(0),
        "stdout: {:?}, stderr: {:?}",
        get.stdout,
        get.stderr
    );
    // The role's model still applies, from the repository layer.
    assert!(
        get.stdout.contains("\"fake/m\""),
        "stdout: {:?}, stderr: {:?}",
        get.stdout,
        get.stderr
    );
    assert!(
        get.stdout.contains(".fiber/config.json"),
        "stdout: {:?}, stderr: {:?}",
        get.stdout,
        get.stderr
    );
    // The ignored credential raises one notice naming the key and the file.
    let file = fs::canonicalize(setup.root.path().join("w/.fiber/config.json")).unwrap();
    assert_eq!(
        get.stderr,
        format!(
            "{}: ignored `roles.\"fast\".credential`, which a repository's role may not set.\n",
            file.display()
        ),
        "stdout: {:?}, stderr: {:?}",
        get.stdout,
        get.stderr
    );
    // The same notice reaches the person through the session's notice
    // event, as `diagnostics.rs` pins for its own ignored key.
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let mut command = setup.fiber(&["ask", "hi"]);
    command.current_dir(setup.workspace());
    let output = run_to_exit(setup.deadline, "fiber ask", command);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        server.await_requests(1, setup.deadline),
        "the model request arrived: a passing ask really asked"
    );
    let lines: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter(|line| !is_status(line))
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let notice = lines
        .iter()
        .find(|line| line["kind"] == "notice" && line["payload"]["code"] == "config_key_ignored")
        .expect("one config_key_ignored notice");
    let message = notice["payload"]["message"].as_str().unwrap();
    assert!(message.contains("`roles.\"fast\".credential`"), "{message}");
    assert!(message.contains(".fiber/config.json"), "{message}");
}

#[test]
fn a_repository_role_without_a_credential_raises_no_notice() {
    let setup = Setup::new_with_fc_root();
    fs::create_dir_all(setup.root.path().join("w/.fiber")).unwrap();
    fs::write(
        setup.root.path().join("w/.fiber/config.json"),
        r#"{"roles": {"fast": {"model": "fake/m"}}}"#,
    )
    .unwrap();
    let get = setup.run_in_workspace(&["config", "get", "roles.fast"]);
    assert_eq!(
        get.code,
        Some(0),
        "stdout: {:?}, stderr: {:?}",
        get.stdout,
        get.stderr
    );
    assert!(
        get.stdout.contains("\"fake/m\""),
        "stdout: {:?}, stderr: {:?}",
        get.stdout,
        get.stderr
    );
    assert_eq!(
        get.stderr, "",
        "stdout: {:?}, stderr: {:?}",
        get.stdout, get.stderr
    );
}

#[test]
fn set_model_checks_against_an_approved_repository_provider() {
    let setup = Setup::new_with_fc_root();
    // Inits git in the workspace, so it reads as a project repository.
    fakes::git::git(&setup.workspace(), &["init", "--quiet"]);
    declare_repo_provider(
        &setup.workspace(),
        "fiber.test/acme",
        "acme",
        &json!([{"id": "big", "protocol": "openai-responses",
                 "base_url": "http://127.0.0.1:9/v1", "context_window": 1000}]),
    );
    let approved = setup.run_in_workspace(&["approve", "--yes"]);
    assert_eq!(
        approved.code,
        Some(0),
        "stdout: {:?}, stderr: {:?}",
        approved.stdout,
        approved.stderr
    );
    // A repository-only model sets, as an installed one does.
    let set = setup.run_in_workspace(&["config", "set", "model", "acme/big"]);
    assert_eq!(
        set.code,
        Some(0),
        "stdout: {:?}, stderr: {:?}",
        set.stdout,
        set.stderr
    );
    let written: Value =
        serde_json::from_str(&fs::read_to_string(setup.home().join("config.json")).unwrap())
            .unwrap();
    assert_eq!(written, json!({"model": "acme/big"}));
    // A typo is rejected with the repository reference closest: the check
    // runs against the merged providers.
    let bad = setup.run_in_workspace(&["config", "set", "model", "acme/typo"]);
    assert_eq!(
        bad.code,
        Some(1),
        "stdout: {:?}, stderr: {:?}",
        bad.stdout,
        bad.stderr
    );
    assert!(bad.stderr.contains("acme/big"), "stderr: {}", bad.stderr);
}
