//! `tools."<name>".max_result_bytes` reaches the loop's caps by tool name.

use super::result_caps;
use crate::test_support;

fn config(overrides: &[&str]) -> config::Config {
    test_support::config("fiber-result-caps-settings", overrides)
}

#[test]
fn no_configured_cap_gives_an_empty_map() {
    assert!(result_caps(&config(&[])).is_empty());
}

#[test]
fn a_set_cap_is_read_by_tool_name() {
    let caps = result_caps(&config(&["tools.read.max_result_bytes=100"]));
    assert_eq!(caps.len(), 1);
    assert_eq!(caps["read"], 100);
}

#[test]
fn a_zero_cap_is_kept() {
    let caps = result_caps(&config(&["tools.shell.max_result_bytes=0"]));
    assert_eq!(caps.len(), 1);
    assert_eq!(caps["shell"], 0);
}

#[test]
fn a_quoted_name_holding_a_dot_is_one_tool() {
    let caps = result_caps(&config(&["tools.\"srv.tool\".max_result_bytes=7"]));
    assert_eq!(caps.len(), 1);
    assert_eq!(caps["srv.tool"], 7);
}

#[test]
fn a_tool_with_other_keys_only_has_no_cap() {
    assert!(result_caps(&config(&["tools.read.deferred=true"])).is_empty());
}
