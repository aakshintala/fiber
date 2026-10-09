//! Tests for the terminal's configuration seam: which project's layer it
//! reads and writes, the rows it builds, and the themes in Fiber home.

use std::fs;
use std::path::{Path, PathBuf};

use tui::{Configure, Layer, Shown, WriteScope};

use super::Seam;

/// Fiber home and two workspaces in one temporary directory.
struct Dirs {
    root: fakes::TempDir,
}

impl Dirs {
    fn new() -> Self {
        let root = fakes::TempDir::new("fiber-configure");
        for dir in ["home", "one", "two"] {
            fs::create_dir_all(root.path().join(dir)).unwrap_or_else(|e| panic!("mkdir: {e}"));
        }
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn workspace(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    /// The project's `config.json` for `workspace`.
    fn project_file(&self, workspace: &Path) -> PathBuf {
        let (_, project) = ::cli::project_of(&self.home(), workspace)
            .unwrap_or_else(|e| panic!("project: {}", e.message));
        self.home()
            .join("projects")
            .join(project.as_str())
            .join("config.json")
    }
}

/// Writes `text` to `file`, making its directory.
fn write(file: &Path, text: &str) {
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir).unwrap_or_else(|e| panic!("mkdir: {e}"));
    }
    fs::write(file, text).unwrap_or_else(|e| panic!("write: {e}"));
}

/// The row for `key` in `seam`'s rows for `workspace`.
fn row(seam: &Seam, workspace: &Path, key: &str) -> tui::SettingRow {
    seam.settings(workspace)
        .unwrap_or_else(|e| panic!("settings: {e}"))
        .into_iter()
        .find(|row| row.key == key)
        .unwrap_or_else(|| panic!("no row {key}"))
}

#[test]
fn settings_reads_the_session_workspaces_project_layer() {
    let dirs = Dirs::new();
    let (one, two) = (dirs.workspace("one"), dirs.workspace("two"));
    assert_ne!(dirs.project_file(&one), dirs.project_file(&two));
    write(&dirs.project_file(&one), r#"{"handoff": {"tokens": 1}}"#);
    write(&dirs.project_file(&two), r#"{"handoff": {"tokens": 2}}"#);
    let seam = Seam::new(dirs.home());
    let first = row(&seam, &one, "handoff.tokens");
    assert_eq!(first.value, Shown::Value("1".to_owned()));
    assert_eq!(first.layer, "project");
    assert_eq!(first.file, Some(dirs.project_file(&one)));
    let second = row(&seam, &two, "handoff.tokens");
    assert_eq!(second.value, Shown::Value("2".to_owned()));
    assert_eq!(second.scope, WriteScope::Any { repo: true });
}

#[test]
fn a_default_row_names_no_file() {
    let dirs = Dirs::new();
    let seam = Seam::new(dirs.home());
    let tokens = row(&seam, &dirs.workspace("one"), "handoff.tokens");
    assert_eq!(
        (tokens.value, tokens.layer.as_str(), tokens.file),
        (Shown::Value("400000".to_owned()), "default", None)
    );
    let model = row(&seam, &dirs.workspace("one"), "model");
    assert_eq!(model.value, Shown::Unset);
}

#[test]
fn skills_disabled_carries_each_layers_own_list() {
    let dirs = Dirs::new();
    let one = dirs.workspace("one");
    write(
        &dirs.home().join("config.json"),
        r#"{"skills": {"disabled": ["a"]}}"#,
    );
    write(
        &dirs.project_file(&one),
        r#"{"skills": {"disabled": ["b"]}}"#,
    );
    let seam = Seam::new(dirs.home());
    let skills = row(&seam, &one, "skills.disabled");
    assert_eq!(skills.layer, "global + project");
    assert_eq!(
        skills.value,
        Shown::Union {
            names: vec![
                ("a".to_owned(), "global".to_owned()),
                ("b".to_owned(), "project".to_owned()),
            ],
            own: vec![
                (Layer::Global, r#"["a"]"#.to_owned()),
                (Layer::Project, r#"["b"]"#.to_owned()),
            ],
        }
    );
}

#[test]
fn skills_disabled_with_the_same_name_in_two_layers_shows_both() {
    let dirs = Dirs::new();
    let one = dirs.workspace("one");
    write(
        &dirs.home().join("config.json"),
        r#"{"skills": {"disabled": ["a"]}}"#,
    );
    write(
        &dirs.project_file(&one),
        r#"{"skills": {"disabled": ["a"]}}"#,
    );
    let seam = Seam::new(dirs.home());
    let skills = row(&seam, &one, "skills.disabled");
    // Each layer's own list decides the row's layers, not the
    // deduplicated names: both layers list `a`.
    assert_eq!(skills.layer, "global + project");
    assert_eq!(
        skills.value,
        Shown::Union {
            names: vec![("a".to_owned(), "global".to_owned())],
            own: vec![
                (Layer::Global, r#"["a"]"#.to_owned()),
                (Layer::Project, r#"["a"]"#.to_owned()),
            ],
        }
    );
}

#[test]
fn set_writes_the_layers_file_as_config_set_does() {
    let dirs = Dirs::new();
    let one = dirs.workspace("one");
    let seam = Seam::new(dirs.home());
    let saved = seam
        .set(&one, Layer::Project, "handoff.tokens", "200000")
        .unwrap_or_else(|e| panic!("set: {e}"));
    assert_eq!(saved.file, dirs.project_file(&one));
    assert!(saved.warnings.is_empty());
    let written: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(dirs.project_file(&one)).unwrap_or_else(|e| panic!("read: {e}")),
    )
    .unwrap_or_else(|e| panic!("json: {e}"));
    assert_eq!(written, serde_json::json!({"handoff": {"tokens": 200000}}));
    let saved = seam
        .set(&one, Layer::Global, "tui.theme", "\"light\"")
        .unwrap_or_else(|e| panic!("set: {e}"));
    assert_eq!(saved.file, dirs.home().join("config.json"));
    let repo = seam
        .set(&one, Layer::Repository, "handoff.tokens", "5")
        .unwrap_or_else(|e| panic!("set: {e}"));
    assert_eq!(repo.file, one.join(".fiber").join("config.json"));
}

#[test]
fn a_refused_write_writes_nothing_and_says_why() {
    let dirs = Dirs::new();
    let one = dirs.workspace("one");
    let seam = Seam::new(dirs.home());
    let error = seam
        .set(&one, Layer::Repository, "tui.hover", "false")
        .err()
        .unwrap_or_else(|| panic!("a repository may not set tui.hover"));
    assert_eq!(error.code, contract::ErrorCode::Usage);
    assert!(error.message.contains("tui.hover"), "{}", error.message);
    assert!(!one.join(".fiber").join("config.json").exists());
}

#[test]
fn themes_lists_json_files_by_name_sorted() {
    let dirs = Dirs::new();
    let themes = dirs.home().join("themes");
    for name in ["solar.json", "dusk.json", ".hidden.json", "notes.txt"] {
        write(&themes.join(name), "{}");
    }
    fs::create_dir_all(themes.join("dir.json")).unwrap_or_else(|e| panic!("mkdir: {e}"));
    let seam = Seam::new(dirs.home());
    assert_eq!(seam.themes(), ["dusk", "solar"]);
    assert!(Seam::new(dirs.root.path().join("none")).themes().is_empty());
}

#[test]
fn theme_builds_the_setting_as_at_start() {
    let dirs = Dirs::new();
    write(&dirs.home().join("themes").join("solar.json"), "{\"x\": 1}");
    let seam = Seam::new(dirs.home());
    assert!(matches!(seam.theme("auto"), tui::ThemeSetting::Follow));
    assert!(matches!(seam.theme("dark"), tui::ThemeSetting::Dark));
    assert!(matches!(seam.theme("light"), tui::ThemeSetting::Light));
    let tui::ThemeSetting::File { name, text } = seam.theme("solar") else {
        panic!("not a file");
    };
    assert_eq!(
        (name.as_str(), text),
        ("solar", Ok("{\"x\": 1}".to_owned()))
    );
    let tui::ThemeSetting::File { text, .. } = seam.theme("missing") else {
        panic!("not a file");
    };
    assert!(text.is_err());
    let tui::ThemeSetting::File { text, .. } = seam.theme("../x") else {
        panic!("not a file");
    };
    assert_eq!(text, Err("not a theme name".to_owned()));
}

#[test]
fn the_global_file_is_fiber_homes_config() {
    let dirs = Dirs::new();
    assert_eq!(
        Seam::new(dirs.home()).global_file(),
        dirs.home().join("config.json")
    );
}

/// One allow line for `prefix`.
fn allow_line(prefix: &str) -> String {
    format!("{{\"decision\":\"allow\",\"tool\":\"shell\",\"prefix\":\"{prefix}\"}}")
}

/// The project's rules file for `workspace`.
fn rules_file(dirs: &Dirs, workspace: &Path) -> PathBuf {
    let (_, project) = ::cli::project_of(&dirs.home(), workspace)
        .unwrap_or_else(|e| panic!("project: {}", e.message));
    dirs.home()
        .join("projects")
        .join(project.as_str())
        .join("rules")
}

#[test]
fn rules_reads_the_session_workspaces_project_file() {
    let dirs = Dirs::new();
    let (one, two) = (dirs.workspace("one"), dirs.workspace("two"));
    assert_ne!(rules_file(&dirs, &one), rules_file(&dirs, &two));
    write(
        &dirs.home().join("rules"),
        &format!("{}\nbad\n", allow_line("g")),
    );
    write(
        &rules_file(&dirs, &one),
        &format!("{}\n", allow_line("one")),
    );
    write(
        &rules_file(&dirs, &two),
        &format!("{}\n", allow_line("two")),
    );
    let seam = Seam::new(dirs.home());
    let (global, project) = seam.rules(&one).unwrap_or_else(|e| panic!("rules: {e}"));
    assert_eq!(global.file, dirs.home().join("rules"));
    let message = global.rows.unwrap_err();
    assert!(
        message.contains(&format!("{}:2:", dirs.home().join("rules").display())),
        "{message}"
    );
    assert_eq!(project.file, rules_file(&dirs, &one));
    let rows = project.rows.unwrap_or_else(|e| panic!("rows: {e}"));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].rule.prefix, "one");
    assert_eq!(rows[0].line, 1);
    let (global, project) = seam.rules(&two).unwrap_or_else(|e| panic!("rules: {e}"));
    assert_eq!(global.file, dirs.home().join("rules"));
    assert_eq!(project.file, rules_file(&dirs, &two));
    let rows = project.rows.unwrap_or_else(|e| panic!("rows: {e}"));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].rule.prefix, "two");
}

#[test]
fn revoke_then_the_next_read_lacks_the_rule() {
    let dirs = Dirs::new();
    let one = dirs.workspace("one");
    let line = allow_line("npm test");
    write(&rules_file(&dirs, &one), &format!("{line}\n"));
    let seam = Seam::new(dirs.home());
    assert_eq!(
        seam.revoke(&one, tui::RulesScope::Project, 1, &line)
            .unwrap_or_else(|e| panic!("revoke: {e}")),
        tui::Revoked::Removed
    );
    let (_, project) = seam.rules(&one).unwrap_or_else(|e| panic!("rules: {e}"));
    assert!(
        project
            .rows
            .unwrap_or_else(|e| panic!("rows: {e}"))
            .is_empty()
    );
}

#[test]
fn a_stale_revoke_writes_nothing() {
    let dirs = Dirs::new();
    let one = dirs.workspace("one");
    let line = allow_line("npm test");
    let file = rules_file(&dirs, &one);
    write(&file, &format!("{line}\n"));
    let before = fs::read(&file).unwrap_or_else(|e| panic!("read: {e}"));
    let seam = Seam::new(dirs.home());
    assert_eq!(
        seam.revoke(&one, tui::RulesScope::Project, 1, &allow_line("other"))
            .unwrap_or_else(|e| panic!("revoke: {e}")),
        tui::Revoked::Stale
    );
    assert_eq!(
        fs::read(&file).unwrap_or_else(|e| panic!("read: {e}")),
        before
    );
}

#[test]
fn revoke_maps_global_and_project_to_their_files() {
    let dirs = Dirs::new();
    let one = dirs.workspace("one");
    let line = allow_line("same");
    write(&dirs.home().join("rules"), &format!("{line}\n"));
    write(&rules_file(&dirs, &one), &format!("{line}\n"));
    let seam = Seam::new(dirs.home());
    assert_eq!(
        seam.revoke(&one, tui::RulesScope::Global, 1, &line)
            .unwrap_or_else(|e| panic!("revoke: {e}")),
        tui::Revoked::Removed
    );
    assert!(
        fs::read(dirs.home().join("rules"))
            .unwrap_or_else(|e| panic!("read: {e}"))
            .is_empty()
    );
    assert_eq!(
        fs::read_to_string(rules_file(&dirs, &one)).unwrap_or_else(|e| panic!("read: {e}")),
        format!("{line}\n")
    );
    assert_eq!(
        seam.revoke(&one, tui::RulesScope::Project, 1, &line)
            .unwrap_or_else(|e| panic!("revoke: {e}")),
        tui::Revoked::Removed
    );
    assert!(
        fs::read(rules_file(&dirs, &one))
            .unwrap_or_else(|e| panic!("read: {e}"))
            .is_empty()
    );
}
