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
        artifact_absolute: None,
        questions: None,
        usage: usage(),
        worktree: None,

        changed_by: None,
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

/// `text` as a finish that names its final-message file.
fn finished_in(text: &str) -> DelegateFinished {
    DelegateFinished {
        artifact: Some("artifacts/final.md".into()),
        artifact_absolute: Some("/delegate/s_d/artifacts/final.md".into()),
        ..finished(text)
    }
}

#[test]
fn a_short_text_is_kept_whole_with_its_fields() {
    let (_home, log) = world();
    let cut = super::bounded(&log, finished_in("Done."));
    assert_eq!(cut, finished_in("Done."));
}

#[test]
fn exactly_16_kib_is_not_cut() {
    let (_home, log) = world();
    let cut = super::bounded(&log, finished_in(&"x".repeat(16_384)));
    assert_eq!(cut.text.len(), 16_384);
}

#[test]
fn one_byte_more_is_cut_and_the_notice_names_the_absolute_file() {
    let (_home, log) = world();
    let full = "y".repeat(16_385);
    let cut = super::bounded(&log, finished_in(&full));
    assert!(cut.text.starts_with(&full[..16_384]));
    assert!(cut.text.contains("[1 bytes cut."));
    assert!(cut.text.contains("/delegate/s_d/artifacts/final.md"));
    // The fields pass through, and the cut writes no file.
    assert_eq!(cut.artifact.as_deref(), Some("artifacts/final.md"));
    assert_eq!(
        cut.artifact_absolute.as_deref(),
        Some("/delegate/s_d/artifacts/final.md")
    );
    assert!(!log.dir().join("artifacts/j_5e10c0ffee123456.txt").exists());
    assert!(!log.dir().join("artifacts/final.md").exists());
}

#[test]
fn a_cut_text_with_no_file_names_no_path() {
    let (_home, log) = world();
    let cut = super::bounded(&log, finished(&"z".repeat(16_385)));
    assert!(
        cut.text
            .contains("[1 bytes cut. The full output could not be saved:")
    );
    assert!(!cut.text.contains("artifacts/"));
    assert_eq!(cut.artifact, None);
    assert_eq!(cut.artifact_absolute, None);
}
