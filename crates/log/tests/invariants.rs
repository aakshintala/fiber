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

/// A generated event: durable without an fsync, ephemeral, or syncing
/// (`docs/events.md`, "Writing").
#[derive(Clone, Copy, Debug)]
enum Kind {
    StepStarted,
    Delta,
    ToolCallStarted,
    ToolCallCompleted,
    MessageCompleted,
}

fn kind() -> impl Strategy<Value = Kind> {
    use Kind::*;
    prop_oneof![
        Just(StepStarted),
        Just(Delta),
        Just(ToolCallStarted),
        Just(ToolCallCompleted),
        Just(MessageCompleted),
    ]
}

/// Whether appending the kind ends with an fsync: the test's own table
/// from `docs/events.md`, "Writing".
fn syncs(kind: &Kind) -> bool {
    use Kind::*;
    match kind {
        ToolCallStarted | ToolCallCompleted | MessageCompleted => true,
        StepStarted | Delta => false,
    }
}

fn event_of(kind: &Kind) -> contract::events::Event {
    use Kind::*;
    match kind {
        StepStarted => empty("step_started"),
        Delta => delta("x"),
        ToolCallStarted => tool_call_started(),
        ToolCallCompleted => tool_call_completed(),
        MessageCompleted => message_completed(),
    }
}

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
        let log = Log::create(tmp.path(), id("s_1"), fakes::clock::FakeClock::new()).unwrap();
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
        let log = Log::open(tmp.path(), id("s_1"), fakes::clock::FakeClock::new()).unwrap();
        let next = log.append(&empty("step_started"), None, None).unwrap();
        prop_assert_eq!(next.seq.map(|s| s.0), Some(u64::try_from(whole).unwrap()));
        let mut expected = written[..whole].to_vec();
        expected.push(next);
        prop_assert_eq!(read(&dir).unwrap(), expected);
    }

    /// A session killed right after any event fsync reopens with every
    /// durable line written so far, and `seq` carries on from there.
    #[test]
    fn a_session_killed_at_each_fsync_boundary_reopens_intact(
        kinds in prop::collection::vec(kind(), 1..30)
            .prop_filter("a syncing kind", |kinds| {
                kinds.iter().any(syncs)
            }),
    ) {
        let tmp = TestDir::new("prop-fsync-cut");
        let log =
            Log::create(tmp.path(), id("s_1"), fakes::clock::FakeClock::new()).unwrap();
        let mut written: Vec<Envelope> = Vec::new();
        written.push(log.append(&session_started(), None, None).unwrap());
        // Each snapshot: the bytes on disk right after an event fsync,
        // and how many durable lines they hold.
        let mut cuts: Vec<(Vec<u8>, usize)> = Vec::new();
        for kind in &kinds {
            let before = log.fsyncs();
            let line = log.append(&event_of(kind), None, None).unwrap();
            if line.is_durable() {
                written.push(line);
            }
            let after = log.fsyncs();
            if after > before {
                let bytes =
                    fs::read(log.dir().join("events.jsonl")).unwrap();
                cuts.push((bytes, written.len()));
            }
        }
        let syncing = kinds.iter().filter(|k| syncs(k)).count();
        prop_assert_eq!(cuts.len(), syncing);

        let scratch = TestDir::new("prop-fsync-cut");
        let dir = scratch.session(&id("s_1"));
        fs::create_dir_all(dir.join("artifacts")).unwrap();
        for (bytes, n) in &cuts {
            // What a kill leaves behind: the bytes on disk, and a
            // `session.lock` still holding the dead holder's pid.
            fs::write(dir.join("events.jsonl"), bytes).unwrap();
            fs::write(dir.join("session.lock"), "4194303\n").unwrap();
            prop_assert_eq!(read(&dir).unwrap(), &written[..*n]);
            let reopened = Log::open(
                scratch.path(),
                id("s_1"),
                fakes::clock::FakeClock::new(),
            )
            .unwrap();
            let next =
                reopened.append(&empty("step_started"), None, None).unwrap();
            prop_assert_eq!(
                next.seq.map(|s| s.0),
                Some(u64::try_from(*n).unwrap())
            );
            let mut expected = written[..*n].to_vec();
            expected.push(next);
            prop_assert_eq!(read(&dir).unwrap(), expected);
            drop(reopened);
        }
    }
}
