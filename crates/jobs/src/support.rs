//! One definition for each test helper the jobs tests share
//! (`docs/code-quality.md` allows shared test support): the zero-adjacent
//! `usage` and `exited` fixtures and the registry builders. Test-only;
//! production never reads this module.

use std::collections::BTreeMap;
use std::sync::Arc;

use contract::ActionId;
use contract::clock::Clock;
use contract::events::{FiberExited, FinalMessage};
use contract::jobs::{Opening, Stop};
use contract::shapes::{Tokens, Usage};
use fakes::clock::FakeClock;
use fakes::{Recorder, TempDir};

use crate::registry::Registry;

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
