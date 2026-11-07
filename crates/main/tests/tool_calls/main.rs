//! The `tool_calls` test binary: one module per file in this directory, so a
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

mod approve;
mod driver_shell_no_timeout;
mod driver_shell_sent;
mod driver_shell_spill;
mod failed_tool_result_text;
mod mcp;
mod mcp_commands;
mod mcp_elicit;
mod mcp_login;
mod mcp_remote;
mod mcp_view;
mod replay_arguments;
mod result_caps;
mod sandbox;
mod search;
mod tool_search;
mod tools;
mod tools_tokens;
mod watchers;
