//! Binary-level tests of `fiber login` and `fiber logout`
//! (`docs/testing.md`, "Levels"; `docs/configuration.md`, "Secrets"): the
//! built `fiber` runs in its own process group with its own `FIBER_HOME` and
//! an installed provider, and nothing here touches the network. Every run
//! carries a wall-clock deadline, and a pseudo-terminal run proves the key is
//! not echoed and the terminal is restored.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::ffi::OsStr;
use std::fs;
use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use support::repo_provider::{declare_repo_provider, git_init};
use support::write_record;

use fakes::{MUST_SUCCEED_WITHIN, ProviderServer, Response, Watchdog};
use rustix::pty;
use rustix::termios::{self, LocalModes};
use serde_json::{Value, json};
use support::{Deadline, KillGroup, Output, Setup, group_alive, spawn_watched};

const KEY: &str = "sk-live-7f3a9c0d1e2b";

impl Setup {
    #[track_caller]
    fn new_with_fl_root() -> Self {
        let deadline = Deadline::start();
        let root = fakes::TempDir::new("fl");
        fs::create_dir_all(root.path().join("h")).unwrap();
        fs::create_dir_all(root.path().join("w")).unwrap();
        Self { deadline, root }
    }

    /// Installs the provider `name`, reading the stored credential
    /// `credential_name` when it is set, and declaring `source` for its key.
    fn install_provider(&self, name: &str, credential_name: Option<&str>, source: Option<Value>) {
        let dir = self.home().join("extensions").join(name);
        let mut data = json!({
            "name": name,
            "models": [{"id": "m", "protocol": "openai-responses", "base_url": "http://127.0.0.1:9/v1", "context_window": 1000}],
        });
        if let Some(stored) = credential_name {
            data["credential_name"] = json!(stored);
        }
        if let Some(source) = source {
            data["credential"] = source;
        }
        fs::create_dir_all(dir.join("providers")).unwrap();
        // The manifest declares the data's URL.
        let mut manifest = json!({"name": name, "version": "v0.0.0", "fiber": "0.0.0", "api": 1});
        if let Some(into) = manifest.as_object_mut() {
            let mut declared = serde_json::Map::new();
            declared.insert(name.to_owned(), json!(["http://127.0.0.1:9/v1"]));
            into.insert("providers".to_owned(), Value::Object(declared));
        }
        fs::write(dir.join("extension.json"), manifest.to_string()).unwrap();
        write_record(&dir);
        fs::write(
            dir.join("providers").join(format!("{name}.json")),
            data.to_string(),
        )
        .unwrap();
    }

    /// Installs the extension `extension` with no provider, declaring
    /// `secrets`.
    fn declare(&self, extension: &str, secrets: &[&str]) {
        let dir = self.home().join("extensions").join(extension);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("extension.json"),
            json!({"name": extension, "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "secrets": secrets})
                .to_string(),
        )
        .unwrap();
        write_record(&dir);
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(args)
            .current_dir(self.root.path().join("w"))
            .env_clear()
            .envs(fakes::check_run())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path())
            .env("FIBER_HOME", self.home());
        command
    }

    /// Runs `fiber` with `args` and `input` on a closed pipe for stdin.
    #[track_caller]
    fn run_with_input(&self, args: &[&str], input: &str) -> Output {
        let mut command = self.command(args);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        self.run_piped(&mut command, args, input)
    }

    /// Runs `fiber` with `args`, stdin on a new pseudo-terminal fed
    /// `input`, and stdout/stderr on pipes: stdin is a terminal while
    /// standard error is not, so there is nobody to ask.
    #[track_caller]
    fn run_with_terminal_stdin(&self, args: &[&str], input: &str) -> Output {
        let terminal = Terminal::open();
        let mut command = self.command(args);
        command
            .stdin(terminal.stdio())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (child, watchdog) = spawn_watched(&mut command);
        let group = child.id();
        let guard = KillGroup(group);
        // The key read turns echo off first: typing waits for that, so no
        // byte precedes the read the way pasted input must not.
        terminal.wait_for_echo_off(self.deadline);
        // Typed once echo is off, as `feed` sends a pipe's bytes: the
        // terminal holds the line until the key prompt reads it.
        let mut typed = fs::File::from(terminal.main.try_clone().unwrap());
        let input = input.to_owned();
        let deadline = self.deadline;
        thread::spawn(move || {
            support::bounded(deadline, "typing on the terminal", move || {
                typed.write_all(input.as_bytes())
            })
            .unwrap();
        });
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
        assert!(terminal.echoes(), "echo was not restored");
        std::mem::forget(guard);
        watchdog.stand_down(self.deadline.cleanup_phase());
        Output::from(output)
    }

    /// Runs `command` with piped outputs and `input` fed on a thread,
    /// waiting for the exit under the deadline with nothing left behind.
    #[track_caller]
    fn run_piped(&self, command: &mut Command, args: &[&str], input: &str) -> Output {
        let (mut child, watchdog) = spawn_watched(command);
        let group = child.id();
        let guard = KillGroup(group);
        feed(&mut child, input);
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

    /// Starts `fiber` with stdin and stderr on a new pseudo-terminal.
    fn on_terminal(&self, args: &[&str]) -> OnTerminal {
        let terminal = Terminal::open();
        let mut command = self.command(args);
        command
            .stdin(terminal.stdio())
            .stdout(Stdio::piped())
            .stderr(terminal.stdio());
        let (child, watchdog) = spawn_watched(&mut command);
        let group = child.id();
        OnTerminal {
            screen: Screen::new(&terminal, self.deadline),
            terminal,
            child: Some(child),
            guard: Some(KillGroup(group)),
            watchdog: Some(watchdog),
            group,
            deadline: self.deadline,
        }
    }

    /// Starts `fiber` with stdin on a pipe holding `input` and stderr on a
    /// new pseudo-terminal.
    fn stderr_on_terminal(&self, args: &[&str], input: &str) -> OnTerminal {
        let terminal = Terminal::open();
        let mut command = self.command(args);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(terminal.stdio());
        let (mut child, watchdog) = spawn_watched(&mut command);
        let group = child.id();
        feed(&mut child, input);
        OnTerminal {
            screen: Screen::new(&terminal, self.deadline),
            terminal,
            child: Some(child),
            guard: Some(KillGroup(group)),
            watchdog: Some(watchdog),
            group,
            deadline: self.deadline,
        }
    }

    /// Every regular file under Fiber home whose text holds `needle`.
    fn files_holding(&self, needle: &str) -> Vec<PathBuf> {
        fn walk(dir: &Path, needle: &str, found: &mut Vec<PathBuf>) {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, needle, found);
                } else if fs::read(&path)
                    .unwrap()
                    .windows(needle.len())
                    .any(|w| w == needle.as_bytes())
                {
                    found.push(path);
                }
            }
        }
        let mut found = Vec::new();
        walk(&self.home(), needle, &mut found);
        found
    }
}

/// Writes `input` to the child's stdin on a thread and closes it, so a
/// child that never reads it is bounded by the run's exit wait. `fiber` may
/// refuse a run before it reads stdin and exit, so a closed pipe is not a
/// failure here: the run's own exit code and message are what the test
/// asserts.
fn feed(child: &mut Child, input: &str) {
    // Taking the pipe closes it once written.
    let mut pipe = child.stdin.take().unwrap();
    let input = input.to_owned();
    thread::spawn(move || match pipe.write_all(input.as_bytes()) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
        Err(e) => panic!("writing to the stdin of `fiber`: {e}"),
    });
}

/// A pseudo-terminal, opened through rustix's safe calls.
struct Terminal {
    main: OwnedFd,
    terminal: fs::File,
}

impl Terminal {
    #[track_caller]
    fn open() -> Self {
        let main = pty::openpt(pty::OpenptFlags::RDWR | pty::OpenptFlags::NOCTTY).unwrap();
        pty::grantpt(&main).unwrap();
        pty::unlockpt(&main).unwrap();
        let name = pty::ptsname(&main, Vec::new()).unwrap();
        let path = PathBuf::from(OsStr::from_bytes(name.as_bytes()));
        let terminal = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        Self { main, terminal }
    }

    fn stdio(&self) -> Stdio {
        Stdio::from(self.terminal.try_clone().unwrap())
    }

    /// Whether the terminal echoes what is typed.
    fn echoes(&self) -> bool {
        echoes(&self.terminal)
    }

    /// Waits until the terminal stops echoing, which is when `fiber` has
    /// begun reading the key. A thread polls the flag, so the wait has a
    /// deadline without this test reading a clock.
    #[track_caller]
    fn wait_for_echo_off(&self, deadline: Deadline) {
        let terminal = self.terminal.try_clone().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let (off, is_off) = mpsc::channel();
        thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                if !echoes(&terminal) {
                    off.send(()).unwrap_or(());
                    return;
                }
                thread::yield_now();
            }
        });
        if deadline.recv(&is_off).is_err() {
            stop.store(true, Ordering::Relaxed);
            panic!("waited until the deadline for `fiber` to turn echo off");
        }
    }
}

fn echoes(terminal: &fs::File) -> bool {
    termios::tcgetattr(terminal)
        .unwrap()
        .local_modes
        .contains(LocalModes::ECHO)
}

/// What `fiber` wrote to its terminal, read on a thread so a wait can have a
/// deadline.
struct Screen {
    chunks: Receiver<String>,
    typed: fs::File,
    seen: String,
    /// The end of the last match: the next wait searches from here, so
    /// text read past a match stays for the next wait.
    consumed: usize,
    deadline: Deadline,
}

impl Screen {
    #[track_caller]
    fn new(terminal: &Terminal, deadline: Deadline) -> Self {
        let reader = fs::File::from(terminal.main.try_clone().unwrap());
        let (send, chunks) = mpsc::channel();
        // A read error is the end of the terminal, on Linux as well as
        // on macOS. The channel's disconnect when the reader ends is the
        // end of the terminal.
        fakes::pty::read_to_eof(reader, move |bytes| {
            send.send(String::from_utf8_lossy(bytes).into())
                .unwrap_or(());
        });
        Self {
            chunks,
            typed: fs::File::from(terminal.main.try_clone().unwrap()),
            seen: String::new(),
            consumed: 0,
            deadline,
        }
    }

    /// Reads until `text` has appeared after the last match, and returns
    /// everything read since the last wait. Bytes read past this match stay
    /// for the next wait.
    #[track_caller]
    fn wait_for(&mut self, text: &str) -> String {
        let mark = self.consumed;
        let end = loop {
            if let Some(at) = self.seen[mark..].find(text) {
                break mark + at + text.len();
            }
            // Each chunk has the deadline: a terminal that goes quiet fails.
            match self.deadline.recv(&self.chunks) {
                Ok(chunk) => self.seen.push_str(&chunk),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    panic!(
                        "the terminal ended before {text:?}: {:?}",
                        &self.seen[mark..]
                    )
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    panic!(
                        "waited until the deadline for {text:?} on the terminal: {:?}",
                        &self.seen[mark..]
                    )
                }
            }
        };
        self.consumed = end;
        self.seen[mark..].to_owned()
    }

    /// Types `text` on a thread bounded by the test's [`Deadline`].
    #[track_caller]
    fn type_text(&mut self, text: &str) {
        let mut typed = self.typed.try_clone().unwrap();
        let text = text.to_owned();
        support::bounded(self.deadline, "typing on the terminal", move || {
            typed.write_all(text.as_bytes())
        })
        .unwrap();
    }
}

/// A `fiber` run on a terminal.
struct OnTerminal {
    screen: Screen,
    terminal: Terminal,
    child: Option<Child>,
    guard: Option<KillGroup>,
    watchdog: Option<Watchdog>,
    group: u32,
    deadline: Deadline,
}

impl OnTerminal {
    /// Waits for `fiber` to exit under the test's [`Deadline`], and asserts
    /// that nothing it started is left in its group, after a timeout too.
    #[track_caller]
    fn finish(&mut self) -> ExitStatus {
        let mut child = self.child.take().unwrap();
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait()).unwrap());
        let status = match self.deadline.recv(&finished) {
            Ok(status) => status.unwrap(),
            Err(_) => support::expired(self.deadline, self.group, &finished, "`fiber` to exit"),
        };
        assert!(
            !group_alive(self.deadline, self.group),
            "`fiber` left a process behind"
        );
        std::mem::forget(self.guard.take());
        self.watchdog
            .take()
            .unwrap()
            .stand_down(self.deadline.cleanup_phase());
        status
    }
}

#[test]
fn a_key_on_a_pipe_is_stored_then_deleted_and_never_printed() {
    let setup = Setup::new_with_fl_root();
    setup.install_provider("acme", None, None);
    let login = setup.run_with_input(&["login", "acme"], &format!("  {KEY}  \n"));
    assert_eq!(login.code, Some(0), "{}", login.stderr);
    assert_eq!(login.stdout, "");
    assert_eq!(login.stderr, "fiber: stored credentials/acme/default\n");
    let file = setup.home().join("credentials/acme/default");
    assert_eq!(fs::read_to_string(&file).unwrap(), KEY);
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(file.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        setup.files_holding(KEY),
        std::slice::from_ref(&file),
        "the key is in the credential file alone"
    );
    let config: Value =
        serde_json::from_str(&fs::read_to_string(setup.home().join("config.json")).unwrap())
            .unwrap();
    assert_eq!(
        config,
        json!({"providers": {"acme": {"credential": "default"}}})
    );

    let again = setup.run_with_input(&["login", "acme"], "other\n");
    assert_eq!(again.code, Some(2));
    assert!(
        again
            .stderr
            .contains("with --as <label>, or run `fiber logout acme --as default` first"),
        "{}",
        again.stderr
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), KEY);

    let logout = setup.run_with_input(&["logout", "acme"], "");
    assert_eq!(logout.code, Some(0), "{}", logout.stderr);
    assert_eq!(logout.stdout, "");
    assert_eq!(logout.stderr, "fiber: removed credentials/acme/default\n");
    assert!(!file.exists());
    let missing = setup.run_with_input(&["logout", "acme"], "");
    assert_eq!(missing.code, Some(1));
    assert_eq!(missing.stderr, "fiber: no stored credential for acme\n");
    assert!(!format!("{}{}", login.stderr, missing.stderr).contains(KEY));
}

#[test]
fn providers_sharing_a_credential_log_in_and_out_through_either() {
    let setup = Setup::new_with_fl_root();
    setup.install_provider("opencode-go", Some("opencode"), None);
    setup.install_provider("opencode-zen", Some("opencode"), None);
    let login = setup.run_with_input(&["login", "opencode-go"], &format!("{KEY}\n"));
    assert_eq!(login.code, Some(0), "{}", login.stderr);
    assert_eq!(login.stderr, "fiber: stored credentials/opencode/default\n");
    let logout = setup.run_with_input(&["logout", "opencode-zen"], "");
    assert_eq!(logout.code, Some(0), "{}", logout.stderr);
    assert_eq!(
        logout.stderr,
        "fiber: removed credentials/opencode/default, which opencode-go also reads\n"
    );
    assert!(!setup.home().join("credentials/opencode/default").exists());
}

#[test]
fn logout_prints_a_configuration_notice_and_keeps_its_status() {
    let setup = Setup::new_with_fl_root();
    setup.install_provider("acme", None, None);
    let login = setup.run_with_input(&["login", "acme"], &format!("{KEY}\n"));
    assert_eq!(login.code, Some(0), "{}", login.stderr);
    let file = setup.home().join("config.json");
    fs::write(&file, r#"{"no_such_key": 1}"#).unwrap();
    let logout = setup.run_with_input(&["logout", "acme"], "");
    assert_eq!(logout.code, Some(0), "{}", logout.stderr);
    let lines: Vec<&str> = logout.stderr.lines().collect();
    assert_eq!(lines.len(), 2, "{}", logout.stderr);
    assert!(lines[0].contains("no_such_key"), "{}", logout.stderr);
    assert!(lines[0].contains("config.json"), "{}", logout.stderr);
    assert_eq!(lines[1], "fiber: removed credentials/acme/default");
}

#[test]
fn a_key_from_the_environment_is_named_and_the_exit_is_non_zero() {
    let setup = Setup::new_with_fl_root();
    setup.install_provider("acme", None, Some(json!({"env": "ACME_API_KEY"})));
    let run = setup.run_with_input(&["logout", "acme"], "");
    assert_eq!(run.code, Some(1));
    assert_eq!(
        run.stderr,
        "fiber: acme's key comes from the environment variable ACME_API_KEY; fiber logout cannot remove it\n"
    );
    assert_eq!(run.stdout, "");
}

#[test]
fn no_provider_is_a_usage_error_without_a_terminal() {
    let setup = Setup::new_with_fl_root();
    setup.install_provider("acme", None, None);
    let login = setup.run_with_input(&["login"], "acme\n");
    assert_eq!(login.code, Some(2));
    assert!(login.stderr.contains("no terminal"), "{}", login.stderr);
    assert_eq!(login.stdout, "");
    assert!(!setup.home().join("credentials").exists());
    let logout = setup.run_with_input(&["logout"], "");
    assert_eq!(logout.code, Some(2));
    assert_eq!(
        logout.stderr,
        "fiber: `fiber logout` takes the provider to log out of. Run `fiber --help` for usage.\n"
    );
    let unknown = setup.run_with_input(&["login", "nope"], "k\n");
    assert_eq!(unknown.code, Some(2));
    assert!(unknown.stderr.contains("the installed providers are acme"));
}

#[test]
fn a_declared_secret_on_a_pipe_is_stored_then_replaced_and_never_printed() {
    let setup = Setup::new_with_fl_root();
    setup.declare("acme", &["acme.api_key"]);
    let login = setup.run_with_input(&["login", "acme.api_key"], "  v1-7f3a9c0d  \n");
    assert_eq!(login.code, Some(0), "{}", login.stderr);
    assert_eq!(login.stdout, "");
    assert_eq!(login.stderr, "fiber: stored credentials/acme.api_key\n");
    let file = setup.home().join("credentials/acme.api_key");
    assert_eq!(fs::read_to_string(&file).unwrap(), "v1-7f3a9c0d");
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        setup.files_holding("v1-7f3a9c0d"),
        std::slice::from_ref(&file),
        "the value is in the secret's file alone"
    );
    assert!(!setup.home().join("config.json").exists());
    let again = setup.run_with_input(&["login", "acme.api_key"], "v2-1e2b\n");
    assert_eq!(again.code, Some(0), "{}", again.stderr);
    assert_eq!(again.stderr, "fiber: replaced credentials/acme.api_key\n");
    assert_eq!(fs::read_to_string(&file).unwrap(), "v2-1e2b");
}

#[test]
fn a_mistyped_secret_or_one_with_as_is_a_usage_error_that_writes_nothing() {
    let setup = Setup::new_with_fl_root();
    setup.install_provider("acme", None, None);
    setup.declare("acme-secrets", &["acme.api_key"]);
    let typo = setup.run_with_input(&["login", "acme.api_kye"], "v1\n");
    assert_eq!(typo.code, Some(2));
    assert!(
        typo.stderr.contains("the installed providers are acme"),
        "{}",
        typo.stderr
    );
    assert!(
        typo.stderr
            .contains("the declared secrets are acme.api_key"),
        "{}",
        typo.stderr
    );
    assert!(!setup.home().join("credentials").exists());
    let labelled = setup.run_with_input(&["login", "acme.api_key", "--as", "work"], "v1\n");
    assert_eq!(labelled.code, Some(2));
    assert_eq!(
        labelled.stderr,
        "fiber: --as applies only to a provider, and `acme.api_key` is a declared secret. Run `fiber --help` for usage.\n"
    );
    assert!(!setup.home().join("credentials").exists());
    assert!(!setup.home().join("config.json").exists());
}

#[test]
fn a_key_typed_on_a_terminal_is_not_echoed_and_echo_comes_back() {
    let setup = Setup::new_with_fl_root();
    setup.install_provider("acme", None, None);
    let mut run = setup.on_terminal(&["login", "acme"]);
    // Sent the moment the prompt shows, with no wait for echo to go off: echo
    // is already off when the prompt is written.
    run.screen.wait_for("Key for acme: ");
    run.screen.type_text(&format!("{KEY}\n"));
    run.screen
        .wait_for("fiber: stored credentials/acme/default");
    // The stored credential precedes the reviewer question: the one
    // installed model is chosen by number.
    // One wait for the input-readiness line: the header, rows and prompt
    // arrive as one burst, so a second wait could mark past them.
    run.screen.wait_for("Reviewer, by number or name: ");
    run.screen.type_text("1\n");
    run.screen.wait_for("fiber: reviewer acme m");
    let status = run.finish();
    let shown = &run.screen.seen;
    assert_eq!(status.code(), Some(0), "{shown:?}");
    assert!(!shown.contains(KEY), "the key was echoed: {shown:?}");
    assert!(run.terminal.echoes(), "echo was not restored");
    assert_eq!(
        fs::read_to_string(setup.home().join("credentials/acme/default")).unwrap(),
        KEY
    );
    let config: Value =
        serde_json::from_str(&fs::read_to_string(setup.home().join("config.json")).unwrap())
            .unwrap();
    assert_eq!(config["providers"]["acme"]["reviewer_model"], json!("m"));
}

#[test]
fn the_provider_menu_on_a_terminal_picks_by_number() {
    let setup = Setup::new_with_fl_root();
    setup.install_provider("acme", None, None);
    setup.install_provider("beta", None, None);
    setup.declare("acme-secrets", &["acme.api_key"]);
    let mut run = setup.on_terminal(&["login"]);
    let menu = run
        .screen
        .wait_for("Provider or secret, by number or name: ");
    assert!(menu.contains("  1) acme\r\n  2) beta\r\n"), "{menu:?}");
    assert!(
        menu.contains("Secrets:\r\n  3) acme.api_key\r\n"),
        "{menu:?}"
    );
    run.screen.type_text("2\n");
    run.screen.wait_for("Key for beta: ");
    run.screen.type_text(&format!("{KEY}\n"));
    // One wait for the input-readiness line: the stored line and the
    // reviewer burst can arrive in one chunk, so a second wait could
    // mark past them.
    run.screen.wait_for("Reviewer, by number or name: ");
    assert!(
        run.screen
            .seen
            .contains("fiber: stored credentials/beta/default"),
        "{:?}",
        run.screen.seen
    );
    run.screen.type_text("1\n");
    run.screen.wait_for("fiber: reviewer beta m");
    let status = run.finish();
    let shown = &run.screen.seen;
    assert_eq!(status.code(), Some(0), "{shown:?}");
    assert!(!shown.contains(KEY), "{shown:?}");
    assert!(setup.home().join("credentials/beta/default").exists());
    assert!(!setup.home().join("credentials/acme").exists());
    let config: Value =
        serde_json::from_str(&fs::read_to_string(setup.home().join("config.json")).unwrap())
            .unwrap();
    assert_eq!(config["providers"]["beta"]["reviewer_model"], json!("m"));
}

#[test]
fn end_of_input_at_the_key_prompt_restores_echo_and_stores_nothing() {
    let setup = Setup::new_with_fl_root();
    setup.install_provider("acme", None, None);
    let mut run = setup.on_terminal(&["login", "acme"]);
    run.screen.wait_for("Key for acme: ");
    run.terminal.wait_for_echo_off(setup.deadline);
    // End of input is the terminal's own character, typed with echo off.
    run.screen.type_text("\u{4}");
    run.screen.wait_for("No key was given");
    let status = run.finish();
    assert_eq!(status.code(), Some(2), "{:?}", run.screen.seen);
    assert!(run.terminal.echoes(), "echo was not restored");
    assert!(!setup.home().join("credentials/acme/default").exists());
}

#[test]
fn an_interrupt_at_the_key_prompt_restores_echo_and_ends_the_login() {
    let setup = Setup::new_with_fl_root();
    setup.install_provider("acme", None, None);
    let mut run = setup.on_terminal(&["login", "acme"]);
    run.screen.wait_for("Key for acme: ");
    run.terminal.wait_for_echo_off(setup.deadline);
    // The guarded helper: it refuses a group of 1 or less.
    assert!(support::kill_group(setup.deadline, run.group, "INT").unwrap());
    let status = run.finish();
    assert_eq!(status.signal(), Some(2), "{status:?}");
    assert!(run.terminal.echoes(), "echo was not restored");
    assert!(!setup.home().join("credentials/acme/default").exists());
}

#[test]
fn a_pipe_on_stdin_is_no_terminal_even_when_stderr_is_one() {
    let setup = Setup::new_with_fl_root();
    setup.install_provider("acme", None, None);
    let mut run = setup.stderr_on_terminal(&["login"], "acme\n");
    // The usage line ends the message; a menu would not print it.
    let shown = run.screen.wait_for("Run `fiber --help` for usage.");
    let status = run.finish();
    assert_eq!(status.code(), Some(2), "{shown:?}");
    assert!(shown.contains("no terminal"), "{shown:?}");
    assert!(!shown.contains("Providers:"), "{shown:?}");
    assert!(!setup.home().join("credentials").exists());
}

#[test]
fn labels_are_stored_and_deleted_through_argv() {
    let setup = Setup::new_with_fl_root();
    setup.install_provider("acme", None, None);
    let work = setup.run_with_input(&["login", "acme", "--as", "work"], &format!("{KEY}\n"));
    assert_eq!(work.code, Some(0), "{}", work.stderr);
    assert_eq!(work.stdout, "");
    assert_eq!(work.stderr, "fiber: stored credentials/acme/work\n");
    let file = setup.home().join("credentials/acme/work");
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let home = setup.run_with_input(&["login", "acme", "--as", "home"], "other-key\n");
    assert_eq!(home.code, Some(0), "{}", home.stderr);
    let same = setup.run_with_input(&["login", "acme", "--as", "work"], "third-key\n");
    assert_eq!(same.code, Some(2));
    assert!(same.stderr.contains("--as"), "{}", same.stderr);
    assert_eq!(setup.files_holding(KEY), std::slice::from_ref(&file));
    let config: Value =
        serde_json::from_str(&fs::read_to_string(setup.home().join("config.json")).unwrap())
            .unwrap();
    assert_eq!(
        config,
        json!({"providers": {"acme": {"credential": "work"}}})
    );

    let bare = setup.run_with_input(&["logout", "acme"], "");
    assert_eq!(bare.code, Some(2));
    assert!(file.exists());
    let both = setup.run_with_input(&["logout", "acme", "--as", "work", "--all"], "");
    assert_eq!(both.code, Some(2));
    assert!(file.exists());
    let one = setup.run_with_input(&["logout", "acme", "--as", "work"], "");
    assert_eq!(one.code, Some(0), "{}", one.stderr);
    assert_eq!(one.stderr, "fiber: removed credentials/acme/work\n");
    assert!(!file.exists());
    let all = setup.run_with_input(&["logout", "acme", "--all"], "");
    assert_eq!(all.code, Some(0), "{}", all.stderr);
    assert_eq!(all.stderr, "fiber: removed credentials/acme/home\n");
    assert!(!setup.home().join("credentials/acme/home").exists());
    assert!(!format!("{}{}{}", work.stderr, same.stderr, bare.stderr).contains(KEY));
}

/// Installs the codex package with its OAuth origin rewritten to `oauth`,
/// so the login talks to the fake.
fn install_codex(setup: &Setup, oauth: &str) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../providers/codex");
    let to = setup.home().join("extensions/codex");
    for file in [
        "extension.json",
        "providers/codex.json",
        "providers/codex.lua",
    ] {
        let text = fs::read_to_string(root.join(file)).unwrap();
        let dir = to.join(file).parent().unwrap().to_path_buf();
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            to.join(file),
            text.replace("https://auth.openai.com", oauth),
        )
        .unwrap();
    }
    write_record(&to);
}

/// A device-code exchange reply carrying `email`.
fn codex_exchange(email: &str) -> String {
    let access = fakes::jwt(&serde_json::json!({
        "https://api.openai.com/auth": { "chatgpt_account_id": "acct_1" },
        "exp": 4_102_444_800u64,
    }));
    let id = fakes::jwt(&serde_json::json!({ "email": email }));
    serde_json::json!({
        "access_token": access,
        "refresh_token": "rt_1",
        "id_token": id,
        "expires_in": 864000,
    })
    .to_string()
}

/// Messages from `login_device`'s stderr reader: chunks, an explicit EOF
/// on a clean read end, and an explicit error on a read failure. The
/// drain succeeds only on EOF; a disconnect without EOF fails, so a
/// reader panic can never pass as a clean drain.
enum StderrMsg {
    Chunk(Vec<u8>),
    Eof,
    ReadError(String),
}

/// The test's halves of a hold on `login_device`'s stderr reader. The
/// reader sends `held` before its send of the chunk carrying the final
/// stored-credentials line, then blocks on `release`; `login_device`
/// runs a non-blocking attempt while the reader is still held, then sends
/// `draining` once that attempt has completed, and the test releases the
/// reader only on `draining`, never on a sleep. This opens #2113's race
/// window on demand: the attempt while held misses the chunk on every run
/// (what the old `try_recv` drain saw), and the blocking drain after
/// release recovers it.
struct DeviceHold {
    held_tx: mpsc::Sender<()>,
    held_rx: mpsc::Receiver<()>,
    release_rx: mpsc::Receiver<()>,
    draining_tx: mpsc::Sender<()>,
}

/// Runs `fiber login codex --device`, reading stderr until `needle`
/// shows, then waiting for the exit under the deadline.
#[track_caller]
fn login_device(setup: &Setup, args: &[&str], needle: &str, hold: Option<DeviceHold>) -> Output {
    let mut command = setup.command(args);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let (mut child, watchdog) = spawn_watched(&mut command);
    let group = child.id();
    let guard = KillGroup(group);
    feed(&mut child, "");
    // Stderr arrives in chunks on a thread, so the wait for the device
    // code carries the deadline instead of hanging on a pipe.
    let (chunks, received) = mpsc::channel();
    let mut err = child.stderr.take().unwrap();
    let deadline = setup.deadline;
    let mut holder = None;
    let mut waiter = None;
    if let Some(device) = hold {
        holder = Some((device.held_tx, device.release_rx));
        waiter = Some((device.held_rx, device.draining_tx));
    }
    thread::spawn(move || {
        // The marker the final stored-credentials line carries.
        const MARKER: &[u8] = b"stored";
        let mut buf = [0u8; 4096];
        // Without a hold every chunk goes out at once.
        let mut released = holder.is_none();
        loop {
            match err.read(&mut buf) {
                Ok(0) => {
                    chunks.send(StderrMsg::Eof).unwrap_or(());
                    break;
                }
                Ok(n) => {
                    let mut chunk = &buf[..n];
                    if !released
                        && let Some(at) = chunk
                            .windows(MARKER.len())
                            .position(|window| window == MARKER)
                    {
                        // The prefix holds the device-code line the
                        // needle waits for, so it goes out at once;
                        // only the marker onward is held back.
                        let (prefix, rest) = chunk.split_at(at);
                        if chunks.send(StderrMsg::Chunk(prefix.to_vec())).is_err() {
                            break;
                        }
                        let (held_tx, release_rx) =
                            holder.as_ref().expect("a held chunk has its hold");
                        held_tx.send(()).unwrap_or(());
                        // The test releases this only after the
                        // non-blocking attempt below has completed.
                        // Ending here drops the sender without EOF,
                        // so the drain fails visibly instead of hanging.
                        if deadline.recv(release_rx).is_err() {
                            break;
                        }
                        released = true;
                        chunk = rest;
                    }
                    if chunks.send(StderrMsg::Chunk(chunk.to_vec())).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    chunks
                        .send(StderrMsg::ReadError(error.to_string()))
                        .unwrap_or(());
                    break;
                }
            }
        }
    });
    let mut stderr = Vec::new();
    loop {
        match setup.deadline.recv(&received) {
            Ok(StderrMsg::Chunk(chunk)) => {
                stderr.extend_from_slice(&chunk);
                if String::from_utf8_lossy(&stderr).contains(needle) {
                    break;
                }
            }
            Ok(StderrMsg::Eof) => {
                panic!("`fiber login codex --device` stderr ended before showing the device code")
            }
            Ok(StderrMsg::ReadError(error)) => {
                panic!("`fiber login codex --device` stderr read failed: {error}")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!(
                    "`fiber login codex --device` stderr reader ended without EOF before the device code"
                )
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("`fiber login codex --device` showed no device code within the deadline")
            }
        }
    }
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait_with_output()).unwrap());
    let output = match setup.deadline.recv(&finished) {
        Ok(output) => output.unwrap(),
        Err(_) => support::expired(
            setup.deadline,
            group,
            &finished,
            "`fiber login codex --device` to exit",
        ),
    };
    // Drain stderr to the pipe's end: the reader sends every chunk it
    // reads plus an explicit EOF, so receiving until EOF keeps the chunk
    // it had not sent when the child exited (#2113). A timeout, a read
    // error, or a disconnect without EOF fails naming the drain; a
    // partial read never passes.
    if let Some((held_rx, draining_tx)) = &waiter {
        // The window is open only once the reader holds its final
        // chunk; without this wait a reader that never holds would pass
        // vacuously.
        if setup.deadline.recv(held_rx).is_err() {
            panic!(
                "`fiber login codex --device` reader never held its final chunk within the deadline"
            );
        }
        // The non-blocking attempt runs while the reader is still held,
        // so the old `try_recv` drain misses the held chunk on every
        // run; the blocking drain below recovers it after release.
        while let Ok(message) = received.try_recv() {
            match message {
                StderrMsg::Chunk(chunk) => stderr.extend_from_slice(&chunk),
                StderrMsg::Eof => {
                    panic!(
                        "`fiber login codex --device` stderr ended before its held chunk was released"
                    )
                }
                StderrMsg::ReadError(error) => {
                    panic!("`fiber login codex --device` stderr read failed: {error}")
                }
            }
        }
        assert!(
            !String::from_utf8_lossy(&stderr).contains("stored"),
            "`fiber login codex --device` drain saw the held chunk before release"
        );
        // The test releases the reader only on this send, after the
        // attempt above has completed, so the window stays open through
        // the attempt instead of closing on a sleep.
        draining_tx.send(()).unwrap_or(());
    }
    loop {
        match setup.deadline.recv(&received) {
            Ok(StderrMsg::Chunk(chunk)) => stderr.extend_from_slice(&chunk),
            Ok(StderrMsg::Eof) => break,
            Ok(StderrMsg::ReadError(error)) => {
                panic!("`fiber login codex --device` stderr read failed: {error}")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("`fiber login codex --device` stderr reader ended without EOF")
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("`fiber login codex --device` stderr stalled within the deadline")
            }
        }
    }
    assert!(
        !group_alive(setup.deadline, group),
        "`fiber` left a process behind"
    );
    std::mem::forget(guard);
    watchdog.stand_down(setup.deadline.cleanup_phase());
    Output {
        code: output.status.code(),
        stdout: String::from_utf8(output.stdout).unwrap(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    }
}

#[test]
fn a_device_login_stores_the_email_label_then_logs_out() {
    let setup = Setup::new_with_fl_root();
    let oauth = fakes::OauthServer::start(vec![
        fakes::OauthReply::raw(
            200,
            &serde_json::json!({
                "device_auth_id": "da_1",
                "user_code": "ABCD-1234",
                "interval": "1",
            })
            .to_string(),
        ),
        fakes::OauthReply::raw(
            200,
            &serde_json::json!({ "authorization_code": "authcode-2", "code_verifier": "verifier-2" })
                .to_string(),
        ),
        fakes::OauthReply::raw(200, &codex_exchange("alice@example.com")),
    ]);
    install_codex(&setup, &oauth.url());
    let login = login_device(
        &setup,
        &["login", "codex", "--device"],
        "enter the code ABCD-1234",
        None,
    );
    assert_eq!(login.code, Some(0), "{}", login.stderr);
    assert_eq!(login.stdout, "");
    assert_eq!(
        login.stderr,
        format!(
            "Go to {}/codex/device and enter the code ABCD-1234\nfiber: stored credentials/codex/alice@example.com\n",
            oauth.url()
        )
    );
    let file = setup.home().join("credentials/codex/alice@example.com");
    assert!(file.is_file());
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let stored: Value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(stored["account_id"], "acct_1");
    assert_eq!(stored["refresh_token"], "rt_1");

    let logout = setup.run_with_input(&["logout", "codex", "--as", "alice@example.com"], "");
    assert_eq!(logout.code, Some(0), "{}", logout.stderr);
    assert_eq!(
        logout.stderr,
        "fiber: removed credentials/codex/alice@example.com\n"
    );
    assert!(!file.exists());
}

#[test]
fn a_device_login_with_as_to_an_already_stored_label_contacts_nothing() {
    let setup = Setup::new_with_fl_root();
    let oauth = fakes::OauthServer::start(vec![]);
    install_codex(&setup, &oauth.url());
    let dir = setup.home().join("credentials/codex");
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join("x");
    fs::write(
        &file,
        serde_json::json!({
            "token": "old",
            "expires_at": 4_102_444_800u64,
            "refresh_token": "rt",
            "account_id": "acct_0",
        })
        .to_string(),
    )
    .unwrap();
    let login = setup.run_with_input(&["login", "codex", "--as", "x"], "");
    assert_eq!(login.code, Some(2), "{}", login.stderr);
    assert!(login.stderr.contains("--as"), "{}", login.stderr);
    assert_eq!(oauth.request_count(), 0);
    assert!(file.is_file());
}

#[test]
fn a_device_login_keeps_stderr_the_reader_has_not_yet_sent() {
    let setup = Setup::new_with_fl_root();
    let oauth = fakes::OauthServer::start(vec![
        fakes::OauthReply::raw(
            200,
            &serde_json::json!({
                "device_auth_id": "da_1",
                "user_code": "ABCD-1234",
                "interval": "1",
            })
            .to_string(),
        ),
        fakes::OauthReply::raw(
            200,
            &serde_json::json!({ "authorization_code": "authcode-2", "code_verifier": "verifier-2" })
                .to_string(),
        ),
        fakes::OauthReply::raw(200, &codex_exchange("alice@example.com")),
    ]);
    install_codex(&setup, &oauth.url());
    // Hold the reader thread before its send of the chunk carrying the
    // final stored-credentials line, and release it only once the drain
    // waits for it (#2113). No sleep orders the window: each side moves
    // on the other's channel send.
    let (held_tx, held_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (draining_tx, draining_rx) = mpsc::channel();
    let deadline = setup.deadline;
    thread::spawn(move || {
        if deadline.recv(&draining_rx).is_err() {
            panic!("`login_device` never reached its stderr drain within the deadline");
        }
        release_tx.send(()).unwrap_or(());
    });
    let login = login_device(
        &setup,
        &["login", "codex", "--device"],
        "enter the code ABCD-1234",
        Some(DeviceHold {
            held_tx,
            held_rx,
            release_rx,
            draining_tx,
        }),
    );
    assert_eq!(login.code, Some(0), "{}", login.stderr);
    assert_eq!(login.stdout, "");
    assert!(
        login
            .stderr
            .contains("fiber: stored credentials/codex/alice@example.com"),
        "the drain dropped the chunk the reader had not sent: {}",
        login.stderr
    );
}

/// Copies the first-party package `providers/<name>` into Fiber home, as
/// `fiber extension install` leaves it, without contacting anything.
fn install_package(setup: &Setup, name: &str) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../providers")
        .join(name);
    let to = setup.home().join("extensions").join(name);
    fs::create_dir_all(to.join("providers")).unwrap();
    fs::write(
        to.join("extension.json"),
        fs::read_to_string(root.join("extension.json")).unwrap(),
    )
    .unwrap();
    for entry in fs::read_dir(root.join("providers")).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            fs::write(
                to.join("providers").join(entry.file_name()),
                fs::read(entry.path()).unwrap(),
            )
            .unwrap();
        }
    }
    write_record(&to);
}

fn global_config(setup: &Setup) -> Value {
    serde_json::from_str(&fs::read_to_string(setup.home().join("config.json")).unwrap()).unwrap()
}

#[test]
fn opencode_go_login_asks_with_nothing_preselected_and_saves_the_pick() {
    let setup = Setup::new_with_fl_root();
    install_package(&setup, "opencode");
    let mut run = setup.on_terminal(&["login", "opencode-go"]);
    run.screen.wait_for("Key for opencode-go: ");
    run.screen.type_text(&format!("{KEY}\n"));
    // One wait for the input-readiness line: the stored line and the
    // reviewer burst can arrive in one chunk, so a second wait could
    // mark past them.
    let shown = run.screen.wait_for("Reviewer, by number or name: ");
    assert!(
        run.screen
            .seen
            .contains("fiber: stored credentials/opencode/default"),
        "the stored credential precedes the question: {shown:?}"
    );
    assert!(
        shown.contains("Reviewer for opencode-go:"),
        "the stored credential precedes the question: {shown:?}"
    );
    assert!(
        !shown.contains("(saved)") && !shown.contains("Saved reviewer"),
        "nothing is preselected on opencode-go: {shown:?}"
    );
    // An invalid answer prints one line naming the problem and asks again:
    // the re-prompt proves the problem line arrived before it.
    run.screen.type_text("nope\n");
    let shown = run.screen.wait_for("Reviewer, by number or name: ");
    assert!(
        shown.contains("`nope` is not a listed reviewer"),
        "{shown:?}"
    );
    run.screen.type_text("muse-spark-1.3-contributor\n");
    run.screen
        .wait_for("fiber: reviewer opencode-go muse-spark-1.3-contributor");
    let status = run.finish();
    let shown = &run.screen.seen;
    assert_eq!(status.code(), Some(0), "{shown:?}");
    assert!(!shown.contains(KEY), "{shown:?}");
    assert!(run.terminal.echoes(), "echo was not restored");
    assert_eq!(
        fs::read_to_string(setup.home().join("credentials/opencode/default")).unwrap(),
        KEY
    );
    let config = global_config(&setup);
    assert_eq!(
        config["providers"]["opencode-go"]["reviewer_model"],
        json!("muse-spark-1.3-contributor")
    );
    assert!(
        config["providers"].get("opencode").is_none(),
        "each provider keeps its own pick: {config}"
    );
}

#[test]
fn anthropic_login_preselects_its_vendor_default_for_enter() {
    let setup = Setup::new_with_fl_root();
    install_package(&setup, "anthropic");
    let mut run = setup.on_terminal(&["login", "anthropic"]);
    run.screen.wait_for("Key for anthropic: ");
    run.screen.type_text(&format!("{KEY}\n"));
    // One wait for the input-readiness line: the stored line and the
    // reviewer burst can arrive in one chunk, so a second wait could
    // mark past them.
    let shown = run.screen.wait_for("Reviewer, by number or name: ");
    assert!(
        run.screen
            .seen
            .contains("fiber: stored credentials/anthropic/default"),
        "the stored credential precedes the question: {shown:?}"
    );
    assert!(
        shown.contains("claude-sonnet-5-5 (saved)"),
        "the vendor default is preselected: {shown:?}"
    );
    run.screen.type_text("\n");
    run.screen
        .wait_for("fiber: reviewer anthropic claude-sonnet-5-5");
    let status = run.finish();
    assert_eq!(status.code(), Some(0), "{:?}", run.screen.seen);
    assert_eq!(
        global_config(&setup)["providers"]["anthropic"]["reviewer_model"],
        json!("claude-sonnet-5-5")
    );
}

#[test]
fn a_piped_login_with_a_terminal_stderr_asks_nothing_and_keeps_a_saved_reviewer() {
    let setup = Setup::new_with_fl_root();
    setup.install_provider("acme", None, None);
    fs::write(
        setup.home().join("config.json"),
        r#"{"providers": {"acme": {"reviewer_model": "old-m"}}}"#,
    )
    .unwrap();
    let mut run = setup.stderr_on_terminal(&["login", "acme"], &format!("{KEY}\n"));
    let shown = run
        .screen
        .wait_for("fiber: stored credentials/acme/default");
    let status = run.finish();
    assert_eq!(status.code(), Some(0), "{shown:?}");
    assert!(!shown.contains("Reviewer"), "{shown:?}");
    assert_eq!(
        global_config(&setup)["providers"]["acme"]["reviewer_model"],
        json!("old-m"),
        "a pipe login writes no reviewer"
    );
}

#[test]
fn a_terminal_stdin_with_piped_stderr_is_no_terminal() {
    let setup = Setup::new_with_fl_root();
    setup.install_provider("acme", None, None);
    fs::write(
        setup.home().join("config.json"),
        r#"{"providers": {"acme": {"reviewer_model": "old-m"}}}"#,
    )
    .unwrap();
    let run = setup.run_with_terminal_stdin(&["login", "acme"], &format!("{KEY}\n"));
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    // The leading newline is the key read's: echo was off on the
    // terminal stdin, so the unread newline is written out.
    assert_eq!(run.stderr, "\nfiber: stored credentials/acme/default\n");
    assert_eq!(
        global_config(&setup)["providers"]["acme"]["reviewer_model"],
        json!("old-m"),
        "without a terminal on stderr nothing is asked"
    );
}

#[test]
fn a_relogin_keeps_a_stale_saved_pick_on_enter() {
    let setup = Setup::new_with_fl_root();
    setup.install_provider("acme", None, None);
    let mut first = setup.on_terminal(&["login", "acme"]);
    first.screen.wait_for("Key for acme: ");
    first.screen.type_text(&format!("{KEY}\n"));
    // One wait for the input-readiness line: the stored line and the
    // reviewer burst can arrive in one chunk, so a second wait could
    // mark past them.
    first.screen.wait_for("Reviewer, by number or name: ");
    assert!(
        first
            .screen
            .seen
            .contains("fiber: stored credentials/acme/default"),
        "{:?}",
        first.screen.seen
    );
    first.screen.type_text("1\n");
    first.screen.wait_for("fiber: reviewer acme m");
    assert_eq!(first.finish().code(), Some(0));
    // A pick the list no longer names stays on Enter.
    let mut config = global_config(&setup);
    config["providers"]["acme"]["reviewer_model"] = json!("stale-m");
    fs::write(
        setup.home().join("config.json"),
        serde_json::to_string(&config).unwrap(),
    )
    .unwrap();
    let mut second = setup.on_terminal(&["login", "acme", "--as", "work"]);
    second.screen.wait_for("Key for acme: ");
    second.screen.type_text(&format!("{KEY}\n"));
    // One wait for the input-readiness line: the stored line, the
    // heading and the prompt can arrive in one chunk, so a second wait
    // could mark past them.
    second.screen.wait_for("Reviewer, by number or name: ");
    assert!(
        second
            .screen
            .seen
            .contains("fiber: stored credentials/acme/work"),
        "{:?}",
        second.screen.seen
    );
    assert!(
        second.screen.seen.contains("Reviewer for acme:"),
        "{:?}",
        second.screen.seen
    );
    assert!(
        second
            .screen
            .seen
            .contains("Saved reviewer `stale-m` is not listed; Enter keeps it."),
        "{:?}",
        second.screen.seen
    );
    second.screen.type_text("\n");
    second.screen.wait_for("fiber: reviewer acme stale-m");
    assert_eq!(second.finish().code(), Some(0));
    assert_eq!(
        global_config(&setup)["providers"]["acme"]["reviewer_model"],
        json!("stale-m")
    );
}

#[test]
fn two_providers_sharing_a_credential_keep_their_own_picks() {
    let setup = Setup::new_with_fl_root();
    install_package(&setup, "opencode");
    let mut go = setup.on_terminal(&["login", "opencode-go"]);
    go.screen.wait_for("Key for opencode-go: ");
    go.screen.type_text(&format!("{KEY}\n"));
    // One wait for the input-readiness line: the stored line and the
    // reviewer burst can arrive in one chunk, so a second wait could
    // mark past them.
    go.screen.wait_for("Reviewer, by number or name: ");
    assert!(
        go.screen
            .seen
            .contains("fiber: stored credentials/opencode/default"),
        "{:?}",
        go.screen.seen
    );
    go.screen.type_text("1\n");
    go.screen.wait_for("fiber: reviewer opencode-go");
    assert_eq!(go.finish().code(), Some(0));
    // The shared directory's `default` label is taken: the second login
    // uses a fresh label, and the vendor default is preselected.
    let mut zen = setup.on_terminal(&["login", "opencode-zen", "--as", "work"]);
    zen.screen.wait_for("Key for opencode-zen: ");
    zen.screen.type_text(&format!("{KEY}\n"));
    // One wait for the input-readiness line: the stored line and the
    // reviewer burst can arrive in one chunk, so a second wait could
    // mark past them.
    let shown = zen.screen.wait_for("Reviewer, by number or name: ");
    assert!(
        zen.screen
            .seen
            .contains("fiber: stored credentials/opencode/work"),
        "the stored credential precedes the question: {shown:?}"
    );
    assert!(shown.contains("(saved)"), "{shown:?}");
    zen.screen.type_text("\n");
    zen.screen.wait_for("fiber: reviewer opencode-zen");
    assert_eq!(zen.finish().code(), Some(0));
    let config = global_config(&setup);
    assert_eq!(
        config["providers"]["opencode-go"]["reviewer_model"],
        json!("claude-haiku-5-5"),
        "the first provider's pick stands: {config}"
    );
    assert_eq!(
        config["providers"]["opencode-zen"]["reviewer_model"],
        json!("muse-spark-1.3"),
        "the package default landed on Enter: {config}"
    );
}

#[test]
fn end_of_input_at_the_reviewer_prompt_keeps_the_credential_and_sets_nothing() {
    let setup = Setup::new_with_fl_root();
    setup.install_provider("acme", None, None);
    fs::write(
        setup.home().join("config.json"),
        r#"{"providers": {"acme": {"reviewer_model": "old-m"}}}"#,
    )
    .unwrap();
    let mut run = setup.on_terminal(&["login", "acme"]);
    run.screen.wait_for("Key for acme: ");
    run.screen.type_text(&format!("{KEY}\n"));
    // One wait for the input-readiness line: the stored line and the
    // reviewer burst can arrive in one chunk, so a second wait could
    // mark past them.
    // The reviewer prompt is the input-readiness barrier, not the save:
    // end of input lands after storage with nothing saved.
    run.screen.wait_for("Reviewer, by number or name: ");
    assert!(
        run.screen
            .seen
            .contains("fiber: stored credentials/acme/default"),
        "{:?}",
        run.screen.seen
    );
    run.screen.type_text("\u{4}");
    run.screen.wait_for("no reviewer was set");
    let status = run.finish();
    let shown = &run.screen.seen;
    assert_eq!(status.code(), Some(0), "{shown:?}");
    assert!(run.terminal.echoes(), "echo was not restored");
    assert_eq!(
        fs::read_to_string(setup.home().join("credentials/acme/default")).unwrap(),
        KEY
    );
    assert_eq!(
        global_config(&setup)["providers"]["acme"]["reviewer_model"],
        json!("old-m"),
        "the previous reviewer setting stands"
    );
}

#[test]
fn an_interrupt_at_the_reviewer_prompt_keeps_the_credential_and_sets_nothing() {
    let setup = Setup::new_with_fl_root();
    setup.install_provider("acme", None, None);
    fs::write(
        setup.home().join("config.json"),
        r#"{"providers": {"acme": {"reviewer_model": "old-m"}}}"#,
    )
    .unwrap();
    let mut run = setup.on_terminal(&["login", "acme"]);
    run.screen.wait_for("Key for acme: ");
    run.screen.type_text(&format!("{KEY}\n"));
    // One wait for the input-readiness line: the stored line and the
    // reviewer burst can arrive in one chunk, so a second wait could
    // mark past them.
    run.screen.wait_for("Reviewer, by number or name: ");
    assert!(
        run.screen
            .seen
            .contains("fiber: stored credentials/acme/default"),
        "{:?}",
        run.screen.seen
    );
    // The prompt blocks on terminal input: the interrupt must end the
    // read with no reviewer set, not hang inside it.
    assert!(support::kill_group(setup.deadline, run.group, "INT").unwrap());
    run.screen.wait_for("no reviewer was set");
    let status = run.finish();
    let shown = &run.screen.seen;
    assert_eq!(status.code(), Some(0), "{shown:?}");
    assert!(run.terminal.echoes(), "echo was not restored");
    assert_eq!(
        fs::read_to_string(setup.home().join("credentials/acme/default")).unwrap(),
        KEY
    );
    assert_eq!(
        global_config(&setup)["providers"]["acme"]["reviewer_model"],
        json!("old-m"),
        "the previous reviewer setting stands"
    );
}

#[test]
fn a_reviewer_write_failure_names_the_key_and_keeps_the_credential() {
    let setup = Setup::new_with_fl_root();
    setup.install_provider("acme", None, None);
    let mut run = setup.on_terminal(&["login", "acme"]);
    run.screen.wait_for("Key for acme: ");
    run.screen.type_text(&format!("{KEY}\n"));
    // One wait for the input-readiness line: the stored line and the
    // reviewer burst can arrive in one chunk, so a second wait could
    // mark past them.
    run.screen.wait_for("Reviewer, by number or name: ");
    assert!(
        run.screen
            .seen
            .contains("fiber: stored credentials/acme/default"),
        "{:?}",
        run.screen.seen
    );
    // Another writer replaces the whole document with `[]` after the
    // prompt: the save fails instead of destroying it.
    fs::write(setup.home().join("config.json"), "[]").unwrap();
    run.screen.type_text("1\n");
    run.screen.wait_for("providers.acme.reviewer_model");
    let status = run.finish();
    let shown = &run.screen.seen;
    assert_eq!(status.code(), Some(1), "{shown:?}");
    assert!(
        !shown.contains("fiber: reviewer"),
        "no success is claimed for the failed save: {shown:?}"
    );
    assert!(
        !shown.contains(KEY),
        "no secret reaches the output: {shown:?}"
    );
    assert!(run.terminal.echoes(), "echo was not restored");
    assert_eq!(
        fs::read_to_string(setup.home().join("credentials/acme/default")).unwrap(),
        KEY
    );
    assert_eq!(
        fs::read_to_string(setup.home().join("config.json")).unwrap(),
        "[]"
    );
}

#[test]
fn an_interrupt_during_reviewer_discovery_keeps_the_credential_and_sets_nothing() {
    // A provider whose `models()` pings a held loopback server: the
    // request proves discovery is in flight before any reviewer prompt.
    let setup = Setup::new_with_fl_root();
    let held = ProviderServer::start([Response::status(200, "{}")]).unwrap();
    held.hold();
    let url = format!("{}/ping", held.url());
    let dir = setup.home().join("extensions").join("held");
    fs::create_dir_all(dir.join("providers")).unwrap();
    fs::write(
        dir.join("extension.json"),
        json!({"name": "held", "version": "v0.0.0", "fiber": "0.0.0", "api": 1,
               "providers": {"held": ["http://127.0.0.1:9/v1"]}})
        .to_string(),
    )
    .unwrap();
    write_record(&dir);
    fs::write(
        dir.join("providers/held.json"),
        json!({"name": "held", "models": [{
            "id": "data-m", "protocol": "openai-responses",
            "base_url": "http://127.0.0.1:9/v1", "context_window": 1000}]})
        .to_string(),
    )
    .unwrap();
    fs::write(
        dir.join("providers/held.lua"),
        format!(
            "fiber.provider(\"held\", {{models = {{timeout = 30000, run = function()\n\
              host.http({{url = \"{url}\"}})\n\
              return {{{{id = \"live-m\", protocol = \"openai-responses\", \
              base_url = \"http://127.0.0.1:9/v1\", context_window = 1000}}}}\n\
              end}}}})\n"
        ),
    )
    .unwrap();
    let mut run = setup.on_terminal(&["login", "held"]);
    run.screen.wait_for("Key for held: ");
    run.screen.type_text(&format!("{KEY}\n"));
    run.screen
        .wait_for("fiber: stored credentials/held/default");
    assert!(
        held.await_requests(1, setup.deadline),
        "discovery requested while the reviewer prompt was still down"
    );
    assert!(
        !run.screen.seen.contains("Reviewer"),
        "no prompt before discovery ends: {:?}",
        run.screen.seen
    );
    assert!(support::kill_group(setup.deadline, run.group, "INT").unwrap());
    held.release();
    run.screen.wait_for("no reviewer was set");
    let status = run.finish();
    let shown = &run.screen.seen;
    assert_eq!(status.code(), Some(0), "{shown:?}");
    assert!(
        !shown.contains("Reviewer, by number or name"),
        "no reviewer was asked after the interrupt: {shown:?}"
    );
    assert!(!shown.contains(KEY), "{shown:?}");
    assert!(run.terminal.echoes(), "echo was not restored");
    assert_eq!(
        fs::read_to_string(setup.home().join("credentials/held/default")).unwrap(),
        KEY
    );
    assert!(
        !global_config(&setup)["providers"]["held"]
            .as_object()
            .is_some_and(|provider| provider.contains_key("reviewer_model")),
        "no reviewer was written"
    );
}

/// Installs the `acme` provider with one data model whose base URL names
/// `placeholders`, read from the global `config/acme.json`.
fn install_host_provider_with(setup: &Setup, placeholders: Value) {
    let dir = setup.home().join("extensions").join("acme");
    fs::create_dir_all(dir.join("providers")).unwrap();
    let mut manifest = json!({"name": "acme", "version": "v0.0.0", "fiber": "0.0.0", "api": 1});
    manifest["providers"] = serde_json::Value::Object(
        [("acme".to_owned(), json!(["https://{workspace}/v1"]))]
            .into_iter()
            .collect(),
    );
    fs::write(dir.join("extension.json"), manifest.to_string()).unwrap();
    write_record(&dir);
    fs::write(
        dir.join("providers").join("acme.json"),
        json!({
            "name": "acme",
            "placeholders": placeholders,
            "models": [{"id": "m", "protocol": "openai-responses",
                        "base_url": "https://{workspace}/v1", "context_window": 1000}],
        })
        .to_string(),
    )
    .unwrap();
}

/// Installs the `acme` provider with the `workspace` placeholder and no
/// environment fallback.
fn install_host_provider(setup: &Setup) {
    install_host_provider_with(setup, json!({"workspace": {}}));
}

/// Runs `fiber models` on pipes, returning its exit code and streams.
fn list_models(setup: &Setup) -> Output {
    let mut command = setup.command(&["models"]);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    setup.run_piped(&mut command, &["models"], "")
}

#[test]
fn screen_wait_for_keeps_trailing_text_for_the_next_wait() {
    // One read holds both prompts. The second wait must find its text
    // already received; closing the sender makes a missed prompt fail at EOF.
    let terminal = Terminal::open();
    let (send, chunks) = mpsc::channel();
    send.send("fiber: stored credentials/acme/default\r\nReviewer, by number or name: ".to_owned())
        .unwrap();
    drop(send);
    let mut screen = Screen {
        chunks,
        typed: fs::File::from(terminal.main.try_clone().unwrap()),
        seen: String::new(),
        consumed: 0,
        deadline: Deadline::after(MUST_SUCCEED_WITHIN),
    };
    let first = screen.wait_for("fiber: stored credentials/acme/default");
    assert!(
        first.contains("fiber: stored credentials/acme/default"),
        "{first:?}"
    );
    let second = screen.wait_for("Reviewer, by number or name: ");
    assert!(
        second.contains("Reviewer, by number or name: "),
        "{second:?}"
    );
}

#[test]
fn a_terminal_login_asks_for_the_host_and_lists_the_models_after() {
    let setup = Setup::new_with_fl_root();
    install_host_provider(&setup);
    let mut run = setup.on_terminal(&["login", "acme"]);
    run.screen.wait_for("Key for acme: ");
    run.screen.type_text(&format!("{KEY}\n"));
    run.screen.type_text("https://adb-1.example.test/\n");
    run.screen
        .wait_for("fiber: stored credentials/acme/default");
    run.screen.wait_for("Reviewer, by number or name: ");
    run.screen.type_text("1\n");
    run.screen.wait_for("fiber: reviewer acme m");
    let status = run.finish();
    let shown = &run.screen.seen;
    assert_eq!(status.code(), Some(0), "{shown:?}");
    assert!(shown.contains("`workspace`"), "{shown:?}");
    let saved: Value =
        serde_json::from_str(&fs::read_to_string(setup.home().join("config/acme.json")).unwrap())
            .unwrap();
    assert_eq!(saved, json!({"workspace": "adb-1.example.test"}));
    let models = list_models(&setup);
    assert_eq!(models.code, Some(0), "{}", models.stderr);
    assert!(models.stdout.contains("acme/m"), "{}", models.stdout);
}

#[test]
fn without_a_terminal_a_login_names_the_setting_and_variable_that_fill_the_host() {
    let setup = Setup::new_with_fl_root();
    install_host_provider_with(
        &setup,
        json!({"workspace": {"env": "FIBER_TEST_2909_UNSET_HOST"}}),
    );
    let output = setup.run_with_input(&["login", "acme"], &format!("{KEY}\n"));
    assert_eq!(output.code, Some(0), "{}", output.stderr);
    assert!(
        output.stderr.contains(
            "fiber: `workspace` has no value; set it in `config/acme.json` in Fiber home, \
             or export `FIBER_TEST_2909_UNSET_HOST`."
        ),
        "{}",
        output.stderr
    );
    assert!(
        output
            .stderr
            .contains("fiber: stored credentials/acme/default\n"),
        "{}",
        output.stderr
    );
    assert!(!setup.home().join("config/acme.json").exists());
}

#[test]
fn a_project_empty_hides_the_saved_host_and_names_its_file() {
    let setup = Setup::new_with_fl_root();
    install_host_provider(&setup);
    let workspace = setup.root.path().join("w");
    let (_, project) = cli::project_of(&setup.home(), &workspace).unwrap();
    let dir = setup
        .home()
        .join("projects")
        .join(project.as_str())
        .join("config");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("acme.json"), r#"{"workspace":""}"#).unwrap();
    let mut run = setup.on_terminal(&["login", "acme"]);
    run.screen.wait_for("Key for acme: ");
    run.screen.type_text(&format!("{KEY}\n"));
    run.screen.type_text("adb-1.example.test\n");
    run.screen
        .wait_for("fiber: stored credentials/acme/default");
    run.screen.wait_for("Reviewer, by number or name: ");
    run.screen.type_text("1\n");
    run.screen.wait_for("fiber: reviewer acme m");
    let status = run.finish();
    let shown = &run.screen.seen;
    assert_eq!(status.code(), Some(0), "{shown:?}");
    let saved: Value =
        serde_json::from_str(&fs::read_to_string(setup.home().join("config/acme.json")).unwrap())
            .unwrap();
    assert_eq!(saved, json!({"workspace": "adb-1.example.test"}));
    let file = dir.join("acme.json");
    assert!(shown.contains("sets it empty and hides it"), "{shown:?}");
    assert!(shown.contains(&file.display().to_string()), "{shown:?}");
    // The mask holds: the model stays out of the list.
    let models = list_models(&setup);
    assert_eq!(models.code, Some(0), "{}", models.stderr);
    assert!(!models.stdout.contains("acme/m"), "{}", models.stdout);
}

#[test]
fn login_stores_a_key_for_an_approved_repository_provider() {
    let setup = Setup::new_with_fl_root();
    git_init(&setup.workspace());
    declare_repo_provider(
        &setup.workspace(),
        "fiber.test/acme",
        "acme",
        &json!([{"id": "m", "protocol": "openai-responses",
                 "base_url": "http://127.0.0.1:9/v1", "context_window": 1000}]),
    );
    let approved = setup.run_with_input(&["approve", "--yes"], "");
    assert_eq!(approved.code, Some(0), "{}", approved.stderr);
    let login = setup.run_with_input(&["login", "acme"], &format!("{KEY}\n"));
    assert_eq!(login.code, Some(0), "{}", login.stderr);
    assert_eq!(login.stdout, "");
    assert_eq!(login.stderr, "fiber: stored credentials/acme/default\n");
    assert_eq!(
        fs::read_to_string(setup.home().join("credentials/acme/default")).unwrap(),
        KEY
    );
}

#[test]
fn login_outside_the_project_finds_no_repository_provider() {
    let setup = Setup::new_with_fl_root();
    git_init(&setup.workspace());
    declare_repo_provider(
        &setup.workspace(),
        "fiber.test/acme",
        "acme",
        &json!([{"id": "m", "protocol": "openai-responses",
                 "base_url": "http://127.0.0.1:9/v1", "context_window": 1000}]),
    );
    let approved = setup.run_with_input(&["approve", "--yes"], "");
    assert_eq!(approved.code, Some(0), "{}", approved.stderr);
    let outside = fakes::TempDir::new("fl-outside");
    // The workspace run's command anywhere else: the approved provider is
    // no login target there.
    let mut command = setup.command(&["login", "acme"]);
    command
        .current_dir(outside.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let login = setup.run_piped(&mut command, &["login", "acme"], &format!("{KEY}\n"));
    assert_ne!(login.code, Some(0), "{}", login.stderr);
    assert!(!login.stderr.contains("stored"), "{}", login.stderr);
    assert!(!setup.home().join("credentials/acme/default").exists());
}
