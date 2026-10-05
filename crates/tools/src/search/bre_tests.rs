//! Tests beside [`super`]: the translation, pinned line by line and checked
//! against the runner's grep on a fixed corpus where that grep is GNU.

use std::fs;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

use grep_regex::RegexMatcherBuilder;
use grep_searcher::{Searcher, SearcherBuilder, Sink, SinkMatch};

use super::{translate_bre, translate_ere};

fn bre(pattern: &str) -> Option<String> {
    translate_bre(pattern)
}

#[test]
fn groups_intervals_alternations_and_quantifiers_translate() {
    assert_eq!(bre("\\(ab\\)"), Some("(ab)".to_owned()));
    assert_eq!(bre("\\(ab\\)*"), Some("(ab)*".to_owned()));
    assert_eq!(bre("a\\{2\\}"), Some("a{2}".to_owned()));
    assert_eq!(bre("a\\{2,3\\}"), Some("a{2,3}".to_owned()));
    assert_eq!(bre("a\\|b"), Some("a|b".to_owned()));
    assert_eq!(bre("a\\+"), Some("a+".to_owned()));
    assert_eq!(bre("a\\?"), Some("a?".to_owned()));
}

#[test]
fn bare_metacharacters_are_literals() {
    assert_eq!(bre("a|b"), Some("a\\|b".to_owned()));
    assert_eq!(bre("a+b"), Some("a\\+b".to_owned()));
    assert_eq!(bre("a?b"), Some("a\\?b".to_owned()));
    assert_eq!(bre("(ab)"), Some("\\(ab\\)".to_owned()));
    assert_eq!(bre("a{2}"), Some("a\\{2\\}".to_owned()));
}

#[test]
fn stars_anchor_or_repeat() {
    assert_eq!(bre("*"), Some("\\*".to_owned()));
    assert_eq!(bre("a*"), Some("a*".to_owned()));
    assert_eq!(bre("\\(*"), Some("(\\*".to_owned()));
    assert_eq!(bre("a\\|*"), Some("a|\\*".to_owned()));
    assert_eq!(bre("^*"), Some("^\\*".to_owned()));
}

#[test]
fn carets_anchor_at_the_start_and_after_groups() {
    assert_eq!(bre("^foo"), Some("^foo".to_owned()));
    assert_eq!(bre("\\(^foo\\)"), Some("(^foo)".to_owned()));
    assert_eq!(bre("\\(^foo\\|bar\\)"), Some("(^foo|bar)".to_owned()));
    assert_eq!(bre("a^b"), Some("a\\^b".to_owned()));
    assert_eq!(bre("^^foo"), Some("^\\^foo".to_owned()));
}

#[test]
fn dollars_anchor_at_the_end_and_before_closes() {
    assert_eq!(bre("foo$"), Some("foo$".to_owned()));
    assert_eq!(bre("a$b"), Some("a\\$b".to_owned()));
    assert_eq!(bre("$"), Some("$".to_owned()));
    assert_eq!(bre("\\(a$\\)"), Some("(a$)".to_owned()));
    assert_eq!(bre("a$\\|b"), Some("a$|b".to_owned()));
}

#[test]
fn word_boundaries_and_buffer_anchors_translate() {
    assert_eq!(bre("\\<foo\\>"), Some("\\b{start}foo\\b{end}".to_owned()));
    assert_eq!(bre("\\`foo"), Some("\\Afoo".to_owned()));
    assert_eq!(bre("foo\\'"), Some("foo\\z".to_owned()));
}

#[test]
fn classes_pass_through_backrefs_fall_back() {
    assert_eq!(bre("\\w"), Some("\\w".to_owned()));
    assert_eq!(bre("\\W"), Some("\\W".to_owned()));
    assert_eq!(bre("\\s"), Some("\\s".to_owned()));
    assert_eq!(bre("\\S"), Some("\\S".to_owned()));
    assert_eq!(bre("\\b"), Some("\\b".to_owned()));
    assert_eq!(bre("\\B"), Some("\\B".to_owned()));
    assert_eq!(bre("\\d"), Some("d".to_owned()));
    assert_eq!(bre("\\1"), None);
    assert_eq!(bre("\\(a\\)\\9"), None);
}

#[test]
fn other_escapes_are_literal_or_hand_over() {
    assert_eq!(bre("\\."), Some("\\.".to_owned()));
    assert_eq!(bre("\\\\"), Some("\\\\".to_owned()));
    assert_eq!(bre("\\/"), Some("\\/".to_owned()));
    assert_eq!(bre("\\ "), Some("\\ ".to_owned()));
    assert_eq!(bre("foo\\"), None);
    assert_eq!(bre("a\\\u{e9}"), None);
    assert_eq!(bre("\\<*"), None);
}

#[test]
fn brackets_copy_with_escapes_and_kept_leaders() {
    assert_eq!(bre("[abc]"), Some("[abc]".to_owned()));
    assert_eq!(bre("[^ab]"), Some("[^ab]".to_owned()));
    assert_eq!(bre("[!ab]"), Some("[\\!ab]".to_owned()));
    assert_eq!(bre("[]a]"), Some("[]a]".to_owned()));
    assert_eq!(bre("[^]a]"), Some("[^]a]".to_owned()));
    assert_eq!(bre("[a\\]"), Some("[a\\\\]".to_owned()));
    assert_eq!(bre("[a-z]"), Some("[a-z]".to_owned()));
    assert_eq!(bre("[[:alpha:]]"), Some("[[:alpha:]]".to_owned()));
    // A `:` after an ordinary member is ordinary too: only right after
    // `[` does `:` open a class.
    assert_eq!(bre("[a::]"), Some("[a::]".to_owned()));
    // A class with no closer hands over, even when a `:]` follows: the
    // bracket never closed.
    assert_eq!(bre("[a[:alpha:]"), None);
    assert_eq!(bre("a[bc"), None);
}

#[test]
fn ere_keeps_everything_but_the_gnu_extensions() {
    assert_eq!(translate_ere("a+b"), Some("a+b".to_owned()));
    assert_eq!(translate_ere("(a|b)"), Some("(a|b)".to_owned()));
    assert_eq!(
        translate_ere("\\<a\\>"),
        Some("\\b{start}a\\b{end}".to_owned())
    );
    assert_eq!(translate_ere("\\1"), None);
    assert_eq!(translate_ere("(a)\\2"), None);
    assert_eq!(translate_ere("a\\"), None);
}

/// Whether the runner's grep speaks GNU: only then do outputs compare.
fn gnu_grep() -> bool {
    let child = Command::new("grep")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    // Every wait has a deadline (`docs/testing.md`, "Waits and timeouts").
    let output = super::super::wait_output(child, "grep --version");
    String::from_utf8_lossy(&output.stdout).contains("GNU")
}

/// One line the engine matched.
struct Hit(bool);

impl Sink for Hit {
    type Error = std::io::Error;

    fn matched(&mut self, _: &Searcher, _: &SinkMatch<'_>) -> Result<bool, std::io::Error> {
        self.0 = true;
        Ok(false)
    }
}

/// Checks the translation against the system grep, line by line: every
/// corpus line the translated pattern matches through the searcher's own
/// engine is one the system grep prints, and vice versa. Only where the
/// runner's grep is GNU; elsewhere the pinned translations above carry it.
fn matches_like_grep(pattern: &str, translated: &str, grep_args: &[&str], case_insensitive: bool) {
    if !gnu_grep() {
        return;
    }
    let corpus = [
        "foo",
        "foobar",
        "foo bar",
        "bar",
        "aab",
        "aaab",
        "a+b",
        "a?b",
        "a|b",
        "(parens)",
        "(foo)",
        "[brackets]",
        "a{2}",
        "]",
        "^caret",
        "dollar$",
        "a*b",
        "a^b",
        "a$b",
        "back\\slash",
        "dot.",
        "UPPER",
        "ab12",
        "a_needle",
    ];
    let matcher = RegexMatcherBuilder::new()
        .case_insensitive(case_insensitive)
        .build(translated)
        .unwrap();
    let mut searcher = SearcherBuilder::new().build();
    let mut engine: Vec<&str> = Vec::new();
    for line in corpus {
        let mut hit = Hit(false);
        searcher
            .search_slice(&matcher, format!("{line}\n").as_bytes(), &mut hit)
            .unwrap();
        if hit.0 {
            engine.push(line);
        }
    }
    let dir = fakes::TempDir::new("fiber-search-bre");
    let file = dir.path().join("corpus.txt");
    fs::write(&file, corpus.join("\n") + "\n").unwrap();
    let child = Command::new("grep")
        .env("LC_ALL", "C")
        .args(grep_args)
        .arg("-e")
        .arg(pattern)
        .arg(&file)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let output = super::super::wait_output(child, &format!("grep -e {pattern:?}"));
    let system_text = String::from_utf8(output.stdout).unwrap();
    let system: Vec<&str> = system_text.lines().collect();
    assert_eq!(engine, system, "BRE {pattern:?} as {translated:?}");
}

#[test]
fn translations_match_grep_line_for_line() {
    // Each pair is the BRE pattern and its pinned translation.
    let cases = [
        ("foo", "foo"),
        ("o", "o"),
        ("foo|bar", "foo\\|bar"),
        ("a+b", "a\\+b"),
        ("a?b", "a\\?b"),
        ("(parens)", "\\(parens\\)"),
        ("[brackets]", "[brackets]"),
        ("a{2}", "a\\{2\\}"),
        ("^foo", "^foo"),
        ("bar$", "bar$"),
        ("^foo$", "^foo$"),
        ("a^b", "a\\^b"),
        ("a$b", "a\\$b"),
        ("^^foo", "^\\^foo"),
        ("\\(foo\\)", "(foo)"),
        ("\\(a\\)*", "(a)*"),
        ("a\\{2\\}", "a{2}"),
        ("a\\{2,3\\}", "a{2,3}"),
        ("foo\\|bar", "foo|bar"),
        ("\\<foo\\>", "\\b{start}foo\\b{end}"),
        ("\\bfoo\\b", "\\bfoo\\b"),
        ("f.o", "f.o"),
        ("\\.", "\\."),
        ("\\\\", "\\\\"),
        ("[abc]", "[abc]"),
        ("[^ab]", "[^ab]"),
        ("[a-c]", "[a-c]"),
        ("[]a]", "[]a]"),
        ("[^]a]", "[^]a]"),
        ("[!a]", "[\\!a]"),
        ("[a\\]", "[a\\\\]"),
        ("[[:alpha:]]", "[[:alpha:]]"),
        ("[a-z]*[0-9]", "[a-z]*[0-9]"),
        ("\\w\\+", "\\w+"),
    ];
    for (pattern, translated) in cases {
        assert_eq!(
            translate_bre(pattern),
            Some(translated.to_owned()),
            "{pattern:?}"
        );
        matches_like_grep(pattern, translated, &[], false);
    }
}

#[test]
fn ere_translations_match_grep_line_for_line() {
    let cases = [
        ("a+b", "a+b"),
        ("(foo|bar)", "(foo|bar)"),
        ("a{2}", "a{2}"),
        ("a?", "a?"),
        ("\\<foo\\>", "\\b{start}foo\\b{end}"),
        ("foo|bar", "foo|bar"),
    ];
    for (pattern, translated) in cases {
        assert_eq!(
            translate_ere(pattern),
            Some(translated.to_owned()),
            "{pattern:?}"
        );
        matches_like_grep(pattern, translated, &["-E"], false);
    }
}

#[test]
fn case_folding_matches_grep_i() {
    let translated = translate_bre("upper").unwrap();
    assert_eq!(translated, "upper");
    matches_like_grep("upper", &translated, &["-i"], true);
}
