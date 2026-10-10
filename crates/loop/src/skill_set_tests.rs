//! Tests for the maintained skill set, over places built in a temporary
//! directory.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::Duration;

use contract::ErrorCode;
use contract::events::{
    Environment, Event, Notice, OpeningMessage, SkillListed, SkillSource, SkillsChanged,
};
use contract::skills::Skills;
use contract::{Envelope, Seq, SessionId};
use fakes::clock::FakeClock;

use super::SkillSet;
use crate::prompt::{DisabledReader, PromptInputs};

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
    let collected = crate::opening::collect(&tree.inputs(), &tree.top());
    set.opened(
        collected.found,
        tree.inputs().skills_disabled,
        &collected.message.skills,
        &collected.notices,
    );
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

/// Opens `set` with what the opening message over `tree` sent.
fn open(set: &SkillSet, tree: &Tree) {
    let collected = crate::opening::collect(&tree.inputs(), &tree.top());
    set.opened(
        collected.found,
        tree.inputs().skills_disabled,
        &collected.message.skills,
        &collected.notices,
    );
}

/// The set for `tree` with `reader` as its `skills.disabled` re-read.
fn reading_set(tree: &Tree, reader: DisabledReader) -> SkillSet {
    let mut inputs = tree.inputs();
    inputs.skills_disabled_now = Some(reader);
    SkillSet::new(inputs, &tree.top())
}

/// A reader answering from `disabled`, shared so the test can switch
/// skills off and on between checks.
fn shared_reader(disabled: Arc<Mutex<Vec<String>>>) -> DisabledReader {
    Arc::new(move || Ok(disabled.lock().unwrap().clone()))
}

/// A `skills_changed` line for `event`, as the log holds it.
fn line(kind: &str, event: &Event) -> Envelope {
    Envelope {
        kind: kind.into(),
        session_id: SessionId("s_test".into()),
        ts: 1,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: Some(Seq(1)),
        payload: event.payload().unwrap(),
    }
}

/// An opening message listing `skills`.
fn opening_with(skills: Vec<SkillListed>) -> Event {
    Event::OpeningMessage(OpeningMessage {
        environment: Environment {
            date: "2026-10-10".into(),
            os: "test-os".into(),
            arch: "test-arch".into(),
            shell: "/bin/sh".into(),
            workspace: "/w".into(),
            git: None,
            session_log: "/s/events.jsonl".into(),
        },
        instruction_files: Vec::new(),
        extension_sections: Vec::new(),
        skills,
    })
}

/// A listed entry for `name`, as a resume folds it.
fn listed(name: &str) -> SkillListed {
    SkillListed {
        name: name.into(),
        description: "d".into(),
        path: format!("/w/.agents/skills/{name}/SKILL.md"),
        source: SkillSource::Repository,
    }
}

fn added_names(changed: &Option<contract::events::SkillsChanged>) -> Vec<&str> {
    changed
        .as_ref()
        .map(|changed| {
            changed
                .added
                .iter()
                .map(|entry| entry.name.as_str())
                .collect()
        })
        .unwrap_or_default()
}

fn removed_names(changed: &Option<contract::events::SkillsChanged>) -> Vec<&str> {
    changed
        .as_ref()
        .map(|changed| {
            changed
                .removed
                .iter()
                .map(String::as_str)
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn no_change_gives_none_and_no_notices() {
    let tree = Tree::new();
    skill(&tree.top().join(".agents/skills"), "a", "a", "d");
    let set = tree.set();
    open(&set, &tree);
    let checked = set.check();
    assert!(checked.changed.is_none());
    assert!(checked.notices.is_empty());
}

#[test]
fn two_skills_added_are_sorted_with_their_entries() {
    let tree = Tree::new();
    let place = tree.top().join(".agents/skills");
    let set = tree.set();
    open(&set, &tree);
    skill(&place, "zeta", "zeta", "Last.");
    skill(&place, "alpha", "alpha", "First.");
    let checked = set.check();
    assert!(checked.notices.is_empty());
    let changed = checked.changed.unwrap();
    assert!(changed.removed.is_empty());
    assert_eq!(added_names(&Some(changed.clone())), ["alpha", "zeta"]);
    let first = &changed.added[0];
    assert_eq!(first.name, "alpha");
    assert_eq!(first.description, "First.");
    assert!(
        first.path.ends_with("alpha/SKILL.md"),
        "{}",
        first.path
    );
    assert_eq!(first.source, SkillSource::Repository);
}

#[test]
fn two_removed_are_sorted() {
    let tree = Tree::new();
    let place = tree.top().join(".agents/skills");
    skill(&place, "zeta", "zeta", "d");
    skill(&place, "alpha", "alpha", "d");
    let set = tree.set();
    open(&set, &tree);
    std::fs::remove_dir_all(place.join("zeta")).unwrap();
    std::fs::remove_dir_all(place.join("alpha")).unwrap();
    let checked = set.check();
    assert!(added_names(&checked.changed).is_empty());
    assert_eq!(removed_names(&checked.changed), ["alpha", "zeta"]);
}

#[test]
fn one_added_and_one_removed_share_one_event() {
    let tree = Tree::new();
    let place = tree.top().join(".agents/skills");
    skill(&place, "a", "a", "d");
    let set = tree.set();
    open(&set, &tree);
    std::fs::remove_dir_all(place.join("a")).unwrap();
    skill(&place, "b", "b", "d");
    let checked = set.check();
    assert_eq!(added_names(&checked.changed), ["b"]);
    assert_eq!(removed_names(&checked.changed), ["a"]);
}

#[test]
fn a_description_edit_and_a_body_edit_give_none() {
    let tree = Tree::new();
    let place = tree.top().join(".agents/skills");
    let path = skill(&place, "a", "a", "old");
    let set = tree.set();
    open(&set, &tree);
    std::fs::write(&path, "---\nname: a\ndescription: new\n---\nBody.\n").unwrap();
    assert!(set.check().changed.is_none());
    std::fs::write(&path, "---\nname: a\ndescription: new\n---\nOther body.\n").unwrap();
    assert!(set.check().changed.is_none());
}

#[test]
fn the_reader_switching_a_skill_off_gives_removed_and_back_on_gives_added() {
    let tree = Tree::new();
    skill(&tree.top().join(".agents/skills"), "a", "a", "d");
    let disabled = Arc::new(Mutex::new(Vec::new()));
    let set = reading_set(&tree, shared_reader(disabled.clone()));
    open(&set, &tree);
    *disabled.lock().unwrap() = vec!["a".into()];
    let checked = set.check();
    assert!(added_names(&checked.changed).is_empty());
    assert_eq!(removed_names(&checked.changed), ["a"]);
    *disabled.lock().unwrap() = Vec::new();
    let checked = set.check();
    assert_eq!(added_names(&checked.changed), ["a"]);
    assert!(removed_names(&checked.changed).is_empty());
}

/// What the test reader answers: the disabled list, or a failure.
#[derive(Clone)]
enum Behavior {
    Ok(Vec<String>),
    Err(String),
}

fn behavior_reader(behavior: Arc<Mutex<Behavior>>) -> DisabledReader {
    Arc::new(move || match behavior.lock().unwrap().clone() {
        Behavior::Ok(names) => Ok(names),
        Behavior::Err(message) => Err(Notice {
            code: ErrorCode::ConfigInvalid,
            message,
            extension: None,
        }),
    })
}

#[test]
fn a_reader_failure_raises_its_notice_once_and_keeps_the_last_known_list() {
    let tree = Tree::new();
    let place = tree.top().join(".agents/skills");
    skill(&place, "a", "a", "d");
    skill(&place, "b", "b", "d");
    let behavior = Arc::new(Mutex::new(Behavior::Ok(Vec::new())));
    let set = reading_set(&tree, behavior_reader(behavior.clone()));
    open(&set, &tree);
    *behavior.lock().unwrap() = Behavior::Ok(vec!["a".into()]);
    let checked = set.check();
    assert_eq!(removed_names(&checked.changed), ["a"]);
    // The failure keeps the `["a"]` list: still `{b}`, so no change.
    *behavior.lock().unwrap() = Behavior::Err("boom 1".into());
    let checked = set.check();
    assert!(checked.changed.is_none());
    assert_eq!(checked.notices.len(), 1);
    assert_eq!(checked.notices[0].code, ErrorCode::ConfigInvalid);
    assert_eq!(checked.notices[0].message, "boom 1");
    // The same failure while it stays raises nothing.
    let checked = set.check();
    assert!(checked.changed.is_none());
    assert!(checked.notices.is_empty());
    // A different message is raised again.
    *behavior.lock().unwrap() = Behavior::Err("boom 2".into());
    let checked = set.check();
    assert_eq!(checked.notices.len(), 1);
    assert_eq!(checked.notices[0].message, "boom 2");
    // Recovery raises nothing.
    *behavior.lock().unwrap() = Behavior::Ok(vec!["a".into()]);
    let checked = set.check();
    assert!(checked.changed.is_none());
    assert!(checked.notices.is_empty());
}

#[test]
fn without_a_reader_set_disabled_holds() {
    let tree = Tree::new();
    skill(&tree.top().join(".agents/skills"), "a", "a", "d");
    let set = tree.set();
    open(&set, &tree);
    set.set_disabled(vec!["a".into()]);
    let checked = set.check();
    assert_eq!(removed_names(&checked.changed), ["a"]);
}

#[test]
fn an_invalid_skill_added_is_left_out_with_one_notice_until_it_is_fixed() {
    let tree = Tree::new();
    let place = tree.top().join(".agents/skills");
    skill(&place, "a", "a", "d");
    let set = tree.set();
    open(&set, &tree);
    write(&place, "b", "no header at all\n");
    let checked = set.check();
    assert!(checked.changed.is_none());
    assert_eq!(checked.notices.len(), 1);
    assert_eq!(checked.notices[0].code, ErrorCode::SkillInvalid);
    let checked = set.check();
    assert!(checked.notices.is_empty());
    write(&place, "b", "---\nname: b\ndescription: d\n---\nBody.\n");
    let checked = set.check();
    assert_eq!(added_names(&checked.changed), ["b"]);
    assert!(checked.notices.is_empty());
}

#[test]
fn an_outranking_skill_raises_shadowed_once_with_no_line_and_loads_the_winner() {
    let tree = Tree::new();
    skill(&tree.home().join("skills"), "p", "same", "personal");
    let set = tree.set();
    open(&set, &tree);
    let won = skill(&tree.top().join(".agents/skills"), "r", "same", "repo");
    let checked = set.check();
    assert!(checked.changed.is_none());
    assert_eq!(checked.notices.len(), 1);
    let notice = &checked.notices[0];
    assert_eq!(notice.code, ErrorCode::SkillShadowed);
    assert!(
        notice.message.contains(&won.display().to_string()),
        "{}",
        notice.message
    );
    assert_eq!(set.listed_file("same"), Some(won));
}

#[test]
fn a_deleted_winner_with_a_shadowed_skill_left_adds_nothing_and_loads_the_remainder() {
    let tree = Tree::new();
    let place = tree.top().join(".agents/skills");
    skill(&place, "r", "same", "repo");
    let kept = skill(&tree.home().join("skills"), "p", "same", "personal");
    let set = tree.set();
    open(&set, &tree);
    std::fs::remove_dir_all(place.join("r")).unwrap();
    let checked = set.check();
    assert!(checked.changed.is_none());
    assert_eq!(set.listed_file("same"), Some(kept));
}

#[test]
fn a_template_added_adds_nothing_but_command_finds_it() {
    let tree = Tree::new();
    let set = tree.set();
    open(&set, &tree);
    let template = write(
        &tree.top().join(".agents/skills"),
        "t",
        "---\nname: t\ndescription: d\ndisable-model-invocation: true\n---\nBody.\n",
    );
    let checked = set.check();
    assert!(checked.changed.is_none());
    assert_eq!(set.command("t"), Some(template));
}

#[test]
fn a_header_turning_on_disable_model_invocation_removes_the_skill() {
    let tree = Tree::new();
    let place = tree.top().join(".agents/skills");
    let path = skill(&place, "a", "a", "d");
    let set = tree.set();
    open(&set, &tree);
    std::fs::write(
        &path,
        "---\nname: a\ndescription: d\ndisable-model-invocation: true\n---\nBody.\n",
    )
    .unwrap();
    let checked = set.check();
    assert_eq!(removed_names(&checked.changed), ["a"]);
}

#[cfg(unix)]
fn deny(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
}

#[cfg(unix)]
fn allow(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    // `0o755`, not `0o644`: a directory without its execute bit cannot
    // be traversed again.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
#[test]
fn an_unreadable_place_keeps_its_skill_while_a_readable_ones_is_removed() {
    let tree = Tree::new();
    let place = tree.top().join(".agents/skills");
    let kept = skill(&place, "kept", "kept", "d");
    skill(&tree.home().join("skills"), "doomed", "doomed", "d");
    let target = tree.root.join("secret");
    skill(&target, "real", "linked", "d");
    std::os::unix::fs::symlink(target.join("real"), place.join("link")).unwrap();
    let elsewhere = tree.root.join("elsewhere");
    skill(&elsewhere, "real", "dangling", "d");
    std::os::unix::fs::symlink(elsewhere.join("real"), place.join("link2")).unwrap();
    let set = tree.set();
    open(&set, &tree);
    // The place and the linked target go dark; the other place stays
    // readable and loses its skill.
    deny(&place);
    deny(&target);
    std::fs::remove_dir_all(tree.home().join("skills/doomed")).unwrap();
    let checked = set.check();
    allow(&place);
    allow(&target);
    assert_eq!(removed_names(&checked.changed), ["doomed"]);
    assert_eq!(set.listed_file("kept"), Some(kept.clone()));
    assert!(set.listed_file("linked").is_some());
    assert_eq!(checked.notices.len(), 1);
    assert_eq!(checked.notices[0].code, ErrorCode::IoFailed);
    // The link whose target is now nothing is removed, while the kept
    // skills stay listed.
    std::fs::remove_dir_all(&elsewhere).unwrap();
    let checked = set.check();
    assert_eq!(removed_names(&checked.changed), ["dangling"]);
    assert!(checked.notices.is_empty());
    // A `SKILL.md` that cannot be read keeps its skill with one notice.
    // The rewrite moves its size past the cache, so the check really
    // tries the read.
    std::fs::write(
        &kept,
        "---\nname: kept\ndescription: a changed description\n---\nBody.\n",
    )
    .unwrap();
    deny(&kept);
    let checked = set.check();
    allow(&kept);
    assert!(checked.changed.is_none());
    assert_eq!(set.listed_file("kept"), Some(kept));
    assert_eq!(checked.notices.len(), 1);
    assert_eq!(checked.notices[0].code, ErrorCode::IoFailed);
}

#[test]
fn a_reader_that_calls_back_into_the_set_does_not_deadlock() {
    for which in 0..3 {
        let tree = Tree::new();
        skill(&tree.top().join(".agents/skills"), "a", "a", "d");
        let gate = Arc::new(Mutex::new(()));
        let slot: Arc<OnceLock<SkillSet>> = Arc::new(OnceLock::new());
        let reader: DisabledReader = {
            let gate = Arc::clone(&gate);
            let slot = Arc::clone(&slot);
            Arc::new(move || {
                let _held = gate.lock().unwrap();
                if let Some(set) = slot.get() {
                    let _ = set.is_disabled("a");
                }
                Ok(Vec::new())
            })
        };
        let set = reading_set(&tree, reader);
        let _ = slot.set(set.clone());
        let (done, finished) = mpsc::channel();
        std::thread::spawn(move || {
            match which {
                0 => {
                    set.check();
                }
                1 => {
                    set.inputs_now();
                }
                _ => {
                    set.command("a");
                }
            }
            done.send(()).unwrap();
        });
        finished.recv_timeout(Duration::from_secs(5)).unwrap();
    }
}

#[test]
fn resume_then_a_lazy_read_leaves_the_baseline_for_the_first_check() {
    let tree = Tree::new();
    let place = tree.top().join(".agents/skills");
    skill(&place, "a", "a", "d");
    write(
        &place,
        "t",
        "---\nname: t\ndescription: d\ndisable-model-invocation: true\n---\nBody.\n",
    );
    let set = tree.set();
    set.resumed(&[line("opening_message", &opening_with(vec![listed("a")]))])
        .unwrap();
    // The template resolves from the lazy read, before any check.
    assert!(set.command("t").is_some());
    assert!(set.check().changed.is_none());
    // With `a` deleted before the first lookup, the lazy read still
    // leaves the baseline alone: the first check removes it.
    let tree = Tree::new();
    let place = tree.top().join(".agents/skills");
    skill(&place, "a", "a", "d");
    write(
        &place,
        "t",
        "---\nname: t\ndescription: d\ndisable-model-invocation: true\n---\nBody.\n",
    );
    let set = tree.set();
    set.resumed(&[line("opening_message", &opening_with(vec![listed("a")]))])
        .unwrap();
    std::fs::remove_dir_all(place.join("a")).unwrap();
    assert!(set.command("t").is_some());
    let checked = set.check();
    assert_eq!(removed_names(&checked.changed), ["a"]);
}

#[test]
fn opened_resets_the_baseline_so_a_present_skill_is_never_added() {
    let tree = Tree::new();
    skill(&tree.top().join(".agents/skills"), "a", "a", "d");
    let set = tree.set();
    open(&set, &tree);
    let checked = set.check();
    assert!(checked.changed.is_none());
}

#[test]
fn resumed_folds_from_the_last_opening_message() {
    let tree = Tree::new();
    let set = tree.set();
    let context = vec![
        line("opening_message", &opening_with(vec![listed("x")])),
        line(
            "skills_changed",
            &Event::SkillsChanged(SkillsChanged {
                added: vec![listed("y")],
                removed: Vec::new(),
            }),
        ),
        line("opening_message", &opening_with(vec![listed("z")])),
        line(
            "skills_changed",
            &Event::SkillsChanged(SkillsChanged {
                added: vec![listed("w")],
                removed: vec!["z".into()],
            }),
        ),
    ];
    set.resumed(&context).unwrap();
    // Only `w` on disk: the baseline is exactly `{w}`, so no change.
    skill(&tree.top().join(".agents/skills"), "w", "w", "d");
    let checked = set.check();
    assert!(checked.changed.is_none());
}

#[test]
fn changed_text_renders_added_then_removed_on_one_line_each() {
    let changed = SkillsChanged {
        added: vec![
            SkillListed {
                name: "zeta".into(),
                description: "last".into(),
                path: "z".into(),
                source: SkillSource::Repository,
            },
            SkillListed {
                name: "a\nb".into(),
                description: "x\r\ny".into(),
                path: "p".into(),
                source: SkillSource::Repository,
            },
        ],
        removed: vec!["c\n\nd".into()],
    };
    assert_eq!(
        SkillSet::changed_text(&changed),
        "Fiber: skill a b can now be loaded: x y\nFiber: skill zeta can now be loaded: last\nFiber: skill c d was removed and can no longer be loaded."
    );
}

#[test]
fn a_lookup_consults_the_reader_before_any_check() {
    let tree = Tree::new();
    skill(&tree.top().join(".agents/skills"), "a", "a", "d");
    let set = reading_set(
        &tree,
        shared_reader(Arc::new(Mutex::new(vec!["a".into()]))),
    );
    assert_eq!(set.command("a"), None);
    assert!(set.is_disabled("a"));
}
