//! Property tests for the log's invariants (`docs/testing.md`, "Invariants").

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code may unwrap, panic and index (docs/code-quality.md, \"Lints\"), helpers outside a #[test] included"
)]

mod common;

use std::fs;

use common::*;
use contract::Envelope;
use log::{Log, read};
use proptest::prelude::*;

proptest! {
    // Few cases, since each makes a session; enough to cut each kind of line
    // at each kind of byte.
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// A session cut off at any byte, as a power cut can leave it, reopens
    /// with every complete line intact and carries on from the next `seq`.
    #[test]
    fn a_log_cut_at_any_byte_reopens_intact(
        durable in prop::collection::vec(any::<bool>(), 1..30),
        cut in any::<prop::sample::Index>(),
    ) {
        let tmp = TestDir::new("prop-cut");
        let log = Log::create(tmp.path(), id("s_1")).unwrap();
        let mut written: Vec<Envelope> = Vec::new();
        for d in &durable {
            let event = if *d { empty("step_started") } else { delta("x") };
            let line = log.append(&event, None, None).unwrap();
            if line.is_durable() {
                written.push(line);
            }
        }
        drop(log);

        let path = tmp.session(&id("s_1")).join("events.jsonl");
        let bytes = fs::read(&path).unwrap();
        let at = cut.index(bytes.len() + 1);
        fs::write(&path, &bytes[..at]).unwrap();
        let whole = bytes[..at].iter().filter(|b| **b == b'\n').count();

        let dir = tmp.session(&id("s_1"));
        prop_assert_eq!(read(&dir).unwrap(), &written[..whole]);
        let log = Log::open(tmp.path(), id("s_1")).unwrap();
        let next = log.append(&empty("step_started"), None, None).unwrap();
        prop_assert_eq!(next.seq.map(|s| s.0), Some(u64::try_from(whole).unwrap()));
        let mut expected = written[..whole].to_vec();
        expected.push(next);
        prop_assert_eq!(read(&dir).unwrap(), expected);
    }
}
