//! The key map overlay's text (`docs/tui.md`, "Bindings").

use crate::bindings::BINDINGS;
use crate::keyset::Keyset;

/// The key map as plain lines: each area's heading, then one line per
/// binding with its keys and other paths, a blank line between areas.
/// A row the person did not set shows the doc's Key cell; a row the
/// person set shows its keys' labels; an unbound row shows `unbound`.
pub(crate) fn lines(keys: &Keyset) -> Vec<String> {
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
        let mut line = format!("  {} · {}", binding.description, keys.shown(binding));
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
