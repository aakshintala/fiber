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
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;

use super::*;
use contract::emit::Emit;
use contract::notes::Evidence;
use contract::provider::ToolDefinition;
use contract::shapes::{DeclaredEffects, Effect};
use contract::tool::{Cancel, Effects, EffectsError, Output, Tool};

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
            Evidence::Allows(tally) => Some((
                tally.subject().to_owned(),
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
    let found = session_items(&gather(
        &home,
        &dir,
        &here_repo,
        Topic::RoutineActions,
        None,
    ));
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
    let found = session_items(&gather(&home, &dir, &inner, Topic::RoutineActions, None));
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
    let found = session_items(&gather(
        &home,
        &dir,
        &here_repo,
        Topic::RoutineActions,
        None,
    ));
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
    let found = session_items(&gather(
        &home,
        &dir,
        &here_repo,
        Topic::RoutineActions,
        None,
    ));
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
    let found = session_items(&gather(
        &home,
        &dir,
        &here_repo,
        Topic::RoutineActions,
        None,
    ));
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
    let found = session_items(&gather(
        &home,
        &dir,
        &workspace,
        Topic::RoutineActions,
        None,
    ));
    assert_eq!(found.len(), 63);
    assert_eq!(found.first().unwrap().0, "p00");
    assert_eq!(found.last().unwrap().0, "p62");
}

#[test]
fn sixty_four_prefixes_are_all_kept() {
    let root = fakes::TempDir::new("doors-notes-cap64");
    let (home, workspace, dir) = capped_home(root.path(), 64);
    let found = session_items(&gather(
        &home,
        &dir,
        &workspace,
        Topic::RoutineActions,
        None,
    ));
    assert_eq!(found.len(), 64);
    assert_eq!(found.first().unwrap().0, "p00");
    assert_eq!(found.last().unwrap().0, "p63");
}

#[test]
fn sixty_five_prefixes_trim_to_sixty_four_in_order() {
    let root = fakes::TempDir::new("doors-notes-cap65");
    let (home, workspace, dir) = capped_home(root.path(), 65);
    let found = session_items(&gather(
        &home,
        &dir,
        &workspace,
        Topic::RoutineActions,
        None,
    ));
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
            Evidence::Session(_) | Evidence::Allows(_) | Evidence::Answer { .. } => None,
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

// --------------------------------------- read-only allow tallies

/// One stubbed classification: the subject and prefix, or an arguments
/// error for an unclassifiable command.
type Verdict<'a> = Result<(&'a str, &'a str), &'a str>;

/// The stub's owned table: each command with its subject and prefix, or
/// its arguments error.
type OwnedTable = std::collections::HashMap<String, Result<(String, String), String>>;

/// A stub shell: each command returns its tabled subject and prefix, or
/// an arguments error, counting every call to pin the per-gather cache.
/// The arguments hold the command alone: no logged workdir ever travels.
struct StubShell {
    name: String,
    table: Mutex<OwnedTable>,
    calls: AtomicUsize,
}

impl StubShell {
    fn new(name: &str, table: &[(&str, Verdict)]) -> Self {
        Self {
            name: name.to_owned(),
            table: Mutex::new(
                table
                    .iter()
                    .map(|(command, verdict)| {
                        (
                            command.to_string(),
                            verdict
                                .as_ref()
                                .map(|(subject, prefix)| (subject.to_string(), prefix.to_string()))
                                .map_err(|error| (*error).to_owned()),
                        )
                    })
                    .collect(),
            ),
            calls: AtomicUsize::new(0),
        }
    }
}

impl Tool for StubShell {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name.clone(),
            description: "a stub shell".to_owned(),
            input_schema: serde_json::Value::Object(serde_json::Map::new()),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(
        &self,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Effects, EffectsError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        assert_eq!(
            arguments.len(),
            1,
            "command-only classification: {arguments:?}"
        );
        let command = arguments
            .get("command")
            .and_then(serde_json::Value::as_str)
            .expect("a command string");
        match self
            .table
            .lock()
            .expect("the table lock")
            .get(command)
            .cloned()
        {
            Some(Ok((subject, prefix))) => Ok(Effects {
                declared: DeclaredEffects {
                    effects: vec![Effect::Reads],
                    reversible: true,
                    paths: None,
                    confined: false,
                },
                subject: Some(subject),
                prefix: Some(prefix),
                always_reviewed: false,
                hosts: None,
            }),
            Some(Err(_)) | None => Err(EffectsError::Arguments("unclassifiable".to_owned())),
        }
    }

    fn run(
        &self,
        _arguments: &serde_json::Map<String, serde_json::Value>,
        _cancel: &dyn Cancel,
        _emit: &dyn Emit,
    ) -> Output {
        panic!("the stub never runs");
    }
}

/// One log line with its action: the join key for one shell call.
fn aline(kind: &str, action: &str, seq: u64, payload: &serde_json::Value) -> String {
    json!({
        "kind": kind,
        "session_id": "s_1",
        "action_id": action,
        "ts": seq,
        "schema_version": 1,
        "seq": seq,
        "payload": payload,
    })
    .to_string()
        + "\n"
}

/// A session holding one shell-call script.
fn seed_calls(home: &Path, key: &str, id: &str, workspace: &str, body: &str) {
    write_log(
        &session_dir(home, key, id),
        format!("{}{}", started(workspace), body).as_bytes(),
    );
}

/// Allowed shell calls: `reviewer` allows, with `ran_by` inner calls when
/// marked.
fn allow_calls(
    actions: &[&str],
    command: &str,
    decided_by: &str,
    ran_by: bool,
    first_seq: u64,
) -> String {
    let mut body = String::new();
    for (at, action) in actions.iter().enumerate() {
        let seq = first_seq + u64::try_from(at).unwrap() * 2;
        let mut requested = json!({"name": "shell", "arguments": {"command": command}});
        if ran_by {
            requested["ran_by"] = json!({"extension": "e", "command_id": "c_1"});
        }
        body.push_str(&aline("tool_call_requested", action, seq, &requested));
        body.push_str(&aline(
            "permission_resolved",
            action,
            seq + 1,
            &json!({"decision": "allow", "decided_by": decided_by}),
        ));
    }
    body
}

/// One denied shell call: `requested` ran, an optional request judged it,
/// and the resolution denied it.
fn deny_call(
    action: &str,
    seq: u64,
    command: &str,
    decided_by: &str,
    rule_subject: Option<&str>,
    reviewed: Option<&str>,
) -> String {
    let mut body = aline(
        "tool_call_requested",
        action,
        seq,
        &json!({"name": "shell", "arguments": {"command": command}}),
    );
    if rule_subject.is_some() {
        let mut request = json!({"request_id": "r_1", "step": "review"});
        if let Some(rule_subject) = rule_subject {
            request["rule"] = json!({"subject": rule_subject, "prefix": rule_subject});
        }
        body.push_str(&aline("permission_requested", action, seq + 1, &request));
    }
    let mut resolved = json!({"decision": "deny", "decided_by": decided_by});
    if rule_subject.is_some() {
        resolved["request_id"] = json!("r_1");
    }
    if let Some(reviewed) = reviewed {
        resolved["arguments"] = json!({"command": reviewed});
    }
    body.push_str(&aline("permission_resolved", action, seq + 2, &resolved));
    body
}

/// One blocked shell call: the request escalated, answered allow.
fn block_call(action: &str, seq: u64, command: &str, ran_by: bool) -> String {
    let mut requested = json!({"name": "shell", "arguments": {"command": command}});
    if ran_by {
        requested["ran_by"] = json!({"extension": "e", "command_id": "c_1"});
    }
    aline("tool_call_requested", action, seq, &requested)
        + &aline(
            "permission_requested",
            action,
            seq + 1,
            &json!({"request_id": "r_1", "step": "review",
                "escalation": {"cause": "consecutive_blocks", "reason": "blocked"}}),
        )
        + &aline(
            "permission_resolved",
            action,
            seq + 2,
            &json!({"request_id": "r_1", "decision": "allow", "decided_by": "person"}),
        )
}

/// The allow tallies in `evidence`, in gather order.
fn allows_items(evidence: &[Evidence]) -> Vec<(String, u32, u32, String)> {
    evidence
        .iter()
        .filter_map(|item| match item {
            Evidence::Allows(tally) => Some((
                tally.subject().to_owned(),
                tally.count(),
                tally.here(),
                tally.text().to_owned(),
            )),
            Evidence::Session(_) | Evidence::Files { .. } | Evidence::Answer { .. } => None,
        })
        .collect()
}

/// Gathers the read-only topic with `shell`, through a workspace repo.
fn gather_allows(home: &Path, key: &str, workspace: &Path, shell: &StubShell) -> Vec<Evidence> {
    let dir = session_dir(home, key, "s_live");
    gather(home, &dir, workspace, Topic::ReadOnlyCommands, Some(shell))
}

#[test]
fn five_reviewer_allows_propose_with_their_text_and_counts() {
    // Five is the counting threshold (docs/permissions.md, "Setting up the
    // notes": "A subject counts once it has at least 5 allows").
    let root = fakes::TempDir::new("doors-notes-query");
    let home = root.path().join("home");
    let workspace = repo(root.path(), "here");
    let key = log::project_key(&worktree::project(&workspace));
    seed_calls(
        &home,
        &key,
        "s_1",
        workspace.to_str().unwrap(),
        &allow_calls(
            &["a_1", "a_2", "a_3", "a_4", "a_5"],
            "acme-cli query --json",
            "reviewer",
            false,
            1,
        ),
    );
    let shell = StubShell::new(
        "shell",
        &[(
            "acme-cli query --json",
            Ok(("acme-cli query --json", "acme-cli query")),
        )],
    );
    let found = allows_items(&gather_allows(&home, &key, &workspace, &shell));
    assert_eq!(
        found,
        [(
            "acme-cli query --json".to_owned(),
            5,
            5,
            "5 reviewer allows for acme-cli query --json".to_owned()
        )]
    );
    assert_eq!(
        shell.calls.load(Ordering::Relaxed),
        1,
        "one classification per gather"
    );
}

#[test]
fn four_allows_stay_silent_while_five_count() {
    // The five-allow minimum (docs/permissions.md, "Setting up the notes":
    // "A subject counts once it has at least 5 allows").
    let root = fakes::TempDir::new("doors-notes-threshold");
    let home = root.path().join("home");
    let workspace = repo(root.path(), "here");
    let key = log::project_key(&worktree::project(&workspace));
    seed_calls(
        &home,
        &key,
        "s_1",
        workspace.to_str().unwrap(),
        &format!(
            "{}{}",
            allow_calls(
                &["a_1", "a_2", "a_3", "a_4"],
                "kubectl -n x",
                "reviewer",
                false,
                1
            ),
            allow_calls(
                &["b_1", "b_2", "b_3", "b_4", "b_5"],
                "acme-cli status",
                "reviewer",
                false,
                9
            ),
        ),
    );
    let shell = StubShell::new(
        "shell",
        &[
            ("kubectl -n x", Ok(("kubectl -n x", "kubectl -n"))),
            (
                "acme-cli status",
                Ok(("acme-cli status", "acme-cli status")),
            ),
        ],
    );
    let found = allows_items(&gather_allows(&home, &key, &workspace, &shell));
    assert_eq!(
        found,
        [(
            "acme-cli status".to_owned(),
            5,
            5,
            "5 reviewer allows for acme-cli status".to_owned()
        )]
    );
}

#[test]
fn a_deny_withholds_with_the_covered_reason_and_a_person_deny_too() {
    let root = fakes::TempDir::new("doors-notes-deny");
    let home = root.path().join("home");
    let workspace = repo(root.path(), "here");
    let key = log::project_key(&worktree::project(&workspace));
    seed_calls(
        &home,
        &key,
        "s_1",
        workspace.to_str().unwrap(),
        &format!(
            "{}{}",
            allow_calls(
                &["a_1", "a_2", "a_3", "a_4", "a_5"],
                "acme-cli query --json",
                "reviewer",
                false,
                1
            ),
            deny_call("a_6", 11, "acme-cli query --json", "reviewer", None, None),
        ),
    );
    seed_calls(
        &home,
        &key,
        "s_2",
        workspace.to_str().unwrap(),
        &format!(
            "{}{}",
            allow_calls(
                &["b_1", "b_2", "b_3", "b_4", "b_5"],
                "acme-cli show --json",
                "reviewer",
                false,
                1
            ),
            deny_call(
                "b_6",
                11,
                "acme-cli show --json",
                "person",
                Some("acme-cli show --json"),
                None
            ),
        ),
    );
    let shell = StubShell::new(
        "shell",
        &[
            (
                "acme-cli query --json",
                Ok(("acme-cli query --json", "acme-cli query")),
            ),
            (
                "acme-cli show --json",
                Ok(("acme-cli show --json", "acme-cli show")),
            ),
        ],
    );
    let found = allows_items(&gather_allows(&home, &key, &workspace, &shell));
    assert_eq!(found.len(), 2);
    for (_, _, _, text) in &found {
        assert!(
            text.ends_with(
                " (not proposed: a call it would cover was denied, blocked or escalated)"
            ),
            "{text}"
        );
    }
}

#[test]
fn builtin_held_and_configured_subjects_stay_silent() {
    let root = fakes::TempDir::new("doors-notes-held");
    let home = root.path().join("home");
    let workspace = repo(root.path(), "here");
    let key = log::project_key(&worktree::project(&workspace));
    seed_calls(
        &home,
        &key,
        "s_1",
        workspace.to_str().unwrap(),
        &format!(
            "{}{}",
            allow_calls(
                &["a_1", "a_2", "a_3", "a_4", "a_5"],
                "ls",
                "reviewer",
                false,
                1
            ),
            allow_calls(
                &["b_1", "b_2", "b_3", "b_4", "b_5"],
                "acme-cli query --json",
                "reviewer",
                false,
                11
            ),
        ),
    );
    // An already-configured ancestor covers the candidate silently.
    let project = config::ProjectKey::new(key.clone()).unwrap();
    let file = config::project_config_file(&home, &project);
    write_file(
        &file,
        r#"{"shell": {"read_only": {"acme-cli": {"flags": []}}}}"#,
    );
    let shell = StubShell::new(
        "shell",
        &[
            ("ls", Ok(("ls", "ls"))),
            (
                "acme-cli query --json",
                Ok(("acme-cli query --json", "acme-cli query")),
            ),
        ],
    );
    assert!(allows_items(&gather_allows(&home, &key, &workspace, &shell)).is_empty());
}

#[test]
fn write_flagged_subjects_drop_while_literal_words_propose() {
    let root = fakes::TempDir::new("doors-notes-write-flag");
    let home = root.path().join("home");
    let workspace = repo(root.path(), "here");
    let key = log::project_key(&worktree::project(&workspace));
    seed_calls(
        &home,
        &key,
        "s_1",
        workspace.to_str().unwrap(),
        &format!(
            "{}{}",
            allow_calls(
                &["a_1", "a_2", "a_3", "a_4", "a_5"],
                "acme-cli query --output=out",
                "reviewer",
                false,
                1
            ),
            allow_calls(
                &["b_1", "b_2", "b_3", "b_4", "b_5"],
                "acme-cli query -oevil",
                "reviewer",
                false,
                11
            ),
        ),
    );
    let shell = StubShell::new(
        "shell",
        &[
            (
                "acme-cli query --output=out",
                Ok(("acme-cli query --output=out", "acme-cli query")),
            ),
            (
                "acme-cli query -oevil",
                Ok(("acme-cli query -oevil", "acme-cli query")),
            ),
        ],
    );
    let found = allows_items(&gather_allows(&home, &key, &workspace, &shell));
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].0, "acme-cli query -oevil");
}

#[test]
fn one_word_valued_and_slash_subjects_stay_counted() {
    let root = fakes::TempDir::new("doors-notes-stays");
    let home = root.path().join("home");
    let workspace = repo(root.path(), "here");
    let key = log::project_key(&worktree::project(&workspace));
    seed_calls(
        &home,
        &key,
        "s_1",
        workspace.to_str().unwrap(),
        &format!(
            "{}{}{}",
            allow_calls(
                &["a_1", "a_2", "a_3", "a_4", "a_5"],
                "gist /tmp",
                "reviewer",
                false,
                1
            ),
            allow_calls(
                &["b_1", "b_2", "b_3", "b_4", "b_5"],
                "acme-cli query --exec=ls",
                "reviewer",
                false,
                11
            ),
            allow_calls(
                &["c_1", "c_2", "c_3", "c_4", "c_5"],
                "acme-cli sub/path",
                "reviewer",
                false,
                21
            ),
        ),
    );
    let shell = StubShell::new(
        "shell",
        &[
            ("gist /tmp", Ok(("gist /tmp", "gist"))),
            (
                "acme-cli query --exec=ls",
                Ok(("acme-cli query --exec=ls", "acme-cli query")),
            ),
            ("acme-cli sub/path", Ok(("acme-cli sub/path", "acme-cli"))),
        ],
    );
    let found = allows_items(&gather_allows(&home, &key, &workspace, &shell));
    assert_eq!(found.len(), 3);
    for (_, _, _, text) in &found {
        assert!(!text.contains("not proposed"), "{text}");
    }
}

#[test]
fn allowed_redirects_keep_their_subject_while_other_redirects_drop() {
    let root = fakes::TempDir::new("doors-notes-redirect");
    let home = root.path().join("home");
    let workspace = repo(root.path(), "here");
    let key = log::project_key(&worktree::project(&workspace));
    seed_calls(
        &home,
        &key,
        "s_1",
        workspace.to_str().unwrap(),
        &format!(
            "{}{}",
            allow_calls(
                &["a_1", "a_2", "a_3", "a_4", "a_5"],
                "acme-cli query --json >/dev/null",
                "reviewer",
                false,
                1
            ),
            allow_calls(
                &["b_1", "b_2", "b_3", "b_4", "b_5"],
                "acme-cli query --json 2>out.txt",
                "reviewer",
                false,
                11
            ),
        ),
    );
    let shell = StubShell::new(
        "shell",
        &[
            (
                "acme-cli query --json >/dev/null",
                Ok(("acme-cli query --json >/dev/null", "acme-cli query")),
            ),
            (
                "acme-cli query --json 2>out.txt",
                Ok(("acme-cli query --json 2>out.txt", "acme-cli query")),
            ),
        ],
    );
    let found = allows_items(&gather_allows(&home, &key, &workspace, &shell));
    assert_eq!(found.len(), 1);
    // The tally counts the exact classifier subject, redirect included;
    // the loop strips the allowed redirect when it renders the entry.
    assert_eq!(found[0].0, "acme-cli query --json >/dev/null");
    assert_eq!(
        found[0].3,
        "5 reviewer allows for acme-cli query --json >/dev/null"
    );
}

#[test]
fn whitespace_variants_share_one_subject_tally() {
    let root = fakes::TempDir::new("doors-notes-whitespace");
    let home = root.path().join("home");
    let workspace = repo(root.path(), "here");
    let key = log::project_key(&worktree::project(&workspace));
    // Three allows with single spaces and two with doubled spaces: the
    // classifier names one subject, so all five count together
    // (docs/permissions.md, "Setting up the notes": "Subjects are
    // counted by their exact text").
    seed_calls(
        &home,
        &key,
        "s_1",
        workspace.to_str().unwrap(),
        &format!(
            "{}{}",
            allow_calls(
                &["a_1", "a_2", "a_3"],
                "acme-cli query --json",
                "reviewer",
                false,
                1
            ),
            allow_calls(
                &["a_4", "a_5"],
                "acme-cli  query  --json",
                "reviewer",
                false,
                7
            ),
        ),
    );
    let shell = StubShell::new(
        "shell",
        &[
            (
                "acme-cli query --json",
                Ok(("acme-cli query --json", "acme-cli query")),
            ),
            (
                "acme-cli  query  --json",
                Ok(("acme-cli query --json", "acme-cli query")),
            ),
        ],
    );
    let found = allows_items(&gather_allows(&home, &key, &workspace, &shell));
    assert_eq!(
        found,
        [(
            "acme-cli query --json".to_owned(),
            5,
            5,
            "5 reviewer allows for acme-cli query --json".to_owned()
        )]
    );
}

#[test]
fn an_unquoted_pipe_stays_silent() {
    let root = fakes::TempDir::new("doors-notes-pipe");
    let home = root.path().join("home");
    let workspace = repo(root.path(), "here");
    let key = log::project_key(&worktree::project(&workspace));
    seed_calls(
        &home,
        &key,
        "s_1",
        workspace.to_str().unwrap(),
        &allow_calls(
            &["a_1", "a_2", "a_3", "a_4", "a_5"],
            "acme-cli query | grep x",
            "reviewer",
            false,
            1,
        ),
    );
    let shell = StubShell::new("shell", &[("acme-cli query | grep x", Err("pipe"))]);
    assert!(allows_items(&gather_allows(&home, &key, &workspace, &shell)).is_empty());
    assert_eq!(shell.calls.load(Ordering::Relaxed), 1);
}

#[test]
fn ran_by_allows_alone_propose_nothing_but_ran_by_denies_withhold() {
    let root = fakes::TempDir::new("doors-notes-ran-by");
    let home = root.path().join("home");
    let workspace = repo(root.path(), "here");
    let key = log::project_key(&worktree::project(&workspace));
    seed_calls(
        &home,
        &key,
        "s_1",
        workspace.to_str().unwrap(),
        &format!(
            "{}{}",
            allow_calls(
                &["a_1", "a_2", "a_3", "a_4", "a_5"],
                "acme-cli show",
                "reviewer",
                true,
                1
            ),
            allow_calls(
                &["b_1", "b_2", "b_3", "b_4", "b_5"],
                "acme-cli query --json",
                "reviewer",
                false,
                11
            ),
        ),
    );
    seed_calls(
        &home,
        &key,
        "s_2",
        workspace.to_str().unwrap(),
        &deny_call("c_1", 1, "acme-cli query --json", "reviewer", None, None),
    );
    // The ran_by deny below joins the same subject through the cache.
    let shell = StubShell::new(
        "shell",
        &[
            ("acme-cli show", Ok(("acme-cli show", "acme-cli show"))),
            (
                "acme-cli query --json",
                Ok(("acme-cli query --json", "acme-cli query")),
            ),
        ],
    );
    let found = allows_items(&gather_allows(&home, &key, &workspace, &shell));
    assert_eq!(found.len(), 1);
    assert!(found[0].3.contains("not proposed"), "{}", found[0].3);
}

#[test]
fn a_reviewer_block_on_a_ran_by_call_withholds() {
    let root = fakes::TempDir::new("doors-notes-block");
    let home = root.path().join("home");
    let workspace = repo(root.path(), "here");
    let key = log::project_key(&worktree::project(&workspace));
    seed_calls(
        &home,
        &key,
        "s_1",
        workspace.to_str().unwrap(),
        &format!(
            "{}{}",
            allow_calls(
                &["a_1", "a_2", "a_3", "a_4", "a_5"],
                "acme-cli query --json",
                "reviewer",
                false,
                1
            ),
            block_call("a_6", 11, "acme-cli query --json", true),
        ),
    );
    let shell = StubShell::new(
        "shell",
        &[(
            "acme-cli query --json",
            Ok(("acme-cli query --json", "acme-cli query")),
        )],
    );
    let found = allows_items(&gather_allows(&home, &key, &workspace, &shell));
    assert_eq!(found.len(), 1);
    assert!(found[0].3.contains("not proposed"), "{}", found[0].3);
}

#[test]
fn a_rule_subject_attributes_a_hook_rewritten_deny_and_spares_outsiders() {
    let root = fakes::TempDir::new("doors-notes-rule");
    let home = root.path().join("home");
    let workspace = repo(root.path(), "here");
    let key = log::project_key(&worktree::project(&workspace));
    // The judged subject covers the query candidate, not the outsider.
    seed_calls(
        &home,
        &key,
        "s_1",
        workspace.to_str().unwrap(),
        &format!(
            "{}{}{}",
            allow_calls(
                &["a_1", "a_2", "a_3", "a_4", "a_5"],
                "acme-cli query --json",
                "reviewer",
                false,
                1
            ),
            allow_calls(
                &["b_1", "b_2", "b_3", "b_4", "b_5"],
                "other-cli query --json",
                "reviewer",
                false,
                11
            ),
            deny_call(
                "c_1",
                21,
                "acme-cli query --xml",
                "reviewer",
                Some("acme-cli query"),
                Some("acme-cli query --json"),
            ),
        ),
    );
    let shell = StubShell::new(
        "shell",
        &[
            (
                "acme-cli query --json",
                Ok(("acme-cli query --json", "acme-cli query")),
            ),
            (
                "other-cli query --json",
                Ok(("other-cli query --json", "other-cli query")),
            ),
        ],
    );
    let found = allows_items(&gather_allows(&home, &key, &workspace, &shell));
    assert_eq!(found.len(), 2);
    let query = found
        .iter()
        .find(|(subject, _, _, _)| subject == "acme-cli query --json")
        .unwrap();
    assert!(query.3.contains("not proposed"), "{}", query.3);
    let outsider = found
        .iter()
        .find(|(subject, _, _, _)| subject == "other-cli query --json")
        .unwrap();
    assert!(!outsider.3.contains("not proposed"), "{}", outsider.3);
}

#[test]
fn an_unparseable_deny_containing_the_command_withholds() {
    let root = fakes::TempDir::new("doors-notes-unparseable");
    let home = root.path().join("home");
    let workspace = repo(root.path(), "here");
    let key = log::project_key(&worktree::project(&workspace));
    seed_calls(
        &home,
        &key,
        "s_1",
        workspace.to_str().unwrap(),
        &format!(
            "{}{}",
            allow_calls(
                &["a_1", "a_2", "a_3", "a_4", "a_5"],
                "acme-cli query --json",
                "reviewer",
                false,
                1
            ),
            deny_call("a_6", 11, "((acme-cli query --json", "reviewer", None, None),
        ),
    );
    let shell = StubShell::new(
        "shell",
        &[
            (
                "acme-cli query --json",
                Ok(("acme-cli query --json", "acme-cli query")),
            ),
            ("((acme-cli query --json", Err("syntax")),
        ],
    );
    let found = allows_items(&gather_allows(&home, &key, &workspace, &shell));
    assert_eq!(found.len(), 1);
    assert!(found[0].3.contains("not proposed"), "{}", found[0].3);
}

#[test]
fn an_outside_classified_deny_spares_the_candidate() {
    let root = fakes::TempDir::new("doors-notes-outside");
    let home = root.path().join("home");
    let workspace = repo(root.path(), "here");
    let key = log::project_key(&worktree::project(&workspace));
    // A denied `echo` names the candidate words, but the entry would never
    // cover an `echo` call: only the command path and flags disqualify
    // (docs/permissions.md, "Setting up the notes").
    seed_calls(
        &home,
        &key,
        "s_1",
        workspace.to_str().unwrap(),
        &format!(
            "{}{}",
            allow_calls(
                &["a_1", "a_2", "a_3", "a_4", "a_5"],
                "acme-cli query --json",
                "reviewer",
                false,
                1
            ),
            deny_call(
                "a_6",
                11,
                "echo acme-cli query --json",
                "reviewer",
                None,
                None
            ),
        ),
    );
    let shell = StubShell::new(
        "shell",
        &[
            (
                "acme-cli query --json",
                Ok(("acme-cli query --json", "acme-cli query")),
            ),
            (
                "echo acme-cli query --json",
                Ok(("echo acme-cli query --json", "echo acme-cli")),
            ),
        ],
    );
    let found = allows_items(&gather_allows(&home, &key, &workspace, &shell));
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].0, "acme-cli query --json");
    assert!(!found[0].3.contains("not proposed"), "{}", found[0].3);
}

#[test]
fn a_narrower_denied_command_withholds() {
    let root = fakes::TempDir::new("doors-notes-narrower");
    let home = root.path().join("home");
    let workspace = repo(root.path(), "here");
    let key = log::project_key(&worktree::project(&workspace));
    // The denied call runs under the candidate path with the same flags:
    // the entry would cover it, so it withholds.
    seed_calls(
        &home,
        &key,
        "s_1",
        workspace.to_str().unwrap(),
        &format!(
            "{}{}",
            allow_calls(
                &["a_1", "a_2", "a_3", "a_4", "a_5"],
                "acme-cli query --json",
                "reviewer",
                false,
                1
            ),
            deny_call(
                "a_6",
                11,
                "acme-cli query status --json",
                "reviewer",
                None,
                None
            ),
        ),
    );
    let shell = StubShell::new(
        "shell",
        &[
            (
                "acme-cli query --json",
                Ok(("acme-cli query --json", "acme-cli query")),
            ),
            (
                "acme-cli query status --json",
                Ok(("acme-cli query status --json", "acme-cli query")),
            ),
        ],
    );
    let found = allows_items(&gather_allows(&home, &key, &workspace, &shell));
    assert_eq!(found.len(), 1);
    assert!(found[0].3.contains("not proposed"), "{}", found[0].3);
}

#[test]
fn a_flag_differing_deny_spares_the_candidate() {
    let root = fakes::TempDir::new("doors-notes-flag-differ");
    let home = root.path().join("home");
    let workspace = repo(root.path(), "here");
    let key = log::project_key(&worktree::project(&workspace));
    // Same command path, but the denied call's flag is not one the entry
    // would hold: the entry would not cover it, so it spares.
    seed_calls(
        &home,
        &key,
        "s_1",
        workspace.to_str().unwrap(),
        &format!(
            "{}{}",
            allow_calls(
                &["a_1", "a_2", "a_3", "a_4", "a_5"],
                "acme-cli query --json",
                "reviewer",
                false,
                1
            ),
            deny_call("a_6", 11, "acme-cli query --xml", "reviewer", None, None),
        ),
    );
    let shell = StubShell::new(
        "shell",
        &[
            (
                "acme-cli query --json",
                Ok(("acme-cli query --json", "acme-cli query")),
            ),
            (
                "acme-cli query --xml",
                Ok(("acme-cli query --xml", "acme-cli query")),
            ),
        ],
    );
    let found = allows_items(&gather_allows(&home, &key, &workspace, &shell));
    assert_eq!(found.len(), 1);
    assert!(!found[0].3.contains("not proposed"), "{}", found[0].3);
}

#[test]
fn pipeline_denies_withhold_while_longer_names_spare() {
    let root = fakes::TempDir::new("doors-notes-contain");
    let home = root.path().join("home");
    let workspace = repo(root.path(), "here");
    let key = log::project_key(&worktree::project(&workspace));
    // Neither deny classifies. The pipeline names the candidate command
    // whole-word, so it withholds; the lookalike's longer name appears in
    // neither deny, so it proposes untouched.
    seed_calls(
        &home,
        &key,
        "s_1",
        workspace.to_str().unwrap(),
        &format!(
            "{}{}{}{}",
            allow_calls(
                &["a_1", "a_2", "a_3", "a_4", "a_5"],
                "acme-cli query --json",
                "reviewer",
                false,
                1
            ),
            allow_calls(
                &["b_1", "b_2", "b_3", "b_4", "b_5"],
                "my-acme-cli query --json",
                "reviewer",
                false,
                11
            ),
            deny_call(
                "a_6",
                21,
                "acme-cli query --json | grep x",
                "reviewer",
                None,
                None
            ),
            deny_call(
                "a_7",
                24,
                "((other-cli query --json",
                "reviewer",
                None,
                None
            ),
        ),
    );
    let shell = StubShell::new(
        "shell",
        &[
            (
                "acme-cli query --json",
                Ok(("acme-cli query --json", "acme-cli query")),
            ),
            (
                "my-acme-cli query --json",
                Ok(("my-acme-cli query --json", "my-acme-cli query")),
            ),
            ("acme-cli query --json | grep x", Err("pipe")),
            ("((other-cli query --json", Err("syntax")),
        ],
    );
    let found = allows_items(&gather_allows(&home, &key, &workspace, &shell));
    assert_eq!(found.len(), 2);
    let denied = found
        .iter()
        .find(|(subject, _, _, _)| subject == "acme-cli query --json")
        .unwrap();
    assert!(denied.3.contains("not proposed"), "{}", denied.3);
    let spared = found
        .iter()
        .find(|(subject, _, _, _)| subject == "my-acme-cli query --json")
        .unwrap();
    assert!(!spared.3.contains("not proposed"), "{}", spared.3);
}

#[test]
fn a_longer_command_name_spares_in_unparseable_denies() {
    let root = fakes::TempDir::new("doors-notes-longer");
    let home = root.path().join("home");
    let workspace = repo(root.path(), "here");
    let key = log::project_key(&worktree::project(&workspace));
    // The unparseable deny names only `my-acme-cli`: the candidate name
    // appears inside it, but never whole-word, so it spares.
    seed_calls(
        &home,
        &key,
        "s_1",
        workspace.to_str().unwrap(),
        &format!(
            "{}{}",
            allow_calls(
                &["a_1", "a_2", "a_3", "a_4", "a_5"],
                "acme-cli query --json",
                "reviewer",
                false,
                1
            ),
            deny_call(
                "a_6",
                11,
                "((my-acme-cli query --json",
                "reviewer",
                None,
                None
            ),
        ),
    );
    let shell = StubShell::new(
        "shell",
        &[
            (
                "acme-cli query --json",
                Ok(("acme-cli query --json", "acme-cli query")),
            ),
            ("((my-acme-cli query --json", Err("syntax")),
        ],
    );
    let found = allows_items(&gather_allows(&home, &key, &workspace, &shell));
    assert_eq!(found.len(), 1);
    assert!(!found[0].3.contains("not proposed"), "{}", found[0].3);
}

#[test]
fn per_subject_here_counts_resolve_like_session_evidence() {
    let root = fakes::TempDir::new("doors-notes-here");
    let home = root.path().join("home");
    let here_repo = repo(root.path(), "here");
    let away_repo = repo(root.path(), "away");
    let key = log::project_key(&worktree::project(&here_repo));
    let here = here_repo.to_str().unwrap();
    let away = away_repo.to_str().unwrap();
    // Two allows recorded here, two recorded away, one with no recording.
    seed_calls(
        &home,
        &key,
        "s_1",
        here,
        &allow_calls(
            &["a_1", "a_2"],
            "acme-cli query --json",
            "reviewer",
            false,
            1,
        ),
    );
    seed_calls(
        &home,
        &key,
        "s_2",
        away,
        &allow_calls(
            &["a_3", "a_4"],
            "acme-cli query --json",
            "reviewer",
            false,
            1,
        ),
    );
    let mut log = json!({"kind": "session_started", "payload": {}}).to_string() + "\n";
    log.push_str(&allow_calls(
        &["a_5"],
        "acme-cli query --json",
        "reviewer",
        false,
        1,
    ));
    write_log(&session_dir(&home, &key, "s_3"), log.as_bytes());
    let shell = StubShell::new(
        "shell",
        &[(
            "acme-cli query --json",
            Ok(("acme-cli query --json", "acme-cli query")),
        )],
    );
    let found = allows_items(&gather_allows(&home, &key, &here_repo, &shell));
    assert_eq!(
        found,
        [(
            "acme-cli query --json".to_owned(),
            5,
            3,
            "5 reviewer allows for acme-cli query --json".to_owned()
        )]
    );
}

#[test]
fn missing_config_covers_nothing_while_malformed_config_fails_closed() {
    let root = fakes::TempDir::new("doors-notes-config");
    let home = root.path().join("home");
    let workspace = repo(root.path(), "here");
    let key = log::project_key(&worktree::project(&workspace));
    seed_calls(
        &home,
        &key,
        "s_1",
        workspace.to_str().unwrap(),
        &allow_calls(
            &["a_1", "a_2", "a_3", "a_4", "a_5"],
            "acme-cli query --json",
            "reviewer",
            false,
            1,
        ),
    );
    let shell = StubShell::new(
        "shell",
        &[(
            "acme-cli query --json",
            Ok(("acme-cli query --json", "acme-cli query")),
        )],
    );
    // No config file at all: the candidate proposes.
    let dir = session_dir(&home, &key, "s_live");
    let found = allows_items(&gather(
        &home,
        &dir,
        &workspace,
        Topic::ReadOnlyCommands,
        Some(&shell),
    ));
    assert_eq!(found.len(), 1);
    // A present-but-malformed layer fails the whole gather closed.
    write_file(
        &config::global_config_file(&home),
        r#"{"shell": {"read_only": []}}"#,
    );
    let found = allows_items(&gather(
        &home,
        &dir,
        &workspace,
        Topic::ReadOnlyCommands,
        Some(&shell),
    ));
    assert!(found.is_empty());
    // A non-object shell value fails closed the same way.
    write_file(
        &config::global_config_file(&home),
        r#"{"shell": "entries"}"#,
    );
    let found = allows_items(&gather(
        &home,
        &dir,
        &workspace,
        Topic::ReadOnlyCommands,
        Some(&shell),
    ));
    assert!(found.is_empty());
}

#[test]
fn userinfo_commands_propose_verbatim_for_the_loop_to_redact() {
    let root = fakes::TempDir::new("doors-notes-userinfo");
    let home = root.path().join("home");
    let workspace = repo(root.path(), "here");
    let key = log::project_key(&worktree::project(&workspace));
    seed_calls(
        &home,
        &key,
        "s_1",
        workspace.to_str().unwrap(),
        &allow_calls(
            &["a_1", "a_2", "a_3", "a_4", "a_5"],
            "acme-cli fetch https://user:token@host/x",
            "reviewer",
            false,
            1,
        ),
    );
    let shell = StubShell::new(
        "shell",
        &[(
            "acme-cli fetch https://user:token@host/x",
            Ok(("acme-cli fetch https://user:token@host/x", "acme-cli fetch")),
        )],
    );
    let found = allows_items(&gather_allows(&home, &key, &workspace, &shell));
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].0, "acme-cli fetch https://user:token@host/x");
}

#[test]
fn a_missing_a_foreign_and_an_other_topic_give_no_allows() {
    let root = fakes::TempDir::new("doors-notes-absent");
    let home = root.path().join("home");
    let workspace = repo(root.path(), "here");
    let key = log::project_key(&worktree::project(&workspace));
    seed_calls(
        &home,
        &key,
        "s_1",
        workspace.to_str().unwrap(),
        &allow_calls(
            &["a_1", "a_2", "a_3", "a_4", "a_5"],
            "acme-cli query --json",
            "reviewer",
            false,
            1,
        ),
    );
    let dir = session_dir(&home, &key, "s_live");
    let shell = StubShell::new(
        "shell",
        &[(
            "acme-cli query --json",
            Ok(("acme-cli query --json", "acme-cli query")),
        )],
    );
    let foreign = StubShell::new(
        "other",
        &[(
            "acme-cli query --json",
            Ok(("acme-cli query --json", "acme-cli query")),
        )],
    );
    for evidence in [
        gather(&home, &dir, &workspace, Topic::ReadOnlyCommands, None),
        gather(
            &home,
            &dir,
            &workspace,
            Topic::ReadOnlyCommands,
            Some(&foreign),
        ),
        gather(&home, &dir, &workspace, Topic::RoutineActions, Some(&shell)),
    ] {
        assert!(allows_items(&evidence).is_empty());
    }
}

#[test]
fn tallies_cap_at_sixty_four() {
    let root = fakes::TempDir::new("doors-notes-cap");
    let home = root.path().join("home");
    let workspace = repo(root.path(), "here");
    let key = log::project_key(&worktree::project(&workspace));
    let mut body = String::new();
    let mut owned = Vec::new();
    let mut seq = 1;
    for index in 0..65 {
        let command = format!("capcmd-{index:02} query --json");
        let prefix = format!("capcmd-{index:02} query");
        let actions: Vec<String> = (0..5).map(|at| format!("a_{index}_{at}")).collect();
        let refs: Vec<&str> = actions.iter().map(String::as_str).collect();
        body.push_str(&allow_calls(&refs, &command, "reviewer", false, seq));
        seq += 10;
        owned.push((command, prefix));
    }
    // One more allow for the last subject: it sorts first by count.
    body.push_str(&allow_calls(
        &["a_last"],
        "capcmd-64 query --json",
        "reviewer",
        false,
        seq,
    ));
    seed_calls(&home, &key, "s_1", workspace.to_str().unwrap(), &body);
    let table: Vec<(&str, Verdict)> = owned
        .iter()
        .map(|(command, prefix)| (command.as_str(), Ok((command.as_str(), prefix.as_str()))))
        .collect();
    let shell = StubShell::new("shell", &table);
    // The doc promises no visible order, so the test pins the cap bound
    // only: sixty-four tallies stay, six-count subject included.
    let found = allows_items(&gather_allows(&home, &key, &workspace, &shell));
    assert_eq!(found.len(), 64);
    let top = found
        .iter()
        .find(|(subject, _, _, _)| subject == "capcmd-64 query --json")
        .unwrap();
    assert_eq!(top.1, 6);
}

/// A shell that executes every tabled command, naming the subject and
/// prefix the built-in classifier reports for commands it owns no rule
/// for (C19): `executes` with both halves, never `reads`.
struct ExecutesShell {
    table: Mutex<std::collections::HashMap<String, (String, String)>>,
}

impl ExecutesShell {
    fn new(table: &[(&str, &str, &str)]) -> Self {
        Self {
            table: Mutex::new(
                table
                    .iter()
                    .map(|(command, subject, prefix)| {
                        (
                            command.to_string(),
                            (subject.to_string(), prefix.to_string()),
                        )
                    })
                    .collect(),
            ),
        }
    }
}

impl Tool for ExecutesShell {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "shell".to_owned(),
            description: "an executing stub shell".to_owned(),
            input_schema: serde_json::Value::Object(serde_json::Map::new()),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(
        &self,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Effects, EffectsError> {
        let command = arguments
            .get("command")
            .and_then(serde_json::Value::as_str)
            .expect("a command string");
        match self
            .table
            .lock()
            .expect("the table lock")
            .get(command)
            .cloned()
        {
            Some((subject, prefix)) => Ok(Effects {
                declared: DeclaredEffects {
                    effects: vec![Effect::Executes],
                    reversible: false,
                    paths: None,
                    confined: false,
                },
                subject: Some(subject),
                prefix: Some(prefix),
                always_reviewed: false,
                hosts: None,
            }),
            None => Err(EffectsError::Arguments("unclassifiable".to_owned())),
        }
    }

    fn run(
        &self,
        _arguments: &serde_json::Map<String, serde_json::Value>,
        _cancel: &dyn Cancel,
        _emit: &dyn Emit,
    ) -> Output {
        panic!("the stub never runs");
    }
}

#[test]
fn an_executes_verdict_with_no_builtin_rule_still_proposes() {
    let root = fakes::TempDir::new("doors-notes-c19-query");
    let home = root.path().join("home");
    let workspace = repo(root.path(), "here");
    let key = log::project_key(&worktree::project(&workspace));
    seed_calls(
        &home,
        &key,
        "s_1",
        workspace.to_str().unwrap(),
        &allow_calls(
            &["a_1", "a_2", "a_3", "a_4", "a_5"],
            "acme-cli query --json",
            "reviewer",
            false,
            1,
        ),
    );
    let shell = ExecutesShell::new(&[(
        "acme-cli query --json",
        "acme-cli query --json",
        "acme-cli query",
    )]);
    let dir = session_dir(&home, &key, "s_live");
    let found = allows_items(&gather(
        &home,
        &dir,
        &workspace,
        Topic::ReadOnlyCommands,
        Some(&shell),
    ));
    assert_eq!(
        found,
        [(
            "acme-cli query --json".to_owned(),
            5,
            5,
            "5 reviewer allows for acme-cli query --json".to_owned()
        )]
    );
}

#[test]
fn builtin_classified_commands_stay_silent_under_c19() {
    let root = fakes::TempDir::new("doors-notes-c19-builtin");
    let home = root.path().join("home");
    let workspace = repo(root.path(), "here");
    let key = log::project_key(&worktree::project(&workspace));
    seed_calls(
        &home,
        &key,
        "s_1",
        workspace.to_str().unwrap(),
        &format!(
            "{}{}",
            allow_calls(
                &["a_1", "a_2", "a_3", "a_4", "a_5"],
                "ls --json",
                "reviewer",
                false,
                1
            ),
            allow_calls(
                &["b_1", "b_2", "b_3", "b_4", "b_5"],
                "git push",
                "reviewer",
                false,
                11
            ),
        ),
    );
    let shell = ExecutesShell::new(&[
        ("ls --json", "ls --json", "ls"),
        ("git push", "git push", "git push"),
    ]);
    let dir = session_dir(&home, &key, "s_live");
    let found = allows_items(&gather(
        &home,
        &dir,
        &workspace,
        Topic::ReadOnlyCommands,
        Some(&shell),
    ));
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn a_layer_file_without_entries_still_proposes() {
    let root = fakes::TempDir::new("doors-notes-no-entries-key");
    let home = root.path().join("home");
    let workspace = repo(root.path(), "here");
    let key = log::project_key(&worktree::project(&workspace));
    seed_calls(
        &home,
        &key,
        "s_1",
        workspace.to_str().unwrap(),
        &allow_calls(
            &["a_1", "a_2", "a_3", "a_4", "a_5"],
            "acme-cli query --json",
            "reviewer",
            false,
            1,
        ),
    );
    write_file(
        &config::global_config_file(&home),
        r#"{"reviewer": {"context": "Notes."}}"#,
    );
    let shell = ExecutesShell::new(&[(
        "acme-cli query --json",
        "acme-cli query --json",
        "acme-cli query",
    )]);
    let dir = session_dir(&home, &key, "s_live");
    let found = allows_items(&gather(
        &home,
        &dir,
        &workspace,
        Topic::ReadOnlyCommands,
        Some(&shell),
    ));
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].0, "acme-cli query --json");
    // A flat `shell.read_only` key is not configuration: the loader
    // ignores it, so it covers nothing either.
    write_file(
        &config::global_config_file(&home),
        r#"{"reviewer": {"context": "Notes."}, "shell.read_only": {"other-cli run": {"flags": []}}}"#,
    );
    let found = allows_items(&gather(
        &home,
        &dir,
        &workspace,
        Topic::ReadOnlyCommands,
        Some(&shell),
    ));
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].0, "acme-cli query --json");
}
