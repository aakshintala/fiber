//! Tests beside [`super::run`]: listing what the walk finds.
//!
//! Expression primaries arrive with the next task; here any expression
//! token hands the call to the system tool before anything is written.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};

use super::{Outcome, run};

fn tree(files: &BTreeMap<&str, &str>) -> fakes::TempDir {
    let dir = fakes::TempDir::new("fiber-search-find");
    for (path, contents) in files {
        let full = dir.path().join(path);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&full, contents).unwrap();
    }
    dir
}

fn listing(dir: &fakes::TempDir, args: &[&str]) -> (i32, String, String) {
    let owned: Vec<OsString> = args.iter().map(OsString::from).collect();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = match run(dir.path(), &owned, &mut stdout, &mut stderr) {
        Outcome::Done(code) => code,
        Outcome::Fallback => panic!("{args:?} fell back"),
    };
    (
        code,
        String::from_utf8(stdout).unwrap(),
        String::from_utf8(stderr).unwrap(),
    )
}

#[test]
fn a_tree_lists_sorted_dot_prefixed_paths() {
    let dir = tree(&BTreeMap::from([
        ("a_hay.txt", "a"),
        ("sub/b_hay.txt", "b"),
    ]));
    let (code, stdout, stderr) = listing(&dir, &["."]);
    assert_eq!(code, 0);
    assert_eq!(stdout, ".\n./a_hay.txt\n./sub\n./sub/b_hay.txt\n");
    assert_eq!(stderr, "");
}

#[test]
fn no_path_lists_the_working_directory() {
    let dir = tree(&BTreeMap::from([("a_hay.txt", "a")]));
    let (code, stdout, _) = listing(&dir, &[]);
    assert_eq!(code, 0);
    assert_eq!(stdout, ".\n./a_hay.txt\n");
}

#[test]
fn ignored_and_version_control_paths_stay_hidden_files_show() {
    let dir = tree(&BTreeMap::from([
        (".gitignore", "skipped_dir/\n"),
        ("skipped_dir/hay.txt", "hay"),
        (".git/HEAD", "ref"),
        (".hid_needle.txt", "hay"),
        ("kept_hay.txt", "hay"),
    ]));
    let (code, stdout, _) = listing(&dir, &["."]);
    assert_eq!(code, 0);
    assert!(!stdout.contains("skipped_dir"), "{stdout}");
    assert!(
        !stdout
            .lines()
            .any(|line| line == "./.git" || line.starts_with("./.git/")),
        "{stdout}"
    );
    assert!(stdout.contains("./.hid_needle.txt"), "{stdout}");
    assert!(stdout.contains("./kept_hay.txt"), "{stdout}");
}

#[test]
fn a_named_ignored_path_is_always_visited() {
    let dir = tree(&BTreeMap::from([
        (".gitignore", "skipped_dir/\n"),
        ("skipped_dir/hay.txt", "hay"),
    ]));
    let (code, stdout, _) = listing(&dir, &["skipped_dir"]);
    assert_eq!(code, 0);
    assert_eq!(stdout, "skipped_dir\nskipped_dir/hay.txt\n");
}

#[test]
fn a_named_file_prints_as_given() {
    let dir = tree(&BTreeMap::from([("sub/a_hay.txt", "a")]));
    let (code, stdout, _) = listing(&dir, &["sub/a_hay.txt"]);
    assert_eq!(code, 0);
    assert_eq!(stdout, "sub/a_hay.txt\n");
}

#[test]
fn a_missing_start_path_complains_and_the_rest_still_lists() {
    let dir = tree(&BTreeMap::from([("a_hay.txt", "a")]));
    let (code, stdout, stderr) = listing(&dir, &["no_such_root", "."]);
    assert_eq!(code, 2);
    assert_eq!(stderr, "find: 'no_such_root': No such file or directory\n");
    assert_eq!(stdout, ".\n./a_hay.txt\n");
}

#[test]
fn an_unreadable_directory_complains_and_the_walk_continues() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tree(&BTreeMap::from([
        ("locked_dir/hay.txt", "hay"),
        ("open_hay.txt", "hay"),
    ]));
    let locked = dir.path().join("locked_dir");
    let kept = fs::metadata(&locked).unwrap().permissions();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let (code, stdout, stderr) = listing(&dir, &["."]);
    fs::set_permissions(&locked, kept).unwrap();
    assert_eq!(code, 2);
    assert!(stderr.contains("locked_dir"), "{stderr}");
    assert!(stderr.contains("Permission denied"), "{stderr}");
    assert!(stdout.contains("./open_hay.txt"), "{stdout}");
}

#[test]
fn an_expression_falls_back_before_anything_is_written() {
    let dir = tree(&BTreeMap::from([("a_hay.txt", "a")]));
    for args in [
        &[".", "-name", "x"][..],
        &["-name", "x"],
        &["-exec", "true", ";"],
    ] {
        let owned: Vec<OsString> = args.iter().map(OsString::from).collect();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        match run(dir.path(), &owned, &mut stdout, &mut stderr) {
            Outcome::Fallback => {}
            Outcome::Done(code) => panic!("{args:?} ran builtin with {code}"),
        }
        assert!(stdout.is_empty(), "{args:?}");
        assert!(stderr.is_empty(), "{args:?}");
    }
}

/// A writer that fails every write and counts them.
struct Fail {
    writes: usize,
}

impl Write for Fail {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        self.writes += 1;
        Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_closed_pipe_stops_the_run_quietly() {
    let dir = tree(&BTreeMap::from([("a_hay.txt", "a")]));
    let owned: Vec<OsString> = [OsString::from("."), OsString::from("a_hay.txt")].to_vec();
    let mut stdout = Fail { writes: 0 };
    let mut stderr = Vec::new();
    let code = match run(dir.path(), &owned, &mut stdout, &mut stderr) {
        Outcome::Done(code) => code,
        Outcome::Fallback => panic!("fell back"),
    };
    assert_eq!(code, 0);
    // The first write fails, the ending is dropped, and the broken pipe
    // stops the second root before it writes: one write, nothing else.
    assert_eq!(stdout.writes, 1);
    assert!(stderr.is_empty());
}
