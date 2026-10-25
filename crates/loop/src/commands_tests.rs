//! The `commands` answer's rows and notices over skills and MCP prompt
//! rows, on places built in a temporary directory.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use contract::ErrorCode;
use contract::events::CommandInfo;
use fakes::clock::FakeClock;

use super::{commands, commands_from_found};
use crate::SkillSet;
use crate::prompt::PromptInputs;

/// A temporary tree: `top` is the repository's top level, `home` is
/// Fiber home, `person` the person's home.
struct Tree {
    _held: fakes::TempDir,
    root: PathBuf,
}

impl Tree {
    fn new() -> Self {
        let held = fakes::TempDir::new("fiber-commands");
        let root = held.path().canonicalize().unwrap();
        for dir in ["top", "home", "person"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        Self { _held: held, root }
    }

    fn top(&self) -> PathBuf {
        self.root.join("top")
    }

    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    fn inputs(&self) -> PromptInputs {
        let clock: Arc<dyn contract::clock::Clock> = FakeClock::new();
        let mut inputs = PromptInputs::new(
            self.home(),
            "/bin/sh".into(),
            self.root.join("events.jsonl").display().to_string(),
            clock,
            fakes::CONTEXT_WINDOW,
        );
        inputs.agents_home = Some(self.root.join("person"));
        inputs
    }
}

/// Writes `<place>/<entry>/SKILL.md` with `name` and `description`, and
/// returns its path.
fn skill(place: &Path, entry: &str, name: &str, description: &str) -> PathBuf {
    let dir = place.join(entry);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("SKILL.md");
    std::fs::write(
        &path,
        format!("---\nname: {name}\ndescription: {description}\n---\nBody.\n"),
    )
    .unwrap();
    path
}

fn row(name: &str, description: &str, hint: Option<&str>, tag: &str) -> CommandInfo {
    CommandInfo {
        extension: false,
        name: name.into(),
        description: description.into(),
        argument_hint: hint.map(str::to_owned),
        tag: tag.into(),
    }
}

fn prompt(name: &str, description: &str, tag: &str) -> CommandInfo {
    row(name, description, Some("<who>"), tag)
}

fn extension(name: &str, description: &str, tag: &str) -> CommandInfo {
    CommandInfo {
        name: name.into(),
        description: description.into(),
        argument_hint: None,
        tag: tag.into(),
        extension: true,
    }
}

#[test]
fn a_commands_refresh_uses_the_exact_check_snapshot() {
    let tree = Tree::new();
    let old = skill(&tree.top().join(".fiber/skills"), "old", "old", "Old.");
    let inputs = tree.inputs();
    let set = SkillSet::new(inputs.clone(), &tree.top());
    let checked = set.check();

    std::fs::remove_file(old).unwrap();
    skill(&tree.top().join(".fiber/skills"), "new", "new", "New.");
    let refreshed = commands_from_found(&checked.inputs, &checked.found, &[], &[]);

    assert!(refreshed.rows.iter().any(|row| row.name == "old"));
    assert!(!refreshed.rows.iter().any(|row| row.name == "new"));
}

#[test]
fn a_template_added_before_the_opening_rebuilds_the_initial_answer() {
    let tree = Tree::new();
    let mut inputs = tree.inputs();
    let extension = tree.root.join("extensions/acme");
    inputs
        .extension_dirs
        .push(("acme".into(), extension.clone()));
    let initial = commands(&inputs, &tree.top(), &[], &[]);
    let set = SkillSet::new(inputs.clone(), &tree.top());
    set.sync_commands(&initial.rows);

    skill(
        &extension.join("prompts"),
        "release",
        "release",
        "Releases.",
    );
    let opening = crate::opening::collect(&inputs, &tree.top());
    assert_eq!(
        set.opened(
            opening.found.clone(),
            inputs.skills_disabled.clone(),
            &opening.message.skills,
            &opening.notices,
        ),
        (true, true)
    );
    let refreshed = commands_from_found(&inputs, &opening.found, &[], &[]);

    assert!(initial.rows.iter().all(|row| row.name != "release"));
    assert!(
        refreshed
            .rows
            .iter()
            .any(|row| row.name == "release" && row.tag == "template")
    );
}

#[test]
fn the_first_opening_refreshes_rows_when_only_a_description_changed() {
    let tree = Tree::new();
    let skill_file = skill(
        &tree.top().join(".fiber/skills"),
        "review",
        "review",
        "Before.",
    );
    let inputs = tree.inputs();
    let initial = commands(&inputs, &tree.top(), &[], &[]);
    let set = SkillSet::new(inputs.clone(), &tree.top());
    set.sync_commands(&initial.rows);
    std::fs::write(
        &skill_file,
        "---\nname: review\ndescription: After.\n---\nBody.\n",
    )
    .unwrap();
    let opening = crate::opening::collect(&inputs, &tree.top());

    assert_eq!(
        set.opened(
            opening.found.clone(),
            inputs.skills_disabled.clone(),
            &opening.message.skills,
            &opening.notices,
        ),
        (true, false)
    );
    let refreshed = commands_from_found(&inputs, &opening.found, &[], &[]);
    let review = refreshed
        .rows
        .iter()
        .find(|row| row.name == "review")
        .unwrap();
    assert_eq!(review.description, "After.");
}

#[test]
fn prompt_rows_follow_the_skills() {
    let tree = Tree::new();
    skill(&tree.top().join(".fiber/skills"), "a", "review", "Reviews.");
    let listed = commands(
        &tree.inputs(),
        &tree.top(),
        &[prompt("greet", "Greets.", "fx")],
        &[],
    );
    assert_eq!(
        listed.rows,
        [
            row("review", "Reviews.", None, "skill"),
            prompt("greet", "Greets.", "fx"),
        ]
    );
    assert!(listed.notices.is_empty());
}

#[test]
fn a_skill_shadows_a_prompt_with_a_notice() {
    let tree = Tree::new();
    let path = skill(
        &tree.top().join(".fiber/skills"),
        "a",
        "greet",
        "Greets in-house.",
    );
    let listed = commands(
        &tree.inputs(),
        &tree.top(),
        &[prompt("greet", "Greets.", "fx")],
        &[],
    );
    assert_eq!(
        listed.rows,
        [row("greet", "Greets in-house.", None, "skill")]
    );
    assert_eq!(listed.notices.len(), 1);
    let notice = &listed.notices[0];
    assert_eq!(notice.code, ErrorCode::SkillShadowed);
    assert!(
        notice.message.contains("`fx`")
            && notice.message.contains("`/greet`")
            && notice.message.contains(&path.display().to_string()),
        "the notice names the server, the prompt and the skill's path: {}",
        notice.message,
    );
}

#[test]
fn the_first_servers_prompt_wins_a_shared_name_with_a_notice() {
    let tree = Tree::new();
    // Sorted by server name, as callers pass them: `aa` first wins.
    let listed = commands(
        &tree.inputs(),
        &tree.top(),
        &[
            prompt("greet", "Greets from aa.", "aa"),
            prompt("greet", "Greets from zz.", "zz"),
        ],
        &[],
    );
    assert_eq!(listed.rows, [prompt("greet", "Greets from aa.", "aa")]);
    assert_eq!(listed.notices.len(), 1);
    let notice = &listed.notices[0];
    assert_eq!(notice.code, ErrorCode::SkillShadowed);
    assert!(
        notice.message.contains("`zz`")
            && notice.message.contains("`aa`")
            && notice.message.contains("`/greet`"),
        "the notice names the loser and what won: {}",
        notice.message,
    );
}

#[test]
fn a_disabled_name_drops_the_prompt_without_a_notice() {
    let tree = Tree::new();
    let mut inputs = tree.inputs();
    inputs.skills_disabled = vec!["greet".into()];
    let listed = commands(
        &inputs,
        &tree.top(),
        &[prompt("greet", "Greets.", "fx")],
        &[],
    );
    assert!(listed.rows.is_empty());
    assert!(listed.notices.is_empty());
}

#[test]
fn no_prompts_gives_the_skill_rows_unchanged() {
    let tree = Tree::new();
    skill(&tree.top().join(".fiber/skills"), "a", "review", "Reviews.");
    let listed = commands(&tree.inputs(), &tree.top(), &[], &[]);
    assert_eq!(listed.rows, [row("review", "Reviews.", None, "skill")]);
    assert!(listed.notices.is_empty());
}

#[test]
fn a_skill_shadows_an_extension_command_with_a_notice() {
    let tree = Tree::new();
    let path = skill(
        &tree.top().join(".fiber/skills"),
        "a",
        "deploy",
        "Deploys in-house.",
    );
    let listed = commands(
        &tree.inputs(),
        &tree.top(),
        &[],
        &[extension("deploy", "Deploys.", "fx-ext")],
    );
    assert_eq!(
        listed.rows,
        [row("deploy", "Deploys in-house.", None, "skill")]
    );
    assert_eq!(listed.notices.len(), 1);
    let notice = &listed.notices[0];
    assert_eq!(notice.code, ErrorCode::SkillShadowed);
    assert_eq!(notice.extension.as_deref(), Some("fx-ext"));
    assert!(
        notice.message.contains("`fx-ext`")
            && notice.message.contains("`/deploy`")
            && notice.message.contains(&path.display().to_string()),
        "the notice names the extension, the command and the skill's path: {}",
        notice.message,
    );
}

#[test]
fn a_template_shadows_an_extension_command() {
    let tree = Tree::new();
    let path = skill(
        &tree.top().join(".agents/skills"),
        "a",
        "deploy",
        "Deploys in-house.",
    );
    // Any discovered skill or template shadows: the path names the winner.
    let listed = commands(
        &tree.inputs(),
        &tree.top(),
        &[],
        &[extension("deploy", "Deploys.", "fx-ext")],
    );
    assert_eq!(listed.rows.len(), 1);
    assert_eq!(listed.rows[0].tag, "skill");
    assert_eq!(listed.notices.len(), 1);
    assert_eq!(listed.notices[0].extension.as_deref(), Some("fx-ext"));
    assert!(
        listed.notices[0]
            .message
            .contains(&path.display().to_string()),
        "{}",
        listed.notices[0].message,
    );
}

#[test]
fn an_unshadowed_extension_command_lists_last_with_no_notice() {
    let tree = Tree::new();
    skill(&tree.top().join(".fiber/skills"), "a", "review", "Reviews.");
    let listed = commands(
        &tree.inputs(),
        &tree.top(),
        &[prompt("greet", "Greets.", "fx")],
        &[extension("ship", "Ships.", "fx-ext")],
    );
    assert_eq!(
        listed.rows,
        [
            row("review", "Reviews.", None, "skill"),
            prompt("greet", "Greets.", "fx"),
            extension("ship", "Ships.", "fx-ext"),
        ]
    );
    assert!(listed.notices.is_empty());
}

#[test]
fn an_extension_command_shadows_a_prompt_with_a_notice() {
    let tree = Tree::new();
    let listed = commands(
        &tree.inputs(),
        &tree.top(),
        &[prompt("greet", "Greets.", "fx")],
        &[extension("greet", "Greets fast.", "fx-ext")],
    );
    assert_eq!(listed.rows, [extension("greet", "Greets fast.", "fx-ext")]);
    assert_eq!(listed.notices.len(), 1);
    let notice = &listed.notices[0];
    assert_eq!(notice.code, ErrorCode::SkillShadowed);
    assert_eq!(notice.extension.as_deref(), Some("fx-ext"));
    assert!(
        notice.message.contains("`fx`")
            && notice.message.contains("`/greet`")
            && notice.message.contains("`fx-ext`"),
        "the notice names the server, the prompt and the extension: {}",
        notice.message,
    );
}

#[test]
fn a_skill_and_a_prompt_and_an_extension_give_two_notices() {
    let tree = Tree::new();
    skill(
        &tree.top().join(".fiber/skills"),
        "a",
        "greet",
        "Greets in-house.",
    );
    let listed = commands(
        &tree.inputs(),
        &tree.top(),
        &[prompt("greet", "Greets.", "fx")],
        &[extension("greet", "Greets fast.", "fx-ext")],
    );
    assert_eq!(listed.rows.len(), 1);
    assert_eq!(listed.rows[0].tag, "skill");
    assert_eq!(listed.notices.len(), 2);
    assert!(
        listed
            .notices
            .iter()
            .all(|notice| notice.code == ErrorCode::SkillShadowed)
    );
    assert!(
        listed
            .notices
            .iter()
            .any(|notice| notice.extension.as_deref() == Some("fx-ext")),
        "{:?}",
        listed.notices,
    );
}

#[test]
fn a_disabled_name_keeps_the_extension_command() {
    let tree = Tree::new();
    let mut inputs = tree.inputs();
    inputs.skills_disabled = vec!["ship".into()];
    let listed = commands(
        &inputs,
        &tree.top(),
        &[],
        &[extension("ship", "Ships.", "fx-ext")],
    );
    assert_eq!(listed.rows, [extension("ship", "Ships.", "fx-ext")]);
    assert!(listed.notices.is_empty());
}
