//! Tests beside [`super::run`]: output formats, exit codes and declines.
//!
//! Each output format also runs against the system grep on the same corpus
//! where that grep is GNU; elsewhere the pinned literals below carry it.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Cursor, Read, Write};
use std::os::unix::ffi::OsStringExt;

use super::{Outcome, run};
use fakes::Deadline;

/// A corpus with byte-exact contents, for binary and invalid UTF-8 too.
fn bytes_tree(files: &BTreeMap<&str, &[u8]>) -> fakes::TempDir {
    let dir = fakes::TempDir::new("fiber-search-grep");
    for (path, contents) in files {
        let full = dir.path().join(path);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&full, contents).unwrap();
    }
    dir
}

fn text_tree(files: &BTreeMap<&str, &str>) -> fakes::TempDir {
    bytes_tree(
        &files
            .iter()
            .map(|(path, contents)| (*path, contents.as_bytes()))
            .collect(),
    )
}

/// Runs the built-in search: the exit code, standard output and standard
/// error as bytes.
fn search(dir: &fakes::TempDir, args: &[&str], stdin: &[u8]) -> (i32, Vec<u8>, Vec<u8>) {
    let owned: Vec<OsString> = args.iter().map(OsString::from).collect();
    let mut input = Cursor::new(stdin.to_vec());
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = match run(dir.path(), &owned, &mut input, &mut stdout, &mut stderr) {
        Outcome::Done(code) => code,
        Outcome::Fallback => panic!("{args:?} fell back"),
    };
    (code, stdout, stderr)
}

/// A bounded generator of matches: `needle` lines up to `limit` bytes,
/// counting what the search actually read.
struct Generator {
    /// Bytes handed out so far.
    read: usize,
    /// Bytes handed out in total before the end.
    limit: usize,
}

impl Read for Generator {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.read >= self.limit {
            return Ok(0);
        }
        let line = b"needle\n";
        let mut given = 0;
        while given + line.len() <= buf.len() && self.read + given + line.len() <= self.limit {
            buf[given..given + line.len()].copy_from_slice(line);
            given += line.len();
        }
        if given == 0 {
            let take = buf.len().min(line.len()).min(self.limit - self.read);
            buf[..take].copy_from_slice(&line[..take]);
            given = take;
        }
        self.read += given;
        Ok(given)
    }
}

/// A filter that fails every read.
struct Failing;

impl Read for Failing {
    fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::other("boom"))
    }
}

/// A closed pipe: every write fails, and counts itself.
struct Closed {
    writes: usize,
}

impl Write for Closed {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        self.writes += 1;
        Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A failed disk: every write and flush fails with a real error.
struct Refused;

impl Write for Refused {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("no space"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("no space"))
    }
}

fn text(dir: &fakes::TempDir, args: &[&str], stdin: &str) -> (i32, String, String) {
    let (code, stdout, stderr) = search(dir, args, stdin.as_bytes());
    (
        code,
        String::from_utf8(stdout).unwrap(),
        String::from_utf8(stderr).unwrap(),
    )
}

#[test]
fn a_filter_reads_standard_input() {
    let dir = text_tree(&BTreeMap::from([("a.txt", "a\n")]));
    let (code, stdout, stderr) = text(&dir, &["b"], "a\nb\nc\n");
    assert_eq!(code, 0);
    assert_eq!(stdout, "b\n");
    assert_eq!(stderr, "");
    let (code, stdout, _) = text(&dir, &["z"], "a\nb\nc\n");
    assert_eq!(code, 1);
    assert_eq!(stdout, "");
}

#[test]
fn recursive_search_skips_what_the_walk_skips() {
    let dir = text_tree(&BTreeMap::from([
        (".gitignore", "skipped_dir/\n"),
        ("skipped_dir/needle.txt", "needle\n"),
        (".git/needle.txt", "needle\n"),
        (".hid_needle.txt", "needle\n"),
        ("kept/needle.txt", "needle\n"),
    ]));
    let (code, stdout, _) = text(&dir, &["-r", "needle", "."], "");
    assert_eq!(code, 0);
    assert_eq!(
        stdout,
        "./.hid_needle.txt:needle\n./kept/needle.txt:needle\n"
    );
}

#[test]
fn a_named_ignored_path_is_always_searched() {
    let dir = text_tree(&BTreeMap::from([
        (".gitignore", "skipped_dir/\n"),
        ("skipped_dir/needle.txt", "needle\n"),
    ]));
    let (code, stdout, _) = text(&dir, &["-r", "needle", "skipped_dir"], "");
    assert_eq!(code, 0);
    assert_eq!(stdout, "skipped_dir/needle.txt:needle\n");
}

#[test]
fn the_path_shows_for_many_files_and_named_directories() {
    let dir = text_tree(&BTreeMap::from([
        ("a.txt", "needle\n"),
        ("b.txt", "needle\n"),
        ("sub/c.txt", "needle\n"),
    ]));
    let (_, single, _) = text(&dir, &["needle", "a.txt"], "");
    assert_eq!(single, "needle\n");
    let (_, pair, _) = text(&dir, &["needle", "a.txt", "b.txt"], "");
    assert_eq!(pair, "a.txt:needle\nb.txt:needle\n");
    let (_, walked, _) = text(&dir, &["-r", "needle", "sub"], "");
    assert_eq!(walked, "sub/c.txt:needle\n");
    let (_, single_recursive, _) = text(&dir, &["-r", "needle", "a.txt"], "");
    assert_eq!(single_recursive, "needle\n");
    let (_, root, _) = text(&dir, &["-r", "needle", "."], "");
    assert!(root.contains("./a.txt:needle\n"), "{root}");
}

#[test]
fn standard_input_labels_when_the_path_shows() {
    let dir = text_tree(&BTreeMap::from([("a.txt", "needle\n")]));
    let (_, both, _) = text(&dir, &["needle", "-", "a.txt"], "needle\n");
    assert_eq!(both, "(standard input):needle\na.txt:needle\n");
}

#[test]
fn line_numbers_count_and_files_with_matches() {
    let dir = text_tree(&BTreeMap::from([
        ("a.txt", "x\nneedle\n"),
        ("b.txt", "plain\n"),
    ]));
    let (_, numbered, _) = text(&dir, &["-n", "needle", "a.txt", "b.txt"], "");
    assert_eq!(numbered, "a.txt:2:needle\n");
    let (code, counts, _) = text(&dir, &["-c", "needle", "a.txt", "b.txt"], "");
    assert_eq!(code, 0);
    assert_eq!(counts, "a.txt:1\nb.txt:0\n");
    let (_, bare, _) = text(&dir, &["-c", "needle", "a.txt"], "");
    assert_eq!(bare, "1\n");
    let (code, listed, _) = text(&dir, &["-l", "needle", "a.txt", "b.txt"], "");
    assert_eq!(code, 0);
    assert_eq!(listed, "a.txt\n");
    let (_, single_listed, _) = text(&dir, &["-l", "needle", "a.txt"], "");
    assert_eq!(single_listed, "a.txt\n");
}

#[test]
fn invert_match_counts_what_does_not_match() {
    let dir = text_tree(&BTreeMap::from([("a.txt", "needle\nplain\n")]));
    let (_, inverted, _) = text(&dir, &["-v", "needle", "a.txt"], "");
    assert_eq!(inverted, "plain\n");
    let (code, counts, _) = text(&dir, &["-c", "-v", "needle", "a.txt"], "");
    assert_eq!(code, 0);
    assert_eq!(counts, "1\n");
}

#[test]
fn count_counts_every_match_in_the_file() {
    let dir = text_tree(&BTreeMap::from([("a.txt", "m1\nm2\n")]));
    let (code, counts, _) = text(&dir, &["-c", "m", "a.txt"], "");
    assert_eq!(code, 0);
    assert_eq!(counts, "2\n");
}

#[test]
fn count_without_a_match_exits_one() {
    // The count still prints, but nothing matched.
    let dir = text_tree(&BTreeMap::from([("a.txt", "needle\n")]));
    let (code, counts, _) = text(&dir, &["-c", "absent", "a.txt"], "");
    assert_eq!(code, 1);
    assert_eq!(counts, "0\n");
}

#[test]
fn count_with_context_prints_only_the_count() {
    // Context and group breaks never print under `-c`: the count is the
    // whole output.
    let dir = text_tree(&BTreeMap::from([("a.txt", "m1\nx\ny\nz\nm2\n")]));
    let (code, counts, _) = text(&dir, &["-c", "-C1", "m", "a.txt"], "");
    assert_eq!(code, 0);
    assert_eq!(counts, "2\n");
}

#[test]
fn case_word_extended_and_fixed_flags() {
    let dir = text_tree(&BTreeMap::from([(
        "a.txt",
        "Needle\nneedles\nNEEDLE\na+b\naab\n",
    )]));
    let (_, insensitive, _) = text(&dir, &["-i", "needle", "a.txt"], "");
    assert_eq!(insensitive, "Needle\nneedles\nNEEDLE\n");
    let (_, words, _) = text(&dir, &["-w", "needle", "a.txt"], "");
    assert_eq!(words, "");
    let (_, extended, _) = text(&dir, &["-E", "a+b", "a.txt"], "");
    assert_eq!(extended, "aab\n");
    let (_, fixed, _) = text(&dir, &["-F", "a+b", "a.txt"], "");
    assert_eq!(fixed, "a+b\n");
    let (_, basic, _) = text(&dir, &["a+b", "a.txt"], "");
    assert_eq!(basic, "a+b\n");
}

#[test]
fn context_groups_join_with_breaks() {
    let dir = text_tree(&BTreeMap::from([(
        "a.txt",
        "1\nneedle\n3\n4\n5\nneedle\n7\n",
    )]));
    let (_, after, _) = text(&dir, &["-A1", "needle", "a.txt"], "");
    assert_eq!(after, "needle\n3\n--\nneedle\n7\n");
    let (_, before, _) = text(&dir, &["-B1", "needle", "a.txt"], "");
    assert_eq!(before, "1\nneedle\n--\n5\nneedle\n");
    let (_, both, _) = text(&dir, &["-C1", "needle", "a.txt"], "");
    assert_eq!(both, "1\nneedle\n3\n--\n5\nneedle\n7\n");
    let (_, numbered, _) = text(&dir, &["-n", "-A1", "needle", "a.txt"], "");
    assert_eq!(numbered, "2:needle\n3-3\n--\n6:needle\n7-7\n");
}

#[test]
fn adjacent_context_merges_without_a_break() {
    let dir = text_tree(&BTreeMap::from([("a.txt", "needle\nmid\nneedle\n")]));
    let (_, merged, _) = text(&dir, &["-A1", "needle", "a.txt"], "");
    assert_eq!(merged, "needle\nmid\nneedle\n");
}

#[test]
fn invert_match_with_context() {
    let dir = text_tree(&BTreeMap::from([("a.txt", "needle\nplain\nother\n")]));
    let (_, inverted, _) = text(&dir, &["-v", "-A1", "needle", "a.txt"], "");
    assert_eq!(inverted, "plain\nother\n");
}

#[test]
fn include_and_exclude_filter_base_names() {
    let dir = text_tree(&BTreeMap::from([
        ("keep_needle.txt", "needle\n"),
        ("skip_needle.log", "needle\n"),
        ("sub/keep_too.txt", "needle\n"),
    ]));
    let (_, included, _) = text(&dir, &["-r", "--include=*.txt", "needle", "."], "");
    assert_eq!(
        included,
        "./keep_needle.txt:needle\n./sub/keep_too.txt:needle\n"
    );
    let (_, excluded, _) = text(&dir, &["-r", "--exclude=*.log", "needle", "."], "");
    assert_eq!(
        excluded,
        "./keep_needle.txt:needle\n./sub/keep_too.txt:needle\n"
    );
    // The last matching rule wins, on a named file too.
    let (_, named_out, _) = text(
        &dir,
        &[
            "--include=*.txt",
            "--exclude=*.txt",
            "needle",
            "keep_needle.txt",
        ],
        "",
    );
    assert_eq!(named_out, "");
}

#[test]
fn a_later_include_beats_an_earlier_exclude() {
    // As GNU reads the rules: the last matching one decides.
    let dir = text_tree(&BTreeMap::from([("a.txt", "needle\n")]));
    let (_, stdout, _) = text(
        &dir,
        &["-r", "--exclude=*.txt", "--include=*.txt", "needle", "."],
        "",
    );
    assert_eq!(stdout, "./a.txt:needle\n");
}

#[test]
fn a_later_exclude_beats_an_earlier_include() {
    let dir = text_tree(&BTreeMap::from([("a.txt", "needle\n")]));
    let (code, stdout, _) = text(
        &dir,
        &["-r", "--include=*.txt", "--exclude=*.txt", "needle", "."],
        "",
    );
    assert_eq!(code, 1);
    assert_eq!(stdout, "");
}

#[test]
fn a_file_no_rule_matches_is_skipped_when_an_include_exists() {
    // With an `--include` given, only matched files are searched.
    let dir = text_tree(&BTreeMap::from([
        ("a.txt", "needle\n"),
        ("b.log", "needle\n"),
    ]));
    let (_, stdout, _) = text(&dir, &["-r", "--include=*.txt", "needle", "."], "");
    assert_eq!(stdout, "./a.txt:needle\n");
}

#[test]
fn a_file_no_rule_matches_is_searched_without_an_include() {
    // With only `--exclude` given, unmatched files are still searched.
    let dir = text_tree(&BTreeMap::from([
        ("a.txt", "needle\n"),
        ("b.log", "needle\n"),
    ]));
    let (_, stdout, _) = text(&dir, &["-r", "--exclude=*.bak", "needle", "."], "");
    assert_eq!(stdout, "./a.txt:needle\n./b.log:needle\n");
}

#[test]
fn binary_files_skip_silently() {
    // As `grep -I`: a NUL byte in what is read skips the file, with no
    // message and no match.
    let dir = bytes_tree(&BTreeMap::from([(
        "a.bin",
        &b"needle\nplain\x00needle\n"[..],
    )]));
    let (code, stdout, stderr) = search(&dir, &["needle", "a.bin"], &[]);
    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
}

#[test]
fn invalid_utf8_searches_as_bytes() {
    let dir = bytes_tree(&BTreeMap::from([("a.txt", &b"needle \xff\nplain\n"[..])]));
    let (code, stdout, _) = search(&dir, &["needle", "a.txt"], &[]);
    assert_eq!(code, 0);
    assert_eq!(stdout, b"needle \xff\n");
}

#[test]
fn a_last_line_without_an_ending_matches() {
    let dir = bytes_tree(&BTreeMap::from([("a.txt", &b"plain\nneedle"[..])]));
    let (code, stdout, _) = search(&dir, &["needle", "a.txt"], &[]);
    assert_eq!(code, 0);
    assert_eq!(stdout, b"needle\n");
}

#[test]
fn an_empty_pattern_matches_every_line() {
    let dir = text_tree(&BTreeMap::from([("a.txt", "x\n")]));
    let (code, stdout, _) = text(&dir, &["", "a.txt"], "");
    assert_eq!(code, 0);
    assert_eq!(stdout, "x\n");
}

#[test]
fn missing_unreadable_and_directory_paths_complain() {
    use std::os::unix::fs::PermissionsExt;
    let dir = text_tree(&BTreeMap::from([
        ("a.txt", "needle\n"),
        ("sub/b.txt", "x\n"),
        ("locked.txt", "x\n"),
    ]));
    let locked = dir.path().join("locked.txt");
    let kept = fs::metadata(&locked).unwrap().permissions();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let (code, stdout, stderr) = text(
        &dir,
        &["needle", "nope.txt", "locked.txt", "sub", "a.txt"],
        "",
    );
    fs::set_permissions(&locked, kept).unwrap();
    assert_eq!(code, 2);
    assert_eq!(stdout, "a.txt:needle\n");
    assert_eq!(
        stderr,
        "grep: nope.txt: No such file or directory\n\
         grep: locked.txt: Permission denied\n\
         grep: sub: Is a directory\n"
    );
}

#[test]
fn exits_zero_one_and_two() {
    let dir = text_tree(&BTreeMap::from([("a.txt", "needle\n")]));
    assert_eq!(text(&dir, &["needle", "a.txt"], "").0, 0);
    assert_eq!(text(&dir, &["absent", "a.txt"], "").0, 1);
    assert_eq!(text(&dir, &["needle", "nope.txt"], "").0, 2);
}

#[test]
fn a_failing_filter_is_an_error() {
    // A filter that cannot be read fails the run, as a file that cannot
    // be opened does: the filter's failure reaches the exit code.
    let dir = text_tree(&BTreeMap::from([("a.txt", "x\n")]));
    let owned: Vec<OsString> = [OsString::from("needle")].to_vec();
    let mut failing = Failing;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = match run(dir.path(), &owned, &mut failing, &mut stdout, &mut stderr) {
        Outcome::Done(code) => code,
        Outcome::Fallback => panic!("fell back"),
    };
    assert_eq!(code, 2);
    assert!(stdout.is_empty());
    assert_eq!(stderr, b"grep: (standard input): boom\n");
}

#[test]
fn an_unreadable_walked_file_complains_and_fails_the_run() {
    // A file the walk finds but cannot open fails the run, even when
    // another file matched: the failure reaches the exit code.
    use std::os::unix::fs::PermissionsExt;
    let dir = text_tree(&BTreeMap::from([
        ("sub/ok.txt", "needle\n"),
        ("sub/locked.txt", "needle\n"),
    ]));
    let locked = dir.path().join("sub/locked.txt");
    let kept = fs::metadata(&locked).unwrap().permissions();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let (code, stdout, stderr) = text(&dir, &["-r", "needle", "sub"], "");
    fs::set_permissions(&locked, kept).unwrap();
    assert_eq!(code, 2);
    assert_eq!(stdout, "sub/ok.txt:needle\n");
    assert_eq!(stderr, "grep: sub/locked.txt: Permission denied\n");
}

#[test]
fn nothing_found_with_skipped_directories_reports_them() {
    let dir = text_tree(&BTreeMap::from([
        (".gitignore", "skipped_dir/\n"),
        ("skipped_dir/needle.txt", "x\n"),
        ("kept.txt", "x\n"),
    ]));
    let (code, stdout, stderr) = text(&dir, &["-r", "needle", "."], "");
    assert_eq!(code, 1);
    assert_eq!(stdout, "");
    assert_eq!(
        stderr,
        "grep: no match. Skipped ignored directories: skipped_dir/. Search one by name, such as `grep -r needle skipped_dir`.\n"
    );
}

#[test]
fn a_match_or_an_error_prints_no_notice() {
    let dir = text_tree(&BTreeMap::from([
        (".gitignore", "skipped_dir/\n"),
        ("skipped_dir/needle.txt", "x\n"),
        ("kept.txt", "needle\n"),
    ]));
    let (_, _, matched) = text(&dir, &["-r", "needle", "."], "");
    assert_eq!(matched, "");
    let (code, _, failed) = text(&dir, &["-r", "absent", "nope.txt"], "");
    assert_eq!(code, 2);
    assert_eq!(failed, "grep: nope.txt: No such file or directory\n");
}

#[test]
fn declined_calls_fall_back_before_anything_is_written() {
    let dir = text_tree(&BTreeMap::from([("a.txt", "aa\nab\n")]));
    // `-l` with `-c` has no output the built-in reproduces.
    let with_both = vec![
        OsString::from("-l"),
        OsString::from("-c"),
        OsString::from("a"),
        OsString::from("a.txt"),
    ];
    // A back-reference, an invalid pattern and a non-UTF-8 pattern need
    // the system grep.
    let with_backref = vec![OsString::from("\\(a\\)\\1"), OsString::from("a.txt")];
    let with_invalid = vec![
        OsString::from("-E"),
        OsString::from("a{2"),
        OsString::from("a.txt"),
    ];
    let with_bytes = vec![OsString::from_vec(vec![0xff]), OsString::from("a.txt")];
    // `-e` is outside the handled flags.
    let with_dash_e = vec![
        OsString::from("-e"),
        OsString::from("a"),
        OsString::from("a.txt"),
    ];
    for args in [
        with_both,
        with_backref,
        with_invalid,
        with_bytes,
        with_dash_e,
    ] {
        let mut input = Cursor::new(Vec::new());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        match run(dir.path(), &args, &mut input, &mut stdout, &mut stderr) {
            Outcome::Fallback => {}
            Outcome::Done(code) => panic!("{args:?} ran builtin with {code}"),
        }
        assert!(stdout.is_empty(), "{args:?}");
        assert!(stderr.is_empty(), "{args:?}");
    }
}

#[test]
fn only_matching_prints_each_match_on_its_own_line() {
    let dir = text_tree(&BTreeMap::from([(
        "a.txt",
        "xneedle yneedle\nnone\nneedle\n",
    )]));
    let (code, stdout, stderr) = text(&dir, &["-o", "needle", "a.txt"], "");
    assert_eq!(code, 0);
    assert_eq!(stdout, "needle\nneedle\nneedle\n");
    assert_eq!(stderr, "");
    matches_like_grep(
        &BTreeMap::from([("a.txt", b"xneedle yneedle\nnone\nneedle\n".as_slice())]),
        &["-o", "needle", "a.txt"],
        None,
        &[],
        Some(b""),
        false,
    );
}

#[test]
fn only_matching_numbers_each_match() {
    let dir = text_tree(&BTreeMap::from([(
        "a.txt",
        "xneedle yneedle\nnone\nneedle\n",
    )]));
    let (code, stdout, stderr) = text(&dir, &["-n", "-o", "needle", "a.txt"], "");
    assert_eq!(code, 0);
    assert_eq!(stdout, "1:needle\n1:needle\n3:needle\n");
    assert_eq!(stderr, "");
    matches_like_grep(
        &BTreeMap::from([("a.txt", b"xneedle yneedle\nnone\nneedle\n".as_slice())]),
        &["-n", "-o", "needle", "a.txt"],
        None,
        &[],
        Some(b""),
        false,
    );
}

#[test]
fn only_matching_shows_the_path_for_many_files() {
    let dir = text_tree(&BTreeMap::from([
        ("a.txt", "xneedle\n"),
        ("b.txt", "needle y\n"),
    ]));
    let (_, pair, _) = text(&dir, &["-o", "needle", "a.txt", "b.txt"], "");
    assert_eq!(pair, "a.txt:needle\nb.txt:needle\n");
    let (_, single, _) = text(&dir, &["-o", "needle", "a.txt"], "");
    assert_eq!(single, "needle\n");
    let (_, numbered, _) = text(&dir, &["-n", "-o", "needle", "a.txt", "b.txt"], "");
    assert_eq!(numbered, "a.txt:1:needle\nb.txt:1:needle\n");
    matches_like_grep(
        &BTreeMap::from([
            ("a.txt", b"xneedle\n".as_slice()),
            ("b.txt", b"needle y\n".as_slice()),
        ]),
        &["-n", "-o", "needle", "a.txt", "b.txt"],
        None,
        &[],
        Some(b""),
        false,
    );
}

#[test]
fn only_matching_reads_standard_input() {
    let dir = text_tree(&BTreeMap::from([("a.txt", "needle\n")]));
    let (code, stdout, stderr) = text(&dir, &["-o", "needle"], "xneedle yneedle\n");
    assert_eq!(code, 0);
    assert_eq!(stdout, "needle\nneedle\n");
    assert_eq!(stderr, "");
    let (_, both, _) = text(&dir, &["-o", "needle", "-", "a.txt"], "xneedle\n");
    assert_eq!(both, "(standard input):needle\na.txt:needle\n");
    matches_like_grep(
        &BTreeMap::from([("a.txt", b"needle\n".as_slice())]),
        &["-o", "needle", "-", "a.txt"],
        Some(b"xneedle\n"),
        &[],
        Some(b""),
        false,
    );
}

#[test]
fn only_matching_searches_recursively_but_skips_ignored() {
    // The review on #497: every `-o` call fell back, so `grep -ro`
    // searched ignored directories. Built in, the walk skips them.
    let dir = text_tree(&BTreeMap::from([
        (".gitignore", "skipped_dir/\n"),
        ("skipped_dir/needle.txt", "needle\n"),
        ("kept/needle.txt", "xneedle yneedle\n"),
    ]));
    let (code, stdout, stderr) = text(&dir, &["-r", "-o", "needle", "."], "");
    assert_eq!(code, 0);
    assert_eq!(
        stdout,
        "./kept/needle.txt:needle\n./kept/needle.txt:needle\n"
    );
    assert_eq!(stderr, "");
    let (_, implicit, _) = text(&dir, &["-r", "-o", "needle"], "");
    assert_eq!(implicit, "kept/needle.txt:needle\nkept/needle.txt:needle\n");
}

#[test]
fn only_matching_skips_empty_matches() {
    let dir = text_tree(&BTreeMap::from([("a.txt", "abc\n"), ("b.txt", "b\n")]));
    // `b*` matches empty at every position: only the `b` prints.
    let (code, stdout, stderr) = text(&dir, &["-o", "b*", "a.txt"], "");
    assert_eq!(code, 0);
    assert_eq!(stdout, "b\n");
    assert_eq!(stderr, "");
    // No non-empty match anywhere: nothing prints, but the line matched.
    let (empty_code, empty_stdout, _) = text(&dir, &["-o", "x*", "b.txt"], "");
    assert_eq!(empty_code, 0);
    assert_eq!(empty_stdout, "");
    matches_like_grep(
        &BTreeMap::from([("a.txt", b"abc\n".as_slice())]),
        &["-o", "b*", "a.txt"],
        None,
        &[],
        Some(b""),
        false,
    );
    matches_like_grep(
        &BTreeMap::from([("b.txt", b"b\n".as_slice())]),
        &["-o", "x*", "b.txt"],
        None,
        &[],
        Some(b""),
        false,
    );
}

#[test]
fn only_matching_counts_matching_lines() {
    // As without `-o`: `-c` counts lines, not matches.
    let dir = text_tree(&BTreeMap::from([("a.txt", "a a\nb\n"), ("b.txt", "zzz\n")]));
    let (code, single, _) = text(&dir, &["-o", "-c", "a", "a.txt"], "");
    assert_eq!(code, 0);
    assert_eq!(single, "1\n");
    let (_, pair, _) = text(&dir, &["-o", "-c", "a", "a.txt", "b.txt"], "");
    assert_eq!(pair, "a.txt:1\nb.txt:0\n");
    let (missing_code, missing, _) = text(&dir, &["-o", "-c", "absent", "a.txt"], "");
    assert_eq!(missing_code, 1);
    assert_eq!(missing, "0\n");
    matches_like_grep(
        &BTreeMap::from([
            ("a.txt", b"a a\nb\n".as_slice()),
            ("b.txt", b"zzz\n".as_slice()),
        ]),
        &["-o", "-c", "a", "a.txt", "b.txt"],
        None,
        &[],
        Some(b""),
        false,
    );
}

#[test]
fn only_matching_lists_matching_files() {
    let dir = text_tree(&BTreeMap::from([
        ("a.txt", "xneedle\n"),
        ("b.txt", "plain\n"),
    ]));
    let (code, stdout, stderr) = text(&dir, &["-o", "-l", "needle", "a.txt", "b.txt"], "");
    assert_eq!(code, 0);
    assert_eq!(stdout, "a.txt\n");
    assert_eq!(stderr, "");
    matches_like_grep(
        &BTreeMap::from([
            ("a.txt", b"xneedle\n".as_slice()),
            ("b.txt", b"plain\n".as_slice()),
        ]),
        &["-o", "-l", "needle", "a.txt", "b.txt"],
        None,
        &[],
        Some(b""),
        false,
    );
}

#[test]
fn only_matching_ignores_case() {
    let dir = text_tree(&BTreeMap::from([("a.txt", "Needle xNEEDLEy needless\n")]));
    let (code, stdout, _) = text(&dir, &["-o", "-i", "needle", "a.txt"], "");
    assert_eq!(code, 0);
    assert_eq!(stdout, "Needle\nNEEDLE\nneedle\n");
    matches_like_grep(
        &BTreeMap::from([("a.txt", b"Needle xNEEDLEy needless\n".as_slice())]),
        &["-o", "-i", "needle", "a.txt"],
        None,
        &[],
        Some(b""),
        false,
    );
}

#[test]
fn only_matching_matches_words() {
    let dir = text_tree(&BTreeMap::from([("a.txt", "needle needles xneedle\n")]));
    let (code, stdout, _) = text(&dir, &["-o", "-w", "needle", "a.txt"], "");
    assert_eq!(code, 0);
    assert_eq!(stdout, "needle\n");
    matches_like_grep(
        &BTreeMap::from([("a.txt", b"needle needles xneedle\n".as_slice())]),
        &["-o", "-w", "needle", "a.txt"],
        None,
        &[],
        Some(b""),
        false,
    );
}

#[test]
fn only_matching_reads_fixed_strings() {
    let dir = text_tree(&BTreeMap::from([("a.txt", "a+b xa+by aab\n")]));
    let (code, stdout, _) = text(&dir, &["-o", "-F", "a+b", "a.txt"], "");
    assert_eq!(code, 0);
    assert_eq!(stdout, "a+b\na+b\n");
    matches_like_grep(
        &BTreeMap::from([("a.txt", b"a+b xa+by aab\n".as_slice())]),
        &["-o", "-F", "a+b", "a.txt"],
        None,
        &[],
        Some(b""),
        false,
    );
}

#[test]
fn only_matching_reads_extended_patterns() {
    let dir = text_tree(&BTreeMap::from([("a.txt", "a+b xaaby\n")]));
    let (code, stdout, _) = text(&dir, &["-o", "-E", "a+b", "a.txt"], "");
    assert_eq!(code, 0);
    assert_eq!(stdout, "aab\n");
    matches_like_grep(
        &BTreeMap::from([("a.txt", b"a+b xaaby\n".as_slice())]),
        &["-o", "-E", "a+b", "a.txt"],
        None,
        &[],
        Some(b""),
        false,
    );
}

#[test]
fn only_matching_reads_basic_patterns() {
    // Without `-E` or `-F` the pattern is a basic regular expression:
    // `\+` repeats, while a bare `+` reads as a literal.
    let dir = text_tree(&BTreeMap::from([("a.txt", "a+b xaaby\n")]));
    let (code, stdout, _) = text(&dir, &["-o", "a\\+b", "a.txt"], "");
    assert_eq!(code, 0);
    assert_eq!(stdout, "aab\n");
    let (_, literal, _) = text(&dir, &["-o", "a+b", "a.txt"], "");
    assert_eq!(literal, "a+b\n");
    matches_like_grep(
        &BTreeMap::from([("a.txt", b"a+b xaaby\n".as_slice())]),
        &["-o", "a\\+b", "a.txt"],
        None,
        &[],
        Some(b""),
        false,
    );
}

#[test]
fn only_matching_prints_invalid_utf8_raw() {
    // As GNU in the C locale: `.` matches any byte but a newline, and
    // the match prints as raw bytes.
    let dir = bytes_tree(&BTreeMap::from([("a.txt", &b"a\xffb\nplain\n"[..])]));
    let (code, stdout, _) = search(&dir, &["-o", "a.b", "a.txt"], &[]);
    assert_eq!(code, 0);
    assert_eq!(stdout, b"a\xffb\n");
    matches_like_grep(
        &BTreeMap::from([("a.txt", b"a\xffb\nplain\n".as_slice())]),
        &["-o", "a.b", "a.txt"],
        None,
        &[],
        Some(b""),
        false,
    );
}

#[test]
fn only_matching_keeps_literal_pipes_builtin() {
    // A pipe that reads as a literal is no alternation: `-F` escapes
    // it, and a bracket member never alternates.
    let dir = text_tree(&BTreeMap::from([("a.txt", "xa|by\n")]));
    let (code, stdout, stderr) = text(&dir, &["-o", "-F", "a|b", "a.txt"], "");
    assert_eq!(code, 0);
    assert_eq!(stdout, "a|b\n");
    assert_eq!(stderr, "");
    let (_, classed, _) = text(&dir, &["-o", "-E", "[a|b]+", "a.txt"], "");
    assert_eq!(classed, "a|b\n");
    matches_like_grep(
        &BTreeMap::from([("a.txt", b"xa|by\n".as_slice())]),
        &["-o", "-F", "a|b", "a.txt"],
        None,
        &[],
        Some(b""),
        false,
    );
    matches_like_grep(
        &BTreeMap::from([("a.txt", b"xa|by\n".as_slice())]),
        &["-o", "-E", "[a|b]+", "a.txt"],
        None,
        &[],
        Some(b""),
        false,
    );
}

#[test]
fn only_matching_with_invert_falls_back() {
    // `-o` with `-v` runs the system grep.
    let dir = text_tree(&BTreeMap::from([("a.txt", "needle\nplain\n")]));
    let args = vec![
        OsString::from("-o"),
        OsString::from("-v"),
        OsString::from("needle"),
        OsString::from("a.txt"),
    ];
    let mut input = Cursor::new(Vec::new());
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    match run(dir.path(), &args, &mut input, &mut stdout, &mut stderr) {
        Outcome::Fallback => {}
        Outcome::Done(code) => panic!("-o -v ran builtin with {code}"),
    }
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
}

#[test]
fn only_matching_with_context_falls_back() {
    // `-o` with `-A`, `-B` or `-C` runs the system grep.
    let dir = text_tree(&BTreeMap::from([("a.txt", "1\nneedle\n3\n")]));
    for flag in ["-A1", "-B1", "-C1"] {
        let args = vec![
            OsString::from("-o"),
            OsString::from(flag),
            OsString::from("needle"),
            OsString::from("a.txt"),
        ];
        let mut input = Cursor::new(Vec::new());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        match run(dir.path(), &args, &mut input, &mut stdout, &mut stderr) {
            Outcome::Fallback => {}
            Outcome::Done(code) => panic!("-o {flag} ran builtin with {code}"),
        }
        assert!(stdout.is_empty(), "{flag}");
        assert!(stderr.is_empty(), "{flag}");
    }
}

/// Whether the translated pattern holds an alternation, pinned case by
/// case: each comparison in `has_alternation` fails fast on its own test.
fn alternation(pattern: &str) -> bool {
    super::has_alternation(pattern)
}

#[test]
fn a_pipe_after_a_leading_right_bracket_is_not_an_alternation() {
    // A leading `]` is a member, not the closer: the pipe stays inside
    // the bracket in both spellings.
    assert!(!alternation("[]|]"));
    assert!(!alternation("[]a|b]"));
}

#[test]
fn a_pipe_after_a_negated_leading_right_bracket_is_not_an_alternation() {
    // A leading `^` only negates: the `]` after it is still a member, as
    // `translate_ere` reads it.
    assert!(!alternation("[^]|]"));
    assert!(!alternation("[^]a|b]"));
}

#[test]
fn a_bare_pipe_after_a_closed_bracket_is_an_alternation() {
    // The closer ended the bracket — even after a repeated caret, whose
    // second read is a member — so the pipe reads as syntax.
    assert!(alternation("[a]|b"));
    assert!(alternation("[^^]|x"));
    assert!(alternation("a|b"));
}

#[test]
fn only_matching_with_an_alternation_falls_back() {
    // With `-o` and an alternation, run the system grep: GNU prints the
    // longest match at each position while the regex crate prints the first
    // alternative, so the spans below would differ.
    let dir = text_tree(&BTreeMap::from([("a.txt", "ab\n")]));
    let basic = vec![
        OsString::from("-o"),
        OsString::from("a\\|ab"),
        OsString::from("a.txt"),
    ];
    let extended = vec![
        OsString::from("-o"),
        OsString::from("-E"),
        OsString::from("a|ab"),
        OsString::from("a.txt"),
    ];
    let lines = vec![
        OsString::from("-o"),
        OsString::from("a\nb"),
        OsString::from("a.txt"),
    ];
    for args in [basic, extended, lines] {
        let mut input = Cursor::new(Vec::new());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        match run(dir.path(), &args, &mut input, &mut stdout, &mut stderr) {
            Outcome::Fallback => {}
            Outcome::Done(code) => panic!("{args:?} ran builtin with {code}"),
        }
        assert!(stdout.is_empty(), "{args:?}");
        assert!(stderr.is_empty(), "{args:?}");
    }
}

#[test]
fn only_matching_on_a_closed_pipe_stops_before_the_input_ends() {
    // A broken pipe makes `-o` output stay quiet and stops reading, as
    // `a_closed_pipe_stops_before_the_input_ends` does for whole lines.
    let dir = text_tree(&BTreeMap::from([("a.txt", "x\n")]));
    let owned: Vec<OsString> = [OsString::from("-o"), OsString::from("needle")].to_vec();
    let path = dir.path().to_path_buf();
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut generator = Generator {
            read: 0,
            limit: 1 << 20,
        };
        let mut closed = Closed { writes: 0 };
        let mut stderr = Vec::new();
        let code = match run(&path, &owned, &mut generator, &mut closed, &mut stderr) {
            Outcome::Done(code) => code,
            Outcome::Fallback => panic!("fell back"),
        };
        done.send((code, generator.read, closed.writes, stderr))
            .unwrap_or(());
    });
    let deadline = std::time::Duration::from_secs(10);
    let (code, read, writes, stderr) = Deadline::after(deadline)
        .recv(&finished)
        .expect("waited 10s for the bounded generator search to finish");
    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert!(read < (1 << 20), "read {read}");
    assert!(writes < 16, "writes {writes}");
}

#[test]
fn an_only_matching_write_failure_is_an_error() {
    let dir = text_tree(&BTreeMap::from([("a.txt", "xneedle yneedle\n")]));
    let owned: Vec<OsString> = [
        OsString::from("-o"),
        OsString::from("needle"),
        OsString::from("a.txt"),
    ]
    .to_vec();
    let mut input = Cursor::new(Vec::new());
    let mut refused = Refused;
    let mut stderr = Vec::new();
    let code = match run(dir.path(), &owned, &mut input, &mut refused, &mut stderr) {
        Outcome::Done(code) => code,
        Outcome::Fallback => panic!("fell back"),
    };
    assert_eq!(code, 2);
    assert_eq!(stderr, b"grep: writing output: no space\n");
}

#[test]
fn context_breaks_separate_matching_files() {
    let dir = text_tree(&BTreeMap::from([("a.txt", "m1\n"), ("b.txt", "m2\n")]));
    // With context, GNU separates one input's lines from the next with
    // `--`, even when both are bare matches.
    let (_, stdout, _) = text(&dir, &["-C1", "m", "a.txt", "b.txt"], "");
    assert_eq!(stdout, "a.txt:m1\n--\nb.txt:m2\n");
    // Without context there is no separator.
    let (_, plain, _) = text(&dir, &["m", "a.txt", "b.txt"], "");
    assert_eq!(plain, "a.txt:m1\nb.txt:m2\n");
    // An input with no match contributes no separator either.
    let (_, gapped, _) = text(&dir, &["-C1", "m1", "a.txt", "b.txt"], "");
    assert_eq!(gapped, "a.txt:m1\n");
}

#[test]
fn one_sided_context_still_separates_matching_files() {
    // The separator needs context on either side, not both.
    let dir = text_tree(&BTreeMap::from([("a.txt", "m1\n"), ("b.txt", "m2\n")]));
    let (_, after, _) = text(&dir, &["-A1", "m", "a.txt", "b.txt"], "");
    assert_eq!(after, "a.txt:m1\n--\nb.txt:m2\n");
    let (_, before, _) = text(&dir, &["-B1", "m", "a.txt", "b.txt"], "");
    assert_eq!(before, "a.txt:m1\n--\nb.txt:m2\n");
}

#[test]
fn standard_input_with_context_separates_the_next_file() {
    // The filter counts as an earlier input: its lines separate from the
    // next file's with `--`, as two files do.
    let dir = text_tree(&BTreeMap::from([("a.txt", "m1\n")]));
    let (_, stdout, _) = text(&dir, &["-C1", "m", "-", "a.txt"], "m0\n");
    assert_eq!(stdout, "(standard input):m0\n--\na.txt:m1\n");
}

#[test]
fn a_walked_directory_with_context_separates_the_next_file() {
    // A walked directory's lines separate from the next input's too.
    let dir = text_tree(&BTreeMap::from([("sub/c.txt", "m2\n"), ("a.txt", "m1\n")]));
    let (_, stdout, _) = text(&dir, &["-r", "-C1", "m", "sub", "a.txt"], "");
    assert_eq!(stdout, "sub/c.txt:m2\n--\na.txt:m1\n");
}

#[test]
fn files_with_matches_labels_standard_input() {
    let dir = text_tree(&BTreeMap::from([("a.txt", "needle\n")]));
    let (code, stdout, _) = text(&dir, &["-l", "needle"], "needle\n");
    assert_eq!(code, 0);
    assert_eq!(stdout, "(standard input)\n");
    // A bare count stays bare.
    let (code, counts, _) = text(&dir, &["-c", "needle"], "a\nneedle\n");
    assert_eq!(code, 0);
    assert_eq!(counts, "1\n");
}

#[test]
fn a_dot_matches_invalid_utf8_bytes() {
    // As GNU in the C locale: `.` matches any byte but a newline.
    let dir = bytes_tree(&BTreeMap::from([("a.txt", &b"\xff\nplain\n"[..])]));
    let (code, stdout, _) = search(&dir, &[".", "a.txt"], &[]);
    assert_eq!(code, 0);
    assert_eq!(stdout, b"\xff\nplain\n");
}

#[test]
fn newline_patterns_read_as_alternatives() {
    // As repeated `-e`: each line of the pattern matches on its own.
    let dir = text_tree(&BTreeMap::from([("a.txt", "a\nb\nc\n")]));
    let (_, fixed, _) = text(&dir, &["-F", "a\nb", "a.txt"], "");
    assert_eq!(fixed, "a\nb\n");
    let (_, basic, _) = text(&dir, &["a\nb", "a.txt"], "");
    assert_eq!(basic, "a\nb\n");
    let (_, extended, _) = text(&dir, &["-E", "a\nb", "a.txt"], "");
    assert_eq!(extended, "a\nb\n");
    // A trailing newline leaves an empty alternative, matching all.
    let (code, all, _) = text(&dir, &["-F", "z\n", "a.txt"], "");
    assert_eq!(code, 0);
    assert_eq!(all, "a\nb\nc\n");
}

#[test]
fn a_utf8_bom_searches_as_raw_bytes() {
    // As GNU in the C locale: the BOM stays part of the line, not a
    // stripped marker.
    let dir = bytes_tree(&BTreeMap::from([(
        "a.txt",
        &b"\xef\xbb\xbfneedle\nplain\n"[..],
    )]));
    let (code, stdout, _) = search(&dir, &["needle", "a.txt"], &[]);
    assert_eq!(code, 0);
    assert_eq!(stdout, b"\xef\xbb\xbfneedle\n");
}

#[test]
fn utf16_bytes_search_as_raw_bytes() {
    // No decoding: UTF-16 bytes hold no contiguous `needle` and read as
    // binary, so the file skips silently as `grep -I` does instead of
    // printing a transcoded match.
    let dir = bytes_tree(&BTreeMap::from([(
        "a.txt",
        &b"\xff\xfen\x00e\x00e\x00d\x00l\x00e\x00\n\x00"[..],
    )]));
    let (code, stdout, stderr) = search(&dir, &["needle", "a.txt"], &[]);
    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
}

#[test]
fn a_closed_pipe_stops_before_the_input_ends() {
    let dir = text_tree(&BTreeMap::from([("a.txt", "x\n")]));
    let owned: Vec<OsString> = [OsString::from("needle")].to_vec();
    let path = dir.path().to_path_buf();
    // The search runs aside so the wait below bounds it: a regression
    // that reads to the end still finishes at the generator's limit,
    // while a hang fails naming what was waited for instead of the
    // harness timing out. The bounded generator is the cleanup: the
    // thread always ends after `limit` bytes.
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut generator = Generator {
            read: 0,
            limit: 1 << 20,
        };
        let mut closed = Closed { writes: 0 };
        let mut stderr = Vec::new();
        let code = match run(&path, &owned, &mut generator, &mut closed, &mut stderr) {
            Outcome::Done(code) => code,
            Outcome::Fallback => panic!("fell back"),
        };
        done.send((code, generator.read, closed.writes, stderr))
            .unwrap_or(());
    });
    let deadline = std::time::Duration::from_secs(10);
    let (code, read, writes, stderr) = Deadline::after(deadline)
        .recv(&finished)
        .expect("waited 10s for the bounded generator search to finish");
    // The first line matched, the pipe closed quietly, and most of the
    // megabyte was never read: nothing accumulated until end of input.
    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert!(read < (1 << 20), "read {read}");
    assert!(writes < 16, "writes {writes}");
}

#[test]
fn a_write_failure_is_an_error() {
    let dir = text_tree(&BTreeMap::from([("a.txt", "needle\n")]));
    let owned: Vec<OsString> = [OsString::from("needle"), OsString::from("a.txt")].to_vec();
    let mut input = Cursor::new(Vec::new());
    let mut refused = Refused;
    let mut stderr = Vec::new();
    let code = match run(dir.path(), &owned, &mut input, &mut refused, &mut stderr) {
        Outcome::Done(code) => code,
        Outcome::Fallback => panic!("fell back"),
    };
    assert_eq!(code, 2);
    assert_eq!(stderr, b"grep: writing output: no space\n");
}

#[test]
fn an_implicit_recursive_root_prints_without_the_dot_slash_prefix() {
    let dir = text_tree(&BTreeMap::from([
        ("a.txt", "needle\n"),
        ("sub/c.txt", "needle\n"),
    ]));
    // `-r` with no path searches `.` but prints paths without the walk's `./`
    // prefix.
    let (code, stdout, stderr) = text(&dir, &["-r", "needle"], "");
    assert_eq!(code, 0);
    assert_eq!(stdout, "a.txt:needle\nsub/c.txt:needle\n");
    assert_eq!(stderr, "");
    // An explicit `.` keeps the prefix.
    let (_, explicit, _) = text(&dir, &["-r", "needle", "."], "");
    assert_eq!(explicit, "./a.txt:needle\n./sub/c.txt:needle\n");
}

#[test]
fn a_closed_pipe_stops_a_recursive_search_before_a_later_error() {
    use std::os::unix::fs::PermissionsExt;
    let dir = text_tree(&BTreeMap::from([
        ("a.txt", "needle\n"),
        ("zzz_locked/hay.txt", "needle\n"),
    ]));
    let locked = dir.path().join("zzz_locked");
    let kept = fs::metadata(&locked).unwrap().permissions();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let owned: Vec<OsString> = [
        OsString::from("-r"),
        OsString::from("needle"),
        OsString::from("."),
    ]
    .to_vec();
    let mut input = Cursor::new(Vec::new());
    let mut closed = Closed { writes: 0 };
    let mut stderr = Vec::new();
    let code = match run(dir.path(), &owned, &mut input, &mut closed, &mut stderr) {
        Outcome::Done(code) => code,
        Outcome::Fallback => panic!("fell back"),
    };
    fs::set_permissions(&locked, kept).unwrap();
    // The first match breaks the pipe, so the walk stops before the
    // locked directory's error instead of failing with permission denied.
    assert_eq!(code, 0);
    assert!(stderr.is_empty());
}

#[test]
fn a_closed_pipe_stops_later_paths_before_their_complaints() {
    let dir = text_tree(&BTreeMap::from([("a.txt", "needle\n")]));
    let owned: Vec<OsString> = [
        OsString::from("needle"),
        OsString::from("a.txt"),
        OsString::from("nope.txt"),
    ]
    .to_vec();
    let mut input = Cursor::new(Vec::new());
    let mut closed = Closed { writes: 0 };
    let mut stderr = Vec::new();
    let code = match run(dir.path(), &owned, &mut input, &mut closed, &mut stderr) {
        Outcome::Done(code) => code,
        Outcome::Fallback => panic!("fell back"),
    };
    // The match in `a.txt` breaks the pipe, so the missing path's
    // complaint never prints and the exit stays what it had so far.
    assert_eq!(code, 0);
    assert!(stderr.is_empty());
}

#[test]
fn a_closed_pipe_with_a_zero_count_stays_quiet_despite_skipped_directories() {
    let dir = text_tree(&BTreeMap::from([
        (".gitignore", "skipped_dir/\n"),
        ("skipped_dir/needle.txt", "x\n"),
        ("kept.txt", "x\n"),
    ]));
    let owned: Vec<OsString> = [
        OsString::from("-r"),
        OsString::from("-c"),
        OsString::from("absent"),
        OsString::from("."),
    ]
    .to_vec();
    let mut input = Cursor::new(Vec::new());
    let mut closed = Closed { writes: 0 };
    let mut stderr = Vec::new();
    let code = match run(dir.path(), &owned, &mut input, &mut closed, &mut stderr) {
        Outcome::Done(code) => code,
        Outcome::Fallback => panic!("fell back"),
    };
    // A broken pipe ends the process quietly with the exit it had so far. The
    // zero counts break the pipe with no match, so the exit stays 1 and the
    // skipped-directory notice never prints.
    assert_eq!(code, 1);
    assert!(
        stderr.is_empty(),
        "stderr: {}",
        String::from_utf8_lossy(&stderr)
    );
}

/// Whether the runner's grep speaks GNU: only then do outputs compare.
#[track_caller]
fn gnu_grep() -> bool {
    use std::os::unix::process::CommandExt;
    let child = std::process::Command::new("grep")
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    // Every wait has a deadline (`docs/testing.md`, "Waits and timeouts").
    let output = super::super::wait_output(child, "grep --version");
    String::from_utf8_lossy(&output.stdout).contains("GNU grep")
}

/// Checks the built-in against the system grep on the same corpus: the same
/// arguments in the same directory with the same standard input give the
/// same standard output and exit code. `system` holds extra arguments for
/// the system only, as `-I` does. `sorted` compares lines as a set: the
/// built-in visits files in sorted order where the system's order is
/// unspecified. Only where the runner's grep is GNU; elsewhere the pinned
/// literals above carry it.
#[track_caller]
fn matches_like_grep(
    files: &BTreeMap<&str, &[u8]>,
    args: &[&str],
    stdin: Option<&[u8]>,
    system: &[&str],
    stderr: Option<&[u8]>,
    sorted: bool,
) {
    if !gnu_grep() {
        return;
    }
    let dir = bytes_tree(files);
    let owned: Vec<OsString> = args.iter().map(OsString::from).collect();
    let mut input = Cursor::new(stdin.unwrap_or_default().to_vec());
    let mut stdout = Vec::new();
    let mut errors = Vec::new();
    let code = match run(dir.path(), &owned, &mut input, &mut stdout, &mut errors) {
        Outcome::Done(code) => code,
        Outcome::Fallback => panic!("{args:?} fell back"),
    };
    if let Some(expected) = stderr {
        assert_eq!(errors, expected, "{args:?}");
    }
    use std::os::unix::process::CommandExt;
    let mut child = std::process::Command::new("grep")
        .args(system)
        .args(args)
        .current_dir(dir.path())
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    if let Some(given) = stdin {
        std::io::Write::write_all(child.stdin.as_mut().unwrap(), given).unwrap();
    }
    drop(child.stdin.take());
    let output = super::super::wait_output(child, &format!("system grep {args:?}"));
    if sorted {
        // Lines as a set: the built-in visits files in sorted order where
        // the system's order is unspecified.
        let mut left: Vec<&[u8]> = stdout.split(|byte| *byte == b'\n').collect();
        let mut right: Vec<&[u8]> = output.stdout.split(|byte| *byte == b'\n').collect();
        left.sort();
        right.sort();
        assert_eq!(left, right, "{args:?}");
    } else {
        assert_eq!(stdout, output.stdout, "{args:?}");
    }
    assert_eq!(code, output.status.code().unwrap_or(99), "{args:?}");
}

/// The text corpus every plain differential case shares.
fn corpus() -> BTreeMap<&'static str, &'static [u8]> {
    BTreeMap::from([
        ("a.txt", b"needle one\nplain\n".as_slice()),
        ("b.txt", b"nothing here\nneedle two\n".as_slice()),
        ("sub/c.txt", b"Needle three\n".as_slice()),
    ])
}

#[test]
fn outputs_match_grep_byte_for_byte() {
    let files = corpus();
    let plain: &[(&[&str], Option<&[u8]>)] = &[
        (&["needle", "a.txt", "b.txt"], None),
        (&["needle", "a.txt"], None),
        (&["-n", "needle", "a.txt", "b.txt"], None),
        (&["-r", "needle", "."], None),
        (&["-r", "needle", "sub"], None),
        (&["-r", "needle", "a.txt"], None),
        (&["-i", "needle", "a.txt", "b.txt", "sub/c.txt"], None),
        (&["-v", "needle", "a.txt"], None),
        (&["-n", "-v", "needle", "a.txt"], None),
        (&["-E", "needle (one|two)", "a.txt", "b.txt"], None),
        (&["-F", "needle (one|two)", "a.txt", "b.txt"], None),
        (&["-w", "needle", "a.txt", "b.txt"], None),
        (&["-c", "needle", "a.txt"], None),
        (&["-c", "needle", "a.txt", "b.txt"], None),
        (&["-c", "-v", "needle", "a.txt"], None),
        (&["-l", "needle", "a.txt", "b.txt"], None),
        (&["-l", "needle", "a.txt"], None),
        (&["-r", "--include=*.txt", "needle", "."], None),
        (&["-r", "--exclude=c.txt", "needle", "."], None),
        // The last matching rule wins, either way round.
        (
            &["-r", "--exclude=*.txt", "--include=*.txt", "needle", "."],
            None,
        ),
        (
            &["-r", "--include=*.txt", "--exclude=*.txt", "needle", "."],
            None,
        ),
        (&["", "a.txt"], None),
        (&["needle", "a.txt", "b.txt", "sub/c.txt"], None),
    ];
    for (args, stdin) in plain {
        let sorted = args.contains(&"-r");
        matches_like_grep(&files, args, *stdin, &[], Some(b""), sorted);
    }
}

#[test]
fn context_matches_grep_byte_for_byte() {
    let files = BTreeMap::from([(
        "ctx.txt",
        b"line1\nneedle one\nline3\nline4\nline5\nneedle two\nline7\n".as_slice(),
    )]);
    let cases: &[&[&str]] = &[
        &["-A1", "needle", "ctx.txt"],
        &["-B1", "needle", "ctx.txt"],
        &["-C1", "needle", "ctx.txt"],
        &["-n", "-C1", "needle", "ctx.txt"],
        &["-v", "-A1", "plain", "ctx.txt"],
        &["-A2", "-B2", "needle", "ctx.txt"],
    ];
    for args in cases {
        matches_like_grep(&files, args, None, &[], Some(b""), false);
    }
}

#[test]
fn filters_match_grep_byte_for_byte() {
    let files = corpus();
    matches_like_grep(
        &files,
        &["needle"],
        Some(b"needle\nplain\n"),
        &[],
        Some(b""),
        false,
    );
    matches_like_grep(
        &files,
        &["-n", "needle"],
        Some(b"needle\nplain\n"),
        &[],
        Some(b""),
        false,
    );
    matches_like_grep(
        &files,
        &["needle", "-", "a.txt"],
        Some(b"needle\n"),
        &[],
        Some(b""),
        false,
    );
    matches_like_grep(
        &files,
        &["-c", "needle", "-"],
        Some(b"a\nneedle\n"),
        &[],
        Some(b""),
        false,
    );
}

#[test]
fn rough_edges_match_grep_byte_for_byte() {
    let files = BTreeMap::from([
        ("unterminated.txt", b"plain\nneedle".as_slice()),
        ("crlf.txt", b"needle one\r\nplain\r\n".as_slice()),
        ("binary.bin", b"needle\nplain\x00needle\n".as_slice()),
        ("invalid.txt", b"needle \xff\nplain\n".as_slice()),
    ]);
    matches_like_grep(
        &files,
        &["needle", "unterminated.txt"],
        None,
        &[],
        Some(b""),
        false,
    );
    matches_like_grep(&files, &["needle", "crlf.txt"], None, &[], Some(b""), false);
    matches_like_grep(
        &files,
        &["-r", "needle", "."],
        None,
        &["-I"],
        Some(b""),
        true,
    );
    matches_like_grep(
        &files,
        &["needle", "invalid.txt"],
        None,
        &[],
        Some(b""),
        false,
    );
}

/// The bracket corpus every bracket differential case shares.
fn bracket_files() -> BTreeMap<&'static str, &'static [u8]> {
    BTreeMap::from([(
        "brackets.txt",
        b"xa\nxb\n^\n^^\n^a\na^\na\nb\nd\n1\n[\n]\n[]\n[]a]\na&b\na~b\na-b\n-\n\\\na\\b\nalpha\na.a\n \n".as_slice(),
    )])
}

/// Runs one bracket case through the whole search against the system grep:
/// the built-in prints what the system grep prints with the same exit.
/// Every pattern matches, so no skipped-directory notice follows. Each
/// case below runs through here under its own name, so a mutant changing
/// one case fails fast under that name instead of hiding in a loop.
#[track_caller]
fn bracket_search_matches_grep(args: &[&str]) {
    let files = bracket_files();
    matches_like_grep(&files, args, None, &[], Some(b""), false);
}

/// One test per bracket pattern and mode, generated from the shared case
/// list: the bracket table in `super::bre` through the whole search, each
/// comparison under its own name.
macro_rules! bracket_search_tests {
    ($($name:ident: [$($arg:literal),*];)*) => {
        $(
            #[test]
            fn $name() {
                bracket_search_matches_grep(&[$($arg),*]);
            }
        )*
    };
}

bracket_search_tests! {
    brackets_bre_double_caret_ab: ["[^^][ab]", "brackets.txt"];
    brackets_ere_double_caret_ab: ["-E", "[^^][ab]", "brackets.txt"];
    brackets_bre_double_caret: ["[^^]", "brackets.txt"];
    brackets_ere_double_caret: ["-E", "[^^]", "brackets.txt"];
    brackets_bre_negated_closer: ["[^]]", "brackets.txt"];
    brackets_ere_negated_closer: ["-E", "[^]]", "brackets.txt"];
    brackets_bre_negated_leading_closer: ["[^]a]", "brackets.txt"];
    brackets_ere_negated_leading_closer: ["-E", "[^]a]", "brackets.txt"];
    brackets_bre_leading_closer: ["[]a]", "brackets.txt"];
    brackets_ere_leading_closer: ["-E", "[]a]", "brackets.txt"];
    brackets_bre_trailing_caret: ["[a^]", "brackets.txt"];
    brackets_ere_trailing_caret: ["-E", "[a^]", "brackets.txt"];
    brackets_bre_open_member: ["[[]", "brackets.txt"];
    brackets_ere_open_member: ["-E", "[[]", "brackets.txt"];
    brackets_bre_lone_closer: ["[]]", "brackets.txt"];
    brackets_ere_lone_closer: ["-E", "[]]", "brackets.txt"];
    brackets_bre_leading_dash: ["[-a]", "brackets.txt"];
    brackets_ere_leading_dash: ["-E", "[-a]", "brackets.txt"];
    brackets_bre_trailing_dash: ["[a-]", "brackets.txt"];
    brackets_ere_trailing_dash: ["-E", "[a-]", "brackets.txt"];
    brackets_bre_dash_range: ["[a-b]", "brackets.txt"];
    brackets_ere_dash_range: ["-E", "[a-b]", "brackets.txt"];
    brackets_bre_double_ampersand: ["[a&&b]", "brackets.txt"];
    brackets_ere_double_ampersand: ["-E", "[a&&b]", "brackets.txt"];
    brackets_bre_double_tilde: ["[a~~b]", "brackets.txt"];
    brackets_ere_double_tilde: ["-E", "[a~~b]", "brackets.txt"];
    brackets_bre_open_bracket_member: ["[a[b]", "brackets.txt"];
    brackets_ere_open_bracket_member: ["-E", "[a[b]", "brackets.txt"];
    brackets_bre_backslash_d: ["[\\d]", "brackets.txt"];
    brackets_ere_backslash_d: ["-E", "[\\d]", "brackets.txt"];
    brackets_bre_escaped_dash: ["[a\\-z]", "brackets.txt"];
    brackets_ere_escaped_dash: ["-E", "[a\\-z]", "brackets.txt"];
    brackets_bre_escaped_closer: ["[a\\]]", "brackets.txt"];
    brackets_ere_escaped_closer: ["-E", "[a\\]]", "brackets.txt"];
    brackets_bre_posix_alpha: ["[[:alpha:]]", "brackets.txt"];
    brackets_ere_posix_alpha: ["-E", "[[:alpha:]]", "brackets.txt"];
    brackets_bre_bang_member: ["[!ab]", "brackets.txt"];
    brackets_ere_bang_member: ["-E", "[!ab]", "brackets.txt"];
}

#[test]
fn errors_match_grep_exit_codes() {
    let files = corpus();
    // Standard error prose differs; standard output and the code do not.
    matches_like_grep(
        &files,
        &["needle", "nope.txt", "a.txt"],
        None,
        &[],
        None,
        false,
    );
    matches_like_grep(&files, &["needle", "sub"], None, &[], None, false);
    matches_like_grep(&files, &["absent", "a.txt"], None, &[], Some(b""), false);
}
