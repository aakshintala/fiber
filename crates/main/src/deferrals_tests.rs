//! `tools.*.deferred` and `mcp.servers.*.declare_in_full` reach the
//! loop's deferral input; `tools."tool_search".deferred` is reported and
//! ignored.

use super::deferrals;
use crate::test_support;

fn config(overrides: &[&str]) -> config::Config {
    test_support::config("fiber-deferrals-settings", overrides)
}

fn notices(config: &config::Config) -> (r#loop::Deferrals, Vec<contract::events::Notice>) {
    let mut notices = Vec::new();
    let deferrals = deferrals(config, &mut notices);
    (deferrals, notices)
}

#[test]
fn absent_keys_give_the_default() {
    let (deferrals, notices) = notices(&config(&[]));
    assert_eq!(deferrals, r#loop::Deferrals::default());
    assert!(notices.is_empty());
}

#[test]
fn a_true_key_is_read_by_tool_name() {
    let (deferrals, notices) = notices(&config(&["tools.x.deferred=true"]));
    assert_eq!(deferrals.tools.len(), 1);
    assert_eq!(deferrals.tools["x"], true);
    assert!(notices.is_empty());
}

#[test]
fn a_false_key_is_read_by_tool_name() {
    let (deferrals, notices) = notices(&config(&["tools.x.deferred=false"]));
    assert_eq!(deferrals.tools.len(), 1);
    assert_eq!(deferrals.tools["x"], false);
    assert!(notices.is_empty());
}

#[test]
fn both_values_together_give_both() {
    let (deferrals, _) = notices(&config(&[
        "tools.x.deferred=true",
        "tools.y.deferred=false",
    ]));
    assert_eq!(deferrals.tools.len(), 2);
    assert_eq!(deferrals.tools["x"], true);
    assert_eq!(deferrals.tools["y"], false);
}

#[test]
fn a_quoted_name_holding_a_dot_is_one_tool() {
    let (deferrals, _) = notices(&config(&["tools.\"srv.tool\".deferred=true"]));
    assert_eq!(deferrals.tools.len(), 1);
    assert_eq!(deferrals.tools["srv.tool"], true);
}

#[test]
fn a_tool_named_only_by_another_key_gives_no_entry() {
    let (deferrals, notices) = notices(&config(&["tools.read.max_result_bytes=100"]));
    assert!(deferrals.tools.is_empty());
    assert!(notices.is_empty());
}

#[test]
fn declare_in_full_true_names_its_server() {
    let (deferrals, notices) = notices(&config(&["mcp.servers.fx.declare_in_full=true"]));
    assert!(deferrals.full_servers.contains("fx"));
    assert!(notices.is_empty());
}

#[test]
fn declare_in_full_false_and_absent_name_no_server() {
    let (deferrals, _) = notices(&config(&["mcp.servers.fx.declare_in_full=false"]));
    assert!(deferrals.full_servers.is_empty());
    let (deferrals, _) = notices(&config(&[]));
    assert!(deferrals.full_servers.is_empty());
}

#[test]
fn a_server_name_holding_a_dot_is_one_server() {
    let (deferrals, _) = notices(&config(&["mcp.servers.\"a.b\".declare_in_full=true"]));
    assert_eq!(deferrals.full_servers.len(), 1);
    assert!(deferrals.full_servers.contains("a.b"));
}

#[test]
fn tool_search_true_gives_no_entry_and_one_notice() {
    let (deferrals, notices) = notices(&config(&["tools.\"tool_search\".deferred=true"]));
    assert!(deferrals.tools.is_empty());
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].code, contract::ErrorCode::ConfigKeyIgnored);
    assert_eq!(notices[0].extension, None);
    assert_eq!(
        notices[0].message,
        "`tools.\"tool_search\".deferred` has no effect: `tool_search` is never deferred."
    );
}

#[test]
fn tool_search_false_gives_no_entry_and_one_notice() {
    let (deferrals, notices) = notices(&config(&["tools.\"tool_search\".deferred=false"]));
    assert!(deferrals.tools.is_empty());
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].code, contract::ErrorCode::ConfigKeyIgnored);
    assert_eq!(
        notices[0].message,
        "`tools.\"tool_search\".deferred` has no effect: `tool_search` is never deferred."
    );
}

#[test]
fn tool_search_with_other_keys_only_gives_no_notice() {
    let (deferrals, notices) = notices(&config(&["tools.tool_search.max_result_bytes=100"]));
    assert!(deferrals.tools.is_empty());
    assert!(notices.is_empty());
}
