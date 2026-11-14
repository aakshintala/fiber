//! Tests for the terminal's configuration seam: which project's layer it
//! reads and writes, the rows it builds, and the themes in Fiber home and
//! installed extensions.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use contract::settings::{SettingControl, SettingsTab};
use tui::{Configure, Layer, Shown, WriteScope};

use super::Seam;
use fakes::Deadline;

/// Fiber home and two workspaces in one temporary directory.
pub(in crate::configure) struct Dirs {
    root: fakes::TempDir,
}

impl Dirs {
    pub(in crate::configure) fn new() -> Self {
        let root = fakes::TempDir::new("fiber-configure");
        for dir in ["home", "one", "two"] {
            fs::create_dir_all(root.path().join(dir)).unwrap_or_else(|e| panic!("mkdir: {e}"));
        }
        Self { root }
    }

    pub(in crate::configure) fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    pub(in crate::configure) fn workspace(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    /// The project's `config.json` for `workspace`.
    pub(in crate::configure) fn project_file(&self, workspace: &Path) -> PathBuf {
        let (_, project) =
            ::cli::project_of(&self.home(), workspace).unwrap_or_else(|e| panic!("project: {e}"));
        self.home()
            .join("projects")
            .join(project.as_str())
            .join("config.json")
    }
}

/// Writes `text` to `file`, making its directory.
pub(in crate::configure) fn write(file: &Path, text: &str) {
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir).unwrap_or_else(|e| panic!("mkdir: {e}"));
    }
    fs::write(file, text).unwrap_or_else(|e| panic!("write: {e}"));
}

/// The row for `key` in `seam`'s rows for `workspace`.
fn row(seam: &Seam, workspace: &Path, key: &str) -> tui::SettingRow {
    row_with(seam, workspace, &[], key)
}

/// The same with the run's `-c` overrides.
fn row_with(seam: &Seam, workspace: &Path, overrides: &[&str], key: &str) -> tui::SettingRow {
    let overrides: Vec<String> = overrides.iter().map(|arg| (*arg).to_owned()).collect();
    seam.settings(workspace, &overrides)
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
    write(&dirs.project_file(&one), r#"{"compaction": {"tokens": 1}}"#);
    write(&dirs.project_file(&two), r#"{"compaction": {"tokens": 2}}"#);
    let seam = Seam::new(dirs.home());
    let first = row(&seam, &one, "compaction.tokens");
    assert_eq!(first.value, Shown::Value("1".to_owned()));
    assert_eq!(first.layer, "project");
    assert_eq!(first.file, Some(dirs.project_file(&one)));
    let second = row(&seam, &two, "compaction.tokens");
    assert_eq!(second.value, Shown::Value("2".to_owned()));
    assert_eq!(second.scope, WriteScope::Any { repo: true });
}

#[test]
fn settings_omit_repository_scope_when_workspace_fiber_is_home() {
    let dirs = Dirs::new();
    let workspace = dirs.workspace("one");
    let home = workspace.join(".fiber");
    fs::create_dir_all(&home).unwrap_or_else(|e| panic!("mkdir: {e}"));
    let seam = Seam::new(home);
    assert_eq!(
        row(&seam, &workspace, "compaction.tokens").scope,
        WriteScope::Any { repo: false }
    );
    // Union settings declare Any without repository writes. With both
    // operands false, repository scope must remain unavailable.
    assert_eq!(
        row(&seam, &workspace, "skills.disabled").scope,
        WriteScope::Any { repo: false }
    );
    assert_eq!(
        row(&seam, &dirs.workspace("two"), "compaction.tokens").scope,
        WriteScope::Any { repo: true }
    );
}

#[test]
fn settings_carry_the_schemas_presentation_and_control() {
    let dirs = Dirs::new();
    let seam = Seam::new(dirs.home());
    let one = dirs.workspace("one");
    let thinking = row(&seam, &one, "thinking");
    assert_eq!(thinking.presentation.tab, Some(SettingsTab::Models));
    assert_eq!(thinking.presentation.label, "Thinking level");
    assert!(!thinking.presentation.description.is_empty());
    assert_eq!(
        thinking.control,
        SettingControl::Fixed(
            contract::ThinkingLevel::NAMES
                .iter()
                .map(|name| (*name).to_owned())
                .collect()
        )
    );
    assert_eq!(
        row(&seam, &one, "reviewer.model").control,
        SettingControl::Model
    );
    assert_eq!(row(&seam, &one, "tui.theme").control, SettingControl::Theme);
    assert_eq!(
        row(&seam, &one, "repository_extensions").control,
        SettingControl::RepositoryExtensions
    );
}

#[test]
fn settings_read_the_runs_overrides_in_order() {
    let dirs = Dirs::new();
    let one = dirs.workspace("one");
    write(
        &dirs.home().join("config.json"),
        r#"{"thinking": "minimal"}"#,
    );
    let seam = Seam::new(dirs.home());
    // Without overrides the global file shows; with them the last wins and
    // names `-c`, which has no file.
    assert_eq!(row(&seam, &one, "thinking").layer, "global");
    let low = row_with(&seam, &one, &["thinking=high", "thinking=low"], "thinking");
    assert_eq!(
        (low.value, low.layer.as_str(), low.file),
        (Shown::Value("\"low\"".to_owned()), "-c", None)
    );
    // A named-model override first and a `-c` for another key after it.
    let model = row_with(
        &seam,
        &one,
        &["model=a/b", "thinking=medium", "model=c/d"],
        "model",
    );
    assert_eq!(model.value, Shown::Value("\"c/d\"".to_owned()));
    assert_eq!(model.layer, "-c");
    // A later read without them is back to the file: nothing is cached.
    assert_eq!(row(&seam, &one, "thinking").layer, "global");
}

#[test]
fn reset_removes_one_layers_override_and_reports_stale_and_absent() {
    let dirs = Dirs::new();
    let one = dirs.workspace("one");
    let global = dirs.home().join("config.json");
    write(&global, r#"{"thinking": "low", "model": "a/b"}"#);
    let seam = Seam::new(dirs.home());
    // A changed value is stale and writes nothing.
    let stale = seam
        .reset(&one, Layer::Global, "thinking", "\"high\"")
        .unwrap_err();
    assert_eq!(stale.code, contract::ErrorCode::StaleRequest);
    assert!(fs::read_to_string(&global).unwrap().contains("low"));
    // A matching value is removed; the other key stays.
    let saved = seam
        .reset(&one, Layer::Global, "thinking", "\"low\"")
        .unwrap();
    assert_eq!(saved.file, global);
    let text = fs::read_to_string(&global).unwrap();
    assert!(!text.contains("thinking") && text.contains("a/b"), "{text}");
    // An absent override is a no-write success and creates no project file.
    let project = dirs.project_file(&one);
    let saved = seam
        .reset(&one, Layer::Project, "thinking", "\"low\"")
        .unwrap();
    assert_eq!(saved.file, project);
    assert!(!project.exists());
    // Expected text that is not JSON, and a key the layer may not hold.
    assert_eq!(
        seam.reset(&one, Layer::Global, "thinking", "low")
            .unwrap_err()
            .code,
        contract::ErrorCode::InvalidArguments
    );
    assert!(
        seam.reset(&one, Layer::Repository, "diagnostics.level", "\"debug\"")
            .is_err()
    );
}

#[test]
fn reset_resolves_the_project_afresh() {
    let dirs = Dirs::new();
    let one = dirs.workspace("one");
    let seam = Seam::new(dirs.home());
    // Prime the cached key, then make the workspace a repository: a set
    // and a reset both use the key it has now.
    drop(row(&seam, &one, "thinking"));
    fakes::git::git(&one, &["init", "-q"]);
    seam.set(&one, Layer::Project, "thinking", "low").unwrap();
    let fresh = dirs.project_file(&one);
    assert!(fresh.exists());
    seam.reset(&one, Layer::Project, "thinking", "\"low\"")
        .unwrap();
    assert!(!fs::read_to_string(&fresh).unwrap().contains("thinking"));
}

#[test]
fn a_default_row_names_no_file() {
    let dirs = Dirs::new();
    let seam = Seam::new(dirs.home());
    let tokens = row(&seam, &dirs.workspace("one"), "compaction.tokens");
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
        .set(&one, Layer::Project, "compaction.tokens", "200000")
        .unwrap_or_else(|e| panic!("set: {e}"));
    assert_eq!(saved.file, dirs.project_file(&one));
    assert!(saved.warnings.is_empty());
    let written: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(dirs.project_file(&one)).unwrap_or_else(|e| panic!("read: {e}")),
    )
    .unwrap_or_else(|e| panic!("json: {e}"));
    assert_eq!(
        written,
        serde_json::json!({"compaction": {"tokens": 200000}})
    );
    let saved = seam
        .set(&one, Layer::Global, "tui.theme", "\"light\"")
        .unwrap_or_else(|e| panic!("set: {e}"));
    assert_eq!(saved.file, dirs.home().join("config.json"));
    let repo = seam
        .set(&one, Layer::Repository, "compaction.tokens", "5")
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

/// Reads the global `config.json` under `home`.
fn global(home: &Path) -> serde_json::Value {
    let text = fs::read_to_string(home.join("config.json")).unwrap_or_else(|e| panic!("read: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("json: {e}"))
}

#[test]
fn drag_widths_write_the_global_file() {
    let dirs = Dirs::new();
    let one = dirs.workspace("one");
    let seam = Seam::new(dirs.home());
    let saved = seam
        .set(&one, Layer::Global, "tui.rail.width", "18.4")
        .unwrap_or_else(|e| panic!("set: {e}"));
    assert_eq!(saved.file, dirs.home().join("config.json"));
    assert_eq!(
        global(&dirs.home()),
        serde_json::json!({"tui": {"rail": {"width": 18.4}}})
    );
    let saved = seam
        .set(&one, Layer::Global, "tui.panel.width", "25")
        .unwrap_or_else(|e| panic!("set: {e}"));
    assert_eq!(saved.file, dirs.home().join("config.json"));
    assert_eq!(
        global(&dirs.home()),
        serde_json::json!({"tui": {"rail": {"width": 18.4}, "panel": {"width": 25}}})
    );
}

#[test]
fn a_width_that_cannot_write_is_an_error() {
    let root = fakes::TempDir::new("fiber-configure-width-fails");
    let home = root.path().join("file");
    write(&home, "not a directory");
    let workspace = root.path().join("one");
    fs::create_dir_all(&workspace).unwrap_or_else(|e| panic!("mkdir: {e}"));
    let seam = Seam::new(home);
    assert!(
        seam.set(&workspace, Layer::Global, "tui.rail.width", "18.4")
            .is_err()
    );
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
    let one = dirs.workspace("one");
    assert_eq!(seam.themes(&one), ["dusk", "solar"]);
    assert!(
        Seam::new(dirs.root.path().join("none"))
            .themes(&one)
            .is_empty()
    );
}

#[test]
fn themes_lists_package_themes_once() {
    let dirs = Dirs::new();
    write(&dirs.home().join("themes").join("solar.json"), "{}");
    let pkg = dirs.home().join("extensions").join("acme");
    write(
        &pkg.join(".fiber.json"),
        r#"{"name":"x","version":"1.0.0","requested":true,"source":{"path":"/p"}}"#,
    );
    write(&pkg.join("themes").join("solar.json"), "{}");
    write(&pkg.join("themes").join("tide.json"), "{}");
    let broken = dirs.home().join("extensions").join("broken");
    write(&broken.join("themes").join("gone.json"), "{}");
    let seam = Seam::new(dirs.home());
    assert_eq!(seam.themes(&dirs.workspace("one")), ["solar", "tide"]);
}

#[test]
fn themes_and_theme_follow_each_workspaces_enabled() {
    let dirs = Dirs::new();
    let (one, two) = (dirs.workspace("one"), dirs.workspace("two"));
    let pkg = dirs.home().join("extensions").join("acme");
    write(
        &pkg.join(".fiber.json"),
        r#"{"name":"acme","version":"1.0.0","requested":true,"source":{"path":"/p"}}"#,
    );
    write(&pkg.join("themes").join("dusk.json"), "acme");
    // Only one's project switches `acme` off.
    write(
        &dirs.project_file(&one),
        r#"{"extensions": {"acme": {"enabled": false}}}"#,
    );
    let seam = Seam::new(dirs.home());
    assert!(seam.themes(&one).is_empty());
    assert_eq!(seam.themes(&two), ["dusk"]);
    // A `tui.theme` naming a switched-off extension's theme reads as a
    // missing file: one notice, then the terminal follows its appearance.
    let tui::ThemeSetting::File { text, .. } = seam.theme(&one, "dusk") else {
        panic!("not a file");
    };
    assert!(text.is_err());
    let tui::ThemeSetting::File { text, .. } = seam.theme(&two, "dusk") else {
        panic!("not a file");
    };
    assert_eq!(text, Ok("acme".to_owned()));
}

#[test]
fn a_workspace_whose_config_fails_to_load_reads_home_themes_only() {
    let dirs = Dirs::new();
    let one = dirs.workspace("one");
    write(&dirs.home().join("themes").join("solar.json"), "{}");
    let pkg = dirs.home().join("extensions").join("acme");
    write(
        &pkg.join(".fiber.json"),
        r#"{"name":"acme","version":"1.0.0","requested":true,"source":{"path":"/p"}}"#,
    );
    write(&pkg.join("themes").join("dusk.json"), "acme");
    // The project file does not parse, so the workspace's configuration
    // fails to load and no extension's `enabled` is known.
    write(&dirs.project_file(&one), "not json");
    let seam = Seam::new(dirs.home());
    assert_eq!(seam.themes(&one), ["solar"]);
    // An extension-only name reads as absent: Fiber home alone is read.
    let tui::ThemeSetting::File { text, .. } = seam.theme(&one, "dusk") else {
        panic!("not a file");
    };
    assert!(text.is_err());
    let tui::ThemeSetting::File { text, .. } = seam.theme(&one, "solar") else {
        panic!("not a file");
    };
    assert_eq!(text, Ok("{}".to_owned()));
}

#[test]
fn theme_builds_the_setting_as_at_start() {
    let dirs = Dirs::new();
    write(&dirs.home().join("themes").join("solar.json"), "{\"x\": 1}");
    let seam = Seam::new(dirs.home());
    let one = dirs.workspace("one");
    assert!(matches!(
        seam.theme(&one, "auto"),
        tui::ThemeSetting::Follow
    ));
    assert!(matches!(seam.theme(&one, "dark"), tui::ThemeSetting::Dark));
    assert!(matches!(
        seam.theme(&one, "light"),
        tui::ThemeSetting::Light
    ));
    let tui::ThemeSetting::File { name, text } = seam.theme(&one, "solar") else {
        panic!("not a file");
    };
    assert_eq!(
        (name.as_str(), text),
        ("solar", Ok("{\"x\": 1}".to_owned()))
    );
    let tui::ThemeSetting::File { text, .. } = seam.theme(&one, "missing") else {
        panic!("not a file");
    };
    assert!(text.is_err());
    let tui::ThemeSetting::File { text, .. } = seam.theme(&one, "../x") else {
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
    let (_, project) =
        ::cli::project_of(&dirs.home(), workspace).unwrap_or_else(|e| panic!("project: {e}"));
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

#[test]
fn save_keys_writes_only_the_edits_to_the_global_keys() {
    use tui::KeyEdit;
    let dirs = Dirs::new();
    write(&dirs.home().join("config.json"), r#"{"model": "acme/m1"}"#);
    let seam = Seam::new(dirs.home());
    seam.save_keys(&[KeyEdit {
        id: "new_session".to_owned(),
        keys: Some(vec!["ctrl+t".to_owned()]),
    }])
    .unwrap_or_else(|e| panic!("save: {e}"));
    assert_eq!(
        fs::read_to_string(dirs.home().join("config.json")).unwrap_or_else(|e| panic!("read: {e}")),
        concat!(
            "{\n",
            "  \"keys\": {\n",
            "    \"new_session\": [\n",
            "      \"ctrl+t\"\n",
            "    ]\n",
            "  },\n",
            "  \"model\": \"acme/m1\"\n",
            "}\n"
        )
    );
}

#[test]
fn save_keys_with_no_entry_removes_it() {
    use tui::KeyEdit;
    let dirs = Dirs::new();
    write(
        &dirs.home().join("config.json"),
        r#"{"keys": {"new_session": ["ctrl+t"], "copy_focused": "y"}}"#,
    );
    let seam = Seam::new(dirs.home());
    seam.save_keys(&[KeyEdit {
        id: "new_session".to_owned(),
        keys: None,
    }])
    .unwrap_or_else(|e| panic!("save: {e}"));
    assert_eq!(
        fs::read_to_string(dirs.home().join("config.json")).unwrap_or_else(|e| panic!("read: {e}")),
        "{\n  \"keys\": {\n    \"copy_focused\": \"y\"\n  }\n}\n"
    );
}

#[test]
fn save_keys_refuses_a_keys_that_is_no_object_leaving_the_file() {
    use tui::KeyEdit;
    let dirs = Dirs::new();
    let text = r#"{"keys": "x"}"#;
    write(&dirs.home().join("config.json"), text);
    let seam = Seam::new(dirs.home());
    let error = match seam.save_keys(&[KeyEdit {
        id: "new_session".to_owned(),
        keys: Some(vec!["ctrl+t".to_owned()]),
    }]) {
        Ok(()) => panic!("a string is no object, but the save succeeded"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("keys"), "{error}");
    assert_eq!(
        fs::read_to_string(dirs.home().join("config.json")).unwrap_or_else(|e| panic!("read: {e}")),
        text
    );
}

#[test]
fn hubs_returns_sorted_configured_names_through_the_seam() {
    use tui::Configure;
    let dirs = Dirs::new();
    write(
        &dirs.home().join("config.json"),
        r#"{"hubs":{"zeta":{"address":"ws://z"},"alpha":{"address":"ws://a"}}}"#,
    );
    let seam = Seam::new(dirs.home());
    assert_eq!(seam.hubs().unwrap(), ["alpha", "zeta"]);
}

#[test]
fn search_backends_returns_sorted_installed_names_through_the_seam() {
    use tui::Configure;
    let dirs = Dirs::new();
    let extension = dirs.home().join("extensions/searches");
    crate::test_support::install_extension(
        &dirs.home(),
        "extensions/searches",
        serde_json::json!({"name":"searches","version":"v1.0.0","fiber":"0.1.0","api":extensions::API}),
        &[],
    );
    write(
        &extension.join("init.lua"),
        r#"fiber.search_backend("zebra", { timeout = 1000, run = function() return {} end })
fiber.search_backend("apple", { timeout = 1000, run = function() return {} end })"#,
    );
    let seam = Seam::new(dirs.home());
    assert_eq!(
        seam.search_backends(&dirs.workspace("one")).unwrap(),
        ["apple", "zebra"]
    );
}

#[test]
fn reviewer_notes_reads_each_layers_own_context_with_its_version() {
    use tui::Configure;
    let dirs = Dirs::new();
    let workspace = dirs.workspace("one");
    write(
        &dirs.home().join("config.json"),
        r#"{"reviewer": {"context": "Our org is acme."}}"#,
    );
    write(
        &dirs.project_file(&workspace),
        r#"{"reviewer": {"context": "Never touch infra/prod."}}"#,
    );
    let seam = Seam::new(dirs.home());
    let notes = seam
        .reviewer_notes(&workspace)
        .unwrap_or_else(|e| panic!("read: {e}"));
    assert_eq!(notes.global.file, dirs.home().join("config.json"));
    assert_eq!(notes.global.text, "Our org is acme.");
    assert_eq!(notes.project.file, dirs.project_file(&workspace));
    assert_eq!(notes.project.text, "Never touch infra/prod.");
    assert_ne!(notes.global.version, "absent");
    assert_ne!(notes.project.version, "absent");
    assert_ne!(notes.global.version, notes.project.version);
}

#[test]
fn save_reviewer_notes_writes_current_layers_and_refuses_stale_ones() {
    fakes::within(
        "seam notes save",
        Deadline::after(Duration::from_secs(10)),
        || {
            use tui::{Configure, FileOutcome, NotesSaved, NotesWrite, ReadOnlyEntry};
            let dirs = Dirs::new();
            let workspace = dirs.workspace("one");
            write(
                &dirs.home().join("config.json"),
                r#"{"model": "a/b", "reviewer": {"context": "old global"}}"#,
            );
            write(
                &dirs.project_file(&workspace),
                r#"{"reviewer": {"context": "old project"}}"#,
            );
            let seam = Seam::new(dirs.home());
            let read = seam
                .reviewer_notes(&workspace)
                .unwrap_or_else(|e| panic!("read: {e}"));
            let write = |version: &str, text: &str, entries: Vec<ReadOnlyEntry>| {
                Some(NotesWrite {
                    version: version.to_owned(),
                    text: text.to_owned(),
                    read_only: entries,
                })
            };
            let entry = ReadOnlyEntry {
                command: "acme-cli query".to_owned(),
                flags: vec!["--json".to_owned()],
            };
            let saved = seam
                .save_reviewer_notes(
                    &workspace,
                    write(&read.global.version, "new global", vec![entry]),
                    write(&read.project.version, "new project", Vec::new()),
                )
                .unwrap_or_else(|e| panic!("save: {e}"));
            let NotesSaved::Written(files) = saved else {
                panic!("a current save writes");
            };
            assert_eq!(files.len(), 2);
            assert_eq!(files[0].file, dirs.home().join("config.json"));
            assert_eq!(files[0].outcome, FileOutcome::Saved);
            assert_eq!(files[1].file, dirs.project_file(&workspace));
            assert_eq!(files[1].outcome, FileOutcome::Saved);
            let global_text = fs::read_to_string(dirs.home().join("config.json"))
                .unwrap_or_else(|e| panic!("read: {e}"));
            assert!(global_text.contains("new global"), "{global_text}");
            assert!(global_text.contains(r#""acme-cli query""#), "{global_text}");
            assert!(global_text.contains("--json"), "{global_text}");
            // The old global version no longer matches: nothing is written.
            let before = (
                fs::read(dirs.home().join("config.json")).unwrap_or_else(|e| panic!("read: {e}")),
                fs::read(dirs.project_file(&workspace)).unwrap_or_else(|e| panic!("read: {e}")),
            );
            let saved = seam
                .save_reviewer_notes(
                    &workspace,
                    write(&read.global.version, "lost global", Vec::new()),
                    write(&read.project.version, "lost project", Vec::new()),
                )
                .unwrap_or_else(|e| panic!("save: {e}"));
            assert_eq!(saved, NotesSaved::Stale);
            assert_eq!(
                fs::read(dirs.home().join("config.json")).unwrap_or_else(|e| panic!("read: {e}")),
                before.0
            );
            assert_eq!(
                fs::read(dirs.project_file(&workspace)).unwrap_or_else(|e| panic!("read: {e}")),
                before.1
            );
        },
    );
}

#[test]
fn project_key_resolves_once_per_workspace() {
    let dirs = Dirs::new();
    let workspace = dirs.workspace("one");
    write(
        &dirs.project_file(&workspace),
        r#"{"compaction": {"tokens": 1}}"#,
    );
    let seam = Seam::new(dirs.home());
    let first = seam
        .project_key(&workspace)
        .unwrap_or_else(|e| panic!("key: {}", e.message));
    let before = dirs.project_file(&workspace);
    fakes::git::git(workspace.parent().unwrap(), &["init", "--quiet"]);
    let after = dirs.project_file(&workspace);
    assert_ne!(
        before, after,
        "a git init moves the workspace's project, or the stale assertion is vacuous"
    );
    // The seam keeps the first key until restart: a stale key is intended.
    let second = seam
        .project_key(&workspace)
        .unwrap_or_else(|e| panic!("key: {}", e.message));
    assert_eq!(first, second);
    // The settings read goes through the same cached key.
    let tokens = row(&seam, &workspace, "compaction.tokens");
    assert_eq!(tokens.value, Shown::Value("1".to_owned()));
    assert_eq!(tokens.file, Some(before));
}
