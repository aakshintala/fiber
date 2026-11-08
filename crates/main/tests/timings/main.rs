//! The `timings` test binary: one module per file in this directory, so a
//! test's name is `<file>::<test>`. A flaky test files its issue under
//! the file (`docs/testing/running.md`, "Flaky tests").

#![allow(
    clippy::unwrap_used,
    reason = "the shared support modules unwrap as a failing test would"
)]

#[path = "../extension_harness/mod.rs"]
mod extension_harness;
#[path = "../support/mod.rs"]
mod support;

mod alloc_stats;
mod dhat_heap;
mod hub_timing;
mod jobs_timing;
mod log_search_timing;
mod loop_timing;
mod mcp_timing;
mod picture_timing;
mod startup_timing;
mod tool_timing;
mod tui_timing_tests;
mod worktree_timing;
