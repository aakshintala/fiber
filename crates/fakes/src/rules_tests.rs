//! Tests of [`NoRules`](super::NoRules) and
//! [`RecordingRules`](super::RecordingRules): the no-op rules read empty
//! and remember without writing, and the recording rules keep every
//! `remember` call's three fields in order.

use contract::SessionId;
use contract::rules::Rules;

use super::{NoRules, RecordingRules};

#[test]
fn no_rules_reads_empty_and_remembers_without_writing() {
    let rules = NoRules;
    let read = rules.read().unwrap();
    assert!(read.global.is_empty());
    assert!(read.project.is_empty());
    assert!(
        rules
            .remember("read", "/workspace/", &SessionId("s_test".into()))
            .is_ok()
    );
}

#[test]
fn recording_rules_records_each_remember_in_order() {
    let rules = RecordingRules::new();
    assert!(rules.calls().is_empty());
    rules
        .remember("read", "/workspace/", &SessionId("s_first".into()))
        .unwrap();
    rules
        .remember("exec", "/tmp/", &SessionId("s_second".into()))
        .unwrap();
    assert_eq!(
        rules.calls(),
        [
            (
                "read".to_string(),
                "/workspace/".to_string(),
                SessionId("s_first".into())
            ),
            (
                "exec".to_string(),
                "/tmp/".to_string(),
                SessionId("s_second".into())
            ),
        ]
    );
    let read = rules.read().unwrap();
    assert!(read.global.is_empty());
    assert!(read.project.is_empty());
}
