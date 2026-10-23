#![allow(clippy::unwrap_used, reason = "tests unwrap")]

use super::super::syntax::Word;
use super::{Found, find, owned, skip};

fn w(cooked: &str) -> Word {
    Word {
        raw: cooked.to_owned(),
        cooked: cooked.to_owned(),
    }
}

fn ws(words: &[&str]) -> Vec<Word> {
    words.iter().map(|word| w(word)).collect()
}

fn configured(name: &str, flags: &[&str]) -> (String, Vec<String>) {
    (
        name.to_owned(),
        flags.iter().map(|flag| flag.to_string()).collect(),
    )
}

fn builtin_name(words: &[Word], configured: &[(String, Vec<String>)]) -> Option<&'static str> {
    match find(words, configured) {
        Found::Builtin(entry) => Some(entry.name),
        Found::Configured(_) => None,
        Found::None => None,
    }
}

fn configured_flags(words: &[Word], configured: &[(String, Vec<String>)]) -> Option<Vec<String>> {
    match find(words, configured) {
        Found::Builtin(_) => None,
        Found::Configured(flags) => Some(flags.to_vec()),
        Found::None => None,
    }
}

#[test]
fn the_longest_built_in_path_wins() {
    let empty: Vec<(String, Vec<String>)> = Vec::new();
    assert_eq!(
        builtin_name(&ws(&["git", "status"]), &empty),
        Some("git status")
    );
    assert_eq!(
        builtin_name(&ws(&["git", "status", "-s"]), &empty),
        Some("git status")
    );
    assert_eq!(builtin_name(&ws(&["ls", "-la"]), &empty), Some("ls"));
    assert_eq!(builtin_name(&ws(&["git", "branch"]), &empty), None);
    assert_eq!(builtin_name(&ws(&["frobnicate", "x"]), &empty), None);
    assert_eq!(builtin_name(&[], &empty), None);
}

#[test]
fn a_built_in_entry_beats_configuration_at_or_below_its_path() {
    let longer = [configured("git status -s", &[])];
    assert_eq!(
        builtin_name(&ws(&["git", "status", "-s"]), &longer),
        Some("git status")
    );
    let same = [configured("git status", &["--json"])];
    assert_eq!(
        builtin_name(&ws(&["git", "status"]), &same),
        Some("git status")
    );
    let plain = [configured("grep", &["-R"])];
    assert_eq!(
        builtin_name(&ws(&["grep", "-R", "x"]), &plain),
        Some("grep")
    );
}

#[test]
fn configuration_answers_what_no_built_in_owns() {
    assert_eq!(
        configured_flags(&ws(&["jq", ".", "f"]), &[configured("jq", &[])]),
        Some(Vec::new())
    );
    assert_eq!(
        configured_flags(
            &ws(&["gh", "pr", "status", "--json"]),
            &[configured("gh pr status", &["--json"])]
        ),
        Some(vec!["--json".to_owned()])
    );
    assert_eq!(
        configured_flags(&ws(&["git", "push"]), &[configured("git", &["push"])]),
        Some(vec!["push".to_owned()])
    );
    assert_eq!(
        configured_flags(
            &ws(&["git", "branch", "foo"]),
            &[configured("git branch", &[])]
        ),
        Some(Vec::new())
    );
    assert_eq!(
        configured_flags(&ws(&["", "a", "b"]), &[configured("", &[])]),
        Some(Vec::new())
    );
    assert_eq!(
        configured_flags(&ws(&["tools/", "a"]), &[configured("tools/", &[])]),
        Some(Vec::new())
    );
    assert_eq!(
        configured_flags(
            &ws(&["/opt/tools/jq", ".", "f"]),
            &[configured("/opt/tools/jq", &[])]
        ),
        Some(Vec::new())
    );
}

#[test]
fn an_owned_name_is_never_consulted() {
    // A built-in match answers first, so these rows own no built-in path:
    // the lookup never reaches configuration.
    for (words, name, flags) in [
        (
            vec!["/usr/bin/grep", "-R", "x"],
            "/usr/bin/grep",
            vec!["-R"],
        ),
        (vec!["./rg", "-L", "x"], "./rg", vec!["-L"]),
        (vec!["/bin/cat", "--foo"], "/bin/cat", vec!["--foo"]),
        (vec!["git", "push"], "/usr/bin/git push", Vec::new()),
        (vec!["sed", "-i", "1p", "f"], "sed", vec!["-i"]),
        (
            vec!["/usr/bin/sed", "-n", "1p", "f"],
            "/usr/bin/sed",
            vec!["-n"],
        ),
    ] {
        let entries = [configured(name, &flags)];
        assert!(matches!(find(&ws(&words), &entries), Found::None), "{name}");
    }
    let empty: Vec<(String, Vec<String>)> = Vec::new();
    assert!(matches!(
        find(&ws(&["/usr/bin/git", "status"]), &empty),
        Found::None
    ));
}

#[test]
fn a_configured_subcommand_path_must_match_word_for_word() {
    // Every subcommand word must equal its part: `other` never stands in
    // for `tool`, even with words to spare.
    assert_eq!(
        configured_flags(&ws(&["my", "other"]), &[configured("my tool", &[])]),
        None
    );
    assert_eq!(
        configured_flags(&ws(&["my"]), &[configured("my tool", &[])]),
        None
    );
    assert_eq!(
        configured_flags(&ws(&["my", "tool"]), &[configured("my tool", &[])]),
        Some(Vec::new())
    );
}

#[test]
fn skip_counts_the_command_path() {
    let empty: Vec<(String, Vec<String>)> = Vec::new();
    assert_eq!(skip(&ws(&["git", "status", "x"]), &empty), 2);
    assert_eq!(skip(&ws(&["ls"]), &empty), 1);
    assert_eq!(skip(&ws(&["frobnicate"]), &empty), 0);
    assert_eq!(skip(&[], &empty), 0);
    assert_eq!(skip(&ws(&["jq", ".", "f"]), &[configured("jq", &[])]), 1);
    assert_eq!(
        skip(
            &ws(&["gh", "pr", "status"]),
            &[configured("gh pr status", &["--json"])]
        ),
        3
    );
    assert_eq!(skip(&ws(&["", "a"]), &[configured("", &[])]), 1);
}

#[test]
fn owned_covers_built_ins_sed_and_exact_paths() {
    for name in [
        "grep",
        "/usr/bin/grep",
        "./rg",
        "/bin/cat",
        "sed",
        "/usr/bin/sed",
        "git status",
        "git status -s",
        "git stash list",
        "git branch --show-current",
    ] {
        assert!(owned(name), "{name}");
    }
    for name in [
        "git",
        "git init",
        "git push --dry-run",
        "git branch",
        "git stash",
        "git push",
        "gh pr status",
        "jq",
        "/opt/tools/jq",
        "tools/",
        "",
    ] {
        assert!(!owned(name), "{name}");
    }
}
