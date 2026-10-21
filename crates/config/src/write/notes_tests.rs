//! Reading and saving the person's reviewer notes (`docs/permissions.md`,
//! "What the person tells it"): each layer's own `reviewer.context` with
//! its version, and a locked read-compare-write that refuses a stale save
//! and writes each file as `docs/state.md`, "Concurrent access" says.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use super::{FileOutcome, NotesSaved, NotesWrite, same_directory, save_reviewer_notes};
use crate::home::ProjectKey;
use crate::reviewer_notes;
use crate::write::atomic::{Stage, fail_nth};
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

    fn save(
        &self,
        global: Option<(&str, &str)>,
        project_layer: Option<(&str, &str)>,
    ) -> Result<NotesSaved, crate::ConfigError> {
        let write = |sent: Option<(&str, &str)>| {
            sent.map(|(version, text)| NotesWrite {
                version: version.to_owned(),
                text: text.to_owned(),
            })
        };
        save_reviewer_notes(
            &self.home,
            &Self::project(),
            write(global),
            write(project_layer),
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
