use serde_json::json;

use crate::busy::{BYTES_PER_TOKEN, TRIGGER_TOKENS, filler};

use super::{ATTACH_TAIL, ONE_MIB, Step, TEN_MIB, feed_step, size_note};

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
fn the_attach_fixtures_are_one_and_ten_mib_of_replies() {
    assert_eq!(ONE_MIB.turns * ONE_MIB.reply_bytes, 1_048_576);
    assert_eq!(TEN_MIB.turns * TEN_MIB.reply_bytes, 10_485_760);
    assert_eq!(TEN_MIB.handoffs, 9);
    for fixture in [&ONE_MIB, &TEN_MIB] {
        assert!(
            fixture.reply_bytes / BYTES_PER_TOKEN < TRIGGER_TOKENS,
            "{:?}",
            fixture.reply_bytes
        );
    }
}

#[test]
fn the_attach_script_ends_with_the_tail() {
    for fixture in [&ONE_MIB, &TEN_MIB] {
        let script = fixture.script_with_last(ATTACH_TAIL);
        assert_eq!(script.len(), fixture.turns + fixture.handoffs + 1);
        let last = script.last().unwrap();
        assert!(
            String::from_utf8_lossy(&last.body).contains(ATTACH_TAIL),
            "{last:?}"
        );
    }
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
