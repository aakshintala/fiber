//! A Fiber delegate's final message is written whole to its own
//! `artifacts/final.md`, whatever its length, and the wake, the
//! `jobs wait` result and the stop result name that path
//! (`docs/delegates.md`, "Results").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use crate::support;

use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

use fakes::deadline::WAIT;
use fakes::{ProviderServer, Request, Response, Watchdog};
use serde_json::{Value, json};
use support::{Deadline, KillGroup, SessionGuard, function_call, hello, stream, text_reply};

/// The marker in the wake notice and the `jobs wait` result ahead of the
/// file's absolute path.
const NAMES_FILE: &str = "Full final message: ";

/// Installs provider `name` with model `m` on `openai-responses` at `url`.
fn install_url(setup: &support::Setup, name: &str, url: &str) {
    let source = setup.root.path().join(format!("src-{name}"));
    let mut manifest = json!({"name": name, "version": "v0.0.0", "fiber": "0.0.0", "api": 1});
    manifest["providers"] = Value::Object(
        [(name.to_owned(), json!([format!("{url}/v1")]))]
            .into_iter()
            .collect(),
    );
    support::write_json(&source.join("extension.json"), &manifest);
    let model = json!({"id": "m", "protocol": "openai-responses", "base_url": format!("{url}/v1"), "context_window": 100000});
    support::write_json(
        &source.join(format!("providers/{name}.json")),
        &json!({
            "name": name,
            "credential": {"env": "FIBER_TEST_FAKE_KEY"},
            "models": [model]
        }),
    );
    let request = extensions::Request::Path(source);
    support::install_with_retry(
        &setup.home(),
        setup.deadline.start_wait(),
        &request,
        "0.0.0",
        None,
    );
}

/// Installs the parent's `pa/m` and the child's `pb/m`, a standing allow
/// for the delegate tools, and a session that keeps serving instead of
/// idling out.
fn configure(setup: &support::Setup, parent: &ProviderServer, child: &ProviderServer) {
    install_url(setup, "pa", &parent.url());
    install_url(setup, "pb", &child.url());
    support::write_json(
        &setup.home().join("config.json"),
        &json!({"model": "pa/m", "reviewer": {"model": "pa/m"},
            "session": {"idle_exit_ms": 3600000}}),
    );
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "allow", "tool": "delegate_spawn", "prefix": ""}),
        ),
    )
    .unwrap();
}

/// Arms the watchdog for `setup`: every child holds the test root on its
/// command line, so a failing test leaves no child behind. First, before
/// any fallible step.
fn arm(setup: &support::Setup) -> SessionGuard {
    SessionGuard::arm(setup.deadline, &setup.root.path().to_string_lossy())
}

/// A `delegate_spawn` call `call` for `model` with `prompt`.
fn spawn_call(call: &str, prompt: &str) -> Value {
    function_call(
        call,
        "delegate_spawn",
        &json!({"description": "scan", "prompt": prompt, "model": "fiber:pb/m"}),
    )
}

/// A `jobs` call `call` doing `action` on `job` (waits for at most 30 s).
fn jobs_call(call: &str, action: &str, job: &str) -> Value {
    function_call(
        call,
        "jobs",
        &json!({"action": action, "job_id": job, "timeout_ms": 30000}),
    )
}

/// A `read` call `call` on `path`.
fn read_call(call: &str, path: &str) -> Value {
    function_call(
        call,
        "read",
        &json!({"path": path, "offset": null, "limit": null, "pages": null}),
    )
}

/// The request body as text.
fn body(request: &Request) -> String {
    String::from_utf8_lossy(&request.body).into_owned()
}

/// A session request declares tools; a reviewer request does not.
fn is_session(text: &str) -> bool {
    text.contains(r#""name":"delegate_spawn""#)
}

/// The text between `marker` and the next quote or backslash (a JSON
/// string's end or escape) in a request body.
fn after(text: &str, marker: &str) -> String {
    let start = text
        .find(marker)
        .unwrap_or_else(|| panic!("no {marker:?} in {text}"))
        + marker.len();
    let rest = &text[start..];
    rest[..rest.find(['"', '\\']).unwrap_or(rest.len())].to_owned()
}

/// The path the wake notice in a request body names.
fn wake_path(text: &str) -> String {
    let wake = text
        .find("finished. Its final message:")
        .unwrap_or_else(|| panic!("no wake notice in {text}"));
    after(&text[wake..], NAMES_FILE)
}

/// The job id a spawn receipt in a request body names.
fn receipt_job(text: &str) -> String {
    let marker = "Started delegate ";
    let start = text.find(marker).unwrap() + marker.len();
    text[start..]
        .split([' ', '.', '"', '\\'])
        .next()
        .unwrap()
        .to_owned()
}

/// The text of a `tool_call_completed` for the log's one call of `name`.
fn result_of(lines: &[Value], name: &str) -> String {
    let requested = lines
        .iter()
        .find(|line| line["kind"] == "tool_call_requested" && line["payload"]["name"] == name)
        .unwrap_or_else(|| panic!("no {name} call in {lines:?}"));
    let action = &requested["action_id"];
    let completed = lines
        .iter()
        .find(|line| line["kind"] == "tool_call_completed" && line["action_id"] == *action)
        .unwrap_or_else(|| panic!("no completion of the {name} call in {lines:?}"));
    completed["payload"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// The payload of the log's `n`th `kind` line.
fn nth(lines: &[Value], kind: &str, n: usize) -> Value {
    lines
        .iter()
        .filter(|line| line["kind"] == kind)
        .nth(n)
        .unwrap_or_else(|| panic!("no {kind} number {n} in {lines:?}"))["payload"]
        .clone()
}

/// The sessions directory for `fiber ask`: its workspace is the launch
/// directory, the test root.
fn ask_sessions(setup: &support::Setup) -> PathBuf {
    let workspace = fs::canonicalize(setup.root.path()).unwrap();
    let key = workspace.to_string_lossy().replace('/', "-");
    setup.home().join("projects").join(key).join("sessions")
}

/// The final-message file of the delegate whose `delegate_started` is
/// `started`.
fn final_file(setup: &support::Setup, started: &Value) -> PathBuf {
    ask_sessions(setup)
        .join(started["delegate_session_id"].as_str().unwrap())
        .join("artifacts/final.md")
}

/// Asserts `named` is the absolute path of `file`.
#[track_caller]
fn assert_names(named: &str, file: &Path) {
    assert!(Path::new(named).is_absolute(), "not absolute: {named}");
    assert_eq!(
        fs::canonicalize(named).unwrap_or_else(|e| panic!("{named} does not exist: {e}")),
        fs::canonicalize(file).unwrap(),
        "names another file: {named}"
    );
}

/// A `fiber ask` running in its own process group with a watchdog beside
/// it. Its stdout lines arrive on a channel; the exit arrives on another.
struct Ask {
    group: u32,
    _guard: KillGroup,
    watchdog: Watchdog,
    lines: mpsc::Receiver<Value>,
    exited: mpsc::Receiver<(Option<i32>, String)>,
    taken: Vec<Value>,
}

impl Ask {
    #[track_caller]
    fn start(setup: &support::Setup, prompt: &str) -> Self {
        fakes::warm_up(Path::new(env!("CARGO_BIN_EXE_fiber")), &["--bogus"], 2);
        let mut child = setup.fiber(&["ask", prompt]).spawn().unwrap();
        let group = child.id();
        let guard = KillGroup(group);
        let watchdog = Watchdog::group(group);
        let stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let (tx, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let line = line.unwrap();
                if line.trim().is_empty() {
                    continue;
                }
                match tx.send(serde_json::from_str(&line).unwrap()) {
                    Ok(()) => {}
                    Err(mpsc::SendError(_)) => break,
                }
            }
        });
        let (done, exited) = mpsc::channel();
        thread::spawn(move || {
            let mut text = String::new();
            stderr.read_to_string(&mut text).unwrap();
            let code = child.wait().unwrap().code();
            match done.send((code, text)) {
                Ok(()) | Err(mpsc::SendError(_)) => {}
            }
        });
        Self {
            group,
            _guard: guard,
            watchdog,
            lines,
            exited,
            taken: Vec::new(),
        }
    }

    /// Stdout lines until one satisfies `done`, all under one wait.
    #[track_caller]
    fn until(&mut self, what: &str, done: impl Fn(&Value) -> bool) {
        let wait = Deadline::after(WAIT);
        loop {
            let line = wait.recv_or_fail(&self.lines, what);
            let stop = done(&line);
            self.taken.push(line);
            if stop {
                return;
            }
        }
    }

    /// The exit under the test's wait, then every stdout line.
    #[track_caller]
    fn finish(mut self) -> Vec<Value> {
        let (code, stderr) =
            Deadline::after(WAIT).recv_or_fail(&self.exited, "`fiber ask` to exit");
        assert_eq!(code, Some(0), "stderr: {stderr}");
        assert!(
            !fakes::group_alive(self.group),
            "`fiber ask` left a process in its group behind"
        );
        self.watchdog.stand_down(Deadline::after(WAIT));
        std::mem::forget(self._guard);
        while let Ok(line) = self.lines.try_recv() {
            self.taken.push(line);
        }
        let wait = Deadline::after(WAIT);
        while let Ok(line) = wait.recv(&self.lines) {
            self.taken.push(line);
        }
        self.taken
    }
}

/// The parent's answers to a delegate run: reviewer requests `allow`; the
/// first session request spawns; the receipt's turn ends; once the wake
/// is in the history, `after_wake` answers with the history's text.
fn parent(
    after_wake: impl Fn(&str) -> Response + Send + Sync + 'static,
) -> impl Fn(&Request) -> Response + Send + Sync + 'static {
    move |request: &Request| {
        let text = body(request);
        if !is_session(&text) {
            return text_reply("allow");
        }
        if !text.contains("Started delegate") {
            return stream(&[spawn_call("call_spawn", "say hello")]);
        }
        if !text.contains("finished. Its final message:") {
            return hello();
        }
        after_wake(&text)
    }
}

/// A parent that, once woken, waits for the delegate with `jobs wait`,
/// then reads the file the wake names.
fn waiting_reader() -> impl Fn(&Request) -> Response + Send + Sync + 'static {
    parent(|text| {
        if !text.contains("call_wait") {
            return stream(&[jobs_call("call_wait", "wait", &receipt_job(text))]);
        }
        if !text.contains("call_read") {
            return stream(&[read_call("call_read", &wake_path(text))]);
        }
        hello()
    })
}

/// Runs a parent that spawns one delegate answering `message` and then
/// waits and reads (`waiting_reader`), and checks what every test of a
/// finished delegate checks: the wake and the wait name the delegate's
/// `artifacts/final.md`, which holds `message` whole and which the parent
/// read. Returns the parent's log lines and the wake's text.
fn finished_delegate(message: &str) -> (Vec<Value>, String, PathBuf) {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    let child = ProviderServer::start([text_reply(message)]).unwrap();
    let parent = ProviderServer::start_responding(waiting_reader()).unwrap();
    configure(&setup, &parent, &child);

    let lines = Ask::start(&setup, "scan the tree").finish();
    let started = nth(&lines, "delegate_started", 0);
    let file = final_file(&setup, &started);
    let bytes = fs::read(&file).unwrap_or_else(|e| panic!("no {}: {e}", file.display()));
    assert_eq!(
        bytes,
        message.as_bytes(),
        "the file holds the whole message"
    );

    let job = started["job_id"].as_str().unwrap();
    let waited = result_of(&lines, "jobs");
    let mut wait_lines = waited.lines();
    assert_eq!(
        wait_lines.next(),
        Some(format!("Job {job} completed.").as_str())
    );
    let named = wait_lines.next().unwrap().strip_prefix(NAMES_FILE).unwrap();
    assert_names(named, &file);

    let woken = parent
        .requests()
        .iter()
        .map(body)
        .find(|text| text.contains("call_read"))
        .expect("a parent request after the read");
    assert_names(&wake_path(&woken), &file);
    // The `read` tool bounds its own result near 16 KiB, on a line
    // boundary; a shorter file comes back whole.
    let read = result_of(&lines, "read");
    let kept = message.len().min(4 * 1024);
    assert!(
        read.starts_with(&message[..kept]),
        "the parent reads the file the wake names"
    );
    assert_eq!(read.len() == message.len(), message.len() <= 16 * 1024);
    (lines, woken, file)
}

#[test]
fn a_delegate_final_message_is_written_to_artifacts_final_md() {
    finished_delegate("The scan found three caches.");
}

#[test]
fn a_delegate_final_message_over_16kib_is_full_in_the_file() {
    let message = (0..3000)
        .map(|n| format!("line {n} of the scan"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(message.len() > 16 * 1024);
    let (lines, woken, file) = finished_delegate(&message);
    // The wake and the logged finish carry the cut text; the file has it all.
    let finished = nth(&lines, "delegate_finished", 0);
    assert!(finished["text"].as_str().unwrap().len() < message.len());
    assert!(
        finished["text"]
            .as_str()
            .unwrap()
            .contains(&file.to_string_lossy().into_owned()),
        "the cut notice names the file: {finished}"
    );
    assert!(
        woken.contains("bytes cut."),
        "the wake carries the cut notice"
    );
}

#[test]
fn a_chained_delegate_reads_the_named_final_file() {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    let line = "The scan found three caches.";
    // The first delegate answers `line`; the second is told a path, reads
    // it and answers with what it read.
    let child = ProviderServer::start_responding(move |request: &Request| {
        let text = body(request);
        let parsed: Value = serde_json::from_slice(&request.body).unwrap();
        let read = parsed["input"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|item| item["type"] == "function_call_output");
        if let Some(read) = read {
            return text_reply(read["output"].as_str().unwrap());
        }
        if text.contains("READ_AND_ECHO:") {
            return stream(&[read_call(
                "call_child_read",
                &after(&text, "READ_AND_ECHO:"),
            )]);
        }
        text_reply(line)
    })
    .unwrap();
    let parent = ProviderServer::start_responding(|request: &Request| {
        let text = body(request);
        if !is_session(&text) {
            return text_reply("allow");
        }
        let started = text.matches("Started delegate").count();
        let woken = text.matches("finished. Its final message:").count();
        match (started, woken) {
            (0, _) => stream(&[spawn_call("call_first", "say hello")]),
            (1, 1) => stream(&[spawn_call(
                "call_second",
                &format!("READ_AND_ECHO:{}", wake_path(&text)),
            )]),
            _ => hello(),
        }
    })
    .unwrap();
    configure(&setup, &parent, &child);

    let lines = Ask::start(&setup, "scan the tree").finish();
    let first = final_file(&setup, &nth(&lines, "delegate_started", 0));
    assert_eq!(fs::read_to_string(&first).unwrap(), line);
    let second = nth(&lines, "delegate_finished", 1);
    assert_eq!(
        second["text"], line,
        "the second delegate read the first's file"
    );
}

#[test]
fn a_stopped_delegate_final_message_is_written_before_the_stop_returns() {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    let child = ProviderServer::start([stall()]).unwrap();
    // Held until the child stalls: the stop then meets a running delegate,
    // whatever the scheduling.
    let parent = ProviderServer::start_responding(|request: &Request| {
        let text = body(request);
        if !is_session(&text) {
            return text_reply("allow");
        }
        if !text.contains("Started delegate") {
            return stream(&[spawn_call("call_spawn", "say hello")]);
        }
        if text.contains("call_stop") {
            return hello();
        }
        stream(&[jobs_call("call_stop", "stop", &receipt_job(&text))])
    })
    .unwrap();
    parent.hold();
    configure(&setup, &parent, &child);

    let mut ask = Ask::start(&setup, "scan the tree");
    assert!(
        parent.await_requests(1, setup.deadline),
        "the spawn call was requested"
    );
    parent.release_one();
    assert!(
        parent.await_requests(2, setup.deadline),
        "the verdict was requested"
    );
    parent.release_one();
    assert!(
        parent.await_requests(3, setup.deadline),
        "the stop was requested"
    );
    assert!(
        child.await_partial(1, setup.deadline),
        "the child stalled its model call"
    );
    parent.release();
    ask.until("the stop call to complete", |line| {
        line["kind"] == "tool_call_completed" && is_stop_result(line)
    });
    let lines = ask.finish();

    let started = nth(&lines, "delegate_started", 0);
    let job = started["job_id"].as_str().unwrap();
    let file = final_file(&setup, &started);
    let stop = lines
        .iter()
        .find(|line| line["kind"] == "tool_call_requested" && line["payload"]["name"] == "jobs")
        .unwrap()["action_id"]
        .clone();
    // The stop, not the notice, claimed the finish.
    let finished = lines
        .iter()
        .find(|line| line["kind"] == "delegate_finished")
        .unwrap();
    assert_eq!(finished["action_id"], stop, "the stop claimed the finish");
    let payload = &finished["payload"];
    let bytes = fs::read(&file).unwrap_or_else(|e| panic!("no {}: {e}", file.display()));
    assert_eq!(
        bytes,
        payload["text"].as_str().unwrap().as_bytes(),
        "the file holds the logged text"
    );
    assert_eq!(payload["artifact"], "artifacts/final.md");
    assert_names(payload["artifact_absolute"].as_str().unwrap(), &file);

    let result = result_of(&lines, "jobs");
    let mut result_lines = result.lines();
    assert_eq!(
        result_lines.next(),
        Some(format!("Job {job} cancelled.").as_str())
    );
    let named = result_lines
        .next()
        .unwrap()
        .strip_prefix(NAMES_FILE)
        .unwrap();
    assert_names(named, &file);
    // The notice's claim was refused: no wake reached the parent.
    assert!(
        parent
            .requests()
            .iter()
            .all(|request| !body(request).contains("finished. Its final message:")),
        "the parent got a wake for a finish the stop claimed"
    );
}

/// Whether `line` completes the parent's `jobs` stop.
fn is_stop_result(line: &Value) -> bool {
    line["payload"]["content"][0]["text"]
        .as_str()
        .is_some_and(|text| text.starts_with("Job ") && text.contains(" cancelled."))
}

/// A child that stalls mid-reply: the parent's stop is what finishes it.
fn stall() -> Response {
    let prefix = "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"Working\"}\n\n";
    Response::stall(200, prefix, prefix.len() + 100000).header("content-type", "text/event-stream")
}
