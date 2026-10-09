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

fn surface() -> Option<ratatui::style::Color> {
    Some(Role::Surface.color())
}

#[test]
fn the_card_is_one_surface_below_the_bubble() {
    // The bubble and its time row float above the card; the card's body
    // sits on the surface tint between half-block edges
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
    assert_eq!(rows.len(), 8);
    let shown = shown_rows(&rows);
    assert_eq!(shown[0], "▄".repeat(5));
    assert_eq!(shown[1], " go ▐");
    assert_eq!(shown[2], "▀".repeat(5));
    assert_eq!(shown[3], "00:00");
    assert_eq!(shown[4], "▄".repeat(60));
    assert_eq!(shown[5], "Hello.");
    assert_eq!(shown[6], "▣ completed");
    assert_eq!(shown[7], "▀".repeat(60));
    for (line, target) in rows.iter().take(4) {
        assert_eq!(line.style.bg, None);
        assert!(target.is_none());
    }
    assert_eq!(rows[4].0.spans[0].style.fg, surface());
    for (line, _) in rows.iter().skip(5).take(2) {
        assert_eq!(line.style.bg, surface());
    }
    assert_eq!(rows[7].0.spans[0].style.fg, surface());
}

#[test]
fn a_turn_with_only_its_prompt_has_no_card() {
    // No entries and no ending: no card edge, no tinted row
    // (`docs/tui.md`, "Turns").
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
    assert_eq!(rows.len(), 4);
    for (line, _) in &rows {
        assert_eq!(line.style.bg, None);
    }
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
    assert_eq!(shown[4], "▄".repeat(60));
    assert_eq!(shown[5], "▣ completed");
    assert_eq!(shown[6], "▀".repeat(60));
    assert_eq!(rows[5].0.style.bg, surface());
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
    assert_eq!(shown[4], "▄".repeat(60));
    assert_eq!(shown[5], "before");
    assert_eq!(shown[6], "▀".repeat(60));
    assert!(shown[7].starts_with("⇄ Handoff"), "{}", shown[7]);
    assert_eq!(shown[8], "▄".repeat(60));
    assert_eq!(shown[9], "after");
    assert_eq!(shown[10], "▣ completed");
    assert_eq!(shown[11], "▀".repeat(60));
    assert_eq!(rows[7].0.style.bg, surface());
}

#[test]
fn an_open_note_sits_inside_the_band() {
    // The band row, "▸ note" and every note row sit on the surface with
    // no edge between them (`docs/tui.md`, "Handoff").
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
        .position(|line| line.starts_with('⇄'))
        .expect("a band");
    assert_eq!(lines[band_at + 1], "  ▸ note");
    assert_eq!(lines[band_at + 2], "    ## Note");
    assert_eq!(lines[band_at + 3], "    keep going");
    for line in app.lines().iter().skip(band_at).take(4) {
        assert_eq!(line.style.bg, surface());
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
    assert_eq!(buf[(78, row)].bg, Role::Surface.color());
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
    assert!(shown[4].starts_with("⇄ Handoff"), "{shown:?}");
    assert_eq!(shown[5], "▄".repeat(60));
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
    assert!(
        shown
            .last()
            .is_some_and(|line| line.starts_with("⇄ Handoff")),
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
    assert!(
        shown
            .last()
            .is_some_and(|line| line.starts_with("⇄ Handoff")),
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
    let (rows, _, _) = draw(
        &cards.turns[0],
        WIDTH,
        Edges {
            top: false,
            bottom: true,
        },
    );
    let shown = shown_rows(&rows);
    assert_eq!(shown[4], "before");
    assert_eq!(shown[5], "▀".repeat(60));
    assert!(shown[6].starts_with("⇄ Handoff"), "{shown:?}");
    // The piece after the band still opens with an edge.
    assert_eq!(shown[7], "▄".repeat(60));
    let (rows, _, _) = draw(
        &cards.turns[0],
        WIDTH,
        Edges {
            top: true,
            bottom: false,
        },
    );
    let shown = shown_rows(&rows);
    assert_eq!(shown[4], "▄".repeat(60));
    assert_eq!(shown[5], "before");
    // The piece before the band still closes with an edge.
    assert_eq!(shown[6], "▀".repeat(60));
    assert!(
        shown.last().is_some_and(|line| line == "▣ completed"),
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
fn nothing_in_the_card_is_indented_or_striped() {
    // Replies, steering messages and answers all start at the card's edge
    // (`docs/tui.md`, "Turns").
    let mut cards = fresh();
    start(&mut cards, "go");
    feed(
        &mut cards,
        "steering_applied",
        None,
        json!({"content": [{"type": "text", "text": "use x"}], "source": "driver"}),
    );
    text(&mut cards, "a_1", "Hello.");
    done(&mut cards);
    let (rows, _, _) = draw(&cards.turns[0], WIDTH, Edges::BOTH);
    for (line, _) in rows.iter().skip(4) {
        let shown = line.to_string();
        if shown.chars().all(|ch| ch == '▄' || ch == '▀') {
            continue;
        }
        assert!(!shown.starts_with(' '), "{shown:?}");
        assert!(!shown.contains('▌'), "{shown:?}");
    }
}
