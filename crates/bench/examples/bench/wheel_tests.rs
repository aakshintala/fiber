//! Tests for the wheel workload's pure functions: latency mapping,
//! calibration compares, report strings, session typing and sample rows.
//! Cases run on literal data, with a fake clock only where the send stamp's
//! ordering is under test, and never a terminal.

use contract::clock::Clock;
use contract::{ActionId, SessionId, TurnId};
use serde_json::json;

use std::time::Duration;

use super::{
    POINTER, RAIL, ROWS, SESSION_COUNT, SESSIONS, conversation, distinct, latencies, position,
    send_stamped, session_samples, sgr_wheel, typed,
};

#[test]
fn the_session_count_is_the_session_plan_length() {
    // The scheduler alternates the wheel row by sample
    // (`docs/performance.md`, "Measuring") over this many sessions: a
    // plan that gains or loses one fails here.
    assert_eq!(usize::try_from(SESSION_COUNT), Ok(SESSIONS.len()));
}
use crate::pty::Frame;
use crate::run::System;

fn writes(every: f64, reports: usize) -> Vec<f64> {
    (1..=reports).map(|k| k as f64 * every).collect()
}

#[test]
fn one_frame_covering_all_reports_gives_each_its_share() {
    let covered = latencies(&writes(8.0, 30), &[(248.0, 30)], 30).unwrap();
    assert_eq!(covered.len(), 30);
    for (k, took) in covered.iter().enumerate() {
        assert_eq!(*took, 248.0 - (k + 1) as f64 * 8.0);
    }
}

#[test]
fn one_frame_per_report_gives_each_its_own_latency() {
    let frames: Vec<(f64, usize)> = (1..=30).map(|k| (k as f64 * 8.0 + 2.0, k)).collect();
    let covered = latencies(&writes(8.0, 30), &frames, 30).unwrap();
    assert!(covered.iter().all(|took| *took == 2.0), "{covered:?}");
}

#[test]
fn a_frame_before_a_reports_write_cannot_cover_it() {
    let covered = latencies(&[100.0], &[(90.0, 1), (110.0, 1)], 1).unwrap();
    assert_eq!(covered, [10.0]);
}

#[test]
fn the_send_stamp_is_read_before_the_write_it_precedes() {
    let clock = fakes::clock::FakeClock::new();
    let origin = clock.now();
    // No code under test waits on the clock here; it stamps elapsed time (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(Duration::from_millis(10));
    // The write itself moves the clock on by 5 ms, as a real write would
    // while the reader runs: the stamp must still be the 10 ms read first.
    let sent = send_stamped(&*clock, origin, || {
        // No code under test waits on the clock here; it stamps elapsed time (ruling 6179406086).
        #[allow(
            clippy::disallowed_methods,
            reason = "no code under test waits on the clock (ruling 6179406086)"
        )]
        clock.advance(Duration::from_millis(5));
        Ok(())
    })
    .unwrap();
    assert_eq!(sent, 10.0);
}

#[test]
fn a_frame_at_a_reports_write_covers_it() {
    let covered = latencies(&[100.0], &[(100.0, 1)], 1).unwrap();
    assert_eq!(covered, [0.0]);
}

#[test]
fn an_index_stepping_back_is_an_error() {
    let err = latencies(&writes(8.0, 2), &[(10.0, 1), (20.0, 0)], 2).unwrap_err();
    assert!(err.contains('0'), "{err}");
}

#[test]
fn an_index_past_the_burst_is_an_error() {
    let err = latencies(&writes(8.0, 30), &[(248.0, 31)], 30).unwrap_err();
    assert!(err.contains("31"), "{err}");
}

#[test]
fn a_report_no_frame_shows_is_an_error() {
    let err = latencies(&writes(8.0, 2), &[(10.0, 1)], 2).unwrap_err();
    assert!(err.contains('2'), "{err}");
}

#[test]
fn position_finds_the_exact_text() {
    let calibrated = ["bottom".to_owned(), "up".to_owned(), "top".to_owned()];
    assert_eq!(position("up", &calibrated), Some(1));
}

#[test]
fn position_misses_a_text_it_never_saw() {
    let calibrated = ["bottom".to_owned(), "up".to_owned()];
    assert_eq!(position("sideways", &calibrated), None);
}

#[test]
fn distinct_passes_all_different_texts() {
    let calibrated = ["a".to_owned(), "b".to_owned(), "c".to_owned()];
    assert!(distinct(&calibrated).is_ok());
}

#[test]
fn distinct_fails_a_repeated_text() {
    let err = distinct(&["a".to_owned(), "b".to_owned(), "a".to_owned()]).unwrap_err();
    assert!(err.contains('2'), "{err}");
}

#[test]
fn the_wheel_up_report_names_button_64() {
    assert_eq!(sgr_wheel(true, 20, 10), "\x1b[<64;21;11M");
}

#[test]
fn the_wheel_down_report_names_button_65() {
    assert_eq!(sgr_wheel(false, 20, 10), "\x1b[<65;21;11M");
}

#[test]
fn conversation_cuts_each_row_to_101_columns() {
    // The second row starts with the 24-column rail, which the cut skips.
    let text = format!("{}\n{}short", "a".repeat(160), " ".repeat(24));
    let cut = conversation(&text);
    let mut rows = cut.split('\n');
    assert_eq!(rows.next(), Some("a".repeat(101).as_str()));
    assert_eq!(rows.next(), Some("short"));
    assert_eq!(rows.next(), None);
}

fn envelope(kind: &str, payload: serde_json::Value) -> contract::Envelope {
    contract::Envelope {
        kind: kind.to_owned(),
        session_id: SessionId("s_test".to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    }
}

#[test]
fn session_started_is_skipped_before_it_can_fail() {
    // `{}` has no `workspace` or `variables`, so parsing would fail: the
    // skip runs first.
    let typed = typed(&envelope("session_started", json!({})), 1).unwrap();
    assert_eq!(typed, None);
}

#[test]
fn fiber_started_is_skipped() {
    let typed = typed(&envelope("fiber_started", json!({})), 0).unwrap();
    assert_eq!(typed, None);
}

#[test]
fn tool_call_started_is_completed_with_no_effects() {
    let line = envelope("tool_call_started", json!({}));
    let (event, turn, action) = typed(&line, 4).unwrap().unwrap();
    assert_eq!(
        event,
        contract::events::Event::ToolCallStarted(contract::events::ToolCallStarted {
            always_reviewed: false,
            declared: contract::shapes::DeclaredEffects {
                effects: Vec::new(),
                reversible: true,
                paths: None,
            },
            arguments: None,
            changed_by: None,
        })
    );
    assert_eq!(turn, None);
    assert_eq!(action, None);
}

#[test]
fn usage_recorded_is_rebuilt_with_the_bench_generation() {
    let line = envelope(
        "usage_recorded",
        json!({"input_tokens": 1200, "output_tokens": 300}),
    );
    let (event, _, _) = typed(&line, 7).unwrap().unwrap();
    let contract::events::Event::UsageRecorded(usage) = event else {
        panic!("not a usage line: {event:?}");
    };
    assert_eq!(usage.generation_id.0, "g_7");
    assert_eq!(usage.model, "fake/m");
    assert_eq!(usage.tokens.input, 1200);
    assert_eq!(usage.tokens.output, 300);
    assert_eq!(usage.input_bytes, 4800);
    assert_eq!(usage.cost, None);
}

#[test]
fn a_delta_is_skipped_as_ephemeral() {
    let line = envelope("assistant_message_delta", json!({"text": "part"}));
    let typed = typed(&line, 9).unwrap();
    assert_eq!(typed, None);
}

#[test]
fn a_durable_line_passes_through_with_its_ids() {
    let mut line = envelope("step_started", json!({}));
    line.turn_id = Some(TurnId("t_1".to_owned()));
    line.action_id = Some(ActionId("a_1".to_owned()));
    let (event, turn, action) = typed(&line, 3).unwrap().unwrap();
    assert_eq!(
        event,
        contract::events::Event::StepStarted(Default::default())
    );
    assert_eq!(turn, Some(TurnId("t_1".to_owned())));
    assert_eq!(action, Some(ActionId("a_1".to_owned())));
}

#[test]
fn an_unknown_kind_is_a_setup_error_naming_it() {
    let err = typed(&envelope("quokka_danced", json!({})), 12).unwrap_err();
    assert!(err.contains("quokka_danced"), "{err}");
    assert!(err.contains("12"), "{err}");
}

#[test]
fn a_known_kind_with_a_bad_payload_is_a_setup_error() {
    let err = typed(&envelope("text_completed", json!({})), 5).unwrap_err();
    assert!(err.contains("text_completed"), "{err}");
    assert!(err.contains('5'), "{err}");
}

#[test]
fn the_sample_rows_carry_both_metrics_and_the_counts_shape() {
    let frames = vec![(4.0, 1736usize, 1usize), (9.0, 1800, 30)];
    let rows = session_samples("read", 22.5, &[1.0; 30], &frames, &[2.0; 30]);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, "wheel_read_ms");
    assert_eq!(rows[0].1, json!(22.5));
    assert_eq!(rows[1].0, "wheel_counts");
    let counts = &rows[1].1;
    assert_eq!(counts["session"], json!("read"));
    assert_eq!(counts["reports"], json!(30));
    assert_eq!(counts["latencies_ms"].as_array().map(Vec::len), Some(30));
    assert_eq!(counts["step_ms"].as_array().map(Vec::len), Some(30));
    let frames = counts["frames"].as_array().cloned().unwrap_or_default();
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0]["ms"], json!(4.0));
    assert_eq!(frames[0]["bytes"], json!(1736));
    assert_eq!(frames[0]["index"], json!(1));
}

#[test]
fn the_paging_sample_row_names_the_paging_metric() {
    let rows = session_samples("paging", 30.0, &[1.0; 30], &[], &[2.0; 30]);
    assert_eq!(rows[0].0, "wheel_paging_ms");
    assert_eq!(rows[1].1["session"], json!("paging"));
}

#[test]
fn recorded_frames_keep_their_bytes_and_text() {
    let frame = Frame {
        at: System.now(),
        bytes: 1736,
        text: "rows".to_owned(),
    };
    let rows = session_samples(
        "read",
        1.0,
        &[1.0; 30],
        &[(4.0, frame.bytes, 2)],
        &[2.0; 30],
    );
    assert_eq!(rows[1].1["frames"][0]["bytes"], json!(1736));
    assert_eq!(rows[1].1["frames"][0]["index"], json!(2));
}

/// The wheel scenario's rail and pointer match the TUI's session
/// layout at the benchmark's terminal size: changing the default
/// rail share or placement fails this test without a release build.
#[test]
fn the_rail_and_pointer_match_the_session_layout() {
    let (rail, conversation) = tui::session_columns(160, 48);
    assert_eq!(usize::from(rail), RAIL, "the rail's columns");
    assert!(
        conversation.contains(&POINTER.0),
        "the pointer column {} is inside the conversation {conversation:?}",
        POINTER.0
    );
    assert!(
        POINTER.1 < ROWS,
        "the pointer row {} is on screen ({ROWS} rows)",
        POINTER.1
    );
}
