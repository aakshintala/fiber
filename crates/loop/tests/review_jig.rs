//! The `review` jig (`docs/testing.md`, "Jigs"): the built `review` binary
//! prints what the loop's reviewer would be sent for a session directory.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code; a failure is the test"
)]

mod support;

use contract::clock::Clock as _;
use contract::events::{
    Anchor, ArgumentRepair, AskStep, CallStatus, DecidedBy, Decision, Event, HandoffCompleted,
    InputItem, InstructionFile, InstructionReason, InstructionSent, Outcome, PermissionRequested,
    PermissionResolved, RanBy, Repair, RepairFix, ReviewedCall, ReviewerRef, RuleScope,
    SessionStarted, StandingRule, SteeringApplied, TextCompleted, ToolCallCompleted,
    ToolCallRequested, ToolCallStarted, TurnOutcome, TurnStarted, Variables, VariablesSource,
};
use contract::provider::{Input, ModelRequest};
use contract::shapes::{ContentPart, DeclaredEffects, Effect, Origin, Point, Sender};
use contract::tool::Tool;
use contract::{ActionId, CommandId, RequestId, Seq, SessionId, TurnId};
use fakes::{Deadline, START_BOUND, Scripted, Watchdog, kill_group};
use serde_json::{Value, json};
use std::io::{Cursor, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;
use support::review::{RequestKind, ReviewArgs, parse_args, replay};
use support::{Session, calls_reply, delivery, handoff, paris, shell};

/// How long the jig exchange may take once the binary has started.
/// `.config/nextest.toml` kills a slow test after a 30s period times 4 (120s
/// total); the test's start bound and its exchange bound each count alone
/// against that limit, and a pass-then-hang path adds the start bound, the
/// exchange deadline and the stand-down, a sum that stays under that limit:
/// `docs/testing.md`, "Waits and timeouts", needs nextest's timeout to be at
/// least twice the test's longest single deadline plus its normal run time, so
/// a hang reports which wait expired.
const JIG_DEADLINE: Duration = Duration::from_secs(10);

/// One prompt turn, asserting it completes.
fn prompt_turn(session: &mut Session, text: &str) {
    session.inbox.send(delivery(text)).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    session.lines();
}

/// One person handoff turn, asserting it completes.
fn handoff_turn(session: &mut Session, id: &str) {
    session.inbox.send(handoff(id, None)).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    session.lines();
}

/// A session directory holding a reviewed call, a handoff, and another
/// reviewed call: the native loop's own log, replayed below.
fn native_session() -> (Session, Arc<fakes::ScriptedProvider>, String) {
    let notes = "Escalate pushes to the person.".to_owned();
    let tool = shell(None, None);
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("shell", paris())]),
            Scripted::text("Done."),
            Scripted::text("Hi there."),
            Scripted::text("The note."),
            calls_reply("", &[("shell", paris())]),
            Scripted::text("Done."),
        ],
        None,
        vec![tool as Arc<dyn Tool>],
    )
    .reviewer_notes(notes.clone());
    let reviewer = session.reviewer(vec![
        Scripted::text("check"),
        Scripted::text("allow: fine"),
        Scripted::text("1"),
        Scripted::text("check"),
        Scripted::text("allow: fine"),
    ]);
    prompt_turn(&mut session, "never push to main");
    prompt_turn(&mut session, "say hi");
    handoff_turn(&mut session, "c_h");
    prompt_turn(&mut session, "now run it");
    (session, reviewer, notes)
}

/// The native reviewer's request at `at`, with the session-bound fields
/// blanked: the cache key names the live session, never the reviewed input.
fn native_request(requests: &[ModelRequest], at: usize) -> ModelRequest {
    let mut request = requests.get(at).unwrap().clone();
    request.cache_key = String::new();
    request
}

/// The replay's request at `at`, with the session-bound fields blanked.
fn replayed_request(requests: &[(RequestKind, ModelRequest)], at: usize) -> ModelRequest {
    let mut request = requests.get(at).unwrap().1.clone();
    request.cache_key = String::new();
    request
}

/// The user texts of a captured request's conversation.
fn texts(request: &ModelRequest) -> Vec<String> {
    request
        .conversation
        .iter()
        .map(|input| match input {
            Input::User { text, .. } => text.clone(),
            Input::Assistant { .. }
            | Input::Reasoning { .. }
            | Input::ToolCall { .. }
            | Input::ToolResult { .. }
            | Input::ToolSearchResult { .. } => {
                panic!("a reviewer request holds user inputs, got {input:?}")
            }
        })
        .collect()
}

#[test]
fn replay_matches_the_native_reviewer_on_a_call_and_a_handoff() {
    let (session, reviewer, notes) = native_session();
    let native = reviewer.requests();
    assert_eq!(native.len(), 5);
    let kinds: Vec<RequestKind> = vec![
        RequestKind::Stage,
        RequestKind::Stage,
        RequestKind::Selection,
        RequestKind::Stage,
        RequestKind::Stage,
    ];
    let home = fakes::TempDir::new("fiber-review-oracle");
    let path = home.path().join("notes.md");
    std::fs::write(&path, &notes).unwrap();
    let args = ReviewArgs {
        dir: session.dir.clone(),
        notes: Some(path),
        keep_all: false,
    };
    let outcome = replay(&args, Cursor::new("1\n"), Vec::new()).unwrap();
    assert_eq!(
        outcome
            .requests
            .iter()
            .map(|(kind, _)| *kind)
            .collect::<Vec<RequestKind>>(),
        kinds,
    );
    for at in 0..5 {
        assert_eq!(
            replayed_request(&outcome.requests, at),
            native_request(&native, at),
            "request {at} matches the native loop's builder",
        );
    }

    // Positive controls: the replayed first stage carries the person, the
    // call, its declared effects with the source workspace, and the stage.
    let first = replayed_request(&outcome.requests, 0);
    assert!(first.system_prompt.contains("You review one tool call"));
    let first_texts = texts(&first);
    assert_eq!(first_texts.len(), 4);
    assert!(
        first_texts[0].contains("never push to main"),
        "{first_texts:?}"
    );
    assert!(first_texts[1].starts_with("Tool call: "), "{first_texts:?}");
    assert!(first_texts[1].contains("\"shell\""), "{first_texts:?}");
    assert!(
        first_texts[2].starts_with("Call under review: the last tool call above, shell."),
        "{first_texts:?}"
    );
    assert!(
        first_texts[2].contains("Workspace root: "),
        "{first_texts:?}"
    );
    assert!(
        first_texts[3].starts_with("## first-pass\n"),
        "{first_texts:?}"
    );
    let second = replayed_request(&outcome.requests, 1);
    let second_texts = texts(&second);
    assert!(
        second_texts[3].starts_with("## second-pass\n"),
        "{second_texts:?}"
    );

    // The selection carries both person messages for the answer to choose
    // from, and the second call's stages see only the kept message.
    let selection = replayed_request(&outcome.requests, 2);
    let selection_texts = texts(&selection);
    assert!(
        selection_texts
            .iter()
            .any(|text| text.contains("never push to main"))
    );
    assert!(selection_texts.iter().any(|text| text.contains("say hi")));
    let after = replayed_request(&outcome.requests, 3);
    let after_texts = texts(&after);
    assert!(
        after_texts
            .iter()
            .any(|text| text.contains("never push to main"))
    );
    assert!(after_texts.iter().any(|text| text.contains("now run it")));
    assert!(!after_texts.iter().any(|text| text.contains("say hi")));
}

#[test]
fn replay_carries_the_persons_notes_into_every_system_prompt() {
    let (session, _reviewer, notes) = native_session();
    let home = fakes::TempDir::new("fiber-review-notes");
    let path = home.path().join("notes.md");
    std::fs::write(&path, &notes).unwrap();
    let args = ReviewArgs {
        dir: session.dir.clone(),
        notes: Some(path),
        keep_all: false,
    };
    let outcome = replay(&args, Cursor::new("1\n"), Vec::new()).unwrap();
    assert_eq!(outcome.requests.len(), 5);
    for (_, request) in &outcome.requests {
        assert!(
            request.system_prompt.contains(&notes),
            "every request carries the notes: {:?}",
            request.system_prompt,
        );
    }
}

#[test]
fn parse_args_reads_flags_and_rejects_bad_argv() {
    let args = parse_args(
        ["--notes", "n.md", "--keep-all", "s"]
            .iter()
            .map(|arg| (*arg).to_owned()),
    )
    .unwrap();
    assert_eq!(args.dir, std::path::PathBuf::from("s"));
    assert_eq!(args.notes, Some(std::path::PathBuf::from("n.md")));
    assert!(args.keep_all);

    let args = parse_args(["s"].iter().map(|arg| (*arg).to_owned())).unwrap();
    assert_eq!(args.dir, std::path::PathBuf::from("s"));
    assert_eq!(args.notes, None);
    assert!(!args.keep_all);

    for argv in [
        Vec::<String>::new(),
        vec!["--notes".to_owned()],
        vec!["--bogus".to_owned(), "s".to_owned()],
        vec!["a".to_owned(), "b".to_owned()],
    ] {
        assert!(parse_args(argv.into_iter()).is_err());
    }
}

#[test]
fn missing_notes_file_is_an_input_error_naming_the_path() {
    let (session, _reviewer, _notes) = native_session();
    let args = ReviewArgs {
        dir: session.dir.clone(),
        notes: Some(std::path::PathBuf::from("/nonexistent/review-notes.md")),
        keep_all: false,
    };
    let problem = replay(&args, Cursor::new(""), Vec::new()).unwrap_err();
    assert!(
        problem.contains("/nonexistent/review-notes.md"),
        "{problem}"
    );
}

/// The example `cargo test` built beside this test binary
/// (`target/<profile>/examples/review`), so the test starts no cargo and
/// waits on no build lock. Warming before returning means every exec site
/// holds only a warmed path.
fn example_path() -> fakes::WarmedJig {
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    path.pop();
    fakes::WarmedJig::warm(path.join("examples").join("review"), &["--bogus"], 2)
}

/// Runs the built `review` binary on `dir` with `answers` as its stdin
/// and `args` after the directory. `keep_open` leaves stdin open, so a
/// second read would block instead of seeing EOF. A [`START_BOUND`]
/// warm-up absorbs the first-exec stall on every call, then one
/// [`JIG_DEADLINE`] covers the exchange and the exit; the watchdog reaps
/// the process group on every path, including failure.
fn run_jig(dir: &Path, answers: &str, keep_open: bool, args: &[&str]) -> Output {
    let warmed = example_path();
    let mut child = Command::new(warmed.path())
        .arg(dir)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    if !answers.is_empty() {
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(answers.as_bytes())
            .unwrap();
    }
    if !keep_open {
        child.stdin.take();
    }
    wait_under(child, JIG_DEADLINE)
}

/// Waits at most `bound` for `child` to exit, reaping it on every path:
/// on expiry the child's process group is killed first, then the test
/// fails naming what it waited for.
fn wait_under(child: Child, bound: Duration) -> Output {
    let group = child.id();
    let watchdog = Watchdog::group(group);
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait_with_output()).unwrap());
    let output = match Deadline::after(bound).recv(&finished) {
        Ok(output) => output.unwrap(),
        Err(_) => {
            kill_group(group, "KILL").unwrap_or(false);
            let output = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
                .recv(&finished)
                .expect("the review jig died after KILL");
            panic!(
                "the review jig did not finish in time: {}",
                String::from_utf8_lossy(&output.unwrap().stdout)
            );
        }
    };
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
    output
}

/// The jig's stdout, one parsed JSON value per line.
fn stdout_lines(output: &Output) -> Vec<Value> {
    assert!(
        output.status.success(),
        "the jig exited {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "the jig is JSONL on stdout only: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// A session directory the tests write event by event.
struct Fixture {
    /// The temporary sessions home, kept alive for the test.
    _home: fakes::TempDir,
    /// The log, kept so its lock stays held while the jig reads.
    _log: log::Log,
    /// The session directory the jig replays.
    dir: PathBuf,
}

impl Fixture {
    /// A fixture whose session is `id` with `workspace`.
    fn new(id: &str, workspace: &str) -> Self {
        let home = fakes::TempDir::new("fiber-review-jig");
        let log = log::Log::create(
            home.path(),
            SessionId(id.to_owned()),
            fakes::clock::FakeClock::new(),
        )
        .unwrap();
        let dir = log.dir().to_path_buf();
        log.append(
            &Event::SessionStarted(SessionStarted {
                workspace: workspace.to_owned(),
                variables: Variables::of(
                    Vec::<(std::ffi::OsString, std::ffi::OsString)>::new(),
                    VariablesSource::Inherited,
                ),
                parent: None,
                forked_from: None,
                rewind: None,
                worktree: None,
                supervised: false,
            }),
            None,
            None,
        )
        .unwrap();
        Self {
            _home: home,
            _log: log,
            dir,
        }
    }

    /// Appends `event` about `turn` and `action`.
    fn append(&self, event: &Event, turn: Option<&str>, action: Option<&str>) {
        self._log
            .append(
                event,
                turn.map(|turn| TurnId(turn.to_owned())),
                action.map(|action| ActionId(action.to_owned())),
            )
            .unwrap();
    }

    /// The log's bytes now, for the unchanged-source assertions.
    fn bytes(&self) -> Vec<u8> {
        std::fs::read(self.dir.join("events.jsonl")).unwrap()
    }
}

/// A driver's message saying `text`.
fn driver_message(text: &str) -> InputItem {
    InputItem::Message {
        content: vec![ContentPart::Text { text: text.into() }],
        sender: Sender {
            origin: Origin::Driver,
            command_id: Some(CommandId("c_driver".into())),
        },
        changed_by: None,
    }
}

/// The turn-starting event of a driver turn saying each of `texts`.
fn user_turn(texts: &[&str]) -> Event {
    Event::TurnStarted(TurnStarted {
        input: texts.iter().map(|text| driver_message(text)).collect(),
    })
}

/// A driver's steering message saying `text`.
fn driver_steering(text: &str) -> Event {
    Event::SteeringApplied(SteeringApplied {
        content: vec![ContentPart::Text { text: text.into() }],
        sender: Sender {
            origin: Origin::Driver,
            command_id: Some(CommandId("c_steer".into())),
        },
        changed_by: None,
    })
}

/// A message from another session saying `text`: never reviewer input.
fn session_steering(text: &str) -> Event {
    Event::SteeringApplied(SteeringApplied {
        content: vec![ContentPart::Text { text: text.into() }],
        sender: Sender {
            origin: Origin::Session {
                from_session_id: SessionId("s_peer".into()),
            },
            command_id: Some(CommandId("c_peer".into())),
        },
        changed_by: None,
    })
}

/// The agent's prose saying `text`: never reviewer input.
fn prose(text: &str) -> Event {
    Event::TextCompleted(TextCompleted {
        text: text.into(),
        provider_item: None,
    })
}

/// A tool result saying `text`: never reviewer input.
fn tool_result(text: &str) -> Event {
    Event::ToolCallCompleted(ToolCallCompleted {
        status: CallStatus::Completed,
        reason: None,
        error: None,
        process: None,
        content: vec![ContentPart::Text { text: text.into() }],
        details: None,
        artifact: None,
        kept: None,
        changes: None,
        control: None,
        changed_by: None,
        protected_paths: None,
        provider_item: None,
        hosts_surfaced: Vec::new(),
        loaded: None,
    })
}

/// A project instruction file: never reviewer input.
fn instruction(text: &str) -> Event {
    Event::InstructionFile(InstructionFile {
        path: "/synthetic/workspace/AGENTS.md".into(),
        reason: InstructionReason::Changed,
        extension: None,
        content: Some(text.into()),
        sent: InstructionSent::Full,
    })
}

/// The request event of an outer call to `name` with `arguments`.
fn requested(name: &str, arguments: Value) -> Event {
    Event::ToolCallRequested(ToolCallRequested {
        name: name.into(),
        arguments,
        provider_id: None,
        repair: None,
        ran_by: None,
        provider_item: None,
    })
}

/// The start event of a call declaring `effects`, reversible or not.
fn started(effects: Vec<Effect>, reversible: bool) -> Event {
    Event::ToolCallStarted(ToolCallStarted {
        always_reviewed: false,
        declared: DeclaredEffects {
            effects,
            reversible,
            paths: None,
            confined: false,
        },
        arguments: None,
        changed_by: None,
    })
}

/// A review-step approval carrying `request_id`: reviewer attribution.
fn review_step(request_id: &str) -> Event {
    Event::PermissionRequested(PermissionRequested {
        always_reviewed: false,
        request_id: RequestId(request_id.into()),
        declared: DeclaredEffects {
            effects: vec![Effect::Executes],
            reversible: false,
            paths: None,
            confined: false,
        },
        step: AskStep::Review {
            escalation: None,
            rule: None,
        },
    })
}

/// A reviewer denial with no `permission_requested`: a blocked call
/// below the escalation limits never started, and its decision carries
/// the call's declared effects.
fn reviewer_denied(reason: &str) -> Event {
    Event::PermissionResolved(PermissionResolved {
        call: Some(ReviewedCall {
            declared: DeclaredEffects {
                effects: vec![Effect::Executes],
                reversible: false,
                paths: None,
                confined: false,
            },
            always_reviewed: false,
            arguments: None,
            changed_by: None,
        }),
        request_id: None,
        decision: Decision::Deny,
        decided_by: DecidedBy::Reviewer,
        reason: Some(reason.into()),
        feedback: None,
        grant: None,
        rule: None,
        reviewer: Some(ReviewerRef {
            model: "fake/reviewer".into(),
            stage: 2,
        }),
        hosts: None,
    })
}

/// A reviewer denial as a log written before the declared-effects change
/// holds it: no keys, so the call replays as `unreplayable`.
fn legacy_reviewer_denied(reason: &str) -> Event {
    Event::PermissionResolved(PermissionResolved {
        call: None,
        request_id: None,
        decision: Decision::Deny,
        decided_by: DecidedBy::Reviewer,
        reason: Some(reason.into()),
        feedback: None,
        grant: None,
        rule: None,
        reviewer: Some(ReviewerRef {
            model: "fake/reviewer".into(),
            stage: 2,
        }),
        hosts: None,
    })
}

/// A headless denial with no reviewer and no start, carrying the call's
/// declared effects.
fn headless_denied(reason: &str) -> Event {
    Event::PermissionResolved(PermissionResolved {
        call: Some(ReviewedCall {
            declared: DeclaredEffects {
                effects: vec![Effect::Executes],
                reversible: false,
                paths: None,
                confined: false,
            },
            always_reviewed: false,
            arguments: None,
            changed_by: None,
        }),
        request_id: None,
        decision: Decision::Deny,
        decided_by: DecidedBy::NoReviewer,
        reason: Some(reason.into()),
        feedback: None,
        grant: None,
        rule: None,
        reviewer: None,
        hosts: None,
    })
}

/// A headless denial as a log written before the declared-effects change
/// holds it: no keys, so the call replays as `unreplayable`.
fn legacy_headless_denied(reason: &str) -> Event {
    Event::PermissionResolved(PermissionResolved {
        call: None,
        request_id: None,
        decision: Decision::Deny,
        decided_by: DecidedBy::NoReviewer,
        reason: Some(reason.into()),
        feedback: None,
        grant: None,
        rule: None,
        reviewer: None,
        hosts: None,
    })
}

/// A person's allow: an explicit non-review decision.
fn person_allowed() -> Event {
    Event::PermissionResolved(PermissionResolved {
        call: None,
        request_id: Some(RequestId("r_person".into())),
        decision: Decision::Allow,
        decided_by: DecidedBy::Person,
        reason: None,
        feedback: None,
        grant: None,
        rule: None,
        reviewer: None,
        hosts: None,
    })
}

/// A standing-rule denial with no start: an explicit non-review
/// decision, never a review request of its own.
fn rule_denied(reason: &str) -> Event {
    Event::PermissionResolved(PermissionResolved {
        call: None,
        request_id: None,
        decision: Decision::Deny,
        decided_by: DecidedBy::StandingRule,
        reason: Some(reason.into()),
        feedback: None,
        grant: None,
        rule: None,
        reviewer: None,
        hosts: None,
    })
}

/// A handoff boundary with `outcome`.
fn handoff_boundary(outcome: Outcome) -> Event {
    Event::HandoffCompleted(HandoffCompleted {
        outcome,
        error: match outcome {
            Outcome::Completed | Outcome::Cancelled => None,
            Outcome::Failed => Some(contract::shapes::Failure {
                code: contract::ErrorCode::RateLimited,
                message: "slow down".into(),
                retry_after_ms: None,
                provider: None,
            }),
        },
        note: None,
        tokens_before: 120,
        instructions: None,
    })
}

/// The ticket's main fixture: a person message pair, two replayable
/// outer calls separated by a successful handoff, a session message, a
/// reviewer-blocked call and a headless-denied call.
fn main_fixture() -> Fixture {
    let fixture = Fixture::new("s_fixture", "/synthetic/workspace");
    fixture.append(
        &user_turn(&["never push to main", "use tabs"]),
        Some("t_1"),
        None,
    );
    fixture.append(
        &requested("shell", json!({"city": "Paris"})),
        Some("t_1"),
        Some("a_1"),
    );
    fixture.append(&review_step("r_1"), Some("t_1"), Some("a_1"));
    fixture.append(
        &started(vec![Effect::Executes], false),
        Some("t_1"),
        Some("a_1"),
    );
    fixture.append(&tool_result("tool result MARKER"), Some("t_1"), Some("a_1"));
    fixture.append(&prose("agent prose MARKER"), Some("t_1"), None);
    fixture.append(&instruction("instruction MARKER"), Some("t_1"), None);
    fixture.append(&session_steering("peer says hi MARKER"), Some("t_1"), None);
    fixture.append(&handoff_boundary(Outcome::Completed), Some("t_1"), None);
    fixture.append(&driver_steering("now run it"), Some("t_2"), None);
    fixture.append(&user_turn(&["also keep logs"]), Some("t_2"), None);
    fixture.append(
        &requested("shell", json!({"city": "Paris"})),
        Some("t_2"),
        Some("a_2"),
    );
    fixture.append(
        &started(vec![Effect::Executes], false),
        Some("t_2"),
        Some("a_2"),
    );
    fixture.append(
        &requested("shell", json!({"city": "Paris"})),
        Some("t_2"),
        Some("a_blocked"),
    );
    fixture.append(
        &reviewer_denied("that push rewrites shared history"),
        Some("t_2"),
        Some("a_blocked"),
    );
    fixture.append(
        &requested("shell", json!({"city": "Paris"})),
        Some("t_2"),
        Some("a_headless"),
    );
    fixture.append(
        &headless_denied("no person can answer"),
        Some("t_2"),
        Some("a_headless"),
    );
    fixture
}

/// One projected review line, from two captured stage requests.
fn projected_review(id: &str, first: &ModelRequest, second: &ModelRequest) -> Value {
    json!({
        "id": id,
        "stages": [
            {"stage": 1, "system_prompt": first.system_prompt, "messages": &first.conversation[..]},
            {"stage": 2, "system_prompt": second.system_prompt, "messages": &second.conversation[..]},
        ],
    })
}

/// One projected selection line, from a captured selection request.
fn projected_selection(seq: u64, request: &ModelRequest) -> Value {
    json!({
        "handoff": seq,
        "system_prompt": request.system_prompt,
        "messages": &request.conversation[..],
    })
}

/// Runs the jig on `dir` both ways and asserts the binary prints exactly
/// the in-process replay's projection: each review in `review_ids` order
/// from its stage pair, each selection in `handoff_seqs` order, and each
/// `unreplayable` line exactly. Returns the binary's lines for the
/// content assertions. `extra` carries flags such as `--keep-all`.
fn check_binary_output(
    dir: &Path,
    answers: &str,
    keep_open: bool,
    extra: &[&str],
    review_ids: &[&str],
    handoff_seqs: &[u64],
    unreplayable_ids: &[&str],
) -> Vec<Value> {
    let keep_all = extra.contains(&"--keep-all");
    let args = ReviewArgs {
        dir: dir.to_path_buf(),
        notes: None,
        keep_all,
    };
    let outcome = replay(&args, Cursor::new(answers.to_owned()), Vec::new()).unwrap();
    let stages: Vec<&ModelRequest> = outcome
        .requests
        .iter()
        .filter_map(|(kind, request)| {
            if *kind == RequestKind::Stage {
                Some(request)
            } else {
                None
            }
        })
        .collect();
    let selections: Vec<&ModelRequest> = outcome
        .requests
        .iter()
        .filter_map(|(kind, request)| {
            if *kind == RequestKind::Selection {
                Some(request)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(stages.len(), review_ids.len() * 2);
    assert_eq!(selections.len(), handoff_seqs.len());
    let output = run_jig(dir, answers, keep_open, extra);
    let lines = stdout_lines(&output);
    let mut reviews = review_ids.iter();
    let mut stage_at = 0;
    let mut handoffs = handoff_seqs.iter();
    let mut selection_at = 0;
    let mut unreplayable = unreplayable_ids.iter();
    for line in &lines {
        if line.get("stages").is_some() {
            let id = reviews.next().unwrap();
            let first = stages.get(stage_at).unwrap();
            let second = stages.get(stage_at + 1).unwrap();
            assert_eq!(line, &projected_review(id, first, second), "review of {id}");
            stage_at += 2;
        } else if line.get("handoff").is_some() {
            let seq = handoffs.next().unwrap();
            let request = selections.get(selection_at).unwrap();
            assert_eq!(line, &projected_selection(*seq, request), "selection {seq}");
            selection_at += 1;
        } else if line.get("unreplayable").is_some() {
            let id = unreplayable.next().unwrap();
            assert_eq!(
                line,
                &json!({"id": id, "unreplayable": "declared effects not logged"}),
            );
        } else {
            panic!("the jig prints reviews, selections and unreplayable lines, got {line}");
        }
    }
    assert_eq!(reviews.len(), 0);
    assert_eq!(handoffs.len(), 0);
    assert_eq!(unreplayable.len(), 0);
    assert_eq!(stage_at, stages.len());
    assert_eq!(selection_at, selections.len());
    lines
}

/// The `seq` of every completed handoff in `dir`, in order.
fn completed_handoffs(dir: &Path) -> Vec<u64> {
    log::read(dir)
        .unwrap()
        .iter()
        .filter(|line| line.kind == "handoff_completed")
        .filter(|line| line.payload.get("outcome").and_then(Value::as_str) == Some("completed"))
        .filter_map(|line| line.seq.map(|seq| seq.0))
        .collect()
}

/// One message's text.
fn message_text(message: &Value) -> String {
    message
        .get("text")
        .and_then(Value::as_str)
        .unwrap()
        .to_owned()
}

/// Every line's user texts, for the content assertions.
fn line_texts(line: &Value) -> Vec<String> {
    if let Some(messages) = line.get("messages").and_then(Value::as_array) {
        return messages.iter().map(message_text).collect();
    }
    line.get("stages")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .flat_map(|stage| {
            stage
                .get("messages")
                .and_then(Value::as_array)
                .unwrap()
                .iter()
                .map(message_text)
        })
        .collect()
}

#[test]
fn built_binary_prints_reviews_and_the_selection_in_order() {
    let fixture = main_fixture();
    let before = fixture.bytes();
    let handoffs = completed_handoffs(&fixture.dir);
    assert_eq!(handoffs.len(), 1);
    let lines = check_binary_output(
        &fixture.dir,
        "1\n",
        false,
        &[],
        &["a_1", "a_2", "a_blocked", "a_headless"],
        &handoffs,
        &[],
    );
    assert_eq!(lines.len(), 5);
    // The first call sees both person messages and its own call, with
    // the source workspace on its effects item.
    let first = line_texts(lines.first().unwrap());
    assert!(first.iter().any(|text| text.contains("never push to main")));
    assert!(first.iter().any(|text| text.contains("use tabs")));
    assert!(first.iter().any(|text| text.contains("Tool call:")));
    assert!(
        first
            .iter()
            .any(|text| text.contains("Workspace root: /synthetic/workspace"))
    );
    // The selection lists both messages for the answer to choose from.
    let selection = line_texts(lines.get(1).unwrap());
    assert!(
        selection
            .iter()
            .any(|text| text.contains("never push to main"))
    );
    assert!(selection.iter().any(|text| text.contains("use tabs")));
    // After the handoff only the kept message and later events appear:
    // no dropped message, no old call, no session message, no prose,
    // no result and no instruction text.
    let after = line_texts(lines.get(2).unwrap());
    assert!(after.iter().any(|text| text.contains("never push to main")));
    assert!(after.iter().any(|text| text.contains("now run it")));
    assert!(after.iter().any(|text| text.contains("also keep logs")));
    for text in &after {
        assert!(!text.contains("use tabs"), "dropped message: {text}");
        assert!(!text.contains("peer says hi"), "session message: {text}");
        assert!(!text.contains("prose MARKER"), "agent prose: {text}");
        assert!(!text.contains("result MARKER"), "tool result: {text}");
        assert!(
            !text.contains("instruction MARKER"),
            "instruction file: {text}"
        );
    }
    let calls: std::collections::BTreeSet<&String> = after
        .iter()
        .filter(|text| text.contains("Tool call:"))
        .collect();
    assert_eq!(
        calls.len(),
        1,
        "the old call leaves at the handoff: {after:?}"
    );
    // The source log is only read, never written.
    assert_eq!(fixture.bytes(), before);
}

#[test]
fn notes_flag_marks_every_system_prompt_and_empty_notes_match_no_notes() {
    let fixture = main_fixture();
    let home = fakes::TempDir::new("fiber-review-jig-notes");
    let marked = home.path().join("marked.md");
    std::fs::write(&marked, "NOTES-MARKER").unwrap();
    let empty = home.path().join("empty.md");
    std::fs::write(&empty, "").unwrap();

    let plain = run_jig(&fixture.dir, "1\n", false, &[]);
    let plain_lines = stdout_lines(&plain);
    let marked_output = run_jig(
        &fixture.dir,
        "1\n",
        false,
        &["--notes", marked.to_str().unwrap()],
    );
    let marked_lines = stdout_lines(&marked_output);
    assert_eq!(marked_lines.len(), plain_lines.len());
    let mut prompts = Vec::new();
    for line in &marked_lines {
        if let Some(stages) = line.get("stages").and_then(Value::as_array) {
            prompts.extend(
                stages
                    .iter()
                    .map(|stage| stage.get("system_prompt").unwrap()),
            );
        } else if let Some(prompt) = line.get("system_prompt") {
            prompts.push(prompt);
        }
    }
    // Two stages per review plus the selection request.
    assert_eq!(prompts.len(), 9);
    for prompt in prompts {
        assert!(prompt.as_str().unwrap().contains("NOTES-MARKER"));
    }
    let empty_output = run_jig(
        &fixture.dir,
        "1\n",
        false,
        &["--notes", empty.to_str().unwrap()],
    );
    assert_eq!(empty_output.stdout, plain.stdout);
}

#[test]
fn keep_none_selection_drops_every_earlier_person_message() {
    let fixture = main_fixture();
    let handoffs = completed_handoffs(&fixture.dir);
    let lines = check_binary_output(
        &fixture.dir,
        "none\n",
        false,
        &[],
        &["a_1", "a_2", "a_blocked", "a_headless"],
        &handoffs,
        &[],
    );
    let after = line_texts(lines.get(2).unwrap());
    assert!(!after.iter().any(|text| text.contains("never push to main")));
    assert!(!after.iter().any(|text| text.contains("use tabs")));
    assert!(after.iter().any(|text| text.contains("now run it")));
    assert!(after.iter().any(|text| text.contains("also keep logs")));
}

#[test]
fn keep_all_matches_the_eof_selection_failure_path() {
    let fixture = main_fixture();
    let kept = run_jig(&fixture.dir, "", false, &["--keep-all"]);
    let failed = run_jig(&fixture.dir, "", false, &[]);
    let kept_lines = stdout_lines(&kept);
    let failed_lines = stdout_lines(&failed);
    // `--keep-all` prints no selection request of its own; the EOF run
    // prints one before its answer ends. Both keep every earlier person
    // message, so their review and unreplayable lines match exactly.
    assert_eq!(kept_lines.len(), 4);
    assert_eq!(failed_lines.len(), 5);
    assert_eq!(kept_lines.first(), failed_lines.first());
    assert_eq!(kept_lines.get(1), failed_lines.get(2));
    assert_eq!(kept_lines.get(2), failed_lines.get(3));
    assert_eq!(kept_lines.get(3), failed_lines.get(4));
    let after = line_texts(kept_lines.get(1).unwrap());
    assert!(after.iter().any(|text| text.contains("never push to main")));
    assert!(after.iter().any(|text| text.contains("use tabs")));
}

#[test]
fn malformed_selection_falls_back_without_carrying_reviewer_prose() {
    let fixture = main_fixture();
    // Stdin stays open: a second read would block instead of seeing EOF,
    // so reusing the cached answer is what lets this finish.
    let output = run_jig(&fixture.dir, "the person said keep ZQX-PROSE\n", true, &[]);
    let lines = stdout_lines(&output);
    assert_eq!(lines.len(), 5);
    let after = line_texts(lines.get(2).unwrap());
    assert!(after.iter().any(|text| text.contains("never push to main")));
    assert!(after.iter().any(|text| text.contains("use tabs")));
    for line in &lines {
        assert!(
            !serde_json::to_string(line).unwrap().contains("ZQX-PROSE"),
            "no reviewer prose enters a later prompt: {line}"
        );
    }
}

#[test]
fn empty_person_handoff_prints_no_request_and_reads_no_answer() {
    let fixture = Fixture::new("s_empty", "/synthetic/workspace");
    fixture.append(&user_turn(&[]), Some("t_1"), None);
    fixture.append(&handoff_boundary(Outcome::Completed), Some("t_1"), None);
    fixture.append(
        &requested("shell", json!({"city": "Paris"})),
        Some("t_2"),
        Some("a_1"),
    );
    fixture.append(
        &started(vec![Effect::Executes], false),
        Some("t_2"),
        Some("a_1"),
    );
    // Stdin is closed at once: the exchange still succeeds, so no handoff
    // read anything.
    let lines = check_binary_output(&fixture.dir, "", false, &[], &["a_1"], &[], &[]);
    assert_eq!(lines.len(), 1);
    let first = line_texts(lines.first().unwrap());
    assert!(
        first.iter().any(|text| text.starts_with("Tool call: ")),
        "{first:?}"
    );
}

#[test]
fn repeated_handoffs_select_from_the_previously_kept_messages() {
    let fixture = Fixture::new("s_repeat", "/synthetic/workspace");
    fixture.append(
        &user_turn(&["first order", "second order"]),
        Some("t_1"),
        None,
    );
    fixture.append(&handoff_boundary(Outcome::Completed), Some("t_1"), None);
    fixture.append(&user_turn(&["third order"]), Some("t_2"), None);
    fixture.append(&handoff_boundary(Outcome::Completed), Some("t_2"), None);
    fixture.append(
        &requested("shell", json!({"city": "Paris"})),
        Some("t_3"),
        Some("a_1"),
    );
    fixture.append(
        &started(vec![Effect::Executes], false),
        Some("t_3"),
        Some("a_1"),
    );
    let handoffs = completed_handoffs(&fixture.dir);
    assert_eq!(handoffs.len(), 2);
    // The first answer keeps the first order; the second keeps the third,
    // numbered second among the previously kept plus the new message.
    let lines = check_binary_output(&fixture.dir, "1\n2\n", false, &[], &["a_1"], &handoffs, &[]);
    assert_eq!(lines.len(), 3);
    let after = line_texts(lines.get(2).unwrap());
    assert!(!after.iter().any(|text| text.contains("first order")));
    assert!(!after.iter().any(|text| text.contains("second order")));
    assert!(after.iter().any(|text| text.contains("third order")));
}

#[test]
fn failed_and_cancelled_handoffs_leave_the_input_intact() {
    let fixture = Fixture::new("s_failed", "/synthetic/workspace");
    fixture.append(&user_turn(&["standing order"]), Some("t_1"), None);
    fixture.append(&handoff_boundary(Outcome::Failed), Some("t_1"), None);
    fixture.append(&handoff_boundary(Outcome::Cancelled), Some("t_1"), None);
    fixture.append(
        &requested("shell", json!({"city": "Paris"})),
        Some("t_2"),
        Some("a_1"),
    );
    fixture.append(
        &started(vec![Effect::Executes], false),
        Some("t_2"),
        Some("a_1"),
    );
    // A failed or cancelled handoff selects nothing: no request prints.
    let lines = check_binary_output(&fixture.dir, "", false, &[], &["a_1"], &[], &[]);
    assert_eq!(lines.len(), 1);
    let first = line_texts(lines.first().unwrap());
    assert!(first.iter().any(|text| text.contains("standing order")));
}

#[test]
fn repaired_arguments_replay_as_the_call_that_would_run() {
    let fixture = Fixture::new("s_repair", "/synthetic/workspace");
    fixture.append(&user_turn(&["go"]), Some("t_1"), None);
    fixture.append(
        &Event::ToolCallRequested(ToolCallRequested {
            name: "shell".into(),
            arguments: json!({"city": "NOWHERE-ORIGINAL"}),
            provider_id: None,
            repair: Some(ArgumentRepair {
                repaired: serde_json::from_value(json!({"city": "Paris"})).unwrap(),
                repairs: vec![Repair {
                    path: "/city".into(),
                    fix: RepairFix::StringParsed,
                }],
            }),
            ran_by: None,
            provider_item: None,
        }),
        Some("t_1"),
        Some("a_1"),
    );
    fixture.append(
        &started(vec![Effect::Executes], false),
        Some("t_1"),
        Some("a_1"),
    );
    let lines = check_binary_output(&fixture.dir, "", false, &[], &["a_1"], &[], &[]);
    let first = line_texts(lines.first().unwrap());
    assert!(first.iter().any(|text| text.contains("Paris")));
    for text in &first {
        assert!(!text.contains("NOWHERE-ORIGINAL"), "{text}");
    }
}

#[test]
fn rewritten_arguments_name_the_rewrite_in_the_effects_item() {
    let fixture = Fixture::new("s_rewrite", "/synthetic/workspace");
    fixture.append(&user_turn(&["go"]), Some("t_1"), None);
    fixture.append(
        &requested("shell", json!({"city": "Nowhere"})),
        Some("t_1"),
        Some("a_1"),
    );
    fixture.append(
        &Event::ToolCallStarted(ToolCallStarted {
            always_reviewed: false,
            declared: DeclaredEffects {
                effects: vec![Effect::Executes],
                reversible: false,
                paths: None,
                confined: false,
            },
            arguments: Some(serde_json::from_value(json!({"city": "Paris"})).unwrap()),
            changed_by: Some(vec!["ext".into()]),
        }),
        Some("t_1"),
        Some("a_1"),
    );
    let lines = check_binary_output(&fixture.dir, "", false, &[], &["a_1"], &[], &[]);
    let first = line_texts(lines.first().unwrap());
    assert!(
        first
            .iter()
            .any(|text| text.contains("Extension ext rewrote this call's arguments")),
        "{first:?}"
    );
    assert!(first.iter().any(|text| text.contains("Paris")));
}

#[test]
fn inner_hosted_and_non_review_calls_print_no_lines_of_their_own() {
    let fixture = Fixture::new("s_history", "/synthetic/workspace");
    fixture.append(&user_turn(&["go"]), Some("t_1"), None);
    fixture.append(
        &requested("shell", json!({"city": "Paris"})),
        Some("t_1"),
        Some("a_outer"),
    );
    fixture.append(
        &started(vec![Effect::Executes], false),
        Some("t_1"),
        Some("a_outer"),
    );
    fixture.append(
        &Event::ToolCallRequested(ToolCallRequested {
            name: "inner".into(),
            arguments: json!({"city": "Paris"}),
            provider_id: None,
            repair: None,
            ran_by: Some(RanBy {
                extension: "ext".into(),
                anchor: Anchor::OuterCall(ActionId("a_outer".into())),
            }),
            provider_item: None,
        }),
        Some("t_1"),
        Some("a_inner"),
    );
    fixture.append(
        &started(vec![Effect::Reads], true),
        Some("t_1"),
        Some("a_inner"),
    );
    // A denial carrying declared effects for an inner call indexes
    // nothing: only outer calls replay, so the line count is unchanged.
    fixture.append(
        &Event::PermissionResolved(PermissionResolved {
            call: Some(ReviewedCall {
                declared: DeclaredEffects {
                    effects: vec![Effect::Reads],
                    reversible: true,
                    paths: None,
                    confined: false,
                },
                always_reviewed: false,
                arguments: None,
                changed_by: None,
            }),
            request_id: None,
            decision: Decision::Deny,
            decided_by: DecidedBy::Reviewer,
            reason: Some("no".into()),
            feedback: None,
            grant: None,
            rule: None,
            reviewer: Some(ReviewerRef {
                model: "fake/reviewer".into(),
                stage: 1,
            }),
            hosts: None,
        }),
        Some("t_1"),
        Some("a_inner"),
    );
    fixture.append(
        &Event::ToolCallRequested(ToolCallRequested {
            name: "web_search".into(),
            arguments: json!({"query": "rust"}),
            provider_id: Some(contract::ProviderCallId("srv_1".into())),
            repair: None,
            ran_by: None,
            provider_item: Some(json!({"type": "server_tool_use", "id": "srv_1"})),
        }),
        Some("t_1"),
        Some("a_hosted"),
    );
    fixture.append(
        &started(vec![Effect::Network], true),
        Some("t_1"),
        Some("a_hosted"),
    );
    fixture.append(
        &requested("shell", json!({"city": "Paris"})),
        Some("t_1"),
        Some("a_person"),
    );
    fixture.append(&person_allowed(), Some("t_1"), Some("a_person"));
    // The person's allow carries declared effects, yet the call is still
    // not a review request: the reviewer never judged it.
    fixture.append(
        &started(vec![Effect::Executes], false),
        Some("t_1"),
        Some("a_person"),
    );
    fixture.append(
        &requested("shell", json!({"city": "Paris"})),
        Some("t_1"),
        Some("a_standing"),
    );
    fixture.append(
        &Event::PermissionRequested(PermissionRequested {
            always_reviewed: false,
            request_id: RequestId("r_standing".into()),
            declared: DeclaredEffects {
                effects: vec![Effect::Executes],
                reversible: false,
                paths: None,
                confined: false,
            },
            step: AskStep::StandingAsk {
                standing_rule: StandingRule {
                    scope: RuleScope::Project,
                    prefix: "run tests".into(),
                },
            },
        }),
        Some("t_1"),
        Some("a_standing"),
    );
    // A standing ask that let the call through: declared effects logged,
    // still not a review request.
    fixture.append(
        &started(vec![Effect::Executes], false),
        Some("t_1"),
        Some("a_standing"),
    );
    fixture.append(
        &requested("shell", json!({"city": "Paris"})),
        Some("t_1"),
        Some("a_rule"),
    );
    // A standing-rule denial never started: no review judged it, so no
    // unreplayable line either.
    fixture.append(
        &rule_denied("a standing rule forbids it"),
        Some("t_1"),
        Some("a_rule"),
    );
    let lines = check_binary_output(&fixture.dir, "", false, &[], &["a_outer"], &[], &[]);
    assert_eq!(lines.len(), 1);
}

#[test]
fn unknown_event_kinds_are_ignored() {
    let fixture = main_fixture();
    append_raw(
        &fixture.dir,
        "s_fixture",
        "future_kind",
        json!({"note": "from the future"}),
    );
    let handoffs = completed_handoffs(&fixture.dir);
    let lines = check_binary_output(
        &fixture.dir,
        "1\n",
        false,
        &[],
        &["a_1", "a_2", "a_blocked", "a_headless"],
        &handoffs,
        &[],
    );
    assert_eq!(lines.len(), 5);
}

#[test]
fn requests_before_starts_correlate_by_action_with_a_request_time_cut() {
    let fixture = Fixture::new("s_order", "/synthetic/workspace");
    fixture.append(&user_turn(&["go"]), Some("t_1"), None);
    fixture.append(
        &requested("shell", json!({"city": "ALPHA"})),
        Some("t_1"),
        Some("a_1"),
    );
    fixture.append(
        &requested("shell", json!({"city": "BETA"})),
        Some("t_1"),
        Some("a_2"),
    );
    fixture.append(
        &started(vec![Effect::Executes], false),
        Some("t_1"),
        Some("a_2"),
    );
    fixture.append(
        &started(vec![Effect::Executes], false),
        Some("t_1"),
        Some("a_1"),
    );
    // Starts log reversed, but lines print in original request order.
    let lines = check_binary_output(&fixture.dir, "", false, &[], &["a_1", "a_2"], &[], &[]);
    assert_eq!(lines.len(), 2);
    // The first request's cut never admits its later sibling.
    let first = line_texts(lines.first().unwrap());
    assert!(first.iter().any(|text| text.contains("ALPHA")));
    assert!(!first.iter().any(|text| text.contains("BETA")));
    // The later request's cut holds the earlier sibling.
    let second = line_texts(lines.get(1).unwrap());
    assert!(second.iter().any(|text| text.contains("ALPHA")));
    assert!(second.iter().any(|text| text.contains("BETA")));
}

#[test]
fn calls_print_at_their_request_position_without_moving_across_handoffs() {
    let fixture = Fixture::new("s_cross", "/synthetic/workspace");
    fixture.append(&user_turn(&["standing order"]), Some("t_1"), None);
    fixture.append(
        &requested("shell", json!({"city": "Paris"})),
        Some("t_1"),
        Some("a_early"),
    );
    fixture.append(&review_step("r_early"), Some("t_1"), Some("a_early"));
    fixture.append(
        &requested("shell", json!({"city": "Paris"})),
        Some("t_1"),
        Some("a_blocked"),
    );
    fixture.append(
        &legacy_reviewer_denied("that push rewrites shared history"),
        Some("t_1"),
        Some("a_blocked"),
    );
    fixture.append(&handoff_boundary(Outcome::Completed), Some("t_1"), None);
    fixture.append(&user_turn(&["after the handoff"]), Some("t_2"), None);
    // The early call's start logs after the handoff, yet its review
    // still prints at its request position, before the selection.
    fixture.append(
        &started(vec![Effect::Executes], false),
        Some("t_2"),
        Some("a_early"),
    );
    let handoffs = completed_handoffs(&fixture.dir);
    assert_eq!(handoffs.len(), 1);
    let lines = check_binary_output(
        &fixture.dir,
        "1\n",
        false,
        &[],
        &["a_early"],
        &handoffs,
        &["a_blocked"],
    );
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0].get("id").unwrap(), &json!("a_early"));
    assert!(lines[0].get("stages").is_some());
    assert_eq!(
        lines[1],
        json!({"id": "a_blocked", "unreplayable": "declared effects not logged"}),
    );
    assert_eq!(lines[2].get("handoff").unwrap(), &json!(handoffs[0]));
    // The request-time cut holds: nothing after the handoff enters the
    // early call's replay.
    let early = line_texts(&lines[0]);
    assert!(early.iter().any(|text| text.contains("standing order")));
    assert!(!early.iter().any(|text| text.contains("after the handoff")));
}

#[test]
fn a_person_message_after_the_request_stays_out_of_its_replay() {
    let fixture = Fixture::new("s_late", "/synthetic/workspace");
    fixture.append(&user_turn(&["go"]), Some("t_1"), None);
    fixture.append(
        &requested("shell", json!({"city": "Paris"})),
        Some("t_1"),
        Some("a_1"),
    );
    fixture.append(&driver_steering("LATE-MESSAGE"), Some("t_1"), None);
    fixture.append(
        &started(vec![Effect::Executes], false),
        Some("t_1"),
        Some("a_1"),
    );
    let lines = check_binary_output(&fixture.dir, "", false, &[], &["a_1"], &[], &[]);
    let first = line_texts(lines.first().unwrap());
    assert!(!first.iter().any(|text| text.contains("LATE-MESSAGE")));
}

#[test]
fn forked_history_replays_across_the_ancestor_point() {
    let parent = Fixture::new("s_parent", "/synthetic/workspace");
    parent.append(&user_turn(&["ancestor instruction"]), Some("t_1"), None);
    parent.append(&user_turn(&["after the fork"]), Some("t_2"), None);
    let home = parent._home.path().to_path_buf();
    let clock = fakes::clock::FakeClock::new();
    let child_log = log::Log::create(home.as_path(), SessionId("s_child".into()), clock).unwrap();
    let child = child_log.dir().to_path_buf();
    child_log
        .append(
            &Event::SessionStarted(SessionStarted {
                workspace: "/synthetic/workspace".to_owned(),
                variables: Variables::of(
                    Vec::<(std::ffi::OsString, std::ffi::OsString)>::new(),
                    VariablesSource::Inherited,
                ),
                parent: None,
                forked_from: Some(Point {
                    session_id: SessionId("s_parent".into()),
                    seq: Seq(1),
                    project: None,
                }),
                rewind: None,
                worktree: None,
                supervised: false,
            }),
            None,
            None,
        )
        .unwrap();
    let append_child = |event: &Event, turn: Option<&str>, action: Option<&str>| {
        child_log
            .append(
                event,
                turn.map(|turn| TurnId(turn.to_owned())),
                action.map(|action| ActionId(action.to_owned())),
            )
            .unwrap();
    };
    append_child(&user_turn(&["child question"]), Some("t_3"), None);
    append_child(
        &requested("shell", json!({"city": "Paris"})),
        Some("t_3"),
        Some("a_1"),
    );
    append_child(
        &started(vec![Effect::Executes], false),
        Some("t_3"),
        Some("a_1"),
    );
    let lines = check_binary_output(&child, "", false, &[], &["a_1"], &[], &[]);
    let first = line_texts(lines.first().unwrap());
    assert!(
        first
            .iter()
            .any(|text| text.contains("ancestor instruction"))
    );
    assert!(first.iter().any(|text| text.contains("child question")));
    assert!(!first.iter().any(|text| text.contains("after the fork")));
}

#[test]
fn bad_argv_and_notes_fail_through_the_binary() {
    let fixture = main_fixture();
    let usage = run_jig(&fixture.dir, "", false, &["--bogus"]);
    assert_eq!(usage.status.code(), Some(2));
    let missing = run_jig(
        &fixture.dir,
        "",
        false,
        &["--notes", "/nonexistent/review-notes.md"],
    );
    assert_eq!(missing.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("/nonexistent/review-notes.md"));
    let warmed = example_path();
    let none = Command::new(warmed.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let output = wait_under(none, JIG_DEADLINE);
    assert_eq!(output.status.code(), Some(2));
}

/// A writer that fails: the jig reports the write instead of its replay.
struct FailWrite;

impl Write for FailWrite {
    fn write(&mut self, _bytes: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("no room"))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn output_write_failure_is_a_jig_error_through_shared_support() {
    let fixture = main_fixture();
    let args = ReviewArgs {
        dir: fixture.dir.clone(),
        notes: None,
        keep_all: false,
    };
    let problem = replay(&args, Cursor::new("1\n"), FailWrite).unwrap_err();
    assert!(problem.contains("cannot write its output"), "{problem}");
}

/// Appends one raw line carrying `kind` and `payload` at the log's end.
fn append_raw(dir: &Path, session: &str, kind: &str, payload: Value) {
    let next = u64::try_from(log::read(dir).unwrap().len()).unwrap();
    let line = json!({
        "kind": kind,
        "session_id": session,
        "ts": 1,
        "schema_version": 1,
        "seq": next,
        "payload": payload,
    });
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(dir.join("events.jsonl"))
        .unwrap();
    file.write_all(serde_json::to_string(&line).unwrap().as_bytes())
        .unwrap();
    file.write_all(b"\n").unwrap();
}

#[test]
fn bad_lines_fail_without_touching_source_bytes() {
    // A complete line that is not JSON.
    let broken = main_fixture();
    std::fs::OpenOptions::new()
        .append(true)
        .open(broken.dir.join("events.jsonl"))
        .unwrap()
        .write_all(b"not json\n")
        .unwrap();
    let before = broken.bytes();
    let output = run_jig(&broken.dir, "", false, &[]);
    assert!(!output.status.success());
    assert!(!output.stderr.is_empty());
    assert_eq!(broken.bytes(), before);

    // A JSON line whose payload does not parse.
    let payload = main_fixture();
    append_raw(&payload.dir, "s_fixture", "tool_call_requested", json!({}));
    let before = payload.bytes();
    let output = run_jig(&payload.dir, "", false, &[]);
    assert!(!output.status.success());
    assert!(!output.stderr.is_empty());
    assert_eq!(payload.bytes(), before);

    // The in-process replay fails the same way.
    let args = ReviewArgs {
        dir: payload.dir.clone(),
        notes: None,
        keep_all: false,
    };
    assert!(replay(&args, Cursor::new(""), Vec::new()).is_err());
}

#[test]
fn missing_session_or_start_is_an_input_error() {
    // No log at all.
    let home = fakes::TempDir::new("fiber-review-jig-missing");
    let absent = home.path().join("s_absent");
    let output = run_jig(&absent, "", false, &[]);
    assert!(!output.status.success());

    // An empty log.
    let empty_home = fakes::TempDir::new("fiber-review-jig-empty");
    let empty_log = log::Log::create(
        empty_home.path(),
        SessionId("s_empty".into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    let empty_dir = empty_log.dir().to_path_buf();
    let output = run_jig(&empty_dir, "", false, &[]);
    assert!(!output.status.success());

    // A log that does not begin with session_started: `create` writes no
    // first line, so the turn is the log's first line.
    let headed_home = fakes::TempDir::new("fiber-review-jig-headed");
    let headed_log = log::Log::create(
        headed_home.path(),
        SessionId("s_headed".into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    let headed_dir = headed_log.dir().to_path_buf();
    headed_log
        .append(&user_turn(&["first"]), Some(TurnId("t_1".into())), None)
        .unwrap();
    let output = run_jig(&headed_dir, "", false, &[]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("session_started"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A log written before the declared-effects change still prints
/// `unreplayable` for both denials: neither decision carries the keys.
#[test]
fn a_log_written_before_the_change_still_prints_unreplayable_for_both_denials() {
    let fixture = Fixture::new("s_legacy", "/synthetic/workspace");
    fixture.append(&user_turn(&["go"]), Some("t_1"), None);
    fixture.append(
        &requested("shell", json!({"city": "Paris"})),
        Some("t_1"),
        Some("a_blocked"),
    );
    fixture.append(
        &legacy_reviewer_denied("that push rewrites shared history"),
        Some("t_1"),
        Some("a_blocked"),
    );
    fixture.append(
        &requested("shell", json!({"city": "Paris"})),
        Some("t_1"),
        Some("a_headless"),
    );
    fixture.append(
        &legacy_headless_denied("no person can answer"),
        Some("t_1"),
        Some("a_headless"),
    );
    let lines = check_binary_output(
        &fixture.dir,
        "",
        false,
        &[],
        &[],
        &[],
        &["a_blocked", "a_headless"],
    );
    assert_eq!(lines.len(), 2);
    assert_eq!(
        lines[0],
        json!({"id": "a_blocked", "unreplayable": "declared effects not logged"}),
    );
    assert_eq!(
        lines[1],
        json!({"id": "a_headless", "unreplayable": "declared effects not logged"}),
    );
}

/// A blocked call a hook rewrote replays with the rewritten arguments:
/// the effects item names the rewrite and the judged city, never the
/// model's original.
#[test]
fn a_blocked_call_a_hook_rewrote_replays_with_the_rewritten_arguments() {
    let fixture = Fixture::new("s_rewritten_denial", "/synthetic/workspace");
    fixture.append(&user_turn(&["go"]), Some("t_1"), None);
    fixture.append(
        &requested("shell", json!({"city": "Nowhere"})),
        Some("t_1"),
        Some("a_1"),
    );
    fixture.append(
        &Event::PermissionResolved(PermissionResolved {
            call: Some(ReviewedCall {
                declared: DeclaredEffects {
                    effects: vec![Effect::Executes],
                    reversible: false,
                    paths: None,
                    confined: false,
                },
                always_reviewed: false,
                arguments: Some(serde_json::from_value(json!({"city": "Paris"})).unwrap()),
                changed_by: Some(vec!["ext".into()]),
            }),
            request_id: None,
            decision: Decision::Deny,
            decided_by: DecidedBy::Reviewer,
            reason: Some("no".into()),
            feedback: None,
            grant: None,
            rule: None,
            reviewer: Some(ReviewerRef {
                model: "fake/reviewer".into(),
                stage: 1,
            }),
            hosts: None,
        }),
        Some("t_1"),
        Some("a_1"),
    );
    let lines = check_binary_output(&fixture.dir, "", false, &[], &["a_1"], &[], &[]);
    assert_eq!(lines.len(), 1);
    let texts = line_texts(lines.first().unwrap());
    let rewrite_items: Vec<_> = texts
        .iter()
        .filter(|text| text.contains("rewrote"))
        .collect();
    assert!(!rewrite_items.is_empty(), "{texts:?}");
    for text in &rewrite_items {
        assert!(text.contains("Paris"), "{text}");
        assert!(!text.contains("Nowhere"), "{text}");
    }
}

/// A `review` exchange held before the binary starts: the wrapper has
/// reported its pid on the ready FIFO and is blocked reading the release
/// FIFO, and the exchange's result arrives on `finished` once the hold is
/// released. Dropping this value without releasing kills the group
/// through the watchdog. The exchange's finish bound is a carried
/// wall-clock deadline stamped at release: the release step carries
/// release plus [`START_BOUND`], so the held start takes none of it.
struct HeldExchange {
    /// The hold directory, kept alive while the FIFOs are held.
    _home: fakes::TempDir,
    /// The release FIFO's write side, kept open until the exchange
    /// finishes so the release line never meets a closed writer.
    hold: std::fs::File,
    group: u32,
    watchdog: Watchdog,
    finished: mpsc::Receiver<std::io::Result<Output>>,
    /// The carried finish deadline, stamped at release. `None` until
    /// [`HeldExchange::release`] runs, so `finish` waits on the same
    /// value the release step carried.
    finish_deadline: Option<Deadline>,
}

impl HeldExchange {
    /// Writes the release line and stamps the exchange's finish deadline
    /// in the actual release path: release plus [`START_BOUND`].
    /// The test reads the carried value before finishing.
    fn release(&mut self) -> Deadline {
        self.hold.write_all(b"release\n").unwrap();
        let deadline = Deadline::after(START_BOUND);
        self.finish_deadline = Some(deadline);
        deadline
    }

    /// Waits only the remaining time until the carried deadline stamped
    /// by [`HeldExchange::release`]: the carried deadline is the wait's
    /// deadline, so the held start takes none of the exchange's bound.
    /// The wait asserts nothing about speed, it only fails a hung child
    /// by name.
    fn finish(self) -> Output {
        let deadline = self.finish_deadline.expect("release before finish");
        let output = match deadline.recv(&self.finished) {
            Ok(output) => output.unwrap(),
            Err(_) => {
                kill_group(self.group, "KILL").unwrap_or(false);
                let output = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
                    .recv(&self.finished)
                    .expect("the review jig died after KILL");
                panic!(
                    "the review jig did not finish in time: {}",
                    String::from_utf8_lossy(&output.unwrap().stdout)
                );
            }
        };
        self.watchdog
            .stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
        output
    }
}

/// Spawns the built binary through an inline `/bin/sh -c` wrapper: the
/// wrapper reports its pid on a [`fakes::children::Ready`] FIFO, blocks
/// reading a parent-held release FIFO, then execs the binary with the
/// remaining positional parameters: no script file is written or executed,
/// and the shell quotes the parameters itself. The parent pre-opens the
/// release FIFO read-write so neither side's open blocks, writes `answers`
/// to the wrapper's stdin as [`run_jig`] does, and reads the pid under
/// [`START_BOUND`] as the signal the child is blocked. Returns the
/// exchange still held; the test releases it.
fn spawn_held_jig(dir: &Path, answers: &str, keep_open: bool, args: &[&str]) -> HeldExchange {
    let home = fakes::TempDir::new("fiber-review-jig-hold");
    let ready = fakes::children::Ready::new(home.path());
    let release_path = home.path().join("release");
    fakes::fifo(&release_path);
    let hold = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&release_path)
        .unwrap();
    let warmed = example_path();
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg(r#"printf '%s\n' "$$" > "$1"; IFS= read -r release < "$2"; shift 2; exec "$@""#)
        .arg("sh")
        .arg(ready.path())
        .arg(&release_path)
        .arg(warmed.path())
        .arg(dir)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let group = child.id();
    let watchdog = Watchdog::group(group);
    if !answers.is_empty() {
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(answers.as_bytes())
            .unwrap();
    }
    if !keep_open {
        child.stdin.take();
    }
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait_with_output()).unwrap());
    ready.wait(fakes::Deadline::after(START_BOUND));
    HeldExchange {
        _home: home,
        hold,
        group,
        watchdog,
        finished,
        finish_deadline: None,
    }
}

/// A start stalled while held still replays once released: the held
/// start takes none of the exchange's own bound. The hold is a
/// non-blocking empty check, never a sleep: nothing can complete while
/// the release line is unwritten, and the carried deadline still lies a
/// whole [`START_BOUND`] past release, so a single bound from spawn
/// would already have expired.
#[test]
fn stalled_start_keeps_the_exchange_deadline_whole() {
    let fixture = main_fixture();
    let mut held = spawn_held_jig(&fixture.dir, "1\n", false, &[]);
    // The release line is unwritten, so completion is impossible unless
    // the child crashed, which must also fail the test.
    assert!(
        matches!(held.finished.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "the exchange waits out the held start"
    );
    let before_release = fakes::clock::SystemClock.now();
    let deadline = held.release();
    assert!(
        deadline.left().saturating_add(
            fakes::clock::SystemClock
                .now()
                .saturating_duration_since(before_release)
        ) >= START_BOUND,
        "the exchange deadline starts at release"
    );
    let lines = stdout_lines(&held.finish());
    assert_eq!(lines.len(), 5);
    assert!(lines[0].get("stages").is_some());
}
