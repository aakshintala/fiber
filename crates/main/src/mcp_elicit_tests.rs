//! An unanswered elicitation ends with its call's timeout, through the
//! real fixture and `McpTool` in a `Loop`, all on one `FakeClock`
//! (`docs/mcp.md`, "Elicitation, sampling and roots" and "Calls"). The
//! loop releases the ask at the deadline and declines on the wire; the
//! call then fails `timeout`. Ruling 10's precedence is proved by the
//! `mcp` crate's tests, which hold a response in its slot first: here the
//! fixture answers only after reading the `decline`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code; a failure is the test's"
)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use contract::clock::{Clock, Wake};
use contract::events::{TextDelta, ToolCallArgumentsDelta, ToolCallRequested};
use contract::inbox::{Ack, Delivery, Message};
use contract::provider::{Delta, Provider, ReplyAction};
use contract::rules::{Rules, RulesError, StandingRules};
use contract::shapes::{ContentPart, Origin, Sender};
use contract::{CommandId, Envelope, SessionId};
use fakes::clock::FakeClock;
use fakes::{Deadline, Scripted, ScriptedProvider};
use log::Log;
use r#loop::{Loop, Model};
use serde_json::{Value, json};

/// How long a test waits, in real time.
const WITHIN: Duration = fakes::MUST_SUCCEED_WITHIN;

/// Standing rules with nothing in them: the fixture's reads take the
/// permission fast path, so no reviewer runs.
struct EmptyRules;

impl Rules for EmptyRules {
    fn read(&self) -> Result<StandingRules, RulesError> {
        Ok(StandingRules {
            global: Vec::new(),
            project: Vec::new(),
        })
    }

    fn remember(
        &self,
        _tool: &str,
        _prefix: &str,
        _session: &contract::SessionId,
    ) -> Result<(), RulesError> {
        Ok(())
    }
}

/// An image child that never runs: the results here are text only.
struct DummyImages;

impl contract::images::Images for DummyImages {
    fn process(
        &self,
        _bytes: &[u8],
        _cancel: &dyn contract::tool::Cancel,
    ) -> Result<contract::provider::ImageRef, contract::images::ImageError> {
        Err(contract::images::ImageError::Unreadable(
            "unused".to_owned(),
        ))
    }
}

/// The inbox wake, as a door wires it: it puts `Delivery::Cancelled` in
/// the inbox through a sender it holds only weakly.
struct InboxWake(std::sync::Weak<mpsc::Sender<Delivery>>);

impl Wake for InboxWake {
    fn wake(&self) {
        if let Some(sender) = self.0.upgrade() {
            let _sent = sender.send(Delivery::Cancelled);
        }
    }
}

/// One model reply calling `tool` with `arguments`, after `text`.
fn calls_reply(text: &str, tool: &str, arguments: Value) -> Scripted {
    let mut end = fakes::reply(text);
    let mut deltas = vec![Delta::Text(TextDelta { text: text.into() })];
    deltas.push(Delta::ToolCallArguments(ToolCallArgumentsDelta {
        index: 0,
        name: Some(tool.into()),
        text: arguments.to_string(),
    }));
    end.actions.push(ReplyAction::ToolCall(ToolCallRequested {
        name: tool.into(),
        arguments,
        provider_id: None,
        repair: None,
        ran_by: None,
        provider_item: None,
    }));
    Scripted {
        deltas,
        end: Ok(end),
    }
}

/// A live tap on the session's log: a thread reads the watcher and the
/// test takes the lines with one deadline per wait. Lines read stay
/// buffered for the final kinds list.
struct Tap {
    /// Every durable line, in order.
    lines: mpsc::Receiver<Envelope>,
    /// Lines read but not yet waited for.
    buffered: Mutex<Vec<Envelope>>,
}

impl Tap {
    fn new(log: &Arc<Log>) -> Self {
        let mut watcher = log.watch();
        let (sent, lines) = mpsc::channel();
        // Dropped with the tap: the send fails and the thread ends.
        thread::spawn(move || {
            loop {
                match watcher.recv() {
                    Ok(Some(line)) => {
                        if sent.send(line).is_err() {
                            return;
                        }
                    }
                    Ok(None) | Err(_) => return,
                }
            }
        });
        Self {
            lines,
            buffered: Mutex::new(Vec::new()),
        }
    }

    /// The next line `matches` accepts. Every line read stays buffered
    /// in log order, the accepted one last, for the final kinds list.
    /// One deadline names the whole wait, however many lines it takes.
    #[track_caller]
    fn wait_until(&self, what: &str, matches: impl Fn(&Envelope) -> bool) -> Envelope {
        if let Some(found) = Self::take_from(&self.buffered, &matches) {
            return found;
        }
        let wait = Deadline::after(WITHIN);
        loop {
            let line = wait.recv_or_fail(&self.lines, what);
            let last = matches(&line);
            self.buffered.lock().unwrap().push(line.clone());
            if last {
                return line;
            }
        }
    }

    /// Every line so far, without waiting and without consuming the
    /// history: new arrivals join the buffer in log order.
    fn pending(&self) -> Vec<Envelope> {
        let mut buffered = self.buffered.lock().unwrap();
        while let Ok(line) = self.lines.try_recv() {
            buffered.push(line);
        }
        buffered.clone()
    }

    /// Removes and returns the first buffered line `matches` accepts.
    fn take_from(
        buffered: &Mutex<Vec<Envelope>>,
        matches: &dyn Fn(&Envelope) -> bool,
    ) -> Option<Envelope> {
        let mut buffered = buffered.lock().unwrap();
        let found = buffered.iter().position(matches)?;
        Some(buffered.remove(found))
    }
}

#[test]
fn an_unanswered_elicitation_is_declined_at_the_calls_deadline_then_the_call_times_out() {
    let root = fakes::TempDir::new("fiber-mcp-elicit");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let fixture = root.path().join("fx");
    std::fs::create_dir_all(&fixture).unwrap();
    // Both tools read and stay offline, so their calls take the
    // permission fast path and no reviewer runs.
    let tools = json!([
        {"name": "ask", "description": "Asks.", "inputSchema": {"type": "object"},
         "annotations": {"readOnlyHint": true, "openWorldHint": false}},
        {"name": "echo", "description": "Echoes.", "inputSchema": {"type": "object"},
         "annotations": {"readOnlyHint": true, "openWorldHint": false}},
    ]);
    std::fs::write(fixture.join("tools.json"), tools.to_string()).unwrap();
    std::fs::write(
        fixture.join("call-ask.json"),
        r#"{"content":[{"type":"text","text":"done"}]}"#,
    )
    .unwrap();
    std::fs::write(fixture.join("call-echo.json"), r#"{"content":[]}"#).unwrap();
    std::fs::write(
        fixture.join("elicit-ask.json"),
        r#"{"message":"Deploy?","requestedSchema":{"type":"object","properties":{"name":{"type":"string","title":"Name"}},"required":["name"]}}"#,
    )
    .unwrap();
    let clock = FakeClock::new();
    let shared: Arc<dyn Clock> = clock.clone();
    // Armed before the start: the fixture runs with this directory as
    // its argument, so an assertion failure below still kills the
    // detached group when the watchdog drops.
    let _watchdog = fakes::Watchdog::matching(&fixture.display().to_string());
    let artifacts = root.path().join("artifacts");
    std::fs::create_dir_all(&artifacts).unwrap();
    let cache = root.path().join("cache");
    let spec = mcp::ServerSpec {
        name: "fx".to_owned(),
        transport: mcp::Transport::Stdio(mcp::StdioSpec {
            command: fakes::mcp_fixture().display().to_string(),
            args: vec![fixture.display().to_string()],
            env: BTreeMap::new(),
            secrets: BTreeMap::new(),
            reader: std::sync::Arc::new(|_| Err(mcp::SecretError::Missing)),
            run: None,
        }),
        startup_timeout: mcp::DEFAULT_STARTUP_TIMEOUT,
        call_timeout: Duration::from_secs(60),
        enabled: None,
        disabled: Vec::new(),
        hints: BTreeMap::new(),
        required: false,
    };
    let images: Arc<dyn contract::images::Images> = Arc::new(DummyImages);
    let starting = Arc::clone(&shared);
    let starting_workspace = workspace.clone();
    let started = fakes::within("the MCP start", Deadline::after(WITHIN), move || {
        mcp::start(
            vec![spec],
            &starting_workspace,
            &artifacts,
            images,
            &cache,
            &starting,
            "0.0.0",
        )
    });
    assert!(started.failed.is_empty());
    let tools: Vec<(String, Arc<dyn contract::tool::Tool>)> = started.tools;
    let scripted = Arc::new(ScriptedProvider::new(vec![
        calls_reply("", "mcp__fx__ask", json!({})),
        calls_reply("", "mcp__fx__echo", json!({})),
        Scripted::text("done."),
    ]));
    let sessions = root.path().join("sessions");
    let log =
        Arc::new(Log::create(&sessions, SessionId("s_elicit".into()), clock.clone()).unwrap());
    let home = root.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let credentials = root.path().join("credentials");
    std::fs::create_dir_all(&credentials).unwrap();
    let tap = Tap::new(&log);
    let (inbox, deliveries) = mpsc::channel();
    let sender = Arc::new(inbox.clone());
    let looped = Loop::start(
        r#loop::Session {
            log: Arc::clone(&log),
            provider: Arc::clone(&scripted) as Arc<dyn Provider>,
            model: Model {
                reference: "fake/model-1".into(),
                cost: None,
                subscription: false,
            },
            prompt: {
                let mut prompt = r#loop::PromptInputs::new(
                    home,
                    "/bin/sh".into(),
                    sessions
                        .join("s_elicit")
                        .join("events.jsonl")
                        .display()
                        .to_string(),
                    Arc::clone(&shared) as Arc<dyn Clock>,
                    fakes::CONTEXT_WINDOW,
                );
                prompt.credential = Some("work".into());
                prompt
            },
            inbox: deliveries,
            tools: tools
                .into_iter()
                .map(|(_, tool)| ("builtin".to_owned(), tool))
                .collect(),
            permissions: r#loop::Permissions {
                workspace: workspace.display().to_string(),
                credentials,
                credential_files: Vec::new(),
                rules: Arc::new(EmptyRules),
            },
        },
        None,
    )
    .unwrap()
    .inbox_wake(Arc::new(InboxWake(Arc::downgrade(&sender))));
    let t0 = clock.now();
    let deadline = t0.checked_add(Duration::from_secs(60)).expect("deadline");
    inbox
        .send(Delivery::Prompt(
            Message {
                content: vec![ContentPart::Text { text: "go".into() }],
                sender: Sender {
                    origin: Origin::Driver,
                    command_id: Some(CommandId("c_1".into())),
                },
            },
            Ack(Box::new(|_| {})),
        ))
        .unwrap();
    let (done, finished) = mpsc::channel();
    let looping = thread::spawn(move || done.send(looped.run()).unwrap());
    let loop_thread = looping.thread().id();
    // The elicitation's `until` equals the call's deadline: only one call
    // is in flight.
    let requested = tap.wait_until("the elicitation's interaction", |line| {
        line.kind == "interaction_requested"
    });
    let request_id = requested.payload["request_id"].as_str().unwrap().to_owned();
    assert_eq!(requested.payload["kind"], "text_input");
    // The loop parks at the deadline; the clock advances only on that
    // signal, first to 1 ms short of it.
    assert!(
        clock.await_thread_parked(loop_thread, Some(deadline), fakes::Deadline::after(WITHIN)),
        "the loop waits on the call's deadline",
    );
    let mark = clock.advance_when_parked(
        Some(deadline),
        Some(loop_thread),
        Duration::from_secs(60) - Duration::from_millis(1),
        &Deadline::after(WITHIN),
    );
    assert!(
        clock.await_parked_since(&mark, Some(deadline), fakes::Deadline::after(WITHIN)),
        "the loop parks at the deadline again",
    );
    assert!(
        tap.pending()
            .iter()
            .all(|line| line.kind != "interaction_resolved"),
        "nothing resolves before the deadline",
    );
    clock.advance_when_parked(
        Some(deadline),
        Some(loop_thread),
        Duration::from_millis(1),
        &Deadline::after(WITHIN),
    );
    // Within one deadline: the fiber-resolved decline comes before the
    // call's failed `timeout` completion.
    let resolved = tap.wait_until("the elicitation's resolution", |line| {
        line.kind == "interaction_resolved"
    });
    assert_eq!(resolved.payload["request_id"], request_id);
    assert_eq!(resolved.payload["by"], "fiber");
    assert_eq!(resolved.payload["declined"], true);
    let completed = tap.wait_until("the call's completion", |line| {
        line.kind == "tool_call_completed" && line.payload["status"] == "failed"
    });
    assert_eq!(completed.payload["error"]["code"], "timeout");
    // The echo call is the log barrier: its completion proves the fixture
    // read the earlier `decline`.
    tap.wait_until("the echo's completion", |line| {
        line.kind == "tool_call_completed" && line.payload["status"] == "completed"
    });
    tap.wait_until("the turn's end", |line| line.kind == "turn_completed");
    let log_text = std::fs::read_to_string(fixture.join("requests.log")).expect("requests");
    let decline = log_text
        .lines()
        .find(|line| line.contains("\"el-") && line.contains("\"action\":\"decline\""))
        .unwrap_or_else(|| panic!("the decline reaches the wire: {log_text}"));
    assert!(
        decline.contains(r#""result":{"action":"decline"}"#),
        "{decline}"
    );
    // The turn's complete ordered kinds, pinned as written, without the
    // position-free `session_status` lines: each tool call's argument
    // fragments arrive as `tool_call_arguments_delta` ahead of its
    // `tool_call_requested`, and one `interaction_requested` pair stands
    // where the unanswered elicitation waited out the call's deadline.
    let lines = tap.pending();
    let start = lines
        .iter()
        .position(|line| line.kind == "turn_started")
        .expect("the turn started");
    let end = lines
        .iter()
        .position(|line| line.kind == "turn_completed")
        .expect("the turn ended");
    let kinds: Vec<&str> = lines[start..=end]
        .iter()
        .filter(|line| line.kind != "session_status")
        .map(|line| line.kind.as_str())
        .collect();
    assert_eq!(
        kinds,
        [
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "interaction_requested",
            "interaction_resolved",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ],
    );
    inbox.send(Delivery::Close(Ack(Box::new(|_| {})))).unwrap();
    let ran = Deadline::after(WITHIN)
        .recv(&finished)
        .expect("run ended in time");
    assert!(ran.is_ok(), "{ran:?}");
    fakes::within("the servers stop", Deadline::after(WITHIN), move || {
        started.servers.stop();
    });
}
