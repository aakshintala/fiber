//! Tests for the conversation's geometry: the size, the scroll position and
//! the window of resident pages, against `Screen` alone.

use contract::{ActionId, Envelope, Seq, SessionId};
use serde_json::{Value, json};

use super::Screen;
use crate::app::Target;

/// A durable line numbered `seq`.
fn envelope(seq: &mut u64, kind: &str, action: Option<&str>, payload: Value) -> Envelope {
    *seq += 1;
    Envelope {
        kind: kind.to_owned(),
        session_id: SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
        ts: 1_000 + *seq,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|id| ActionId(id.to_owned())),
        seq: Some(Seq(*seq - 1)),
        payload: payload.as_object().cloned().unwrap_or_default(),
    }
}

/// A screen holding `turns` turns, each a prompt, a thinking block and a
/// reply, following.
fn screen(turns: usize) -> Screen {
    let mut seq = 0;
    let mut screen = Screen::new();
    for turn in 0..turns {
        let message = format!("a_m{turn}");
        let thought = format!("a_r{turn}");
        let prompt = format!("prompt {turn}");
        let lines = [
            envelope(
                &mut seq,
                "turn_started",
                None,
                json!({"input": [{"type": "message", "source": "driver",
                    "content": [{"type": "text", "text": prompt}]}]}),
            ),
            envelope(&mut seq, "step_started", None, json!({})),
            envelope(
                &mut seq,
                "assistant_message_started",
                Some(&message),
                json!({}),
            ),
            envelope(&mut seq, "reasoning_started", Some(&thought), json!({})),
            envelope(
                &mut seq,
                "reasoning_completed",
                Some(&thought),
                json!({"text": "weigh it"}),
            ),
            envelope(
                &mut seq,
                "text_completed",
                Some(&message),
                json!({"text": format!("reply {turn}")}),
            ),
            envelope(
                &mut seq,
                "assistant_message_completed",
                Some(&message),
                json!({"outcome": "completed"}),
            ),
            envelope(
                &mut seq,
                "turn_completed",
                None,
                json!({"outcome": "completed"}),
            ),
        ];
        for line in &lines {
            screen.pages_mut().apply(line);
        }
    }
    screen
}

/// Every row the screen's pages count.
fn total(screen: &Screen) -> usize {
    screen.pages().index().total()
}

#[test]
fn a_new_screen_is_80_by_24_and_follows() {
    let screen = Screen::new();
    assert_eq!((screen.width(), screen.height()), (80, 24));
    assert_eq!(screen.top(), None);
    assert!(!screen.has_new());
}

#[test]
fn set_size_holds_at_least_one_by_one() {
    let mut screen = Screen::new();
    screen.set_size(0, 0);
    assert_eq!((screen.width(), screen.height()), (1, 1));
    screen.set_size(120, 40);
    assert_eq!((screen.width(), screen.height()), (120, 40));
}

#[test]
fn jump_sets_the_top_and_follow_clears_it_and_the_new_flag() {
    let mut screen = Screen::new();
    // No clamp until a settle: an empty screen keeps the row asked for.
    screen.jump(5);
    assert_eq!(screen.top(), Some(5));
    screen.changed();
    assert!(screen.has_new());
    screen.follow();
    assert_eq!(screen.top(), None);
    assert!(!screen.has_new());
}

#[test]
fn changed_marks_new_only_while_scrolled_up() {
    let mut screen = Screen::new();
    screen.changed();
    assert!(!screen.has_new());
    screen.jump(1);
    screen.changed();
    assert!(screen.has_new());
}

#[test]
fn page_up_moves_by_the_height_less_one_and_at_least_one() {
    let mut screen = screen(10);
    let total = total(&screen);
    assert!(total > 30, "{total} rows");
    screen.page(true, 10);
    assert_eq!(screen.top(), Some(total - 10 - 9));
    screen.page(true, 1);
    assert_eq!(screen.top(), Some(total - 10 - 10));
    screen.page(true, 0);
    assert_eq!(screen.top(), Some(total - 10 - 11));
    screen.page(false, 4);
    assert_eq!(screen.top(), Some(total - 10 - 8));
    screen.page(false, total);
    assert_eq!(screen.top(), None);
    // PageDown while following stays following.
    screen.page(false, 10);
    assert_eq!(screen.top(), None);
}

#[test]
fn settle_clamps_a_top_past_the_bottom() {
    let mut screen = screen(4);
    let total = total(&screen);
    screen.settle(5);
    assert_eq!(screen.top(), None, "a following screen keeps following");
    screen.jump(usize::MAX);
    screen.settle(5);
    assert_eq!(screen.top(), Some(total - 5));
    screen.jump(2);
    screen.settle(5);
    assert_eq!(screen.top(), Some(2));
}

#[test]
fn scroll_bar_is_the_view_top_and_the_total() {
    let mut screen = screen(4);
    let total = total(&screen);
    assert!(total > 8, "{total} rows");
    assert_eq!(screen.scroll_bar(5), (total - 5, total));
    assert_eq!(screen.scroll_bar(total + 5), (0, total));
    screen.jump(3);
    assert_eq!(screen.scroll_bar(5), (3, total));
    // A top past the bottom shows the bottom.
    screen.jump(total);
    assert_eq!(screen.scroll_bar(5), (total - 5, total));
}

#[test]
fn needs_is_empty_when_every_page_is_resident() {
    let mut screen = screen(60);
    assert!(screen.pages().index().pages().len() > 3, "too few pages");
    assert_eq!(screen.needs(5), Vec::new());
    // Settled at the bottom, the first page drops; scrolled back to the
    // top, the window needs it again.
    screen.settle(5);
    assert!(screen.pages().part(0).is_none(), "the first page stayed");
    assert_eq!(screen.needs(5), Vec::new());
    screen.jump(0);
    let needs = screen.needs(5);
    let first = &screen.pages().index().pages()[0];
    assert_eq!(
        needs.first(),
        Some(&(first.session.clone(), first.first_seq..=first.last_seq))
    );
}

#[test]
fn reveal_scrolls_up_to_a_row_above_the_top_and_follows_at_the_bottom() {
    let mut screen = screen(6);
    // At 6 columns a thinking block's line wraps to four rows.
    screen.set_size(6, 24);
    screen.wrap_at(6);
    let total = total(&screen);
    let mut items = screen.pages().focus_items();
    items.sort_by_key(|(row, _, _)| *row);
    // Every focus stop is one row: each card row draws whole.
    assert!(!items.is_empty());
    assert!(items.iter().all(|(_, rows, _)| *rows == 1));
    let (above, _, _) = items[4];
    let (below, _, _) = items[items.len() - 2];
    // An item above the top shows its row.
    screen.jump(100);
    assert_eq!(screen.top(), Some(100));
    screen.reveal(above, 2);
    assert_eq!(screen.top(), Some(above));
    // An item below the screen scrolls down just far enough.
    let end = below + 1;
    assert!(end - 2 < total - 2, "{items:?}");
    screen.reveal(below, 2);
    assert_eq!(screen.top(), Some(end - 2));
    // A height that holds every row puts the top at the bottom: following.
    screen.reveal(below, total);
    assert_eq!(screen.top(), None);
    // A row no item starts on changes nothing.
    screen.jump(2);
    screen.reveal(usize::MAX, 3);
    assert_eq!(screen.top(), Some(2));
}

#[test]
fn reveal_with_no_height_changes_nothing() {
    let mut screen = screen(6);
    let (first, _, _) = screen.pages().focus_items()[0];
    screen.jump(first + 3);
    screen.reveal(first, 0);
    assert_eq!(screen.top(), Some(first + 3));
}

#[test]
fn clear_drops_the_pages_and_follows() {
    let mut screen = screen(4);
    screen.jump(2);
    screen.changed();
    screen.clear();
    assert_eq!(total(&screen), 0);
    assert_eq!(screen.top(), None);
    assert!(!screen.has_new());
}

#[test]
fn open_marks_new_while_scrolled_up_and_reports_whether_it_changed() {
    let mut screen = screen(2);
    let target = screen
        .pages()
        .rows()
        .into_iter()
        .find_map(|(_, target)| target)
        .expect("a target");
    assert!(screen.open(target));
    assert!(!screen.has_new(), "following shows nothing new");
    screen.jump(0);
    assert!(!screen.open(Target::Group(999)));
    assert!(!screen.has_new(), "nothing opened");
    assert!(screen.open(target));
    assert!(screen.has_new());
}

#[test]
fn wrap_at_reports_a_new_width() {
    let mut screen = screen(1);
    assert!(!screen.wrap_at(80), "80 is the width already");
    assert!(screen.wrap_at(40));
    assert!(!screen.wrap_at(40));
    assert!(screen.wrap_at(0), "0 wraps at 1, a new width");
    assert!(!screen.wrap_at(1));
}

#[test]
fn a_frozen_screen_does_not_scroll() {
    let mut screen = screen(4);
    let height = 5;
    screen.scroll_rows(true, 3, height);
    let top = screen.top();
    assert!(top.is_some(), "scrolled up from the bottom");
    screen.set_frozen(true);
    assert!(screen.frozen());
    screen.scroll_rows(true, 2, height);
    screen.page(true, height);
    screen.jump(0);
    screen.reveal(1, height);
    assert_eq!(screen.top(), top, "nothing moved while frozen");
    assert!(
        !screen.wrap_at(40),
        "a frozen width stores without recounting"
    );
    assert_eq!(screen.pages().wrap_width(), 80);
    screen.set_frozen(false);
    assert!(!screen.frozen());
    assert_eq!(
        screen.pages().wrap_width(),
        40,
        "unfreezing applies the stored width once"
    );
    screen.scroll_rows(true, 2, height);
    assert_ne!(screen.top(), top, "unfrozen scrolls again");
}

/// A durable line numbered `seq` in segment `session`.
fn segment(
    seq: &mut u64,
    session: &str,
    kind: &str,
    action: Option<&str>,
    payload: Value,
) -> Envelope {
    let mut line = envelope(seq, kind, action, payload);
    line.session_id = SessionId(session.to_owned());
    line
}

/// One turn's eight lines under `session`, numbered from `seq`.
fn segment_turn(seq: &mut u64, session: &str, turn: usize) -> Vec<Envelope> {
    let message = format!("c_m{turn}");
    let thought = format!("c_r{turn}");
    let prompt = format!("prompt {turn}");
    [
        segment(
            seq,
            session,
            "turn_started",
            None,
            json!({"input": [{"type": "message", "source": "driver",
                "content": [{"type": "text", "text": prompt}]}]}),
        ),
        segment(seq, session, "step_started", None, json!({})),
        segment(
            seq,
            session,
            "assistant_message_started",
            Some(&message),
            json!({}),
        ),
        segment(seq, session, "reasoning_started", Some(&thought), json!({})),
        segment(
            seq,
            session,
            "reasoning_completed",
            Some(&thought),
            json!({"text": "weigh it"}),
        ),
        segment(
            seq,
            session,
            "text_completed",
            Some(&message),
            json!({"text": format!("reply {turn}")}),
        ),
        segment(
            seq,
            session,
            "assistant_message_completed",
            Some(&message),
            json!({"outcome": "completed"}),
        ),
        segment(
            seq,
            session,
            "turn_completed",
            None,
            json!({"outcome": "completed"}),
        ),
    ]
    .into_iter()
    .collect()
}

/// The ancestor's session: the chain's first segment.
const ANCESTOR: &str = "s_bbbbbbbbbbbbbbbb";
/// The own run's session: the chain's last segment.
const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

#[test]
fn the_window_over_the_seam_asks_each_side_under_its_session() {
    let mut screen = Screen::new();
    let mut log = Vec::new();
    let mut aseq = 0u64;
    for turn in 0..20 {
        let lines = segment_turn(&mut aseq, ANCESTOR, turn);
        for line in &lines {
            screen.pages_mut().apply(line);
        }
        log.extend(lines);
    }
    let mut dseq = 0u64;
    for turn in 20..30 {
        let lines = segment_turn(&mut dseq, "s_aaaaaaaaaaaaaaaa", turn);
        for line in &lines {
            screen.pages_mut().apply(line);
        }
        log.extend(lines);
    }
    // The seam cuts a page: no page holds both segments' lines.
    let seam = screen
        .pages()
        .index()
        .pages()
        .iter()
        .position(|page| page.session == SessionId(SESSION.to_owned()))
        .expect("the own run");
    assert!(seam > 1, "the ancestor spans pages");
    let joint = total(&screen);
    // Over the seam, the window holds pages from both sides.
    let start = screen.pages().index().start(seam);
    screen.jump(start);
    screen.settle(10);
    let window = screen.pages().index().window(start, 10);
    let sessions: Vec<SessionId> = window
        .map(|at| screen.pages().index().pages()[at].session.clone())
        .collect();
    assert!(
        sessions.iter().any(|session| session.0 == ANCESTOR),
        "the window holds an ancestor page"
    );
    assert!(
        sessions
            .iter()
            .any(|session| session.0 == "s_aaaaaaaaaaaaaaaa"),
        "the window holds the own run's page"
    );
    // Away and back, the seam pages dropped: each is asked under its
    // own session, and loading settles.
    screen.jump(0);
    screen.settle(10);
    screen.jump(start);
    screen.settle(10);
    let mut needs = screen.needs(10);
    assert!(!needs.is_empty(), "the seam pages dropped");
    assert!(
        needs.iter().any(|(session, _)| session.0 == ANCESTOR),
        "the ancestor side is asked under its session"
    );
    assert!(
        needs
            .iter()
            .any(|(session, _)| session.0 == "s_aaaaaaaaaaaaaaaa"),
        "the own side is asked under its session"
    );
    for (session, range) in std::mem::take(&mut needs) {
        let lines: Vec<Envelope> = log
            .iter()
            .filter(|line| {
                line.session_id == session && line.seq.is_some_and(|seq| range.contains(&seq))
            })
            .cloned()
            .collect();
        assert!(!lines.is_empty(), "no lines for {session:?} {range:?}");
        screen.pages_mut().load(&session, &lines);
    }
    assert!(screen.needs(10).is_empty());
    assert_eq!(total(&screen), joint, "the joint count spans the seam");
    assert_eq!(screen.scroll_bar(10).1, joint);
}

/// A screen with one turn holding one group of `calls` read calls, each
/// completed with `lines_each` lines of content and a read `details`.
/// The group's ledger and the calls in `opened` are opened. Returns the
/// screen with the scroll freed (following).
fn read_group(
    calls: usize,
    opened: &[bool],
    lines_each: usize,
    asking: &[bool],
) -> (Screen, Target, Vec<Target>) {
    let mut seq = 0;
    let mut screen = Screen::new();
    let mut feed = |screen: &mut Screen, kind: &str, action: Option<&str>, payload: Value| {
        screen
            .pages_mut()
            .apply(&envelope(&mut seq, kind, action, payload));
    };
    feed(
        &mut screen,
        "turn_started",
        None,
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
    );
    feed(&mut screen, "step_started", None, json!({}));
    feed(
        &mut screen,
        "assistant_message_started",
        Some("a_m"),
        json!({}),
    );
    for i in 0..calls {
        let action = format!("a_c{i}");
        feed(
            &mut screen,
            "tool_call_requested",
            Some(&action),
            json!({"name": "read", "arguments": {"path": format!("src/{i}.rs")}}),
        );
    }
    for i in 0..calls {
        let action = format!("a_c{i}");
        let text = (1..=lines_each)
            .map(|n| format!("L{n:04} of call {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        feed(
            &mut screen,
            "tool_call_completed",
            Some(&action),
            json!({"status": "completed",
                "content": [{"type": "text", "text": text}],
                "details": {"start_line": 1}}),
        );
        if *asking.get(i).unwrap_or(&false) {
            feed(
                &mut screen,
                "permission_requested",
                Some(&action),
                json!({"request_id": format!("r_{i}"), "effects": ["executes"],
                    "reversible": true, "step": "standing_ask",
                    "standing_rule": {"scope": "global", "prefix": "npm"}}),
            );
        }
    }
    feed(
        &mut screen,
        "text_completed",
        Some("a_m"),
        json!({"text": "done"}),
    );
    feed(
        &mut screen,
        "assistant_message_completed",
        Some("a_m"),
        json!({"outcome": "completed"}),
    );
    let rows = screen.pages().rows();
    let group = rows
        .iter()
        .find_map(|(_, target)| {
            matches!(target, Some(Target::Group(_))).then(|| (*target).unwrap())
        })
        .unwrap_or_else(|| {
            panic!(
                "no group rows={:?}",
                screen
                    .pages()
                    .rows()
                    .iter()
                    .map(|(l, _)| l.to_string())
                    .collect::<Vec<_>>()
            )
        });
    screen.open(group);
    let rows = screen.pages().rows();
    let mut call_targets = Vec::new();
    for (_, target) in &rows {
        if let Some(Target::Call(_)) = target {
            call_targets.push((*target).unwrap());
        }
    }
    assert_eq!(call_targets.len(), calls, "one row target per call");
    for (i, target) in call_targets.iter().enumerate() {
        if *opened.get(i).unwrap_or(&false) {
            screen.open(*target);
        }
    }
    (screen, group, call_targets)
}

/// The absolute wrapped rows of page 0's lines, and their targets.
fn page_rows(screen: &Screen) -> (Vec<usize>, Vec<Option<Target>>) {
    let (rows, _) = screen.pages().page_text(0).expect("page 0 resident");
    let mut out = Vec::new();
    let mut row = 0usize;
    for (line, target) in &rows {
        out.push((row, *target));
        row += crate::view::rows(line, screen.pages().wrap_width());
    }
    let (starts, targets) = out.into_iter().unzip();
    (starts, targets)
}

/// The absolute row of page 0's line holding `target`, if it draws.
fn line_row(screen: &Screen, target: Target) -> Option<usize> {
    let (starts, targets) = page_rows(screen);
    starts
        .into_iter()
        .zip(targets)
        .find_map(|(row, drawn)| (drawn == Some(target)).then_some(row))
}

/// The absolute rows of the opened block of `call`: every line whose
/// text scopes end in it, in order.
fn opened_rows(screen: &Screen, call: Target) -> Vec<usize> {
    let (rows, texts) = screen.pages().page_text(0).expect("page 0 resident");
    let width = screen.pages().wrap_width();
    let mut out = Vec::new();
    let mut row = 0usize;
    for ((line, _), text) in rows.iter().zip(&texts) {
        let count = crate::view::rows(line, width);
        if text
            .scopes
            .iter()
            .rev()
            .find(|scope| matches!(scope, Target::Group(_) | Target::Call(_)))
            == Some(&call)
        {
            for r in 0..count {
                out.push(row + r);
            }
        }
        row += count;
    }
    out
}

#[test]
fn the_call_holding_the_top_row_pins_and_nothing_else() {
    let (mut screen, group, calls) = read_group(1, &[true], 30, &[false]);
    let call = calls[0];
    let first = opened_rows(&screen, call)
        .first()
        .copied()
        .expect("opened rows");
    screen.jump(first);
    screen.settle(5);
    let pin = screen.pin(5).expect("a pin inside opened content");
    assert_eq!(pin.target, call, "the innermost scope pins, not {group:?}");
    assert!(pin.row < first, "the header sits above the top row");
}

#[test]
fn a_ledger_row_outside_any_opened_call_pins_the_group() {
    let (mut screen, group, calls) = read_group(2, &[false, false], 30, &[false, false]);
    let row = line_row(&screen, calls[1]).expect("the second call row");
    screen.jump(row);
    screen.settle(5);
    let pin = screen.pin(5).expect("a pin on a ledger row");
    assert_eq!(pin.target, group, "a ledger row pins its group");
}

#[test]
fn two_opened_calls_pin_the_one_the_top_row_is_in() {
    let (mut screen, group, calls) = read_group(2, &[true, true], 30, &[false, false]);
    let second = opened_rows(&screen, calls[1])
        .first()
        .copied()
        .expect("second opened rows");
    screen.jump(second);
    screen.settle(5);
    assert_eq!(
        screen.pin(5).map(|pin| pin.target),
        Some(calls[1]),
        "scrolled into the second call pins the second"
    );
    let first = opened_rows(&screen, calls[0])
        .first()
        .copied()
        .expect("first opened rows");
    screen.jump(first);
    screen.settle(5);
    assert_eq!(
        screen.pin(5).map(|pin| pin.target),
        Some(calls[0]),
        "scrolled into the first call pins the first"
    );
    let row = line_row(&screen, calls[1]).expect("the second call row");
    screen.jump(row);
    screen.settle(5);
    assert_eq!(
        screen.pin(5).map(|pin| pin.target),
        Some(group),
        "top on the second call's own row pins the group"
    );
}

#[test]
fn the_pin_edges_follow_the_calls_rows() {
    let (mut screen, group, calls) = read_group(2, &[true, false], 30, &[false, false]);
    let call = calls[0];
    let header = line_row(&screen, call).expect("the call row");
    screen.jump(header);
    screen.settle(5);
    assert_eq!(
        screen.pin(5).map(|pin| pin.target),
        Some(group),
        "top on the call's own row pins no call"
    );
    let opened = opened_rows(&screen, call);
    let (first, last) = (
        opened.first().copied().expect("a first opened row"),
        opened.last().copied().expect("a last opened row"),
    );
    screen.jump(first);
    screen.settle(5);
    assert_eq!(
        screen.pin(5).map(|pin| pin.target),
        Some(call),
        "top on the first opened row pins the call"
    );
    screen.jump(last);
    screen.settle(5);
    assert_eq!(
        screen.pin(5).map(|pin| pin.target),
        Some(call),
        "top on the last opened row pins the call"
    );
    let after = line_row(&screen, calls[1]).expect("the next call row");
    assert!(after > last, "the next row sits past the opened block");
    // The top stays exact: no settle clamps it past the bottom here.
    screen.jump(after);
    assert_eq!(
        screen.pin(1).map(|pin| pin.target),
        Some(group),
        "top on the row after pins the group"
    );
}

#[test]
fn nothing_pins_while_following() {
    let (screen, _, _) = read_group(1, &[true], 30, &[false]);
    assert_eq!(screen.top(), None, "the builder follows");
    assert_eq!(screen.pin(5), None, "following pins nothing");
}

#[test]
fn nothing_pins_when_the_top_row_is_on_a_dropped_page() {
    let (mut screen, _, _) = read_group(1, &[true], 30, &[false]);
    let mut seq = 1000;
    for turn in 0..60 {
        for (kind, action, payload) in [
            (
                "turn_started",
                None,
                json!({"input": [{"type": "message", "source": "driver",
                    "content": [{"type": "text", "text": format!("filler {turn}")}]}]}),
            ),
            ("step_started", None, json!({})),
            (
                "text_completed",
                Some(format!("fm{turn}").as_str()),
                json!({"text": "filler"}),
            ),
            (
                "assistant_message_completed",
                Some(format!("fm{turn}").as_str()),
                json!({"outcome": "completed"}),
            ),
            ("turn_completed", None, json!({"outcome": "completed"})),
        ] {
            screen
                .pages_mut()
                .apply(&envelope(&mut seq, kind, action, payload));
        }
    }
    screen.jump(usize::MAX);
    screen.settle(5);
    assert!(screen.pages().part(0).is_none(), "the first page dropped");
    screen.jump(0);
    assert_eq!(screen.pin(5), None, "a dropped page pins nothing");
}

#[test]
fn a_ledger_open_only_for_an_approval_does_not_pin() {
    let (mut screen, _, calls) = read_group(1, &[false], 5, &[true]);
    let row = line_row(&screen, calls[0]).expect("the asking call row");
    // The ledger draws for the asking call while closed: the row after
    // the call's row is its (empty) opened nothing, so the top inside
    // the ledger is the call's own row.
    screen.jump(row.saturating_add(1));
    screen.settle(5);
    assert_eq!(screen.pin(5), None, "a closed ledger pins nothing");
}

#[test]
fn an_opened_thought_with_no_ledger_does_not_pin() {
    let mut seq = 0;
    let mut screen = Screen::new();
    let mut feed = |screen: &mut Screen, kind: &str, action: Option<&str>, payload: Value| {
        screen
            .pages_mut()
            .apply(&envelope(&mut seq, kind, action, payload));
    };
    feed(
        &mut screen,
        "turn_started",
        None,
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
    );
    feed(&mut screen, "step_started", None, json!({}));
    feed(&mut screen, "reasoning_started", Some("a_t"), json!({}));
    feed(
        &mut screen,
        "reasoning_completed",
        Some("a_t"),
        json!({"text": "a long thought that opens"}),
    );
    feed(
        &mut screen,
        "text_completed",
        Some("a_m"),
        json!({"text": "done"}),
    );
    feed(
        &mut screen,
        "assistant_message_completed",
        Some("a_m"),
        json!({"outcome": "completed"}),
    );
    let thought = screen
        .pages()
        .rows()
        .into_iter()
        .find_map(|(_, target)| target)
        .expect("a thought target");
    screen.open(thought);
    let (rows, texts) = screen.pages().page_text(0).expect("page 0 resident");
    let width = screen.pages().wrap_width();
    let mut row = 0usize;
    let mut inside = None;
    for ((line, _), text) in rows.iter().zip(&texts) {
        let count = crate::view::rows(line, width);
        if text.scopes.contains(&thought) && inside.is_none() {
            inside = Some(row + 1);
        }
        row += count;
    }
    let top = inside.expect("opened thought rows");
    screen.jump(top);
    screen.settle(5);
    assert_eq!(screen.pin(5), None, "a thought pins nothing");
}

#[test]
fn a_top_row_on_a_call_image_row_pins_the_group() {
    let mut seq = 0;
    let mut screen = Screen::new();
    let mut feed = |screen: &mut Screen, kind: &str, action: Option<&str>, payload: Value| {
        screen
            .pages_mut()
            .apply(&envelope(&mut seq, kind, action, payload));
    };
    feed(
        &mut screen,
        "turn_started",
        None,
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
    );
    feed(&mut screen, "step_started", None, json!({}));
    feed(
        &mut screen,
        "assistant_message_started",
        Some("a_m"),
        json!({}),
    );
    feed(
        &mut screen,
        "tool_call_requested",
        Some("a_c0"),
        json!({"name": "read", "arguments": {"path": "src/0.rs"}}),
    );
    feed(
        &mut screen,
        "tool_call_completed",
        Some("a_c0"),
        json!({"status": "completed",
            "content": [
                {"type": "text", "text": "ok"},
                {"type": "image", "path": "a.png", "mime_type": "image/png",
                    "width": 100, "height": 100}],
            "details": {"start_line": 1}}),
    );
    feed(
        &mut screen,
        "text_completed",
        Some("a_m"),
        json!({"text": "done"}),
    );
    feed(
        &mut screen,
        "assistant_message_completed",
        Some("a_m"),
        json!({"outcome": "completed"}),
    );
    feed(
        &mut screen,
        "turn_completed",
        None,
        json!({"outcome": "completed"}),
    );
    let rows = screen.pages().rows();
    let group = rows
        .iter()
        .find_map(|(_, target)| {
            matches!(target, Some(Target::Group(_))).then(|| (*target).unwrap())
        })
        .unwrap_or_else(|| {
            panic!(
                "no group rows={:?}",
                screen
                    .pages()
                    .rows()
                    .iter()
                    .map(|(l, _)| l.to_string())
                    .collect::<Vec<_>>()
            )
        });
    screen.open(group);
    let call = screen
        .pages()
        .rows()
        .into_iter()
        .find_map(|(_, target)| matches!(target, Some(Target::Call(_))).then(|| target.unwrap()))
        .expect("a call target");
    screen.open(call);
    let (drawn, texts) = screen.pages().page_text(0).expect("page 0 resident");
    let width = screen.pages().wrap_width();
    let mut row = 0usize;
    let mut image_row = None;
    for ((line, _), text) in drawn.iter().zip(&texts) {
        let count = crate::view::rows(line, width);
        let scopes = &text.scopes;
        if scopes
            == &vec![Target::Group(match group {
                Target::Group(id) => id,
                Target::Call(_)
                | Target::CallView(_)
                | Target::Thought(_)
                | Target::Login
                | Target::Note(_)
                | Target::Image(_)
                | Target::Orphans(_)
                | Target::Copy { .. } => unreachable!(),
            })]
            && line.to_string().contains("a.png")
        {
            image_row = Some(row);
        }
        row += count;
    }
    let top = image_row.expect("an image row under the call");
    screen.jump(top);
    screen.settle(5);
    assert_eq!(
        screen.pin(5).map(|pin| pin.target),
        Some(group),
        "an image row pins the group, not the call"
    );
}

#[test]
fn the_pin_row_is_the_headers_first_row() {
    let (mut screen, _, calls) = read_group(1, &[true], 30, &[false]);
    screen.set_size(80, 24);
    screen.wrap_at(20);
    let call = calls[0];
    let header = line_row(&screen, call).expect("the call row");
    let first = opened_rows(&screen, call)
        .first()
        .copied()
        .expect("opened rows");
    assert!(first > header, "a wrapping line sits above the header");
    screen.jump(first);
    screen.settle(5);
    let pin = screen.pin(5).expect("a pin");
    assert_eq!(pin.target, call);
    assert_eq!(pin.row, header, "the pin row is the header's first row");
}

#[test]
fn settle_keeps_the_top_rows_page() {
    let (mut screen, _, calls) = read_group(1, &[true], 30, &[false]);
    let first = opened_rows(&screen, calls[0])
        .first()
        .copied()
        .expect("opened rows");
    screen.jump(first);
    screen.settle(5);
    assert!(screen.pages().part(0).is_some(), "the top row's page stays");
    assert_eq!(
        screen.pin(5).map(|pin| pin.target),
        Some(calls[0]),
        "the pin survives the settle"
    );
}

/// The call's id.
fn call_id(call: Target) -> usize {
    if let Target::Call(id) = call {
        id
    } else {
        panic!("a call target, not {call:?}");
    }
}

/// The opened call's more row: the focus stop inside an opened call a
/// reveal can name.
fn more_row(screen: &Screen, call: Target) -> usize {
    let want = crate::mouse::TargetId::Line(Target::CallView(call_id(call)));
    screen
        .pages()
        .focus_items()
        .into_iter()
        .find(|(_, _, id)| *id == want)
        .map(|(row, _, _)| row)
        .expect("the more row is a focus stop")
}

#[test]
fn reveal_stops_the_focus_row_under_the_pin() {
    let (mut screen, _, calls) = read_group(2, &[true, false], 30, &[false, false]);
    let call = calls[0];
    let more = more_row(&screen, call);
    // From below the opened block, revealing the more row scrolls up:
    // the row would land on row 0, so it lands on row 1 instead.
    let last = screen.pages().index().total();
    screen.jump(last);
    screen.settle(4);
    assert!(
        screen.top().is_some_and(|top| top > more),
        "the more row is above the top"
    );
    screen.reveal(more, 4);
    assert_eq!(
        screen.top(),
        Some(more.saturating_sub(1)),
        "the reveal lands one row above the more row"
    );
    assert_eq!(
        screen.pin(4).map(|pin| pin.target),
        Some(call),
        "the pin stays on the call"
    );
}

#[test]
fn reveal_without_a_pin_is_unchanged() {
    let mut screen = screen(3);
    let turn = screen
        .pages()
        .focus_items()
        .into_iter()
        .find(|(_, _, id)| *id == crate::mouse::TargetId::Turn(1))
        .map(|(row, _, _)| row)
        .expect("the second turn is a focus stop");
    let last = screen.pages().index().total();
    screen.jump(last);
    screen.settle(10);
    assert!(
        screen.top().is_some_and(|top| top > turn),
        "the turn is above the top"
    );
    screen.reveal(turn, 10);
    assert_eq!(
        screen.top(),
        Some(turn),
        "with no pin the reveal lands on the row"
    );
    assert_eq!(screen.pin(10), None, "no pin without an opened call");
}

#[test]
fn land_stops_under_the_pin() {
    let (mut screen, _, calls) = read_group(1, &[true], 30, &[false]);
    let call = calls[0];
    let first = opened_rows(&screen, call)
        .first()
        .copied()
        .expect("opened rows");
    let row = first + 5;
    screen.land(row);
    assert_eq!(
        screen.top(),
        Some(row - 1),
        "the landing stops above the row"
    );
    assert_eq!(
        screen.pin(5).map(|pin| pin.target),
        Some(call),
        "the pin stays on the call"
    );
}

#[test]
fn land_without_a_pin_jumps_to_the_row() {
    let mut screen = screen(1);
    let (drawn, _) = screen.pages().page_text(0).expect("page 0 resident");
    let width = screen.pages().wrap_width();
    let mut row = 0usize;
    let mut reply = None;
    for (line, _) in &drawn {
        let count = crate::view::rows(line, width);
        if line.to_string().contains("reply 0") {
            reply = Some(row);
            break;
        }
        row += count;
    }
    let reply = reply.expect("a reply row");
    screen.land(reply + 1);
    assert_eq!(
        screen.top(),
        Some(reply + 1),
        "with no pin the landing jumps to the row"
    );
    assert_eq!(screen.pin(5), None, "no pin without an opened call");
}

#[test]
fn landing_on_the_row_below_the_header_clears_the_pin_and_uses_the_true_top() {
    let (mut screen, group, _) = read_group(1, &[false], 5, &[false]);
    let header = line_row(&screen, group).expect("the header row");
    let below = header + 1;
    screen.land(below);
    assert_eq!(
        screen.top(),
        Some(header),
        "the landing uses the true top row"
    );
    assert_eq!(screen.pin(5), None, "the header on row 0 pins nothing");
}

#[test]
fn a_closed_ledger_pins_nothing_on_the_asking_call_row() {
    // The asking call keeps its row while its ledger is closed, and the
    // row's own text still names the group: the pin needs the ledger
    // open, so the key match alone pins nothing (`docs/tui.md`,
    // "Pinned call").
    let (mut screen, group, calls) = read_group(1, &[false], 5, &[true]);
    screen.open(group);
    let row = line_row(&screen, calls[0]).expect("the asking call row");
    screen.jump(row);
    screen.settle(1);
    assert_eq!(screen.top(), Some(row), "the call row stays on top");
    assert_eq!(screen.pin(1), None, "a closed ledger pins nothing");
}

#[test]
fn an_opened_summary_on_the_top_row_pins_nothing() {
    // The summary row names no scope of its own: the top row's text sits
    // outside every section, so nothing pins (`docs/tui.md`,
    // "Pinned call").
    let (mut screen, group, _) = read_group(1, &[true], 30, &[false]);
    let header = line_row(&screen, group).expect("the summary row");
    screen.jump(header);
    screen.settle(5);
    assert_eq!(screen.top(), Some(header), "the summary stays on top");
    assert_eq!(screen.pin(5), None, "the top row pins nothing");
}

#[test]
fn every_pin_sits_above_its_top_row() {
    // A section's header draws before its body opens its scope, so a
    // found header is never the top row itself: every pin sits above
    // the row it pins for (`docs/tui.md`, "Pinned call").
    for (_, mut screen) in [
        ("open", read_group(1, &[true], 30, &[false]).0),
        ("approval", read_group(1, &[false], 5, &[true]).0),
        ("two", read_group(2, &[true, false], 30, &[false, false]).0),
    ] {
        let total = total(&screen);
        for top in 0..total {
            screen.jump(top);
            if let Some(pin) = screen.pin(1) {
                assert!(
                    pin.row < top,
                    "the pin sits above its top row {top}: {pin:?}"
                );
            }
        }
    }
}
