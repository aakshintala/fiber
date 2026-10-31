use std::time::Duration;

use contract::clock::Clock;
use fakes::clock::FakeClock;
use serde_json::json;

use crate::busy::filler;
use crate::test_support::loopback;

use super::{
    ATTACH_PASS, ATTACH_TAIL, FIXTURE_COUNT, MEMORY_FIXTURE, OPEN_HEIGHT, OPEN_WIDTH, OpenFigures,
    PLANS, PROBE_A, PROBE_B, SPAN_METRIC, Step, TURN_REPLY_BYTES, attach_stage_rows,
    await_pass_end, feed_step, memory_rows, open_command, open_error_samples, parse_open_line,
    pass_entry, plan_turns, replay_finished, size_note, span_samples, stage_rows,
};

#[test]
fn the_fixture_count_is_the_plan_length() {
    // The scheduler alternates the attach row by sample
    // (`docs/performance.md`, "Measuring") over this many fixtures: a
    // plan that gains or loses one fails here.
    assert_eq!(usize::try_from(FIXTURE_COUNT), Ok(PLANS.len()));
}

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
fn only_the_10_mib_fixture_reports_terminal_memory() {
    // docs/performance.md:74,77,227-229: only the 10 MiB fixture reports
    // the two memory readings, terminal peak RSS and retained RssAnon.
    assert_eq!(MEMORY_FIXTURE, "10 MiB");
    for (label, _) in PLANS {
        let rows = memory_rows(label, 51200, 48000);
        if label == "10 MiB" {
            assert_eq!(rows.len(), 2);
        } else {
            assert!(rows.is_empty(), "{label}: {rows:?}");
        }
    }
    // An unknown fixture reports nothing, never a zero.
    assert!(memory_rows("2 MiB", 51200, 48000).is_empty());
}

#[test]
fn the_attach_memory_rows_carry_the_terminals_peak_and_retained() {
    let rows = memory_rows("10 MiB", 51200, 48000);
    let value = |id: &str| {
        rows.iter()
            .find(|(metric, _)| *metric == id)
            .unwrap_or_else(|| panic!("{id} is not sampled"))
            .1
            .clone()
    };
    assert_eq!(value("terminal_attach_rss_kib"), json!(51200));
    assert_eq!(value("terminal_attach_retained_kib"), json!(48000));
    // A different retained reading is a different sample.
    assert_ne!(rows, memory_rows("10 MiB", 51200, 48001));
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

#[test]
fn replay_finished_needs_the_ack_then_the_tail() {
    let ack = json!({"kind": "command_accepted", "payload": {"command_id": "c_1"}});
    // The acknowledgement arrives first, but alone it never finishes.
    assert!(!replay_finished(false, &ack));
    assert!(!replay_finished(true, &ack));
    let tail = json!({"kind": "session_event", "payload": {"text": "quokkas"}});
    assert!(!replay_finished(false, &tail));
    assert!(replay_finished(true, &tail));
}

#[test]
fn replay_finished_ignores_a_line_without_the_tail() {
    let line = json!({"kind": "session_event", "payload": {"text": "wombats"}});
    assert!(!replay_finished(true, &line));
    // The tail nested anywhere in the line still finishes.
    let nested = json!({"kind": "session_event", "payload": {"lines": ["quokkas"]}});
    assert!(replay_finished(true, &nested));
}

fn stages() -> super::Samples {
    stage_rows(
        "1 MiB",
        1_050_231,
        31.0,
        20.0,
        2.0,
        5.0,
        &[
            ("frames_1", 1, 1.5),
            ("frames_4096", 3, 4.0),
            ("frames_64", 10, 30.0),
        ],
    )
}

#[test]
fn stage_rows_reports_every_stage_with_the_fixture_and_log_size() {
    let rows = stages();
    let names: Vec<&str> = rows
        .iter()
        .map(|(_, sample)| {
            sample
                .get("stage")
                .and_then(|stage| stage.as_str())
                .unwrap_or_default()
        })
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
        .map(|(_, sample)| {
            sample
                .get("ms")
                .and_then(|ms| ms.as_f64())
                .unwrap_or(f64::NAN)
        })
        .collect();
    assert_eq!(ms, [31.0, 20.0, 2.0, 5.0, 1.5, 4.0, 30.0]);
    // A different hub time is a different row, so equal medians never hide
    // a swapped stage.
    assert_ne!(
        stages(),
        stage_rows(
            "1 MiB",
            1_050_231,
            31.0,
            21.0,
            2.0,
            5.0,
            &[
                ("frames_1", 1, 1.5),
                ("frames_4096", 3, 4.0),
                ("frames_64", 10, 30.0)
            ],
        )
    );
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
        .map(|(_, sample)| {
            sample
                .get("frames")
                .and_then(|frames| frames.as_u64())
                .unwrap_or(0)
        })
        .collect();
    assert_eq!(counts, [1, 3, 10]);
    // Zero frames still reports its count, rather than dropping the key.
    let rows = stage_rows(
        "10 MiB",
        10_492_016,
        88.0,
        60.0,
        9.0,
        20.0,
        &[("frames_1", 1, 6.0)],
    );
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

fn open(figures: (f64, f64, usize, f64)) -> OpenFigures {
    OpenFigures {
        parse_ms: figures.0,
        fold_ms: figures.1,
        frames: figures.2,
        frame_ms: figures.3,
    }
}

#[test]
fn the_jigs_open_line_parses_into_what_it_measured() {
    // The jig prints one JSON line; its numbers land on the figures.
    let line = r#"{"parse_ms":2.0,"fold_ms":5.0,"frames":1,"frame_ms":1.5}"#;
    assert_eq!(parse_open_line(line), Ok(open((2.0, 5.0, 1, 1.5))));
    // A different parse time is a different measurement, so equal medians
    // never hide a swapped stage.
    assert_ne!(parse_open_line(line), Ok(open((3.0, 5.0, 1, 1.5))));
}

#[test]
fn the_jigs_open_line_without_its_keys_is_an_error_naming_them() {
    // Not JSON at all names the line, not a key.
    let err = parse_open_line("not json").unwrap_err();
    assert!(err.contains("not JSON"), "{err}");
    // Each missing key names itself, so a renamed key fails here before
    // the release job.
    for key in ["parse_ms", "fold_ms", "frames", "frame_ms"] {
        let mut parsed: serde_json::Value =
            serde_json::from_str(r#"{"parse_ms":2.0,"fold_ms":5.0,"frames":1,"frame_ms":1.5}"#)
                .unwrap_or_else(|error| panic!("{error}"));
        parsed
            .as_object_mut()
            .unwrap_or_else(|| panic!("object"))
            .remove(key);
        let err = parse_open_line(&parsed.to_string()).unwrap_err();
        assert!(err.contains(key), "{key}: {err}");
    }
    // A JSON line of the wrong shape names no time, not a report.
    let err = parse_open_line("[1, 2]").unwrap_err();
    assert!(err.contains("parse_ms"), "{err}");
}

#[test]
fn the_jigs_open_line_rejects_a_time_that_is_not_a_time() {
    // Negative, non-finite and non-numeric times are errors naming the
    // key, so a clock slip fails rather than lowering a median.
    for bad in ["-1.0", "NaN", "Infinity", "\"soon\""] {
        let line = format!(r#"{{"parse_ms":{bad},"fold_ms":5.0,"frames":1,"frame_ms":1.5}}"#);
        let err = parse_open_line(&line).unwrap_err();
        assert!(err.contains("parse_ms"), "{bad}: {err}");
    }
    for bad in ["\"one\"", "1.5", "-3"] {
        let line = format!(r#"{{"parse_ms":2.0,"fold_ms":5.0,"frames":{bad},"frame_ms":1.5}}"#);
        let err = parse_open_line(&line).unwrap_err();
        assert!(err.contains("frames"), "{bad}: {err}");
    }
    assert!(parse_open_line(r#"{"parse_ms":0.0,"fold_ms":0.0,"frames":0,"frame_ms":0.0}"#).is_ok());
}

#[test]
fn the_open_jig_runs_with_the_log_at_the_ptys_size_and_only_path_set() {
    // The jig reopens the session's own log at the pty's size, with the
    // single final frame spelled as its number.
    let command = open_command(
        std::path::Path::new("/r/paging"),
        std::path::Path::new("/h/s/events.jsonl"),
        OPEN_WIDTH,
        OPEN_HEIGHT,
        &usize::MAX.to_string(),
        Some(std::ffi::OsStr::new("/usr/bin")),
    );
    assert_eq!(command.get_program(), "/r/paging");
    let args: Vec<&std::ffi::OsStr> = command.get_args().collect();
    assert_eq!(
        args,
        [
            "open",
            "/h/s/events.jsonl",
            "60",
            "12",
            "18446744073709551615"
        ]
    );
    let set: Vec<(&std::ffi::OsStr, Option<&std::ffi::OsStr>)> = command.get_envs().collect();
    assert_eq!(
        set,
        [(
            std::ffi::OsStr::new("PATH"),
            Some(std::ffi::OsStr::new("/usr/bin"))
        )]
    );
    assert!(
        format!("{command:?}").starts_with("env -i "),
        "the environment is not cleared: {command:?}"
    );
}

#[test]
fn attach_stage_rows_maps_each_jig_run_onto_its_stage() {
    // Parse and fold come from the single-frame run at any frame count;
    // each frame count lands on its own stage with its own count.
    let one = open((2.0, 5.0, 1, 1.5));
    let batched = open((9.0, 9.0, 3, 4.0));
    let dense = open((9.0, 9.0, 10, 30.0));
    assert_eq!(
        attach_stage_rows(
            "1 MiB",
            1_050_231,
            31.0,
            20.0,
            Some((&one, &batched, &dense))
        ),
        stages()
    );
    // A different dense count is a different row, so equal medians never
    // hide a swapped frame stage.
    assert_ne!(
        attach_stage_rows(
            "1 MiB",
            1_050_231,
            31.0,
            20.0,
            Some((&one, &batched, &dense))
        ),
        attach_stage_rows(
            "1 MiB",
            1_050_231,
            31.0,
            20.0,
            Some((&one, &batched, &open((9.0, 9.0, 11, 30.0))))
        )
    );
}

#[test]
fn attach_stage_rows_without_the_jig_reports_terminal_and_hub_only() {
    // docs/performance.md:231-237: terminal and hub_replay run outside
    // the paging jig; without it, only those two stages are measured.
    let rows = attach_stage_rows("1 MiB", 1_050_231, 31.0, 20.0, None);
    let names: Vec<&str> = rows
        .iter()
        .map(|(_, sample)| {
            sample
                .get("stage")
                .and_then(|stage| stage.as_str())
                .unwrap_or_default()
        })
        .collect();
    assert_eq!(names, ["terminal", "hub_replay"]);
    assert_eq!(rows.len(), 2);
    for (_, sample) in &rows {
        assert_eq!(sample.get("fixture"), Some(&json!("1 MiB")));
        assert_eq!(sample.get("log_bytes"), Some(&json!(1_050_231)));
    }
}

#[test]
fn a_failing_open_run_keeps_the_terminal_and_hub_rows_with_a_note() {
    // docs/performance.md:231-237: terminal and hub_replay run outside
    // the paging jig. Its failure leaves those two measurements intact.
    let error = "the paging jig's open run exited 2; stderr: usage";
    let (rows, note) = open_error_samples("1 MiB", 1_050_231, 31.0, 20.0, error);
    let names: Vec<&str> = rows
        .iter()
        .map(|(_, sample)| {
            sample
                .get("stage")
                .and_then(|stage| stage.as_str())
                .unwrap_or_default()
        })
        .collect();
    assert_eq!(names, ["terminal", "hub_replay"]);
    assert_eq!(rows.len(), 2);
    // The same rows the no-jig path emits, not emptied or zeroed ones.
    assert_eq!(
        rows,
        attach_stage_rows("1 MiB", 1_050_231, 31.0, 20.0, None)
    );
    // The note names the unavailable stages and the jig's error.
    for stage in ["parse", "fold", "frames_1", "frames_4096", "frames_64"] {
        assert!(note.contains(stage), "{stage}: {note}");
    }
    assert!(note.contains(error), "{note}");
    // A different error is a different note, so equal notes never hide
    // a swallowed failure.
    assert_ne!(
        note,
        open_error_samples("1 MiB", 1_050_231, 31.0, 20.0, "other").1
    );
}

#[test]
fn pass_end_fails_when_the_thumb_never_arrives() {
    use std::io::Write;

    // The tail on screen with no thumb: the pass never ends.
    let clock = FakeClock::new();
    let (terminal, mut drawer) = loopback();
    drawer
        .write_all(ATTACH_TAIL.as_bytes())
        .expect("showing the tail");
    terminal
        .wait_for(
            &*clock,
            clock
                .now()
                .checked_add(Duration::from_secs(1))
                .expect("fake deadline is representable"),
            ATTACH_TAIL,
        )
        .expect("the tail draws");
    let started = clock.now();
    // A deadline already reached: the pass cannot end.
    let err =
        await_pass_end(&terminal, &*clock, started, started).expect_err("no thumb ever shows");
    assert!(err.contains(ATTACH_PASS), "{err}");
}

#[test]
fn pass_end_returns_once_the_thumb_shows() {
    use std::io::Write;

    let clock = FakeClock::new();
    let (terminal, mut drawer) = loopback();
    drawer
        .write_all(format!("{ATTACH_TAIL}{ATTACH_PASS}").as_bytes())
        .expect("showing the tail and the thumb");
    terminal
        .wait_for(
            &*clock,
            clock
                .now()
                .checked_add(Duration::from_secs(1))
                .expect("fake deadline is representable"),
            ATTACH_PASS,
        )
        .expect("the thumb draws");
    let started = clock.now();
    let until = started
        .checked_add(Duration::from_secs(1))
        .expect("fake deadline is representable");
    let ms = await_pass_end(&terminal, &*clock, started, until).expect("the thumb shows");
    assert!(ms.is_finite() && ms >= 0.0, "{ms}");
}

#[test]
fn the_pass_end_entries_carry_the_fixture_and_log_size() {
    // docs/performance.md:184-185,223-227: both attach observations use
    // the same fixture and run; each entry names that fixture and log size.
    for label in ["1 MiB", "10 MiB"] {
        let (metric, entry) = pass_entry(label, 1_050_231, 41.0);
        assert_eq!(metric, "terminal_attach_pass_ms");
        assert_eq!(entry.get("fixture"), Some(&json!(label)));
        assert_eq!(entry.get("log_bytes"), Some(&json!(1_050_231)));
        assert_eq!(entry.get("ms"), Some(&json!(41.0)));
    }
}

/// A home directory holding `logs/tui-<pid>.log` with `text`, as the TUI
/// under test leaves it.
fn span_home(pid: u32, text: &str) -> fakes::TempDir {
    let dir = fakes::TempDir::new("attach-spans");
    let logs = dir.path().join("logs");
    std::fs::create_dir_all(&logs).expect("creating logs");
    std::fs::write(logs.join(format!("tui-{pid}.log")), text).expect("writing the tui log");
    dir
}

/// One diagnostic line with `code` and `data`.
fn span_line(code: &str, data: serde_json::Value) -> String {
    json!({
        "ts": 1_700_000_000_000_u64,
        "level": "debug",
        "process": "tui",
        "code": code,
        "message": "An operation ended.",
        "data": data,
    })
    .to_string()
}

fn span_op(op: &str, ms: u64) -> String {
    span_line("op_timing", json!({"op": op, "ms": ms}))
}

/// Each `op_timing` line in the sample's TUI log becomes one
/// `attach_span_ms` row carrying the fixture, the sample, the operation
/// and its milliseconds; other codes and blank lines are not spans. The
/// terminal operation inventory and its per-process limits are in
/// `docs/state.md:423-453`.
#[test]
fn span_samples_emit_one_row_per_op_timing_line() {
    let text = [
        span_op("tui.extensions", 3),
        String::new(),
        span_line("info", json!({"note": "ready"})),
        span_op("tui.attached_frame", 7),
        span_op("tui.tail_wait", 19),
    ]
    .join("\n");
    let dir = span_home(4242, &text);
    let mut rows = span_samples(dir.path(), 4242, "10 MiB", 10_485_760, 2).expect("spans read");
    let mut want = vec![
        (
            SPAN_METRIC,
            json!({"fixture": "10 MiB", "log_bytes": 10_485_760, "sample": 2, "op": "tui.extensions", "ms": 3}),
        ),
        (
            SPAN_METRIC,
            json!({"fixture": "10 MiB", "log_bytes": 10_485_760, "sample": 2, "op": "tui.attached_frame", "ms": 7}),
        ),
        (
            SPAN_METRIC,
            json!({"fixture": "10 MiB", "log_bytes": 10_485_760, "sample": 2, "op": "tui.tail_wait", "ms": 19}),
        ),
    ];
    // `docs/state.md:423-453` promises counts and scopes, not log order,
    // so the comparison sorts both sides: the exact rows and their count
    // still match, in any order.
    let key =
        |(_, sample): &(&str, serde_json::Value)| serde_json::to_string(sample).unwrap_or_default();
    rows.sort_by_key(|row| key(row));
    want.sort_by_key(|row| key(row));
    assert_eq!(rows, want);
    assert_eq!(SPAN_METRIC, "attach_span_ms");
}

/// Each sample reads only its own TUI process's log: two samples in one
/// home attribute their spans to their own sample number.
#[test]
fn span_samples_attribute_each_pid_log_to_its_own_sample() {
    let dir = fakes::TempDir::new("attach-spans-two");
    let logs = dir.path().join("logs");
    std::fs::create_dir_all(&logs).expect("creating logs");
    std::fs::write(logs.join("tui-100.log"), span_op("tui.tail_wait", 11)).expect("first log");
    std::fs::write(logs.join("tui-200.log"), span_op("tui.tail_wait", 13)).expect("second log");
    let first = span_samples(dir.path(), 100, "1 MiB", 1_048_576, 0).expect("first spans");
    let second = span_samples(dir.path(), 200, "1 MiB", 1_048_576, 1).expect("second spans");
    // docs/state.md:446-448,453 promises one tui.tail_wait line per process.
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].1["sample"], json!(0));
    assert_eq!(first[0].1["ms"], json!(11));
    // The same docs/state.md:446-448,453 promise applies to this process.
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].1["sample"], json!(1));
    assert_eq!(second[0].1["ms"], json!(13));
}

/// A line that is not JSON fails the span result instead of disappearing
/// into it.
#[test]
fn span_samples_fail_on_a_non_json_line() {
    let dir = span_home(4242, "not json\n");
    let err =
        span_samples(dir.path(), 4242, "1 MiB", 1_048_576, 0).expect_err("malformed line fails");
    assert!(err.contains("is not JSON"), "{err}");
}

/// An `op_timing` line with no string `data.op` or no whole `data.ms`
/// fails the span result instead of counting as zero.
#[test]
fn span_samples_fail_on_an_op_timing_line_without_op_or_ms() {
    for data in [
        json!({"ms": 3}),
        json!({"op": "tui.tail_wait"}),
        json!({"op": "tui.tail_wait", "ms": "3"}),
        json!({"op": 7, "ms": 3}),
    ] {
        let dir = span_home(4242, &span_line("op_timing", data));
        let err = span_samples(dir.path(), 4242, "1 MiB", 1_048_576, 0)
            .expect_err("malformed span fails");
        assert!(err.contains("no string data.op or whole data.ms"), "{err}");
    }
}

/// A log that cannot be read fails the span result: a directory at the
/// log's path is not a missing file.
#[test]
fn span_samples_fail_when_the_log_cannot_be_read() {
    let dir = fakes::TempDir::new("attach-spans-unreadable");
    let logs = dir.path().join("logs");
    std::fs::create_dir_all(logs.join("tui-4242.log")).expect("a directory at the log path");
    let err = span_samples(dir.path(), 4242, "1 MiB", 1_048_576, 0).expect_err("unreadable fails");
    assert!(err.contains("tui-4242.log"), "{err}");
}
