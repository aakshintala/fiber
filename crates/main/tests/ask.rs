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
use std::io::Write;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fakes::{ProviderServer, Response};
use rustix::pty;
use serde_json::{Value, json};

/// How long one `fiber` run may take.
const DEADLINE: Duration = Duration::from_secs(20);

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// A temporary root holding Fiber home and the workspace, removed on drop.
/// Its name is short: a session's socket path must fit in 103 bytes on
/// macOS.
struct Setup {
    root: PathBuf,
}

impl Setup {
    fn new() -> Self {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("fa{}-{n}", std::process::id()));
        fs::remove_dir_all(&root).unwrap_or(());
        fs::create_dir_all(root.join("h")).unwrap();
        fs::create_dir_all(root.join("w")).unwrap();
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.join("h")
    }

    /// Installs a provider `fake` with model `m` on `openai-responses` at the
    /// fake server, and makes `fake/m` the configured model.
    fn provider(&self, server: &ProviderServer) {
        let source = self.root.join("src");
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
        extensions::install(&self.home(), &source, "0.0.0").unwrap();
        write(
            &self.home().join("config.json"),
            &json!({"model": "fake/m"}),
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
        let to = self.root.join(format!("pkg-{name}"));
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
        let terminal = Terminal::open();
        fs::File::from(terminal.main.try_clone().unwrap())
            .write_all(typed.as_bytes())
            .unwrap();
        let home = self.home();
        self.run(home.to_str().unwrap(), args, terminal.stdin(), None, &[])
    }

    /// Runs `fiber` in its own process group, waits for it under
    /// [`DEADLINE`], and asserts that nothing it started is left in the
    /// group, after a timeout too (`docs/testing.md`, "Running tests").
    fn run(
        &self,
        home: &str,
        args: &[&str],
        stdin: Stdio,
        text: Option<&str>,
        env: &[(&str, &str)],
    ) -> Run {
        let mut child = Command::new(env!("CARGO_BIN_EXE_fiber"))
            .args(args)
            .current_dir(self.root.join("w"))
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.root)
            .env("FIBER_HOME", home)
            .env("FIBER_TEST_FAKE_KEY", "sk-test")
            .envs(env.iter().copied())
            .stdin(stdin)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .unwrap();
        let group = child.id();
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
                kill_group(group);
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
        Run::from(output)
    }

    /// Runs `fiber` with `args` and these environment variables set over its
    /// own, stdin not a terminal.
    fn fiber_env(&self, args: &[&str], env: &[(&str, &str)]) -> Run {
        let home = self.home();
        self.run(home.to_str().unwrap(), args, Stdio::null(), None, env)
    }

    fn fiber(&self, args: &[&str], stdin: Option<&str>) -> Run {
        self.fiber_with_home(self.home().to_str().unwrap(), args, stdin)
    }
}

impl Drop for Setup {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap_or(());
    }
}

fn write(file: &Path, value: &Value) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, value.to_string()).unwrap();
}

/// Whether any process remains in process group `group`.
fn group_alive(group: u32) -> bool {
    Command::new("kill")
        .args(["-0", "--", &format!("-{group}")])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
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

fn kill_group(group: u32) {
    Command::new("kill")
        .args(["-KILL", "--", &format!("-{group}")])
        .status()
        .unwrap();
}

/// One finished run: its exit code, stdout's lines, as text and parsed, and
/// stderr.
struct Run {
    code: Option<i32>,
    raw: Vec<String>,
    lines: Vec<Value>,
    stderr: String,
}

impl From<Output> for Run {
    fn from(output: Output) -> Self {
        let stdout = String::from_utf8(output.stdout).unwrap();
        let raw: Vec<String> = stdout.lines().map(str::to_owned).collect();
        let lines = raw
            .iter()
            // `fiber list` prints text; a `kind` lookup on it fails the test.
            .map(|l| serde_json::from_str(l).unwrap_or(Value::Null))
            .collect();
        Self {
            code: output.status.code(),
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
        let workspace = fs::canonicalize(setup.root.join("w")).unwrap();
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

/// The event kinds of a turn answered by [`hello`].
const HELLO_KINDS: [&str; 11] = [
    "fiber_started",
    "session_started",
    "turn_started",
    "step_started",
    "assistant_message_started",
    "assistant_message_delta",
    "assistant_message_delta",
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
    assert_eq!(run.lines[0]["payload"]["resumed"], false);
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
    assert_eq!(requests[0].header("authorization"), Some("<masked>"));
    assert!(String::from_utf8_lossy(&requests[0].body).contains("\"hi\""));
}

#[test]
fn a_prompt_on_stdin_runs_one_turn() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);

    let run = setup.fiber(&["ask"], Some("review the brief\n"));

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(turn_input(&run), "review the brief\n");
    assert_eq!(run.kinds().first(), Some(&"fiber_started"));
    assert_eq!(run.last()["payload"]["text"], "Hello.");
}

#[test]
fn a_failed_turn_exits_1_with_the_turns_error() {
    let setup = Setup::new();
    let server = ProviderServer::start([Response::status(503, "{}")]).unwrap();
    setup.provider(&server);

    let run = setup.fiber(&["ask", "hi"], None);

    assert_eq!(run.code, Some(1));
    assert_eq!(
        run.kinds(),
        [
            "fiber_started",
            "session_started",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
    );
    let turn = &run.lines[6]["payload"];
    assert_eq!(turn["outcome"], "failed");
    let exited = &run.last()["payload"];
    assert_eq!(exited["exit_code"], 1);
    assert_eq!(exited["error"], turn["error"]);
    assert_eq!(exited["error"]["code"], "provider_unavailable");
    assert_eq!(exited.get("text"), None);
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

    assert_pre_session(&setup.fiber(&["ask", "hi"], Some("and this")), 2, "usage");
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
        run.stderr.contains("`--model` takes a model"),
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
fn fiber_without_ask_is_a_usage_error_naming_fiber_ask() {
    let setup = Setup::new();

    for args in [&[][..], &["hi"][..]] {
        let run = setup.fiber(args, None);

        assert_eq!(run.code, Some(2));
        assert!(run.stderr.contains("fiber ask"), "stderr: {}", run.stderr);
        // Not `fiber ask`, so no event line.
        assert!(run.lines.is_empty());
    }
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

/// Installs the package at `path` with `fiber install`, stdin not a
/// terminal, so it does not ask.
fn install(setup: &Setup, path: &Path) {
    let run = setup.fiber(&["install", path.to_str().unwrap()], None);
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

    let declined = setup.fiber_typing(&["install", path], "n\n");
    assert_eq!(declined.code, Some(1), "stderr: {}", declined.stderr);
    assert!(!installed.exists());

    let run = setup.fiber_typing(&["install", path], "y\n");
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(
        run.stderr,
        format!(
            "Install github.com/aakshintala/fiber/providers/opencode from {path}\n\
             Version 0.0.0\n\
             Provider opencode-go: https://opencode.ai/zen/go/v1\n\
             Provider opencode-zen: https://opencode.ai/zen/v1\n\
             Go ahead? [y/N] fiber: installed github.com/aakshintala/fiber/providers/opencode\n"
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
    let listed = setup.fiber(&["list"], None);
    assert_eq!(listed.code, Some(0), "stderr: {}", listed.stderr);
    assert_eq!(listed.raw, [format!("{name} 0.0.0 local")]);
    let updated = setup.fiber(&["update", "muse"], None);
    assert_eq!(updated.code, Some(0), "stderr: {}", updated.stderr);
    assert_eq!(updated.stderr, format!("fiber: installed {name}\n"));
    let removed = setup.fiber(&["remove", "muse"], None);
    assert_eq!(removed.code, Some(0), "stderr: {}", removed.stderr);
    assert_eq!(removed.stderr, format!("fiber: removed {name}\n"));
    assert!(setup.fiber(&["list"], None).raw.is_empty());
    let again = setup.fiber(&["remove", "muse"], None);
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
    let run = setup.fiber_env(&["install", "openrouter"], &[("PATH", "")]);
    assert_eq!(run.code, Some(2), "{}", run.stderr);
    assert!(run.stderr.contains("Install git"), "{}", run.stderr);
}

#[test]
fn the_extension_commands_take_their_arguments() {
    let setup = Setup::new();
    for args in [
        vec!["install"],
        vec!["install", "a", "b"],
        vec!["update"],
        vec!["remove"],
        vec!["list", "x"],
    ] {
        let run = setup.fiber(&args, None);
        assert_eq!(run.code, Some(2), "{args:?}: {}", run.stderr);
    }
}

/// Asserts a completed turn that answered the weather question.
fn assert_weather(run: &Run) {
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let step = [
        "step_started",
        "assistant_message_started",
        "assistant_message_delta",
    ];
    let mut kinds = vec!["fiber_started", "session_started", "turn_started"];
    kinds.extend(step);
    kinds.extend([
        "tool_call_arguments_delta",
        "reasoning_started",
        "reasoning_completed",
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
        &[("OPENCODE_API_KEY", "sk-test")],
    );

    assert_weather(&run);
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request.path, "/zen/go/v1/responses");
        assert_eq!(request.header("x-opencode-session"), Some(run.session_id()));
        assert!(request.header("user-agent").unwrap().starts_with("fiber/"));
    }
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
        &[("META_API_KEY", "sk-test")],
    );

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(
        run.kinds(),
        [
            "fiber_started",
            "session_started",
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
    assert_eq!(requests[0].header("x-opencode-session"), None);
    assert!(
        requests[0]
            .header("user-agent")
            .unwrap()
            .starts_with("fiber/")
    );
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
        &[("OPENCODE_API_KEY", "sk-test")],
    );

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), HELLO_KINDS);
    assert_eq!(run.last()["payload"]["text"], "Hello.");
    let requests = server.requests();
    assert_eq!(requests[0].path, "/zen/v1/responses");
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
    for args in [&["install"][..], &["install", "a", "b"][..]] {
        let run = setup.fiber(args, None);
        assert_eq!(run.code, Some(2));
        assert!(
            run.stderr.contains("fiber install <name or path>"),
            "{}",
            run.stderr
        );
    }
    let run = setup.fiber(&["install", "/nonexistent"], None);
    assert_eq!(run.code, Some(1));
    assert!(!setup.home().join("extensions").exists());
}

/// A live turn with `muse-spark-1.3-contributor`, opt in by naming the key
/// file in `var` (`docs/testing.md`, "Live calls and evals"). Prints the
/// event kinds and the final text.
fn live(var: &str, package_name: &str, provider: &str, key_env: &str) {
    let Some(key_file) = std::env::var_os(var) else {
        return;
    };
    let key = fs::read_to_string(key_file).unwrap();
    let setup = Setup::new();
    install(&setup, &package(package_name));

    let model = format!("{provider}/muse-spark-1.3-contributor");
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
        "OPENCODE_API_KEY",
    );
}

#[test]
fn live_muse_completes_one_turn() {
    live("FIBER_LIVE_MUSE_KEY_FILE", "muse", "muse", "META_API_KEY");
}
