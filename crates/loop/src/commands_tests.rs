//! The `commands` answer's rows and notices over skills and MCP prompt
//! rows, on places built in a temporary directory.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};

use contract::ErrorCode;
use contract::SessionId;
use contract::commands::AddDirectoryArgs;
use contract::events::CommandInfo;
use contract::inbox::{Ack, Answer};
use fakes::Deadline;
use fakes::clock::FakeClock;
use log::Log;

use super::{commands, commands_from_found};
use crate::prompt::PromptInputs;
use crate::{CommandsRefresh, Loop, Model, Permissions, Session, SkillSet};

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
    set.sync_commands(&initial.runnable);

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
    set.sync_commands(&initial.runnable);
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
fn refreshing_rows_forwards_the_same_snapshot_shadow_notices() {
    let tree = Tree::new();
    skill(
        &tree.top().join(".fiber/skills"),
        "clash",
        "clash",
        "Skill.",
    );
    let inputs = tree.inputs();
    let checked = SkillSet::new(inputs.clone(), &tree.top()).check();
    let commands = Arc::new(Mutex::new(Vec::new()));
    let stored = Arc::clone(&commands);
    let clock = Arc::clone(&inputs.clock);
    let log = Arc::new(Log::create(&tree.root, SessionId("s_test".into()), clock).unwrap());
    let (_inbox, receiver) = mpsc::channel();
    let provider: Arc<dyn contract::provider::Provider> =
        Arc::new(fakes::ScriptedProvider::new(Vec::new()));
    let mut looped = Loop::start(
        Session {
            log,
            provider,
            model: Model {
                reference: "fake/model".into(),
                cost: None,
                subscription: false,
            },
            prompt: inputs.clone(),
            inbox: receiver,
            tools: Vec::new(),
            permissions: Permissions {
                workspace: tree.top().display().to_string(),
                credentials: tree.home().join("credentials"),
                credential_files: Vec::new(),
                rules: Arc::new(fakes::NoRules),
            },
        },
        None,
    )
    .unwrap()
    .answerable(false)
    .commands_refresh(CommandsRefresh {
        extensions: Arc::new(|| vec![extension("clash", "Extension.", "fiber.test/acme")]),
        replace: Arc::new(move |rows| *stored.lock().unwrap() = rows),
    });

    let notices = looped.refresh_commands(&checked.inputs, &checked.found);

    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].code, ErrorCode::SkillShadowed);
    assert_eq!(notices[0].extension.as_deref(), Some("fiber.test/acme"));
    assert_eq!(commands.lock().unwrap()[0].tag, "skill");
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

/// A loop on `tree` whose workspace is `top`, with its log.
fn started(tree: &Tree) -> (Loop, Arc<Log>) {
    let inputs = tree.inputs();
    let log = Arc::new(
        Log::create(&tree.root, SessionId("s_test".into()), inputs.clock.clone()).unwrap(),
    );
    let (_inbox, receiver) = mpsc::channel();
    let looped = Loop::start(
        Session {
            log: Arc::clone(&log),
            provider: Arc::new(fakes::ScriptedProvider::new(Vec::new())),
            model: Model {
                reference: "fake/model".into(),
                cost: None,
                subscription: false,
            },
            prompt: inputs,
            inbox: receiver,
            tools: Vec::new(),
            permissions: Permissions {
                workspace: tree.top().display().to_string(),
                credentials: tree.home().join("credentials"),
                credential_files: Vec::new(),
                rules: Arc::new(fakes::NoRules),
            },
        },
        None,
    )
    .unwrap();
    (looped, log)
}

/// An acknowledgement recording its answer, failing the test past the
/// deadline: every blocking wait carries one, so a bug fails instead of
/// hanging.
fn capture() -> (Ack, mpsc::Receiver<Answer>) {
    let (tx, rx) = mpsc::channel();
    (
        Ack(Box::new(move |answer| {
            drop(tx.send(answer));
        })),
        rx,
    )
}

fn answered(rx: &mpsc::Receiver<Answer>) -> Answer {
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(rx)
        .expect("the ack runs")
}

/// Every `directory_added` payload the loop's log holds, in order.
fn added(tree: &Tree) -> Vec<String> {
    log::read(&tree.root.join("s_test"))
        .unwrap()
        .into_iter()
        .filter(|line| line.kind == "directory_added")
        .map(|line| {
            line.payload
                .get("path")
                .and_then(|path| path.as_str())
                .unwrap()
                .to_owned()
        })
        .collect()
}

#[test]
fn add_directory_writes_the_resolved_directory_and_accepts() {
    let tree = Tree::new();
    let shared = tree.root.join("shared");
    std::fs::create_dir_all(&shared).unwrap();
    let resolved = shared.canonicalize().unwrap().display().to_string();
    let (mut looped, _) = started(&tree);
    let (ack, rx) = capture();
    looped
        .take_add_directory(
            AddDirectoryArgs {
                path: shared.display().to_string(),
            },
            ack,
        )
        .unwrap();
    assert!(matches!(answered(&rx), Ok(None)));
    assert_eq!(added(&tree), [resolved]);
}

#[test]
fn add_directory_resolves_a_relative_path_against_the_workspace() {
    let tree = Tree::new();
    let shared = tree.root.join("shared");
    std::fs::create_dir_all(&shared).unwrap();
    let resolved = shared.canonicalize().unwrap().display().to_string();
    let (mut looped, _) = started(&tree);
    let (ack, rx) = capture();
    looped
        .take_add_directory(
            AddDirectoryArgs {
                path: "../shared".into(),
            },
            ack,
        )
        .unwrap();
    assert!(matches!(answered(&rx), Ok(None)));
    // A relative path inside the workspace stays covered: no event.
    let sub = tree.top().join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    let (ack, rx) = capture();
    looped
        .take_add_directory(AddDirectoryArgs { path: "sub".into() }, ack)
        .unwrap();
    assert!(matches!(answered(&rx), Ok(None)));
    assert_eq!(added(&tree), [resolved]);
}

#[test]
fn add_directory_resolves_a_symlink_to_its_target() {
    let tree = Tree::new();
    let shared = tree.root.join("shared");
    std::fs::create_dir_all(&shared).unwrap();
    std::os::unix::fs::symlink(&shared, tree.top().join("link")).unwrap();
    let resolved = shared.canonicalize().unwrap().display().to_string();
    let (mut looped, _) = started(&tree);
    let (ack, rx) = capture();
    looped
        .take_add_directory(
            AddDirectoryArgs {
                path: "link".into(),
            },
            ack,
        )
        .unwrap();
    assert!(matches!(answered(&rx), Ok(None)));
    assert_eq!(added(&tree), [resolved]);
}

#[test]
fn add_directory_rejects_a_missing_or_non_directory_path() {
    for missing in [true, false] {
        let tree = Tree::new();
        let path = if missing {
            tree.root.join("missing").display().to_string()
        } else {
            let file = tree.root.join("note.txt");
            std::fs::write(&file, "hi").unwrap();
            file.display().to_string()
        };
        let (mut looped, _) = started(&tree);
        let (ack, rx) = capture();
        looped
            .take_add_directory(AddDirectoryArgs { path: path.clone() }, ack)
            .unwrap();
        let Err(rejection) = answered(&rx) else {
            panic!("{path} was accepted");
        };
        assert_eq!(rejection.code, ErrorCode::InvalidArguments, "{path}");
        assert!(added(&tree).is_empty(), "{path}");
    }
}

#[test]
fn add_directory_accepts_a_covered_path_without_an_event() {
    let tree = Tree::new();
    let sub = tree.top().join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    let (mut looped, _) = started(&tree);
    for path in [
        tree.top().display().to_string(),
        sub.display().to_string(),
        "sub".to_owned(),
    ] {
        let (ack, rx) = capture();
        looped
            .take_add_directory(AddDirectoryArgs { path: path.clone() }, ack)
            .unwrap();
        assert!(matches!(answered(&rx), Ok(None)), "{path}");
    }
    assert!(added(&tree).is_empty());
}

#[test]
fn add_directory_adds_an_already_added_folder_only_once() {
    let tree = Tree::new();
    let shared = tree.root.join("shared");
    std::fs::create_dir_all(&shared).unwrap();
    let resolved = shared.canonicalize().unwrap().display().to_string();
    let (mut looped, _) = started(&tree);
    for _ in 0..2 {
        let (ack, rx) = capture();
        looped
            .take_add_directory(
                AddDirectoryArgs {
                    path: shared.display().to_string(),
                },
                ack,
            )
            .unwrap();
        assert!(matches!(answered(&rx), Ok(None)));
    }
    assert_eq!(added(&tree), [resolved]);
}

#[test]
fn seed_inherited_writes_each_new_root_as_the_loops_own_event() {
    let tree = Tree::new();
    let shared = tree.root.join("shared");
    std::fs::create_dir_all(&shared).unwrap();
    let resolved = shared.canonicalize().unwrap();
    let (mut looped, _) = started(&tree);
    // A covered root writes nothing, and a root removed since the
    // parent folded it is skipped: only the new root becomes an event.
    looped
        .seed_inherited(vec![tree.top(), tree.root.join("missing"), shared.clone()])
        .unwrap();
    assert_eq!(added(&tree), [resolved.display().to_string()]);
    // Seeding again writes nothing more: the loop's own event covers
    // the root now.
    looped.seed_inherited(vec![shared]).unwrap();
    assert_eq!(added(&tree).len(), 1);
}
