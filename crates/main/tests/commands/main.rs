//! The `commands` test binary: one module per file in this directory, so a
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

mod archive;
mod change_workspace;
mod changed_files_editor;
mod clear;
mod completion;
mod config;
mod delete;
mod diagnostics;
mod doctor;
mod doctor_command;
mod exit_alias;
mod feedback;
mod import;
mod install_script;
mod name_session;
mod prune;
mod pwd;
mod release_install;
mod release_notes;
mod release_pack;
mod setup;
mod slash_aliases;
mod startup_log;
mod stdout_errors;
mod tui_silent_exit;
mod tui_startup_failure;
mod update;
