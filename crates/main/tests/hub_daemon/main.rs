//! The `hub_daemon` test binary: one module per file in this directory, so a
//! test's name is `<file>::<test>`. A flaky test files its issue under
//! the file (`docs/testing/running.md`, "Flaky tests").

#![allow(
    clippy::unwrap_used,
    reason = "the shared support modules unwrap as a failing test would"
)]

#[path = "../support/hub_start.rs"]
#[allow(dead_code, reason = "each module of this binary uses part of it")]
mod hub_start;
#[path = "../support/mod.rs"]
mod support;

mod changed_files_remote;
mod deadline;
mod feed;
mod hub;
mod hub_delegate_resume;
mod hub_environment;
mod hub_exit_window;
mod hub_list;
mod hub_login;
mod hub_pairing;
mod hub_prune;
mod hub_remote_config;
mod hub_remote_terminal;
mod hub_restart_feed;
mod hub_resume;
mod hub_resume_ack;
mod hub_start_overrides;
mod hub_start_worktree;
mod hub_status;
mod hub_stream_closed;
mod hub_version;
mod keeper;
mod keeper_version;
mod shutdown;
mod socket;
mod stream_closed_notice;
mod subscribe_read_error;
mod support_helpers;
