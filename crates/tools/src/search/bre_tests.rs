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
fn class_operators_escape_as_members() {
    // Rust reads these as set operators while GNU reads members: the
    // translation escapes them, keeping ranges untouched.
    assert_eq!(bre("[a&&b]"), Some("[a\\&\\&b]".to_owned()));
    assert_eq!(bre("[a&b]"), Some("[a\\&b]".to_owned()));
    assert_eq!(bre("[a~~b]"), Some("[a\\~\\~b]".to_owned()));
    assert_eq!(bre("[a~b]"), Some("[a\\~b]".to_owned()));
    assert_eq!(bre("[a[b]"), Some("[a\\[b]".to_owned()));
    assert_eq!(bre("[a-z]"), Some("[a-z]".to_owned()));
    assert_eq!(bre("[a-]"), Some("[a-]".to_owned()));
    assert_eq!(bre("[[:alpha:]&&b]"), Some("[[:alpha:]\\&\\&b]".to_owned()));
}

#[test]
fn a_double_dash_in_a_class_hands_over() {
    // A `--` there is a range or an error in GNU, never set difference:
    // no escaping keeps both readings, so the system decides.
    assert_eq!(bre("[a--b]"), None);
    assert_eq!(bre("[--a]"), None);
}

#[test]
fn ere_class_operators_escape_as_members() {
    assert_eq!(translate_ere("[a&&b]"), Some("[a\\&\\&b]".to_owned()));
    assert_eq!(translate_ere("[a&b]"), Some("[a\\&b]".to_owned()));
    assert_eq!(translate_ere("[a~~b]"), Some("[a\\~\\~b]".to_owned()));
    assert_eq!(translate_ere("[a~b]"), Some("[a\\~b]".to_owned()));
    assert_eq!(translate_ere("[a[b]"), Some("[a\\[b]".to_owned()));
    assert_eq!(translate_ere("[a-z]"), Some("[a-z]".to_owned()));
    assert_eq!(translate_ere("[a-]"), Some("[a-]".to_owned()));
    assert_eq!(translate_ere("[^ab]"), Some("[^ab]".to_owned()));
    assert_eq!(translate_ere("[]a]"), Some("[]a]".to_owned()));
    assert_eq!(translate_ere("[[:alpha:]]"), Some("[[:alpha:]]".to_owned()));
    // Outside a class the operators stay literal, as before.
    assert_eq!(translate_ere("a&&b"), Some("a&&b".to_owned()));
    assert_eq!(translate_ere("a--b"), Some("a--b".to_owned()));
}

#[test]
fn ere_double_dash_in_a_class_hands_over() {
    assert_eq!(translate_ere("[a--b]"), None);
    assert_eq!(translate_ere("[--a]"), None);
}

#[test]
fn a_dash_after_a_non_dash_stays_a_member() {
    // Only `--` hands over (the tests above): a dash after anything else
    // is a member or a range, in both modes.
    assert_eq!(bre("[a-b]"), Some("[a-b]".to_owned()));
    assert_eq!(bre("[-a]"), Some("[-a]".to_owned()));
    assert_eq!(translate_ere("[a-b]"), Some("[a-b]".to_owned()));
    assert_eq!(translate_ere("[-a]"), Some("[-a]".to_owned()));
}

#[test]
fn a_single_dash_next_to_a_class_chunk_hands_nothing_over() {
    // The `--` handover compares each dash with its neighbour across a
    // `[:...:]` chunk as well: neither pattern holds two dashes in a row,
    // so the translation stays (both still reach the system grep: neither
    // is a valid regex, so `compile` hands over instead).
    assert_eq!(bre("[a-[:alpha:]b]"), Some("[a-[:alpha:]b]".to_owned()));
    assert_eq!(
        translate_ere("[a-[:alpha:]b]"),
        Some("[a-[:alpha:]b]".to_owned())
    );
    // A dash inside the chunk after a non-dash is a member too.
    assert_eq!(bre("[a-[:-:]c]"), Some("[a-[:-:]c]".to_owned()));
    assert_eq!(translate_ere("[a-[:-:]c]"), Some("[a-[:-:]c]".to_owned()));
}

#[test]
fn bre_second_caret_is_a_member() {
    // Only the first caret negates; the runner's grep reads `[^^]` as
    // every character but `^`.
    assert_eq!(bre("[^^]"), Some("[^^]".to_owned()));
    assert_eq!(bre("[^^][ab]"), Some("[^^][ab]".to_owned()));
    assert_eq!(bre("[^]^]"), Some("[^]^]".to_owned()));
}

#[test]
fn ere_second_caret_is_a_member() {
    // The repeated `^` left `fresh` set, so `[^^][ab]` translated to
    // `[^^]\[ab]` and missed `xa`: only the first caret negates now.
    assert_eq!(translate_ere("[^^]"), Some("[^^]".to_owned()));
    assert_eq!(translate_ere("[^^][ab]"), Some("[^^][ab]".to_owned()));
    assert_eq!(translate_ere("[^]^]"), Some("[^]^]".to_owned()));
    assert_eq!(translate_ere("[a^]"), Some("[a^]".to_owned()));
}

#[test]
fn ere_backslash_is_a_member_in_a_class() {
    // Inside brackets a backslash is ordinary, as the runner's grep
    // reads it: `\d` there is a backslash or `d`, never a digit class.
    assert_eq!(translate_ere("[\\]"), Some("[\\\\]".to_owned()));
    assert_eq!(translate_ere("[\\d]"), Some("[\\\\d]".to_owned()));
    assert_eq!(translate_ere("[a\\-z]"), Some("[a\\\\-z]".to_owned()));
    assert_eq!(translate_ere("[a\\]]"), Some("[a\\\\]]".to_owned()));
}

#[test]
fn ere_unterminated_class_hands_over() {
    // An open bracket never closed hands over before anything is read:
    // the system reports it.
    assert_eq!(translate_ere("[a"), None);
    assert_eq!(translate_ere("[]"), None);
    assert_eq!(translate_ere("[^]"), None);
    assert_eq!(translate_ere("[a\\"), None);
}

#[test]
fn class_and_element_openers_hand_over() {
    // A `[` opening a character class (`[:`), a collating element (`[.`)
    // or an equivalence element (`[=`) never reads as a member: the
    // runner's grep errors on an invalid one and runs a valid element,
    // neither of which an escaping keeps, so the system decides.
    for pattern in ["[[.a.]]", "[[=a=]]", "[a[.b]", "[a[=b]", "[a[:b]"] {
        assert_eq!(bre(pattern), None, "{pattern:?}");
        assert_eq!(translate_ere(pattern), None, "{pattern:?}");
    }
}

/// One bracket pattern and its pinned translations: `None` hands over to
/// the system grep before anything is read.
struct BracketCase {
    /// The pattern as given.
    pattern: &'static str,
    /// The BRE translation, or nothing for the handover.
    bre: Option<&'static str>,
    /// The ERE translation, or nothing for the handover.
    ere: Option<&'static str>,
}

/// Every position-dependent bracket rule in one table: negation and a
/// repeated caret, a leading `]` with and without negation, a trailing
/// caret, a literal `[`, a lone `]` class, edge dashes, POSIX classes,
/// set operators, a literal backslash, and the invalid or unsupported
/// classes that hand over. Each runs through BRE and ERE against the
/// same input corpus below.
const BRACKETS: &[BracketCase] = &[
    BracketCase {
        pattern: "[^^]",
        bre: Some("[^^]"),
        ere: Some("[^^]"),
    },
    BracketCase {
        pattern: "[^]]",
        bre: Some("[^]]"),
        ere: Some("[^]]"),
    },
    BracketCase {
        pattern: "[]a]",
        bre: Some("[]a]"),
        ere: Some("[]a]"),
    },
    BracketCase {
        pattern: "[^]a]",
        bre: Some("[^]a]"),
        ere: Some("[^]a]"),
    },
    BracketCase {
        pattern: "[a^]",
        bre: Some("[a^]"),
        ere: Some("[a^]"),
    },
    BracketCase {
        pattern: "[^]^]",
        bre: Some("[^]^]"),
        ere: Some("[^]^]"),
    },
    BracketCase {
        pattern: "[[]",
        bre: Some("[\\[]"),
        ere: Some("[\\[]"),
    },
    BracketCase {
        pattern: "[]]",
        bre: Some("[]]"),
        ere: Some("[]]"),
    },
    BracketCase {
        pattern: "[a-]",
        bre: Some("[a-]"),
        ere: Some("[a-]"),
    },
    BracketCase {
        pattern: "[-a]",
        bre: Some("[-a]"),
        ere: Some("[-a]"),
    },
    BracketCase {
        pattern: "[[:alpha:][:digit:]]",
        bre: Some("[[:alpha:][:digit:]]"),
        ere: Some("[[:alpha:][:digit:]]"),
    },
    BracketCase {
        pattern: "[a&&b]",
        bre: Some("[a\\&\\&b]"),
        ere: Some("[a\\&\\&b]"),
    },
    BracketCase {
        pattern: "[a~~b]",
        bre: Some("[a\\~\\~b]"),
        ere: Some("[a\\~\\~b]"),
    },
    BracketCase {
        pattern: "[a[b]",
        bre: Some("[a\\[b]"),
        ere: Some("[a\\[b]"),
    },
    BracketCase {
        pattern: "[\\]",
        bre: Some("[\\\\]"),
        ere: Some("[\\\\]"),
    },
    BracketCase {
        pattern: "[a\\]]",
        bre: Some("[a\\\\]]"),
        ere: Some("[a\\\\]]"),
    },
    BracketCase {
        pattern: "[\\d]",
        bre: Some("[\\\\d]"),
        ere: Some("[\\\\d]"),
    },
    BracketCase {
        pattern: "[a\\-z]",
        bre: Some("[a\\\\-z]"),
        ere: Some("[a\\\\-z]"),
    },
    BracketCase {
        pattern: "[^^][ab]",
        bre: Some("[^^][ab]"),
        ere: Some("[^^][ab]"),
    },
    BracketCase {
        pattern: "[!ab]",
        bre: Some("[\\!ab]"),
        ere: Some("[!ab]"),
    },
    BracketCase {
        pattern: "[a--b]",
        bre: None,
        ere: None,
    },
    BracketCase {
        pattern: "[[.a.]]",
        bre: None,
        ere: None,
    },
    BracketCase {
        pattern: "[[=a=]]",
        bre: None,
        ere: None,
    },
    BracketCase {
        pattern: "[a[.b]",
        bre: None,
        ere: None,
    },
    BracketCase {
        pattern: "[a[=b]",
        bre: None,
        ere: None,
    },
    BracketCase {
        pattern: "[a[:b]",
        bre: None,
        ere: None,
    },
    BracketCase {
        pattern: "[a",
        bre: None,
        ere: None,
    },
];

/// Checks one bracket translation against the system grep on the bracket
/// corpus: the engine prints the same lines with the same exit code.
/// Only where the runner's grep is GNU; elsewhere the pinned table
/// above carries it.
fn bracket_matches_like_grep(pattern: &str, translated: &str, grep_args: &[&str]) {
    if !gnu_grep() {
        return;
    }
    let corpus = [
        "^", "^^", "^a", "a^", "xa", "xb", "a", "b", "c", "d", "1", "[", "]", "[]", "[]a]",
        "[^]a]", "[ab]", "&", "~", "-", "a&b", "a~b", "a-b", "-a", "a-", "a]", "\\", "a\\b",
        "alpha", "a.a", "a1", " ",
    ];
    let matcher = RegexMatcherBuilder::new().build(translated).unwrap();
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
    let engine_code = if engine.is_empty() { 1 } else { 0 };
    let dir = fakes::TempDir::new("fiber-search-brackets");
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
    assert_eq!(engine, system, "bracket {pattern:?} as {translated:?}");
    assert_eq!(
        engine_code,
        output.status.code().unwrap_or(99),
        "bracket {pattern:?} exit"
    );
}

/// Checks one BRACKETS row in one mode: the pinned translation, then the
/// system grep on the bracket corpus where that grep is GNU. Every row
/// below runs through here under its own name, so a mutant changing one
/// case fails fast under that name instead of hiding in a loop.
fn bracket_case_in_mode(pattern: &str, ere: bool) {
    let case = BRACKETS
        .iter()
        .find(|case| case.pattern == pattern)
        .unwrap_or_else(|| panic!("no BRACKETS row for {pattern:?}"));
    if ere {
        assert_eq!(
            translate_ere(pattern).as_deref(),
            case.ere,
            "ERE {pattern:?}"
        );
        if let Some(translated) = case.ere {
            bracket_matches_like_grep(pattern, translated, &["-E"]);
        }
    } else {
        assert_eq!(
            translate_bre(pattern).as_deref(),
            case.bre,
            "BRE {pattern:?}"
        );
        if let Some(translated) = case.bre {
            bracket_matches_like_grep(pattern, translated, &[]);
        }
    }
}

/// One test per BRACKETS row and mode, generated from the shared table:
/// the table stays the source of the pinned translations, while each case
/// runs (and fails) under its own name.
macro_rules! bracket_tests {
    ($($bre:ident, $ere:ident: $pattern:literal;)*) => {
        $(
            #[test]
            fn $bre() {
                bracket_case_in_mode($pattern, false);
            }
            #[test]
            fn $ere() {
                bracket_case_in_mode($pattern, true);
            }
        )*
    };
}

bracket_tests! {
    bracket_bre_double_caret, bracket_ere_double_caret: "[^^]";
    bracket_bre_negated_closer, bracket_ere_negated_closer: "[^]]";
    bracket_bre_leading_closer, bracket_ere_leading_closer: "[]a]";
    bracket_bre_negated_leading_closer, bracket_ere_negated_leading_closer: "[^]a]";
    bracket_bre_trailing_caret, bracket_ere_trailing_caret: "[a^]";
    bracket_bre_negated_closer_and_caret, bracket_ere_negated_closer_and_caret: "[^]^]";
    bracket_bre_open_member, bracket_ere_open_member: "[[]";
    bracket_bre_lone_closer, bracket_ere_lone_closer: "[]]";
    bracket_bre_trailing_dash, bracket_ere_trailing_dash: "[a-]";
    bracket_bre_leading_dash, bracket_ere_leading_dash: "[-a]";
    bracket_bre_posix_pair, bracket_ere_posix_pair: "[[:alpha:][:digit:]]";
    bracket_bre_double_ampersand, bracket_ere_double_ampersand: "[a&&b]";
    bracket_bre_double_tilde, bracket_ere_double_tilde: "[a~~b]";
    bracket_bre_open_bracket_member, bracket_ere_open_bracket_member: "[a[b]";
    bracket_bre_lone_backslash, bracket_ere_lone_backslash: "[\\]";
    bracket_bre_escaped_closer, bracket_ere_escaped_closer: "[a\\]]";
    bracket_bre_backslash_d, bracket_ere_backslash_d: "[\\d]";
    bracket_bre_escaped_dash, bracket_ere_escaped_dash: "[a\\-z]";
    bracket_bre_double_caret_pair, bracket_ere_double_caret_pair: "[^^][ab]";
    bracket_bre_bang_member, bracket_ere_bang_member: "[!ab]";
    bracket_bre_double_dash, bracket_ere_double_dash: "[a--b]";
    bracket_bre_collating_element, bracket_ere_collating_element: "[[.a.]]";
    bracket_bre_equivalence_element, bracket_ere_equivalence_element: "[[=a=]]";
    bracket_bre_open_collating, bracket_ere_open_collating: "[a[.b]";
    bracket_bre_open_equivalence, bracket_ere_open_equivalence: "[a[=b]";
    bracket_bre_open_class, bracket_ere_open_class: "[a[:b]";
    bracket_bre_unterminated, bracket_ere_unterminated: "[a";
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
fn class_operators_match_grep_line_for_line() {
    // Each case is the BRE pattern, the ERE spelling and the shared
    // pinned translation: GNU reads the operators as members.
    let cases = [
        ("[a&&b]", "[a&&b]", "[a\\&\\&b]"),
        ("[a~~b]", "[a~~b]", "[a\\~\\~b]"),
        ("[a[b]", "[a[b]", "[a\\[b]"),
    ];
    for (bre_pattern, ere_pattern, translated) in cases {
        assert_eq!(
            translate_bre(bre_pattern),
            Some(translated.to_owned()),
            "{bre_pattern:?}"
        );
        assert_eq!(
            translate_ere(ere_pattern),
            Some(translated.to_owned()),
            "{ere_pattern:?}"
        );
        class_matches_like_grep(bre_pattern, translated, &[]);
        class_matches_like_grep(ere_pattern, translated, &["-E"]);
    }
}

/// Checks a class-operator translation against the system grep on lines
/// holding the operators themselves: every corpus line the translated
/// pattern matches through the searcher's own engine is one the system
/// grep prints, and vice versa. Only where the runner's grep is GNU;
/// elsewhere the pinned translations above carry it.
fn class_matches_like_grep(pattern: &str, translated: &str, grep_args: &[&str]) {
    if !gnu_grep() {
        return;
    }
    let corpus = [
        "a", "&", "b", "~", "-", "[", "a&b", "ab", "a-b", "a~b", "[ab]", "c",
    ];
    let matcher = RegexMatcherBuilder::new().build(translated).unwrap();
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
    let dir = fakes::TempDir::new("fiber-search-classes");
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
    assert_eq!(engine, system, "class {pattern:?} as {translated:?}");
}

#[test]
fn case_folding_matches_grep_i() {
    let translated = translate_bre("upper").unwrap();
    assert_eq!(translated, "upper");
    matches_like_grep("upper", &translated, &["-i"], true);
}
