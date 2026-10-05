//! The built-in tools' guidelines (`docs/system-prompt.md`, "Tool
//! guidelines"): one `##` section per tool in
//! `crates/tools/prompt/guidelines.md`.

/// The guideline body for `name`: the section's text with its heading
/// excluded and trailing whitespace trimmed. `None` when the file holds
/// no such section.
pub(crate) fn of(name: &str) -> Option<String> {
    let md = include_str!("../prompt/guidelines.md");
    let mut heading: Option<&str> = None;
    let mut lines: Vec<&str> = Vec::new();
    let mut capture: Option<Vec<&str>> = None;
    for line in md.lines() {
        if let Some(at) = line.strip_prefix("## ") {
            if heading.is_some_and(|h| h == name) {
                capture = Some(std::mem::take(&mut lines));
                break;
            }
            heading = Some(at);
            lines.clear();
        } else if heading.is_some_and(|h| h == name) {
            lines.push(line);
        }
    }
    if capture.is_none() && heading.is_some_and(|h| h == name) {
        capture = Some(lines);
    }
    let lines = capture?;
    let start = lines
        .iter()
        .position(|l| !l.trim().is_empty())
        .unwrap_or(lines.len());
    let end = lines
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map_or(0, |at| at + 1);
    if start >= end {
        return Some(String::new());
    }
    Some(
        lines
            .iter()
            .skip(start)
            .take(end.saturating_sub(start))
            .copied()
            .collect::<Vec<_>>()
            .join("\n")
            .trim_end()
            .to_owned(),
    )
}

#[cfg(test)]
#[path = "guidelines_tests.rs"]
mod tests;
