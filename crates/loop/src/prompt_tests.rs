//! Tests for `prompt::section` and `prompt::fill`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use std::path::PathBuf;
use std::sync::Arc;

use super::{PromptInputs, fill, section, system_prompt};

/// Every optional input absent, for system prompt tests: the opening
/// message fields never reach `system_prompt`.
fn empty_inputs() -> PromptInputs {
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    PromptInputs::new(
        PathBuf::from("/test-home"),
        "/bin/sh".into(),
        "/test-home/s_test/events.jsonl".into(),
        clock,
    )
}

fn full_inputs() -> PromptInputs {
    let mut inputs = empty_inputs();
    inputs.system = Some("Custom system.".into());
    inputs.append = Some("Appendix.".into());
    inputs.addendum = Some("Addendum.".into());
    inputs.extensions = vec![
        ("zeta".into(), "Z text.".into()),
        ("alpha".into(), "A text.".into()),
    ];
    inputs
}

fn tools() -> Vec<(String, Option<String>, bool)> {
    vec![
        ("write".into(), Some("Write guide.".into()), false),
        ("read".into(), Some("Read guide.".into()), false),
    ]
}

#[test]
fn system_prompt_orders_all_five_parts() {
    let prompt = system_prompt(&full_inputs(), "fake/model-1", true, &tools());
    let custom = prompt.find("Custom system.").unwrap();
    let tools_at = prompt.find("# Tools").unwrap();
    let session = prompt.find("# Session").unwrap();
    let alpha = prompt.find("alpha").unwrap();
    let appendix = prompt.find("Appendix.").unwrap();
    assert!(custom < tools_at && tools_at < session && session < alpha && alpha < appendix);
    assert!(prompt.contains("Nobody is present"));
    assert!(prompt.contains("Addendum."));
    // Tools and extensions in name order regardless of input order.
    assert!(prompt.find("### read").unwrap() < prompt.find("### write").unwrap());
    assert!(alpha < prompt.find("zeta").unwrap());
}

#[test]
fn system_md_replaces_fibers_text_and_keeps_the_rest() {
    let prompt = system_prompt(&full_inputs(), "m", false, &[]);
    assert!(prompt.contains("Custom system."));
    assert!(!prompt.contains("operating inside Fiber"));
    assert!(prompt.contains("# Session"));
}

#[test]
fn default_inputs_use_fibers_text() {
    let prompt = system_prompt(&empty_inputs(), "m", false, &[]);
    assert!(prompt.contains("operating inside Fiber"));
}

#[test]
fn deferred_tools_leave_their_guidelines_out() {
    let tools = vec![
        ("read".into(), Some("Read guide.".into()), true),
        ("write".into(), Some("Write guide.".into()), false),
    ];
    let prompt = system_prompt(&empty_inputs(), "m", false, &tools);
    assert!(!prompt.contains("Read guide."));
    assert!(prompt.contains("Write guide."));
}

#[test]
fn unattended_line_only_when_unattended() {
    let plain = system_prompt(&empty_inputs(), "m", false, &[]);
    assert!(!plain.contains("Nobody is present"));
    let away = system_prompt(&empty_inputs(), "m", true, &[]);
    assert!(away.contains("Nobody is present"));
}

#[test]
fn empty_parts_leave_no_blank_line() {
    let prompt = system_prompt(&empty_inputs(), "m", false, &[]);
    assert!(!prompt.contains("\n\n\n"));
    assert!(!prompt.contains("# Tools"));
    assert!(!prompt.contains("Appendix"));
    // Whitespace-only files count as absent.
    let mut inputs = empty_inputs();
    inputs.system = Some("   \n".into());
    inputs.append = Some("\n".into());
    inputs.addendum = Some("  ".into());
    inputs.extensions = vec![("e".into(), "  ".into())];
    let prompt = system_prompt(&inputs, "m", false, &[]);
    assert!(prompt.contains("operating inside Fiber"));
    assert!(!prompt.contains("# Tools"));
}

#[test]
fn same_inputs_give_equal_strings_twice() {
    let inputs = full_inputs();
    let tools = tools();
    assert_eq!(
        system_prompt(&inputs, "m", true, &tools),
        system_prompt(&inputs, "m", true, &tools)
    );
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 14_695_981_039_346_656_037;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(1_099_511_628_211);
    }
    hash
}

fn pinned(bytes: &[u8], len: usize, hash: u64) {
    assert_eq!(
        bytes.len(),
        len,
        "length changed: edit is a reviewed change"
    );
    assert_eq!(
        fnv1a(bytes),
        hash,
        "bytes changed: edit is a reviewed change"
    );
}

#[test]
fn system_md_bytes_are_pinned() {
    pinned(
        include_bytes!("../prompt/system.md"),
        1219,
        0xd3272595a6580bfd,
    );
}

#[test]
fn opening_md_bytes_are_pinned() {
    pinned(
        include_bytes!("../prompt/opening.md"),
        320,
        0xf7d734bea035be64,
    );
}

#[test]
fn messages_md_bytes_are_pinned() {
    pinned(
        include_bytes!("../prompt/messages.md"),
        2346,
        0x5536193a138753f0,
    );
}

#[test]
fn reviewer_md_bytes_are_pinned() {
    pinned(
        include_bytes!("../prompt/reviewer.md"),
        6976,
        0xdc89833223efef4e,
    );
}

#[test]
fn messages_md_holds_every_section_the_doc_names() {
    let md = include_str!("../prompt/messages.md");
    for name in [
        "tools",
        "tool",
        "session",
        "unattended",
        "instruction-file",
        "no-instruction-files",
        "subdirectory-file",
        "created-file",
        "replaced-file",
        "diff-file",
        "deleted-file",
        "date",
        "extension",
        "nudge",
        "handoff-note",
        "handoff-focus",
    ] {
        assert!(
            !section(md, name).is_empty(),
            "messages.md has no ## {name} section"
        );
    }
}

#[test]
fn fill_replaces_known_and_leaves_unknown_alone() {
    assert_eq!(
        fill("hello {name}, {x}!", &[("name", "world")]),
        "hello world, {x}!"
    );
}

#[test]
fn fill_does_not_rescan_inserted_text() {
    assert_eq!(
        fill("at {path}", &[("path", "/tmp/{date}"), ("date", "today")]),
        "at /tmp/{date}"
    );
}

#[test]
fn fill_names_may_hold_dashes_underscores_slashes_and_digits() {
    assert_eq!(fill("{a-b}", &[("a-b", "X")]), "X");
    assert_eq!(fill("{a_b}", &[("a_b", "X")]), "X");
    assert_eq!(fill("{a/b}", &[("a/b", "X")]), "X");
    assert_eq!(fill("{a1}", &[("a1", "X")]), "X");
    assert_eq!(fill("{a-b_c/d1}", &[("a-b_c/d1", "X")]), "X");
}

#[test]
fn fill_leaves_unclosed_and_empty_braces_alone() {
    assert_eq!(fill("a {oops", &[]), "a {oops");
    assert_eq!(fill("a {", &[]), "a {");
    assert_eq!(fill("a {}", &[]), "a {}");
    assert_eq!(fill("{} {name}", &[("name", "X")]), "{} X");
}

#[test]
fn fill_leaves_a_brace_inside_a_name_alone() {
    assert_eq!(fill("{a{b}", &[]), "{a{b}");
    assert_eq!(fill("{a{b}", &[("b", "B")]), "{aB");
}

#[test]
fn fill_keeps_multibyte_text_around_placeholders() {
    assert_eq!(
        fill("héllo {name} wörld ", &[("name", "X")]),
        "héllo X wörld "
    );
    assert_eq!(fill("日{name}本", &[("name", "X")]), "日X本");
}

#[test]
fn fill_value_holding_placeholders_is_not_rescanned() {
    assert_eq!(
        fill("{a} {date}", &[("a", "{date}"), ("date", "today")]),
        "{date} today"
    );
}

#[test]
fn section_strips_blank_lines_at_its_ends() {
    let md = "## a\n\nbody\n\n\n## b\nother\n";
    assert_eq!(section(md, "a"), "## a\n\nbody");
    assert_eq!(section(md, "b"), "## b\nother");
}

#[test]
fn missing_section_is_empty() {
    assert_eq!(section("## a\nbody\n", "b"), "");
}
