//! Reading and saving the person's reviewer notes (`docs/permissions.md`,
//! "What the person tells it"): each layer's own `reviewer.context` with
//! its version, and a locked read-compare-write that refuses a stale save
//! and writes each file as `docs/state.md`, "Concurrent access" says.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use super::{
    FileOutcome, NotesSaved, NotesWrite, ReadOnlyEntryProblem, ReadOnlyFlags, same_directory,
    save_reviewer_notes, validate_read_only_entry,
};
use crate::home::ProjectKey;
use crate::reviewer_notes;
use crate::write::atomic::{Stage, fail_nth, reset_write_count, write_count};
use fakes::Deadline;

/// Fiber home in a temporary directory, removed on drop.
struct Setup {
    _tmp: fakes::TempDir,
    home: PathBuf,
}

impl Setup {
    fn new() -> Self {
        let tmp = fakes::TempDir::new("fiber-notes");
        let home = tmp.path().join("home");
        fs::create_dir_all(&home).unwrap();
        Self { _tmp: tmp, home }
    }

    fn project() -> ProjectKey {
        ProjectKey::new("proj").unwrap()
    }

    fn global(&self) -> PathBuf {
        self.home.join("config.json")
    }

    fn project_file(&self) -> PathBuf {
        self.home.join("projects").join("proj").join("config.json")
    }

    fn write(&self, file: &Path, text: &str) {
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, text).unwrap();
    }

    fn bytes(&self, file: &Path) -> Option<Vec<u8>> {
        match fs::read(file) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => panic!("read {}: {error}", file.display()),
        }
    }

    fn read(&self) -> crate::ReviewerNotes {
        reviewer_notes(&self.home, &Self::project()).unwrap()
    }

    fn entry(command: &str, flags: &[&str]) -> ReadOnlyFlags {
        ReadOnlyFlags {
            command: command.to_owned(),
            flags: flags.iter().map(|flag| flag.to_string()).collect(),
        }
    }

    fn save(
        &self,
        global: Option<(&str, &str)>,
        project_layer: Option<(&str, &str)>,
    ) -> Result<NotesSaved, crate::ConfigError> {
        self.save_entries(global, project_layer, Vec::new(), Vec::new())
    }

    fn save_entries(
        &self,
        global: Option<(&str, &str)>,
        project_layer: Option<(&str, &str)>,
        global_entries: Vec<ReadOnlyFlags>,
        project_entries: Vec<ReadOnlyFlags>,
    ) -> Result<NotesSaved, crate::ConfigError> {
        let write = |sent: Option<(&str, &str)>, read_only: Vec<ReadOnlyFlags>| {
            sent.map(|(version, text)| NotesWrite {
                version: version.to_owned(),
                text: text.to_owned(),
                read_only,
            })
        };
        save_reviewer_notes(
            &self.home,
            &Self::project(),
            write(global, global_entries),
            write(project_layer, project_entries),
        )
    }

    /// No temp file beside `file`: a failed write removes its own.
    fn no_temp_beside(&self, file: &Path) {
        let parent = file.parent().unwrap();
        if !parent.exists() {
            return;
        }
        for entry in fs::read_dir(parent).unwrap() {
            let entry = entry.unwrap();
            assert!(
                !entry
                    .file_name()
                    .to_str()
                    .unwrap_or_default()
                    .ends_with(".tmp"),
                "a temp file beside {}",
                file.display()
            );
        }
    }
}

/// The `Written` files' outcomes, global first.
fn written(saved: NotesSaved) -> Vec<(PathBuf, bool, bool)> {
    let NotesSaved::Written(files) = saved else {
        panic!("a current save writes, got Stale");
    };
    files
        .into_iter()
        .map(|saved| {
            let (kept, failed) = match saved.outcome {
                FileOutcome::Saved => (true, false),
                FileOutcome::SavedUnsynced(_) => (true, true),
                FileOutcome::NotSaved(_) => (false, true),
            };
            (saved.file, kept, failed)
        })
        .collect()
}

#[test]
fn current_versions_write_both_contexts_and_keep_every_other_key() {
    fakes::within(
        "current notes save",
        Deadline::after(Duration::from_secs(10)),
        || {
            let setup = Setup::new();
            setup.write(
            &setup.global(),
            r#"{"model": "a/b", "other": {"x": 1}, "reviewer": {"context": "old global", "tone": "dry"}}"#,
        );
            setup.write(
                &setup.project_file(),
                r#"{"reviewer": {"context": "old project"}}"#,
            );
            let read = setup.read();
            let saved = setup
                .save(
                    Some((&read.global.version, "new global")),
                    Some((&read.project.version, "new project")),
                )
                .unwrap();
            assert_eq!(
                written(saved),
                [
                    (setup.global(), true, false),
                    (setup.project_file(), true, false)
                ]
            );
            let global: Value =
                serde_json::from_slice(&setup.bytes(&setup.global()).unwrap()).unwrap();
            assert_eq!(
                global,
                json!({"model": "a/b", "other": {"x": 1}, "reviewer": {"context": "new global", "tone": "dry"}})
            );
            let project: Value =
                serde_json::from_slice(&setup.bytes(&setup.project_file()).unwrap()).unwrap();
            assert_eq!(project, json!({"reviewer": {"context": "new project"}}));
        },
    );
}

#[test]
fn a_layer_not_sent_is_not_read_or_written_and_has_no_entry() {
    fakes::within(
        "unsent layer save",
        Deadline::after(Duration::from_secs(10)),
        || {
            let setup = Setup::new();
            setup.write(&setup.global(), r#"{"reviewer": {"context": "old"}}"#);
            let read = setup.read();
            assert_eq!(read.project.version, "absent");
            let saved = setup
                .save(Some((&read.global.version, "new")), None)
                .unwrap();
            assert_eq!(written(saved), [(setup.global(), true, false)]);
            assert!(!setup.project_file().exists());
            let global: Value =
                serde_json::from_slice(&setup.bytes(&setup.global()).unwrap()).unwrap();
            assert_eq!(global, json!({"reviewer": {"context": "new"}}));
        },
    );
}

#[test]
fn no_sent_layer_writes_nothing() {
    fakes::within(
        "empty notes save",
        Deadline::after(Duration::from_secs(10)),
        || {
            let setup = Setup::new();
            let saved = setup.save(None, None).unwrap();
            assert_eq!(written(saved), []);
            assert!(!setup.global().exists());
            assert!(!setup.project_file().exists());
        },
    );
}

#[test]
fn a_stale_global_leaves_both_files_byte_for_byte() {
    fakes::within(
        "stale global save",
        Deadline::after(Duration::from_secs(10)),
        || {
            for (name, mutate) in [
                (
                    "context changed",
                    r#"{"reviewer": {"context": "someone else"}}"#.to_owned(),
                ),
                (
                    "another key changed",
                    r#"{"model": "c/d", "reviewer": {"context": "old global"}}"#.to_owned(),
                ),
            ] {
                let setup = Setup::new();
                setup.write(
                    &setup.global(),
                    r#"{"reviewer": {"context": "old global"}}"#,
                );
                setup.write(
                    &setup.project_file(),
                    r#"{"reviewer": {"context": "old project"}}"#,
                );
                let read = setup.read();
                setup.write(&setup.global(), &mutate);
                let before = (
                    setup.bytes(&setup.global()),
                    setup.bytes(&setup.project_file()),
                );
                let saved = setup
                    .save(
                        Some((&read.global.version, "new global")),
                        Some((&read.project.version, "new project")),
                    )
                    .unwrap();
                assert!(matches!(saved, NotesSaved::Stale), "{name}");
                assert_eq!(setup.bytes(&setup.global()), before.0, "{name}");
                assert_eq!(setup.bytes(&setup.project_file()), before.1, "{name}");
            }
        },
    );
}

#[test]
fn a_stale_project_leaves_both_files_byte_for_byte() {
    fakes::within(
        "stale project save",
        Deadline::after(Duration::from_secs(10)),
        || {
            for (name, mutate) in [
                (
                    "context changed",
                    Some(r#"{"reviewer": {"context": "someone else"}}"#.to_owned()),
                ),
                ("file deleted", None),
            ] {
                let setup = Setup::new();
                setup.write(
                    &setup.global(),
                    r#"{"reviewer": {"context": "old global"}}"#,
                );
                setup.write(
                    &setup.project_file(),
                    r#"{"reviewer": {"context": "old project"}}"#,
                );
                let read = setup.read();
                let before = setup.bytes(&setup.global());
                match &mutate {
                    Some(text) => setup.write(&setup.project_file(), text),
                    None => fs::remove_file(setup.project_file()).unwrap(),
                }
                let saved = setup
                    .save(
                        Some((&read.global.version, "new global")),
                        Some((&read.project.version, "new project")),
                    )
                    .unwrap();
                assert!(matches!(saved, NotesSaved::Stale), "{name}");
                assert_eq!(setup.bytes(&setup.global()), before, "{name}");
                match &mutate {
                    Some(_) => assert!(
                        setup
                            .bytes(&setup.project_file())
                            .unwrap()
                            .starts_with(b"{"),
                        "{name}"
                    ),
                    None => assert!(!setup.project_file().exists(), "{name}"),
                }
            }
        },
    );
}

#[test]
fn a_created_file_is_a_stale_save() {
    fakes::within(
        "created file save",
        Deadline::after(Duration::from_secs(10)),
        || {
            let setup = Setup::new();
            setup.write(
                &setup.global(),
                r#"{"reviewer": {"context": "old global"}}"#,
            );
            let read = setup.read();
            assert_eq!(read.project.version, "absent");
            let before = setup.bytes(&setup.global());
            setup.write(
                &setup.project_file(),
                r#"{"reviewer": {"context": "created elsewhere"}}"#,
            );
            let created = setup.bytes(&setup.project_file());
            let saved = setup
                .save(
                    Some((&read.global.version, "new global")),
                    Some((&read.project.version, "new project")),
                )
                .unwrap();
            assert!(matches!(saved, NotesSaved::Stale));
            assert_eq!(setup.bytes(&setup.global()), before);
            assert_eq!(setup.bytes(&setup.project_file()), created);
        },
    );
}

#[test]
fn a_sync_failure_after_the_rename_is_saved_with_an_error() {
    fakes::within(
        "sync failure save",
        Deadline::after(Duration::from_secs(10)),
        || {
            let setup = Setup::new();
            setup.write(
                &setup.global(),
                r#"{"reviewer": {"context": "old global"}}"#,
            );
            setup.write(
                &setup.project_file(),
                r#"{"reviewer": {"context": "old project"}}"#,
            );
            let read = setup.read();
            fail_nth(1, Stage::AfterRename);
            let saved = setup
                .save(
                    Some((&read.global.version, "new global")),
                    Some((&read.project.version, "new project")),
                )
                .unwrap();
            // The global rename landed, so it is saved with an error; the save
            // goes on to the project file.
            assert_eq!(
                written(saved),
                [
                    (setup.global(), true, true),
                    (setup.project_file(), true, false)
                ]
            );
            let global: Value =
                serde_json::from_slice(&setup.bytes(&setup.global()).unwrap()).unwrap();
            assert_eq!(global, json!({"reviewer": {"context": "new global"}}));
            let project: Value =
                serde_json::from_slice(&setup.bytes(&setup.project_file()).unwrap()).unwrap();
            assert_eq!(project, json!({"reviewer": {"context": "new project"}}));
            setup.no_temp_beside(&setup.global());
        },
    );
}

#[test]
fn a_failed_second_rename_leaves_the_project_untouched() {
    fakes::within(
        "failed second rename save",
        Deadline::after(Duration::from_secs(10)),
        || {
            let setup = Setup::new();
            setup.write(
                &setup.global(),
                r#"{"reviewer": {"context": "old global"}}"#,
            );
            setup.write(
                &setup.project_file(),
                r#"{"reviewer": {"context": "old project"}}"#,
            );
            let read = setup.read();
            let before = setup.bytes(&setup.project_file());
            fail_nth(2, Stage::BeforeRename);
            let saved = setup
                .save(
                    Some((&read.global.version, "new global")),
                    Some((&read.project.version, "new project")),
                )
                .unwrap();
            assert_eq!(
                written(saved),
                [
                    (setup.global(), true, false),
                    (setup.project_file(), false, true)
                ]
            );
            let global: Value =
                serde_json::from_slice(&setup.bytes(&setup.global()).unwrap()).unwrap();
            assert_eq!(global, json!({"reviewer": {"context": "new global"}}));
            assert_eq!(setup.bytes(&setup.project_file()), before);
            setup.no_temp_beside(&setup.project_file());
        },
    );
}

#[test]
fn a_failed_first_rename_still_writes_the_project() {
    fakes::within(
        "failed first rename save",
        Deadline::after(Duration::from_secs(10)),
        || {
            let setup = Setup::new();
            setup.write(
                &setup.global(),
                r#"{"reviewer": {"context": "old global"}}"#,
            );
            setup.write(
                &setup.project_file(),
                r#"{"reviewer": {"context": "old project"}}"#,
            );
            let read = setup.read();
            let before = setup.bytes(&setup.global());
            fail_nth(1, Stage::BeforeRename);
            let saved = setup
                .save(
                    Some((&read.global.version, "new global")),
                    Some((&read.project.version, "new project")),
                )
                .unwrap();
            assert_eq!(
                written(saved),
                [
                    (setup.global(), false, true),
                    (setup.project_file(), true, false)
                ]
            );
            assert_eq!(setup.bytes(&setup.global()), before);
            let project: Value =
                serde_json::from_slice(&setup.bytes(&setup.project_file()).unwrap()).unwrap();
            assert_eq!(project, json!({"reviewer": {"context": "new project"}}));
            setup.no_temp_beside(&setup.global());
        },
    );
}

#[test]
fn a_root_or_reviewer_of_the_wrong_shape_refuses_both_files() {
    fakes::within(
        "malformed root save",
        Deadline::after(Duration::from_secs(10)),
        || {
            for (name, root) in [
                ("array root", "[1,2]"),
                (
                    "string reviewer",
                    r#"{"reviewer": "keep me", "model": "a/b"}"#,
                ),
            ] {
                for layer in ["global", "project"] {
                    let setup = Setup::new();
                    setup.write(
                        &setup.global(),
                        r#"{"reviewer": {"context": "old global"}}"#,
                    );
                    setup.write(
                        &setup.project_file(),
                        r#"{"reviewer": {"context": "old project"}}"#,
                    );
                    let read = setup.read();
                    match layer {
                        "global" => setup.write(&setup.global(), root),
                        _ => setup.write(&setup.project_file(), root),
                    }
                    let before = (
                        setup.bytes(&setup.global()),
                        setup.bytes(&setup.project_file()),
                    );
                    let error = setup
                        .save(
                            Some((&read.global.version, "new global")),
                            Some((&read.project.version, "new project")),
                        )
                        .unwrap_err();
                    assert!(
                        error.to_string().contains(match layer {
                            "global" => "config.json",
                            _ => "proj",
                        }),
                        "{name} in {layer} names its file: {error}"
                    );
                    assert_eq!(setup.bytes(&setup.global()), before.0, "{name} in {layer}");
                    assert_eq!(
                        setup.bytes(&setup.project_file()),
                        before.1,
                        "{name} in {layer}"
                    );
                }
            }
        },
    );
}

#[test]
fn an_aliased_project_writes_nothing() {
    fakes::within(
        "the aliased notes save",
        Deadline::after(Duration::from_secs(10)),
        || {
            let setup = Setup::new();
            setup.write(
                &setup.global(),
                r#"{"reviewer": {"context": "old global"}}"#,
            );
            fs::create_dir_all(setup.home.join("projects")).unwrap();
            symlink(&setup.home, setup.home.join("projects").join("proj")).unwrap();
            let read = setup.read();
            let before = setup.bytes(&setup.global());
            let error = setup
                .save(
                    Some((&read.global.version, "new global")),
                    Some((&read.project.version, "new project")),
                )
                .unwrap_err();
            assert!(!error.to_string().is_empty());
            assert_eq!(setup.bytes(&setup.global()), before);
        },
    );
}

#[test]
fn same_directory_by_path_and_by_symlink_is_true() {
    fakes::within(
        "same directory",
        Deadline::after(Duration::from_secs(10)),
        || {
            let setup = Setup::new();
            let real = setup.home.join("real");
            fs::create_dir_all(&real).unwrap();
            let link = setup.home.join("link");
            symlink(&real, &link).unwrap();
            // Two spellings of one directory: a plain path, and a symlink to it.
            assert!(matches!(
                same_directory(&real.join("config.json"), &real.join("notes.toml")),
                Ok(true)
            ));
            assert!(matches!(
                same_directory(&real.join("config.json"), &link.join("notes.toml")),
                Ok(true)
            ));
        },
    );
}

#[test]
fn same_directory_for_two_sibling_directories_is_false() {
    fakes::within(
        "sibling directories",
        Deadline::after(Duration::from_secs(10)),
        || {
            let setup = Setup::new();
            let first = setup.home.join("first");
            let second = setup.home.join("second");
            fs::create_dir_all(&first).unwrap();
            fs::create_dir_all(&second).unwrap();
            // Same device, different inode: only `&&` gives false here.
            let result = same_directory(&first.join("config.json"), &second.join("config.json"));
            assert!(matches!(result, Ok(false)), "got {result:?}");
        },
    );
}

#[test]
fn same_directory_with_a_missing_project_directory_is_false() {
    fakes::within(
        "missing project directory",
        Deadline::after(Duration::from_secs(10)),
        || {
            let setup = Setup::new();
            let project = setup.home.join("missing").join("config.json");
            let result = same_directory(&setup.global(), &project);
            assert!(matches!(result, Ok(false)), "got {result:?}");
        },
    );
}

#[test]
fn same_directory_with_a_project_parent_under_a_file_is_an_io_error() {
    fakes::within(
        "project parent under a file",
        Deadline::after(Duration::from_secs(10)),
        || {
            let setup = Setup::new();
            let afile = setup.home.join("afile");
            fs::write(&afile, "not a directory").unwrap();
            let project = afile.join("sub").join("notes.toml");
            let result = same_directory(&setup.global(), &project);
            assert!(
                matches!(result, Err(crate::ConfigError::Io { .. })),
                "got {result:?}"
            );
        },
    );
}

// ------------------------------------------------------- read-only entries

/// One overlap case: the global and project files, the kept entries per
/// layer, the refused entry, and whether it was kept in the project layer.
type OverlapCase<'a> = (
    Option<&'a str>,
    Option<&'a str>,
    Vec<(&'a str, Vec<&'a str>)>,
    Vec<(&'a str, Vec<&'a str>)>,
    &'a str,
    bool,
);

/// The refusal for an entry names it and writes neither file.
fn refused(
    error: crate::ConfigError,
    file: &Path,
    command: &str,
    before: (Option<Vec<u8>>, Option<Vec<u8>>),
    setup: &Setup,
) {
    let crate::ConfigError::WrongType {
        key, source_name, ..
    } = &error
    else {
        panic!("an entry refusal is WrongType: {error:?}");
    };
    assert_eq!(key, &format!("shell.read_only.\"{command}\""));
    assert!(
        source_name.contains(&file.display().to_string()),
        "{source_name} names the layer"
    );
    assert_eq!(
        error.code(),
        contract::ErrorCode::ConfigInvalid,
        "a refusal reports config_invalid"
    );
    assert_eq!(
        setup.bytes(&setup.global()),
        before.0,
        "neither file changes"
    );
    assert_eq!(
        setup.bytes(&setup.project_file()),
        before.1,
        "neither file changes"
    );
}

#[test]
fn the_entry_validator_rejects_each_bad_shape() {
    let flags = |flags: &[&str]| {
        flags
            .iter()
            .map(|flag| flag.to_string())
            .collect::<Vec<_>>()
    };
    assert!(matches!(
        validate_read_only_entry("", &[]),
        Err(ReadOnlyEntryProblem::Empty)
    ));
    assert!(matches!(
        validate_read_only_entry("ls", &[]),
        Err(ReadOnlyEntryProblem::Builtin)
    ));
    assert!(matches!(
        validate_read_only_entry("git status extra", &[]),
        Err(ReadOnlyEntryProblem::Builtin)
    ));
    assert!(matches!(
        validate_read_only_entry("/bin/ls", &[]),
        Err(ReadOnlyEntryProblem::Builtin)
    ));
    assert!(validate_read_only_entry("gist query", &[]).is_ok());
    assert!(validate_read_only_entry("acme-cli query", &flags(&["--json"])).is_ok());
    assert!(matches!(
        validate_read_only_entry("acme-cli query", &flags(&["--output"])),
        Err(ReadOnlyEntryProblem::UnsafeFlag)
    ));
    assert!(matches!(
        validate_read_only_entry("acme-cli query", &flags(&["--output=out.txt"])),
        Err(ReadOnlyEntryProblem::UnsafeFlag)
    ));
    // The ruling's words are literal: "-oevil" is not "-o".
    assert!(validate_read_only_entry("acme-cli query", &flags(&["-oevil"])).is_ok());
    assert!(matches!(
        validate_read_only_entry("./scripts/q", &flags(&["--json"])),
        Err(ReadOnlyEntryProblem::SlashPath)
    ));
    assert!(matches!(
        validate_read_only_entry("acme-cli sub/path", &flags(&["--json"])),
        Err(ReadOnlyEntryProblem::SlashPath)
    ));
    assert!(matches!(
        validate_read_only_entry("acme-cli", &flags(&["--json"])),
        Err(ReadOnlyEntryProblem::OneWord)
    ));
    assert!(matches!(
        validate_read_only_entry("acme-cli query", &flags(&["--exec=ls"])),
        Err(ReadOnlyEntryProblem::ValuedFlag)
    ));
    assert!(matches!(
        validate_read_only_entry("acme-cli query", &flags(&["--exec="])),
        Err(ReadOnlyEntryProblem::ValuedFlag)
    ));
}

#[test]
fn entries_merge_beside_the_notes_and_keep_every_other_key() {
    fakes::within(
        "entry merge save",
        Deadline::after(Duration::from_secs(10)),
        || {
            let setup = Setup::new();
            setup.write(
                &setup.global(),
                r#"{"model": "a/b", "reviewer": {"context": "old global", "tone": "dry"}}"#,
            );
            setup.write(
                &setup.project_file(),
                r#"{"reviewer": {"context": "old project"}, "shell": {"read_only": {"acme-cli query": {"flags": ["--json"]}}}}"#,
            );
            let read = setup.read();
            let saved = setup
                .save_entries(
                    Some((&read.global.version, "new global")),
                    Some((&read.project.version, "old project")),
                    vec![Setup::entry("acme-cli show", &["--json"])],
                    vec![Setup::entry("acme-cli push", &[])],
                )
                .unwrap();
            assert!(matches!(saved, NotesSaved::Written(_)));
            let global: Value =
                serde_json::from_slice(&setup.bytes(&setup.global()).unwrap()).unwrap();
            assert_eq!(global["model"], json!("a/b"));
            assert_eq!(global["reviewer"]["context"], json!("new global"));
            assert_eq!(global["reviewer"]["tone"], json!("dry"));
            assert_eq!(
                global["shell"]["read_only"]["acme-cli show"]["flags"],
                json!(["--json"])
            );
            // An absent or empty entry list changes no configured entry.
            let project: Value =
                serde_json::from_slice(&setup.bytes(&setup.project_file()).unwrap()).unwrap();
            assert_eq!(project["reviewer"]["context"], json!("old project"));
            assert_eq!(
                project["shell"]["read_only"]["acme-cli query"]["flags"],
                json!(["--json"])
            );
            assert_eq!(
                project["shell"]["read_only"]["acme-cli push"]["flags"],
                json!([])
            );
        },
    );
}

#[test]
fn an_entries_only_save_adds_no_empty_context_field() {
    fakes::within(
        "entries-only save",
        Deadline::after(Duration::from_secs(10)),
        || {
            let setup = Setup::new();
            setup.write(&setup.global(), r#"{"shell": {"read_only": {}}}"#);
            let read = setup.read();
            assert_eq!(read.global.text, "");
            let saved = setup
                .save_entries(
                    Some((&read.global.version, "")),
                    None,
                    vec![Setup::entry("acme-cli query", &["--json"])],
                    Vec::new(),
                )
                .unwrap();
            assert!(matches!(saved, NotesSaved::Written(_)));
            let global: Value =
                serde_json::from_slice(&setup.bytes(&setup.global()).unwrap()).unwrap();
            assert!(global.get("reviewer").is_none(), "no empty context field");
            assert_eq!(
                global["shell"]["read_only"]["acme-cli query"]["flags"],
                json!(["--json"])
            );
        },
    );
}

#[test]
fn bad_entries_fail_naming_the_entry_with_neither_file_changed() {
    fakes::within(
        "bad entry save",
        Deadline::after(Duration::from_secs(10)),
        || {
            for (command, flags) in [
                ("acme-cli query", vec!["--output"]),
                ("ls", vec!["--json"]),
                ("git status", vec![]),
                ("/bin/ls", vec![]),
                ("acme-cli query", vec!["--exec=ls"]),
                ("acme-cli query", vec!["--exec="]),
                ("", vec!["--json"]),
                ("acme-cli", vec!["--json"]),
                ("./scripts/q", vec!["--json"]),
                ("acme-cli sub/path", vec![]),
            ] {
                let setup = Setup::new();
                setup.write(
                    &setup.global(),
                    r#"{"reviewer": {"context": "old global"}}"#,
                );
                setup.write(
                    &setup.project_file(),
                    r#"{"reviewer": {"context": "old project"}}"#,
                );
                let before = (
                    setup.bytes(&setup.global()),
                    setup.bytes(&setup.project_file()),
                );
                let read = setup.read();
                let result = setup.save_entries(
                    Some((&read.global.version, "new global")),
                    Some((&read.project.version, "new project")),
                    vec![Setup::entry(command, &flags)],
                    Vec::new(),
                );
                let Err(error) = result else {
                    panic!("{command} is refused");
                };
                refused(error, &setup.global(), command, before, &setup);
            }
        },
    );
}

#[test]
fn overlapping_entries_fail_against_either_layer_a_sibling_or_a_flip() {
    fakes::within(
        "overlapping entry save",
        Deadline::after(Duration::from_secs(10)),
        || {
            // Every kept entry must fail.
            let cases: Vec<OverlapCase> = vec![
                // Ancestor then descendant within reach of the union.
                (
                    Some(r#"{"shell": {"read_only": {"acme-cli": {"flags": []}}}}"#),
                    None,
                    vec![("acme-cli query", vec!["--json"])],
                    vec![],
                    "acme-cli query",
                    false,
                ),
                // Descendant configured, ancestor kept.
                (
                    Some(r#"{"shell": {"read_only": {"acme-cli query": {"flags": ["--json"]}}}}"#),
                    None,
                    vec![("acme-cli", vec![])],
                    vec![],
                    "acme-cli",
                    false,
                ),
                // Overlap with the project layer, which sends nothing.
                (
                    None,
                    Some(r#"{"shell": {"read_only": {"acme-cli": {"flags": []}}}}"#),
                    vec![("acme-cli query", vec!["--json"])],
                    vec![],
                    "acme-cli query",
                    false,
                ),
                // A layer flip: the sibling run keeps the other side.
                (
                    Some(r#"{"shell": {"read_only": {"acme-cli": {"flags": []}}}}"#),
                    None,
                    vec![],
                    vec![("acme-cli query", vec!["--json"])],
                    "acme-cli query",
                    true,
                ),
                // Siblings in one layer overlap each other.
                (
                    None,
                    None,
                    vec![("acme-cli query", vec!["--json"]), ("acme-cli", vec![])],
                    vec![],
                    "acme-cli",
                    false,
                ),
                // Siblings across layers overlap each other.
                (
                    None,
                    None,
                    vec![("acme-cli query", vec!["--json"])],
                    vec![("acme-cli", vec![])],
                    "acme-cli",
                    true,
                ),
            ];
            for (global, project, global_kept, project_kept, named, in_project) in cases {
                let setup = Setup::new();
                if let Some(text) = global {
                    setup.write(&setup.global(), text);
                }
                if let Some(text) = project {
                    setup.write(&setup.project_file(), text);
                }
                let before = (
                    setup.bytes(&setup.global()),
                    setup.bytes(&setup.project_file()),
                );
                let read = setup.read();
                let kept = |kept: Vec<(&str, Vec<&str>)>| {
                    kept.into_iter()
                        .map(|(command, flags)| Setup::entry(command, &flags))
                        .collect()
                };
                let result = setup.save_entries(
                    Some((&read.global.version, "")),
                    Some((&read.project.version, "")),
                    kept(global_kept),
                    kept(project_kept),
                );
                let Err(error) = result else {
                    panic!("{named} overlaps and is refused");
                };
                let file = if in_project {
                    setup.project_file()
                } else {
                    setup.global()
                };
                refused(error, &file, named, before, &setup);
            }
            // A word boundary is not an overlap: "query2" is another word.
            let setup = Setup::new();
            setup.write(
                &setup.global(),
                r#"{"shell": {"read_only": {"acme-cli query2": {"flags": []}}}}"#,
            );
            let read = setup.read();
            let saved = setup
                .save_entries(
                    Some((&read.global.version, "")),
                    None,
                    vec![Setup::entry("acme-cli query", &["--json"])],
                    Vec::new(),
                )
                .unwrap();
            assert!(matches!(saved, NotesSaved::Written(_)));
        },
    );
}

#[test]
fn duplicate_commands_in_one_layer_fail_before_any_write() {
    fakes::within(
        "duplicate entry save",
        Deadline::after(Duration::from_secs(10)),
        || {
            let setup = Setup::new();
            setup.write(
                &setup.global(),
                r#"{"reviewer": {"context": "old global"}}"#,
            );
            setup.write(
                &setup.project_file(),
                r#"{"reviewer": {"context": "old project"}}"#,
            );
            let before = (
                setup.bytes(&setup.global()),
                setup.bytes(&setup.project_file()),
            );
            let read = setup.read();
            let result = setup.save_entries(
                Some((&read.global.version, "new global")),
                Some((&read.project.version, "new project")),
                vec![
                    Setup::entry("acme-cli query", &["--json"]),
                    Setup::entry("acme-cli query", &[]),
                ],
                Vec::new(),
            );
            let Err(error) = result else {
                panic!("a repeated command is refused");
            };
            refused(error, &setup.global(), "acme-cli query", before, &setup);
        },
    );
}

#[test]
fn a_malformed_layer_fails_an_entry_merge_with_neither_file_written() {
    fakes::within(
        "malformed entry save",
        Deadline::after(Duration::from_secs(10)),
        || {
            for malformed in [
                r#"{"shell": {"read_only": []}}"#,
                r#"{"shell": {"read_only": {"acme-cli query": ["--json"]}}}"#,
                r#"{"shell": {"read_only": {"acme-cli query": {"flags": "--json"}}}}"#,
                r#"{"shell": {"read_only": {"acme-cli query": {}}}}"#,
                r#"{"shell": "entries"}"#,
            ] {
                // Malformed in the sent layer.
                let setup = Setup::new();
                setup.write(&setup.global(), malformed);
                setup.write(
                    &setup.project_file(),
                    r#"{"reviewer": {"context": "old project"}}"#,
                );
                let before = (
                    setup.bytes(&setup.global()),
                    setup.bytes(&setup.project_file()),
                );
                let read = setup.read();
                let result = setup.save_entries(
                    Some((&read.global.version, "")),
                    None,
                    vec![Setup::entry("acme-cli query", &["--json"])],
                    Vec::new(),
                );
                let Err(error) = result else {
                    panic!("{malformed} is refused");
                };
                refused(error, &setup.global(), "acme-cli query", before, &setup);
                // Malformed in the layer that sends nothing.
                let setup = Setup::new();
                setup.write(
                    &setup.global(),
                    r#"{"reviewer": {"context": "old global"}}"#,
                );
                setup.write(&setup.project_file(), malformed);
                let before = (
                    setup.bytes(&setup.global()),
                    setup.bytes(&setup.project_file()),
                );
                let read = setup.read();
                let result = setup.save_entries(
                    Some((&read.global.version, "")),
                    None,
                    vec![Setup::entry("acme-cli query", &["--json"])],
                    Vec::new(),
                );
                let Err(error) = result else {
                    panic!("{malformed} in the other layer is refused");
                };
                refused(error, &setup.global(), "acme-cli query", before, &setup);
            }
        },
    );
}

#[test]
fn a_flat_shell_key_is_not_a_configured_entry() {
    fakes::within(
        "flat shell key save",
        Deadline::after(Duration::from_secs(10)),
        || {
            let setup = Setup::new();
            setup.write(
                &setup.global(),
                r#"{"shell.read_only": {"acme-cli query": {"flags": ["--json"]}}}"#,
            );
            let read = setup.read();
            let saved = setup
                .save_entries(
                    Some((&read.global.version, "")),
                    None,
                    vec![Setup::entry("acme-cli query", &["--json"])],
                    Vec::new(),
                )
                .unwrap();
            assert!(matches!(saved, NotesSaved::Written(_)));
            let global: Value =
                serde_json::from_slice(&setup.bytes(&setup.global()).unwrap()).unwrap();
            assert_eq!(
                global["shell"]["read_only"]["acme-cli query"]["flags"],
                json!(["--json"])
            );
        },
    );
}

#[test]
fn a_stale_version_with_entries_writes_nothing() {
    fakes::within(
        "stale entry save",
        Deadline::after(Duration::from_secs(10)),
        || {
            let setup = Setup::new();
            setup.write(
                &setup.global(),
                r#"{"reviewer": {"context": "old global"}}"#,
            );
            setup.write(
                &setup.project_file(),
                r#"{"reviewer": {"context": "old project"}}"#,
            );
            let read = setup.read();
            setup.write(&setup.global(), r#"{"reviewer": {"context": "changed"}}"#);
            let before = (
                setup.bytes(&setup.global()),
                setup.bytes(&setup.project_file()),
            );
            let saved = setup
                .save_entries(
                    Some((&read.global.version, "new global")),
                    Some((&read.project.version, "new project")),
                    vec![Setup::entry("acme-cli query", &["--json"])],
                    Vec::new(),
                )
                .unwrap();
            assert!(matches!(saved, NotesSaved::Stale));
            assert_eq!(setup.bytes(&setup.global()), before.0);
            assert_eq!(setup.bytes(&setup.project_file()), before.1);
        },
    );
}

#[test]
fn already_present_entries_converge_to_saved_without_rewriting() {
    fakes::within(
        "converging entry save",
        Deadline::after(Duration::from_secs(10)),
        || {
            let setup = Setup::new();
            setup.write(
                &setup.global(),
                r#"{"reviewer": {"context": "old global"}}"#,
            );
            let read = setup.read();
            let saved = setup
                .save_entries(
                    Some((&read.global.version, "old global")),
                    None,
                    vec![Setup::entry("acme-cli query", &["--json"])],
                    Vec::new(),
                )
                .unwrap();
            assert!(matches!(saved, NotesSaved::Written(_)));
            let read = setup.read();
            reset_write_count();
            let saved = setup
                .save_entries(
                    Some((&read.global.version, "old global")),
                    None,
                    vec![Setup::entry("acme-cli query", &["--json"])],
                    Vec::new(),
                )
                .unwrap();
            assert!(matches!(saved, NotesSaved::Written(_)));
            assert_eq!(write_count(), 0, "a converged save rewrites nothing");
        },
    );
}
