//! A Fiber delegate's finish, cut once at the write (`docs/delegates.md`,
//! "Results"): the first 16 KiB stays in `delegate_finished.text`, the full
//! text goes to `artifacts/<job_id>.txt`. Nothing else cuts delegate text.

use contract::events::DelegateFinished;
use contract::tool::Bound;
use log::Log;

/// `finished` with its text cut to [`Bound::DEFAULT`], recording the full
/// text's artifact when it was cut. A text of exactly 16 KiB is not cut.
pub(crate) fn bounded(log: &Log, finished: DelegateFinished) -> DelegateFinished {
    let cap = Bound::DEFAULT.start.saturating_add(Bound::DEFAULT.end);
    if finished.text.len() <= cap {
        return finished;
    }
    let name = format!("{}.txt", finished.job_id.0);
    let (text, artifact) = log.cut_output(&finished.text, Bound::DEFAULT, &name);
    DelegateFinished {
        text,
        artifact,
        ..finished
    }
}

#[cfg(test)]
#[path = "delegated_tests.rs"]
mod tests;
