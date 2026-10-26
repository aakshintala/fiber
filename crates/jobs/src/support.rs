//! One definition for each test helper the jobs tests share
//! (`docs/code-quality.md` allows shared test support): the zero-adjacent
//! `usage` and `exited` fixtures, the registry builders, and the inert
//! worktree finish. Test-only; production never reads this module.

use std::collections::BTreeMap;
use std::sync::Arc;

use contract::ActionId;
use contract::clock::Clock;
use contract::events::{DelegateStarted, FiberExited, FinalMessage};
use contract::jobs::{Opening, Stop};
use contract::shapes::{Tokens, Usage};
use fakes::clock::FakeClock;
use fakes::{Recorder, TempDir};

use crate::registry::Registry;
use crate::{EndWorktree, FinishWorktree};

/// A small usage: some tokens and a known cost.
pub(crate) fn usage() -> Usage {
    Usage {
        tokens: Tokens {
            input: 10,
            cache_read: 0,
            cache_write: BTreeMap::new(),
            output: 5,
        },
        cost: Some(0.25),
        subscription_cost: 0.0,
    }
}

/// A clean `fiber_exited` carrying `text`.
pub(crate) fn exited(text: &str) -> FiberExited {
    FiberExited {
        exit_code: 0,
        usage: usage(),
        final_message: Some(FinalMessage {
            final_action_id: ActionId("a_1".into()),
            text: text.to_owned(),
        }),
        error: None,
        suspended_on: None,
        questions: None,
    }
}

/// A clean end for `id`: exited 0 with no output.
pub(crate) fn ended_ok(id: &str) -> contract::events::JobCompleted {
    contract::events::JobCompleted {
        job_id: contract::JobId(id.into()),
        status: contract::events::Outcome::Completed,
        error: None,
        process: Some(contract::shapes::Process {
            exit_code: Some(0),
            signal: None,
            timed_out: false,
        }),
        output_tail: None,

        notice: None,
        changed_by: None,
    }
}

/// A delegate's finish for `id` with `text`.
pub(crate) fn delegate_finished(id: &str, text: &str) -> contract::events::DelegateFinished {
    contract::events::DelegateFinished {
        job_id: contract::JobId(id.into()),
        text: text.to_owned(),
        artifact: None,
        questions: None,
        usage: usage(),
        worktree: None,

        changed_by: None,
    }
}

/// A delegate's first run, for `job_id`, as `delegate_spawn` records it.
pub(crate) fn delegate_started(job_id: &str) -> DelegateStarted {
    DelegateStarted {
        job_id: contract::JobId(job_id.into()),
        delegate_session_id: contract::SessionId("s_d000000000000001".into()),
        harness: "fiber".into(),
        model: "fiber:fake/m".into(),
        workspace: "/w".into(),
        worktree: None,
        forked_from: None,
    }
}

/// A worktree finish that reports the recorded path and branch as clean:
/// the inert mock for tests that never isolate.
pub(crate) fn clean_finish() -> FinishWorktree {
    Arc::new(|end: EndWorktree| contract::events::FinishedWorktree {
        worktree: contract::shapes::Worktree {
            path: end.path.to_string_lossy().into_owned(),
            branch: end.branch,
            repository: None,
        },
        dirty: false,
    })
}

/// A registry on the given clock, with its temp directory.
pub(crate) fn registry_with(clock: Arc<dyn Clock>) -> (TempDir, Arc<Registry>) {
    let dir = TempDir::new("fiber-jobs");
    let artifacts = dir.path().join("artifacts");
    std::fs::create_dir(&artifacts).unwrap();
    let registry = Registry::new(artifacts, clock, Arc::new(Recorder::default()));
    (dir, registry)
}

/// A registry on a fresh fake clock, with its temp directory.
pub(crate) fn world() -> (TempDir, Arc<Registry>) {
    let clock: Arc<dyn Clock> = FakeClock::new();
    registry_with(clock)
}

/// An opening for a shell job called `description`.
pub(crate) fn opening(description: &str) -> Opening {
    Opening {
        tool: "shell".into(),
        description: description.into(),
        stop: Stop(Box::new(|| {})),
        lines: false,
        input: None,
    }
}
