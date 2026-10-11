//! The process-wide variables source (`docs/invocation.md`, "A session's environment").
//!
//! Each test runs in its own nextest process, so setting the source here
//! never leaks into another test.

use contract::events::VariablesSource;

use super::{set_variables_source, variables, variables_source};

#[test]
fn unset_source_is_inherited() {
    assert_eq!(variables_source(), VariablesSource::Inherited);
    assert_eq!(variables().source, VariablesSource::Inherited);
}

#[test]
fn setting_login_shell_is_observed() {
    set_variables_source(VariablesSource::LoginShell);
    assert_eq!(variables_source(), VariablesSource::LoginShell);
    assert_eq!(variables().source, VariablesSource::LoginShell);
}
