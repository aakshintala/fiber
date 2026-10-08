//! `delegated::bounded` cuts a delegate's final message once, at the write.

#![allow(clippy::unwrap_used, reason = "test code")]

use std::collections::BTreeMap;
use std::sync::Arc;

use contract::events::DelegateFinished;
use contract::shapes::{Tokens, Usage};
use contract::{JobId, SessionId};
use log::Log;

fn usage() -> Usage {
    Usage {
        tokens: Tokens {
            input: 0,
            cache_read: 0,
            cache_write: BTreeMap::new(),
            output: 0,
        },
        cost: Some(0.0),
        subscription_cost: 0.0,
    }
}

fn finished(text: &str) -> DelegateFinished {
    DelegateFinished {
        job_id: JobId("j_5e10c0ffee123456".into()),
        text: text.into(),
        artifact: None,
        questions: None,
        usage: usage(),
        worktree: None,
    }
}

fn world() -> (fakes::TempDir, Log) {
    let home = fakes::TempDir::new("fiber-delegated");
    let clock = fakes::clock::FakeClock::new();
    let log = Log::create(
        home.path(),
        SessionId("s_test".into()),
        Arc::clone(&clock) as _,
    )
    .unwrap();
    (home, log)
}

#[test]
fn a_short_text_is_kept_whole_with_no_artifact() {
    let (_home, log) = world();
    let cut = super::bounded(&log, finished("Done."));
    assert_eq!(cut.text, "Done.");
    assert_eq!(cut.artifact, None);
}

#[test]
fn exactly_16_kib_is_not_cut() {
    let (_home, log) = world();
    let cut = super::bounded(&log, finished(&"x".repeat(16_384)));
    assert_eq!(cut.text.len(), 16_384);
    assert_eq!(cut.artifact, None);
}

#[test]
fn one_byte_more_is_cut_with_the_full_text_in_the_job_artifact() {
    let (_home, log) = world();
    let full = "y".repeat(16_385);
    let cut = super::bounded(&log, finished(&full));
    assert!(cut.text.starts_with(&full[..16_384]));
    assert!(cut.text.contains("[1 bytes cut."));
    assert!(cut.text.contains("artifacts/j_5e10c0ffee123456.txt"));
    assert_eq!(
        cut.artifact.as_deref(),
        Some("artifacts/j_5e10c0ffee123456.txt")
    );
    let kept = std::fs::read_to_string(log.dir().join("artifacts/j_5e10c0ffee123456.txt")).unwrap();
    assert_eq!(kept, full);
}
