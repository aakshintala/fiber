//! The fsync cost per durable line (`docs/events.md`, "Testing";
//! `docs/performance.md`). Not a test: it asserts no timing, and it is
//! ignored so it runs only when asked for:
//!
//! `cargo nextest run -p log --release --run-ignored only --no-capture fsync_cost`

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code may unwrap, panic and index (docs/code-quality.md, \"Lints\"), helpers outside a #[test] included"
)]

mod common;

use std::time::Instant;

use common::*;
use log::Log;

/// The median time, in microseconds, to append one line of `event` over
/// `runs` runs of `lines` lines each.
fn per_line_us(event: &contract::events::Event, runs: usize, lines: u32) -> f64 {
    let mut samples: Vec<f64> = (0..runs)
        .map(|_| {
            let tmp = TestDir::new("fsync-cost");
            let log = Log::create(tmp.path(), id("s_1")).unwrap();
            let start = Instant::now();
            for _ in 0..lines {
                log.append(event, None, None).unwrap();
            }
            start.elapsed().as_secs_f64() * 1e6 / f64::from(lines)
        })
        .collect();
    samples.sort_by(f64::total_cmp);
    samples[runs / 2]
}

#[test]
#[ignore = "a measurement, run on demand"]
#[allow(clippy::print_stdout, reason = "the measurement is the output")]
fn fsync_cost() {
    let synced = per_line_us(&tool_call_completed(), 5, 1000);
    let unsynced = per_line_us(&empty("step_started"), 5, 1000);
    println!(
        "{} {}: fsynced durable line {synced:.1} us, unsynced durable line {unsynced:.1} us, \
         fsync alone {:.1} us (median of 5 runs of 1000 lines)",
        std::env::consts::OS,
        std::env::consts::ARCH,
        synced - unsynced,
    );
}
