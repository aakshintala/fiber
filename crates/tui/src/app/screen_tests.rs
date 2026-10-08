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
    assert_eq!(needs.first(), Some(&(first.first_seq..=first.last_seq)));
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
    let (tall, rows, _) = items
        .iter()
        .copied()
        .find(|(_, rows, _)| *rows > 2)
        .expect("a wrapped item");
    // An item taller than the height shows its first rows.
    screen.reveal(tall, 2);
    assert_eq!(screen.top(), Some(tall), "{rows} rows");
    // An item below the screen scrolls down just far enough.
    let (below, below_rows, _) = items
        .iter()
        .copied()
        .find(|(row, _, _)| *row > tall + 4)
        .expect("an item below");
    let end = below + below_rows.min(2);
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
