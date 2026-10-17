//! Tests for the trigger, the context size and the note request.

use std::collections::BTreeMap;

use contract::ActionId;
use contract::events::{
    ContextNudged, Control, HandoffCompleted, Note, Outcome, SkillLoad, ToolCallRequested,
    TurnOutcome,
};
use contract::provider::Input;
use contract::shapes::{Question, Tokens};
use serde_json::json;

use super::{
    Carry, HandoffSettings, context_tokens, estimate, note_request_text, prompt_tokens, trigger_at,
};

// Task 1 coverage: the skills sent again after a handoff (`docs/handoff.md`,
// item 7) ride in `Carry.resend`, render from `Event::SkillsResent`, and
// load through the one `admitted` predicate.

fn settings(enabled: bool, tokens: u64, window_fraction: f64) -> HandoffSettings {
    HandoffSettings {
        enabled,
        tokens,
        window_fraction,
        nudge: true,
    }
}

#[test]
fn the_defaults_are_the_documented_ones() {
    let defaults = HandoffSettings::default();
    assert!(defaults.enabled);
    assert_eq!(defaults.tokens, 400_000);
    assert!((defaults.window_fraction - 0.7).abs() < f64::EPSILON);
    assert!(defaults.nudge);
}

#[test]
fn off_has_no_trigger() {
    assert_eq!(trigger_at(&settings(false, 400_000, 0.7), 1_000_000), None);
}

#[test]
fn the_token_trigger_wins_on_a_big_window() {
    assert_eq!(
        trigger_at(&settings(true, 400_000, 0.7), 1_000_000),
        Some(400_000)
    );
}

#[test]
fn the_window_fraction_wins_on_a_small_window() {
    assert_eq!(
        trigger_at(&settings(true, 400_000, 0.7), 200_000),
        Some(140_000)
    );
}

#[test]
fn the_fraction_of_the_window_is_floored() {
    // 0.7 of 1,001 is 700.7.
    assert_eq!(trigger_at(&settings(true, 400_000, 0.7), 1_001), Some(700));
    // 0.5 of 1,001 is 500.5.
    assert_eq!(trigger_at(&settings(true, 400_000, 0.5), 1_001), Some(500));
}

#[test]
fn a_token_trigger_below_the_fraction_is_kept() {
    assert_eq!(
        trigger_at(&settings(true, 1_000, 0.7), 200_000),
        Some(1_000)
    );
}

#[test]
fn context_tokens_sum_the_prompt_and_the_output() {
    let tokens = Tokens {
        input: 1,
        cache_read: 10,
        cache_write: BTreeMap::from([("5m".to_owned(), 100), ("1h".to_owned(), 1_000)]),
        output: 10_000,
    };
    assert_eq!(context_tokens(&tokens), 11_111);
    assert_eq!(prompt_tokens(&tokens), 1_111);
}

fn call(name: &str, arguments: serde_json::Value) -> Input {
    Input::ToolCall {
        action_id: ActionId("a_1".into()),
        call: ToolCallRequested {
            name: name.into(),
            arguments,
            provider_id: None,
            repair: None,
            ran_by: None,
            provider_item: None,
        },
        model: "m".into(),
    }
}

#[test]
fn an_input_estimates_a_quarter_of_its_bytes_rounded_up() {
    let user = |text: &str| Input::User {
        text: text.into(),
        images: Vec::new(),
    };
    assert_eq!(estimate(&user("")), 0);
    assert_eq!(estimate(&user("abcd")), 1);
    assert_eq!(estimate(&user("abcde")), 2);
    assert_eq!(
        estimate(&Input::Assistant {
            model: "m".into(),
            text: "abcdefgh".into(),
            provider_item: None,
        }),
        2
    );
    assert_eq!(
        estimate(&Input::Reasoning {
            model: "m".into(),
            text: "abcdefghi".into(),
            provider_item: None,
        }),
        3
    );
    assert_eq!(
        estimate(&Input::ToolResult {
            pdfs: Vec::new(),
            action_id: ActionId("a_1".into()),
            text: "abcd".into(),
            is_error: false,
            images: Vec::new(),
        }),
        1
    );
}

#[test]
fn a_tool_call_estimates_its_name_and_arguments() {
    // `read` is 4 bytes and `{"a":1}` is 7.
    assert_eq!(estimate(&call("read", json!({"a": 1}))), 3);
}

#[test]
fn the_note_request_is_the_note_section_alone_without_instructions() {
    let text = note_request_text("/log/events.jsonl", None);
    assert!(text.starts_with("Fiber is about to restart your context."));
    assert!(text.contains("The session log is at /log/events.jsonl."));
    assert!(text.ends_with("make no tool calls."));
}

#[test]
fn instructions_follow_the_note_section_after_a_blank_line() {
    let plain = note_request_text("/log", None);
    let text = note_request_text("/log", Some("focus on tests"));
    assert_eq!(
        text,
        format!(
            "{plain}\n\nThe person asked that the next stretch of work focus on:\n\nfocus on tests"
        )
    );
}

fn completed(note: Option<Note>) -> HandoffCompleted {
    HandoffCompleted {
        outcome: Outcome::Completed,
        error: None,
        note,
        tokens_before: 0,
        instructions: None,
    }
}

fn user(text: &str) -> Input {
    Input::User {
        text: text.into(),
        images: Vec::new(),
    }
}

#[test]
fn a_restart_is_the_carried_input_then_the_note() {
    let mut carry = Carry {
        input: vec![user("one"), user("two")],
        texts: vec![
            (ActionId("a_other".into()), "not it".into()),
            (ActionId("a_note".into()), "first".into()),
            (ActionId("a_note".into()), "second".into()),
        ],
        nudged: true,
        ..Carry::default()
    };
    let done = completed(Some(Note::Actions {
        note: vec![ActionId("a_note".into())],
    }));
    let restarted = carry.restart(&done);
    assert_eq!(restarted, [user("one"), user("two"), user("first\nsecond")]);
    // A new context: not nudged, and the window's texts are spent.
    assert!(!carry.nudged);
    assert!(carry.texts.is_empty());
}

#[test]
fn a_restart_lists_the_jobs_still_running_in_start_order() {
    let mut carry = Carry {
        jobs: vec![
            ("j_2".into(), "second job".into()),
            ("j_1".into(), "first job".into()),
        ],
        texts: vec![(ActionId("a_note".into()), "note".into())],
        ..Carry::default()
    };
    let done = completed(Some(Note::Actions {
        note: vec![ActionId("a_note".into())],
    }));
    let restarted = carry.restart(&done);
    assert_eq!(restarted.len(), 2);
    assert_eq!(restarted[0], user("note"));
    let Input::User { text, .. } = &restarted[1] else {
        panic!("{restarted:?}");
    };
    assert!(
        text.ends_with("\n\n- j_2: second job\n- j_1: first job"),
        "{text}"
    );
}

#[test]
fn a_restart_without_jobs_has_no_jobs_line() {
    let mut carry = Carry {
        texts: vec![(ActionId("a_note".into()), "note".into())],
        ..Carry::default()
    };
    let restarted = carry.restart(&completed(Some(Note::Actions {
        note: vec![ActionId("a_note".into())],
    })));
    assert_eq!(restarted, [user("note")]);
}

#[test]
fn a_restart_forgets_the_artifacts_of_results_left_behind() {
    let mut carry = Carry {
        texts: vec![(ActionId("a_note".into()), "note".into())],
        artifacts: vec![(ActionId("a_old".into()), "artifacts/a_old.txt".into())],
        ..Carry::default()
    };
    carry.restart(&completed(Some(Note::Actions {
        note: vec![ActionId("a_note".into())],
    })));
    // Memory follows the context: the old result is not in the new one.
    assert!(carry.artifacts.is_empty());
}

#[test]
fn a_hooks_note_text_is_the_note() {
    let mut carry = Carry::default();
    let restarted = carry.restart(&completed(Some(Note::Hook {
        note_text: "from a hook".into(),
        extension: "ext".into(),
    })));
    assert_eq!(restarted, [user("from a hook")]);
}

#[test]
fn the_nudge_names_its_numbers_and_the_log() {
    let carry = Carry {
        session_log: "/log/events.jsonl".into(),
        ..Carry::default()
    };
    let text = carry.nudge_text(&ContextNudged {
        tokens: 266_700,
        trigger_at: 400_000,
    });
    assert!(text.contains("holds 266700 tokens"), "{text}");
    assert!(text.contains("At 400000 tokens"), "{text}");
    assert!(text.contains("/log/events.jsonl"), "{text}");
}

fn question(text: &str) -> Question {
    Question {
        header: "h".into(),
        question: text.into(),
        options: Vec::new(),
        multi_select: None,
    }
}

fn asking(texts: &[&str]) -> Control {
    Control {
        handoff: None,
        questions: Some(texts.iter().map(|text| question(text)).collect()),
        skill: None,
        name: None,
    }
}

/// A carry whose last reply requested `a_1`, `a_2` and `a_3`, in that order.
fn three_calls() -> (Carry, [ActionId; 3]) {
    let mut carry = Carry::default();
    let ids = [1, 2, 3].map(|n| ActionId(format!("a_{n}")));
    for id in &ids {
        carry.call_requested(id, &call("ask_user", json!({})));
    }
    (carry, ids)
}

#[test]
fn the_questions_asked_follow_call_order_not_completion_order() {
    let (mut carry, [first, second, third]) = three_calls();
    carry.call_completed(&third, &user("r3"), None, Some(&asking(&["c"])));
    carry.call_completed(&second, &user("r2"), None, None);
    carry.call_completed(&first, &user("r1"), None, Some(&asking(&["a", "b"])));
    assert_eq!(carry.asked(), [question("a"), question("b"), question("c")]);
    assert!(carry.noted().is_empty());
}

#[test]
fn a_handoff_note_alone_asks_nothing() {
    let (mut carry, [first, second, _]) = three_calls();
    let note = Control {
        handoff: Some("the note".into()),
        questions: None,
        skill: None,
        name: None,
    };
    carry.call_completed(&first, &user("r1"), None, Some(&note));
    carry.call_completed(&second, &user("r2"), None, None);
    assert!(carry.asked().is_empty());
    assert_eq!(carry.noted(), [first]);
    assert_eq!(carry.step[0].note.as_deref(), Some("the note"));
}

fn skilled(name: &str, path: &str) -> Control {
    Control {
        handoff: None,
        questions: None,
        skill: Some(SkillLoad {
            name: name.into(),
            path: path.into(),
        }),
        name: None,
    }
}

#[test]
fn a_completed_call_with_control_skill_appends_its_load() {
    let (mut carry, [first, ..]) = three_calls();
    carry.call_completed(
        &first,
        &user("r1"),
        None,
        Some(&skilled("tdd", "/w/tdd/SKILL.md")),
    );
    assert_eq!(
        carry.skills,
        [SkillLoad {
            name: "tdd".into(),
            path: "/w/tdd/SKILL.md".into(),
        }]
    );
}

#[test]
fn reloading_a_skill_keeps_its_position_and_takes_the_new_path() {
    let (mut carry, [first, second, _]) = three_calls();
    carry.call_completed(
        &first,
        &user("r1"),
        None,
        Some(&skilled("a", "/old/a/SKILL.md")),
    );
    carry.call_completed(
        &second,
        &user("r2"),
        None,
        Some(&skilled("b", "/w/b/SKILL.md")),
    );
    carry.call_completed(
        &first,
        &user("r3"),
        None,
        Some(&skilled("a", "/new/a/SKILL.md")),
    );
    assert_eq!(
        carry.skills,
        [
            SkillLoad {
                name: "a".into(),
                path: "/new/a/SKILL.md".into(),
            },
            SkillLoad {
                name: "b".into(),
                path: "/w/b/SKILL.md".into(),
            },
        ]
    );
}

#[test]
fn a_control_without_a_skill_and_no_control_leave_the_list_unchanged() {
    let (mut carry, [first, second, _]) = three_calls();
    carry.call_completed(&first, &user("r1"), None, Some(&asking(&["q"])));
    carry.call_completed(&second, &user("r2"), None, None);
    assert!(carry.skills.is_empty());
}

#[test]
fn a_restart_on_a_completed_handoff_leaves_skills_empty() {
    let (mut carry, [first, ..]) = three_calls();
    carry.call_completed(
        &first,
        &user("r1"),
        None,
        Some(&skilled("tdd", "/w/tdd/SKILL.md")),
    );
    carry.restart(&completed(Some(Note::Actions {
        note: vec![first.clone()],
    })));
    assert!(carry.skills.is_empty());
    // The load moves into the resend the next opening sends again.
    assert_eq!(carry.resend, [load("tdd", "/w/tdd/SKILL.md")]);
}

#[test]
fn only_a_cancelled_handoff_ends_the_turn_interrupted() {
    // A person cancelled the turn: the handoff ends it interrupted.
    assert_eq!(
        super::cancelled(Outcome::Cancelled),
        Some(crate::ended(TurnOutcome::Interrupted, None)),
    );
    // Every other ending leaves the turn to the handoff's own outcome.
    assert_eq!(super::cancelled(Outcome::Completed), None);
    assert_eq!(super::cancelled(Outcome::Failed), None);
}

#[test]
fn two_session_start_contexts_take_the_opening_slot_in_order() {
    let mut carry = Carry::default();
    assert_eq!(carry.take_after_opening(), None);
    carry.opened();
    assert_eq!(carry.take_after_opening(), Some(1));
    // Every take clears: the second take finds nothing.
    assert_eq!(carry.take_after_opening(), None);
    carry.opened();
    // The first context takes index 1 and advances the slot past itself.
    assert_eq!(carry.place_context(Some(1), "session_start"), Some(1));
    // The second takes index 2, in run order.
    assert_eq!(carry.place_context(Some(2), "session_start"), Some(2));
    assert_eq!(carry.take_after_opening(), Some(3));
}

#[test]
fn only_a_session_start_context_honours_the_opening_slot() {
    let mut carry = Carry::default();
    carry.opened();
    let slot = carry.take_after_opening();
    // Any other hook appends and leaves the slot cleared.
    assert_eq!(carry.place_context(slot, "turn_start"), None);
    assert_eq!(carry.take_after_opening(), None);
    carry.opened();
    let slot = carry.take_after_opening();
    assert_eq!(carry.place_context(slot, "before_message"), None);
    assert_eq!(carry.take_after_opening(), None);
}

fn load(name: &str, path: &str) -> contract::events::SkillLoad {
    contract::events::SkillLoad {
        name: name.into(),
        path: path.into(),
    }
}

fn resent(skills: &[(&str, &str, &str)]) -> contract::events::Event {
    contract::events::Event::SkillsResent(contract::events::SkillsResent {
        skills: skills
            .iter()
            .map(|(name, path, content)| contract::events::SkillSent {
                name: (*name).into(),
                path: (*path).into(),
                content: (*content).into(),
            })
            .collect(),
    })
}

fn opening() -> contract::events::Event {
    contract::events::Event::OpeningMessage(contract::events::OpeningMessage {
        environment: contract::events::Environment {
            date: "2023-11-14".into(),
            os: "test-os".into(),
            arch: "test-arch".into(),
            shell: "/bin/sh".into(),
            workspace: "/w".into(),
            git: None,
            session_log: "/w/s_test/events.jsonl".into(),
        },
        instruction_files: Vec::new(),
        extension_sections: Vec::new(),
        skills: Vec::new(),
    })
}

fn session_start_context(text: &str) -> contract::events::Event {
    contract::events::Event::ContextAdded(contract::events::ContextAdded {
        text: text.into(),
        extension: "fiber.test/inbox".into(),
        hook: "session_start".into(),
    })
}

fn render_into(
    carry: &mut Carry,
    conversation: &mut Vec<Input>,
    event: &contract::events::Event,
    action: Option<&ActionId>,
) {
    crate::conversation::render(
        conversation,
        event,
        action,
        "fake/model-1",
        &mut std::collections::BTreeMap::new(),
        carry,
    );
}

#[test]
fn a_restart_on_a_completed_handoff_moves_the_loads_into_resend() {
    let (mut carry, [first, second, _]) = three_calls();
    carry.call_completed(
        &first,
        &user("r1"),
        None,
        Some(&skilled("alpha", "/w/alpha/SKILL.md")),
    );
    carry.call_completed(
        &second,
        &user("r2"),
        None,
        Some(&skilled("beta", "/w/beta/SKILL.md")),
    );
    // A reload keeps its first-load position and takes the later path.
    carry.call_completed(
        &first,
        &user("r3"),
        None,
        Some(&skilled("alpha", "/w/new-alpha/SKILL.md")),
    );
    carry.restart(&completed(None));
    assert!(carry.skills.is_empty());
    assert_eq!(
        carry.resend,
        [
            load("alpha", "/w/new-alpha/SKILL.md"),
            load("beta", "/w/beta/SKILL.md"),
        ]
    );
    // A second restart replaces the resend, never appends to it.
    carry.call_completed(
        &first,
        &user("r4"),
        None,
        Some(&skilled("gamma", "/w/gamma/SKILL.md")),
    );
    carry.restart(&completed(None));
    assert_eq!(carry.resend, [load("gamma", "/w/gamma/SKILL.md")]);
}

#[test]
fn the_opening_message_and_a_resend_clear_the_armed_resend() {
    let mut carry = Carry {
        resend: vec![load("alpha", "/w/alpha/SKILL.md")],
        ..Carry::default()
    };
    let mut conversation = Vec::new();
    render_into(&mut carry, &mut conversation, &opening(), None);
    assert!(carry.resend.is_empty());
    carry.resend = vec![load("alpha", "/w/alpha/SKILL.md")];
    render_into(
        &mut carry,
        &mut conversation,
        &resent(&[("alpha", "/w/alpha/SKILL.md", "Alpha.")]),
        None,
    );
    assert!(carry.resend.is_empty());
}

#[test]
fn a_resend_appends_each_skill_after_the_note_in_event_order() {
    let mut carry = Carry {
        input: vec![user("ask")],
        ..Carry::default()
    };
    let mut conversation = vec![user("ask"), user("the note")];
    render_into(
        &mut carry,
        &mut conversation,
        &resent(&[
            ("alpha", "/w/alpha/SKILL.md", "Alpha body."),
            ("beta", "/w/beta/SKILL.md", "Beta body."),
        ]),
        None,
    );
    assert_eq!(
        conversation,
        [
            user("ask"),
            user("the note"),
            user("/w/alpha/SKILL.md\n\nAlpha body."),
            user("/w/beta/SKILL.md\n\nBeta body."),
        ]
    );
    // The render records the loads but never touches the turn's input.
    assert_eq!(carry.input, [user("ask")]);
    assert_eq!(
        carry.skills,
        [
            load("alpha", "/w/alpha/SKILL.md"),
            load("beta", "/w/beta/SKILL.md"),
        ]
    );
}

#[test]
fn a_full_handoff_renders_the_opening_then_the_context_then_the_skills() {
    let (mut carry, [first, second]) = (
        Carry::default(),
        [ActionId("a_1".into()), ActionId("a_2".into())],
    );
    carry.input = vec![user("ask")];
    carry.jobs = vec![("j_1".into(), "job one".into())];
    carry.call_completed(
        &first,
        &user("r1"),
        None,
        Some(&skilled("alpha", "/w/alpha/SKILL.md")),
    );
    carry.call_completed(
        &second,
        &user("r2"),
        None,
        Some(&skilled("beta", "/w/beta/SKILL.md")),
    );
    let mut conversation = carry.restart(&completed(None));
    render_into(
        &mut carry,
        &mut conversation,
        &resent(&[
            ("alpha", "/w/alpha/SKILL.md", "Alpha body."),
            ("beta", "/w/beta/SKILL.md", "Beta body."),
        ]),
        None,
    );
    render_into(&mut carry, &mut conversation, &opening(), None);
    render_into(
        &mut carry,
        &mut conversation,
        &session_start_context("hook context"),
        None,
    );
    assert_eq!(conversation.len(), 7);
    assert!(matches!(&conversation[1], Input::User { text, .. } if text == "hook context"));
    assert_eq!(conversation[5], user("/w/alpha/SKILL.md\n\nAlpha body."));
    assert_eq!(conversation[6], user("/w/beta/SKILL.md\n\nBeta body."));
    assert!(matches!(&conversation[0], Input::User { text, .. } if text.contains("2023-11-14")));
}

#[test]
fn an_empty_resend_appends_nothing() {
    let mut carry = Carry::default();
    let mut conversation = vec![user("ask")];
    render_into(&mut carry, &mut conversation, &resent(&[]), None);
    assert_eq!(conversation, [user("ask")]);
    assert!(carry.skills.is_empty());
}

#[test]
fn a_skill_sent_again_is_loaded_in_the_new_context() {
    let mut carry = Carry::default();
    let mut conversation = Vec::new();
    render_into(
        &mut carry,
        &mut conversation,
        &resent(&[
            ("alpha", "/w/alpha/SKILL.md", "Alpha body."),
            ("beta", "/w/beta/SKILL.md", "Beta body."),
        ]),
        None,
    );
    // The next handoff arms the resend from the skills sent again.
    carry.restart(&completed(None));
    assert_eq!(
        carry.resend,
        [
            load("alpha", "/w/alpha/SKILL.md"),
            load("beta", "/w/beta/SKILL.md"),
        ]
    );
    // A skill the next resend leaves out is not carried afterwards.
    let mut conversation = Vec::new();
    render_into(
        &mut carry,
        &mut conversation,
        &resent(&[("alpha", "/w/alpha/SKILL.md", "Alpha body.")]),
        None,
    );
    carry.restart(&completed(None));
    assert_eq!(carry.resend, [load("alpha", "/w/alpha/SKILL.md")]);
}

#[test]
fn admitted_only_passes_a_fiber_run_completion() {
    use contract::events::CallStatus;
    let control = skilled("alpha", "/w/alpha/SKILL.md");
    assert!(crate::conversation::admitted(CallStatus::Completed, false, Some(&control)).is_some());
    assert!(crate::conversation::admitted(CallStatus::Failed, false, Some(&control)).is_none());
    assert!(crate::conversation::admitted(CallStatus::Denied, false, Some(&control)).is_none());
    assert!(crate::conversation::admitted(CallStatus::Cancelled, false, Some(&control)).is_none());
    // A provider-run record never counts, even carrying a skill.
    assert!(crate::conversation::admitted(CallStatus::Completed, true, Some(&control)).is_none());
    assert!(crate::conversation::admitted(CallStatus::Completed, false, None).is_none());
}

#[test]
fn a_provider_run_record_carrying_a_skill_renders_raw_and_loads_nothing() {
    use contract::events::{CallStatus, ToolCallCompleted};
    use contract::shapes::ContentPart;
    let mut carry = Carry::default();
    let mut conversation = Vec::new();
    let done = contract::events::Event::ToolCallCompleted(ToolCallCompleted {
        status: CallStatus::Completed,
        reason: None,
        error: None,
        process: None,
        content: vec![ContentPart::Text { text: "ok".into() }],
        details: None,
        artifact: None,
        kept: None,
        changes: None,
        control: Some(skilled("alpha", "/w/alpha/SKILL.md")),
        changed_by: None,
        provider_item: Some(json!({"type": "hosted_tool_call"})),
        hosts_surfaced: Vec::new(),
    });
    render_into(
        &mut carry,
        &mut conversation,
        &done,
        Some(&ActionId("a_1".into())),
    );
    assert!(carry.skills.is_empty());
    assert_eq!(conversation.len(), 1);
    assert!(matches!(
        &conversation[0],
        Input::Assistant { text, provider_item: Some(_), .. } if text.is_empty()
    ));
}

#[test]
fn an_inner_calls_completion_loads_no_skill() {
    use contract::events::{RanBy, ToolCallRequested};
    let inner = ActionId("a_inner".into());
    let mut carry = Carry::default();
    let mut conversation = Vec::new();
    let requested = contract::events::Event::ToolCallRequested(ToolCallRequested {
        name: "inner".into(),
        arguments: json!({}),
        provider_id: None,
        repair: None,
        ran_by: Some(RanBy {
            extension: "ext".into(),
            anchor: contract::events::Anchor::OuterCall(ActionId("a_0".into())),
        }),
        provider_item: None,
    });
    render_into(&mut carry, &mut conversation, &requested, Some(&inner));
    assert!(carry.is_inner(&inner));
    let done = contract::events::Event::ToolCallCompleted(contract::events::ToolCallCompleted {
        status: contract::events::CallStatus::Completed,
        reason: None,
        error: None,
        process: None,
        content: vec![contract::shapes::ContentPart::Text { text: "ok".into() }],
        details: None,
        artifact: None,
        kept: None,
        changes: None,
        control: Some(skilled("alpha", "/w/alpha/SKILL.md")),
        changed_by: None,
        provider_item: None,
        hosts_surfaced: Vec::new(),
    });
    render_into(&mut carry, &mut conversation, &done, Some(&inner));
    assert!(carry.skills.is_empty());
}

#[test]
fn loaded_keeps_first_load_order_and_the_latest_path() {
    let mut carry = Carry::default();
    carry.loaded(load("alpha", "/old/alpha/SKILL.md"));
    carry.loaded(load("beta", "/w/beta/SKILL.md"));
    carry.loaded(load("alpha", "/new/alpha/SKILL.md"));
    assert_eq!(
        carry.skills,
        [
            load("alpha", "/new/alpha/SKILL.md"),
            load("beta", "/w/beta/SKILL.md"),
        ]
    );
}
