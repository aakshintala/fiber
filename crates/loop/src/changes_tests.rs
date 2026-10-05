//! Tests for `changes`: the turn-start check, the own-edit tracking, the
//! subdirectory files, the date line, and the resume fold. File
//! modification times are set explicitly, never waited for.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use contract::events::{Event, InstructionReason, InstructionSent};
use contract::shapes::{DeclaredEffects, Effect};
use contract::{Envelope, Seq, SessionId};
use fakes::clock::FakeClock;

use super::{State, apply, clean, send_as_diff, unified_diff};
use crate::opening;
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

fn root() -> (PathBuf, fakes::TempDir) {
    let held = fakes::TempDir::new("fiber-changes");
    (held.path().to_path_buf(), held)
}

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, content).unwrap();
}

fn set_mtime(path: &Path, secs: u64) {
    let file = std::fs::File::options().write(true).open(path).unwrap();
    file.set_modified(UNIX_EPOCH + Duration::from_secs(secs))
        .unwrap();
}

fn readonly(path: &Path, yes: bool) {
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    permissions.set_readonly(yes);
    std::fs::set_permissions(path, permissions).unwrap();
}

/// The state after the opening message over `workspace` in `home`: built
/// from the message and folded into what the model had, as the loop's
/// render does when it writes the line.
fn initial(home: &Path, workspace: &Path, clock: &Arc<FakeClock>) -> State {
    let collected = opening::collect(&inputs(home, clock), workspace);
    let mut state = State::initial(&collected.message, workspace, home);
    apply(&mut state.had, &Event::OpeningMessage(collected.message));
    state
}

fn declared(paths: Option<&[&str]>) -> DeclaredEffects {
    DeclaredEffects {
        effects: vec![Effect::Writes],
        reversible: true,
        paths: paths.map(|paths| paths.iter().map(|path| (*path).to_owned()).collect()),
    }
}

fn envelope(kind: &str, event: &Event) -> Envelope {
    Envelope {
        kind: kind.into(),
        session_id: SessionId("s_test".into()),
        ts: 1,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        // Durable lines carry `seq`: without it the fold skips them.
        seq: Some(Seq(1)),
        payload: event.payload().unwrap(),
    }
}

#[test]
fn unified_diff_headers_name_the_path_on_both_sides() {
    let diff = unified_diff("a\nb\n", "a\nc\n", "/w/AGENTS.md");
    assert!(diff.contains("--- /w/AGENTS.md"), "{diff}");
    assert!(diff.contains("+++ /w/AGENTS.md"), "{diff}");
    assert!(diff.contains("-b\n"), "{diff}");
    assert!(diff.contains("+c\n"), "{diff}");
}

#[test]
fn unified_diff_of_identical_text_is_empty() {
    assert_eq!(unified_diff("a\n", "a\n", "/w/AGENTS.md"), "");
}

#[test]
fn send_as_diff_is_full_only_when_the_diff_is_longer() {
    assert_eq!(send_as_diff(5, 10), InstructionSent::Diff);
    // Exactly as long is not longer: still a diff.
    assert_eq!(send_as_diff(7, 7), InstructionSent::Diff);
    assert_eq!(send_as_diff(10, 5), InstructionSent::Full);
}

#[test]
fn apply_opening_replaces_what_the_model_had() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    write(&workspace.join("AGENTS.md"), "Leaf.\n");
    let fake = clock();
    let message = opening::collect(&inputs(&home, &fake), &workspace).message;
    let mut had = BTreeMap::from([("stale".to_owned(), "old".to_owned())]);
    apply(&mut had, &Event::OpeningMessage(message.clone()));
    assert_eq!(had.len(), 1);
    assert_eq!(
        had.get(&message.instruction_files[0].path),
        Some(&"Leaf.\n".to_owned())
    );
}

#[test]
fn apply_instruction_sets_and_clears_one_path() {
    let mut had = BTreeMap::new();
    let set = Event::InstructionFile(contract::events::InstructionFile {
        path: "/w/AGENTS.md".into(),
        reason: InstructionReason::Changed,
        content: Some("new".into()),
        sent: InstructionSent::Diff,
    });
    apply(&mut had, &set);
    assert_eq!(had.get("/w/AGENTS.md"), Some(&"new".to_owned()));
    let clear = Event::InstructionFile(contract::events::InstructionFile {
        path: "/w/AGENTS.md".into(),
        reason: InstructionReason::Deleted,
        content: None,
        sent: InstructionSent::Deleted,
    });
    apply(&mut had, &clear);
    assert!(!had.contains_key("/w/AGENTS.md"));
}

#[test]
fn apply_ignores_every_other_line() {
    let mut had = BTreeMap::from([("a".to_owned(), "b".to_owned())]);
    apply(
        &mut had,
        &Event::Notice(contract::events::Notice {
            code: contract::ErrorCode::IoFailed,
            message: "nope".into(),
            extension: None,
        }),
    );
    assert_eq!(had, BTreeMap::from([("a".to_owned(), "b".to_owned())]));
}

#[test]
fn clean_drops_dots_and_applies_dotdots_lexically() {
    let workspace = Path::new("/w");
    assert_eq!(
        clean(&workspace.join("a/./b")),
        Some(PathBuf::from("/w/a/b"))
    );
    assert_eq!(
        clean(&workspace.join("a/b/../c")),
        Some(PathBuf::from("/w/a/c"))
    );
    // Past the root the path does not resolve.
    assert_eq!(clean(Path::new("/..")), None);
    assert_eq!(clean(Path::new("../x")), None);
    assert_eq!(clean(&workspace.join("sub")), Some(PathBuf::from("/w/sub")));
}

/// `path` with symlinks resolved, as the state records paths.
fn canon(path: &Path) -> PathBuf {
    path.canonicalize().unwrap()
}

#[test]
fn no_change_sends_nothing() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    write(&workspace.join("AGENTS.md"), "Leaf.\n");
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
    assert!(out.notices.is_empty());
    assert!(out.date.is_none());
}

#[test]
fn size_and_mtime_equal_means_unchanged_without_reading() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    let file = workspace.join("AGENTS.md");
    write(&file, "Leaf.\n");
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    // Unreadable, but with the same size and time: the shortcut skips the
    // read, so no `io_failed` names it.
    readonly(&file, true);
    let out = state.check(&*fake);
    readonly(&file, false);
    assert!(out.files.is_empty());
    assert!(out.notices.is_empty());
}

#[test]
fn mtime_change_with_same_content_sends_nothing() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    let file = workspace.join("AGENTS.md");
    write(&file, "Leaf.\n");
    set_mtime(&file, 2_000);
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    set_mtime(&file, 3_000);
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
    assert!(out.notices.is_empty());
    // Sizes moved on: a second check with no further change is quiet even
    // when the file can no longer be read.
    readonly(&file, true);
    let out = state.check(&*fake);
    readonly(&file, false);
    assert!(out.files.is_empty());
    assert!(out.notices.is_empty());
}

#[test]
fn changed_file_sends_a_diff() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    let file = workspace.join("AGENTS.md");
    let old = (0..100).map(|n| format!("line {n}\n")).collect::<String>();
    write(&file, &old);
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    let new = old.replace("line 50\n", "line fifty\n");
    write(&file, &new);
    let out = state.check(&*fake);
    assert!(out.notices.is_empty());
    assert_eq!(out.files.len(), 1);
    let change = &out.files[0];
    assert_eq!(change.path, canon(&file).display().to_string());
    assert_eq!(change.reason, InstructionReason::Changed);
    assert_eq!(change.sent, InstructionSent::Diff);
    assert_eq!(change.content.as_deref(), Some(new.as_str()));
}

#[test]
fn changed_file_sends_full_text_when_the_diff_is_longer() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    let file = workspace.join("AGENTS.md");
    write(&file, "ab\n");
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    // A wholesale change to a tiny file: the headers alone outweigh it.
    write(&file, "cd\n");
    let out = state.check(&*fake);
    assert_eq!(out.files.len(), 1);
    assert_eq!(out.files[0].reason, InstructionReason::Changed);
    assert_eq!(out.files[0].sent, InstructionSent::Full);
    assert_eq!(out.files[0].content.as_deref(), Some("cd\n"));
}

#[test]
fn deleted_file_sends_deleted_then_stays_silent() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    let file = workspace.join("AGENTS.md");
    write(&file, "Leaf.\n");
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    std::fs::remove_file(&file).unwrap();
    let out = state.check(&*fake);
    assert_eq!(out.files.len(), 1);
    assert_eq!(out.files[0].reason, InstructionReason::Deleted);
    assert_eq!(out.files[0].sent, InstructionSent::Deleted);
    assert_eq!(out.files[0].content, None);
    for line in &out.files {
        apply(&mut state.had, &Event::InstructionFile(line.clone()));
    }
    // Still gone: nothing more to say.
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
    assert!(out.notices.is_empty());
}

#[test]
fn deleted_then_recreated_file_is_created_with_full_text() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    let file = workspace.join("AGENTS.md");
    write(&file, "First.\n");
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    std::fs::remove_file(&file).unwrap();
    let out = state.check(&*fake);
    assert_eq!(out.files.len(), 1);
    for line in &out.files {
        apply(&mut state.had, &Event::InstructionFile(line.clone()));
    }
    // Back with new content: `created`, never a diff against the version
    // before deletion.
    write(&file, "Second.\n");
    let out = state.check(&*fake);
    assert_eq!(out.files.len(), 1);
    assert_eq!(out.files[0].reason, InstructionReason::Created);
    assert_eq!(out.files[0].sent, InstructionSent::Full);
    assert_eq!(out.files[0].content.as_deref(), Some("Second.\n"));
}

#[test]
fn new_workspace_file_is_created() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    assert!(state.check(&*fake).files.is_empty());
    let file = workspace.join("AGENTS.md");
    write(&file, "Leaf.\n");
    let out = state.check(&*fake);
    assert_eq!(out.files.len(), 1);
    assert_eq!(out.files[0].reason, InstructionReason::Created);
    assert_eq!(out.files[0].sent, InstructionSent::Full);
    assert_eq!(out.files[0].content.as_deref(), Some("Leaf.\n"));
}

#[test]
fn claude_file_is_created_then_agents_wins_beside_it() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    let claude = workspace.join("CLAUDE.md");
    write(&claude, "Claude rules.\n");
    let out = state.check(&*fake);
    assert_eq!(out.files.len(), 1);
    assert_eq!(out.files[0].path, canon(&claude).display().to_string());
    assert_eq!(out.files[0].reason, InstructionReason::Created);
    for line in &out.files {
        apply(&mut state.had, &Event::InstructionFile(line.clone()));
    }
    // An `AGENTS.md` beside the tracked `CLAUDE.md`: the new file is
    // `created`, and the tracked one stays tracked.
    let agents = workspace.join("AGENTS.md");
    write(&agents, "Agents rules.\n");
    let out = state.check(&*fake);
    assert_eq!(out.files.len(), 1);
    assert_eq!(out.files[0].path, canon(&agents).display().to_string());
    assert_eq!(out.files[0].reason, InstructionReason::Created);
    for line in &out.files {
        apply(&mut state.had, &Event::InstructionFile(line.clone()));
    }
    write(&claude, "Claude rules, revised.\n");
    let out = state.check(&*fake);
    assert_eq!(out.files.len(), 1);
    assert_eq!(out.files[0].path, canon(&claude).display().to_string());
    assert_eq!(out.files[0].reason, InstructionReason::Changed);
}

#[test]
fn pointer_only_claude_is_no_candidate_until_it_holds_rules() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    let claude = workspace.join("CLAUDE.md");
    write(&claude, "@AGENTS.md\n");
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
    assert!(out.notices.is_empty());
    write(&claude, "Real rules.\n");
    let out = state.check(&*fake);
    assert_eq!(out.files.len(), 1);
    assert_eq!(out.files[0].reason, InstructionReason::Created);
}

#[test]
fn unreadable_tracked_file_notices_once_per_stat_change() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    let file = workspace.join("AGENTS.md");
    write(&file, "Leaf.\n");
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    // A directory where the file was: size and time differ, and no read
    // succeeds, on any platform and user.
    std::fs::remove_file(&file).unwrap();
    std::fs::create_dir(&file).unwrap();
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
    assert_eq!(out.notices.len(), 1);
    assert!(out.notices[0].message.contains(&file.display().to_string()));
    // Same size and time: the notice does not repeat.
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
    assert!(out.notices.is_empty());
    // Changed again (an entry lands): the notice fires once more.
    write(&file.join("marker"), "x");
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
    assert_eq!(out.notices.len(), 1);
    std::fs::remove_dir_all(&file).unwrap();
}

#[test]
fn unreadable_untracked_candidate_is_silent() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    // A directory where a new file would be: nothing was ever sent, so
    // there is no change to report, and the next check meets it again.
    std::fs::create_dir(workspace.join("AGENTS.md")).unwrap();
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
    assert!(out.notices.is_empty());
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
    assert!(out.notices.is_empty());
}

#[test]
fn changes_come_in_path_order() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    write(&workspace.join("AGENTS.md"), "Leaf.\n");
    write(&home.join("AGENTS.md"), "Global.\n");
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    // A tracked change and an untracked creation together: one list, in
    // path order. A subdirectory nobody touched is not checked, so its
    // new file stays invisible until a call reaches it.
    write(&workspace.join("AGENTS.md"), "Leaf, revised.\n");
    write(&home.join("AGENTS.md"), "Global, revised.\n");
    write(&workspace.join("sub/AGENTS.md"), "Sub.\n");
    let out = state.check(&*fake);
    assert_eq!(out.files.len(), 2);
    let paths: Vec<&str> = out.files.iter().map(|file| file.path.as_str()).collect();
    let mut sorted = paths.clone();
    sorted.sort();
    assert_eq!(paths, sorted);
}

#[test]
fn date_advances_only_to_a_later_date() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    // The fake clock reads 2023-11-14T22:13:20Z.
    assert!(state.check(&*fake).date.is_none());
    fake.advance(Duration::from_secs(2 * 3_600));
    let out = state.check(&*fake);
    assert_eq!(
        out.date.as_ref().map(|date| date.date.as_str()),
        Some("2023-11-15")
    );
    // The new date is last given: no repeat.
    assert!(state.check(&*fake).date.is_none());
    // An earlier date never appends a line.
    state.date = "2099-01-01".to_owned();
    assert!(state.check(&*fake).date.is_none());
}

#[test]
fn own_edit_sends_nothing_but_moves_what_the_model_had() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    let file = workspace.join("AGENTS.md");
    write(&file, "Leaf.\n");
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    let workspace = canon(&workspace);
    write(&file, "Revised by the call.\n");
    let own = state.call_completed(&workspace, &declared(Some(&["AGENTS.md"])));
    assert_eq!(own.len(), 1);
    assert_eq!(own[0].reason, InstructionReason::OwnEdit);
    assert_eq!(own[0].sent, InstructionSent::None);
    assert_eq!(own[0].content.as_deref(), Some("Revised by the call.\n"));
    for line in &own {
        apply(&mut state.had, &Event::InstructionFile(line.clone()));
    }
    // The next check finds the file exactly as recorded.
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
    assert!(out.notices.is_empty());
}

#[test]
fn own_edit_with_equal_content_sends_nothing() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    let file = workspace.join("AGENTS.md");
    write(&file, "Leaf.\n");
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    let workspace = canon(&workspace);
    let own = state.call_completed(&workspace, &declared(Some(&["AGENTS.md"])));
    assert!(own.is_empty());
    assert!(state.take_queued().is_empty());
}

#[test]
fn own_call_deleting_a_tracked_file_is_an_empty_own_edit() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    let file = workspace.join("AGENTS.md");
    write(&file, "Leaf.\n");
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    std::fs::remove_file(&file).unwrap();
    let workspace = canon(&workspace);
    let own = state.call_completed(&workspace, &declared(Some(&["AGENTS.md"])));
    assert_eq!(own.len(), 1);
    assert_eq!(own[0].reason, InstructionReason::OwnEdit);
    assert_eq!(own[0].sent, InstructionSent::None);
    assert_eq!(own[0].content, None);
    for line in &own {
        apply(&mut state.had, &Event::InstructionFile(line.clone()));
    }
    // Tracked as absent: the turn-start check stays silent too.
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
}

#[test]
fn own_edit_ignores_undeclared_outside_and_unresolvable_paths() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    let file = workspace.join("AGENTS.md");
    write(&file, "Leaf.\n");
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    write(&file, "Changed.\n");
    // None of these names the tracked file.
    assert!(state.call_completed(&workspace, &declared(None)).is_empty());
    assert!(
        state
            .call_completed(&workspace, &declared(Some(&["other.txt"])))
            .is_empty()
    );
    assert!(
        state
            .call_completed(&workspace, &declared(Some(&["../outside.txt"])))
            .is_empty()
    );
    assert!(
        state
            .call_completed(
                &workspace,
                &declared(Some(&["../../../../../../../../../../x"]))
            )
            .is_empty()
    );
    assert!(state.take_queued().is_empty());
}

#[test]
fn own_edit_leaves_an_unreadable_file_as_it_was() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    let file = workspace.join("AGENTS.md");
    write(&file, "Leaf.\n");
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    readonly(&file, true);
    let workspace = canon(&workspace);
    let own = state.call_completed(&workspace, &declared(Some(&["AGENTS.md"])));
    readonly(&file, false);
    assert!(own.is_empty());
}

#[test]
fn subdirectory_file_queues_once_per_context() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    write(&workspace.join("sub/AGENTS.md"), "Sub rules.\n");
    let own = state.call_completed(&workspace, &declared(Some(&["sub/notes.txt"])));
    assert!(own.is_empty());
    let queued = state.take_queued();
    assert_eq!(queued.len(), 1);
    let Event::InstructionFile(line) = &queued[0] else {
        panic!("queued a subdirectory line");
    };
    assert_eq!(line.reason, InstructionReason::Subdirectory);
    assert_eq!(line.sent, InstructionSent::Full);
    assert_eq!(line.content.as_deref(), Some("Sub rules.\n"));
    assert!(state.take_queued().is_empty());
    // Once per context: touching it again queues nothing.
    let own = state.call_completed(&workspace, &declared(Some(&["sub/other.txt"])));
    assert!(own.is_empty());
    assert!(state.take_queued().is_empty());
}

#[test]
fn subdirectory_walk_reaches_nested_dirs_and_a_declared_dir() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    write(&workspace.join("a/b/AGENTS.md"), "Deep rules.\n");
    std::fs::create_dir_all(workspace.join("plain")).unwrap();
    write(&workspace.join("plain/AGENTS.md"), "Plain rules.\n");
    let own = state.call_completed(&workspace, &declared(Some(&["a/b/c.txt", "plain"])));
    assert!(own.is_empty());
    let queued = state.take_queued();
    assert_eq!(queued.len(), 2);
    let mut paths: Vec<String> = queued
        .iter()
        .map(|event| {
            let Event::InstructionFile(line) = event else {
                panic!("queued subdirectory lines");
            };
            assert_eq!(line.reason, InstructionReason::Subdirectory);
            line.path.clone()
        })
        .collect();
    paths.sort();
    assert_eq!(
        paths,
        vec![
            workspace.join("a/b/AGENTS.md").display().to_string(),
            workspace.join("plain/AGENTS.md").display().to_string(),
        ]
    );
}

#[test]
fn subdirectory_without_a_file_only_joins_the_checked_set() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    std::fs::create_dir_all(workspace.join("empty")).unwrap();
    let own = state.call_completed(&workspace, &declared(Some(&["empty/x.txt"])));
    assert!(own.is_empty());
    assert!(state.take_queued().is_empty());
    // But the directory is checked from now on: a file appearing there is
    // `created` at the next turn start.
    write(&workspace.join("empty/AGENTS.md"), "Late rules.\n");
    let out = state.check(&*fake);
    assert_eq!(out.files.len(), 1);
    assert_eq!(out.files[0].reason, InstructionReason::Created);
}

#[test]
fn resumed_state_reads_each_file_before_sending() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    let file = workspace.join("AGENTS.md");
    write(&file, "Leaf.\n");
    let fake = clock();
    let message = opening::collect(&inputs(&home, &fake), &workspace).message;
    let lines = vec![envelope("opening_message", &Event::OpeningMessage(message))];
    // No size or time remembered: an identical file sends nothing, without
    // a recorded stat to shortcut on.
    let mut state = State::resumed(&lines, &workspace, &home).unwrap();
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
    assert!(out.notices.is_empty());
    assert!(out.date.is_none());
}

#[test]
fn resumed_state_folds_changes_deletes_and_the_date() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    let kept = workspace.join("AGENTS.md");
    let gone = workspace.join("sub/AGENTS.md");
    write(&kept, "Kept v2.\n");
    write(&gone, "Gone.\n");
    let fake = clock();
    let message = opening::collect(&inputs(&home, &fake), &workspace).message;
    assert_eq!(message.instruction_files.len(), 1);
    let changed = Event::InstructionFile(contract::events::InstructionFile {
        path: canon(&kept).display().to_string(),
        reason: InstructionReason::Changed,
        content: Some("Kept v2.\n".into()),
        sent: InstructionSent::Diff,
    });
    let deleted = Event::InstructionFile(contract::events::InstructionFile {
        path: canon(&gone).display().to_string(),
        reason: InstructionReason::Deleted,
        content: None,
        sent: InstructionSent::Deleted,
    });
    let dated = Event::DateChanged(contract::events::DateChanged {
        date: "2023-11-20".into(),
    });
    let lines = vec![
        envelope("opening_message", &Event::OpeningMessage(message)),
        envelope("instruction_file", &changed),
        envelope("instruction_file", &deleted),
        envelope("date_changed", &dated),
    ];
    // The deletion really happened: the file is gone from the disk.
    std::fs::remove_file(&gone).unwrap();
    let mut state = State::resumed(&lines, &workspace, &home).unwrap();
    // The fold matches the live map: the kept file sends nothing, the
    // deleted one stays silent while gone, and the date is last given.
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
    assert!(out.date.is_none());
    // The deleted path stays tracked as absent: back again, it is
    // `created` with the full text.
    write(&gone, "Returned.\n");
    let out = state.check(&*fake);
    assert_eq!(out.files.len(), 1);
    assert_eq!(out.files[0].reason, InstructionReason::Created);
    assert_eq!(out.files[0].sent, InstructionSent::Full);
}

#[test]
fn resumed_state_forgets_subdirectories_that_held_no_file() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let fake = clock();
    let message = opening::collect(&inputs(&home, &fake), &workspace).message;
    let lines = vec![envelope("opening_message", &Event::OpeningMessage(message))];
    let state = State::resumed(&lines, &workspace, &home).unwrap();
    // `lonely/` was checked before the resume but never sent a file, so
    // the log cannot restore it: touching it again checks it.
    assert!(!state.dirs.contains(&workspace.join("lonely")));
}
