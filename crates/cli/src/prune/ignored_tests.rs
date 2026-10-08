//! Tests for the ignored-file summary: grouping, the no-follow size walk,
//! and the row segment, on `TempDir` trees and hand-built entry lists.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use super::*;

fn entry(path: &str, is_dir: bool) -> worktree::IgnoredEntry {
    worktree::IgnoredEntry {
        path: PathBuf::from(path),
        is_dir,
    }
}

#[test]
fn groups_sum_only_the_listed_paths() {
    let home = fakes::TempDir::new("cli-ignored-groups");
    fs::create_dir_all(home.path().join("sub/build")).unwrap();
    fs::write(home.path().join("sub/build/out.bin"), "12345").unwrap();
    fs::write(home.path().join("sub/t"), "x".repeat(10 * 1024)).unwrap();
    let summary = summarize(home.path(), &[entry("sub/build", true)]);
    assert_eq!(summary.groups, vec![("sub/".to_owned(), 5)]);
    assert_eq!(summary.unreadable, 0);
}

#[test]
fn a_bare_file_has_no_suffix_and_a_directory_does() {
    let home = fakes::TempDir::new("cli-ignored-suffix");
    fs::create_dir_all(home.path().join("target")).unwrap();
    fs::write(home.path().join("target/out.bin"), "12345").unwrap();
    fs::write(home.path().join(".env"), "123456").unwrap();
    let summary = summarize(home.path(), &[entry("target", true), entry(".env", false)]);
    assert_eq!(
        summary.groups,
        vec![(".env".to_owned(), 6), ("target/".to_owned(), 5)]
    );
}

#[test]
fn a_nested_file_suffixes_its_top_level() {
    let home = fakes::TempDir::new("cli-ignored-nested");
    fs::create_dir_all(home.path().join("a/b")).unwrap();
    fs::write(home.path().join("a/b/c.txt"), "123").unwrap();
    let summary = summarize(home.path(), &[entry("a/b/c.txt", false)]);
    assert_eq!(summary.groups, vec![("a/".to_owned(), 3)]);
}

#[test]
fn summarize_orders_by_bytes_then_label() {
    let home = fakes::TempDir::new("cli-ignored-order");
    fs::write(home.path().join("b"), "123456").unwrap();
    fs::write(home.path().join("a"), "123456").unwrap();
    fs::create_dir_all(home.path().join("big")).unwrap();
    fs::write(home.path().join("big/out.bin"), "1234567890").unwrap();
    let summary = summarize(
        home.path(),
        &[entry("b", false), entry("a", false), entry("big", true)],
    );
    assert_eq!(
        summary.groups,
        vec![
            ("big/".to_owned(), 10),
            ("a".to_owned(), 6),
            ("b".to_owned(), 6),
        ]
    );
}

#[test]
fn a_symlink_counts_its_link_length_not_its_target() {
    let home = fakes::TempDir::new("cli-ignored-link");
    let outside = home.path().join("outside.bin");
    fs::write(&outside, "y".repeat(100_000)).unwrap();
    std::os::unix::fs::symlink(&outside, home.path().join("link.bin")).unwrap();
    let summary = summarize(home.path(), &[entry("link.bin", false)]);
    let link_len = fs::symlink_metadata(home.path().join("link.bin"))
        .unwrap()
        .len();
    assert!(link_len < 100_000);
    assert_eq!(summary.groups, vec![("link.bin".to_owned(), link_len)]);
}

#[test]
fn a_symlinked_directory_is_never_descended_into() {
    let home = fakes::TempDir::new("cli-ignored-link-dir");
    let outside = home.path().join("real");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("big.bin"), "y".repeat(100_000)).unwrap();
    std::os::unix::fs::symlink(&outside, home.path().join("dir")).unwrap();
    let summary = summarize(home.path(), &[entry("dir", true)]);
    let link_len = fs::symlink_metadata(home.path().join("dir")).unwrap().len();
    assert_eq!(summary.groups, vec![("dir/".to_owned(), link_len)]);
}

#[test]
fn an_unreadable_directory_is_skipped_and_counted() {
    let home = fakes::TempDir::new("cli-ignored-unreadable");
    let locked = home.path().join("target/locked");
    fs::create_dir_all(&locked).unwrap();
    fs::write(home.path().join("target/out.bin"), "12345").unwrap();
    fs::write(locked.join("secret.bin"), "secret").unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let summary = summarize(home.path(), &[entry("target", true)]);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(summary.groups, vec![("target/".to_owned(), 5)]);
    assert_eq!(summary.unreadable, 1);
}

#[test]
fn sized_counts_bytes_and_unreadable_exactly() {
    let home = fakes::TempDir::new("cli-ignored-sized");
    let locked = home.path().join("target/locked");
    fs::create_dir_all(&locked).unwrap();
    fs::write(home.path().join("target/out.bin"), "12345").unwrap();
    fs::write(locked.join("secret.bin"), "secret").unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let result = sized(home.path().join("target"));
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(result, (5, 1));
}

#[test]
fn push_children_pushes_readable_and_counts_each_unreadable() {
    let mut stack = Vec::new();
    let children = vec![
        Ok(PathBuf::from("a")),
        Err(std::io::Error::other("entry")),
        Err(std::io::Error::other("entry")),
        Ok(PathBuf::from("b")),
    ];
    let mut unreadable = 0_u64;
    push_children(children.into_iter(), &mut stack, &mut unreadable);
    assert_eq!(unreadable, 2);
    assert_eq!(stack, vec![PathBuf::from("a"), PathBuf::from("b")]);
}

#[test]
fn a_missing_listed_path_counts_unreadable() {
    let home = fakes::TempDir::new("cli-ignored-missing");
    let summary = summarize(home.path(), &[entry("gone", false)]);
    assert_eq!(summary.groups, vec![("gone".to_owned(), 0)]);
    assert_eq!(summary.unreadable, 1);
}

#[test]
fn a_control_character_in_a_name_prints_escaped() {
    let home = fakes::TempDir::new("cli-ignored-control");
    fs::write(home.path().join("we\nird"), "123").unwrap();
    let summary = summarize(home.path(), &[entry("we\nird", false)]);
    assert_eq!(summary.groups, vec![("we\\nird".to_owned(), 3)]);
}

#[test]
fn segment_strings() {
    let one = Summary {
        groups: vec![("target/".to_owned(), 5)],
        unreadable: 0,
    };
    assert_eq!(segment(&one), "  ignored 5 B: target/");
    let two = Summary {
        groups: vec![("target/".to_owned(), 8192), (".env".to_owned(), 6)],
        unreadable: 0,
    };
    assert_eq!(segment(&two), "  ignored 8.0 KiB: target/, .env");
    let unread = Summary {
        groups: vec![("target/".to_owned(), 5120)],
        unreadable: 1,
    };
    assert_eq!(
        segment(&unread),
        "  ignored 5.0 KiB: target/ (1 unreadable)"
    );
    let empty = Summary {
        groups: Vec::new(),
        unreadable: 0,
    };
    assert_eq!(segment(&empty), "");
}
