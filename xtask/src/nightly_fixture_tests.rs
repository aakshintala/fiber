use std::path::Path;
use std::process::{Command, Stdio};

use fakes::TempDir;

use super::*;

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} in {}", dir.display());
}

fn tags(dir: &Path) -> Vec<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .arg("tag")
        .arg("--list")
        .output()
        .unwrap();
    assert!(output.status.success());
    let mut listed: Vec<String> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    listed.sort();
    listed
}

/// A repo with one commit carrying `tags`, in tag-creation order.
fn repo_with(tag_names: &[&str]) -> TempDir {
    let dir = TempDir::new("nightly-fixture");
    git(dir.path(), &["init", "-q", "-b", "main"]);
    dir.write("file", "content\n");
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-qm", "base"]);
    for tag in tag_names {
        git(dir.path(), &["tag", tag]);
    }
    dir
}

fn keeps(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn keeps_survive_and_other_tags_are_pruned() {
    let dir = repo_with(&[
        "v0.0.1",
        "0.0.1-nightly.20261103",
        "v0.0.1-nightly.20261103",
        "v0.0.1-nightly.20261104",
        "v0.0.1-nightly.20261105",
        "v0.0.1-nightly.2026110",
    ]);
    let remaining = prune(
        dir.path(),
        &keeps(&["0.0.1-nightly.20261103", "0.0.1-nightly.20261104"]),
    )
    .unwrap();
    assert_eq!(remaining, 2);
    assert_eq!(
        tags(dir.path()),
        [
            "v0.0.1-nightly.20261103".to_owned(),
            "v0.0.1-nightly.20261104".to_owned(),
        ]
    );
}

#[test]
fn prune_mirrors_the_bare_clone_shape() {
    let origin = repo_with(&[
        "v0.0.1",
        "v0.0.1-nightly.20261103",
        "v0.0.1-nightly.20261104",
    ]);
    let parent = TempDir::new("nightly-fixture-clone");
    let bare = parent.path().join("clone");
    git(
        parent.path(),
        &[
            "clone",
            "--bare",
            "--no-checkout",
            origin.path().to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    let remaining = prune(&bare, &keeps(&["0.0.1-nightly.20261104"])).unwrap();
    assert_eq!(remaining, 1);
    assert_eq!(tags(&bare), ["v0.0.1-nightly.20261104".to_owned()]);
}

#[test]
fn missing_keep_errors_naming_it_and_deletes_nothing() {
    let dir = repo_with(&["v0.0.1-nightly.20261103"]);
    let error = prune(dir.path(), &keeps(&["0.0.1-nightly.20261104"])).unwrap_err();
    assert!(error.contains("v0.0.1-nightly.20261104"), "{error}");
    assert!(error.contains("--keep"), "{error}");
    assert_eq!(tags(dir.path()), ["v0.0.1-nightly.20261103".to_owned()]);
}

#[test]
fn non_nightly_keep_errors_before_any_delete() {
    let dir = repo_with(&["v0.0.1", "v0.0.1-nightly.20261103"]);
    let error = prune(dir.path(), &keeps(&["0.0.1"])).unwrap_err();
    assert!(error.contains("--keep 0.0.1"), "{error}");
    assert_eq!(
        tags(dir.path()),
        ["v0.0.1".to_owned(), "v0.0.1-nightly.20261103".to_owned()]
    );
}

#[test]
fn empty_repo_errors() {
    let dir = repo_with(&[]);
    let error = prune(dir.path(), &keeps(&["0.0.1-nightly.20261104"])).unwrap_err();
    assert!(error.contains("v0.0.1-nightly.20261104"), "{error}");
    assert_eq!(tags(dir.path()), Vec::<String>::new());
}

#[test]
fn not_a_repo_errors_naming_the_dir() {
    let dir = TempDir::new("nightly-fixture-plain");
    let error = prune(dir.path(), &keeps(&["0.0.1-nightly.20261104"])).unwrap_err();
    assert!(error.contains(&dir.path().display().to_string()), "{error}");
}

#[test]
fn run_wires_repo_and_keeps() {
    let dir = repo_with(&["v0.0.1", "v0.0.1-nightly.20261103"]);
    assert_eq!(
        run(&[
            "--repo".to_owned(),
            dir.path().to_str().unwrap().to_owned(),
            "--keep".to_owned(),
            "0.0.1-nightly.20261103".to_owned(),
        ]),
        Ok(true)
    );
    assert_eq!(tags(dir.path()), ["v0.0.1-nightly.20261103".to_owned()]);
}

#[test]
fn run_rejects_bad_arity_and_flags() {
    assert!(run(&[]).is_err());
    assert!(run(&["--repo".to_owned(), "/nowhere".to_owned(),]).is_err());
    assert!(
        run(&[
            "--repo".to_owned(),
            "/nowhere".to_owned(),
            "--keep".to_owned(),
        ])
        .is_err()
    );
    assert!(
        run(&[
            "--repo".to_owned(),
            "/nowhere".to_owned(),
            "--keep".to_owned(),
            "0.0.1-nightly.20261104".to_owned(),
            "--prune".to_owned(),
        ])
        .is_err()
    );
}
