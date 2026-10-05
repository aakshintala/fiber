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
use std::time::Duration;

use fakes::Watchdog;
use rustix::pty;
use rustix::termios::{self, LocalModes};
use serde_json::{Value, json};

/// How long one `fiber` run, or one wait on its terminal, may take.
const DEADLINE: Duration = Duration::from_secs(20);

const KEY: &str = "sk-live-7f3a9c0d1e2b";

/// A temporary root holding Fiber home and the workspace, removed on drop.
struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        let root = fakes::TempDir::new("fl");
        fs::create_dir_all(root.path().join("h")).unwrap();
        fs::create_dir_all(root.path().join("w")).unwrap();
        Self { root }
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
            "models": [{"id": "m", "protocol": "openai-responses", "base_url": "http://127.0.0.1:9/v1"}],
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
        fs::write(
            dir.join("providers").join(format!("{name}.json")),
            data.to_string(),
        )
        .unwrap();
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
        // Taking the pipe closes it once written.
        let mut pipe = child.stdin.take().unwrap();
        pipe.write_all(input.as_bytes()).unwrap();
        drop(pipe);
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait_with_output()).unwrap());
        let output = match finished.recv_timeout(DEADLINE) {
            Ok(output) => output.unwrap(),
            Err(_) => {
                fakes::kill_group(group, "KILL").unwrap();
                let reaped = finished.recv_timeout(DEADLINE).is_ok();
                assert!(!group_alive(group), "`fiber` left a process behind");
                panic!(
                    "waited {DEADLINE:?} for `fiber {}` to exit (reaped after the kill: {reaped})",
                    args.join(" ")
                );
            }
        };
        assert!(!group_alive(group), "`fiber` left a process behind");
        std::mem::forget(guard);
        watchdog.stand_down(DEADLINE);
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
            screen: Screen::new(&terminal),
            terminal,
            child: Some(child),
            guard: Some(KillGroup(group)),
            watchdog: Some(watchdog),
            group,
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

/// Kills process group `group` on drop. After the child is reaped and the
/// group is empty, [`std::mem::forget`] skips that kill.
struct KillGroup(u32);

impl Drop for KillGroup {
    fn drop(&mut self) {
        // A panic between spawn and reap still kills the group. Failure
        // here is ignored: the process may already be gone.
        match fakes::kill_group(self.0, "KILL") {
            Ok(_) | Err(_) => {}
        }
    }
}

/// Whether any process remains in process group `group`.
fn group_alive(group: u32) -> bool {
    fakes::kill_group(group, "0").unwrap()
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
    fn wait_for_echo_off(&self) {
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
        if is_off.recv_timeout(DEADLINE).is_err() {
            stop.store(true, Ordering::Relaxed);
            panic!("waited {DEADLINE:?} for `fiber` to turn echo off");
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
    chunks: Receiver<Option<String>>,
    typed: fs::File,
    seen: String,
}

impl Screen {
    fn new(terminal: &Terminal) -> Self {
        let mut reader = fs::File::from(terminal.main.try_clone().unwrap());
        let (send, chunks) = mpsc::channel();
        thread::spawn(move || {
            let mut buffer = [0u8; 4096];
            // A read error is the end of the terminal, on Linux as well as
            // on macOS.
            while let Ok(n) = reader.read(&mut buffer) {
                if n == 0
                    || send
                        .send(Some(String::from_utf8_lossy(&buffer[..n]).into()))
                        .is_err()
                {
                    return;
                }
            }
            send.send(None).unwrap_or(());
        });
        Self {
            chunks,
            typed: fs::File::from(terminal.main.try_clone().unwrap()),
            seen: String::new(),
        }
    }

    /// Reads until `text` has appeared, and returns everything read since
    /// the last wait.
    fn wait_for(&mut self, text: &str) -> String {
        let mark = self.seen.len();
        while !self.seen[mark..].contains(text) {
            // Each chunk has the deadline: a terminal that goes quiet fails.
            match self.chunks.recv_timeout(DEADLINE) {
                Ok(Some(chunk)) => self.seen.push_str(&chunk),
                Ok(None) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    panic!(
                        "the terminal ended before {text:?}: {:?}",
                        &self.seen[mark..]
                    )
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    panic!(
                        "waited {DEADLINE:?} for {text:?} on the terminal: {:?}",
                        &self.seen[mark..]
                    )
                }
            }
        }
        self.seen[mark..].to_owned()
    }

    fn type_text(&mut self, text: &str) {
        self.typed.write_all(text.as_bytes()).unwrap();
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
}

impl OnTerminal {
    /// Waits for `fiber` to exit under [`DEADLINE`], and asserts that nothing
    /// it started is left in its group, after a timeout too.
    fn finish(&mut self) -> ExitStatus {
        let mut child = self.child.take().unwrap();
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait()).unwrap());
        let status = match finished.recv_timeout(DEADLINE) {
            Ok(status) => status.unwrap(),
            Err(_) => {
                fakes::kill_group(self.group, "KILL").unwrap();
                let reaped = finished.recv_timeout(DEADLINE).is_ok();
                assert!(!group_alive(self.group), "`fiber` left a process behind");
                panic!("waited {DEADLINE:?} for `fiber` to exit (reaped after the kill: {reaped})");
            }
        };
        assert!(!group_alive(self.group), "`fiber` left a process behind");
        std::mem::forget(self.guard.take());
        self.watchdog.take().unwrap().stand_down(DEADLINE);
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
        again.stderr.contains("run `fiber logout acme` first"),
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
    assert!(!setup.home().join("credentials/opencode").exists());
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
fn a_key_typed_on_a_terminal_is_not_echoed_and_echo_comes_back() {
    let setup = Setup::new();
    setup.provider("acme", None, None);
    let mut run = setup.on_terminal(&["login", "acme"]);
    run.screen.wait_for("Key for acme: ");
    run.terminal.wait_for_echo_off();
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
    let mut run = setup.on_terminal(&["login"]);
    let menu = run.screen.wait_for("Provider, by number or name: ");
    assert!(menu.contains("  1) acme\r\n  2) beta\r\n"), "{menu:?}");
    run.screen.type_text("2\n");
    run.screen.wait_for("Key for beta: ");
    run.terminal.wait_for_echo_off();
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
    run.terminal.wait_for_echo_off();
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
    run.terminal.wait_for_echo_off();
    // The guarded helper: it refuses a group of 1 or less.
    assert!(fakes::kill_group(run.group, "INT").unwrap());
    let status = run.finish();
    assert_eq!(status.signal(), Some(2), "{status:?}");
    assert!(run.terminal.echoes(), "echo was not restored");
    assert!(!setup.home().join("credentials/acme/default").exists());
}
