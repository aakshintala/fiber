//! The diagnostics leaf (`docs/state.md`, "What each part holds"): the writer
//! behind the per-process files in `logs/`, the operation timer every timed
//! place goes through, and the usage, rate and allocation folds. It depends
//! only on `contract` and `support`, so `tui` takes it without taking `log`;
//! `log` re-exports these paths so existing `log::diag` users keep compiling.

#[cfg(any(test, feature = "alloc-stats"))]
pub mod alloc_stats;
#[cfg(test)]
mod alloc_stats_tests;
pub mod diag;
pub mod rate;
pub mod usage;

pub use diag::{Diag, Timer, timed};
pub use rate::{Rate, RateFold};
pub use usage::usage;
