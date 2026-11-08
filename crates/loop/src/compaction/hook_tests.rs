//! A compaction under a shutdown: neither a hook nor a note request runs,
//! with hooks set or not.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;

use contract::TurnId;
use contract::events::{CompactionTrigger, EventKind, Outcome};
use contract::hook::{
    AfterToolAnswer, AfterToolCall, AfterToolOutcome, BeforeCompaction, BeforeCompactionAnswer,
    BeforeCompactionOutcome, BeforeMessage, BeforeMessageAnswer, BeforeMessageOutcome,
    BeforeModelCall, BeforeToolAnswer, BeforeToolCall, BeforeToolOutcome, CheckAnswer,
    CheckOutcome, ContextAnswer, ContextOutcome, Hooks,
};
use contract::inbox::Delivery;
use fakes::clock::FakeClock;
use fakes::{NoRules, ScriptedProvider};
use log::Log;
use serde_json::json;

use crate::TurnCancel;

/// A `Hooks` fake that counts its `before_compaction` calls and answers a
/// hook's note. Under a shutdown it is never called.
struct CompactionHooks {
    calls: AtomicUsize,
}

impl CompactionHooks {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Hooks for CompactionHooks {
    fn after_tool(&self, _call: &AfterToolCall<'_>) -> AfterToolAnswer {
        AfterToolAnswer {
            outcome: AfterToolOutcome::Unchanged,
            changed_by: Vec::new(),
            notices: Vec::new(),
        }
    }

    fn before_tool(&self, _call: &BeforeToolCall<'_>) -> BeforeToolAnswer {
        BeforeToolAnswer {
            outcome: BeforeToolOutcome::Unchanged,
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

    fn session_start(&self, _start: &contract::hook::SessionStart<'_>) -> ContextAnswer {
        ContextAnswer {
            outcome: ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }

    fn turn_start(&self, _turn: &contract::hook::TurnStart<'_>) -> ContextAnswer {
        ContextAnswer {
            outcome: ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }

    fn before_model_call(&self, _call: &BeforeModelCall<'_>) -> CheckAnswer {
        CheckAnswer {
            outcome: CheckOutcome::Passed,
            notices: Vec::new(),
        }
    }

    fn turn_end(&self, _end: &contract::hook::TurnEnd<'_>) -> ContextAnswer {
        ContextAnswer {
            outcome: ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }

    fn before_compaction(&self, _compaction: &BeforeCompaction<'_>) -> BeforeCompactionAnswer {
        self.calls.fetch_add(1, Ordering::SeqCst);
        BeforeCompactionAnswer {
            outcome: BeforeCompactionOutcome::Note {
                text: "Hook note.".into(),
                extension: "fiber.test/notes".into(),
            },
            notices: Vec::new(),
        }
    }

    fn deliver_to(&self, _inbox: mpsc::Sender<Delivery>) {}
}

#[test]
fn a_shutdown_cancels_a_compaction_before_any_hook_or_note_request() {
    for with_hooks in [true, false] {
        let home = fakes::TempDir::new("fiber-compaction-shutdown");
        let workspace = home.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let clock = FakeClock::new();
        let dyn_clock: Arc<dyn contract::clock::Clock> = clock.clone();
        let log = Arc::new(
            Log::create(home.path(), contract::SessionId("s_test".into()), dyn_clock).unwrap(),
        );
        let provider = Arc::new(ScriptedProvider::new(Vec::new()));
        let (_inbox, rx) = mpsc::channel();
        let cancel = Arc::new(TurnCancel::default());
        let hooks = CompactionHooks::new();
        let mut looped = crate::Loop::start(
            crate::Session {
                log: Arc::clone(&log),
                provider: Arc::clone(&provider) as Arc<dyn contract::provider::Provider>,
                model: crate::Model {
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
                    Arc::clone(&clock) as Arc<dyn contract::clock::Clock>,
                    fakes::CONTEXT_WINDOW,
                ),
                inbox: rx,
                tools: Vec::new(),
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
        .cancelled_by(Arc::clone(&cancel))
        .budget(Some(0.0));
        if with_hooks {
            looped = looped.hooks(Arc::clone(&hooks) as Arc<dyn Hooks>);
        }
        assert!(looped.ensure_preamble().unwrap());
        cancel.shutdown(143);
        let outcome = looped
            .run_compaction(
                &TurnId("t_test".into()),
                super::note::Source::Requested {
                    trigger: CompactionTrigger::Auto,
                    instructions: None,
                },
                1200,
            )
            .unwrap();
        assert_eq!(outcome, Outcome::Cancelled, "with_hooks: {with_hooks}");
        let lines = log::read(&home.path().join("s_test")).unwrap();
        let kinds: Vec<EventKind> = lines.iter().map(|line| line.kind()).collect();
        // The preamble build is the test's setup; the compaction writes the
        // last two lines.
        assert_eq!(
            kinds,
            [
                EventKind::SessionStarted,
                EventKind::PreambleBuilt,
                EventKind::OpeningMessage,
                EventKind::CompactionStarted,
                EventKind::CompactionCompleted,
            ]
        );
        assert_eq!(
            serde_json::Value::Object(lines[4].payload.clone()),
            json!({"outcome": "cancelled", "tokens_before": 1200}),
            "with_hooks: {with_hooks}"
        );
        assert!(
            provider.requests().is_empty(),
            "no note request runs: {with_hooks}"
        );
        assert_eq!(hooks.calls(), 0, "no hook runs: {with_hooks}");
    }
}
