//! Snapshot tests beside [`super::to_markdown`]: about ten real pages under
//! open licences (`tests/pages/README.md`), each checked against its saved
//! expected markdown.
//!
//! `#![allow(..., reason = ...)]` header shared by the test modules in this
//! crate: tests unwrap and index freely.
#![allow(clippy::unwrap_used, reason = "tests unwrap")]
#![allow(clippy::indexing_slicing, reason = "tests index")]

use super::to_markdown;

/// Whether an expected file may be written: only when `FIBER_UPDATE_PAGES=1`
/// is set and `CI` is not.
fn may_update() -> bool {
    std::env::var("FIBER_UPDATE_PAGES").as_deref() == Ok("1") && std::env::var("CI").is_err()
}

#[test]
fn real_pages_convert_to_their_expected_markdown() {
    // The snapshot pages beside this test, compiled in: reading them at
    // run time would read the repository at test time (`docs/ci.md`,
    // "Selection"). Each entry names the page, then holds its HTML and
    // its expected markdown.
    const PAGES: &[(&str, &str, &str)] = &[
        (
            "mdn-anchor",
            include_str!("../../tests/pages/mdn-anchor.html"),
            include_str!("../../tests/pages/mdn-anchor.md"),
        ),
        (
            "mdn-code",
            include_str!("../../tests/pages/mdn-code.html"),
            include_str!("../../tests/pages/mdn-code.md"),
        ),
        (
            "mdn-table",
            include_str!("../../tests/pages/mdn-table.html"),
            include_str!("../../tests/pages/mdn-table.md"),
        ),
        (
            "python-datastructures",
            include_str!("../../tests/pages/python-datastructures.html"),
            include_str!("../../tests/pages/python-datastructures.md"),
        ),
        (
            "python-errors",
            include_str!("../../tests/pages/python-errors.html"),
            include_str!("../../tests/pages/python-errors.md"),
        ),
        (
            "python-introduction",
            include_str!("../../tests/pages/python-introduction.html"),
            include_str!("../../tests/pages/python-introduction.md"),
        ),
        (
            "rust-data-types",
            include_str!("../../tests/pages/rust-data-types.html"),
            include_str!("../../tests/pages/rust-data-types.md"),
        ),
        (
            "rust-option",
            include_str!("../../tests/pages/rust-option.html"),
            include_str!("../../tests/pages/rust-option.md"),
        ),
        (
            "rust-ownership",
            include_str!("../../tests/pages/rust-ownership.html"),
            include_str!("../../tests/pages/rust-ownership.md"),
        ),
        (
            "wiki-toml",
            include_str!("../../tests/pages/wiki-toml.html"),
            include_str!("../../tests/pages/wiki-toml.md"),
        ),
    ];
    assert!(!PAGES.is_empty(), "no pages compiled in");
    for (name, html, expected) in PAGES.iter().copied() {
        let markdown = to_markdown(html);
        if may_update() {
            let expected_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/pages")
                .join(format!("{name}.md"));
            std::fs::write(&expected_path, &markdown).unwrap();
            continue;
        }
        if markdown != expected {
            let diff = similar::TextDiff::from_lines(expected, &markdown)
                .unified_diff()
                .header(
                    &format!("expected {name}.md"),
                    &format!("actual {name}.html"),
                )
                .to_string();
            panic!("{name} changed:\n{diff}");
        }
    }
}
