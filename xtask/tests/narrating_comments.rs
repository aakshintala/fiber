//! Program-level checks of narrating-comments against disposable Git repositories.

#![allow(
    clippy::unwrap_used,
    reason = "test code may unwrap (docs/code-quality.md, \"Lints\")"
)]

use std::process::{Command, Stdio};
use std::time::Duration;

use fakes::TempDir;

#[path = "common/mod.rs"]
mod common;

fn git(dir: &TempDir, args: &[&str]) {
    let mut command = Command::new("git");
    command
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir.path())
        .stdin(Stdio::null());
    let (code, output) = common::run("git fixture", command, &[], Duration::from_secs(30));
    assert_eq!(code, Some(0), "git {args:?}: {output}");
}

fn setup() -> TempDir {
    let dir = TempDir::new("xtask-narrating-comments");
    git(&dir, &["init", "-q"]);
    dir.write("src/lib.rs", "// previously old\nfn original() {}\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "baseline"]);
    dir
}

fn run(dir: &TempDir, args: &[&str]) -> (Option<i32>, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
    command
        .args(args)
        .current_dir(dir.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .stdin(Stdio::null());
    common::run(
        "xtask narrating-comments",
        command,
        &[],
        Duration::from_secs(30),
    )
}

#[test]
fn reports_new_comment_but_passes_clean_baseline() {
    let dir = setup();
    let (code, out) = run(&dir, &["narrating-comments", "--base", "HEAD"]);
    assert_eq!(code, Some(0), "{out}");
    assert_eq!(out.trim(), "narrating-comments: ok");

    dir.write(
        "src/lib.rs",
        "// previously old\nfn original() {}\n// now uses the cache\n",
    );
    let (code, out) = run(&dir, &["narrating-comments", "--base", "HEAD"]);
    assert_eq!(code, Some(1), "{out}");
    assert_eq!(
        out.lines().collect::<Vec<_>>(),
        ["narrating-comments: src/lib.rs:3: comment narrates history"]
    );
}

#[test]
fn finds_shell_comments_and_untracked_source() {
    let dir = setup();
    dir.write(
        "scripts/new check.sh",
        "#!/bin/sh\necho ok # now uses cache\n",
    );
    let (code, out) = run(&dir, &["narrating-comments", "--base", "HEAD"]);
    assert_eq!(code, Some(1), "{out}");
    assert_eq!(
        out.lines().collect::<Vec<_>>(),
        ["narrating-comments: scripts/new check.sh:2: comment narrates history"]
    );
}

#[test]
fn staged_additions_are_included() {
    let dir = setup();
    dir.write(
        "src/lib.rs",
        "// previously old\nfn original() {}\n// changed from old\n",
    );
    git(&dir, &["add", "src/lib.rs"]);
    let (code, out) = run(&dir, &["narrating-comments", "--base", "HEAD"]);
    assert_eq!(code, Some(1), "{out}");
    assert_eq!(
        out.lines().collect::<Vec<_>>(),
        ["narrating-comments: src/lib.rs:3: comment narrates history"]
    );
}

#[test]
fn committed_additions_are_included_and_deleted_violations_are_not() {
    let dir = setup();
    dir.write(
        "src/lib.rs",
        "// previously old\nfn original() {}\n// changed from old\n",
    );
    git(&dir, &["add", "src/lib.rs"]);
    git(&dir, &["commit", "-qm", "comment"]);
    let (code, out) = run(&dir, &["narrating-comments", "--base", "HEAD~1"]);
    assert_eq!(code, Some(1), "{out}");
    dir.write("src/lib.rs", "fn original() {}\n");
    let (code, out) = run(&dir, &["narrating-comments", "--base", "HEAD"]);
    assert_eq!(code, Some(0), "{out}");
}

#[test]
fn unchanged_history_and_ignored_or_binary_inputs_do_not_fail() {
    let dir = setup();
    dir.write("src/lib.rs", "// previously old\nfn changed_code() {}\n");
    let (code, out) = run(&dir, &["narrating-comments", "--base", "HEAD"]);
    assert_eq!(code, Some(0), "{out}");
    dir.write(".gitignore", "ignored.rs\n");
    git(&dir, &["add", ".gitignore"]);
    git(&dir, &["commit", "-qm", "ignore generated input"]);
    dir.write("ignored.rs", "// now uses cache\n");
    dir.write("generated.rs", "// now uses cache\n\0binary\n");
    let (code, out) = run(&dir, &["narrating-comments", "--base", "HEAD"]);
    assert_eq!(code, Some(0), "{out}");
}

#[test]
fn a_rename_does_not_reintroduce_old_comments_but_new_edits_are_checked() {
    let dir = setup();
    git(&dir, &["mv", "src/lib.rs", "src/renamed.rs"]);
    let (code, out) = run(&dir, &["narrating-comments", "--base", "HEAD"]);
    assert_eq!(code, Some(0), "{out}");
    dir.write(
        "src/renamed.rs",
        "// previously old\nfn original() {}\n// now uses cache\n",
    );
    let (code, out) = run(&dir, &["narrating-comments", "--base", "HEAD"]);
    assert_eq!(code, Some(1), "{out}");
    assert!(
        out.contains("src/renamed.rs:3: comment narrates history"),
        "{out}"
    );
}

#[test]
fn missing_base_and_invalid_revision_are_errors() {
    let dir = setup();
    let (code, out) = run(&dir, &["narrating-comments"]);
    assert_eq!(code, Some(2), "{out}");
    assert!(out.contains("--base is required"), "{out}");
    let (code, out) = run(&dir, &["narrating-comments", "--base", "no-such-revision"]);
    assert_eq!(code, Some(2), "{out}");
    assert!(out.contains("no-such-revision"), "{out}");
    let outside_git = TempDir::new("xtask-narrating-comments-no-git");
    let (code, out) = run(&outside_git, &["narrating-comments", "--base", "HEAD"]);
    assert_eq!(code, Some(2), "{out}");
    assert!(out.contains("git merge-base"), "{out}");
}

fn diagnostics(output: &str) -> Vec<&str> {
    let mut lines = output
        .lines()
        .filter(|line| line.contains("comment narrates history"))
        .collect::<Vec<_>>();
    lines.sort_unstable();
    lines
}

#[test]
fn ruled_phrases_and_boundaries_are_reported_exactly() {
    let dir = setup();
    let lines = [
        concat!("/", "/ changed from old"),
        concat!("/", "/ PREVIOUSLY old"),
        concat!("/", "/ now uses cache"),
        concat!("/", "/ fixed the bug where it broke"),
        concat!("/", "/ (previously!)"),
        concat!("/", "/ unchanged from old"),
        concat!("/", "/ know uses"),
        concat!("/", "/ previously_named"),
        concat!("/", "/ xpreviously"),
        concat!("/", "/ replaced; instead of; used to; was changed"),
        concat!("/", "/ now\t\tuses cache"),
        concat!("/", "/ fixed the\tbug where"),
        concat!("/", "/ now", "\n", "/", "/ uses cache"),
    ];
    let body = lines.join("\n");
    dir.write("src/phrases.rs", &body);
    let (code, output) = run(&dir, &["narrating-comments", "--base", "HEAD"]);
    assert_eq!(code, Some(1), "{output}");
    assert_eq!(
        diagnostics(&output),
        [
            "narrating-comments: src/phrases.rs:11: comment narrates history",
            "narrating-comments: src/phrases.rs:12: comment narrates history",
            "narrating-comments: src/phrases.rs:1: comment narrates history",
            "narrating-comments: src/phrases.rs:2: comment narrates history",
            "narrating-comments: src/phrases.rs:3: comment narrates history",
            "narrating-comments: src/phrases.rs:4: comment narrates history",
            "narrating-comments: src/phrases.rs:5: comment narrates history",
        ]
    );
}

#[test]
fn added_hunks_keep_new_side_coordinates_and_ignore_deletions() {
    let dir = setup();
    dir.write(
        "src/lib.rs",
        concat!(
            "/",
            "/ previously old\n",
            "fn one() {}\n",
            "fn two() {}\n",
            "fn three() {}\n",
            "fn four() {}\n",
            "fn five() {}\n",
        ),
    );
    git(&dir, &["add", "src/lib.rs"]);
    dir.write(
        "src/lib.rs",
        concat!(
            "/",
            "/ previously old\n",
            "fn one() {}\n",
            "fn two() {}\n",
            "fn three() {}\n",
            "/",
            "/ now uses cache\n",
            "fn four() {}\n",
            "fn five() {}\n",
            "/",
            "/ changed from old\n",
        ),
    );
    let (code, output) = run(&dir, &["narrating-comments", "--base", "HEAD"]);
    assert_eq!(code, Some(1), "{output}");
    assert_eq!(
        diagnostics(&output),
        [
            "narrating-comments: src/lib.rs:5: comment narrates history",
            "narrating-comments: src/lib.rs:8: comment narrates history",
        ]
    );
}

#[test]
fn stateless_comment_forms_and_hash_language_scope_are_reported() {
    let dir = setup();
    let fixtures = [
        (
            "src/forms.rs",
            concat!(
                "/",
                "/ previously",
                "\n",
                "let s = r#\"",
                "/",
                "/ now uses cache\"#;\n",
                "let s = \"",
                "/",
                "/ changed from old\";\n",
                "/* previously */\n",
                "* now uses cache\n",
                "continuation previously\n",
                "code /* previously */\n",
                "code ",
                "/",
                "/ fixed the bug where\n"
            ),
        ),
        (
            "tools/check.sh",
            concat!(
                "echo x#previously\n",
                "echo ok # previously\n",
                "printf '%s' ' # now uses cache'\n",
                "cat <<'END'\n",
                "# changed from old\n",
                "END\n",
                "plain heredoc text\n",
                "echo x#ignored # now uses cache\n"
            ),
        ),
        ("data.py", "value = ' # previously'\n"),
        ("settings.toml", "value = \" # now uses cache\"\n"),
        ("manifest.yaml", "value: ' # changed from old'\n"),
        (
            "manifest.yml",
            "value: \" # fixed the bug where it failed\"\n",
        ),
        ("notes.txt", concat!("/", "/ previously\n")),
    ];
    for (path, text) in fixtures {
        dir.write(path, text);
    }
    git(
        &dir,
        &[
            "add",
            "src/forms.rs",
            "tools/check.sh",
            "data.py",
            "settings.toml",
            "manifest.yaml",
            "manifest.yml",
            "notes.txt",
        ],
    );
    let (code, output) = run(&dir, &["narrating-comments", "--base", "HEAD"]);
    assert_eq!(code, Some(1), "{output}");
    assert_eq!(
        diagnostics(&output),
        [
            "narrating-comments: data.py:1: comment narrates history",
            "narrating-comments: manifest.yaml:1: comment narrates history",
            "narrating-comments: manifest.yml:1: comment narrates history",
            "narrating-comments: settings.toml:1: comment narrates history",
            "narrating-comments: src/forms.rs:1: comment narrates history",
            "narrating-comments: src/forms.rs:2: comment narrates history",
            "narrating-comments: src/forms.rs:3: comment narrates history",
            "narrating-comments: src/forms.rs:4: comment narrates history",
            "narrating-comments: src/forms.rs:5: comment narrates history",
            "narrating-comments: src/forms.rs:8: comment narrates history",
            "narrating-comments: tools/check.sh:2: comment narrates history",
            "narrating-comments: tools/check.sh:3: comment narrates history",
            "narrating-comments: tools/check.sh:5: comment narrates history",
            "narrating-comments: tools/check.sh:8: comment narrates history",
        ]
    );
}
