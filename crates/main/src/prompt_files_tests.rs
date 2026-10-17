//! Tests for `prompt_files`: `agents_home`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use super::agents_home;

#[test]
fn the_agents_home_is_an_absolute_home_value() {
    assert_eq!(
        agents_home(Some("/home/alice".into())),
        Some(std::path::PathBuf::from("/home/alice"))
    );
    assert_eq!(agents_home(None), None);
    assert_eq!(agents_home(Some("".into())), None);
    assert_eq!(agents_home(Some("relative/home".into())), None);
}
