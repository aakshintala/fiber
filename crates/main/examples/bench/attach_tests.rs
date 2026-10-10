use serde_json::json;

use crate::busy::filler;

use super::{
    ATTACH_TAIL, PROBE_A, PROBE_B, Step, TURN_REPLY_BYTES, feed_step, plan_turns, size_note,
    stage_rows,
};

fn status(id: &str, state: &str) -> serde_json::Value {
    json!({
        "kind": "session_status",
        "session_id": id,
        "payload": {"state": state},
    })
}

fn left(id: &str, how: &str) -> serde_json::Value {
    json!({
        "kind": "session_left",
        "payload": {"session_id": id, "how": how},
    })
}

#[test]
fn feed_step_is_gone_on_an_idle_status_then_session_left() {
    // The order the hub sends for a left session: its last idle status,
    // then its `session_left`. With the process gone the status waits.
    assert_eq!(feed_step(&status("s_1", "idle"), "s_1", false), Step::Wait);
    assert!(matches!(
        feed_step(&left("s_1", "crashed"), "s_1", false),
        Step::Gone(_)
    ));
    // Another session leaving is nothing about this one.
    assert_eq!(feed_step(&left("s_2", "exited"), "s_1", true), Step::Wait);
}

#[test]
fn feed_step_is_ready_on_an_idle_status_while_the_process_runs() {
    assert_eq!(feed_step(&status("s_1", "idle"), "s_1", true), Step::Ready);
    assert_eq!(
        feed_step(&json!({"kind": "hub_hello"}), "s_1", true),
        Step::Wait
    );
    assert_eq!(
        feed_step(&json!({"kind": "command_accepted"}), "s_1", true),
        Step::Wait
    );
}

#[test]
fn feed_step_waits_on_another_sessions_status_and_a_busy_state() {
    assert_eq!(feed_step(&status("s_2", "idle"), "s_1", true), Step::Wait);
    assert_eq!(
        feed_step(&status("s_1", "streaming"), "s_1", true),
        Step::Wait
    );
}

#[test]
fn plan_turns_grows_whole_turns_to_the_bands_low_end() {
    // A 15,000-byte first turn with 12,000-byte growth turns: eighty-eight
    // turns reach 1,059,000, inside the 1 MiB band, and eight hundred
    // seventy-four reach 10,491,000, inside the 10 MiB band.
    assert_eq!(plan_turns(1_048_576, 15_000, 12_000).unwrap(), 88);
    assert_eq!(size_note("1 MiB", 15_000 + 87 * 12_000), None);
    assert_eq!(plan_turns(10_485_760, 15_000, 12_000).unwrap(), 874);
    assert_eq!(size_note("10 MiB", 15_000 + 873 * 12_000), None);
    // A shortfall smaller than one growth turn still takes a whole one.
    assert_eq!(plan_turns(1_048_576, 1_040_000, 12_000).unwrap(), 2);
}

#[test]
fn plan_turns_needs_one_turn_at_most_and_fails_on_empty_growth() {
    assert_eq!(plan_turns(1_048_576, 1_048_576, 170_000).unwrap(), 1);
    assert_eq!(plan_turns(1_048_576, 2_000_000, 170_000).unwrap(), 1);
    assert!(plan_turns(1_048_576, 200_000, 0).is_err());
}

#[test]
fn the_probe_fixtures_measure_one_turn_then_a_turn_with_its_handoff() {
    assert_eq!((PROBE_A.turns, PROBE_A.handoffs), (1, 0));
    assert_eq!((PROBE_B.turns, PROBE_B.handoffs), (2, 1));
    assert_eq!(PROBE_A.reply_bytes, TURN_REPLY_BYTES);
    assert_eq!(PROBE_B.reply_bytes, TURN_REPLY_BYTES);
    // Exactly what each generation consumes, so back-to-back generations
    // in one home serve exactly their own entries.
    assert_eq!(PROBE_A.script_prefix().len(), 1);
    assert_eq!(PROBE_B.script_prefix().len(), 3);
    // A growth turn stays far below the handoff trigger, so no automatic
    // handoff runs while the log grows. A compile-time check pins it.
}

#[test]
fn the_attach_script_ends_with_the_tail() {
    let fixture = crate::resume::Fixture {
        metric: "terminal_attach_ms",
        turns: 3,
        reply_bytes: TURN_REPLY_BYTES,
        handoffs: 2,
    };
    let script = fixture.script_with_last(ATTACH_TAIL);
    assert_eq!(script.len(), fixture.turns + fixture.handoffs + 1);
    let last = script.last().unwrap();
    assert!(
        String::from_utf8_lossy(&last.body).contains(ATTACH_TAIL),
        "{last:?}"
    );
    for seed in 0..16 {
        assert!(!filler(4096, seed).contains(ATTACH_TAIL));
    }
}

#[test]
fn size_note_is_none_at_each_bound_and_some_just_outside() {
    for (fixture, low, high) in [
        ("1 MiB", 1_048_576, 2_097_152),
        ("10 MiB", 10_485_760, 11_534_336),
    ] {
        assert_eq!(size_note(fixture, low), None);
        assert_eq!(size_note(fixture, high - 1), None);
        assert!(size_note(fixture, low - 1).is_some());
        assert!(size_note(fixture, high).is_some());
    }
    assert!(size_note("2 MiB", 2_097_152).is_some());
}

fn stages() -> super::Samples {
    stage_rows(
        "1 MiB",
        1_050_231,
        31.0,
        20.0,
        2.0,
        5.0,
        &[("frames_1", 1, 1.5), ("frames_4096", 3, 4.0), ("frames_64", 10, 30.0)],
    )
}

#[test]
fn stage_rows_reports_every_stage_with_the_fixture_and_log_size() {
    let rows = stages();
    let names: Vec<&str> = rows
        .iter()
        .map(|(_, sample)| sample.get("stage").and_then(|stage| stage.as_str()).unwrap_or_default())
        .collect();
    assert_eq!(
        names,
        [
            "terminal",
            "hub_replay",
            "parse",
            "fold",
            "frames_1",
            "frames_4096",
            "frames_64"
        ]
    );
    for (_, sample) in &rows {
        assert_eq!(sample.get("fixture"), Some(&json!("1 MiB")));
        assert_eq!(sample.get("log_bytes"), Some(&json!(1_050_231)));
    }
    let ms: Vec<f64> = rows
        .iter()
        .map(|(_, sample)| sample.get("ms").and_then(|ms| ms.as_f64()).unwrap_or(f64::NAN))
        .collect();
    assert_eq!(ms, [31.0, 20.0, 2.0, 5.0, 1.5, 4.0, 30.0]);
    // A different hub time is a different row, so equal medians never hide
    // a swapped stage.
    assert_ne!(stages(), stage_rows(
        "1 MiB",
        1_050_231,
        31.0,
        21.0,
        2.0,
        5.0,
        &[("frames_1", 1, 1.5), ("frames_4096", 3, 4.0), ("frames_64", 10, 30.0)],
    ));
}

#[test]
fn stage_rows_counts_the_frames_only_on_the_frame_stages() {
    let rows = stages();
    for (_, sample) in rows.iter().take(4) {
        assert!(sample.get("frames").is_none(), "{sample}");
    }
    let counts: Vec<u64> = rows
        .iter()
        .skip(4)
        .map(|(_, sample)| sample.get("frames").and_then(|frames| frames.as_u64()).unwrap_or(0))
        .collect();
    assert_eq!(counts, [1, 3, 10]);
    // Zero frames still reports its count, rather than dropping the key.
    let rows = stage_rows("10 MiB", 10_492_016, 88.0, 60.0, 9.0, 20.0, &[("frames_1", 1, 6.0)]);
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[4].1.get("frames"), Some(&json!(1)));
    assert_eq!(rows[0].1.get("frames"), None);
}

#[test]
fn stage_rows_keeps_the_metric_id_the_bench_report_ignores() {
    for (metric, _) in stages() {
        assert_eq!(metric, "attach_stage_ms");
    }
}
