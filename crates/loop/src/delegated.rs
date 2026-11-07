//! A Fiber delegate's finish, cut once at the write (`docs/delegates.md`,
//! "Results"): the first 16 KiB stays in `delegate_finished.text`, and the
//! full message is in the delegate's `artifacts/final.md`, which the cut
//! notice names. Nothing else cuts delegate text, and nothing here writes a
//! file.

use std::path::Path;

use contract::events::DelegateFinished;
use contract::tool::Bound;
use log::Log;

/// `finished` with its text cut to [`Bound::DEFAULT`], the notice naming
/// the final-message file when the finish names one. A text of exactly
/// 16 KiB is not cut.
// debt: `_log` is unused because the delivery writes the file; drop it with
// the `log` parameter of `calls::job_event` when that signature next changes.
pub(crate) fn bounded(_log: &Log, finished: DelegateFinished) -> DelegateFinished {
    let cap = Bound::DEFAULT.start.saturating_add(Bound::DEFAULT.end);
    if finished.text.len() <= cap {
        return finished;
    }
    let (head, tail) = Log::cut_points(&finished.text, Bound::DEFAULT);
    let removed = u64::try_from(tail.saturating_sub(head)).unwrap_or(u64::MAX);
    let saved = finished
        .artifact_absolute
        .as_deref()
        .map(Path::new)
        .ok_or("the delegate's final-message file was not written");
    let notice = log::cut_notice(removed, saved);
    let text = log::cut_joined(
        finished.text.get(..head).unwrap_or_default(),
        &notice,
        finished.text.get(tail..).unwrap_or_default(),
    );
    DelegateFinished { text, ..finished }
}

#[cfg(test)]
#[path = "delegated_tests.rs"]
mod tests;
