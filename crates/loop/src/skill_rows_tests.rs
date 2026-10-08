//! Tests for the `skills` answer's rows over discovery's winners and
//! losers, on places built in a temporary directory.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use contract::events::SkillSource;
use fakes::clock::FakeClock;

use super::{discover, rows};
use crate::prompt::PromptInputs;

/// A temporary tree: `top` is the repository's top level, `home` is
/// Fiber home, `person` the person's home.
struct Tree {
    _held: fakes::TempDir,
    root: PathBuf,
}

impl Tree {
    fn new() -> Self {
        let held = fakes::TempDir::new("fiber-skill-rows");
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

    fn person(&self) -> PathBuf {
        self.root.join("person")
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
        inputs.agents_home = Some(self.person());
        inputs
    }

    fn with_extension(&self, name: &str) -> (PromptInputs, PathBuf) {
        let mut inputs = self.inputs();
        let dir = self.root.join("ext").join(name);
        inputs.extension_dirs = vec![(name.into(), dir.clone())];
        (inputs, dir)
    }
}

/// Writes `<place>/<entry>/SKILL.md` with `name` and `description`, and
/// returns its path.
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
fn a_shadowed_skill_follows_its_winner_and_each_names_the_other() {
    let tree = Tree::new();
    let won = skill(&tree.top().join(".fiber/skills"), "a", "same", "first");
    let lost = skill(&tree.home().join("skills"), "a", "same", "second");
    let found = discover(&tree.inputs(), &tree.top());
    let got = rows(&found, &[]);
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].name, "same");
    assert_eq!(got[0].description, "first");
    assert_eq!(got[0].path, won.display().to_string());
    assert_eq!(got[0].source, SkillSource::Repository);
    assert_eq!(got[0].shadows, [lost.display().to_string()]);
    assert_eq!(got[0].shadowed_by, None);
    assert_eq!(got[1].name, "same");
    assert_eq!(got[1].description, "second");
    assert_eq!(got[1].path, lost.display().to_string());
    assert_eq!(got[1].source, SkillSource::Personal);
    assert!(got[1].shadows.is_empty());
    assert_eq!(got[1].shadowed_by, Some(won.display().to_string()));
}

#[test]
fn two_winners_each_list_only_their_own_shadowed_skill() {
    let tree = Tree::new();
    let won_a = skill(&tree.top().join(".fiber/skills"), "a", "a", "d");
    let won_b = skill(&tree.top().join(".agents/skills"), "a", "b", "d");
    let lost_a = skill(&tree.home().join("skills"), "a", "a", "d");
    let lost_b = skill(&tree.person().join(".agents/skills"), "a", "b", "d");
    let found = discover(&tree.inputs(), &tree.top());
    let got = rows(&found, &[]);
    assert_eq!(
        got.iter().map(|row| row.name.as_str()).collect::<Vec<_>>(),
        ["a", "a", "b", "b"]
    );
    assert_eq!(got[0].shadows, [lost_a.display().to_string()]);
    assert_eq!(got[1].shadowed_by, Some(won_a.display().to_string()));
    assert_eq!(got[2].shadows, [lost_b.display().to_string()]);
    assert_eq!(got[3].shadowed_by, Some(won_b.display().to_string()));
}

#[test]
fn a_three_way_clash_gives_one_winner_shadowing_two_in_discovery_order() {
    let tree = Tree::new();
    let won = skill(&tree.top().join(".fiber/skills"), "a", "same", "d");
    let second = skill(&tree.home().join("skills"), "a", "same", "d");
    let third = skill(&tree.person().join(".agents/skills"), "a", "same", "d");
    let found = discover(&tree.inputs(), &tree.top());
    let got = rows(&found, &[]);
    assert_eq!(got.len(), 3);
    assert_eq!(
        got[0].shadows,
        [second.display().to_string(), third.display().to_string()]
    );
    assert_eq!(got[1].shadowed_by, Some(won.display().to_string()));
    assert_eq!(got[2].shadowed_by, Some(won.display().to_string()));
    assert!(got[1].shadows.is_empty());
    assert!(got[2].shadows.is_empty());
}

#[test]
fn a_switched_off_name_marks_the_winner_and_its_shadowed_skill() {
    let tree = Tree::new();
    skill(&tree.top().join(".fiber/skills"), "a", "off", "d");
    skill(&tree.home().join("skills"), "a", "off", "d");
    skill(&tree.home().join("skills"), "b", "on", "d");
    let found = discover(&tree.inputs(), &tree.top());
    let got = rows(&found, &["off".to_owned()]);
    assert_eq!(got.len(), 3);
    assert!(got[0].disabled, "the winner is marked");
    assert!(got[1].disabled, "its shadowed skill is marked");
    assert!(!got[2].disabled, "the other name is not marked");
}

#[test]
fn a_name_not_switched_off_is_not_marked() {
    let tree = Tree::new();
    skill(&tree.top().join(".fiber/skills"), "a", "off", "d");
    skill(&tree.home().join("skills"), "a", "on", "d");
    let found = discover(&tree.inputs(), &tree.top());
    let got = rows(&found, &["off".to_owned()]);
    assert_eq!(got.len(), 2);
    assert!(!got.iter().any(|row| row.name == "on" && row.disabled));
    assert!(got.iter().any(|row| row.name == "on" && !row.disabled));
    assert!(got.iter().any(|row| row.name == "off" && row.disabled));
}

#[test]
fn switching_off_the_winner_never_promotes_the_shadowed_skill() {
    let tree = Tree::new();
    let won = skill(&tree.top().join(".fiber/skills"), "a", "same", "first");
    skill(&tree.home().join("skills"), "a", "same", "second");
    let found = discover(&tree.inputs(), &tree.top());
    let got = rows(&found, &["same".to_owned()]);
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].path, won.display().to_string());
    assert!(got[0].disabled);
    assert_eq!(got[1].shadowed_by, Some(won.display().to_string()));
    assert!(got[1].disabled);
}

#[test]
fn an_extensions_skill_names_its_extension_and_no_other_source_does() {
    let tree = Tree::new();
    skill(&tree.top().join(".fiber/skills"), "a", "repo", "d");
    skill(&tree.home().join("skills"), "a", "home", "d");
    skill(&tree.home().join("docs/skills"), "a", "built", "d");
    let (inputs, dir) = tree.with_extension("acme");
    skill(&dir.join("skills"), "a", "ext", "d");
    let found = discover(&inputs, &tree.top());
    let got = rows(&found, &[]);
    assert_eq!(got.len(), 4);
    let by_name = |name: &str| got.iter().find(|row| row.name == name).unwrap();
    assert_eq!(by_name("repo").source, SkillSource::Repository);
    assert_eq!(by_name("repo").extension, None);
    assert_eq!(by_name("home").source, SkillSource::Personal);
    assert_eq!(by_name("home").extension, None);
    assert_eq!(by_name("built").source, SkillSource::Builtin);
    assert_eq!(by_name("built").extension, None);
    assert_eq!(by_name("ext").source, SkillSource::Extension);
    assert_eq!(by_name("ext").extension, Some("acme".to_owned()));
}

#[test]
fn a_prompts_skill_and_a_disable_model_invocation_skill_are_not_model_invocable() {
    let tree = Tree::new();
    skill(&tree.top().join(".fiber/skills"), "a", "plain", "d");
    write(
        &tree.home().join("skills"),
        "b",
        "---\nname: hidden\ndescription: d\ndisable-model-invocation: true\n---\nBody.\n",
    );
    let (inputs, dir) = tree.with_extension("acme");
    skill(&dir.join("prompts"), "a", "prompted", "d");
    let found = discover(&inputs, &tree.top());
    let got = rows(&found, &[]);
    let by_name = |name: &str| got.iter().find(|row| row.name == name).unwrap();
    assert!(by_name("plain").model_invocable);
    assert!(!by_name("hidden").model_invocable);
    assert!(!by_name("prompted").model_invocable);
    assert_eq!(by_name("prompted").extension, Some("acme".to_owned()));
}

#[test]
fn model_invocable_is_unchanged_by_disabled_or_shadowing() {
    let tree = Tree::new();
    skill(&tree.top().join(".fiber/skills"), "a", "same", "d");
    skill(&tree.home().join("skills"), "a", "same", "d");
    let found = discover(&tree.inputs(), &tree.top());
    let got = rows(&found, &["same".to_owned()]);
    assert_eq!(got.len(), 2);
    assert!(got[0].model_invocable);
    assert!(got[1].model_invocable);
}

#[test]
fn an_invalid_skill_has_no_row_and_shadows_nothing() {
    // An invalid loser: the winner stands alone, with no shadowed row.
    let tree = Tree::new();
    skill(&tree.top().join(".fiber/skills"), "a", "same", "first");
    write(
        &tree.home().join("skills"),
        "a",
        "---\nname: same\n---\nBody.\n",
    );
    let found = discover(&tree.inputs(), &tree.top());
    let got = rows(&found, &[]);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].description, "first");
    assert!(got[0].shadows.is_empty());
    // An invalid would-be winner: the later skill wins it outright.
    let tree = Tree::new();
    let won = write(
        &tree.home().join("skills"),
        "a",
        "---\nname: same\ndescription: second\n---\nBody.\n",
    );
    write(
        &tree.top().join(".fiber/skills"),
        "a",
        "---\nname: same\n---\nBody.\n",
    );
    let found = discover(&tree.inputs(), &tree.top());
    let got = rows(&found, &[]);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].path, won.display().to_string());
    assert!(got[0].shadows.is_empty());
    assert_eq!(got[0].shadowed_by, None);
}

#[test]
fn a_place_reached_twice_gives_one_row() {
    // The person's home is a symlink to the repository's top level, so
    // `~/.agents/skills` is the repository's `.agents/skills` through a
    // second path: discovery reads it once and it shadows nothing.
    let tree = Tree::new();
    let path = skill(&tree.top().join(".agents/skills"), "a", "same", "d");
    let link = tree.root.join("person-link");
    std::os::unix::fs::symlink(tree.top(), &link).unwrap();
    let mut inputs = tree.inputs();
    inputs.agents_home = Some(link);
    let found = discover(&inputs, &tree.top());
    let got = rows(&found, &[]);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].path, path.display().to_string());
    assert!(got[0].shadows.is_empty());
    assert!(found.notices.is_empty());
}

#[test]
fn no_place_means_no_rows() {
    let tree = Tree::new();
    let found = tree_inputs_discover(&tree);
    assert!(rows(&found, &[]).is_empty());
    assert!(rows(&found, &["missing".to_owned()]).is_empty());
}

fn tree_inputs_discover(tree: &Tree) -> super::Discovered {
    discover(&tree.inputs(), &tree.top())
}

#[test]
fn the_session_skills_are_read_from_the_repository_top_level() {
    let tree = Tree::new();
    std::fs::create_dir_all(tree.top().join(".git")).unwrap();
    let workspace = tree.top().join("work");
    std::fs::create_dir_all(&workspace).unwrap();
    let path = skill(&tree.top().join(".agents/skills"), "a", "top", "d");
    let got = crate::skills(&tree.inputs(), &workspace);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].name, "top");
    assert_eq!(got[0].path, path.display().to_string());
}

#[test]
fn discovery_still_keeps_one_winner_per_name_and_the_same_notices() {
    let tree = Tree::new();
    let won = skill(&tree.top().join(".fiber/skills"), "a", "same", "first");
    let lost = skill(&tree.home().join("skills"), "a", "same", "second");
    let other = skill(&tree.home().join("skills"), "b", "other", "d");
    let found = discover(&tree.inputs(), &tree.top());
    assert_eq!(
        found
            .skills
            .iter()
            .map(|skill| skill.listed.name.as_str())
            .collect::<Vec<_>>(),
        ["same", "other"]
    );
    assert_eq!(found.skills[0].listed.description, "first");
    assert_eq!(found.skills[1].listed.path, other.display().to_string());
    let messages: Vec<String> = found
        .notices
        .iter()
        .map(|notice| notice.message.clone())
        .collect();
    assert_eq!(
        messages,
        [format!(
            "Skill same at {} is shadowed by {}, which is used.",
            lost.display(),
            won.display()
        )]
    );
    assert_eq!(found.shadowed.len(), 1);
    assert_eq!(
        found.shadowed[0].found.listed.path,
        lost.display().to_string()
    );
    assert_eq!(found.shadowed[0].by, won.display().to_string());
}
