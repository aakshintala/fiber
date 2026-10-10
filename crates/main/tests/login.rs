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
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use support::write_record;

use fakes::Watchdog;
use rustix::pty;
use rustix::termios::{self, LocalModes};
use serde_json::{Value, json};
use support::{Deadline, group_alive};

const KEY: &str = "sk-live-7f3a9c0d1e2b";

/// A temporary root holding Fiber home and the workspace, removed on drop.
struct Setup {
    root: fakes::TempDir,
    deadline: Deadline,
}

impl Setup {
    fn new() -> Self {
        let deadline = Deadline::start();
        let root = fakes::TempDir::new("fl");
        fs::create_dir_all(root.path().join("h")).unwrap();
        fs::create_dir_all(root.path().join("w")).unwrap();
        Self { deadline, root }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("h")
    }

    /// Installs the provider `name`, reading the stored credential
    /// `credential_name` when it is set, and declaring `source` for its key.
    fn provider(&self, name: &str, credential_name: Option<&str>, source: Option<Value>) {
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
        fs::write(
            dir.join("extension.json"),
            json!({"name": name, "version": "v0.0.0", "fiber": "0.0.0", "api": 1}).to_string(),
        )
        .unwrap();
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
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path())
            .env("FIBER_HOME", self.home());
        command
    }

    /// Runs `fiber` with `args` and `input` on a closed pipe for stdin.
    fn fiber(&self, args: &[&str], input: &str) -> Run {
        let mut command = self.command(args);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (mut child, watchdog) = spawn_watched(&mut command);
        let group = child.id();
        let guard = KillGroup(group);
        feed(&mut child, input);
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait_with_output()).unwrap());
        let output = match finished.recv_timeout(self.deadline.left()) {
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
        watchdog.stand_down(self.deadline.cleanup());
        Run {
            code: output.status.code(),
            stdout: String::from_utf8(output.stdout).unwrap(),
            stderr: String::from_utf8(output.stderr).unwrap(),
        }
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

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn spawn_watched(command: &mut Command) -> (Child, Watchdog) {
    let child = command.process_group(0).spawn().unwrap();
    let group = child.id();
    // A failed watchdog spawn still kills the child on unwind.
    let guard = KillGroup(group);
    let watchdog = Watchdog::group(group);
    std::mem::forget(guard);
    (child, watchdog)
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

/// Kills process group `group` on drop. After the child is reaped and the
/// group is empty, [`std::mem::forget`] skips that kill.
struct KillGroup(u32);

impl Drop for KillGroup {
    fn drop(&mut self) {
        // A panic between spawn and reap still kills the group. Failure
        // here is ignored: the process may already be gone.
        support::kill_group_detached(self.0, "KILL");
    }
}

/// A pseudo-terminal, opened through rustix's safe calls.
struct Terminal {
    main: OwnedFd,
    terminal: fs::File,
}

impl Terminal {
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
        if is_off.recv_timeout(deadline.left()).is_err() {
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
    deadline: Deadline,
}

impl Screen {
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
            deadline,
        }
    }

    /// Reads until `text` has appeared, and returns everything read since
    /// the last wait.
    fn wait_for(&mut self, text: &str) -> String {
        let mark = self.seen.len();
        while !self.seen[mark..].contains(text) {
            // Each chunk has the deadline: a terminal that goes quiet fails.
            match self.chunks.recv_timeout(self.deadline.left()) {
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
        }
        self.seen[mark..].to_owned()
    }

    /// Types `text` on a thread bounded by the test's [`Deadline`].
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
    fn finish(&mut self) -> ExitStatus {
        let mut child = self.child.take().unwrap();
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait()).unwrap());
        let status = match finished.recv_timeout(self.deadline.left()) {
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
            .stand_down(self.deadline.cleanup());
        status
    }
}

#[test]
fn a_key_on_a_pipe_is_stored_then_deleted_and_never_printed() {
    let setup = Setup::new();
    setup.provider("acme", None, None);
    let login = setup.fiber(&["login", "acme"], &format!("  {KEY}  \n"));
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

    let again = setup.fiber(&["login", "acme"], "other\n");
    assert_eq!(again.code, Some(2));
    assert!(
        again
            .stderr
            .contains("with --as <label>, or run `fiber logout acme --as default` first"),
        "{}",
        again.stderr
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), KEY);

    let logout = setup.fiber(&["logout", "acme"], "");
    assert_eq!(logout.code, Some(0), "{}", logout.stderr);
    assert_eq!(logout.stdout, "");
    assert_eq!(logout.stderr, "fiber: removed credentials/acme/default\n");
    assert!(!file.exists());
    let missing = setup.fiber(&["logout", "acme"], "");
    assert_eq!(missing.code, Some(1));
    assert_eq!(missing.stderr, "fiber: no stored credential for acme\n");
    assert!(!format!("{}{}", login.stderr, missing.stderr).contains(KEY));
}

#[test]
fn providers_sharing_a_credential_log_in_and_out_through_either() {
    let setup = Setup::new();
    setup.provider("opencode-go", Some("opencode"), None);
    setup.provider("opencode-zen", Some("opencode"), None);
    let login = setup.fiber(&["login", "opencode-go"], &format!("{KEY}\n"));
    assert_eq!(login.code, Some(0), "{}", login.stderr);
    assert_eq!(login.stderr, "fiber: stored credentials/opencode/default\n");
    let logout = setup.fiber(&["logout", "opencode-zen"], "");
    assert_eq!(logout.code, Some(0), "{}", logout.stderr);
    assert_eq!(
        logout.stderr,
        "fiber: removed credentials/opencode/default, which opencode-go also reads\n"
    );
    assert!(!setup.home().join("credentials/opencode/default").exists());
}

#[test]
fn a_key_from_the_environment_is_named_and_the_exit_is_non_zero() {
    let setup = Setup::new();
    setup.provider("acme", None, Some(json!({"env": "ACME_API_KEY"})));
    let run = setup.fiber(&["logout", "acme"], "");
    assert_eq!(run.code, Some(1));
    assert_eq!(
        run.stderr,
        "fiber: acme's key comes from the environment variable ACME_API_KEY; fiber logout cannot remove it\n"
    );
    assert_eq!(run.stdout, "");
}

#[test]
fn no_provider_is_a_usage_error_without_a_terminal() {
    let setup = Setup::new();
    setup.provider("acme", None, None);
    let login = setup.fiber(&["login"], "acme\n");
    assert_eq!(login.code, Some(2));
    assert!(login.stderr.contains("no terminal"), "{}", login.stderr);
    assert_eq!(login.stdout, "");
    assert!(!setup.home().join("credentials").exists());
    let logout = setup.fiber(&["logout"], "");
    assert_eq!(logout.code, Some(2));
    assert_eq!(
        logout.stderr,
        "fiber: `fiber logout` takes the provider to log out of. Run `fiber --help` for usage.\n"
    );
    let unknown = setup.fiber(&["login", "nope"], "k\n");
    assert_eq!(unknown.code, Some(2));
    assert!(unknown.stderr.contains("the installed providers are acme"));
}

#[test]
fn a_declared_secret_on_a_pipe_is_stored_then_replaced_and_never_printed() {
    let setup = Setup::new();
    setup.declare("acme", &["acme.api_key"]);
    let login = setup.fiber(&["login", "acme.api_key"], "  v1-7f3a9c0d  \n");
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
    let again = setup.fiber(&["login", "acme.api_key"], "v2-1e2b\n");
    assert_eq!(again.code, Some(0), "{}", again.stderr);
    assert_eq!(again.stderr, "fiber: replaced credentials/acme.api_key\n");
    assert_eq!(fs::read_to_string(&file).unwrap(), "v2-1e2b");
}

#[test]
fn a_mistyped_secret_or_one_with_as_is_a_usage_error_that_writes_nothing() {
    let setup = Setup::new();
    setup.provider("acme", None, None);
    setup.declare("acme-secrets", &["acme.api_key"]);
    let typo = setup.fiber(&["login", "acme.api_kye"], "v1\n");
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
    let labelled = setup.fiber(&["login", "acme.api_key", "--as", "work"], "v1\n");
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
    let setup = Setup::new();
    setup.provider("acme", None, None);
    let mut run = setup.on_terminal(&["login", "acme"]);
    // Sent the moment the prompt shows, with no wait for echo to go off: echo
    // is already off when the prompt is written.
    run.screen.wait_for("Key for acme: ");
    run.screen.type_text(&format!("{KEY}\n"));
    run.screen
        .wait_for("fiber: stored credentials/acme/default");
    let status = run.finish();
    let shown = &run.screen.seen;
    assert_eq!(status.code(), Some(0), "{shown:?}");
    assert!(!shown.contains(KEY), "the key was echoed: {shown:?}");
    assert!(run.terminal.echoes(), "echo was not restored");
    assert_eq!(
        fs::read_to_string(setup.home().join("credentials/acme/default")).unwrap(),
        KEY
    );
}

#[test]
fn the_provider_menu_on_a_terminal_picks_by_number() {
    let setup = Setup::new();
    setup.provider("acme", None, None);
    setup.provider("beta", None, None);
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
    run.screen
        .wait_for("fiber: stored credentials/beta/default");
    let status = run.finish();
    let shown = &run.screen.seen;
    assert_eq!(status.code(), Some(0), "{shown:?}");
    assert!(!shown.contains(KEY), "{shown:?}");
    assert!(setup.home().join("credentials/beta/default").exists());
    assert!(!setup.home().join("credentials/acme").exists());
}

#[test]
fn end_of_input_at_the_key_prompt_restores_echo_and_stores_nothing() {
    let setup = Setup::new();
    setup.provider("acme", None, None);
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
    let setup = Setup::new();
    setup.provider("acme", None, None);
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
    let setup = Setup::new();
    setup.provider("acme", None, None);
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
    let setup = Setup::new();
    setup.provider("acme", None, None);
    let work = setup.fiber(&["login", "acme", "--as", "work"], &format!("{KEY}\n"));
    assert_eq!(work.code, Some(0), "{}", work.stderr);
    assert_eq!(work.stdout, "");
    assert_eq!(work.stderr, "fiber: stored credentials/acme/work\n");
    let file = setup.home().join("credentials/acme/work");
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let home = setup.fiber(&["login", "acme", "--as", "home"], "other-key\n");
    assert_eq!(home.code, Some(0), "{}", home.stderr);
    let same = setup.fiber(&["login", "acme", "--as", "work"], "third-key\n");
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

    let bare = setup.fiber(&["logout", "acme"], "");
    assert_eq!(bare.code, Some(2));
    assert!(file.exists());
    let both = setup.fiber(&["logout", "acme", "--as", "work", "--all"], "");
    assert_eq!(both.code, Some(2));
    assert!(file.exists());
    let one = setup.fiber(&["logout", "acme", "--as", "work"], "");
    assert_eq!(one.code, Some(0), "{}", one.stderr);
    assert_eq!(one.stderr, "fiber: removed credentials/acme/work\n");
    assert!(!file.exists());
    let all = setup.fiber(&["logout", "acme", "--all"], "");
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
    for file in ["extension.json", "providers/codex.json", "init.lua"] {
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

/// Runs `fiber login codex --device`, reading stderr until `needle`
/// shows, then waiting for the exit under the deadline.
fn login_device(setup: &Setup, args: &[&str], needle: &str) -> Run {
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
    thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match err.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if chunks.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    let mut stderr = Vec::new();
    loop {
        let chunk = received
            .recv_timeout(setup.deadline.left())
            .unwrap_or_else(|_| {
                panic!("`fiber login codex --device` showed no device code within the deadline")
            });
        stderr.extend_from_slice(&chunk);
        if String::from_utf8_lossy(&stderr).contains(needle) {
            break;
        }
    }
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait_with_output()).unwrap());
    let output = match finished.recv_timeout(setup.deadline.left()) {
        Ok(output) => output.unwrap(),
        Err(_) => support::expired(
            setup.deadline,
            group,
            &finished,
            "`fiber login codex --device` to exit",
        ),
    };
    while let Ok(chunk) = received.try_recv() {
        stderr.extend_from_slice(&chunk);
    }
    assert!(
        !group_alive(setup.deadline, group),
        "`fiber` left a process behind"
    );
    std::mem::forget(guard);
    watchdog.stand_down(setup.deadline.cleanup());
    Run {
        code: output.status.code(),
        stdout: String::from_utf8(output.stdout).unwrap(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    }
}

#[test]
fn a_device_login_stores_the_email_label_then_logs_out() {
    let setup = Setup::new();
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

    let logout = setup.fiber(&["logout", "codex", "--as", "alice@example.com"], "");
    assert_eq!(logout.code, Some(0), "{}", logout.stderr);
    assert_eq!(
        logout.stderr,
        "fiber: removed credentials/codex/alice@example.com\n"
    );
    assert!(!file.exists());
}

#[test]
fn a_device_login_with_as_to_an_already_stored_label_contacts_nothing() {
    let setup = Setup::new();
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
    let login = setup.fiber(&["login", "codex", "--as", "x"], "");
    assert_eq!(login.code, Some(2), "{}", login.stderr);
    assert!(login.stderr.contains("--as"), "{}", login.stderr);
    assert_eq!(oauth.request_count(), 0);
    assert!(file.is_file());
}
