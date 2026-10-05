//! Tests beside [`super::run`]: listing what the walk finds, and the
//! expression that filters it.

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
fn an_unknown_primary_falls_back_before_anything_is_written() {
    let dir = tree(&BTreeMap::from([("a_hay.txt", "a")]));
    for args in [
        &[".", "-nousuch"][..],
        &["-exec", "true", ";"],
        &[".", "-o"],
        &[".", "!"],
        &[".", "("],
        &[".", "-print"],
        &[".", "-delete"],
        &[".", "-empty"],
        &[".", "-a"],
        &[".", "-not"],
        &[".", "-type", "x"],
        &[".", "-name", "x", "extra_path"],
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

#[test]
fn a_closed_pipe_stops_the_walk_before_a_later_error() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tree(&BTreeMap::from([
        ("a_hay.txt", "a"),
        ("zzz_locked/hay.txt", "hay"),
    ]));
    let locked = dir.path().join("zzz_locked");
    let kept = fs::metadata(&locked).unwrap().permissions();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let owned: Vec<OsString> = vec![OsString::from(".")];
    let mut stdout = Fail { writes: 0 };
    let mut stderr = Vec::new();
    let code = match run(dir.path(), &owned, &mut stdout, &mut stderr) {
        Outcome::Done(code) => code,
        Outcome::Fallback => panic!("fell back"),
    };
    fs::set_permissions(&locked, kept).unwrap();
    // The root's write fails, so the walk stops before the locked
    // directory's error: ruling 13 on #298 keeps the exit it had so far
    // and stays quiet instead of failing with permission denied.
    assert_eq!(code, 0);
    assert_eq!(stdout.writes, 1);
    assert!(stderr.is_empty());
}

/// A pipe that closes on flush: writes go nowhere, the flush reports the
/// closed pipe, so the run after the loop reads as broken with nothing
/// printed.
struct FlushClosed;

impl Write for FlushClosed {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
    }
}

#[test]
fn a_closed_pipe_flush_with_no_match_stays_quiet_despite_skipped_directories() {
    let dir = tree(&BTreeMap::from([
        (".gitignore", "skipped_dir/\n"),
        ("skipped_dir/hay.txt", "hay"),
        ("kept_hay.txt", "hay"),
    ]));
    let owned: Vec<OsString> = [
        OsString::from("."),
        OsString::from("-name"),
        OsString::from("no_such_name"),
    ]
    .to_vec();
    let mut stdout = FlushClosed;
    let mut stderr = Vec::new();
    let code = match run(dir.path(), &owned, &mut stdout, &mut stderr) {
        Outcome::Done(code) => code,
        Outcome::Fallback => panic!("fell back"),
    };
    // Ruling 13 on #298: a broken pipe ends the process quietly with
    // the exit it had so far. The flush breaks the pipe with nothing
    // printed, so the skipped-directory notice never prints.
    assert_eq!(code, 0);
    assert!(
        stderr.is_empty(),
        "stderr: {}",
        String::from_utf8_lossy(&stderr)
    );
}

fn filtered(dir: &fakes::TempDir, args: &[&str]) -> (i32, Vec<String>, String) {
    let (code, stdout, stderr) = listing(dir, args);
    let lines = stdout.lines().map(str::to_owned).collect();
    (code, lines, stderr)
}

#[test]
fn name_matches_base_names() {
    let dir = tree(&BTreeMap::from([
        ("a_needle.txt", "a"),
        ("b_hay.txt", "b"),
        ("sub/c_needle.txt", "c"),
    ]));
    let (code, lines, stderr) = filtered(&dir, &[".", "-name", "*needle*"]);
    assert_eq!(code, 0);
    assert_eq!(lines, ["./a_needle.txt", "./sub/c_needle.txt"]);
    assert_eq!(stderr, "");
}

#[test]
fn name_is_case_sensitive_iname_is_not() {
    let dir = tree(&BTreeMap::from([("a_Needle.txt", "a")]));
    let (_, sensitive, _) = filtered(&dir, &[".", "-name", "*needle*"]);
    assert!(sensitive.is_empty());
    let (code, insensitive, _) = filtered(&dir, &[".", "-iname", "*needle*"]);
    assert_eq!(code, 0);
    assert_eq!(insensitive, ["./a_Needle.txt"]);
}

#[test]
fn name_supports_question_marks_and_classes() {
    let dir = tree(&BTreeMap::from([
        ("a_needle.txt", "a"),
        ("b_needle.txt", "b"),
        ("c_hay.txt", "c"),
    ]));
    let (_, question, _) = filtered(&dir, &[".", "-name", "?_needle.txt"]);
    assert_eq!(question, ["./a_needle.txt", "./b_needle.txt"]);
    let (_, class, _) = filtered(&dir, &[".", "-name", "[ab]_needle.txt"]);
    assert_eq!(class, ["./a_needle.txt", "./b_needle.txt"]);
    let (_, negated, _) = filtered(&dir, &[".", "-name", "[!ab]*"]);
    // The root's own name is `.`, which the class accepts.
    assert_eq!(negated, [".", "./c_hay.txt"]);
}

#[test]
fn path_matches_the_printed_path() {
    let dir = tree(&BTreeMap::from([
        ("a_needle.txt", "a"),
        ("needle_dir/x_hay.txt", "x"),
        ("sub/c_needle.txt", "c"),
    ]));
    let (code, lines, _) = filtered(&dir, &[".", "-path", "*needle*"]);
    assert_eq!(code, 0);
    assert_eq!(
        lines,
        [
            "./a_needle.txt",
            "./needle_dir",
            "./needle_dir/x_hay.txt",
            "./sub/c_needle.txt"
        ]
    );
    let (_, scoped, _) = filtered(&dir, &[".", "-path", "./sub/*.txt"]);
    assert_eq!(scoped, ["./sub/c_needle.txt"]);
}

#[test]
fn type_sorts_files_directories_and_links() {
    let dir = tree(&BTreeMap::from([
        ("a_hay.txt", "a"),
        ("sub/b_hay.txt", "b"),
    ]));
    std::os::unix::fs::symlink("a_hay.txt", dir.path().join("lnk_hay.txt")).unwrap();
    let (_, files, _) = filtered(&dir, &[".", "-type", "f"]);
    assert_eq!(files, ["./a_hay.txt", "./sub/b_hay.txt"]);
    let (_, dirs, _) = filtered(&dir, &[".", "-type", "d"]);
    assert_eq!(dirs, [".", "./sub"]);
    let (_, links, _) = filtered(&dir, &[".", "-type", "l"]);
    assert_eq!(links, ["./lnk_hay.txt"]);
    let (_, impossible, _) = filtered(&dir, &[".", "-type", "f", "-type", "d"]);
    assert!(impossible.is_empty());
}

#[test]
fn type_leaves_other_kinds_out() {
    use std::os::unix::net::UnixListener;
    let dir = fakes::TempDir::new("fiber-search-socket");
    let socket = dir.path().join("sock");
    let _held = UnixListener::bind(&socket).unwrap();
    let meta = fs::symlink_metadata(&socket).unwrap();
    assert_eq!(super::kind_of(&meta.file_type()), None);
}

#[test]
fn a_named_directory_link_keeps_its_link_type() {
    let dir = tree(&BTreeMap::from([("sub/a_hay.txt", "a")]));
    std::os::unix::fs::symlink("sub", dir.path().join("lnk")).unwrap();
    // GNU tests the root itself: a link to a directory is a link.
    let (_, links, _) = filtered(&dir, &["lnk", "-type", "l"]);
    assert_eq!(links, ["lnk"]);
    let (_, dirs, _) = filtered(&dir, &["lnk", "-type", "d"]);
    assert!(dirs.is_empty(), "{dirs:?}");
    // Without a test the root still lists itself and below.
    let (code, lines, _) = filtered(&dir, &["lnk"]);
    assert_eq!(code, 0);
    assert_eq!(lines, ["lnk", "lnk/a_hay.txt"]);
}

#[test]
fn posix_classes_match() {
    let dir = tree(&BTreeMap::from([
        ("a_needle.txt", "a"),
        ("1_needle.txt", "1"),
    ]));
    let (code, lines, _) = filtered(&dir, &[".", "-name", "[[:alpha:]]_needle.txt"]);
    assert_eq!(code, 0);
    assert_eq!(lines, ["./a_needle.txt"]);
}

#[test]
fn a_write_failure_is_an_error() {
    struct Refused;
    impl Write for Refused {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("no space"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("no space"))
        }
    }
    let dir = tree(&BTreeMap::from([("a_hay.txt", "a")]));
    let owned: Vec<OsString> = [OsString::from(".")].to_vec();
    let mut refused = Refused;
    let mut stderr = Vec::new();
    let code = match run(dir.path(), &owned, &mut refused, &mut stderr) {
        Outcome::Done(code) => code,
        Outcome::Fallback => panic!("fell back"),
    };
    assert_eq!(code, 2);
    assert_eq!(stderr, b"find: writing output: no space\n");
}

#[test]
fn maxdepth_and_mindepth_bound_the_tree() {
    let dir = tree(&BTreeMap::from([("sub/deep/f_hay.txt", "hay")]));
    let (_, max_one, _) = filtered(&dir, &[".", "-maxdepth", "1"]);
    assert_eq!(max_one, [".", "./sub"]);
    let (_, max_zero, _) = filtered(&dir, &[".", "-maxdepth", "0"]);
    assert_eq!(max_zero, ["."]);
    let (_, min_one, _) = filtered(&dir, &[".", "-mindepth", "1"]);
    assert!(!min_one.contains(&".".to_owned()), "{min_one:?}");
    assert!(
        min_one.contains(&"./sub/deep/f_hay.txt".to_owned()),
        "{min_one:?}"
    );
    let (_, min_two, _) = filtered(&dir, &[".", "-mindepth", "2"]);
    assert_eq!(min_two, ["./sub/deep", "./sub/deep/f_hay.txt"]);
}

#[test]
fn a_later_depth_limit_wins() {
    let dir = tree(&BTreeMap::from([("sub/f_hay.txt", "hay")]));
    let (_, lines, _) = filtered(&dir, &[".", "-maxdepth", "0", "-maxdepth", "1"]);
    assert_eq!(lines, [".", "./sub"]);
}

#[test]
fn newer_compares_modification_times_strictly() {
    use std::time::{Duration, SystemTime};
    let dir = tree(&BTreeMap::from([
        ("old_hay.txt", "old"),
        ("same_hay.txt", "same"),
        ("new_hay.txt", "new"),
        ("mark_ref.txt", "ref"),
    ]));
    // Fixed stamps, not the clock: the order is what the test needs.
    let moment = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let stamp = |path: &std::path::Path, at: SystemTime| {
        fs::OpenOptions::new()
            .read(true)
            .open(path)
            .unwrap()
            .set_modified(at)
            .unwrap();
    };
    stamp(
        &dir.path().join("old_hay.txt"),
        moment - Duration::from_secs(60),
    );
    stamp(&dir.path().join("same_hay.txt"), moment);
    stamp(&dir.path().join("mark_ref.txt"), moment);
    stamp(
        &dir.path().join("new_hay.txt"),
        moment + Duration::from_secs(60),
    );
    // The directory itself is as old as the reference, so strict newness
    // leaves it out like the equal file.
    stamp(dir.path(), moment);
    let (code, lines, _) = filtered(&dir, &[".", "-newer", "mark_ref.txt"]);
    assert_eq!(code, 0);
    assert_eq!(lines, ["./new_hay.txt"]);
}

#[test]
fn a_missing_newer_reference_fails_with_no_listing() {
    let dir = tree(&BTreeMap::from([("a_hay.txt", "a")]));
    let (code, stdout, stderr) = listing(&dir, &[".", "-newer", "no_such_ref.txt"]);
    assert_eq!(code, 2);
    assert_eq!(stdout, "");
    assert_eq!(
        stderr,
        "find: 'no_such_ref.txt': No such file or directory\n"
    );
}

#[test]
fn adjacent_tests_are_anded() {
    let dir = tree(&BTreeMap::from([
        ("needle_dir/x_hay.txt", "x"),
        ("a_needle.txt", "a"),
    ]));
    let (code, lines, _) = filtered(&dir, &[".", "-name", "*needle*", "-type", "d"]);
    assert_eq!(code, 0);
    assert_eq!(lines, ["./needle_dir"]);
}

#[test]
fn a_missing_argument_is_a_usage_error() {
    let dir = tree(&BTreeMap::from([("a_hay.txt", "a")]));
    for (flag, message) in [
        ("-name", "find: missing argument to '-name'"),
        ("-iname", "find: missing argument to '-iname'"),
        ("-path", "find: missing argument to '-path'"),
        ("-type", "find: missing argument to '-type'"),
        ("-maxdepth", "find: missing argument to '-maxdepth'"),
        ("-mindepth", "find: missing argument to '-mindepth'"),
        ("-newer", "find: missing argument to '-newer'"),
    ] {
        let (code, stdout, stderr) = listing(&dir, &[flag]);
        assert_eq!(code, 2, "{flag}");
        assert_eq!(stdout, "", "{flag}");
        assert_eq!(stderr, format!("{message}\n"), "{flag}");
    }
}

#[test]
fn a_bad_depth_is_a_usage_error() {
    let dir = tree(&BTreeMap::from([("a_hay.txt", "a")]));
    for (flag, value) in [
        ("-maxdepth", "x"),
        ("-maxdepth", "-1"),
        ("-mindepth", ""),
        ("-mindepth", "1.5"),
    ] {
        let (code, stdout, stderr) = listing(&dir, &[flag, value]);
        assert_eq!(code, 2, "{flag} {value}");
        assert_eq!(stdout, "", "{flag} {value}");
        assert_eq!(
            stderr,
            format!("find: invalid argument '{value}' for '{flag}'\n"),
            "{flag} {value}"
        );
    }
}

#[test]
fn a_root_file_passes_the_tests_like_any_entry() {
    let dir = tree(&BTreeMap::from([("a_hay.txt", "a")]));
    let (code, lines, _) = filtered(&dir, &["a_hay.txt", "-type", "f"]);
    assert_eq!(code, 0);
    assert_eq!(lines, ["a_hay.txt"]);
    let (_, hidden, _) = filtered(&dir, &["a_hay.txt", "-type", "d"]);
    assert!(hidden.is_empty());
}

#[test]
fn a_root_directory_below_mindepth_still_walks() {
    let dir = tree(&BTreeMap::from([("sub/f_hay.txt", "hay")]));
    let (code, lines, _) = filtered(&dir, &[".", "-mindepth", "1"]);
    assert_eq!(code, 0);
    assert_eq!(lines, ["./sub", "./sub/f_hay.txt"]);
}

#[test]
fn nothing_printed_with_skipped_directories_reports_them() {
    let dir = tree(&BTreeMap::from([
        (".gitignore", "skipped_dir/\n"),
        ("skipped_dir/hay.txt", "hay"),
        ("kept_hay.txt", "hay"),
    ]));
    let (code, stdout, stderr) = listing(&dir, &[".", "-name", "no_such_name"]);
    assert_eq!(code, 0);
    assert_eq!(stdout, "");
    assert_eq!(
        stderr,
        "find: no match. Skipped ignored directories: skipped_dir/. Search one by name, such as `find skipped_dir`.\n"
    );
}

#[test]
fn a_match_prints_no_notice_and_an_error_prints_none_either() {
    let dir = tree(&BTreeMap::from([
        (".gitignore", "skipped_dir/\n"),
        ("skipped_dir/hay.txt", "hay"),
        ("kept_hay.txt", "hay"),
    ]));
    let (_, _, matched_stderr) = listing(&dir, &[".", "-name", "kept*"]);
    assert_eq!(matched_stderr, "");
    let (code, _, failed_stderr) = listing(&dir, &["no_such_root", "-name", "no_such_name"]);
    assert_eq!(code, 2);
    assert_eq!(
        failed_stderr,
        "find: 'no_such_root': No such file or directory\n"
    );
}

#[test]
fn globs_match_bytes() {
    use super::glob_match;
    let cases: &[(&[u8], &[u8], bool, bool)] = &[
        (b"", b"", false, true),
        (b"", b"a", false, false),
        (b"a", b"a", false, true),
        (b"a", b"b", false, false),
        (b"*", b"", false, true),
        (b"*", b"anything at all", false, true),
        (b"*.txt", b"a.txt", false, true),
        (b"*.txt", b"a.txtx", false, false),
        (b"a*b", b"aXYZb", false, true),
        (b"a*b", b"ab", false, true),
        (b"a*b", b"aXbY", false, false),
        (b"a?b", b"aXb", false, true),
        (b"a?b", b"ab", false, false),
        (b"a?b", b"aXYb", false, false),
        (b"?", b"", false, false),
        (b"**", b"x", false, true),
        (b"[abc]", b"b", false, true),
        (b"[abc]", b"d", false, false),
        (b"[a-c]", b"b", false, true),
        (b"[^a-c]", b"d", false, true),
        (b"[^a-c]", b"b", false, false),
        (b"[!a-c]", b"d", false, true),
        (b"[]a]", b"]", false, true),
        (b"[]a]", b"a", false, true),
        (b"[^]a]", b"]", false, false),
        (b"[^]a]", b"b", false, true),
        (b"[a\\]]", b"a", false, true),
        (b"[a\\]]", b"]", false, true),
        (b"[a\\]]", b"\\", false, false),
        (b"[a-c-e]", b"-", false, true),
        (b"a\\*b", b"a*b", false, true),
        (b"a\\*b", b"aXb", false, false),
        (b"a\\\\b", b"a\\b", false, true),
        (b"a\\", b"a\\", false, true),
        // An unclosed `[` is a literal: only `[` itself matches one, and a
        // trailing `-` is a literal member, not a range.
        (b"[abc", b"[abc", false, true),
        (b"[abc", b"x", false, false),
        (b"[a", b"xa", false, false),
        (b"[a-]", b"a", false, true),
        (b"[a-]", b"-", false, true),
        (b"[a-]", b"b", false, false),
        (b"ABC", b"abc", false, false),
        (b"ABC", b"abc", true, true),
        (b"*.TXT", b"a.txt", true, true),
        (b"[a-z]", b"A", false, false),
        (b"[a-z]", b"A", true, true),
        (b"[[:alpha:]]", b"b", false, true),
        (b"[[:alpha:]]", b"1", false, false),
        // Every POSIX class matches its own members: one case per arm.
        (b"[[:alnum:]]", b"5", false, true),
        (b"[[:blank:]]", b" ", false, true),
        (b"[[:cntrl:]]", b"\x01", false, true),
        (b"[[:graph:]]", b"!", false, true),
        (b"[[:graph:]]", b" ", false, false),
        (b"[[:lower:]]", b"q", false, true),
        (b"[[:print:]]", b" ", false, true),
        (b"[[:punct:]]", b"!", false, true),
        (b"[[:punct:]]", b"a", false, false),
        (b"[[:space:]]", b"\n", false, true),
        (b"[[:upper:]]", b"Q", false, true),
        (b"[[:upper:]]", b"q", false, false),
        (b"[[:alpha:]][[:digit:]]", b"a1", false, true),
        (b"[[:alpha:]][[:digit:]]", b"ab", false, false),
        (b"[a[:digit:]]", b"5", false, true),
        (b"[a[:digit:]]", b"b", false, false),
        (b"[^[:digit:]]", b"a", false, true),
        (b"[^[:digit:]]", b"5", false, false),
        (b"[[:xdigit:]]", b"f", false, true),
        (b"[[:xdigit:]]", b"g", false, false),
        (b"[[:alpha:]-]", b"-", false, true),
        // An unknown class leaves the `[` an ordinary member, as in
        // bash: the class reads `[`, `:`, `f`, `o` with a literal `]`.
        (b"[[:foo:]]", b"[]", false, true),
        (b"[[:foo:]]", b"f]", false, true),
        (b"[[:foo:]]", b"x", false, false),
    ];
    for (pattern, text, ignore_case, expected) in cases {
        assert_eq!(
            glob_match(pattern, text, *ignore_case),
            *expected,
            "{:?} vs {:?} (case: {ignore_case})",
            String::from_utf8_lossy(pattern),
            String::from_utf8_lossy(text),
        );
    }
}
