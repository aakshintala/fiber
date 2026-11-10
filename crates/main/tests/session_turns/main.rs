//! The `session_turns` test binary: one module per file in this directory, so a
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

mod compaction_kept;
mod continue_session;
mod from_session;
mod history_page_failed;
mod quit_after_completed;
mod reload;
mod resume;
mod retry;
mod rewind;
mod session_command;
mod session_list;
mod session_message;
mod session_search;
mod sessions;
mod sessions_search;
mod stop;
