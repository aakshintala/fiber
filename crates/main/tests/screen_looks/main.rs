//! The `screen_looks` test binary: one module per file in this directory, so a
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

mod approval_look;
mod ask_user_form_look;
mod group_look;
mod handoff_look;
mod hover_look;
mod item_views_look;
mod light_no_color_look;
mod link_look;
mod look;
mod look_markdown;
mod opened_call_look;
mod panel_look;
mod rail_look;
mod search_look;
mod shell_look;
mod swapped_panels_look;
mod working_line_look;
