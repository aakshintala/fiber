//! What a resume does with the jobs a keeper handed over.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use contract::events::{
    Event, JobCompleted, JobStarted, Outcome, SessionStarted, Variables, VariablesSource,
};
use contract::{JobId, SessionId};

use super::{Resumed, resumed};

/// A log with `j_run` and `j_kept` started and never ended, `j_done` started
/// and ended, and `j_again` ended and then started again.
fn folded() -> (fakes::TempDir, Resumed) {
    let root = fakes::TempDir::new("fiber-keep-jobs");
    let log = log::Log::create(
        root.path(),
        SessionId("s_1".into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    log.append(
        &Event::SessionStarted(SessionStarted {
            workspace: "/w".into(),
            variables: Variables {
                path: String::new(),
                names: Vec::new(),
                source: VariablesSource::Inherited,
            },
            parent: None,
            forked_from: None,
            rewind: None,
            worktree: None,
            supervised: false,
        }),
        None,
        None,
    )
    .unwrap();
    let start = |id: &str| {
        log.append(
            &Event::JobStarted(JobStarted {
                job_id: JobId(id.into()),
                tool: None,
                extension: None,
                description: format!("job {id}"),
                output_path: format!("artifacts/{id}.log"),

                pgid: None,
                start_time: None,
                kind: None,
            }),
            None,
            None,
        )
        .unwrap();
    };
    let end = |id: &str| {
        log.append(
            &Event::JobCompleted(JobCompleted {
                job_id: JobId(id.into()),
                status: Outcome::Completed,
                error: None,
                process: None,
                output_tail: None,

                notice: None,
                changed_by: None,
                protected_paths: None,
            }),
            None,
            None,
        )
        .unwrap();
    };
    start("j_run");
    start("j_kept");
    start("j_done");
    end("j_done");
    start("j_again");
    end("j_again");
    start("j_again");
    let folded = resumed(&root.path().join("s_1")).unwrap();
    (root, folded)
}

fn orphans(folded: &Resumed) -> std::collections::BTreeSet<&str> {
    folded
        .orphans
        .iter()
        .map(|orphan| orphan.job_id.0.as_str())
        .collect()
}

#[test]
fn keeper_resume_removes_only_the_named_orphan_candidates() {
    let (_root, mut folded) = folded();
    assert_eq!(
        orphans(&folded),
        ["j_run", "j_kept", "j_again"].into_iter().collect()
    );
    folded.keep_jobs(&[JobId("j_kept".into())]);
    // The unaccounted job is still orphaned, in its place.
    assert_eq!(orphans(&folded), ["j_run", "j_again"].into_iter().collect());
}

#[test]
fn keeper_resume_ignores_jobs_that_are_not_candidates() {
    let (_root, mut folded) = folded();
    // An ended job, and one the log never had: nothing to remove, nothing
    // invented.
    folded.keep_jobs(&[JobId("j_done".into()), JobId("j_unknown".into())]);
    assert_eq!(
        orphans(&folded),
        ["j_run", "j_kept", "j_again"].into_iter().collect()
    );
    folded.keep_jobs(&[]);
    assert_eq!(
        orphans(&folded),
        ["j_run", "j_kept", "j_again"].into_iter().collect()
    );
}

#[test]
fn keeper_resume_removes_a_reopened_job_once_and_keeps_the_rest_of_the_fold() {
    let (_root, mut folded) = folded();
    let end = folded.end;
    let window = folded.window;
    let workspace = folded.workspace.clone();
    folded.keep_jobs(&[JobId("j_again".into()), JobId("j_run".into())]);
    assert_eq!(orphans(&folded), ["j_kept"].into_iter().collect());
    // The replay, the delegate sessions and the window are untouched.
    assert_eq!(folded.end, end);
    assert_eq!(folded.window, window);
    assert_eq!(folded.workspace, workspace);
    assert!(folded.delegate_sessions.is_empty());
    // Keeping a job twice is the same as once.
    folded.keep_jobs(&[JobId("j_again".into())]);
    assert_eq!(orphans(&folded), ["j_kept"].into_iter().collect());
}
