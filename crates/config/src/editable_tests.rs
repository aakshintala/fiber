//! Editable paths use the same builders as local configuration views.
use std::path::{Path, PathBuf};

use super::is_editable_config_path;
use crate::{Layer, ProjectKey, Source};

fn project() -> ProjectKey {
    ProjectKey::new("project".to_owned()).unwrap()
}

#[test]
fn every_configuration_and_rules_layer_is_editable() {
    let (home, workspace) = (Path::new("/fiber"), Path::new("/workspace"));
    let project = project();
    for path in [
        "/fiber/config.json",
        "/fiber/projects/project/config.json",
        "/workspace/.fiber/config.json",
        "/fiber/rules",
        "/fiber/projects/project/rules",
        "/any/skill/SKILL.md",
    ] {
        assert!(
            is_editable_config_path(home, workspace, &project, Path::new(path)),
            "{path}"
        );
    }
}

#[test]
fn configuration_and_rules_targets_agree_with_the_local_builders() {
    let (home, workspace) = (Path::new("/fiber"), Path::new("/workspace"));
    let project = project();
    for layer in [Layer::Global, Layer::Project, Layer::Repository] {
        let source = layer.source(home, workspace, &project);
        let path = match source {
            Source::Global(path) | Source::Project(path) | Source::Repository(path) => path,
            Source::Default | Source::Run => panic!("a file layer must name a file"),
        };
        assert!(is_editable_config_path(home, workspace, &project, &path));
    }
    let (global, project_file) = crate::rules::files(home, &project);
    for path in [global, project_file] {
        assert!(is_editable_config_path(home, workspace, &project, &path));
    }
}

#[test]
fn unrelated_relative_and_other_projects_paths_are_not_editable() {
    let (home, workspace) = (Path::new("/fiber"), Path::new("/workspace"));
    let project = project();
    for path in [
        "/fiber/credentials/name/default",
        "/workspace/.fiber/other.json",
        "/fiber/projects/other/config.json",
        "/fiber/projects/other/rules",
        "/other/config.json",
        "/other/rules",
        "/workspace/.fiber/rules",
        "/fiber/nested/../config.json",
        "/fiber/../fiber/rules",
        "/any/SKILL.md.bak",
        "/any/skill.md",
        "SKILL.md",
        "config.json",
        "",
    ] {
        assert!(
            !is_editable_config_path(home, workspace, &project, Path::new(path)),
            "{path}"
        );
    }
    for path in ["/fiber/config.json", "/any/SKILL.md"] {
        assert!(!is_editable_config_path(
            home,
            Path::new("relative"),
            &project,
            Path::new(path)
        ));
    }
}

#[test]
fn paths_are_compared_as_paths_without_utf8_conversion_or_disk_access() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    let home = PathBuf::from(OsString::from_vec(b"/fiber-\xff".to_vec()));
    let workspace = PathBuf::from(OsString::from_vec(b"/workspace-\xfe".to_vec()));
    let project = project();
    assert!(is_editable_config_path(
        &home,
        &workspace,
        &project,
        &home.join("config.json")
    ));
    assert!(is_editable_config_path(
        &home,
        &workspace,
        &project,
        &workspace.join(".fiber/config.json")
    ));
    assert!(is_editable_config_path(
        &home,
        &workspace,
        &project,
        &workspace.join("SKILL.md")
    ));
    assert!(!is_editable_config_path(
        &home,
        &workspace,
        &project,
        &workspace.join("other.json")
    ));
}
