//! Tests for the key map's text.

use super::lines;
use crate::bindings::BINDINGS;

#[test]
fn every_binding_shows_under_its_area_with_its_other_paths() {
    let lines = lines();
    // Five headings, four blank lines between them, one line per binding.
    assert_eq!(lines.len(), BINDINGS.len() + 5 + 4);
    assert_eq!(lines.first().map(String::as_str), Some("Sessions"));
    assert!(lines.contains(&"  Open the key map · F1 · /? or /help".to_owned()));
    assert!(lines.contains(&"  Paste an image · Ctrl+V".to_owned()));
    let steering = lines.iter().position(|line| line == "Steering");
    let drop = lines
        .iter()
        .position(|line| line == "  Drop it · ⌥X · its mouse target");
    assert_eq!(steering.map(|at| at + 3), drop);
    assert_eq!(
        steering
            .and_then(|at| lines.get(at - 1))
            .map(String::as_str),
        Some("")
    );
}
