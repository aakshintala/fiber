//! Binary-level tests of `fiber ask` (`docs/testing.md`, "Levels"): the
//! built `fiber` runs in its own process group with its own `FIBER_HOME`,
//! holding an ordinary provider whose base URL is the fake server.

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
use std::io::{BufRead, Write};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fakes::{ProviderServer, Request, Response, Watchdog, fingerprint};
use rustix::pty;
use serde_json::{Value, json};

#[path = "../../provider/tests/support/probes.rs"]
mod probes;

/// How long one `fiber` run may take.
const DEADLINE: Duration = Duration::from_secs(20);

/// Set on the re-exec of [`sleep_stands_in_for_fiber`]. Unset, that test
/// returns without spawning anything.
const WATCHDOG_STAND_IN_ENV: &str = "FIBER_WATCHDOG_STAND_IN";

/// A temporary root holding Fiber home and the workspace, removed on drop.
/// Its name is short: a session's socket path must fit in 103 bytes on
/// macOS.
struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        let root = fakes::TempDir::new("fa");
        fs::create_dir_all(root.path().join("h")).unwrap();
        fs::create_dir_all(root.path().join("w")).unwrap();
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("h")
    }

    /// Installs a provider `fake` with model `m` on `openai-responses` at the
    /// fake server, and makes `fake/m` the configured model.
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
                "models": [{"id": "m", "protocol": "openai-responses", "base_url": format!("{}/v1", server.url())}]
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
            &json!({"model": "fake/m"}),
        );
    }

    /// Disables retries for the next run: `retry.attempts` 0, so a
    /// retryable failure fails at once, with no backoff to sleep through.
    fn no_retry(&self) {
        write(
            &self.home().join("config.json"),
            &json!({"model": "fake/m", "retry": {"attempts": 0}}),
        );
    }

    /// Runs `fiber` with `args`, `stdin` piped in (closed when `None`) and
    /// `FIBER_HOME` set to `home`.
    fn fiber_with_home(&self, home: &str, args: &[&str], stdin: Option<&str>) -> Run {
        self.run(home, args, Stdio::piped(), stdin, &[])
    }

    /// Runs `fiber` with `args` and the extra environment `env`.
    fn fiber_with_env(&self, args: &[&str], env: &[(&str, &str)]) -> Run {
        let home = self.home();
        self.run(home.to_str().unwrap(), args, Stdio::piped(), None, env)
    }

    /// Copies the first-party package `providers/<name>` with every base
    /// URL's origin `origin` replaced by `url`, and returns the copy.
    fn package(&self, name: &str, origin: &str, url: &str) -> PathBuf {
        let from = package(name);
        let to = self.root.path().join(format!("pkg-{name}"));
        let mut files = vec!["extension.json".to_owned()];
        for entry in fs::read_dir(from.join("providers")).unwrap() {
            let file = entry.unwrap().file_name();
            files.push(format!("providers/{}", file.to_str().unwrap()));
        }
        for file in files {
            let text = fs::read_to_string(from.join(&file)).unwrap();
            fs::create_dir_all(to.join(&file).parent().unwrap()).unwrap();
            fs::write(to.join(&file), text.replace(origin, url)).unwrap();
        }
        to
    }

    /// Runs `fiber` with `args` and its stdin on a pseudo-terminal.
    fn fiber_on_terminal(&self, args: &[&str]) -> Run {
        self.fiber_typing(args, "")
    }

    /// Runs `fiber` with `args`, its stdin on a pseudo-terminal where
    /// `typed` was already typed.
    fn fiber_typing(&self, args: &[&str], typed: &str) -> Run {
        self.fiber_typing_env(args, typed, &[])
    }

    /// [`Self::fiber_typing`] with extra environment variables.
    fn fiber_typing_env(&self, args: &[&str], typed: &str, env: &[(&str, &str)]) -> Run {
        let terminal = Terminal::open();
        fs::File::from(terminal.main.try_clone().unwrap())
            .write_all(typed.as_bytes())
            .unwrap();
        let home = self.home();
        self.run(home.to_str().unwrap(), args, terminal.stdin(), None, env)
    }

    /// Runs `fiber` in its own process group, waits for it under
    /// [`DEADLINE`], and asserts that nothing it started is left in the
    /// group, after a timeout too (`docs/testing.md`, "Running tests").
    /// A watchdog beside it kills that group if this process dies first.
    fn run(
        &self,
        home: &str,
        args: &[&str],
        stdin: Stdio,
        text: Option<&str>,
        env: &[(&str, &str)],
    ) -> Run {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(args)
            .current_dir(self.root.path().join("w"))
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path())
            .env("FIBER_HOME", home)
            .env("FIBER_TEST_FAKE_KEY", "sk-test")
            .envs(env.iter().copied())
            .stdin(stdin)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (mut child, watchdog) = spawn_watched(&mut command);
        let group = child.id();
        let guard = KillGroup(group);
        // Taking the pipe closes it once written.
        if let Some(mut pipe) = child.stdin.take()
            && let Some(text) = text
        {
            pipe.write_all(text.as_bytes()).unwrap();
        }
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait_with_output()).unwrap());
        let output = match finished.recv_timeout(DEADLINE) {
            Ok(output) => output.unwrap(),
            Err(_) => {
                fakes::kill_group(group, "KILL").unwrap();
                // Reaps the killed child, so the check below sees the group
                // as the kill left it.
                let reaped = finished.recv_timeout(DEADLINE).is_ok();
                assert!(
                    !group_alive(group),
                    "`fiber` left a process in its group behind"
                );
                panic!(
                    "waited {DEADLINE:?} for `fiber {}` to exit (reaped after the kill: {reaped})",
                    args.join(" ")
                );
            }
        };
        assert!(
            !group_alive(group),
            "`fiber` left a process in its group behind"
        );
        // The group is empty. Skip the drop, which would kill it again,
        // and tell the watchdog to exit without signalling.
        std::mem::forget(guard);
        watchdog.stand_down(DEADLINE);
        Run::from(output)
    }

    fn fiber(&self, args: &[&str], stdin: Option<&str>) -> Run {
        self.fiber_with_home(self.home().to_str().unwrap(), args, stdin)
    }

    /// `stdin` is the child's standard input as given. A write end the caller
    /// still holds stays open until this returns, so an unread pipe is open
    /// when `fiber` exits.
    fn fiber_with_stdio(&self, args: &[&str], stdin: Stdio) -> Run {
        self.run(self.home().to_str().unwrap(), args, stdin, None, &[])
    }
}

/// The recorded credential header equals the fingerprint of `value`, the
/// bytes that package sends (`docs/testing.md`, "Model calls").
fn assert_fingerprint(request: &Request, header: &str, value: &str) {
    assert_eq!(
        request.header(header),
        Some(fingerprint(value).as_str()),
        "{header}"
    );
}

fn write(file: &Path, value: &Value) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, value.to_string()).unwrap();
}

/// Whether any process remains in process group `group`.
fn group_alive(group: u32) -> bool {
    fakes::kill_group(group, "0").unwrap()
}

/// A pseudo-terminal, opened through rustix's safe calls. The main side
/// stays open while the run uses the terminal side.
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

    fn stdin(&self) -> Stdio {
        Stdio::from(self.terminal.try_clone().unwrap())
    }
}

/// Spawns `command` in a new process group, then a watchdog in its own
/// group. The watchdog's stdin is a pipe only this process holds: a newline
/// means the child is reaped, and EOF means this process died, so the
/// watchdog kills the group. The watchdog is started immediately after the
/// child; a kill in the gap between the two spawns can still orphan it.
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

#[test]
fn dropping_the_group_guard_kills_the_group() {
    let mut child = Command::new("sleep")
        .arg("30")
        .process_group(0)
        .spawn()
        .unwrap();
    let group = child.id();
    drop(KillGroup(group));
    // On Linux `kill -0` still succeeds for a killed zombie until it is
    // reaped, so the group is checked after `wait`.
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait()).unwrap());
    let status = match finished.recv_timeout(DEADLINE) {
        Ok(status) => status.unwrap(),
        Err(_) => panic!("waited {DEADLINE:?} for the process group to die"),
    };
    assert_eq!(status.signal(), Some(9));
    assert!(!group_alive(group));
}

/// Run by [`a_killed_test_kills_the_stand_in_group`]: `sleep` through
/// [`spawn_watched`], stdout inherited so the stand-in holds the pipe the
/// parent reads. Prints its group and waits. On its own it does nothing.
#[test]
#[allow(clippy::print_stdout, reason = "the parent test reads this line")]
fn sleep_stands_in_for_fiber() {
    let Ok(_) = std::env::var(WATCHDOG_STAND_IN_ENV) else {
        return;
    };
    let mut command = Command::new("sleep");
    command.arg("60").stdout(Stdio::inherit());
    let (child, _watchdog) = spawn_watched(&mut command);
    println!("group {}", child.id());
    std::io::stdout().flush().unwrap();
    let mut rest = String::new();
    std::io::stdin().read_line(&mut rest).unwrap();
}

/// SIGKILL of the test process is EOF to the watchdog, which kills the
/// stand-in's group. EOF on the inherited stdout arrives only once that
/// group is gone (`docs/testing.md`, "Running tests").
#[test]
fn a_killed_test_kills_the_stand_in_group() {
    let mut helper = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "sleep_stands_in_for_fiber", "--nocapture"])
        .env(WATCHDOG_STAND_IN_ENV, "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = helper.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = std::io::BufReader::new(stdout);
        // libtest prints its own lines before the helper's.
        let group = (&mut reader)
            .lines()
            .map_while(Result::ok)
            .find_map(|line| line.strip_prefix("group ")?.parse().ok());
        send_group(&tx, group);
        // EOF comes once every process holding the pipe has exited.
        match std::io::copy(&mut reader, &mut std::io::sink()) {
            Ok(_) | Err(_) => {}
        }
        send_group(&tx, None);
    });
    let group = match rx.recv_timeout(DEADLINE) {
        Ok(Some(group)) => group,
        Ok(None) => panic!("the stand-in exited before printing its group"),
        Err(_) => panic!("waited {DEADLINE:?} for the stand-in to print its group"),
    };
    // Dropping this kills the group if the test fails before the watchdog does.
    let _guard = KillGroup(group);
    match helper.kill() {
        Ok(()) | Err(_) => {}
    }
    let (done, finished) = mpsc::channel();
    thread::spawn(move || match done.send(helper.wait()) {
        Ok(()) | Err(mpsc::SendError(_)) => {}
    });
    let status = match finished.recv_timeout(DEADLINE) {
        Ok(status) => status.unwrap(),
        Err(_) => panic!("waited {DEADLINE:?} for the killed test process to exit"),
    };
    assert_eq!(status.signal(), Some(9));
    match rx.recv_timeout(DEADLINE) {
        Ok(None) => {}
        Ok(Some(_)) | Err(_) => {
            panic!("waited {DEADLINE:?} for stand-in group {group} to die")
        }
    }
}

fn send_group(tx: &mpsc::Sender<Option<u32>>, message: Option<u32>) {
    match tx.send(message) {
        Ok(()) | Err(mpsc::SendError(_)) => {}
    }
}

/// One finished run: its exit code, stdout's lines, as text and parsed, and
/// stderr.
struct Run {
    code: Option<i32>,
    stdout: String,
    raw: Vec<String>,
    lines: Vec<Value>,
    stderr: String,
}

/// A `session_status` line: ephemeral, and written by an observer thread, so
/// where it falls among the loop's own lines is not what these tests pin.
/// `tests/socket.rs` reads it.
fn is_status(line: &str) -> bool {
    line.contains(r#""kind":"session_status""#)
}

impl From<Output> for Run {
    fn from(output: Output) -> Self {
        let stdout = String::from_utf8(output.stdout).unwrap();
        let raw: Vec<String> = stdout
            .lines()
            .filter(|l| !is_status(l))
            .map(str::to_owned)
            .collect();
        let lines = raw
            .iter()
            // `fiber list` prints text; a `kind` lookup on it fails the test.
            .map(|l| serde_json::from_str(l).unwrap_or(Value::Null))
            .collect();
        Self {
            code: output.status.code(),
            stdout,
            raw,
            lines,
            stderr: String::from_utf8(output.stderr).unwrap(),
        }
    }
}

impl Run {
    fn kinds(&self) -> Vec<&str> {
        self.lines
            .iter()
            .map(|l| l["kind"].as_str().unwrap())
            .collect()
    }

    fn last(&self) -> &Value {
        self.lines.last().expect("stdout has a line")
    }

    fn session_id(&self) -> &str {
        self.lines[0]["session_id"].as_str().unwrap()
    }

    /// The session's directory, from its id.
    fn session_dir(&self, setup: &Setup) -> PathBuf {
        let workspace = fs::canonicalize(setup.root.path().join("w")).unwrap();
        let key = workspace.to_string_lossy().replace('/', "-");
        setup
            .home()
            .join("projects")
            .join(key)
            .join("sessions")
            .join(self.session_id())
    }
}

/// An `openai-responses` stream answering `Hello.` in two fragments.
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

/// An Anthropic Messages stream answering `Hello.` in two fragments.
fn anthropic_hello() -> Response {
    let events = [
        json!({"type": "message_start", "message": {
            "id": "msg_1", "usage": {"input_tokens": 10, "output_tokens": 0}
        }}),
        json!({"type": "content_block_start", "index": 0,
            "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 0,
            "delta": {"type": "text_delta", "text": "Hel"}}),
        json!({"type": "content_block_delta", "index": 0,
            "delta": {"type": "text_delta", "text": "lo."}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "message_delta",
            "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 3}}),
        json!({"type": "message_stop"}),
    ];
    let body: String = events
        .iter()
        .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
        .collect();
    Response::stream(body)
}

/// A Gemini `streamGenerateContent` stream answering `Hello.` in two parts.
fn gemini_hello() -> Response {
    let chunks = [
        json!({"responseId": "resp_1", "candidates": [{"content": {
            "parts": [{"text": "Hel"}], "role": "model"}}]}),
        json!({"responseId": "resp_1", "candidates": [{"content": {
            "parts": [{"text": "lo."}], "role": "model"},
            "finishReason": "STOP"}],
            "usageMetadata": {"promptTokenCount": 10, "candidatesTokenCount": 3}}),
    ];
    let body: String = chunks.iter().map(|c| format!("data: {c}\n\n")).collect();
    Response::stream(body)
}

/// An `openai-completions` stream answering `Hello.` in two fragments,
/// its usage chunk carrying `cost`: OpenRouter's inline figure.
fn completions_hello(id: &str, cost: Value) -> Response {
    let chunks = [
        json!({"id": id, "object": "chat.completion.chunk", "choices": [
            {"index": 0, "delta": {"role": "assistant", "content": "Hel"}}]}),
        json!({"id": id, "object": "chat.completion.chunk", "choices": [
            {"index": 0, "delta": {"content": "lo."}, "finish_reason": "stop"}]}),
        json!({"id": id, "choices": [], "usage": {"prompt_tokens": 15,
            "completion_tokens": 9, "total_tokens": 24, "cost": cost,
            "prompt_tokens_details": {"cached_tokens": 14}}}),
    ];
    let mut body: String = chunks
        .iter()
        .map(|chunk| format!("data: {chunk}\n\n"))
        .collect();
    body.push_str("data: [DONE]\n\n");
    Response::stream(body)
}

/// The event kinds of a turn answered by [`hello`].
const HELLO_KINDS: [&str; 15] = [
    "session_started",
    "fiber_started",
    "extensions_loaded",
    "preamble_built",
    "opening_message",
    "turn_started",
    "step_started",
    "assistant_message_started",
    "assistant_message_delta",
    "assistant_message_delta",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "fiber_exited",
];

/// The text of the first message in the session's `turn_started`.
fn turn_input(run: &Run) -> &str {
    let started = run
        .lines
        .iter()
        .find(|l| l["kind"] == "turn_started")
        .unwrap();
    started["payload"]["input"][0]["content"][0]["text"]
        .as_str()
        .unwrap()
}

#[test]
fn a_prompt_as_an_argument_runs_one_turn_and_stdout_is_the_log() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);

    let run = setup.fiber(&["ask", "hi"], None);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), HELLO_KINDS);
    assert_eq!(run.stderr, "");
    assert_eq!(run.lines[1]["payload"]["resumed"], false);
    assert_eq!(turn_input(&run), "hi");
    let exited = &run.last()["payload"];
    assert_eq!(exited["exit_code"], 0);
    assert_eq!(exited["text"], "Hello.");
    let message = run
        .lines
        .iter()
        .find(|l| l["kind"] == "assistant_message_completed")
        .unwrap();
    assert_eq!(exited["final_action_id"], message["action_id"]);
    assert_eq!(exited["usage"]["tokens"]["output"], 3);
    assert_eq!(exited.get("error"), None);

    // Stdout filtered to this session's durable lines is the log, byte for
    // byte.
    let dir = run.session_dir(&setup);
    let durable: String = run
        .raw
        .iter()
        .zip(&run.lines)
        .filter(|(_, l)| l.get("seq").is_some() && l["session_id"] == run.session_id())
        .map(|(raw, _)| format!("{raw}\n"))
        .collect();
    assert_eq!(
        fs::read_to_string(dir.join("events.jsonl")).unwrap(),
        durable
    );

    // The socket was bound in `run/` and is gone after exit.
    assert!(setup.home().join("run").is_dir());
    assert!(!setup.home().join("run").join(run.session_id()).exists());

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/v1/responses");
    assert_fingerprint(&requests[0], "authorization", "Bearer sk-test");
    assert!(String::from_utf8_lossy(&requests[0].body).contains("\"hi\""));
}

/// Copies the repository's `docs/skills/` into Fiber home's `docs/skills/`,
/// read at run time so the test checks what ships.
fn install_docs_skills(home: &Path) {
    let from = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/skills");
    for entry in fs::read_dir(&from).unwrap() {
        let entry = entry.unwrap();
        let dir = home.join("docs/skills").join(entry.file_name());
        fs::create_dir_all(&dir).unwrap();
        fs::copy(entry.path().join("SKILL.md"), dir.join("SKILL.md")).unwrap();
    }
}

fn opening_skills(run: &Run) -> Vec<(String, String, String)> {
    let opening = run
        .lines
        .iter()
        .find(|l| l["kind"] == "opening_message")
        .unwrap();
    opening["payload"]["skills"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["name"].as_str().unwrap().to_owned(),
                s["path"].as_str().unwrap().to_owned(),
                s["source"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

#[test]
fn the_opening_message_lists_the_built_in_skills_from_home_docs() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    install_docs_skills(&setup.home());

    let run = setup.fiber(&["ask", "hi"], None);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), HELLO_KINDS);
    let home = setup.home();
    // The listing names the canonical directory, past macOS's `/var`
    // symlink.
    let docs = home.join("docs/skills").canonicalize().unwrap();
    assert_eq!(
        opening_skills(&run),
        [
            (
                "cache-warming".to_owned(),
                docs.join("cache-warming/SKILL.md").display().to_string(),
                "builtin".to_owned(),
            ),
            (
                "using-fiber".to_owned(),
                docs.join("using-fiber/SKILL.md").display().to_string(),
                "builtin".to_owned(),
            ),
        ]
    );

    fs::remove_dir_all(home.join("docs")).unwrap();
    let run = setup.fiber(&["ask", "hi"], None);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), HELLO_KINDS);
    assert!(opening_skills(&run).is_empty());
}

#[test]
fn a_prompt_on_stdin_runs_one_turn() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);

    let run = setup.fiber(&["ask"], Some("review the brief\n"));

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), HELLO_KINDS);
    assert_eq!(turn_input(&run), "review the brief\n");
    assert_eq!(run.last()["payload"]["text"], "Hello.");
}

#[test]
fn a_failed_turn_exits_1_with_the_turns_error() {
    let setup = Setup::new();
    let server = ProviderServer::start([Response::status(503, "{}")]).unwrap();
    setup.provider(&server);
    setup.no_retry();

    let run = setup.fiber(&["ask", "hi"], None);

    assert_eq!(run.code, Some(1));
    assert_eq!(
        run.kinds(),
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
    );
    let turn = &run.lines[8]["payload"];
    assert_eq!(turn["outcome"], "failed");
    let exited = &run.last()["payload"];
    assert_eq!(exited["exit_code"], 1);
    assert_eq!(exited["error"], turn["error"]);
    assert_eq!(exited["error"]["code"], "provider_unavailable");
    assert_eq!(exited.get("text"), None);
    let message = exited["error"]["message"].as_str().unwrap();
    assert_eq!(run.stderr, format!("fiber: {message}\n"));
    assert!(run.session_dir(&setup).join("events.jsonl").is_file());
}

/// The one line stdout holds when the process failed before any session.
fn assert_pre_session(run: &Run, exit: i32, code: &str) {
    assert_eq!(run.code, Some(exit));
    assert_eq!(run.kinds(), ["fiber_exited"]);
    let line = run.last();
    assert_eq!(line.get("session_id"), None);
    assert_eq!(line["payload"]["exit_code"], exit);
    assert_eq!(line["payload"]["error"]["code"], code);
    let message = line["payload"]["error"]["message"].as_str().unwrap();
    assert_eq!(run.stderr, format!("fiber: {message}\n"));
}

#[test]
fn two_prompts_or_none_is_a_usage_error() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);

    assert_pre_session(&setup.fiber(&["ask"], None), 2, "usage");
    assert_pre_session(&setup.fiber(&["ask", "a", "b"], None), 2, "usage");
    assert_pre_session(&setup.fiber(&["ask", "--model", "x"], None), 2, "usage");
    assert_pre_session(&setup.fiber(&["ask", "--verbose"], None), 2, "usage");
    assert_pre_session(
        &setup.fiber(&["ask", "x", "fake/m", "hi"], None),
        2,
        "usage",
    );
    let run = setup.fiber(&["ask", "--model"], None);
    assert_pre_session(&run, 2, "usage");
    assert!(
        run.stderr
            .contains("A value is required for '--model <model>' but none was supplied."),
        "{}",
        run.stderr
    );
    assert!(server.requests().is_empty());
    assert!(!setup.home().join("projects").exists());
}

#[test]
fn a_failure_before_any_session_ends_stdout_with_fiber_exited_and_no_session_id() {
    let setup = Setup::new();

    assert_pre_session(&setup.fiber(&["ask", "hi"], None), 1, "no_model");
    assert_pre_session(&setup.fiber_with_home("", &["ask", "hi"], None), 2, "usage");
    assert_pre_session(
        &setup.fiber_with_home("rel", &["ask", "hi"], None),
        2,
        "usage",
    );
    assert!(!setup.home().join("projects").exists());
}

#[test]
fn a_missing_credential_fails_before_the_session() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    let source = setup.home().join("extensions/fake/providers/fake.json");
    let text = fs::read_to_string(&source)
        .unwrap()
        .replace("FIBER_TEST_FAKE_KEY", "FIBER_TEST_UNSET_KEY");
    fs::write(&source, text).unwrap();

    assert_pre_session(&setup.fiber(&["ask", "hi"], None), 1, "credential_missing");
}

#[test]
fn a_bedrock_converse_model_fails_before_the_session() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    let source = setup.root.path().join("src");
    write(
        &source.join("extension.json"),
        &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}),
    );
    write(
        &source.join("providers/fake.json"),
        &json!({
            "name": "fake",
            "credential": {"env": "FIBER_TEST_FAKE_KEY"},
            "models": [{"id": "m", "protocol": "bedrock-converse", "base_url": format!("{}/v1", server.url())}]
        }),
    );
    extensions::plan(
        &setup.home(),
        &extensions::Request::Path(source),
        "0.0.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    write(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m"}),
    );

    assert_pre_session(
        &setup.fiber_with_env(&["ask", "hi"], &[("FIBER_TEST_FAKE_KEY", "key")]),
        1,
        "protocol_unsupported",
    );
    assert!(server.requests().is_empty());
}

#[test]
fn fiber_without_ask_is_a_usage_error_naming_fiber_ask() {
    let setup = Setup::new();

    let bare = setup.fiber(&[], None);
    assert_eq!(bare.code, Some(2));
    assert!(bare.stderr.contains("fiber ask"), "stderr: {}", bare.stderr);
    assert!(bare.lines.is_empty());

    let unknown = setup.fiber(&["hi"], None);
    assert_eq!(unknown.code, Some(2));
    assert!(unknown.lines.is_empty());
    assert!(
        unknown.stderr.contains("Unrecognized subcommand 'hi'"),
        "stderr: {}",
        unknown.stderr
    );
}

/// The menu clap prints, including the trailing newline it appends.
const MENU: &str = r#"Fiber, a coding agent.

Usage: fiber <command> [arguments]

Sessions:
  ask [--model <model>] [--resume <id>] [<prompt>] [-]  Run one session of one turn; its events go to stdout
  sessions export <id> [<path>]                         Write the session's log and its artifacts to <path>
  models [<search>] [--json]                            List the models the installed providers serve

Fiber itself:
  login [<provider>] [--as <label>]         Store a provider's key
  logout <provider> [--as <label> | --all]  Delete a provider's stored key
  help [<command>]                          Print this menu, or a command's help
  version                                   Print the version

Extensions:
  extension install <name or path>  Install an extension and its dependencies
  extension update [<name>]         Update one extension, or every installed extension, to its newest tag
  extension remove <name>           Remove an extension, the dependencies nothing else uses, and their data
  extension list                    List installed extensions: name, version and commit
  approve [--yes]                   Show what this repository ships and approve it

Configuration:
  config get <key>                              Print the effective value and the layer it came from
  config set [--project | --repo] <key> <value>  Write one key in one layer's file

Flags:
  -h, --help     Print this menu
  -v, --version  Print the version

Examples:
  fiber ask "review the diff on this branch"
  fiber ask < brief.md
  git diff | fiber ask "review this diff" -
  fiber extension install openrouter
  fiber login openrouter
  fiber help ask
"#;

fn printed(run: &Run) -> String {
    if run.raw.is_empty() {
        String::new()
    } else {
        format!("{}\n", run.raw.join("\n"))
    }
}

fn assert_help(run: &Run, about: &str) {
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.stderr, "");
    let text = printed(run);
    assert!(text.contains(about), "{text}");
    assert!(!text.contains("fiber_exited"), "{text}");
}

#[test]
fn help_prints_the_menu() {
    let setup = Setup::new();
    for args in [&["-h"][..], &["--help"], &["help"]] {
        let run = setup.fiber(args, None);
        assert_eq!(run.code, Some(0), "{args:?} stderr: {}", run.stderr);
        assert_eq!(run.stderr, "");
        assert_eq!(printed(&run), MENU, "{args:?}");
    }
}

#[test]
fn ask_help_prints_no_event_stream() {
    let setup = Setup::new();
    for args in [&["ask", "--help"][..], &["ask", "-h"], &["help", "ask"]] {
        let run = setup.fiber(args, None);
        assert_help(&run, "Run one session of one turn; its events go to stdout");
    }
}

#[test]
fn help_for_every_command_matches_the_flag() {
    let setup = Setup::new();
    for name in ["ask", "extension", "version", "help"] {
        let via_help = setup.fiber(&["help", name], None);
        let via_flag = setup.fiber(&[name, "--help"], None);
        assert_eq!(via_help.code, Some(0), "{name}: {}", via_help.stderr);
        assert_eq!(via_flag.code, Some(0), "{name}: {}", via_flag.stderr);
        assert_eq!(via_help.stderr, "");
        assert_eq!(via_flag.stderr, "");
        assert_eq!(via_help.stdout, via_flag.stdout, "{name}");
        assert!(
            via_flag
                .stdout
                .lines()
                .any(|line| line.starts_with(&format!("Usage: fiber {name}"))),
            "{name}: {}",
            via_flag.stdout
        );
    }
    for (noun, verb) in [
        ("sessions", "export"),
        ("extension", "install"),
        ("extension", "update"),
        ("extension", "remove"),
        ("extension", "list"),
        ("config", "get"),
        ("config", "set"),
    ] {
        let via_help = setup.fiber(&["help", noun, verb], None);
        let via_flag = setup.fiber(&[noun, verb, "--help"], None);
        assert_eq!(via_help.code, Some(0), "{noun} {verb}: {}", via_help.stderr);
        assert_eq!(via_flag.code, Some(0), "{noun} {verb}: {}", via_flag.stderr);
        assert_eq!(via_help.stderr, "");
        assert_eq!(via_flag.stderr, "");
        assert_eq!(via_help.stdout, via_flag.stdout, "{noun} {verb}");
        assert!(
            via_flag
                .stdout
                .lines()
                .any(|line| line.starts_with(&format!("Usage: fiber {noun} {verb}"))),
            "{noun} {verb}: {}",
            via_flag.stdout
        );
    }
}

#[test]
fn each_command_prints_its_own_help() {
    let setup = Setup::new();
    let commands = [
        (
            "extension install",
            "Install an extension and its dependencies",
        ),
        (
            "extension update",
            "Update one extension, or every installed extension, to its newest tag",
        ),
        (
            "extension remove",
            "Remove an extension, the dependencies nothing else uses, and their data",
        ),
        (
            "extension list",
            "List installed extensions: name, version and commit",
        ),
        ("version", "Print the version"),
        ("help", "Print this menu, or a command's help"),
    ];
    for (name, about) in commands {
        if name.starts_with("extension ") {
            let verb = name.strip_prefix("extension ").unwrap();
            assert_help(&setup.fiber(&["extension", verb, "--help"], None), about);
        } else {
            for args in [vec![name, "--help"], vec!["help", name]] {
                assert_help(&setup.fiber(&args, None), about);
            }
        }
    }
}

#[test]
fn version_prints_the_package_version() {
    let setup = Setup::new();
    let line = match option_env!("FIBER_COMMIT") {
        Some(commit) => {
            assert!(
                commit.len() >= 4
                    && commit
                        .bytes()
                        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')),
                "{commit}"
            );
            format!("fiber 0.0.0 ({commit})\n")
        }
        None => "fiber 0.0.0\n".to_owned(),
    };
    for args in [&["-v"][..], &["--version"], &["version"]] {
        let run = setup.fiber(args, None);
        assert_eq!(run.code, Some(0), "{args:?} stderr: {}", run.stderr);
        assert_eq!(run.stderr, "");
        assert_eq!(printed(&run), line, "{args:?}");
    }
}

#[test]
fn a_capital_v_is_an_unknown_argument() {
    let setup = Setup::new();
    let run = setup.fiber(&["-V"], None);
    assert_eq!(run.code, Some(2));
    assert!(run.lines.is_empty());
    assert_eq!(
        run.stderr,
        "fiber: Unexpected argument '-V' found. Run `fiber --help` for usage.\n"
    );
}

#[test]
fn an_unknown_subcommand_suggests_the_nearest() {
    let setup = Setup::new();
    let run = setup.fiber(&["extension", "i"], None);
    assert_eq!(run.code, Some(2));
    assert!(run.lines.is_empty());
    assert_eq!(
        run.stderr,
        "fiber: Unrecognized subcommand 'i'; did you mean 'install'? Run `fiber --help` for usage.\n"
    );
}

#[test]
fn an_ask_parse_error_is_one_exited_line() {
    let setup = Setup::new();
    let run = setup.fiber(&["ask", "--modle", "x", "hi"], None);
    assert_pre_session(&run, 2, "usage");
    assert_eq!(
        run.stderr,
        "fiber: Unexpected argument '--modle' found; did you mean '--model'? Run `fiber --help` for usage.\n"
    );
}

#[test]
fn ask_with_the_wrong_shape_is_one_exited_line() {
    let setup = Setup::new();
    let sentence = "fiber: `fiber ask` takes one prompt, then an optional `-`; quote the prompt. Run `fiber --help` for usage.\n";
    for args in [&["ask", "a", "b"][..], &["ask", "-", "a"]] {
        let run = setup.fiber(args, None);
        assert_pre_session(&run, 2, "usage");
        assert_eq!(run.stderr, sentence, "{args:?}");
    }
}

#[test]
fn bare_fiber_names_ask_and_prints_nothing() {
    let setup = Setup::new();
    let run = setup.fiber(&[], None);
    assert_eq!(run.code, Some(2));
    assert!(run.lines.is_empty());
    assert_eq!(
        run.stderr,
        "fiber: The terminal door is not built; run `fiber ask \"<prompt>\"`. Run `fiber --help` for usage.\n"
    );
}

#[test]
fn a_prompt_argument_does_not_wait_on_an_open_stdin() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let (reader, writer) = std::io::pipe().unwrap();
    let run = setup.fiber_with_stdio(&["ask", "hi"], Stdio::from(reader));
    drop(writer);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), HELLO_KINDS);
    assert_eq!(turn_input(&run), "hi");
}

#[test]
fn a_prompt_argument_leaves_stdin_unread() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let run = setup.fiber(&["ask", "hi"], Some("and this"));
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), HELLO_KINDS);
    assert_eq!(turn_input(&run), "hi");
}

#[test]
fn a_prompt_and_a_dash_append_stdin() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let run = setup.fiber(&["ask", "hi", "-"], Some("more"));
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), HELLO_KINDS);
    assert_eq!(turn_input(&run), "hi\nmore");
}

#[test]
fn a_dash_reads_stdin_as_the_prompt() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let run = setup.fiber(&["ask", "-"], Some("brief"));
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), HELLO_KINDS);
    assert_eq!(turn_input(&run), "brief");
}

#[test]
fn help_does_not_read_home_or_a_provider() {
    let setup = Setup::new();
    let menu = setup.fiber_with_home("", &["--help"], None);
    assert_eq!(menu.code, Some(0), "stderr: {}", menu.stderr);
    assert_eq!(menu.stderr, "");
    assert_eq!(printed(&menu), MENU);

    let ask = setup.fiber(&["ask", "--help"], None);
    assert_help(&ask, "Run one session of one turn; its events go to stdout");
}

#[test]
fn no_prompt_with_stdin_on_a_terminal_is_a_usage_error() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);

    let run = setup.fiber_on_terminal(&["ask"]);

    assert_pre_session(&run, 2, "usage");
    assert!(server.requests().is_empty());
}

#[test]
fn a_prompt_argument_with_stdin_on_a_terminal_runs_without_reading_it() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);

    let run = setup.fiber_on_terminal(&["ask", "hi"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), HELLO_KINDS);
    assert_eq!(turn_input(&run), "hi");
    assert_eq!(run.last()["payload"]["text"], "Hello.");
}

/// A first-party provider package in the repository's `providers/`.
fn package(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../providers")
        .join(name)
}

/// The OpenCode Go tool exchange with `muse-spark-1.3-contributor`: the
/// recorded tool call, then the probe's answer after the tool result.
fn go_exchange() -> [Response; 2] {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let call = root.join("crates/provider/tests/recordings/opencode-go-tool-call.sse");
    let answer = root.join("research/opencode-probe/raw/go_stream_tools_1.sse");
    [
        Response::stream(fs::read(call).unwrap()),
        Response::stream(fs::read(answer).unwrap()),
    ]
}

/// Installs the package at `path` with `fiber extension install`, stdin not a
/// terminal, so it does not ask.
fn install(setup: &Setup, path: &Path) {
    let run = setup.fiber(&["extension", "install", path.to_str().unwrap()], None);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(!run.stderr.contains("Go ahead?"), "{}", run.stderr);
}

#[test]
fn install_in_a_terminal_shows_the_providers_and_their_urls_and_asks() {
    let setup = Setup::new();
    let opencode = package("opencode");
    let path = opencode.to_str().unwrap();
    let installed = setup
        .home()
        .join("extensions/github.com-aakshintala-fiber-providers-opencode");

    let declined = setup.fiber_typing(&["extension", "install", path], "n\n");
    assert_eq!(declined.code, Some(1), "stderr: {}", declined.stderr);
    assert!(!installed.exists());

    let run = setup.fiber_typing(&["extension", "install", path], "y\n");
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(
        run.stderr,
        format!(
            "Install github.com/aakshintala/fiber/providers/opencode from {}\n\
             Version 0.0.0\n\
             Provider opencode-go: https://opencode.ai/zen/go/v1\n\
             Provider opencode-zen: https://opencode.ai/zen/v1\n\
             Go ahead? [y/N/s to show the full source] fiber: installed github.com/aakshintala/fiber/providers/opencode\n",
            fs::canonicalize(&opencode).unwrap().display()
        )
    );
    assert!(installed.join("providers/opencode-go.json").is_file());
    assert!(installed.join("providers/opencode-zen.json").is_file());
}

#[test]
fn list_and_remove_show_and_delete_what_an_install_put_in_home() {
    let setup = Setup::new();
    let muse = package("muse");
    install(&setup, &muse);
    let name = "github.com/aakshintala/fiber/providers/muse";
    let listed = setup.fiber(&["extension", "list"], None);
    assert_eq!(listed.code, Some(0), "stderr: {}", listed.stderr);
    assert_eq!(listed.raw, [format!("{name} 0.0.0 local")]);
    let updated = setup.fiber(&["extension", "update", "muse"], None);
    assert_eq!(updated.code, Some(0), "stderr: {}", updated.stderr);
    assert_eq!(updated.stderr, format!("fiber: installed {name}\n"));
    let removed = setup.fiber(&["extension", "remove", "muse"], None);
    assert_eq!(removed.code, Some(0), "stderr: {}", removed.stderr);
    assert_eq!(removed.stderr, format!("fiber: removed {name}\n"));
    assert!(setup.fiber(&["extension", "list"], None).raw.is_empty());
    let again = setup.fiber(&["extension", "remove", "muse"], None);
    assert_eq!(again.code, Some(1));
    assert!(
        again.stderr.contains("is not installed"),
        "{}",
        again.stderr
    );
}

#[test]
fn install_by_name_without_git_fails_as_usage_and_says_to_install_it() {
    let setup = Setup::new();
    let run = setup.fiber_with_env(&["extension", "install", "openrouter"], &[("PATH", "")]);
    assert_eq!(run.code, Some(2), "{}", run.stderr);
    assert!(run.stderr.contains("Install git"), "{}", run.stderr);
}

#[test]
fn the_extension_commands_take_their_arguments() {
    let setup = Setup::new();
    for args in [
        vec!["extension", "install"],
        vec!["extension", "install", "a", "b"],
        vec!["extension", "remove"],
        vec!["extension", "list", "x"],
        vec!["extension", "update", "a", "b"],
    ] {
        let run = setup.fiber(&args, None);
        assert_eq!(run.code, Some(2), "{args:?}: {}", run.stderr);
    }
}

#[test]
fn extension_update_all_updates_every_requested_extension() {
    let setup = Setup::new();
    install(&setup, &package("muse"));
    install(&setup, &package("opencode"));
    let run = setup.fiber(&["extension", "update"], None);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert!(run.raw.is_empty());
    assert!(
        run.stderr
            .contains("fiber: installed github.com/aakshintala/fiber/providers/muse")
    );
    assert!(
        run.stderr
            .contains("fiber: installed github.com/aakshintala/fiber/providers/opencode")
    );
}

#[test]
fn extension_update_all_on_an_empty_home_exits_zero() {
    let setup = Setup::new();
    let run = setup.fiber(&["extension", "update"], None);
    assert_eq!(run.code, Some(0));
    assert_eq!(run.stderr, "");
    assert!(run.raw.is_empty());
}

#[test]
fn extension_update_all_skips_unrequested_dependencies() {
    let setup = Setup::new();
    let gh = Github::new(&setup);
    gh.release("v0.1.0");
    gh.release_needs_muse("v1.0.0", "v0.1.0");
    let muse_v1 = git(&gh.repo, &["rev-parse", "v0.1.0^{commit}"]);
    let run = setup.fiber_with_env(&["extension", "install", NEEDS_MUSE], &gh.env());
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let listed = setup.fiber_with_env(&["extension", "list"], &[]);
    assert!(
        listed
            .raw
            .iter()
            .any(|line| line.contains(&format!("{MUSE} v0.1.0 {muse_v1}"))),
        "{:?}",
        listed.raw
    );
    gh.release("v0.2.0");
    gh.release_needs_muse("v1.0.1", "v0.1.0");
    let run = setup.fiber_with_env(&["extension", "update"], &gh.env());
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let listed = setup.fiber_with_env(&["extension", "list"], &[]);
    assert!(
        listed
            .raw
            .iter()
            .any(|line| line.contains(&format!("{MUSE} v0.1.0 {muse_v1}"))),
        "muse must stay on v0.1.0 when update-all skips unrequested extensions: {:?}",
        listed.raw
    );
    assert!(
        listed
            .raw
            .iter()
            .any(|line| line.contains("needs-muse") && line.contains("v1.0.1")),
        "{:?}",
        listed.raw
    );
    // muse is still a dependency only: removing what needed it removes it.
    let removed = setup.fiber(&["extension", "remove", NEEDS_MUSE], None);
    assert_eq!(removed.code, Some(0), "{}", removed.stderr);
    assert_eq!(
        removed.stderr,
        format!("fiber: removed {NEEDS_MUSE}\nfiber: removed {MUSE}\n")
    );
    assert!(setup.fiber(&["extension", "list"], None).raw.is_empty());
}

#[test]
fn extension_update_all_stops_at_the_first_failure() {
    let setup = Setup::new();
    let muse = setup.package("muse", "", "");
    install(&setup, &package("opencode"));
    install(&setup, &muse);
    fs::remove_dir_all(&muse).unwrap();
    let run = setup.fiber(&["extension", "update"], None);
    assert_ne!(run.code, Some(0), "{}", run.stderr);
    assert!(
        !run.stderr
            .contains("installed github.com/aakshintala/fiber/providers/opencode"),
        "{}",
        run.stderr
    );
}

#[test]
fn old_extension_command_names_are_unknown() {
    let setup = Setup::new();
    for args in [
        &["install", "x"][..],
        &["update"][..],
        &["remove", "x"][..],
        &["list"][..],
    ] {
        let run = setup.fiber(args, None);
        assert_eq!(run.code, Some(2), "{args:?}: {}", run.stderr);
        assert!(run.raw.is_empty());
        let lines: Vec<_> = run.stderr.lines().collect();
        assert_eq!(lines.len(), 1, "{args:?}: {:?}", lines);
        assert!(
            lines[0].starts_with("fiber: Unrecognized subcommand"),
            "{}",
            lines[0]
        );
    }
}

/// Runs the system `git` in `dir`.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
        .args(["-c", "init.defaultBranch=main"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// Fiber's own repository as a local one, with `providers/muse` committed
/// and tagged `tag`, and the environment that makes `https://github.com/`
/// mean the directory holding it. This is git's own `url.<base>.insteadOf`,
/// so the shipped binary needs no test switch.
struct Github {
    repo: PathBuf,
    key: String,
    value: String,
}

impl Github {
    fn new(setup: &Setup) -> Self {
        let base = setup.root.path().join("gh");
        let repo = base.join("aakshintala/fiber");
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "--quiet"]);
        Self {
            repo,
            key: format!("url.file://{}/.insteadOf", base.display()),
            value: "https://github.com/".into(),
        }
    }

    fn env(&self) -> [(&str, &str); 3] {
        [
            ("GIT_CONFIG_COUNT", "1"),
            ("GIT_CONFIG_KEY_0", &self.key),
            ("GIT_CONFIG_VALUE_0", &self.value),
        ]
    }

    /// Commits `providers/muse` with a note file and tags it.
    fn release(&self, tag: &str) {
        let dir = self.repo.join("providers/muse");
        fs::create_dir_all(dir.join("providers")).unwrap();
        for file in ["extension.json", "providers/muse.json"] {
            fs::copy(package("muse").join(file), dir.join(file)).unwrap();
        }
        fs::write(dir.join("NOTES.md"), format!("notes for {tag}\n")).unwrap();
        git(&self.repo, &["add", "."]);
        git(&self.repo, &["commit", "--quiet", "-m", tag]);
        git(&self.repo, &["tag", tag]);
    }

    /// Commits `providers/needs-muse`, which depends on [`MUSE`] at `muse_min`.
    fn release_needs_muse(&self, tag: &str, muse_min: &str) {
        let dir = self.repo.join("providers/needs-muse");
        fs::create_dir_all(dir.join("providers")).unwrap();
        let manifest = format!(
            r#"{{"name":"{NEEDS_MUSE}","version":"0.0.0","fiber":"0.0.0","api":1,"depends":{{"{MUSE}":"{muse_min}"}}}}"#
        );
        fs::write(dir.join("extension.json"), manifest).unwrap();
        fs::write(dir.join("providers/.gitkeep"), "").unwrap();
        fs::write(dir.join("NOTES.md"), format!("needs-muse {tag}\n")).unwrap();
        git(&self.repo, &["add", "."]);
        git(&self.repo, &["commit", "--quiet", "-m", tag]);
        git(&self.repo, &["tag", tag]);
    }
}

const MUSE: &str = "github.com/aakshintala/fiber/providers/muse";
const NEEDS_MUSE: &str = "github.com/aakshintala/fiber/providers/needs-muse";

#[test]
fn install_by_short_name_fetches_from_git_headless_and_list_shows_the_commit() {
    let setup = Setup::new();
    let gh = Github::new(&setup);
    gh.release("v0.1.0");
    let commit = git(&gh.repo, &["rev-parse", "v0.1.0^{commit}"]);
    let run = setup.fiber_with_env(&["extension", "install", "muse"], &gh.env());
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.stderr, format!("fiber: installed {MUSE}\n"));
    let listed = setup.fiber_with_env(&["extension", "list"], &[]);
    assert_eq!(listed.raw, [format!("{MUSE} v0.1.0 {commit}")]);
    assert!(
        setup
            .home()
            .join("extensions/github.com-aakshintala-fiber-providers-muse/providers/muse.json")
            .is_file()
    );
}

#[test]
fn install_by_name_in_a_terminal_shows_the_version_asks_and_can_show_the_source() {
    let setup = Setup::new();
    let gh = Github::new(&setup);
    gh.release("v0.1.0");
    let installed = setup
        .home()
        .join("extensions/github.com-aakshintala-fiber-providers-muse");
    let declined = setup.fiber_typing_env(&["extension", "install", "muse"], "n\n", &gh.env());
    assert_eq!(declined.code, Some(1), "stderr: {}", declined.stderr);
    assert!(
        declined.stderr.contains("Version v0.1.0\n"),
        "{}",
        declined.stderr
    );
    assert!(!installed.exists());
    let run = setup.fiber_typing_env(&["extension", "install", "muse"], "s\ny\n", &gh.env());
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.stderr.matches("Go ahead?").count(), 2, "{}", run.stderr);
    assert!(run.stderr.contains("--- NOTES.md"), "{}", run.stderr);
    assert!(run.stderr.contains("notes for v0.1.0"), "{}", run.stderr);
    assert!(installed.join("NOTES.md").is_file());
}

#[test]
fn update_moves_to_the_newest_tag_and_a_terminal_shows_what_changed() {
    let setup = Setup::new();
    let gh = Github::new(&setup);
    gh.release("v0.1.0");
    let run = setup.fiber_with_env(&["extension", "install", "muse"], &gh.env());
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    gh.release("v0.2.0");
    let declined = setup.fiber_typing_env(&["extension", "update", "muse"], "n\n", &gh.env());
    assert_eq!(declined.code, Some(1), "stderr: {}", declined.stderr);
    assert!(declined.stderr.contains("Update "), "{}", declined.stderr);
    assert!(declined.stderr.contains("NOTES.md"), "{}", declined.stderr);
    assert!(setup.fiber_with_env(&["extension", "list"], &[]).raw[0].contains("v0.1.0"));
    let run = setup.fiber_with_env(&["extension", "update", "muse"], &gh.env());
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(setup.fiber_with_env(&["extension", "list"], &[]).raw[0].contains(" v0.2.0 "));
}

#[test]
fn remove_in_a_terminal_lists_the_data_and_asks_and_headless_goes_ahead() {
    let setup = Setup::new();
    let gh = Github::new(&setup);
    gh.release("v0.1.0");
    assert_eq!(
        setup
            .fiber_with_env(&["extension", "install", "muse"], &gh.env())
            .code,
        Some(0)
    );
    let data = setup
        .home()
        .join("data/github.com-aakshintala-fiber-providers-muse");
    let settings = setup
        .home()
        .join("config/github.com-aakshintala-fiber-providers-muse.json");
    fs::create_dir_all(&data).unwrap();
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    fs::write(data.join("index"), "x").unwrap();
    fs::write(&settings, "{}").unwrap();
    let declined = setup.fiber_typing(&["extension", "remove", "muse"], "n\n");
    assert_eq!(declined.code, Some(1), "stderr: {}", declined.stderr);
    assert!(
        declined
            .stderr
            .contains(&format!("Delete {}", data.display())),
        "{}",
        declined.stderr
    );
    assert!(
        declined
            .stderr
            .contains(&format!("Delete {}", settings.display())),
        "{}",
        declined.stderr
    );
    assert!(data.join("index").exists() && settings.exists());
    let approved = setup.fiber_typing(&["extension", "remove", "muse"], "y\n");
    assert_eq!(approved.code, Some(0), "stderr: {}", approved.stderr);
    assert!(!data.exists() && !settings.exists());
    assert!(
        setup
            .fiber_with_env(&["extension", "list"], &[])
            .raw
            .is_empty()
    );

    assert_eq!(
        setup
            .fiber_with_env(&["extension", "install", "muse"], &gh.env())
            .code,
        Some(0)
    );
    fs::create_dir_all(&data).unwrap();
    let headless = setup.fiber(&["extension", "remove", "muse"], None);
    assert_eq!(headless.code, Some(0), "stderr: {}", headless.stderr);
    assert!(!data.exists());
}

/// Asserts a completed turn that answered the weather question.
fn assert_weather(run: &Run) {
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let step = [
        "step_started",
        "assistant_message_started",
        "assistant_message_delta",
    ];
    let mut kinds = vec![
        "session_started",
        "fiber_started",
        "extensions_loaded",
        "preamble_built",
        "opening_message",
        "turn_started",
    ];
    kinds.extend(step);
    kinds.extend([
        "tool_call_arguments_delta",
        "reasoning_started",
        "reasoning_completed",
        "text_completed",
        "tool_call_requested",
        "usage_recorded",
        "assistant_message_completed",
        "tool_call_completed",
    ]);
    kinds.extend(step);
    kinds.extend(["assistant_message_delta"; 4]);
    kinds.extend([
        "reasoning_started",
        "reasoning_completed",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
        "fiber_exited",
    ]);
    assert_eq!(run.kinds(), kinds);
    let call = run
        .lines
        .iter()
        .find(|l| l["kind"] == "tool_call_requested")
        .unwrap();
    assert_eq!(call["payload"]["name"], "get_weather");
    assert_eq!(
        run.last()["payload"]["text"],
        "The weather in Paris is 18°C and clear."
    );
}

#[test]
fn opencode_go_installed_by_path_completes_a_turn_with_its_session_header() {
    let setup = Setup::new();
    let server = ProviderServer::start(go_exchange()).unwrap();
    install(
        &setup,
        &setup.package("opencode", "https://opencode.ai", &server.url()),
    );

    let run = setup.fiber_with_env(
        &[
            "ask",
            "--model",
            "opencode-go/muse-spark-1.3-contributor",
            "What is the weather in Paris?",
        ],
        &[("OPENCODE_API_KEY", "sk-test-opencode-go")],
    );

    assert_weather(&run);
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request.path, "/zen/go/v1/responses");
        assert_fingerprint(request, "authorization", "Bearer sk-test-opencode-go");
        assert_eq!(request.header("x-opencode-session"), Some(run.session_id()));
        assert!(request.header("user-agent").unwrap().starts_with("fiber/"));
    }
    let recorded: Vec<_> = run
        .lines
        .iter()
        .filter(|line| line["kind"] == "usage_recorded")
        .collect();
    assert_eq!(recorded.len(), 2);
    for line in &recorded {
        assert_eq!(line["payload"]["subscription"], true);
        assert!(line["payload"]["cost"].as_f64().is_some());
    }
    let totals = &run.last()["payload"]["usage"];
    assert_eq!(totals["cost"], 0.0);
    assert!(totals["subscription_cost"].as_f64().unwrap() > 0.0);
}

#[test]
fn a_zero_budget_fails_the_turn_before_the_provider_is_called() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    write(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "budget": {"usd": 0}}),
    );

    let run = setup.fiber(&["ask", "hi"], None);

    assert_eq!(run.code, Some(1), "stderr: {}", run.stderr);
    assert_eq!(
        run.kinds(),
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "turn_completed",
            "fiber_exited",
        ]
    );
    let completed = run
        .lines
        .iter()
        .find(|line| line["kind"] == "turn_completed")
        .unwrap();
    assert_eq!(completed["payload"]["outcome"], "failed");
    assert_eq!(completed["payload"]["error"]["code"], "budget_exceeded");
    assert_eq!(
        completed["payload"]["error"]["message"],
        "The session reached its spending budget of $0.00 (budget.usd)."
    );
    assert!(server.requests().is_empty());
    assert_eq!(run.last()["payload"]["exit_code"], 1);
}

/// Meta's own `openai-responses` stream for `muse-spark-1.3-contributor`,
/// rebuilt from the lines the probe kept: reasoning, then
/// `response.incomplete` at the probe's 16-token cap, with no text.
fn muse_recording() -> Response {
    let file = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../research/provider-errors/raw/muse-responses.ok-stream.json");
    let probe: Value = serde_json::from_slice(&fs::read(file).unwrap()).unwrap();
    let body: String = probe["stream"]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| format!("{}\n", line[1].as_str().unwrap()))
        .collect();
    Response::stream(body)
}

#[test]
fn muse_installed_by_path_completes_a_turn_on_metas_recorded_stream() {
    let setup = Setup::new();
    let server = ProviderServer::start([muse_recording()]).unwrap();
    install(
        &setup,
        &setup.package("muse", "https://api.meta.ai", &server.url()),
    );

    let run = setup.fiber_with_env(
        &["ask", "--model", "muse/muse-spark-1.3-contributor", "hi"],
        &[("META_API_KEY", "sk-test-muse")],
    );

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(
        run.kinds(),
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
    );
    let exited = &run.last()["payload"];
    assert_eq!(exited["exit_code"], 0);
    assert_eq!(exited["text"], "");
    assert_eq!(exited["usage"]["tokens"]["input"], 10);
    assert_eq!(exited["usage"]["tokens"]["output"], 16);
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/v1/responses");
    assert_fingerprint(&requests[0], "authorization", "Bearer sk-test-muse");
    assert_eq!(requests[0].header("x-opencode-session"), None);
    assert!(
        requests[0]
            .header("user-agent")
            .unwrap()
            .starts_with("fiber/")
    );
}

/// One recorded probe exchange as a fake-server response: the stream's
/// bytes, or the recorded error status with its body.
fn probe(path: &str, label: &str) -> Response {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let exchanges = probes::read(&root.join(path)).unwrap();
    let exchange = exchanges
        .into_iter()
        .find(|e| e.label.contains(label))
        .unwrap();
    match exchange.response {
        probes::Recorded::Stream(bytes) => Response::stream(bytes),
        probes::Recorded::Status(status, body) => Response::status(status, body),
    }
}

#[test]
fn anthropic_installed_by_path_completes_a_turn_on_its_recorded_streams() {
    let setup = Setup::new();
    let server = ProviderServer::start([
        probe(
            "research/anthropic-messages-probe/raw/stream.json",
            "stream tool use",
        ),
        probe(
            "research/anthropic-messages-probe/raw/stream.json",
            "stream plain text",
        ),
    ])
    .unwrap();
    install(
        &setup,
        &setup.package("anthropic", "https://api.anthropic.com", &server.url()),
    );

    let run = setup.fiber_with_env(
        &[
            "ask",
            "--model",
            "anthropic/claude-sonnet-5-5",
            "What is the weather in Paris? Use the tool.",
        ],
        &[("ANTHROPIC_API_KEY", "sk-test-anthropic")],
    );

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let mut kinds = vec![
        "session_started",
        "fiber_started",
        "extensions_loaded",
        "preamble_built",
        "opening_message",
        "turn_started",
        "step_started",
        "assistant_message_started",
    ];
    kinds.extend(["tool_call_arguments_delta"; 4]);
    kinds.extend([
        "tool_call_requested",
        "usage_recorded",
        "assistant_message_completed",
        "tool_call_completed",
        "step_started",
        "assistant_message_started",
    ]);
    kinds.extend(["assistant_message_delta"; 3]);
    kinds.extend([
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
        "fiber_exited",
    ]);
    assert_eq!(run.kinds(), kinds);
    assert_eq!(run.last()["payload"]["text"], "Hello, lovely human!");
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request.path, "/v1/messages");
        assert_fingerprint(request, "x-api-key", "sk-test-anthropic");
    }
}

#[test]
fn openai_installed_by_path_completes_a_turn_and_sends_store_false() {
    let setup = Setup::new();
    let server = ProviderServer::start([
        probe(
            "research/openai-responses-probe/raw/probe.json",
            "tool no strict",
        ),
        probe(
            "research/openai-responses-probe/raw/probe.json",
            "instructions #1",
        ),
    ])
    .unwrap();
    install(
        &setup,
        &setup.package("openai", "https://api.openai.com", &server.url()),
    );

    let run = setup.fiber_with_env(
        &[
            "ask",
            "--model",
            "openai/gpt-6-luna",
            "Call f with a=\"x\".",
        ],
        &[("OPENAI_API_KEY", "sk-test-openai")],
    );

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(
        run.kinds(),
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
    );
    assert_eq!(run.last()["payload"]["text"], "7");
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request.path, "/v1/responses");
        assert_fingerprint(request, "authorization", "Bearer sk-test-openai");
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["store"], Value::Bool(false));
    }
}

#[test]
fn gemini_installed_by_path_completes_a_turn_on_its_recorded_streams() {
    let setup = Setup::new();
    let server = ProviderServer::start([
        probe(
            "research/google-generative-ai-probe/raw/id-emitted-gemini-3.1-flash-lite.json",
            "id-emitted",
        ),
        probe(
            "research/google-generative-ai-probe/raw/sse2-stream-ok.json",
            "sse2-stream-ok",
        ),
    ])
    .unwrap();
    install(
        &setup,
        &setup.package(
            "gemini",
            "https://generativelanguage.googleapis.com",
            &server.url(),
        ),
    );

    let run = setup.fiber_with_env(
        &[
            "ask",
            "--model",
            "gemini/gemini-3.1-flash-lite",
            "Plot point x=3 y=4 with v \"a\".",
        ],
        &[("GEMINI_API_KEY", "sk-test-gemini")],
    );

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    // The `functionCall` part carries a `thoughtSignature`, which decodes
    // as reasoning. The second recording joins its text fragments and
    // trailing signature into one text part.
    assert_eq!(
        run.kinds(),
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "tool_call_arguments_delta",
            "reasoning_started",
            "reasoning_completed",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
    );
    assert_eq!(
        run.last()["payload"]["text"],
        "Hello! How can I help you today?"
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(
            request.path,
            "/v1beta/models/gemini-3.1-flash-lite:streamGenerateContent?alt=sse"
        );
        assert_fingerprint(request, "x-goog-api-key", "sk-test-gemini");
    }
}

#[test]
fn anthropic_installed_by_path_completes_a_turn_on_a_scripted_stream() {
    let setup = Setup::new();
    let server = ProviderServer::start([anthropic_hello()]).unwrap();
    install(
        &setup,
        &setup.package("anthropic", "https://api.anthropic.com", &server.url()),
    );

    let run = setup.fiber_with_env(
        &["ask", "--model", "anthropic/claude-sonnet-5-5", "hi"],
        &[("ANTHROPIC_API_KEY", "sk-test-anthropic")],
    );

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), HELLO_KINDS);
    assert_eq!(run.last()["payload"]["text"], "Hello.");
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/v1/messages");
    assert_fingerprint(&requests[0], "x-api-key", "sk-test-anthropic");
}

#[test]
fn openai_installed_by_path_completes_a_turn_on_a_scripted_stream() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    install(
        &setup,
        &setup.package("openai", "https://api.openai.com", &server.url()),
    );

    let run = setup.fiber_with_env(
        &["ask", "--model", "openai/gpt-6-luna", "hi"],
        &[("OPENAI_API_KEY", "sk-test-openai")],
    );

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), HELLO_KINDS);
    assert_eq!(run.last()["payload"]["text"], "Hello.");
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/v1/responses");
    assert_fingerprint(&requests[0], "authorization", "Bearer sk-test-openai");
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["store"], Value::Bool(false));
}

#[test]
fn openrouter_installed_by_path_records_the_inline_cost_on_a_completed_turn() {
    let setup = Setup::new();
    let server =
        ProviderServer::start([completions_hello("gen-abc123", json!(0.0000072))]).unwrap();
    install(
        &setup,
        &setup.package("openrouter", "https://openrouter.ai", &server.url()),
    );

    let run = setup.fiber_with_env(
        &["ask", "--model", "openrouter/z-ai/glm-5.3-flash", "hi"],
        &[("OPENROUTER_API_KEY", "sk-test-openrouter")],
    );

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), HELLO_KINDS);
    assert_eq!(run.last()["payload"]["text"], "Hello.");
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/api/v1/chat/completions");
    assert_fingerprint(&requests[0], "authorization", "Bearer sk-test-openrouter");
    // The stream's 15 prompt tokens hold 14 cached, so the declared
    // prices give about 0.0000051: the inline figure stands instead.
    let recorded: Vec<_> = run
        .lines
        .iter()
        .filter(|line| line["kind"] == "usage_recorded")
        .collect();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0]["payload"]["cost"], json!(0.0000072));
    assert_eq!(recorded[0]["payload"]["generation_id"], "gen-abc123");
}

#[test]
fn openrouter_sends_the_cache_key_and_anthropic_markers_for_a_claude_model() {
    let setup = Setup::new();
    let server = ProviderServer::start([completions_hello("gen-one", json!(0.0001))]).unwrap();
    install(
        &setup,
        &setup.package("openrouter", "https://openrouter.ai", &server.url()),
    );

    // The default lifetime is 1 hour: the markers carry `ttl: "1h"`.
    let run = setup.fiber_with_env(
        &[
            "ask",
            "--model",
            "openrouter/anthropic/claude-sonnet-5.5",
            "hi",
        ],
        &[("OPENROUTER_API_KEY", "sk-test-openrouter")],
    );
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), HELLO_KINDS);
    let built = run
        .lines
        .iter()
        .find(|l| l["kind"] == "preamble_built")
        .unwrap();
    assert_eq!(built["payload"]["cache_lifetime"], "1h");
    let body: Value = serde_json::from_slice(&server.requests()[0].body).unwrap();
    assert_eq!(body["session_id"], run.session_id());
    assert_eq!(body["prompt_cache_key"], run.session_id());
    let hour = json!({"type": "ephemeral", "ttl": "1h"});
    // The system message holds several parts; the marker is on its last.
    // The opening message stands between it and the prompt, so the
    // prompt's marker is on the messages' last.
    let system = body["messages"][0]["content"].as_array().unwrap();
    assert_eq!(system.last().unwrap()["cache_control"], hour);
    let last = body["messages"].as_array().unwrap();
    assert_eq!(last.last().unwrap()["content"][0]["cache_control"], hour);
    // A per-session `-c cache.lifetime=5m` run is not covered here; see #414.
}

/// Runs `fiber ask` for the OpenRouter Claude model against `server`, with
/// `home_config` as the global `config.json` and `repo_config` as the
/// workspace's `.fiber/config.json` when given. Returns the run and the
/// request it sent.
fn openrouter_claude_request(
    server: &ProviderServer,
    setup: &Setup,
    home_config: Value,
    repo_config: Option<Value>,
) -> (Run, Value) {
    install(
        setup,
        &setup.package("openrouter", "https://openrouter.ai", &server.url()),
    );
    write(&setup.home().join("config.json"), &home_config);
    if let Some(repo) = repo_config {
        write(
            &setup.root.path().join("w").join(".fiber/config.json"),
            &repo,
        );
    }
    let run = setup.fiber_with_env(
        &[
            "ask",
            "--model",
            "openrouter/anthropic/claude-sonnet-5.5",
            "hi",
        ],
        &[("OPENROUTER_API_KEY", "sk-test-openrouter")],
    );
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let body: Value = serde_json::from_slice(&server.requests()[0].body).unwrap();
    (run, body)
}

/// A 5-minute run records `"5m"` in `preamble_built` and marks the system
/// and last-message blocks with a `ttl`-less marker.
fn assert_five_minute_markers(run: &Run, body: &Value) {
    assert_eq!(run.kinds(), HELLO_KINDS);
    let built = run
        .lines
        .iter()
        .find(|l| l["kind"] == "preamble_built")
        .unwrap();
    assert_eq!(built["payload"]["cache_lifetime"], "5m");
    let five = json!({"type": "ephemeral"});
    let system = body["messages"][0]["content"].as_array().unwrap();
    assert_eq!(system.last().unwrap()["cache_control"], five);
    let last = body["messages"].as_array().unwrap();
    assert_eq!(last.last().unwrap()["content"][0]["cache_control"], five);
}

#[test]
fn a_per_model_cache_lifetime_of_five_minutes_marks_the_request_without_ttl() {
    let setup = Setup::new();
    let server = ProviderServer::start([completions_hello("gen-one", json!(0.0001))]).unwrap();
    let (run, body) = openrouter_claude_request(
        &server,
        &setup,
        json!({"models": {"openrouter/anthropic/claude-sonnet-5.5": {"cache": {"lifetime": "5m"}}}}),
        None,
    );
    assert_five_minute_markers(&run, &body);
}

#[test]
fn a_repository_cache_lifetime_of_five_minutes_marks_the_request_without_ttl() {
    let setup = Setup::new();
    let server = ProviderServer::start([completions_hello("gen-one", json!(0.0001))]).unwrap();
    let (run, body) = openrouter_claude_request(
        &server,
        &setup,
        json!({"model": "openrouter/anthropic/claude-sonnet-5.5"}),
        Some(json!({"cache": {"lifetime": "5m"}})),
    );
    assert_five_minute_markers(&run, &body);
}

#[test]
fn the_openrouter_package_declares_completions_models_with_the_cache_key() {
    let providers = config::read_providers(&package("openrouter")).unwrap();
    assert_eq!(providers.len(), 1);
    let provider = &providers[0];
    assert_eq!(provider.name, "openrouter");
    assert_eq!(
        provider.reviewer_model.as_deref(),
        Some("anthropic/claude-sonnet-5.5")
    );
    assert!(
        provider
            .models
            .iter()
            .any(|m| m.id == "anthropic/claude-sonnet-5.5")
    );
    for model in &provider.models {
        assert_eq!(
            model.protocol,
            config::Protocol::OpenaiCompletions,
            "{}",
            model.id
        );
        assert_eq!(
            model.base_url, "https://openrouter.ai/api/v1",
            "{}",
            model.id
        );
        assert_eq!(
            model.compat["cache_key_field"], "session_id",
            "{}",
            model.id
        );
        assert_eq!(
            model.compat.get("anthropic"),
            model
                .id
                .starts_with("anthropic/")
                .then_some(&Value::Bool(true)),
            "{}",
            model.id
        );
    }
}

#[test]
fn gemini_installed_by_path_completes_a_turn_on_a_scripted_stream() {
    let setup = Setup::new();
    let server = ProviderServer::start([gemini_hello()]).unwrap();
    install(
        &setup,
        &setup.package(
            "gemini",
            "https://generativelanguage.googleapis.com",
            &server.url(),
        ),
    );

    let run = setup.fiber_with_env(
        &["ask", "--model", "gemini/gemini-3.1-flash-lite", "hi"],
        &[("GEMINI_API_KEY", "sk-test-gemini")],
    );

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), HELLO_KINDS);
    assert_eq!(run.last()["payload"]["text"], "Hello.");
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].path,
        "/v1beta/models/gemini-3.1-flash-lite:streamGenerateContent?alt=sse"
    );
    assert_fingerprint(&requests[0], "x-goog-api-key", "sk-test-gemini");
}

#[test]
fn the_first_party_key_packages_declare_their_protocol_url_and_prices() {
    let packages = [
        (
            "anthropic",
            config::Protocol::AnthropicMessages,
            "https://api.anthropic.com/v1",
            &[
                "claude-fable-5-1",
                "claude-opus-5-5",
                "claude-sonnet-5-5",
                "claude-haiku-4-5",
                "claude-opus-5",
                "claude-sonnet-5",
                "claude-fable-5",
                "claude-opus-4-8",
                "claude-opus-4-7",
                "claude-sonnet-4-6",
                "claude-opus-4-6",
                "claude-opus-4-5",
                "claude-sonnet-4-5",
            ][..],
        ),
        (
            "openai",
            config::Protocol::OpenaiResponses,
            "https://api.openai.com/v1",
            &[
                "gpt-6.1-sol",
                "gpt-6-sol",
                "gpt-6-astra",
                "gpt-6-luna",
                "gpt-4.1-mini",
                "gpt-4o-mini",
                "gpt-5.5-pro",
                "gpt-5.6-sol",
                "gpt-5.4",
                "gpt-5.6-terra",
                "gpt-5.4-pro",
                "o3",
                "gpt-4.1",
                "gpt-5.5",
                "gpt-5",
                "gpt-4o",
                "gpt-5.4-nano",
                "gpt-5.3-codex",
                "gpt-5.2",
                "gpt-5.4-mini",
                "gpt-5.1",
                "o1-pro",
                "gpt-5-pro",
                "gpt-5.2-pro",
                "gpt-5-mini",
                "gpt-5-nano",
                "gpt-5.6-luna",
            ][..],
        ),
        (
            "gemini",
            config::Protocol::GoogleGenerativeAi,
            "https://generativelanguage.googleapis.com/v1beta",
            &[
                "gemini-3.1-pro-preview",
                "gemini-3.8-flash",
                "gemini-3.5-flash-lite",
                "gemini-3.1-flash-lite",
                "gemini-3-flash-preview",
                "gemini-3.1-pro-preview-customtools",
                "gemini-3.1-flash-lite-preview",
                "gemini-3.5-flash",
                "gemini-3.6-flash",
                "gemini-3.7-flash",
            ][..],
        ),
    ];
    for (name, protocol, url, ids) in packages {
        let providers = config::read_providers(&package(name)).unwrap();
        assert_eq!(providers.len(), 1, "{name}");
        let provider = &providers[0];
        assert_eq!(provider.name, name);
        let reviewer = match name {
            "anthropic" => "claude-sonnet-5-5",
            "openai" => "gpt-6-luna",
            _ => "gemini-3.8-flash",
        };
        assert_eq!(provider.reviewer_model.as_deref(), Some(reviewer), "{name}");
        assert!(provider.models.iter().any(|m| m.id == reviewer), "{name}");
        let model_ids: Vec<&str> = provider.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(model_ids, Vec::from(ids));
        for model in &provider.models {
            assert_eq!(model.protocol, protocol, "{}", model.id);
            assert_eq!(model.base_url, url, "{}", model.id);
            assert!(model.context_window.unwrap_or(0) > 0, "{}", model.id);
            assert!(model.max_output_tokens.unwrap_or(0) > 0, "{}", model.id);
            assert!(!model.input.is_empty(), "{}", model.id);
            for kind in &model.input {
                assert!(kind == "text" || kind == "image", "{}", model.id);
            }
            let cost = model.cost.as_ref().unwrap();
            assert!(cost.input > 0.0 && cost.output > 0.0, "{}", model.id);
            let mut previous = 0;
            for tier in &cost.tiers {
                assert!(tier.input_tokens_above > previous, "{}", model.id);
                previous = tier.input_tokens_above;
                assert!(
                    tier.input >= 0.0
                        && tier.output >= 0.0
                        && tier.cache_read >= 0.0
                        && tier.cache_write >= 0.0,
                    "{}",
                    model.id
                );
            }
            if name == "openai" {
                assert_eq!(
                    model.compat.get("store"),
                    Some(&Value::Bool(false)),
                    "{}",
                    model.id
                );
            } else {
                assert!(model.compat.get("store").is_none(), "{}", model.id);
            }
        }
    }
}

#[test]
fn a_zen_model_is_sent_to_zens_url() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    install(
        &setup,
        &setup.package("opencode", "https://opencode.ai", &server.url()),
    );

    let run = setup.fiber_with_env(
        &["ask", "--model", "opencode-zen/muse-spark-1.3", "hi"],
        &[("OPENCODE_API_KEY", "sk-test-zen")],
    );

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), HELLO_KINDS);
    assert_eq!(run.last()["payload"]["text"], "Hello.");
    let requests = server.requests();
    assert_eq!(requests[0].path, "/zen/v1/responses");
    assert_fingerprint(&requests[0], "authorization", "Bearer sk-test-zen");
    assert_eq!(
        requests[0].header("x-opencode-session"),
        Some(run.session_id())
    );
}

#[test]
fn every_go_model_is_a_subscription_on_gos_url_and_every_zen_model_is_not() {
    let providers = config::read_providers(&package("opencode")).unwrap();
    let names: Vec<&str> = providers.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names.len(), 2, "{names:?}");
    for provider in &providers {
        let (url, subscription) = match provider.name.as_str() {
            "opencode-go" => ("https://opencode.ai/zen/go/v1", true),
            "opencode-zen" => ("https://opencode.ai/zen/v1", false),
            other => panic!("unexpected provider {other}"),
        };
        assert!(!provider.models.is_empty(), "{}", provider.name);
        for model in &provider.models {
            assert_eq!(model.base_url, url, "{}", model.id);
            assert_eq!(model.subscription, subscription, "{}", model.id);
            assert!(model.cost.is_some(), "{} declares its prices", model.id);
            assert_eq!(model.compat["cache_key_header"], "x-opencode-session");
        }
    }
    let go = providers.iter().find(|p| p.name == "opencode-go").unwrap();
    assert!(
        go.models
            .iter()
            .any(|m| m.id == "muse-spark-1.3-contributor")
    );
}

#[test]
fn install_takes_one_name_or_path() {
    let setup = Setup::new();
    let missing = setup.fiber(&["extension", "install"], None);
    assert_eq!(missing.code, Some(2));
    assert_eq!(
        missing.stderr,
        "fiber: The following required arguments were not provided: <name or path>. Run `fiber --help` for usage.\n"
    );
    let extra = setup.fiber(&["extension", "install", "a", "b"], None);
    assert_eq!(extra.code, Some(2));
    assert_eq!(
        extra.stderr,
        "fiber: Unexpected argument 'b' found. Run `fiber --help` for usage.\n"
    );
    let run = setup.fiber(&["extension", "install", "/nonexistent"], None);
    assert_eq!(run.code, Some(1));
    let root = setup.home().join("extensions");
    let installed = fs::read_dir(&root).is_ok_and(|entries| {
        entries
            .filter_map(Result::ok)
            .any(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
    });
    assert!(!installed, "a refused install leaves no extension");
    assert!(!setup.home().join(".extensions.lock").exists());
}

/// A live turn with `model`, opt in by naming the key
/// file in `var` (`docs/testing.md`, "Live calls and evals"). Prints the
/// event kinds and the final text.
fn live(var: &str, package_name: &str, provider: &str, model: &str, key_env: &str) {
    let Some(key_file) = std::env::var_os(var) else {
        return;
    };
    let key = fs::read_to_string(key_file).unwrap();
    let setup = Setup::new();
    install(&setup, &package(package_name));

    let model = format!("{provider}/{model}");
    let run = setup.fiber_with_env(
        &["ask", "--model", &model, "Reply with one short sentence."],
        &[(key_env, key.trim())],
    );

    eprintln!("kinds: {:?}", run.kinds());
    eprintln!("text: {}", run.last()["payload"]["text"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(!run.last()["payload"]["text"].as_str().unwrap().is_empty());
}

#[test]
fn live_opencode_go_completes_one_turn() {
    live(
        "FIBER_LIVE_OPENCODE_KEY_FILE",
        "opencode",
        "opencode-go",
        "muse-spark-1.3-contributor",
        "OPENCODE_API_KEY",
    );
}

#[test]
fn live_muse_completes_one_turn() {
    live(
        "FIBER_LIVE_MUSE_KEY_FILE",
        "muse",
        "muse",
        "muse-spark-1.3-contributor",
        "META_API_KEY",
    );
}

#[test]
fn live_anthropic_completes_one_turn() {
    live(
        "FIBER_LIVE_ANTHROPIC_KEY_FILE",
        "anthropic",
        "anthropic",
        "claude-sonnet-5-5",
        "ANTHROPIC_API_KEY",
    );
}

#[test]
fn live_openai_completes_one_turn() {
    live(
        "FIBER_LIVE_OPENAI_KEY_FILE",
        "openai",
        "openai",
        "gpt-6-luna",
        "OPENAI_API_KEY",
    );
}

#[test]
fn live_gemini_completes_one_turn() {
    live(
        "FIBER_LIVE_GEMINI_KEY_FILE",
        "gemini",
        "gemini",
        "gemini-3.1-flash-lite",
        "GEMINI_API_KEY",
    );
}

#[test]
fn two_runs_send_byte_identical_preambles() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);

    let first = setup.fiber(&["ask", "one"], None);
    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
    let second = setup.fiber(&["ask", "two"], None);
    assert_eq!(second.code, Some(0), "stderr: {}", second.stderr);

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    let first_body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    let second_body: Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert_eq!(first_body["instructions"], second_body["instructions"]);
    assert_eq!(first_body["tools"], second_body["tools"]);
    let names: Vec<_> = first_body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "edit",
            "handoff",
            "jobs",
            "read",
            "shell",
            "web_fetch",
            "write"
        ]
    );
}

/// Installs the `review-pr` skill with body `Review the pull request named
/// in the arguments.` in the workspace's `.agents/skills/`.
fn install_review_skill(setup: &Setup) {
    let dir = setup.root.path().join("w/.agents/skills/review-pr");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("SKILL.md"),
        "---\nname: review-pr\ndescription: Reviews a pull request.\n---\nReview the pull request named in the arguments.\n",
    )
    .unwrap();
}

#[test]
fn a_slash_prompt_runs_the_skill_with_the_rest_as_its_arguments() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    install_review_skill(&setup);

    let run = setup.fiber(&["ask", "/review-pr 42"], None);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), HELLO_KINDS);
    let expanded = "Review the pull request named in the arguments.\n\n42";
    assert_eq!(turn_input(&run), expanded);
    // The fake provider's received user message carries the expanded text.
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert!(
        String::from_utf8_lossy(&requests[0].body)
            .contains(&serde_json::to_string(expanded).unwrap())
    );
}

#[test]
fn a_prompt_naming_no_skill_is_sent_as_written() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    install_review_skill(&setup);

    let run = setup.fiber(&["ask", "/nope x"], None);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), HELLO_KINDS);
    assert_eq!(turn_input(&run), "/nope x");
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert!(
        String::from_utf8_lossy(&requests[0].body)
            .contains(&serde_json::to_string("/nope x").unwrap())
    );
}

#[test]
fn a_slash_prompt_runs_a_prompt_template() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    // A prompt template: a skill with `disable-model-invocation: true`. It
    // is left out of the listing but keeps its `/name`.
    let dir = setup.root.path().join("w/.agents/skills/plan");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("SKILL.md"),
        "---\nname: plan\ndescription: Plans the work.\ndisable-model-invocation: true\n---\nPlan the work below.\n",
    )
    .unwrap();

    let run = setup.fiber(&["ask", "/plan 42"], None);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), HELLO_KINDS);
    assert_eq!(turn_input(&run), "Plan the work below.\n\n42");
}

/// Installs the fixture extension and points its provider at `server`,
/// with `fixture/m1` as the configured model.
fn fixture(setup: &Setup, server: &ProviderServer) {
    extensions::plan(
        &setup.home(),
        &extensions::Request::Path(fakes::lua_fixture()),
        "0.1.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    config::store_secret(
        &setup.home(),
        "fixture.url",
        &config::Secret::new(server.url()),
    )
    .unwrap();
    config::store_secret(
        &setup.home(),
        "fixture.api_key",
        &config::Secret::new("k1".into()),
    )
    .unwrap();
    write(
        &setup.home().join("config.json"),
        &json!({"model": "fixture/m1"}),
    );
}

/// A token expiry far enough ahead that the running binary reads it as
/// future, as a fixed stamp rather than a read of the test's clock.
const EXPIRES_AT: u64 = 2_000_000_000;

/// A reply the fixture's `models()` and `credential()` both read: the model
/// list and the token in one body, so the background refresh and the token
/// request succeed in either arrival order.
fn listing_and_token() -> Response {
    Response::status(
        200,
        json!({
            "data": [{"id": "m1", "context_length": 1000}],
            "access_token": "tok-1",
            "expires_at": EXPIRES_AT,
        })
        .to_string(),
    )
}

/// Whether `value` is 64 lowercase hex digits, a SHA-256 in hex.
fn is_hex64(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The model's request: exactly one went out, carrying the token and the
/// signature.
fn only_model_request(server: &ProviderServer) -> Request {
    let sent: Vec<Request> = server
        .requests()
        .into_iter()
        .filter(|request| request.path == "/v1/responses")
        .collect();
    assert_eq!(sent.len(), 1, "{sent:?}");
    let request = &sent[0];
    assert_fingerprint(request, "authorization", "Bearer tok-1");
    assert_eq!(
        request.header("x-fixture-saw"),
        Some("body_sha256,headers,method,url")
    );
    let signature = request.header("x-fixture-signature").unwrap_or("");
    assert!(is_hex64(signature), "{signature:?}");
    let content_sha = request.header("x-fixture-content-sha256").unwrap_or("");
    assert!(is_hex64(content_sha), "{content_sha:?}");
    sent.into_iter().next().unwrap()
}

#[test]
fn a_lua_providers_model_answers_with_the_token_and_sign_headers() {
    let setup = Setup::new();
    // With no cache discovery runs synchronously once and no refresh
    // runs: one reply each for discovery, the token and the model request.
    let server = ProviderServer::start_with_fallback(
        [listing_and_token(), listing_and_token(), hello()],
        hello(),
    )
    .unwrap();
    fixture(&setup, &server);

    let run = setup.fiber(&["ask", "hi"], None);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let exited = &run.last()["payload"];
    assert_eq!(exited["exit_code"], 0);
    assert_eq!(exited["text"], "Hello.");
    only_model_request(&server);
    // Discovery cached the list for the next start.
    let cached: Value = serde_json::from_str(
        &fs::read_to_string(setup.home().join("cache/models/fixture.json")).unwrap(),
    )
    .unwrap();
    assert!(
        cached
            .as_array()
            .unwrap()
            .iter()
            .any(|model| model["id"] == "m1"),
        "{cached:?}"
    );
    // Discovery ran synchronously once at startup: with no cache no
    // background refresh runs, so exactly one `GET /v1/models` went out.
    // The run above waited for the session to finish, past when a refresh
    // would have arrived. The token request went out too.
    let requests = server.requests();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "GET" && request.path == "/v1/models")
            .count(),
        1,
        "{requests:?}"
    );
    assert!(
        requests.iter().any(|request| request.path == "/token"),
        "{requests:?}"
    );
}

#[test]
fn a_cached_list_serves_the_model_while_the_refresh_runs_in_the_background() {
    let setup = Setup::new();
    let server = ProviderServer::start_with_fallback(
        [
            // No model list: if startup ran `models()` synchronously it
            // would fail, so the run succeeding proves the cache served.
            Response::status(
                200,
                json!({"access_token": "tok-1", "expires_at": EXPIRES_AT}).to_string(),
            ),
            listing_and_token(),
            hello(),
        ],
        hello(),
    )
    .unwrap();
    fixture(&setup, &server);
    let file = setup.home().join("cache/models/fixture.json");
    write(
        &file,
        &json!([{
            "id": "m1",
            "protocol": "openai-responses",
            "base_url": format!("{}/v1", server.url()),
        }]),
    );
    // The copy is stale: backdated past `model_lists.refresh_after`, so
    // startup refreshes it in the background (`docs/model-routing.md`,
    // "Model discovery"). An ancient mtime is older than any age cap,
    // without reading the clock.
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1))
        .unwrap();
    let run = setup.fiber(&["ask", "hi"], None);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let exited = &run.last()["payload"];
    assert_eq!(exited["exit_code"], 0);
    assert_eq!(exited["text"], "Hello.");
    assert!(
        server.await_requests(3, DEADLINE),
        "waited for the refresh, the token and the model request"
    );
    let mut paths: Vec<String> = server
        .requests()
        .iter()
        .map(|request| format!("{} {}", request.method, request.path))
        .collect();
    paths.sort();
    assert_eq!(
        paths,
        ["GET /v1/models", "POST /token", "POST /v1/responses"]
    );
    only_model_request(&server);
}

#[test]
fn a_credential_that_errors_fails_before_any_session_line() {
    let setup = Setup::new();
    let server = ProviderServer::start([
        Response::status(
            200,
            json!({"data": [{"id": "m1", "context_length": 1000}]}).to_string(),
        ),
        Response::status(500, "{}"),
    ])
    .unwrap();
    fixture(&setup, &server);

    let run = setup.fiber(&["ask", "hi"], None);

    assert_pre_session(&run, 1, "credential_failed");
}

#[test]
fn a_lua_provider_without_credential_uses_the_stored_key() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    let source = setup.root.path().join("src-plain");
    write(
        &source.join("extension.json"),
        &json!({"name": "plain", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}),
    );
    fs::write(
        source.join("init.lua"),
        format!(
            "fiber.provider(\"plain\", {{\n models = {{\n   timeout = 5000,\n   run = function()\n     return {{ {{ id = \"m1\", protocol = \"openai-responses\", base_url = \"{}/v1\" }} }}\n   end,\n }},\n}})\n",
            server.url()
        ),
    )
    .unwrap();
    extensions::plan(
        &setup.home(),
        &extensions::Request::Path(source),
        "0.0.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    config::store_credential(
        &setup.home(),
        "plain",
        "default",
        &config::Secret::new("k-plain".into()),
    )
    .unwrap();
    write(
        &setup.home().join("config.json"),
        &json!({"model": "plain/m1"}),
    );

    let run = setup.fiber(&["ask", "hi"], None);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), HELLO_KINDS);
    assert_eq!(run.last()["payload"]["text"], "Hello.");
    assert!(
        server.await_requests(1, DEADLINE),
        "waited for the model request"
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert_eq!(requests[0].path, "/v1/responses");
    assert_fingerprint(&requests[0], "authorization", "Bearer k-plain");
    assert!(
        !requests.iter().any(|request| request.path == "/token"),
        "{requests:?}"
    );
}
