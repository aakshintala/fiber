use std::path::{Path, PathBuf};

use contract::shapes::Effect;

use super::super::classify::classify;
use super::{MAX_GITFILE_BYTES, repository_ok};

fn workspace(prefix: &str) -> (fakes::TempDir, PathBuf) {
    let dir = fakes::TempDir::new(prefix);
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    (dir, workspace)
}

fn make_git_dir(workspace: &Path) {
    std::fs::create_dir_all(workspace.join(".git")).unwrap();
}

fn make_bare(workspace: &Path) {
    std::fs::write(workspace.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::create_dir_all(workspace.join("objects")).unwrap();
    std::fs::create_dir_all(workspace.join("refs")).unwrap();
}

fn write_gitfile(workspace: &Path, content: &str) {
    std::fs::write(workspace.join(".git"), content).unwrap();
}

#[test]
fn a_git_directory_reads() {
    let (_dir, workspace) = workspace("fiber-shell-git-dir");
    make_git_dir(&workspace);
    assert!(repository_ok(&workspace));
    let nested = workspace.join("sub");
    std::fs::create_dir_all(&nested).unwrap();
    assert!(repository_ok(&nested));
}

#[test]
fn no_repository_reads() {
    let (_dir, workspace) = workspace("fiber-shell-git-none");
    assert!(repository_ok(&workspace));
}

#[test]
fn a_missing_workdir_reads() {
    let (_dir, workspace) = workspace("fiber-shell-git-missing");
    assert!(repository_ok(&workspace.join("nope")));
}

#[test]
fn a_missing_workdir_beneath_a_bare_repository_reads() {
    let (_dir, workspace) = workspace("fiber-shell-git-missing-bare");
    make_bare(&workspace);
    assert!(repository_ok(&workspace.join("missing")));
    let effects = classify("git status", &workspace.join("missing"), &[]);
    assert_eq!(effects.declared.effects, vec![Effect::Reads]);
}

#[test]
fn a_bare_repository_executes() {
    let (_dir, workspace) = workspace("fiber-shell-git-bare");
    make_bare(&workspace);
    assert!(!repository_ok(&workspace));
    let nested = workspace.join("sub");
    std::fs::create_dir_all(&nested).unwrap();
    assert!(!repository_ok(&nested));
}

#[test]
fn a_worktree_gitfile_reads() {
    let (_dir, workspace) = workspace("fiber-shell-git-worktree");
    let repo = workspace.join("repo");
    let target = repo.join(".git").join("worktrees").join("wt");
    std::fs::create_dir_all(&target).unwrap();
    let checkout = workspace.join("checkout");
    std::fs::create_dir_all(&checkout).unwrap();
    let absolute = format!("gitdir: {}\n", target.to_str().unwrap());
    write_gitfile(&checkout, &absolute);
    assert!(repository_ok(&checkout));
    let relative = "gitdir: ../repo/.git/worktrees/wt\n";
    write_gitfile(&checkout, relative);
    assert!(repository_ok(&checkout));
}

#[test]
fn an_unverifiable_gitfile_executes() {
    let (_dir, workspace) = workspace("fiber-shell-git-bad");
    // An absolute target with no `.git` component, a missing target, a
    // file target, and a worktree target inside a bare `x.git` names no
    // verifiable repository.
    let elsewhere = workspace.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let bare = workspace.join("x.git");
    std::fs::create_dir_all(&bare).unwrap();
    make_bare(&bare);
    let file = workspace.join("file");
    std::fs::write(&file, "data\n").unwrap();
    let cases = [
        format!("gitdir: {}\n", elsewhere.to_str().unwrap()),
        format!("gitdir: {}/missing\n", workspace.to_str().unwrap()),
        format!("gitdir: {}\n", file.to_str().unwrap()),
        format!("gitdir: {}\n", bare.to_str().unwrap()),
        // Malformed lines: no prefix, no space, two lines, no newline
        // split, and an empty target.
        "hello\n".to_owned(),
        format!("gitdir:{}\n", elsewhere.to_str().unwrap()),
        format!(
            "gitdir: {}\ngitdir: {}\n",
            elsewhere.to_str().unwrap(),
            elsewhere.to_str().unwrap()
        ),
        "gitdir: \n".to_owned(),
        // An embedded NUL never resolves.
        "gitdir: /tmp/\0/bad\n".to_owned(),
    ];
    for content in cases {
        write_gitfile(&workspace, &content);
        assert!(!repository_ok(&workspace), "{content:?}");
    }
}

#[test]
fn partial_bare_markers_read() {
    let (_dir, workspace) = workspace("fiber-shell-git-partial");
    std::fs::write(workspace.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    assert!(repository_ok(&workspace));
    std::fs::create_dir_all(workspace.join("objects")).unwrap();
    assert!(repository_ok(&workspace));
}

#[test]
fn an_unreadable_git_shape_executes() {
    let (_dir, workspace) = workspace("fiber-shell-git-shape");
    // A socket `.git` is neither a directory, a file nor a symlink, so
    // the call is reviewed.
    let _socket = std::os::unix::net::UnixListener::bind(workspace.join(".git")).unwrap();
    assert!(!repository_ok(&workspace));
}

#[test]
fn a_non_utf8_gitfile_executes() {
    let (_dir, workspace) = workspace("fiber-shell-git-utf8");
    std::fs::write(workspace.join(".git"), b"gitdir: \xff\n").unwrap();
    assert!(!repository_ok(&workspace));
}

#[test]
fn a_symlink_git_executes() {
    let (_dir, workspace) = workspace("fiber-shell-git-symlink");
    let repo = workspace.join("repo");
    make_git_dir(&repo);
    std::os::unix::fs::symlink(repo.join(".git"), workspace.join(".git")).unwrap();
    assert!(!repository_ok(&workspace));
}

#[test]
fn a_nul_workdir_executes() {
    assert!(!repository_ok(Path::new("/tmp/\0/bad")));
}

#[test]
fn the_gitfile_size_cap_holds_at_512_bytes() {
    let (_dir, workspace) = workspace("fiber-shell-git-size");
    let repo = workspace.join("repo");
    make_git_dir(&repo);
    let base = format!("{}/.git", repo.to_str().unwrap());
    // The target is padded with trailing slashes, which change nothing
    // about the directory it names.
    for (size, reads) in [(511, true), (512, true), (513, false)] {
        let target = format!("{base}{}", "/".repeat(size - 9 - base.len()));
        let content = format!("gitdir: {target}\n");
        assert_eq!(content.len(), size, "{size}");
        assert!(content.len() <= MAX_GITFILE_BYTES + 1);
        write_gitfile(&workspace, &content);
        assert_eq!(repository_ok(&workspace), reads, "{size}");
    }
}

#[test]
fn git_reads_in_a_repository_and_executes_in_a_bare_one() {
    let (_dir, repo) = workspace("fiber-shell-git-classify-repo");
    make_git_dir(&repo);
    let effects = classify("git status", &repo, &[]);
    assert_eq!(effects.declared.effects, vec![Effect::Reads]);
    let effects = classify("git log --oneline x", &repo, &[]);
    assert_eq!(effects.declared.effects, vec![Effect::Reads]);
    assert_eq!(
        effects.declared.paths,
        Some(vec![repo.join("x").to_str().unwrap().to_owned()])
    );
    let (_bare_dir, bare) = workspace("fiber-shell-git-classify-bare");
    make_bare(&bare);
    for command in ["git status", "git log --oneline x"] {
        let effects = classify(command, &bare, &[]);
        assert_eq!(
            effects.declared.effects,
            vec![Effect::Executes],
            "{command}"
        );
        assert!(effects.declared.paths.is_none(), "{command}");
    }
}

#[test]
fn a_configured_git_entry_takes_the_same_guards() {
    let entries = [("git archive".to_owned(), Vec::new())];
    let (_dir, repo) = workspace("fiber-shell-git-configured-repo");
    make_git_dir(&repo);
    let effects = classify("git archive x", &repo, &entries);
    assert_eq!(effects.declared.effects, vec![Effect::Reads]);
    let (_bare_dir, bare) = workspace("fiber-shell-git-configured-bare");
    make_bare(&bare);
    let effects = classify("git archive x", &bare, &entries);
    assert_eq!(effects.declared.effects, vec![Effect::Executes]);
    assert!(effects.declared.paths.is_none());
}
