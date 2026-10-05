//! What a repository declares: which items, and which files each pins.

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;

use contract::ErrorCode;
use contract::events::OfferedKind;
use serde_json::{Value, json};

use super::{RepoItem, declared_items};
use crate::Error;

const HOOKS_FILE: &str = ".fiber/config/github.com-aakshintala-fiber-extensions-hooks.json";

/// A git repository in `<tmp>/repo`, with Fiber home beside it in
/// `<tmp>/home`.
pub(super) struct Repo {
    tmp: fakes::TempDir,
}

impl Repo {
    pub(super) fn new() -> Self {
        let repo = Self::bare();
        let status = Command::new("git")
            .args(["init", "-q"])
            .arg(repo.root())
            .status()
            .unwrap();
        assert!(status.success());
        repo
    }

    /// A directory git knows nothing about.
    pub(super) fn bare() -> Self {
        let tmp = fakes::TempDir::new("fiber-repo");
        fs::create_dir_all(tmp.path().join("repo")).unwrap();
        fs::create_dir_all(tmp.path().join("home")).unwrap();
        Self { tmp }
    }

    pub(super) fn root(&self) -> PathBuf {
        self.tmp.path().join("repo")
    }

    pub(super) fn home(&self) -> PathBuf {
        self.tmp.path().join("home")
    }

    /// A file outside the repository, beside it.
    pub(super) fn elsewhere(&self, rel: &str, text: &str) -> PathBuf {
        let path = self.tmp.path().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        path
    }

    pub(super) fn write(&self, rel: &str, text: &str) {
        let path = self.root().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    pub(super) fn executable(&self, rel: &str) {
        fs::set_permissions(self.root().join(rel), fs::Permissions::from_mode(0o755)).unwrap();
    }

    pub(super) fn link(&self, rel: &str, target: &Path) {
        let path = self.root().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        symlink(target, path).unwrap();
    }

    pub(super) fn config(&self, value: &Value) {
        self.write(".fiber/config.json", &value.to_string());
    }

    pub(super) fn hooks(&self, value: &Value) {
        self.write(HOOKS_FILE, &json!({"hooks": value}).to_string());
    }

    /// An extension package at `dir` named `name`, with an entry script.
    pub(super) fn package(&self, dir: &str, name: &str, extra: &Value) {
        let mut manifest = json!({"name": name, "version": "v1.0.0", "fiber": "0.1.0", "api": 1});
        if let (Some(base), Some(more)) = (manifest.as_object_mut(), extra.as_object()) {
            base.extend(more.clone());
        }
        self.write(&format!("{dir}/extension.json"), &manifest.to_string());
        self.write(&format!("{dir}/init.lua"), "-- entry\n");
    }

    pub(super) fn items(&self) -> Vec<RepoItem> {
        declared_items(&self.root()).unwrap()
    }

    pub(super) fn item(&self, kind: OfferedKind, name: &str) -> RepoItem {
        self.items()
            .into_iter()
            .find(|i| i.kind == kind && i.name == name)
            .unwrap()
    }
}

pub(super) fn rels(item: &RepoItem) -> Vec<&str> {
    item.files.iter().map(|f| f.rel.as_str()).collect()
}

fn server(command: &str, args: &Value) -> Value {
    json!({"mcp": {"servers": {"db": {"command": command, "args": args}}}})
}

#[test]
fn items_come_in_offer_order_with_their_names_paths_and_required() {
    let repo = Repo::new();
    repo.package("tools/b", "fiber.test/b", &json!({}));
    repo.package("tools/a", "fiber.test/a", &json!({}));
    repo.config(&json!({
        "repository_extensions": [{"path": "tools/b", "required": true}, {"path": "tools/a"}],
        "mcp": {"servers": {"z": {"command": "x", "required": true}, "y": {"url": "https://x"}}},
    }));
    repo.hooks(
        &json!({"fmt": {"point": "after_tool", "command": "cargo"}, "build": {"point": "x"}}),
    );
    let got: Vec<_> = repo
        .items()
        .iter()
        .map(|i| (i.kind, i.name.clone(), i.path.clone(), i.required))
        .collect();
    let hooks_path = HOOKS_FILE.to_owned();
    let config_path = ".fiber/config.json".to_owned();
    assert_eq!(
        got,
        vec![
            (
                OfferedKind::Extension,
                "fiber.test/b".into(),
                "tools/b".into(),
                true
            ),
            (
                OfferedKind::Extension,
                "fiber.test/a".into(),
                "tools/a".into(),
                false
            ),
            (OfferedKind::Hook, "build".into(), hooks_path.clone(), false),
            (OfferedKind::Hook, "fmt".into(), hooks_path, false),
            (
                OfferedKind::McpServer,
                "y".into(),
                config_path.clone(),
                false
            ),
            (OfferedKind::McpServer, "z".into(), config_path, true),
        ]
    );
}

#[test]
fn a_repository_that_declares_nothing_has_no_items() {
    let repo = Repo::new();
    assert!(repo.items().is_empty());
}

#[test]
fn an_extension_pins_the_files_git_does_not_ignore() {
    let repo = Repo::new();
    repo.package("pkg", "fiber.test/p", &json!({}));
    repo.write(".gitignore", "node_modules/\n");
    repo.write("pkg/.gitignore", "dist\n");
    repo.write("pkg/node_modules/dep/index.js", "x");
    repo.write("pkg/dist", "built");
    repo.write("pkg/skills/s/SKILL.md", "skill");
    repo.write("pkg/extension.json.d/extension.json", "{}");
    repo.write("other/file", "not in the package");
    repo.config(&json!({"repository_extensions": [{"path": "pkg"}]}));
    let item = repo.items().remove(0);
    assert_eq!(
        rels(&item),
        [
            ".gitignore",
            "extension.json",
            "extension.json.d/extension.json",
            "init.lua",
            "skills/s/SKILL.md"
        ]
    );
}

#[test]
fn an_extension_path_must_be_a_package_directory_inside_the_repository() {
    let repo = Repo::new();
    repo.package("pkg", "fiber.test/p", &json!({}));
    repo.write("file.txt", "x");
    let outside = repo.elsewhere("elsewhere/extension.json", "{}");
    let cases = [
        ("/etc", "is absolute"),
        (outside.parent().unwrap().to_str().unwrap(), "is absolute"),
        ("../elsewhere", "is outside the repository"),
        ("pkg/../../elsewhere", "is outside the repository"),
        ("missing", "does not exist"),
        ("file.txt", "is not a directory"),
    ];
    for (path, why) in cases {
        repo.config(&json!({"repository_extensions": [{"path": path}]}));
        let e = declared_items(&repo.root()).unwrap_err();
        assert!(
            matches!(&e, Error::BadRepositoryPath { path: p, why: w } if p == path && w.starts_with(why)),
            "{path}: {e}"
        );
        assert_eq!(e.code(), ErrorCode::ConfigInvalid);
    }
}

#[test]
fn a_link_that_leaves_the_repository_is_not_a_package() {
    let repo = Repo::new();
    let outside = repo.elsewhere("elsewhere/extension.json", "{}");
    repo.link("pkg", outside.parent().unwrap());
    repo.config(&json!({"repository_extensions": [{"path": "pkg"}]}));
    let e = declared_items(&repo.root()).unwrap_err();
    assert!(matches!(e, Error::BadRepositoryPath { .. }), "{e}");
}

#[test]
fn a_package_in_a_directory_git_does_not_know_names_the_package() {
    let repo = Repo::bare();
    repo.package("pkg", "fiber.test/p", &json!({}));
    repo.config(&json!({"repository_extensions": [{"path": "pkg"}]}));
    let e = declared_items(&repo.root()).unwrap_err();
    assert!(
        matches!(&e, Error::Pin { item, .. } if item == "fiber.test/p"),
        "{e}"
    );
}

#[test]
fn a_package_without_a_manifest_is_an_error() {
    let repo = Repo::new();
    repo.write("pkg/init.lua", "x");
    repo.config(&json!({"repository_extensions": [{"path": "pkg"}]}));
    assert!(declared_items(&repo.root()).is_err());
}

#[test]
fn a_command_names_a_file_only_when_it_has_a_slash() {
    let repo = Repo::new();
    // A file named like the bare word is not what a PATH lookup runs.
    repo.write("cargo", "not the program");
    repo.write("scripts/run.sh", "echo");
    repo.config(&server("scripts/run.sh", &json!([])));
    repo.write("scripts/other.sh", "echo");
    let item = repo.item(OfferedKind::McpServer, "db");
    assert_eq!(rels(&item), ["scripts/run.sh"]);
    repo.config(&server("cargo", &json!([])));
    assert!(rels(&repo.item(OfferedKind::McpServer, "db")).is_empty());
}

#[test]
fn every_argument_may_name_a_file_and_a_miss_is_not_an_error() {
    let repo = Repo::new();
    repo.write("a.js", "1");
    repo.write("lib/b.js", "2");
    repo.write("dir/inner.txt", "3");
    repo.config(&server(
        "node",
        &json!([
            "a.js",
            "--flag",
            "lib/b.js",
            "lib/b.js",
            "missing.js",
            "dir",
            "a.js/under",
            "x\u{0}y",
            "https://example.test/x"
        ]),
    ));
    let item = repo.item(OfferedKind::McpServer, "db");
    assert_eq!(rels(&item), ["a.js", "lib/b.js"]);
    assert!(item.outside.is_empty());
}

#[test]
fn a_hook_pins_the_files_its_command_and_args_name() {
    let repo = Repo::new();
    repo.write("scripts/warn.sh", "echo");
    repo.write("scripts/lib.sh", "lib");
    repo.hooks(&json!({"warn": {"point": "session_start", "command": "scripts/warn.sh", "args": ["scripts/lib.sh"]}}));
    let item = repo.item(OfferedKind::Hook, "warn");
    assert_eq!(rels(&item), ["scripts/lib.sh", "scripts/warn.sh"]);
    assert_eq!(
        item.declaration,
        Some(
            json!({"name": "warn", "entry": {"point": "session_start", "command": "scripts/warn.sh", "args": ["scripts/lib.sh"]}})
        )
    );
}

#[test]
fn a_link_out_of_the_repository_is_not_pinned_and_is_named() {
    let repo = Repo::new();
    let outside = repo.elsewhere("elsewhere/steal.sh", "echo");
    repo.link("scripts/steal.sh", &outside);
    repo.config(&server(
        "scripts/steal.sh",
        &json!([outside.to_str().unwrap()]),
    ));
    let item = repo.item(OfferedKind::McpServer, "db");
    assert!(item.files.is_empty());
    assert_eq!(
        item.outside,
        ["scripts/steal.sh", outside.to_str().unwrap()]
    );
}

#[test]
fn a_link_inside_the_repository_pins_its_target() {
    let repo = Repo::new();
    repo.write("real/run.sh", "echo");
    repo.link("scripts/run.sh", &repo.root().join("real/run.sh"));
    repo.config(&server("scripts/run.sh", &json!([])));
    let item = repo.item(OfferedKind::McpServer, "db");
    assert_eq!(rels(&item), ["real/run.sh"]);
}

#[test]
fn a_sibling_directory_with_the_repository_name_as_a_prefix_is_outside() {
    let repo = Repo::new();
    let evil = repo.elsewhere("repo-evil/run.sh", "echo");
    let inside = repo.root().join("run.sh");
    repo.write("run.sh", "echo");
    repo.config(&server(
        "/x/placeholder",
        &json!([evil.to_str().unwrap(), inside.to_str().unwrap()]),
    ));
    let item = repo.item(OfferedKind::McpServer, "db");
    assert_eq!(rels(&item), ["run.sh"]);
    assert_eq!(item.outside, [evil.to_str().unwrap()]);
}

#[test]
fn an_extension_file_that_is_a_link_out_is_not_pinned() {
    let repo = Repo::new();
    repo.package("pkg", "fiber.test/p", &json!({}));
    let outside = repo.elsewhere("elsewhere/secret", "s");
    repo.link("pkg/leak", &outside);
    repo.link("pkg/alias", &repo.root().join("pkg/init.lua"));
    repo.config(&json!({"repository_extensions": [{"path": "pkg"}]}));
    let item = repo.items().remove(0);
    assert_eq!(rels(&item), ["alias", "extension.json", "init.lua"]);
    assert_eq!(item.outside, ["leak"]);
}

#[test]
fn a_link_loop_fails_the_item_naming_it() {
    let repo = Repo::new();
    repo.link("scripts/loop", Path::new("loop"));
    repo.config(&server("scripts/loop", &json!([])));
    let e = declared_items(&repo.root()).unwrap_err();
    assert!(matches!(&e, Error::Pin { item, .. } if item == "db"), "{e}");
    assert_eq!(e.code(), ErrorCode::IoFailed);
}

#[test]
fn only_the_repository_layer_declares_a_server() {
    let repo = Repo::new();
    fs::write(
        repo.home().join("config.json"),
        r#"{"mcp": {"servers": {"mine": {"command": "x"}}}}"#,
    )
    .unwrap();
    assert!(repo.items().is_empty());
}

#[test]
fn an_extension_path_that_cannot_be_resolved_for_another_reason_is_not_reported_missing() {
    let repo = Repo::new();
    repo.link("loop", Path::new("loop"));
    repo.config(&json!({"repository_extensions": [{"path": "loop"}]}));
    let e = declared_items(&repo.root()).unwrap_err();
    assert!(matches!(e, Error::Io { .. }), "{e}");
}

#[test]
fn a_git_that_cannot_start_is_missing_only_when_it_is_not_found() {
    use std::io::{Error as IoError, ErrorKind};
    let dir = Path::new("/x");
    assert!(matches!(
        super::declared::spawn_error(dir, IoError::from(ErrorKind::NotFound)),
        Error::GitMissing
    ));
    assert!(matches!(
        super::declared::spawn_error(dir, IoError::from(ErrorKind::PermissionDenied)),
        Error::Io { .. }
    ));
}

#[test]
fn a_path_that_cannot_be_examined_fails_the_item_and_a_missing_one_is_skipped() {
    let repo = Repo::new();
    repo.write("locked/run.sh", "x");
    repo.write("dir/inner", "x");
    let locked = repo.root().join("locked");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    repo.config(&server("locked/run.sh", &json!([])));
    let denied = declared_items(&repo.root());
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    let e = denied.unwrap_err();
    assert!(matches!(&e, Error::Pin { item, .. } if item == "db"), "{e}");
    repo.config(&server("missing.sh", &json!(["dir"])));
    assert!(rels(&repo.item(OfferedKind::McpServer, "db")).is_empty());
}
