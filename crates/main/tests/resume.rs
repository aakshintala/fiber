//! Binary-level tests of `fiber ask --resume` (`docs/testing.md`, "Levels"):
//! the built `fiber` runs in its own process group with its own
//! `FIBER_HOME`, holding an ordinary provider whose base URL is the fake
//! server.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test helpers; a failure is the test's; a live test prints its outcome"
)]

mod support;

use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;

use contract::events::{
    Event, SessionStarted, ToolCallRequested, ToolCallStarted, TurnStarted, Variables,
    VariablesSource,
};
use contract::shapes::{ContentPart, DeclaredEffects, Origin, Sender};
use contract::{ActionId, CommandId, SessionId, TurnId};
use fakes::{ProviderServer, Request, Response};
use serde_json::{Value, json};
use support::{
    Deadline, Setup, first_line, function_call, group_alive, hello_inline_completed as hello,
    is_status, stream, text_reply,
};

impl Setup {
    fn new_with_fa_root() -> Self {
        let deadline = Deadline::start();
        let root = fakes::TempDir::new("fa");
        fs::create_dir_all(root.path().join("h")).unwrap();
        fs::create_dir_all(root.path().join("w")).unwrap();
        Self { deadline, root }
    }

    /// Installs a provider `fake` with `models` on `openai-responses` at the
    /// fake server, and makes `default` the configured model.
    fn provider_models(&self, server: &ProviderServer, models: &[&str], default: &str) {
        let source = self.root.path().join("src");
        let ids: Vec<Value> = models
            .iter()
            .map(|m| {
                json!({"id": m, "protocol": "openai-responses", "base_url": format!("{}/v1", server.url()), "context_window": 100000})
            })
            .collect();
        write(
            &source.join("extension.json"),
            &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}),
        );
        write(
            &source.join("providers/fake.json"),
            &json!({
                "name": "fake",
                "credential": {"env": "FIBER_TEST_FAKE_KEY"},
                "models": ids,
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
        write(&self.home().join("config.json"), &json!({"model": default}));
    }

    /// The project's sessions directory.
    fn sessions(&self) -> PathBuf {
        let workspace = fs::canonicalize(self.workspace()).unwrap();
        let key = workspace.to_string_lossy().replace('/', "-");
        self.home().join("projects").join(key).join("sessions")
    }

    /// Runs `fiber` with `args` in its own process group, waiting under the
    /// test's [`Deadline`]. A watchdog beside it kills that group if this process
    /// dies first.
    #[track_caller]
    fn run_in_workspace(&self, args: &[&str]) -> Run {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(args)
            .current_dir(self.workspace())
            .env_clear()
            .envs(fakes::check_run())
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
        watchdog.stand_down(self.deadline.cleanup());
        Run::from(output)
    }
}

fn write(file: &Path, value: &Value) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, value.to_string()).unwrap();
}

/// Spawns `command` in a new process group, then a watchdog in its own
/// group.
fn spawn_watched(command: &mut Command) -> (std::process::Child, fakes::Watchdog) {
    let child = command.process_group(0).spawn().unwrap();
    let group = child.id();
    let guard = KillGroup(group);
    let watchdog = fakes::Watchdog::group(group);
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
        let raw: Vec<String> = stdout
            .lines()
            .filter(|l| !is_status(l))
            .map(str::to_owned)
            .collect();
        let lines = raw
            .iter()
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

    /// Stdout's durable lines for its session, one per line with the newline.
    fn durable(&self) -> String {
        self.raw
            .iter()
            .zip(&self.lines)
            .filter(|(_, l)| {
                l.get("seq").is_some() && l["session_id"] == self.lines[0]["session_id"]
            })
            .map(|(raw, _)| format!("{raw}\n"))
            .collect()
    }
}

/// The one line stdout holds when the process failed before any session.
fn assert_pre_session(run: &Run, exit: i32, code: &str) {
    assert_eq!(run.code, Some(exit), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), ["fiber_exited"]);
    let line = run.last();
    assert_eq!(line.get("session_id"), None);
    assert_eq!(line["payload"]["exit_code"], exit);
    assert_eq!(line["payload"]["error"]["code"], code);
    let message = line["payload"]["error"]["message"].as_str().unwrap();
    assert_eq!(run.stderr, format!("fiber: {message}\n"));
}

/// A session log left by hand: `session_started`, then `events`.
fn hand_built(setup: &Setup, id: &str, events: Vec<(Event, Option<TurnId>, Option<ActionId>)>) {
    let sessions = setup.sessions();
    let log = log::Log::create(
        &sessions,
        SessionId(id.into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    log.append(
        &Event::SessionStarted(SessionStarted {
            workspace: fs::canonicalize(setup.workspace())
                .unwrap()
                .display()
                .to_string(),
            variables: Variables {
                path: String::new(),
                names: Vec::new(),
                source: VariablesSource::Inherited,
            },
            parent: None,
            forked_from: None,
            rewind: None,
            worktree: None,
        }),
        None,
        None,
    )
    .unwrap();
    for (event, turn, action) in events {
        log.append(&event, turn, action).unwrap();
    }
}

fn turn_started(text: &str) -> Event {
    Event::TurnStarted(TurnStarted {
        input: vec![contract::events::InputItem::Message {
            content: vec![ContentPart::Text { text: text.into() }],
            sender: Sender {
                origin: Origin::Driver,
                command_id: Some(CommandId("c_1".into())),
            },
            changed_by: None,
        }],
    })
}

fn requested(name: &str) -> Event {
    Event::ToolCallRequested(ToolCallRequested {
        name: name.into(),
        arguments: json!({"city": "Paris"}),
        provider_id: None,
        repair: None,
        ran_by: None,
        provider_item: None,
    })
}

fn started() -> Event {
    Event::ToolCallStarted(ToolCallStarted {
        declared: DeclaredEffects {
            effects: Vec::new(),
            reversible: true,
            paths: None,
        },
        arguments: None,
        changed_by: None,
    })
}

fn t() -> TurnId {
    TurnId("t_1".into())
}

fn a(id: &str) -> ActionId {
    ActionId(id.into())
}

#[test]
fn a_second_ask_with_a_unique_prefix_continues_the_session() {
    let setup = Setup::new_with_fa_root();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);

    let first = setup.run_in_workspace(&["ask", "one"]);
    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
    assert_eq!(
        first.kinds(),
        [
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
        ]
    );
    let id = first.session_id().to_owned();
    let prefix = &id[..8];

    let second = setup.run_in_workspace(&["ask", "--resume", prefix, "two"]);
    assert_eq!(second.code, Some(0), "stderr: {}", second.stderr);
    assert_eq!(second.session_id(), id);
    assert_eq!(second.lines[0]["payload"]["resumed"], true);
    // No `session_started`: the session keeps its first line. No
    // `opening_message` either: the log already holds one.
    assert_eq!(
        second.kinds(),
        [
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
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
        ]
    );

    // Stdout's durable lines are the log's tail, byte for byte, and `seq`
    // continues across the two processes.
    let log = fs::read_to_string(setup.sessions().join(&id).join("events.jsonl")).unwrap();
    assert!(log.starts_with(&first.durable()));
    assert_eq!(&log[first.durable().len()..], &second.durable());
    let seqs: Vec<u64> = log
        .lines()
        .map(|l| {
            serde_json::from_str::<Value>(l).unwrap()["seq"]
                .as_u64()
                .unwrap()
        })
        .collect();
    assert_eq!(seqs, (0..seqs.len() as u64).collect::<Vec<_>>());

    // The second request holds the first turn and the new prompt.
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    let body = String::from_utf8_lossy(&requests[1].body);
    assert!(body.contains("one"), "{body}");
    assert!(body.contains("Hello."), "{body}");
    assert!(body.contains("two"), "{body}");
}

#[test]
fn a_resumed_run_sends_the_fixed_results_and_writes_no_call_started() {
    let setup = Setup::new_with_fa_root();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    hand_built(
        &setup,
        "s_fixed1",
        vec![
            (turn_started("one"), Some(t()), None),
            (requested("search"), Some(t()), Some(a("a_1"))),
            (started(), Some(t()), Some(a("a_1"))),
            (requested("read"), Some(t()), Some(a("a_2"))),
        ],
    );

    let run = setup.run_in_workspace(&["ask", "--resume", "s_fixed1", "two"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.session_id(), "s_fixed1");
    assert_eq!(run.lines[0]["payload"]["resumed"], true);
    assert_eq!(
        run.kinds(),
        [
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
        ]
    );

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    let body = String::from_utf8_lossy(&requests[0].body);
    assert!(
        body.contains("Its outcome is unknown: it may have run."),
        "{body}"
    );
    assert!(body.contains("It never ran."), "{body}");

    // The resumed run re-runs nothing: the log's only `tool_call_started`
    // is the crash's own.
    let log = fs::read_to_string(setup.sessions().join("s_fixed1").join("events.jsonl")).unwrap();
    let kinds: Vec<String> = log
        .lines()
        .map(|l| {
            serde_json::from_str::<Value>(l).unwrap()["kind"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "session_started",
            "turn_started",
            "tool_call_requested",
            "tool_call_started",
            "tool_call_requested",
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
    );
}

#[test]
fn the_logs_last_model_beats_the_flag_and_the_default() {
    let setup = Setup::new_with_fa_root();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider_models(&server, &["m1", "m2"], "fake/m1");
    hand_built(&setup, "s_model1", vec![]);
    // The session used `fake/m2` before the crash.
    {
        let sessions = setup.sessions();
        let log = log::Log::open(
            &sessions,
            SessionId("s_model1".into()),
            fakes::clock::FakeClock::new(),
        )
        .unwrap();
        log.append(
            &Event::ModelChanged(contract::events::ModelChanged {
                before: contract::events::ModelSettings {
                    model: "fake/m1".into(),
                    thinking: None,
                    cache_lifetime: contract::events::CacheLifetime::OneHour,
                    credential: None,
                },
                after: contract::events::ModelSettings {
                    model: "fake/m2".into(),
                    thinking: None,
                    cache_lifetime: contract::events::CacheLifetime::OneHour,
                    credential: None,
                },
                source: contract::events::SwitchSource::Driver,
            }),
            Some(t()),
            Some(a("a_1")),
        )
        .unwrap();
    }

    let run = setup.run_in_workspace(&["ask", "--resume", "s_model1", "--model", "fake/m1", "two"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(
        run.kinds(),
        [
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
        ]
    );

    let requests: Vec<Request> = server.requests();
    assert_eq!(requests.len(), 1);
    let body = String::from_utf8_lossy(&requests[0].body);
    assert!(body.contains("\"model\":\"m2\""), "{body}");
}

#[test]
fn resume_failures_end_stdout_with_a_pre_session_fiber_exited() {
    let setup = Setup::new_with_fa_root();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    // Two sessions sharing the prefix `s_`: the id is `s_` plus hex, so a
    // manual pair under it is ambiguous.
    for id in ["s_aaa", "s_aab"] {
        hand_built(&setup, id, vec![]);
    }

    // `--resume` with no value, and with an empty value, are usage errors.
    assert_pre_session(&setup.run_in_workspace(&["ask", "--resume"]), 2, "usage");
    assert_pre_session(
        &setup.run_in_workspace(&["ask", "--resume", ""]),
        2,
        "usage",
    );
    // An ambiguous prefix is a usage error naming the matches.
    let ambiguous = setup.run_in_workspace(&["ask", "--resume", "s_aa", "x"]);
    assert_pre_session(&ambiguous, 2, "usage");
    assert!(ambiguous.stderr.contains("s_aaa"), "{}", ambiguous.stderr);
    assert!(ambiguous.stderr.contains("s_aab"), "{}", ambiguous.stderr);
    // An unknown id is `session_not_found`.
    assert_pre_session(
        &setup.run_in_workspace(&["ask", "--resume", "s_nope", "x"]),
        1,
        "session_not_found",
    );
    assert!(server.requests().is_empty());
}

#[test]
fn a_held_session_fails_session_held() {
    let setup = Setup::new_with_fa_root();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let first = setup.run_in_workspace(&["ask", "one"]);
    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
    assert_eq!(
        first.kinds(),
        [
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
        ]
    );
    let id = first.session_id().to_owned();
    assert_eq!(server.requests().len(), 1);

    // The test holds the log's lock, as a live session would.
    let _held = log::Log::open(
        &setup.sessions(),
        SessionId(id.clone()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    let run = setup.run_in_workspace(&["ask", "--resume", &id, "two"]);
    assert_pre_session(&run, 1, "session_held");
    assert!(run.stderr.contains(&id), "{}", run.stderr);
    // The failed resume sent nothing: the only request is the first run's.
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn a_failure_before_the_session_leaves_the_log_untouched() {
    let setup = Setup::new_with_fa_root();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let first = setup.run_in_workspace(&["ask", "one"]);
    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
    assert_eq!(
        first.kinds(),
        [
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
        ]
    );
    let id = first.session_id().to_owned();
    let events = setup.sessions().join(&id).join("events.jsonl");
    let before = fs::read(&events).unwrap();
    // The credential the session used is gone: resolving the model fails
    // before any session line is written.
    let source = setup.home().join("extensions/fake/providers/fake.json");
    let text = fs::read_to_string(&source)
        .unwrap()
        .replace("FIBER_TEST_FAKE_KEY", "FIBER_TEST_UNSET_KEY");
    fs::write(&source, text).unwrap();

    let run = setup.run_in_workspace(&["ask", "--resume", &id, "two"]);
    assert_pre_session(&run, 1, "credential_missing");
    assert_eq!(fs::read(&events).unwrap(), before);
    assert!(setup.sessions().join(&id).is_dir());
}

#[test]
fn a_failed_resumed_turn_prints_its_error_on_stderr() {
    let setup = Setup::new_with_fa_root();
    let server = ProviderServer::start([hello(), Response::status(503, "{}")]).unwrap();
    setup.provider(&server);
    write(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "retry": {"attempts": 0}}),
    );

    let first = setup.run_in_workspace(&["ask", "one"]);
    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
    assert_eq!(first.stderr, "");
    let id = first.session_id().to_owned();

    let run = setup.run_in_workspace(&["ask", "--resume", &id, "two"]);
    assert_eq!(run.code, Some(1), "stderr: {}", run.stderr);
    assert_eq!(
        run.kinds(),
        [
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
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
    assert_eq!(exited["exit_code"], 1);
    assert_eq!(exited["error"]["code"], "provider_unavailable");
    let message = exited["error"]["message"].as_str().unwrap();
    assert_eq!(run.stderr, format!("fiber: {message}\n"));
}

/// Sets `providers.fake.credential` to `label` and stores `labels` as
/// `credentials/fake/<label>`, each holding `<label>-key`.
fn labels(setup: &Setup, label: &str, stored: &[&str]) {
    for name in stored {
        config::store_credential(
            &setup.home(),
            "fake",
            name,
            &config::Secret::new(format!("{name}-key\n")),
        )
        .unwrap();
    }
    write(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "providers": {"fake": {"credential": label}}}),
    );
}

/// The kinds of one successful `ask`: a new session, or a resume of one.
fn ask_kinds(resumed: bool) -> Vec<&'static str> {
    let mut kinds = if resumed {
        vec!["fiber_started", "extensions_loaded", "preamble_built"]
    } else {
        vec![
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
            "opening_message",
        ]
    };
    kinds.extend([
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
    ]);
    kinds
}

fn preamble_label(run: &Run) -> Value {
    let built = run.lines.iter().find(|l| l["kind"] == "preamble_built");
    built.unwrap()["payload"]["credential"].clone()
}

#[test]
fn a_new_session_uses_and_records_the_label_the_configuration_names() {
    let setup = Setup::new_with_fa_root();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    labels(&setup, "work", &["work", "other"]);

    let run = setup.run_in_workspace(&["ask", "one"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), ask_kinds(false));
    assert_eq!(preamble_label(&run), "work");
    let requests = server.requests();
    assert_eq!(
        requests[0].header("authorization"),
        Some(fakes::fingerprint("Bearer work-key").as_str())
    );
}

#[test]
fn a_session_with_no_label_set_uses_the_default_label() {
    let setup = Setup::new_with_fa_root();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);

    let run = setup.run_in_workspace(&["ask", "one"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), ask_kinds(false));
    assert_eq!(preamble_label(&run), "default");
    assert_eq!(
        server.requests()[0].header("authorization"),
        Some(fakes::fingerprint("Bearer sk-test").as_str())
    );
}

#[test]
fn a_resumed_session_keeps_its_label_when_the_configuration_changes() {
    let setup = Setup::new_with_fa_root();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    labels(&setup, "work", &["work", "other"]);
    let first = setup.run_in_workspace(&["ask", "one"]);
    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
    let id = first.session_id().to_owned();

    labels(&setup, "other", &[]);
    let second = setup.run_in_workspace(&["ask", "--resume", &id, "two"]);
    assert_eq!(second.code, Some(0), "stderr: {}", second.stderr);
    assert_eq!(first.kinds(), ask_kinds(false));
    assert_eq!(second.kinds(), ask_kinds(true));
    assert_eq!(preamble_label(&second), "work");
    let requests = server.requests();
    assert_eq!(
        requests[1].header("authorization"),
        Some(fakes::fingerprint("Bearer work-key").as_str())
    );
}

#[test]
fn a_resume_whose_label_no_longer_exists_fails_before_the_session() {
    let setup = Setup::new_with_fa_root();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    labels(&setup, "work", &["work", "other"]);
    let first = setup.run_in_workspace(&["ask", "one"]);
    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
    assert_eq!(first.kinds(), ask_kinds(false));
    let id = first.session_id().to_owned();
    let events = setup.sessions().join(&id).join("events.jsonl");
    let before = fs::read(&events).unwrap();

    fs::remove_file(setup.home().join("credentials/fake/work")).unwrap();
    labels(&setup, "other", &[]);
    let run = setup.run_in_workspace(&["ask", "--resume", &id, "two"]);
    assert_pre_session(&run, 1, "credential_missing");
    assert!(
        run.stderr.contains("credentials/fake/work")
            && run
                .stderr
                .contains("The labels for `fake` are: default, other."),
        "{}",
        run.stderr
    );
    assert_eq!(fs::read(&events).unwrap(), before);
}

/// A `fiber ask` still running, with its stdout kept drained and its stderr
/// kept for the failure, if any.
struct Running {
    child: Child,
    watchdog: fakes::Watchdog,
    group: u32,
    guard: KillGroup,
    stdout: mpsc::Receiver<String>,
    stderr: mpsc::Receiver<String>,
    deadline: Deadline,
}

/// Starts `fiber` with `args` in its own process group, as [`Setup::fiber`]
/// runs it, but returns before it exits.
#[track_caller]
fn start(setup: &Setup, args: &[&str]) -> Running {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
    command
        .args(args)
        .current_dir(setup.workspace())
        .env_clear()
        .envs(fakes::check_run())
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", setup.root.path())
        .env("FIBER_HOME", setup.home())
        .env("FIBER_TEST_FAKE_KEY", "sk-test")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let (mut child, watchdog) = spawn_watched(&mut command);
    let group = child.id();
    let guard = KillGroup(group);
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let (err_tx, err_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut buf = String::new();
        match std::io::Read::read_to_string(&mut reader, &mut buf) {
            Ok(_) | Err(_) => {}
        }
        match err_tx.send(buf) {
            Ok(()) | Err(_) => {}
        }
    });
    let (tx, stdout_rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            match tx.send(line.unwrap()) {
                Ok(()) => {}
                Err(mpsc::SendError(_)) => break,
            }
        }
    });
    Running {
        child,
        watchdog,
        group,
        guard,
        stdout: stdout_rx,
        stderr: err_rx,
        deadline: setup.deadline,
    }
}

/// Reads `stdout` until a `clients` line arrives, each line taking what
/// remains of the test's [`Deadline`].
#[track_caller]
fn until_clients(deadline: Deadline, stdout: &mpsc::Receiver<String>) -> Value {
    loop {
        let line = deadline
            .recv(stdout)
            .expect("waited until the deadline for a clients line");
        let line: Value = serde_json::from_str(&line).unwrap();
        if line["kind"] == "clients" {
            return line;
        }
    }
}

/// Waits for `running` to exit successfully under the test's [`Deadline`].
#[track_caller]
fn finish(running: Running) {
    let Running {
        mut child,
        watchdog,
        group,
        guard,
        stdout,
        stderr,
        deadline,
    } = running;
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait()).unwrap());
    let status = match deadline.recv(&finished) {
        Ok(status) => status.unwrap(),
        Err(_) => support::expired(deadline, group, &finished, "fiber to exit"),
    };
    let stderr = deadline
        .recv(&stderr)
        .expect("waited until the deadline for stderr to close");
    assert!(status.success(), "stderr: {stderr}");
    assert!(
        !group_alive(deadline, group),
        "fiber left a process in its group"
    );
    // The group is empty. Skip the drop, which would kill it again.
    std::mem::forget(guard);
    drop(stdout);
    watchdog.stand_down(deadline.cleanup());
}

/// One finished background run: its exit code, stdout's lines, and stderr.
struct Finished {
    code: Option<i32>,
    lines: Vec<Value>,
    stderr: String,
}

/// Waits for `running` to exit under the test's [`Deadline`], killing its
/// group on expiry like [`Setup::fiber`] does, and returns what it printed. Its
/// stdout sender is dropped once the process closes stdout, so collecting
/// the lines ends once the process has exited.
#[track_caller]
fn finish_output(running: Running) -> Finished {
    let Running {
        mut child,
        watchdog,
        group,
        guard,
        stdout,
        stderr,
        deadline,
    } = running;
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait()).unwrap());
    let status = match deadline.recv(&finished) {
        Ok(status) => status.unwrap(),
        Err(_) => support::expired(deadline, group, &finished, "`fiber` to exit"),
    };
    assert!(
        !group_alive(deadline, group),
        "`fiber` left a process in its group behind"
    );
    std::mem::forget(guard);
    watchdog.stand_down(deadline.cleanup());
    let stderr = deadline
        .recv(&stderr)
        .expect("waited until the deadline for stderr to close");
    let mut lines = Vec::new();
    loop {
        match deadline.recv(&stdout) {
            Ok(line) if is_status(&line) => {}
            Ok(line) => lines.push(serde_json::from_str(&line).unwrap()),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("waited until the deadline for stdout to close");
            }
        }
    }
    Finished {
        code: status.code(),
        lines,
        stderr,
    }
}

#[test]
fn a_second_ask_while_the_first_turn_runs_attaches_and_is_rejected() {
    let setup = Setup::new_with_fa_root();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    server.hold();
    let running = start(&setup, &["ask", "hi"]);
    let started = first_line(setup.deadline, &running.stdout);
    assert_eq!(started["kind"], "session_started");
    let id = started["session_id"].as_str().unwrap().to_owned();
    assert!(
        server.await_requests(1, setup.deadline.left()),
        "the held response was requested"
    );

    // The session is held by the live first run, so the second attaches
    // instead of opening a second writer. The `clients` line proves it
    // subscribed before the release below: without it the first run could
    // exit first and the second would resume as a writer.
    let second = start(&setup, &["ask", "--resume", &id, "x"]);
    let attached = until_clients(setup.deadline, &running.stdout);
    assert_eq!(attached["payload"]["count"], 1);
    // Its prompt queues behind the held provider call, so the rejection
    // arrives once the loop drains: `closing`, not `busy`. `fiber ask`
    // queues `close` with its prompt (`Session::ask`), and
    // docs/invocation.md, "Lifecycle" says "`close` ends the session
    // whoever else is attached. It accepts no more prompts, finishes the
    // turn in flight, then any running jobs", so a second prompt to an
    // `ask` session is rejected `closing` (`loop::inbox`, `admit_running`;
    // `closing` is a listed driver rejection in docs/invocation.md,
    // "Driver commands"). `busy` applies to a session that was not sent
    // `close`, and takes the same attach path, covered at crate level in
    // `doors/tests/attach.rs`.
    server.release();
    let second = finish_output(second);
    assert_eq!(second.code, Some(1), "stderr: {}", second.stderr);
    assert_eq!(second.lines.len(), 1, "{:?}", second.lines);
    let line = &second.lines[0];
    assert_eq!(line["kind"], "fiber_exited");
    assert_eq!(line.get("session_id"), None);
    assert_eq!(line["payload"]["exit_code"], 1);
    assert_eq!(line["payload"]["error"]["code"], "closing");
    // Three `closing` outcomes of the attach race: the loop rejected
    // the queued prompt; the attach saw the session close or its
    // `fiber_exited` before an answer; or the prompt reached the session
    // after the loop's last receive, so its acknowledgement was dropped
    // uncalled (`doors/src/client.rs`). All three are the `closing`
    // failure of an attach that started nothing.
    let message = line["payload"]["error"]["message"].as_str().unwrap();
    assert!(
        message == "The session is closing and takes no new turn."
            || message == format!("session {id} ended before its turn completed")
            || message == "The session ended before answering.",
        "{message}"
    );
    assert_eq!(second.stderr, format!("fiber: {message}\n"));

    finish(running);

    // The log has one `fiber_started`: the attach opened no second writer.
    let log = fs::read_to_string(setup.sessions().join(&id).join("events.jsonl")).unwrap();
    let kinds: Vec<String> = log
        .lines()
        .map(|line| {
            serde_json::from_str::<Value>(line).unwrap()["kind"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(
        kinds.iter().filter(|kind| *kind == "fiber_started").count(),
        1,
        "{kinds:?}"
    );
    assert!(
        !kinds.contains(&"clients".to_owned()),
        "the attach's `clients` line is ephemeral, never logged: {kinds:?}"
    );
    // The attach sent no provider request of its own.
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn a_changed_append_system_changes_the_resumed_request() {
    let setup = Setup::new_with_fa_root();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);

    let first = setup.run_in_workspace(&["ask", "one"]);
    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
    let id = first.session_id().to_owned();
    let prefix = &id[..8];

    fs::write(setup.home().join("APPEND_SYSTEM.md"), "Be terse.\n").unwrap();
    let second = setup.run_in_workspace(&["ask", "--resume", prefix, "two"]);
    assert_eq!(second.code, Some(0), "stderr: {}", second.stderr);

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    let first_body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    let second_body: Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert_ne!(first_body["instructions"], second_body["instructions"]);
    assert!(
        second_body["instructions"]
            .as_str()
            .unwrap()
            .contains("Be terse."),
        "{}",
        second_body["instructions"]
    );
}

/// A standing-ask approval carrying `request_id`.
fn ask_request(request_id: &str) -> Event {
    Event::PermissionRequested(contract::events::PermissionRequested {
        request_id: contract::RequestId(request_id.into()),
        declared: DeclaredEffects {
            effects: vec![contract::shapes::Effect::Executes],
            reversible: true,
            paths: None,
        },
        step: contract::events::AskStep::StandingAsk {
            standing_rule: contract::events::StandingRule {
                scope: contract::events::RuleScope::Project,
                prefix: "run tests".into(),
            },
        },
    })
}

/// A review approval carrying `request_id`, an escalation and a rule offer.
fn review_request(request_id: &str) -> Event {
    Event::PermissionRequested(contract::events::PermissionRequested {
        request_id: contract::RequestId(request_id.into()),
        declared: DeclaredEffects {
            effects: vec![contract::shapes::Effect::Executes],
            reversible: true,
            paths: None,
        },
        step: contract::events::AskStep::Review {
            escalation: Some(contract::events::Escalation::ConsecutiveBlocks {
                reason: "it writes".into(),
            }),
            rule: Some(contract::events::RuleOffer {
                subject: "run tests".into(),
                prefix: "run tests".into(),
            }),
        },
    })
}

fn msg_started() -> Event {
    Event::AssistantMessageStarted(contract::events::Empty {})
}

fn fiber_start() -> Event {
    Event::FiberStarted(contract::events::FiberStarted {
        version: "0.0.0".into(),
        resumed: false,
    })
}

fn fiber_exit(suspended_on: Option<&str>) -> Event {
    Event::FiberExited(contract::events::FiberExited {
        exit_code: 0,
        usage: contract::shapes::Usage {
            tokens: contract::shapes::Tokens {
                input: 0,
                cache_read: 0,
                cache_write: Default::default(),
                output: 0,
            },
            cost: Some(0.0),
            subscription_cost: 0.0,
        },
        final_message: None,
        error: None,
        suspended_on: suspended_on.map(|id| contract::RequestId(id.into())),
        questions: None,
    })
}

#[test]
fn a_suspended_approval_is_refused_then_the_prompt_runs_next() {
    let setup = Setup::new_with_fa_root();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    hand_built(
        &setup,
        "s_susp1",
        vec![
            (turn_started("one"), Some(t()), None),
            (msg_started(), Some(t()), Some(a("a_0"))),
            (requested("search"), Some(t()), Some(a("a_1"))),
            (ask_request("r_9"), Some(t()), Some(a("a_1"))),
            (fiber_start(), None, None),
            (fiber_exit(Some("r_9")), None, None),
        ],
    );
    let events = setup.sessions().join("s_susp1").join("events.jsonl");
    let before = fs::read(&events).unwrap().len();

    let run = setup.run_in_workspace(&["ask", "--resume", "s_susp1", "next"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.session_id(), "s_susp1");
    assert_eq!(run.lines[0]["payload"]["resumed"], true);
    // No `turn_started` for the finishing turn: it completes the
    // suspended one, then the prompt starts its own.
    assert_eq!(
        run.kinds(),
        [
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
            "opening_message",
            "permission_requested",
            "permission_resolved",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
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
        ]
    );

    // The re-raised `request_id` matches the original, on both lines.
    let raised = run
        .lines
        .iter()
        .find(|l| l["kind"] == "permission_requested")
        .unwrap();
    assert_eq!(raised["payload"]["request_id"], "r_9");
    let resolved = run
        .lines
        .iter()
        .find(|l| l["kind"] == "permission_resolved")
        .unwrap();
    assert_eq!(resolved["payload"]["request_id"], "r_9");
    assert_eq!(resolved["payload"]["decision"], "deny");
    assert_eq!(resolved["payload"]["decided_by"], "cancel");
    assert_eq!(
        resolved["payload"]["reason"],
        "The session was resumed with nobody to answer."
    );
    assert!(resolved["payload"].get("reviewer").is_none());

    // Stdout's durable lines are the log's tail, byte for byte.
    let log = fs::read_to_string(&events).unwrap();
    assert_eq!(&log[before..], &run.durable());
}

#[test]
fn a_suspended_review_request_is_denied_by_cancel_on_resume() {
    let setup = Setup::new_with_fa_root();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    hand_built(
        &setup,
        "s_susp2",
        vec![
            (turn_started("one"), Some(t()), None),
            (msg_started(), Some(t()), Some(a("a_0"))),
            (requested("exec"), Some(t()), Some(a("a_1"))),
            (review_request("r_9"), Some(t()), Some(a("a_1"))),
            (fiber_start(), None, None),
            (fiber_exit(Some("r_9")), None, None),
        ],
    );
    let events = setup.sessions().join("s_susp2").join("events.jsonl");
    let before = fs::read(&events).unwrap().len();

    let run = setup.run_in_workspace(&["ask", "--resume", "s_susp2", "next"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.session_id(), "s_susp2");
    assert_eq!(run.lines[0]["payload"]["resumed"], true);
    // No `turn_started` for the finishing turn: it completes the
    // suspended one, then the prompt starts its own.
    assert_eq!(
        run.kinds(),
        [
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
            "opening_message",
            "permission_requested",
            "permission_resolved",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
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
        ]
    );

    let raised = run
        .lines
        .iter()
        .find(|l| l["kind"] == "permission_requested")
        .unwrap();
    assert_eq!(raised["payload"]["request_id"], "r_9");
    let resolved = run
        .lines
        .iter()
        .find(|l| l["kind"] == "permission_resolved")
        .unwrap();
    assert_eq!(resolved["payload"]["request_id"], "r_9");
    assert_eq!(resolved["payload"]["decision"], "deny");
    assert_eq!(resolved["payload"]["decided_by"], "cancel");
    assert_eq!(
        resolved["payload"]["reason"],
        "The session was resumed with nobody to answer."
    );
    assert!(resolved["payload"].get("reviewer").is_none());

    // Stdout's durable lines are the log's tail, byte for byte.
    let log = fs::read_to_string(&events).unwrap();
    assert_eq!(&log[before..], &run.durable());
}

#[test]
fn a_turn_suspended_after_a_completed_handoff_re_raises_its_approval() {
    // `main`'s order: open, the pass, `fiber_started` and the extension
    // lines, then the resume, whose window ends where the pass ended.
    let setup = Setup::new_with_fa_root();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    let opening = |os: &str| {
        Event::OpeningMessage(contract::events::OpeningMessage {
            environment: contract::events::Environment {
                date: "2023-11-14".into(),
                os: os.into(),
                arch: "test-arch".into(),
                shell: "/bin/sh".into(),
                workspace: "/w".into(),
                git: None,
                session_log: "/log/events.jsonl".into(),
            },
            instruction_files: Vec::new(),
            extension_sections: Vec::new(),
            skills: Vec::new(),
        })
    };
    let text = |text: &str| {
        Event::TextCompleted(contract::events::TextCompleted {
            text: text.into(),
            provider_item: None,
        })
    };
    hand_built(
        &setup,
        "s_hand1",
        vec![
            (opening("old-os"), Some(t()), None),
            (turn_started("early words"), Some(t()), None),
            (msg_started(), Some(t()), Some(a("a_0"))),
            (text("early answer"), Some(t()), Some(a("a_0"))),
            (turn_started("two"), Some(t()), None),
            (
                Event::HandoffStarted(contract::events::HandoffStarted {
                    trigger: contract::events::HandoffTrigger::Auto,
                }),
                Some(t()),
                None,
            ),
            (msg_started(), Some(t()), Some(a("a_note"))),
            (text("the handoff note"), Some(t()), Some(a("a_note"))),
            (
                Event::HandoffCompleted(contract::events::HandoffCompleted {
                    outcome: contract::events::Outcome::Completed,
                    error: None,
                    note: Some(contract::events::Note::Actions {
                        note: vec![a("a_note")],
                    }),
                    tokens_before: 1000,
                    instructions: None,
                }),
                Some(t()),
                None,
            ),
            (opening("new-os"), Some(t()), None),
            (turn_started("three"), Some(t()), None),
            (msg_started(), Some(t()), Some(a("a_5"))),
            (requested("search"), Some(t()), Some(a("a_1"))),
            (ask_request("r_9"), Some(t()), Some(a("a_1"))),
            (fiber_start(), None, None),
            (fiber_exit(Some("r_9")), None, None),
        ],
    );

    let run = setup.run_in_workspace(&["ask", "--resume", "s_hand1", "next"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let kinds = run.kinds();
    // The context's opening message is in the window: none is written.
    assert!(!kinds.contains(&"opening_message"), "{kinds:?}");
    let raised = run
        .lines
        .iter()
        .find(|l| l["kind"] == "permission_requested")
        .unwrap();
    assert_eq!(raised["payload"]["request_id"], "r_9");
    assert_eq!(raised["action_id"], "a_1");
    let resolved = run
        .lines
        .iter()
        .find(|l| l["kind"] == "permission_resolved")
        .unwrap();
    assert_eq!(resolved["payload"]["request_id"], "r_9");

    // The finishing turn's request is the context after the handoff.
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    let body = String::from_utf8_lossy(&requests[0].body);
    assert!(body.contains("the handoff note"), "{body}");
    assert!(body.contains("three"), "{body}");
    assert!(!body.contains("early words"), "{body}");
    assert!(!body.contains("early answer"), "{body}");
}

/// Appends `line` to the session log in `dir` as raw JSON, past the writer:
/// an envelope that reads with a payload that need not read as its kind.
fn append_raw(dir: &Path, line: &contract::Envelope) {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(dir.join("events.jsonl"))
        .unwrap();
    writeln!(file, "{}", serde_json::to_string(line).unwrap()).unwrap();
}

#[test]
fn an_unreadable_line_before_the_last_handoff_does_not_fail_the_resume() {
    // Only the window a resume actually needs is parsed (`docs/events.md`,
    // "Only the window a consumer actually needs is parsed"): the
    // unreadable `text_completed` line sits before the handoff turn's
    // `turn_started`, where the window starts, so the resume never reads
    // its payload.
    let setup = Setup::new_with_fa_root();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let text = |text: &str| {
        Event::TextCompleted(contract::events::TextCompleted {
            text: text.into(),
            provider_item: None,
        })
    };
    hand_built(
        &setup,
        "s_unread1",
        vec![
            (turn_started("early words"), Some(t()), None),
            (msg_started(), Some(t()), Some(a("a_0"))),
            (text("early answer"), Some(t()), Some(a("a_0"))),
        ],
    );
    // A `text_completed` line, a kind the pass never folds, whose envelope
    // reads but whose payload does not read as its kind (`text` is a
    // number, not a string).
    let dir = setup.sessions().join("s_unread1");
    let seq = fs::read_to_string(dir.join("events.jsonl"))
        .unwrap()
        .lines()
        .count() as u64;
    append_raw(
        &dir,
        &contract::Envelope {
            kind: "text_completed".into(),
            session_id: SessionId("s_unread1".into()),
            ts: 1,
            schema_version: contract::SCHEMA_VERSION,
            turn_id: Some(t()),
            action_id: Some(a("a_0")),
            seq: Some(contract::Seq(seq)),
            payload: json!({"text": 7}).as_object().unwrap().clone(),
        },
    );
    {
        let sessions = setup.sessions();
        let log = log::Log::open(
            &sessions,
            SessionId("s_unread1".into()),
            fakes::clock::FakeClock::new(),
        )
        .unwrap();
        for (event, action) in [
            (turn_started("two"), None),
            (
                Event::HandoffStarted(contract::events::HandoffStarted {
                    trigger: contract::events::HandoffTrigger::Auto,
                }),
                None,
            ),
            (msg_started(), Some(a("a_note"))),
            (text("the handoff note"), Some(a("a_note"))),
            (
                Event::HandoffCompleted(contract::events::HandoffCompleted {
                    outcome: contract::events::Outcome::Completed,
                    error: None,
                    note: Some(contract::events::Note::Actions {
                        note: vec![a("a_note")],
                    }),
                    tokens_before: 1000,
                    instructions: None,
                }),
                None,
            ),
            (
                Event::OpeningMessage(contract::events::OpeningMessage {
                    environment: contract::events::Environment {
                        date: "2023-11-14".into(),
                        os: "new-os".into(),
                        arch: "test-arch".into(),
                        shell: "/bin/sh".into(),
                        workspace: "/w".into(),
                        git: None,
                        session_log: "/log/events.jsonl".into(),
                    },
                    instruction_files: Vec::new(),
                    extension_sections: Vec::new(),
                    skills: Vec::new(),
                }),
                None,
            ),
        ] {
            log.append(&event, Some(t()), action).unwrap();
        }
    }

    let run = setup.run_in_workspace(&["ask", "--resume", "s_unread1", "next"]);
    // The resume reads only its window, so the prompt runs: the provider
    // is asked and the turn completes on the context after the handoff.
    // The unreadable line is an earlier process's, which `fiber_exited`
    // does not report, so the process exits 0.
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let requests = server.requests();
    assert_eq!(requests.len(), 1, "stderr: {}", run.stderr);
    assert_eq!(run.session_id(), "s_unread1");
    assert_eq!(run.lines[0]["payload"]["resumed"], true);
    assert!(
        run.kinds().contains(&"turn_completed"),
        "kinds: {:?}, stderr: {}",
        run.kinds(),
        run.stderr
    );
    let body = String::from_utf8_lossy(&requests[0].body);
    assert!(body.contains("the handoff note"), "{body}");
    assert!(body.contains("next"), "{body}");
    assert!(!body.contains("early answer"), "{body}");
}

#[test]
fn a_resumed_turn_reviews_with_the_notes_on_disk() {
    // A resume is a new process that reads configuration again: notes
    // changed on disk take effect at resume (`docs/permissions.md`,
    // "What the person tells it").
    let setup = Setup::new_with_fa_root();
    let lua = setup.home().join("data/notes/x.lua").display().to_string();
    let server = ProviderServer::start([
        hello(),
        stream(&[function_call(
            "write_lua",
            "write",
            &json!({"path": lua, "content": "return {}\n"}),
        )]),
        text_reply("allow"),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    write(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m",
            "reviewer": {"model": "fake/m", "context": "Our org is acme."}}),
    );

    let first = setup.run_in_workspace(&["ask", "one"]);
    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
    let id = first.session_id().to_owned();
    let prefix = id[..8].to_owned();

    write(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m",
            "reviewer": {"model": "fake/m", "context": "Our org is globex."}}),
    );
    let second = setup.run_in_workspace(&["ask", "--resume", prefix.as_str(), "save the snippet"]);
    assert_eq!(second.code, Some(0), "stderr: {}", second.stderr);

    let requests = server.requests();
    assert_eq!(requests.len(), 4);
    let body: Value = serde_json::from_slice(&requests[2].body).unwrap();
    let instructions = body["instructions"].as_str().unwrap();
    assert!(
        instructions.contains("Our org is globex."),
        "{instructions:?}"
    );
    assert!(
        !instructions.contains("Our org is acme."),
        "{instructions:?}"
    );
}

/// A resume switching the label records `model_changed` with the recorded
/// settings as `before`: the label and every setting the log last had.
#[test]
fn a_resume_with_a_new_label_records_model_changed() {
    let setup = Setup::new_with_fa_root();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    labels(&setup, "work", &["work", "home"]);
    write(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "providers": {"fake": {"credential": "work"}}, "cache": {"lifetime": "5m"}}),
    );
    let first = setup.run_in_workspace(&["ask", "one"]);
    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
    let id = first.session_id().to_owned();

    // The configuration changes the cache lifetime; the resume switches
    // the label.
    write(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "providers": {"fake": {"credential": "work"}}}),
    );
    let second = setup.run_in_workspace(&["ask", "--resume", &id, "--credential", "home", "two"]);
    assert_eq!(second.code, Some(0), "stderr: {}", second.stderr);
    let mut expected = vec!["fiber_started", "extensions_loaded", "model_changed"];
    expected.extend(ask_kinds(true)[2..].iter().copied());
    assert_eq!(second.kinds(), expected);
    let changed = second
        .lines
        .iter()
        .find(|line| line["kind"] == "model_changed")
        .unwrap();
    assert_eq!(
        changed["payload"]["before"],
        json!({"model": "fake/m", "cache_lifetime": "5m", "credential": "work"})
    );
    assert_eq!(
        changed["payload"]["after"],
        json!({"model": "fake/m", "cache_lifetime": "1h", "credential": "home"})
    );
    assert_eq!(changed["payload"]["source"], "driver");
    let built = second
        .lines
        .iter()
        .find(|line| line["kind"] == "preamble_built")
        .unwrap();
    assert_eq!(built["payload"]["reason"], "resume");
    assert_eq!(built["payload"]["credential"], "home");
    let requests = server.requests();
    assert_eq!(
        requests[1].header("authorization"),
        Some(fakes::fingerprint("Bearer home-key").as_str())
    );
}

/// A resume with the recorded label records nothing.
#[test]
fn a_resume_with_the_same_label_records_nothing() {
    let setup = Setup::new_with_fa_root();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    labels(&setup, "work", &["work", "home"]);
    let first = setup.run_in_workspace(&["ask", "one"]);
    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
    let id = first.session_id().to_owned();

    let second = setup.run_in_workspace(&["ask", "--resume", &id, "--credential", "work", "two"]);
    assert_eq!(second.code, Some(0), "stderr: {}", second.stderr);
    assert_eq!(second.kinds(), ask_kinds(true));
}

/// A resume with a label that names nothing fails before the session, and
/// the log stays byte for byte as it was.
#[test]
fn a_resume_with_an_absent_label_fails_before_the_session() {
    let setup = Setup::new_with_fa_root();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    labels(&setup, "work", &["work", "home"]);
    let first = setup.run_in_workspace(&["ask", "one"]);
    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
    let id = first.session_id().to_owned();
    let events = setup.sessions().join(&id).join("events.jsonl");
    let before = fs::read(&events).unwrap();

    let run = setup.run_in_workspace(&["ask", "--resume", &id, "--credential", "nope", "two"]);
    assert_pre_session(&run, 1, "credential_missing");
    assert!(run.stderr.contains("home"), "stderr: {}", run.stderr);
    assert!(run.stderr.contains("work"), "stderr: {}", run.stderr);
    assert_eq!(fs::read(&events).unwrap(), before);
}

/// A live resume with a label switches before the prompt's turn: the
/// request carries the new label's key, and the log holds `model_changed`
/// then `preamble_built` before the turn.
#[test]
fn a_live_resume_with_a_new_label_switches_before_the_prompt() {
    let setup = Setup::new_with_fa_root();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    labels(&setup, "work", &["work", "home"]);
    let id = doors::mint("s_");
    let workspace = setup.workspace();
    let running = start(
        &setup,
        &[
            "session",
            "--id",
            &id,
            "--workspace",
            workspace.to_str().unwrap(),
        ],
    );
    // The socket is bound before the session's first stdout line, so the
    // line is the signal the socket accepts.
    first_line(setup.deadline, &running.stdout);
    let socket = support::Socket::connect(setup.deadline, &setup.home().join("run").join(&id));

    let run = setup.run_in_workspace(&["ask", "--resume", &id, "--credential", "home", "hi"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let requests = server.requests();
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert_eq!(
        requests[0].header("authorization"),
        Some(fakes::fingerprint("Bearer home-key").as_str())
    );
    let kinds: Vec<String> = fs::read_to_string(setup.sessions().join(&id).join("events.jsonl"))
        .unwrap()
        .lines()
        .map(|line| {
            serde_json::from_str::<Value>(line).unwrap()["kind"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "model_changed",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
        .map(str::to_owned)
        .to_vec(),
    );

    socket.send(r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#);
    socket.send(r#"{"id":"c_close","command":"close"}"#);
    finish(running);
}

/// A live resume with a label that names nothing is rejected, and no turn
/// starts.
#[test]
fn a_live_resume_with_an_absent_label_is_rejected() {
    let setup = Setup::new_with_fa_root();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    labels(&setup, "work", &["work", "home"]);
    let id = doors::mint("s_");
    let workspace = setup.workspace();
    let running = start(
        &setup,
        &[
            "session",
            "--id",
            &id,
            "--workspace",
            workspace.to_str().unwrap(),
        ],
    );
    // The socket is bound before the session's first stdout line, so the
    // line is the signal the socket accepts.
    first_line(setup.deadline, &running.stdout);
    let socket = support::Socket::connect(setup.deadline, &setup.home().join("run").join(&id));

    let run = setup.run_in_workspace(&["ask", "--resume", &id, "--credential", "nope", "hi"]);
    assert_eq!(run.code, Some(1), "stderr: {}", run.stderr);
    assert_eq!(run.lines.len(), 1, "{:?}", run.lines);
    assert_eq!(
        run.lines[0]["payload"]["error"]["code"],
        "credential_missing"
    );
    let message = run.lines[0]["payload"]["error"]["message"]
        .as_str()
        .unwrap();
    assert!(message.contains("home"), "{message}");
    assert!(message.contains("work"), "{message}");
    assert!(server.requests().is_empty(), "no turn started");
    let kinds: Vec<String> = fs::read_to_string(setup.sessions().join(&id).join("events.jsonl"))
        .unwrap()
        .lines()
        .map(|line| {
            serde_json::from_str::<Value>(line).unwrap()["kind"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(
        kinds,
        ["session_started", "fiber_started", "extensions_loaded"]
            .map(str::to_owned)
            .to_vec(),
    );

    socket.send(r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#);
    socket.send(r#"{"id":"c_close","command":"close"}"#);
    finish(running);
}

#[test]
fn a_resumed_session_declares_delegate_spawn() {
    let setup = Setup::new_with_fa_root();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);

    let first = setup.run_in_workspace(&["ask", "one"]);
    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
    let id = first.session_id().to_owned();

    let second = setup.run_in_workspace(&["ask", "--resume", &id, "two"]);
    assert_eq!(second.code, Some(0), "stderr: {}", second.stderr);

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        let names: Vec<_> = body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"delegate_spawn"), "{names:?}");
    }
}
