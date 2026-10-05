//! Tests beside [`super`]: the skipped directories and the notice lines.

use std::collections::BTreeMap;
use std::fs;

use super::{find_line, grep_line, skipped, union};

fn tree(files: &BTreeMap<&str, &str>) -> fakes::TempDir {
    let dir = fakes::TempDir::new("fiber-search-notice");
    for (path, contents) in files {
        let full = dir.path().join(path);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&full, contents).unwrap();
    }
    dir
}

#[test]
fn skipped_names_top_level_ignored_directories_sorted() {
    let dir = tree(&BTreeMap::from([
        (".gitignore", "sk_b/\nsk_a/\n"),
        ("sk_b/hay.txt", "hay"),
        ("sk_a/hay.txt", "hay"),
        ("kept_dir/hay.txt", "hay"),
    ]));
    assert_eq!(skipped(dir.path(), dir.path()), ["sk_a/", "sk_b/"]);
}

#[test]
fn skipped_leaves_version_control_out() {
    let dir = tree(&BTreeMap::from([
        (".gitignore", "sk_c/\n"),
        ("sk_c/hay.txt", "hay"),
        (".git/HEAD", "ref"),
    ]));
    assert_eq!(skipped(dir.path(), dir.path()), ["sk_c/"]);
}

#[test]
fn skipped_is_empty_when_nothing_was_skipped() {
    let dir = tree(&BTreeMap::from([
        ("kept_dir/hay.txt", "hay"),
        (".hid_dir/hay.txt", "hay"),
    ]));
    assert!(skipped(dir.path(), dir.path()).is_empty());
}

#[test]
fn skipped_leaves_ignored_files_out() {
    // Only directories are reported: an ignored file is not a directory
    // to search by name.
    let dir = tree(&BTreeMap::from([
        (".gitignore", "skipped.txt\n"),
        ("skipped.txt", "hay"),
    ]));
    assert!(skipped(dir.path(), dir.path()).is_empty());
}

#[test]
fn lines_name_the_directories_and_an_example() {
    let skipped = ["sk_a/".to_owned(), "sk_b/".to_owned()];
    assert_eq!(
        grep_line("needle", &skipped).unwrap(),
        "grep: no match. Skipped ignored directories: sk_a/, sk_b/. Search one by name, such as `grep -r needle sk_a`."
    );
    assert_eq!(
        find_line(&skipped).unwrap(),
        "find: no match. Skipped ignored directories: sk_a/, sk_b/. Search one by name, such as `find sk_a`."
    );
}

#[test]
fn lines_are_absent_when_nothing_was_skipped() {
    assert_eq!(grep_line("needle", &[]), None);
    assert_eq!(find_line(&[]), None);
}

#[test]
fn union_sorts_without_repeats() {
    assert_eq!(
        union(
            vec!["b/".to_owned()],
            vec!["a/".to_owned(), "b/".to_owned()]
        ),
        ["a/", "b/"]
    );
    assert_eq!(union(Vec::new(), Vec::new()), Vec::<String>::new());
}
