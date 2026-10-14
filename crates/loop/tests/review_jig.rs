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

use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

use contract::events::TurnOutcome;
use contract::provider::ModelRequest;
use contract::tool::Tool;
use fakes::Scripted;
use support::review::{RequestKind, ReviewArgs, parse_args, replay};
use support::{Session, calls_reply, delivery, handoff, paris, shell};

/// How long the whole jig exchange may take. `.config/nextest.toml` kills
/// a slow test after a 30s period times 4 (120s total); with the 5s
/// watchdog stand-down the deadlines sum to 25s, so 120s is at least twice
/// them: `docs/testing.md`, "Waits and timeouts", needs nextest's timeout
/// to be at least twice the test's own deadlines, so a hang reports which
/// wait expired.
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
/// blanked: cache key and session directory name the live session, never
/// the reviewed input.
fn native_request(requests: &[ModelRequest], at: usize) -> ModelRequest {
    let mut request = requests.get(at).unwrap().clone();
    request.cache_key = String::new();
    request.session_dir = Default::default();
    request
}

/// The replay's request at `at`, with the session-bound fields blanked.
fn replayed_request(requests: &[(RequestKind, ModelRequest)], at: usize) -> ModelRequest {
    let mut request = requests.get(at).unwrap().1.clone();
    request.cache_key = String::new();
    request.session_dir = Default::default();
    request
}

/// The user texts of a captured request's conversation.
fn texts(request: &ModelRequest) -> Vec<String> {
    request
        .conversation
        .iter()
        .map(|input| match input {
            contract::provider::Input::User { text, .. } => text.clone(),
            contract::provider::Input::Assistant { .. }
            | contract::provider::Input::Reasoning { .. }
            | contract::provider::Input::ToolCall { .. }
            | contract::provider::Input::ToolResult { .. } => {
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
    let _unused = JIG_DEADLINE;

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
        first_texts[2].starts_with("Declared effects: "),
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
