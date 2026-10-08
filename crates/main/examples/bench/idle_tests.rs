use std::cell::Cell;
use std::collections::BTreeMap;
use std::time::Duration;

use contract::clock::Clock;
use fakes::clock::FakeClock;

use super::{row_needle, settle};
use crate::linux::Counts;
use crate::run::PROBE;

/// Past this many reads a reader errs, so a loop that never ends fails
/// fast.
const GUARD: usize = 10_000;

fn reading(state: char, voluntary: u64) -> Result<BTreeMap<u32, Counts>, String> {
    Ok(BTreeMap::from([(
        4121,
        Counts {
            voluntary,
            involuntary: 0,
            name: Some("fiber".to_owned()),
            state,
        },
    )]))
}

/// Runs `settle` over `within` with a reader that returns `script(n)` on
/// its `n`th call (from 0). Returns the result, the reads made and how far
/// the clock moved.
fn run(
    within: Duration,
    script: impl Fn(usize) -> Result<BTreeMap<u32, Counts>, String>,
) -> (Result<BTreeMap<u32, Counts>, String>, usize, Duration) {
    let clock = FakeClock::new();
    let start = clock.now();
    let calls = Cell::new(0);
    let got = settle(&*clock, within, || {
        let n = calls.get();
        calls.set(n + 1);
        if n >= GUARD {
            return Err("progress guard".to_owned());
        }
        script(n)
    });
    if let Err(err) = &got {
        assert_ne!(err, "progress guard");
    }
    (got, calls.get(), clock.now() - start)
}

fn from(
    script: Vec<Result<BTreeMap<u32, Counts>, String>>,
) -> impl Fn(usize) -> Result<BTreeMap<u32, Counts>, String> {
    move |n| {
        script
            .get(n)
            .cloned()
            .unwrap_or(Err("past the script".to_owned()))
    }
}

#[test]
fn two_equal_sleeping_readings_settle_on_the_second() {
    let (got, reads, moved) = run(3 * PROBE, from(vec![reading('S', 1), reading('S', 1)]));
    assert_eq!(got, reading('S', 1));
    assert_eq!(reads, 2);
    assert_eq!(moved, PROBE);
}

#[test]
fn a_count_that_moves_waits_for_the_next_reading() {
    let script = vec![reading('S', 1), reading('S', 2), reading('S', 2)];
    let (got, reads, moved) = run(3 * PROBE, from(script));
    assert_eq!(got, reading('S', 2));
    assert_eq!(reads, 3);
    assert_eq!(moved, 2 * PROBE);
}

#[test]
fn an_awake_thread_waits_until_it_sleeps() {
    let script = vec![reading('R', 1), reading('S', 1), reading('S', 1)];
    let (got, reads, _) = run(3 * PROBE, from(script));
    assert_eq!(got, reading('S', 1));
    assert_eq!(reads, 3);
}

#[test]
fn a_reading_that_settles_at_the_deadline_is_taken() {
    let script = vec![
        reading('S', 1),
        reading('S', 2),
        reading('S', 3),
        reading('S', 3),
    ];
    let (got, reads, moved) = run(3 * PROBE, from(script));
    assert_eq!(got, reading('S', 3));
    assert_eq!(reads, 4);
    assert_eq!(moved, 3 * PROBE);
}

#[test]
fn threads_that_never_settle_err_naming_them() {
    let (got, reads, moved) = run(3 * PROBE, |_| reading('R', 1));
    assert_eq!(
        got,
        Err("the threads did not settle within 15 ms: 4121 (fiber) is R".to_owned())
    );
    assert_eq!(reads, 4);
    assert_eq!(moved, 3 * PROBE);
}

#[test]
fn within_zero_errs_after_one_reading() {
    let (got, reads, _) = run(Duration::ZERO, from(vec![reading('S', 1), reading('S', 1)]));
    assert!(got.is_err());
    assert_eq!(reads, 1);
}

#[test]
fn a_failed_read_ends_the_wait_with_its_error() {
    let script = vec![
        reading('S', 1),
        Err("reading /proc/1/task: gone".to_owned()),
    ];
    let (got, reads, _) = run(3 * PROBE, from(script));
    assert_eq!(got, Err("reading /proc/1/task: gone".to_owned()));
    assert_eq!(reads, 2);
}

const ID: &str = "s_00112233445566aa";

fn row(id: &str, name: &str) -> String {
    format!(
        "{}\n",
        serde_json::json!({"session_id": id, "name": name, "how": "exited"})
    )
}

fn hub_only() -> Vec<String> {
    vec!["hub".to_owned()]
}

#[test]
fn the_needle_is_the_rows_name() {
    assert_eq!(
        row_needle(&row(ID, "turn 0"), &hub_only(), ID),
        Ok("turn 0".to_owned())
    );
}

#[test]
fn a_row_with_no_name_shows_its_id() {
    assert_eq!(row_needle(&row(ID, ""), &[], ID), Ok(ID.to_owned()));
}

#[test]
fn control_characters_in_the_name_become_spaces() {
    assert_eq!(
        row_needle(&row(ID, "a\tb\nc"), &[], ID),
        Ok("a b c".to_owned())
    );
}

#[test]
fn the_last_row_for_the_id_wins() {
    let recent = format!(
        "{}{}{}",
        row(ID, "first"),
        row("s_ffeeddccbbaa9988", "other"),
        row(ID, "last")
    );
    assert_eq!(row_needle(&recent, &[], ID), Ok("last".to_owned()));
}

#[test]
fn no_row_for_the_id_errs_naming_it() {
    let want = Err(format!("the seeded session {ID} left no recent.jsonl row"));
    assert_eq!(
        row_needle(&row("s_ffeeddccbbaa9988", "other"), &[], ID),
        want
    );
    assert_eq!(row_needle("", &[], ID), want);
}

#[test]
fn a_session_socket_in_run_errs_naming_it() {
    let entries = vec!["hub".to_owned(), "s_0123456789abcdef".to_owned()];
    assert_eq!(
        row_needle(&row(ID, "turn 0"), &entries, ID),
        Err("run/ still holds session s_0123456789abcdef".to_owned())
    );
}
