//! The setup's state machine (`docs/permissions.md`, "Setting up the
//! notes"), driven without an `App`: keys, pastes and answers in, intents
//! out.

use std::path::PathBuf;

use contract::ErrorCode;
use contract::notes::{Evidence, ProposeNotesArgs, Proposed, SessionTally, Topic};
use contract::shapes::Failure;

use super::template::{DiffRow, NotesLayer};
use super::{Act, LineState, Screen, Setup};
use crate::configure::{
    ConfigureError, FileOutcome, FileSaved, NotesFile, NotesSaved, NotesWrite, ReviewerNotes,
};
use crate::keys::Key;

// -------------------------------------------------------------- builders

fn notes(global: &str, project: &str) -> ReviewerNotes {
    ReviewerNotes {
        global: NotesFile {
            file: PathBuf::from("/home/fiber/config.json"),
            text: global.to_string(),
            version: "global-v1".to_string(),
        },
        project: NotesFile {
            file: PathBuf::from("/home/fiber/projects/p/config.json"),
            text: project.to_string(),
            version: "project-v1".to_string(),
        },
    }
}

fn files(text: &str) -> Evidence {
    Evidence::Files {
        text: text.to_string(),
    }
}

fn answer(text: &str) -> Evidence {
    Evidence::Answer {
        text: text.to_string(),
    }
}

fn session(prefix: &str, count: u32, here: u32) -> Evidence {
    Evidence::Session(SessionTally::new(prefix.to_string(), count, here).expect("a valid tally"))
}

fn line(text: &str, cites: &[usize]) -> Proposed {
    Proposed::new(text.to_string(), cites.to_vec()).expect("a cited line")
}

fn failure(message: &str) -> Failure {
    Failure {
        code: ErrorCode::ConfigInvalid,
        message: message.to_string(),
        retry_after_ms: None,
        provider: None,
    }
}

fn error(message: &str) -> ConfigureError {
    ConfigureError {
        code: ErrorCode::ConfigInvalid,
        message: message.to_string(),
    }
}

fn press(setup: &mut Setup, key: Key) -> Act {
    setup.key(&key)
}

fn typed(setup: &mut Setup, text: &str) {
    for c in text.chars() {
        setup.key(&Key::Char(c));
    }
}

/// Enter on the question, and the answer that gives `proposed` over
/// `evidence`.
fn ask(setup: &mut Setup, evidence: &[Evidence], proposed: &[Proposed]) {
    assert!(matches!(press(setup, Key::Enter), Act::Propose(_)));
    setup.proposed(evidence, proposed, None);
    assert_eq!(setup.screen(), Screen::Lines);
}

/// Opens on `notes` and asks topic 1, which gives `proposed`.
fn first_lines(notes: ReviewerNotes, evidence: &[Evidence], proposed: &[Proposed]) -> Setup {
    let mut setup = Setup::open(notes);
    ask(&mut setup, evidence, proposed);
    setup
}

/// Gets past the current topic with no lines.
fn skip(setup: &mut Setup) {
    ask(setup, &[], &[]);
    assert_eq!(press(setup, Key::Enter), Act::None);
}

fn texts(setup: &Setup) -> Vec<String> {
    setup
        .line_rows()
        .iter()
        .map(|row| row.text.to_string())
        .collect()
}

fn focused(setup: &Setup) -> Option<String> {
    setup
        .line_rows()
        .iter()
        .find(|row| row.focused)
        .map(|row| row.text.to_string())
}

fn states(setup: &Setup) -> Vec<LineState> {
    setup.line_rows().iter().map(|row| row.state).collect()
}

fn save_of(act: Act) -> (Option<NotesWrite>, Option<NotesWrite>) {
    let Act::Save { global, project } = act else {
        panic!("not a save: {act:?}");
    };
    (global, project)
}

/// Walks every remaining topic with no lines, to the diff.
fn finish_to_diff(setup: &mut Setup) {
    for _ in Topic::ALL {
        if setup.screen() == Screen::Diff {
            break;
        }
        skip(setup);
    }
    assert_eq!(setup.screen(), Screen::Diff);
}

// ----------------------------------------------------------- the question

#[test]
fn opening_shows_topic_one_with_both_layers_read() {
    let setup = Setup::open(notes("", ""));
    assert_eq!(setup.screen(), Screen::Question);
    assert_eq!(setup.topic(), Topic::SourceControl);
    assert_eq!(setup.number(), 1);
    assert_eq!(setup.answer(), "");
    assert_eq!(setup.error(), None);
}

#[test]
fn the_topics_run_in_the_order_the_contract_gives_to_the_diff() {
    let mut setup = Setup::open(notes("", ""));
    let mut seen = Vec::new();
    for _ in Topic::ALL {
        assert_eq!(setup.screen(), Screen::Question);
        seen.push(setup.topic());
        assert_eq!(setup.number(), seen.len());
        skip(&mut setup);
    }
    assert_eq!(seen, Topic::ALL);
    assert_eq!(setup.screen(), Screen::Diff);
}

#[test]
fn typing_backspace_and_a_multi_line_paste_fill_the_field() {
    let mut setup = Setup::open(notes("", ""));
    typed(&mut setup, "abcd");
    press(&mut setup, Key::Backspace);
    assert_eq!(setup.answer(), "abc");
    setup.paste("x\ny\r\nz");
    assert_eq!(setup.answer(), "abcx y  z");
    for _ in 0..20 {
        press(&mut setup, Key::Backspace);
    }
    assert_eq!(setup.answer(), "");
}

#[test]
fn enter_sends_the_answer_only_when_it_holds_more_than_whitespace() {
    let mut setup = Setup::open(notes("", ""));
    typed(&mut setup, " \t ");
    let Act::Propose(args) = press(&mut setup, Key::Enter) else {
        panic!("no request");
    };
    assert_eq!(args.answer, None);
    assert_eq!(args.topic, Topic::SourceControl);
    assert_eq!(setup.screen(), Screen::Waiting);

    let mut setup = Setup::open(notes("", ""));
    typed(&mut setup, "  acme ");
    let Act::Propose(args) = press(&mut setup, Key::Enter) else {
        panic!("no request");
    };
    assert_eq!(
        args.answer.as_deref(),
        Some("  acme "),
        "the answer is not trimmed"
    );
}

#[test]
fn an_empty_answer_sends_no_answer_key() {
    let mut setup = Setup::open(notes("", ""));
    let Act::Propose(args) = press(&mut setup, Key::Enter) else {
        panic!("no request");
    };
    assert_eq!(
        args,
        ProposeNotesArgs {
            topic: Topic::SourceControl,
            answer: None,
            earlier: Vec::new(),
        }
    );
}

#[test]
fn the_request_carries_the_topics_bullets_from_both_layers_global_first() {
    let global = "### Source control\n- g1\n- shared\n### Internal hosts\n- other topic\n";
    let project = "### Source control\n-  p1  \n- shared\n- \n";
    let mut setup = Setup::open(notes(global, project));
    let Act::Propose(args) = press(&mut setup, Key::Enter) else {
        panic!("no request");
    };
    assert_eq!(args.earlier, ["g1", "shared", "p1"]);
}

/// A layer holds a line when it lists that text under the topic, not when
/// it lists the topic, or the text under another topic.
#[test]
fn a_line_struck_where_it_is_held_is_left_out_though_the_other_layer_has_the_topic() {
    let global = "### Source control\n- mine\n";
    let project = "### Source control\n- other\n### Internal hosts\n- mine\n";
    let mut setup = Setup::open(notes(global, project));
    ask(&mut setup, &[], &[]);
    assert_eq!(focused(&setup).as_deref(), Some("mine"));
    press(&mut setup, Key::Char('x'));
    // The next topic's question, then back to this topic and its question.
    press(&mut setup, Key::Esc);
    press(&mut setup, Key::Esc);
    assert_eq!(setup.screen(), Screen::Question);
    assert_eq!(setup.topic(), Topic::SourceControl);
    let Act::Propose(args) = press(&mut setup, Key::Enter) else {
        panic!("no request");
    };
    assert_eq!(args.earlier, ["other"]);
}

#[test]
fn enter_while_waiting_sends_nothing_and_every_key_but_esc_is_swallowed() {
    let mut setup = Setup::open(notes("", ""));
    typed(&mut setup, "acme");
    assert!(matches!(press(&mut setup, Key::Enter), Act::Propose(_)));
    for key in [
        Key::Enter,
        Key::Char('x'),
        Key::Tab,
        Key::Up,
        Key::Backspace,
        Key::PageDown,
    ] {
        assert_eq!(press(&mut setup, key), Act::None);
        assert_eq!(setup.screen(), Screen::Waiting);
    }
    assert_eq!(setup.answer(), "acme");
    setup.paste("ignored");
    assert_eq!(setup.answer(), "acme");
}

#[test]
fn esc_while_waiting_abandons_the_request_and_keeps_the_answer() {
    let mut setup = Setup::open(notes("", ""));
    typed(&mut setup, "acme");
    press(&mut setup, Key::Enter);
    assert_eq!(press(&mut setup, Key::Esc), Act::Abandon);
    assert_eq!(setup.screen(), Screen::Question);
    assert_eq!(setup.answer(), "acme");
    // An answer arriving now is not for a waiting request.
    setup.proposed(&[files("f")], &[line("x", &[0])], None);
    assert_eq!(setup.screen(), Screen::Question);
}

#[test]
fn esc_on_topic_one_closes() {
    let mut setup = Setup::open(notes("", ""));
    assert_eq!(press(&mut setup, Key::Esc), Act::Close);
}

#[test]
fn esc_on_a_later_question_returns_to_the_previous_topics_lines_with_decisions_kept() {
    let mut setup = first_lines(notes("", ""), &[files("f")], &[line("a", &[0])]);
    assert_eq!(press(&mut setup, Key::Enter), Act::None);
    assert_eq!(setup.screen(), Screen::Question);
    assert_eq!(setup.topic(), Topic::InternalHosts);
    assert_eq!(press(&mut setup, Key::Esc), Act::None);
    assert_eq!(setup.screen(), Screen::Lines);
    assert_eq!(setup.topic(), Topic::SourceControl);
    assert_eq!(texts(&setup), ["a"]);
    assert_eq!(states(&setup), [LineState::Kept]);
}

#[test]
fn a_paste_goes_nowhere_off_a_field() {
    let mut setup = first_lines(notes("", ""), &[files("f")], &[line("a", &[0])]);
    setup.paste("not in a field");
    assert_eq!(texts(&setup), ["a"]);
    assert_eq!(setup.answer(), "");
    press(&mut setup, Key::Enter);
    finish_to_diff(&mut setup);
    setup.paste("not in a field");
    assert_eq!(setup.screen(), Screen::Diff);
}

#[test]
fn a_failed_answer_shows_its_message_and_the_field_stays_editable() {
    let mut setup = Setup::open(notes("", ""));
    typed(&mut setup, "ab");
    press(&mut setup, Key::Enter);
    setup.proposed(&[], &[], Some(&failure("the model call failed")));
    assert_eq!(setup.screen(), Screen::Question);
    assert_eq!(setup.error(), Some("the model call failed"));
    typed(&mut setup, "c");
    assert_eq!(setup.answer(), "abc");
    // Sending again clears the message.
    press(&mut setup, Key::Enter);
    assert_eq!(setup.error(), None);
}

#[test]
fn a_refusal_reopens_the_question_with_its_message_and_the_answer() {
    let mut setup = Setup::open(notes("", ""));
    typed(&mut setup, "ab");
    press(&mut setup, Key::Enter);
    setup.refused("A turn is running.");
    assert_eq!(setup.screen(), Screen::Question);
    assert_eq!(setup.error(), Some("A turn is running."));
    assert_eq!(setup.answer(), "ab");
}

#[test]
fn a_refusal_changes_nothing_off_the_question_and_the_waiting_screens() {
    let mut setup = first_lines(notes("", ""), &[files("f")], &[line("a", &[0])]);
    setup.refused("A turn is running.");
    assert_eq!(setup.screen(), Screen::Lines);
    assert_eq!(setup.error(), None);
}

#[test]
fn lost_with_a_request_waiting_reopens_the_question_with_the_restart_row() {
    let mut setup = Setup::open(notes("", ""));
    typed(&mut setup, "acme");
    press(&mut setup, Key::Enter);
    setup.lost();
    assert_eq!(setup.screen(), Screen::Question);
    assert_eq!(
        setup.error(),
        Some("The session restarted. Enter proposes again.")
    );
    assert_eq!(setup.answer(), "acme");
}

#[test]
fn lost_with_nothing_waiting_changes_nothing() {
    let mut setup = Setup::open(notes("", ""));
    typed(&mut setup, "acme");
    setup.lost();
    assert_eq!(setup.screen(), Screen::Question);
    assert_eq!(setup.error(), None);
    let mut setup = first_lines(notes("", ""), &[files("f")], &[line("a", &[0])]);
    setup.lost();
    assert_eq!(setup.screen(), Screen::Lines);
    press(&mut setup, Key::Enter);
    finish_to_diff(&mut setup);
    setup.lost();
    assert_eq!(setup.screen(), Screen::Diff);
}

// ------------------------------------------------------------- the lines

fn layers(setup: &Setup) -> Vec<NotesLayer> {
    setup.line_rows().iter().map(|row| row.layer).collect()
}

#[test]
fn each_proposal_starts_on_the_layer_its_evidence_gives() {
    let evidence = [
        files("a remote"),
        answer("my answer"),
        session("gh pr merge", 4, 4),
        session("cargo publish", 3, 1),
    ];
    let proposed = [
        line("from files", &[0]),
        line("from the answer", &[1]),
        line("all here", &[2]),
        line("not all here", &[3]),
        line("two tallies, one all here", &[2, 3]),
        line("two tallies, both all here", &[2, 2]),
    ];
    let setup = first_lines(notes("", ""), &evidence, &proposed);
    assert_eq!(
        layers(&setup),
        [
            NotesLayer::PerProject,
            NotesLayer::Global,
            NotesLayer::PerProject,
            NotesLayer::Global,
            NotesLayer::Global,
            NotesLayer::PerProject,
        ]
    );
}

#[test]
fn a_cite_out_of_range_beside_an_in_range_one_is_ignored() {
    let setup = first_lines(
        notes("", ""),
        &[files("a remote")],
        &[line("kept", &[0, 7])],
    );
    assert_eq!(texts(&setup), ["kept"]);
    assert_eq!(layers(&setup), [NotesLayer::PerProject]);
    assert_eq!(setup.evidence(), ["a remote"]);
}

#[test]
fn a_proposal_with_every_cite_out_of_range_is_dropped() {
    let setup = first_lines(
        notes("", ""),
        &[files("a remote")],
        &[line("dropped", &[3, 4]), line("kept", &[0])],
    );
    assert_eq!(texts(&setup), ["kept"]);
}

#[test]
fn a_proposal_citing_two_kinds_is_dropped() {
    let setup = first_lines(
        notes("", ""),
        &[files("a remote"), answer("mine")],
        &[line("mixed", &[0, 1]), line("kept", &[1])],
    );
    assert_eq!(texts(&setup), ["kept"]);
}

#[test]
fn evidence_is_the_cited_items_only() {
    let setup = first_lines(
        notes("", ""),
        &[files("first"), files("never cited"), answer("third")],
        &[line("a", &[0]), line("b", &[2])],
    );
    assert_eq!(setup.evidence(), ["first", "third"]);
}

#[test]
fn zero_width_and_bidirectional_characters_are_stripped_from_what_is_stored() {
    let setup = first_lines(
        notes("", ""),
        &[files("re\u{200b}mote\u{202e}")],
        &[line("ac\u{200b}me\u{202e} org", &[0])],
    );
    assert_eq!(texts(&setup), ["acme org"]);
    assert_eq!(setup.evidence(), ["remote"]);
}

#[test]
fn a_proposal_that_cleans_to_nothing_is_dropped() {
    let setup = first_lines(
        notes("", ""),
        &[files("f")],
        &[line("\u{200b}\u{202e}", &[0]), line("kept", &[0])],
    );
    assert_eq!(texts(&setup), ["kept"]);
}

#[test]
fn the_first_open_proposal_has_the_focus() {
    let notes = notes("### Source control\n- earlier\n", "");
    let setup = first_lines(notes, &[files("f")], &[line("a", &[0]), line("b", &[0])]);
    assert_eq!(texts(&setup), ["earlier", "a", "b"]);
    assert_eq!(focused(&setup).as_deref(), Some("a"));
}

#[test]
fn earlier_lines_come_first_start_kept_and_never_flip() {
    let notes = notes(
        "### Source control\n- from global\n",
        "### Source control\n- from project\n",
    );
    let mut setup = first_lines(notes, &[files("f")], &[line("new", &[0])]);
    let rows = setup.line_rows();
    assert_eq!(
        rows.iter()
            .map(|r| (r.earlier, r.layer, r.state))
            .collect::<Vec<_>>(),
        [
            (true, NotesLayer::Global, LineState::Kept),
            (true, NotesLayer::PerProject, LineState::Kept),
            (false, NotesLayer::PerProject, LineState::Open),
        ]
    );
    // The focus is on the proposal; move it to an earlier line and press Tab.
    press(&mut setup, Key::Up);
    press(&mut setup, Key::Up);
    assert_eq!(focused(&setup).as_deref(), Some("from global"));
    press(&mut setup, Key::Tab);
    assert_eq!(layers(&setup)[0], NotesLayer::Global);
    assert!(setup.line_rows().iter().all(|r| r.flipped_from.is_none()));
}

#[test]
fn up_and_down_move_the_focus_and_stop_at_both_ends() {
    let mut setup = first_lines(
        notes("", ""),
        &[files("f")],
        &[line("a", &[0]), line("b", &[0]), line("c", &[0])],
    );
    assert_eq!(focused(&setup).as_deref(), Some("a"));
    press(&mut setup, Key::Up);
    assert_eq!(focused(&setup).as_deref(), Some("a"));
    press(&mut setup, Key::Down);
    press(&mut setup, Key::Down);
    assert_eq!(focused(&setup).as_deref(), Some("c"));
    press(&mut setup, Key::Down);
    assert_eq!(focused(&setup).as_deref(), Some("c"));
    press(&mut setup, Key::Up);
    assert_eq!(focused(&setup).as_deref(), Some("b"));
}

#[test]
fn tab_flips_the_layer_both_ways_and_the_note_shows_only_while_it_differs() {
    let mut setup = first_lines(notes("", ""), &[files("f")], &[line("a", &[0])]);
    let flipped = |setup: &Setup| setup.line_rows()[0].flipped_from;
    assert_eq!(layers(&setup), [NotesLayer::PerProject]);
    assert_eq!(flipped(&setup), None);
    press(&mut setup, Key::Tab);
    assert_eq!(layers(&setup), [NotesLayer::Global]);
    assert_eq!(flipped(&setup), Some(NotesLayer::PerProject));
    press(&mut setup, Key::Tab);
    assert_eq!(layers(&setup), [NotesLayer::PerProject]);
    assert_eq!(flipped(&setup), None);
}

#[test]
fn enter_confirms_the_focused_proposal() {
    let mut setup = first_lines(
        notes("", ""),
        &[files("f")],
        &[line("a", &[0]), line("b", &[0])],
    );
    press(&mut setup, Key::Enter);
    assert_eq!(states(&setup), [LineState::Kept, LineState::Open]);
}

#[test]
fn x_strikes_the_focused_proposal() {
    let mut setup = first_lines(
        notes("", ""),
        &[files("f")],
        &[line("a", &[0]), line("b", &[0])],
    );
    press(&mut setup, Key::Char('x'));
    assert_eq!(states(&setup), [LineState::Struck, LineState::Open]);
}

#[test]
fn enter_on_a_struck_proposal_keeps_it() {
    let mut setup = first_lines(
        notes("", ""),
        &[files("f")],
        &[line("a", &[0]), line("b", &[0])],
    );
    press(&mut setup, Key::Char('x'));
    // The focus moved to b; go back to the struck line.
    press(&mut setup, Key::Up);
    assert_eq!(focused(&setup).as_deref(), Some("a"));
    press(&mut setup, Key::Enter);
    assert_eq!(states(&setup), [LineState::Kept, LineState::Open]);
}

#[test]
fn x_on_an_earlier_line_strikes_it_and_enter_on_the_struck_line_keeps_it() {
    let notes = notes("", "### Source control\n- earlier\n");
    let mut setup = first_lines(notes, &[files("f")], &[line("a", &[0])]);
    press(&mut setup, Key::Up);
    assert_eq!(focused(&setup).as_deref(), Some("earlier"));
    press(&mut setup, Key::Char('x'));
    assert_eq!(states(&setup)[0], LineState::Struck);
    press(&mut setup, Key::Up);
    press(&mut setup, Key::Enter);
    assert_eq!(states(&setup)[0], LineState::Kept);
}

#[test]
fn a_decision_moves_to_the_next_undecided_line_below_wrapping_to_above() {
    let mut setup = first_lines(
        notes("", ""),
        &[files("f")],
        &[
            line("a", &[0]),
            line("b", &[0]),
            line("c", &[0]),
            line("d", &[0]),
        ],
    );
    // Decide c first: the next open one below is d.
    press(&mut setup, Key::Down);
    press(&mut setup, Key::Down);
    press(&mut setup, Key::Enter);
    assert_eq!(focused(&setup).as_deref(), Some("d"));
    // Decide d: nothing below is open, so wrap to a.
    press(&mut setup, Key::Char('x'));
    assert_eq!(focused(&setup).as_deref(), Some("a"));
    // Decide a: b is below it.
    press(&mut setup, Key::Enter);
    assert_eq!(focused(&setup).as_deref(), Some("b"));
    // Decide b: none left open, the next topic's question opens.
    assert_eq!(press(&mut setup, Key::Enter), Act::None);
    assert_eq!(setup.screen(), Screen::Question);
    assert_eq!(setup.topic(), Topic::InternalHosts);
}

#[test]
fn a_wrap_skips_a_decided_line_above() {
    let mut setup = first_lines(
        notes("", ""),
        &[files("f")],
        &[line("a", &[0]), line("b", &[0]), line("c", &[0])],
    );
    press(&mut setup, Key::Enter);
    assert_eq!(focused(&setup).as_deref(), Some("b"));
    // Decide c out of order, then b: a is decided, so the topic is done.
    press(&mut setup, Key::Down);
    press(&mut setup, Key::Enter);
    assert_eq!(focused(&setup).as_deref(), Some("b"));
    press(&mut setup, Key::Enter);
    assert_eq!(setup.screen(), Screen::Question);
}

#[test]
fn after_topic_five_the_diff_opens() {
    let mut setup = Setup::open(notes("", ""));
    for _ in 0..4 {
        skip(&mut setup);
    }
    assert_eq!(setup.topic(), Topic::Never);
    ask(&mut setup, &[files("f")], &[line("a", &[0])]);
    assert_eq!(press(&mut setup, Key::Enter), Act::None);
    assert_eq!(setup.screen(), Screen::Diff);
}

#[test]
fn a_topic_with_nothing_to_show_has_no_rows_and_enter_opens_the_next_topic() {
    let mut setup = Setup::open(notes("", ""));
    ask(&mut setup, &[], &[]);
    assert!(setup.line_rows().is_empty());
    assert_eq!(press(&mut setup, Key::Char('x')), Act::None);
    assert_eq!(setup.screen(), Screen::Lines);
    assert_eq!(press(&mut setup, Key::Enter), Act::None);
    assert_eq!(setup.screen(), Screen::Question);
    assert_eq!(setup.topic(), Topic::InternalHosts);
}

#[test]
fn a_topic_with_only_earlier_lines_moves_on_after_the_last_decision() {
    let notes = notes("", "### Source control\n- earlier\n");
    let mut setup = first_lines(notes, &[], &[]);
    assert_eq!(texts(&setup), ["earlier"]);
    assert_eq!(focused(&setup).as_deref(), Some("earlier"));
    press(&mut setup, Key::Char('x'));
    assert_eq!(setup.screen(), Screen::Question);
    assert_eq!(setup.topic(), Topic::InternalHosts);
}

// --------------------------------------------------------------- editing

#[test]
fn e_opens_a_field_holding_the_line_and_enter_commits_it_confirmed() {
    let mut setup = first_lines(
        notes("", ""),
        &[files("f")],
        &[line("acme", &[0]), line("other", &[0])],
    );
    press(&mut setup, Key::Char('e'));
    assert_eq!(setup.line_rows()[0].editing, Some("acme"));
    assert_eq!(setup.line_rows()[1].editing, None);
    typed(&mut setup, " org");
    press(&mut setup, Key::Backspace);
    setup.paste("\nx");
    assert_eq!(setup.line_rows()[0].editing, Some("acme or x"));
    press(&mut setup, Key::Enter);
    assert_eq!(texts(&setup), ["acme or x", "other"]);
    assert_eq!(states(&setup), [LineState::Kept, LineState::Open]);
    assert_eq!(focused(&setup).as_deref(), Some("other"));
    assert!(setup.line_rows().iter().all(|r| r.editing.is_none()));
}

#[test]
fn esc_leaves_an_edited_line_as_it_was() {
    let mut setup = first_lines(notes("", ""), &[files("f")], &[line("acme", &[0])]);
    press(&mut setup, Key::Char('e'));
    typed(&mut setup, " changed");
    assert_eq!(press(&mut setup, Key::Esc), Act::None);
    assert_eq!(texts(&setup), ["acme"]);
    assert_eq!(states(&setup), [LineState::Open]);
    assert_eq!(setup.screen(), Screen::Lines);
}

#[test]
fn a_zero_width_character_typed_into_an_edit_commits_without_it() {
    let mut setup = first_lines(notes("", ""), &[files("f")], &[line("acme", &[0])]);
    press(&mut setup, Key::Char('e'));
    typed(&mut setup, "\u{200b}!");
    press(&mut setup, Key::Enter);
    finish_to_diff(&mut setup);
    let (_, project) = save_of(press(&mut setup, Key::Enter));
    assert_eq!(
        project.map(|w| w.text),
        Some("### Source control\n- acme!\n".to_string())
    );
}

#[test]
fn an_emptied_edit_strikes_the_line() {
    let mut setup = first_lines(
        notes("", ""),
        &[files("f")],
        &[line("ab", &[0]), line("c", &[0])],
    );
    press(&mut setup, Key::Char('e'));
    press(&mut setup, Key::Backspace);
    press(&mut setup, Key::Backspace);
    press(&mut setup, Key::Enter);
    assert_eq!(states(&setup), [LineState::Struck, LineState::Open]);
}

#[test]
fn an_edit_that_cleans_to_nothing_strikes_the_line() {
    let mut setup = first_lines(notes("", ""), &[files("f")], &[line("a", &[0])]);
    press(&mut setup, Key::Char('e'));
    press(&mut setup, Key::Backspace);
    typed(&mut setup, "\u{200b} ");
    press(&mut setup, Key::Enter);
    assert_eq!(setup.screen(), Screen::Question);
    finish_to_diff(&mut setup);
    assert!(setup.layers().iter().all(|layer| layer.rows.is_empty()));
}

#[test]
fn e_on_an_earlier_line_opens_no_field() {
    let notes = notes("", "### Source control\n- earlier\n");
    let mut setup = first_lines(notes, &[files("f")], &[line("a", &[0])]);
    press(&mut setup, Key::Up);
    press(&mut setup, Key::Char('e'));
    assert!(setup.line_rows().iter().all(|r| r.editing.is_none()));
    press(&mut setup, Key::Char('q'));
    assert_eq!(
        states(&setup)[0],
        LineState::Kept,
        "a letter edited nothing"
    );
}

#[test]
fn keys_other_than_the_fields_are_swallowed_while_editing() {
    let mut setup = first_lines(notes("", ""), &[files("f")], &[line("a", &[0])]);
    press(&mut setup, Key::Char('e'));
    for key in [Key::Tab, Key::Up, Key::Down, Key::PageUp] {
        assert_eq!(press(&mut setup, key), Act::None);
    }
    assert_eq!(setup.line_rows()[0].editing, Some("a"));
    assert_eq!(
        layers(&setup),
        [NotesLayer::PerProject],
        "Tab flipped a line"
    );
}

#[test]
fn esc_from_the_lines_returns_to_the_question_dropping_the_run() {
    let notes = notes("", "### Source control\n- earlier\n");
    let mut setup = Setup::open(notes);
    typed(&mut setup, "acme");
    ask(&mut setup, &[files("f")], &[line("a", &[0])]);
    // Strike the earlier line, confirm nothing, go back.
    press(&mut setup, Key::Up);
    press(&mut setup, Key::Char('x'));
    assert_eq!(press(&mut setup, Key::Esc), Act::None);
    assert_eq!(setup.screen(), Screen::Question);
    assert_eq!(setup.answer(), "acme");
    assert!(setup.line_rows().is_empty(), "the proposals were kept");
    // The strike is kept: the next request leaves the line out.
    let Act::Propose(args) = press(&mut setup, Key::Enter) else {
        panic!("no request");
    };
    assert!(args.earlier.is_empty(), "{:?}", args.earlier);
    setup.proposed(&[files("f")], &[line("b", &[0])], None);
    assert_eq!(texts(&setup), ["earlier", "b"]);
    assert_eq!(states(&setup)[0], LineState::Struck);
}

#[test]
fn a_line_struck_in_one_layer_still_goes_in_the_request_while_the_other_holds_it() {
    let both = "### Source control\n- same\n";
    let mut setup = first_lines(notes(both, both), &[files("f")], &[line("a", &[0])]);
    press(&mut setup, Key::Up);
    press(&mut setup, Key::Up);
    assert_eq!(setup.line_rows()[0].layer, NotesLayer::Global);
    press(&mut setup, Key::Char('x'));
    press(&mut setup, Key::Esc);
    let Act::Propose(args) = press(&mut setup, Key::Enter) else {
        panic!("no request");
    };
    assert_eq!(args.earlier, ["same"]);
}

// -------------------------------------------------------- diff and save

#[test]
fn only_a_changed_layer_is_sent_with_the_version_it_read() {
    let mut setup = first_lines(
        notes("### Source control\n- g\n", ""),
        &[files("a remote"), answer("mine")],
        &[line("acme repo", &[0]), line("my host", &[1])],
    );
    press(&mut setup, Key::Enter);
    press(&mut setup, Key::Enter);
    finish_to_diff(&mut setup);
    let (global, project) = save_of(press(&mut setup, Key::Enter));
    assert_eq!(
        global,
        Some(NotesWrite {
            version: "global-v1".to_string(),
            text: "### Source control\n- g\n- my host\n".to_string(),
        })
    );
    assert_eq!(
        project,
        Some(NotesWrite {
            version: "project-v1".to_string(),
            text: "### Source control\n- acme repo\n".to_string(),
        })
    );

    // A line kept on one layer leaves the other unsent.
    let mut setup = first_lines(notes("", ""), &[files("f")], &[line("only", &[0])]);
    press(&mut setup, Key::Enter);
    finish_to_diff(&mut setup);
    let (global, project) = save_of(press(&mut setup, Key::Enter));
    assert_eq!(global, None);
    assert_eq!(project.map(|w| w.version), Some("project-v1".to_string()));
}

#[test]
fn a_flipped_line_goes_to_the_layer_it_was_flipped_to() {
    let mut setup = first_lines(notes("", ""), &[files("f")], &[line("a", &[0])]);
    press(&mut setup, Key::Tab);
    press(&mut setup, Key::Enter);
    finish_to_diff(&mut setup);
    let (global, project) = save_of(press(&mut setup, Key::Enter));
    assert_eq!(
        global.map(|w| w.text),
        Some("### Source control\n- a\n".to_string())
    );
    assert_eq!(project, None);
}

#[test]
fn an_edited_line_is_saved_as_edited() {
    let mut setup = first_lines(notes("", ""), &[files("f")], &[line("acme", &[0])]);
    press(&mut setup, Key::Char('e'));
    typed(&mut setup, "!");
    press(&mut setup, Key::Enter);
    finish_to_diff(&mut setup);
    let (_, project) = save_of(press(&mut setup, Key::Enter));
    assert_eq!(
        project.map(|w| w.text),
        Some("### Source control\n- acme!\n".to_string())
    );
}

#[test]
fn a_struck_earlier_line_and_the_prose_around_it_save_exactly() {
    let project = "intro\n### Source control\r\n- keep\r\n- drop\r\nprose\n";
    let mut setup = first_lines(notes("", project), &[], &[]);
    press(&mut setup, Key::Down);
    press(&mut setup, Key::Char('x'));
    finish_to_diff(&mut setup);
    let (global, project) = save_of(press(&mut setup, Key::Enter));
    assert_eq!(global, None);
    assert_eq!(
        project.map(|w| w.text),
        Some("intro\n### Source control\r\n- keep\r\nprose\n".to_string())
    );
}

#[test]
fn the_diff_rows_show_the_saved_text_and_an_unchanged_layer_has_none() {
    let mut setup = first_lines(
        notes("", "### Source control\n- earlier\n"),
        &[files("f")],
        &[line("new", &[0])],
    );
    press(&mut setup, Key::Enter);
    finish_to_diff(&mut setup);
    let layers = setup.layers();
    assert_eq!(layers.len(), 2);
    assert_eq!(layers[0].layer, NotesLayer::Global);
    assert_eq!(layers[0].file, PathBuf::from("/home/fiber/config.json"));
    assert!(layers[0].rows.is_empty());
    assert_eq!(layers[1].layer, NotesLayer::PerProject);
    assert_eq!(
        layers[1].file,
        PathBuf::from("/home/fiber/projects/p/config.json")
    );
    assert_eq!(
        layers[1].rows,
        [
            DiffRow::Context("### Source control".to_string()),
            DiffRow::Context("- earlier".to_string()),
            DiffRow::Added("- new".to_string()),
        ]
    );
}

#[test]
fn esc_on_the_diff_cancels_and_sends_nothing() {
    let mut setup = first_lines(notes("", ""), &[files("f")], &[line("a", &[0])]);
    press(&mut setup, Key::Enter);
    finish_to_diff(&mut setup);
    assert_eq!(press(&mut setup, Key::Esc), Act::Close);
}

#[test]
fn a_diff_where_no_layer_changed_yields_the_unchanged_close_and_no_save() {
    // A proposal struck, an earlier line kept: the all-keep run.
    let notes = notes("", "### Source control\n- earlier\n");
    let mut setup = first_lines(notes, &[files("f")], &[line("a", &[0])]);
    press(&mut setup, Key::Char('x'));
    finish_to_diff(&mut setup);
    assert_eq!(press(&mut setup, Key::Enter), Act::Unchanged);
    assert_eq!(setup.screen(), Screen::Diff);
    // A line already owned, confirmed again, changes nothing either.
    let notes = notes_with_same("### Source control\n- a\n");
    let mut setup = first_lines(notes, &[files("f")], &[line("a", &[0])]);
    press(&mut setup, Key::Enter);
    finish_to_diff(&mut setup);
    assert_eq!(press(&mut setup, Key::Enter), Act::Unchanged);
}

fn notes_with_same(project: &str) -> ReviewerNotes {
    notes("", project)
}

#[test]
fn the_diff_scrolls_one_row_or_one_page_and_clamps_at_both_edges() {
    let mut setup = Setup::open(notes("", ""));
    finish_to_diff(&mut setup);
    // 50 rows, 10 fit: the scroll runs 0 to 40, a page is 9.
    setup.note_viewport(50, 10);
    press(&mut setup, Key::Up);
    assert_eq!(setup.scroll(), 0);
    press(&mut setup, Key::Down);
    assert_eq!(setup.scroll(), 1);
    press(&mut setup, Key::PageDown);
    assert_eq!(setup.scroll(), 10);
    press(&mut setup, Key::PageUp);
    assert_eq!(setup.scroll(), 1);
    press(&mut setup, Key::PageUp);
    assert_eq!(setup.scroll(), 0);
    for _ in 0..10 {
        press(&mut setup, Key::PageDown);
    }
    assert_eq!(setup.scroll(), 40);
    press(&mut setup, Key::Down);
    assert_eq!(setup.scroll(), 40);
    press(&mut setup, Key::Up);
    assert_eq!(setup.scroll(), 39);
}

#[test]
fn a_page_is_at_least_one_row_and_a_diff_that_fits_does_not_scroll() {
    let mut setup = Setup::open(notes("", ""));
    finish_to_diff(&mut setup);
    setup.note_viewport(5, 1);
    press(&mut setup, Key::PageDown);
    assert_eq!(setup.scroll(), 1, "a page of capacity 1 moves one row");

    let mut setup = Setup::open(notes("", ""));
    finish_to_diff(&mut setup);
    setup.note_viewport(5, 5);
    press(&mut setup, Key::Down);
    press(&mut setup, Key::PageDown);
    assert_eq!(setup.scroll(), 0, "a diff that fits scrolled");
}

// -------------------------------------------------------------- the save

fn saved(file: &str, outcome: FileOutcome) -> FileSaved {
    FileSaved {
        file: PathBuf::from(file),
        outcome,
    }
}

fn at_the_diff_with_a_line() -> Setup {
    let mut setup = first_lines(notes("", ""), &[files("f")], &[line("a", &[0])]);
    press(&mut setup, Key::Enter);
    finish_to_diff(&mut setup);
    setup
}

#[test]
fn written_with_every_file_saved_shows_the_final_screen_and_offers_a_reload() {
    let mut setup = at_the_diff_with_a_line();
    let files = vec![
        saved("/g", FileOutcome::Saved),
        saved("/p", FileOutcome::Saved),
    ];
    assert_eq!(
        setup.saved(Ok(NotesSaved::Written(files.clone()))),
        Act::None
    );
    assert_eq!(setup.screen(), Screen::Saved);
    assert_eq!(setup.saved_files(), files.as_slice());
    assert!(setup.any_saved());
    // A second Enter cannot send a second save.
    assert_eq!(press(&mut setup, Key::Enter), Act::None);
    assert_eq!(press(&mut setup, Key::Char('r')), Act::Reload);
    assert_eq!(press(&mut setup, Key::Char('n')), Act::Close);
    assert_eq!(press(&mut setup, Key::Esc), Act::Close);
}

#[test]
fn written_with_the_first_file_saved_unsynced_and_the_second_saved() {
    let mut setup = at_the_diff_with_a_line();
    let files = vec![
        saved("/g", FileOutcome::SavedUnsynced(error("sync failed"))),
        saved("/p", FileOutcome::Saved),
    ];
    setup.saved(Ok(NotesSaved::Written(files.clone())));
    assert_eq!(setup.saved_files(), files.as_slice());
    assert!(setup.any_saved());
    assert_eq!(press(&mut setup, Key::Char('r')), Act::Reload);
}

#[test]
fn written_with_the_first_saved_and_the_second_not_saved_still_offers_a_reload() {
    let mut setup = at_the_diff_with_a_line();
    let files = vec![
        saved("/g", FileOutcome::Saved),
        saved("/p", FileOutcome::NotSaved(error("refused"))),
    ];
    setup.saved(Ok(NotesSaved::Written(files.clone())));
    assert_eq!(setup.saved_files(), files.as_slice());
    assert!(setup.any_saved());
    assert_eq!(press(&mut setup, Key::Char('r')), Act::Reload);
}

#[test]
fn written_with_a_file_unsynced_only_offers_a_reload_for_the_new_file_in_place() {
    let mut setup = at_the_diff_with_a_line();
    setup.saved(Ok(NotesSaved::Written(vec![saved(
        "/g",
        FileOutcome::SavedUnsynced(error("sync failed")),
    )])));
    assert!(setup.any_saved());
}

#[test]
fn written_with_no_file_saved_offers_no_reload_and_only_esc_closes() {
    let mut setup = at_the_diff_with_a_line();
    let files = vec![
        saved("/g", FileOutcome::NotSaved(error("a"))),
        saved("/p", FileOutcome::NotSaved(error("b"))),
    ];
    setup.saved(Ok(NotesSaved::Written(files.clone())));
    assert_eq!(setup.screen(), Screen::Saved);
    assert_eq!(setup.saved_files(), files.as_slice());
    assert!(!setup.any_saved());
    assert_eq!(press(&mut setup, Key::Char('r')), Act::None);
    assert_eq!(press(&mut setup, Key::Char('n')), Act::None);
    assert_eq!(press(&mut setup, Key::Enter), Act::None);
    assert_eq!(press(&mut setup, Key::Esc), Act::Close);
}

#[test]
fn stale_reads_again_and_redraws_the_diff_with_the_decisions_applied_to_the_fresh_text() {
    let project = "### Source control\n- gone soon\n- stays\n";
    let mut setup = first_lines(
        notes("", project),
        &[files("f")],
        &[line("new", &[0]), line("other", &[0])],
    );
    // Strike the first earlier line, confirm the first proposal.
    press(&mut setup, Key::Up);
    press(&mut setup, Key::Up);
    assert_eq!(focused(&setup).as_deref(), Some("gone soon"));
    press(&mut setup, Key::Char('x'));
    press(&mut setup, Key::Enter);
    press(&mut setup, Key::Char('x'));
    finish_to_diff(&mut setup);
    assert!(matches!(press(&mut setup, Key::Enter), Act::Save { .. }));
    assert_eq!(setup.saved(Ok(NotesSaved::Stale)), Act::Reread);
    assert_eq!(setup.screen(), Screen::Diff, "a stale save closed the diff");

    // Another writer removed the struck bullet and added one of its own.
    let mut fresh = notes("", "### Source control\n- stays\n- theirs\n");
    fresh.project.version = "project-v2".to_string();
    setup.reread(Ok(fresh));
    assert_eq!(setup.screen(), Screen::Diff);
    let (_, project) = save_of(press(&mut setup, Key::Enter));
    let project = project.expect("the project layer changed");
    assert_eq!(project.version, "project-v2");
    assert_eq!(
        project.text,
        "### Source control\n- stays\n- theirs\n- new\n"
    );
}

#[test]
fn a_second_stale_in_a_row_reads_again_and_a_second_save_holds_each_line_once() {
    let mut setup = at_the_diff_with_a_line();
    assert!(matches!(press(&mut setup, Key::Enter), Act::Save { .. }));
    assert_eq!(setup.saved(Ok(NotesSaved::Stale)), Act::Reread);
    setup.reread(Ok(notes("", "")));
    assert!(matches!(press(&mut setup, Key::Enter), Act::Save { .. }));
    assert_eq!(setup.saved(Ok(NotesSaved::Stale)), Act::Reread);
    setup.reread(Ok(notes("", "prose\n")));
    let (_, project) = save_of(press(&mut setup, Key::Enter));
    assert_eq!(
        project.map(|w| w.text),
        Some("prose\n\n### Source control\n- a\n".to_string())
    );
}

#[test]
fn a_confirmed_line_the_fresh_text_already_holds_is_not_added_a_second_time() {
    // The first save landed and its answer was lost: the fresh read already
    // holds the line, so the saved text equals it and nothing is sent.
    let mut setup = at_the_diff_with_a_line();
    assert!(matches!(press(&mut setup, Key::Enter), Act::Save { .. }));
    assert_eq!(setup.saved(Ok(NotesSaved::Stale)), Act::Reread);
    let mut fresh = notes("", "### Source control\n- a\n");
    fresh.project.version = "project-v2".to_string();
    setup.reread(Ok(fresh));
    assert!(setup.layers().iter().all(|layer| layer.rows.is_empty()));
    assert_eq!(press(&mut setup, Key::Enter), Act::Unchanged);
}

#[test]
fn a_failed_save_stays_on_the_diff_with_the_error() {
    let mut setup = at_the_diff_with_a_line();
    assert!(matches!(press(&mut setup, Key::Enter), Act::Save { .. }));
    assert_eq!(setup.saved(Err(error("config.json: busy"))), Act::None);
    assert_eq!(setup.screen(), Screen::Diff);
    assert_eq!(setup.error(), Some("config.json: busy"));
    // Another Enter tries again and clears the row.
    assert!(matches!(press(&mut setup, Key::Enter), Act::Save { .. }));
    assert_eq!(setup.error(), None);
}

#[test]
fn a_failed_reread_stays_on_the_diff_with_the_error() {
    let mut setup = at_the_diff_with_a_line();
    assert_eq!(setup.saved(Ok(NotesSaved::Stale)), Act::Reread);
    setup.reread(Err(error("config.json: unreadable")));
    assert_eq!(setup.screen(), Screen::Diff);
    assert_eq!(setup.error(), Some("config.json: unreadable"));
}

#[test]
fn a_rerun_on_the_text_a_partial_save_wrote_shows_the_saved_lines_as_earlier() {
    let mut setup = at_the_diff_with_a_line();
    assert!(matches!(press(&mut setup, Key::Enter), Act::Save { .. }));
    setup.saved(Ok(NotesSaved::Written(vec![
        saved("/g", FileOutcome::NotSaved(error("refused"))),
        saved("/p", FileOutcome::Saved),
    ])));
    // A later run reads what the save wrote.
    let mut rerun = Setup::open(notes("", "### Source control\n- a\n"));
    ask(&mut rerun, &[files("f")], &[line("b", &[0])]);
    let rows = rerun.line_rows();
    assert_eq!(
        rows.iter().map(|r| r.earlier).collect::<Vec<_>>(),
        [true, false]
    );
    assert_eq!(texts(&rerun), ["a", "b"]);
}

// ----------------------------------------------------------- run by run

#[test]
fn esc_from_the_lines_drops_that_runs_confirmed_and_edited_lines_but_not_an_earlier_strike() {
    let project = "### Source control\n- earlier\n";
    let mut setup = Setup::open(notes("", project));
    ask(
        &mut setup,
        &[files("f")],
        &[line("one", &[0]), line("two", &[0])],
    );
    // Strike the earlier line, confirm one, edit two.
    press(&mut setup, Key::Up);
    press(&mut setup, Key::Char('x'));
    press(&mut setup, Key::Enter);
    press(&mut setup, Key::Char('e'));
    typed(&mut setup, "!");
    press(&mut setup, Key::Enter);
    assert_eq!(setup.screen(), Screen::Question);
    // Back to the lines of topic 1, then to its question.
    press(&mut setup, Key::Esc);
    assert_eq!(setup.screen(), Screen::Lines);
    press(&mut setup, Key::Esc);
    assert_eq!(setup.screen(), Screen::Question);

    // A new run for the topic; the old lines are gone.
    ask(&mut setup, &[files("f")], &[line("three", &[0])]);
    press(&mut setup, Key::Enter);
    finish_to_diff(&mut setup);
    let (global, project) = save_of(press(&mut setup, Key::Enter));
    assert_eq!(global, None);
    let text = project.expect("changed").text;
    assert_eq!(text, "### Source control\n- three\n");
    assert!(!text.contains("one") && !text.contains("two") && !text.contains("two!"));
    assert!(!text.contains("earlier"), "the strike was lost");
}

#[test]
fn a_second_run_saves_its_confirmed_lines_beside_the_first_topics() {
    let mut setup = Setup::open(notes("", ""));
    ask(&mut setup, &[files("f")], &[line("one", &[0])]);
    press(&mut setup, Key::Enter);
    // Topic 2 and back: topic 1's confirmed line stays.
    press(&mut setup, Key::Esc);
    assert_eq!(texts(&setup), ["one"]);
    press(&mut setup, Key::Enter);
    skip_current_question_with_lines(&mut setup, &[answer("a")], &[line("two", &[0])]);
    finish_to_diff(&mut setup);
    let (global, project) = save_of(press(&mut setup, Key::Enter));
    assert_eq!(
        global.map(|w| w.text),
        Some("### Internal hosts\n- two\n".to_string())
    );
    assert_eq!(
        project.map(|w| w.text),
        Some("### Source control\n- one\n".to_string())
    );
}

/// Asks the question on show and confirms every line the answer gives.
fn skip_current_question_with_lines(setup: &mut Setup, ev: &[Evidence], lines: &[Proposed]) {
    ask(setup, ev, lines);
    for _ in lines {
        press(setup, Key::Enter);
    }
}

#[test]
fn the_save_applies_decisions_from_every_topic_in_topic_order() {
    let mut setup = Setup::open(notes("", ""));
    for topic_lines in ["one", "two", "three", "four", "five"] {
        skip_current_question_with_lines(&mut setup, &[files("f")], &[line(topic_lines, &[0])]);
    }
    assert_eq!(setup.screen(), Screen::Diff);
    let (_, project) = save_of(press(&mut setup, Key::Enter));
    assert_eq!(
        project.map(|w| w.text),
        Some(
            "### Source control\n- one\n\n### Internal hosts\n- two\n\n### Internal tools\n- three\n\n### Routine actions\n- four\n\n### What must never happen\n- five\n"
                .to_string()
        )
    );
}
