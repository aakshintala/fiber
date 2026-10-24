//! A supervised session runs unanswerable and stays that way
//! (`docs/permissions.md`, "Supervised sessions"): escalations become
//! blocks through the reused delegate seam, a supervisor steer or a person
//! prompt reaches the reviewer as person words, and a retry is judged
//! again.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use contract::emit::Emit;
use contract::events::{TurnOutcome, VariablesSource};
use contract::inbox::{Ack, Delivery, Message};
use contract::provider::{Delta, Provider, ReplyAction, ToolDefinition};
use contract::shapes::{ContentPart, DeclaredEffects, Effect, Origin, Sender};
use contract::tool::{Bound, Cancel, Effects, EffectsError, Output, Tool};
use contract::{CommandId, SessionId};
use fakes::{Deadline, NoRules};
use log::Log;
use serde_json::{Map, Value, json};

use super::variables_source;
use crate::{BlockLimits, Loop, Model, Reviewer, TurnCancel};

/// How long a driven turn may take before the test fails naming the
/// missing turn end, instead of hanging.
const TURN_DEADLINE: Duration = Duration::from_secs(10);

/// A tool whose calls always meet the reviewer, and return one line.
struct ProbeTool {
    ran: std::sync::Mutex<Vec<Value>>,
}

impl ProbeTool {
    fn new() -> Self {
        Self {
            ran: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn ran(&self) -> Vec<Value> {
        self.ran.lock().unwrap().clone()
    }
}

impl Tool for ProbeTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "probe".to_owned(),
            description: "A reading tool.".to_owned(),
            input_schema: json!({"type": "object"}),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, _: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Ok(Effects {
            declared: DeclaredEffects {
                effects: vec![Effect::Reads],
                reversible: true,
                paths: None,
            },
            subject: None,
            prefix: None,
            always_reviewed: true,
            hosts: None,
        })
    }

    fn run(&self, arguments: &Map<String, Value>, _: &dyn Cancel, _: &dyn Emit) -> Output {
        self.ran
            .lock()
            .unwrap()
            .push(Value::Object(arguments.clone()));
        Output {
            content: vec![ContentPart::Text {
                text: "Ran it.".into(),
            }],
            ..Output::default()
        }
    }

    fn bound(&self) -> Bound {
        Bound::DEFAULT
    }
}

/// A supervised loop on a scripted provider, with its inbox, log and
/// session directory held.
struct Supervised {
    looped: Option<Loop>,
    inbox: mpsc::Sender<Delivery>,
    dir: std::path::PathBuf,
    _home: fakes::TempDir,
}

fn harness(script: Vec<fakes::Scripted>, tool: Arc<ProbeTool>) -> Supervised {
    let home = fakes::TempDir::new("fiber-supervised");
    let workspace = home.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let credentials = home.path().join("credentials");
    std::fs::create_dir_all(&credentials).unwrap();
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let log =
        Arc::new(Log::create(home.path(), SessionId("s_test".into()), Arc::clone(&clock)).unwrap());
    let provider = Arc::new(fakes::ScriptedProvider::new(script));
    let (inbox, rx) = mpsc::channel();
    let session_log = home
        .path()
        .join("s_test/events.jsonl")
        .display()
        .to_string();
    let looped = Loop::supervised(
        crate::Session {
            log: Arc::clone(&log),
            provider: Arc::clone(&provider) as Arc<dyn Provider>,
            model: Model {
                reference: "fake/model".into(),
                cost: None,
                subscription: false,
            },
            prompt: crate::prompt::PromptInputs::new(
                home.path().to_path_buf(),
                "/bin/sh".into(),
                session_log,
                Arc::clone(&clock),
                fakes::CONTEXT_WINDOW,
            ),
            inbox: rx,
            tools: vec![("probe".to_owned(), tool as Arc<dyn Tool>)],
            permissions: crate::Permissions {
                workspace: workspace.display().to_string(),
                credentials,
                credential_files: Vec::new(),
                rules: Arc::new(NoRules),
            },
        },
        None,
    )
    .unwrap()
    .cancelled_by(Arc::new(TurnCancel::default()));
    Supervised {
        looped: Some(looped),
        inbox,
        dir: home.path().join("s_test"),
        _home: home,
    }
}

impl Supervised {
    /// Gives the loop a reviewer answering `script`, returning its
    /// provider so the test reads what each reviewer request carried.
    fn reviewer(&mut self, script: Vec<fakes::Scripted>) -> Arc<fakes::ScriptedProvider> {
        let provider = Arc::new(fakes::ScriptedProvider::new(script));
        let looped = self.looped.take().unwrap().reviewer(
            Ok(Reviewer {
                provider: Arc::clone(&provider) as Arc<dyn Provider>,
                model: Model {
                    reference: "fake/reviewer".into(),
                    cost: None,
                    subscription: false,
                },
                thinking_levels: Vec::new(),
                cache_lifetime: contract::events::CacheLifetime::OneHour,
                context_window: 1_000_000,
            }),
            BlockLimits::default(),
        );
        self.looped = Some(looped);
        provider
    }

    /// Sends the driver's `text` as the next turn's prompt.
    fn prompt(&self, text: &str) {
        self.inbox
            .send(Delivery::Prompt(
                driver_message(text, "c_go"),
                Ack(Box::new(|_| {})),
            ))
            .unwrap();
    }

    /// Sends the driver's `text` as steering, as a supervisor's `steer`
    /// arrives.
    fn steer(&self, text: &str) {
        self.inbox
            .send(Delivery::Steer(
                driver_message(text, "c_steer"),
                Ack(Box::new(|_| {})),
            ))
            .unwrap();
    }

    /// Runs one turn on its own thread, failing at [`TURN_DEADLINE`]
    /// naming the missing turn end.
    #[track_caller]
    fn turn(&mut self) -> Option<TurnOutcome> {
        let mut looped = self.looped.take().unwrap();
        let (done, finished) = mpsc::channel();
        thread::spawn(move || {
            let outcome = looped.turn().unwrap();
            done.send((looped, outcome)).unwrap();
        });
        let (looped, outcome) = Deadline::after(TURN_DEADLINE)
            .recv(&finished)
            .expect("the turn ended in time");
        self.looped = Some(looped);
        outcome
    }

    fn lines(&self) -> Vec<contract::Envelope> {
        log::read(&self.dir).unwrap()
    }
}

fn driver_message(text: &str, command: &str) -> Message {
    Message {
        content: vec![ContentPart::Text { text: text.into() }],
        sender: Sender {
            origin: Origin::Driver,
            command_id: Some(CommandId(command.into())),
        },
    }
}

/// A model reply requesting one `probe` call.
fn call_reply() -> fakes::Scripted {
    let arguments = json!({"city": "Paris"});
    let mut end = fakes::reply("Checking.");
    end.actions
        .push(ReplyAction::ToolCall(contract::events::ToolCallRequested {
            name: "probe".into(),
            arguments: arguments.clone(),
            provider_id: None,
            repair: None,
            ran_by: None,
            provider_item: None,
        }));
    fakes::Scripted {
        deltas: vec![
            Delta::Text(contract::events::TextDelta {
                text: "Checking.".into(),
            }),
            Delta::ToolCallArguments(contract::events::ToolCallArgumentsDelta {
                index: 0,
                name: Some("probe".into()),
                text: arguments.to_string(),
            }),
        ],
        end: Ok(end),
    }
}

fn kinds(lines: &[contract::Envelope]) -> Vec<&str> {
    lines.iter().map(|line| line.kind.as_str()).collect()
}

fn resolved(lines: &[contract::Envelope]) -> Vec<&contract::Envelope> {
    lines
        .iter()
        .filter(|line| line.kind == "permission_resolved")
        .collect()
}

#[test]
fn supervised_start_writes_supervised_and_denies_without_asking() {
    assert_eq!(variables_source(), VariablesSource::Inherited);
    let tool = Arc::new(ProbeTool::new());
    let mut session = harness(
        vec![call_reply(), fakes::Scripted::text("Done.")],
        Arc::clone(&tool),
    );
    session.reviewer(vec![
        fakes::Scripted::text("maybe ZQX-FIRST"),
        fakes::Scripted::text("perhaps ZQX-LAST"),
    ]);
    session.prompt("go");
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    let started = lines
        .iter()
        .find(|line| line.kind == "session_started")
        .expect("a session_started line");
    assert_eq!(started.payload["supervised"], json!(true));
    // Nobody was asked: no approval waits on anyone.
    assert!(!kinds(&lines).contains(&"permission_requested"));
    assert!(!kinds(&lines).contains(&"notice"));
    let denied = resolved(&lines);
    assert_eq!(denied.len(), 1);
    assert_eq!(denied[0].payload["decision"], json!("deny"));
    assert_eq!(denied[0].payload["decided_by"], json!("reviewer"));
    // The turn carries on past the block to the model's close.
    let end = lines
        .iter()
        .find(|line| line.kind == "turn_completed")
        .expect("a turn_completed line");
    assert_eq!(end.payload["outcome"], json!("completed"));
    assert!(tool.ran().is_empty());
}

#[test]
fn supervised_retry_after_a_steer_is_judged_again_and_allowed() {
    let tool = Arc::new(ProbeTool::new());
    let mut session = harness(
        vec![
            call_reply(),
            fakes::Scripted::text("Done."),
            call_reply(),
            fakes::Scripted::text("Done."),
        ],
        Arc::clone(&tool),
    );
    let reviewer = session.reviewer(vec![
        fakes::Scripted::text("maybe ZQX-FIRST"),
        fakes::Scripted::text("perhaps ZQX-LAST"),
        fakes::Scripted::text("check"),
        fakes::Scripted::text("allow fine"),
    ]);
    session.prompt("go");
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(resolved(&session.lines()).len(), 1);
    // The supervisor steers its grant as driver words; the person's own
    // prompt would arrive the same way.
    session.steer("you may run probe because I asked");
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    // The retry is judged again, never answered by the steer itself: each
    // judged call takes a first and a second pass.
    assert_eq!(reviewer.requests().len(), 4);
    assert!(!kinds(&lines).contains(&"permission_requested"));
    let judged = resolved(&lines);
    assert_eq!(judged.len(), 2);
    assert_eq!(judged[0].payload["decision"], json!("deny"));
    assert_eq!(judged[1].payload["decision"], json!("allow"));
    assert_eq!(judged[1].payload["decided_by"], json!("reviewer"));
    assert_eq!(tool.ran().len(), 1);
}
