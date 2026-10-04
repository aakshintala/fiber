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
    // An exclusion wins over an inclusion, on a named file too.
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
    // Provisional ruling 20 on #298: `-o` hands over until the owner rules.
    let with_only = vec![
        OsString::from("-o"),
        OsString::from("a"),
        OsString::from("a.txt"),
    ];
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
        with_only,
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
fn a_closed_pipe_stops_before_the_input_ends() {
    let dir = text_tree(&BTreeMap::from([("a.txt", "x\n")]));
    let owned: Vec<OsString> = [OsString::from("needle")].to_vec();
    let mut generator = Generator {
        read: 0,
        limit: 1 << 20,
    };
    let mut closed = Closed { writes: 0 };
    let mut stderr = Vec::new();
    let code = match run(dir.path(), &owned, &mut generator, &mut closed, &mut stderr) {
        Outcome::Done(code) => code,
        Outcome::Fallback => panic!("fell back"),
    };
    // The first line matched, the pipe closed quietly, and most of the
    // megabyte was never read: nothing accumulated until end of input.
    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert!(generator.read < generator.limit, "read {}", generator.read);
    assert!(closed.writes < 16, "writes {}", closed.writes);
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

/// Whether the runner's grep speaks GNU: only then do outputs compare.
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
