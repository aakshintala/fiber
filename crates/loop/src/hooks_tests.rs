//! The `after_tool` call site: what the hooks are shown, and how their answer
//! reaches the completion, the artifact and the next request.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code"
)]

use fakes::MUST_SUCCEED_WITHIN;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use contract::emit::Emit;
use contract::events::{CallStatus, Notice, TextDelta, ToolCallArgumentsDelta, ToolCallRequested};
use contract::hook::{
    AfterToolAnswer, AfterToolCall, AfterToolOutcome, BeforeMessage, BeforeMessageAnswer,
    BeforeMessageOutcome, BeforeToolAnswer, BeforeToolCall, BeforeToolOutcome, CheckAnswer,
    CheckOutcome, Hooks,
};
use contract::inbox::{Ack, Delivery, Message};
use contract::provider::{Delta, Input, ModelRequest, Provider, ReplyAction, ToolDefinition};
use contract::shapes::{ContentPart, DeclaredEffects, Effect, Failure, Origin, Process, Sender};
use contract::tool::{
    Bound, Cancel, Effects, EffectsError, Output, SpillFile, Spilled, Tool, Unsaved,
};
use contract::{CommandId, Envelope, ErrorCode, SessionId};
use fakes::{Deadline, NoRules};
use log::Log;
use serde_json::{Map, Value, json};

use crate::{Loop, Model, TurnCancel};

/// What a hook was shown, owned.
#[derive(Debug, Clone, PartialEq)]
struct Seen {
    tool: String,
    arguments: Map<String, Value>,
    status: CallStatus,
    content: String,
    details: Option<Value>,
    process: Option<Process>,
    spilled: Option<(std::path::PathBuf, u64)>,
}

/// Hooks that record each call and give one scripted answer.
struct FakeHooks {
    seen: Mutex<Vec<Seen>>,
    answer: AfterToolAnswer,
    before: Option<BeforeToolOutcome>,
}

impl FakeHooks {
    fn new(outcome: AfterToolOutcome, changed_by: &[&str], notices: Vec<Notice>) -> Arc<Self> {
        Arc::new(Self {
            seen: Mutex::new(Vec::new()),
            before: None,
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
            spilled: call.spilled.map(|file| (file.path.clone(), file.size)),
        });
        self.answer.clone()
    }

    fn before_tool(&self, _call: &BeforeToolCall<'_>) -> BeforeToolAnswer {
        BeforeToolAnswer {
            outcome: self.before.clone().unwrap_or(BeforeToolOutcome::Unchanged),
            notices: Vec::new(),
        }
    }

    fn check_tool(&self, _call: &BeforeToolCall<'_>) -> CheckAnswer {
        CheckAnswer {
            outcome: CheckOutcome::Passed,
            notices: Vec::new(),
        }
    }

    fn before_message(&self, _message: &BeforeMessage<'_>) -> BeforeMessageAnswer {
        BeforeMessageAnswer {
            outcome: BeforeMessageOutcome::Passed {
                replaced: None,
                context: Vec::new(),
            },
            notices: Vec::new(),
        }
    }

    fn session_start(
        &self,
        _start: &contract::hook::SessionStart<'_>,
    ) -> contract::hook::ContextAnswer {
        contract::hook::ContextAnswer {
            outcome: contract::hook::ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }

    fn turn_start(&self, _turn: &contract::hook::TurnStart<'_>) -> contract::hook::ContextAnswer {
        contract::hook::ContextAnswer {
            outcome: contract::hook::ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }

    fn before_model_call(
        &self,
        _call: &contract::hook::BeforeModelCall<'_>,
    ) -> contract::hook::CheckAnswer {
        contract::hook::CheckAnswer {
            outcome: contract::hook::CheckOutcome::Passed,
            notices: Vec::new(),
        }
    }

    fn turn_end(&self, _end: &contract::hook::TurnEnd<'_>) -> contract::hook::ContextAnswer {
        contract::hook::ContextAnswer {
            outcome: contract::hook::ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }

    fn before_handoff(
        &self,
        _handoff: &contract::hook::BeforeHandoff<'_>,
    ) -> contract::hook::BeforeHandoffAnswer {
        fakes::no_note()
    }

    fn deliver_to(&self, _inbox: std::sync::mpsc::Sender<Delivery>) {}
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
    /// Builds the output when the call runs, instead of returning `output`.
    make: Option<Arc<dyn Fn() -> Output + Send + Sync>>,
}

fn fixed(output: Output) -> Fixed {
    Fixed {
        output,
        bound: Bound::DEFAULT,
        effects: vec![Effect::Reads],
        cancel: None,
        shutdown: None,
        make: None,
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
            always_reviewed: false,
            hosts: None,
        })
    }

    fn run(&self, _: &Map<String, Value>, _: &dyn Cancel, _: &dyn Emit) -> Output {
        if let Some(cancel) = &self.cancel {
            assert!(cancel.cancel());
        }
        if let Some(cancel) = &self.shutdown {
            cancel.shutdown(143);
        }
        match &self.make {
            Some(make) => make(),
            None => self.output.clone(),
        }
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
                | Input::ToolCall { .. }
                | Input::ToolSearchResult { .. } => None,
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

#[track_caller]
fn run(
    tool: Fixed,
    script: Vec<fakes::Scripted>,
    hooks: Option<Arc<FakeHooks>>,
    cancel: Arc<TurnCancel>,
) -> Ran {
    let ran = run_with(tool, script, hooks, cancel, false, false);
    assert!(ran.turn.is_ok(), "{:?}", ran.turn);
    ran
}

/// [`run`], with the session's `artifacts/` replaced by a file when
/// `break_artifacts` is set, so no artifact can be written. When `pinned`
/// is set, the person's name pins the session before the turn starts.
#[track_caller]
fn run_with(
    tool: Fixed,
    script: Vec<fakes::Scripted>,
    hooks: Option<Arc<FakeHooks>>,
    cancel: Arc<TurnCancel>,
    break_artifacts: bool,
    pinned: bool,
) -> Ran {
    run_in(
        fakes::TempDir::new("fiber-hooks"),
        tool,
        script,
        hooks,
        cancel,
        break_artifacts,
        pinned,
    )
}

/// [`run_with`], with the test's own `home`: a tool building its output at
/// run time can write its spill file into the session's `artifacts/`.
#[track_caller]
fn run_in(
    home: fakes::TempDir,
    tool: Fixed,
    script: Vec<fakes::Scripted>,
    hooks: Option<Arc<FakeHooks>>,
    cancel: Arc<TurnCancel>,
    break_artifacts: bool,
    pinned: bool,
) -> Ran {
    let watchdog = fakes::Watchdog::matching(&home.path().display().to_string());
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
        crate::Session {
            log,
            provider: Arc::clone(&provider) as Arc<dyn Provider>,
            model: Model {
                reference: "fake/model".into(),
                cost: None,
                subscription: false,
            },
            prompt: crate::prompt::PromptInputs::new(
                home.path().to_path_buf(),
                "/bin/sh".into(),
                home.path()
                    .join("s_test/events.jsonl")
                    .display()
                    .to_string(),
                Arc::clone(&clock),
                fakes::CONTEXT_WINDOW,
            ),
            inbox: rx,
            tools: vec![("builtin".to_owned(), Arc::new(tool) as Arc<dyn Tool>)],
            permissions: crate::Permissions {
                workspace: workspace.display().to_string(),
                credentials: home.path().join("credentials"),
                credential_files: Vec::new(),
                rules: Arc::new(NoRules),
            },
        },
        None,
    )
    .unwrap()
    .answerable(false)
    .cancelled_by(cancel);
    if let Some(hooks) = hooks {
        looped = looped.hooks(hooks);
    }
    if pinned {
        looped.naming.fold(&contract::events::Event::SessionNamed(
            contract::events::SessionNamed {
                name: Some("Mine".into()),
                by: contract::events::NamedBy::Person,
            },
        ));
    }
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let _sent = done_tx.send(looped.turn());
    });
    let turn = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&done_rx)
        .expect("the turn ended");
    drop(inbox);
    // The loop is dropped once its turn returns, so the watcher ends. A
    // turn that fails writes no `turn_completed`: the end breaks the loop
    // too.
    let streamed = fakes::within(
        "a turn_completed line or the log's end",
        Deadline::after(MUST_SUCCEED_WITHIN),
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
    watchdog.stand_down(fakes::Deadline::after(MUST_SUCCEED_WITHIN));
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
fn rewriting_content_drops_only_tool_match_details_in_every_output_shape() {
    let original = json!({"command":"grep x a", "exit_code":0,
        "matches":[{"path":"a","lines":[{"line":1,"text":"x","spans":[[0,1]]}]}]});
    for shape in ["whole", "spilled", "unsaved"] {
        for (content, details, artifact, expected) in [
            (
                Some("redacted"),
                None,
                None,
                json!({"command":"grep x a", "exit_code":0}),
            ),
            (
                Some("redacted"),
                Some(json!({"own":true})),
                None,
                json!({"own":true}),
            ),
            (None, None, Some("artifact only"), original.clone()),
            (None, None, None, original.clone()),
        ] {
            let home = fakes::TempDir::new("fiber-hook-match-redaction");
            let mut tool = match shape {
                "whole" => fixed(text("raw")),
                "spilled" => spilling(&home, vec![b'a'; 100], "HHHH", "TTTT", 92, Bound::DEFAULT),
                "unsaved" => unsaved(
                    "HHHH",
                    "TTTT",
                    92,
                    "disk failed".into(),
                    None,
                    Bound::DEFAULT,
                ),
                _ => unreachable!(),
            };
            tool.output.details = Some(original.clone());
            if let Some(make) = tool.make.take() {
                let original = original.clone();
                tool.make = Some(Arc::new(move || {
                    let mut output = make();
                    output.details = Some(original.clone());
                    output
                }));
            }
            let hooks = FakeHooks::new(changed(content, details, artifact), &["acme"], Vec::new());
            let ran = run_spilled(home, tool, Some(hooks));
            assert_eq!(
                ran.completed().payload["details"],
                expected,
                "{shape}, {content:?}"
            );
        }
    }
}

#[test]
fn replacement_preserves_nonobject_tool_details() {
    let ran = run(
        fixed(Output {
            details: Some(json!("opaque")),
            ..text("raw")
        }),
        script("probe", json!({})),
        Some(FakeHooks::new(
            changed(Some("redacted"), None, None),
            &["acme"],
            Vec::new(),
        )),
        Arc::new(TurnCancel::default()),
    );
    assert_eq!(ran.completed().payload["details"], json!("opaque"));
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
fn whole_output_kept_ranges_follow_the_cut_and_utf8_boundaries() {
    for (full, bound, expected) in [
        (
            "abcdefghijklmnopqrst",
            Bound { start: 4, end: 3 },
            Some(json!([[0, 4], [17, 20]])),
        ),
        (
            "abcdefghijklmnopqrst",
            Bound { start: 4, end: 0 },
            Some(json!([[0, 4]])),
        ),
        ("abc", Bound { start: 4, end: 0 }, None),
        ("abcd", Bound { start: 4, end: 0 }, None),
        ("abcde", Bound { start: 4, end: 0 }, Some(json!([[0, 4]]))),
        ("abcde", Bound { start: 0, end: 0 }, Some(json!([]))),
        (
            "aébcéz",
            Bound { start: 2, end: 2 },
            Some(json!([[0, 1], [7, 8]])),
        ),
    ] {
        let ran = run(
            Fixed {
                bound,
                ..fixed(text(full))
            },
            script("probe", json!({})),
            None,
            Arc::new(TurnCancel::default()),
        );
        let payload = &ran.completed().payload;
        assert_eq!(
            payload.get("kept"),
            expected.as_ref(),
            "{full:?}, {bound:?}"
        );
        if let Some(ranges) = expected {
            let artifact = payload["artifact"].as_str().unwrap();
            let saved = std::fs::read_to_string(ran.session.join(artifact)).unwrap();
            assert_eq!(saved, full);
            let shown = payload["content"][0]["text"].as_str().unwrap();
            for range in ranges.as_array().unwrap() {
                let start = usize::try_from(range[0].as_u64().unwrap()).unwrap();
                let end = usize::try_from(range[1].as_u64().unwrap()).unwrap();
                assert!(shown.contains(&saved[start..end]));
            }
        }
    }
}

#[test]
fn kept_ranges_count_the_before_tool_rewrite_line() {
    let mut hooks = FakeHooks::new(AfterToolOutcome::Unchanged, &[], Vec::new());
    Arc::get_mut(&mut hooks).unwrap().before = Some(BeforeToolOutcome::Rewritten {
        arguments: Map::new(),
        changed_by: vec!["acme".into()],
    });
    let ran = run(
        Fixed {
            bound: Bound { start: 4, end: 3 },
            ..fixed(text("abcdefghijklmnopqrst"))
        },
        script("probe", json!({})),
        Some(hooks),
        Arc::new(TurnCancel::default()),
    );
    let payload = &ran.completed().payload;
    let artifact = payload["artifact"].as_str().unwrap();
    let saved = std::fs::read_to_string(ran.session.join(artifact)).unwrap();
    let expected =
        "Extension acme rewrote this call's arguments; it ran with {}.\nabcdefghijklmnopqrst";
    assert_eq!(saved, expected);
    assert_eq!(
        payload["kept"],
        json!([[0, 4], [expected.len() - 3, expected.len()]])
    );
    assert!(ran.result_sent().starts_with("Exte\n["));
    assert!(ran.result_sent().ends_with("rst"));
}

#[test]
fn hook_content_ranges_index_the_replacement_not_the_artifact() {
    let hooks = FakeHooks::new(
        changed(Some("abcdefghijklmnopqrst"), None, Some("artifact")),
        &["acme"],
        Vec::new(),
    );
    let ran = run(
        Fixed {
            bound: Bound { start: 4, end: 3 },
            ..fixed(text("raw"))
        },
        script("probe", json!({})),
        Some(hooks),
        Arc::new(TurnCancel::default()),
    );
    let payload = &ran.completed().payload;
    assert_eq!(payload["kept"], json!([[0, 4], [17, 20]]));
    let path = payload["artifact"].as_str().unwrap();
    assert_eq!(
        std::fs::read_to_string(ran.session.join(path)).unwrap(),
        "artifact"
    );
}

#[test]
fn discarded_output_has_no_kept_ranges() {
    for outcome in [
        AfterToolOutcome::Withheld {
            extension: "acme".into(),
        },
        AfterToolOutcome::Failed(contract::hook::HookFailure {
            extension: "acme".into(),
            message: "broken".into(),
        }),
    ] {
        let ran = run(
            Fixed {
                bound: Bound { start: 1, end: 0 },
                ..fixed(text("raw"))
            },
            script("probe", json!({})),
            Some(FakeHooks::new(outcome, &[], Vec::new())),
            Arc::new(TurnCancel::default()),
        );
        assert!(ran.completed().payload.get("kept").is_none());
    }
    let cancel = Arc::new(TurnCancel::default());
    let ran = run(
        Fixed {
            shutdown: Some(Arc::clone(&cancel)),
            bound: Bound { start: 1, end: 0 },
            ..fixed(text("raw"))
        },
        script("probe", json!({})),
        None,
        cancel,
    );
    assert!(ran.completed().payload.get("kept").is_none());
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
        false,
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
        false,
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
            replaces: Vec::new(),
        }],
        vec![notice],
    )
    .unwrap();
    drop(log);
    let streamed = fakes::within(
        "the log's remaining lines (extensions)",
        Deadline::after(MUST_SUCCEED_WITHIN),
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
        Deadline::after(MUST_SUCCEED_WITHIN),
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
#[track_caller]
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

fn naming_output() -> Output {
    Output {
        content: vec![
            ContentPart::Text {
                text: "named".into(),
            },
            ContentPart::Image {
                path: "artifacts/shot.png".into(),
                mime_type: "image/png".into(),
                width: 1,
                height: 1,
            },
        ],
        control: Some(contract::events::Control {
            handoff: None,
            questions: None,
            skill: None,
            name: Some("Fix the parser".into()),
        }),
        ..Output::default()
    }
}

#[test]
fn a_naming_call_writes_its_name_after_the_hook_notices() {
    let notice = Notice {
        code: ErrorCode::HookFailed,
        message: "The `after_tool` hook failed: boom.".into(),
        extension: Some("acme".into()),
    };
    let hooks = FakeHooks::new(AfterToolOutcome::Unchanged, &[], vec![notice]);
    let ran = run_with(
        fixed(naming_output()),
        script("probe", json!({})),
        Some(hooks),
        Arc::new(TurnCancel::default()),
        false,
        false,
    );
    let at = |kind: &str| ran.streamed.iter().position(|l| l.kind == kind).unwrap();
    let kinds: Vec<&str> = ran.streamed.iter().map(|line| line.kind.as_str()).collect();
    assert_eq!(
        kinds,
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "text_completed",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "notice",
            "session_named",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    assert!(at("notice") < at("session_named"));
    assert!(at("session_named") < at("tool_call_completed"));
    let named = &ran.streamed[at("session_named")];
    assert_eq!(named.payload["name"], "Fix the parser");
    assert_eq!(named.payload["by"], "model");
    let completed = ran.completed();
    assert_eq!(completed.payload["status"], "completed");
}

#[test]
fn a_pinned_call_reaches_the_hook_as_failed_with_the_pin_message() {
    let hooks = FakeHooks::new(AfterToolOutcome::Unchanged, &[], Vec::new());
    let ran = run_with(
        fixed(naming_output()),
        script("probe", json!({})),
        Some(Arc::clone(&hooks)),
        Arc::new(TurnCancel::default()),
        false,
        true,
    );
    assert_eq!(hooks.seen()[0].status, CallStatus::Failed);
    let kinds: Vec<&str> = ran.streamed.iter().map(|line| line.kind.as_str()).collect();
    assert_eq!(
        kinds,
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "text_completed",
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
        ]
    );
    assert_eq!(hooks.seen()[0].content, crate::naming::PINNED.to_owned());
    let completed = ran.completed();
    assert_eq!(completed.payload["status"], "failed");
    assert_eq!(completed.payload["error"]["code"], "name_pinned");
    assert_eq!(completed.payload["error"]["message"], crate::naming::PINNED);
    assert_eq!(
        completed.payload["content"],
        json!([{"type": "text", "text": "the person named this session"}])
    );
    assert!(!ran.lines.iter().any(|line| line.kind == "session_named"));
}

#[test]
fn a_pinned_call_replaced_by_a_hook_keeps_the_pin_error() {
    let hooks = FakeHooks::new(changed(Some("redacted"), None, None), &["acme"], Vec::new());
    let ran = run_with(
        fixed(naming_output()),
        script("probe", json!({})),
        Some(hooks),
        Arc::new(TurnCancel::default()),
        false,
        true,
    );
    let completed = ran.completed();
    let kinds: Vec<&str> = ran.streamed.iter().map(|line| line.kind.as_str()).collect();
    assert_eq!(
        kinds,
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "text_completed",
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
        ]
    );
    assert_eq!(completed.payload["status"], "failed");
    assert_eq!(completed.payload["error"]["code"], "name_pinned");
    assert_eq!(completed.payload["error"]["message"], crate::naming::PINNED);
    assert_eq!(completed.payload["content"][0]["text"], "redacted");
    assert_eq!(completed.payload["changed_by"], json!(["acme"]));
}

#[test]
fn a_pinned_call_withheld_by_a_hook_keeps_the_pin_error() {
    let hooks = FakeHooks::new(
        AfterToolOutcome::Withheld {
            extension: "acme".into(),
        },
        &["acme"],
        Vec::new(),
    );
    let ran = run_with(
        fixed(naming_output()),
        script("probe", json!({})),
        Some(hooks),
        Arc::new(TurnCancel::default()),
        false,
        true,
    );
    let completed = ran.completed();
    let kinds: Vec<&str> = ran.streamed.iter().map(|line| line.kind.as_str()).collect();
    assert_eq!(
        kinds,
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "text_completed",
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
        ]
    );
    assert_eq!(completed.payload["status"], "failed");
    assert_eq!(completed.payload["error"]["code"], "name_pinned");
    assert_eq!(
        completed.payload["content"],
        json!([{"type": "text", "text": "Output withheld: the `after_tool` hook of extension acme failed."}])
    );
}

#[test]
fn a_pinned_call_is_cut_to_its_bound() {
    let ran = run_with(
        Fixed {
            bound: Bound { start: 10, end: 0 },
            ..fixed(naming_output())
        },
        script("probe", json!({})),
        None,
        Arc::new(TurnCancel::default()),
        false,
        true,
    );
    let completed = ran.completed();
    let kinds: Vec<&str> = ran.streamed.iter().map(|line| line.kind.as_str()).collect();
    assert_eq!(
        kinds,
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "text_completed",
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
        ]
    );
    assert_eq!(completed.payload["status"], "failed");
    assert_eq!(completed.payload["error"]["code"], "name_pinned");
    let content = completed.payload["content"][0]["text"].as_str().unwrap();
    assert!(
        content.starts_with("the person\n[19 bytes cut"),
        "{content}"
    );
}

/// The spill file's name in the session's `artifacts/`.
const SPILL_NAME: &str = "s_spill.txt";

/// A tool that writes `bytes` to the session's `artifacts/` when it runs
/// and returns them spilled, holding `head` and `tail` with `removed`
/// bytes between them.
fn spilling(
    home: &fakes::TempDir,
    bytes: Vec<u8>,
    head: &str,
    tail: &str,
    removed: u64,
    bound: Bound,
) -> Fixed {
    let dir = home.path().join("s_test").join("artifacts");
    let (head, tail) = (head.to_owned(), tail.to_owned());
    Fixed {
        output: text("unused"),
        bound,
        effects: vec![Effect::Reads],
        cancel: None,
        shutdown: None,
        make: Some(Arc::new(move || {
            std::fs::write(dir.join(SPILL_NAME), &bytes).unwrap();
            Output {
                content: Vec::new(),
                spilled: Some(Spilled {
                    head_bytes: head.len() as u64,
                    tail_bytes: tail.len() as u64,
                    head: head.clone(),
                    tail: tail.clone(),
                    removed,
                    file: Ok(SpillFile {
                        path: dir.join(SPILL_NAME),
                        size: u64::try_from(bytes.len()).unwrap(),
                    }),
                }),
                ..Output::default()
            }
        })),
    }
}

/// Runs one `probe` call of `tool` under `hooks` in `home`'s session.
fn run_spilled(home: fakes::TempDir, tool: Fixed, hooks: Option<Arc<FakeHooks>>) -> Ran {
    run_in(
        home,
        tool,
        script("probe", json!({})),
        hooks,
        Arc::new(TurnCancel::default()),
        false,
        false,
    )
}

/// The cut text of 100 spilled `a` bytes held as `HHHH` and `TTTT`: what
/// the completion and the hook both see.
fn cut_of(path: &std::path::Path) -> String {
    format!(
        "HHHH\n[92 bytes cut. The full output is in {}; read it with `read`.]\nTTTT",
        path.display()
    )
}

#[test]
fn spilled_kept_ranges_index_raw_bytes_and_drop_on_separate_artifact() {
    let home = fakes::TempDir::new("fiber-spilled-ranges");
    let dir = home.path().join("s_test/artifacts");
    let tool = Fixed {
        make: Some(Arc::new(move || {
            let bytes = b"\xffa123456z\xff";
            std::fs::write(dir.join(SPILL_NAME), bytes).unwrap();
            Output {
                spilled: Some(Spilled {
                    head: "�a".into(),
                    tail: "z�".into(),
                    head_bytes: 2,
                    tail_bytes: 2,
                    removed: 6,
                    file: Ok(SpillFile {
                        path: dir.join(SPILL_NAME),
                        size: 10,
                    }),
                }),
                ..Output::default()
            }
        })),
        ..fixed(text("unused"))
    };
    let ran = run_spilled(home, tool, None);
    assert_eq!(ran.completed().payload["kept"], json!([[0, 2], [8, 10]]));
    let bytes = std::fs::read(ran.session.join("artifacts").join(SPILL_NAME)).unwrap();
    assert_eq!(&bytes[..2], b"\xffa");
    assert_eq!(&bytes[8..], b"z\xff");
}

#[test]
fn spilled_kept_is_absent_when_a_hook_returns_separate_artifact_text() {
    let home = fakes::TempDir::new("fiber-spilled-artifact-only");
    let dir = home.path().join("s_test/artifacts");
    let tool = Fixed {
        make: Some(Arc::new(move || {
            let bytes = b"\xffa123456z\xff";
            std::fs::write(dir.join(SPILL_NAME), bytes).unwrap();
            Output {
                spilled: Some(Spilled {
                    head: "�a".into(),
                    tail: "z�".into(),
                    head_bytes: 2,
                    tail_bytes: 2,
                    removed: 6,
                    file: Ok(SpillFile {
                        path: dir.join(SPILL_NAME),
                        size: 10,
                    }),
                }),
                ..Output::default()
            }
        })),
        ..fixed(text("unused"))
    };
    let hook = FakeHooks::new(
        changed(None, None, Some("new artifact")),
        &["acme"],
        Vec::new(),
    );
    let ran = run_spilled(home, tool, Some(hook));
    let completed = ran.completed();
    assert!(completed.payload.get("kept").is_none(), "{completed:?}");
}

#[test]
fn spill_replacement_content_has_its_own_ranges() {
    for saved in [false, true] {
        for replacement in ["abc", "abcd", "abcde"] {
            let home = fakes::TempDir::new("fiber-spilled-replacement-ranges");
            let tool = if saved {
                spilling(
                    &home,
                    vec![b'a'; 100],
                    "HHHH",
                    "TTTT",
                    92,
                    Bound { start: 4, end: 0 },
                )
            } else {
                unsaved(
                    "HHHH",
                    "TTTT",
                    92,
                    "disk failed".into(),
                    None,
                    Bound { start: 4, end: 0 },
                )
            };
            let hooks = FakeHooks::new(
                changed(Some(replacement), None, None),
                &["acme"],
                Vec::new(),
            );
            let ran = run_spilled(home, tool, Some(hooks));
            let expected = (replacement.len() > 4).then(|| json!([[0, 4]]));
            assert_eq!(ran.completed().payload.get("kept"), expected.as_ref());
        }
    }
}

#[test]
fn unsaved_spill_ranges_describe_the_output_stream() {
    for removed in [1, 92] {
        let home = fakes::TempDir::new("fiber-unsaved-ranges");
        let tool = unsaved(
            "HHHH",
            "TTTT",
            removed,
            "disk failed".into(),
            None,
            Bound::DEFAULT,
        );
        let ran = run_spilled(home, tool, None);
        assert_eq!(
            ran.completed().payload["kept"],
            json!([[0, 4], [4 + removed, 8 + removed]])
        );
    }
}

#[test]
fn a_spilled_call_without_hooks_completes_cut_with_its_file_as_artifact() {
    let home = fakes::TempDir::new("fiber-spilled-plain");
    let tool = spilling(&home, vec![b'a'; 100], "HHHH", "TTTT", 92, Bound::DEFAULT);
    let ran = run_spilled(home, tool, None);
    let completed = ran.completed();
    let path = ran.session.join("artifacts").join(SPILL_NAME);
    let full = cut_of(&path);
    assert_eq!(
        completed.payload["content"],
        json!([{"type": "text", "text": full}])
    );
    assert_eq!(completed.payload["artifact"], "artifacts/s_spill.txt");
    assert_eq!(ran.result_sent(), full);
    // No second artifact: the directory holds only the spill.
    let entries: Vec<_> = std::fs::read_dir(ran.session.join("artifacts"))
        .unwrap()
        .collect();
    assert_eq!(entries.len(), 1);
}

#[test]
fn a_recording_hook_sees_the_cut_text_with_the_path_and_size() {
    let home = fakes::TempDir::new("fiber-spilled-seen");
    let hooks = FakeHooks::new(AfterToolOutcome::Unchanged, &[], Vec::new());
    let tool = spilling(&home, vec![b'a'; 100], "HHHH", "TTTT", 92, Bound::DEFAULT);
    let ran = run_spilled(home, tool, Some(Arc::clone(&hooks)));
    let path = ran.session.join("artifacts").join(SPILL_NAME);
    let seen = hooks.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].content, cut_of(&path));
    assert_eq!(seen[0].spilled, Some((path, 100)));
}

#[test]
fn a_hooks_artifact_text_replaces_the_spilled_file() {
    let home = fakes::TempDir::new("fiber-spilled-replaced");
    let hooks = FakeHooks::new(changed(None, None, Some("replaced")), &["acme"], Vec::new());
    let tool = spilling(&home, vec![b'a'; 100], "HHHH", "TTTT", 92, Bound::DEFAULT);
    let ran = run_spilled(home, tool, Some(hooks));
    let completed = ran.completed();
    let path = ran.session.join("artifacts").join(SPILL_NAME);
    assert_eq!(completed.payload["content"][0]["text"], cut_of(&path));
    assert_eq!(completed.payload["artifact"], "artifacts/s_spill.txt");
    assert_eq!(completed.payload["changed_by"], json!(["acme"]));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "replaced");
    let entries: Vec<_> = std::fs::read_dir(ran.session.join("artifacts"))
        .unwrap()
        .collect();
    assert_eq!(entries.len(), 1);
}

#[test]
fn an_artifact_only_hook_keeps_the_spilled_cut_text_with_its_notice() {
    let home = fakes::TempDir::new("fiber-spilled-hook-artifact-only");
    let hooks = FakeHooks::new(changed(None, None, Some("replaced")), &["acme"], Vec::new());
    let tool = spilling(
        &home,
        vec![b'a'; 100],
        "HHHH",
        "TTTT",
        92,
        Bound { start: 4, end: 0 },
    );
    let ran = run_spilled(home, tool, Some(hooks));
    let completed = ran.completed();
    let path = ran.session.join("artifacts").join(SPILL_NAME);
    // The already-cut text is kept unchanged past the cap: the notice
    // still reports the command's 92 missing bytes, not a second cut's.
    assert_eq!(completed.payload["content"][0]["text"], cut_of(&path));
    assert_eq!(completed.payload["artifact"], "artifacts/s_spill.txt");
    assert_eq!(completed.payload["changed_by"], json!(["acme"]));
    // Only the artifact text replaced the file: nothing else was written.
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "replaced");
    let entries: Vec<_> = std::fs::read_dir(ran.session.join("artifacts"))
        .unwrap()
        .collect();
    assert_eq!(entries.len(), 1);
}

#[test]
fn a_details_only_hook_keeps_the_spilled_cut_text_and_file() {
    let home = fakes::TempDir::new("fiber-spilled-hook-details-only");
    let hooks = FakeHooks::new(
        changed(None, Some(json!({"n": 1})), None),
        &["acme"],
        Vec::new(),
    );
    let tool = spilling(
        &home,
        vec![b'a'; 100],
        "HHHH",
        "TTTT",
        92,
        Bound { start: 4, end: 0 },
    );
    let ran = run_spilled(home, tool, Some(hooks));
    let completed = ran.completed();
    let path = ran.session.join("artifacts").join(SPILL_NAME);
    // The already-cut text is kept unchanged past the cap: the notice
    // still reports the command's 92 missing bytes, not a second cut's.
    assert_eq!(completed.payload["content"][0]["text"], cut_of(&path));
    assert_eq!(completed.payload["details"], json!({"n": 1}));
    assert_eq!(completed.payload["artifact"], "artifacts/s_spill.txt");
    assert_eq!(completed.payload["changed_by"], json!(["acme"]));
    assert_eq!(std::fs::read(&path).unwrap(), vec![b'a'; 100]);
}

#[test]
fn a_hooks_content_over_the_cap_is_cut_naming_the_spilled_file() {
    let home = fakes::TempDir::new("fiber-spilled-hook-cap");
    let returned = "r".repeat(20);
    let hooks = FakeHooks::new(changed(Some(&returned), None, None), &["acme"], Vec::new());
    let tool = spilling(
        &home,
        vec![b'a'; 100],
        "HHHH",
        "TTTT",
        92,
        Bound { start: 4, end: 0 },
    );
    let ran = run_spilled(home, tool, Some(hooks));
    let completed = ran.completed();
    let path = ran.session.join("artifacts").join(SPILL_NAME);
    assert_eq!(
        completed.payload["content"][0]["text"],
        json!(format!(
            "rrrr\n[16 bytes cut. The full output is in {}; read it with `read`.]",
            path.display()
        ))
    );
    // Nothing is written for it: the file stays the artifact, alone.
    assert_eq!(completed.payload["artifact"], "artifacts/s_spill.txt");
    assert_eq!(std::fs::read(&path).unwrap(), vec![b'a'; 100]);
    let entries: Vec<_> = std::fs::read_dir(ran.session.join("artifacts"))
        .unwrap()
        .collect();
    assert_eq!(entries.len(), 1);
}

#[test]
fn a_hooks_content_at_the_cap_is_not_cut_and_leaves_the_spilled_file() {
    let home = fakes::TempDir::new("fiber-spilled-hook-at-cap");
    let hooks = FakeHooks::new(changed(Some("rrrr"), None, None), &["acme"], Vec::new());
    let tool = spilling(
        &home,
        vec![b'a'; 100],
        "HHHH",
        "TTTT",
        92,
        Bound { start: 4, end: 0 },
    );
    let ran = run_spilled(home, tool, Some(hooks));
    let completed = ran.completed();
    assert_eq!(completed.payload["content"][0]["text"], "rrrr");
    assert_eq!(completed.payload["artifact"], "artifacts/s_spill.txt");
    assert_eq!(completed.payload["changed_by"], json!(["acme"]));
    // Nothing is cut for it: the file stays the artifact, alone.
    let path = ran.session.join("artifacts").join(SPILL_NAME);
    assert_eq!(std::fs::read(&path).unwrap(), vec![b'a'; 100]);
}

#[test]
fn a_withheld_spill_replaces_the_file_and_names_no_artifact() {
    let home = fakes::TempDir::new("fiber-spilled-withheld");
    let hooks = FakeHooks::new(
        AfterToolOutcome::Withheld {
            extension: "acme".into(),
        },
        &["acme"],
        Vec::new(),
    );
    let tool = spilling(&home, vec![b'a'; 100], "HHHH", "TTTT", 92, Bound::DEFAULT);
    let ran = run_spilled(home, tool, Some(hooks));
    let completed = ran.completed();
    let withheld = "Output withheld: the `after_tool` hook of extension acme failed.";
    assert_eq!(
        completed.payload["content"],
        json!([{"type": "text", "text": withheld}])
    );
    assert!(completed.payload.get("artifact").is_none());
    assert_eq!(completed.payload["changed_by"], json!(["acme"]));
    assert_eq!(
        std::fs::read_to_string(ran.session.join("artifacts").join(SPILL_NAME)).unwrap(),
        format!("{withheld}\n")
    );
}

#[test]
fn a_spilled_call_shaped_under_a_shutdown_keeps_no_file() {
    let home = fakes::TempDir::new("fiber-spilled-shutdown");
    let cancel = Arc::new(TurnCancel::default());
    let dir = home.path().join("s_test").join("artifacts");
    let bytes = vec![b'a'; 100];
    let tool = Fixed {
        output: text("unused"),
        bound: Bound::DEFAULT,
        effects: vec![Effect::Reads],
        cancel: None,
        shutdown: Some(Arc::clone(&cancel)),
        make: Some(Arc::new(move || {
            std::fs::write(dir.join(SPILL_NAME), &bytes).unwrap();
            Output {
                content: Vec::new(),
                spilled: Some(Spilled {
                    head_bytes: 4,
                    tail_bytes: 4,
                    head: "HHHH".into(),
                    tail: "TTTT".into(),
                    removed: 92,
                    file: Ok(SpillFile {
                        path: dir.join(SPILL_NAME),
                        size: 100,
                    }),
                }),
                ..Output::default()
            }
        })),
    };
    let ran = run_in(
        home,
        tool,
        script("probe", json!({})),
        None,
        cancel,
        false,
        false,
    );
    let completed = ran.completed();
    assert_eq!(completed.payload["content"], json!([]));
    assert!(completed.payload.get("artifact").is_none());
    assert!(
        std::fs::read_dir(ran.session.join("artifacts"))
            .unwrap()
            .next()
            .is_none(),
        "the spill was removed"
    );
}

/// A tool that returns `spilled` as its whole output, without writing any file.
fn unsaved(
    head: &str,
    tail: &str,
    removed: u64,
    message: String,
    left: Option<std::path::PathBuf>,
    bound: Bound,
) -> Fixed {
    let (head, tail) = (head.to_owned(), tail.to_owned());
    Fixed {
        output: text("unused"),
        bound,
        effects: vec![Effect::Reads],
        cancel: None,
        shutdown: None,
        make: Some(Arc::new(move || Output {
            content: Vec::new(),
            spilled: Some(Spilled {
                head_bytes: head.len() as u64,
                tail_bytes: tail.len() as u64,
                head: head.clone(),
                tail: tail.clone(),
                removed,
                file: Err(Unsaved {
                    message: message.clone(),
                    left: left.clone(),
                }),
            }),
            ..Output::default()
        })),
    }
}

#[test]
fn a_spill_that_was_not_kept_names_no_artifact_and_shows_no_file() {
    let home = fakes::TempDir::new("fiber-spilled-unsaved");
    let message = "could not save the output to /nowhere/s_x.txt: boom".to_owned();
    let hooks = FakeHooks::new(AfterToolOutcome::Unchanged, &[], Vec::new());
    let tool = unsaved("HHHH", "TTTT", 92, message.clone(), None, Bound::DEFAULT);
    let ran = run_spilled(home, tool, Some(Arc::clone(&hooks)));
    let completed = ran.completed();
    let notice = format!("[92 bytes cut. The full output could not be saved: {message}.]");
    assert_eq!(
        completed.payload["content"],
        json!([{"type": "text", "text": format!("HHHH\n{notice}\nTTTT")}])
    );
    assert!(completed.payload.get("artifact").is_none());
    assert!(completed.payload.get("changed_by").is_none());
    let seen = hooks.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].spilled, None);
}

#[test]
fn a_hooks_artifact_text_on_an_unsaved_spill_goes_to_the_action_file() {
    let home = fakes::TempDir::new("fiber-spilled-unsaved-artifact");
    let message = "could not save the output to /nowhere/s_x.txt: boom".to_owned();
    let hooks = FakeHooks::new(changed(None, None, Some("replaced")), &["acme"], Vec::new());
    let tool = unsaved("HHHH", "TTTT", 92, message, None, Bound::DEFAULT);
    let ran = run_spilled(home, tool, Some(hooks));
    let completed = ran.completed();
    let artifact = completed.payload["artifact"].as_str().unwrap();
    assert!(artifact.ends_with(".txt"), "{artifact}");
    assert_ne!(artifact, "artifacts/s_spill.txt");
    assert_eq!(completed.payload["changed_by"], json!(["acme"]));
    assert_eq!(
        std::fs::read_to_string(ran.session.join(artifact)).unwrap(),
        "replaced"
    );
}

#[test]
fn a_details_only_hook_on_an_unsaved_spill_names_no_artifact_and_saves_nothing() {
    let home = fakes::TempDir::new("fiber-spilled-unsaved-details-only");
    let message = "could not save the output to /nowhere/s_x.txt: boom".to_owned();
    let hooks = FakeHooks::new(
        changed(None, Some(json!({"n": 1})), None),
        &["acme"],
        Vec::new(),
    );
    // Past the cap, so cutting the failure summary again would save it
    // as the artifact: the outcome must stay as shaped instead.
    let tool = unsaved(
        "HHHH",
        "TTTT",
        92,
        message.clone(),
        None,
        Bound { start: 4, end: 0 },
    );
    let ran = run_spilled(home, tool, Some(hooks));
    let completed = ran.completed();
    let notice = format!("[92 bytes cut. The full output could not be saved: {message}.]");
    assert_eq!(
        completed.payload["content"],
        json!([{"type": "text", "text": format!("HHHH\n{notice}\nTTTT")}])
    );
    assert_eq!(completed.payload["details"], json!({"n": 1}));
    assert!(completed.payload.get("artifact").is_none());
    assert_eq!(completed.payload["changed_by"], json!(["acme"]));
    // Nothing was saved for the failure summary.
    assert!(
        !ran.session.join("artifacts").exists()
            || std::fs::read_dir(ran.session.join("artifacts"))
                .unwrap()
                .next()
                .is_none(),
        "no artifact was written for the unsaved spill"
    );
}

#[test]
fn replaced_content_on_an_unsaved_spill_is_sent_with_who_changed_it() {
    let home = fakes::TempDir::new("fiber-spilled-unsaved-changed");
    let message = "could not save the output to /nowhere/s_x.txt: boom".to_owned();
    let hooks = FakeHooks::new(changed(Some("new"), None, None), &["acme"], Vec::new());
    let tool = unsaved("HHHH", "TTTT", 92, message, None, Bound::DEFAULT);
    let ran = run_spilled(home, tool, Some(hooks));
    let completed = ran.completed();
    assert_eq!(
        completed.payload["content"],
        json!([{"type": "text", "text": "new"}])
    );
    assert!(completed.payload.get("artifact").is_none());
    assert_eq!(completed.payload["changed_by"], json!(["acme"]));
}

#[test]
fn empty_replaced_content_on_an_unsaved_spill_sends_no_text_part() {
    let home = fakes::TempDir::new("fiber-spilled-unsaved-empty");
    let message = "could not save the output to /nowhere/s_x.txt: boom".to_owned();
    let hooks = FakeHooks::new(changed(Some(""), None, None), &["acme"], Vec::new());
    let tool = unsaved("HHHH", "TTTT", 92, message, None, Bound::DEFAULT);
    let ran = run_spilled(home, tool, Some(hooks));
    let completed = ran.completed();
    assert_eq!(completed.payload["content"], json!([]));
    assert!(completed.payload.get("artifact").is_none());
    assert_eq!(completed.payload["changed_by"], json!(["acme"]));
}

#[test]
fn a_left_file_is_removed_before_the_completion_is_written() {
    let home = fakes::TempDir::new("fiber-spilled-left");
    let dir = home.path().join("s_test").join("artifacts");
    let left = dir.join("s_left.txt");
    let message = "could not save the output to /nowhere/s_x.txt: boom".to_owned();
    let tool = {
        let (left, message) = (left.clone(), message.clone());
        Fixed {
            output: text("unused"),
            bound: Bound::DEFAULT,
            effects: vec![Effect::Reads],
            cancel: None,
            shutdown: None,
            make: Some(Arc::new(move || {
                std::fs::write(&left, b"partial").unwrap();
                Output {
                    content: Vec::new(),
                    spilled: Some(Spilled {
                        head_bytes: 4,
                        tail_bytes: 4,
                        head: "HHHH".into(),
                        tail: "TTTT".into(),
                        removed: 92,
                        file: Err(Unsaved {
                            message: message.clone(),
                            left: Some(left.clone()),
                        }),
                    }),
                    ..Output::default()
                }
            })),
        }
    };
    let ran = run_spilled(home, tool, None);
    assert!(ran.turn.is_ok());
    assert!(!ran.session.join("artifacts").join("s_left.txt").exists());
}

#[test]
fn a_left_file_that_survives_fails_the_turn() {
    use std::os::unix::fs::PermissionsExt;
    let home = fakes::TempDir::new("fiber-spilled-left-stuck");
    let other = fakes::TempDir::new("fiber-spilled-left-other");
    let left = other.path().join("s_left.txt");
    std::fs::write(&left, b"partial").unwrap();
    std::fs::set_permissions(other.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
    let message = "could not save the output to /nowhere/s_x.txt: boom".to_owned();
    let tool = unsaved(
        "HHHH",
        "TTTT",
        92,
        message,
        Some(left.clone()),
        Bound::DEFAULT,
    );
    let ran = run_spilled(home, tool, None);
    std::fs::set_permissions(other.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let Err(crate::Error::Log(_)) = &ran.turn else {
        panic!("a surviving file went on: {:?}", ran.turn);
    };
    assert!(left.is_file());
}

#[test]
fn a_spill_inside_the_session_but_outside_artifacts_is_refused_not_written() {
    let home = fakes::TempDir::new("fiber-spilled-in-session-outside");
    let path = home.path().join("s_test").join("s_spill.txt");
    let hooks = FakeHooks::new(changed(None, None, Some("replaced")), &["acme"], Vec::new());
    let tool = Fixed {
        output: text("unused"),
        bound: Bound::DEFAULT,
        effects: vec![Effect::Reads],
        cancel: None,
        shutdown: None,
        make: Some(Arc::new(move || Output {
            content: Vec::new(),
            spilled: Some(Spilled {
                head_bytes: 4,
                tail_bytes: 5,
                head: "raw ".into(),
                tail: "bytes".into(),
                removed: 6,
                file: Ok(SpillFile {
                    path: path.clone(),
                    size: 16,
                }),
            }),
            ..Output::default()
        })),
    };
    let ran = run_spilled(home, tool, Some(Arc::clone(&hooks)));
    let Err(crate::Error::Log(_)) = &ran.turn else {
        panic!("a spill outside artifacts went on: {:?}", ran.turn);
    };
    assert!(!ran.session.join("s_spill.txt").exists());
}

#[test]
fn a_left_file_outside_artifacts_is_not_removed_and_fails_the_turn() {
    let home = fakes::TempDir::new("fiber-spilled-left-outside");
    let message = "could not save the output to /nowhere/s_x.txt: boom".to_owned();
    let tool = {
        let (message, session) = (
            message.clone(),
            home.path().join("s_test").join("s_left.txt"),
        );
        Fixed {
            output: text("unused"),
            bound: Bound::DEFAULT,
            effects: vec![Effect::Reads],
            cancel: None,
            shutdown: None,
            make: Some(Arc::new(move || {
                std::fs::write(&session, b"partial").unwrap();
                Output {
                    content: Vec::new(),
                    spilled: Some(Spilled {
                        head_bytes: 4,
                        tail_bytes: 4,
                        head: "HHHH".into(),
                        tail: "TTTT".into(),
                        removed: 92,
                        file: Err(Unsaved {
                            message: message.clone(),
                            left: Some(session.clone()),
                        }),
                    }),
                    ..Output::default()
                }
            })),
        }
    };
    let ran = run_spilled(home, tool, None);
    let Err(crate::Error::Log(_)) = &ran.turn else {
        panic!("a left file outside artifacts went on: {:?}", ran.turn);
    };
    assert_eq!(
        std::fs::read(ran.session.join("s_left.txt")).unwrap(),
        b"partial"
    );
}

#[test]
fn a_spill_with_nothing_removed_joins_head_and_tail_with_no_notice() {
    let home = fakes::TempDir::new("fiber-spilled-whole");
    let tool = spilling(
        &home,
        b"HHHHTTTT".to_vec(),
        "HHHH",
        "TTTT",
        0,
        Bound::DEFAULT,
    );
    let ran = run_spilled(home, tool, None);
    let completed = ran.completed();
    assert_eq!(completed.payload["content"][0]["text"], "HHHHTTTT");
    assert_eq!(completed.payload["artifact"], "artifacts/s_spill.txt");
    assert!(completed.payload.get("kept").is_none());
}

#[test]
fn text_parts_beside_a_spill_are_ignored_and_images_are_kept() {
    let home = fakes::TempDir::new("fiber-spilled-images");
    let dir = home.path().join("s_test").join("artifacts");
    let bytes = vec![b'a'; 100];
    let tool = Fixed {
        output: text("unused"),
        bound: Bound::DEFAULT,
        effects: vec![Effect::Reads],
        cancel: None,
        shutdown: None,
        make: Some(Arc::new(move || {
            std::fs::write(dir.join(SPILL_NAME), &bytes).unwrap();
            Output {
                content: vec![
                    ContentPart::Text {
                        text: "ignored".into(),
                    },
                    ContentPart::Image {
                        path: "artifacts/shot.png".into(),
                        mime_type: "image/png".into(),
                        width: 1,
                        height: 1,
                    },
                ],
                spilled: Some(Spilled {
                    head_bytes: 4,
                    tail_bytes: 4,
                    head: "HHHH".into(),
                    tail: "TTTT".into(),
                    removed: 92,
                    file: Ok(SpillFile {
                        path: dir.join(SPILL_NAME),
                        size: 100,
                    }),
                }),
                ..Output::default()
            }
        })),
    };
    let ran = run_spilled(home, tool, None);
    let completed = ran.completed();
    let content = completed.payload["content"].as_array().unwrap();
    assert_eq!(content.len(), 2);
    assert_eq!(
        content[0]["text"],
        cut_of(&ran.session.join("artifacts").join(SPILL_NAME))
    );
    assert_eq!(content[1]["type"], "image");
    assert!(!ran.on_disk().contains("ignored"));
}

#[test]
fn a_spill_outside_the_session_is_replaced_at_its_own_path() {
    let home = fakes::TempDir::new("fiber-spilled-outside");
    let elsewhere = fakes::TempDir::new("fiber-spilled-elsewhere");
    let path = elsewhere.path().join("s_out.txt");
    std::fs::write(&path, b"raw output bytes").unwrap();
    let hooks = FakeHooks::new(changed(None, None, Some("replaced")), &["acme"], Vec::new());
    let tool = Fixed {
        output: text("unused"),
        bound: Bound::DEFAULT,
        effects: vec![Effect::Reads],
        cancel: None,
        shutdown: None,
        make: Some(Arc::new(move || Output {
            content: Vec::new(),
            spilled: Some(Spilled {
                head_bytes: 4,
                tail_bytes: 5,
                head: "raw ".into(),
                tail: "bytes".into(),
                removed: 6,
                file: Ok(SpillFile {
                    path: path.clone(),
                    size: 16,
                }),
            }),
            ..Output::default()
        })),
    };
    let ran = run_spilled(home, tool, Some(Arc::clone(&hooks)));
    let completed = ran.completed();
    // Not under the session directory, so the artifact is the absolute path.
    assert_eq!(
        completed.payload["artifact"],
        json!(elsewhere.path().join("s_out.txt").display().to_string())
    );
    assert_eq!(
        std::fs::read_to_string(elsewhere.path().join("s_out.txt")).unwrap(),
        "replaced"
    );
    let seen = hooks.seen();
    assert_eq!(seen.len(), 1);
    assert!(seen[0].content.contains("raw "), "{}", seen[0].content);
    assert_eq!(
        seen[0].spilled,
        Some((elsewhere.path().join("s_out.txt"), 16))
    );
}

#[test]
fn a_shutdown_with_an_unsaved_spill_removes_its_left_file() {
    let home = fakes::TempDir::new("fiber-spilled-shutdown-left");
    let dir = home.path().join("s_test").join("artifacts");
    let left = dir.join("s_left.txt");
    let cancel = Arc::new(TurnCancel::default());
    let message = "could not save the output to /nowhere/s_x.txt: boom".to_owned();
    let tool = Fixed {
        output: text("unused"),
        bound: Bound::DEFAULT,
        effects: vec![Effect::Reads],
        cancel: None,
        shutdown: Some(Arc::clone(&cancel)),
        make: Some(Arc::new(move || {
            std::fs::write(&left, b"partial").unwrap();
            Output {
                content: Vec::new(),
                spilled: Some(Spilled {
                    head_bytes: 4,
                    tail_bytes: 4,
                    head: "HHHH".into(),
                    tail: "TTTT".into(),
                    removed: 92,
                    file: Err(Unsaved {
                        message: message.clone(),
                        left: Some(left.clone()),
                    }),
                }),
                ..Output::default()
            }
        })),
    };
    let ran = run_in(
        home,
        tool,
        script("probe", json!({})),
        None,
        cancel,
        false,
        false,
    );
    let completed = ran.completed();
    assert_eq!(completed.payload["content"], json!([]));
    assert!(completed.payload.get("artifact").is_none());
    assert!(
        std::fs::read_dir(ran.session.join("artifacts"))
            .unwrap()
            .next()
            .is_none(),
        "the left file was removed"
    );
}

fn command_hook_failure() -> Arc<FakeHooks> {
    FakeHooks::new(
        AfterToolOutcome::Failed(contract::hook::HookFailure {
            extension: "x".into(),
            message: "m".into(),
        }),
        &[],
        Vec::new(),
    )
}

const COMMAND_HOOK_REFUSAL: &str =
    "Output withheld: the `after_tool` hook of extension x could not run: m.";

fn assert_command_hook_failed(ran: &Ran) {
    let completed = ran.completed();
    assert_eq!(completed.payload["status"], "failed");
    assert_eq!(completed.payload["error"]["code"], "hook_failed");
    assert_eq!(completed.payload["error"]["message"], COMMAND_HOOK_REFUSAL);
    assert_eq!(
        completed.payload["content"],
        json!([{"type": "text", "text": COMMAND_HOOK_REFUSAL}])
    );
    assert!(completed.payload.get("artifact").is_none());
    assert!(!ran.on_disk().contains("raw secret"));
    let action = completed.action_id.as_ref().unwrap();
    assert_eq!(
        ran.streamed
            .iter()
            .filter(|line| line.action_id.as_ref() == Some(action))
            .map(|line| line.kind.as_str())
            .collect::<Vec<_>>(),
        [
            "tool_call_requested",
            "tool_call_started",
            "tool_call_completed"
        ]
    );
}

#[test]
fn a_command_hook_failure_withholds_plain_output_and_fails_the_call() {
    let ran = run(
        fixed(text("raw secret")),
        script("probe", json!({})),
        Some(command_hook_failure()),
        Arc::new(TurnCancel::default()),
    );
    assert_command_hook_failed(&ran);
}

fn image_output(mut tool: Fixed, images: Vec<ContentPart>) -> Fixed {
    let make = tool.make.take().unwrap();
    tool.make = Some(Arc::new(move || {
        let mut output = make();
        output.content.extend(images.clone());
        output
    }));
    tool
}

fn one_image() -> Vec<ContentPart> {
    vec![ContentPart::Image {
        path: "artifacts/shot.png".into(),
        mime_type: "image/png".into(),
        width: 1,
        height: 1,
    }]
}

#[test]
fn a_command_hook_failure_replaces_spilled_output_and_suppresses_images() {
    for images in [Vec::new(), one_image()] {
        let home = fakes::TempDir::new("fiber-command-hook-spill");
        let tool = spilling(
            &home,
            b"raw secret".to_vec(),
            "raw secret",
            "",
            0,
            Bound::DEFAULT,
        );
        let ran = run_spilled(
            home,
            image_output(tool, images),
            Some(command_hook_failure()),
        );
        assert_command_hook_failed(&ran);
        assert_eq!(
            std::fs::read_to_string(ran.session.join("artifacts").join(SPILL_NAME)).unwrap(),
            format!("{COMMAND_HOOK_REFUSAL}\n")
        );
    }
}

#[test]
fn a_command_hook_failure_withholds_unsaved_output_and_suppresses_images() {
    for images in [Vec::new(), one_image()] {
        let home = fakes::TempDir::new("fiber-command-hook-unsaved");
        let left = home.path().join("left.txt");
        std::fs::write(&left, "raw secret").unwrap();
        let tool = unsaved(
            "raw secret",
            "",
            0,
            "not saved".into(),
            Some(left.clone()),
            Bound::DEFAULT,
        );
        let ran = run_spilled(
            home,
            image_output(tool, images),
            Some(command_hook_failure()),
        );
        assert_command_hook_failed(&ran);
        assert!(!left.exists());
    }
}

#[test]
fn empty_replaced_content_on_a_spill_sends_no_text_part() {
    let home = fakes::TempDir::new("fiber-spilled-empty");
    let hooks = FakeHooks::new(changed(Some(""), None, None), &["acme"], Vec::new());
    let tool = spilling(&home, vec![b'a'; 100], "HHHH", "TTTT", 92, Bound::DEFAULT);
    let ran = run_spilled(home, tool, Some(hooks));
    let completed = ran.completed();
    assert_eq!(completed.payload["content"], json!([]));
    assert_eq!(completed.payload["artifact"], "artifacts/s_spill.txt");
}

/// `output` carrying the one loaded tool `a`.
fn loaded(text: &str) -> Output {
    Output {
        loaded: Some(vec!["a".into()]),
        ..self::text(text)
    }
}

/// `tool`'s `make` output with `loaded: Some(["a"])` set, for the spill
/// shapes, which build their output at run time.
fn with_loaded(mut tool: Fixed) -> Fixed {
    let make = tool.make.take().expect("a make-built output");
    tool.make = Some(Arc::new(move || {
        let mut output = make();
        output.loaded = Some(vec!["a".into()]);
        output
    }));
    tool
}

#[test]
fn a_withheld_search_result_carries_no_loaded() {
    let hooks = FakeHooks::new(
        AfterToolOutcome::Withheld {
            extension: "acme".into(),
        },
        &[],
        Vec::new(),
    );
    let ran = run(
        fixed(loaded("secret")),
        script("probe", json!({})),
        Some(Arc::clone(&hooks)),
        Arc::new(TurnCancel::default()),
    );
    let completed = ran.completed();
    assert_eq!(completed.payload["status"], "completed");
    assert!(completed.payload.get("loaded").is_none());
    assert_eq!(
        completed.payload["content"],
        json!([{"type": "text", "text": super::withheld_line("acme")}])
    );
}

#[test]
fn a_withheld_spilled_search_result_carries_no_loaded() {
    let home = fakes::TempDir::new("fiber-loaded-withheld-spilled");
    let hooks = FakeHooks::new(
        AfterToolOutcome::Withheld {
            extension: "acme".into(),
        },
        &[],
        Vec::new(),
    );
    let tool = with_loaded(spilling(
        &home,
        vec![b'a'; 100],
        "HHHH",
        "TTTT",
        92,
        Bound::DEFAULT,
    ));
    let ran = run_spilled(home, tool, Some(Arc::clone(&hooks)));
    let completed = ran.completed();
    assert!(completed.payload.get("loaded").is_none());
    assert_eq!(
        completed.payload["content"],
        json!([{"type": "text", "text": super::withheld_line("acme")}])
    );
}

#[test]
fn a_withheld_unsaved_search_result_carries_no_loaded() {
    let home = fakes::TempDir::new("fiber-loaded-withheld-unsaved");
    let message = "could not save the output to /nowhere/s_x.txt: boom".to_owned();
    let hooks = FakeHooks::new(
        AfterToolOutcome::Withheld {
            extension: "acme".into(),
        },
        &[],
        Vec::new(),
    );
    let tool = with_loaded(unsaved("HHHH", "TTTT", 92, message, None, Bound::DEFAULT));
    let ran = run_spilled(home, tool, Some(Arc::clone(&hooks)));
    let completed = ran.completed();
    assert!(completed.payload.get("loaded").is_none());
    assert_eq!(
        completed.payload["content"],
        json!([{"type": "text", "text": super::withheld_line("acme")}])
    );
}

#[test]
fn a_search_result_lost_to_a_failed_hook_carries_no_loaded() {
    let failure = contract::hook::HookFailure {
        extension: "acme".into(),
        message: "broken".into(),
    };
    let hooks = FakeHooks::new(AfterToolOutcome::Failed(failure.clone()), &[], Vec::new());
    let ran = run(
        fixed(loaded("secret")),
        script("probe", json!({})),
        Some(Arc::clone(&hooks)),
        Arc::new(TurnCancel::default()),
    );
    let completed = ran.completed();
    assert_eq!(completed.payload["status"], "failed");
    assert!(completed.payload.get("loaded").is_none());
    assert_eq!(
        completed.payload["content"][0]["text"],
        serde_json::Value::String(super::failed_line(&failure))
    );
}

#[test]
fn a_search_result_shaped_under_a_shutdown_carries_no_loaded() {
    let (ran, hooks) = shut_down_during(loaded("partial secret, long enough to be cut"));
    assert!(hooks.seen().is_empty());
    let completed = ran.completed();
    assert_eq!(completed.payload["content"], json!([]));
    assert!(completed.payload.get("loaded").is_none());
}

#[test]
fn a_hook_rewrite_of_a_search_result_keeps_loaded() {
    let hooks = FakeHooks::new(changed(Some("hooked"), None, None), &["acme"], Vec::new());
    let ran = run(
        fixed(loaded("secret")),
        script("probe", json!({})),
        Some(Arc::clone(&hooks)),
        Arc::new(TurnCancel::default()),
    );
    let completed = ran.completed();
    assert_eq!(completed.payload["loaded"], json!(["a"]));
    assert_eq!(
        completed.payload["content"],
        json!([{"type": "text", "text": "hooked"}])
    );
}

#[test]
fn an_unchanged_search_result_keeps_loaded() {
    let hooks = FakeHooks::new(AfterToolOutcome::Unchanged, &[], Vec::new());
    let ran = run(
        fixed(loaded("secret")),
        script("probe", json!({})),
        Some(Arc::clone(&hooks)),
        Arc::new(TurnCancel::default()),
    );
    let completed = ran.completed();
    assert_eq!(completed.payload["loaded"], json!(["a"]));
    assert_eq!(
        completed.payload["content"],
        json!([{"type": "text", "text": "secret"}])
    );
}

#[test]
fn an_unchanged_spilled_search_result_keeps_loaded() {
    let home = fakes::TempDir::new("fiber-loaded-unchanged-spilled");
    let hooks = FakeHooks::new(AfterToolOutcome::Unchanged, &[], Vec::new());
    let tool = with_loaded(spilling(
        &home,
        vec![b'a'; 100],
        "HHHH",
        "TTTT",
        92,
        Bound::DEFAULT,
    ));
    let ran = run_spilled(home, tool, Some(Arc::clone(&hooks)));
    let completed = ran.completed();
    assert_eq!(completed.payload["loaded"], json!(["a"]));
    let path = ran.session.join("artifacts").join(SPILL_NAME);
    assert_eq!(
        completed.payload["content"],
        json!([{"type": "text", "text": cut_of(&path)}])
    );
}

#[test]
fn an_unchanged_unsaved_search_result_keeps_loaded() {
    let home = fakes::TempDir::new("fiber-loaded-unchanged-unsaved");
    let message = "could not save the output to /nowhere/s_x.txt: boom".to_owned();
    let hooks = FakeHooks::new(AfterToolOutcome::Unchanged, &[], Vec::new());
    let tool = with_loaded(unsaved(
        "HHHH",
        "TTTT",
        92,
        message.clone(),
        None,
        Bound::DEFAULT,
    ));
    let ran = run_spilled(home, tool, Some(Arc::clone(&hooks)));
    let completed = ran.completed();
    assert_eq!(completed.payload["loaded"], json!(["a"]));
    let notice = format!("[92 bytes cut. The full output could not be saved: {message}.]");
    assert_eq!(
        completed.payload["content"],
        json!([{"type": "text", "text": format!("HHHH\n{notice}\nTTTT")}])
    );
}

#[test]
fn a_denied_search_carries_no_loaded() {
    let hooks = FakeHooks::new(changed(Some("x"), None, None), &["acme"], Vec::new());
    let ran = run(
        Fixed {
            effects: vec![Effect::Executes],
            ..fixed(loaded("secret"))
        },
        script("probe", json!({})),
        Some(Arc::clone(&hooks)),
        Arc::new(TurnCancel::default()),
    );
    assert!(hooks.seen().is_empty());
    assert_eq!(ran.completed().payload["status"], "denied");
    assert!(ran.completed().payload.get("loaded").is_none());
}

#[test]
fn a_cancelled_search_is_shown_to_the_hook_as_the_cancel_line() {
    let hooks = FakeHooks::new(AfterToolOutcome::Unchanged, &[], Vec::new());
    let cancel = Arc::new(TurnCancel::default());
    let ran = run(
        Fixed {
            cancel: Some(Arc::clone(&cancel)),
            ..fixed(loaded("partial secret"))
        },
        script("probe", json!({})),
        Some(Arc::clone(&hooks)),
        cancel,
    );
    let seen = hooks.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].status, CallStatus::Cancelled);
    assert_eq!(
        seen[0].content,
        "The search was cancelled; no tool was loaded."
    );
    let completed = ran.completed();
    assert_eq!(completed.payload["status"], "cancelled");
    assert!(completed.payload.get("loaded").is_none());
    assert_eq!(
        completed.payload["content"],
        json!([{"type": "text", "text": "The search was cancelled; no tool was loaded."}])
    );
}

#[test]
fn a_cancelled_search_rewritten_before_it_ran_completes_with_the_cancel_line_only() {
    let mut hooks = FakeHooks::new(AfterToolOutcome::Unchanged, &[], Vec::new());
    Arc::get_mut(&mut hooks).unwrap().before = Some(BeforeToolOutcome::Rewritten {
        arguments: Map::new(),
        changed_by: vec!["acme".into()],
    });
    let cancel = Arc::new(TurnCancel::default());
    let ran = run(
        Fixed {
            cancel: Some(Arc::clone(&cancel)),
            ..fixed(loaded("partial secret"))
        },
        script("probe", json!({})),
        Some(Arc::clone(&hooks)),
        cancel,
    );
    let completed = ran.completed();
    assert_eq!(completed.payload["status"], "cancelled");
    assert!(completed.payload.get("loaded").is_none());
    assert_eq!(
        completed.payload["content"],
        json!([{"type": "text", "text": "The search was cancelled; no tool was loaded."}])
    );
}
