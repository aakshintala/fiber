use super::{branch_exists, git, repo};

#[test]
fn repo_has_branch_main_with_one_commit() {
    let home = repo("fakes-git");
    assert_eq!(
        git(home.path(), &["rev-parse", "--abbrev-ref", "HEAD"]),
        "main"
    );
    assert_eq!(git(home.path(), &["rev-list", "--count", "HEAD"]), "1");
    assert_eq!(
        std::fs::read_to_string(home.path().join("file.txt")).unwrap(),
        "x"
    );
}

#[test]
fn branch_exists_reports_a_present_and_an_absent_branch() {
    let home = repo("fakes-git-exists");
    assert!(branch_exists(home.path(), "main"));
    assert!(!branch_exists(home.path(), "no-such-branch"));
}

#[test]
#[should_panic(expected = "git")]
fn git_panics_on_a_failing_command() {
    let home = repo("fakes-git-fails");
    git(home.path(), &["rev-parse", "no-such-rev"]);
}
