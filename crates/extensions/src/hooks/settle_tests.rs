//! Settling a session's extension names from metadata: the tool `replaces`
//! check, then commands, then tool clashes.

#![allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]

use std::collections::BTreeSet;
use std::sync::Arc;

use config::{Config, ProjectKey, Sources};
use contract::ErrorCode;
use contract::clock::Clock;
use contract::events::Notice;
use fakes::clock::FakeClock;

use super::{Meta, Settled, settle};

/// `fiber.test/<short>`, replacing `replaces`, with these commands and tools.
fn meta(short: &str, replaces: &[&str], commands: &[&str], tools: &[&str]) -> Meta {
    Meta {
        extension: format!("fiber.test/{short}"),
        replaces: replaces.iter().map(|s| (*s).to_owned()).collect(),
        commands: commands
            .iter()
            .map(|name| ((*name).to_owned(), String::new()))
            .collect(),
        tools: tools.iter().map(|s| (*s).to_owned()).collect(),
    }
}

fn settled(metas: &[Meta]) -> Settled {
    let root = fakes::TempDir::new("fiber-settle");
    let config = Config::load(Sources {
        home: root.path().to_owned(),
        workspace: root.path().to_owned(),
        project: ProjectKey::new("p").unwrap(),
        overrides: Vec::new(),
    })
    .unwrap();
    let clock: Arc<dyn Clock> = FakeClock::new();
    settle(metas, &config, root.path(), &clock)
}

fn admitted(pairs: &[(&str, &str)]) -> BTreeSet<(String, String)> {
    pairs
        .iter()
        .map(|(ext, tool)| (format!("fiber.test/{ext}"), (*tool).to_owned()))
        .collect()
}

fn unloaded(shorts: &[&str]) -> BTreeSet<String> {
    shorts.iter().map(|s| format!("fiber.test/{s}")).collect()
}

fn undeclared(tool: &str, short: &str) -> Notice {
    Notice {
        code: ErrorCode::ExtensionFailed,
        message: format!(
            "Tool `{tool}` replaces a built-in tool its manifest does not list in `replaces`; `fiber.test/{short}` is not loaded."
        ),
        extension: Some(format!("fiber.test/{short}")),
    }
}

fn clash(message: &str) -> Notice {
    Notice {
        code: ErrorCode::ExtensionFailed,
        message: message.to_owned(),
        extension: None,
    }
}

#[test]
fn a_declared_replacement_of_a_built_in_tool_is_admitted() {
    let settled = settled(&[meta("myread", &["read"], &[], &["read"])]);
    assert_eq!(settled.tools, admitted(&[("myread", "read")]));
    assert!(settled.unloaded.is_empty());
    assert_eq!(settled.notices, []);
}

#[test]
fn an_undeclared_replacement_unloads_the_extension_naming_its_first_tool() {
    let settled = settled(&[
        meta("myread", &[], &[], &["write", "read", "mine"]),
        meta("other", &[], &[], &["other"]),
    ]);
    assert_eq!(settled.unloaded, unloaded(&["myread"]));
    assert_eq!(settled.tools, admitted(&[("other", "other")]));
    assert_eq!(settled.notices, [undeclared("read", "myread")]);
}

#[test]
fn web_search_is_a_built_in_whatever_the_model() {
    let settled = settled(&[meta("search", &[], &[], &["web_search"])]);
    assert_eq!(settled.unloaded, unloaded(&["search"]));
    assert_eq!(settled.notices, [undeclared("web_search", "search")]);
}

#[test]
fn a_name_one_extension_registers_is_admitted() {
    let settled = settled(&[meta("a", &[], &[], &["x"])]);
    assert_eq!(settled.tools, admitted(&[("a", "x")]));
    assert_eq!(settled.notices, []);
}

#[test]
fn two_extensions_registering_one_name_both_lose_it_and_keep_the_rest() {
    let settled = settled(&[
        meta("b", &[], &[], &["x", "only_b"]),
        meta("a", &[], &[], &["x", "only_a"]),
    ]);
    assert!(settled.unloaded.is_empty());
    assert_eq!(settled.tools, admitted(&[("a", "only_a"), ("b", "only_b")]));
    assert_eq!(
        settled.notices,
        [clash(
            "Extensions `fiber.test/a` and `fiber.test/b` both register the tool `x`, so neither gets it."
        )]
    );
}

#[test]
fn three_extensions_registering_one_name_give_one_notice_naming_all_three() {
    let settled = settled(&[
        meta("c", &[], &[], &["x"]),
        meta("a", &[], &[], &["x"]),
        meta("b", &[], &[], &["x"]),
    ]);
    assert!(settled.tools.is_empty());
    assert_eq!(
        settled.notices,
        [clash(
            "Extensions `fiber.test/a`, `fiber.test/b` and `fiber.test/c` both register the tool `x`, so neither gets it."
        )]
    );
}

#[test]
fn an_extension_unloaded_by_its_tools_takes_no_part_in_a_command_clash() {
    let settled = settled(&[
        meta("myread", &[], &["notes"], &["read"]),
        meta("notes", &[], &["notes"], &[]),
    ]);
    assert_eq!(settled.unloaded, unloaded(&["myread"]));
    assert_eq!(settled.notices, [undeclared("read", "myread")]);
}

#[test]
fn an_extension_unloaded_by_its_commands_takes_no_part_in_a_tool_clash() {
    let settled = settled(&[
        meta("model", &[], &["model"], &["x"]),
        meta("other", &[], &[], &["x"]),
    ]);
    assert_eq!(settled.unloaded, unloaded(&["model"]));
    assert_eq!(settled.tools, admitted(&[("other", "x")]));
    assert_eq!(settled.notices.len(), 1, "{:?}", settled.notices);
    assert_eq!(
        settled.notices[0].extension.as_deref(),
        Some("fiber.test/model")
    );
}
