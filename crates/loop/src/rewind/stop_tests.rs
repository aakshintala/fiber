//! Tests for the rewind note's stopped-jobs section: which `job_completed`
//! lines of the old log the note lists, read from the log only.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::path::PathBuf;
use std::sync::Arc;

use contract::events::{
    Event, ExtensionStateSet, JobCompleted, JobStarted, OnFork, Outcome, SessionStarted,
    TurnCompleted, Variables, VariablesSource,
};
use contract::shapes::Failure;
use contract::{ErrorCode, JobId, Seq, SessionId};
use log::Log;

use super::{StoppedJob, stopped_jobs};
use crate::rewind::{rewind_note, rewind_note_with_jobs};

fn session_start() -> Event {
    Event::SessionStarted(SessionStarted {
        workspace: "/w".to_owned(),
        variables: Variables {
            path: "/usr/bin".to_owned(),
            names: Vec::new(),
            source: VariablesSource::Inherited,
        },
        parent: None,
        forked_from: None,
        rewind: None,
        worktree: None,
        supervised: false,
    })
}

fn started(id: &str, description: &str) -> Event {
    Event::JobStarted(JobStarted {
        job_id: JobId(id.into()),
        tool: None,
        extension: None,
        description: description.into(),
        output_path: "/tmp/out".into(),
    })
}

fn completed(id: &str, status: Outcome, error: Option<Failure>) -> Event {
    Event::JobCompleted(JobCompleted {
        job_id: JobId(id.into()),
        status,
        error,
        process: None,
        output_tail: None,

        notice: None,
        changed_by: None,
        protected_paths: None,
    })
}

fn cancelled(id: &str) -> Event {
    completed(id, Outcome::Cancelled, None)
}

fn finished(id: &str) -> Event {
    completed(id, Outcome::Completed, None)
}

fn indeterminate(id: &str) -> Event {
    completed(
        id,
        Outcome::Failed,
        Some(Failure {
            code: ErrorCode::Indeterminate,
            message: "the job did not stop in time".into(),
            retry_after_ms: None,
            provider: None,
        }),
    )
}

fn orphaned(id: &str) -> Event {
    completed(
        id,
        Outcome::Failed,
        Some(Failure {
            code: ErrorCode::Orphaned,
            message: "the job was left running".into(),
            retry_after_ms: None,
            provider: None,
        }),
    )
}

fn delegate_done(id: &str) -> Event {
    Event::DelegateFinished(contract::events::DelegateFinished {
        job_id: JobId(id.into()),
        text: "done".into(),
        artifact: None,
        questions: None,
        usage: contract::shapes::Usage {
            tokens: contract::shapes::Tokens {
                input: 0,
                cache_read: 0,
                cache_write: std::collections::BTreeMap::new(),
                output: 0,
            },
            cost: Some(0.0),
            subscription_cost: 0.0,
        },
        worktree: None,

        changed_by: None,
    })
}

fn turn_done() -> Event {
    Event::TurnCompleted(TurnCompleted {
        outcome: contract::events::TurnOutcome::Completed,
        error: None,
        questions: None,
    })
}

fn rewound_line() -> Event {
    Event::Rewound(contract::events::Rewound {
        new_session_id: SessionId("s_new000000000001".into()),
        seq: Seq(12),
        from_session_id: None,
        summary: None,
    })
}

/// The session log holding `events`, written through the log: the home is
/// returned to keep it alive, the directory to read it back.
fn session_log(events: &[Event]) -> (fakes::TempDir, PathBuf) {
    let home = fakes::TempDir::new("loop-stop-note");
    let clock = fakes::clock::FakeClock::new();
    let log = Log::create(
        home.path(),
        SessionId("s_old000000000001".into()),
        Arc::clone(&clock) as _,
    )
    .unwrap();
    for event in events {
        log.append(event, None, None).unwrap();
    }
    (home, log.dir().to_path_buf())
}

fn stopped(ids: &[(&str, Option<&str>)]) -> Vec<StoppedJob> {
    ids.iter()
        .map(|(id, description)| StoppedJob {
            job_id: JobId((*id).into()),
            description: description.map(str::to_owned),
        })
        .collect()
}

/// A hook's carried state beside a stop: a durable line the stop run
/// must read past.
fn state_set() -> Event {
    Event::ExtensionStateSet(ExtensionStateSet {
        extension: "redactor".into(),
        key: "seen".into(),
        value: serde_json::json!(true),
        on_fork: OnFork::Fresh,
    })
}

#[test]
fn hook_state_between_two_stops_keeps_the_earlier_job_in_the_note() {
    let (_home, dir) = session_log(&[
        session_start(),
        started("j_1", "npm run dev"),
        started("j_2", "lint"),
        cancelled("j_1"),
        state_set(),
        cancelled("j_2"),
        rewound_line(),
    ]);
    assert_eq!(
        stopped_jobs(&dir).unwrap(),
        stopped(&[("j_1", Some("npm run dev")), ("j_2", Some("lint"))])
    );
}

#[test]
fn the_scan_stops_at_the_settled_turn_before_the_run() {
    let (_home, dir) = session_log(&[
        session_start(),
        started("j_0", "stale"),
        cancelled("j_0"),
        // The settled turn before the stop wait: the boundary the scan
        // must not cross, even with hook state past it.
        turn_done(),
        state_set(),
        started("j_1", "npm run dev"),
        cancelled("j_1"),
        rewound_line(),
    ]);
    assert_eq!(
        stopped_jobs(&dir).unwrap(),
        stopped(&[("j_1", Some("npm run dev"))])
    );
}

#[test]
fn the_trailing_run_before_rewound_lists_the_stops_in_log_order() {
    let (_home, dir) = session_log(&[
        session_start(),
        started("j_1", "npm run dev"),
        started("j_2", "lint"),
        cancelled("j_1"),
        cancelled("j_2"),
        rewound_line(),
    ]);
    assert_eq!(
        stopped_jobs(&dir).unwrap(),
        stopped(&[("j_1", Some("npm run dev")), ("j_2", Some("lint"))])
    );
}

#[test]
fn a_completion_before_a_turn_completed_is_not_listed() {
    let (_home, dir) = session_log(&[
        session_start(),
        cancelled("j_0"),
        turn_done(),
        started("j_1", "npm run dev"),
        cancelled("j_1"),
        rewound_line(),
    ]);
    assert_eq!(
        stopped_jobs(&dir).unwrap(),
        stopped(&[("j_1", Some("npm run dev"))])
    );
}

#[test]
fn indeterminate_is_listed_a_natural_end_is_not() {
    let (_home, dir) = session_log(&[
        session_start(),
        started("j_1", "one"),
        started("j_2", "two"),
        started("j_3", "three"),
        started("j_4", "four"),
        cancelled("j_1"),
        finished("j_2"),
        indeterminate("j_3"),
        // Failed with no error: not a stop the rewind made.
        completed("j_4", Outcome::Failed, None),
        rewound_line(),
    ]);
    assert_eq!(
        stopped_jobs(&dir).unwrap(),
        stopped(&[("j_1", Some("one")), ("j_3", Some("three"))])
    );
}

#[test]
fn an_orphaned_completion_is_skipped() {
    let (_home, dir) = session_log(&[
        session_start(),
        started("j_1", "one"),
        orphaned("j_1"),
        rewound_line(),
    ]);
    assert_eq!(stopped_jobs(&dir).unwrap(), Vec::new());
}

#[test]
fn a_job_with_no_started_is_listed_by_id() {
    let (_home, dir) = session_log(&[session_start(), cancelled("j_9"), rewound_line()]);
    assert_eq!(stopped_jobs(&dir).unwrap(), stopped(&[("j_9", None)]));
}

#[test]
fn a_log_not_ending_in_rewound_yields_nothing() {
    let (_home, dir) = session_log(&[session_start(), started("j_1", "one"), cancelled("j_1")]);
    assert_eq!(stopped_jobs(&dir).unwrap(), Vec::new());
}

#[test]
fn a_delegate_finish_between_the_stops_does_not_break_the_run() {
    let (_home, dir) = session_log(&[
        session_start(),
        started("j_1", "one"),
        started("j_2", "two"),
        cancelled("j_1"),
        delegate_done("j_2"),
        cancelled("j_2"),
        rewound_line(),
    ]);
    assert_eq!(
        stopped_jobs(&dir).unwrap(),
        stopped(&[("j_1", Some("one")), ("j_2", Some("two"))])
    );
}

#[test]
fn a_completion_after_a_non_job_line_does_not_join_the_run() {
    let (_home, dir) = session_log(&[
        session_start(),
        cancelled("j_1"),
        turn_done(),
        cancelled("j_2"),
        rewound_line(),
    ]);
    assert_eq!(stopped_jobs(&dir).unwrap(), stopped(&[("j_2", None)]));
}

#[test]
fn with_no_stops_the_note_equals_rewind_note_byte_for_byte() {
    let (_home, old) = session_log(&[session_start(), turn_done()]);
    let (point_home, dir) = session_log(&[session_start()]);
    let _keep = point_home;
    assert_eq!(
        rewind_note_with_jobs(&old, &dir, Seq(0)).unwrap(),
        rewind_note(&dir, Seq(0)).unwrap()
    );
}

/// The jobs section of `note`: everything from its opening line on.
fn jobs_section(note: &str) -> &str {
    let at = note
        .find("Jobs that were running")
        .expect("the note has a jobs section");
    &note[at..]
}

#[test]
fn a_start_before_the_trailing_run_still_describes_the_stop() {
    // The start is written when the job starts, long before the trailing
    // run: the note joins it by id across the whole log, past the lines
    // between, so the stop keeps its description.
    let (_home, old) = session_log(&[
        session_start(),
        started("j_1", "npm run dev"),
        turn_done(),
        turn_done(),
        cancelled("j_1"),
        rewound_line(),
    ]);
    let (_point_home, dir) = session_log(&[session_start()]);
    let note = rewind_note_with_jobs(&old, &dir, Seq(0)).unwrap();
    assert!(jobs_section(&note).contains("- j_1: npm run dev"), "{note}");
}

#[test]
fn with_jobs_and_no_file_changes_the_section_follows_unchanged() {
    let (_home, old) = session_log(&[
        session_start(),
        started("j_1", "npm run dev"),
        cancelled("j_1"),
        rewound_line(),
    ]);
    let (_point_home, dir) = session_log(&[session_start()]);
    let note = rewind_note_with_jobs(&old, &dir, Seq(0)).unwrap();
    let unchanged = note
        .find("No file was written")
        .expect("the note opens unchanged");
    let section = jobs_section(&note);
    assert!(
        unchanged < note.find("Jobs that were running").unwrap(),
        "{note}"
    );
    assert_eq!(
        section,
        "Jobs that were running when this conversation was rewound were stopped:\n\
         - j_1: npm run dev\n\
         Start any of them again if you still need it."
    );
}

#[test]
fn with_an_ancestor_point_the_section_comes_from_the_old_log() {
    let (_home, old) = session_log(&[
        session_start(),
        started("j_old", "old work"),
        cancelled("j_old"),
        rewound_line(),
    ]);
    // The point's own log holds a decoy run: the note ignores it.
    let (_point_home, dir) = session_log(&[
        session_start(),
        started("j_decoy", "decoy"),
        cancelled("j_decoy"),
        rewound_line(),
    ]);
    let note = rewind_note_with_jobs(&old, &dir, Seq(0)).unwrap();
    let section = jobs_section(&note);
    assert!(section.contains("- j_old: old work"), "{note}");
    assert!(!section.contains("j_decoy"), "{note}");
}
