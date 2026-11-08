//! The `delegation` test binary: one module per file in this directory, so a
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

mod ask;
mod ask_worktree;
mod delegate_final_message;
mod delegate_provider_message;
mod delegates;
mod supervised;
