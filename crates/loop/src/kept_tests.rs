#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};

use contract::events::{Event, InputItem, JobCompleted, JobStarted, Outcome};
use contract::hook::{self, AfterToolAnswer, AfterToolCall, AfterToolOutcome, Hooks, JobDelivery};
use contract::shapes::{ContentPart, Origin, Sender};
use contract::{Envelope, JobId, SessionId};
use fakes::{Deadline, MUST_SUCCEED_WITHIN};
use serde_json::{Value, json};

fn line(kind: &str, payload: Value) -> Envelope {
    serde_json::from_value(json!({"kind":kind,"session_id":"s_one","ts":0,
        "schema_version":1,"seq":1,"payload":payload}))
    .expect("envelope")
}
fn completed(id: &str) -> Envelope {
    line("job_completed", json!({"job_id":id,"status":"completed"}))
}

#[test]
fn completions_from_multiple_crashes_keep_log_order_until_a_turn_consumes_them() {
    let lines = vec![
        line("fiber_started", json!({})),
        completed("j_one"),
        line("fiber_started", json!({})),
        completed("j_two"),
    ];
    assert_eq!(
        super::collect(&lines)
            .expect("kept")
            .iter()
            .map(|end| end.job_id.0.as_str())
            .collect::<Vec<_>>(),
        vec!["j_one", "j_two"]
    );
    let mut consumed = lines;
    consumed.push(line("turn_started", json!({"input":[]})));
    assert!(super::collect(&consumed).expect("consumed").is_empty());
}

#[test]
fn session_and_extension_owned_endings_are_not_keeper_deliveries() {
    let mut orphan = completed("j_orphan");
    orphan
        .payload
        .insert("error".into(), json!({"code":"orphaned","message":"gone"}));
    let mut session = completed("j_session");
    session.turn_id = Some(contract::TurnId("t_one".into()));
    let mut hooked = completed("j_hooked");
    hooked
        .payload
        .insert("notice".into(), json!("already delivered"));
    let lines = vec![
        completed("j_before"),
        line("fiber_exited", json!({})),
        line("fiber_started", json!({})),
        orphan,
        session,
        hooked,
        line(
            "job_started",
            json!({"job_id":"j_extension","extension":"x"}),
        ),
        completed("j_extension"),
        completed("j_keeper"),
    ];
    assert_eq!(
        super::collect(&lines)
            .expect("kept")
            .iter()
            .map(|end| end.job_id.0.as_str())
            .collect::<Vec<_>>(),
        vec!["j_keeper"]
    );
}

#[test]
fn completions_in_closed_process_spans_are_not_delivered() {
    let lines = vec![
        line("fiber_started", json!({})),
        completed("j_closed"),
        line("moved", json!({})),
        line("fiber_started", json!({})),
        completed("j_keeper"),
    ];
    assert_eq!(
        super::collect(&lines)
            .expect("kept")
            .iter()
            .map(|end| end.job_id.0.as_str())
            .collect::<Vec<_>>(),
        vec!["j_keeper"]
    );
}

fn with_home(what: &str, test: impl FnOnce(PathBuf) + Send + 'static) {
    let home = fakes::TempDir::new("fiber-kept");
    let watchdog = fakes::Watchdog::matching(&home.path().display().to_string());
    let path = home.path().to_path_buf();
    fakes::within(what, Deadline::after(MUST_SUCCEED_WITHIN), move || {
        test(path);
    });
    watchdog.stand_down(Deadline::after(MUST_SUCCEED_WITHIN));
}

fn with_loop(test: impl FnOnce(crate::Loop) + Send + 'static) {
    with_home("kept completions delivered", move |home| {
        let workspace = home.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
        let log = Arc::new(
            log::Log::create(&home, SessionId("s_test".into()), Arc::clone(&clock)).unwrap(),
        );
        let (_inbox, rx) = mpsc::channel();
        let looped = crate::Loop::start(
            crate::Session {
                log,
                provider: Arc::new(fakes::ScriptedProvider::new(Vec::new())),
                model: crate::Model {
                    reference: "fake/model".into(),
                    cost: None,
                    subscription: false,
                },
                prompt: crate::prompt::PromptInputs::new(
                    home.clone(),
                    "/bin/sh".into(),
                    home.join("s_test/events.jsonl").display().to_string(),
                    clock,
                    fakes::CONTEXT_WINDOW,
                ),
                inbox: rx,
                tools: Vec::new(),
                permissions: crate::Permissions {
                    workspace: workspace.display().to_string(),
                    credentials: home.join("credentials"),
                    credential_files: Vec::new(),
                    rules: Arc::new(fakes::NoRules),
                },
            },
            None,
        )
        .unwrap();
        test(looped);
    });
}

fn job(id: &str, status: Outcome) -> JobCompleted {
    serde_json::from_value(json!({"job_id": id, "status": status})).unwrap()
}

#[test]
fn kept_ended_completions_load_tails_but_unstarted_does_not() {
    with_loop(|mut looped| {
        for (id, status, output) in [
            ("j_failed", Outcome::Failed, "failure details\n"),
            ("j_completed", Outcome::Completed, "successful output\n"),
            ("j_cancelled", Outcome::Cancelled, "cancelled output\n"),
        ] {
            let path = format!("artifacts/{id}.log");
            std::fs::write(looped.log.dir().join(&path), output).unwrap();
            looped
                .write(
                    &Event::JobStarted(JobStarted {
                        job_id: JobId(id.into()),
                        tool: Some("shell".into()),
                        extension: None,
                        description: "kept job".into(),
                        output_path: path,
                    }),
                    None,
                    None,
                )
                .unwrap();
            looped.kept.push(job(id, status));
        }
        looped.kept.push(job("j_unstarted", Outcome::Failed));
        // `docs/events.md`, "`job_completed`": every ended outcome carries
        // the tail of its output, capped; "Resume": keeper completions are
        // sent with the next turn.
        assert_eq!(
            looped.deliver_kept().unwrap(),
            Some(InputItem::Message {
                content: vec![ContentPart::Text {
                    text: concat!(
                        "Fiber: background job j_failed ended: failed.\n",
                        "Last output:\nfailure details\n\n",
                        "Fiber: background job j_completed ended: completed.\n",
                        "Last output:\nsuccessful output\n\n",
                        "Fiber: background job j_cancelled ended: cancelled.\n",
                        "Last output:\ncancelled output\n\n",
                        "Fiber: background job j_unstarted ended: failed.",
                    )
                    .into(),
                }],
                sender: Sender {
                    origin: Origin::Fiber,
                    command_id: None,
                },
                changed_by: None,
            })
        );
        assert!(looped.kept.is_empty());
        assert_eq!(looped.deliver_kept().unwrap(), None);
    });
}

struct CompletionHooks {
    seen: Mutex<Vec<String>>,
}

impl Hooks for CompletionHooks {
    fn after_tool(&self, call: &AfterToolCall<'_>) -> AfterToolAnswer {
        assert_eq!(call.delivery, Some(JobDelivery::Completion));
        self.seen.lock().unwrap().push(call.content.to_owned());
        AfterToolAnswer {
            outcome: AfterToolOutcome::Changed {
                content: Some(format!("Reviewed: {}", call.content)),
                details: None,
                artifact: None,
            },
            changed_by: vec!["shared".into(), "redactor".into()],
            notices: Vec::new(),
        }
    }

    fn before_tool(&self, _: &hook::BeforeToolCall<'_>) -> hook::BeforeToolAnswer {
        hook::BeforeToolAnswer {
            outcome: hook::BeforeToolOutcome::Unchanged,
            notices: Vec::new(),
        }
    }

    fn check_tool(&self, _: &hook::BeforeToolCall<'_>) -> hook::CheckAnswer {
        hook::CheckAnswer {
            outcome: hook::CheckOutcome::Passed,
            notices: Vec::new(),
        }
    }

    fn before_message(&self, _: &hook::BeforeMessage<'_>) -> hook::BeforeMessageAnswer {
        hook::BeforeMessageAnswer {
            outcome: hook::BeforeMessageOutcome::Passed {
                replaced: None,
                context: Vec::new(),
            },
            notices: Vec::new(),
        }
    }

    fn session_start(&self, _: &hook::SessionStart<'_>) -> hook::ContextAnswer {
        hook::ContextAnswer {
            outcome: hook::ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }

    fn turn_start(&self, _: &hook::TurnStart<'_>) -> hook::ContextAnswer {
        hook::ContextAnswer {
            outcome: hook::ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }

    fn before_model_call(&self, _: &hook::BeforeModelCall<'_>) -> hook::CheckAnswer {
        hook::CheckAnswer {
            outcome: hook::CheckOutcome::Passed,
            notices: Vec::new(),
        }
    }

    fn turn_end(&self, _: &hook::TurnEnd<'_>) -> hook::ContextAnswer {
        hook::ContextAnswer {
            outcome: hook::ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }

    fn before_handoff(&self, _: &hook::BeforeHandoff<'_>) -> hook::BeforeHandoffAnswer {
        fakes::no_note()
    }

    fn deliver_to(&self, _: mpsc::Sender<contract::inbox::Delivery>) {}
}

#[test]
fn kept_completion_hooks_share_deduplicated_attribution() {
    with_loop(|looped| {
        let hooks = Arc::new(CompletionHooks {
            seen: Mutex::new(Vec::new()),
        });
        let mut looped = looped.hooks(hooks.clone());
        looped.kept = vec![
            job("j_one", Outcome::Completed),
            job("j_two", Outcome::Completed),
        ];
        // `docs/events.md`, "`changed_by`": extensions are named once in
        // hook order, even when they changed both completion notices.
        assert_eq!(
            looped.deliver_kept().unwrap(),
            Some(InputItem::Message {
                content: vec![ContentPart::Text {
                    text: concat!(
                        "Reviewed: Fiber: background job j_one ended: completed.\n\n",
                        "Reviewed: Fiber: background job j_two ended: completed.",
                    )
                    .into(),
                }],
                sender: Sender {
                    origin: Origin::Fiber,
                    command_id: None,
                },
                changed_by: Some(vec!["shared".into(), "redactor".into()]),
            })
        );
        assert_eq!(
            *hooks.seen.lock().unwrap(),
            [
                "Fiber: background job j_one ended: completed.",
                "Fiber: background job j_two ended: completed.",
            ]
        );
        assert_eq!(looped.deliver_kept().unwrap(), None);
    });
}

#[test]
fn tail_reads_present_artifacts_and_omits_missing_or_empty_output() {
    with_home("artifact tails read", |dir| {
        std::fs::create_dir_all(dir.join("artifacts")).unwrap();
        let present = Path::new("artifacts/present.log");
        std::fs::write(dir.join(present), "\u{10000} output\n").unwrap();
        assert_eq!(
            super::tail(&dir, present).as_deref(),
            Some("\u{10000} output\n")
        );
        let empty = Path::new("artifacts/empty.log");
        std::fs::write(dir.join(empty), "").unwrap();
        assert_eq!(super::tail(&dir, empty), None);
        assert_eq!(super::tail(&dir, Path::new("artifacts/missing.log")), None);
    });
}

#[test]
fn tail_discards_only_split_utf8_prefix_at_the_cap() {
    with_home("split UTF-8 artifact tails read", |dir| {
        std::fs::create_dir_all(dir.join("artifacts")).unwrap();
        let recorded = Path::new("artifacts/long.log");
        for continuation_bytes in 0..=3 {
            let suffix = "z".repeat(2048 - continuation_bytes);
            // The capped read starts inside the four-byte character,
            // or immediately after it when no continuation byte remains.
            let output = format!("discarded\u{10000}{suffix}");
            std::fs::write(dir.join(recorded), output).unwrap();
            let tail = super::tail(&dir, recorded).expect("nonempty UTF-8 tail");
            assert_eq!(tail.len(), suffix.len());
            assert!(
                tail == suffix,
                "tail content after {continuation_bytes} split bytes"
            );
        }
    });
}
