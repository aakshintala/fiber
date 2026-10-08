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

use super::commands;
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
        name: name.into(),
        description: description.into(),
        argument_hint: hint.map(str::to_owned),
        tag: tag.into(),
    }
}

fn prompt(name: &str, description: &str, tag: &str) -> CommandInfo {
    row(name, description, Some("<who>"), tag)
}

#[test]
fn prompt_rows_follow_the_skills() {
    let tree = Tree::new();
    skill(&tree.top().join(".fiber/skills"), "a", "review", "Reviews.");
    let listed = commands(
        &tree.inputs(),
        &tree.top(),
        &[prompt("greet", "Greets.", "fx")],
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
    // Passed `zz` first: the rows sort by server name, so `aa` wins.
    let listed = commands(
        &tree.inputs(),
        &tree.top(),
        &[
            prompt("greet", "Greets from zz.", "zz"),
            prompt("greet", "Greets from aa.", "aa"),
        ],
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
    let listed = commands(&inputs, &tree.top(), &[prompt("greet", "Greets.", "fx")]);
    assert!(listed.rows.is_empty());
    assert!(listed.notices.is_empty());
}

#[test]
fn no_prompts_gives_the_skill_rows_unchanged() {
    let tree = Tree::new();
    skill(&tree.top().join(".fiber/skills"), "a", "review", "Reviews.");
    let listed = commands(&tree.inputs(), &tree.top(), &[]);
    assert_eq!(listed.rows, [row("review", "Reviews.", None, "skill")]);
    assert!(listed.notices.is_empty());
}
