//! Binary-level tests of the terminal door (`docs/testing.md`, "Screens"):
//! the real binary in a pseudo-terminal: the first frame, a journey that
//! types a prompt, sees the answer and cancels a turn, an approval
//! answered from the panel, resize, and no tty.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test helpers; a failure is the test's; a live test prints its outcome"
)]

use std::ffi::OsStr;
use std::fs;
use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use fakes::{ProviderServer, Response, Watchdog};
use rustix::pty;
use serde_json::{Value, json};

/// One named wall-clock deadline for every blocking wait.
const DEADLINE: Duration = Duration::from_secs(30);

/// How long a process group may take to empty after `fiber` exits.
const GROUP_DEADLINE: Duration = Duration::from_secs(5);

/// How long the reap after a kill, and a watchdog's stand-down, may take:
/// the child is already dead then. With [`DEADLINE`] and [`GROUP_DEADLINE`]
/// the waits of one `fiber` run sum to 45 s at most, under half of nextest's
/// 120 s kill.
const REAP_DEADLINE: Duration = Duration::from_secs(10);

/// A temporary root holding Fiber home and the workspace, removed on drop.
struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        let root = fakes::TempDir::new("ft");
        fs::create_dir_all(root.path().join("h")).unwrap();
        fs::create_dir_all(root.path().join("w")).unwrap();
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("h")
    }

    fn workspace(&self) -> PathBuf {
        self.root.path().join("w")
    }

    /// Installs a provider `fake` with model `m` on `openai-responses` at the
    /// fake server, makes `fake/m` the configured model, and idles the hub
    /// out a second after its last client leaves, so no hub lingers.
    fn provider(&self, server: &ProviderServer) {
        let source = self.root.path().join("src");
        write(
            &source.join("extension.json"),
            &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}),
        );
        write(
            &source.join("providers/fake.json"),
            &json!({
                "name": "fake",
                "credential": {"env": "FIBER_TEST_FAKE_KEY"},
                "models": [{"id": "m", "protocol": "openai-responses", "base_url": format!("{}/v1", server.url()), "context_window": 100000}]
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
        write(
            &self.home().join("config.json"),
            &json!({"model": "fake/m", "hub": {"idle_exit_ms": 1000}}),
        );
    }
}

/// A pseudo-terminal at 60x12. The main side stays open while the run uses
/// the terminal side.
struct Terminal {
    main: OwnedFd,
    terminal: fs::File,
}

impl Terminal {
    fn open() -> Self {
        let main = pty::openpt(pty::OpenptFlags::RDWR | pty::OpenptFlags::NOCTTY).unwrap();
        // Not inherited: a hub `fiber` starts would hold the master open.
        rustix::io::fcntl_setfd(&main, rustix::io::FdFlags::CLOEXEC).unwrap();
        pty::grantpt(&main).unwrap();
        pty::unlockpt(&main).unwrap();
        let name = pty::ptsname(&main, Vec::new()).unwrap();
        let path = PathBuf::from(OsStr::from_bytes(name.as_bytes()));
        let terminal = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        rustix::termios::tcsetwinsize(
            &terminal,
            rustix::termios::Winsize {
                ws_col: 60,
                ws_row: 12,
                ws_xpixel: 0,
                ws_ypixel: 0,
            },
        )
        .unwrap();
        Self { main, terminal }
    }

    fn stdin(&self) -> Stdio {
        Stdio::from(self.terminal.try_clone().unwrap())
    }
}

/// Spawns `command` in a new process group, then a watchdog in its own
/// group. Dropping the watchdog kills the group; standing it down exits
/// quietly once the child is reaped and the group is empty.
fn spawn_watched(command: &mut Command) -> (Child, Watchdog) {
    let child = command.process_group(0).spawn().unwrap();
    let group = child.id();
    let guard = KillGroup(group);
    let watchdog = Watchdog::group(group);
    std::mem::forget(guard);
    (child, watchdog)
}

/// Kills process group `group` on drop.
struct KillGroup(u32);

impl Drop for KillGroup {
    fn drop(&mut self) {
        match fakes::kill_group(self.0, "KILL") {
            Ok(_) | Err(_) => {}
        }
    }
}

/// The terminal under test: its child, the pty master, and one reader
/// thread appending every master chunk to the output, waking a waiter
/// after each.
/// Sessions the hub starts run in their own process groups, guarded by
/// a matching watchdog on the workspace; the hub idles out on its own.
struct Run {
    child: Child,
    /// The hub's socket, removed when the hub exits.
    hub_socket: PathBuf,
    watchdog: Watchdog,
    #[allow(dead_code, reason = "held until the end to kill sessions on drop")]
    sessions: Watchdog,
    main: fs::File,
    /// One wake per chunk appended; held by one waiter at a time.
    wakes: Arc<Mutex<mpsc::Receiver<()>>>,
    output: Arc<Mutex<Vec<u8>>>,
    /// Where the last `read_until` match ended.
    seen: usize,
}

impl Run {
    /// Spawns `fiber` with no arguments on a pty: standard input, output
    /// and error all on the terminal side, as on a real terminal.
    fn terminal(setup: &Setup) -> Self {
        let terminal = Terminal::open();
        let sessions = Watchdog::matching(setup.workspace().to_str().unwrap());
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .current_dir(setup.workspace())
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", setup.root.path())
            .env("FIBER_HOME", setup.home())
            .env("FIBER_TEST_FAKE_KEY", "sk-test")
            .stdin(terminal.stdin())
            .stdout(terminal.stdin())
            .stderr(terminal.stdin());
        let (child, watchdog) = spawn_watched(&mut command);
        let main = fs::File::from(terminal.main);
        let (tx, rx) = mpsc::channel();
        let output = Arc::new(Mutex::new(Vec::new()));
        let appended = Arc::clone(&output);
        let mut dup = main.try_clone().unwrap();
        thread::Builder::new()
            .name("terminal-read".to_owned())
            .spawn(move || {
                let mut buf = [0u8; 4096];
                loop {
                    match dup.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            appended.lock().unwrap().extend_from_slice(&buf[..n]);
                            if tx.send(()).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            })
            .unwrap();
        Self {
            child,
            hub_socket: setup.home().join("run").join("hub"),
            watchdog,
            sessions,
            main,
            wakes: Arc::new(Mutex::new(rx)),
            output,
            seen: 0,
        }
    }

    /// Types bytes into the terminal.
    fn write(&mut self, bytes: &[u8]) {
        self.main.write_all(bytes).unwrap();
        self.main.flush().unwrap();
    }

    /// The output so far.
    fn output(&self) -> Vec<u8> {
        self.output.lock().unwrap().clone()
    }

    /// Reads until the output after the last match holds `needle`, under
    /// one named deadline for the whole wait, however much other output
    /// arrives. On expiry the panic names what it waited for and shows the
    /// output, so a stall says how far the journey got.
    fn read_until(&mut self, needle: &str) {
        let (output, wakes, seen) = (Arc::clone(&self.output), Arc::clone(&self.wakes), self.seen);
        let wanted = needle.as_bytes().to_vec();
        let (done, found) = mpsc::channel();
        thread::spawn(move || {
            let wakes = wakes.lock().unwrap();
            loop {
                let end = output.lock().unwrap()[seen..]
                    .windows(wanted.len())
                    .position(|window| window == wanted)
                    .map(|at| seen + at + wanted.len());
                if let Some(end) = end {
                    done.send(end).unwrap_or(());
                    return;
                }
                if wakes.recv().is_err() {
                    return;
                }
            }
        });
        let Ok(end) = found.recv_timeout(DEADLINE) else {
            let output = self.output();
            let output = String::from_utf8_lossy(&output);
            panic!("waited {DEADLINE:?} for {needle:?}; output: {output:?}");
        };
        self.seen = end;
    }

    /// Waits for the child to exit, reaps it, and returns its output, then
    /// for the hub to idle out and remove its socket: a hub whose home is
    /// deleted under it never exits. Dropping `self` kills the hub's
    /// sessions through the matching watchdog.
    fn wait(self) -> std::process::Output {
        // The reader thread holds only a dup of the master; dropping it
        // here does not close the child's terminal.
        let group = self.child.id();
        let guard = KillGroup(group);
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(self.child.wait_with_output()).unwrap());
        let output = match finished.recv_timeout(DEADLINE) {
            Ok(output) => output.unwrap(),
            Err(_) => {
                fakes::kill_group(group, "KILL").unwrap();
                let reaped = finished.recv_timeout(REAP_DEADLINE).is_ok();
                assert!(
                    fakes::group_empties(group, GROUP_DEADLINE),
                    "`fiber` left a process in its group behind"
                );
                panic!("waited {DEADLINE:?} for `fiber` to exit (reaped after the kill: {reaped})");
            }
        };
        assert!(
            fakes::group_empties(group, GROUP_DEADLINE),
            "`fiber` left a process in its group behind"
        );
        std::mem::forget(guard);
        self.watchdog.stand_down(REAP_DEADLINE);
        until_socket(&self.hub_socket, false, "the hub to idle out");
        output
    }
}

/// Waits under [`DEADLINE`] until `socket` exists or not, as `present`
/// says, naming `what` on expiry.
fn until_socket(socket: &Path, present: bool, what: &str) {
    let socket = socket.to_owned();
    let (done, reached) = mpsc::channel();
    thread::spawn(move || {
        while socket.exists() != present {
            thread::yield_now();
        }
        done.send(()).unwrap_or(());
    });
    assert!(
        reached.recv_timeout(DEADLINE).is_ok(),
        "waited {DEADLINE:?} for {what}"
    );
}

/// Whether `haystack` holds `needle` as bytes.
fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

/// "Hello." in two deltas.
fn hello() -> Response {
    let events = [
        json!({"type": "response.output_text.delta", "delta": "Hel"}),
        json!({"type": "response.output_text.delta", "delta": "lo."}),
        json!({"type": "response.output_item.done", "item": {
            "type": "message", "content": [{"type": "output_text", "text": "Hello."}]
        }}),
        json!({"type": "response.completed", "response": {
            "id": "resp_1", "status": "completed",
            "usage": {"input_tokens": 10, "input_tokens_details": {"cached_tokens": 4}, "output_tokens": 3}
        }}),
    ];
    let body: String = events
        .iter()
        .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
        .collect();
    Response::stream(body)
}

/// A reply that stalls mid-body: the turn stays running until cancelled.
fn stalled() -> Response {
    let prefix = "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"Working\"}\n\n";
    Response::stall(200, prefix, prefix.len() + 100000).header("content-type", "text/event-stream")
}

#[test]
fn typing_a_prompt_sees_the_answer_and_cancels_a_turn() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), stalled()]).unwrap();
    setup.provider(&server);
    let mut run = Run::terminal(&setup);
    // The first frame draws the input line before any byte is written to
    // the master: no detection reply goes in.
    run.read_until(">");
    // Enter goes out once the hub connects. Unchanged cells are never
    // rewritten, spaces included, so each wait matches one word.
    run.write(b"say hi\r");
    run.read_until("Hello.");
    run.read_until("completed");
    // The second prompt starts a stalled turn; Esc interrupts it.
    run.write(b"again\r");
    run.read_until("Working");
    run.write(b"\x1b");
    run.read_until("interrupted");
    run.write(b"\x03\x03");
    // After the last frame the output holds the alternate-screen leave and
    // the cursor shown.
    run.read_until("\x1b[?1049l");
    run.read_until("\x1b[?25h");
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}

/// A reply that calls the shell with `echo hi`.
fn calls_echo_hi() -> Response {
    let events = [
        json!({"type": "response.output_item.done", "item": {
            "type": "function_call", "id": "fc_call_1", "call_id": "call_1", "name": "shell",
            "arguments": json!({"command": "echo hi"}).to_string()
        }}),
        json!({"type": "response.completed", "response": {
            "id": "resp_1", "status": "completed",
            "usage": {"input_tokens": 10, "input_tokens_details": {"cached_tokens": 4}, "output_tokens": 3}
        }}),
    ];
    let body: String = events
        .iter()
        .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
        .collect();
    Response::stream(body)
}

#[test]
fn a_standing_ask_opens_the_approval_panel_and_allow_once_runs_the_call() {
    let setup = Setup::new();
    let server = ProviderServer::start([calls_echo_hi(), hello()]).unwrap();
    setup.provider(&server);
    // A standing ask for this exact command: with the terminal connected
    // the loop asks a person (`docs/permissions.md`, "Headless").
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "ask", "tool": "shell", "prefix": "echo hi"})
        ),
    )
    .unwrap();
    let mut run = Run::terminal(&setup);
    run.read_until(">");
    run.write(b"run it\r");
    run.read_until("asked by a global rule: echo hi");
    run.read_until("allow once");
    // Enter on the first choice allows once; the call runs and the turn
    // finishes with the answer.
    run.write(b"\r");
    run.read_until("Hello.");
    run.read_until("completed");
    run.write(b"\x03\x03");
    run.read_until("\x1b[?1049l");
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
    // The model got the call's output, not a denial.
    let requests = server.requests();
    let second: Value = serde_json::from_slice(&requests[1].body).unwrap();
    let result = second["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "function_call_output")
        .unwrap();
    assert_eq!(result["output"], "hi\nExit code 0.\n");
}

#[test]
fn resize_redraws_the_input_line_on_the_new_last_row() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let mut run = Run::terminal(&setup);
    run.read_until(">");
    let marked = run.output().len();
    rustix::termios::tcsetwinsize(
        &run.main,
        rustix::termios::Winsize {
            ws_col: 40,
            ws_row: 10,
            ws_xpixel: 0,
            ws_ypixel: 0,
        },
    )
    .unwrap();
    fakes::kill_pid(run.child.id(), "WINCH").unwrap();
    // The next frame draws the input line on row 10; rows 11 and 12 are
    // never addressed again.
    run.read_until("\x1b[10;1H");
    let output = run.output();
    let fresh = &output[marked..];
    assert!(!contains(fresh, "\x1b[11;1H"));
    assert!(!contains(fresh, "\x1b[12;1H"));
    // The hub `fiber` started is up before the quit, so `wait` sees it
    // idle out rather than start after the home is gone.
    until_socket(&run.hub_socket, true, "the hub to start");
    run.write(b"\x03\x03");
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn without_a_tty_bare_fiber_names_ask() {
    assert_names_ask(Stdio::piped(), "standard input and output");
}

#[test]
fn a_tty_on_standard_input_alone_is_not_enough() {
    let terminal = Terminal::open();
    assert_names_ask(terminal.stdin(), "standard output");
}

/// Runs bare `fiber` with `stdin` and standard output and error piped:
/// with no tty on `missing`, it exits 2 naming `fiber ask`.
fn assert_names_ask(stdin: Stdio, missing: &str) {
    let setup = Setup::new();
    let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
    command
        .current_dir(setup.workspace())
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", setup.root.path())
        .env("FIBER_HOME", setup.home())
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let (child, watchdog) = spawn_watched(&mut command);
    let group = child.id();
    let guard = KillGroup(group);
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait_with_output()).unwrap());
    let output = finished
        .recv_timeout(DEADLINE)
        .unwrap_or_else(|_| panic!("waited {DEADLINE:?} for `fiber` to exit, no tty on {missing}"))
        .unwrap();
    assert!(
        fakes::group_empties(group, GROUP_DEADLINE),
        "waited {GROUP_DEADLINE:?} for the process group to empty"
    );
    std::mem::forget(guard);
    watchdog.stand_down(REAP_DEADLINE);
    assert_eq!(output.status.code(), Some(2), "no tty on {missing}");
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "fiber: The terminal needs a tty; run `fiber ask \"<prompt>\"`. Run `fiber --help` for usage.\n"
    );
}

fn write(file: &Path, value: &Value) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, value.to_string()).unwrap();
}
