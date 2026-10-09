//! Binary-level tests of the terminal door (`docs/testing.md`, "Screens"):
//! the real binary in a pseudo-terminal: the first frame, a journey that
//! types a prompt, sees the answer and cancels a turn, an approval
//! answered from the panel, a repository's offer answered from its view,
//! resize, and no tty.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test helpers; a failure is the test's; a live test prints its outcome"
)]

mod support;

use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use fakes::{ProviderServer, Response, Watchdog};
use rustix::pty;
use serde_json::{Value, json};
use support::Deadline;

/// A temporary root holding Fiber home and the workspace, removed on drop.
struct Setup {
    root: fakes::TempDir,
    deadline: Deadline,
}

impl Setup {
    fn new() -> Self {
        Self::within(Deadline::start())
    }

    fn within(deadline: Deadline) -> Self {
        let root = fakes::TempDir::new("ft");
        fs::create_dir_all(root.path().join("h")).unwrap();
        fs::create_dir_all(root.path().join("w")).unwrap();
        Self { deadline, root }
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
        self.provider_with(&json!({}), server);
    }

    /// [`Setup::provider`], with the fake model declaring `input`
    /// `["text", "image"]`, so a pasted image is sent as an image part.
    fn provider_with_images(&self, server: &ProviderServer) {
        self.provider_with(&json!({"input": ["text", "image"]}), server);
    }

    fn provider_with(&self, model_extra: &Value, server: &ProviderServer) {
        let source = self.root.path().join("src");
        write(
            &source.join("extension.json"),
            &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}),
        );
        let mut model = json!({"id": "m", "protocol": "openai-responses",
            "base_url": format!("{}/v1", server.url()), "context_window": 100000});
        for (key, value) in model_extra.as_object().unwrap() {
            model[key] = value.clone();
        }
        write(
            &source.join("providers/fake.json"),
            &json!({
                "name": "fake",
                "credential": {"env": "FIBER_TEST_FAKE_KEY"},
                "models": [model]
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

    /// The project's sessions directory, through the canonical workspace,
    /// as the project key names it.
    fn sessions(&self) -> PathBuf {
        let workspace = fs::canonicalize(self.workspace()).unwrap();
        let key = workspace.to_string_lossy().replace('/', "-");
        self.home().join("projects").join(key).join("sessions")
    }

    /// The one session in [`Setup::sessions`].
    fn only_session(&self) -> String {
        let mut ids: Vec<String> = fs::read_dir(self.sessions())
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(ids.len(), 1, "one session");
        ids.pop().unwrap()
    }

    /// Runs `fiber` with `args` headless in its own process group,
    /// waiting under the test's [`Deadline`]. A watchdog beside it kills
    /// that group if this process dies first.
    fn fiber(&self, args: &[&str]) -> std::process::Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(args)
            .current_dir(self.workspace())
            .env_clear()
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
            fakes::group_empties(group, self.deadline.left()),
            "`fiber` left a process in its group behind"
        );
        std::mem::forget(guard);
        watchdog.stand_down(self.deadline.cleanup());
        output
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
        support::kill_group_detached(self.0, "KILL");
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
    deadline: Deadline,
}

impl Run {
    /// Spawns `fiber` with no arguments on a pty: standard input, output
    /// and error all on the terminal side, as on a real terminal.
    fn terminal(setup: &Setup) -> Self {
        Self::terminal_full(setup, &[], &[])
    }

    /// Spawns `fiber` as [`terminal`] does, with `env` added to the
    /// child's environment.
    fn terminal_with(setup: &Setup, env: &[(&str, &str)]) -> Self {
        Self::terminal_full(setup, env, &[])
    }

    /// Spawns `fiber` as [`terminal`] does, with `args` after the binary.
    fn terminal_args(setup: &Setup, args: &[&str]) -> Self {
        Self::terminal_full(setup, &[], args)
    }

    /// Spawns `fiber` as [`terminal`] does, with `env` added to the
    /// child's environment and `args` after the binary.
    fn terminal_full(setup: &Setup, env: &[(&str, &str)], args: &[&str]) -> Self {
        let terminal = Terminal::open();
        let sessions = Watchdog::matching(setup.workspace().to_str().unwrap());
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .current_dir(setup.workspace())
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", setup.root.path())
            .env("FIBER_HOME", setup.home())
            .env("FIBER_TEST_FAKE_KEY", "sk-test");
        for (key, value) in env {
            command.env(key, value);
        }
        command.args(args);
        command
            .stdin(terminal.stdin())
            .stdout(terminal.stdin())
            .stderr(terminal.stdin());
        let (child, watchdog) = spawn_watched(&mut command);
        let main = fs::File::from(terminal.main);
        let (tx, rx) = mpsc::channel();
        let output = Arc::new(Mutex::new(Vec::new()));
        let appended = Arc::clone(&output);
        let dup = main.try_clone().unwrap();
        fakes::pty::read_to_eof(dup, move |bytes| {
            appended.lock().unwrap().extend_from_slice(bytes);
            tx.send(()).unwrap_or(());
        });
        Self {
            child,
            hub_socket: setup.home().join("run").join("hub"),
            watchdog,
            sessions,
            main,
            wakes: Arc::new(Mutex::new(rx)),
            output,
            seen: 0,
            deadline: setup.deadline,
        }
    }

    /// Types bytes into the terminal, on a thread bounded by the test's
    /// [`Deadline`].
    fn write(&mut self, bytes: &[u8]) {
        let mut main = self.main.try_clone().unwrap();
        let bytes = bytes.to_vec();
        support::bounded(self.deadline, "typing on the terminal", move || {
            main.write_all(&bytes)?;
            main.flush()
        })
        .unwrap();
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
        let Ok(end) = found.recv_timeout(self.deadline.left()) else {
            let output = self.output();
            let output = String::from_utf8_lossy(&output);
            panic!("waited until the deadline for {needle:?}; output: {output:?}");
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
        let output = match finished.recv_timeout(self.deadline.left()) {
            Ok(output) => output.unwrap(),
            Err(_) => support::expired(self.deadline, group, &finished, "`fiber` to exit"),
        };
        assert!(
            fakes::group_empties(group, self.deadline.left()),
            "`fiber` left a process in its group behind"
        );
        std::mem::forget(guard);
        self.watchdog.stand_down(self.deadline.cleanup());
        until_socket(
            self.deadline,
            &self.hub_socket,
            false,
            "the hub to idle out",
        );
        output
    }
}

/// Waits under the test's [`Deadline`] until `socket` exists or not, as
/// `present` says, naming `what` on expiry.
fn until_socket(deadline: Deadline, socket: &Path, present: bool, what: &str) {
    let socket = socket.to_owned();
    let (done, reached) = mpsc::channel();
    thread::spawn(move || {
        while socket.exists() != present {
            thread::yield_now();
        }
        done.send(()).unwrap_or(());
    });
    assert!(
        reached.recv_timeout(deadline.left()).is_ok(),
        "waited until the deadline for {what}"
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

/// A reply saying `text` in two deltas.
fn reply(text: &str) -> Response {
    let at = text.len() / 2;
    let (first, rest) = text.split_at(at);
    let events = [
        json!({"type": "response.output_text.delta", "delta": first}),
        json!({"type": "response.output_text.delta", "delta": rest}),
        json!({"type": "response.output_item.done", "item": {
            "type": "message", "content": [{"type": "output_text", "text": text}]
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
    // The reply streams in two deltas, so only the first delta's text
    // arrives whole; the turn's close says it finished.
    run.read_until("Hel");
    run.read_until("completed");
    // The second prompt starts a stalled turn; Esc interrupts it.
    run.write(b"again\r");
    run.read_until("Working");
    // The elapsed count proves the working line draws: a count whose
    // width changes rewrites its cells whole, while unchanged cells are
    // never rewritten, so no other word of the line arrives whole.
    run.read_until("10s");
    run.write(b"\x1b");
    run.read_until("interrupted");
    run.write(b"\x03\x03\r");
    // The turn just ended, so its idle status may still be on its way: the
    // quit either exits at once or asks first (`docs/tui.md`, "Quit"). The
    // Enter goes out with the Ctrl+C bytes, so it is processed after them:
    // it leaves working sessions running, and when the terminal already
    // exited it is never read.
    // After the last frame the output holds the alternate-screen leave and
    // the cursor shown, then one resume line per live session ("On exit").
    run.read_until("\x1b[?25h");
    run.read_until("fiber resume");
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}

/// A finished turn sends an OSC 9 desktop notification where the
/// terminal supports one.
#[test]
fn a_finished_turn_sends_an_osc_9_notification() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let mut run = Run::terminal_with(&setup, &[("TERM_PROGRAM", "ghostty")]);
    run.read_until(">");
    run.write(b"say hi\r");
    // The reply streams in two deltas, so only the first delta's text
    // arrives whole; the turn's close says it finished.
    run.read_until("Hel");
    run.read_until("\x1b]9;Fiber: ");
    run.write(b"\x03\x03\r");
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
    // The reply streams in two deltas, so only the first delta's text
    // arrives whole; the turn's close says it finished.
    run.read_until("Hel");
    run.read_until("completed");
    // As above: the turn just ended, so quitting either exits at once or
    // asks first. The Enter leaves the session running, and exiting prints
    // its resume line ("Quit", "On exit").
    run.write(b"\x03\x03\r");
    run.read_until("fiber resume");
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
fn a_repository_offer_swaps_in_and_approve_lets_the_turn_run() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    // The workspace's repository declares one MCP server nobody approved.
    write(
        &setup.workspace().join(".fiber/config.json"),
        &json!({"mcp": {"servers": {"db": {"command": "/bin/echo"}}}}),
    );
    let mut run = Run::terminal(&setup);
    run.read_until(">");
    run.write(b"say hi\r");
    // One word: the 60-column terminal wraps the TUI-files line, and
    // unchanged cells are never rewritten, so a phrase can arrive split.
    run.read_until("installed");
    // From skip, ← chooses approve; ↓ moves to Send, and Enter sends.
    run.write(b"\x1b[D");
    run.write(b"\x1b[B");
    run.write(b"\r");
    // The turn runs only once the offer resolves, so the answer shows the
    // reply was accepted and the session counted this terminal first.
    // The reply streams in two deltas, so only the first delta's text
    // arrives whole; the turn's close says it finished.
    run.read_until("Hel");
    run.read_until("completed");
    // As above: quitting either exits at once or asks first.
    run.write(b"\x03\x03\r");
    run.read_until("fiber resume");
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn resize_redraws_the_input_line_on_the_new_last_row() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let mut run = Run::terminal(&setup);
    run.read_until(">");
    // The first frame paints every cell on the theme's background, so it
    // is read through its last row before the resize.
    run.read_until("\x1b[12;1H");
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
    support::kill_pid(setup.deadline, run.child.id(), "WINCH").unwrap();
    // The next frame draws the input line on row 10; rows 11 and 12 are
    // never addressed again.
    run.read_until("\x1b[10;1H");
    let output = run.output();
    let fresh = &output[marked..];
    assert!(!contains(fresh, "\x1b[11;1H"));
    assert!(!contains(fresh, "\x1b[12;1H"));
    // The hub `fiber` started is up before the quit, so `wait` sees it
    // idle out rather than start after the home is gone.
    until_socket(setup.deadline, &run.hub_socket, true, "the hub to start");
    run.write(b"\x03\x03");
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn without_a_tty_bare_fiber_names_ask() {
    let deadline = Deadline::start();
    assert_names_ask(deadline, Stdio::piped(), "standard input and output", &[]);
}

#[test]
fn a_tty_on_standard_input_alone_is_not_enough() {
    let deadline = Deadline::start();
    let terminal = Terminal::open();
    assert_names_ask(deadline, terminal.stdin(), "standard output", &[]);
}

#[test]
fn resume_and_continue_without_a_tty_name_ask() {
    let deadline = Deadline::start();
    for args in [&["resume", "s_1"][..], &["resume"][..], &["continue"][..]] {
        assert_names_ask(deadline, Stdio::piped(), "standard input and output", args);
        let terminal = Terminal::open();
        assert_names_ask(deadline, terminal.stdin(), "standard output", args);
    }
}

#[test]
fn resume_opens_the_session_a_prefix_names() {
    let setup = Setup::new();
    let server = ProviderServer::start([reply("Hello.")]).unwrap();
    setup.provider(&server);
    let asked = setup.fiber(&["ask", "say hi"]);
    assert_eq!(asked.status.code(), Some(0));
    let id = setup.only_session();
    let mut run = Run::terminal_args(&setup, &["resume", &id[..4]]);
    run.read_until(">");
    run.read_until("Hello.");
    run.write(b"\x03\x03\r");
    run.read_until("\x1b[?25h");
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn continue_opens_the_latest_session() {
    let setup = Setup::new();
    let server = ProviderServer::start([reply("First."), reply("Second.")]).unwrap();
    setup.provider(&server);
    assert_eq!(setup.fiber(&["ask", "first"]).status.code(), Some(0));
    assert_eq!(setup.fiber(&["ask", "second"]).status.code(), Some(0));
    let mut run = Run::terminal_args(&setup, &["continue"]);
    run.read_until(">");
    run.read_until("Second.");
    run.write(b"\x03\x03\r");
    run.read_until("\x1b[?25h");
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn resume_without_an_id_opens_home_at_the_session_list() {
    let setup = Setup::new();
    let server = ProviderServer::start([reply("Hello.")]).unwrap();
    setup.provider(&server);
    let asked = setup.fiber(&["ask", "say hi"]);
    assert_eq!(asked.status.code(), Some(0));
    let mut run = Run::terminal_args(&setup, &["resume"]);
    run.read_until(">");
    // The exited session is listed by its first prompt.
    run.read_until("say hi");
    // The list is focused, so Enter opens the row: the reply shows.
    run.write(b"\r");
    run.read_until("Hello.");
    run.write(b"\x03\x03\r");
    run.read_until("\x1b[?25h");
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn continue_with_no_session_is_a_usage_error() {
    let setup = Setup::new();
    let mut run = Run::terminal_args(&setup, &["continue"]);
    run.read_until("No session in this project to continue");
    let output = run.wait();
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn resume_with_an_unknown_prefix_fails_before_any_frame() {
    let setup = Setup::new();
    let mut run = Run::terminal_args(&setup, &["resume", "s_zzz"]);
    run.read_until("no session at");
    let output = run.wait();
    assert_eq!(output.status.code(), Some(1));
}

/// Runs `fiber` with `args`, `stdin` and standard output and error
/// piped: with no tty on `missing`, it exits 2 naming `fiber ask`.
fn assert_names_ask(deadline: Deadline, stdin: Stdio, missing: &str, args: &[&str]) {
    let setup = Setup::within(deadline);
    let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
    command
        .args(args)
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
    let output = match finished.recv_timeout(setup.deadline.left()) {
        Ok(output) => output.unwrap(),
        Err(_) => support::expired(
            setup.deadline,
            group,
            &finished,
            &format!("`fiber` to exit, no tty on {missing}"),
        ),
    };
    assert!(
        fakes::group_empties(group, setup.deadline.left()),
        "waited until the deadline for the process group to empty"
    );
    std::mem::forget(guard);
    watchdog.stand_down(setup.deadline.cleanup());
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

/// A 1x1 PNG, 69 bytes: within every cap, as `session_command.rs` holds
/// it. The fake clipboard program prints these bytes; the session stores
/// them byte for byte.
const PIXEL: [u8; 69] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0x00,
    0x00, 0x03, 0x01, 0x01, 0x00, 0xc9, 0xfe, 0x92, 0xef, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e,
    0x44, 0xae, 0x42, 0x60, 0x82,
];

/// Waits under the setup's deadline for exactly one `artifacts/i_*.png`
/// under the session directory, equal byte for byte to [`PIXEL`].
fn stored_pixel(setup: &Setup) {
    let sessions = log::sessions_dir(&setup.home(), &doors::project(&setup.workspace()));
    loop {
        let mut found = Vec::new();
        if let Ok(entries) = fs::read_dir(&sessions) {
            for entry in entries.flatten() {
                if let Ok(files) = fs::read_dir(entry.path().join("artifacts")) {
                    found.extend(files.flatten().map(|file| file.path()).filter(|path| {
                        path.file_name()
                            .and_then(OsStr::to_str)
                            .is_some_and(|name| name.starts_with("i_") && name.ends_with(".png"))
                    }));
                }
            }
        }
        if found.len() == 1 && fs::read(&found[0]).unwrap_or_default() == PIXEL {
            return;
        }
        if setup.deadline.left().is_zero() {
            panic!("waited until the deadline for one stored pixel; found: {found:?}");
        }
        thread::yield_now();
    }
}

#[test]
fn ctrl_v_pastes_an_image_that_the_session_stores() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider_with_images(&server);
    // A fake clipboard program first on the child's PATH, printing the
    // pixel in its real program's form.
    let bin = setup.root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let (name, body) = if cfg!(target_os = "macos") {
        let hex: String = PIXEL.iter().map(|byte| format!("{byte:02X}")).collect();
        (
            "osascript",
            format!("printf '\\302\\253data PNGf{hex}\\302\\273\\n'"),
        )
    } else {
        let octal: String = PIXEL.iter().map(|byte| format!("\\{byte:03o}")).collect();
        ("wl-paste", format!("printf '{octal}'"))
    };
    fakes::script(&bin, name, &body);
    let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .unwrap();
    let mut env: Vec<(&str, &str)> = vec![("PATH", path.to_str().unwrap())];
    if !cfg!(target_os = "macos") {
        env.push(("WAYLAND_DISPLAY", "fiber-test"));
    }
    let mut run = Run::terminal_with(&setup, &env);
    run.read_until(">");
    run.write(&[0x16]);
    run.read_until("[Image #1]");
    run.write(b"\r");
    // The reply streams in two deltas, so only the first delta's text
    // arrives whole; quitting needs the turn finished, which the close says.
    run.read_until("completed");
    stored_pixel(&setup);
    run.write(b"\x03\x03\r");
    run.read_until("\x1b[?25h");
    run.read_until("fiber resume");
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn resume_draws_the_reply_then_its_closed_turn() {
    let setup = Setup::new();
    let server = ProviderServer::start([reply("marker reply")]).unwrap();
    setup.provider(&server);
    let asked = setup.fiber(&["ask", "say hi"]);
    assert_eq!(asked.status.code(), Some(0));
    let id = setup.only_session();
    let mut run = Run::terminal_args(&setup, &["resume", &id[..4]]);
    run.read_until(">");
    // The turn ended before the attach: the reply draws, then the turn's
    // close, which folds only once `turn_completed` arrives.
    run.read_until("marker reply");
    run.read_until("▣ completed");
    run.write(b"\x03\x03\r");
    run.read_until("\x1b[?25h");
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}
