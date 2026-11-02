use super::*;
use crate::inbox::{TurnInput, Waited};
use crate::{Loop, Model};
use contract::events::{
    CacheLifetime, CommandResult, Event, PreambleBuilt, PreambleReason, UsageRecorded,
};
use contract::inbox::{Ack, Answer, Delivery};
use contract::notes::{AllowsTally, Evidence, NotesAsk, ProposeNotesArgs, SessionTally, Topic};
use contract::provider::{Input, Provider};
use contract::{ErrorCode, SessionId, TurnId};
use fakes::{Deadline, Scripted, ScriptedProvider};
use std::sync::{Arc, mpsc};
use std::time::Duration;

const DEADLINE: Duration = Duration::from_secs(10);

fn started(
    provider: Arc<dyn Provider>,
) -> (
    Loop,
    Arc<log::Log>,
    fakes::TempDir,
    Arc<fakes::clock::FakeClock>,
) {
    let home = fakes::TempDir::new("fiber-notes");
    let workspace = home.path().join("workspace");
    let credentials = home.path().join("credentials");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&credentials).unwrap();
    let clock = fakes::clock::FakeClock::new();
    let log =
        Arc::new(log::Log::create(home.path(), SessionId("s_test".into()), clock.clone()).unwrap());
    let (_tx, rx) = mpsc::channel();
    let looped = Loop::start(
        crate::Session {
            log: log.clone(),
            provider,
            model: Model {
                reference: "fake/model".into(),
                cost: None,
                subscription: false,
            },
            prompt: crate::prompt::PromptInputs::new(
                home.path().to_path_buf(),
                "/bin/sh".into(),
                log.dir().join("events.jsonl").display().to_string(),
                clock.clone(),
                fakes::CONTEXT_WINDOW,
            ),
            inbox: rx,
            tools: Vec::new(),
            permissions: crate::Permissions {
                workspace: workspace.display().to_string(),
                credentials,
                credential_files: Vec::new(),
                rules: Arc::new(fakes::NoRules),
            },
        },
        None,
    )
    .unwrap();
    (looped, log, home, clock)
}

fn ask(answer: Option<&str>) -> NotesAsk {
    NotesAsk {
        args: ProposeNotesArgs {
            topic: Topic::SourceControl,
            answer: answer.map(str::to_owned),
            earlier: vec!["  earlier line  ".into(), "  ".into()],
        },
        evidence: vec![Evidence::Files {
            text: "remote evidence".into(),
        }],
    }
}

fn capture() -> (Ack, mpsc::Receiver<Answer>) {
    let (tx, rx) = mpsc::channel();
    (
        Ack(Box::new(move |answer| {
            tx.send(answer).unwrap();
        })),
        rx,
    )
}

fn received(rx: &mpsc::Receiver<Answer>) -> Answer {
    Deadline::after(DEADLINE).recv_or_fail(rx, "notes acknowledgement")
}

fn new_lines(log: &log::Log, before: u64) -> Vec<contract::Envelope> {
    log.range(before, 256).unwrap()
}

#[test]
fn idle_notes_send_only_evidence_and_write_usage_before_answering() {
    let provider = Arc::new(ScriptedProvider::new([Scripted::text(
        "1 | remote rule\n2 | answer rule",
    )]));
    let (mut looped, log, _home, _clock) = started(provider.clone());
    log.append(
        &Event::TurnStarted(contract::events::TurnStarted {
            input: vec![contract::events::InputItem::Message {
                content: vec![contract::shapes::ContentPart::Text {
                    text: "PRIVATE PRIOR TURN".into(),
                }],
                sender: contract::shapes::Sender {
                    origin: contract::shapes::Origin::Driver,
                    command_id: None,
                },
                changed_by: None,
            }],
        }),
        Some(TurnId("t_prior".into())),
        None,
    )
    .unwrap();
    let before = log.count();
    let (tx, rx) = mpsc::channel();
    let at_ack = log.clone();
    let ack = Ack(Box::new(move |answer| {
        tx.send((answer, at_ack.count())).unwrap();
    }));
    let mut input = TurnInput::of(Vec::new());
    looped
        .admit_idle(
            Delivery::ProposeNotes(ask(Some("  person answer  ")), ack),
            &mut input,
        )
        .unwrap();
    let (answer, count) = Deadline::after(DEADLINE).recv_or_fail(&rx, "notes result after usage");
    assert_eq!(count, before + 1);
    let Ok(Some(CommandResult::ProposeNotes {
        evidence,
        proposed,
        error,
    })) = answer
    else {
        panic!("unexpected answer: {answer:?}");
    };
    assert_eq!(
        evidence,
        vec![
            Evidence::Files {
                text: "remote evidence".into()
            },
            Evidence::Answer {
                text: "person answer".into()
            }
        ]
    );
    assert_eq!(
        proposed.iter().map(|p| p.text()).collect::<Vec<_>>(),
        ["remote rule", "answer rule"]
    );
    assert_eq!(proposed[0].cites(), [0]);
    assert_eq!(proposed[1].cites(), [1]);
    assert!(error.is_none());
    let requests = provider.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert!(request.tools.is_empty());
    assert_eq!(request.tool_choice, "auto");
    assert_eq!(request.cache_key, "s_test:notes");
    assert_eq!(request.cache_lifetime, CacheLifetime::FiveMinutes);
    assert_eq!(request.thinking, looped.chosen);
    assert_eq!(request.previous_end, None);
    assert_eq!(request.sent_tools, None);
    assert_eq!(request.max_output_tokens, Some(4096));
    assert!(request.media.is_empty());
    assert!(
        request
            .system_prompt
            .contains(Topic::SourceControl.covers())
    );
    assert_eq!(&request.conversation[..], vec![Input::User { text: "Evidence:\n1 [files] | remote evidence\n2 [answer] | person answer\n\nEarlier lines:\n- earlier line\n".into(), images: Vec::new() }]);
    let lines = new_lines(&log, before);
    assert_eq!(
        lines.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(),
        ["usage_recorded"]
    );
    assert_eq!(lines[0].turn_id, None);
    assert_eq!(lines[0].action_id, None);
    assert!(lines[0].payload.get("reviewer").is_none());
    assert_eq!(lines[0].payload["model"], "fake/model");
}

#[test]
fn blank_answers_and_earlier_lines_do_not_enter_the_request() {
    for answer in [None, Some(" \t ")] {
        let provider = Arc::new(ScriptedProvider::new([Scripted::text("1 | remote rule")]));
        let (mut looped, _log, _home, _clock) = started(provider.clone());
        let (ack, rx) = capture();
        looped.take_notes(ask(answer), ack).unwrap();
        let Ok(Some(CommandResult::ProposeNotes { evidence, .. })) = received(&rx) else {
            panic!("missing result");
        };
        assert_eq!(evidence.len(), 1);
        assert_eq!(
            &provider.requests()[0].conversation[..],
            vec![Input::User {
                text: "Evidence:\n1 [files] | remote evidence\n\nEarlier lines:\n- earlier line\n"
                    .into(),
                images: Vec::new()
            }]
        );
    }
}

#[test]
fn failed_and_cancelled_calls_write_usage_then_accept_with_an_error() {
    let failure = contract::shapes::Failure {
        code: ErrorCode::IoFailed,
        message: "scripted failure".into(),
        retry_after_ms: None,
        provider: None,
    };
    for (script, expected) in [
        (
            Scripted::failed_after(failure.clone(), fakes::reply("").usage()),
            failure,
        ),
        (
            Scripted::cancelled_after(fakes::reply("").usage()),
            contract::shapes::Failure {
                code: ErrorCode::Cancelled,
                message: "The notes request was cancelled.".into(),
                retry_after_ms: None,
                provider: None,
            },
        ),
    ] {
        let provider = Arc::new(ScriptedProvider::new([script]));
        let (mut looped, log, _home, _clock) = started(provider.clone());
        let before = log.count();
        let (ack, rx) = capture();
        let at_ack = log.clone();
        let ack = Ack(Box::new(move |answer| {
            assert_eq!(
                at_ack.count(),
                before + 1,
                "failed calls record usage before answering"
            );
            (ack.0)(answer);
        }));
        looped.take_notes(ask(None), ack).unwrap();
        let Ok(Some(CommandResult::ProposeNotes {
            proposed,
            error,
            evidence,
        })) = received(&rx)
        else {
            panic!("missing result");
        };
        assert!(proposed.is_empty());
        assert_eq!(evidence.len(), 1);
        assert_eq!(*error.unwrap(), expected);
        assert_eq!(provider.requests().len(), 1);
        let lines = new_lines(&log, before);
        assert_eq!(
            lines.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(),
            ["usage_recorded"]
        );
        assert_eq!(lines[0].turn_id, None);
        assert_eq!(lines[0].action_id, None);
    }
}

#[test]
fn budget_boundary_stops_notes_without_a_request_or_usage() {
    for limit in [0.49, 0.50, 0.51] {
        let provider = Arc::new(ScriptedProvider::new([Scripted::text("1 | rule")]));
        let (mut looped, log, _home, _clock) = started(provider.clone());
        looped.budget = Some(limit);
        let spent = crate::usage::recorded(
            fakes::reply("").usage(),
            Some(0.50),
            "fake/model",
            None,
            false,
        );
        looped.ledger.record(&spent.line);
        let before = log.count();
        let (ack, rx) = capture();
        looped.take_notes(ask(None), ack).unwrap();
        let Ok(Some(CommandResult::ProposeNotes {
            proposed, error, ..
        })) = received(&rx)
        else {
            panic!("missing result");
        };
        if limit > 0.50 {
            assert_eq!(proposed.len(), 1);
            assert!(error.is_none());
            assert_eq!(provider.requests().len(), 1);
            assert_eq!(new_lines(&log, before).len(), 1);
        } else {
            assert!(proposed.is_empty());
            let error = error.unwrap();
            assert_eq!(error.code, ErrorCode::BudgetExceeded);
            assert_eq!(
                error.message,
                format!("The session reached its spending budget of ${limit:.2} (budget.usd).")
            );
            assert!(provider.requests().is_empty());
            assert!(new_lines(&log, before).is_empty());
        }
    }
}

fn read_only_ask() -> NotesAsk {
    NotesAsk {
        args: ProposeNotesArgs {
            topic: Topic::ReadOnlyCommands,
            answer: None,
            earlier: Vec::new(),
        },
        evidence: Vec::new(),
    }
}

fn read_only_tally(prefix: &str, subject: &str) -> Evidence {
    Evidence::Allows(AllowsTally::new(prefix.to_owned(), subject.to_owned(), 5, 5).unwrap())
}

#[test]
fn read_only_notes_ignore_a_spent_budget() {
    let observed = Arc::new(ScriptedProvider::new([Scripted::text("unused")]));
    let (mut looped, log, _home, _clock) = started(observed.clone());
    looped.budget = Some(0.0);
    let spent = crate::usage::recorded(
        fakes::reply("").usage(),
        Some(0.50),
        "fake/model",
        None,
        false,
    );
    looped.ledger.record(&spent.line);
    let before = log.count();
    let mut ask = read_only_ask();
    ask.evidence
        .push(read_only_tally("acme-cli", "acme-cli query --json"));
    let (ack, rx) = capture();
    looped.take_notes(ask, ack).unwrap();
    let Ok(Some(CommandResult::ProposeNotes {
        proposed, error, ..
    })) = received(&rx)
    else {
        panic!("missing result");
    };
    assert!(error.is_none());
    assert_eq!(proposed.len(), 1);
    assert_eq!(
        proposed[0].text(),
        "shell.read_only.\"acme-cli query\".flags = [\"--json\"]"
    );
    assert_eq!(proposed[0].cites(), [0]);
    assert!(observed.requests().is_empty());
    assert!(new_lines(&log, before).is_empty());
}

#[test]
fn read_only_notes_withhold_a_prefix_that_breaks_mid_word() {
    // "acme" does not end at a word boundary of the subject, so the
    // remainder falls back to the whole subject: its second word withholds
    // the group instead of rendering a wrong entry.
    let (mut looped, _log, _home, _clock) = started(Arc::new(ScriptedProvider::new([])));
    let mut ask = read_only_ask();
    ask.evidence
        .push(read_only_tally("acme", "acme-cli query --json"));
    let (ack, rx) = capture();
    looped.take_notes(ask, ack).unwrap();
    let Ok(Some(CommandResult::ProposeNotes {
        proposed, evidence, ..
    })) = received(&rx)
    else {
        panic!("missing result");
    };
    assert!(proposed.is_empty());
    assert_eq!(evidence.len(), 1);
    let Evidence::Allows(tally) = &evidence[0] else {
        panic!("an allow tally is kept");
    };
    assert_eq!(
        tally.text(),
        "words after the command: an entry would cover other subcommands"
    );
}

#[test]
fn drains_refuse_notes_while_busy_closing_settling_or_rewound() {
    for state in [
        "running",
        "waiting",
        "idle_closed",
        "running_closed",
        "settling",
        "rewound",
    ] {
        let provider = Arc::new(ScriptedProvider::new([]));
        let (mut looped, log, _home, _clock) = started(provider.clone());
        let before = log.count();
        let (ack, rx) = capture();
        let delivery = Delivery::ProposeNotes(ask(None), ack);
        let turn = TurnId("t_test".into());
        match state {
            "running" => looped.admit_running(delivery, &turn).unwrap(),
            "waiting" => assert!(matches!(
                looped
                    .take_while_waiting(&contract::RequestId("r_test".into()), delivery, &turn)
                    .unwrap(),
                Waited::Again
            )),
            "idle_closed" => {
                let ack = Ack(Box::new(|_| {}));
                looped.take_close(ack);
                looped
                    .admit_idle(delivery, &mut TurnInput::of(Vec::new()))
                    .unwrap();
            }
            "running_closed" => {
                let ack = Ack(Box::new(|_| {}));
                looped.take_close(ack);
                looped.admit_running(delivery, &turn).unwrap();
            }
            "settling" => {
                looped.cancel.shutdown(143);
                looped.deferred.push_back(delivery);
                looped.settled(Ok(())).unwrap();
            }
            "rewound" => {
                looped.rewound = true;
                looped
                    .admit_idle(delivery, &mut TurnInput::of(Vec::new()))
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let error = received(&rx).unwrap_err();
        assert_eq!(
            error.code,
            if matches!(state, "running" | "waiting") {
                ErrorCode::Busy
            } else {
                ErrorCode::Closing
            },
            "{state}"
        );
        if state == "rewound" {
            assert_eq!(error.message, crate::rewind::command::REWOUND_CLOSING);
        }
        assert!(provider.requests().is_empty());
        assert!(new_lines(&log, before).is_empty());
    }
}

#[test]
fn held_notes_are_skipped_when_seeking_an_approval_answer() {
    let (mut looped, _log, _home, _clock) = started(Arc::new(ScriptedProvider::new([])));
    let (ack, _rx) = capture();
    looped
        .deferred
        .push_back(Delivery::ProposeNotes(ask(None), ack));
    assert!(
        looped
            .take_held_answer(&contract::RequestId("r_test".into()))
            .is_none()
    );
    assert!(matches!(
        looped.deferred.front(),
        Some(Delivery::ProposeNotes(..))
    ));
}

#[test]
fn a_failed_append_returns_before_calling_the_ack() {
    use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    if std::env::var_os("FIBER_NOTES_LIMIT_CHILD").is_none() {
        let exe = std::env::current_exe().unwrap();
        // Ignored signals survive exec, so no unsafe signal handler is
        // needed in Rust. The size limit applies only in this child.
        let child = Command::new("/bin/sh")
            .args(["-s", "--"])
            .arg(exe)
            .args([
                "--exact",
                "notes::tests::a_failed_append_returns_before_calling_the_ack",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("FIBER_NOTES_LIMIT_CHILD", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .unwrap();
        let output = fakes::finished(
            "notes append failure child",
            child,
            b"trap '' XFSZ\nexec \"$@\"\n",
            Deadline::after(DEADLINE),
        );
        assert!(
            output.status.success(),
            "child failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        return;
    }
    let (mut looped, log, _home, _clock) =
        started(Arc::new(ScriptedProvider::new([Scripted::text(
            "1 | rule",
        )])));
    let path = log.dir().join("events.jsonl");
    let before = std::fs::read(&path).unwrap();
    let saved = getrlimit(Resource::Fsize);
    setrlimit(
        Resource::Fsize,
        Rlimit {
            current: Some(u64::try_from(before.len()).unwrap()),
            maximum: saved.maximum,
        },
    )
    .unwrap();
    let (ack, rx) = capture();
    let result = looped.take_notes(ask(None), ack);
    setrlimit(Resource::Fsize, saved).unwrap();
    assert!(
        matches!(result, Err(crate::Error::Log(log::Error::Io { source, .. })) if source.raw_os_error() == Some(rustix::io::Errno::FBIG.raw_os_error())),
        "append must return EFBIG"
    );
    assert!(matches!(
        Deadline::after(DEADLINE).recv(&rx),
        Err(mpsc::RecvTimeoutError::Disconnected)
    ));
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

struct LookupProvider {
    inner: ScriptedProvider,
}

impl contract::provider::CostLookup for LookupProvider {
    fn cost(&self, generation_id: &contract::GenerationId) -> Option<f64> {
        assert_eq!(generation_id.0, "gen_1");
        Some(0.25)
    }
}

impl Provider for LookupProvider {
    fn call(
        &self,
        request: &contract::provider::ModelRequest,
    ) -> Box<dyn contract::provider::ModelCall> {
        self.inner.call(request)
    }
    fn wire_tools(
        &self,
        tools: &[contract::provider::ToolDefinition],
    ) -> Vec<serde_json::Map<String, serde_json::Value>> {
        self.inner.wire_tools(tools)
    }
    fn cost_lookup(&self) -> Option<Arc<dyn contract::provider::CostLookup>> {
        Some(Arc::new(Self {
            inner: ScriptedProvider::new([]),
        }))
    }
}

#[test]
fn a_late_cost_replaces_the_notes_calls_usage_without_a_turn_or_action() {
    use contract::clock::Clock;
    let provider = Arc::new(LookupProvider {
        inner: ScriptedProvider::new([Scripted::text("1 | rule")]),
    });
    let (mut looped, log, _home, clock) = started(provider);
    let before = log.count();
    let due = clock.now() + crate::late_cost::LOOKUP_AFTER;
    let (ack, rx) = capture();
    looped
        .admit_idle(
            Delivery::ProposeNotes(ask(None), ack),
            &mut TurnInput::of(Vec::new()),
        )
        .unwrap();
    assert!(matches!(
        received(&rx),
        Ok(Some(CommandResult::ProposeNotes { error: None, .. }))
    ));
    let first = new_lines(&log, before);
    assert_eq!(
        first
            .iter()
            .map(|line| line.kind.as_str())
            .collect::<Vec<_>>(),
        ["usage_recorded"]
    );
    let deadline = Deadline::after(DEADLINE);
    let mark =
        clock.advance_when_parked(Some(due), None, crate::late_cost::LOOKUP_AFTER, &deadline);
    assert!(
        clock.await_parked_since(&mark, None, deadline),
        "lookup has pushed its settled record before parking with an empty queue"
    );
    looped.write_settled().unwrap();
    let lines = new_lines(&log, before);
    assert_eq!(
        lines
            .iter()
            .map(|line| line.kind.as_str())
            .collect::<Vec<_>>(),
        ["usage_recorded", "usage_recorded"]
    );
    assert_eq!(
        lines[1].payload["generation_id"],
        lines[0].payload["generation_id"]
    );
    assert_eq!(lines[1].payload["cost"], 0.25);
    for line in &lines {
        assert_eq!(line.turn_id, None);
        assert_eq!(line.action_id, None);
        assert!(line.payload.get("reviewer").is_none());
    }
}

#[test]
fn a_notes_proposal_after_a_request_leaves_the_cache_expiry_put() {
    use contract::shapes::Tokens;
    use contract::{Envelope, GenerationId};
    use serde_json::{Value, json};
    use std::collections::BTreeMap;

    fn envelope(kind: &str, ts: u64, action: Option<&str>, payload: Value) -> Envelope {
        let mut line = json!({
            "kind": kind, "session_id": "s_test", "ts": ts,
            "schema_version": 1, "payload": payload,
        });
        // A folded line is durable if and only if it carries `seq`.
        line["seq"] = json!(ts);
        if let Some(action) = action {
            line["action_id"] = json!(action);
        }
        serde_json::from_value(line).unwrap()
    }

    let provider = Arc::new(ScriptedProvider::new([Scripted::text("1 | rule")]));
    let (mut looped, log, _home, _clock) = started(provider);
    let before = log.count();
    let (ack, rx) = capture();
    looped
        .admit_idle(
            Delivery::ProposeNotes(ask(None), ack),
            &mut TurnInput::of(Vec::new()),
        )
        .unwrap();
    assert!(matches!(
        received(&rx),
        Ok(Some(CommandResult::ProposeNotes { error: None, .. }))
    ));
    let notes = new_lines(&log, before);
    assert_eq!(
        notes
            .iter()
            .map(|line| line.kind.as_str())
            .collect::<Vec<_>>(),
        ["usage_recorded"]
    );
    // The notes call's line carries no action and no marker.
    assert_eq!(notes[0].action_id, None);
    assert!(notes[0].payload.get("warming").is_none());

    let mut fold = crate::status::Fold::new(
        "-w".to_owned(),
        "/w".to_owned(),
        "fake/model".to_owned(),
        None,
        Box::new(Vec::new),
        Box::new(|| None),
        Box::new(|| 0),
    );
    let preamble = PreambleBuilt {
        reason: PreambleReason::Start,
        model: "fake/model".into(),
        context_window: 1000,
        trigger_at: None,
        budget: None,
        thinking: None,
        tool_choice: "auto".into(),
        cache_lifetime: CacheLifetime::OneHour,
        credential: None,
        system_prompt: "s".into(),
        tools: Vec::new(),
        replaced: Vec::new(),
    };
    fold.observe(&envelope(
        "preamble_built",
        1000,
        None,
        serde_json::to_value(preamble).unwrap(),
    ));
    let request = UsageRecorded {
        generation_id: GenerationId("g_1".into()),
        model: "fake/model".into(),
        tokens: Tokens {
            input: 10,
            cache_read: 0,
            cache_write: BTreeMap::new(),
            output: 3,
        },
        input_bytes: 1,
        input_media: None,
        web_searches: None,
        cost: None,
        subscription: None,
        extension: None,
        origin_session_id: None,
        reviewer: None,
        warming: None,
    };
    fold.observe(&envelope(
        "usage_recorded",
        2000,
        Some("a1"),
        serde_json::to_value(request).unwrap(),
    ));
    let pinned = fold.status().cache_expires;
    assert_eq!(pinned, Some(2000 + 3_600_000));
    fold.observe(&notes[0]);
    assert_eq!(fold.status().cache_expires, pinned);
}

#[test]
fn notes_number_each_kind_in_evidence_order_and_answer_last() {
    let provider = Arc::new(ScriptedProvider::new([Scripted::text("3 | rule")]));
    let (mut looped, _log, _home, _clock) = started(provider.clone());
    let mut asked = ask(Some("answer"));
    asked.evidence.push(Evidence::Session(
        SessionTally::new("p".into(), 1, 1).unwrap(),
    ));
    let (ack, rx) = capture();
    looped.take_notes(asked, ack).unwrap();
    let Ok(Some(CommandResult::ProposeNotes {
        evidence,
        proposed,
        error,
    })) = received(&rx)
    else {
        panic!("missing result");
    };
    assert_eq!(
        evidence.iter().map(Evidence::kind).collect::<Vec<_>>(),
        [
            contract::notes::EvidenceKind::Files,
            contract::notes::EvidenceKind::Session,
            contract::notes::EvidenceKind::Answer
        ]
    );
    assert_eq!(proposed[0].cites(), [2]);
    assert!(error.is_none());
    assert_eq!(&provider.requests()[0].conversation[..], vec![Input::User { text: "Evidence:\n1 [files] | remote evidence\n2 [session] | 1 escalation answered allow for `p`\n3 [answer] | answer\n\nEarlier lines:\n- earlier line\n".into(), images: Vec::new() }]);
}

#[test]
fn observed_notes_use_the_session_factory_and_each_topics_instructions() {
    for topic in Topic::ALL {
        if topic == Topic::ReadOnlyCommands {
            // The model-free topic sends no provider request and writes
            // no usage: its branch is pinned beside the inbox test.
            let observed = Arc::new(ScriptedProvider::new([]));
            let (mut looped, log, _home, _clock) = started(observed.clone());
            let before = log.count();
            let mut ask = ask(None);
            ask.args.topic = topic;
            ask.args.earlier = Vec::new();
            let (ack, rx) = capture();
            looped
                .admit_idle(
                    Delivery::ProposeNotes(ask, ack),
                    &mut TurnInput::of(Vec::new()),
                )
                .unwrap();
            let Ok(Some(CommandResult::ProposeNotes {
                proposed, error, ..
            })) = received(&rx)
            else {
                panic!("missing result");
            };
            assert!(proposed.is_empty());
            assert!(error.is_none());
            assert!(observed.requests().is_empty());
            assert!(new_lines(&log, before).is_empty());
            continue;
        }
        let unused = Arc::new(ScriptedProvider::new([]));
        let observed = Arc::new(ScriptedProvider::new([Scripted::text("1 | rule")]));
        let (mut looped, _log, _home, _clock) = started(unused.clone());
        let factory = observed.clone();
        looped = looped.observed(Some(Arc::new(move |request, attempt| {
            assert_eq!(attempt.get(), 1);
            factory.call(request)
        })));
        let mut ask = ask(None);
        ask.args.topic = topic;
        let (ack, rx) = capture();
        looped
            .admit_idle(
                Delivery::ProposeNotes(ask, ack),
                &mut TurnInput::of(Vec::new()),
            )
            .unwrap();
        assert!(matches!(
            received(&rx),
            Ok(Some(CommandResult::ProposeNotes { error: None, .. }))
        ));
        assert!(unused.requests().is_empty());
        let requests = observed.requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].system_prompt.contains(topic.covers()));
    }
}

struct NoRepositoryCode;
impl contract::repository::RepositoryCode for NoRepositoryCode {
    fn unapproved(
        &self,
    ) -> Result<Vec<contract::repository::Unapproved>, contract::shapes::Failure> {
        Ok(Vec::new())
    }
    fn file(&self, _: &str) -> Result<String, contract::shapes::Failure> {
        panic!("no file is requested")
    }
    fn decide(
        &self,
        _: &contract::events::OfferedItem,
        _: contract::events::OfferDecision,
    ) -> Result<contract::repository::Decided, contract::shapes::Failure> {
        panic!("no decision is sent")
    }
}

#[test]
fn an_offer_wait_defers_notes_without_sending_a_request() {
    let provider = Arc::new(ScriptedProvider::new([]));
    let (mut looped, log, _home, _clock) = started(provider.clone());
    let (tx, rx) = mpsc::channel();
    looped.inbox = crate::inbox::queue::Queue::new(rx);
    let offer = contract::events::RepositoryCodeOffered {
        request_id: contract::RequestId("r_offer".into()),
        items: Vec::new(),
    };
    let mut folded = crate::offer::Folded::default();
    folded.fold(&Event::RepositoryCodeOffered(offer));
    looped.repository = crate::offer::State::resumed(folded);
    looped = looped.repository_code(Arc::new(NoRepositoryCode));
    log.append(
        &Event::Clients(contract::events::Clients { count: 1 }),
        None,
        None,
    )
    .unwrap();
    looped.answerable = true;
    let before = log.count();
    let (ack, answer) = capture();
    tx.send(Delivery::ProposeNotes(ask(None), ack)).unwrap();
    tx.send(Delivery::Close(Ack(Box::new(|_| {})))).unwrap();
    let (done, finished) = mpsc::channel();
    std::thread::spawn(move || {
        let result = looped.offer(None);
        done.send((looped, result)).unwrap();
    });
    let (looped, result) = Deadline::after(DEADLINE)
        .recv_or_fail(&finished, "offer takes close after deferring notes");
    assert!(result.unwrap());
    assert!(matches!(
        looped.deferred.front(),
        Some(Delivery::ProposeNotes(..))
    ));
    assert!(provider.requests().is_empty());
    assert!(matches!(answer.try_recv(), Err(mpsc::TryRecvError::Empty)));
    assert_eq!(
        new_lines(&log, before)
            .iter()
            .map(|line| line.kind.as_str())
            .collect::<Vec<_>>(),
        ["repository_code_offered"]
    );
}

fn evidence() -> Vec<Evidence> {
    vec![
        Evidence::Files {
            text: "remote".into(),
        },
        Evidence::Files { text: "CI".into() },
        Evidence::Session(SessionTally::new("p".into(), 1, 1).unwrap()),
        Evidence::Answer {
            text: "ours".into(),
        },
    ]
}

#[test]
fn proposals_keep_valid_citations_as_zero_based_indices() {
    let proposals = read_proposals(
        "2 | second\n1,2 | both\n3 | session\n4 | answer",
        &evidence(),
        &[],
    );
    assert_eq!(
        proposals
            .iter()
            .map(|p| (p.text(), p.cites().to_vec()))
            .collect::<Vec<_>>(),
        vec![
            ("second", vec![1]),
            ("both", vec![0, 1]),
            ("session", vec![2]),
            ("answer", vec![3])
        ]
    );
}

#[test]
fn proposals_drop_each_invalid_or_repeated_line() {
    for line in [
        "missing numbers",
        "| missing",
        "x | not a number",
        "0 | zero",
        "5 | past end",
        "1,3 | mixed",
        "1,5 | later citation past end",
        "1, | missing number",
        "1 |   ",
        "1 | earlier",
    ] {
        let reply = format!("{line}\n2 | valid\n2 | valid");
        let proposals = read_proposals(&reply, &evidence(), &["earlier".into()]);
        assert_eq!(proposals.len(), 1, "{line}");
        assert_eq!(proposals[0].text(), "valid", "{line}");
    }
    assert!(read_proposals("1 | no evidence\n0 | zero", &[], &[]).is_empty());
}

#[test]
fn proposals_strip_userinfo_before_deduplicating() {
    let proposals = read_proposals(
        "1 | https://u:p@h/\n2 | https://h/\n1 | https://u:p@old/",
        &evidence(),
        &["https://old/".into()],
    );
    assert_eq!(proposals.len(), 1);
    assert_eq!(proposals[0].text(), "https://h/");
}
