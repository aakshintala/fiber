//! Tests for the turn card's surface: one surface below the bubble,
//! broken at a handoff's band, with half-block edges
//! (`docs/tui.md`, "Look", "Turns", "Handoff").

use contract::{ActionId, Envelope, SessionId};
use jiff::tz::TimeZone;
use serde_json::{Value, json};

use super::Pieces;
use crate::rows::{RowText, Rows};
use crate::surface::Edges;
use crate::theme::Role;
use crate::turn::{Fold, Row, Turn, fold_line};

fn is_edge_row(line: &ratatui::text::Line<'_>) -> bool {
    let mut graphemes = line.styled_graphemes(ratatui::style::Style::default());
    graphemes.next().is_some_and(|first| {
        matches!(first.symbol, "▄" | "▀") && graphemes.all(|g| matches!(g.symbol, "▄" | "▀"))
    })
}

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";
const WIDTH: u16 = 60;

/// One envelope at `ts` milliseconds.
fn envelope(kind: &str, action: Option<&str>, ts: u64, payload: Value) -> Envelope {
    Envelope {
        kind: kind.to_owned(),
        session_id: SessionId(SESSION.to_owned()),
        ts,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|id| ActionId(id.to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    }
}

/// Turns folded from envelopes.
struct Cards {
    turns: Vec<Turn>,
    fold: Fold,
}

fn fresh() -> Cards {
    Cards {
        turns: Vec::new(),
        fold: Fold::default(),
    }
}

fn feed(cards: &mut Cards, kind: &str, action: Option<&str>, payload: Value) {
    fold_line(
        &mut cards.turns,
        &mut cards.fold,
        &envelope(kind, action, 0, payload),
    );
}

fn start(cards: &mut Cards, text: &str) {
    feed(
        cards,
        "turn_started",
        None,
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": text}]}]}),
    );
}

fn text(cards: &mut Cards, action: &str, text: &str) {
    feed(
        cards,
        "text_completed",
        Some(action),
        json!({ "text": text }),
    );
}

fn band(cards: &mut Cards) {
    feed(cards, "handoff_started", None, json!({ "trigger": "auto" }));
}

/// The handoff completes, so later text is a reply, not the note.
fn handed_off(cards: &mut Cards) {
    feed(
        cards,
        "handoff_completed",
        None,
        json!({"outcome": "completed", "tokens_before": 1_000}),
    );
}

fn done(cards: &mut Cards) {
    feed(
        cards,
        "turn_completed",
        None,
        json!({"outcome": "completed"}),
    );
}

fn failed(cards: &mut Cards) {
    feed(
        cards,
        "turn_completed",
        None,
        json!({"outcome": "failed", "error": {"code": "io_failed", "message": "boom"}}),
    );
}

fn steered(cards: &mut Cards, text: &str) {
    feed(
        cards,
        "steering_applied",
        None,
        json!({"content": [{"type": "text", "text": text}], "source": "driver"}),
    );
}

fn step(cards: &mut Cards) {
    feed(cards, "step_started", None, json!({}));
}

fn requested(cards: &mut Cards, action: &str, name: &str, arguments: Value) {
    feed(
        cards,
        "tool_call_requested",
        Some(action),
        json!({"name": name, "arguments": arguments}),
    );
}

fn completed(cards: &mut Cards, action: &str, extra: Value) {
    let mut payload = json!({"status": "completed", "content": [{"type": "text", "text": "ok"}]});
    if let (Some(payload), Some(extra)) = (payload.as_object_mut(), extra.as_object()) {
        payload.extend(extra.clone());
    }
    feed(cards, "tool_call_completed", Some(action), payload);
}

/// The card's rows at `width` with `edges`, and which pieces drew.
fn draw(turn: &Turn, width: u16, edges: Edges) -> (Vec<Row>, Vec<RowText>, Pieces) {
    let mut out = Rows::default();
    let pieces = turn.rows(
        width,
        &TimeZone::UTC,
        edges,
        &crate::image::Layout::default(),
        &mut out,
    );
    let (rows, texts) = out.into_parts();
    (rows, texts, pieces)
}

fn shown_rows(rows: &[Row]) -> Vec<String> {
    rows.iter().map(|(line, _)| line.to_string()).collect()
}

fn handoff() -> Option<ratatui::style::Color> {
    Some(Role::Handoff.color())
}

fn turn() -> Option<ratatui::style::Color> {
    Some(Role::Turn.color())
}

#[test]
fn the_card_is_a_turn_surface_below_the_bubble() {
    // The bubble and its time row float above the card after a blank
    // row; the card's body sits on the turn tint between half-block
    // edges, its blocks separated by one blank row
    // (`docs/tui.md`, "Look", "Turns").
    let mut cards = fresh();
    start(&mut cards, "go");
    text(&mut cards, "a_1", "Hello.");
    done(&mut cards);
    let (rows, _, pieces) = draw(&cards.turns[0], WIDTH, Edges::BOTH);
    assert_eq!(
        pieces,
        Pieces {
            first: true,
            last: true
        }
    );
    let shown = shown_rows(&rows);
    assert_eq!(
        shown,
        [
            String::new(),
            "▄".repeat(5),
            " go ▐".to_owned(),
            "▀".repeat(5),
            "you 00:00 ".to_owned(),
            String::new(),
            "▄".repeat(60),
            " Hello.".to_owned(),
            String::new(),
            " ▣ completed".to_owned(),
            "▀".repeat(60),
        ]
    );
    for (line, target) in rows.iter().take(6) {
        assert_eq!(line.style.bg, None);
        assert!(target.is_none());
    }
    assert_eq!(rows[6].0.spans[0].style.fg, turn());
    for (line, _) in rows.iter().skip(7).take(3) {
        assert_eq!(line.style.bg, turn());
    }
    assert_eq!(rows[10].0.spans[0].style.fg, turn());
}

#[test]
fn a_turn_has_a_blank_row_before_its_bubble_and_before_its_card() {
    // One blank row opens the turn, and one separates the time row from
    // the card; a turn with no card yet keeps the blank row ahead of
    // where the card draws (`docs/tui.md`, "Turns").
    let mut cards = fresh();
    start(&mut cards, "go");
    text(&mut cards, "a_1", "Hello.");
    done(&mut cards);
    let (rows, texts, _) = draw(&cards.turns[0], WIDTH, Edges::BOTH);
    let shown = shown_rows(&rows);
    assert_eq!(shown[0], "");
    assert_eq!(shown[5], "");
    assert_eq!(texts[0], RowText::plain());
    assert_eq!(texts[5], RowText::plain());
    let mut cards = fresh();
    start(&mut cards, "go");
    let (rows, _, pieces) = draw(&cards.turns[0], WIDTH, Edges::BOTH);
    assert_eq!(
        pieces,
        Pieces {
            first: false,
            last: false
        }
    );
    assert_eq!(
        shown_rows(&rows),
        [
            String::new(),
            "▄".repeat(5),
            " go ▐".to_owned(),
            "▀".repeat(5),
            "you 00:00 ".to_owned(),
            String::new()
        ]
    );
    for (line, _) in &rows {
        assert_eq!(line.style.bg, None);
    }
}

#[test]
fn the_time_row_reads_you_and_ends_one_column_before_the_edge() {
    // "you HH:MM" dim with one trailing blank, outside the card's tint
    // (`docs/tui.md`, "Turns").
    use ratatui::style::Modifier;
    let mut cards = fresh();
    start(&mut cards, "go");
    text(&mut cards, "a_1", "Hello.");
    done(&mut cards);
    let (rows, texts, _) = draw(&cards.turns[0], WIDTH, Edges::BOTH);
    let (line, target) = &rows[4];
    assert_eq!(line.to_string(), "you 00:00 ");
    assert!(line.to_string().ends_with(' '));
    assert_eq!(line.alignment, Some(ratatui::layout::Alignment::Right));
    assert!(line.style.add_modifier.contains(Modifier::DIM));
    assert_eq!(line.style.bg, None);
    assert!(target.is_none());
    assert_eq!(texts[4].skip, 0);
    assert_eq!(texts[4].tail, 0);
}

#[test]
fn a_finished_turn_with_no_entries_is_a_card_of_its_closing_line() {
    let mut cards = fresh();
    start(&mut cards, "go");
    done(&mut cards);
    let (rows, _, pieces) = draw(&cards.turns[0], WIDTH, Edges::BOTH);
    assert_eq!(
        pieces,
        Pieces {
            first: true,
            last: true
        }
    );
    let shown = shown_rows(&rows);
    assert_eq!(shown[6], "▄".repeat(60));
    assert_eq!(shown[7], " ▣ completed");
    assert_eq!(shown[8], "▀".repeat(60));
    assert_eq!(rows[7].0.style.bg, turn());
}

#[test]
fn the_summary_names_the_closing_line_when_it_differs_from_the_card() {
    use contract::events::{TurnCompleted, TurnOutcome};

    use crate::turn::DrawSummary;
    let mut cards = fresh();
    start(&mut cards, "go");
    done(&mut cards);
    // The card closed completed, but the summary closes the same turn
    // as failed: the summary's ending names the closing line, not the
    // card's own.
    let turn = &cards.turns[0];
    let ended = (
        TurnCompleted {
            outcome: TurnOutcome::Failed,
            error: None,
            questions: None,
        },
        5_000u64,
    );
    let summary = DrawSummary {
        started: turn.started,
        calls: turn.calls,
        spend: &turn.spend,
        ended: Some(&ended),
    };
    let mut out = Rows::default();
    turn.rows_with_summary(
        WIDTH,
        &TimeZone::UTC,
        Edges::BOTH,
        &crate::image::Layout::default(),
        Some(summary),
        &mut out,
    );
    let (rows, _) = out.into_parts();
    let shown = shown_rows(&rows);
    assert!(
        shown.iter().any(|row| row.contains("▣ failed")),
        "the summary's failure names the closing line: {shown:?}"
    );
    assert!(
        !shown.iter().any(|row| row.contains("▣ completed")),
        "the card's own ending does not draw: {shown:?}"
    );
}

#[test]
fn an_ended_summary_leaves_the_open_cards_group_collapsed() {
    use contract::events::{TurnCompleted, TurnOutcome};

    use crate::turn::DrawSummary;
    let mut cards = fresh();
    start(&mut cards, "go");
    step(&mut cards);
    requested(&mut cards, "a_1", "read", json!({"path": "a.rs"}));
    // The card is still open, but the summary closes the turn: the
    // group draws collapsed, without its in-flight call
    // (`docs/tui.md`, "Tool groups and the ledger").
    let turn = &cards.turns[0];
    let ended = (
        TurnCompleted {
            outcome: TurnOutcome::Completed,
            error: None,
            questions: None,
        },
        5_000u64,
    );
    let summary = DrawSummary {
        started: turn.started,
        calls: turn.calls,
        spend: &turn.spend,
        ended: Some(&ended),
    };
    let mut out = Rows::default();
    turn.rows_with_summary(
        WIDTH,
        &TimeZone::UTC,
        Edges::BOTH,
        &crate::image::Layout::default(),
        Some(summary),
        &mut out,
    );
    let (rows, _) = out.into_parts();
    let shown = shown_rows(&rows);
    assert!(
        shown.iter().any(|row| row.contains("▣ completed")),
        "the summary closes the card: {shown:?}"
    );
    assert!(
        shown.iter().any(|row| row.contains("Read 1 file")),
        "the group still draws its summary row: {shown:?}"
    );
    assert!(
        !shown.iter().any(|row| row.contains("read a.rs")),
        "the in-flight call stays off the collapsed row: {shown:?}"
    );
}

#[test]
fn a_handoff_breaks_the_card_into_two_surfaces() {
    // An edge closes the piece before the band, and one opens the piece
    // after it; the band's line keeps the surface tint
    // (`docs/tui.md`, "Handoff").
    let mut cards = fresh();
    start(&mut cards, "go");
    text(&mut cards, "a_1", "before");
    band(&mut cards);
    handed_off(&mut cards);
    text(&mut cards, "a_2", "after");
    done(&mut cards);
    let (rows, _, pieces) = draw(&cards.turns[0], WIDTH, Edges::BOTH);
    assert_eq!(
        pieces,
        Pieces {
            first: true,
            last: true
        }
    );
    let shown = shown_rows(&rows);
    assert_eq!(shown[6], "▄".repeat(60));
    assert_eq!(shown[7], " before");
    assert_eq!(shown[8], "▀".repeat(60));
    // The band is one content row with half-block edges over SGR 49:
    // the card ends at the band, and a second card below it continues
    // the turn.
    assert_eq!(shown[9], "▄".repeat(60));
    let left = " ⇄ handoff · automatic";
    let right = "1k → … · 00:00 ";
    assert_eq!(
        shown[10],
        format!(
            "{left}{}{right}",
            " ".repeat(60 - crate::format::width(left) - crate::format::width(right))
        ),
        "{shown:?}"
    );
    assert_eq!(shown[11], "▀".repeat(60));
    assert_eq!(shown[12], "▄".repeat(60));
    assert_eq!(shown[13], " after");
    assert_eq!(shown[14], "");
    assert_eq!(shown[15], " ▣ completed");
    assert_eq!(shown[16], "▀".repeat(60));
    assert_eq!(rows[10].0.style.bg, handoff());
    // The ▣ line closes the turn once, below the band; none above it.
    assert_eq!(
        shown.iter().filter(|row| row.contains('▣')).count(),
        1,
        "{shown:?}"
    );
}

#[test]
fn an_open_note_sits_inside_the_band() {
    // The band row carries the half-block edges; "▸ note" and every
    // note row sit under its lower edge on the handoff tint with no
    // edges (`docs/tui.md`, "Handoff").
    use std::path::PathBuf;

    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    use crate::app::Target;
    use crate::link::Line as LinkLine;

    let mut app = crate::app::App::new(PathBuf::from("/w"));
    app.set_size(80, 24);
    app.attach(contract::SessionId(SESSION.to_owned()));
    let feed = |app: &mut crate::app::App, kind: &str, action: Option<&str>, payload: Value| {
        app.on_line(LinkLine::Session(envelope(kind, action, 0, payload)));
    };
    feed(
        &mut app,
        "turn_started",
        None,
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
    );
    feed(
        &mut app,
        "text_completed",
        Some("a_1"),
        json!({"text": "Working."}),
    );
    feed(
        &mut app,
        "handoff_started",
        None,
        json!({"trigger": "auto"}),
    );
    feed(
        &mut app,
        "assistant_message_delta",
        Some("a_n"),
        json!({"text": "## Note"}),
    );
    feed(
        &mut app,
        "text_completed",
        Some("a_n"),
        json!({"text": "## Note\nkeep going"}),
    );
    feed(
        &mut app,
        "handoff_completed",
        None,
        json!({"outcome": "completed", "tokens_before": 1_000}),
    );
    let note = app
        .targets()
        .into_iter()
        .find_map(|(_, target)| matches!(target, Target::Note(_)).then_some(target));
    assert!(note.is_some(), "{:?}", app.lines());
    if let Some(note) = note {
        app.open(note);
    }
    let lines: Vec<String> = app.lines().iter().map(ToString::to_string).collect();
    let band_at = lines
        .iter()
        .position(|line| line.contains('⇄'))
        .expect("a band");
    assert!(lines[band_at - 1].chars().all(|c| c == '▄'));
    assert!(lines[band_at + 1].chars().all(|c| c == '▀'));
    assert_eq!(lines[band_at + 2], "  ▸ note");
    assert_eq!(lines[band_at + 3], "    ## Note");
    assert_eq!(lines[band_at + 4], "    keep going");
    assert_eq!(app.lines()[band_at].style.bg, handoff());
    for line in app.lines().iter().skip(band_at + 2).take(3) {
        assert_eq!(line.style.bg, handoff());
    }
    // Through the view the tint reaches the last text column.
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    crate::view::render(&app, area, &mut buf, None);
    let row = (0..area.height)
        .find(|y| {
            (0..area.width)
                .map(|x| buf[(x, *y)].symbol().to_owned())
                .collect::<String>()
                .contains("keep going")
        })
        .expect("a note row on screen");
    assert_eq!(buf[(78, row)].bg, Role::Handoff.color());
}

#[test]
fn a_band_first_or_last_draws_no_empty_piece() {
    // A band as the first entry draws no edge pair before it; a running
    // turn whose last entry is a band draws none after it
    // (`docs/tui.md`, "Handoff").
    let mut cards = fresh();
    start(&mut cards, "go");
    band(&mut cards);
    handed_off(&mut cards);
    text(&mut cards, "a_1", "after");
    done(&mut cards);
    let (rows, _, pieces) = draw(&cards.turns[0], WIDTH, Edges::BOTH);
    assert_eq!(
        pieces,
        Pieces {
            first: false,
            last: true
        }
    );
    let shown = shown_rows(&rows);
    assert_eq!(shown[6], "▄".repeat(60));
    assert!(shown[7].starts_with(" ⇄ handoff · automatic"), "{shown:?}");
    assert_eq!(shown[8], "▀".repeat(60));
    assert_eq!(shown[9], "▄".repeat(60));
    let mut cards = fresh();
    start(&mut cards, "go");
    text(&mut cards, "a_1", "before");
    band(&mut cards);
    let (rows, _, pieces) = draw(&cards.turns[0], WIDTH, Edges::BOTH);
    assert_eq!(
        pieces,
        Pieces {
            first: true,
            last: false
        }
    );
    let shown = shown_rows(&rows);
    // The running band is content row and edges: the lower edge is
    // last, the content row above it.
    assert_eq!(shown[shown.len() - 1], "▀".repeat(60));
    assert!(
        shown[shown.len() - 2].starts_with(" ⇄ handoff · automatic"),
        "{shown:?}"
    );
}

#[test]
fn a_blank_reply_draws_no_piece() {
    // A `text_completed` of only whitespace renders no row, so nothing
    // after the band draws (`docs/tui.md`, "Turns").
    let mut cards = fresh();
    start(&mut cards, "go");
    band(&mut cards);
    text(&mut cards, "a_1", "   ");
    let (rows, _, pieces) = draw(&cards.turns[0], WIDTH, Edges::BOTH);
    assert_eq!(
        pieces,
        Pieces {
            first: false,
            last: false
        }
    );
    let shown = shown_rows(&rows);
    // The running band is content row and edges: the lower edge is
    // last, the content row above it.
    assert_eq!(shown[shown.len() - 1], "▀".repeat(60));
    assert!(
        shown[shown.len() - 2].starts_with(" ⇄ handoff · automatic"),
        "{shown:?}"
    );
}

#[test]
fn edges_the_page_withholds_are_not_drawn() {
    // A page cut inside a turn withholds the edges at the cut
    // (`docs/tui.md`, "History and paging").
    let mut cards = fresh();
    start(&mut cards, "go");
    text(&mut cards, "a_1", "before");
    band(&mut cards);
    handed_off(&mut cards);
    text(&mut cards, "a_2", "after");
    done(&mut cards);
    // A page continuation carries no prompt bubble: only the pieces draw.
    cards.turns[0].prompts.clear();
    let (rows, _, _) = draw(
        &cards.turns[0],
        WIDTH,
        Edges {
            top: false,
            bottom: true,
        },
    );
    let shown = shown_rows(&rows);
    assert_eq!(shown[0], "");
    assert_eq!(shown[1], " before");
    assert_eq!(shown[2], "▀".repeat(60));
    assert_eq!(shown[3], "▄".repeat(60));
    assert!(shown[4].starts_with(" ⇄ handoff · automatic"), "{shown:?}");
    assert_eq!(shown[5], "▀".repeat(60));
    // The piece after the band still opens with an edge.
    assert_eq!(shown[6], "▄".repeat(60));
    let (rows, _, _) = draw(
        &cards.turns[0],
        WIDTH,
        Edges {
            top: true,
            bottom: false,
        },
    );
    let shown = shown_rows(&rows);
    assert_eq!(shown[0], "▄".repeat(60));
    assert_eq!(shown[1], " before");
    // The piece before the band still closes with an edge.
    assert_eq!(shown[2], "▀".repeat(60));
    assert!(
        shown.last().is_some_and(|line| line == " ▣ completed"),
        "{shown:?}"
    );
}

#[test]
fn rows_reports_which_pieces_drew() {
    // Whether the first and the last piece drew, edges not counted
    // (`docs/tui.md`, "Turns").
    let draw_pieces = |build: &dyn Fn(&mut Cards)| {
        let mut cards = fresh();
        start(&mut cards, "go");
        build(&mut cards);
        draw(&cards.turns[0], WIDTH, Edges::BOTH).2
    };
    assert_eq!(
        draw_pieces(&|_| {}),
        Pieces {
            first: false,
            last: false
        },
        "no entries"
    );
    assert_eq!(
        draw_pieces(&|cards| text(cards, "a_1", "hi")),
        Pieces {
            first: true,
            last: true
        },
        "one reply"
    );
    assert_eq!(
        draw_pieces(&|cards| {
            text(cards, "a_1", "hi");
            band(cards);
        }),
        Pieces {
            first: true,
            last: false
        },
        "a reply then a band"
    );
    assert_eq!(
        draw_pieces(&|cards| {
            band(cards);
            handed_off(cards);
            text(cards, "a_1", "hi");
        }),
        Pieces {
            first: false,
            last: true
        },
        "a band then a reply"
    );
    assert_eq!(
        draw_pieces(&|cards| band(cards)),
        Pieces {
            first: false,
            last: false
        },
        "a band alone"
    );
    assert_eq!(
        draw_pieces(&|cards| {
            band(cards);
            text(cards, "a_1", "   ");
        }),
        Pieces {
            first: false,
            last: false
        },
        "a band then a blank reply"
    );
    assert_eq!(
        draw_pieces(&|cards| {
            text(cards, "a_1", "   ");
            band(cards);
            handed_off(cards);
            text(cards, "a_2", "hi");
        }),
        Pieces {
            first: false,
            last: true
        },
        "a blank reply then a band then a reply"
    );
}

#[test]
fn a_later_band_does_not_change_whether_the_first_piece_drew() {
    let mut cards = fresh();
    start(&mut cards, "go");
    band(&mut cards);
    handed_off(&mut cards);
    text(&mut cards, "a_1", "middle");
    band(&mut cards);
    handed_off(&mut cards);
    text(&mut cards, "a_2", "last");
    assert_eq!(
        draw(&cards.turns[0], WIDTH, Edges::BOTH).2,
        Pieces {
            first: false,
            last: true
        }
    );
}

#[test]
fn whether_a_piece_draws_does_not_depend_on_the_width() {
    // Every entry draws at least one row at any width of 1 or more, or
    // none at every width (`docs/tui.md`, "History and paging").
    let draw_pieces = |case: usize, width: u16| {
        let mut cards = fresh();
        start(&mut cards, "go");
        match case {
            1 => text(&mut cards, "a_1", "hi"),
            2 => {
                text(&mut cards, "a_1", "hi");
                band(&mut cards);
            }
            3 => {
                band(&mut cards);
                handed_off(&mut cards);
                text(&mut cards, "a_1", "hi");
            }
            4 => band(&mut cards),
            5 => {
                band(&mut cards);
                text(&mut cards, "a_1", "   ");
            }
            6 => {
                text(&mut cards, "a_1", "   ");
                band(&mut cards);
                handed_off(&mut cards);
                text(&mut cards, "a_2", "hi");
            }
            _ => {}
        }
        draw(&cards.turns[0], width, Edges::BOTH).2
    };
    let names = [
        "no entries",
        "one reply",
        "a reply then a band",
        "a band then a reply",
        "a band alone",
        "a band then a blank reply",
        "a blank reply then a band then a reply",
    ];
    for (case, name) in names.iter().enumerate() {
        let at_40 = draw_pieces(case, 40);
        for width in [1, 40, 120] {
            assert_eq!(draw_pieces(case, width), at_40, "{name} at {width}");
        }
    }
}

#[test]
fn nothing_in_the_card_is_indented_past_its_padding() {
    // Replies, steering messages and answers all start at the card's
    // one-column padding, with no stripe (`docs/tui.md`, "Turns").
    let mut cards = fresh();
    start(&mut cards, "go");
    steered(&mut cards, "use x");
    text(&mut cards, "a_1", "Hello.");
    done(&mut cards);
    let (rows, _, _) = draw(&cards.turns[0], WIDTH, Edges::BOTH);
    for (line, _) in rows.iter().skip(6) {
        let shown = line.to_string();
        if is_edge_row(line) {
            continue;
        }
        if shown.is_empty() {
            continue;
        }
        assert!(shown.starts_with(' '), "{shown:?}");
        assert!(!shown.starts_with("  "), "{shown:?}");
        assert!(!shown.contains('▌') && !shown.contains('▐'), "{shown:?}");
    }
}

#[test]
fn the_card_rows_sit_one_column_inside_the_tint() {
    // At W = 20 every piece row starts with a blank on the turn
    // background and is at most W - 1 cells, whatever its entry
    // (`docs/tui.md`, "Turns").
    use serde_json::json;
    let mut cards = fresh();
    start(&mut cards, "go");
    text(&mut cards, "a_1", "Hello.");
    steered(&mut cards, "use x");
    step(&mut cards);
    requested(&mut cards, "a_2", "read", json!({"path": "src/a.rs"}));
    completed(&mut cards, "a_2", json!({}));
    feed(
        &mut cards,
        "mcp_server_failed",
        None,
        json!({"server": "one", "reason": "died", "will_restart": true,
            "error": {"code": "mcp_server_unavailable", "message": "one stopped."}}),
    );
    requested(
        &mut cards,
        "a_q",
        "ask_user",
        json!({"questions": [{"header": "Base"}, {"header": "Name"}]}),
    );
    feed(
        &mut cards,
        "interaction_requested",
        None,
        json!({"request_id": "r_1", "kind": "form", "action_ids": ["a_q"],
            "fields": [{"header": "Base", "question": "Which?"},
                {"header": "Name", "question": "What?"}]}),
    );
    feed(
        &mut cards,
        "interaction_resolved",
        None,
        json!({"request_id": "r_1", "by": "person",
            "answers": [{"labels": ["main"]}, {"skipped": true}]}),
    );
    completed(&mut cards, "a_q", json!({}));
    failed(&mut cards);
    let (rows, _, _) = draw(&cards.turns[0], 20, Edges::BOTH);
    let shown = shown_rows(&rows);
    assert!(shown.iter().any(|row| row.contains("Hello.")), "{shown:?}");
    assert!(shown.iter().any(|row| row.contains("use x")), "{shown:?}");
    assert!(shown.iter().any(|row| row.contains("Read")), "{shown:?}");
    assert!(shown.iter().any(|row| row.contains("⚠")), "{shown:?}");
    assert!(
        shown.iter().any(|row| row.contains("you answered")),
        "{shown:?}"
    );
    assert!(shown.iter().any(|row| row.contains("✗")), "{shown:?}");
    assert!(
        shown.iter().any(|row| row.contains("▣ failed")),
        "{shown:?}"
    );
    let mut in_card = false;
    for (line, _) in &rows {
        let shown = line.to_string();
        if !shown.is_empty() && is_edge_row(line) {
            // The card's own edges span the width; the bubble's fit
            // their block.
            if line
                .spans
                .first()
                .is_some_and(|span| span.style.fg == turn())
            {
                assert_eq!(line.width(), 20, "{shown:?}");
                in_card = !in_card;
            }
            continue;
        }
        if shown.is_empty() || !in_card {
            continue;
        }
        assert!(shown.starts_with(' '), "{shown:?}");
        assert!(line.width() <= 19, "{shown:?}");
        assert_eq!(line.style.bg, turn(), "{shown:?}");
    }
}

#[test]
fn blocks_in_a_card_are_separated_by_one_blank_row() {
    // A reply, a group, a steer and the ▣ line: exactly one blank card
    // row between each pair (`docs/tui.md`, "Turns").
    use serde_json::json;
    let mut cards = fresh();
    start(&mut cards, "go");
    text(&mut cards, "a_1", "Hello.");
    step(&mut cards);
    requested(&mut cards, "a_2", "read", json!({"path": "src/a.rs"}));
    completed(&mut cards, "a_2", json!({}));
    steered(&mut cards, "use x");
    done(&mut cards);
    let (rows, _, _) = draw(&cards.turns[0], WIDTH, Edges::BOTH);
    let shown = shown_rows(&rows);
    let reply = shown
        .iter()
        .position(|row| row.contains("Hello."))
        .expect("a reply");
    let group = shown
        .iter()
        .position(|row| row.contains("Read"))
        .expect("a group");
    let steer = shown
        .iter()
        .position(|row| row.starts_with(" steer"))
        .expect("a steer");
    let closing = shown
        .iter()
        .position(|row| row.contains("▣"))
        .expect("a closing");
    assert!(
        reply < group && group < steer && steer < closing,
        "{shown:?}"
    );
    for (first, second) in [(reply, group), (group, steer), (steer, closing)] {
        let between: Vec<&String> = shown[first + 1..second].iter().collect();
        assert_eq!(
            between.iter().filter(|row| row.is_empty()).count(),
            1,
            "{shown:?}"
        );
        // The blank is the last row before the next block: every block
        // holds together.
        assert!(
            between.last().is_some_and(|row| row.is_empty()),
            "{shown:?}"
        );
    }
}

#[test]
fn a_block_that_draws_nothing_adds_no_gap() {
    // A blank reply between two replies: the two drawn blocks sit one
    // blank row apart, not two (`docs/tui.md`, "Turns").
    let mut cards = fresh();
    start(&mut cards, "go");
    text(&mut cards, "a_1", "hi");
    text(&mut cards, "a_2", "   ");
    text(&mut cards, "a_3", "yo");
    done(&mut cards);
    let (rows, _, _) = draw(&cards.turns[0], WIDTH, Edges::BOTH);
    let shown = shown_rows(&rows);
    let first = shown
        .iter()
        .position(|row| row == " hi")
        .expect("a first reply");
    let second = shown
        .iter()
        .position(|row| row == " yo")
        .expect("a second reply");
    assert_eq!(second, first + 2, "{shown:?}");
    assert_eq!(shown[first + 1], "");
}

#[test]
fn a_piece_whose_top_edge_is_withheld_starts_with_a_gap() {
    // A page cut inside a turn withholds the top edge, and the piece
    // opens with a blank row instead, so the joined pages draw what one
    // fold draws (`docs/tui.md`, "History and paging").
    let mut cards = fresh();
    start(&mut cards, "go");
    text(&mut cards, "a_1", "Hello.");
    done(&mut cards);
    // A page continuation carries no prompt bubble: only the card draws.
    cards.turns[0].prompts.clear();
    let (rows, _, pieces) = draw(
        &cards.turns[0],
        WIDTH,
        Edges {
            top: false,
            bottom: true,
        },
    );
    assert_eq!(
        pieces,
        Pieces {
            first: true,
            last: true
        }
    );
    let shown = shown_rows(&rows);
    assert_eq!(shown[0], "");
    assert_eq!(shown[1], " Hello.");
}

#[test]
fn a_card_narrower_than_three_columns_has_no_padding() {
    // Below three columns there is no room past the padding: the pad is
    // zero and rows keep the width (`docs/tui.md`, "Turns").
    for (width, inner) in [(2, 2), (3, 1)] {
        let mut cards = fresh();
        start(&mut cards, "go");
        text(&mut cards, "a_1", "hi");
        done(&mut cards);
        let (rows, _, _) = draw(&cards.turns[0], width, Edges::BOTH);
        let shown = shown_rows(&rows);
        if width == 2 {
            assert!(
                shown.iter().any(|row| row.contains("hi")),
                "{width}: {shown:?}"
            );
        }
        for (line, _) in &rows {
            // The time row is right-aligned text, never padded: only
            // the card's own rows keep the width.
            if line.style.bg.is_some_and(|bg| Some(bg) == turn()) || is_edge_row(line) {
                assert!(line.width() <= usize::from(width), "{width}: {line}");
            }
        }
        if width == 2 {
            assert!(shown.contains(&"hi".to_owned()), "{shown:?}");
        } else {
            assert!(shown.contains(&" h".to_owned()), "{shown:?}");
            assert!(shown.contains(&" i".to_owned()), "{shown:?}");
            let _ = inner;
        }
    }
}

#[test]
fn a_card_at_three_columns_keeps_every_row_inside_its_padding() {
    // At W = 3 the inner width is 1: a reply with a wide glyph, a timed
    // steer, an opened edit group with a long path, a failure and the ▣
    // line all fit in two cells starting with a blank
    // (`docs/tui.md`, "Turns").
    use serde_json::json;
    let mut cards = fresh();
    start(&mut cards, "go");
    text(&mut cards, "a_1", "界 x");
    steered(&mut cards, "use x");
    step(&mut cards);
    let path: String = std::iter::repeat_n('p', 60).collect();
    requested(&mut cards, "a_2", "edit", json!({"path": path}));
    completed(
        &mut cards,
        "a_2",
        json!({"details": {"diff": "-a\n+b"}, "changes": [{"path": path, "added": 3, "removed": 1}]}),
    );
    cards.turns[0].set_open(
        &crate::app::Target::Group(crate::turn::target_id("a_2")),
        true,
    );
    failed(&mut cards);
    let (rows, _, _) = draw(&cards.turns[0], 3, Edges::BOTH);
    let shown = shown_rows(&rows);
    assert!(shown.iter().any(|row| row.contains('▣')), "{shown:?}");
    for (line, _) in &rows {
        let shown = line.to_string();
        if !shown.is_empty() && is_edge_row(line) {
            assert_eq!(line.width(), 3, "{shown:?}");
            continue;
        }
        if shown.is_empty() || !line.style.bg.is_some_and(|bg| Some(bg) == turn()) {
            continue;
        }
        assert_eq!(line.width(), 2, "{shown:?}");
        assert!(shown.starts_with(' '), "{shown:?}");
        assert!(
            line.styled_graphemes(ratatui::style::Style::default())
                .all(|grapheme| crate::format::width(grapheme.symbol) <= 1),
            "{shown:?}"
        );
    }
}

#[test]
fn an_opened_edit_group_wraps_inside_the_padding() {
    // At W = 30 an opened edit group with a 60-cell path keeps every
    // ledger row inside columns 1..=28, and its counts keep the added
    // and removed colours (`docs/tui.md`, "Turns").
    use serde_json::json;
    let mut cards = fresh();
    start(&mut cards, "go");
    step(&mut cards);
    let path: String = std::iter::repeat_n('p', 60).collect();
    requested(&mut cards, "a_2", "edit", json!({"path": path}));
    completed(
        &mut cards,
        "a_2",
        json!({"details": {"diff": "-a\n+b"}, "changes": [{"path": path, "added": 3, "removed": 1}]}),
    );
    cards.turns[0].set_open(
        &crate::app::Target::Group(crate::turn::target_id("a_2")),
        true,
    );
    done(&mut cards);
    let (rows, _, _) = draw(&cards.turns[0], 30, Edges::BOTH);
    let shown = shown_rows(&rows);
    assert!(shown.iter().any(|row| row.contains("+3")), "{shown:?}");
    assert!(shown.iter().any(|row| row.contains("−1")), "{shown:?}");
    let mut ledger = 0;
    for (line, _) in &rows {
        let shown = line.to_string();
        if is_edge_row(line) || shown.is_empty() {
            continue;
        }
        if !line.style.bg.is_some_and(|bg| Some(bg) == turn()) {
            continue;
        }
        assert!(shown.starts_with(' '), "{shown:?}");
        assert!(line.width() <= 29, "{shown:?}");
        if shown.contains('p') {
            ledger += 1;
        }
    }
    assert!(ledger > 0, "{shown:?}");
    let added = rows
        .iter()
        .flat_map(|(line, _)| &line.spans)
        .find(|span| span.style.fg == Some(Role::Added.color()))
        .expect("the added count keeps its span");
    assert!(added.content.contains("+3"), "{added:?}");
    let removed = rows
        .iter()
        .flat_map(|(line, _)| &line.spans)
        .find(|span| span.style.fg == Some(Role::Removed.color()))
        .expect("the removed count keeps its span");
    assert!(removed.content.contains("−1"), "{removed:?}");
}

#[test]
fn a_group_stays_collapsed_after_text_closes_it_in_an_open_turn() {
    let mut cards = fresh();
    start(&mut cards, "go");
    step(&mut cards);
    requested(&mut cards, "a_1", "read", json!({"path": "a.rs"}));
    completed(&mut cards, "a_1", json!({}));
    text(&mut cards, "a_m", "finished the read");
    let (rows, _, _) = draw(&cards.turns[0], WIDTH, Edges::BOTH);
    let shown = shown_rows(&rows);
    assert!(
        shown.iter().any(|row| row.contains("Read 1 file")),
        "{shown:?}"
    );
    assert!(
        !shown.iter().any(|row| row.contains("read a.rs")),
        "{shown:?}"
    );
}

#[test]
fn an_open_turn_with_no_prompt_or_card_has_no_rows() {
    let mut cards = fresh();
    start(&mut cards, "go");
    cards.turns[0].prompts.clear();
    let (rows, _, pieces) = draw(&cards.turns[0], WIDTH, Edges::BOTH);
    assert_eq!(
        pieces,
        Pieces {
            first: false,
            last: false
        }
    );
    assert!(rows.is_empty());
}

#[test]
fn non_driver_start_messages_render_inside_the_card_not_as_prompt_bubbles() {
    let mut cards = fresh();
    feed(
        &mut cards,
        "turn_started",
        None,
        json!({"input": [
            {"type":"message", "source":"driver", "content":[{"type":"text","text":"go"}]},
            {"type":"message", "source":"fiber", "content":[{"type":"text","text":"Fiber: quota notice"}]}
        ]}),
    );
    text(&mut cards, "a_1", "Hello.");
    done(&mut cards);
    let (rows, _, _) = draw(&cards.turns[0], WIDTH, Edges::BOTH);
    let shown = shown_rows(&rows);
    // Only the person's prompt floats above its card, and the Fiber notice
    // sits inside it without its `Fiber: ` prefix (`docs/tui.md`, "Turns").
    assert_eq!(shown.iter().filter(|row| row.contains("go")).count(), 1);
    assert!(shown.iter().any(|row| row.contains("Fiber")), "{shown:?}");
    assert!(
        shown.iter().any(|row| row.contains("quota notice")),
        "{shown:?}"
    );
    assert!(
        !shown.iter().any(|row| row.contains("Fiber: quota notice")),
        "{shown:?}"
    );
    assert_eq!(
        shown.iter().filter(|row| row.starts_with("you ")).count(),
        1,
        "{shown:?}"
    );
}

#[test]
fn message_entries_keep_each_source_label_and_timestamp() {
    use ratatui::style::Modifier;

    let sender = "s_bbbbbbbbbbbbbbbb";
    let mut cards = fresh();
    feed(
        &mut cards,
        "turn_started",
        None,
        json!({"input": [
            {"type":"message", "source":"driver", "content":[{"type":"text","text":"go"}]},
            {"type":"message", "source":"session", "from_session_id":sender, "content":[{"type":"text","text":"please check"}]},
            {"type":"message", "source":"extension", "extension":"reviewer", "content":[{"type":"text","text":"looks good"}]},
            {"type":"message", "source":"fiber", "content":[{"type":"text","text":"Fiber: quota notice"}]},
            {"type":"message", "source":"extension", "extension":"reviewer", "content":[]}
        ]}),
    );
    done(&mut cards);
    let mut out = Rows::default();
    out.session_names = std::sync::Arc::new(std::collections::HashMap::from([(
        SessionId(sender.to_owned()),
        "worker".to_owned(),
    )]));
    let input = cards.turns[0].card_input_with_names(None, false, &out.session_names);
    let messages: Vec<_> = input
        .entries
        .iter()
        .filter_map(|entry| match entry {
            crate::ext::TurnEntry::Message {
                source,
                label,
                text,
                ts,
                ..
            } => Some((*source, label.as_ref(), *text, *ts)),
            crate::ext::TurnEntry::Reply { .. }
            | crate::ext::TurnEntry::Steer { .. }
            | crate::ext::TurnEntry::Group(_)
            | crate::ext::TurnEntry::Aside(_)
            | crate::ext::TurnEntry::Band(_)
            | crate::ext::TurnEntry::Answers(_) => None,
        })
        .collect();
    // Start-input messages open the card in the input's order, each with its
    // source label (`docs/tui.md`, "Turns").
    assert_eq!(
        messages,
        [
            ("session", "worker", "please check", 0),
            ("extension", "reviewer", "looks good", 0),
            ("fiber", "Fiber", "Fiber: quota notice", 0),
            ("extension", "reviewer", "", 0),
        ]
    );
    cards.turns[0].rows(
        WIDTH,
        &TimeZone::UTC,
        Edges::BOTH,
        &crate::image::Layout::default(),
        &mut out,
    );
    let (rows, texts) = out.into_parts();
    let shown = shown_rows(&rows);
    for expected in [
        "worker · 00:00",
        "please check",
        "reviewer · 00:00",
        "looks good",
        "Fiber · 00:00",
        "quota notice",
    ] {
        assert!(
            shown.iter().any(|row| row.contains(expected)),
            "missing {expected:?}: {shown:?}"
        );
    }
    assert!(
        !shown.iter().any(|row| row.contains("Fiber: quota notice")),
        "{shown:?}"
    );
    let fiber = rows
        .iter()
        .find(|(line, _)| line.to_string().contains("Fiber · 00:00"))
        .unwrap();
    let fiber_label = fiber
        .0
        .spans
        .iter()
        .find(|span| span.content == "Fiber")
        .unwrap();
    assert_eq!(
        fiber_label.style.fg,
        Some(Role::Muted.color()),
        "a Fiber label is muted, not dim"
    );
    let fiber_time = fiber
        .0
        .spans
        .iter()
        .find(|span| span.content == " · 00:00 ")
        .unwrap_or_else(|| panic!("fiber time span: {:?}", fiber.0.spans));
    assert!(
        fiber_time.style.add_modifier.contains(Modifier::DIM),
        "the time under it stays dim"
    );
    let fiber_body = rows
        .iter()
        .find(|(line, _)| line.to_string().contains("quota notice"))
        .unwrap_or_else(|| panic!("fiber body row: {shown:?}"));
    assert!(
        fiber_body
            .0
            .spans
            .iter()
            .filter(|span| span.content.contains("quota notice"))
            .all(|span| span.style.add_modifier.contains(Modifier::DIM)),
        "the Fiber body is dim"
    );
    let session = rows
        .iter()
        .find(|(line, _)| line.to_string().contains("worker · 00:00"))
        .unwrap();
    let session_label = session
        .0
        .spans
        .iter()
        .find(|span| span.content == "worker")
        .unwrap();
    assert_eq!(session_label.style.fg, Some(Role::Secondary.color()));
    assert!(
        texts.iter().any(|text| text.tail > 0),
        "the rule is excluded from copied text"
    );
    // The copied card text keeps each message's label and body, in draw
    // order, and loses the labelled rule (`docs/tui.md`, "Turns";
    // `docs/tui.md`, "Selection and copy").
    let copied: Vec<String> = crate::logical::logical(&rows, &texts)
        .into_iter()
        .map(|line| line.text)
        .collect();
    for expected in [
        "worker · 00:00",
        "please check",
        "reviewer · 00:00",
        "looks good",
        "Fiber · 00:00",
        "quota notice",
    ] {
        assert!(
            copied.contains(&expected.to_owned()),
            "missing {expected:?}: {copied:?}"
        );
    }
    assert!(
        copied.iter().all(|line| !line.contains('─')),
        "the labelled rule is decoration: {copied:?}"
    );
}

#[test]
fn a_mid_turn_non_driver_message_is_not_drawn_as_bold_steer() {
    let mut cards = fresh();
    start(&mut cards, "go");
    feed(
        &mut cards,
        "steering_applied",
        None,
        json!({"content": [{"type":"text","text":"Fiber: quota notice"}], "source":"fiber"}),
    );
    done(&mut cards);
    let (rows, _, _) = draw(&cards.turns[0], WIDTH, Edges::BOTH);
    let shown = shown_rows(&rows);
    assert!(shown.iter().any(|row| row.contains("Fiber")), "{shown:?}");
    assert!(
        shown.iter().any(|row| row.contains("quota notice")),
        "{shown:?}"
    );
    assert!(!shown.iter().any(|row| row.contains("steer")), "{shown:?}");
}

#[test]
fn a_session_message_without_an_actual_name_falls_back_to_the_sender_id() {
    let sender = "s_bbbbbbbbbbbbbbbb";
    let mut cards = fresh();
    feed(
        &mut cards,
        "turn_started",
        None,
        json!({"input": [
            {"type":"message", "source":"driver", "content":[{"type":"text","text":"go"}]},
            {"type":"message", "source":"session", "from_session_id":sender,
             "content":[{"type":"text","text":"please check"}]}
        ]}),
    );
    done(&mut cards);
    // No status has named the sender, and a `named` status with an
    // empty name names nothing: the sender's session id is the label.
    let unlabelled: std::collections::HashMap<SessionId, String> = std::collections::HashMap::new();
    let blank = std::collections::HashMap::from([(SessionId(sender.to_owned()), String::new())]);
    for names in [unlabelled, blank] {
        let mut out = Rows::default();
        out.session_names = std::sync::Arc::new(names);
        cards.turns[0].rows(
            WIDTH,
            &TimeZone::UTC,
            Edges::BOTH,
            &crate::image::Layout::default(),
            &mut out,
        );
        let (rows, _) = out.into_parts();
        let shown = shown_rows(&rows);
        assert!(
            shown.iter().any(|row| row.contains(sender)),
            "the sender id labels the message: {shown:?}"
        );
    }
}

#[test]
fn a_session_message_label_follows_the_current_name_at_draw() {
    let sender = "s_bbbbbbbbbbbbbbbb";
    let mut cards = fresh();
    feed(
        &mut cards,
        "turn_started",
        None,
        json!({"input": [
            {"type":"message", "source":"driver", "content":[{"type":"text","text":"go"}]},
            {"type":"message", "source":"session", "from_session_id":sender,
             "content":[{"type":"text","text":"please check"}]}
        ]}),
    );
    done(&mut cards);
    // The card resolves the label at draw time, so a rename reaches a
    // resident card (`docs/tui.md`, "Turns").
    for (name, other) in [("worker", "renamed"), ("renamed", "worker")] {
        let mut out = Rows::default();
        out.session_names = std::sync::Arc::new(std::collections::HashMap::from([(
            SessionId(sender.to_owned()),
            name.to_owned(),
        )]));
        cards.turns[0].rows(
            WIDTH,
            &TimeZone::UTC,
            Edges::BOTH,
            &crate::image::Layout::default(),
            &mut out,
        );
        let (rows, _) = out.into_parts();
        let shown = shown_rows(&rows);
        assert!(
            shown
                .iter()
                .any(|row| row.contains(&format!("{name} · 00:00"))),
            "the label is {name:?}: {shown:?}"
        );
        assert!(
            !shown
                .iter()
                .any(|row| row.contains(&format!("{other} · 00:00"))),
            "the label is not {other:?}: {shown:?}"
        );
    }
}

#[test]
fn a_message_image_draws_below_its_text() {
    let sender = "s_bbbbbbbbbbbbbbbb";
    let mut cards = fresh();
    feed(
        &mut cards,
        "turn_started",
        None,
        json!({"input": [
            {"type":"message", "source":"driver", "content":[{"type":"text","text":"go"}]},
            {"type":"message", "source":"session", "from_session_id":sender, "content":[
                {"type":"text", "text":"please check"},
                {"type":"image", "path":"artifacts/a.png", "mime_type":"image/png",
                 "width":10, "height":8}]}
        ]}),
    );
    done(&mut cards);
    let mut out = Rows::default();
    out.session_names = std::sync::Arc::new(std::collections::HashMap::from([(
        SessionId(sender.to_owned()),
        "worker".to_owned(),
    )]));
    cards.turns[0].rows(
        WIDTH,
        &TimeZone::UTC,
        Edges::BOTH,
        &crate::image::Layout::default(),
        &mut out,
    );
    let (rows, _) = out.into_parts();
    let shown = shown_rows(&rows);
    let label = shown
        .iter()
        .position(|row| row.contains("worker · 00:00"))
        .unwrap_or_else(|| panic!("the label row: {shown:?}"));
    let text = shown
        .iter()
        .position(|row| row.contains("please check"))
        .unwrap_or_else(|| panic!("the text row: {shown:?}"));
    let image = shown
        .iter()
        .position(|row| row.contains("▣ a.png · 10×8"))
        .unwrap_or_else(|| panic!("the image row: {shown:?}"));
    // Images draw below text (`docs/tui.md`, "Turns").
    assert!(
        label < text && text < image,
        "the image draws below the text: {shown:?}"
    );
}

#[test]
fn an_open_card_or_a_handoff_does_not_get_a_trailing_gap() {
    let mut cards = fresh();
    start(&mut cards, "go");
    text(&mut cards, "a_1", "hi");
    let (rows, _, _) = draw(&cards.turns[0], WIDTH, Edges::BOTH);
    let shown = shown_rows(&rows);
    assert_eq!(shown[shown.len() - 2], " hi");
    assert_eq!(shown[shown.len() - 1], "▀".repeat(60));

    let mut cards = fresh();
    start(&mut cards, "go");
    band(&mut cards);
    let (rows, _, pieces) = draw(&cards.turns[0], WIDTH, Edges::BOTH);
    assert_eq!(
        pieces,
        Pieces {
            first: false,
            last: false
        }
    );
    assert!(
        shown_rows(&rows)
            .last()
            .is_some_and(|row| *row == "▀".repeat(60))
    );
}

#[test]
fn a_bubble_whose_card_draws_nothing_yet_keeps_a_trailing_gap() {
    // A running turn with only its bubble keeps the blank row one fold
    // has ahead of its card, where the blank before the card lands once
    // it draws (`docs/tui.md`, "Turns", "History and paging").
    let mut cards = fresh();
    start(&mut cards, "go");
    let (rows, texts, pieces) = draw(&cards.turns[0], WIDTH, Edges::BOTH);
    assert_eq!(
        pieces,
        Pieces {
            first: false,
            last: false
        }
    );
    let shown = shown_rows(&rows);
    assert_eq!(shown[4], "you 00:00 ");
    assert_eq!(shown[5], "");
    assert_eq!(texts[5], RowText::plain());
}

/// The NO_COLOR look: every role paints to the terminal's default.
fn no_color_look() -> crate::look::Look {
    let (look, notice) = crate::look::Look::new(crate::look::ThemeSetting::Dark, &|name: &str| {
        (name == "NO_COLOR").then(|| "1".to_owned())
    });
    assert_eq!(notice, None);
    look
}

/// A finished turn's card rendered and painted without colour.
fn no_color_screen(text: &str) -> ratatui::buffer::Buffer {
    use std::path::PathBuf;

    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    use crate::link::Line as LinkLine;

    crate::surface::init(&|name: &str| (name == "NO_COLOR").then(|| "1".to_owned()));
    let mut app = crate::app::App::new(PathBuf::from("/w"));
    app.set_size(80, 24);
    app.attach(contract::SessionId(SESSION.to_owned()));
    let feed = |app: &mut crate::app::App, kind: &str, action: Option<&str>, payload: Value| {
        app.on_line(LinkLine::Session(envelope(kind, action, 0, payload)));
    };
    feed(
        &mut app,
        "turn_started",
        None,
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
    );
    feed(
        &mut app,
        "text_completed",
        Some("a_1"),
        json!({"text": text}),
    );
    feed(
        &mut app,
        "turn_completed",
        None,
        json!({"outcome": "completed"}),
    );
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    crate::view::render(&app, area, &mut buf, None);
    no_color_look().paint(&mut buf);
    buf
}

#[test]
fn no_color_boxes_a_turn_card_and_leaves_the_prompt_bare() {
    use ratatui::style::{Color, Modifier};
    let buf = no_color_screen("Hello.");
    let text: Vec<String> = (0..24u16)
        .map(|y| (0..80u16).map(|x| buf[(x, y)].symbol()).collect())
        .collect();
    // The prompt bubble's edges draw as blanks and its rows stay: no
    // box sides on the prompt, whose text sits right-aligned.
    let prompt = u16::try_from(
        text.iter()
            .position(|row| row.contains("go"))
            .expect("the prompt's row"),
    )
    .expect("a row");
    assert_eq!(buf[(0, prompt)].symbol(), " ");
    assert_eq!(buf[(79, prompt)].symbol(), " ");
    assert_eq!(text[usize::from(prompt).saturating_sub(1)].trim(), "");
    // The card's top edge draws ┌─┐ and its bottom └─┘, dim, on the
    // default colours, with the reply readable between │ sides. The
    // card is one column narrower than the screen, leaving the scroll
    // bar's column blank.
    let top = u16::try_from(
        text.iter()
            .position(|row| row.starts_with('┌'))
            .expect("the card's top edge"),
    )
    .expect("a row");
    assert_eq!(text[usize::from(top)], format!("┌{}┐ ", "─".repeat(77)));
    let bottom = u16::try_from(
        text.iter()
            .position(|row| row.starts_with('└'))
            .expect("the card's bottom edge"),
    )
    .expect("a row");
    assert!(bottom > top);
    assert_eq!(text[usize::from(bottom)], format!("└{}┘ ", "─".repeat(77)));
    for x in 0..79u16 {
        for y in [top, bottom] {
            let cell = &buf[(x, y)];
            assert_eq!(
                (cell.fg, cell.bg),
                (Color::Reset, Color::Reset),
                "({x}, {y})"
            );
            assert_eq!(cell.modifier, Modifier::DIM, "({x}, {y})");
        }
    }
    let reply = u16::try_from(
        text.iter()
            .position(|row| row.contains("Hello."))
            .expect("the reply's row"),
    )
    .expect("a row");
    assert!(reply > top && reply < bottom);
    for y in top + 1..bottom {
        // A card cut across pages draws no edge at the cut, and the
        // paint keeps the sides dim on the default colours.
        for x in [0, 78] {
            let row: String = (0..80u16).map(|x| buf[(x, y)].symbol()).collect();
            if row.trim().is_empty() {
                continue;
            }
            let cell = &buf[(x, y)];
            assert_eq!(cell.symbol(), "│", "({x}, {y}): {row:?}");
            assert_eq!(
                (cell.fg, cell.bg),
                (Color::Reset, Color::Reset),
                "({x}, {y})"
            );
            assert_eq!(cell.modifier, Modifier::DIM, "({x}, {y})");
        }
    }
    // Bold survives the paint: state words still show with no colour.
    let closing = u16::try_from(
        text.iter()
            .position(|row| row.contains("completed"))
            .expect("the closing row"),
    )
    .expect("a row");
    assert!(closing > top && closing < bottom);
}

#[test]
fn no_color_keeps_box_rows_out_of_the_logical_copy() {
    // The box changes only the edge rows' glyphs: the row count, the
    // edge positions, the body text and the decoration flags match the
    // coloured card across every edge combination, bands included.
    let colour = |edges| {
        crate::surface::init(&|name: &str| (name == "COLORTERM").then(|| "truecolor".to_owned()));
        let mut cards = fresh();
        start(&mut cards, "go");
        band(&mut cards);
        handed_off(&mut cards);
        text(&mut cards, "a_1", "after");
        done(&mut cards);
        draw(&cards.turns[0], WIDTH, edges)
    };
    let boxed = |edges| {
        crate::surface::init(&|name: &str| (name == "NO_COLOR").then(|| "1".to_owned()));
        let mut cards = fresh();
        start(&mut cards, "go");
        band(&mut cards);
        handed_off(&mut cards);
        text(&mut cards, "a_1", "after");
        done(&mut cards);
        draw(&cards.turns[0], WIDTH, edges)
    };
    let is_box = |row: &str| !row.is_empty() && (row.starts_with('┌') || row.starts_with('└'));
    for edges in [
        Edges::BOTH,
        Edges {
            top: false,
            bottom: true,
        },
        Edges {
            top: true,
            bottom: false,
        },
    ] {
        let (colour_rows, colour_texts, _) = colour(edges);
        let (box_rows, box_texts, _) = boxed(edges);
        assert_eq!(box_rows.len(), colour_rows.len(), "{edges:?}");
        for ((box_row, box_text), (colour_row, colour_text)) in box_rows
            .iter()
            .zip(&box_texts)
            .zip(colour_rows.iter().zip(&colour_texts))
        {
            let (box_line, colour_line) = (box_row.0.to_string(), colour_row.0.to_string());
            assert_eq!(box_text.decoration, colour_text.decoration, "{box_line:?}");
            if is_edge_row(&colour_row.0) && colour_row.0.width() == usize::from(WIDTH) {
                // The card's edges box; the prompt bubble's narrow
                // edges stay half blocks, blanked by the paint pass.
                assert!(is_box(&box_line), "{box_line:?}");
            } else {
                assert_eq!(box_line, colour_line, "{box_line:?}");
            }
        }
        // The band and the reply keep their joined text without the
        // borders; no box glyph leaks into the logical copy.
        let logical: Vec<String> = box_texts
            .iter()
            .zip(&box_rows)
            .filter(|(text, _)| !text.decoration)
            .map(|(_, row)| row.0.to_string())
            .collect();
        assert!(
            logical.iter().any(|row| row.contains("after")),
            "{logical:?}"
        );
        assert!(
            logical
                .iter()
                .all(|row| !row.contains('┌') && !row.contains('│')),
            "{logical:?}"
        );
    }
}

/// The closing line's seq ends only a closed card: noting a seq on an
/// open card moves no ending, while the line that closed one does.
#[test]
fn an_open_card_keeps_no_end_seq() {
    use contract::Seq;
    let mut cards = fresh();
    start(&mut cards, "look");
    text(&mut cards, "a_m", "hello");
    let turn = cards.turns.last_mut().expect("the open card");
    turn.note_seq(Some(Seq(0)));
    assert!(turn.end_seq.is_none(), "an open card ends nowhere");
    done(&mut cards);
    let turn = cards.turns.last_mut().expect("the closed card");
    assert!(turn.ended.is_some(), "the card closed");
    turn.note_seq(Some(Seq(9)));
    assert_eq!(turn.end_seq, Some(Seq(9)), "the closing line ends it");
}

/// A card drawn through a seam drawing `fake`, with no summary.
fn draw_seamed(
    turn: &Turn,
    width: u16,
    fake: &std::rc::Rc<crate::ext::tests::SlotRenderer>,
    flat: bool,
) -> (Vec<Row>, Vec<RowText>) {
    let mut out = Rows::default().with_seam(crate::ext::tests::seam(fake, "s", 0, flat));
    turn.rows_with_summary(
        width,
        &TimeZone::UTC,
        Edges::BOTH,
        &crate::image::Layout::default(),
        None,
        &mut out,
    );
    out.into_parts()
}

/// A rich turn: a prompt, a reply, steering, a thought with a call, a
/// warning, answered and declined forms, a done band and an orphaned
/// job, still running.
fn rich() -> Cards {
    let mut cards = fresh();
    start(&mut cards, "go");
    text(&mut cards, "a_1", "Hello.");
    steered(&mut cards, "use x");
    feed(&mut cards, "reasoning_started", Some("a_t"), json!({}));
    feed(
        &mut cards,
        "reasoning_completed",
        Some("a_t"),
        json!({"text": "Plan."}),
    );
    step(&mut cards);
    requested(&mut cards, "a_2", "read", json!({"path": "src/a.rs"}));
    completed(&mut cards, "a_2", json!({}));
    feed(
        &mut cards,
        "mcp_server_failed",
        None,
        json!({"server": "one", "reason": "died", "will_restart": true,
            "error": {"code": "mcp_server_unavailable", "message": "one stopped."}}),
    );
    requested(
        &mut cards,
        "a_q",
        "ask_user",
        json!({"questions": [{"header": "Base"}]}),
    );
    feed(
        &mut cards,
        "interaction_requested",
        None,
        json!({"request_id": "r_1", "kind": "form", "action_ids": ["a_q"],
            "fields": [{"header": "Base", "question": "Which?"}]}),
    );
    feed(
        &mut cards,
        "interaction_resolved",
        None,
        json!({"request_id": "r_1", "by": "person",
            "answers": [{"labels": ["main"]}], "note": "go"}),
    );
    completed(&mut cards, "a_q", json!({}));
    requested(
        &mut cards,
        "a_d",
        "ask_user",
        json!({"questions": [{"header": "Other"}]}),
    );
    feed(
        &mut cards,
        "interaction_requested",
        None,
        json!({"request_id": "r_2", "kind": "form", "action_ids": ["a_d"],
            "fields": [{"header": "Other", "question": "What?"}]}),
    );
    feed(
        &mut cards,
        "interaction_resolved",
        None,
        json!({"request_id": "r_2", "by": "person", "declined": true}),
    );
    completed(&mut cards, "a_d", json!({}));
    band(&mut cards);
    handed_off(&mut cards);
    feed(
        &mut cards,
        "job_started",
        None,
        json!({"job_id": "j1", "description": "Build", "output_path": "/tmp/o"}),
    );
    feed(
        &mut cards,
        "job_completed",
        None,
        json!({"job_id": "j1", "status": "failed",
            "error": {"code": "orphaned", "message": "left"}}),
    );
    cards
}

#[test]
fn a_running_card_marks_its_open_group_running() {
    use crate::ext::TurnEntry;
    use crate::turn::Entry;
    let mut cards = rich();
    let turn = &mut cards.turns[0];
    let group_at = turn
        .entries
        .iter()
        .find_map(|entry| match entry {
            Entry::Group(at) => Some(*at),
            Entry::Reply { .. }
            | Entry::Steer(_)
            | Entry::Aside(_)
            | Entry::Band(_)
            | Entry::Answers(_)
            | Entry::Message(_) => None,
        })
        .expect("a card group");
    let entry_at = turn
        .entries
        .iter()
        .position(|entry| matches!(entry, Entry::Group(at) if *at == group_at))
        .expect("the selected group entry");
    turn.open_group = Some(group_at);
    let card = turn.card_input(None, false);
    let TurnEntry::Group(group) = &card.entries[entry_at] else {
        panic!("the selected card entry is a group");
    };

    // (`docs/tui.md`, "The slots": card entries stay in order; `group_line` exposes whether this is the running open group.)
    assert!(matches!(group.state, crate::ext::TurnGroupState::Running));
}

#[test]
fn a_turn_card_replacement_draws_whole_and_skips_nested_slots() {
    use crate::ext::tests::SlotScript;
    let cards = rich();
    let fake = std::rc::Rc::new(crate::ext::tests::SlotRenderer::filling(
        &["turn_card", "prompt", "reply", "thinking_line"],
        SlotScript::Replace(vec![vec![crate::ext::tests::raw("custom")]]),
    ));
    let (rows, _) = draw_seamed(&cards.turns[0], 60, &fake, false);
    let shown = shown_rows(&rows);
    // (`docs/tui.md`, "How a TUI extension runs": each drawn item is cached by input and width.)
    assert_eq!(fake.calls.borrow().len(), 1, "{:?}", fake.calls.borrow());
    assert_eq!(fake.calls.borrow()[0].1, "turn_card");
    assert!(
        shown.iter().any(|row| row.starts_with("custom")),
        "{shown:?}"
    );
    for built in ["Hello.", "use x", "Read", "⚠", "you answered", "⇄"] {
        assert!(
            !shown.iter().any(|row| row.contains(built)),
            "{shown:?} keeps {built}"
        );
    }
}

#[test]
fn a_turn_card_replacement_marks_its_drawn_card_edges() {
    use crate::ext::tests::{SlotRenderer, SlotScript, raw};
    let cards = rich();
    let fake = std::rc::Rc::new(SlotRenderer::filling(
        &["turn_card"],
        SlotScript::Replace(vec![vec![raw("custom")]]),
    ));
    let mut out = Rows::default().with_seam(crate::ext::tests::seam(&fake, "s", 0, false));
    let pieces = cards.turns[0].rows_with_summary(
        60,
        &jiff::tz::TimeZone::UTC,
        Edges::BOTH,
        &crate::image::Layout::default(),
        None,
        &mut out,
    );

    // (`docs/tui.md`, "The slots": `turn_card` replaces the whole card with its returned lines.)
    assert!(pieces.first);
    assert!(pieces.last);
    assert!(
        out.into_parts()
            .0
            .iter()
            .any(|(line, _)| line.to_string().starts_with("custom"))
    );
}

#[test]
fn a_turn_card_decorator_draws_the_built_in_card_under_its_badge() {
    use crate::ext::tests::{SlotRenderer, SlotScript, raw};
    let cards = rich();
    let fake = std::rc::Rc::new(SlotRenderer::filling(
        &["turn_card"],
        SlotScript::Decorate {
            badge: vec![vec![raw("BADGE")]],
            trim: 5,
        },
    ));
    let (rows, _) = draw_seamed(&cards.turns[0], 60, &fake, false);
    assert_eq!(fake.builtins.borrow().as_slice(), &[55]);
    let shown = shown_rows(&rows);
    assert!(
        shown.iter().any(|row| row.starts_with("BADGE")),
        "{shown:?}"
    );
    assert!(shown.iter().any(|row| row.contains("Hello.")), "{shown:?}");
}

#[test]
fn a_turn_card_error_falls_back_with_one_failure() {
    use crate::ext::FailureKind;
    use crate::ext::tests::{SlotRenderer, SlotScript};
    let cards = rich();
    let fake = std::rc::Rc::new(SlotRenderer::filling(
        &["turn_card"],
        SlotScript::Fail {
            kind: FailureKind::HookFailed,
            message: "boom".into(),
        },
    ));
    let bound = crate::ext::tests::seam(&fake, "s", 0, false);
    for _ in 0..2 {
        let mut out = Rows::default().with_seam(bound.clone());
        cards.turns[0].rows_with_summary(
            60,
            &TimeZone::UTC,
            Edges::BOTH,
            &crate::image::Layout::default(),
            None,
            &mut out,
        );
        let shown = shown_rows(&out.into_parts().0);
        assert!(shown.iter().any(|row| row.contains("Hello.")), "{shown:?}");
    }
    // (`docs/tui.md`, "When an extension fails": one notice names the extension and slot once until `/reload`.)
    assert_eq!(
        fake.calls.borrow().len(),
        1,
        "the failure calls nothing again"
    );
    let listed = bound.cache.take_failures();
    // (`docs/tui.md`, "When an extension fails": one notice names the extension and slot once until `/reload`.)
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].0, "turn_card");
    assert_eq!(listed[0].1.kind, FailureKind::HookFailed);
}

#[test]
fn a_turn_card_timeout_lists_one_extension_failed() {
    use crate::ext::FailureKind;
    use crate::ext::tests::{SlotRenderer, SlotScript};
    let cards = rich();
    let fake = std::rc::Rc::new(SlotRenderer::filling(
        &["turn_card"],
        SlotScript::Fail {
            kind: FailureKind::ExtensionFailed,
            message: "slow".into(),
        },
    ));
    let bound = crate::ext::tests::seam(&fake, "s", 0, false);
    let mut out = Rows::default().with_seam(bound.clone());
    cards.turns[0].rows_with_summary(
        60,
        &TimeZone::UTC,
        Edges::BOTH,
        &crate::image::Layout::default(),
        None,
        &mut out,
    );
    let listed = bound.cache.take_failures();
    // (`docs/tui.md`, "When an extension fails": one notice names the extension and slot once until `/reload`.)
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].1.kind, FailureKind::ExtensionFailed);
    let shown = shown_rows(&out.into_parts().0);
    assert!(shown.iter().any(|row| row.contains("Hello.")), "{shown:?}");
}

#[test]
fn the_card_running_follows_the_open_turn() {
    use crate::ext::TurnCardState;
    let mut cards = fresh();
    start(&mut cards, "go");
    assert!(matches!(
        cards.turns[0].card_input(None, false).state,
        TurnCardState::Running
    ));
    done(&mut cards);
    assert!(matches!(
        cards.turns[0].card_input(None, false).state,
        TurnCardState::Ended(_)
    ));
}

#[test]
fn the_card_input_uses_the_summary_totals_and_ending() {
    use contract::events::{TurnCompleted, TurnOutcome};

    use crate::turn::DrawSummary;
    let mut cards = fresh();
    start(&mut cards, "go");
    step(&mut cards);
    requested(&mut cards, "a_1", "read", json!({"path": "a.rs"}));
    feed(
        &mut cards,
        "usage_recorded",
        None,
        json!({"generation_id": "g1", "model": "fake/m",
            "tokens": {"input": 10, "cache_read": 0, "cache_write": {}, "output": 5},
            "input_bytes": 0, "cost": null}),
    );
    // The card's own totals differ from the summary's, so the input
    // names its source: the card's 10 input tokens against the
    // summary's empty spend.
    let turn = &cards.turns[0];
    assert_eq!(turn.spend.usage().tokens.input, 10);
    assert!(matches!(
        turn.card_input(None, false).state,
        crate::ext::TurnCardState::Running
    ));
    let empty = crate::format::Spend::default();
    let done = (
        TurnCompleted {
            outcome: TurnOutcome::Completed,
            error: None,
            questions: None,
        },
        5_000u64,
    );
    let summary = DrawSummary {
        started: 9,
        calls: 4,
        spend: &empty,
        ended: Some(&done),
    };
    let input = turn.card_input(Some(summary), false);
    assert_eq!(input.started, 9);
    assert_eq!(input.calls, 4);
    assert_eq!(input.usage.tokens.input, 0);
    assert!(
        matches!(
            input.state,
            crate::ext::TurnCardState::Ended(crate::ext::TurnEnded::Completed { ended_ts: 5_000 })
        ),
        "the summary's ending names the close: {:?}",
        input.state
    );
    assert!(matches!(input.state, crate::ext::TurnCardState::Ended(_)));
    let group_running = input.entries.iter().find_map(|entry| match entry {
        crate::ext::TurnEntry::Group(group) => Some(group.state),
        crate::ext::TurnEntry::Reply { .. }
        | crate::ext::TurnEntry::Steer { .. }
        | crate::ext::TurnEntry::Aside(_)
        | crate::ext::TurnEntry::Band(_)
        | crate::ext::TurnEntry::Answers(_)
        | crate::ext::TurnEntry::Message { .. } => None,
    });
    assert_eq!(group_running, Some(crate::ext::TurnGroupState::Finished));
    // An open summary keeps the card's own ending and running: the
    // turn the summary does not close still runs.
    let open = DrawSummary {
        started: 9,
        calls: 4,
        spend: &empty,
        ended: None,
    };
    let input = turn.card_input(Some(open), false);
    assert_eq!(input.started, 9);
    assert!(matches!(input.state, crate::ext::TurnCardState::Running));
}

#[test]
fn the_card_input_holds_every_entry_variant() {
    use crate::ext::{TurnAnswers, TurnAside, TurnBandState, TurnEntry};
    let cards = rich();
    let card = cards.turns[0].card_input(None, false);
    // (`docs/tui.md`, "The slots", `turn_card`: the turn's prompts in order.)
    assert_eq!(card.prompts.len(), 1);
    assert_eq!(card.prompts[0].text, "go");
    assert!(card.prompts[0].images.is_empty());
    assert_eq!(card.started, 0);
    assert!(matches!(card.state, crate::ext::TurnCardState::Running));
    assert!(!card.flat);
    let kinds: Vec<&str> = card
        .entries
        .iter()
        .map(|entry| match entry {
            TurnEntry::Reply { text } => {
                assert_eq!(*text, "Hello.");
                "reply"
            }
            TurnEntry::Steer { text, images, ts } => {
                assert_eq!(*text, "use x");
                assert!(images.is_empty());
                assert_eq!(*ts, 0);
                "steer"
            }
            TurnEntry::Group(group) => {
                // (`docs/tui.md`, "The slots", `turn_card`: a group entry carries the ledger's sections.)
                // The thought folds at step zero, the call after the
                // step line, so they land in different sections.
                assert!(group.sections.len() > 1);
                let thoughts: Vec<&&str> = group
                    .sections
                    .iter()
                    .flat_map(|section| section.thoughts.iter().map(|thought| &thought.text))
                    .collect();
                assert_eq!(thoughts, [&"Plan."]);
                let calls: Vec<&&str> = group
                    .sections
                    .iter()
                    .flat_map(|section| section.calls.iter().map(|call| &call.name))
                    .collect();
                assert_eq!(calls, [&"read", &"ask_user", &"ask_user"]);
                assert!(matches!(
                    group.state,
                    crate::ext::TurnGroupState::Running | crate::ext::TurnGroupState::Finished
                ));
                "group"
            }
            TurnEntry::Aside(TurnAside::Warning { message }) => {
                assert!(message.contains("one stopped"), "{message}");
                "warning"
            }
            TurnEntry::Aside(TurnAside::Orphans { jobs, open }) => {
                // (`docs/tui.md`, "After a crash": the jobs the new process marked `orphaned` are one line naming them all.)
                assert_eq!(jobs.len(), 1);
                assert_eq!(jobs[0].name, "Build");
                assert_eq!(jobs[0].message, "left");
                assert!(!open);
                "orphans"
            }
            TurnEntry::Aside(TurnAside::Line { .. }) => "line",
            TurnEntry::Band(band) => {
                assert_eq!(band.trigger, "automatic");
                let TurnBandState::Done(sizes) = band.state else {
                    panic!("a done band carries its sizes: {:?}", band.state);
                };
                assert_eq!(sizes.before, 1_000);
                assert_eq!(sizes.after, None);
                "band"
            }
            TurnEntry::Answers(TurnAnswers::Given { parts, note, .. }) => {
                // (`docs/tui.md`, "The slots", `answers`: each question's part, in field order.)
                assert_eq!(parts.len(), 1);
                // The header rides padded to 13 cells, as the row draws it.
                assert_eq!(parts[0].header, "Base         ");
                assert_eq!(*note, Some("go"));
                "given"
            }
            TurnEntry::Answers(TurnAnswers::Declined) => "declined",
            TurnEntry::Message { .. } => "message",
        })
        .collect();
    for expected in [
        "reply", "steer", "group", "warning", "given", "declined", "band", "orphans",
    ] {
        assert!(kinds.contains(&expected), "{kinds:?} misses {expected}");
    }
}
