//! Tests beside [`super`]: what the walk finds, skips and reports.
//!
//! Corpus names dodge every common global gitignore (`target/`, `*.log`,
//! `node_modules/`), so a runner's own excludes cannot leak into a temp
//! tree; the cases that need an ignored directory ignore distinctive names
//! through the tree's own files instead.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use super::{DirRoot, Found, Root, io_message, root_of, walk, walk_error};

/// A corpus: relative paths mapped to file contents. A trailing `/` on a
/// key makes a directory; `.gitignore` and `.ignore` are files like any
/// other.
fn tree(files: &BTreeMap<&str, &str>) -> fakes::TempDir {
    let dir = fakes::TempDir::new("fiber-search-walk");
    for (path, contents) in files {
        let full = dir.path().join(path);
        if path.ends_with('/') {
            fs::create_dir_all(&full).unwrap();
        } else {
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&full, contents).unwrap();
        }
    }
    dir
}

fn found(results: &[Result<Found, super::WalkError>]) -> Vec<(PathBuf, bool, usize)> {
    results
        .iter()
        .map(|result| {
            let found = result.as_ref().unwrap();
            (found.display.clone(), found.file_type.is_dir(), found.depth)
        })
        .collect()
}

/// An absolute root printing its own paths.
fn dir_root(dir: &Path, name: &str) -> DirRoot {
    // The walker resolves a relative root against the process's own
    // directory, so tests pass absolute paths and spell what they expect
    // from them.
    let root = dir.join(name);
    DirRoot {
        walk: root.clone(),
        show: root,
    }
}

/// An absolute root printing the path as given.
fn shown(dir: &Path, name: &str) -> DirRoot {
    DirRoot {
        walk: dir.join(name),
        show: PathBuf::from(name),
    }
}

fn joined(dir: &Path, names: &[&str]) -> Vec<PathBuf> {
    names.iter().map(|name| dir.join(name)).collect()
}

#[test]
fn entries_come_sorted_with_the_root_first() {
    let dir = tree(&BTreeMap::from([
        ("b_hay.txt", "b"),
        ("a_needle.txt", "a"),
        ("sub/z_deep.txt", "z"),
        ("sub/a_deep.txt", "a"),
    ]));
    let root = dir.path().join(".");
    let results = walk(
        dir.path(),
        &DirRoot {
            walk: root,
            show: PathBuf::from("."),
        },
        None,
    )
    .collect::<Vec<_>>();
    let entries = found(&results);
    assert_eq!(
        entries
            .iter()
            .map(|(path, _, _)| path.to_owned())
            .collect::<Vec<_>>(),
        [
            PathBuf::from("."),
            PathBuf::from("./a_needle.txt"),
            PathBuf::from("./b_hay.txt"),
            PathBuf::from("./sub"),
            PathBuf::from("./sub/a_deep.txt"),
            PathBuf::from("./sub/z_deep.txt"),
        ]
    );
    assert_eq!(
        entries
            .iter()
            .map(|(_, _, depth)| *depth)
            .collect::<Vec<_>>(),
        [0, 1, 1, 1, 2, 2]
    );
}

#[test]
fn paths_print_below_the_root_as_given() {
    let dir = tree(&BTreeMap::from([("sub/f_hay.txt", "hay")]));
    let results = walk(dir.path(), &shown(dir.path(), "sub"), None).collect::<Vec<_>>();
    let entries = found(&results);
    assert_eq!(
        entries
            .iter()
            .map(|(path, _, _)| path.to_owned())
            .collect::<Vec<_>>(),
        [PathBuf::from("sub"), PathBuf::from("sub/f_hay.txt")]
    );
}

#[test]
fn a_gitignored_directory_is_skipped() {
    let dir = tree(&BTreeMap::from([
        (".gitignore", "skipped_dir/\n"),
        ("skipped_dir/hay.txt", "hay"),
        ("kept_dir/hay.txt", "hay"),
    ]));
    let results = walk(dir.path(), &dir_root(dir.path(), "."), None).collect::<Vec<_>>();
    let entries = found(&results);
    let names: Vec<PathBuf> = entries.into_iter().map(|(path, _, _)| path).collect();
    assert!(
        !names
            .iter()
            .any(|name| name.starts_with(dir.path().join("skipped_dir"))),
        "{names:?}"
    );
    assert!(
        names.contains(&dir.path().join("kept_dir/hay.txt")),
        "{names:?}"
    );
}

#[test]
fn an_ignore_file_is_read_too() {
    let dir = tree(&BTreeMap::from([
        (".ignore", "skipped_two/\n"),
        ("skipped_two/hay.txt", "hay"),
        ("kept_two/hay.txt", "hay"),
    ]));
    let results = walk(dir.path(), &dir_root(dir.path(), "."), None).collect::<Vec<_>>();
    let entries = found(&results);
    let names: Vec<PathBuf> = entries.into_iter().map(|(path, _, _)| path).collect();
    assert!(
        !names
            .iter()
            .any(|name| name.starts_with(dir.path().join("skipped_two"))),
        "{names:?}"
    );
    assert!(
        names.contains(&dir.path().join("kept_two/hay.txt")),
        "{names:?}"
    );
}

#[test]
fn version_control_directories_are_always_skipped() {
    let dir = tree(&BTreeMap::from([
        (".git/objects/pack", "pack"),
        (".svn/entries", "entries"),
        (".hg/store", "store"),
        ("plain/hay.txt", "hay"),
    ]));
    let results = walk(dir.path(), &dir_root(dir.path(), "."), None).collect::<Vec<_>>();
    let entries = found(&results);
    let names: Vec<PathBuf> = entries.into_iter().map(|(path, _, _)| path).collect();
    for buried in [".git", ".svn", ".hg"] {
        assert!(
            !names
                .iter()
                .any(|name| name.starts_with(dir.path().join(buried))),
            "{names:?}"
        );
    }
    assert!(
        names.contains(&dir.path().join("plain/hay.txt")),
        "{names:?}"
    );
}

#[test]
fn hidden_files_are_searched() {
    let dir = tree(&BTreeMap::from([(".hid_needle.txt", "hay")]));
    let results = walk(dir.path(), &dir_root(dir.path(), "."), None).collect::<Vec<_>>();
    let entries = found(&results);
    let names: Vec<PathBuf> = entries.into_iter().map(|(path, _, _)| path).collect();
    assert!(
        names.contains(&dir.path().join(".hid_needle.txt")),
        "{names:?}"
    );
}

#[test]
fn a_named_ignored_path_is_always_entered() {
    let dir = tree(&BTreeMap::from([
        (".gitignore", "skipped_dir/\n"),
        ("skipped_dir/hay.txt", "hay"),
    ]));
    let results = walk(dir.path(), &dir_root(dir.path(), "skipped_dir"), None).collect::<Vec<_>>();
    let entries = found(&results);
    let names: Vec<PathBuf> = entries.into_iter().map(|(path, _, _)| path).collect();
    assert_eq!(
        names,
        joined(dir.path(), &["skipped_dir", "skipped_dir/hay.txt"])
    );
}

#[test]
fn a_named_link_to_a_directory_is_walked_through_the_link() {
    let dir = tree(&BTreeMap::from([("real_dir/f_hay.txt", "hay")]));
    symlink("real_dir", dir.path().join("lnk_dir")).unwrap();
    let results = walk(dir.path(), &dir_root(dir.path(), "lnk_dir"), None).collect::<Vec<_>>();
    let entries = found(&results);
    assert_eq!(
        entries
            .iter()
            .map(|(path, _, depth)| (path.to_owned(), *depth))
            .collect::<Vec<_>>(),
        [
            (dir.path().join("lnk_dir"), 0),
            (dir.path().join("lnk_dir/f_hay.txt"), 1),
        ]
    );
}

#[test]
fn links_the_walk_finds_are_listed_never_followed() {
    let dir = tree(&BTreeMap::from([
        ("real_dir/f_hay.txt", "hay"),
        ("real_file.txt", "hay"),
    ]));
    symlink("real_file.txt", dir.path().join("lnk_file.txt")).unwrap();
    symlink("real_dir", dir.path().join("lnk_sub")).unwrap();
    let results = walk(dir.path(), &dir_root(dir.path(), "."), None).collect::<Vec<_>>();
    let entries = found(&results);
    let names: Vec<PathBuf> = entries.iter().map(|(path, _, _)| path.clone()).collect();
    assert!(
        names.contains(&dir.path().join("lnk_file.txt")),
        "{names:?}"
    );
    assert!(names.contains(&dir.path().join("lnk_sub")), "{names:?}");
    let link = dir.path().join("lnk_sub");
    assert!(
        !names
            .iter()
            .any(|name| name != &link && name.starts_with(&link)),
        "{names:?}"
    );
    for result in &results {
        let found = result.as_ref().unwrap();
        if found.display.ends_with("lnk_file.txt") || found.display.ends_with("lnk_sub") {
            assert!(found.file_type.is_symlink(), "{}", found.display.display());
        }
    }
}

#[test]
fn a_named_version_control_directory_is_entered() {
    let dir = tree(&BTreeMap::from([(".git/HEAD", "ref")]));
    let results = walk(dir.path(), &dir_root(dir.path(), ".git"), None).collect::<Vec<_>>();
    let entries = found(&results);
    let names: Vec<PathBuf> = entries.into_iter().map(|(path, _, _)| path).collect();
    assert_eq!(names, joined(dir.path(), &[".git", ".git/HEAD"]));
}

#[test]
fn a_file_named_like_a_vcs_directory_is_kept() {
    let dir = tree(&BTreeMap::from([("sub/.git", "not a directory")]));
    let results = walk(dir.path(), &dir_root(dir.path(), "."), None).collect::<Vec<_>>();
    let entries = found(&results);
    let names: Vec<PathBuf> = entries.into_iter().map(|(path, _, _)| path).collect();
    assert!(names.contains(&dir.path().join("sub/.git")), "{names:?}");
}

#[test]
fn max_depth_caps_the_walk() {
    let dir = tree(&BTreeMap::from([("sub/deep/f_hay.txt", "hay")]));
    let shallow = walk(dir.path(), &dir_root(dir.path(), "."), Some(1)).collect::<Vec<_>>();
    let entries = found(&shallow);
    let names: Vec<PathBuf> = entries.into_iter().map(|(path, _, _)| path).collect();
    assert!(names.contains(&dir.path().join("sub")), "{names:?}");
    assert!(
        !names
            .iter()
            .any(|name| name.starts_with(dir.path().join("sub/deep"))),
        "{names:?}"
    );
    let full = walk(dir.path(), &dir_root(dir.path(), "."), None).collect::<Vec<_>>();
    let entries = found(&full);
    let names: Vec<PathBuf> = entries.into_iter().map(|(path, _, _)| path).collect();
    assert!(
        names.contains(&dir.path().join("sub/deep/f_hay.txt")),
        "{names:?}"
    );
}

/// Restores `path`'s permissions on drop, so a temp tree stays removable.
struct Unlocked {
    path: PathBuf,
    kept: fs::Permissions,
}

impl Drop for Unlocked {
    fn drop(&mut self) {
        fs::set_permissions(&self.path, self.kept.clone()).unwrap();
    }
}

#[test]
fn an_unreadable_directory_is_reported_and_the_walk_continues() {
    let dir = tree(&BTreeMap::from([
        ("locked_dir/hay.txt", "hay"),
        ("open_hay.txt", "hay"),
    ]));
    let locked = dir.path().join("locked_dir");
    let kept = fs::metadata(&locked).unwrap().permissions();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let _unlocked = Unlocked {
        path: locked.clone(),
        kept,
    };
    let results = walk(dir.path(), &dir_root(dir.path(), "."), None).collect::<Vec<_>>();
    let mut failures = Vec::new();
    let mut names = Vec::new();
    for result in &results {
        match result {
            Ok(found) => names.push(found.display.clone()),
            Err(error) => failures.push((error.display.clone(), error.message.clone())),
        }
    }
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(failures[0].0.starts_with(&locked), "{failures:?}");
    assert_eq!(failures[0].1, "Permission denied");
    assert!(
        names.contains(&dir.path().join("open_hay.txt")),
        "{names:?}"
    );
}

#[test]
fn root_of_sorts_paths() {
    let dir = tree(&BTreeMap::from([
        ("real_dir/f_hay.txt", "hay"),
        ("real_file.txt", "hay"),
    ]));
    symlink("real_file.txt", dir.path().join("lnk_file.txt")).unwrap();
    symlink("real_dir", dir.path().join("lnk_dir")).unwrap();
    symlink("nowhere", dir.path().join("dangling")).unwrap();
    let file = root_of(dir.path(), Path::new("real_file.txt")).unwrap();
    assert!(matches!(file, Root::File(_)));
    let sub = root_of(dir.path(), Path::new("real_dir")).unwrap();
    assert!(matches!(sub, Root::Dir(_)));
    let linked_file = root_of(dir.path(), Path::new("lnk_file.txt")).unwrap();
    assert!(matches!(linked_file, Root::File(_)));
    let linked_dir = root_of(dir.path(), Path::new("lnk_dir")).unwrap();
    assert!(matches!(linked_dir, Root::Dir(_)));
    let dangling = root_of(dir.path(), Path::new("dangling")).unwrap();
    assert!(matches!(dangling, Root::File(_)));
    let missing = root_of(dir.path(), Path::new("no_such_path.txt")).unwrap_err();
    assert_eq!(missing.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn errors_without_a_path_blame_the_root() {
    let root = DirRoot {
        walk: PathBuf::from("w"),
        show: PathBuf::from("s"),
    };
    let denied = || ignore::Error::Io(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
    let cases = [
        (ignore::Error::Partial(vec![denied()]), "Permission denied"),
        (
            ignore::Error::WithLineNumber {
                line: 3,
                err: Box::new(denied()),
            },
            "Permission denied",
        ),
        (
            ignore::Error::WithDepth {
                depth: 2,
                err: Box::new(denied()),
            },
            "Permission denied",
        ),
        (
            ignore::Error::Loop {
                ancestor: PathBuf::from("a"),
                child: PathBuf::from("c"),
            },
            "File system loop found: c points to an ancestor a",
        ),
        (denied(), "Permission denied"),
        (
            ignore::Error::Glob {
                glob: Some("[".to_owned()),
                err: "boom".to_owned(),
            },
            "error parsing glob '[': boom",
        ),
        (
            ignore::Error::Glob {
                glob: None,
                err: "boom".to_owned(),
            },
            "boom",
        ),
        (
            ignore::Error::UnrecognizedFileType("zzz".to_owned()),
            "unrecognized file type: zzz",
        ),
        (
            ignore::Error::InvalidDefinition,
            "invalid definition (format is type:glob, e.g., html:*.html)",
        ),
    ];
    for (error, message) in cases {
        let failure = walk_error(&root, error);
        assert_eq!(failure.display, PathBuf::from("s"));
        assert_eq!(failure.message, message);
    }
}

#[test]
fn io_message_speaks_gnu() {
    assert_eq!(
        io_message(&std::io::Error::from(std::io::ErrorKind::NotFound)),
        "No such file or directory"
    );
    assert_eq!(
        io_message(&std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
        "Permission denied"
    );
    assert_eq!(
        io_message(&std::io::Error::from(std::io::ErrorKind::IsADirectory)),
        "Is a directory"
    );
    assert_eq!(io_message(&std::io::Error::other("boom")), "boom");
}

#[test]
fn the_walk_yields_its_first_entry_before_it_reads_the_root() {
    let dir = tree(&BTreeMap::from([("a/x.txt", "x"), ("b/", "")]));
    let root = dir_root(dir.path(), ".");
    let mut walked = walk(dir.path(), &root, None);
    let first = walked.next().unwrap().unwrap();
    assert_eq!(first.depth, 0);
    fs::write(dir.path().join("b/new.txt"), "new").unwrap();
    let rest: Vec<_> = walked.collect();
    let names: Vec<PathBuf> = rest
        .iter()
        .filter_map(|result| result.as_ref().ok())
        .map(|found| found.display.clone())
        .collect();
    assert!(names.contains(&dir.path().join("b/new.txt")), "{names:?}");
}
