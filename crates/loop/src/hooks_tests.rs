//! The `after_tool` call site: what the hooks are shown, and how their answer
//! reaches the completion, the artifact and the next request.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code"
)]

use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use contract::emit::Emit;
use contract::events::{CallStatus, Notice, TextDelta, ToolCallArgumentsDelta, ToolCallRequested};
use contract::hook::{AfterToolAnswer, AfterToolCall, AfterToolOutcome, Hooks};
use contract::inbox::{Ack, Delivery, Message};
use contract::provider::{Delta, Input, ModelRequest, Provider, ReplyAction, ToolDefinition};
use contract::rules::{Rules, RulesError, StandingRules};
use contract::shapes::{ContentPart, DeclaredEffects, Effect, Failure, Origin, Process, Sender};
use contract::tool::{Bound, Cancel, Effects, EffectsError, Output, Tool};
use contract::{CommandId, Envelope, ErrorCode, SessionId};
use log::Log;
use serde_json::{Map, Value, json};

use crate::{Loop, Model, TurnCancel};

const TURN_DEADLINE: Duration = Duration::from_secs(10);

/// What a hook was shown, owned.
#[derive(Debug, Clone, PartialEq)]
struct Seen {
    tool: String,
    arguments: Map<String, Value>,
    status: CallStatus,
    content: String,
    details: Option<Value>,
    process: Option<Process>,
}

/// Hooks that record each call and give one scripted answer.
struct FakeHooks {
    seen: Mutex<Vec<Seen>>,
    answer: AfterToolAnswer,
}

impl FakeHooks {
    fn new(outcome: AfterToolOutcome, changed_by: &[&str], notices: Vec<Notice>) -> Arc<Self> {
        Arc::new(Self {
            seen: Mutex::new(Vec::new()),
            answer: AfterToolAnswer {
                outcome,
                changed_by: changed_by.iter().map(|s| (*s).to_owned()).collect(),
                notices,
            },
        })
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

impl Hooks for FakeHooks {
    fn after_tool(&self, call: &AfterToolCall<'_>) -> AfterToolAnswer {
        self.seen.lock().unwrap().push(Seen {
            tool: call.tool.to_owned(),
            arguments: call.arguments.clone(),
            status: call.status,
            content: call.content.to_owned(),
            details: call.details.cloned(),
            process: call.process.cloned(),
        });
        self.answer.clone()
    }

    fn deliver_to(&self, _inbox: std::sync::mpsc::Sender<Delivery>) {}
}

/// Rules that hold nothing.
struct NoRules;

impl Rules for NoRules {
    fn read(&self) -> Result<StandingRules, RulesError> {
        Ok(StandingRules::default())
    }

    fn remember(&self, _: &str, _: &str, _: &SessionId) -> Result<(), RulesError> {
        Ok(())
    }
}

/// A tool that returns `output`, after cancelling the turn when `cancel` is
/// set. Its bound is `bound`.
struct Fixed {
    output: Output,
    bound: Bound,
    effects: Vec<Effect>,
    cancel: Option<Arc<TurnCancel>>,
    /// Starts a shutdown on this signal while the call runs.
    shutdown: Option<Arc<TurnCancel>>,
}

fn fixed(output: Output) -> Fixed {
    Fixed {
        output,
        bound: Bound::DEFAULT,
        effects: vec![Effect::Reads],
        cancel: None,
        shutdown: None,
    }
}

impl Tool for Fixed {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "probe".to_owned(),
            description: "Returns a fixed output.".to_owned(),
            input_schema: json!({"type": "object"}),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, _: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Ok(Effects {
            declared: DeclaredEffects {
                effects: self.effects.clone(),
                reversible: true,
                paths: None,
            },
            subject: Some(String::new()),
            prefix: None,
        })
    }

    fn run(&self, _: &Map<String, Value>, _: &dyn Cancel, _: &dyn Emit) -> Output {
        if let Some(cancel) = &self.cancel {
            assert!(cancel.cancel());
        }
        if let Some(cancel) = &self.shutdown {
            cancel.shutdown(143);
        }
        self.output.clone()
    }

    fn bound(&self) -> Bound {
        self.bound
    }
}

fn text(text: &str) -> Output {
    Output {
        content: vec![ContentPart::Text { text: text.into() }],
        ..Output::default()
    }
}

/// A reply calling `name` with `arguments`, then one that ends the turn.
fn script(name: &str, arguments: Value) -> Vec<fakes::Scripted> {
    let mut end = fakes::reply("Checking.");
    end.actions.push(ReplyAction::ToolCall(ToolCallRequested {
        name: name.into(),
        arguments: arguments.clone(),
        provider_id: None,
        repair: None,
        ran_by: None,
        provider_item: None,
    }));
    vec![
        fakes::Scripted {
            deltas: vec![
                Delta::Text(TextDelta {
                    text: "Checking.".into(),
                }),
                Delta::ToolCallArguments(ToolCallArgumentsDelta {
                    index: 0,
                    name: Some(name.into()),
                    text: arguments.to_string(),
                }),
            ],
            end: Ok(end),
        },
        fakes::Scripted::text("Done."),
    ]
}

struct Ran {
    /// What the turn returned.
    turn: Result<Option<contract::events::TurnOutcome>, crate::Error>,
    requests: Vec<ModelRequest>,
    lines: Vec<Envelope>,
    /// Every line written, ephemeral ones included, through `turn_completed`.
    streamed: Vec<Envelope>,
    session: std::path::PathBuf,
    _home: fakes::TempDir,
}

impl Ran {
    fn completed(&self) -> &Envelope {
        self.lines
            .iter()
            .find(|line| line.kind == "tool_call_completed")
            .expect("a completion")
    }

    /// The tool result the second request carries.
    fn result_sent(&self) -> String {
        self.requests[1]
            .conversation
            .iter()
            .find_map(|input| match input {
                Input::ToolResult { text, .. } => Some(text.clone()),
                Input::User { .. }
                | Input::Assistant { .. }
                | Input::Reasoning { .. }
                | Input::ToolCall { .. } => None,
            })
            .expect("a tool result")
    }

    /// Every file's text under the session directory, joined.
    fn on_disk(&self) -> String {
        let mut all = String::new();
        let mut dirs = vec![self.session.clone()];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                } else {
                    all.push_str(&String::from_utf8_lossy(&std::fs::read(&path).unwrap()));
                }
            }
        }
        all
    }
}

fn run(
    tool: Fixed,
    script: Vec<fakes::Scripted>,
    hooks: Option<Arc<FakeHooks>>,
    cancel: Arc<TurnCancel>,
) -> Ran {
    let ran = run_with(tool, script, hooks, cancel, false);
    assert!(ran.turn.is_ok(), "{:?}", ran.turn);
    ran
}

/// [`run`], with the session's `artifacts/` replaced by a file when
/// `break_artifacts` is set, so no artifact can be written.
fn run_with(
    tool: Fixed,
    script: Vec<fakes::Scripted>,
    hooks: Option<Arc<FakeHooks>>,
    cancel: Arc<TurnCancel>,
    break_artifacts: bool,
) -> Ran {
    let home = fakes::TempDir::new("fiber-hooks");
    let workspace = home.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let log =
        Arc::new(Log::create(home.path(), SessionId("s_test".into()), Arc::clone(&clock)).unwrap());
    let mut watcher = log.watch();
    if break_artifacts {
        let artifacts = home.path().join("s_test").join("artifacts");
        std::fs::remove_dir_all(&artifacts).unwrap();
        std::fs::write(&artifacts, "not a directory").unwrap();
    }
    let provider = Arc::new(fakes::ScriptedProvider::new(script));
    let (inbox, rx) = mpsc::channel();
    inbox
        .send(Delivery::Prompt(
            Message {
                content: vec![ContentPart::Text { text: "go".into() }],
                sender: Sender {
                    origin: Origin::Driver,
                    command_id: Some(CommandId("c_go".into())),
                },
            },
            Ack(Box::new(|_| {})),
        ))
        .unwrap();
    let mut looped = Loop::start(
        log,
        Arc::clone(&provider) as Arc<dyn Provider>,
        Model {
            reference: "fake/model".into(),
            cost: None,
            subscription: false,
        },
        crate::prompt::PromptInputs::new(
            home.path().to_path_buf(),
            "/bin/sh".into(),
            home.path()
                .join("s_test/events.jsonl")
                .display()
                .to_string(),
            Arc::clone(&clock),
            fakes::CONTEXT_WINDOW,
        ),
        rx,
        vec![("builtin".to_owned(), Arc::new(tool) as Arc<dyn Tool>)],
        crate::Permissions {
            workspace: workspace.display().to_string(),
            credentials: home.path().join("credentials"),
            credential_files: Vec::new(),
            rules: Arc::new(NoRules),
        },
    )
    .unwrap()
    .answerable(false)
    .cancelled_by(cancel);
    if let Some(hooks) = hooks {
        looped = looped.hooks(hooks);
    }
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let _sent = done_tx.send(looped.turn());
    });
    let turn = done_rx.recv_timeout(TURN_DEADLINE).expect("the turn ended");
    drop(inbox);
    // The loop is dropped once its turn returns, so the watcher ends. A
    // turn that fails writes no `turn_completed`: the end breaks the loop
    // too.
    let streamed = fakes::within(
        "a turn_completed line or the log's end",
        TURN_DEADLINE,
        move || {
            let mut streamed = Vec::new();
            while let Ok(Some(line)) = watcher.recv() {
                let last = line.kind == "turn_completed";
                streamed.push(line);
                if last {
                    break;
                }
            }
            streamed
        },
    );
    let session = home.path().join("s_test");
    Ran {
        turn,
        streamed,
        requests: provider.requests(),
        lines: log::read(&session).unwrap(),
        session,
        _home: home,
    }
}

fn changed(
    content: Option<&str>,
    details: Option<Value>,
    artifact: Option<&str>,
) -> AfterToolOutcome {
    AfterToolOutcome::Changed {
        content: content.map(str::to_owned),
        details,
        artifact: artifact.map(str::to_owned),
    }
}

#[test]
fn the_hook_sees_the_call_and_its_full_output() {
    let hooks = FakeHooks::new(AfterToolOutcome::Unchanged, &[], Vec::new());
    let output = Output {
        content: vec![
            ContentPart::Text { text: "one".into() },
            ContentPart::Text { text: "two".into() },
        ],
        details: Some(json!({"diff": "+x"})),
        process: Some(Process {
            exit_code: Some(0),
            signal: None,
            timed_out: false,
        }),
        ..Output::default()
    };
    run(
        fixed(output),
        script("probe", json!({"path": "a.txt"})),
        Some(Arc::clone(&hooks)),
        Arc::new(TurnCancel::default()),
    );
    let seen = hooks.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].tool, "probe");
    assert_eq!(
        Value::Object(seen[0].arguments.clone()),
        json!({"path": "a.txt"})
    );
    assert_eq!(seen[0].status, CallStatus::Completed);
    assert_eq!(seen[0].content, "one\ntwo");
    assert_eq!(seen[0].details, Some(json!({"diff": "+x"})));
    assert_eq!(seen[0].process.as_ref().and_then(|p| p.exit_code), Some(0));
}

#[test]
fn replaced_content_is_logged_and_sent_and_names_who_changed_it() {
    let hooks = FakeHooks::new(
        changed(Some("[redacted]"), None, None),
        &["acme"],
        Vec::new(),
    );
    let ran = run(
        fixed(text("the secret is hunter2")),
        script("probe", json!({})),
        Some(hooks),
        Arc::new(TurnCancel::default()),
    );
    let completed = ran.completed();
    assert_eq!(completed.payload["status"], "completed");
    assert_eq!(
        completed.payload["content"],
        json!([{"type": "text", "text": "[redacted]"}])
    );
    assert_eq!(completed.payload["changed_by"], json!(["acme"]));
    assert!(completed.payload.get("artifact").is_none());
    assert_eq!(ran.result_sent(), "[redacted]");
    assert!(!ran.on_disk().contains("hunter2"));
}

#[test]
fn replaced_details_keep_the_tools_content() {
    let hooks = FakeHooks::new(
        changed(None, Some(json!({"n": 1})), None),
        &["acme"],
        Vec::new(),
    );
    let ran = run(
        fixed(Output {
            details: Some(json!({"n": 0})),
            ..text("kept")
        }),
        script("probe", json!({})),
        Some(hooks),
        Arc::new(TurnCancel::default()),
    );
    let completed = ran.completed();
    assert_eq!(completed.payload["content"][0]["text"], "kept");
    assert_eq!(completed.payload["details"], json!({"n": 1}));
}

#[test]
fn a_change_with_no_details_keeps_the_tools_details() {
    let hooks = FakeHooks::new(changed(Some("new"), None, None), &["acme"], Vec::new());
    let ran = run(
        fixed(Output {
            details: Some(json!({"n": 0})),
            ..text("old")
        }),
        script("probe", json!({})),
        Some(hooks),
        Arc::new(TurnCancel::default()),
    );
    assert_eq!(ran.completed().payload["details"], json!({"n": 0}));
}

#[test]
fn empty_replaced_content_sends_no_text_part() {
    let hooks = FakeHooks::new(changed(Some(""), None, None), &["acme"], Vec::new());
    let ran = run(
        fixed(text("gone")),
        script("probe", json!({})),
        Some(hooks),
        Arc::new(TurnCancel::default()),
    );
    assert_eq!(ran.completed().payload["content"], json!([]));
    assert!(!ran.on_disk().contains("gone"));
}

#[test]
fn replaced_content_past_the_cap_is_cut_and_the_artifact_holds_what_was_returned() {
    let returned = "r".repeat(20);
    let hooks = FakeHooks::new(changed(Some(&returned), None, None), &["acme"], Vec::new());
    let ran = run(
        Fixed {
            bound: Bound { start: 4, end: 0 },
            ..fixed(text("original-output-text-here"))
        },
        script("probe", json!({})),
        Some(hooks),
        Arc::new(TurnCancel::default()),
    );
    let completed = ran.completed();
    let artifact = completed.payload["artifact"].as_str().unwrap();
    assert_eq!(
        std::fs::read_to_string(ran.session.join(artifact)).unwrap(),
        returned
    );
    let content = completed.payload["content"][0]["text"].as_str().unwrap();
    assert!(content.starts_with("rrrr\n[16 bytes cut."), "{content}");
    assert!(!ran.on_disk().contains("original-output"));
}

#[test]
fn content_at_the_cap_is_not_cut() {
    let hooks = FakeHooks::new(changed(Some("rrrr"), None, None), &["acme"], Vec::new());
    let ran = run(
        Fixed {
            bound: Bound { start: 4, end: 0 },
            ..fixed(text("original"))
        },
        script("probe", json!({})),
        Some(hooks),
        Arc::new(TurnCancel::default()),
    );
    let completed = ran.completed();
    assert_eq!(completed.payload["content"][0]["text"], "rrrr");
    assert!(completed.payload.get("artifact").is_none());
}

#[test]
fn artifact_text_is_written_even_when_the_content_fits() {
    let hooks = FakeHooks::new(
        changed(Some("summary"), None, Some("the whole log")),
        &["acme"],
        Vec::new(),
    );
    let ran = run(
        fixed(text("raw build output")),
        script("probe", json!({})),
        Some(hooks),
        Arc::new(TurnCancel::default()),
    );
    let completed = ran.completed();
    assert_eq!(completed.payload["content"][0]["text"], "summary");
    let artifact = completed.payload["artifact"].as_str().unwrap();
    assert!(artifact.ends_with(".txt"), "{artifact}");
    assert_eq!(
        std::fs::read_to_string(ran.session.join(artifact)).unwrap(),
        "the whole log"
    );
    assert!(!ran.on_disk().contains("raw build output"));
}

#[test]
fn artifact_text_replaces_what_the_cut_would_write() {
    let hooks = FakeHooks::new(
        changed(Some("summary-past-the-cap"), None, Some("the whole log")),
        &["acme"],
        Vec::new(),
    );
    let ran = run(
        Fixed {
            bound: Bound { start: 4, end: 0 },
            ..fixed(text("raw"))
        },
        script("probe", json!({})),
        Some(hooks),
        Arc::new(TurnCancel::default()),
    );
    let completed = ran.completed();
    let artifact = completed.payload["artifact"].as_str().unwrap();
    assert_eq!(
        std::fs::read_to_string(ran.session.join(artifact)).unwrap(),
        "the whole log"
    );
    assert!(
        completed.payload["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("summ\n")
    );
}

#[test]
fn a_withheld_output_is_one_line_with_no_artifact_and_its_status_kept() {
    let hooks = FakeHooks::new(
        AfterToolOutcome::Withheld {
            extension: "acme".into(),
        },
        &["acme"],
        Vec::new(),
    );
    let failure = Failure {
        code: ErrorCode::NonzeroExit,
        message: "Exit code 1.".into(),
        retry_after_ms: None,
        provider: None,
    };
    let ran = run(
        Fixed {
            bound: Bound { start: 4, end: 0 },
            ..fixed(Output {
                content: vec![
                    ContentPart::Text {
                        text: "secret output past the cap".into(),
                    },
                    ContentPart::Image {
                        path: "artifacts/shot.png".into(),
                        mime_type: "image/png".into(),
                        width: 1,
                        height: 1,
                    },
                ],
                error: Some(failure.clone()),
                details: Some(json!({"n": 0})),
                ..Output::default()
            })
        },
        script("probe", json!({})),
        Some(hooks),
        Arc::new(TurnCancel::default()),
    );
    let completed = ran.completed();
    assert_eq!(completed.payload["status"], "failed");
    assert_eq!(completed.payload["error"]["code"], "nonzero_exit");
    assert_eq!(
        completed.payload["content"],
        json!([{"type": "text", "text": "Output withheld: the `after_tool` hook of extension acme failed."}])
    );
    assert!(completed.payload.get("artifact").is_none());
    assert!(completed.payload.get("details").is_none());
    assert_eq!(completed.payload["changed_by"], json!(["acme"]));
    assert!(!ran.on_disk().contains("secret output"));
    assert!(
        !ran.session.join("artifacts").exists()
            || std::fs::read_dir(ran.session.join("artifacts"))
                .unwrap()
                .next()
                .is_none()
    );
}

#[test]
fn an_unchanged_answer_leaves_the_output_and_names_nobody() {
    let hooks = FakeHooks::new(AfterToolOutcome::Unchanged, &[], Vec::new());
    let ran = run(
        Fixed {
            bound: Bound { start: 4, end: 0 },
            ..fixed(text("original-past-the-cap"))
        },
        script("probe", json!({})),
        Some(hooks),
        Arc::new(TurnCancel::default()),
    );
    let completed = ran.completed();
    assert!(completed.payload.get("changed_by").is_none());
    let artifact = completed.payload["artifact"].as_str().unwrap();
    assert_eq!(
        std::fs::read_to_string(ran.session.join(artifact)).unwrap(),
        "original-past-the-cap"
    );
}

#[test]
fn images_follow_the_hooks_content() {
    let hooks = FakeHooks::new(changed(Some("new"), None, None), &["acme"], Vec::new());
    let ran = run(
        fixed(Output {
            content: vec![
                ContentPart::Image {
                    path: "artifacts/shot.png".into(),
                    mime_type: "image/png".into(),
                    width: 1,
                    height: 1,
                },
                ContentPart::Text { text: "old".into() },
            ],
            ..Output::default()
        }),
        script("probe", json!({})),
        Some(hooks),
        Arc::new(TurnCancel::default()),
    );
    let content = &ran.completed().payload["content"];
    assert_eq!(content[0], json!({"type": "text", "text": "new"}));
    assert_eq!(content[1]["type"], "image");
    assert_eq!(content.as_array().unwrap().len(), 2);
}

#[test]
fn the_hooks_notices_carry_no_action_id_and_precede_the_completion() {
    let notice = Notice {
        code: ErrorCode::HookFailed,
        message: "The `after_tool` hook failed: boom.".into(),
        extension: Some("acme".into()),
    };
    let hooks = FakeHooks::new(AfterToolOutcome::Unchanged, &[], vec![notice]);
    let ran = run(
        fixed(text("x")),
        script("probe", json!({})),
        Some(hooks),
        Arc::new(TurnCancel::default()),
    );
    let at = |kind: &str| ran.streamed.iter().position(|l| l.kind == kind).unwrap();
    let notice = &ran.streamed[at("notice")];
    assert_eq!(notice.payload["code"], "hook_failed");
    assert_eq!(notice.payload["extension"], "acme");
    assert!(notice.action_id.is_none());
    assert!(at("notice") < at("tool_call_completed"));
}

#[test]
fn a_failed_call_is_shown_to_the_hook_as_failed() {
    let hooks = FakeHooks::new(AfterToolOutcome::Unchanged, &[], Vec::new());
    run(
        fixed(Output {
            error: Some(Failure {
                code: ErrorCode::ToolError,
                message: "no".into(),
                retry_after_ms: None,
                provider: None,
            }),
            ..text("partial")
        }),
        script("probe", json!({})),
        Some(Arc::clone(&hooks)),
        Arc::new(TurnCancel::default()),
    );
    assert_eq!(hooks.seen()[0].status, CallStatus::Failed);
}

#[test]
fn a_cancelled_call_is_shown_to_the_hook_as_cancelled() {
    let hooks = FakeHooks::new(
        changed(Some("[redacted]"), None, None),
        &["acme"],
        Vec::new(),
    );
    let cancel = Arc::new(TurnCancel::default());
    let ran = run(
        Fixed {
            cancel: Some(Arc::clone(&cancel)),
            ..fixed(text("partial secret"))
        },
        script("probe", json!({})),
        Some(Arc::clone(&hooks)),
        cancel,
    );
    assert_eq!(hooks.seen()[0].status, CallStatus::Cancelled);
    let completed = ran.completed();
    assert_eq!(completed.payload["status"], "cancelled");
    assert_eq!(completed.payload["content"][0]["text"], "[redacted]");
}

#[test]
fn a_call_that_never_ran_calls_no_hook() {
    let hooks = FakeHooks::new(changed(Some("x"), None, None), &["acme"], Vec::new());
    let ran = run(
        fixed(text("x")),
        script("missing", json!({})),
        Some(Arc::clone(&hooks)),
        Arc::new(TurnCancel::default()),
    );
    assert!(hooks.seen().is_empty());
    assert_eq!(ran.completed().payload["status"], "failed");
    assert!(ran.completed().payload.get("changed_by").is_none());
}

#[test]
fn a_denied_call_calls_no_hook() {
    let hooks = FakeHooks::new(changed(Some("x"), None, None), &["acme"], Vec::new());
    let ran = run(
        Fixed {
            effects: vec![Effect::Executes],
            ..fixed(text("x"))
        },
        script("probe", json!({})),
        Some(Arc::clone(&hooks)),
        Arc::new(TurnCancel::default()),
    );
    assert!(hooks.seen().is_empty());
    assert_eq!(ran.completed().payload["status"], "denied");
}

#[test]
fn without_hooks_the_output_is_the_tools() {
    let ran = run(
        fixed(text("plain")),
        script("probe", json!({})),
        None,
        Arc::new(TurnCancel::default()),
    );
    let completed = ran.completed();
    assert_eq!(completed.payload["content"][0]["text"], "plain");
    assert!(completed.payload.get("changed_by").is_none());
    assert_eq!(ran.result_sent(), "plain");
}

#[test]
fn a_hooks_artifact_that_cannot_be_written_fails_the_turn() {
    let hooks = FakeHooks::new(
        changed(Some("summary"), None, Some("the whole log")),
        &["acme"],
        Vec::new(),
    );
    let ran = run_with(
        fixed(text("raw")),
        script("probe", json!({})),
        Some(hooks),
        Arc::new(TurnCancel::default()),
        true,
    );
    let Err(crate::Error::Log(_)) = &ran.turn else {
        panic!("the turn went on: {:?}", ran.turn)
    };
    assert!(
        !ran.lines
            .iter()
            .any(|line| line.kind == "tool_call_completed")
    );
}

#[test]
fn a_hooks_artifact_that_cannot_be_written_fails_the_turn_after_a_cut() {
    let hooks = FakeHooks::new(
        changed(Some("summary-past-the-cap"), None, Some("the whole log")),
        &["acme"],
        Vec::new(),
    );
    let ran = run_with(
        Fixed {
            bound: Bound { start: 4, end: 0 },
            ..fixed(text("raw"))
        },
        script("probe", json!({})),
        Some(hooks),
        Arc::new(TurnCancel::default()),
        true,
    );
    let Err(crate::Error::Log(_)) = &ran.turn else {
        panic!("the turn went on: {:?}", ran.turn)
    };
    assert!(
        !ran.lines
            .iter()
            .any(|line| line.kind == "tool_call_completed")
    );
}

#[test]
fn extensions_loaded_writes_the_set_then_each_notice() {
    let home = fakes::TempDir::new("fiber-extensions-loaded");
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let log = Log::create(home.path(), SessionId("s_test".into()), clock).unwrap();
    let mut watcher = log.watch();
    let notice = Notice {
        code: ErrorCode::ExtensionFailed,
        message: "`after_tool` hook not registered: missing `timeout`".into(),
        extension: Some("acme".into()),
    };
    crate::extensions_loaded(
        &log,
        vec![contract::events::LoadedExtension {
            name: "acme".into(),
            version: "v1.0.0".into(),
        }],
        vec![notice],
    )
    .unwrap();
    drop(log);
    let streamed = fakes::within(
        "the log's remaining lines (extensions)",
        TURN_DEADLINE,
        move || {
            let mut streamed = Vec::new();
            loop {
                match watcher.recv() {
                    Ok(Some(line)) => streamed.push(line),
                    Ok(None) => break,
                    Err(e) => panic!("the extensions watcher failed: {e}"),
                }
            }
            streamed
        },
    );
    let kinds: Vec<&str> = streamed.iter().map(|l| l.kind.as_str()).collect();
    assert_eq!(kinds, ["extensions_loaded", "notice"]);
    assert_eq!(
        serde_json::to_value(&streamed[0].payload).unwrap(),
        json!({"extensions": [{"name": "acme", "version": "v1.0.0"}]})
    );
    assert!(streamed[0].seq.is_some());
    assert_eq!(streamed[1].payload["code"], "extension_failed");
    assert_eq!(streamed[1].payload["extension"], "acme");
}

#[test]
fn mcp_servers_started_writes_each_failure_then_each_notice() {
    let home = fakes::TempDir::new("fiber-mcp-servers-started");
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let log = Log::create(home.path(), SessionId("s_test".into()), clock).unwrap();
    let mut watcher = log.watch();
    crate::mcp_servers_started(
        &log,
        vec![contract::events::McpServerFailed {
            server: "fx".into(),
            reason: contract::events::ServerFailure::Deadline,
            will_restart: false,
            error: Failure {
                code: ErrorCode::McpServerUnavailable,
                message: "The MCP server `fx` did not answer before its startup deadline of 5000 ms. Raise `startup_timeout_ms` under `mcp.servers.fx` if it needs longer.".into(),
                retry_after_ms: None,
                provider: None,
            },
        }],
        vec![Notice {
            code: ErrorCode::RepositoryCodeSkipped,
            message: "The MCP server `repo` was skipped.".into(),
            extension: None,
        }],
    )
    .unwrap();
    drop(log);
    let streamed = fakes::within(
        "the log's remaining lines (MCP servers)",
        TURN_DEADLINE,
        move || {
            let mut streamed = Vec::new();
            loop {
                match watcher.recv() {
                    Ok(Some(line)) => streamed.push(line),
                    Ok(None) => break,
                    Err(e) => panic!("the MCP watcher failed: {e}"),
                }
            }
            streamed
        },
    );
    let kinds: Vec<&str> = streamed.iter().map(|l| l.kind.as_str()).collect();
    assert_eq!(kinds, ["mcp_server_failed", "notice"]);
    assert_eq!(streamed[0].payload["server"], "fx");
    assert_eq!(streamed[0].payload["reason"], "deadline");
    assert_eq!(
        streamed[0].payload["error"]["code"],
        "mcp_server_unavailable"
    );
    assert_eq!(streamed[1].payload["code"], "repository_code_skipped");
}

/// Runs one `probe` call that starts a shutdown while it runs and returns
/// `output`, cut to 4 + 4 bytes, under hooks that would rewrite it.
fn shut_down_during(output: Output) -> (Ran, Arc<FakeHooks>) {
    let hooks = FakeHooks::new(
        changed(Some("rewritten"), None, None),
        &["acme"],
        Vec::new(),
    );
    let cancel = Arc::new(TurnCancel::default());
    let ran = run(
        Fixed {
            shutdown: Some(Arc::clone(&cancel)),
            bound: Bound { start: 4, end: 4 },
            ..fixed(output)
        },
        script("probe", json!({})),
        Some(Arc::clone(&hooks)),
        cancel,
    );
    (ran, hooks)
}

#[test]
fn a_call_shaped_under_a_shutdown_calls_no_hook_and_keeps_no_output() {
    let (ran, hooks) = shut_down_during(Output {
        details: Some(json!({"secret": true})),
        ..text("partial secret, long enough to be cut")
    });
    assert!(hooks.seen().is_empty());
    let completed = ran.completed();
    assert_eq!(completed.payload["status"], "cancelled");
    assert_eq!(completed.payload["content"], json!([]));
    assert_eq!(completed.payload.get("details"), None);
    assert_eq!(completed.payload.get("artifact"), None);
    assert_eq!(completed.payload.get("changed_by"), None);
    assert!(!ran.on_disk().contains("partial secret"));
    assert_eq!(
        ran.turn.as_ref().unwrap(),
        &Some(contract::events::TurnOutcome::Interrupted)
    );
    // The shutdown sends no further request.
    assert_eq!(ran.requests.len(), 1);
}

#[test]
fn a_failed_call_shaped_under_a_shutdown_keeps_no_output() {
    let (ran, hooks) = shut_down_during(Output {
        error: Some(Failure {
            code: ErrorCode::ToolError,
            message: "failed with a secret".into(),
            retry_after_ms: None,
            provider: None,
        }),
        ..text("failed with a secret, long enough to be cut")
    });
    assert!(hooks.seen().is_empty());
    let completed = ran.completed();
    assert_eq!(completed.payload["content"], json!([]));
    assert_eq!(completed.payload.get("artifact"), None);
    assert!(!ran.on_disk().contains("with a secret, long"));
}
