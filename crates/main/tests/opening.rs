//! Binary-level tests of extension sections in the opening message
//! (`docs/system-prompt.md`, "Extension sections"; `docs/testing.md`,
//! "Levels"): the built `fiber` runs in its own process group with its own
//! `FIBER_HOME`, holding an ordinary provider whose base URL is the fake
//! server, and a data-only extension whose manifest names section files.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;

use fakes::{ProviderServer, Response, Watchdog};
use serde_json::{Value, json};
use support::Deadline;

/// The fixture extension's name.
const FIXTURE: &str = "fiber.test/notes";

/// A temporary root holding Fiber home and the workspace, removed on drop.
/// Its name is short: a session's socket path must fit in 103 bytes on
/// macOS.
struct Setup {
    root: fakes::TempDir,
    deadline: Deadline,
}

impl Setup {
    fn new() -> Self {
        let deadline = Deadline::start();
        let root = fakes::TempDir::new("fh");
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

    /// Installs `fiber.test/<short>` as a data-only extension whose
    /// manifest `opening` is `opening`.
    fn section_fixture(&self, short: &str, opening: &Value) {
        let source = self.root.path().join("src").join(short);
        write(
            &source.join("extension.json"),
            &json!({"name": format!("fiber.test/{short}"), "version": "v1.0.0",
                "fiber": "0.1.0", "api": 1, "opening": opening})
            .to_string(),
        );
        extensions::plan(
            &self.home(),
            &extensions::Request::Path(source),
            "0.1.0",
            &extensions::Origin::github(),
            &*fakes::clock::FakeClock::new(),
        )
        .unwrap()
        .commit()
        .unwrap();
    }

    /// Installs `fiber.test/<short>` as a data-only extension whose
    /// manifest names `prompt.md`, holding `text`.
    fn prompt_fixture(&self, short: &str, text: &str) {
        let source = self.root.path().join("src").join(short);
        write(
            &source.join("extension.json"),
            &json!({"name": format!("fiber.test/{short}"), "version": "v1.0.0",
                "fiber": "0.1.0", "api": 1, "prompt": "prompt.md"})
            .to_string(),
        );
        write(&source.join("prompt.md"), text);
        extensions::plan(
            &self.home(),
            &extensions::Request::Path(source),
            "0.1.0",
            &extensions::Origin::github(),
            &*fakes::clock::FakeClock::new(),
        )
        .unwrap()
        .commit()
        .unwrap();
    }

    /// Writes `content` to the fixture's machine data directory.
    fn machine_file(&self, name: &str, content: &str) -> PathBuf {
        let slug = config::dir_name(FIXTURE);
        let path = self.home().join("data").join(slug).join(name);
        write(&path, content);
        path
    }

    /// Installs a provider `fake` with model `m` on `openai-responses` at the
    /// fake server, and writes configuration naming `fake/m` with `extra`.
    fn provider(&self, server: &ProviderServer, extra: &Value) {
        let source = self.root.path().join("src").join("fake");
        write(
            &source.join("extension.json"),
            &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}).to_string(),
        );
        write(
            &source.join("providers/fake.json"),
            &json!({
                "name": "fake",
                "credential": {"env": "FIBER_TEST_FAKE_KEY"},
                "models": [{"id": "m", "protocol": "openai-responses", "base_url": format!("{}/v1", server.url()), "context_window": 100000}]
            })
            .to_string(),
        );
        extensions::plan(
            &self.home(),
            &extensions::Request::Path(source),
            "0.1.0",
            &extensions::Origin::github(),
            &*fakes::clock::FakeClock::new(),
        )
        .unwrap()
        .commit()
        .unwrap();
        let mut config = json!({"model": "fake/m"});
        for (key, value) in extra.as_object().into_iter().flatten() {
            config[key] = value.clone();
        }
        write(&self.home().join("config.json"), &config.to_string());
    }

    /// Runs `fiber` in its own process group, waits for it under the
    /// test's [`Deadline`], and asserts that nothing it started is left in the
    /// group. A watchdog beside it kills that group if this process dies
    /// first.
    fn run(&self, args: &[&str]) -> Run {
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
        Run::from(output)
    }

    /// Starts a server answering `Hello.`, installs the provider, and runs
    /// `fiber ask`.
    fn ask(&self, config: &Value) -> (Run, ProviderServer) {
        let server = ProviderServer::start([hello()]).unwrap();
        self.provider(&server, config);
        let run = self.run(&["ask", "hi"]);
        assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
        (run, server)
    }
}

fn write(file: &Path, text: &str) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, text).unwrap();
}

// debt: the runner, `spawn_watched` and `KillGroup` copy `tools.rs`'s, as
// the six other binary test files in this crate each do; move all seven
// copies into `fakes` together when a change to one has to be made in all.

/// Spawns `command` in a new process group, then a watchdog in its own
/// group, which kills the group if this process dies first.
fn spawn_watched(command: &mut Command) -> (Child, Watchdog) {
    let child = command.process_group(0).spawn().unwrap();
    let group = child.id();
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
        support::kill_group_detached(self.0, "KILL");
    }
}

/// One finished run: its exit code, stdout's lines parsed, and stderr.
struct Run {
    code: Option<i32>,
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
        let lines = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .filter(|l| !is_status(l))
            .map(|line| serde_json::from_str(line).unwrap_or(Value::Null))
            .collect();
        Self {
            code: output.status.code(),
            lines,
            stderr: String::from_utf8(output.stderr).unwrap(),
        }
    }
}

impl Run {
    fn kinds(&self) -> Vec<&str> {
        self.lines
            .iter()
            .map(|line| line["kind"].as_str().unwrap())
            .collect()
    }

    fn first(&self, kind: &str) -> &Value {
        self.lines
            .iter()
            .find(|line| line["kind"] == kind)
            .unwrap_or_else(|| panic!("no {kind} line"))
    }
}

/// An `openai-responses` stream answering `Hello.`.
fn hello() -> Response {
    let mut body = String::new();
    for event in [
        json!({"type": "response.output_text.delta", "delta": "Hello."}),
        json!({"type": "response.output_item.done", "item": {
            "type": "message", "content": [{"type": "output_text", "text": "Hello."}]
        }}),
    ] {
        body.push_str(&format!(
            "event: {}\ndata: {event}\n\n",
            event["type"].as_str().unwrap()
        ));
    }
    let done = json!({"type": "response.completed", "response": {
        "id": "resp_1", "status": "completed",
        "usage": {"input_tokens": 10, "input_tokens_details": {"cached_tokens": 4}, "output_tokens": 3}
    }});
    body.push_str(&format!(
        "event: {}\ndata: {done}\n\n",
        done["type"].as_str().unwrap()
    ));
    Response::stream(body)
}

/// The event kinds of a turn answered by [`hello`]: one text fragment.
const HELLO_KINDS: [&str; 14] = [
    "session_started",
    "fiber_started",
    "extensions_loaded",
    "preamble_built",
    "opening_message",
    "turn_started",
    "step_started",
    "assistant_message_started",
    "assistant_message_delta",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "fiber_exited",
];

#[test]
fn an_installed_section_is_recorded_with_its_extension_in_order_and_sent() {
    let setup = Setup::new();
    setup.section_fixture("notes", &json!({"machine": ["b.md", "a.md"]}));
    let a = setup.machine_file("a.md", "First.\n");
    let b = setup.machine_file("b.md", "Second.\n");
    let (run, server) = setup.ask(&json!({}));
    assert_eq!(run.kinds(), HELLO_KINDS);

    // `opening_message` records both files, in manifest order, with the
    // extension; with no budget the key is absent.
    let opening = run.first("opening_message");
    assert_eq!(
        opening["payload"]["extension_sections"],
        json!([{
            "extension": FIXTURE,
            "files": [
                {"path": b.display().to_string(), "content": "Second.\n"},
                {"path": a.display().to_string(), "content": "First.\n"},
            ],
        }])
    );
    // The first request sends the section after the instruction files.
    let body = String::from_utf8(server.requests()[0].body.clone()).unwrap();
    let heading = body
        .find("# From the fiber.test/notes extension")
        .expect("the section heading");
    assert!(body.contains("###"));
    assert!(body.find("Second.").unwrap() > heading);
    assert!(body.find("First.").unwrap() > heading);
    assert!(!body.contains("Prune"), "{body}");
}

#[test]
fn a_disabled_section_extension_has_no_section() {
    let setup = Setup::new();
    setup.section_fixture("notes", &json!({"machine": ["a.md"]}));
    setup.machine_file("a.md", "First.\n");
    let (run, server) = setup.ask(&json!({"extensions": {FIXTURE: {"enabled": false}}}));
    assert_eq!(run.kinds(), HELLO_KINDS);

    let opening = run.first("opening_message");
    assert!(opening["payload"].get("extension_sections").is_none());
    let body = String::from_utf8(server.requests()[0].body.clone()).unwrap();
    assert!(!body.contains("fiber.test/notes"), "{body}");
}

#[test]
fn a_removed_section_extension_has_no_section() {
    let setup = Setup::new();
    setup.section_fixture("notes", &json!({"machine": ["a.md"]}));
    setup.machine_file("a.md", "First.\n");
    extensions::removal(&setup.home(), FIXTURE, &*fakes::clock::FakeClock::new())
        .unwrap()
        .commit()
        .unwrap();
    let (run, server) = setup.ask(&json!({}));
    assert_eq!(run.kinds(), HELLO_KINDS);

    let opening = run.first("opening_message");
    assert!(opening["payload"].get("extension_sections").is_none());
    let body = String::from_utf8(server.requests()[0].body.clone()).unwrap();
    assert!(!body.contains("fiber.test/notes"), "{body}");
}

#[test]
fn an_over_budget_section_ends_with_the_prune_line() {
    let setup = Setup::new();
    setup.section_fixture("notes", &json!({"machine": ["big.md"], "budget_bytes": 5}));
    setup.machine_file("big.md", "123456");
    let (run, server) = setup.ask(&json!({}));
    assert_eq!(run.kinds(), HELLO_KINDS);

    let opening = run.first("opening_message");
    assert_eq!(
        opening["payload"]["extension_sections"],
        json!([{
            "extension": FIXTURE,
            "files": [{"path": setup.home().join("data/fiber.test-notes/big.md").display().to_string(), "content": "123456"}],
            "budget_bytes": 5,
        }])
    );
    let body = String::from_utf8(server.requests()[0].body.clone()).unwrap();
    assert!(
        body.contains("Fiber: these files are 6 bytes, over their budget of 5 bytes. Prune them."),
        "{body}"
    );
}

#[test]
fn the_model_addendum_and_the_extension_prompt_reach_the_system_prompt() {
    let setup = Setup::new();
    setup.prompt_fixture("guide", "Read before shell.\n");
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server, &json!({}));
    // The installed model names an addendum file holding its own text.
    write(
        &setup.home().join("extensions/fake/providers/fake.json"),
        &json!({
            "name": "fake",
            "credential": {"env": "FIBER_TEST_FAKE_KEY"},
            "models": [{"id": "m", "protocol": "openai-responses",
                        "base_url": format!("{}/v1", server.url()), "context_window": 100000,
                        "prompt_addendum": "prompts/m.md"}]
        })
        .to_string(),
    );
    write(
        &setup.home().join("extensions/fake/prompts/m.md"),
        "Answer with a haiku.\n",
    );
    let run = setup.run(&["ask", "hi"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), HELLO_KINDS);

    // `preamble_built` records the system prompt as sent: the model's
    // addendum after the model line, and the extension's text under a
    // heading naming it.
    let system = run.first("preamble_built")["payload"]["system_prompt"]
        .as_str()
        .unwrap();
    assert!(system.contains("Answer with a haiku."), "{system}");
    assert!(system.contains("Read before shell."), "{system}");
    assert!(system.contains("fiber.test/guide"), "{system}");
}
