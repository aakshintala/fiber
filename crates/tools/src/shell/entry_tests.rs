#![allow(clippy::unwrap_used, reason = "tests unwrap")]

use super::super::syntax::Word;
use super::{Found, find, owned};

fn w(cooked: &str) -> Word {
    Word {
        raw: cooked.to_owned(),
        cooked: cooked.to_owned(),
        substituted: false,
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

fn builtin_found(
    words: &[Word],
    configured: &[(String, Vec<String>)],
) -> Option<(&'static str, Vec<String>)> {
    match find(words, configured) {
        Found::Builtin(entry, flags) => Some((entry.name, flags.to_vec())),
        Found::Configured(_, _) => None,
        Found::None => None,
    }
}

fn builtin_name(words: &[Word], configured: &[(String, Vec<String>)]) -> Option<&'static str> {
    builtin_found(words, configured).map(|(name, _)| name)
}

fn configured_flags(words: &[Word], configured: &[(String, Vec<String>)]) -> Option<Vec<String>> {
    match find(words, configured) {
        Found::Builtin(..) => None,
        Found::Configured(flags, _) => Some(flags.to_vec()),
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
fn a_same_named_configured_entry_merges_its_flags() {
    // A configured entry whose name is the built-in entry's command path
    // word for word adds its flags to the built-in's. The lookup reports
    // both; the classifier walks them together.
    assert_eq!(
        builtin_found(
            &ws(&["git", "status"]),
            &[configured("git status", &["--json"])]
        ),
        Some(("git status", vec!["--json".to_owned()]))
    );
    assert_eq!(
        builtin_found(&ws(&["grep", "-R", "x"]), &[configured("grep", &["-R"])]),
        Some(("grep", vec!["-R".to_owned()]))
    );
    // The person's first same-named entry wins, as with configured-only
    // matches.
    assert_eq!(
        builtin_found(
            &ws(&["git", "status"]),
            &[
                configured("git status", &["--a"]),
                configured("git status", &["--b"])
            ]
        ),
        Some(("git status", vec!["--a".to_owned()]))
    );
    // An empty configured flag list merges nothing.
    assert_eq!(
        builtin_found(&ws(&["cat", "--foo"]), &[configured("cat", &[])]),
        Some(("cat", Vec::new()))
    );
    // With no same-named entry the built-in stands alone.
    assert_eq!(
        builtin_found(&ws(&["git", "status"]), &[configured("jq", &["-r"])]),
        Some(("git status", Vec::new()))
    );
}

#[test]
fn only_an_exact_built_in_key_merges() {
    // A longer configured path never merges into its shorter built-in.
    assert_eq!(
        builtin_found(
            &ws(&["git", "status", "-s"]),
            &[configured("git status -s", &[])]
        ),
        Some(("git status", Vec::new()))
    );
    // A shorter configured name never merges into a longer built-in path.
    assert_eq!(
        builtin_found(&ws(&["git", "status"]), &[configured("git", &["--x"])]),
        Some(("git status", Vec::new()))
    );
    // A path-qualified name never merges into its basename's built-in and
    // is never consulted.
    let entries = [configured("/usr/bin/grep", &["-R"])];
    assert_eq!(
        configured_flags(&ws(&["/usr/bin/grep", "-R", "x"]), &entries),
        None
    );
    assert!(matches!(
        find(&ws(&["/usr/bin/grep", "-R", "x"]), &entries),
        Found::None
    ));
    // An exact-form command with no table entry has nothing to merge into:
    // `git stash list` still matches nothing, so the exact form keeps
    // precedence and the longer call executes.
    let stash = [configured("git stash list", &[])];
    assert!(matches!(
        find(&ws(&["git", "stash", "list"]), &stash),
        Found::None
    ));
    // A table entry with no flag list takes no added flags.
    assert_eq!(
        builtin_found(
            &ws(&["git", "branch", "--show-current"]),
            &[configured("git branch --show-current", &["--foo"])]
        ),
        Some(("git branch --show-current", Vec::new()))
    );
    assert_eq!(
        builtin_found(&ws(&["pwd", "--foo"]), &[configured("pwd", &["--foo"])]),
        Some(("pwd", Vec::new()))
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
        // A wrapper step is never consulted however configured: the
        // classifier unwraps it itself, so no entry widens `xargs` past
        // its six commands or `env` past its flag rule.
        (vec!["xargs", "-0", "x"], "xargs", vec!["-0"]),
        (vec!["env", "-i", "x"], "env", vec!["-i"]),
        (vec!["timeout", "5", "x"], "timeout", Vec::new()),
        (vec!["cd", "x"], "cd", Vec::new()),
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
    // Every subcommand word must equal its part (`docs/tools.md`,
    // "Shell", "Effects": a subcommand with no entry of its own
    // makes the call `executes`): `other` never stands in for `tool`,
    // even with words to spare.
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
fn owned_covers_built_ins_sed_and_exact_paths() {
    for name in [
        "grep",
        "/usr/bin/grep",
        "./rg",
        "/bin/cat",
        "sed",
        "/usr/bin/sed",
        "cd",
        "/usr/bin/cd",
        "env",
        "timeout",
        "xargs",
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
