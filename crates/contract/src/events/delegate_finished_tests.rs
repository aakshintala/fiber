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
fn the_final_message_file_is_artifacts_final_md() {
    assert_eq!(super::FINAL_MESSAGE, "artifacts/final.md");
}

#[test]
fn a_settled_finish_names_no_file_and_an_old_line_replays_without_one() {
    let mut finished = exited(0, Some("Done."), None).delegate_finished(&job_id());
    assert_eq!(finished.artifact_absolute, None);
    // An absent path is not written.
    let line = serde_json::to_value(&finished).unwrap();
    assert!(line.get("artifact_absolute").is_none(), "{line}");
    // A present path round-trips, and a line without the field replays as
    // none.
    finished.artifact_absolute = Some("/s/artifacts/final.md".into());
    let mut line = serde_json::to_value(&finished).unwrap();
    assert_eq!(line["artifact_absolute"], "/s/artifacts/final.md");
    let back: super::super::DelegateFinished = serde_json::from_value(line.clone()).unwrap();
    assert_eq!(back, finished);
    line.as_object_mut().unwrap().remove("artifact_absolute");
    let old: super::super::DelegateFinished = serde_json::from_value(line).unwrap();
    assert_eq!(old.artifact_absolute, None);
}
