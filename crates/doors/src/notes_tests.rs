//! Tests for `propose_notes` session evidence: counts and project
//! attribution across sessions.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::json;

use super::*;
use contract::notes::Evidence;

fn write_log(dir: &Path, bytes: &[u8]) {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join("events.jsonl"), bytes).unwrap();
}

fn session_dir(home: &Path, key: &str, id: &str) -> PathBuf {
    home.join("projects").join(key).join("sessions").join(id)
}

fn line(kind: &str, session: &str, seq: u64, payload: &serde_json::Value) -> String {
    json!({
        "kind": kind,
        "session_id": session,
        "ts": seq,
        "schema_version": 1,
        "seq": seq,
        "payload": payload,
    })
    .to_string()
        + "\n"
}

fn started(workspace: &str) -> String {
    json!({"kind": "session_started", "payload": {"workspace": workspace}}).to_string() + "\n"
}

fn pair(session: &str, seq: u64, req: &str, prefix: &str) -> String {
    let request = json!({
        "request_id": req,
        "step": "review",
        "escalation": {"cause": "consecutive_blocks", "reason": "blocked"},
        "rule": {"subject": prefix, "prefix": prefix},
    });
    let resolved = json!({
        "request_id": req,
        "decision": "allow",
        "decided_by": "person",
    });
    line("permission_requested", session, seq, &request)
        + &line("permission_resolved", session, seq + 1, &resolved)
}

/// A seeded session holding one counted escalation per prefix.
fn seed(home: &Path, key: &str, id: &str, workspace: &str, prefixes: &[&str]) {
    let mut log = started(workspace);
    for (index, prefix) in prefixes.iter().enumerate() {
        let seq = u64::try_from(index).unwrap() * 2 + 1;
        log.push_str(&pair(id, seq, &format!("{id}-r{index}"), prefix));
    }
    write_log(&session_dir(home, key, id), log.as_bytes());
}

/// `git -C dir args`, with the redirecting variables removed.
fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .arg("-C")
        .arg(dir)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .stdin(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
}

/// A git repository with one commit: `root/name`, held by the caller.
fn repo(root: &Path, name: &str) -> PathBuf {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "--quiet"]);
    git(&dir, &["commit", "--quiet", "--allow-empty", "-m", "first"]);
    dir
}

fn session_items(evidence: &[Evidence]) -> Vec<(String, u32, u32, String)> {
    evidence
        .iter()
        .filter_map(|item| match item {
            Evidence::Session(tally) => Some((
                tally.prefix().to_owned(),
                tally.count(),
                tally.here(),
                tally.text().to_owned(),
            )),
            Evidence::Files { .. } | Evidence::Answer { .. } => None,
        })
        .collect()
}

#[test]
fn counts_span_projects_with_here_only_for_this_one() {
    let root = fakes::TempDir::new("doors-notes-projects");
    let home = root.path().join("home");
    let here_repo = repo(root.path(), "here");
    let away_repo = repo(root.path(), "away");
    let here_key = log::project_key(&worktree::project(&here_repo));
    let away_key = log::project_key(&worktree::project(&away_repo));
    assert_ne!(here_key, away_key);
    seed(
        &home,
        &here_key,
        "s_here",
        here_repo.to_str().unwrap(),
        &["gh pr merge", "gh pr merge"],
    );
    seed(
        &home,
        &away_key,
        "s_away",
        away_repo.to_str().unwrap(),
        &["gh pr merge"],
    );
    let dir = session_dir(&home, &here_key, "s_live");
    let found = session_items(&gather(&home, &dir, &here_repo));
    assert_eq!(
        found,
        [(
            "gh pr merge".to_owned(),
            3,
            2,
            "3 escalations answered allow for `gh pr merge`".to_owned()
        )]
    );
}

#[test]
fn a_shared_slug_still_files_by_resolved_identity() {
    let root = fakes::TempDir::new("doors-notes-slug");
    let home = root.path().join("home");
    // `<tmp>/a-b` and `<tmp>/a/b` share one project slug.
    let outside = repo(root.path(), "a-b");
    let inner = repo(&root.path().join("a"), "b");
    let left = log::project_key(&worktree::project(&outside));
    let right = log::project_key(&worktree::project(&inner));
    assert_eq!(left, right);
    seed(
        &home,
        &left,
        "s_left",
        outside.to_str().unwrap(),
        &["gh pr merge"],
    );
    seed(
        &home,
        &right,
        "s_right",
        inner.to_str().unwrap(),
        &["gh pr merge"],
    );
    let dir = session_dir(&home, &right, "s_live");
    let found = session_items(&gather(&home, &dir, &inner));
    assert_eq!(
        found,
        [(
            "gh pr merge".to_owned(),
            2,
            1,
            "2 escalations answered allow for `gh pr merge`".to_owned()
        )]
    );
}

#[test]
fn a_recorded_workspace_that_is_gone_counts_away() {
    let root = fakes::TempDir::new("doors-notes-gone");
    let home = root.path().join("home");
    let here_repo = repo(root.path(), "here");
    let here_key = log::project_key(&worktree::project(&here_repo));
    let gone = root.path().join("gone");
    seed(
        &home,
        &here_key,
        "s_gone",
        gone.to_str().unwrap(),
        &["gh pr merge"],
    );
    seed(
        &home,
        &here_key,
        "s_here",
        here_repo.to_str().unwrap(),
        &["gh pr merge"],
    );
    let dir = session_dir(&home, &here_key, "s_live");
    let found = session_items(&gather(&home, &dir, &here_repo));
    assert_eq!(
        found,
        [(
            "gh pr merge".to_owned(),
            2,
            1,
            "2 escalations answered allow for `gh pr merge`".to_owned()
        )]
    );
}

#[test]
fn one_escalation_reads_singular() {
    let root = fakes::TempDir::new("doors-notes-singular");
    let home = root.path().join("home");
    let here_repo = repo(root.path(), "here");
    let here_key = log::project_key(&worktree::project(&here_repo));
    seed(
        &home,
        &here_key,
        "s_one",
        here_repo.to_str().unwrap(),
        &["make lint"],
    );
    let dir = session_dir(&home, &here_key, "s_live");
    let found = session_items(&gather(&home, &dir, &here_repo));
    assert_eq!(
        found,
        [(
            "make lint".to_owned(),
            1,
            1,
            "1 escalation answered allow for `make lint`".to_owned()
        )]
    );
}

#[test]
fn session_items_sort_by_count_then_prefix() {
    let root = fakes::TempDir::new("doors-notes-order");
    let home = root.path().join("home");
    let here_repo = repo(root.path(), "here");
    let here_key = log::project_key(&worktree::project(&here_repo));
    let workspace = here_repo.to_str().unwrap();
    seed(
        &home,
        &here_key,
        "s_a",
        workspace,
        &["b", "a", "c", "b", "a", "c"],
    );
    seed(
        &home,
        &here_key,
        "s_b",
        workspace,
        &["x", "x", "x", "z", "z", "y"],
    );
    let dir = session_dir(&home, &here_key, "s_live");
    let found = session_items(&gather(&home, &dir, &here_repo));
    let order: Vec<(&str, u32)> = found
        .iter()
        .map(|(prefix, count, _, _)| (prefix.as_str(), *count))
        .collect();
    assert_eq!(
        order,
        [("x", 3), ("a", 2), ("b", 2), ("c", 2), ("z", 2), ("y", 1),]
    );
}

fn capped_home(root: &Path, count: usize) -> (PathBuf, PathBuf, PathBuf) {
    let home = root.join("home");
    let workspace = repo(root, "here");
    let key = log::project_key(&worktree::project(&workspace));
    let mut log = started(workspace.to_str().unwrap());
    for index in 0..count {
        let prefix = format!("p{index:02}");
        let seq = u64::try_from(index).unwrap() * 2 + 1;
        log.push_str(&pair("s_cap", seq, &format!("r{index}"), &prefix));
    }
    write_log(&session_dir(&home, &key, "s_cap"), log.as_bytes());
    let dir = session_dir(&home, &key, "s_live");
    (home, workspace, dir)
}

#[test]
fn sixty_three_prefixes_are_all_kept() {
    let root = fakes::TempDir::new("doors-notes-cap63");
    let (home, workspace, dir) = capped_home(root.path(), 63);
    let found = session_items(&gather(&home, &dir, &workspace));
    assert_eq!(found.len(), 63);
    assert_eq!(found.first().unwrap().0, "p00");
    assert_eq!(found.last().unwrap().0, "p62");
}

#[test]
fn sixty_four_prefixes_are_all_kept() {
    let root = fakes::TempDir::new("doors-notes-cap64");
    let (home, workspace, dir) = capped_home(root.path(), 64);
    let found = session_items(&gather(&home, &dir, &workspace));
    assert_eq!(found.len(), 64);
    assert_eq!(found.first().unwrap().0, "p00");
    assert_eq!(found.last().unwrap().0, "p63");
}

#[test]
fn sixty_five_prefixes_trim_to_sixty_four_in_order() {
    let root = fakes::TempDir::new("doors-notes-cap65");
    let (home, workspace, dir) = capped_home(root.path(), 65);
    let found = session_items(&gather(&home, &dir, &workspace));
    assert_eq!(found.len(), 64);
    assert_eq!(found.first().unwrap().0, "p00");
    assert_eq!(found.last().unwrap().0, "p63");
}

fn write_file(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn files_text(evidence: &[Evidence]) -> Vec<String> {
    evidence
        .iter()
        .filter_map(|item| match item {
            Evidence::Files { text } => Some(text.clone()),
            Evidence::Session(_) | Evidence::Answer { .. } => None,
        })
        .collect()
}

/// A launch directory that is a git repository with one remote.
fn remote_repo(root: &Path, name: &str, remote: &str, url: &str) -> PathBuf {
    let workspace = repo(root, name);
    git(&workspace, &["remote", "add", remote, url]);
    workspace
}

#[test]
fn remote_urls_strip_userinfo_and_keep_scp_form() {
    let root = fakes::TempDir::new("doors-notes-remote");
    let workspace = remote_repo(
        root.path(),
        "work",
        "origin",
        "https://user:token@example.com/acme/fiber.git",
    );
    git(
        &workspace,
        &["remote", "add", "admin", "git@example.com:acme/other.git"],
    );
    let found = files_text(&files_evidence(&workspace));
    assert_eq!(
        found,
        [
            "git remote admin: git@example.com:acme/other.git".to_owned(),
            "git remote origin: https://example.com/acme/fiber.git".to_owned(),
        ]
    );
    for item in &found {
        assert!(!item.contains("user"), "no userinfo: {item}");
        assert!(!item.contains("token"), "no token: {item}");
    }
}

#[test]
fn an_unreadable_git_dir_gives_no_remote_item_with_ci_still_present() {
    let root = fakes::TempDir::new("doors-notes-r36");
    let workspace = repo(root.path(), "work");
    write_file(
        &workspace.join(".gitlab-ci.yml"),
        "job:\n  script: make lint\n",
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(workspace.join(".git"), fs::Permissions::from_mode(0o000)).unwrap();
        let found = files_text(&files_evidence(&workspace));
        fs::set_permissions(workspace.join(".git"), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            found.iter().all(|item| !item.starts_with("git remote")),
            "no remote item: {found:?}"
        );
        assert!(
            found
                .iter()
                .any(|item| item.starts_with("CI configuration")),
            "the CI item still shows: {found:?}"
        );
    }
}

#[test]
fn remote_urls_keep_only_the_fetch_url_when_push_differs() {
    let root = fakes::TempDir::new("doors-notes-push");
    let workspace = repo(root.path(), "work");
    git(
        &workspace,
        &["remote", "add", "origin", "git@example.com:acme/fiber.git"],
    );
    git(
        &workspace,
        &[
            "remote",
            "set-url",
            "--push",
            "origin",
            "git@example.com:acme/push.git",
        ],
    );
    assert_eq!(
        super::remote_urls(&workspace),
        [(
            "origin".to_owned(),
            "git@example.com:acme/fiber.git".to_owned()
        )]
    );
}

#[test]
fn a_non_zero_remote_listing_gives_no_remote_item() {
    let root = fakes::TempDir::new("doors-notes-remote-fail");
    let plain = root.path().join("plain");
    fs::create_dir_all(&plain).unwrap();
    assert_eq!(super::remote_urls(&plain), Vec::new());
}

#[test]
fn an_npmrc_gives_only_its_stripped_registry_lines() {
    let root = fakes::TempDir::new("doors-notes-npmrc");
    let workspace = root.path().join("work");
    write_file(
        &workspace.join(".npmrc"),
        "registry=https://user:pass@registry.example.com/npm/\n\
         @acme:registry=https://npm.example.com/\n\
         //registry.example.com/:_authToken=SECRET-TOKEN\n",
    );
    let found = files_text(&files_evidence(&workspace));
    assert_eq!(
        found,
        [
            "registry .npmrc: https://registry.example.com/npm/".to_owned(),
            "registry .npmrc: https://npm.example.com/".to_owned(),
        ]
    );
    for item in &found {
        assert!(!item.contains("SECRET-TOKEN"), "no token line: {item}");
    }
}

#[test]
fn an_at_scoped_non_registry_key_is_excluded() {
    let root = fakes::TempDir::new("doors-notes-npmrc-scope");
    let workspace = root.path().join("work");
    write_file(
        &workspace.join(".npmrc"),
        "@acme:registry=https://npm.example.com/\n\
         @acme:authToken=SECRET-TOKEN\n",
    );
    let found = files_text(&files_evidence(&workspace));
    assert_eq!(
        found,
        ["registry .npmrc: https://npm.example.com/".to_owned()]
    );
}

#[test]
fn all_registry_files_give_only_their_registry_lines() {
    let root = fakes::TempDir::new("doors-notes-registries");
    let workspace = root.path().join("work");
    write_file(
        &workspace.join(".yarnrc.yml"),
        "npmRegistryServer: \"https://yarn:user@example.com/npm\"\n\
         otherKey: secret\n",
    );
    write_file(
        &workspace.join(".cargo/config.toml"),
        "[registries.acme]\n\
         index = \"https://cargo:secret@example.com/index\"\n\
         other = \"secret\"\n\
         [profile]\n\
         opt-level = 3\n",
    );
    write_file(
        &workspace.join("pip.conf"),
        "[global]\n\
         index-url = https://pip:secret@example.com/simple\n\
         extra-index-url = https://user:pass@extra.example.com/simple\n\
         trusted-host = example.com\n",
    );
    let found = files_text(&files_evidence(&workspace));
    assert_eq!(
        found,
        [
            "registry .yarnrc.yml: https://example.com/npm".to_owned(),
            "registry .cargo/config.toml: https://example.com/index".to_owned(),
            "registry pip.conf: https://example.com/simple".to_owned(),
            "registry pip.conf: https://extra.example.com/simple".to_owned(),
        ]
    );
    for item in &found {
        assert!(!item.contains("secret"), "no secret: {item}");
        assert!(!item.contains("user"), "no userinfo: {item}");
    }
}

#[test]
fn registry_and_registries_sections_are_both_kept() {
    let root = fakes::TempDir::new("doors-notes-cargo-sections");
    let workspace = root.path().join("work");
    write_file(
        &workspace.join(".cargo/config.toml"),
        "[registry]\n\
         index = \"https://cargo:secret@example.com/index\"\n\
         [registries.acme]\n\
         registry = \"https://cargo:secret@example.com/acme\"\n",
    );
    let found = files_text(&files_evidence(&workspace));
    assert_eq!(
        found,
        [
            "registry .cargo/config.toml: https://example.com/index".to_owned(),
            "registry .cargo/config.toml: https://example.com/acme".to_owned(),
        ]
    );
}

#[test]
fn a_non_registry_key_in_a_registry_section_is_excluded() {
    let root = fakes::TempDir::new("doors-notes-cargo-key");
    let workspace = root.path().join("work");
    write_file(
        &workspace.join(".cargo/config.toml"),
        "[registry]\n\
         other = \"secret\"\n\
         registry = \"https://cargo:secret@example.com/r\"\n",
    );
    let found = files_text(&files_evidence(&workspace));
    assert_eq!(
        found,
        ["registry .cargo/config.toml: https://example.com/r".to_owned()]
    );
}

#[test]
fn a_ci_file_strips_userinfo_from_its_urls() {
    let root = fakes::TempDir::new("doors-notes-ci-url");
    let workspace = root.path().join("work");
    write_file(
        &workspace.join(".gitlab-ci.yml"),
        "job:\n  script: curl https://bot:secret@ci.example.com/script.sh\n",
    );
    let found = files_text(&files_evidence(&workspace));
    assert_eq!(found.len(), 1);
    assert!(
        found[0].contains("https://ci.example.com/script.sh"),
        "{}",
        found[0]
    );
    assert!(!found[0].contains("bot:secret"), "{}", found[0]);
}

fn ci_home(root: &Path, count: usize) -> PathBuf {
    let workspace = root.join("work");
    for index in 0..count {
        write_file(
            &workspace
                .join(".github/workflows")
                .join(format!("job{index:02}.yml")),
            &format!("name: job{index:02}\n"),
        );
    }
    workspace
}

#[test]
fn seven_ci_files_are_all_kept() {
    let root = fakes::TempDir::new("doors-notes-ci7");
    let workspace = ci_home(root.path(), 7);
    let found = files_text(&files_evidence(&workspace));
    assert_eq!(found.len(), 7);
    assert!(found[0].contains("job00.yml"), "{}", found[0]);
    assert!(found[6].contains("job06.yml"), "{}", found[6]);
}

#[test]
fn eight_ci_files_are_all_kept() {
    let root = fakes::TempDir::new("doors-notes-ci8");
    let workspace = ci_home(root.path(), 8);
    let found = files_text(&files_evidence(&workspace));
    assert_eq!(found.len(), 8);
    assert!(found[0].contains("job00.yml"), "{}", found[0]);
    assert!(found[7].contains("job07.yml"), "{}", found[7]);
}

#[test]
fn nine_ci_files_trim_to_eight_in_path_order() {
    let root = fakes::TempDir::new("doors-notes-ci9");
    let workspace = ci_home(root.path(), 9);
    let found = files_text(&files_evidence(&workspace));
    assert_eq!(found.len(), 8);
    assert!(found[0].contains("job00.yml"), "{}", found[0]);
    assert!(found[7].contains("job07.yml"), "{}", found[7]);
    assert!(
        found.iter().all(|item| !item.contains("job08")),
        "{found:?}"
    );
}

#[test]
fn a_non_yml_workflow_file_is_excluded() {
    let root = fakes::TempDir::new("doors-notes-ci-ext");
    let workspace = root.path().join("work");
    write_file(&workspace.join(".github/workflows/job.yml"), "name: job\n");
    write_file(
        &workspace.join(".github/workflows/other.yaml"),
        "name: other\n",
    );
    write_file(
        &workspace.join(".github/workflows/notes.txt"),
        "not a workflow\n",
    );
    let found = files_text(&files_evidence(&workspace));
    assert_eq!(found.len(), 2);
    assert!(found[0].contains("job.yml"), "{}", found[0]);
    assert!(found[1].contains("other.yaml"), "{}", found[1]);
    assert!(
        found.iter().all(|item| !item.contains("notes.txt")),
        "{found:?}"
    );
}

#[test]
fn links_outside_the_workspace_give_no_item_but_an_inner_link_reads() {
    let root = fakes::TempDir::new("doors-notes-links");
    let workspace = root.path().join("work");
    let outside = root.path().join("outside");
    write_file(&outside.join(".gitlab-ci.yml"), "name: outside\n");
    write_file(
        &outside.join(".npmrc"),
        "registry=https://outside.example.com/\n",
    );
    fs::create_dir_all(outside.join("flows")).unwrap();
    write_file(&outside.join("flows/evil.yml"), "name: evil\n");
    write_file(&workspace.join("inner.yml"), "name: inner\n");
    fs::create_dir_all(workspace.join(".github")).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            outside.join(".gitlab-ci.yml"),
            workspace.join(".gitlab-ci.yml"),
        )
        .unwrap();
        std::os::unix::fs::symlink(outside.join(".npmrc"), workspace.join(".npmrc")).unwrap();
        std::os::unix::fs::symlink(outside.join("flows"), workspace.join(".github/workflows"))
            .unwrap();
        std::os::unix::fs::symlink(
            workspace.join("inner.yml"),
            workspace.join("azure-pipelines.yml"),
        )
        .unwrap();
        let found = files_text(&files_evidence(&workspace));
        assert!(
            !found.iter().any(|item| item.contains("outside")),
            "{found:?}"
        );
        assert!(!found.iter().any(|item| item.contains("evil")), "{found:?}");
        assert_eq!(found.len(), 1);
        assert!(found[0].contains("inner"), "{}", found[0]);
    }
}

#[test]
fn a_directory_and_a_fifo_where_ci_files_belong_give_no_item() {
    let root = fakes::TempDir::new("doors-notes-nonregular");
    let workspace = root.path().join("work");
    // A FIFO opened for reading would block, so a passing test proves
    // non-regular paths are never opened.
    fs::create_dir_all(workspace.join(".gitlab-ci.yml")).unwrap();
    let fifo = workspace.join("azure-pipelines.yml");
    fs::create_dir_all(workspace.as_path()).unwrap();
    fakes::fifo(&fifo);
    let found = files_text(&files_evidence(&workspace));
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn item_text_cuts_to_4096_bytes_at_a_character_boundary() {
    assert_eq!(super::cut_text(&"a".repeat(4095)).len(), 4095);
    assert_eq!(super::cut_text(&"a".repeat(4096)).len(), 4096);
    assert_eq!(super::cut_text(&"a".repeat(4097)).len(), 4096);
    // A cut landing inside `é` (two bytes) ends before it.
    let text = "a".repeat(4095) + "é";
    assert_eq!(super::cut_text(&text).len(), 4095);
    let text = "a".repeat(4096) + "é";
    assert_eq!(super::cut_text(&text).len(), 4096);
}

#[test]
fn a_long_ci_file_cuts_its_item_to_4096_bytes() {
    let root = fakes::TempDir::new("doors-notes-long");
    let workspace = root.path().join("work");
    write_file(&workspace.join(".gitlab-ci.yml"), &"x".repeat(6000));
    let found = files_text(&files_evidence(&workspace));
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].len(), 4096);
}

#[test]
fn outside_git_ci_and_registry_show_with_no_remote_item() {
    let root = fakes::TempDir::new("doors-notes-norepo");
    let workspace = root.path().join("work");
    write_file(&workspace.join(".gitlab-ci.yml"), "name: plain\n");
    write_file(
        &workspace.join(".npmrc"),
        "registry=https://plain.example.com/\n",
    );
    let found = files_text(&files_evidence(&workspace));
    assert_eq!(
        found,
        [
            "CI configuration .gitlab-ci.yml:\nname: plain\n".to_owned(),
            "registry .npmrc: https://plain.example.com/".to_owned(),
        ]
    );
}
