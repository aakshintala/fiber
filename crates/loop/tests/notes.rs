//! Reviewer-note requests through the loop's public inbox.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

mod support;

use std::sync::mpsc;

use contract::ErrorCode;
use contract::events::{CommandResult, TurnOutcome};
use contract::inbox::{Ack, Answer, Delivery};
use contract::notes::{AllowsTally, Evidence, NotesAsk, ProposeNotesArgs, Topic};
use fakes::{Deadline, Scripted};
use support::{DEADLINE, Session, spawn_run};

const BUSY: &str = "A turn is running; send `steer` to add to it.";
const CLOSING: &str = "The session is closing and takes no new turn.";

fn capture() -> (Ack, mpsc::Receiver<Answer>) {
    let (tx, rx) = mpsc::channel();
    (
        Ack(Box::new(move |answer| {
            tx.send(answer).unwrap();
        })),
        rx,
    )
}

fn take(rx: &mpsc::Receiver<Answer>) -> Answer {
    Deadline::after(DEADLINE).recv_or_fail(rx, "notes acknowledgement")
}

fn read_only_ask() -> NotesAsk {
    NotesAsk {
        args: ProposeNotesArgs {
            topic: Topic::ReadOnlyCommands,
            answer: None,
            earlier: vec![],
        },
        evidence: vec![],
    }
}

fn tally(prefix: &str, subject: &str) -> Evidence {
    Evidence::Allows(AllowsTally::new(prefix.into(), subject.into(), 5, 5).unwrap())
}

fn withheld(subject: &str, reason: &str) -> Evidence {
    Evidence::Allows(AllowsTally::from_wire(subject.into(), 5, 5, reason.into()).unwrap())
}

#[test]
fn notes_on_the_inbox_record_usage_before_acknowledging() {
    let mut session = Session::new(vec![Scripted::text("1 | This is ours.")], None);
    let dir = session.dir.clone();
    let (answer, answered) = mpsc::channel();
    session
        .inbox
        .send(Delivery::ProposeNotes(
            NotesAsk {
                args: ProposeNotesArgs {
                    topic: Topic::SourceControl,
                    answer: None,
                    earlier: vec![],
                },
                evidence: vec![Evidence::Files {
                    text: "our remote".into(),
                }],
            },
            Ack(Box::new(move |result| {
                // Read inside the ack, not after it: a later append cannot satisfy this assertion.
                answer.send((result, log::read(&dir).unwrap())).unwrap();
            })),
        ))
        .unwrap();
    let finished = spawn_run(&mut session);
    let (result, at_ack) =
        Deadline::after(DEADLINE).recv_or_fail(&answered, "notes request acknowledged");
    let Ok(Some(CommandResult::ProposeNotes {
        evidence,
        proposed,
        error,
    })) = result
    else {
        panic!("notes returned the wrong result: {result:?}");
    };
    assert!(error.is_none());
    assert_eq!(
        evidence,
        [Evidence::Files {
            text: "our remote".into()
        }]
    );
    assert_eq!(proposed.len(), 1);
    assert_eq!(proposed[0].text(), "This is ours.");
    assert_eq!(proposed[0].cites(), [0]);
    assert_eq!(
        at_ack
            .iter()
            .map(|line| line.kind.as_str())
            .collect::<Vec<_>>(),
        ["session_started", "usage_recorded"]
    );
    let usage = &at_ack[1];
    assert!(usage.turn_id.is_none());
    assert!(usage.action_id.is_none());
    assert!(usage.payload.get("reviewer").is_none());
    assert_eq!(session.requests().len(), 1);
    session
        .inbox
        .send(Delivery::Close(support::ignore()))
        .unwrap();
    let ran = Deadline::after(DEADLINE).recv_or_fail(&finished, "notes session closed");
    assert!(ran.is_ok(), "{ran:?}");
}

/// Runs one read-only ask through the public inbox, closing the session
/// once its ack arrives: the log snapshot is read inside the ack.
fn run_read_only(session: &mut Session, ask: NotesAsk) -> (Answer, Vec<contract::Envelope>) {
    let dir = session.dir.clone();
    let (answer, answered) = mpsc::channel();
    session
        .inbox
        .send(Delivery::ProposeNotes(
            ask,
            Ack(Box::new(move |result| {
                answer.send((result, log::read(&dir).unwrap())).unwrap();
            })),
        ))
        .unwrap();
    let finished = spawn_run(session);
    let (result, at_ack) =
        Deadline::after(DEADLINE).recv_or_fail(&answered, "notes request acknowledged");
    session
        .inbox
        .send(Delivery::Close(support::ignore()))
        .unwrap();
    let ran = Deadline::after(DEADLINE).recv_or_fail(&finished, "notes session closed");
    assert!(ran.is_ok(), "{ran:?}");
    (result, at_ack)
}

fn kinds(lines: &[contract::Envelope]) -> Vec<&str> {
    lines.iter().map(|line| line.kind.as_str()).collect()
}

#[test]
fn read_only_notes_render_an_eligible_entry_without_a_request_or_usage() {
    let mut session = Session::new(vec![Scripted::text("unused")], None);
    let mut ask = read_only_ask();
    let files = Evidence::Files {
        text: "our remote".into(),
    };
    ask.evidence = vec![files.clone(), tally("acme-cli", "acme-cli query --json")];
    let (result, at_ack) = run_read_only(&mut session, ask);
    let Ok(Some(CommandResult::ProposeNotes {
        evidence,
        proposed,
        error,
    })) = result
    else {
        panic!("read-only notes returned the wrong result: {result:?}");
    };
    assert!(error.is_none());
    assert_eq!(
        evidence,
        vec![files, tally("acme-cli", "acme-cli query --json")]
    );
    assert_eq!(proposed.len(), 1);
    assert_eq!(
        proposed[0].text(),
        "shell.read_only.\"acme-cli query\".flags = [\"--json\"]"
    );
    assert_eq!(proposed[0].cites(), [1]);
    assert_eq!(kinds(&at_ack), ["session_started"]);
    assert!(session.requests().is_empty());
}

#[test]
fn read_only_notes_with_an_answer_are_invalid_arguments() {
    let mut session = Session::new(vec![Scripted::text("unused")], None);
    let mut ask = read_only_ask();
    ask.args.answer = Some("my answer".into());
    let files = Evidence::Files {
        text: "our remote".into(),
    };
    ask.evidence = vec![files.clone()];
    let (result, at_ack) = run_read_only(&mut session, ask);
    let Ok(Some(CommandResult::ProposeNotes {
        evidence,
        proposed,
        error,
    })) = result
    else {
        panic!("an answered read-only request is rejected: {result:?}");
    };
    assert!(proposed.is_empty());
    let error = error.expect("invalid arguments are reported");
    assert_eq!(error.code, ErrorCode::InvalidArguments);
    assert_eq!(error.message, "read-only notes take no answer");
    assert_eq!(evidence, vec![files]);
    assert_eq!(kinds(&at_ack), ["session_started"]);
    assert!(session.requests().is_empty());
}

#[test]
fn read_only_notes_with_earlier_lines_are_invalid_arguments() {
    let mut session = Session::new(vec![Scripted::text("unused")], None);
    let mut ask = read_only_ask();
    ask.args.earlier = vec!["keep this".into()];
    let files = Evidence::Files {
        text: "our remote".into(),
    };
    ask.evidence = vec![files.clone()];
    let (result, at_ack) = run_read_only(&mut session, ask);
    let Ok(Some(CommandResult::ProposeNotes {
        evidence,
        proposed,
        error,
    })) = result
    else {
        panic!("a read-only request with earlier lines is rejected: {result:?}");
    };
    assert!(proposed.is_empty());
    let error = error.expect("invalid arguments are reported");
    assert_eq!(error.code, ErrorCode::InvalidArguments);
    assert_eq!(error.message, "read-only notes take no earlier lines");
    assert_eq!(evidence, vec![files]);
    assert_eq!(kinds(&at_ack), ["session_started"]);
    assert!(session.requests().is_empty());
}

#[test]
fn read_only_notes_with_empty_earlier_are_accepted_as_omitted() {
    let mut session = Session::new(vec![Scripted::text("unused")], None);
    let mut ask = read_only_ask();
    ask.args.earlier = Vec::new();
    ask.evidence = vec![tally("acme-cli", "acme-cli query --json")];
    let (result, at_ack) = run_read_only(&mut session, ask);
    let Ok(Some(CommandResult::ProposeNotes {
        proposed, error, ..
    })) = result
    else {
        panic!("empty earlier is the omitted value: {result:?}");
    };
    assert!(error.is_none());
    assert_eq!(proposed.len(), 1);
    assert_eq!(kinds(&at_ack), ["session_started"]);
    assert!(session.requests().is_empty());
}

#[test]
fn read_only_notes_withhold_an_ambiguous_tally() {
    let mut session = Session::new(vec![Scripted::text("unused")], None);
    let mut ask = read_only_ask();
    ask.evidence = vec![tally("kubectl", "kubectl -n x get pods")];
    let (result, at_ack) = run_read_only(&mut session, ask);
    let Ok(Some(CommandResult::ProposeNotes {
        evidence,
        proposed,
        error,
    })) = result
    else {
        panic!("an ambiguous tally withholds its entry: {result:?}");
    };
    assert!(error.is_none());
    assert!(proposed.is_empty());
    assert_eq!(
        evidence,
        vec![withheld(
            "kubectl -n x get pods",
            "words after the command: an entry would cover other subcommands"
        )]
    );
    assert_eq!(kinds(&at_ack), ["session_started"]);
    assert!(session.requests().is_empty());
}

fn ambiguous_withheld(mut request: NotesAsk, subject: &str, subject_text: &str) {
    let mut session = Session::new(vec![Scripted::text("unused")], None);
    request.evidence = vec![tally("kubectl", subject)];
    let (result, _) = run_read_only(&mut session, request);
    let Ok(Some(CommandResult::ProposeNotes {
        evidence,
        proposed,
        error,
    })) = result
    else {
        panic!("{subject_text} withholds its entry: {result:?}");
    };
    assert!(error.is_none());
    assert!(proposed.is_empty());
    assert_eq!(
        evidence,
        vec![withheld(
            subject,
            "words after the command: an entry would cover other subcommands"
        )]
    );
}

#[test]
fn read_only_notes_withhold_a_non_flag_word_after_the_subcommand() {
    // Only the left side of the disjunction holds: the word is not a
    // flag and holds neither `<` nor `>`.
    let ask = read_only_ask();
    ambiguous_withheld(
        ask,
        "kubectl -n x get pods",
        "a non-flag word after the subcommand",
    );
}

#[test]
fn read_only_notes_withhold_a_flag_with_only_a_left_angle_after_the_subcommand() {
    // No later non-flag word masks the angle check; only the left angle
    // condition holds.
    let ask = read_only_ask();
    ambiguous_withheld(
        ask,
        "kubectl get -n --json<",
        "a flag holding `<` after the subcommand",
    );
}

#[test]
fn read_only_notes_withhold_a_flag_with_only_a_right_angle_after_the_subcommand() {
    // No later non-flag word masks the angle check; only the right angle
    // condition holds.
    let ask = read_only_ask();
    ambiguous_withheld(
        ask,
        "kubectl get -n --json>",
        "a flag holding `>` after the subcommand",
    );
}

#[test]
fn read_only_notes_withhold_a_flag_with_a_left_angle_after_the_subcommand() {
    // Only the middle side of the disjunction holds: the word is a
    // flag and holds `<` but not `>`.
    let ask = read_only_ask();
    ambiguous_withheld(
        ask,
        "kubectl -n x --json< pods",
        "a flag holding `<` after the subcommand",
    );
}

#[test]
fn read_only_notes_withhold_a_flag_with_a_right_angle_after_the_subcommand() {
    // Only the right side of the disjunction holds: the word is a
    // flag and holds `>` but not `<`.
    let ask = read_only_ask();
    ambiguous_withheld(
        ask,
        "kubectl -n x --json> pods",
        "a flag holding `>` after the subcommand",
    );
}

#[test]
fn read_only_notes_withhold_a_subcommand_holding_a_left_angle() {
    // Only the left side of the disjunction holds: the subcommand
    // holds `<` but not `>`.
    let ask = read_only_ask();
    ambiguous_withheld(ask, "kubectl se<d x", "a subcommand holding `<`");
}

#[test]
fn read_only_notes_withhold_a_subcommand_holding_a_right_angle() {
    // Only the right side of the disjunction holds: the subcommand
    // holds `>` but not `<`.
    let ask = read_only_ask();
    ambiguous_withheld(ask, "kubectl se>d x", "a subcommand holding `>`");
}

#[test]
fn read_only_notes_withhold_a_one_word_tally() {
    let mut session = Session::new(vec![Scripted::text("unused")], None);
    let mut ask = read_only_ask();
    ask.evidence = vec![tally("acme-cli", "acme-cli --json")];
    let (result, _) = run_read_only(&mut session, ask);
    let Ok(Some(CommandResult::ProposeNotes {
        evidence,
        proposed,
        error,
    })) = result
    else {
        panic!("a one-word tally withholds its entry: {result:?}");
    };
    assert!(error.is_none());
    assert!(proposed.is_empty());
    assert_eq!(
        evidence,
        vec![withheld(
            "acme-cli --json",
            "one word: an entry names a subcommand"
        )]
    );
}

#[test]
fn read_only_notes_withhold_a_valued_flag_tally() {
    let mut session = Session::new(vec![Scripted::text("unused")], None);
    let mut ask = read_only_ask();
    ask.evidence = vec![tally("git", "git log --exec=ls")];
    let (result, _) = run_read_only(&mut session, ask);
    let Ok(Some(CommandResult::ProposeNotes {
        evidence,
        proposed,
        error,
    })) = result
    else {
        panic!("a valued flag withholds its entry: {result:?}");
    };
    assert!(error.is_none());
    assert!(proposed.is_empty());
    assert_eq!(
        evidence,
        vec![withheld(
            "git log --exec=ls",
            "takes a value: an entry can't limit it"
        )]
    );
}

#[test]
fn read_only_notes_withhold_a_slash_word_tally() {
    let mut session = Session::new(vec![Scripted::text("unused")], None);
    let mut ask = read_only_ask();
    ask.evidence = vec![tally("./scripts/q", "./scripts/q --json")];
    let (result, _) = run_read_only(&mut session, ask);
    let Ok(Some(CommandResult::ProposeNotes {
        evidence,
        proposed,
        error,
    })) = result
    else {
        panic!("a slash word withholds its entry: {result:?}");
    };
    assert!(error.is_none());
    assert!(proposed.is_empty());
    assert_eq!(
        evidence,
        vec![withheld(
            "./scripts/q --json",
            "a path, not a command name: an entry names a command"
        )]
    );
}

#[test]
fn read_only_notes_withhold_a_covered_tally() {
    let mut session = Session::new(vec![Scripted::text("unused")], None);
    let mut ask = read_only_ask();
    let covered = tally("acme-cli", "acme-cli query --json");
    let Evidence::Allows(tally) = &covered else {
        panic!("a tally is built");
    };
    ask.evidence = vec![Evidence::Allows(tally.clone().with_reason(
        "not proposed: a call it would cover was denied, blocked or escalated",
    ))];
    let (result, _) = run_read_only(&mut session, ask);
    let Ok(Some(CommandResult::ProposeNotes {
        evidence,
        proposed,
        error,
    })) = result
    else {
        panic!("a covered tally withholds its entry: {result:?}");
    };
    assert!(error.is_none());
    assert!(proposed.is_empty());
    assert_eq!(
        evidence,
        vec![withheld(
            "acme-cli query --json",
            "not proposed: a call it would cover was denied, blocked or escalated"
        )]
    );
}

#[test]
fn read_only_notes_withhold_a_mixed_prefix_group_with_one_reason() {
    let mut session = Session::new(vec![Scripted::text("unused")], None);
    let mut ask = read_only_ask();
    ask.evidence = vec![
        tally("kubectl", "kubectl get --json"),
        tally("kubectl", "kubectl -n x get pods"),
    ];
    let (result, _) = run_read_only(&mut session, ask);
    let Ok(Some(CommandResult::ProposeNotes {
        evidence,
        proposed,
        error,
    })) = result
    else {
        panic!("a mixed group withholds its entry: {result:?}");
    };
    assert!(error.is_none());
    assert!(proposed.is_empty());
    assert_eq!(
        evidence,
        vec![
            withheld(
                "kubectl get --json",
                "words after the command: an entry would cover other subcommands"
            ),
            withheld(
                "kubectl -n x get pods",
                "words after the command: an entry would cover other subcommands"
            ),
        ]
    );
}

#[test]
fn read_only_notes_render_an_allowed_redirect_subject() {
    let mut session = Session::new(vec![Scripted::text("unused")], None);
    let mut ask = read_only_ask();
    ask.evidence = vec![tally("acme-cli query", "acme-cli query --json >/dev/null")];
    let (result, _) = run_read_only(&mut session, ask);
    let Ok(Some(CommandResult::ProposeNotes {
        proposed, error, ..
    })) = result
    else {
        panic!("an allowed redirect still renders: {result:?}");
    };
    assert!(error.is_none());
    assert_eq!(proposed.len(), 1);
    assert_eq!(
        proposed[0].text(),
        "shell.read_only.\"acme-cli query\".flags = [\"--json\"]"
    );
    assert_eq!(proposed[0].cites(), [0]);
}

#[test]
fn read_only_notes_union_flags_and_cite_the_whole_group() {
    let mut session = Session::new(vec![Scripted::text("unused")], None);
    let mut ask = read_only_ask();
    let files = Evidence::Files {
        text: "our remote".into(),
    };
    ask.evidence = vec![
        files,
        tally("acme-cli query", "acme-cli query --json"),
        tally("acme-cli query", "acme-cli query --yaml"),
    ];
    let (result, _) = run_read_only(&mut session, ask);
    let Ok(Some(CommandResult::ProposeNotes {
        proposed, error, ..
    })) = result
    else {
        panic!("an eligible group renders one entry: {result:?}");
    };
    assert!(error.is_none());
    assert_eq!(proposed.len(), 1);
    assert_eq!(
        proposed[0].text(),
        "shell.read_only.\"acme-cli query\".flags = [\"--json\", \"--yaml\"]"
    );
    assert_eq!(proposed[0].cites(), [1, 2]);
}

#[test]
fn read_only_notes_withhold_divergent_subcommands() {
    let mut session = Session::new(vec![Scripted::text("unused")], None);
    let mut ask = read_only_ask();
    ask.evidence = vec![
        tally("acme-cli", "acme-cli query --json"),
        tally("acme-cli", "acme-cli push --json"),
    ];
    let (result, _) = run_read_only(&mut session, ask);
    let Ok(Some(CommandResult::ProposeNotes {
        evidence,
        proposed,
        error,
    })) = result
    else {
        panic!("divergent subcommands withhold the entry: {result:?}");
    };
    assert!(error.is_none());
    assert!(proposed.is_empty());
    assert_eq!(
        evidence,
        vec![
            withheld(
                "acme-cli query --json",
                "words after the command: an entry would cover other subcommands"
            ),
            withheld(
                "acme-cli push --json",
                "words after the command: an entry would cover other subcommands"
            ),
        ]
    );
}

#[test]
fn read_only_notes_mid_turn_are_busy() {
    let (ack, answers) = capture();
    let mut session = Session::injecting(
        vec![Scripted::text("Done.")],
        vec![Delivery::ProposeNotes(read_only_ask(), ack)],
    );
    session.inbox.send(support::delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(take(&answers), support::rejected(ErrorCode::Busy, BUSY));
    assert_eq!(session.requests().len(), 1);
}

#[test]
fn read_only_notes_while_closing_are_closing() {
    let mut session = Session::new(vec![Scripted::text("Hi.")], None);
    let (ack, answers) = capture();
    session
        .inbox
        .send(Delivery::Close(support::ignore()))
        .unwrap();
    session
        .inbox
        .send(Delivery::ProposeNotes(read_only_ask(), ack))
        .unwrap();
    let finished = spawn_run(&mut session);
    assert_eq!(
        take(&answers),
        support::rejected(ErrorCode::Closing, CLOSING)
    );
    let ran = Deadline::after(DEADLINE).recv_or_fail(&finished, "notes session closed");
    assert!(ran.is_ok(), "{ran:?}");
}
