//! Tests for the attention bytes at the loop level: an `attention` line's
//! bytes reach the tty after the frame, and a failed write keeps the
//! terminal running.

use super::Input;
use super::tests::{feed, new_loop};
use crate::link::Line;
use ratatui::backend::TestBackend;
use std::fs::File;

/// One waiting `attention` line for `s_aaaaaaaaaaaaaaaa`.
fn waiting() -> Input {
    Input::Hub(Line::Hub(contract::HubLine {
        kind: "attention".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::json!({
            "session_id": "s_aaaaaaaaaaaaaaaa",
            "name": "fix tests",
            "workspace": "/w",
            "reason": "waiting",
            "summary": "approval: Run cargo test",
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    }))
}

#[test]
fn an_attention_line_writes_its_bytes_after_the_frame() {
    let dir = fakes::TempDir::new("tui-attention");
    let path = dir.path().join("tty");
    let tty = File::create(&path).unwrap_or_else(|err| panic!("create: {err}"));
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), Some(tty));
    lp.app.set_osc9(true);
    assert_eq!(feed(&mut lp, vec![waiting()]), 0);
    let bytes = std::fs::read(&path).unwrap_or_else(|err| panic!("read: {err}"));
    let needle = "\x1b]9;Fiber: fix tests needs you: approval: Run cargo test\x07";
    assert_eq!(
        bytes
            .windows(needle.len())
            .filter(|window| *window == needle.as_bytes())
            .count(),
        1
    );
}

#[test]
fn a_failed_write_keeps_the_terminal_running() {
    let dir = fakes::TempDir::new("tui-attention-failed");
    let path = dir.path().join("tty");
    File::create(&path).unwrap_or_else(|err| panic!("create: {err}"));
    // Open read-only, so every write fails.
    let tty = File::open(&path).unwrap_or_else(|err| panic!("open: {err}"));
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), Some(tty));
    assert_eq!(
        feed(&mut lp, vec![waiting(), Input::Bytes(b"x".to_vec())]),
        0
    );
    assert_eq!(lp.app.take_alerts(), Vec::<u8>::new());
    assert_eq!(lp.app.draft(), "x");
}
