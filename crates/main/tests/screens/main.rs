//! The `screens` test binary: one module per file in this directory, so a
//! test's name is `<file>::<test>`. A flaky test files its issue under
//! the file (`docs/testing/running.md`, "Flaky tests").

#![allow(
    clippy::unwrap_used,
    reason = "the shared support modules unwrap as a failing test would"
)]

#[path = "../support/hub_start.rs"]
mod hub_start;
#[path = "../support/mod.rs"]
mod support;

mod ask_user_form;
mod at_path_search;
mod attach_first_frame;
mod call_view;
mod exit_window;
mod home_drop;
mod home_rail;
mod image_png;
mod inline_images;
mod journeys;
mod line_start_end;
mod pinned_call_row;
mod placeholders_picker;
mod pty_sync_output;
mod rail_focus;
mod rail_shown;
mod rail_state_tints;
mod steer_drawn;
mod terminal;
mod terminal_overrides;
mod tui_click_extension;
mod tui_command_extension;
mod tui_context_after_cd;
mod tui_key_whole_screen;
mod wheel_garble;
mod wheel_scroll;
mod workspace_picker;
