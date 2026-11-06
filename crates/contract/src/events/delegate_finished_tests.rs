//! Tests first for the extracted settle fold: the finish mirrors the live
//! arm on every exit shape, with the usage copied through and no worktree.

use super::super::{FiberExited, FinalMessage};
use crate::shapes::Failure;
use crate::shapes::{Tokens, Usage};
use crate::{ActionId, ErrorCode, JobId};

fn job_id() -> JobId {
    JobId("j_0123456789abcdef".into())
}

fn usage() -> Usage {
    Usage {
        tokens: Tokens {
            input: 7,
            cache_read: 1,
            cache_write: std::collections::BTreeMap::new(),
            output: 3,
        },
        cost: Some(0.02),
        subscription_cost: 0.0,
    }
}

fn exited(code: i32, text: Option<&str>, error: Option<Failure>) -> FiberExited {
    FiberExited {
        exit_code: code,
        usage: usage(),
        final_message: text.map(|text| FinalMessage {
            final_action_id: ActionId("a_9".into()),
            text: text.to_owned(),
        }),
        error,
        suspended_on: None,
        questions: None,
    }
}

fn failed(message: &str) -> Failure {
    Failure {
        code: ErrorCode::InvalidArguments,
        message: message.to_owned(),
        retry_after_ms: None,
        provider: None,
    }
}

#[test]
fn a_finished_mirrors_the_live_arm_on_three_exits() {
    // A clean exit, a nonzero exit carrying its error, and a bare nonzero
    // exit: the text follows the final message in every row.
    for (line, text) in [
        (exited(0, Some("Done."), None), "Done."),
        (
            exited(1, Some("almost"), Some(failed("Bad model."))),
            "almost",
        ),
        (exited(1, None, None), ""),
    ] {
        let finished = line.delegate_finished(&job_id());
        assert_eq!(finished.job_id, job_id());
        assert_eq!(finished.text, text);
        assert_eq!(finished.artifact, None);
        assert_eq!(finished.worktree, None);
        assert_eq!(finished.changed_by, None);
    }
}

#[test]
fn usage_is_copied_from_the_exit_line() {
    // No aggregate is computed: the settle carries exactly what the exit
    // line reported.
    let line = exited(0, Some("Done."), None);
    let finished = line.delegate_finished(&job_id());
    assert_eq!(finished.usage, usage());
}

#[test]
fn the_fold_sets_no_worktree() {
    // Only the live drive assigns a worktree, after the fold: the fold
    // itself leaves it unset.
    let line = exited(0, Some("Done."), None);
    let finished = line.delegate_finished(&job_id());
    assert_eq!(finished.worktree, None);
    assert_eq!(finished.questions, None);
}
