//! Tests for the status view's frame (`docs/tui.md`, "Swapped views").

use super::{Facts, frame};
use crate::swapped::{Ink, List, render};
use contract::SessionId;
use contract::events::SessionStatus;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::json;

/// A `session_status` with the fields the view reads.
fn status(
    git: serde_json::Value,
    turns: Option<u64>,
    calls: u64,
    cost: Option<f64>,
) -> SessionStatus {
    let mut payload = json!({
        "name": "fix the parser",
        "workspace": "/w/fiber",
        "project": "-w-fiber",
        "model": "anthropic/opus",
        "thinking": "high",
        "state": "idle",
        "since": 0, "started": 0, "calls": calls,
        "context": {"tokens": 268_000, "window": 1_000_000},
        "spend": {"tokens": {"input": 1, "cache_read": 0, "cache_write": {}, "output": 2},
            "cost": cost, "subscription_cost": 0.5},
        "delegates": 0, "jobs": 0, "clients": 2,
    });
    if !git.is_null() {
        payload["git"] = git;
    }
    if let Some(turns) = turns {
        payload["turns"] = json!(turns);
    }
    serde_json::from_value(payload).unwrap_or_else(|error| panic!("a status: {error}"))
}

/// Each row's text, the cells joined.
fn lines(facts: &Facts<'_>) -> Vec<String> {
    frame(facts, List::default())
        .rows
        .iter()
        .map(|row| row.iter().map(|(text, _, _)| text.as_str()).collect())
        .collect()
}

#[test]
fn every_row_names_what_the_terminal_holds() {
    let session = SessionId("s_aaaaaaaaaaaaaaaa".to_owned());
    let status = status(json!({"branch": "main"}), Some(3), 12, Some(1.35));
    let facts = Facts {
        session: Some(&session),
        status: Some(&status),
        credential: Some(Some("work")),
        hub: None,
        version: Some("0.4.0"),
    };
    assert_eq!(
        lines(&facts),
        [
            "session      s_aaaaaaaaaaaaaaaa · fix the parser",
            "workspace    /w/fiber · branch main",
            "model        anthropic/opus · thinking high",
            "credential   work",
            "permissions  auto",
            "hub          this machine",
            "version      0.4.0",
            "context      268k of 1M tokens",
            "spend        $1.35 billed · $0.50 subscription",
            "turns        3 turns · 12 calls",
            "clients      2",
        ]
    );
    let built = frame(&facts, List::default());
    assert_eq!(built.title, "Status");
    assert_eq!(built.command, "/status");
    // The name dim, the value plain.
    assert_eq!(built.rows[0][0].2, Ink::Muted);
    assert_eq!(built.rows[0][1].2, Ink::Plain);
}

#[test]
fn values_not_received_yet_say_so_dim() {
    let facts = Facts {
        session: None,
        status: None,
        credential: None,
        hub: Some("work-box"),
        version: None,
    };
    let built = frame(&facts, List::default());
    let rows = lines(&facts);
    for (at, name) in [
        (0, "session"),
        (1, "workspace"),
        (2, "model"),
        (3, "credential"),
        (6, "version"),
        (7, "context"),
        (8, "spend"),
        (9, "turns"),
        (10, "clients"),
    ] {
        assert_eq!(rows[at], format!("{name:<13}not known yet"), "{name}");
        assert_eq!(built.rows[at][1].2, Ink::Muted, "{name}");
    }
    assert_eq!(rows[4], "permissions  auto");
    assert_eq!(rows[5], "hub          work-box");
}

#[test]
fn the_rows_follow_what_the_status_says() {
    let session = SessionId("s_aaaaaaaaaaaaaaaa".to_owned());
    // A detached HEAD, no turn count, one call, no known cost, and a
    // provider that takes no credential.
    let detached = status(json!({"branch": null}), None, 1, None);
    let facts = Facts {
        session: Some(&session),
        status: Some(&detached),
        credential: Some(None),
        hub: None,
        version: Some("0.4.0"),
    };
    let rows = lines(&facts);
    assert_eq!(rows[1], "workspace    /w/fiber · detached");
    assert_eq!(rows[3], "credential   none");
    assert_eq!(rows[8], "spend        unknown billed · $0.50 subscription");
    assert_eq!(rows[9], "turns        1 call");
    // Outside a git repository, and one turn.
    let plain = status(serde_json::Value::Null, Some(1), 2, Some(0.0));
    let facts = Facts {
        status: Some(&plain),
        ..facts
    };
    let rows = lines(&facts);
    assert_eq!(rows[1], "workspace    /w/fiber");
    assert_eq!(rows[9], "turns        1 turn · 2 calls");
}

#[test]
fn the_status_view_snapshot_is_rendered_through_the_shared_frame() {
    let session = SessionId("s_aaaaaaaaaaaaaaaa".to_owned());
    let status = status(json!({"branch": "main"}), Some(3), 12, Some(1.35));
    let facts = Facts {
        session: Some(&session),
        status: Some(&status),
        credential: Some(Some("work")),
        hub: None,
        version: Some("0.4.0"),
    };
    let built = frame(&facts, List::default());
    let area = Rect::new(0, 0, 80, 16);
    let mut buffer = Buffer::empty(area);
    let mut targets = Vec::new();
    render(&built, area, &mut buffer, &mut targets);
    let screen = (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!("status_80x16", screen);
}
