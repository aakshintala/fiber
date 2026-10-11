//! The lowest declared thinking level (`docs/model-routing.md`, "Thinking"):
//! every `ThinkingLevel::ALL` singleton gives itself, array order is
//! ignored, and no levels give none.

use contract::ThinkingLevel;

use super::lowest;

#[test]
fn every_singleton_gives_itself() {
    for level in ThinkingLevel::ALL {
        assert_eq!(lowest(&[level]), Some(level));
    }
}

#[test]
fn no_levels_give_none() {
    assert_eq!(lowest(&[]), None);
}

#[test]
fn array_order_is_ignored() {
    assert_eq!(
        lowest(&[ThinkingLevel::High, ThinkingLevel::Minimal]),
        Some(ThinkingLevel::Minimal)
    );
}
