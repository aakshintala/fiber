//! Screen tests for the scroll-up stall through long messages (`#4059`).
//!
//! The ticket shape is thousands of entries with single messages and tool
//! outputs of tens of thousands of rows, linked across four sessions. The
//! shape test pins that shape; the screen test pins visible rows and page
//! loads.

use std::ops::RangeInclusive;

use contract::events::EventKind;
use contract::{Envelope, Seq, SessionId};
use serde_json::Value;

use crate::app::App;
use crate::link::Line;

/// Counts durable lines (those carrying `seq`) in `events`.
fn durable_count(events: &str) -> usize {
    events
        .lines()
        .filter(|line| !line.trim().is_empty() && line.contains("\"seq\""))
        .count()
}

/// The `text` of every `text_completed` in `events`, in order.
fn message_texts(events: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in events.lines() {
        if line.trim().is_empty() || !line.contains("\"text_completed\"") {
            continue;
        }
        let envelope: contract::Envelope = serde_json::from_str(line)
            .unwrap_or_else(|error| panic!("ticket line does not parse: {error}"));
        if envelope.kind() != EventKind::TextCompleted {
            continue;
        }
        let text = envelope
            .payload
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        out.push(text);
    }
    out
}

/// The `text` of every tool output in `events`, in order.
fn output_texts(events: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in events.lines() {
        if line.trim().is_empty() || !line.contains("\"tool_call_completed\"") {
            continue;
        }
        let envelope: contract::Envelope = serde_json::from_str(line)
            .unwrap_or_else(|error| panic!("ticket line does not parse: {error}"));
        if envelope.kind() != EventKind::ToolCallCompleted {
            continue;
        }
        let text = envelope
            .payload
            .get("content")
            .and_then(Value::as_array)
            .and_then(|content| content.first())
            .and_then(|first| first.get("text"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        out.push(text);
    }
    out
}

/// Rows in `text`: paragraphs separated by blank lines. Long ticket texts
/// join rows with double newlines so markdown draws each as its own row;
/// single newlines would fold as soft breaks.
fn rows(text: &str) -> usize {
    if text.is_empty() {
        0
    } else {
        text.split("\n\n").count()
    }
}

/// The ticket session carries thousands of entries with one long message
/// and one long tool output of tens of thousands of rows, and the chain
/// links four sessions with three `forked_from` links. The jig reports the
/// pages and rows the session draws.
#[test]
fn shape_ticket_session() {
    let events = crate::paging_session::ticket_session(1);
    let durable = durable_count(&events);
    assert!(
        durable >= 3_000,
        "ticket session holds thousands of entries, found {durable}"
    );
    let steps = events
        .lines()
        .filter(|line| line.contains("\"step_started\""))
        .count();
    assert_eq!(steps, crate::paging_session::TICKET_LARGE_STEPS);
    let messages = message_texts(&events);
    let longest_message = messages.iter().map(|text| rows(text)).max().unwrap_or(0);
    assert_eq!(
        longest_message,
        crate::paging_session::TICKET_LONG_ROWS,
        "one single message holds tens of thousands of rows"
    );
    let long_messages = messages
        .iter()
        .filter(|text| rows(text) == crate::paging_session::TICKET_LONG_ROWS)
        .count();
    assert_eq!(
        long_messages, 1,
        "exactly one message is long, found {long_messages}"
    );
    let outputs = output_texts(&events);
    let longest_output = outputs.iter().map(|text| rows(text)).max().unwrap_or(0);
    assert_eq!(
        longest_output,
        crate::paging_session::TICKET_LONG_ROWS,
        "one tool output holds tens of thousands of rows"
    );
    let long_outputs = outputs
        .iter()
        .filter(|text| rows(text) == crate::paging_session::TICKET_LONG_ROWS)
        .count();
    assert_eq!(
        long_outputs, 1,
        "exactly one tool output is long, found {long_outputs}"
    );
    // The message beside the long one stays short: flipping the step
    // equality would make every step long.
    let short_messages = messages.iter().filter(|text| rows(text) == 1).count();
    assert!(
        short_messages >= 100,
        "steps beside the long message stay short, found {short_messages} single-row messages"
    );
    let chain = crate::paging_session::ticket_chain();
    assert_eq!(chain.len(), 4, "the chain holds four sessions");
    for (at, (_, log)) in chain.iter().enumerate() {
        if at == 1 || at == 2 {
            let small_messages = message_texts(log);
            let small_long = small_messages
                .iter()
                .filter(|text| rows(text) == crate::paging_session::TICKET_LONG_ROWS)
                .count();
            assert_eq!(
                small_long, 0,
                "middle links stay small, found {small_long} long messages"
            );
        }
    }
    let mut links = 0usize;
    for (_, log) in &chain {
        let first = log
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or_default()
            .to_owned();
        if first.contains("forked_from") {
            links += 1;
        }
    }
    assert_eq!(links, 3, "several forked_from links join the chain");
    let mut app = crate::app::App::new(None);
    app.set_size(160, 48);
    for line in events.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let envelope: contract::Envelope = serde_json::from_str(line)
            .unwrap_or_else(|error| panic!("ticket line does not parse: {error}"));
        if app.session().is_none() {
            app.attach(envelope.session_id.clone());
        }
        app.on_line(crate::link::Line::Session(envelope));
    }
    let pages = app.pages().index().pages().len();
    let rows_total = app.pages().index().total();
    assert!(pages > 0, "the jig pages the ticket session");
    assert!(
        rows_total > 20_000,
        "the long message draws tens of thousands of rows, found {rows_total}"
    );
}

/// The durable lines in `range`, as the hub answers for `session`.
fn history(lines: &[Envelope], session: &SessionId, range: &RangeInclusive<Seq>) -> Vec<Envelope> {
    lines
        .iter()
        .filter(|line| {
            &line.session_id == session && line.seq.is_some_and(|seq| range.contains(&seq))
        })
        .cloned()
        .collect()
}

#[test]
fn screen_pages_visible_rows() {
    let events = crate::paging_session::ticket_session(1);
    let lines: Vec<Envelope> = events
        .lines()
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("ticket line does not parse: {error}"))
        })
        .collect();
    let mut app = App::new(None);
    app.set_size(80, 48);
    app.attach(
        lines
            .first()
            .expect("the ticket session has a first line")
            .session_id
            .clone(),
    );
    for line in &lines {
        app.on_line(Line::Session(line.clone()));
    }

    let long_seq = lines
        .iter()
        .find(|line| {
            line.kind() == EventKind::TextCompleted
                && line
                    .payload
                    .get("text")
                    .and_then(Value::as_str)
                    .is_some_and(|text| {
                        text.starts_with("the session log holds every durable line")
                    })
        })
        .and_then(|line| line.seq)
        .expect("the ticket message has a durable sequence");
    let page_at = app
        .pages()
        .index()
        .pages()
        .iter()
        .position(|page| page.first_seq <= long_seq && long_seq <= page.last_seq)
        .expect("the long message belongs to a page");
    let top = app
        .pages()
        .index()
        .starts()
        .get(page_at)
        .copied()
        .expect("the message page has a row start");
    app.jump(top);
    let bound = app.pages().index().pages().len().saturating_add(1);
    let mut page_loads = 0;
    for _ in 0..bound {
        let Some((session, range)) = app.needs().into_iter().next() else {
            break;
        };
        app.load(&session, history(&lines, &session, &range));
        page_loads += 1;
        assert_ne!(
            app.needs().first(),
            Some(&(session, range)),
            "loading did not settle"
        );
    }
    assert!(
        app.needs().is_empty(),
        "the screen window is still missing pages"
    );
    // `docs/tui.md`, "History and paging": the window covers the screen
    // plus one screen above and below, and pages load inside the frame.
    assert!(page_loads >= 1, "the visible message needs a page load");

    let shown = app.shown(top, 48);
    assert!(
        shown.lines.iter().any(|(line, _, _)| {
            line.to_string()
                .contains("the session log holds every durable line")
        }),
        "the visible rows contain the long message"
    );
}
