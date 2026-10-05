//! The built-in tools' guidelines (`docs/system-prompt.md`, "Tool
//! guidelines"): one `##` section per tool in
//! `crates/tools/prompt/guidelines.md`.

/// The guideline body for `name`: the section's text with its heading
/// excluded and blank lines at either end removed. `None` when the file
/// holds no such section.
// `loop` keeps its own copy of the splitter (`loop::prompt::section`):
// the crates are separate, so this one keeps the shortest form here.
pub(crate) fn of(name: &str) -> Option<String> {
    let md = include_str!("../prompt/guidelines.md");
    let mut lines: Vec<&str> = Vec::new();
    let mut in_section = false;
    for line in md.lines() {
        if let Some(at) = line.strip_prefix("## ") {
            if in_section {
                break;
            }
            if at == name {
                in_section = true;
            }
        } else if in_section {
            lines.push(line);
        }
    }
    if !in_section {
        return None;
    }
    while lines.first().is_some_and(|l| l.trim().is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    Some(lines.join("\n"))
}

#[cfg(test)]
#[path = "guidelines_tests.rs"]
mod tests;
