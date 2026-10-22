//! Binary-level tests of `fiber setup` (`docs/testing.md`, "Levels"): the
//! built `fiber` runs in its own process group with its own `FIBER_HOME`,
//! holding an ordinary provider for the login step and stub managers for
//! the hub step. Without a terminal only the flags apply; the question
//! flow runs with stdin on a pseudo-terminal. No test reads or sleeps on
//! the real clock; every wait takes the test's one deadline.

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
        watchdog.stand_down(self.deadline.cleanup_phase());
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
fn no_flags_leave_their_steps_out_without_asking() {
    let setup = Setup::new();
    let run = setup.run_setup(&["setup", "--no-completions", "--no-hub"], None, &[]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let lines: Vec<&str> = run.stderr.lines().collect();
    assert_eq!(
        lines,
        [
            "skipped providers: run fiber login",
            "skipped import: run fiber import",
            "skipped model: run fiber setup --model <model>",
        ],
        "{}",
        run.stderr
    );
    // The refused steps wrote nothing.
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
fn an_unreadable_startup_file_fails_before_any_write() {
    let setup = Setup::new();
    // A directory fails to read with an error other than `NotFound`: the
    // step must report the read instead of wiping the file's contents.
    fs::create_dir(setup.root.path().join(".bashrc")).unwrap();
    let run = setup.run_setup(&["setup", "--completions", "bash"], None, &[]);
    assert_eq!(run.code, Some(1), "stderr: {}", run.stderr);
    assert!(
        run.stderr.contains("setup completions failed: reading"),
        "{}",
        run.stderr
    );
    // Nothing was written: the existing contents survive.
    assert!(setup.root.path().join(".bashrc").is_dir());
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

impl Setup {
    /// Runs `fiber` with `args` and stdin on a new pseudo-terminal fed
    /// `typed`, stdout and stderr on pipes: stdin is a terminal, so the
    /// questions are asked. The test types whole answers up front; every
    /// setup question reads whole lines in order, so nothing is consumed
    /// early.
    #[track_caller]
    fn run_on_terminal(&self, args: &[&str], typed: &str, env: &[(&str, &str)]) -> Output {
        self.run_on_terminal_for(
            args,
            typed,
            env,
            &format!("`fiber {}` to exit", args.join(" ")),
        )
    }

    /// [`Setup::run_on_terminal`], naming in `what` the event the run's
    /// exit stands for, so a deadline expiry says what never happened.
    #[track_caller]
    fn run_on_terminal_for(
        &self,
        args: &[&str],
        typed: &str,
        env: &[(&str, &str)],
        what: &str,
    ) -> Output {
        let terminal = support::pty::open(120, 32);
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
            .stdin(terminal.stdio())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (child, watchdog) = support::spawn_watched(&mut command);
        let group = child.id();
        let guard = support::KillGroup(group);
        let mut main = fs::File::from(terminal.main);
        main.write_all(typed.as_bytes()).unwrap();
        // The terminal echoes what was typed, and the login's echo change
        // waits for that output to drain: the master is read until the
        // run ends.
        let writer = std::sync::Arc::new(std::sync::Mutex::new(main.try_clone().unwrap()));
        let reader = support::pty::Reader::start_on(
            main,
            std::sync::Arc::new(std::sync::Mutex::new(support::pty::Shared::new(120, 32))),
            writer,
            self.deadline,
        );
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait_with_output()).unwrap());
        let output = match self.deadline.recv(&finished) {
            Ok(output) => output.unwrap(),
            Err(_) => support::expired(self.deadline, group, &finished, what),
        };
        assert!(reader.stop(), "the terminal's reader stopped");
        assert!(
            !group_alive(self.deadline, group),
            "`fiber` left a process in its group behind"
        );
        std::mem::forget(guard);
        watchdog.stand_down(self.deadline.cleanup_phase());
        Output::from(output)
    }

    /// Installs `fake` with a thinking model served from `server`.
    fn install_thinking_provider(&self, server: &ProviderServer) {
        let source = self.root.path().join("src");
        support::write_json(
            &source.join("extension.json"),
            &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "providers": {"fake": [format!("{}/v1", server.url())]}}),
        );
        support::write_json(
            &source.join("providers/fake.json"),
            &json!({
                "name": "fake",
                "credential": {"env": "FIBER_TEST_FAKE_KEY"},
                "models": [{"id": "m", "protocol": "openai-responses", "base_url": format!("{}/v1", server.url()), "context_window": 100000, "thinking_levels": ["low", "high"], "thinking_default": "low"}]
            }),
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
    }

    /// Installs an unsigned second provider with its own model.
    fn install_other(&self, server: &ProviderServer) {
        let source = self.root.path().join("other");
        support::write_json(
            &source.join("extension.json"),
            &json!({"name": "other", "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "providers": {"other": [format!("{}/v1", server.url())]}}),
        );
        support::write_json(
            &source.join("providers/other.json"),
            &json!({
                "name": "other",
                "credential": {"env": "FIBER_TEST_OTHER_KEY"},
                "models": [{"id": "o1", "protocol": "openai-responses", "base_url": format!("{}/v1", server.url()), "context_window": 100000}]
            }),
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
    }
}

/// The position of `needle` in `text`, for asserting prompt order.
fn position(text: &str, needle: &str) -> usize {
    text.find(needle)
        .unwrap_or_else(|| panic!("{needle:?} not in {text:?}"))
}

#[test]
fn the_terminal_run_asks_each_question_with_the_current_answers_preselected() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.install_thinking_provider(&server);
    setup.install_other(&server);
    // The current answers: fake is signed in, the global model is set,
    // and the Claude directory waits for its import.
    config::store_credential(
        &setup.home(),
        "fake",
        "default",
        &config::Secret::new("sk-seed".into()),
    )
    .unwrap();
    fs::write(
        setup.home().join("config.json"),
        json!({"model": "fake/m", "unrelated": "kept"}).to_string(),
    )
    .unwrap();
    fs::create_dir_all(setup.root.path().join(".claude")).unwrap();

    // Skip the providers, import Claude's configuration, keep the model
    // and thinking, install the completions, skip the hub.
    let run = setup.run_on_terminal(&["setup"], "\ny\n\n\ny\nn\n", &[("SHELL", "/bin/zsh")]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let order = [
        "Providers:",
        "1) fake (signed in)",
        "2) other",
        "Sign in to providers",
        "Import Claude Code's configuration (~/.claude)? [y/N]:",
        "Default model (current: fake/m):",
        "1) fake/m",
        "Pick the default model",
        "Thinking for fake/m (current: unset): low, high",
        "Install completions for zsh (~/.zshrc)? [y/N]:",
        "Register the hub as a login service? [y/N]:",
    ];
    let mut mark = 0;
    for needle in order {
        let at = position(&run.stderr[mark..], needle);
        mark += at + needle.len();
    }
    // The unsigned provider's model is never offered.
    assert!(!run.stderr.contains("other/o1"), "{}", run.stderr);
    assert!(!run.stderr.contains("other (signed in)"), "{}", run.stderr);
    // The providers question was skipped, so the seeded credential is
    // untouched and no login ran.
    assert_eq!(
        fs::read_to_string(setup.home().join("credentials/fake/default")).unwrap(),
        "sk-seed"
    );
    // The import ran and wrote its receipt.
    let receipt = read_json(&setup.home().join("import-claude-code.json"));
    assert_eq!(receipt["source"], "claude-code", "{receipt}");
    // The completions answer installed the one startup line.
    let startup = setup.root.path().join(".zshrc");
    assert_eq!(
        fs::read_to_string(&startup).unwrap(),
        "eval \"$(fiber completion zsh)\"\n",
    );
    // Kept answers wrote nothing: the model and the unrelated key are
    // unchanged, and no thinking was set.
    let config = read_json(&setup.home().join("config.json"));
    assert_eq!(
        config,
        json!({"model": "fake/m", "unrelated": "kept"}),
        "{config}"
    );
    // The hub answer left no unit behind.
    assert!(setup.hub_units().is_empty());
}

#[test]
fn a_rerun_shows_done_steps_and_leaves_startup_files_alone() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.install_thinking_provider(&server);
    setup.install_other(&server);
    config::store_credential(
        &setup.home(),
        "fake",
        "default",
        &config::Secret::new("sk-seed".into()),
    )
    .unwrap();
    fs::write(
        setup.home().join("config.json"),
        json!({"model": "fake/m", "models": {"fake/m": {"thinking": "low"}}}).to_string(),
    )
    .unwrap();
    fs::write(
        setup.home().join("import-claude-code.json"),
        json!({"source": "claude-code"}).to_string(),
    )
    .unwrap();
    let startup = setup.root.path().join(".zshrc");
    fs::write(&startup, "eval \"$(fiber completion zsh)\"\n").unwrap();
    let before = fs::read(&startup).unwrap();
    // The hub is already registered, so the rerun names it instead of
    // asking.
    let installed = setup.run_setup(
        &["hub", "install"],
        None,
        &[("PATH", &stub_managers(&setup))],
    );
    assert_eq!(installed.code, Some(0), "stderr: {}", installed.stderr);
    assert_eq!(setup.hub_units().len(), 1);

    // Skip the providers, keep the model and thinking, keep the done
    // completions: the import and the hub are shown as done, never asked.
    let run = setup.run_on_terminal(&["setup"], "\n\n\nn\n", &[("SHELL", "/bin/zsh")]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(
        run.stderr.contains("Claude Code: already imported (done)"),
        "{}",
        run.stderr
    );
    assert!(
        run.stderr.contains("Default model (current: fake/m):"),
        "{}",
        run.stderr
    );
    assert!(
        run.stderr
            .contains("Thinking for fake/m (current: low): low, high"),
        "{}",
        run.stderr
    );
    assert!(
        run.stderr
            .contains("Completions for zsh are installed (done)."),
        "{}",
        run.stderr
    );
    assert!(run.stderr.contains("login service"), "{}", run.stderr);
    assert!(run.stderr.contains("is installed"), "{}", run.stderr);
    assert!(!run.stderr.contains("Register the hub"), "{}", run.stderr);
    // Nothing was rewritten: the startup file is byte-identical with its
    // one line, and the config keeps its thinking.
    assert_eq!(fs::read(&startup).unwrap(), before);
    assert_eq!(fs::read_to_string(&startup).unwrap().lines().count(), 1);
    let config = read_json(&setup.home().join("config.json"));
    assert_eq!(config["models"]["fake/m"]["thinking"], "low", "{config}");
}

#[test]
fn without_providers_the_first_and_third_steps_skip_and_nothing_is_asked() {
    let setup = Setup::new();
    let run = setup.run_on_terminal(&["setup", "--no-hub"], "", &[]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(
        run.stderr.contains("skipped providers: run fiber login"),
        "{}",
        run.stderr
    );
    assert!(
        run.stderr.contains("skipped import: run fiber import"),
        "{}",
        run.stderr
    );
    assert!(
        run.stderr
            .contains("skipped model: run fiber setup --model <model>"),
        "{}",
        run.stderr
    );
    assert!(
        run.stderr
            .contains("skipped completions: run fiber setup --completions <shell>"),
        "{}",
        run.stderr
    );
    assert!(!run.stderr.contains("[y/N]"), "{}", run.stderr);
    assert!(!run.stderr.contains("by number or name"), "{}", run.stderr);
}

/// The run's exit under the deadline stands for the login after the pick
/// returning: a second lock of standard input never returns.
const AFTER_THE_PICK: &str = "`fiber setup` login after the provider pick";

#[test]
fn picking_a_fresh_provider_in_the_first_question_logs_in_on_setups_reader() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);

    // Pick `fake`, type its key, keep the model.
    let run = setup.run_on_terminal_for(
        &["setup", "--no-hub", "--no-completions"],
        "1\nsk-typed\n\n",
        &[],
        AFTER_THE_PICK,
    );
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(
        run.stderr
            .contains("fiber: stored credentials/fake/default"),
        "{}",
        run.stderr
    );
    assert_eq!(
        fs::read_to_string(setup.home().join("credentials/fake/default")).unwrap(),
        "sk-typed"
    );
    assert!(!run.stdout.contains("sk-typed"), "{}", run.stdout);
    assert!(!run.stderr.contains("sk-typed"), "{}", run.stderr);
    // The run went on past the login to the model question.
    let stored = position(&run.stderr, "fiber: stored credentials/fake/default");
    let asked = position(&run.stderr, "Default model (current: fake/m):");
    assert!(stored < asked, "{}", run.stderr);
}

#[test]
fn picking_a_provider_with_a_stored_credential_ends_with_the_logins_refusal() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    config::store_credential(
        &setup.home(),
        "fake",
        "default",
        &config::Secret::new("sk-seed".into()),
    )
    .unwrap();

    let run = setup.run_on_terminal_for(
        &["setup", "--no-hub", "--no-completions"],
        "1\n",
        &[],
        AFTER_THE_PICK,
    );
    assert_ne!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(
        run.stderr
            .contains("credentials/fake/default is already stored"),
        "{}",
        run.stderr
    );
    assert!(
        run.stderr.contains("fiber: setup providers failed"),
        "{}",
        run.stderr
    );
    assert_eq!(
        fs::read_to_string(setup.home().join("credentials/fake/default")).unwrap(),
        "sk-seed"
    );
    // Setup ends with the code `fiber login fake` gives for the same home.
    let login = setup.run_setup(&["login", "fake"], Some(""), &[]);
    assert_ne!(login.code, Some(0), "stderr: {}", login.stderr);
    assert_eq!(run.code, login.code, "{}", login.stderr);
}

#[test]
fn the_provider_flag_on_a_terminal_reads_the_key_from_setups_reader() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);

    let run = setup.run_on_terminal_for(
        &[
            "setup",
            "--provider",
            "fake",
            "--no-hub",
            "--no-completions",
        ],
        "sk-flag\n\n",
        &[],
        "`fiber setup --provider fake` login on a terminal",
    );
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(
        fs::read_to_string(setup.home().join("credentials/fake/default")).unwrap(),
        "sk-flag"
    );
}

#[test]
fn setup_prints_no_model_unconfigured_notice_and_keeps_those_models_out() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    config::store_credential(
        &setup.home(),
        "fake",
        "default",
        &config::Secret::new("sk-seed".into()),
    )
    .unwrap();
    // Vertex's models need the `project` setting, which nothing sets: the
    // run's environment is cleared of `GOOGLE_CLOUD_PROJECT`.
    let to = setup.root.path().join("pkg-vertex");
    support::package::copy_package(
        "vertex",
        &to,
        "https://aiplatform.googleapis.com",
        "http://127.0.0.1:9",
    );
    let installed = setup.run_setup(&["extension", "install", to.to_str().unwrap()], None, &[]);
    assert_eq!(installed.code, Some(0), "stderr: {}", installed.stderr);
    config::store_credential(
        &setup.home(),
        "vertex",
        "default",
        &config::Secret::new("sk-vertex".into()),
    )
    .unwrap();

    // Skip the providers, keep the model.
    let run = setup.run_on_terminal(&["setup", "--no-hub", "--no-completions"], "\n\n", &[]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(run.stderr.starts_with("Providers:"), "{}", run.stderr);
    assert!(!run.stderr.contains("needs the setting"), "{}", run.stderr);
    assert!(!run.stderr.contains("vertex/"), "{}", run.stderr);
    assert!(run.stderr.contains("1) fake/m"), "{}", run.stderr);

    // `fiber models` still prints the notice.
    let models = setup.run_setup(&["models"], None, &[]);
    assert!(
        models.stderr.contains("needs the setting `project`"),
        "{}",
        models.stderr
    );
}
