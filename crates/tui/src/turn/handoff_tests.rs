//! Tests for the handoff band and the nudge, driven through the app.

use std::path::PathBuf;

use ratatui::style::Modifier;
use serde_json::{Value, json};

use crate::app::{App, Target};
use crate::link::Line;
use crate::theme::{Role, style};

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

/// 14:20 UTC in milliseconds: the band's fed time.
const TEA: u64 = 51_600_000;

fn app() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(80, 24);
    app.attach(contract::SessionId(SESSION.to_owned()));
    app
}

fn feed_ts(app: &mut App, kind: &str, action: Option<&str>, ts: u64, payload: Value) {
    app.on_line(Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|id| contract::ActionId(id.to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    }));
}

fn feed(app: &mut App, kind: &str, action: Option<&str>, payload: Value) {
    feed_ts(app, kind, action, 0, payload);
}

fn texts(app: &App) -> Vec<String> {
    app.lines().iter().map(ToString::to_string).collect()
}

fn start(app: &mut App) {
    feed(
        app,
        "turn_started",
        None,
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
    );
}

fn preamble(app: &mut App, trigger_at: Option<u64>) {
    let mut payload = json!({"reason": "start", "model": "fake/m", "context_window": 1_000_000,
        "tool_choice": "auto", "cache_lifetime": "5m", "system_prompt": "", "tools": []});
    if let (Some(at), Some(object)) = (trigger_at, payload.as_object_mut()) {
        object.insert("trigger_at".to_owned(), json!(at));
    }
    feed(app, "preamble_built", None, payload);
}

fn started(app: &mut App, trigger: &str) {
    feed(app, "handoff_started", None, json!({ "trigger": trigger }));
}

fn started_at(app: &mut App, trigger: &str, ts: u64) {
    feed_ts(
        app,
        "handoff_started",
        None,
        ts,
        json!({ "trigger": trigger }),
    );
}

fn completed(app: &mut App, payload: Value) {
    feed(app, "handoff_completed", None, payload);
}

fn completed_at(app: &mut App, payload: Value, ts: u64) {
    feed_ts(app, "handoff_completed", None, ts, payload);
}

fn usage(app: &mut App, id: &str, input: u64, extra: Value) {
    let mut payload = json!({"generation_id": id, "model": "fake/m",
        "tokens": {"input": input, "cache_read": 2_000, "cache_write": {"5m": 1_000}, "output": 50},
        "input_bytes": 0, "cost": null});
    if let (Some(payload), Some(extra)) = (payload.as_object_mut(), extra.as_object()) {
        payload.extend(extra.clone());
    }
    feed(app, "usage_recorded", Some("a_x"), payload);
}

/// The band's content row: the row holding "⇄".
fn band(app: &App) -> String {
    texts(app)
        .into_iter()
        .find(|line| line.contains('⇄'))
        .unwrap_or_default()
}

/// The band's content row index in [`texts`].
fn band_at(app: &App) -> usize {
    texts(app)
        .iter()
        .position(|line| line.contains('⇄'))
        .expect("a band row")
}

/// The closed band row for `left` and `right` at the app's width.
fn full_row(app: &App, left: &str, right: &str) -> String {
    let row = band(app);
    let width = crate::format::width(&row);
    let fill = width
        .saturating_sub(crate::format::width(left))
        .saturating_sub(crate::format::width(right));
    format!("{left}{}{right}", " ".repeat(fill))
}

/// A closed automatic band, fed at 14:20.
fn closed_auto() -> App {
    let mut app = app();
    preamble(&mut app, Some(400_000));
    start(&mut app);
    started_at(&mut app, "auto", TEA);
    completed_at(
        &mut app,
        json!({"outcome": "completed", "note": ["a_n"], "tokens_before": 402_000}),
        TEA,
    );
    app
}

/// [`closed_auto`], sized by the first request after it.
fn sized_auto() -> App {
    let mut app = closed_auto();
    usage(&mut app, "g_next", 29_000, json!({}));
    assert!(band(&app).contains("402k → 32k"));
    app
}

/// The band's content line from [`App::lines`]: the line holding "⇄".
fn band_line(app: &App) -> ratatui::text::Line<'static> {
    app.lines()
        .into_iter()
        .find(|line| line.to_string().contains('⇄'))
        .expect("a band line")
}

#[test]
fn the_closed_band_is_one_row_with_edges() {
    let app = closed_auto();
    let left = " ⇄ handoff · automatic at 400k";
    let right = "402k → … · 14:20 ";
    assert_eq!(band(&app), full_row(&app, left, right));
    // The band row is exactly the conversation's width.
    assert_eq!(crate::format::width(&band(&app)), 79);
    // Half-block edges wrap the band row only.
    let lines = texts(&app);
    let at = band_at(&app);
    assert!(lines[at - 1].chars().all(|c| c == '▄'), "{lines:?}");
    assert_eq!(crate::format::width(&lines[at - 1]), 79);
    assert!(lines[at + 1].chars().all(|c| c == '▀'), "{lines:?}");
    assert_eq!(crate::format::width(&lines[at + 1]), 79);
}

#[test]
fn each_trigger_reads_as_the_band_says_it() {
    for (trigger_at, trigger, says) in [
        (Some(400_000), "auto", "automatic at 400k"),
        (None, "auto", "automatic"),
        (Some(400_000), "person", "you asked with /handoff"),
        (Some(400_000), "overflow", "the request did not fit"),
    ] {
        let mut app = app();
        preamble(&mut app, trigger_at);
        start(&mut app);
        started_at(&mut app, trigger, TEA);
        assert_eq!(
            band(&app),
            full_row(
                &app,
                &format!(" ⇄ handoff · {says}"),
                "● writing the note… · 14:20 "
            ),
            "{trigger}"
        );
    }
    // The latest `preamble_built` wins, an absent `trigger_at` included.
    let mut app = app();
    preamble(&mut app, Some(400_000));
    preamble(&mut app, None);
    start(&mut app);
    started_at(&mut app, "auto", TEA);
    assert_eq!(
        band(&app),
        full_row(
            &app,
            " ⇄ handoff · automatic",
            "● writing the note… · 14:20 "
        ),
    );
}

#[test]
fn the_band_takes_its_time_from_the_started_envelope() {
    // A band the model started with its tool reads the completed
    // envelope's time instead.
    let mut app = app();
    start(&mut app);
    completed_at(
        &mut app,
        json!({"outcome": "completed", "note": ["a_n"], "tokens_before": 402_000}),
        TEA,
    );
    assert_eq!(
        band(&app),
        full_row(
            &app,
            " ⇄ handoff · the model handed off",
            "402k → … · 14:20 "
        ),
    );
}

#[test]
fn a_time_past_jiffs_range_omits_the_clock() {
    let mut app = app();
    preamble(&mut app, Some(400_000));
    start(&mut app);
    started_at(&mut app, "auto", u64::MAX);
    completed_at(
        &mut app,
        json!({"outcome": "completed", "note": ["a_n"], "tokens_before": 402_000}),
        u64::MAX,
    );
    assert_eq!(
        band(&app),
        full_row(&app, " ⇄ handoff · automatic at 400k", "402k → …"),
    );
}

#[test]
fn the_band_reads_the_cards_zone() {
    let mut app = app();
    app.set_zone(crate::local_time::new_york());
    preamble(&mut app, Some(400_000));
    start(&mut app);
    // 14:20 UTC is 09:20 in New York in January 1970.
    started_at(&mut app, "auto", TEA);
    assert!(
        band(&app).ends_with("● writing the note… · 09:20 "),
        "{}",
        band(&app)
    );
}

#[test]
fn the_note_goes_into_the_band_and_the_size_waits_for_the_next_call() {
    let mut app = app();
    preamble(&mut app, Some(400_000));
    start(&mut app);
    feed(
        &mut app,
        "text_completed",
        Some("a_1"),
        json!({"text": "Working."}),
    );
    started_at(&mut app, "auto", TEA);
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
    // The note's own call sizes nothing: the handoff has not completed.
    usage(&mut app, "g_note", 1_000, json!({}));
    let lines = texts(&app);
    let at = band_at(&app);
    assert_eq!(
        lines[at],
        full_row(
            &app,
            " ⇄ handoff · automatic at 400k",
            "● writing the note… · 14:20 "
        ),
    );
    assert!(lines[at - 1].chars().all(|c| c == '▄'));
    assert!(lines[at + 1].chars().all(|c| c == '▀'));
    completed_at(
        &mut app,
        json!({"outcome": "completed", "note": ["a_n"], "tokens_before": 402_000}),
        TEA,
    );
    assert_eq!(
        band(&app),
        full_row(&app, " ⇄ handoff · automatic at 400k", "402k → … · 14:20 "),
    );
    // A correction to the note's call, a copy and an extension's call are
    // not the first request after it.
    usage(&mut app, "g_note", 5_000, json!({}));
    usage(
        &mut app,
        "g_copy",
        5_000,
        json!({"origin_session_id": "s_bbbbbbbbbbbbbbbb"}),
    );
    usage(&mut app, "g_ext", 5_000, json!({"extension": "x"}));
    assert_eq!(
        band(&app),
        full_row(&app, " ⇄ handoff · automatic at 400k", "402k → … · 14:20 "),
    );
    usage(&mut app, "g_next", 29_000, json!({}));
    assert_eq!(
        band(&app),
        full_row(
            &app,
            " ⇄ handoff · automatic at 400k",
            "402k → 32k · 14:20 "
        ),
    );
    // Only the first call after it sizes it.
    usage(&mut app, "g_later", 90_000, json!({}));
    assert_eq!(
        band(&app),
        full_row(
            &app,
            " ⇄ handoff · automatic at 400k",
            "402k → 32k · 14:20 "
        ),
    );
}

#[test]
fn a_call_that_reported_no_input_tokens_leaves_the_size_waiting() {
    let mut app = app();
    preamble(&mut app, Some(400_000));
    start(&mut app);
    started_at(&mut app, "auto", TEA);
    completed_at(
        &mut app,
        json!({"outcome": "completed", "note": ["a_n"], "tokens_before": 402_000}),
        TEA,
    );
    // A call that failed before its provider named a generation: no input
    // tokens, so no size, whatever its output.
    usage(
        &mut app,
        "fiber-0123456789abcdef",
        0,
        json!({"tokens": {"input": 0, "cache_read": 0, "cache_write": {}, "output": 5}}),
    );
    assert_eq!(
        band(&app),
        full_row(&app, " ⇄ handoff · automatic at 400k", "402k → … · 14:20 "),
    );
    usage(&mut app, "g_next", 29_000, json!({}));
    assert_eq!(
        band(&app),
        full_row(
            &app,
            " ⇄ handoff · automatic at 400k",
            "402k → 32k · 14:20 "
        ),
    );
}

#[test]
fn a_failed_or_cancelled_handoff_leaves_the_context_unchanged() {
    let mut app = app();
    start(&mut app);
    started_at(&mut app, "person", TEA);
    completed_at(
        &mut app,
        json!({"outcome": "failed", "tokens_before": 10_000,
            "error": {"code": "io_failed", "message": "The note request failed."}}),
        TEA,
    );
    assert_eq!(
        band(&app),
        full_row(
            &app,
            " ⇄ handoff · you asked with /handoff",
            "failed · the context is unchanged · 14:20 "
        ),
    );
    usage(&mut app, "g_next", 29_000, json!({}));
    started_at(&mut app, "overflow", TEA);
    completed_at(
        &mut app,
        json!({"outcome": "cancelled", "tokens_before": 10_000}),
        TEA,
    );
    let lines = texts(&app);
    let at = band_at(&app);
    assert_eq!(
        lines[at],
        full_row(
            &app,
            " ⇄ handoff · you asked with /handoff",
            "failed · the context is unchanged · 14:20 "
        ),
    );
    // The second band follows the first's lower edge: edge, content,
    // edge. `cancelled` is wider than `failed`, so at this width the
    // left is cut with one "…" and the right keeps its columns.
    assert!(lines[at + 1].chars().all(|c| c == '▀'));
    assert!(lines[at + 2].chars().all(|c| c == '▄'));
    let second = "cancelled · the context is unchanged · 14:20 ";
    assert!(lines[at + 3].ends_with(second));
    assert_eq!(lines[at + 3].chars().filter(|c| *c == '…').count(), 1,);
    assert_eq!(
        crate::format::width(&lines[at + 3]),
        crate::format::width(&lines[at + 2]),
    );
}

#[test]
fn the_outcome_reads_in_attention() {
    let mut app = app();
    start(&mut app);
    started_at(&mut app, "person", TEA);
    completed_at(
        &mut app,
        json!({"outcome": "failed", "tokens_before": 10_000,
            "error": {"code": "io_failed", "message": "The note request failed."}}),
        TEA,
    );
    let line = band_line(&app);
    let outcome = line
        .spans
        .iter()
        .find(|span| span.content.contains("the context is unchanged"))
        .expect("the outcome span");
    assert_eq!(outcome.style, style(Role::Attention));
    // The failure's message stays in the log only: the band reads the
    // outcome word, never the message.
    assert!(!line.to_string().contains("The note request failed."));
}

#[test]
fn the_band_row_styles_each_part() {
    let app = sized_auto();
    let line = band_line(&app);
    let bold = Modifier::BOLD;
    // " ⇄ " and "handoff" in secondary, both bold.
    assert_eq!(line.spans[0].content, " ⇄ ");
    assert_eq!(
        line.spans[0].style,
        style(Role::Secondary).add_modifier(bold)
    );
    assert_eq!(line.spans[1].content, "handoff");
    assert_eq!(
        line.spans[1].style,
        style(Role::Secondary).add_modifier(bold)
    );
    // The trigger dim.
    assert_eq!(line.spans[2].content, " · automatic at 400k");
    assert!(line.spans[2].style.add_modifier.contains(Modifier::DIM));
    // The sizes bold.
    let sizes = line
        .spans
        .iter()
        .find(|span| span.content == "402k → 32k")
        .expect("the sizes span");
    assert!(sizes.style.add_modifier.contains(bold));
    // The clock dim.
    let clock = line
        .spans
        .iter()
        .find(|span| span.content.contains("14:20"))
        .expect("the clock span");
    assert!(clock.style.add_modifier.contains(Modifier::DIM));
    // The content row sits on the handoff tint.
    assert_eq!(line.style.bg, Some(Role::Handoff.color()));
    // The edges read in handoff over the default background.
    let lines = app.lines();
    let at = lines
        .iter()
        .position(|line| line.to_string().contains('⇄'))
        .expect("a band row");
    for edge in [&lines[at - 1], &lines[at + 1]] {
        assert_eq!(edge.style.bg, None);
        for span in &edge.spans {
            assert_eq!(span.style.fg, Some(Role::Handoff.color()));
        }
    }
    assert!(lines[at - 1].to_string().chars().all(|c| c == '▄'));
    assert!(lines[at + 1].to_string().chars().all(|c| c == '▀'));
}

#[test]
fn whole_thousands_drop_their_zero_decimal() {
    for (tokens, says) in [
        (999, "999"),
        (1_000, "1k"),
        (1_049, "1k"),
        (1_050, "1.1k"),
        (32_000, "32k"),
        (32_400, "32.4k"),
        (402_000, "402k"),
        (999_949, "999.9k"),
        (999_950, "1M"),
        (1_000_000, "1M"),
    ] {
        assert_eq!(super::size_for_tests(tokens), says, "{tokens}");
    }
}

#[test]
fn the_band_fits_its_width() {
    let right = "402k → 32k · 14:20 ";
    for width in [100u16, 60, 40, 30, 20, 12] {
        let mut app = app();
        app.set_size(width, 24);
        preamble(&mut app, Some(400_000));
        start(&mut app);
        started_at(&mut app, "auto", TEA);
        completed_at(
            &mut app,
            json!({"outcome": "completed", "note": ["a_n"], "tokens_before": 402_000}),
            TEA,
        );
        usage(&mut app, "g_next", 29_000, json!({}));
        let row = band(&app);
        let cells = crate::format::width(&row);
        assert_eq!(cells, edge_above(&app), "width {width}: {row:?}");
        if !row.contains('…') {
            // Everything fits: no cut.
        } else if cells >= crate::format::width(right) + 2 {
            // Below the full width the left part ends in one "…" with
            // the right part's columns intact.
            assert_eq!(
                row.chars().filter(|c| *c == '…').count(),
                1,
                "width {width}: {row:?}"
            );
            assert!(row.ends_with(right), "width {width}: {row:?}");
        } else {
            // The right part alone does not fit: one "…" for the row.
            assert_eq!(
                row.chars().filter(|c| *c == '…').count(),
                1,
                "width {width}: {row:?}"
            );
        }
    }
    // Width zero draws an empty row, never panics.
    let mut app = app();
    app.set_size(0, 24);
    preamble(&mut app, Some(400_000));
    start(&mut app);
    started_at(&mut app, "auto", TEA);
    assert!(band(&app).is_empty());
}

/// The edge row above the band: the band's drawn width.
fn edge_above(app: &App) -> usize {
    let lines = texts(app);
    let at = band_at(app);
    crate::format::width(&lines[at - 1])
}

#[test]
fn the_fit_boundary_cuts_left_but_keeps_right() {
    // A sized band carries no "…" of its own, so the cut's one "…" is
    // the row's only one. Above the full width the row draws whole;
    // at and below it the left is cut and the right keeps its columns.
    let left = " ⇄ handoff · automatic at 400k";
    let right = "402k → 32k · 14:20 ";
    let full = crate::format::width(left) + 1 + crate::format::width(right);
    let mut drawn = Vec::new();
    for width in 40..100u16 {
        let mut app = app();
        app.set_size(width, 24);
        preamble(&mut app, Some(400_000));
        start(&mut app);
        started_at(&mut app, "auto", TEA);
        completed_at(
            &mut app,
            json!({"outcome": "completed", "note": ["a_n"], "tokens_before": 402_000}),
            TEA,
        );
        usage(&mut app, "g_next", 29_000, json!({}));
        drawn.push((width, band(&app)));
    }
    let (whole, cut): (Vec<_>, Vec<_>) = drawn.into_iter().partition(|(_, row)| !row.contains('…'));
    assert!(!whole.is_empty() && !cut.is_empty());
    // The narrowest whole row is exactly the full width.
    let narrowest = whole
        .iter()
        .map(|(_, row)| crate::format::width(row))
        .min()
        .expect("a whole row");
    assert_eq!(narrowest, full);
    for (width, row) in &cut {
        let cells = crate::format::width(row);
        assert_eq!(
            row.chars().filter(|c| *c == '…').count(),
            1,
            "width {width}: {row:?}"
        );
        if cells >= crate::format::width(right) + 2 {
            assert!(row.ends_with(right), "width {width}: {row:?}");
        }
    }
}

#[test]
fn a_writing_band_marks_its_dot_where_it_lands() {
    let mut app = app();
    preamble(&mut app, Some(400_000));
    start(&mut app);
    started_at(&mut app, "auto", TEA);
    let shown = app.shown(0, usize::MAX);
    let at = shown
        .lines
        .iter()
        .position(|(line, _, _)| line.to_string().contains('⇄'))
        .expect("a band line");
    let text = shown.lines[at].0.to_string();
    let col = text.chars().take_while(|c| *c != '●').count();
    let col = u16::try_from(col).expect("a column");
    assert_eq!(shown.spins, vec![(at, col)]);
    // The dot lands after the left part and the fill.
    assert_eq!(col, 51);
}

#[test]
fn a_writing_band_cut_past_its_dot_marks_nothing() {
    // At widths 20 and 12 the right part alone cannot fit, so the whole
    // row is cut and no cell spins.
    for width in [20u16, 12] {
        let mut app = app();
        app.set_size(width, 24);
        preamble(&mut app, Some(400_000));
        start(&mut app);
        started_at(&mut app, "auto", TEA);
        let shown = app.shown(0, usize::MAX);
        assert!(shown.spins.is_empty(), "width {width}");
        let row = band(&app);
        assert_eq!(
            crate::format::width(&row),
            edge_above(&app),
            "width {width}"
        );
    }
}

#[test]
fn a_finished_band_is_not_marked() {
    let app = closed_auto();
    assert!(band(&app).contains("402k → …"));
    assert_eq!(app.shown(0, usize::MAX).spins, Vec::new());
}

#[test]
fn a_hooks_note_and_a_tool_started_handoff() {
    let mut app = app();
    start(&mut app);
    // The model's own tool writes no `handoff_started`.
    completed_at(
        &mut app,
        json!({"outcome": "completed", "tokens_before": 300_000,
            "note_text": "From the hook.", "extension": "notes"}),
        TEA,
    );
    let lines = texts(&app);
    let at = band_at(&app);
    assert_eq!(
        lines[at],
        full_row(
            &app,
            " ⇄ handoff · the model handed off",
            "300k → … · 14:20 "
        ),
    );
    assert!(lines[at - 1].chars().all(|c| c == '▄'));
    assert!(lines[at + 1].chars().all(|c| c == '▀'));
    assert_eq!(lines[at + 2], "  ▸ note");
    let note = app.targets().into_iter().map(|(_, t)| t).next();
    if let Some(note) = note {
        app.open(note);
    }
    let lines = texts(&app);
    let at = band_at(&app);
    assert_eq!(lines[at + 3], "    From the hook.");
    // With no turn open there is no card to break.
    let mut app = self::app();
    started(&mut app, "person");
    completed(
        &mut app,
        json!({"outcome": "completed", "tokens_before": 1}),
    );
    assert!(texts(&app).is_empty());
}

#[test]
fn the_band_ends_the_open_group() {
    let mut app = app();
    start(&mut app);
    feed(
        &mut app,
        "tool_call_requested",
        Some("a_1"),
        json!({"name": "read", "arguments": {"path": "a.rs"}}),
    );
    started_at(&mut app, "auto", TEA);
    completed_at(
        &mut app,
        json!({"outcome": "completed", "tokens_before": 1_000}),
        TEA,
    );
    feed(
        &mut app,
        "tool_call_requested",
        Some("a_2"),
        json!({"name": "read", "arguments": {"path": "b.rs"}}),
    );
    feed(
        &mut app,
        "turn_completed",
        None,
        json!({"outcome": "completed"}),
    );
    let lines = texts(&app);
    let at = band_at(&app);
    // The card above ends at the band; a second card below it continues
    // the turn, and only that card's ▣ line closes it.
    assert!(lines[at - 1].chars().all(|c| c == '▄'));
    let closings: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.contains('▣'))
        .map(|(at, _)| at)
        .collect();
    assert_eq!(closings.len(), 1, "{lines:?}");
    assert!(closings[0] > at, "{lines:?}");
}

#[test]
fn the_nudge_is_one_dim_line_where_it_happened() {
    let nudge = "◔ Context at 268k, two thirds of the way to the 400k handoff. The \
                 model was told a handoff keeps the work going.";
    let mut app = app();
    feed(
        &mut app,
        "context_nudged",
        None,
        json!({"tokens": 268_000, "trigger_at": 400_000}),
    );
    start(&mut app);
    feed(
        &mut app,
        "context_nudged",
        None,
        json!({"tokens": 268_000, "trigger_at": 400_000}),
    );
    assert_eq!(
        texts(&app),
        [
            nudge,
            "",
            "▄▄▄▄▄",
            " go ▐",
            "▀▀▀▀▀",
            "you 00:00 ",
            "",
            "▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄",
            " ◔ Context at 268k, two thirds of the way to the 400k handoff. The model was",
            " told a handoff keeps the work going.",
            "▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀",
        ]
    );
    // Both pieces of the split nudge line stay dim.
    assert!(
        app.lines()
            .iter()
            .filter(|line| line.to_string().starts_with(" ◔ Context")
                || line.to_string() == " was told a handoff keeps the work going.")
            .all(|line| line
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::DIM))
    );
}

/// A band whose note streamed as `deltas` (action and text) and then
/// `done` (action and full text), completed; its note opened.
fn note_of(deltas: &[(&str, &str)], done: &[(&str, &str)]) -> Vec<String> {
    let mut app = app();
    start(&mut app);
    started(&mut app, "person");
    for (action, text) in deltas {
        feed(
            &mut app,
            "assistant_message_delta",
            Some(action),
            json!({ "text": text }),
        );
    }
    for (action, text) in done {
        feed(
            &mut app,
            "text_completed",
            Some(action),
            json!({ "text": text }),
        );
    }
    completed(
        &mut app,
        json!({"outcome": "completed", "tokens_before": 1_000}),
    );
    let note = app
        .targets()
        .into_iter()
        .find_map(|(_, target)| matches!(target, Target::Note(_)).then_some(target));
    assert!(note.is_some(), "{:?}", texts(&app));
    if let Some(note) = note {
        app.open(note);
    }
    let lines = texts(&app);
    let note_at = lines
        .iter()
        .position(|line| line == "  ▸ note")
        .expect("a note row");
    lines[note_at..].to_vec()
}

#[test]
fn a_streamed_note_joins_each_parts_deltas() {
    // A part's deltas join; the note keeps them with no completion.
    assert_eq!(
        note_of(&[("a_1", "## No"), ("a_1", "te")], &[]),
        ["  ▸ note", "    ## Note"]
    );
    // Each part streams on its own.
    assert_eq!(
        note_of(&[("a_1", "one"), ("a_2", "two"), ("a_1", " more")], &[]),
        ["  ▸ note", "    one more", "    two"]
    );
    // A completion replaces its own part only.
    assert_eq!(
        note_of(&[("a_1", "one")], &[("a_2", "two")]),
        ["  ▸ note", "    one", "    two"]
    );
}

#[test]
fn set_open_reports_whether_the_note_matched() {
    let mut band = super::Band::new(3, "the model handed off".to_owned(), TEA);
    // The matching target sets the flag and answers true, both ways.
    assert!(band.set_open(&Target::Note(3), true));
    assert!(band.open);
    assert!(band.set_open(&Target::Note(3), false));
    assert!(!band.open);
    // Another band's note and another kind answer false and change nothing.
    assert!(!band.set_open(&Target::Note(4), true));
    assert!(!band.open);
    assert!(!band.set_open(&Target::Orphans(3), true));
    assert!(!band.open);
}

#[test]
fn the_band_entry_carries_trigger_ts_and_state() {
    use crate::ext::TurnBandState;
    let mut writing = super::Band::new(1, "automatic".to_owned(), 6);
    let mut band = writing.turn_entry();
    assert_eq!(band.trigger, "automatic");
    assert_eq!(band.ts, 6);
    assert!(matches!(band.state, TurnBandState::Writing));
    assert_eq!(band.sizes, None);
    for (state, cancelled) in [
        (
            super::State::Done {
                before: 7,
                after: Some(9),
            },
            None,
        ),
        (super::State::Unchanged { cancelled: false }, Some(false)),
        (super::State::Unchanged { cancelled: true }, Some(true)),
    ] {
        writing.state = state;
        band = writing.turn_entry();
        match (band.state, cancelled) {
            (TurnBandState::Done, None) => {
                let sizes = band.sizes.expect("a done band carries sizes");
                assert_eq!((sizes.before, sizes.after), (7, Some(9)));
            }
            (TurnBandState::Unchanged { cancelled: got }, Some(want)) => {
                assert_eq!(got, want);
                assert_eq!(band.sizes, None);
            }
            (state, cancelled) => panic!("wrong state {state:?} {cancelled:?}"),
        }
    }
}
