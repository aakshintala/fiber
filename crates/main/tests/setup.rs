//! Binary-level tests of `fiber setup` without a terminal
//! (`docs/testing.md`, "Levels"): the built `fiber` runs in its own
//! process group with its own `FIBER_HOME`, holding an ordinary provider
//! for the login step and stub managers for the hub step. No test reads or
//! sleeps on the real clock; every wait takes the test's one deadline.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

use fakes::ProviderServer;
use serde_json::{Value, json};
use support::{Output, Setup, group_alive};

/// The stub managers record to the test root, which `HOME` already names.
fn stub_dir(setup: &Setup) -> PathBuf {
    setup.root.path().join("bin")
}

/// Installs the `systemctl` and `launchctl` stubs and returns a `PATH`
/// with them first: `systemctl` always succeeds, `launchctl` fails
/// `print` (the service is not loaded) and succeeds otherwise, and each
/// records its argv.
fn stub_managers(setup: &Setup) -> String {
    let dir = stub_dir(setup);
    fs::create_dir_all(&dir).unwrap();
    write_stub(
        &dir.join("systemctl"),
        "#!/bin/sh\nprintf '%s\\n' \"systemctl $*\" >>\"$HOME/calls-systemctl\"\nexit 0\n",
    );
    write_stub(
        &dir.join("launchctl"),
        "#!/bin/sh\nprintf '%s\\n' \"launchctl $*\" >>\"$HOME/calls-launchctl\"\nif [ \"$1\" = \"print\" ]; then exit 1; fi\nexit 0\n",
    );
    let path = std::env::var_os("PATH").unwrap_or_default();
    format!("{}:{}", dir.display(), Path::new(&path).display())
}

fn write_stub(file: &Path, text: &str) {
    fs::write(file, text).unwrap();
    let mut permissions = fs::metadata(file).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(file, permissions).unwrap();
}

impl Setup {
    /// Runs `fiber` with `args` and `stdin` piped in (closed when `None`),
    /// `HOME` at the test root and `FIBER_HOME` at the test home, with
    /// `env` added last so a test can extend `PATH` with its stubs.
    #[track_caller]
    fn run_setup(&self, args: &[&str], stdin: Option<&str>, env: &[(&str, &str)]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(args)
            .current_dir(self.root.path().join("w"))
            .env_clear()
            .envs(fakes::check_run())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path())
            .env("FIBER_HOME", self.home())
            .env("FIBER_TEST_FAKE_KEY", "sk-test")
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (mut child, watchdog) = support::spawn_watched(&mut command);
        let group = child.id();
        let guard = support::KillGroup(group);
        if let Some(mut pipe) = child.stdin.take()
            && let Some(text) = stdin
        {
            let text = text.to_owned();
            thread::spawn(move || match pipe.write_all(text.as_bytes()) {
                Ok(()) | Err(_) => {}
            });
        }
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
            "`fiber` left a process in its group behind"
        );
        std::mem::forget(guard);
        watchdog.stand_down(self.deadline.cleanup());
        Output::from(output)
    }

    /// Every `fiber-hub-*` unit file the run wrote, on either platform.
    fn hub_units(&self) -> Vec<PathBuf> {
        let mut units = Vec::new();
        for dir in [
            self.root.path().join(".config/systemd/user"),
            self.root.path().join("Library/LaunchAgents"),
        ] {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                if entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("fiber-hub-")
                {
                    units.push(entry.path());
                }
            }
        }
        units.sort();
        units
    }
}

fn read_json(file: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(file).unwrap()).unwrap()
}

#[test]
fn one_flag_per_step_applies_every_answer() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    // A malformed receipt is offered again: the import runs and replaces
    // it with one that parses with the matching source.
    fs::write(setup.home().join("import-claude-code.json"), "not json").unwrap();
    // An unrelated global key survives the model write.
    fs::write(
        setup.home().join("config.json"),
        json!({"unrelated": "kept"}).to_string(),
    )
    .unwrap();
    // The source directory exists, as the import offer requires.
    fs::create_dir_all(setup.root.path().join(".claude")).unwrap();
    let path = stub_managers(&setup);

    let run = setup.run_setup(
        &[
            "setup",
            "--provider",
            "fake",
            "--model",
            "fake/m",
            "--thinking",
            "high",
            "--import",
            "claude-code",
            "--completions",
            "zsh",
            "--hub",
        ],
        Some("sk-test-login\n"),
        &[("PATH", &path)],
    );
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(!run.stderr.contains("skipped"), "{}", run.stderr);
    // The key piped on stdin is stored; the value itself is never printed.
    assert!(
        setup.home().join("credentials/fake/default").is_file(),
        "stderr: {}",
        run.stderr
    );
    assert!(!run.stdout.contains("sk-test-login"), "{}", run.stdout);
    assert!(!run.stderr.contains("sk-test-login"), "{}", run.stderr);
    // The receipt parses with the matching source.
    let receipt = read_json(&setup.home().join("import-claude-code.json"));
    assert_eq!(receipt["source"], "claude-code", "{receipt}");
    // The model and thinking land on the same keys the picker saves, and
    // the unrelated key survives.
    let config = read_json(&setup.home().join("config.json"));
    assert_eq!(config["model"], "fake/m", "{config}");
    assert_eq!(config["models"]["fake/m"]["thinking"], "high", "{config}");
    assert_eq!(config["unrelated"], "kept", "{config}");
    // The startup file gains exactly one completion line.
    let startup = setup.root.path().join(".zshrc");
    let text = fs::read_to_string(&startup).unwrap();
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| line.contains("fiber completion zsh"))
        .collect();
    assert_eq!(lines.len(), 1, "{text}");
    // The hub's login-service file lands exactly once.
    assert_eq!(setup.hub_units().len(), 1, "stderr: {}", run.stderr);

    // Answering completions again leaves the startup file byte-identical.
    let again = setup.run_setup(&["setup", "--completions", "zsh"], None, &[]);
    assert_eq!(again.code, Some(0), "stderr: {}", again.stderr);
    assert_eq!(fs::read_to_string(&startup).unwrap(), text);
}

#[test]
fn without_flags_nothing_is_asked_and_each_skip_names_its_command() {
    let setup = Setup::new();
    let run = setup.run_setup(&["setup"], None, &[]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.stdout, "", "stdout: {}", run.stdout);
    let lines: Vec<&str> = run.stderr.lines().collect();
    assert_eq!(
        lines,
        [
            "skipped providers: run fiber login",
            "skipped import: run fiber import",
            "skipped model: run fiber setup --model <model>",
            "skipped completions: run fiber setup --completions <shell>",
            "skipped hub: run fiber setup --hub",
        ],
        "{}",
        run.stderr
    );
    // Nothing was asked, so nothing was written.
    assert_eq!(fs::read_dir(setup.home()).unwrap().count(), 0);
    assert!(!setup.root.path().join(".zshrc").exists());
    assert!(setup.hub_units().is_empty());
}

#[test]
fn a_model_flag_with_nobody_signed_in_still_writes_the_model() {
    let setup = Setup::new();
    let run = setup.run_setup(&["setup", "--model", "fake/m"], None, &[]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let config = read_json(&setup.home().join("config.json"));
    assert_eq!(config["model"], "fake/m", "{config}");
    // The model step ran, so only the other four print their skip lines.
    let lines: Vec<&str> = run.stderr.lines().collect();
    assert_eq!(
        lines,
        [
            "skipped providers: run fiber login",
            "skipped import: run fiber import",
            "skipped completions: run fiber setup --completions <shell>",
            "skipped hub: run fiber setup --hub",
        ],
        "{}",
        run.stderr
    );
}

#[test]
fn a_startup_file_without_a_trailing_newline_keeps_its_last_line() {
    let setup = Setup::new();
    fs::write(setup.root.path().join(".zshrc"), "export KEEP=1").unwrap();
    let run = setup.run_setup(&["setup", "--completions", "zsh"], None, &[]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(
        fs::read_to_string(setup.root.path().join(".zshrc")).unwrap(),
        "export KEEP=1\neval \"$(fiber completion zsh)\"\n"
    );
}

#[test]
fn completions_for_bash_and_fish_land_in_their_startup_files() {
    let setup = Setup::new();
    for (shell, file) in [("bash", ".bashrc"), ("fish", ".config/fish/config.fish")] {
        let run = setup.run_setup(&["setup", "--completions", shell], None, &[]);
        assert_eq!(run.code, Some(0), "{shell}: {}", run.stderr);
        let text = fs::read_to_string(setup.root.path().join(file)).unwrap();
        let expected = format!("eval \"$(fiber completion {shell})\"\n");
        assert_eq!(text, expected, "{shell}");
    }
}

#[test]
fn an_unknown_provider_fails_naming_the_step_and_runs_nothing_later() {
    let setup = Setup::new();
    let run = setup.run_setup(
        &["setup", "--provider", "nope", "--model", "fake/m"],
        None,
        &[],
    );
    assert_ne!(run.code, Some(0), "stdout: {}", run.stdout);
    assert!(
        run.stderr.contains("setup providers failed"),
        "{}",
        run.stderr
    );
    // No later step ran: the model was never written.
    assert!(!setup.home().join("config.json").exists());
}

#[test]
fn thinking_without_a_model_fails_naming_the_step() {
    let setup = Setup::new();
    let run = setup.run_setup(&["setup", "--thinking", "high"], None, &[]);
    assert_eq!(run.code, Some(1), "stderr: {}", run.stderr);
    assert!(run.stderr.contains("setup model failed"), "{}", run.stderr);
    // The model step failed, so the later steps never ran.
    assert!(!run.stderr.contains("skipped hub"), "{}", run.stderr);
}
