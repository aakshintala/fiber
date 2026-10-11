//! Tests for [`SkillReader`], through the public API on real temporary
//! directories.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use contract::skills::{SkillRead, Skills};
use fakes::clock::FakeClock;

use super::SkillReader;
use crate::prompt::PromptInputs;
use crate::skill_set::SkillSet;

struct Tree {
    _held: fakes::TempDir,
    root: PathBuf,
}

impl Tree {
    fn new() -> Self {
        let held = fakes::TempDir::new("fiber-skill-reader");
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

    fn reader(&self) -> SkillReader {
        SkillSet::new(self.inputs(), &self.top()).reader()
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
fn a_listed_repository_skill_returns_the_path_discovery_opened() {
    let tree = Tree::new();
    let path = skill(&tree.top().join(".agents/skills"), "tdd", "tdd", "d");
    let reader = tree.reader();
    let file = reader.file("tdd").unwrap();
    assert_eq!(file, path);
    assert_eq!(file.display().to_string(), path.display().to_string());
}

#[test]
fn an_unknown_name_returns_none() {
    let tree = Tree::new();
    skill(&tree.top().join(".agents/skills"), "tdd", "tdd", "d");
    assert_eq!(tree.reader().file("nope"), None);
}

#[test]
fn a_skill_that_disables_model_invocation_returns_none() {
    let tree = Tree::new();
    write(
        &tree.top().join(".agents/skills"),
        "tdd",
        "---\nname: tdd\ndescription: d\ndisable-model-invocation: true\n---\nBody.\n",
    );
    assert_eq!(tree.reader().file("tdd"), None);
}

#[test]
fn a_name_in_skills_disabled_returns_none() {
    let tree = Tree::new();
    skill(&tree.top().join(".agents/skills"), "tdd", "tdd", "d");
    let mut inputs = tree.inputs();
    inputs.skills_disabled = vec!["tdd".into()];
    let reader = SkillSet::new(inputs, &tree.top()).reader();
    assert_eq!(reader.file("tdd"), None);
}

#[test]
fn a_skill_in_an_extension_prompts_returns_none() {
    let tree = Tree::new();
    let ext = tree.root.join("ext");
    let path = skill(&ext.join("prompts"), "tdd", "tdd", "d");
    assert!(path.exists());
    let mut inputs = tree.inputs();
    inputs.extension_dirs = vec![("acme".into(), ext)];
    let reader = SkillSet::new(inputs, &tree.top()).reader();
    assert_eq!(reader.file("tdd"), None);
}

#[test]
fn a_repository_skill_shadowing_a_personal_one_returns_the_repository_file() {
    let tree = Tree::new();
    skill(&tree.root.join("person/.agents/skills"), "tdd", "tdd", "d");
    let repo = skill(&tree.top().join(".agents/skills"), "tdd", "tdd", "d");
    assert_eq!(tree.reader().file("tdd"), Some(repo));
}

#[test]
fn with_two_skills_each_name_returns_its_own_file() {
    let tree = Tree::new();
    skill(&tree.top().join(".agents/skills"), "a", "a", "d");
    let b = skill(&tree.top().join(".agents/skills"), "b", "b", "d");
    assert_eq!(tree.reader().file("b"), Some(b));
}

#[test]
fn a_built_in_skill_under_home_docs_skills_is_found() {
    let tree = Tree::new();
    let path = skill(&tree.home().join("docs/skills"), "tdd", "tdd", "d");
    assert_eq!(tree.reader().file("tdd"), Some(path));
}

#[test]
fn a_workspace_below_a_git_repository_finds_a_skill_at_its_top_level() {
    let tree = Tree::new();
    let top = tree.top();
    std::fs::create_dir_all(top.join(".git")).unwrap();
    let path = skill(&top.join(".agents/skills"), "tdd", "tdd", "d");
    let sub = top.join("a/b");
    std::fs::create_dir_all(&sub).unwrap();
    let reader = SkillSet::new(tree.inputs(), &sub).reader();
    assert_eq!(reader.file("tdd"), Some(path));
}

#[test]
fn body_returns_the_trimmed_text_with_crlf_joined() {
    let tree = Tree::new();
    let path = write(
        &tree.top().join(".agents/skills"),
        "tdd",
        "---\nname: tdd\ndescription: d\n---\n\n\nLine one.\r\nLine two.\n\n\n",
    );
    let reader = tree.reader();
    assert_eq!(
        reader.body("tdd", &path),
        Ok("Line one.\nLine two.".to_owned())
    );
}

#[test]
fn a_rewritten_file_returns_the_new_body_on_the_second_call() {
    let tree = Tree::new();
    let path = skill(&tree.top().join(".agents/skills"), "tdd", "tdd", "d");
    let reader = tree.reader();
    assert_eq!(reader.body("tdd", &path), Ok("Body.".to_owned()));
    std::fs::write(&path, "---\nname: tdd\ndescription: d\n---\nNew body.\n").unwrap();
    assert_eq!(reader.body("tdd", &path), Ok("New body.".to_owned()));
}

#[test]
fn a_header_naming_another_skill_is_invalid() {
    let tree = Tree::new();
    let path = skill(&tree.top().join(".agents/skills"), "tdd", "other", "d");
    assert_eq!(tree.reader().body("tdd", &path), Err(SkillRead::Invalid));
}

#[test]
fn a_header_that_disables_model_invocation_is_invalid() {
    let tree = Tree::new();
    let path = write(
        &tree.top().join(".agents/skills"),
        "tdd",
        "---\nname: tdd\ndescription: d\ndisable-model-invocation: true\n---\nBody.\n",
    );
    assert_eq!(tree.reader().body("tdd", &path), Err(SkillRead::Invalid));
}

#[test]
fn a_file_with_no_header_is_invalid() {
    let tree = Tree::new();
    let path = write(
        &tree.top().join(".agents/skills"),
        "tdd",
        "Just prose, no header.\n",
    );
    assert_eq!(tree.reader().body("tdd", &path), Err(SkillRead::Invalid));
}

#[test]
fn a_directory_at_the_path_is_an_io_error_naming_the_path() {
    let tree = Tree::new();
    let dir = tree.top().join(".agents/skills/tdd");
    std::fs::create_dir_all(&dir).unwrap();
    let error = tree.reader().body("tdd", &dir).unwrap_err();
    let SkillRead::Io(message) = error else {
        panic!("not an Io error");
    };
    assert!(message.contains(&dir.display().to_string()), "{message}");
}
