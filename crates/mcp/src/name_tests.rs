//! A short qualified name is unchanged, and one at exactly the limit too.

use super::{HASH_LEN, MAX_NAME_LEN, qualified};

#[test]
fn a_short_name_is_unchanged() {
    assert_eq!(qualified("fx", "echo"), "mcp__fx__echo");
}

#[test]
fn a_name_at_exactly_the_limit_is_unchanged() {
    // `mcp__` is 5 chars and `__` is 2, so server and tool fill the rest.
    let server = "s".repeat(28);
    let tool = "t".repeat(29);
    let name = qualified(&server, &tool);
    assert_eq!(name.chars().count(), MAX_NAME_LEN);
    assert_eq!(name, format!("mcp__{server}__{tool}"));
}

#[test]
fn one_char_over_is_cut_to_exactly_the_limit() {
    let server = "s".repeat(28);
    let tool = "t".repeat(30);
    let full = format!("mcp__{server}__{tool}");
    assert_eq!(full.chars().count(), MAX_NAME_LEN + 1);
    let name = qualified(&server, &tool);
    assert_eq!(name.chars().count(), MAX_NAME_LEN);
    let head: String = full.chars().take(MAX_NAME_LEN - 1 - HASH_LEN).collect();
    assert!(name.starts_with(&head));
    let tail = name.get(head.len()..).unwrap_or_default();
    assert!(tail.starts_with('_'));
    assert_eq!(tail.len(), 1 + HASH_LEN);
    assert!(tail[1..].chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn two_names_sharing_a_long_prefix_stay_distinct() {
    let server = "s".repeat(60);
    let first = qualified(&server, "tool-alpha");
    let second = qualified(&server, "tool-beta");
    assert_ne!(first, second);
    assert_eq!(first.chars().count(), MAX_NAME_LEN);
    assert_eq!(second.chars().count(), MAX_NAME_LEN);
}

#[test]
fn the_hash_covers_the_whole_name_not_the_cut() {
    // Two names whose first `MAX_NAME_LEN - 1 - HASH_LEN` chars agree but
    // whose tails differ: hashing the cut alone would collide.
    let prefix = "p".repeat(MAX_NAME_LEN);
    let first = qualified("srv", &format!("{prefix}-alpha"));
    let second = qualified("srv", &format!("{prefix}-beta"));
    assert_ne!(first, second);
}

#[test]
fn multibyte_chars_cut_on_a_char_boundary() {
    let server = "é".repeat(60);
    let name = qualified(&server, "tool");
    assert_eq!(name.chars().count(), MAX_NAME_LEN);
    assert!(name.is_char_boundary(name.len()));
}
