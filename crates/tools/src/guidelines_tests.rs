//! Tests for `guidelines::of`: every built-in tool with a section
//! returns it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use contract::tool::Tool;

use super::of;

/// `Skills` with no entry: `file` is `None`, and `body` is an error.
struct NoSkills;

impl contract::skills::Skills for NoSkills {
    fn file(&self, _name: &str) -> Option<std::path::PathBuf> {
        None
    }

    fn body(
        &self,
        _name: &str,
        _file: &std::path::Path,
    ) -> Result<String, contract::skills::SkillRead> {
        Err(contract::skills::SkillRead::Invalid)
    }
}

/// A `SearchBackend` with no results.
struct NoResults;

impl contract::search::SearchBackend for NoResults {
    fn search(
        &self,
        _query: &str,
        _domains: &contract::search::Domains,
        _cancel: &dyn contract::tool::Cancel,
    ) -> Result<Option<Vec<contract::search::SearchResult>>, contract::shapes::Failure> {
        Ok(Some(Vec::new()))
    }
}

#[test]
fn every_builtin_tool_with_a_section_returns_it() {
    let files = crate::Files::new(std::path::PathBuf::from("/ws"));
    let read = files.read();
    let write = files.write();
    let edit = files.edit();
    let shell = crate::Shell::new(std::env::temp_dir(), fakes::clock::FakeClock::new());
    let skill = crate::Skill::new(std::sync::Arc::new(NoSkills));
    let hosted = crate::HostedSearch::new("web_search_20250305".to_owned());
    let backend = crate::BackendSearch::new(std::sync::Arc::new(NoResults));
    let cases = [
        (read.definition().name, read.guidelines()),
        (write.definition().name, write.guidelines()),
        (edit.definition().name, edit.guidelines()),
        (shell.definition().name, shell.guidelines()),
        (crate::Handoff.definition().name, crate::Handoff.guidelines()),
        (skill.definition().name, skill.guidelines()),
        (hosted.definition().name, hosted.guidelines()),
        (backend.definition().name, backend.guidelines()),
    ];
    for (name, guidelines) in cases {
        let text = guidelines.unwrap_or_else(|| panic!("{name} has guidelines"));
        assert_eq!(text, of(&name).unwrap(), "{name}");
    }
}

#[test]
fn unknown_tool_has_no_guidelines() {
    assert_eq!(of("nope"), None);
}

#[test]
fn a_section_stops_before_the_next_heading() {
    // `read` is followed by a blank line and `## edit`: neither the
    // blank line nor the next section's text is part of it.
    let read = of("read").unwrap();
    assert!(!read.contains("Change an existing file"), "{read}");
    assert!(!read.contains("## "), "{read}");
    assert_eq!(read, read.trim(), "{read}");
}

#[test]
fn leading_and_trailing_blank_lines_are_removed() {
    // Every section starts with a blank line after its heading and
    // ends before a blank line and the next heading.
    for name in ["read", "edit", "write", "shell"] {
        let text = of(name).unwrap();
        assert!(!text.is_empty(), "{name}");
        assert_eq!(text, text.trim(), "{name}: {text}");
    }
}
