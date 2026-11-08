//! The `extension_host` test binary: one module per file in this directory, so a
//! test's name is `<file>::<test>`. A flaky test files its issue under
//! the file (`docs/testing/running.md`, "Flaky tests").

#![allow(
    clippy::unwrap_used,
    reason = "the shared support modules unwrap as a failing test would"
)]

#[path = "../extension_harness/mod.rs"]
mod extension_harness;
#[path = "../support/hub_start.rs"]
mod hub_start;
#[path = "../support/mod.rs"]
mod support;

mod damaged_extension;
mod extension_ask;
mod extension_callback_shutdown;
mod extension_case;
mod extension_case_reviewer;
mod extension_codemode;
mod extension_commands;
mod extension_diagnostics;
mod extension_export;
mod extension_lazy_commands;
mod extension_project;
mod extension_remove_missing;
mod extension_state_resume;
mod extension_step;
mod extension_test;
mod extension_tool_keep;
mod extension_tools;
mod extension_update_all;
mod hooks;
mod hooks_entries;
mod hooks_offer;
mod import_offer;
mod memory;
mod opening;
mod process_extensions;
mod propose_notes;
mod repository_hooks;
mod repository_load;
mod repository_offer;
mod repository_servers;
mod review_sweep;
mod reviewer_notes;
mod reviewer_setup;
mod skill;
mod tui_extensions;
