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
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/pages");
    let mut pages: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "html"))
        .collect();
    pages.sort();
    assert!(!pages.is_empty(), "no pages in {}", dir.display());
    for page in pages {
        let bytes = std::fs::read(&page).unwrap();
        let html = String::from_utf8_lossy(&bytes);
        let markdown = to_markdown(&html);
        let expected_path = page.with_extension("md");
        if may_update() {
            std::fs::write(&expected_path, &markdown).unwrap();
            continue;
        }
        let expected = std::fs::read_to_string(&expected_path)
            .unwrap_or_else(|_| panic!("no expected markdown at {}", expected_path.display()));
        if expected != markdown {
            let diff = similar::TextDiff::from_lines(&expected, &markdown)
                .unified_diff()
                .header(
                    &format!("expected {}", expected_path.display()),
                    &format!("actual {}", page.display()),
                )
                .to_string();
            panic!("{} changed:\n{diff}", page.display());
        }
    }
}
