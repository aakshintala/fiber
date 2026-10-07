//! The key map overlay's text (`docs/tui.md`, "Bindings").

use crate::bindings::BINDINGS;

/// The key map as plain lines: each area's heading, then one line per
/// binding with its keys and other paths, a blank line between areas.
pub(crate) fn lines() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut area = None;
    for binding in BINDINGS {
        if area != Some(binding.area) {
            if area.is_some() {
                out.push(String::new());
            }
            out.push(binding.area.to_owned());
            area = Some(binding.area);
        }
        let mut line = format!("  {} · {}", binding.description, binding.keys);
        if !binding.other_paths.is_empty() {
            line.push_str(" · ");
            line.push_str(binding.other_paths);
        }
        out.push(line);
    }
    out
}

#[cfg(test)]
#[path = "keymap_tests.rs"]
mod tests;
