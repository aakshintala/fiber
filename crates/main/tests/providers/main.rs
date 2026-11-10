//! The `providers` test binary: one module per file in this directory, so a
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

mod bedrock;
mod cache_countdown;
mod codex;
mod credential_signals;
mod credential_timeout;
mod databricks;
mod live_codex;
mod live_databricks;
mod live_opencode;
mod live_openrouter;
mod login;
mod model_picker_keys;
mod model_picker_name;
mod model_switch;
mod models;
mod models_hub_env;
mod opencode;
mod opencode_go_store_false;
mod placeholders;
mod provider_diagnostics;
mod provider_request_builtin_tools;
mod quota;
mod quota_fetch_session;
mod scripted;
mod vertex;
mod web_search;
