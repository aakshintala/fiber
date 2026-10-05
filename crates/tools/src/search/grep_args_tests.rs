//! Tests beside [`super::parse`]: flags, operands and declines.

use std::ffi::OsString;

use super::{Mode, Parsed, parse};

fn args(words: &[&str]) -> Vec<OsString> {
    words.iter().map(OsString::from).collect()
}

fn run(words: &[&str]) -> super::Options {
    match parse(&args(words)) {
        Parsed::Run(options) => options,
        Parsed::Fallback => panic!("{words:?} fell back"),
        Parsed::Error(message) => panic!("{words:?} failed: {message}"),
    }
}

fn fallback(words: &[&str]) {
    match parse(&args(words)) {
        Parsed::Fallback => {}
        Parsed::Run(_) => panic!("{words:?} ran"),
        Parsed::Error(message) => panic!("{words:?} failed: {message}"),
    }
}

fn error(words: &[&str]) -> String {
    match parse(&args(words)) {
        Parsed::Error(message) => message,
        Parsed::Fallback => panic!("{words:?} fell back"),
        Parsed::Run(_) => panic!("{words:?} ran"),
    }
}

#[test]
fn flags_pattern_and_paths_split() {
    let options = run(&["-rn", "needle", "a.txt", "b.txt"]);
    assert!(options.recursive);
    assert!(options.line_numbers);
    assert_eq!(options.pattern, b"needle");
    assert_eq!(options.paths.len(), 2);
    assert_eq!(options.mode, Mode::Basic);
    assert!(!options.invert);
}

#[test]
fn every_handled_short_flag_sets_its_field() {
    let options = run(&["-n", "-i", "-v", "-l", "-c", "-w", "needle"]);
    assert!(options.line_numbers);
    assert!(options.ignore_case);
    assert!(options.invert);
    assert!(options.files_with_matches);
    assert!(options.count);
    assert!(options.word);
}

#[test]
fn extended_and_fixed_last_wins() {
    assert_eq!(run(&["-E", "needle"]).mode, Mode::Extended);
    assert_eq!(run(&["-F", "needle"]).mode, Mode::Fixed);
    assert_eq!(run(&["-E", "-F", "needle"]).mode, Mode::Fixed);
    assert_eq!(run(&["-F", "-E", "needle"]).mode, Mode::Extended);
}

#[test]
fn context_takes_joined_separate_and_both_sides() {
    assert_eq!(run(&["-A3", "needle"]).after, 3);
    assert_eq!(run(&["-A", "3", "needle"]).after, 3);
    assert_eq!(run(&["-B2", "-A1", "needle"]).before, 2);
    let both = run(&["-C2", "needle"]);
    assert_eq!((both.before, both.after), (2, 2));
    let over = run(&["-C2", "-A1", "needle"]);
    assert_eq!((over.before, over.after), (2, 1));
    assert_eq!(run(&["-A0", "needle"]).after, 0);
}

#[test]
fn include_and_exclude_take_equals_and_separate_forms() {
    let options = run(&["--include=*.rs", "--exclude", "*.log", "needle"]);
    assert_eq!(options.filters.len(), 2);
    assert!(options.filters[0].include);
    assert_eq!(options.filters[0].glob, b"*.rs");
    assert!(!options.filters[1].include);
    assert_eq!(options.filters[1].glob, b"*.log");
}

#[test]
fn double_dash_ends_flags() {
    let options = run(&["--", "-n"]);
    assert_eq!(options.pattern, b"-n");
    assert!(options.paths.is_empty());
    assert!(!options.line_numbers);
    let paths = run(&["-r", "--", "-n", "-x"]);
    assert!(paths.recursive);
    assert_eq!(paths.pattern, b"-n");
    assert_eq!(paths.paths.len(), 1);
}

#[test]
fn a_lone_dash_is_standard_input_not_a_flag() {
    // A lone `-` is shorter than any flag cluster, so it reads as the
    // pattern or a path.
    let options = run(&["-", "needle"]);
    assert_eq!(options.pattern, b"-");
    assert_eq!(options.paths.len(), 1);
    let filter = run(&["needle", "-"]);
    assert_eq!(filter.pattern, b"needle");
    assert_eq!(filter.paths.len(), 1);
}

#[test]
fn an_unhandled_flag_hands_over() {
    for words in [
        &["-e", "needle"][..],
        &["-q", "needle"],
        &["-R", "needle"],
        &["-G", "needle"],
        &["--recursive", "needle"],
        &["--color", "needle"],
        &["-rnq", "needle"],
        &["needle", "-n"],
        &["--"],
    ] {
        // `--` alone still needs a pattern: the system prints its usage.
        fallback(words);
    }
}

#[test]
fn no_pattern_hands_over() {
    fallback(&[]);
    fallback(&["-rn"]);
}

#[test]
fn a_missing_value_is_a_usage_error() {
    assert_eq!(error(&["-A"]), "grep: option '-A' requires an argument");
    assert_eq!(
        error(&["-B2", "-C"]),
        "grep: option '-C' requires an argument"
    );
    assert_eq!(
        error(&["--include"]),
        "grep: option '--include' requires an argument"
    );
}

#[test]
fn a_bad_context_length_is_a_usage_error() {
    assert_eq!(
        error(&["-Ax", "needle"]),
        "grep: 'x': invalid context length argument"
    );
    assert_eq!(
        error(&["-A", "3x", "needle"]),
        "grep: '3x': invalid context length argument"
    );
    assert_eq!(
        error(&["-A", "99999999999999999999999", "needle"]),
        "grep: '99999999999999999999999': invalid context length argument"
    );
}

#[test]
fn only_matching_sets_the_flag() {
    assert!(run(&["-o", "needle"]).only_matching);
}
