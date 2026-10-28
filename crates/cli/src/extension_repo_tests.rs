//! `fiber extension list` and `remove` in a project whose repository
//! declares extensions: what each command writes, the records it leaves,
//! and what it never touches. Approvals go through `extensions::Store`,
//! the way `fiber approve` records them.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code; a failure is the test's"
)]

use std::fs;
use std::io::{BufRead, Cursor};
use std::path::{Path, PathBuf};

use super::{Io, install, list_in, remove_in, update_all};
use crate::CliError;
use extensions::Request;

const FIBER_VERSION: &str = "0.1.0";

/// One repository package a test declares: its directory, manifest,
/// extra files and whether it is required.
type Declared<'a> = (&'a str, serde_json::Value, &'a [(&'a str, &'a str)], bool);

/// First-party names, whose short and full spellings differ: what the
/// short-and-full-name remove tests declare.
const MEMORY: &str = "github.com/aakshintala/fiber/extensions/memory";

/// A Fiber home and a git workspace in a temporary directory. No test
/// touches the real home or the test process's environment.
struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        let setup = Self {
            root: fakes::TempDir::new("fiber-cli-extension-repo"),
        };
        fs::create_dir_all(setup.home()).unwrap();
        fs::create_dir_all(setup.workspace()).unwrap();
        setup
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn workspace(&self) -> PathBuf {
        self.root.path().join("workspace")
    }

    fn project(&self) -> config::ProjectKey {
        crate::project_of(&self.home(), &self.workspace())
            .unwrap()
            .1
    }

    /// A workspace declaring these repository packages.
    fn declare(&self, pkgs: &[Declared]) {
        let workspace = self.workspace();
        if !workspace.join(".git").exists() {
            fakes::git::git(&workspace, &["init", "--quiet"]);
        }
        let mut paths = Vec::new();
        for (dir, manifest, files, required) in pkgs {
            let mut entry = serde_json::json!({"path": dir});
            if *required {
                entry["required"] = serde_json::json!(true);
            }
            paths.push(entry);
            write_file(
                &workspace.join(dir).join("extension.json"),
                &manifest.to_string(),
            );
            for (file, text) in *files {
                write_file(&workspace.join(dir).join(file), text);
            }
        }
        write_file(
            &workspace.join(".fiber/config.json"),
            &serde_json::json!({"repository_extensions": paths}).to_string(),
        );
    }

    /// Approves the declared extension `name` for its current content,
    /// the way `fiber approve` records it, and returns the content hash.
    fn approve(&self, name: &str) -> String {
        let clock = fakes::clock::FakeClock::new();
        let items = extensions::declared_items(&self.workspace(), &*clock).unwrap();
        let item = items.into_iter().find(|found| found.name == name).unwrap();
        let mut index = extensions::Index::load(&self.home());
        let digest = extensions::hash(&mut index, &item).unwrap();
        extensions::Store::new(&self.home(), &self.project(), clock)
            .approve(&item, &digest)
            .unwrap();
        digest
    }

    /// Installs the personal extension in `source` without a terminal,
    /// failing the test when it does not exit zero.
    fn install_personal(&self, dir: &str, manifest: &serde_json::Value) {
        let source = self.root.path().join("src").join(dir);
        write_file(&source.join("extension.json"), &manifest.to_string());
        let clock = fakes::clock::FakeClock::new();
        let mut unread = Unread;
        let mut err = Vec::new();
        // nextest runs each test in its own process, so no other test's
        // child can hold this lock (`docs/testing/running.md`).
        let got = install(
            &self.home(),
            Request::Path(source.clone()),
            None,
            FIBER_VERSION,
            &extensions::Origin::github(),
            &*clock,
            Io {
                terminal: false,
                input: &mut unread,
                err: &mut err,
            },
        );
        assert_eq!(got.unwrap(), 0, "{}", String::from_utf8(err).unwrap());
    }

    /// The approval record for `digest`, wherever the project keeps it.
    fn approval(&self, digest: &str) -> Option<serde_json::Value> {
        for project in fs::read_dir(self.home().join("projects")).unwrap() {
            let path = project.unwrap().path().join("approvals").join(digest);
            if path.is_file() {
                return Some(serde_json::from_slice(&fs::read(path).unwrap()).unwrap());
            }
        }
        None
    }
}

fn write_file(file: &Path, text: &str) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, text).unwrap();
}

fn manifest(name: &str, version: &str) -> serde_json::Value {
    serde_json::json!({ "name": name, "version": version, "fiber": "0.1.0", "api": 1 })
}

/// Input that fails the test if anything reads it: without a terminal
/// nothing is asked and nothing is read.
struct Unread;

impl std::io::Read for Unread {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("nothing is read without a terminal");
    }
}

impl BufRead for Unread {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        panic!("nothing is read without a terminal");
    }

    fn consume(&mut self, _: usize) {}
}

/// Runs `list_in` for `workspace`: its stdout, its stderr, and its
/// failure, if any.
fn run_list_in(home: &Path, workspace: &Path) -> (String, String, Result<(), CliError>) {
    let clock = fakes::clock::FakeClock::new();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let got = list_in(home, workspace, &*clock, &mut out, &mut err);
    (
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
        got,
    )
}

/// Runs `remove_in` for `typed`: its stderr, and its exit code or
/// failure.
fn run_remove_in(
    home: &Path,
    workspace: &Path,
    typed: &str,
    terminal: bool,
    input: &mut dyn BufRead,
) -> (String, Result<i32, CliError>) {
    let clock = fakes::clock::FakeClock::new();
    let mut err = Vec::new();
    let got = remove_in(
        home,
        workspace,
        typed,
        &*clock,
        Io {
            terminal,
            input: &mut *input,
            err: &mut err,
        },
    );
    (String::from_utf8(err).unwrap(), got)
}

/// Every byte Fiber home holds under `pinned/` and the approvals: what
/// the update tests prove unchanged.
fn snapshot(home: &Path) -> Vec<(String, Vec<u8>)> {
    let mut paths = Vec::new();
    for top in ["pinned", "projects"] {
        let mut dirs = vec![home.join(top)];
        while let Some(dir) = dirs.pop() {
            let mut entries: Vec<_> = fs::read_dir(&dir)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect();
            entries.sort();
            for path in entries {
                if path.is_dir() {
                    dirs.push(path);
                } else {
                    paths.push(path);
                }
            }
        }
    }
    let mut found: Vec<(String, Vec<u8>)> = paths
        .iter()
        .map(|path| {
            (
                path.strip_prefix(home)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                fs::read(path).unwrap(),
            )
        })
        .collect();
    found.sort();
    found
}

#[test]
fn list_shows_personal_lines_then_repository_rows() {
    let setup = Setup::new();
    setup.install_personal("y", &manifest("github.com/acme/y", "v1.0.0"));
    setup.declare(&[("pkg", manifest("github.com/acme/pkg", "v2.0.0"), &[], false)]);
    let digest = setup.approve("github.com/acme/pkg");
    let (out, err, got) = run_list_in(&setup.home(), &setup.workspace());
    got.unwrap();
    assert_eq!(err, "");
    assert_eq!(
        out,
        format!(
            "github.com/acme/y v1.0.0 local\ngithub.com/acme/pkg v2.0.0 repository {} pkg {digest}\n",
            worktree::project(&setup.workspace()).display()
        )
    );
}

#[test]
fn list_shows_nothing_for_unapproved_or_neverd_content() {
    let setup = Setup::new();
    setup.declare(&[
        ("a", manifest("github.com/acme/a", "v1.0.0"), &[], false),
        ("b", manifest("github.com/acme/b", "v1.0.0"), &[], false),
        ("c", manifest("github.com/acme/c", "v1.0.0"), &[], false),
    ]);
    let clock = fakes::clock::FakeClock::new();
    let home = setup.home();
    let items = extensions::declared_items(&setup.workspace(), &*clock).unwrap();
    let mut index = extensions::Index::load(&home);
    let b = items
        .into_iter()
        .find(|found| found.name == "github.com/acme/b")
        .unwrap();
    let b_digest = extensions::hash(&mut index, &b).unwrap();
    extensions::Store::new(&home, &setup.project(), clock)
        .never(&b, &b_digest)
        .unwrap();
    setup.approve("github.com/acme/a");
    let (out, err, got) = run_list_in(&home, &setup.workspace());
    got.unwrap();
    assert_eq!(err, "");
    assert!(
        out.lines()
            .all(|line| !line.contains("github.com/acme/b") && !line.contains("github.com/acme/c")),
        "{out}"
    );
    assert_eq!(out.lines().count(), 1, "{out}");
    assert!(
        out.starts_with("github.com/acme/a v1.0.0 repository "),
        "{out}"
    );
}

#[test]
fn list_reports_a_bad_declaration_and_lists_personal() {
    let setup = Setup::new();
    setup.install_personal("y", &manifest("github.com/acme/y", "v1.0.0"));
    fakes::git::git(&setup.workspace(), &["init", "--quiet"]);
    write_file(
        &setup.workspace().join(".fiber/config.json"),
        &serde_json::json!({"repository_extensions": [{"path": "nope"}]}).to_string(),
    );
    let (out, err, got) = run_list_in(&setup.home(), &setup.workspace());
    got.unwrap();
    assert_eq!(
        err,
        "fiber: `repository_extensions` path `nope` does not exist.\n"
    );
    assert_eq!(out, "github.com/acme/y v1.0.0 local\n");
}

/// A workspace declaring the first-party `memory` package and a
/// personal install of the same name with data, approved: what both
/// remove spellings leave behind.
fn removal_setup() -> (Setup, String) {
    let setup = Setup::new();
    setup.install_personal("memory", &manifest(MEMORY, "v1.0.0"));
    write_file(&setup.home().join("data/memory/notes.txt"), "personal data");
    setup.declare(&[("pkg", manifest(MEMORY, "v2.0.0"), &[], false)]);
    let digest = setup.approve(MEMORY);
    (setup, digest)
}

#[test]
fn remove_nevers_the_current_content_by_short_name() {
    let (setup, digest) = removal_setup();
    let mut unread = Unread;
    let (err, got) = run_remove_in(
        &setup.home(),
        &setup.workspace(),
        "memory",
        false,
        &mut unread,
    );
    assert_eq!(got.unwrap(), 0, "{err}");
    assert_eq!(
        err,
        format!(
            "fiber: removed {MEMORY} from this project; it is not offered again until its content changes\n"
        )
    );
    let body = setup.approval(&digest).unwrap();
    assert_eq!(body["decision"], "never");
    assert!(setup.home().join("pinned").join(&digest).is_dir());
    assert!(setup.home().join("extensions/memory").is_dir());
    assert!(setup.home().join("data/memory/notes.txt").is_file());
}

#[test]
fn remove_nevers_the_current_content_by_full_name() {
    let (setup, digest) = removal_setup();
    let mut unread = Unread;
    let (err, got) = run_remove_in(
        &setup.home(),
        &setup.workspace(),
        MEMORY,
        false,
        &mut unread,
    );
    assert_eq!(got.unwrap(), 0, "{err}");
    assert_eq!(
        err,
        format!(
            "fiber: removed {MEMORY} from this project; it is not offered again until its content changes\n"
        )
    );
    let body = setup.approval(&digest).unwrap();
    assert_eq!(body["decision"], "never");
    assert!(setup.home().join("pinned").join(&digest).is_dir());
    assert!(setup.home().join("extensions/memory").is_dir());
    assert!(setup.home().join("data/memory/notes.txt").is_file());
}

#[test]
fn remove_declined_records_nothing() {
    let (setup, digest) = removal_setup();
    let mut input = Cursor::new("n\n");
    let (err, got) = run_remove_in(&setup.home(), &setup.workspace(), MEMORY, true, &mut input);
    assert_eq!(got.unwrap(), 1, "{err}");
    assert_eq!(
        err,
        format!("Remove {MEMORY}\nGo ahead? [y/N] fiber: nothing was removed.\n")
    );
    let body = setup.approval(&digest).unwrap();
    assert_eq!(body["decision"], "approve");
    assert!(setup.home().join("pinned").join(&digest).is_dir());
}

#[test]
fn remove_falls_through_without_an_approval_for_the_current_content() {
    let setup = Setup::new();
    setup.declare(&[("pkg", manifest("github.com/acme/pkg", "v2.0.0"), &[], false)]);
    let mut unread = Unread;
    let (err, got) = run_remove_in(
        &setup.home(),
        &setup.workspace(),
        "github.com/acme/pkg",
        false,
        &mut unread,
    );
    let failure = got.unwrap_err();
    assert!(
        matches!(&failure, CliError::Extensions(_)),
        "expected Extensions, got {failure:?}"
    );
    assert_eq!(failure.code(), contract::ErrorCode::ExtensionNotInstalled);
    assert_eq!(err, "");
}

/// The project key is computed only when the workspace declares an
/// extension (ruling 17): a plain workspace computes none, so neither
/// `list_in` nor `remove_in` reaches `git`.
#[test]
fn plain_workspace_computes_no_project() {
    let setup = Setup::new();
    setup.install_personal("y", &manifest("github.com/acme/y", "v1.0.0"));
    let got = super::repository_project(&setup.home(), &setup.workspace()).unwrap();
    assert_eq!(got, None);
    let (out, err, listed) = run_list_in(&setup.home(), &setup.workspace());
    listed.unwrap();
    assert_eq!(err, "");
    assert_eq!(out, "github.com/acme/y v1.0.0 local\n", "{out}");
    let mut unread = Unread;
    let (err, removed) = run_remove_in(
        &setup.home(),
        &setup.workspace(),
        "github.com/acme/y",
        false,
        &mut unread,
    );
    assert_eq!(removed.unwrap(), 0, "{err}");
    assert_eq!(err, "fiber: removed github.com/acme/y\n");
}

#[test]
fn declaring_workspace_computes_the_project() {
    let setup = Setup::new();
    setup.declare(&[("pkg", manifest("github.com/acme/pkg", "v2.0.0"), &[], false)]);
    let got = super::repository_project(&setup.home(), &setup.workspace()).unwrap();
    assert_eq!(got, Some(setup.project()));
}

#[test]
fn remove_reports_a_bad_declaration_and_removes_the_personal_install() {
    let setup = Setup::new();
    setup.install_personal("x", &manifest("github.com/acme/x", "v1.0.0"));
    fakes::git::git(&setup.workspace(), &["init", "--quiet"]);
    write_file(
        &setup.workspace().join(".fiber/config.json"),
        &serde_json::json!({"repository_extensions": [{"path": "nope"}]}).to_string(),
    );
    let mut unread = Unread;
    let (err, got) = run_remove_in(
        &setup.home(),
        &setup.workspace(),
        "github.com/acme/x",
        false,
        &mut unread,
    );
    assert_eq!(got.unwrap(), 0, "{err}");
    assert_eq!(
        err,
        "fiber: `repository_extensions` path `nope` does not exist.\nfiber: removed github.com/acme/x\n"
    );
    assert!(!setup.home().join("extensions/github.com-acme-x").exists());
}

#[test]
fn update_all_leaves_repository_copies_and_approvals_alone() {
    let (setup, _) = removal_setup();
    let before = snapshot(&setup.home());
    let mut unread = Unread;
    let clock = fakes::clock::FakeClock::new();
    let mut err = Vec::new();
    let got = update_all(
        &setup.home(),
        FIBER_VERSION,
        &extensions::Origin::github(),
        &*clock,
        Io {
            terminal: false,
            input: &mut unread,
            err: &mut err,
        },
    );
    got.unwrap();
    assert_eq!(snapshot(&setup.home()), before);
}

#[test]
fn update_of_the_personal_install_leaves_repository_copies_and_approvals_alone() {
    let (setup, _) = removal_setup();
    let before = snapshot(&setup.home());
    let clock = fakes::clock::FakeClock::new();
    let mut unread = Unread;
    let mut err = Vec::new();
    let got = install(
        &setup.home(),
        Request::Update(MEMORY.to_owned()),
        None,
        FIBER_VERSION,
        &extensions::Origin::github(),
        &*clock,
        Io {
            terminal: false,
            input: &mut unread,
            err: &mut err,
        },
    );
    got.unwrap();
    assert_eq!(snapshot(&setup.home()), before);
}

#[test]
fn update_of_a_repository_only_name_fails_and_changes_nothing() {
    let setup = Setup::new();
    setup.declare(&[("pkg", manifest("github.com/acme/pkg", "v2.0.0"), &[], false)]);
    let digest = setup.approve("github.com/acme/pkg");
    let before = snapshot(&setup.home());
    let clock = fakes::clock::FakeClock::new();
    let mut unread = Unread;
    let mut err = Vec::new();
    let got = install(
        &setup.home(),
        Request::Update("github.com/acme/pkg".to_owned()),
        None,
        FIBER_VERSION,
        &extensions::Origin::github(),
        &*clock,
        Io {
            terminal: false,
            input: &mut unread,
            err: &mut err,
        },
    );
    let failure = got.unwrap_err();
    assert!(
        matches!(&failure, CliError::Extensions(_)),
        "expected Extensions, got {failure:?}"
    );
    assert_eq!(failure.code(), contract::ErrorCode::ExtensionMissing);
    assert_eq!(snapshot(&setup.home()), before);
    assert!(setup.home().join("pinned").join(&digest).is_dir());
}
