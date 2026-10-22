//! The generated command reference (`docs/ci.md`, "The user docs site"):
//! a drift test regenerates `docs/user/commands.md` from the same command
//! definitions as `fiber help` and fails when the committed copy differs.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use std::path::PathBuf;

/// The committed page, from the repository root.
const COMMITTED: &str = include_str!("../../../docs/user/commands.md");

/// The visible subcommand paths in definition order, depth first: the same
/// definitions as `fiber help` and shell completion.
fn subcommand_paths(command: &clap::Command, prefix: &[String], out: &mut Vec<Vec<String>>) {
    for sub in command.get_subcommands().filter(|sub| !sub.is_hide_set()) {
        let mut path = prefix.to_vec();
        path.push(sub.get_name().to_owned());
        out.push(path.clone());
        subcommand_paths(sub, &path, out);
    }
}

/// The whole page: the header, then the menu under `## fiber`, then each
/// visible subcommand's help in definition order, depth first. Help width
/// never depends on a terminal, and no section holds a version string.
fn render_reference() -> String {
    let command = crate::cli::command();
    let mut sections = vec![Vec::new()];
    subcommand_paths(&command, &[], &mut sections);
    let mut page = String::from(
        "# Command reference\n\
         \n\
         Generated from `fiber help`. Do not edit by hand; run \
         `FIBER_UPDATE_COMMANDS=1 cargo nextest run -p main --bins command_reference` \
         to regenerate.\n\
         \n",
    );
    for path in &sections {
        let name = if path.is_empty() {
            "fiber".to_owned()
        } else {
            format!("fiber {}", path.join(" "))
        };
        let help = crate::cli::render_help(path).unwrap();
        page.push_str(&format!(
            "## {name}\n\n```text\n{}\n```\n\n",
            help.trim_end()
        ));
    }
    page
}

// debt: two copies of Difference/first_difference/excerpt (here and xtask/tests/models_dev.rs, which adds a model id); merge into fakes when a third copy appears.
/// Where `expected` and `actual` first differ.
struct Difference {
    /// The first differing byte offset.
    offset: usize,
    /// Its 1-based line in the committed text.
    line: usize,
}

/// The first byte where `expected` and `actual` differ; None when they
/// are identical.
fn first_difference(expected: &str, actual: &str) -> Option<Difference> {
    let mut offset = expected
        .bytes()
        .zip(actual.bytes())
        .take_while(|pair| pair.0 == pair.1)
        .count();
    if offset == expected.len() && offset == actual.len() {
        return None;
    }
    while expected.get(..offset).is_none() && offset > 0 {
        offset -= 1;
    }
    let before = expected.get(..offset).unwrap_or("");
    let line = before.bytes().filter(|byte| *byte == b'\n').count() + 1;
    Some(Difference { offset, line })
}

/// At most 200 bytes of the line holding `offset`.
fn excerpt(text: &str, offset: usize) -> String {
    let before = text.get(..offset.min(text.len())).unwrap_or("");
    let start = before.rfind('\n').map(|index| index + 1).unwrap_or(0);
    let line = text.get(start..).unwrap_or("").lines().next().unwrap_or("");
    let mut end = line.len().min(200);
    while line.get(..end).is_none() && end > 0 {
        end -= 1;
    }
    line.get(..end).unwrap_or("").to_owned()
}

/// One failure message for a committed page that differs from the
/// generated one: never the whole page, only the lengths and the first
/// difference.
fn drift_message(expected: &str, actual: &str, difference: &Difference) -> String {
    format!(
        "docs/user/commands.md: committed {} bytes, generated {} bytes; first difference at byte {} (line {}); committed: {}; generated: {}; run `FIBER_UPDATE_COMMANDS=1 cargo nextest run -p main --bins command_reference` to regenerate",
        expected.len(),
        actual.len(),
        difference.offset,
        difference.line,
        excerpt(expected, difference.offset),
        excerpt(actual, difference.offset),
    )
}

/// Whether the drift test writes the page instead of comparing: only when
/// `FIBER_UPDATE_COMMANDS=1` is set and `CI` is not.
fn may_update() -> bool {
    std::env::var("FIBER_UPDATE_COMMANDS").as_deref() == Ok("1") && std::env::var("CI").is_err()
}

#[test]
fn first_difference_reports_offset_and_line() {
    assert!(first_difference("ab", "ab").is_none());
    let difference = first_difference("x", "y").unwrap();
    assert_eq!(difference.offset, 0);
    assert_eq!(difference.line, 1);
    let expected = "a\nb\nc\nd\n";
    let actual = "a\nb\nX\nd\n";
    let difference = first_difference(expected, actual).unwrap();
    assert_eq!(difference.offset, 4);
    assert_eq!(difference.line, 3);
    let difference = first_difference("abc", "ab").unwrap();
    assert_eq!((difference.offset, difference.line), (2, 1));
    let difference = first_difference("ab", "abc").unwrap();
    assert_eq!((difference.offset, difference.line), (2, 1));
    let difference = first_difference("éa", "èa").unwrap();
    assert_eq!((difference.offset, difference.line), (0, 1));
}

/// The headings of the rendered page: lines starting with `## `.
fn headings(page: &str) -> Vec<&str> {
    page.lines()
        .filter(|line| line.starts_with("## "))
        .collect()
}

#[test]
fn generated_page_holds_visible_commands_and_hides_hidden_ones() {
    let page = render_reference();
    let heads = headings(&page);
    assert!(heads.contains(&"## fiber sessions search"), "{heads:?}");
    assert!(heads.contains(&"## fiber extension install"), "{heads:?}");
    assert!(
        heads
            .iter()
            .position(|head| *head == "## fiber sessions")
            .is_some_and(|sessions| heads
                .iter()
                .position(|head| *head == "## fiber sessions search")
                .is_some_and(|search| sessions < search)),
        "{heads:?}"
    );
    assert!(
        !heads.contains(&"## fiber grep"),
        "hidden commands stay out of the reference: {heads:?}"
    );
    assert!(
        !page.contains(&crate::cli::version_line()),
        "the reference holds no version string"
    );
}

#[test]
fn generated_page_matches_the_committed_one() {
    let actual = render_reference();
    if may_update() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/user/commands.md");
        std::fs::write(&path, &actual).unwrap();
        return;
    }
    if let Some(difference) = first_difference(COMMITTED, &actual) {
        panic!("{}", drift_message(COMMITTED, &actual, &difference));
    }
}
