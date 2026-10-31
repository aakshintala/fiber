//! Tests for the approval queue: folding, the panel, keys and answers.

use super::{Mark, Panel, PanelKey, Queue};
use crate::keys::{Edit, Key};
use crate::theme::Role;
use ratatui::style::{Modifier, Style};
use std::ops::Range;

const S_A: &str = "s_aaaaaaaaaaaaaaaa";
const S_B: &str = "s_bbbbbbbbbbbbbbbb";

const HINT: &str = "↑↓ choose · Enter confirms · typing goes to the feedback · click a choice";

/// One session envelope.
fn envelope(
    session: &str,
    kind: &str,
    payload: serde_json::Value,
    action: &str,
) -> contract::Envelope {
    contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId(action.to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    }
}

/// A `tool_call_requested` for `action`.
fn call(
    session: &str,
    action: &str,
    name: &str,
    arguments: serde_json::Value,
) -> contract::Envelope {
    envelope(
        session,
        "tool_call_requested",
        serde_json::json!({"name": name, "arguments": arguments}),
        action,
    )
}

/// A `permission_requested` with `keys` beside its request id.
fn request(session: &str, action: &str, id: &str, keys: serde_json::Value) -> contract::Envelope {
    let mut payload =
        serde_json::json!({"request_id": id, "effects": ["executes"], "reversible": true});
    if let (Some(into), Some(from)) = (payload.as_object_mut(), keys.as_object()) {
        into.extend(from.clone());
    }
    envelope(session, "permission_requested", payload, action)
}

/// A standing ask by a global rule on `prefix`.
fn standing_ask(session: &str, action: &str, id: &str, prefix: &str) -> contract::Envelope {
    request(
        session,
        action,
        id,
        serde_json::json!({"step": "standing_ask",
            "standing_rule": {"scope": "global", "prefix": prefix}}),
    )
}

/// A review request with `extra` keys (`escalation`, `rule`).
fn review(session: &str, id: &str, extra: serde_json::Value) -> contract::Envelope {
    let mut keys = serde_json::json!({"step": "review"});
    if let (Some(into), Some(from)) = (keys.as_object_mut(), extra.as_object()) {
        into.extend(from.clone());
    }
    request(session, "a_9", id, keys)
}

/// A review request offering the rule `npm test`.
fn offering(session: &str, id: &str) -> contract::Envelope {
    review(
        session,
        id,
        serde_json::json!({"rule": {"subject": "npm test --watch", "prefix": "npm test"}}),
    )
}

/// A `permission_resolved` for `id`.
fn resolved(session: &str, id: &str) -> contract::Envelope {
    envelope(
        session,
        "permission_resolved",
        serde_json::json!({"request_id": id, "decision": "deny", "decided_by": "cancel"}),
        "a_1",
    )
}

/// A queue holding `lines`, folded in order.
fn folded(lines: &[contract::Envelope]) -> Queue {
    let mut queue = Queue::default();
    for line in lines {
        queue.fold(line);
    }
    queue
}

/// The open panel at width 80.
fn rendered(queue: &Queue) -> Panel {
    queue.panel(80).expect("the panel is open")
}

/// The open panel with the session name from `name`.
fn named(queue: &Queue, name: Option<&str>) -> Panel {
    queue
        .panel_with(80, &|_| name.map(str::to_owned))
        .expect("the panel is open")
}

/// The panel's lines, or none when it is closed.
fn panel(queue: &Queue) -> Vec<String> {
    queue.panel(80).map(|panel| panel.lines).unwrap_or_default()
}

/// The panel's line at `row`.
fn row(queue: &Queue, row: usize) -> String {
    panel(queue).get(row).cloned().unwrap_or_default()
}

/// The panel's header line.
fn header(queue: &Queue) -> String {
    row(queue, 0)
}

/// The header for `k` of `n` from `session` at width 80: the left part,
/// spaces, and right-aligned "Esc puts it aside".
fn header_text(k: usize, n: usize, session: &str) -> String {
    header_at(k, n, session, 80)
}

/// The header for `k` of `n` from `session` at `width` columns.
fn header_at(k: usize, n: usize, session: &str, width: usize) -> String {
    let left = format!("Approval {k} of {n} · {session}");
    let pad = width
        .saturating_sub(crate::format::width(&left).saturating_add("Esc puts it aside".len()))
        .max(1);
    format!("{left}{}{}", " ".repeat(pad), "Esc puts it aside")
}

/// "Approval k of n": bold in `attention`.
fn head_style() -> Style {
    Style::new()
        .fg(Role::Attention.color())
        .add_modifier(Modifier::BOLD)
}

/// The asking session: in `attention`.
fn session_style() -> Style {
    Style::new().fg(Role::Attention.color())
}

/// "irreversible": bold in `error`.
fn loud_style() -> Style {
    Style::new()
        .fg(Role::Error.color())
        .add_modifier(Modifier::BOLD)
}

/// A dim run.
fn dim_style() -> Style {
    Style::new().add_modifier(Modifier::DIM)
}

/// A focused label.
fn bold_style() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

/// A focused label over a dim run.
fn dim_bold_style() -> Style {
    Style::new()
        .add_modifier(Modifier::DIM)
        .add_modifier(Modifier::BOLD)
}

/// A focused label over an `info` run.
fn remember_bold_style() -> Style {
    Style::new()
        .fg(Role::Info.color())
        .add_modifier(Modifier::BOLD)
}

/// The tool's name: bold in `info`.
fn tool_style() -> Style {
    Style::new()
        .fg(Role::Info.color())
        .add_modifier(Modifier::BOLD)
}

/// The focused choice's mark: in `attention`.
fn focus_style() -> Style {
    Style::new().fg(Role::Attention.color())
}

/// A remembering choice's rule prefix: in `info`.
fn remember_style() -> Style {
    Style::new().fg(Role::Info.color())
}

/// One expected styled run.
fn mark(line: usize, bytes: Range<usize>, style: Style) -> Mark {
    Mark { line, bytes, style }
}

/// Presses `key` `times` times on the open panel.
fn press(queue: &mut Queue, key: Key, times: usize) {
    for _ in 0..times {
        assert_eq!(queue.on_key(&key), Some(PanelKey::Handled), "{key:?}");
    }
}

/// Answers the shown request as `c_1`, returning the parsed line.
fn answer(queue: &mut Queue) -> serde_json::Value {
    assert_eq!(queue.on_key(&Key::Enter), Some(PanelKey::Answer));
    let line = queue.answer("c_1").unwrap_or_default();
    serde_json::from_str(&line).unwrap_or_default()
}

#[test]
fn a_standing_ask_shows_two_choices_with_marks() {
    let queue = folded(&[
        call(
            S_A,
            "a_1",
            "shell",
            serde_json::json!({"command": "echo hi"}),
        ),
        standing_ask(S_A, "a_1", "r_1", "echo hi"),
    ]);
    let shown = rendered(&queue);
    let facts = "runs a command · asked by the global rule echo hi";
    assert_eq!(
        shown.lines,
        vec![
            header_text(1, 1, S_A),
            "shell echo hi".to_owned(),
            facts.to_owned(),
            String::new(),
            "▸ 1 Allow once".to_owned(),
            "  2 Deny  type to add feedback".to_owned(),
            String::new(),
            HINT.to_owned(),
        ]
    );
    assert!(!shown.alert);
    assert_eq!(shown.caret, None);
    assert_eq!(
        shown.marks,
        vec![
            mark(0, 0..15, head_style()),
            mark(0, 15..37, session_style()),
            mark(0, 64..81, dim_style()),
            mark(1, 0..5, tool_style()),
            mark(1, 6..13, bold_style()),
            mark(2, 0..facts.len(), dim_style()),
            mark(4, 0..4, focus_style()),
            mark(4, 4..5, dim_style()),
            mark(4, 6..16, bold_style()),
            mark(5, 2..3, dim_style()),
            mark(5, 8..30, dim_style()),
            mark(7, 0..HINT.len(), dim_style()),
        ]
    );
}

#[test]
fn a_request_offering_a_rule_shows_both_remembering_rows() {
    let queue = folded(&[offering(S_A, "r_1")]);
    let shown = rendered(&queue);
    let facts = "runs a command · no rule allows this call";
    assert_eq!(
        shown.lines,
        vec![
            header_text(1, 1, S_A),
            facts.to_owned(),
            String::new(),
            "▸ 1 Allow once".to_owned(),
            "  2 Allow for this session npm test *".to_owned(),
            "  3 Always allow in this project npm test *".to_owned(),
            "  4 Deny  type to add feedback".to_owned(),
            String::new(),
            HINT.to_owned(),
        ]
    );
    assert!(!shown.alert);
    assert_eq!(shown.caret, None);
    assert_eq!(
        shown.marks,
        vec![
            mark(0, 0..15, head_style()),
            mark(0, 15..37, session_style()),
            mark(0, 64..81, dim_style()),
            mark(1, 0..facts.len(), dim_style()),
            mark(3, 0..4, focus_style()),
            mark(3, 4..5, dim_style()),
            mark(3, 6..16, bold_style()),
            mark(4, 2..3, dim_style()),
            mark(4, 27..37, remember_style()),
            mark(5, 2..3, dim_style()),
            mark(5, 33..43, remember_style()),
            mark(6, 2..3, dim_style()),
            mark(6, 8..30, dim_style()),
            mark(8, 0..HINT.len(), dim_style()),
        ]
    );
}

#[test]
fn a_review_without_a_rule_shows_two_choices() {
    let queue = folded(&[review(S_A, "r_1", serde_json::json!({}))]);
    assert_eq!(
        panel(&queue),
        vec![
            header_text(1, 1, S_A),
            "runs a command · no rule allows this call".to_owned(),
            String::new(),
            "▸ 1 Allow once".to_owned(),
            "  2 Deny  type to add feedback".to_owned(),
            String::new(),
            HINT.to_owned(),
        ]
    );
    assert!(!rendered(&queue).alert);
}

#[test]
fn the_facts_line_words_effects_paths_and_reasons() {
    // Each declared effect in words.
    for (effect, words) in [
        ("executes", "runs a command"),
        ("writes", "writes files"),
        ("reads", "reads files"),
        ("network", "uses the network"),
    ] {
        let queue = folded(&[request(
            S_A,
            "a_1",
            "r_1",
            serde_json::json!({"effects": [effect],
                "step": "standing_ask",
                "standing_rule": {"scope": "global", "prefix": "echo hi"}}),
        )]);
        assert_eq!(
            row(&queue, 1),
            format!("{words} · asked by the global rule echo hi"),
            "{effect}"
        );
    }
    // Two effects join with ", ".
    let queue = folded(&[request(
        S_A,
        "a_1",
        "r_1",
        serde_json::json!({"effects": ["reads", "network"],
            "step": "standing_ask",
            "standing_rule": {"scope": "global", "prefix": "echo hi"}}),
    )]);
    assert_eq!(
        row(&queue, 1),
        "reads files, uses the network · asked by the global rule echo hi"
    );
    // Paths join with ", " after the effects.
    let queue = folded(&[request(
        S_A,
        "a_1",
        "r_1",
        serde_json::json!({"paths": ["a/b", "c.txt"],
            "step": "standing_ask",
            "standing_rule": {"scope": "global", "prefix": "echo hi"}}),
    )]);
    assert_eq!(
        row(&queue, 1),
        "runs a command · a/b, c.txt · asked by the global rule echo hi"
    );
    // No effects and no paths: the line starts at why it asked.
    let queue = folded(&[request(
        S_A,
        "a_1",
        "r_1",
        serde_json::json!({"effects": [],
            "step": "standing_ask",
            "standing_rule": {"scope": "global", "prefix": "echo hi"}}),
    )]);
    assert_eq!(row(&queue, 1), "asked by the global rule echo hi");
    // No effects with paths: the line starts at the paths.
    let queue = folded(&[request(
        S_A,
        "a_1",
        "r_1",
        serde_json::json!({"effects": [], "paths": ["notes.md"],
            "step": "standing_ask",
            "standing_rule": {"scope": "global", "prefix": "echo hi"}}),
    )]);
    assert_eq!(
        row(&queue, 1),
        "notes.md · asked by the global rule echo hi"
    );
}

#[test]
fn the_session_name_comes_from_the_lookup() {
    let queue = folded(&[standing_ask(S_A, "a_1", "r_1", "echo hi")]);
    assert_eq!(
        named(&queue, Some("main")).lines[0],
        header_text(1, 1, "main")
    );
    // No name, or an empty one, falls back to the id.
    assert_eq!(named(&queue, None).lines[0], header_text(1, 1, S_A));
    assert_eq!(named(&queue, Some("")).lines[0], header_text(1, 1, S_A));
    // Control characters in a name become spaces.
    assert_eq!(
        named(&queue, Some("ma\nin")).lines[0],
        header_text(1, 1, "ma in")
    );
}

#[test]
fn the_header_pads_esc_to_the_right_and_wraps_past_the_width() {
    let queue = folded(&[standing_ask(S_A, "a_1", "r_1", "echo hi")]);
    let left = format!("Approval 1 of 1 · {S_A}");
    let right = "Esc puts it aside";
    let at = |width: u16| {
        queue
            .panel(width)
            .map(|panel| panel.lines)
            .unwrap_or_default()
            .into_iter()
            .next()
            .unwrap_or_default()
    };
    // Left and right alone: one space, past the width, so the line wraps.
    assert_eq!(at(53), format!("{left} {right}"));
    // One column more: one space, ending in the last column.
    assert_eq!(at(54), format!("{left} {right}"));
    assert_eq!(crate::format::width(&at(54)), 54);
    // Two columns more: two spaces, ending in the last column.
    assert_eq!(at(55), format!("{left}  {right}"));
    assert_eq!(crate::format::width(&at(55)), 55);
}

#[test]
fn two_requests_move_to_the_next_on_answer_or_put_aside() {
    let mut queue = folded(&[
        standing_ask(S_A, "a_1", "r_1", "one"),
        standing_ask(S_A, "a_2", "r_2", "two"),
    ]);
    assert_eq!(header(&queue), header_text(1, 2, S_A));
    answer(&mut queue);
    assert_eq!(header(&queue), header_text(1, 1, S_A));
    assert_eq!(
        row(&queue, 1),
        "runs a command · asked by the global rule two"
    );
    // Esc on the first of two shows the second, still counted behind it.
    let mut queue = folded(&[
        standing_ask(S_A, "a_1", "r_1", "one"),
        standing_ask(S_A, "a_2", "r_2", "two"),
    ]);
    press(&mut queue, Key::Esc, 1);
    assert_eq!(header(&queue), header_text(2, 2, S_A));
    press(&mut queue, Key::Esc, 1);
    assert!(queue.panel(80).is_none());
    assert_eq!(
        queue.badge(0).as_deref(),
        Some("  ⚠ 2 approvals waiting · click to reopen")
    );
}

#[test]
fn digits_type_feedback_instead_of_choosing() {
    // Two choices: "2" is feedback, and the panel stays open.
    let mut queue = folded(&[standing_ask(S_A, "a_1", "r_1", "echo hi")]);
    assert_eq!(queue.on_key(&Key::Char('2')), Some(PanelKey::Handled));
    assert_eq!(row(&queue, 4), "▸ 2 Deny  2");
    assert_eq!(rendered(&queue).caret, Some((4, 11)));
    assert!(queue.badge(0).is_none());
    // Enter denies with the feedback.
    assert_eq!(queue.on_key(&Key::Enter), Some(PanelKey::Answer));
    let line = queue.answer("c_1").unwrap_or_default();
    let reply: serde_json::Value = serde_json::from_str(&line).unwrap_or_default();
    assert_eq!(
        reply["args"],
        serde_json::json!({"request_id": "r_1", "decision": "deny", "feedback": "2"})
    );
    // Four choices: "3" is feedback too.
    let mut queue = folded(&[offering(S_A, "r_1")]);
    assert_eq!(queue.on_key(&Key::Char('3')), Some(PanelKey::Handled));
    assert_eq!(row(&queue, 6), "▸ 4 Deny  3");
    assert_eq!(queue.on_key(&Key::Enter), Some(PanelKey::Answer));
    let line = queue.answer("c_1").unwrap_or_default();
    let reply: serde_json::Value = serde_json::from_str(&line).unwrap_or_default();
    assert_eq!(
        reply["args"],
        serde_json::json!({"request_id": "r_1", "decision": "deny", "feedback": "3"})
    );
}

#[test]
fn delete_to_line_start_clears_the_feedback() {
    let mut queue = folded(&[standing_ask(S_A, "a_1", "r_1", "echo hi")]);
    for ch in "one two three".chars() {
        press(&mut queue, Key::Char(ch), 1);
    }
    assert_eq!(row(&queue, 4), "▸ 2 Deny  one two three");
    // Ghostty sends ⌘Backspace as `\x15`, the `delete_to_line_start` key.
    queue.on_edit(&Edit::DeleteToLineStart);
    assert_eq!(row(&queue, 4), "▸ 2 Deny  type to add feedback");
}

#[test]
fn delete_word_deletes_the_previous_feedback_word() {
    let mut queue = folded(&[standing_ask(S_A, "a_1", "r_1", "echo hi")]);
    for ch in "one two".chars() {
        press(&mut queue, Key::Char(ch), 1);
    }
    assert_eq!(row(&queue, 4), "▸ 2 Deny  one two");
    queue.on_edit(&Edit::DeleteWord);
    assert_eq!(row(&queue, 4), "▸ 2 Deny  one ");
}

#[test]
fn the_caret_shows_only_with_feedback_on_deny() {
    let mut queue = folded(&[standing_ask(S_A, "a_1", "r_1", "echo hi")]);
    assert_eq!(rendered(&queue).caret, None);
    press(&mut queue, Key::Char('a'), 1);
    assert_eq!(row(&queue, 4), "▸ 2 Deny  a");
    assert_eq!(rendered(&queue).caret, Some((4, 11)));
    // Backspace to empty clears the caret and the prompt returns.
    press(&mut queue, Key::Backspace, 1);
    assert_eq!(row(&queue, 4), "▸ 2 Deny  type to add feedback");
    assert_eq!(rendered(&queue).caret, None);
    // Away from deny there is no caret even with feedback kept.
    press(&mut queue, Key::Char('b'), 1);
    press(&mut queue, Key::Up, 1);
    assert_eq!(row(&queue, 3), "▸ 1 Allow once");
    assert_eq!(rendered(&queue).caret, None);
}

#[test]
fn the_badge_counts_waiting_and_extra() {
    // Nothing waiting: no badge.
    assert_eq!(Queue::default().badge(0), None);
    // The panel open: no badge even with extra held elsewhere.
    let mut queue = folded(&[standing_ask(S_A, "a_1", "r_1", "one")]);
    assert_eq!(queue.badge(1), None);
    press(&mut queue, Key::Esc, 1);
    assert_eq!(
        queue.badge(0).as_deref(),
        Some("  ⚠ 1 approvals waiting · click to reopen")
    );
    // Extra held elsewhere counts with the waiting ones.
    assert_eq!(
        queue.badge(2).as_deref(),
        Some("  ⚠ 3 approvals waiting · click to reopen")
    );
    // Two waiting: N = 2.
    let mut queue = folded(&[
        standing_ask(S_A, "a_1", "r_1", "one"),
        standing_ask(S_A, "a_2", "r_2", "two"),
    ]);
    press(&mut queue, Key::Esc, 2);
    assert_eq!(
        queue.badge(0).as_deref(),
        Some("  ⚠ 2 approvals waiting · click to reopen")
    );
}

#[test]
fn focusing_a_choice_bolds_its_label_and_tail() {
    let mut queue = folded(&[offering(S_A, "r_1")]);
    press(&mut queue, Key::Down, 1);
    assert_eq!(row(&queue, 4), "▸ 2 Allow for this session npm test *");
    assert_eq!(
        rendered(&queue).marks,
        vec![
            mark(0, 0..15, head_style()),
            mark(0, 15..37, session_style()),
            mark(0, 64..81, dim_style()),
            mark(1, 0..42, dim_style()),
            mark(3, 2..3, dim_style()),
            mark(4, 0..4, focus_style()),
            mark(4, 4..5, dim_style()),
            mark(4, 6..29, bold_style()),
            mark(4, 29..39, remember_bold_style()),
            mark(5, 2..3, dim_style()),
            mark(5, 33..43, remember_style()),
            mark(6, 2..3, dim_style()),
            mark(6, 8..30, dim_style()),
            mark(8, 0..HINT.len(), dim_style()),
        ]
    );
    // Deny focused with no feedback: the prompt is dim and bold.
    press(&mut queue, Key::Down, 2);
    assert_eq!(row(&queue, 6), "▸ 4 Deny  type to add feedback");
    let marks = rendered(&queue).marks;
    assert!(marks.contains(&mark(6, 6..10, bold_style())), "{marks:?}");
    assert!(
        marks.contains(&mark(6, 10..32, dim_bold_style())),
        "{marks:?}"
    );
}

#[test]
fn long_feedback_scrolls_to_its_end_so_the_row_never_wraps() {
    let mut queue = folded(&[standing_ask(S_A, "a_1", "r_1", "echo hi")]);
    for ch in "y".repeat(96).chars().chain("END!".chars()) {
        press(&mut queue, Key::Char(ch), 1);
    }
    // The prefix takes 8 cells, so 72 stay for the tail: the row shows
    // the feedback's end on one 80-cell row, with the cursor after it.
    let deny = row(&queue, 4);
    assert_eq!(crate::format::width(&deny), 80);
    assert!(deny.ends_with("END!"), "{deny}");
    assert_eq!(rendered(&queue).caret, Some((4, 80)));
    // Away from deny the row stays scrolled, with no caret.
    press(&mut queue, Key::Up, 1);
    assert_eq!(crate::format::width(&row(&queue, 4)), 80);
    assert_eq!(rendered(&queue).caret, None);
}

#[test]
fn feedback_that_exactly_fits_shows_whole_and_one_cell_more_scrolls() {
    // The prefix takes 8 cells: a 70-character feedback with its two
    // spaces fits 80 exactly and shows whole.
    let mut queue = folded(&[standing_ask(S_A, "a_1", "r_1", "echo hi")]);
    for _ in 0..70 {
        press(&mut queue, Key::Char('x'), 1);
    }
    assert_eq!(row(&queue, 4), format!("▸ 2 Deny  {}", "x".repeat(70)));
    assert_eq!(rendered(&queue).caret, Some((4, 80)));
    // One cell more: the leading space gives way to the feedback's end.
    let mut queue = folded(&[standing_ask(S_A, "a_1", "r_1", "echo hi")]);
    for _ in 0..71 {
        press(&mut queue, Key::Char('x'), 1);
    }
    assert_eq!(row(&queue, 4), format!("▸ 2 Deny {}", "x".repeat(71)));
    assert_eq!(rendered(&queue).caret, Some((4, 80)));
}

#[test]
fn a_long_rule_prefix_stays_complete_on_remembering_rows() {
    let prefix = "p".repeat(100);
    let queue = folded(&[review(
        S_A,
        "r_1",
        serde_json::json!({"rule": {"subject": "npm test --watch", "prefix": prefix}}),
    )]);
    let shown = rendered(&queue);
    // Scrolling is Deny feedback only: the remembering rows keep the
    // whole prefix, past the width if needed.
    for head in ["Allow for this session", "Always allow in this project"] {
        let row = shown
            .lines
            .iter()
            .find(|row| row.contains(head))
            .cloned()
            .unwrap_or_default();
        assert!(row.contains(&format!("{prefix} *")), "{row}");
        assert!(crate::format::width(&row) > 80, "{row}");
    }
}

#[test]
fn each_choice_sends_its_answer() {
    let cases = [
        (
            0,
            serde_json::json!({"request_id": "r_1", "decision": "allow"}),
        ),
        (
            1,
            serde_json::json!({"request_id": "r_1", "decision": "allow",
            "remember": {"scope": "session", "prefix": "npm test"}}),
        ),
        (
            2,
            serde_json::json!({"request_id": "r_1", "decision": "allow",
            "remember": {"scope": "project", "prefix": "npm test"}}),
        ),
        (
            3,
            serde_json::json!({"request_id": "r_1", "decision": "deny"}),
        ),
    ];
    for (downs, args) in cases {
        let mut queue = folded(&[offering(S_A, "r_1")]);
        press(&mut queue, Key::Down, downs);
        assert_eq!(
            answer(&mut queue),
            serde_json::json!({"id": "c_1", "command": "reply", "session_id": S_A, "args": args}),
            "{downs} down"
        );
        assert!(queue.panel(80).is_none());
        assert!(queue.badge(0).is_none());
    }
}

#[test]
fn feedback_goes_with_deny_only_when_it_has_text() {
    let mut queue = folded(&[offering(S_A, "r_1")]);
    press(&mut queue, Key::Char(' '), 3);
    assert_eq!(
        answer(&mut queue)["args"],
        serde_json::json!({"request_id": "r_1", "decision": "deny"})
    );
    queue.fold(&offering(S_A, "r_2"));
    for ch in "use pnpmx".chars() {
        press(&mut queue, Key::Char(ch), 1);
    }
    press(&mut queue, Key::Backspace, 1);
    assert_eq!(row(&queue, 6), "▸ 4 Deny  use pnpm");
    assert_eq!(
        answer(&mut queue)["args"],
        serde_json::json!({"request_id": "r_2", "decision": "deny", "feedback": "use pnpm"})
    );
}

#[test]
fn typing_or_backspace_moves_the_cursor_to_deny() {
    let mut queue = folded(&[offering(S_A, "r_1")]);
    press(&mut queue, Key::Char('x'), 1);
    assert!(row(&queue, 6).starts_with("▸"));
    press(&mut queue, Key::Up, 3);
    assert_eq!(row(&queue, 3), "▸ 1 Allow once");
    press(&mut queue, Key::Backspace, 1);
    assert_eq!(row(&queue, 6), "▸ 4 Deny  type to add feedback");
}

#[test]
fn up_and_down_stop_at_the_ends() {
    // A standing ask offers no rule: one Down reaches deny, and more stay.
    let mut queue = folded(&[standing_ask(S_A, "a_1", "r_1", "echo hi")]);
    press(&mut queue, Key::Down, 5);
    assert_eq!(answer(&mut queue)["args"]["decision"], "deny");
    queue.fold(&offering(S_A, "r_2"));
    press(&mut queue, Key::Down, 5);
    press(&mut queue, Key::Up, 2);
    assert_eq!(answer(&mut queue)["args"]["remember"]["scope"], "session");
    queue.fold(&offering(S_A, "r_3"));
    press(&mut queue, Key::Down, 1);
    press(&mut queue, Key::Up, 5);
    assert_eq!(
        answer(&mut queue)["args"],
        serde_json::json!({"request_id": "r_3", "decision": "allow"})
    );
}

#[test]
fn the_panel_says_why_it_asked_and_tints_an_escalation() {
    let cases = [
        (
            serde_json::json!({"escalation": {"cause": "consecutive_blocks", "reason": "rm -rf"}}),
            "runs a command · the reviewer blocked the last calls: rm -rf",
        ),
        (
            serde_json::json!({"escalation": {"cause": "session_blocks", "reason": "too many"}}),
            "runs a command · the reviewer has blocked too many calls: too many",
        ),
        (
            serde_json::json!({"escalation": {"cause": "reviewer_failed",
                "error": {"code": "io_failed", "message": "no model"}}}),
            "runs a command · the reviewer failed",
        ),
    ];
    for (extra, facts) in cases {
        let queue = folded(&[review(S_A, "r_1", extra)]);
        assert_eq!(row(&queue, 1), facts);
        assert_eq!(
            queue.panel(80).map(|panel| panel.alert),
            Some(true),
            "{facts}"
        );
    }
    let queue = folded(&[review(S_A, "r_1", serde_json::json!({}))]);
    assert_eq!(row(&queue, 1), "runs a command · no rule allows this call");
    assert_eq!(queue.panel(80).map(|panel| panel.alert), Some(false));
    let queue = folded(&[request(
        S_A,
        "a_1",
        "r_1",
        serde_json::json!({"step": "standing_ask",
            "standing_rule": {"scope": "project", "prefix": "git push"}}),
    )]);
    assert_eq!(
        row(&queue, 1),
        "runs a command · asked by the project rule git push"
    );
}

#[test]
fn an_irreversible_call_says_so_in_the_header_only_when_not_reversible() {
    let irreversible = folded(&[request(
        S_A,
        "a_1",
        "r_1",
        serde_json::json!({"reversible": false, "step": "review"}),
    )]);
    let left = format!("Approval 1 of 1 · {S_A} · irreversible");
    let pad = 80_usize
        .saturating_sub(crate::format::width(&left).saturating_add("Esc puts it aside".len()))
        .max(1);
    assert_eq!(
        header(&irreversible),
        format!("{left}{}{}", " ".repeat(pad), "Esc puts it aside")
    );
    let marks = rendered(&irreversible).marks;
    assert!(
        marks
            .iter()
            .any(|mark| mark.line == 0 && mark.style == loud_style()),
        "{marks:?}"
    );
    // Reversible: the header ends at the session, with no loud run.
    let queue = folded(&[request(
        S_A,
        "a_1",
        "r_1",
        serde_json::json!({"reversible": true, "step": "review"}),
    )]);
    assert_eq!(header(&queue), header_text(1, 1, S_A));
    assert!(
        rendered(&queue)
            .marks
            .iter()
            .all(|mark| mark.style != loud_style()),
        "{marks:?}"
    );
}

#[test]
fn a_raw_string_argument_shows_as_typed() {
    let queue = folded(&[
        call(S_A, "a_1", "shell", serde_json::json!("not {json")),
        standing_ask(S_A, "a_1", "r_1", "echo hi"),
    ]);
    assert_eq!(row(&queue, 1), "shell not {json");
}

#[test]
fn requests_from_two_sessions_share_one_queue() {
    let mut queue = folded(&[
        call(S_B, "a_1", "read", serde_json::json!({"path": "b"})),
        standing_ask(S_A, "a_1", "r_1", "one"),
        standing_ask(S_B, "a_1", "r_1", "two"),
        standing_ask(S_A, "a_2", "r_2", "three"),
    ]);
    assert_eq!(header(&queue), header_text(1, 3, S_A));
    assert_eq!(answer(&mut queue)["session_id"], S_A);
    assert_eq!(header(&queue), header_text(1, 2, S_B));
    assert_eq!(row(&queue, 1), "read b");
    let answered = answer(&mut queue);
    assert_eq!(answered["session_id"], S_B);
    assert_eq!(answered["args"]["request_id"], "r_1");
    assert_eq!(header(&queue), header_text(1, 1, S_A));
}

#[test]
fn a_repeated_request_adds_nothing() {
    let line = standing_ask(S_A, "a_1", "r_1", "one");
    let queue = folded(&[line.clone(), line]);
    assert_eq!(header(&queue), header_text(1, 1, S_A));
}

/// A queue with three requests, the panel on the first.
fn three() -> Queue {
    folded(&[
        standing_ask(S_A, "a_1", "r_1", "p1"),
        standing_ask(S_A, "a_2", "r_2", "p2"),
        standing_ask(S_A, "a_3", "r_3", "p3"),
    ])
}

#[test]
fn esc_steps_through_the_queue_then_closes() {
    let mut queue = three();
    assert_eq!(header(&queue), header_text(1, 3, S_A));
    press(&mut queue, Key::Esc, 1);
    assert_eq!(header(&queue), header_text(2, 3, S_A));
    press(&mut queue, Key::Esc, 1);
    assert_eq!(header(&queue), header_text(3, 3, S_A));
    assert!(queue.badge(0).is_none());
    press(&mut queue, Key::Esc, 1);
    assert!(queue.panel(80).is_none());
    assert_eq!(
        queue.badge(0).as_deref(),
        Some("  ⚠ 3 approvals waiting · click to reopen")
    );
    // A closed panel takes no key.
    assert_eq!(queue.on_key(&Key::Esc), None);
    assert_eq!(queue.answer("c_1"), None);
}

#[test]
fn scrolling_keys_pass_through_the_open_panel() {
    let mut queue = three();
    for key in [Key::PageUp, Key::PageDown, Key::End, Key::CtrlC] {
        assert_eq!(queue.on_key(&key), None, "{key:?}");
    }
}

#[test]
fn a_request_after_one_put_aside_waits_behind_the_badge() {
    let mut queue = folded(&[standing_ask(S_A, "a_1", "r_1", "one")]);
    press(&mut queue, Key::Esc, 1);
    queue.fold(&standing_ask(S_A, "a_2", "r_2", "two"));
    assert!(queue.panel(80).is_none());
    assert_eq!(
        queue.badge(0).as_deref(),
        Some("  ⚠ 2 approvals waiting · click to reopen")
    );
    // Once the one put aside resolves, a new request opens at itself.
    queue.fold(&resolved(S_A, "r_1"));
    queue.fold(&resolved(S_A, "r_2"));
    assert!(queue.badge(0).is_none());
    queue.fold(&standing_ask(S_A, "a_3", "r_3", "three"));
    assert_eq!(
        row(&queue, 1),
        "runs a command · asked by the global rule three"
    );
}

#[test]
fn a_request_while_the_panel_is_open_joins_the_queue() {
    let queue = folded(&[
        standing_ask(S_A, "a_1", "r_1", "one"),
        standing_ask(S_A, "a_2", "r_2", "two"),
    ]);
    assert_eq!(header(&queue), header_text(1, 2, S_A));
    assert_eq!(
        row(&queue, 1),
        "runs a command · asked by the global rule one"
    );
}

#[test]
fn open_first_reopens_at_the_first_request_with_its_feedback() {
    assert!(!Queue::default().open_first());
    let mut queue = three();
    press(&mut queue, Key::Char('x'), 1);
    press(&mut queue, Key::Esc, 3);
    assert!(queue.open_first());
    assert_eq!(header(&queue), header_text(1, 3, S_A));
    // Feedback typed for a request stays with it while it waits.
    assert_eq!(row(&queue, 4), "▸ 2 Deny  x");
}

#[test]
fn alt_a_moves_to_the_next_wrapping() {
    let mut queue = three();
    press(&mut queue, Key::AltA, 1);
    assert_eq!(header(&queue), header_text(2, 3, S_A));
    press(&mut queue, Key::AltA, 2);
    assert_eq!(header(&queue), header_text(1, 3, S_A));
    // Alone in the queue, it stays.
    let mut queue = folded(&[standing_ask(S_A, "a_1", "r_1", "one")]);
    press(&mut queue, Key::AltA, 1);
    assert_eq!(header(&queue), header_text(1, 1, S_A));
}

#[test]
fn a_restored_reply_puts_its_request_back_where_it_was() {
    let mut queue = three();
    press(&mut queue, Key::AltA, 1);
    answer(&mut queue);
    assert_eq!(header(&queue), header_text(2, 2, S_A));
    queue.restore("c_9");
    assert_eq!(header(&queue), header_text(2, 2, S_A));
    queue.restore("c_1");
    assert_eq!(header(&queue), header_text(3, 3, S_A));
    press(&mut queue, Key::AltA, 1);
    assert_eq!(
        row(&queue, 1),
        "runs a command · asked by the global rule p1"
    );
    press(&mut queue, Key::AltA, 1);
    assert_eq!(
        row(&queue, 1),
        "runs a command · asked by the global rule p2"
    );
}

#[test]
fn a_restored_reply_reopens_a_closed_panel() {
    let mut queue = folded(&[standing_ask(S_A, "a_1", "r_1", "one")]);
    answer(&mut queue);
    assert!(queue.panel(80).is_none());
    queue.restore("c_1");
    assert_eq!(header(&queue), header_text(1, 1, S_A));
}

#[test]
fn a_reopened_request_is_no_longer_put_aside() {
    let mut queue = folded(&[standing_ask(S_A, "a_1", "r_1", "one")]);
    press(&mut queue, Key::Esc, 1);
    assert!(queue.open_first());
    answer(&mut queue);
    assert!(queue.panel(80).is_none());
    queue.restore("c_1");
    // Nothing waits put aside, so the request back opens the panel.
    assert_eq!(header(&queue), header_text(1, 1, S_A));
}

#[test]
fn a_resolved_request_leaves_the_queue() {
    let mut queue = three();
    // Requests never seen change nothing.
    queue.fold(&resolved(S_A, "r_9"));
    queue.fold(&resolved(S_B, "r_1"));
    assert_eq!(header(&queue), header_text(1, 3, S_A));
    answer(&mut queue);
    // An answered request resolving leaves the shown one in place, and a
    // later restore of its reply puts nothing back.
    queue.fold(&resolved(S_A, "r_1"));
    assert_eq!(header(&queue), header_text(1, 2, S_A));
    queue.restore("c_1");
    assert_eq!(header(&queue), header_text(1, 2, S_A));
    // The shown request resolving elsewhere moves the panel on.
    queue.fold(&resolved(S_A, "r_2"));
    assert_eq!(header(&queue), header_text(1, 1, S_A));
    queue.fold(&resolved(S_A, "r_3"));
    assert!(queue.panel(80).is_none());
    assert!(queue.badge(0).is_none());
}

/// The queue through the app: keys, `/approvals`, and replies on the link.
mod through_the_app {
    use super::{S_A, S_B, call, envelope, standing_ask};
    use crate::app::{App, Effect};
    use crate::keys::Key;
    use crate::link::Line;
    use contract::clock::Clock;
    use std::path::PathBuf;

    /// An app connected to the hub and attached to `S_A`.
    fn attached() -> App {
        let mut app = App::new(PathBuf::from("/w"));
        let hello = contract::HubLine {
            kind: "hub_hello".to_owned(),
            ts: 0,
            schema_version: contract::SCHEMA_VERSION,
            payload: serde_json::Map::new(),
        };
        assert!(app.on_line(Line::Hub(hello)).is_empty());
        app.attach(contract::SessionId(S_A.to_owned()));
        app
    }

    /// Folds one session envelope, answering the `commands` a turn start
    /// asks for with no rows: a turn start may change the runnable set,
    /// so the terminal asks again at each one.
    fn feed(app: &mut App, line: contract::Envelope) {
        let session = line.session_id.clone();
        let out = app.on_line(Line::Session(line));
        // Nothing else sends: only a turn start asks, and only once.
        assert!(out.len() <= 1, "{out:?}");
        let Some(first) = out.first() else {
            return;
        };
        let asked = parse(first);
        assert_eq!(asked["command"], "commands");
        let answer = Line::Session(contract::Envelope {
            kind: "command_accepted".to_owned(),
            session_id: session,
            ts: 0,
            schema_version: contract::SCHEMA_VERSION,
            turn_id: None,
            action_id: None,
            seq: None,
            payload: serde_json::json!({"command_id": asked["id"],
                "result": {"commands": []}})
            .as_object()
            .cloned()
            .unwrap_or_default(),
        });
        assert!(app.on_line(answer).is_empty());
    }

    /// A `turn_started` with one message.
    fn turn_started(session: &str, text: &str) -> contract::Envelope {
        envelope(
            session,
            "turn_started",
            serde_json::json!({"input": [{"type": "message", "source": "driver",
                "content": [{"type": "text", "text": text}]}]}),
            "a_0",
        )
    }

    /// The single line an effect sends.
    fn one_line(effect: Effect) -> String {
        match effect {
            Effect::Send(lines) => {
                assert_eq!(lines.len(), 1);
                lines.into_iter().next().unwrap_or_default()
            }
            Effect::None
            | Effect::Quit
            | Effect::ListFiles
            | Effect::ReadImage(_)
            | Effect::Feedback { .. }
            | Effect::FindPause { .. }
            | Effect::Search { .. }
            | Effect::Editor { .. }
            | Effect::Exit(_)
            | Effect::Copy(_)
            | Effect::OpenLink(_)
            | Effect::OpenFile(_) => {
                panic!("expected one line")
            }
        }
    }

    /// Parses one command line.
    fn parse(line: &str) -> serde_json::Value {
        serde_json::from_str(line).unwrap_or_default()
    }

    /// Types `text` and presses Enter, returning the effect.
    fn send(app: &mut App, text: &str, now: std::time::Instant) -> Effect {
        for ch in text.chars() {
            assert_eq!(app.on_key(Key::Char(ch), now), Effect::None);
        }
        app.on_key(Key::Enter, now)
    }

    /// The panel's lines, or none when it is closed.
    fn panel(app: &App) -> Vec<String> {
        app.panel().map(|panel| panel.lines).unwrap_or_default()
    }

    /// The panel's header line.
    fn header(app: &App) -> String {
        panel(app).first().cloned().unwrap_or_default()
    }

    /// The expected header at the app's default width: the column is 80
    /// wide and the panel pads to its inset of 78.
    fn app_header(k: usize, n: usize, session: &str) -> String {
        super::header_at(k, n, session, 78)
    }

    /// The panel's line at `row`.
    fn row(app: &App, row: usize) -> String {
        panel(app).get(row).cloned().unwrap_or_default()
    }

    /// Presses `key` `times` times, each doing nothing visible to the hub.
    fn press(app: &mut App, key: Key, times: usize, now: std::time::Instant) {
        for _ in 0..times {
            assert_eq!(app.on_key(key.clone(), now), Effect::None);
        }
    }

    /// The reply an Enter sends, with its `id` taken out.
    fn reply(app: &mut App, now: std::time::Instant) -> serde_json::Value {
        let mut value = parse(&one_line(app.on_key(Key::Enter, now)));
        let id = value
            .as_object_mut()
            .and_then(|line| line.remove("id"))
            .unwrap_or_default();
        assert!(id.as_str().is_some_and(|id| id.starts_with("c_")));
        value
    }

    #[test]
    fn a_standing_ask_opens_the_panel_and_enter_allows_once() {
        let now = fakes::clock::FakeClock::new().now();
        let mut app = attached();
        feed(
            &mut app,
            call(
                S_A,
                "a_1",
                "shell",
                serde_json::json!({"command": "echo hi"}),
            ),
        );
        feed(&mut app, standing_ask(S_A, "a_1", "r_1", "echo hi"));
        let facts = "runs a command · asked by the global rule echo hi";
        assert_eq!(
            panel(&app),
            vec![
                app_header(1, 1, S_A),
                "shell echo hi".to_owned(),
                facts.to_owned(),
                String::new(),
                "▸ 1 Allow once".to_owned(),
                "  2 Deny  type to add feedback".to_owned(),
                String::new(),
                super::HINT.to_owned(),
            ]
        );
        assert_eq!(app.panel().map(|panel| panel.alert), Some(false));
        assert_eq!(
            reply(&mut app, now),
            serde_json::json!({"command": "reply", "session_id": S_A,
                "args": {"request_id": "r_1", "decision": "allow"}})
        );
        assert!(app.panel().is_none());
        assert!(app.badge().is_none());
    }

    /// An app with three requests, the panel on the first.
    fn three() -> App {
        let mut app = attached();
        for n in 1..=3 {
            feed(
                &mut app,
                standing_ask(S_A, &format!("a_{n}"), &format!("r_{n}"), &format!("p{n}")),
            );
        }
        app
    }

    #[test]
    fn esc_steps_through_the_queue_then_closes() {
        let now = fakes::clock::FakeClock::new().now();
        let mut app = three();
        feed(&mut app, turn_started(S_A, "hi"));
        assert_eq!(header(&app), app_header(1, 3, S_A));
        // Esc never cancels while the panel is open.
        assert_eq!(app.on_key(Key::Esc, now), Effect::None);
        assert_eq!(header(&app), app_header(2, 3, S_A));
        assert_eq!(app.on_key(Key::Esc, now), Effect::None);
        assert_eq!(header(&app), app_header(3, 3, S_A));
        assert!(app.badge().is_none());
        assert_eq!(app.on_key(Key::Esc, now), Effect::None);
        assert!(app.panel().is_none());
        assert_eq!(
            app.badge().as_deref(),
            Some("  ⚠ 3 approvals waiting · click to reopen")
        );
        // With the panel closed, Esc cancels a busy turn again.
        let line = one_line(app.on_key(Key::Esc, now));
        assert_eq!(parse(&line)["command"], "cancel");
    }

    #[test]
    fn slash_approvals_reopens_at_the_first_request() {
        let now = fakes::clock::FakeClock::new().now();
        let mut app = three();
        for ch in "x".chars() {
            app.on_key(Key::Char(ch), now);
        }
        press(&mut app, Key::Esc, 3, now);
        assert!(app.panel().is_none());
        assert_eq!(send(&mut app, "/approvals ", now), Effect::None);
        assert_eq!(app.draft(), "");
        assert_eq!(header(&app), app_header(1, 3, S_A));
        // Feedback typed for a request stays with it while it waits.
        assert_eq!(row(&app, 4), "▸ 2 Deny  x");
    }

    #[test]
    fn slash_approvals_with_nothing_waiting_says_so() {
        let now = fakes::clock::FakeClock::new().now();
        let mut app = attached();
        assert_eq!(send(&mut app, "/approvals", now), Effect::None);
        assert_eq!(app.draft(), "");
        assert_eq!(app.notice(), Some("No requests waiting."));
        assert!(app.panel().is_none());
    }

    #[test]
    fn alt_a_opens_and_moves_to_the_next_wrapping() {
        let now = fakes::clock::FakeClock::new().now();
        let mut app = attached();
        press(&mut app, Key::AltA, 1, now);
        assert_eq!(app.notice(), Some("No requests waiting."));
        let mut app = three();
        press(&mut app, Key::Esc, 3, now);
        press(&mut app, Key::AltA, 1, now);
        assert_eq!(header(&app), app_header(1, 3, S_A));
        press(&mut app, Key::AltA, 1, now);
        assert_eq!(header(&app), app_header(2, 3, S_A));
        press(&mut app, Key::AltA, 2, now);
        assert_eq!(header(&app), app_header(1, 3, S_A));
    }

    #[test]
    fn up_down_and_alt_a_do_nothing_to_the_draft_when_closed() {
        let now = fakes::clock::FakeClock::new().now();
        let mut app = attached();
        app.on_key(Key::Char('a'), now);
        press(&mut app, Key::Up, 1, now);
        press(&mut app, Key::Down, 1, now);
        assert_eq!(app.draft(), "a");
        assert!(app.panel().is_none());
    }

    /// The id of the reply an Enter sends.
    fn reply_id(app: &mut App, now: std::time::Instant) -> String {
        parse(&one_line(app.on_key(Key::Enter, now)))["id"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    #[test]
    fn a_rejected_reply_reopens_a_closed_panel() {
        let now = fakes::clock::FakeClock::new().now();
        let mut app = attached();
        feed(&mut app, standing_ask(S_A, "a_1", "r_1", "one"));
        let id = reply_id(&mut app, now);
        assert!(app.panel().is_none());
        feed(
            &mut app,
            envelope(
                S_A,
                "command_rejected",
                serde_json::json!({"command_id": id, "code": "stale_request", "message": "gone"}),
                "a_0",
            ),
        );
        assert_eq!(header(&app), app_header(1, 1, S_A));
    }

    #[test]
    fn a_reply_with_the_link_down_sends_nothing_and_keeps_the_request() {
        let now = fakes::clock::FakeClock::new().now();
        let mut app = attached();
        feed(&mut app, standing_ask(S_A, "a_1", "r_1", "one"));
        app.disconnected();
        assert_eq!(app.on_key(Key::Enter, now), Effect::None);
        assert_eq!(header(&app), app_header(1, 1, S_A));
    }

    #[test]
    fn a_failed_reply_write_keeps_the_request() {
        let now = fakes::clock::FakeClock::new().now();
        let mut app = attached();
        feed(&mut app, standing_ask(S_A, "a_1", "r_1", "one"));
        let line = one_line(app.on_key(Key::Enter, now));
        assert!(app.panel().is_none());
        app.write_failed(&[line]);
        assert_eq!(header(&app), app_header(1, 1, S_A));
        assert_eq!(app.draft(), "");
    }

    #[test]
    fn the_conversation_gives_up_rows_for_the_panel_and_the_badge() {
        let now = fakes::clock::FakeClock::new().now();
        let mut app = attached();
        app.set_size(60, 20);
        assert_eq!(app.conversation_height(), 17);
        feed(&mut app, standing_ask(S_A, "a_1", "r_1", "one"));
        // Header, facts, two choices and the hint with the panel's edges
        // replace the input line.
        assert_eq!(app.conversation_height(), 10);
        feed(
            &mut app,
            call(S_A, "a_2", "shell", serde_json::json!("w ".repeat(35))),
        );
        feed(&mut app, standing_ask(S_A, "a_2", "r_2", "two"));
        app.on_key(Key::AltA, now);
        // The call wraps onto two rows.
        assert_eq!(app.conversation_height(), 8);
        press(&mut app, Key::Esc, 1, now);
        assert!(app.panel().is_none());
        assert_eq!(app.conversation_height(), 16);
    }

    #[test]
    fn another_sessions_request_joins_and_its_reply_names_that_session() {
        let now = fakes::clock::FakeClock::new().now();
        let mut app = attached();
        feed(&mut app, standing_ask(S_B, "a_1", "r_1", "two"));
        // Another session's other lines stay out.
        feed(&mut app, turn_started(S_B, "hi"));
        assert!(app.lines().is_empty());
        assert_eq!(header(&app), app_header(1, 1, S_B));
        // Typing goes to the feedback; the draft is untouched.
        for ch in "no".chars() {
            assert_eq!(app.on_key(Key::Char(ch), now), Effect::None);
        }
        assert_eq!(app.draft(), "");
        assert_eq!(
            reply(&mut app, now),
            serde_json::json!({"command": "reply", "session_id": S_B,
                "args": {"request_id": "r_1", "decision": "deny", "feedback": "no"}})
        );
    }
}

#[test]
fn ctrl_g_and_ctrl_r_do_nothing_in_the_panel() {
    let mut queue = folded(&[offering(S_A, "r_1")]);
    let before = panel(&queue);
    press(&mut queue, Key::CtrlG, 1);
    press(&mut queue, Key::CtrlR, 1);
    assert_eq!(panel(&queue), before);
}

#[test]
fn the_steering_keys_do_nothing_while_the_panel_is_open() {
    let mut queue = three();
    let before = panel(&queue);
    press(&mut queue, Key::AltUp, 1);
    press(&mut queue, Key::AltDown, 1);
    press(&mut queue, Key::AltX, 1);
    assert_eq!(panel(&queue), before);
    // With the panel closed they are not the panel's.
    assert_eq!(Queue::default().on_key(&Key::AltX), None);
}

#[test]
fn an_unknown_kind_naming_a_waiting_request_changes_nothing() {
    let mut queue = folded(&[standing_ask(S_A, "a_1", "r_1", "one")]);
    queue.fold(&envelope(
        S_A,
        "notice",
        serde_json::json!({"request_id": "r_1", "message": "hi"}),
        "a_1",
    ));
    assert_eq!(header(&queue), header_text(1, 1, S_A));
}

#[test]
fn permission_resolved_still_removes_an_approval() {
    let mut queue = folded(&[standing_ask(S_A, "a_1", "r_1", "one")]);
    queue.fold(&resolved(S_A, "r_1"));
    assert!(queue.panel(80).is_none());
    assert!(queue.badge(0).is_none());
}

#[test]
fn a_call_with_no_name_draws_no_empty_tool_run() {
    let queue = folded(&[
        call(S_A, "a_1", "", serde_json::json!({})),
        request(S_A, "a_1", "r_1", serde_json::json!({"step": "review"})),
    ]);
    let shown = rendered(&queue);
    assert_eq!(shown.lines[1], "");
    assert!(
        shown.marks.iter().all(|mark| !mark.bytes.is_empty()),
        "{:?}",
        shown.marks
    );
}

/// In secondary: the title's session and count.
fn secondary_style() -> Style {
    Style::new().fg(Role::Secondary.color())
}

/// Bold in secondary: the title's "Question" and its place.
fn secondary_bold_style() -> Style {
    secondary_style().add_modifier(Modifier::BOLD)
}

/// An `interaction_requested` form `id` from `session` with one question.
fn form_asked(session: &str, id: &str) -> contract::Envelope {
    contract::Envelope {
        kind: "interaction_requested".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::json!({"request_id": id, "kind": "form", "fields": [
            {"header": "Base", "question": "Which branch?",
             "options": [{"label": "main"}, {"label": "dev"}]}]})
        .as_object()
        .cloned()
        .unwrap_or_default(),
    }
}

/// An `interaction_requested` confirm `id` from `session`.
fn confirm_asked(session: &str, id: &str) -> contract::Envelope {
    contract::Envelope {
        kind: "interaction_requested".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::json!({"request_id": id, "kind": "confirm",
            "prompt": "Continue?"})
        .as_object()
        .cloned()
        .unwrap_or_default(),
    }
}

/// A form's title at width 80 for `session` with `questions` questions and
/// `place`, or a one-question title when `questions` is `None`.
fn question_title(
    session: &str,
    questions: Option<usize>,
    place: Option<(usize, usize)>,
) -> String {
    let mut left = format!("Question from {session}");
    if let Some(questions) = questions {
        left.push_str(&format!(" · {questions} questions, any can be skipped"));
    }
    if let Some((k, n)) = place {
        left.push_str(&format!(" · {k} of {n}"));
    }
    let pad = 80_usize
        .saturating_sub(crate::format::width(&left).saturating_add("Esc: chat about this".len()))
        .max(1);
    format!("{left}{}{}", " ".repeat(pad), "Esc: chat about this")
}

#[test]
fn a_single_form_title_has_no_place_suffix() {
    let queue = folded(&[form_asked(S_A, "r_1")]);
    assert_eq!(header(&queue), question_title(S_A, Some(1), None));
    assert!(!header(&queue).contains(" of "), "{}", header(&queue));
    // "Question" is the only bold secondary run.
    let marks: Vec<_> = rendered(&queue)
        .marks
        .iter()
        .filter(|mark| mark.line == 0 && mark.style == secondary_bold_style())
        .map(|mark| mark.bytes.clone())
        .collect();
    assert!(matches!(marks.as_slice(), [only] if *only == (0..8)));
}

#[test]
fn two_forms_title_the_first_one_of_two() {
    let queue = folded(&[form_asked(S_A, "r_1"), form_asked(S_A, "r_2")]);
    assert_eq!(header(&queue), question_title(S_A, Some(1), Some((1, 2))));
    let shown = rendered(&queue);
    let line = shown.lines.first().cloned().unwrap_or_default();
    let place: Vec<_> = shown
        .marks
        .iter()
        .filter(|mark| mark.line == 0 && mark.style == secondary_bold_style())
        .map(|mark| mark.bytes.clone())
        .collect();
    assert_eq!(place.len(), 2);
    assert_eq!(&line[place[1].clone()], " · 1 of 2");
}

#[test]
fn alt_a_moves_to_the_second_form_title() {
    let mut queue = folded(&[form_asked(S_A, "r_1"), form_asked(S_A, "r_2")]);
    assert_eq!(queue.on_key(&Key::AltA), Some(PanelKey::Handled));
    assert_eq!(header(&queue), question_title(S_A, Some(1), Some((2, 2))));
}

#[test]
fn answering_the_first_form_leaves_the_second_without_a_suffix() {
    let mut queue = folded(&[form_asked(S_A, "r_1"), form_asked(S_A, "r_2")]);
    press(&mut queue, Key::Enter, 1);
    assert!(queue.answer("c_1").is_some());
    assert_eq!(header(&queue), question_title(S_A, Some(1), None));
}

#[test]
fn three_forms_title_one_of_three() {
    let queue = folded(&[
        form_asked(S_A, "r_1"),
        form_asked(S_A, "r_2"),
        form_asked(S_A, "r_3"),
    ]);
    assert_eq!(header(&queue), question_title(S_A, Some(1), Some((1, 3))));
}

#[test]
fn a_form_behind_an_approval_counts_both() {
    let mut queue = folded(&[
        standing_ask(S_A, "a_1", "r_a", "ls"),
        form_asked(S_A, "r_f"),
    ]);
    assert!(queue.open_first());
    assert_eq!(header(&queue), header_text(1, 2, S_A));
    assert_eq!(queue.on_key(&Key::Esc), Some(PanelKey::Handled));
    assert_eq!(header(&queue), question_title(S_A, Some(1), Some((2, 2))));
}

#[test]
fn the_title_names_the_session_or_falls_back_to_its_id() {
    let queue = folded(&[form_asked(S_A, "r_1")]);
    assert_eq!(
        named(&queue, Some("run it"))
            .lines
            .first()
            .cloned()
            .unwrap_or_default(),
        question_title("run it", Some(1), None)
    );
    assert_eq!(header(&queue), question_title(S_A, Some(1), None));
    assert_eq!(
        named(&queue, Some(""))
            .lines
            .first()
            .cloned()
            .unwrap_or_default(),
        question_title(S_A, Some(1), None)
    );
    // Control characters in a name draw as spaces.
    let shown = named(&queue, Some("a\x1bb"));
    let line = shown.lines.first().cloned().unwrap_or_default();
    assert!(line.contains("from a b"), "{line}");
    assert!(!line.chars().any(|ch| ch.is_control()), "{line}");
}

#[test]
fn a_one_question_title_has_no_count() {
    let queue = folded(&[confirm_asked(S_A, "r_c")]);
    assert_eq!(header(&queue), question_title(S_A, None, None));
    assert!(!header(&queue).contains("questions"), "{}", header(&queue));
    let shown = rendered(&queue);
    let marks: Vec<_> = shown
        .marks
        .iter()
        .filter(|mark| mark.line == 0)
        .map(|mark| (mark.bytes.clone(), mark.style))
        .collect();
    assert_eq!(
        marks,
        [
            (0..8, secondary_bold_style()),
            (8..32, secondary_style()),
            (60..80, dim_style()),
        ]
    );
}

#[test]
fn two_one_question_titles_show_their_place() {
    let queue = folded(&[confirm_asked(S_A, "r_c1"), confirm_asked(S_A, "r_c2")]);
    assert_eq!(header(&queue), question_title(S_A, None, Some((1, 2))));
}

/// The queue folds requests through the borrowed decoder: a tool call,
/// a permission request and an interaction question queue, and a line
/// missing a required key queues nothing.
#[test]
fn queue_fold_reads_requests_through_the_borrowed_decoder() {
    let tool = call(S_A, "a_1", "read", serde_json::json!({"path": "a.png"}));
    assert!(
        crate::app::read_ref::<contract::events::ToolCallRequested>(&tool.payload).is_some(),
        "the tool call decodes"
    );
    let ask = standing_ask(S_A, "a_1", "r_1", "npm");
    assert!(
        crate::app::read_ref::<contract::events::PermissionRequested>(&ask.payload).is_some(),
        "the permission request decodes"
    );
    let confirm = confirm_asked(S_A, "r_2");
    assert!(
        crate::app::read_ref::<contract::events::InteractionRequested>(&confirm.payload).is_some(),
        "the interaction question decodes"
    );
    let queue = folded(&[tool, ask, confirm]);
    assert!(!panel(&queue).is_empty(), "the three requests queue");
    // Each malformed line misses a required key, so it queues nothing.
    for line in [
        envelope(
            S_A,
            "tool_call_requested",
            serde_json::json!({"name": "read"}),
            "a_9",
        ),
        envelope(
            S_A,
            "permission_requested",
            serde_json::json!({"request_id": "r_9", "effects": ["executes"], "reversible": true}),
            "a_9",
        ),
        envelope(
            S_A,
            "interaction_requested",
            serde_json::json!({"request_id": "r_9", "kind": "confirm"}),
            "a_9",
        ),
    ] {
        assert!(panel(&folded(&[line])).is_empty(), "no panel opens");
    }
}
