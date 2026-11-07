//! Screen tests for the scroll-up stall through long messages (`#4059`).
//!
//! The ticket shape is thousands of entries with single messages and tool
//! outputs of tens of thousands of rows, linked across four sessions. The
//! shape test pins that shape; the page-load test pins that loading one
//! page stays window-bounded.

use contract::events::EventKind;
use serde_json::Value;

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
    let events = crate::paging_session::ticket_session();
    let durable = durable_count(&events);
    assert!(
        durable >= 3_000,
        "ticket session holds thousands of entries, found {durable}"
    );
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
