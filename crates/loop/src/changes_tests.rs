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

use super::{State, apply, clean, unified_diff};
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
    let prompt = inputs(home, clock);
    let collected = opening::collect(&prompt, workspace);
    let mut state = State::initial(&collected.message, workspace, &prompt);
    apply(&mut state.had, &Event::OpeningMessage(collected.message));
    state
}

/// The prompt's extension sections: each extension's name, its files'
/// paths, and its budget.
type Sections = Vec<(String, Vec<PathBuf>, Option<u64>)>;

/// `inputs` with `sections` as the prompt's extension sections: each
/// extension's name, its files' paths, and its budget.
fn sectioned_inputs(home: &Path, clock: &Arc<FakeClock>, sections: Sections) -> PromptInputs {
    let mut prompt = inputs(home, clock);
    prompt.extension_sections = sections;
    prompt
}

/// The state after an opening message over `workspace` with `sections`:
/// built from the message and folded into what the model had, as the
/// loop's render does when it writes the line.
fn initial_sectioned(
    home: &Path,
    workspace: &Path,
    clock: &Arc<FakeClock>,
    sections: Sections,
) -> State {
    let prompt = sectioned_inputs(home, clock, sections);
    let collected = opening::collect(&prompt, workspace);
    let mut state = State::initial(&collected.message, workspace, &prompt);
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
        extension: None,
        content: Some("new".into()),
        sent: InstructionSent::Diff,
    });
    apply(&mut had, &set);
    assert_eq!(had.get("/w/AGENTS.md"), Some(&"new".to_owned()));
    let clear = Event::InstructionFile(contract::events::InstructionFile {
        path: "/w/AGENTS.md".into(),
        reason: InstructionReason::Deleted,
        extension: None,
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
fn changed_file_sends_a_diff_when_it_matches_the_new_length() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    // The headers name the path, so the search runs against the path in
    // force.
    let path = canon(&workspace).join("AGENTS.md").display().to_string();
    // The tail filler lines sit outside the hunk's context, so each one
    // grows the new content without growing the diff; the old change
    // line's padding tunes the parity. One pair lands byte-exact.
    let mut pair = None;
    for extra in 0..8usize {
        for block in 8..500usize {
            let old = format!("change{}\n{}", "o".repeat(extra), "f\n".repeat(block));
            let new = format!("changed\n{}", "f\n".repeat(block));
            if unified_diff(&old, &new, &path).len() == new.len() {
                pair = Some((old, new));
                break;
            }
        }
        if pair.is_some() {
            break;
        }
    }
    let Some((old, new)) = pair else {
        panic!("no equal-length diff found");
    };
    // Precondition: the diff is exactly the new content's byte length,
    // so `>` and `>=` disagree here.
    assert_eq!(unified_diff(&old, &new, &path).len(), new.len());
    let file = workspace.join("AGENTS.md");
    write(&file, &old);
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    write(&file, &new);
    let out = state.check(&*fake);
    assert_eq!(out.files.len(), 1);
    assert_eq!(out.files[0].reason, InstructionReason::Changed);
    // Equal lengths send the diff: the full text goes out only when the
    // diff is longer than the new file.
    assert_eq!(out.files[0].sent, InstructionSent::Diff);
    assert_eq!(out.files[0].content.as_deref(), Some(new.as_str()));
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
    // Absent candidates stay silent: nothing was ever sent, and no
    // failure is named.
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
    assert!(out.notices.is_empty());
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
    assert!(out.notices.is_empty());
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
fn unreadable_untracked_candidate_is_named_once_per_stat_change() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    // A directory where a new file would be: nothing was ever sent, but
    // the failure is still named, once per change of size and time.
    std::fs::create_dir(workspace.join("AGENTS.md")).unwrap();
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
    assert_eq!(out.notices.len(), 1);
    assert!(
        out.notices[0]
            .message
            .contains(&canon(&workspace).join("AGENTS.md").display().to_string())
    );
    // Same size and time: the notice does not repeat.
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
    assert!(out.notices.is_empty());
    std::fs::remove_dir(workspace.join("AGENTS.md")).unwrap();
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
fn untracked_path_outside_the_workspace_changes_nothing() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    let workspace = canon(&workspace);
    let before = state.dirs.clone();
    // Outside the workspace and never tracked: no own edit, and no
    // subdirectory is adopted above the workspace.
    write(&home.join("outside.txt"), "Outside.\n");
    let own = state.call_completed(&workspace, &declared(Some(&["../outside.txt"])));
    assert!(own.is_empty());
    assert!(state.take_queued().is_empty());
    assert_eq!(state.dirs, before);
}

#[test]
fn own_call_on_a_file_under_a_file_records_nothing() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    let workspace = canon(&workspace);
    // Tracked through the subdirectory queue, then a file where its
    // directory was: the read fails with more than absence (`ENOTDIR`),
    // on any platform and user, so the call records no deletion.
    write(&workspace.join("sub/AGENTS.md"), "Sub rules.\n");
    assert!(
        state
            .call_completed(&workspace, &declared(Some(&["sub/notes.txt"])))
            .is_empty()
    );
    assert_eq!(state.take_queued().len(), 1);
    std::fs::remove_dir_all(workspace.join("sub")).unwrap();
    std::fs::write(workspace.join("sub"), "not a directory").unwrap();
    let own = state.call_completed(&workspace, &declared(Some(&["sub/AGENTS.md"])));
    assert!(own.is_empty());
    assert!(state.take_queued().is_empty());
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
    let mut state = State::resumed(&lines, &workspace, &inputs(&home, &fake)).unwrap();
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
        extension: None,
        content: Some("Kept v2.\n".into()),
        sent: InstructionSent::Diff,
    });
    let deleted = Event::InstructionFile(contract::events::InstructionFile {
        path: canon(&gone).display().to_string(),
        reason: InstructionReason::Deleted,
        extension: None,
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
    let mut state = State::resumed(&lines, &workspace, &inputs(&home, &fake)).unwrap();
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

/// `envelope` for the call `action`.
fn action_envelope(kind: &str, event: &Event, action: &str) -> Envelope {
    Envelope {
        action_id: Some(contract::ActionId(action.into())),
        ..envelope(kind, event)
    }
}

/// The `tool_call_started` line of a call `action` declaring `paths`.
fn started(action: &str, paths: Option<&[&str]>) -> Envelope {
    let event = Event::ToolCallStarted(contract::events::ToolCallStarted {
        declared: declared(paths),
        arguments: None,
        changed_by: None,
    });
    action_envelope("tool_call_started", &event, action)
}

/// The `tool_call_completed` line of the call `action`.
fn finished(action: &str) -> Envelope {
    let event = Event::ToolCallCompleted(contract::events::ToolCallCompleted {
        status: contract::events::CallStatus::Completed,
        reason: None,
        error: None,
        process: None,
        content: Vec::new(),
        details: None,
        artifact: None,
        changes: None,
        control: None,
        changed_by: None,
        provider_item: None,
    });
    action_envelope("tool_call_completed", &event, action)
}

/// A resume over `lines`, then `sub/AGENTS.md` created in `workspace`:
/// what the next turn-start check sends.
fn resumed_then_created(
    lines: &[Envelope],
    home: &Path,
    workspace: &Path,
    fake: &Arc<FakeClock>,
) -> Vec<contract::events::InstructionFile> {
    let mut state = State::resumed(lines, workspace, &inputs(home, fake)).unwrap();
    assert!(state.take_queued().is_empty());
    write(&workspace.join("sub/AGENTS.md"), "Late rules.\n");
    state.check(&**fake).files
}

#[test]
fn resumed_state_restores_subdirectories_its_context_touched() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(workspace.join("sub/deep")).unwrap();
    let workspace = canon(&workspace);
    let fake = clock();
    let message = opening::collect(&inputs(&home, &fake), &workspace).message;
    let lines = vec![
        envelope("opening_message", &Event::OpeningMessage(message)),
        started("a_1", Some(&["sub/deep/x.txt"])),
        finished("a_1"),
    ];
    // Nothing is sent at the resume: the touched directories held no file.
    let mut state = State::resumed(&lines, &workspace, &inputs(&home, &fake)).unwrap();
    assert!(state.take_queued().is_empty());
    assert!(state.check(&*fake).files.is_empty());
    // A file created later in an ancestor the call reached is `created`
    // at the next turn start, as in the live session.
    write(&workspace.join("sub/AGENTS.md"), "Late rules.\n");
    let out = state.check(&*fake);
    assert_eq!(out.files.len(), 1);
    assert_eq!(
        out.files[0].path,
        workspace.join("sub/AGENTS.md").display().to_string()
    );
    assert_eq!(out.files[0].reason, InstructionReason::Created);
    assert_eq!(out.files[0].sent, InstructionSent::Full);
    // And in the path's own parent.
    write(&workspace.join("sub/deep/AGENTS.md"), "Deeper.\n");
    let out = state.check(&*fake);
    assert_eq!(out.files.len(), 1);
    assert_eq!(out.files[0].reason, InstructionReason::Created);
}

#[test]
fn resumed_state_restores_a_declared_directory() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(workspace.join("sub")).unwrap();
    let workspace = canon(&workspace);
    let fake = clock();
    let message = opening::collect(&inputs(&home, &fake), &workspace).message;
    let lines = vec![
        envelope("opening_message", &Event::OpeningMessage(message)),
        started("a_1", Some(&["sub"])),
        finished("a_1"),
    ];
    let files = resumed_then_created(&lines, &home, &workspace, &fake);
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].reason, InstructionReason::Created);
}

#[test]
fn resumed_state_drops_a_declared_directory_removed_before_the_resume() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(workspace.join("sub")).unwrap();
    let workspace = canon(&workspace);
    let fake = clock();
    let message = opening::collect(&inputs(&home, &fake), &workspace).message;
    let lines = vec![
        envelope("opening_message", &Event::OpeningMessage(message)),
        started("a_1", Some(&["sub"])),
        finished("a_1"),
    ];
    // The call declared `sub/` while it existed; it is gone at the resume,
    // so the resume drops it: remade after it, the check sends nothing.
    std::fs::remove_dir(workspace.join("sub")).unwrap();
    let mut state = State::resumed(&lines, &workspace, &inputs(&home, &fake)).unwrap();
    assert!(state.take_queued().is_empty());
    assert!(state.check(&*fake).files.is_empty());
    write(&workspace.join("sub/AGENTS.md"), "Late rules.\n");
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
    assert!(out.notices.is_empty());
}

#[test]
fn resumed_state_checks_a_declared_file_without_a_notice() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    write(&workspace.join("sub/x.txt"), "data\n");
    let workspace = canon(&workspace);
    let fake = clock();
    let message = opening::collect(&inputs(&home, &fake), &workspace).message;
    let lines = vec![
        envelope("opening_message", &Event::OpeningMessage(message)),
        started("a_1", Some(&["sub/x.txt"])),
        finished("a_1"),
    ];
    // The restored `sub/x.txt` is a file: checking it finds no candidate
    // under it, sends nothing and names no failure.
    let mut state = State::resumed(&lines, &workspace, &inputs(&home, &fake)).unwrap();
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
    assert!(out.notices.is_empty());
}

#[test]
fn resumed_file_turned_directory_queues_when_a_call_touches_it() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    write(&workspace.join("sub/x.txt"), "data\n");
    let workspace = canon(&workspace);
    let fake = clock();
    let message = opening::collect(&inputs(&home, &fake), &workspace).message;
    let lines = vec![
        envelope("opening_message", &Event::OpeningMessage(message)),
        started("a_1", Some(&["sub/x.txt"])),
        finished("a_1"),
    ];
    let mut state = State::resumed(&lines, &workspace, &inputs(&home, &fake)).unwrap();
    assert!(state.take_queued().is_empty());
    assert!(state.check(&*fake).files.is_empty());
    // The declared file became a directory holding an instruction
    // file: still nothing at the check, until a call touches it.
    std::fs::remove_file(workspace.join("sub/x.txt")).unwrap();
    write(&workspace.join("sub/x.txt/AGENTS.md"), "Late rules.\n");
    assert!(state.check(&*fake).files.is_empty());
    let own = state.call_completed(&workspace, &declared(Some(&["sub/x.txt"])));
    assert!(own.is_empty());
    let queued = state.take_queued();
    assert_eq!(queued.len(), 1);
    let Event::InstructionFile(line) = &queued[0] else {
        panic!("queued a subdirectory line");
    };
    assert_eq!(
        line.path,
        workspace.join("sub/x.txt/AGENTS.md").display().to_string()
    );
    assert_eq!(line.reason, InstructionReason::Subdirectory);
    assert_eq!(line.sent, InstructionSent::Full);
    assert_eq!(line.content.as_deref(), Some("Late rules.\n"));
}

#[test]
fn resumed_state_skips_calls_that_touched_nothing_it_counts() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(workspace.join("sub")).unwrap();
    let workspace = canon(&workspace);
    let fake = clock();
    let message = opening::collect(&inputs(&home, &fake), &workspace).message;
    // A call with no paths, one never completed, one whose completion
    // names another call, a path outside the workspace, and one that does
    // not resolve: none restores `sub/`, and the outside directory is not
    // checked.
    let elsewhere = canon(&home).join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let outside = elsewhere.display().to_string();
    let lines = vec![
        envelope("opening_message", &Event::OpeningMessage(message)),
        started("a_1", None),
        finished("a_1"),
        started("a_2", Some(&["sub/x.txt"])),
        started(
            "a_3",
            Some(&[outside.as_str(), "../../../../../../../../../../../../.."]),
        ),
        finished("a_3"),
        finished("a_4"),
    ];
    let mut state = State::resumed(&lines, &workspace, &inputs(&home, &fake)).unwrap();
    assert!(!state.dirs.contains(&elsewhere));
    // Not even once it holds a candidate: the next check sends nothing.
    write(&elsewhere.join("AGENTS.md"), "Outside rules.\n");
    assert!(state.check(&*fake).files.is_empty());
    std::fs::remove_file(elsewhere.join("AGENTS.md")).unwrap();
    let files = resumed_then_created(&lines, &home, &workspace, &fake);
    assert!(files.is_empty());
}

#[test]
fn resumed_state_forgets_subdirectories_an_earlier_context_touched() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(workspace.join("sub")).unwrap();
    let workspace = canon(&workspace);
    let fake = clock();
    let message = opening::collect(&inputs(&home, &fake), &workspace).message;
    // The call ran in the context before a handoff's new opening message:
    // the new context has not touched `sub/`.
    let lines = vec![
        envelope("opening_message", &Event::OpeningMessage(message.clone())),
        started("a_1", Some(&["sub/x.txt"])),
        finished("a_1"),
        envelope("opening_message", &Event::OpeningMessage(message)),
    ];
    let files = resumed_then_created(&lines, &home, &workspace, &fake);
    assert!(files.is_empty());
}

#[test]
fn queued_file_edited_by_a_later_call_is_dropped() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    let workspace = canon(&workspace);
    write(&workspace.join("sub/AGENTS.md"), "Queued rules.\n");
    // The first call's completion queues the subdirectory file; the
    // second call of the same batch edits it.
    assert!(
        state
            .call_completed(&workspace, &declared(Some(&["sub/notes.txt"])))
            .is_empty()
    );
    write(&workspace.join("sub/AGENTS.md"), "Revised by the call.\n");
    let own = state.call_completed(&workspace, &declared(Some(&["sub/AGENTS.md"])));
    assert_eq!(own.len(), 1);
    assert_eq!(own[0].reason, InstructionReason::OwnEdit);
    for line in &own {
        apply(&mut state.had, &Event::InstructionFile(line.clone()));
    }
    // The queued snapshot is stale: what the model had already matches
    // what is in force, so nothing is emitted, and the sizes moved on.
    assert!(state.take_queued().is_empty());
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
    assert!(out.notices.is_empty());
    // A later outside change is still seen: the cached sizes did not
    // stick at the stale snapshot.
    write(&workspace.join("sub/AGENTS.md"), "Revised outside.\n");
    let out = state.check(&*fake);
    assert_eq!(out.files.len(), 1);
    assert_eq!(out.files[0].reason, InstructionReason::Changed);
}

#[test]
fn queued_file_written_before_the_subdirectory_call_is_queued_fresh() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    let workspace = canon(&workspace);
    // The edit lands before the subdirectory call completes: its
    // completion finds nothing tracked to record, and the later queue
    // reads what is in force now.
    write(&workspace.join("sub/AGENTS.md"), "Written first.\n");
    assert!(
        state
            .call_completed(&workspace, &declared(Some(&["sub/AGENTS.md"])))
            .is_empty()
    );
    assert!(
        state
            .call_completed(&workspace, &declared(Some(&["sub/notes.txt"])))
            .is_empty()
    );
    let queued = state.take_queued();
    assert_eq!(queued.len(), 1);
    let Event::InstructionFile(line) = &queued[0] else {
        panic!("queued a subdirectory line");
    };
    assert_eq!(line.content.as_deref(), Some("Written first.\n"));
}

#[test]
fn queued_file_deleted_by_a_later_call_is_dropped() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    let workspace = canon(&workspace);
    write(&workspace.join("sub/AGENTS.md"), "Queued rules.\n");
    assert!(
        state
            .call_completed(&workspace, &declared(Some(&["sub/notes.txt"])))
            .is_empty()
    );
    // Deleted by the next call of the batch: never sent, so no farewell.
    std::fs::remove_file(workspace.join("sub/AGENTS.md")).unwrap();
    let own = state.call_completed(&workspace, &declared(Some(&["sub/AGENTS.md"])));
    assert!(own.is_empty());
    assert!(state.take_queued().is_empty());
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
    assert!(out.notices.is_empty());
}

#[test]
fn queued_file_deleted_before_the_subdirectory_call_queues_nothing() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    let workspace = canon(&workspace);
    write(&workspace.join("sub/AGENTS.md"), "Gone before the queue.\n");
    std::fs::remove_file(workspace.join("sub/AGENTS.md")).unwrap();
    assert!(
        state
            .call_completed(&workspace, &declared(Some(&["sub/notes.txt"])))
            .is_empty()
    );
    assert!(state.take_queued().is_empty());
}

#[test]
fn queued_file_changed_outside_before_flush_goes_out_fresh() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    let workspace = canon(&workspace);
    write(&workspace.join("sub/AGENTS.md"), "Queued rules.\n");
    assert!(
        state
            .call_completed(&workspace, &declared(Some(&["sub/notes.txt"])))
            .is_empty()
    );
    // No own edit in between: the flush carries what is in force now.
    write(
        &workspace.join("sub/AGENTS.md"),
        "Changed before the flush.\n",
    );
    let queued = state.take_queued();
    assert_eq!(queued.len(), 1);
    let Event::InstructionFile(line) = &queued[0] else {
        panic!("queued a subdirectory line");
    };
    assert_eq!(line.content.as_deref(), Some("Changed before the flush.\n"));
}

#[test]
fn queued_file_unreadable_at_flush_is_named() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    let workspace = canon(&workspace);
    write(&workspace.join("sub/AGENTS.md"), "Queued rules.\n");
    assert!(
        state
            .call_completed(&workspace, &declared(Some(&["sub/notes.txt"])))
            .is_empty()
    );
    // A directory where the queued file was: the stale creation is
    // dropped and the failure named instead.
    std::fs::remove_file(workspace.join("sub/AGENTS.md")).unwrap();
    std::fs::create_dir(workspace.join("sub/AGENTS.md")).unwrap();
    let queued = state.take_queued();
    assert_eq!(queued.len(), 1);
    let Event::Notice(notice) = &queued[0] else {
        panic!("queued a notice, got {queued:?}");
    };
    assert!(
        notice
            .message
            .contains(&workspace.join("sub/AGENTS.md").display().to_string())
    );
    std::fs::remove_dir(workspace.join("sub/AGENTS.md")).unwrap();
}

#[test]
fn failing_stat_on_a_tracked_file_is_named_at_once() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    let workspace = canon(&workspace);
    // Tracked through the subdirectory queue, then its directory is
    // replaced by a file: neither the sizes nor the read succeed.
    write(&workspace.join("sub/AGENTS.md"), "Sub rules.\n");
    assert!(
        state
            .call_completed(&workspace, &declared(Some(&["sub/notes.txt"])))
            .is_empty()
    );
    assert_eq!(state.take_queued().len(), 1);
    std::fs::remove_dir_all(workspace.join("sub")).unwrap();
    std::fs::write(workspace.join("sub"), "not a directory").unwrap();
    // Even with unknown sizes the first failure is named: `None` (no
    // notice yet) differs from `Some(None)` (noticed with unknown sizes).
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
    assert_eq!(out.notices.len(), 1);
    assert!(
        out.notices[0]
            .message
            .contains(&workspace.join("sub/AGENTS.md").display().to_string())
    );
    // Same unknown sizes: the notice does not repeat.
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
    assert!(out.notices.is_empty());
}

#[test]
fn failing_stat_on_an_untracked_candidate_is_named() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    // A file where a checked directory should be: the directory joins
    // the checked set first, then no sizes and no read succeed.
    std::fs::create_dir_all(workspace.join("sub")).unwrap();
    assert!(
        state
            .call_completed(&workspace, &declared(Some(&["sub/x"])))
            .is_empty()
    );
    assert!(state.take_queued().is_empty());
    std::fs::remove_dir(workspace.join("sub")).unwrap();
    std::fs::write(workspace.join("sub"), "not a directory").unwrap();
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
    assert_eq!(out.notices.len(), 1);
    assert!(
        out.notices[0]
            .message
            .contains(&workspace.join("sub/AGENTS.md").display().to_string())
    );
}

/// A candidate that fails to read and then reads again with the same
/// size and time is sent: the failure leaves an unknown baseline, not
/// the failing sizes, so the next check reads again instead of taking
/// the size-and-time shortcut.
#[cfg(unix)]
#[test]
fn unreadable_candidate_readable_again_with_same_stat_is_created() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    let file = workspace.join("AGENTS.md");
    write(&file, "Leaf.\n");
    deny(&file);
    // Precondition: the read really fails with permission denied, so the
    // check below exercises the failure and not the shortcut. Under a
    // user that can still read the file (root) there is no failure to
    // exercise, so the test fails instead of passing vacuously.
    assert_eq!(
        std::fs::read(&file).unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    // Unreadable: named once...
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
    assert_eq!(out.notices.len(), 1);
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
    assert!(out.notices.is_empty());
    // ...readable again with the same size and time (a permission change
    // touches neither): `created`, not skipped as unchanged.
    allow(&file);
    let out = state.check(&*fake);
    assert_eq!(out.files.len(), 1);
    assert_eq!(out.files[0].reason, InstructionReason::Created);
    assert_eq!(out.files[0].sent, InstructionSent::Full);
    assert_eq!(out.files[0].content.as_deref(), Some("Leaf.\n"));
}

/// A tracked file that fails to read and then reads again at its old size
/// and time is compared with what the model had: the failure leaves an
/// unknown baseline, so the check reads instead of taking the shortcut.
#[cfg(unix)]
#[test]
fn unreadable_tracked_file_readable_again_at_its_old_stat_is_compared() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    let file = workspace.join("AGENTS.md");
    write(&file, "Leaf.\n");
    set_mtime(&file, 2_000);
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    // New sizes so the shortcut does not skip the read, then unreadable.
    set_mtime(&file, 3_000);
    deny(&file);
    // Precondition: the read really fails with permission denied; see the
    // candidate case above.
    assert_eq!(
        std::fs::read(&file).unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
    assert_eq!(out.notices.len(), 1);
    // Readable again with the original size and time but revised content
    // (a permission change touches neither size nor time, and the
    // revision restores both): `changed`, not skipped as unchanged.
    allow(&file);
    write(&file, "Leaf?\n");
    set_mtime(&file, 2_000);
    let out = state.check(&*fake);
    assert_eq!(out.files.len(), 1);
    assert_eq!(out.files[0].reason, InstructionReason::Changed);
    // A wholesale change to a tiny file: the headers alone outweigh it.
    assert_eq!(out.files[0].sent, InstructionSent::Full);
    assert_eq!(out.files[0].content.as_deref(), Some("Leaf?\n"));
}

#[cfg(unix)]
#[test]
fn permission_denied_tracked_file_is_named_once_per_stat_change() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    let file = workspace.join("AGENTS.md");
    write(&file, "Leaf.\n");
    set_mtime(&file, 2_000);
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    // New sizes so the shortcut does not skip the read, then unreadable.
    set_mtime(&file, 3_000);
    deny(&file);
    // Precondition: the read really fails with permission denied, so the
    // check below exercises the failure and not the shortcut. Under a
    // user that can still read the file (root) there is no failure to
    // exercise, so the test fails instead of passing vacuously.
    assert_eq!(
        std::fs::read(&file).unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    let out = state.check(&*fake);
    allow(&file);
    assert!(out.files.is_empty());
    assert_eq!(out.notices.len(), 1);
    assert!(out.notices[0].message.contains(&file.display().to_string()));
}

#[cfg(unix)]
#[test]
fn permission_denied_untracked_candidate_is_named() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let file = workspace.join("AGENTS.md");
    write(&file, "Leaf.\n");
    deny(&file);
    // Precondition: the read really fails with permission denied; see the
    // tracked case above.
    assert_eq!(
        std::fs::read(&file).unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    let out = state.check(&*fake);
    allow(&file);
    assert!(out.files.is_empty());
    assert_eq!(out.notices.len(), 1);
    assert!(out.notices[0].message.contains(&file.display().to_string()));
    // Still unreadable with the same sizes: no repeat.
    deny(&file);
    let out = state.check(&*fake);
    allow(&file);
    assert!(out.files.is_empty());
    assert!(out.notices.is_empty());
}

/// Makes `path` unreadable while its sizes stay readable.
#[cfg(unix)]
fn deny(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
}

/// Makes `path` readable again after [`deny`].
#[cfg(unix)]
fn allow(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
}

#[test]
fn home_file_appears_changes_and_is_deleted() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    assert!(state.check(&*fake).files.is_empty());
    // Appears: `<home>/AGENTS.md` is the home candidate.
    write(&home.join("AGENTS.md"), "Global.\n");
    let out = state.check(&*fake);
    assert_eq!(out.files.len(), 1);
    assert_eq!(
        out.files[0].path,
        canon(&home.join("AGENTS.md")).display().to_string()
    );
    assert_eq!(out.files[0].reason, InstructionReason::Created);
    for line in &out.files {
        apply(&mut state.had, &Event::InstructionFile(line.clone()));
    }
    // Changes: a diff against what the model had.
    write(&home.join("AGENTS.md"), "Global, revised.\n");
    let out = state.check(&*fake);
    assert_eq!(out.files.len(), 1);
    assert_eq!(out.files[0].reason, InstructionReason::Changed);
    for line in &out.files {
        apply(&mut state.had, &Event::InstructionFile(line.clone()));
    }
    // Deleted: one line saying its instructions no longer apply.
    std::fs::remove_file(home.join("AGENTS.md")).unwrap();
    let out = state.check(&*fake);
    assert_eq!(out.files.len(), 1);
    assert_eq!(out.files[0].reason, InstructionReason::Deleted);
}

#[test]
fn home_ignores_claude_md() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    // The global file is `<home>/AGENTS.md` only: a home `CLAUDE.md`
    // beside no global file is not adopted.
    write(&home.join("CLAUDE.md"), "Claude rules.\n");
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
    assert!(out.notices.is_empty());
}

#[test]
fn subdirectory_adopts_claude_md() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    let workspace = canon(&workspace);
    // Below home any directory follows the shared precedence: with no
    // `AGENTS.md`, `CLAUDE.md` stands in.
    write(&workspace.join("sub/CLAUDE.md"), "Claude rules.\n");
    assert!(
        state
            .call_completed(&workspace, &declared(Some(&["sub/notes.txt"])))
            .is_empty()
    );
    let queued = state.take_queued();
    assert_eq!(queued.len(), 1);
    let Event::InstructionFile(line) = &queued[0] else {
        panic!("queued a subdirectory line");
    };
    assert_eq!(
        line.path,
        workspace.join("sub/CLAUDE.md").display().to_string()
    );
}

#[test]
fn recreated_identical_file_is_still_created() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    let file = workspace.join("AGENTS.md");
    write(&file, "First.\n");
    set_mtime(&file, 2_000);
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    std::fs::remove_file(&file).unwrap();
    let out = state.check(&*fake);
    assert_eq!(out.files.len(), 1);
    for line in &out.files {
        apply(&mut state.had, &Event::InstructionFile(line.clone()));
    }
    // Back with the same bytes at the same size and time: only the
    // recorded tracking tells it apart from unchanged, so the deletion
    // must have moved the sizes on.
    write(&file, "First.\n");
    set_mtime(&file, 2_000);
    let out = state.check(&*fake);
    assert_eq!(out.files.len(), 1);
    assert_eq!(out.files[0].reason, InstructionReason::Created);
    assert_eq!(out.files[0].sent, InstructionSent::Full);
}

#[test]
fn changed_then_quiet_without_further_change() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    let file = workspace.join("AGENTS.md");
    write(&file, "Leaf.\n");
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    write(&file, "Leaf, revised.\n");
    let out = state.check(&*fake);
    assert_eq!(out.files.len(), 1);
    for line in &out.files {
        apply(&mut state.had, &Event::InstructionFile(line.clone()));
    }
    // Nothing further changed: the recorded sizes match, so no re-read.
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
    assert!(out.notices.is_empty());
}

#[test]
fn own_call_on_unreadable_tracked_file_records_nothing() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    let file = workspace.join("AGENTS.md");
    write(&file, "Leaf.\n");
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    let workspace = canon(&workspace);
    // A directory where the tracked file was: the read fails with more
    // than absence, so the call records no deletion.
    std::fs::remove_file(&file).unwrap();
    std::fs::create_dir(&file).unwrap();
    let own = state.call_completed(&workspace, &declared(Some(&["AGENTS.md"])));
    assert!(own.is_empty());
    assert!(state.take_queued().is_empty());
    std::fs::remove_dir(&file).unwrap();
}

#[test]
fn touch_records_exactly_the_checked_set() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    let workspace = canon(&workspace);
    let home_canon = canon(&home);
    // A declared file touches its ancestors strictly below the
    // workspace; the workspace itself and everything above stay out.
    assert!(
        state
            .call_completed(&workspace, &declared(Some(&["a/b/c.txt"])))
            .is_empty()
    );
    assert_eq!(
        state.dirs,
        [
            home_canon.clone(),
            workspace.clone(),
            workspace.join("a"),
            workspace.join("a/b")
        ]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>()
    );
    // A declared directory touches itself too.
    std::fs::create_dir_all(workspace.join("plain")).unwrap();
    assert!(
        state
            .call_completed(&workspace, &declared(Some(&["plain"])))
            .is_empty()
    );
    assert!(state.dirs.contains(&workspace.join("plain")));
    // The workspace itself, an outside path and an unresolvable one
    // touch nothing new.
    let before = state.dirs.clone();
    assert!(
        state
            .call_completed(&workspace, &declared(Some(&["."])))
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
    assert_eq!(state.dirs, before);
    assert!(state.take_queued().is_empty());
}

#[test]
fn unreadable_subdirectory_file_queues_a_notice() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    let workspace = canon(&workspace);
    // A directory where the subdirectory file should be: the touch names
    // the failure, and the flush passes the notice through as is.
    std::fs::create_dir_all(workspace.join("sub/AGENTS.md")).unwrap();
    assert!(
        state
            .call_completed(&workspace, &declared(Some(&["sub/notes.txt"])))
            .is_empty()
    );
    let queued = state.take_queued();
    assert_eq!(queued.len(), 1);
    let Event::Notice(notice) = &queued[0] else {
        panic!("queued a notice, got {queued:?}");
    };
    assert!(
        notice
            .message
            .contains(&workspace.join("sub/AGENTS.md").display().to_string())
    );
    // Named with these sizes: the next turn-start check stays silent.
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
    assert!(out.notices.is_empty());
    std::fs::remove_dir(workspace.join("sub/AGENTS.md")).unwrap();
}

#[test]
fn unreadable_home_file_is_named() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let fake = clock();
    let mut state = initial(&home, &workspace, &fake);
    // A directory where the global file should be: the home candidate
    // stays the file, so the read names it instead of silence.
    std::fs::create_dir(home.join("AGENTS.md")).unwrap();
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
    assert_eq!(out.notices.len(), 1);
    assert!(
        out.notices[0]
            .message
            .contains(&canon(&home).join("AGENTS.md").display().to_string())
    );
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
    assert!(out.notices.is_empty());
    std::fs::remove_dir(home.join("AGENTS.md")).unwrap();
}

#[test]
fn home_through_a_file_names_its_global_file() {
    let (home, _held) = root();
    // `home` names a file, not a directory: `<home>/AGENTS.md` cannot
    // even be listed (`ENOTDIR`), so the check names it instead of
    // staying silent the way an absent global file does.
    let home_file = home.join("home-file");
    std::fs::write(&home_file, "not a directory").unwrap();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let fake = clock();
    let mut state = initial(&home_file, &workspace, &fake);
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
    assert_eq!(out.notices.len(), 1);
    assert!(
        out.notices[0]
            .message
            .contains(&home_file.join("AGENTS.md").display().to_string())
    );
}

/// One section file under `home`, with `budget` as its budget: the path
/// and the manifest entry naming it.
fn section_file(home: &Path, budget: Option<u64>) -> (PathBuf, Sections) {
    let path = home.join("data/fiber.test-notes/a.md");
    let sections = vec![("fiber.test/notes".into(), vec![path.clone()], budget)];
    (path, sections)
}

#[test]
fn section_file_changed_outside_gives_a_diff_with_extension() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let (path, sections) = section_file(&home, None);
    let old = (0..100).map(|n| format!("line {n}\n")).collect::<String>();
    write(&path, &old);
    let fake = clock();
    let mut state = initial_sectioned(&home, &workspace, &fake, sections);
    let new = old.replace("line 50\n", "line fifty\n");
    write(&path, &new);
    let out = state.check(&*fake);
    assert!(out.notices.is_empty());
    assert_eq!(out.files.len(), 1);
    let change = &out.files[0];
    assert_eq!(change.path, path.display().to_string());
    assert_eq!(change.reason, InstructionReason::Changed);
    assert_eq!(change.extension.as_deref(), Some("fiber.test/notes"));
    assert_eq!(change.sent, InstructionSent::Diff);
    assert_eq!(change.content.as_deref(), Some(new.as_str()));
}

#[test]
fn section_file_changed_wholesale_sends_full_text_with_extension() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let (path, sections) = section_file(&home, None);
    write(&path, "ab\n");
    let fake = clock();
    let mut state = initial_sectioned(&home, &workspace, &fake, sections);
    // A wholesale change to a tiny file: the headers alone outweigh it.
    write(&path, "cd\n");
    let out = state.check(&*fake);
    assert_eq!(out.files.len(), 1);
    assert_eq!(out.files[0].reason, InstructionReason::Changed);
    assert_eq!(out.files[0].extension.as_deref(), Some("fiber.test/notes"));
    assert_eq!(out.files[0].sent, InstructionSent::Full);
    assert_eq!(out.files[0].content.as_deref(), Some("cd\n"));
}

#[test]
fn section_file_deleted_gives_one_line_with_extension() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let (path, sections) = section_file(&home, None);
    write(&path, "Notes.\n");
    let fake = clock();
    let mut state = initial_sectioned(&home, &workspace, &fake, sections);
    std::fs::remove_file(&path).unwrap();
    let out = state.check(&*fake);
    assert_eq!(out.files.len(), 1);
    assert_eq!(out.files[0].reason, InstructionReason::Deleted);
    assert_eq!(out.files[0].extension.as_deref(), Some("fiber.test/notes"));
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
fn section_file_absent_at_build_is_created_with_full_text() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    // The manifest names a path that does not exist: the build sends
    // nothing for it.
    let (path, sections) = section_file(&home, None);
    let fake = clock();
    let mut state = initial_sectioned(&home, &workspace, &fake, sections);
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
    assert!(out.notices.is_empty());
    write(&path, "New notes.\n");
    let out = state.check(&*fake);
    assert_eq!(out.files.len(), 1);
    assert_eq!(out.files[0].reason, InstructionReason::Created);
    assert_eq!(out.files[0].extension.as_deref(), Some("fiber.test/notes"));
    assert_eq!(out.files[0].sent, InstructionSent::Full);
    assert_eq!(out.files[0].content.as_deref(), Some("New notes.\n"));
}

#[test]
fn section_file_untouched_sends_nothing() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let (path, sections) = section_file(&home, None);
    write(&path, "Notes.\n");
    let fake = clock();
    // Present and untouched: silence. The manifest's other path stays
    // absent: silence too.
    let mut sections = sections;
    sections[0]
        .1
        .push(home.join("data/fiber.test-notes/missing.md"));
    let mut state = initial_sectioned(&home, &workspace, &fake, sections);
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
    assert!(out.notices.is_empty());
}

#[test]
fn unreadable_section_file_names_the_section_once() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let (path, sections) = section_file(&home, None);
    write(&path, "Notes.\n");
    let fake = clock();
    let mut state = initial_sectioned(&home, &workspace, &fake, sections);
    // A directory where the file was: size and time differ, and no read
    // succeeds, on any platform and user.
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
    assert_eq!(out.notices.len(), 1);
    assert_eq!(
        out.notices[0].extension.as_deref(),
        Some("fiber.test/notes")
    );
    assert!(out.notices[0].message.contains(&path.display().to_string()));
    // Same size and time: the notice does not repeat.
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
    assert!(out.notices.is_empty());
    std::fs::remove_dir_all(&path).unwrap();
}

#[test]
fn apply_opening_inserts_section_files_into_had() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let (path, sections) = section_file(&home, None);
    write(&path, "Notes.\n");
    let fake = clock();
    let message = opening::collect(&sectioned_inputs(&home, &fake, sections), &workspace).message;
    assert_eq!(message.extension_sections.len(), 1);
    let mut had = BTreeMap::new();
    apply(&mut had, &Event::OpeningMessage(message));
    assert_eq!(
        had.get(&path.display().to_string()),
        Some(&"Notes.\n".to_owned())
    );
}

#[test]
fn path_in_both_roles_is_tracked_once_with_the_section_extension() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    let file = workspace.join("AGENTS.md");
    write(&file, "Leaf.\n");
    let fake = clock();
    // The manifest names the workspace instruction file too.
    let sections = vec![("fiber.test/notes".into(), vec![canon(&file)], None)];
    let mut state = initial_sectioned(&home, &workspace, &fake, sections);
    // Tracked once, under one key.
    assert_eq!(state.files.len(), 1);
    write(&file, "Leaf, revised.\n");
    let out = state.check(&*fake);
    assert_eq!(out.files.len(), 1);
    assert_eq!(out.files[0].reason, InstructionReason::Changed);
    assert_eq!(out.files[0].extension.as_deref(), Some("fiber.test/notes"));
}

#[test]
fn resumed_restores_section_paths_and_had() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let (path, sections) = section_file(&home, None);
    let old = (0..100).map(|n| format!("line {n}\n")).collect::<String>();
    write(&path, &old);
    let fake = clock();
    let prompt = sectioned_inputs(&home, &fake, sections);
    let message = opening::collect(&prompt, &workspace).message;
    let lines = vec![envelope("opening_message", &Event::OpeningMessage(message))];
    let mut state = State::resumed(&lines, &workspace, &prompt).unwrap();
    // What the model had comes from the section files.
    assert_eq!(state.had.get(&path.display().to_string()), Some(&old));
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
    assert!(out.notices.is_empty());
    // An outside edit after the resume gives a diff with the extension.
    let new = old.replace("line 50\n", "line fifty\n");
    write(&path, &new);
    let out = state.check(&*fake);
    assert_eq!(out.files.len(), 1);
    assert_eq!(out.files[0].reason, InstructionReason::Changed);
    assert_eq!(out.files[0].extension.as_deref(), Some("fiber.test/notes"));
    assert_eq!(out.files[0].sent, InstructionSent::Diff);
}

#[test]
fn resumed_ignores_a_section_path_the_manifest_no_longer_names() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let (path, sections) = section_file(&home, None);
    write(&path, "Notes.\n");
    let fake = clock();
    // The log's opening message sent the section file, but today's
    // manifest names nothing.
    let message = opening::collect(&sectioned_inputs(&home, &fake, sections), &workspace).message;
    assert_eq!(message.extension_sections.len(), 1);
    let lines = vec![envelope("opening_message", &Event::OpeningMessage(message))];
    let mut state = State::resumed(&lines, &workspace, &inputs(&home, &fake)).unwrap();
    // Deleted after the resume: not tracked, so not even a deleted line.
    std::fs::remove_file(&path).unwrap();
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
    assert!(out.notices.is_empty());
}

#[test]
fn resumed_ignores_a_historical_section_line_the_manifest_no_longer_names() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let (path, sections) = section_file(&home, None);
    write(&path, "Notes.\n");
    let fake = clock();
    // The log holds a section `own_edit` for the path, but today's
    // manifest names nothing.
    let message = opening::collect(&sectioned_inputs(&home, &fake, sections), &workspace).message;
    assert_eq!(message.extension_sections.len(), 1);
    let key = path.display().to_string();
    let own = Event::InstructionFile(contract::events::InstructionFile {
        path: key.clone(),
        reason: InstructionReason::OwnEdit,
        extension: Some("fiber.test/notes".into()),
        content: Some("Revised by the call.\n".into()),
        sent: InstructionSent::None,
    });
    let lines = vec![
        envelope("opening_message", &Event::OpeningMessage(message)),
        envelope("instruction_file", &own),
    ];
    let state = State::resumed(&lines, &workspace, &inputs(&home, &fake)).unwrap();
    // Neither the path nor its directory is tracked.
    assert!(!state.files.contains_key(&key));
    assert!(!state.dirs.contains(path.parent().unwrap()));
    let mut state = state;
    // Edited outside after the resume: not tracked, so nothing sent.
    write(&path, "Edited outside.\n");
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
    assert!(out.notices.is_empty());
    assert!(!state.files.contains_key(&key));
    // Deleted after the resume: not even a deleted line.
    std::fs::remove_file(&path).unwrap();
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
    assert!(out.notices.is_empty());
    assert!(!state.files.contains_key(&key));
    // Its directory never became checked: an `AGENTS.md` beside it is
    // not adopted.
    write(&path.parent().unwrap().join("AGENTS.md"), "Stowaway.\n");
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
    assert!(out.notices.is_empty());
}

#[test]
fn resumed_tracks_a_manifest_path_new_since_the_log() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let (path, sections) = section_file(&home, None);
    write(&path, "Notes.\n");
    let fake = clock();
    // The log's opening message never named the file; today's manifest
    // does. It exists, so the first check sends it as created.
    let message = opening::collect(&inputs(&home, &fake), &workspace).message;
    assert!(message.extension_sections.is_empty());
    let lines = vec![envelope("opening_message", &Event::OpeningMessage(message))];
    let prompt = sectioned_inputs(&home, &fake, sections);
    let mut state = State::resumed(&lines, &workspace, &prompt).unwrap();
    let out = state.check(&*fake);
    assert_eq!(out.files.len(), 1);
    assert_eq!(out.files[0].reason, InstructionReason::Created);
    assert_eq!(out.files[0].extension.as_deref(), Some("fiber.test/notes"));
    assert_eq!(out.files[0].sent, InstructionSent::Full);
    assert_eq!(out.files[0].content.as_deref(), Some("Notes.\n"));
}

#[test]
fn own_edit_of_a_section_file_records_extension_and_sends_nothing() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let (path, sections) = section_file(&home, None);
    write(&path, "Notes.\n");
    let fake = clock();
    let mut state = initial_sectioned(&home, &workspace, &fake, sections);
    let workspace = canon(&workspace);
    // Declared exactly as the manifest names it.
    let key = path.display().to_string();
    write(&path, "Revised by the call.\n");
    let own = state.call_completed(&workspace, &declared(Some(&[key.as_str()])));
    assert_eq!(own.len(), 1);
    assert_eq!(own[0].reason, InstructionReason::OwnEdit);
    assert_eq!(own[0].extension.as_deref(), Some("fiber.test/notes"));
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
fn own_deletion_of_a_section_file_is_an_empty_own_edit_with_extension() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let (path, sections) = section_file(&home, None);
    write(&path, "Notes.\n");
    let fake = clock();
    let mut state = initial_sectioned(&home, &workspace, &fake, sections);
    let workspace = canon(&workspace);
    std::fs::remove_file(&path).unwrap();
    let key = path.display().to_string();
    let own = state.call_completed(&workspace, &declared(Some(&[key.as_str()])));
    assert_eq!(own.len(), 1);
    assert_eq!(own[0].reason, InstructionReason::OwnEdit);
    assert_eq!(own[0].extension.as_deref(), Some("fiber.test/notes"));
    assert_eq!(own[0].sent, InstructionSent::None);
    assert_eq!(own[0].content, None);
    for line in &own {
        apply(&mut state.had, &Event::InstructionFile(line.clone()));
    }
    // Tracked as absent: the turn-start check stays silent too.
    let out = state.check(&*fake);
    assert!(out.files.is_empty());
}

/// A budgeted section file with `content`: the path and the manifest
/// entry budgeting it at `budget`.
fn budgeted(home: &Path, name: &str, content: &str, budget: Option<u64>) -> (PathBuf, Sections) {
    let path = home.join(format!("data/{name}/a.md"));
    write(&path, content);
    (path.clone(), vec![(name.into(), vec![path], budget)])
}

/// The prune lines a call to `tool` declaring `effects` over `paths` gets.
fn pruned_with(
    state: &State,
    workspace: &Path,
    tool: &str,
    effects: Vec<Effect>,
    paths: &[&str],
) -> Vec<String> {
    let mut call = declared(Some(paths));
    call.effects = effects;
    state.prune_lines(workspace, tool, &call)
}

/// The prune lines a `tool` call declaring `paths` gets.
fn pruned(state: &State, workspace: &Path, tool: &str, paths: Option<&[&str]>) -> Vec<String> {
    state.prune_lines(workspace, tool, &declared(paths))
}

#[test]
fn prune_line_write_over_budget() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    // Two files of 3 bytes: the line needs their sum of 6.
    let (first, mut sections) = budgeted(&home, "fiber.test/notes", "123", Some(5));
    let second = home.join("data/fiber.test/notes/b.md");
    write(&second, "456");
    sections[0].1.push(second);
    let fake = clock();
    let state = initial_sectioned(&home, &workspace, &fake, sections);
    let key = first.display().to_string();
    assert_eq!(
        pruned(&state, &workspace, "write", Some(&[key.as_str()])),
        vec![
            "Fiber: these files are 6 bytes, over their budget of 5 bytes. Prune them.".to_owned()
        ]
    );
}

#[test]
fn prune_line_edit_over_budget() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let (path, sections) = budgeted(&home, "fiber.test/notes", "123456", Some(5));
    let fake = clock();
    let state = initial_sectioned(&home, &workspace, &fake, sections);
    let key = path.display().to_string();
    assert_eq!(
        pruned(&state, &workspace, "edit", Some(&[key.as_str()])),
        vec![
            "Fiber: these files are 6 bytes, over their budget of 5 bytes. Prune them.".to_owned()
        ]
    );
}

#[test]
fn prune_line_at_or_under_budget_is_none() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    // Exactly at budget is not over: only strictly greater counts.
    let (path, sections) = budgeted(&home, "fiber.test/notes", "123456", Some(6));
    let fake = clock();
    let state = initial_sectioned(&home, &workspace, &fake, sections);
    let key = path.display().to_string();
    assert!(pruned(&state, &workspace, "write", Some(&[key.as_str()])).is_empty());
    // Under budget: silence too.
    let (path, sections) = budgeted(&home, "fiber.test/notes", "12", Some(5));
    let state = initial_sectioned(&home, &workspace, &fake, sections);
    let key = path.display().to_string();
    assert!(pruned(&state, &workspace, "write", Some(&[key.as_str()])).is_empty());
}

#[test]
fn prune_line_without_a_budget_is_none() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let (path, sections) = budgeted(&home, "fiber.test/notes", "123456", None);
    let fake = clock();
    let state = initial_sectioned(&home, &workspace, &fake, sections);
    let key = path.display().to_string();
    assert!(pruned(&state, &workspace, "write", Some(&[key.as_str()])).is_empty());
}

#[test]
fn prune_line_shell_call_over_budget_is_none() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let (path, sections) = budgeted(&home, "fiber.test/notes", "123456", Some(5));
    let fake = clock();
    let state = initial_sectioned(&home, &workspace, &fake, sections);
    let key = path.display().to_string();
    // A shell edit is never seen when it happens.
    assert!(pruned(&state, &workspace, "bash", Some(&[key.as_str()])).is_empty());
}

#[test]
fn prune_line_call_touching_no_section_file_is_none() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let (_path, sections) = budgeted(&home, "fiber.test/notes", "123456", Some(5));
    let fake = clock();
    let state = initial_sectioned(&home, &workspace, &fake, sections);
    assert!(pruned(&state, &workspace, "write", Some(&["other.txt"])).is_empty());
    assert!(pruned(&state, &workspace, "write", None).is_empty());
}

#[test]
fn prune_lines_two_over_budget_sections_come_in_name_order() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let (beta, beta_section) = budgeted(&home, "fiber.test/beta", "12345678", Some(7));
    let (alpha, alpha_section) = budgeted(&home, "fiber.test/alpha", "123456", Some(5));
    // Send order is beta first: the lines still come in name order.
    let mut sections = beta_section;
    sections.extend(alpha_section);
    let fake = clock();
    let state = initial_sectioned(&home, &workspace, &fake, sections);
    let lines = pruned(
        &state,
        &workspace,
        "write",
        Some(&[
            beta.display().to_string().as_str(),
            alpha.display().to_string().as_str(),
        ]),
    );
    assert_eq!(
        lines,
        vec![
            "Fiber: these files are 6 bytes, over their budget of 5 bytes. Prune them.".to_owned(),
            "Fiber: these files are 8 bytes, over their budget of 7 bytes. Prune them.".to_owned(),
        ]
    );
}

#[test]
fn prune_line_follows_a_declared_write_not_the_tool_name() {
    let (home, _held) = root();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let (path, sections) = budgeted(&home, "fiber.test/notes", "123456", Some(5));
    let fake = clock();
    let state = initial_sectioned(&home, &workspace, &fake, sections);
    let key = path.display().to_string();
    let line = "Fiber: these files are 6 bytes, over their budget of 5 bytes. Prune them.";
    // A tool of any name that declares `writes` gets the line.
    assert_eq!(
        pruned_with(
            &state,
            &workspace,
            "notes",
            vec![Effect::Writes],
            &[key.as_str()]
        ),
        vec![line.to_owned()]
    );
    // Writes among other effects still counts.
    assert_eq!(
        pruned_with(
            &state,
            &workspace,
            "notes",
            vec![Effect::Reads, Effect::Writes],
            &[key.as_str()]
        ),
        vec![line.to_owned()]
    );
    // A tool named `write` that declares only a read gets none.
    assert!(
        pruned_with(
            &state,
            &workspace,
            "write",
            vec![Effect::Reads],
            &[key.as_str()]
        )
        .is_empty()
    );
    assert!(pruned_with(&state, &workspace, "write", Vec::new(), &[key.as_str()]).is_empty());
}
