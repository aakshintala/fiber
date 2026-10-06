//! Tests for `opening::collect` and `opening::render`: the environment,
//! instruction file discovery and precedence, rendering from the logged
//! fields, and the `instructions_large` notice.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use contract::clock::Clock;
use contract::events::Event;
use contract::provider::Input;
use contract::{Envelope, Seq, SessionId};
use fakes::clock::FakeClock;

use super::{collect, date_of, render};
use crate::prompt::PromptInputs;

fn clock() -> Arc<FakeClock> {
    FakeClock::new()
}

fn inputs(home: &Path, clock: &Arc<FakeClock>) -> PromptInputs {
    let owned: Arc<FakeClock> = Arc::clone(clock);
    let clock: Arc<dyn contract::clock::Clock> = owned;
    PromptInputs::new(
        home.to_path_buf(),
        "/bin/sh".into(),
        home.join("events.jsonl").display().to_string(),
        clock,
    )
}

fn dir() -> (PathBuf, fakes::TempDir) {
    let held = fakes::TempDir::new("fiber-opening");
    let path = held.path().to_path_buf();
    (path.clone(), held)
}

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, content).unwrap();
}

fn collected(home: &Path, workspace: &Path, clock: &Arc<FakeClock>) -> super::Collected {
    collect(&inputs(home, clock), workspace)
}

/// `path` with symlinks resolved, as `collect` records paths.
fn canon(path: &Path) -> PathBuf {
    path.canonicalize().unwrap()
}

#[test]
fn date_of_pins_epoch_leap_day_year_end_and_midnight_edge() {
    // Each `(seconds, date)` is midnight UTC of `date`, written as
    // literals independent of the code under test: flipping any operator
    // in `civil_from_days` moves one of them.
    let cases = [
        (0_u64, "1970-01-01"),
        (946_598_400_u64, "1999-12-31"),
        (951_782_400_u64, "2000-02-29"),
        (951_868_800_u64, "2000-03-01"),
        (1_709_164_800_u64, "2024-02-29"),
        (1_735_603_200_u64, "2024-12-31"),
        (4_107_456_000_u64, "2100-02-28"),
        (4_107_542_400_u64, "2100-03-01"),
        (13_574_563_200_u64, "2400-02-29"),
        (13_574_649_600_u64, "2400-03-01"),
        (253_402_214_400_u64, "9999-12-31"),
    ];
    for (secs, date) in cases {
        assert_eq!(
            date_of(UNIX_EPOCH + Duration::from_secs(secs)),
            date,
            "{secs}s"
        );
    }
    // One second before midnight UTC is still the old date: 2000-03-01
    // minus one second is 2000-02-29, a leap day.
    assert_eq!(
        date_of(UNIX_EPOCH + Duration::from_secs(951_868_799)),
        "2000-02-29"
    );
    // One second before midnight UTC is still the old date.
    assert_eq!(
        date_of(UNIX_EPOCH + Duration::from_secs(1_700_006_399)),
        "2023-11-14"
    );
    assert_eq!(
        date_of(UNIX_EPOCH + Duration::from_secs(1_700_006_400)),
        "2023-11-15"
    );
    // Before the epoch reads as the epoch's date.
    assert_eq!(date_of(UNIX_EPOCH - Duration::from_secs(60)), "1970-01-01");
    // The fake clock's wall is 2023-11-14T22:13:20Z.
    assert_eq!(date_of(clock().wall()), "2023-11-14");
}

#[test]
fn environment_holds_date_platform_shell_workspace_and_log() {
    let (home, _held) = dir();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let fake = clock();
    let message = collected(&home, &workspace, &fake).message;
    let environment = &message.environment;
    assert_eq!(environment.date, "2023-11-14");
    assert_eq!(environment.os, std::env::consts::OS);
    assert_eq!(environment.arch, std::env::consts::ARCH);
    assert_eq!(environment.shell, "/bin/sh");
    assert_eq!(
        environment.workspace,
        std::fs::canonicalize(&workspace)
            .unwrap()
            .display()
            .to_string()
    );
    assert!(environment.git.is_none());
    assert_eq!(
        environment.session_log,
        home.join("events.jsonl").display().to_string()
    );
    assert!(message.instruction_files.is_empty());
    assert!(message.skills.is_empty());
    // Absent everywhere is silence, not a notice: a flipped
    // `NotFound` guard would name the absent files instead.
    assert!(collected(&home, &workspace, &fake).notices.is_empty());
}

#[test]
fn plain_repo_reports_its_branch() {
    let (home, _held) = dir();
    let workspace = home.join("workspace");
    write(&workspace.join(".git/HEAD"), "ref: refs/heads/main\n");
    let fake = clock();
    let message = collected(&home, &workspace, &fake).message;
    assert_eq!(
        message.environment.git,
        Some(contract::events::Git {
            branch: Some("main".into())
        })
    );
    assert!(render(&message).contains("yes, branch main"));
}

#[test]
fn nested_branch_keeps_its_slashes() {
    let (home, _held) = dir();
    let workspace = home.join("workspace");
    write(
        &workspace.join(".git/HEAD"),
        "ref: refs/heads/feature/work\n",
    );
    let fake = clock();
    let message = collected(&home, &workspace, &fake).message;
    assert_eq!(
        message.environment.git.unwrap().branch,
        Some("feature/work".into())
    );
}

#[test]
fn detached_head_reports_no_branch() {
    let (home, _held) = dir();
    let workspace = home.join("workspace");
    write(
        &workspace.join(".git/HEAD"),
        "3a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d\n",
    );
    let fake = clock();
    let message = collected(&home, &workspace, &fake).message;
    assert_eq!(
        message.environment.git,
        Some(contract::events::Git { branch: None })
    );
    assert!(render(&message).contains("yes, detached HEAD"));
}

#[test]
fn worktree_git_file_with_absolute_target_reports_its_branch() {
    let (home, _held) = dir();
    let workspace = home.join("workspace");
    let target = home.join("elsewhere/gitdir");
    write(&target.join("HEAD"), "ref: refs/heads/work\n");
    // A `.git` file pointing outside the tree still counts: the top level
    // is the directory holding the `.git` file.
    write(
        &workspace.join(".git"),
        &format!("gitdir: {}\n", target.display()),
    );
    let fake = clock();
    let message = collected(&home, &workspace, &fake).message;
    assert_eq!(message.environment.git.unwrap().branch, Some("work".into()));
}

#[test]
fn worktree_git_file_with_relative_target_and_detached_head() {
    let (home, _held) = dir();
    let workspace = home.join("workspace");
    let target = home.join("real.git");
    write(
        &target.join("HEAD"),
        "9f8e7d6c5b4a39281706f5e4d3c2b1a09f8e7d6c\n",
    );
    write(&workspace.join(".git"), "gitdir: ../real.git\n");
    let fake = clock();
    let message = collected(&home, &workspace, &fake).message;
    assert_eq!(
        message.environment.git,
        Some(contract::events::Git { branch: None })
    );
}

#[test]
fn no_repository_reports_no_git() {
    let (home, _held) = dir();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let fake = clock();
    let message = collected(&home, &workspace, &fake).message;
    assert!(message.environment.git.is_none());
    assert!(render(&message).contains("- Git: no"));
}

#[test]
fn empty_directories_collect_nothing_silently() {
    let (home, _held) = dir();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    // Neither directory holds any candidate: no file is sent, and no
    // failure is named for the files that are not there.
    let fake = clock();
    let collected = collected(&home, &workspace, &fake);
    assert!(collected.message.instruction_files.is_empty());
    assert!(collected.notices.is_empty());
}

#[test]
fn malformed_git_file_is_not_a_repository() {
    let (home, _held) = dir();
    let workspace = home.join("workspace");
    write(&workspace.join(".git"), "not a gitdir line\n");
    let fake = clock();
    let message = collected(&home, &workspace, &fake).message;
    assert!(message.environment.git.is_none());
}

#[test]
fn empty_git_file_and_empty_target_are_not_a_repository() {
    let (home, _held) = dir();
    let workspace = home.join("workspace");
    write(&workspace.join(".git"), "");
    let fake = clock();
    assert!(
        collected(&home, &workspace, &fake)
            .message
            .environment
            .git
            .is_none()
    );
    write(&workspace.join(".git"), "gitdir:\n");
    assert!(
        collected(&home, &workspace, &fake)
            .message
            .environment
            .git
            .is_none()
    );
    write(&workspace.join(".git"), "gitdir:   \n");
    assert!(
        collected(&home, &workspace, &fake)
            .message
            .environment
            .git
            .is_none()
    );
}

#[test]
fn symlinked_workspace_resolves_to_canonical_paths() {
    let (home, _held) = dir();
    let real = home.join("real");
    write(&real.join(".git/HEAD"), "ref: refs/heads/main\n");
    write(&real.join("AGENTS.md"), "Real rules.\n");
    let link = home.join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let fake = clock();
    let message = collected(&home, &link, &fake).message;
    assert_eq!(
        message.environment.workspace,
        std::fs::canonicalize(&real).unwrap().display().to_string()
    );
    assert_eq!(message.instruction_files.len(), 1);
    assert_eq!(
        message.instruction_files[0].path,
        real.canonicalize()
            .unwrap()
            .join("AGENTS.md")
            .display()
            .to_string()
    );
}

#[test]
fn agents_beats_claude_in_the_same_directory() {
    let (home, _held) = dir();
    let workspace = home.join("workspace");
    write(&workspace.join("AGENTS.md"), "Agents rules.\n");
    write(&workspace.join("CLAUDE.md"), "Claude rules.\n");
    let fake = clock();
    let message = collected(&home, &workspace, &fake).message;
    assert_eq!(message.instruction_files.len(), 1);
    assert_eq!(message.instruction_files[0].content, "Agents rules.\n");
}

#[test]
fn claude_is_sent_when_there_is_no_agents_file() {
    let (home, _held) = dir();
    let workspace = home.join("workspace");
    write(&workspace.join("CLAUDE.md"), "Claude rules.\n");
    let fake = clock();
    let message = collected(&home, &workspace, &fake).message;
    assert_eq!(message.instruction_files.len(), 1);
    assert!(
        message.instruction_files[0].path.ends_with("CLAUDE.md"),
        "{}",
        message.instruction_files[0].path
    );
}

#[test]
fn pointer_only_claude_is_never_read() {
    let (home, _held) = dir();
    let workspace = home.join("workspace");
    write(&workspace.join("CLAUDE.md"), "@AGENTS.md\n");
    let fake = clock();
    let collected = collected(&home, &workspace, &fake);
    assert!(collected.message.instruction_files.is_empty());
    assert!(collected.notices.is_empty());
}

#[test]
fn global_file_comes_first_then_top_level_down_to_workspace() {
    let (home, _held) = dir();
    write(&home.join("AGENTS.md"), "Global rules.\n");
    let root = home.join("repo");
    let workspace = root.join("sub/dir");
    write(&root.join(".git/HEAD"), "ref: refs/heads/main\n");
    write(&root.join("AGENTS.md"), "Root rules.\n");
    write(&workspace.join("AGENTS.md"), "Leaf rules.\n");
    let fake = clock();
    let message = collected(&home, &workspace, &fake).message;
    let paths: Vec<&str> = message
        .instruction_files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    let canon = canon(&home);
    assert_eq!(
        paths,
        [
            canon.join("AGENTS.md").display().to_string(),
            canon.join("repo/AGENTS.md").display().to_string(),
            canon.join("repo/sub/dir/AGENTS.md").display().to_string(),
        ]
    );
    assert_eq!(message.instruction_files[0].content, "Global rules.\n");
}

#[test]
fn outside_git_only_the_workspace_is_read() {
    let (home, _held) = dir();
    // No `.git` anywhere and no global file: the workspace's parent file
    // is not read, the workspace's own is.
    let parent = home.join("parent");
    let workspace = parent.join("workspace");
    write(&parent.join("AGENTS.md"), "Parent rules.\n");
    write(&workspace.join("AGENTS.md"), "Leaf rules.\n");
    let fake = clock();
    let message = collected(&home, &workspace, &fake).message;
    let paths: Vec<&str> = message
        .instruction_files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    assert_eq!(
        paths,
        [canon(&workspace).join("AGENTS.md").display().to_string(),]
    );
}

#[test]
fn empty_file_is_sent_as_it_is() {
    let (home, _held) = dir();
    let workspace = home.join("workspace");
    write(&workspace.join("AGENTS.md"), "");
    let fake = clock();
    let message = collected(&home, &workspace, &fake).message;
    assert_eq!(message.instruction_files.len(), 1);
    assert_eq!(message.instruction_files[0].content, "");
}

#[test]
fn non_utf8_file_is_read_lossily() {
    let (home, _held) = dir();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("AGENTS.md"), [0xff, 0xfe, b'a']).unwrap();
    let fake = clock();
    let message = collected(&home, &workspace, &fake).message;
    assert_eq!(message.instruction_files.len(), 1);
    assert!(
        message.instruction_files[0].content.contains('�'),
        "{}",
        message.instruction_files[0].content
    );
    assert!(message.instruction_files[0].content.ends_with('a'));
}

#[test]
fn unreadable_file_is_left_out_with_an_io_failed_notice() {
    let (home, _held) = dir();
    let workspace = home.join("workspace");
    // A directory where the file should be cannot be read as one.
    std::fs::create_dir_all(workspace.join("AGENTS.md")).unwrap();
    let fake = clock();
    let collected = collected(&home, &workspace, &fake);
    assert!(collected.message.instruction_files.is_empty());
    assert_eq!(collected.notices.len(), 1);
    assert_eq!(collected.notices[0].code, contract::ErrorCode::IoFailed);
    assert!(
        collected.notices[0]
            .message
            .contains(&canon(&workspace).join("AGENTS.md").display().to_string()),
        "{}",
        collected.notices[0].message
    );
}

#[test]
fn an_unreadable_agents_does_not_fall_through_to_claude() {
    let (home, _held) = dir();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(workspace.join("AGENTS.md")).unwrap();
    std::fs::write(workspace.join("CLAUDE.md"), "claude rules").unwrap();
    let fake = clock();
    let collected = collected(&home, &workspace, &fake);
    assert!(collected.message.instruction_files.is_empty());
    assert_eq!(collected.notices.len(), 1);
}

#[test]
fn unreadable_claude_is_a_notice_too() {
    let (home, _held) = dir();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(workspace.join("CLAUDE.md")).unwrap();
    let fake = clock();
    let collected = collected(&home, &workspace, &fake);
    assert!(collected.message.instruction_files.is_empty());
    assert_eq!(collected.notices.len(), 1);
    assert_eq!(collected.notices[0].code, contract::ErrorCode::IoFailed);
}

#[test]
fn global_agents_dir_is_an_io_failed_notice() {
    let (home, _held) = dir();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    // A directory where the global file should be cannot be read as one:
    // absent would be silence, so one notice names it.
    std::fs::create_dir_all(home.join("AGENTS.md")).unwrap();
    let fake = clock();
    let collected = collected(&home, &workspace, &fake);
    assert!(collected.message.instruction_files.is_empty());
    assert_eq!(collected.notices.len(), 1);
    assert_eq!(collected.notices[0].code, contract::ErrorCode::IoFailed);
    assert!(
        collected.notices[0]
            .message
            .contains(&canon(&home).join("AGENTS.md").display().to_string()),
        "{}",
        collected.notices[0].message
    );
}

#[test]
fn workspace_equal_to_home_sends_global_once() {
    let (home, _held) = dir();
    write(&home.join("AGENTS.md"), "Global rules.\n");
    let fake = clock();
    let message = collected(&home, &home, &fake).message;
    assert_eq!(message.instruction_files.len(), 1);
    assert_eq!(message.instruction_files[0].content, "Global rules.\n");
}

#[test]
fn chain_top_level_equal_to_home_sends_global_once() {
    let (home, _held) = dir();
    write(&home.join(".git/HEAD"), "ref: refs/heads/main\n");
    write(&home.join("AGENTS.md"), "Global rules.\n");
    let workspace = home.join("sub/dir");
    std::fs::create_dir_all(&workspace).unwrap();
    let fake = clock();
    let message = collected(&home, &workspace, &fake).message;
    // The chain's top level is home itself: the global read already sent
    // it, so only one copy reaches the message.
    assert_eq!(message.instruction_files.len(), 1);
    assert_eq!(message.instruction_files[0].content, "Global rules.\n");
}

#[test]
fn rendering_carries_each_file_under_its_path() {
    let (home, _held) = dir();
    write(&home.join("AGENTS.md"), "Global rules.\n");
    let workspace = home.join("workspace");
    write(&workspace.join("AGENTS.md"), "Leaf rules.\n");
    let fake = clock();
    let message = collected(&home, &workspace, &fake).message;
    let text = render(&message);
    assert!(text.starts_with("This message is from Fiber, not the person."));
    assert!(text.contains("- Date: 2023-11-14"));
    assert!(text.contains(&format!("- Shell: {}", message.environment.shell)));
    for file in &message.instruction_files {
        assert!(text.contains(&file.path), "{text}");
        assert!(text.contains(&file.content), "{text}");
    }
    let global = home.join("AGENTS.md").display().to_string();
    let leaf = workspace.join("AGENTS.md").display().to_string();
    assert!(text.find(&global).unwrap() < text.find(&leaf).unwrap());
    // The file's directory is named too.
    assert!(text.contains(&canon(&home).display().to_string()));
    // No skills are listed yet, so the heading is left out.
    assert!(!text.contains("# Skills"), "{text}");
    assert!(!text.contains("{skills}"), "{text}");
}

#[test]
fn rendering_with_no_files_says_so() {
    let (home, _held) = dir();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let fake = clock();
    let text = render(&collected(&home, &workspace, &fake).message);
    assert!(
        text.contains("This project has no instruction files."),
        "{text}"
    );
}

#[test]
fn skills_render_from_their_logged_fields() {
    let (home, _held) = dir();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let fake = clock();
    let mut message = collected(&home, &workspace, &fake).message;
    message.skills = vec![contract::events::SkillListed {
        name: "review".into(),
        description: "Reviews code.".into(),
        path: "/skills/review/SKILL.md".into(),
        source: contract::events::SkillSource::Builtin,
    }];
    let text = render(&message);
    assert!(text.contains("# Skills"), "{text}");
    assert!(text.contains("review"), "{text}");
    assert!(text.contains("Reviews code."), "{text}");
    assert!(text.contains("/skills/review/SKILL.md"), "{text}");
}

#[test]
fn placeholders_inside_inserted_text_are_not_rescanned() {
    let (home, _held) = dir();
    let workspace = home.join("workspace");
    write(&workspace.join("AGENTS.md"), "The date is {date}.\n");
    let fake = clock();
    let text = render(&collected(&home, &workspace, &fake).message);
    assert!(text.contains("The date is {date}."), "{text}");
}

#[test]
fn live_and_resumed_conversations_render_the_same_opening() {
    let (home, _held) = dir();
    let workspace = home.join("workspace");
    write(&workspace.join("AGENTS.md"), "Leaf rules.\n");
    // A listing whose description spans lines and holds a placeholder.
    write(
        &workspace.join(".agents/skills/review/SKILL.md"),
        "---\nname: review\ndescription: >\n  Reviews a diff on {date}.\n\n  Second paragraph.\n---\nBody\n",
    );
    let fake = clock();
    let message = collected(&home, &workspace, &fake).message;
    assert_eq!(message.skills.len(), 1);
    let rendered = render(&message);
    assert!(
        rendered.contains("Reviews a diff on {date}.\nSecond paragraph."),
        "{rendered}"
    );
    let mut live = Vec::new();
    let mut had = std::collections::BTreeMap::new();
    crate::conversation::render(
        &mut live,
        &Event::OpeningMessage(message.clone()),
        None,
        "fake/model-1",
        &mut had,
        &mut crate::handoff::Carry::default(),
    );
    assert!(matches!(&live[0], Input::User { text } if text == &rendered));
    let line = Envelope {
        kind: "opening_message".into(),
        session_id: SessionId("s_test".into()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: Some(Seq(1)),
        payload: Event::OpeningMessage(message).payload().unwrap(),
    };
    let rebuilt = crate::conversation::rebuild(&[line], "fake/model-1").unwrap();
    assert_eq!(live, rebuilt);
}

#[test]
fn large_instruction_text_writes_instructions_large_naming_three() {
    let (home, _held) = dir();
    let root = home.join("repo");
    let workspace = root.join("sub");
    write(&root.join(".git/HEAD"), "ref: refs/heads/main\n");
    write(&root.join("AGENTS.md"), &"a".repeat(200));
    write(&workspace.join("AGENTS.md"), &"b".repeat(150));
    let fake = clock();
    let mut with = inputs(&home, &fake);
    with.context_window = Some(1_000);
    // 200 + 150 + 51 system bytes = 401: over 10% of 1,000 tokens.
    with.system = Some("s".repeat(51));
    let collected = collect(&with, &workspace);
    assert_eq!(collected.notices.len(), 1);
    assert_eq!(
        collected.notices[0].code,
        contract::ErrorCode::InstructionsLarge
    );
    let text = &collected.notices[0].message;
    assert!(text.contains("about 100 tokens"), "{text}");
    let first = text.find("200 bytes").unwrap();
    let second = text.find("150 bytes").unwrap();
    let third = text.find("51 bytes").unwrap();
    assert!(first < second && second < third, "{text}");
}

#[test]
fn exactly_ten_percent_is_not_large() {
    let (home, _held) = dir();
    let workspace = home.join("workspace");
    // 400 bytes is exactly 10% of a 1,000-token window: no notice.
    write(&workspace.join("AGENTS.md"), &"a".repeat(400));
    let fake = clock();
    let mut with = inputs(&home, &fake);
    with.context_window = Some(1_000);
    assert!(collect(&with, &workspace).notices.is_empty());
}

#[test]
fn one_byte_over_ten_percent_is_large() {
    let (home, _held) = dir();
    let workspace = home.join("workspace");
    write(&workspace.join("AGENTS.md"), &"a".repeat(401));
    let fake = clock();
    let mut with = inputs(&home, &fake);
    with.context_window = Some(1_000);
    let collected = collect(&with, &workspace);
    assert_eq!(collected.notices.len(), 1);
    assert_eq!(
        collected.notices[0].code,
        contract::ErrorCode::InstructionsLarge
    );
}

#[test]
fn unknown_context_window_skips_the_size_check() {
    let (home, _held) = dir();
    let workspace = home.join("workspace");
    write(&workspace.join("AGENTS.md"), &"a".repeat(10_000));
    let fake = clock();
    let mut unknown = inputs(&home, &fake);
    unknown.context_window = None;
    assert!(collect(&unknown, &workspace).notices.is_empty());
    unknown.context_window = Some(0);
    assert!(collect(&unknown, &workspace).notices.is_empty());
}

#[test]
fn extension_texts_and_append_count_toward_the_size() {
    let (home, _held) = dir();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let fake = clock();
    let mut with = inputs(&home, &fake);
    with.context_window = Some(1_000);
    with.append = Some("a".repeat(300));
    with.extensions = vec![("ext".into(), "b".repeat(150))];
    let collected = collect(&with, &workspace);
    assert_eq!(collected.notices.len(), 1);
    let text = &collected.notices[0].message;
    assert!(text.contains("APPEND_SYSTEM.md"), "{text}");
    assert!(text.contains("ext"), "{text}");
}

#[test]
fn whitespace_only_texts_do_not_count_toward_the_size() {
    let (home, _held) = dir();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let fake = clock();
    let mut with = inputs(&home, &fake);
    with.context_window = Some(1_000);
    // 500 blank bytes would pass 10% if counted; left-out parts are not.
    with.system = Some(" ".repeat(500));
    with.extensions = vec![("ext".into(), "\n  \n".into())];
    assert!(collect(&with, &workspace).notices.is_empty());
}

const SKILLS_SENTENCE: &str = "Each entry below is a skill. When a task matches a skill's description, load it with the `skill` tool.";

#[test]
fn a_repository_skill_is_listed_last_with_its_name_description_path_and_source() {
    let (home, _held) = dir();
    let workspace = home.join("workspace");
    write(&workspace.join("AGENTS.md"), "Leaf rules.\n");
    let path = workspace.join(".agents/skills/review/SKILL.md");
    write(
        &path,
        "---\nname: review\ndescription: Reviews a diff.\n---\nBody\n",
    );
    let fake = clock();
    let collected = collected(&home, &workspace, &fake);
    assert!(collected.notices.is_empty(), "{:?}", collected.notices);
    let [skill] = collected.message.skills.as_slice() else {
        panic!("{:?}", collected.message.skills);
    };
    assert_eq!(skill.name, "review");
    assert_eq!(skill.description, "Reviews a diff.");
    assert_eq!(skill.path, canon(&path).display().to_string());
    assert_eq!(skill.source, contract::events::SkillSource::Repository);
    let text = render(&collected.message);
    let entry = format!("- review: Reviews a diff. ({})", skill.path);
    assert!(
        text.ends_with(&format!("# Skills\n\n{SKILLS_SENTENCE}\n\n{entry}\n")),
        "{text}"
    );
}

#[test]
fn a_repository_in_git_reads_its_top_levels_skills_from_a_subdirectory() {
    let (home, _held) = dir();
    let root = home.join("repo");
    let workspace = root.join("sub");
    write(&root.join(".git/HEAD"), "ref: refs/heads/main\n");
    write(
        &root.join(".fiber/skills/top/SKILL.md"),
        "---\nname: top\ndescription: d\n---\n",
    );
    write(
        &workspace.join(".fiber/skills/nested/SKILL.md"),
        "---\nname: nested\ndescription: d\n---\n",
    );
    let fake = clock();
    let message = collected(&home, &workspace, &fake).message;
    let names: Vec<&str> = message.skills.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["top"]);
}

#[test]
fn with_no_skills_the_heading_and_sentence_are_absent_and_the_bytes_are_unchanged() {
    let (home, _held) = dir();
    let workspace = home.join("workspace");
    write(&workspace.join("AGENTS.md"), "Leaf rules.\n");
    let fake = clock();
    let message = collected(&home, &workspace, &fake).message;
    assert!(message.skills.is_empty());
    let environment = &message.environment;
    let file = &message.instruction_files[0];
    let expected = format!(
        "This message is from Fiber, not the person. It describes your environment and carries the project's instruction files.\n\n\
         # Environment\n\n\
         - Date: {}\n- Platform: {} {}\n- Shell: {}\n- Workspace: {}\n- Git: no\n- Session log: {}\n\n\
         # Instruction files\n\n{}",
        environment.date,
        environment.os,
        environment.arch,
        environment.shell,
        environment.workspace,
        environment.session_log,
        crate::prompt::fill(
            &crate::prompt::body(include_str!("../prompt/messages.md"), "instruction-file"),
            &[
                ("path", file.path.as_str()),
                (
                    "dir",
                    &Path::new(&file.path)
                        .parent()
                        .unwrap()
                        .display()
                        .to_string()
                ),
                ("content", file.content.as_str()),
            ],
        ),
    );
    assert_eq!(render(&message), expected);
    assert!(!expected.contains("# Skills"));
    assert!(!expected.contains(SKILLS_SENTENCE));
}

#[test]
fn a_switched_off_skill_is_not_listed() {
    let (home, _held) = dir();
    let workspace = home.join("workspace");
    for name in ["keep", "drop"] {
        write(
            &workspace.join(format!(".agents/skills/{name}/SKILL.md")),
            &format!("---\nname: {name}\ndescription: d\n---\n"),
        );
    }
    let fake = clock();
    let mut with = inputs(&home, &fake);
    with.skills_disabled = vec!["drop".into()];
    let message = collect(&with, &workspace).message;
    let names: Vec<&str> = message.skills.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["keep"]);
}

#[test]
fn notices_come_in_the_documented_order() {
    use contract::ErrorCode::{
        InstructionsLarge, IoFailed, SkillInvalid, SkillShadowed, SkillsLarge,
    };
    let (home, _held) = dir();
    let workspace = home.join("workspace");
    // An unreadable instruction file.
    std::fs::create_dir_all(workspace.join("AGENTS.md")).unwrap();
    let places = workspace.join(".agents/skills");
    // Discovery order: entries in byte order of their names.
    std::fs::create_dir_all(places.join("a/SKILL.md")).unwrap();
    write(&places.join("b/SKILL.md"), "no header");
    write(
        &places.join("c/SKILL.md"),
        "---\nname: same\ndescription: d\n---\n",
    );
    write(
        &places.join("d/SKILL.md"),
        "---\nname: same\ndescription: d\n---\n",
    );
    let fake = clock();
    let mut with = inputs(&home, &fake);
    // The system text and the listing both pass 10% of this window.
    with.context_window = Some(10);
    with.system = Some("s".repeat(100));
    let codes: Vec<_> = collect(&with, &workspace)
        .notices
        .into_iter()
        .map(|notice| notice.code)
        .collect();
    assert_eq!(
        codes,
        [
            IoFailed,
            InstructionsLarge,
            IoFailed,
            SkillInvalid,
            SkillShadowed,
            SkillsLarge
        ]
    );
}
