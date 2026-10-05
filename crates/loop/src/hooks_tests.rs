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
}

fn fixed(output: Output) -> Fixed {
    Fixed {
        output,
        bound: Bound::DEFAULT,
        effects: vec![Effect::Reads],
        cancel: None,
    }
}

impl Tool for Fixed {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "probe".to_owned(),
            description: "Returns a fixed output.".to_owned(),
            input_schema: json!({"type": "object"}),
            deferred: false,
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
    let home = fakes::TempDir::new("fiber-hooks");
    let workspace = home.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let log =
        Arc::new(Log::create(home.path(), SessionId("s_test".into()), Arc::clone(&clock)).unwrap());
    let mut watcher = log.watch();
    let provider = Arc::new(fakes::ScriptedProvider::new(script));
    let (inbox, rx) = mpsc::channel();
    inbox
        .send(Delivery::Prompt(
            Message {
                content: vec![ContentPart::Text { text: "go".into() }],
                sender: Sender {
                    origin: Origin::Driver,
                    command_id: CommandId("c_go".into()),
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
        ),
        rx,
        vec![("builtin".to_owned(), Arc::new(tool) as Arc<dyn Tool>)],
        crate::Permissions {
            workspace: workspace.display().to_string(),
            credentials: home.path().join("credentials"),
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
    done_rx
        .recv_timeout(TURN_DEADLINE)
        .expect("the turn ended")
        .unwrap();
    drop(inbox);
    let mut streamed = Vec::new();
    while let Some(line) = watcher.recv().unwrap() {
        let last = line.kind == "turn_completed";
        streamed.push(line);
        if last {
            break;
        }
    }
    let session = home.path().join("s_test");
    Ran {
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
        retry_after: None,
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
fn the_hooks_notices_are_written_under_the_call_before_its_completion() {
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
    assert_eq!(notice.action_id, ran.completed().action_id);
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
                retry_after: None,
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
