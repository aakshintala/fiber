//! Tests for the maintained skill set, over places built in a temporary
//! directory.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use contract::skills::Skills;
use fakes::clock::FakeClock;

use super::SkillSet;
use crate::prompt::PromptInputs;

struct Tree {
    _held: fakes::TempDir,
    root: PathBuf,
}

impl Tree {
    fn new() -> Self {
        let held = fakes::TempDir::new("fiber-skill-set");
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

    fn set(&self) -> SkillSet {
        SkillSet::new(self.inputs(), &self.top())
    }
}

fn skill(place: &Path, entry: &str, name: &str, description: &str) -> PathBuf {
    write(
        place,
        entry,
        &format!("---\nname: {name}\ndescription: {description}\n---\nBody.\n"),
    )
}

fn write(place: &Path, entry: &str, text: &str) -> PathBuf {
    let dir = place.join(entry);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("SKILL.md");
    std::fs::write(&path, text).unwrap();
    path
}

#[test]
fn a_lookup_before_opened_reads_once_and_misses_a_later_skill() {
    let tree = Tree::new();
    let path = skill(&tree.top().join(".agents/skills"), "tdd", "tdd", "d");
    let set = tree.set();
    assert_eq!(set.command("tdd"), Some(path.clone()));
    assert_eq!(set.listed_file("tdd"), Some(path));
    skill(&tree.top().join(".agents/skills"), "late", "late", "d");
    assert_eq!(set.command("late"), None);
    assert_eq!(set.listed_file("late"), None);
}

#[test]
fn after_opened_the_new_skill_is_found() {
    let tree = Tree::new();
    skill(&tree.top().join(".agents/skills"), "tdd", "tdd", "d");
    let set = tree.set();
    assert!(set.command("tdd").is_some());
    let late = skill(&tree.top().join(".agents/skills"), "late", "late", "d");
    let collected = crate::opening::collect(&set.inputs_now(), &tree.top());
    set.opened(collected.found, set.inputs_now().skills_disabled);
    assert_eq!(set.command("late"), Some(late.clone()));
    assert_eq!(set.listed_file("late"), Some(late));
}

#[test]
fn command_returns_a_template_and_an_extension_prompt_but_listed_file_returns_neither() {
    let tree = Tree::new();
    let template = write(
        &tree.top().join(".agents/skills"),
        "template",
        "---\nname: template\ndescription: d\ndisable-model-invocation: true\n---\nBody.\n",
    );
    let ext = tree.root.join("ext");
    let prompt = skill(&ext.join("prompts"), "deploy", "deploy", "d");
    let mut inputs = tree.inputs();
    inputs.extension_dirs = vec![("acme".into(), ext)];
    let set = SkillSet::new(inputs, &tree.top());
    assert_eq!(set.command("template"), Some(template));
    assert_eq!(set.command("deploy"), Some(prompt));
    assert_eq!(set.listed_file("template"), None);
    assert_eq!(set.listed_file("deploy"), None);
}

#[test]
fn set_disabled_hides_one_name_and_leaves_another() {
    let tree = Tree::new();
    skill(&tree.top().join(".agents/skills"), "a", "a", "d");
    skill(&tree.top().join(".agents/skills"), "b", "b", "d");
    let set = tree.set();
    assert!(set.command("a").is_some());
    set.set_disabled(vec!["a".into()]);
    assert_eq!(set.command("a"), None);
    assert_eq!(set.listed_file("a"), None);
    assert!(set.is_disabled("a"));
    assert!(!set.is_disabled("b"));
    assert!(set.command("b").is_some());
}

#[test]
fn a_shared_name_returns_the_winners_file() {
    let tree = Tree::new();
    let won = skill(&tree.top().join(".agents/skills"), "a", "same", "d");
    skill(&tree.home().join("skills"), "a", "same", "d");
    let set = tree.set();
    assert_eq!(set.command("same"), Some(won.clone()));
    assert_eq!(set.listed_file("same"), Some(won));
}

#[test]
fn adopting_a_set_into_itself_is_a_no_op() {
    let tree = Tree::new();
    let path = skill(&tree.top().join(".agents/skills"), "tdd", "tdd", "d");
    let set = tree.set();
    assert_eq!(set.command("tdd"), Some(path.clone()));
    set.adopt(&set);
    assert_eq!(set.command("tdd"), Some(path));
}

#[test]
fn adopt_moves_the_state_so_a_reader_answers_from_it_not_disk() {
    let tree = Tree::new();
    let path = skill(&tree.top().join(".agents/skills"), "tdd", "tdd", "d");
    let from = tree.set();
    assert_eq!(from.command("tdd"), Some(path));
    let target = SkillSet::new(tree.inputs(), &tree.root.join("elsewhere"));
    let reader = target.reader();
    target.adopt(&from);
    assert!(reader.file("tdd").is_some());
    std::fs::remove_dir_all(tree.top().join(".agents/skills")).unwrap();
    assert!(reader.file("tdd").is_some());
    assert!(target.command("tdd").is_some());
}
