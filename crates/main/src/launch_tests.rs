//! Tests for the launch description: pure, over invented identity paths
//! in a temporary directory, so no test runs a child process.

use super::launch;
use config::{Config, Sources};

/// Loads the configuration for `home` and `workspace`, with `overrides`
/// as `-c key=value` reads them.
fn config(home: &std::path::Path, workspace: &std::path::Path, overrides: Vec<String>) -> Config {
    let project = config::ProjectKey::new("-w").unwrap_or_else(|err| panic!("key: {err}"));
    Config::load(Sources {
        home: home.to_path_buf(),
        workspace: workspace.to_path_buf(),
        project,
        overrides,
    })
    .unwrap_or_else(|err| panic!("config: {err}"))
}

#[test]
fn an_identity_equal_to_the_workspace_is_not_git() {
    let dir = fakes::TempDir::new("fiber-launch");
    let workspace = dir.path().to_path_buf();
    // The identity path is canonical, as `doors::project` makes it.
    let identity = workspace
        .canonicalize()
        .unwrap_or_else(|err| panic!("canonical: {err}"));
    let config = config(dir.path(), &workspace, Vec::new());
    let launch = launch(
        workspace.clone(),
        &identity,
        &config,
        tui::ThemeSetting::Follow,
        dir.path(),
    );
    assert_eq!(launch.workspace, workspace);
    assert_eq!(launch.project, log::project_key(&identity));
    assert!(!launch.git);
    assert!(launch.hover);
    assert_eq!(launch.version, env!("CARGO_PKG_VERSION"));
}

#[test]
fn an_identity_elsewhere_is_git_and_names_the_project() {
    let dir = fakes::TempDir::new("fiber-launch-git");
    let workspace = dir.path().join("repo");
    std::fs::create_dir_all(workspace.join(".git")).unwrap_or_else(|err| panic!("mkdir: {err}"));
    let identity = workspace.join(".git");
    let config = config(dir.path(), &workspace, Vec::new());
    let launch = launch(
        workspace.clone(),
        &identity,
        &config,
        tui::ThemeSetting::Follow,
        dir.path(),
    );
    assert_eq!(launch.workspace, workspace);
    assert_eq!(launch.project, log::project_key(&identity));
    assert!(launch.git);
}

#[test]
fn model_thinking_and_logo_glyph_come_from_config() {
    let dir = fakes::TempDir::new("fiber-launch-model");
    let workspace = dir.path().to_path_buf();
    let identity = workspace
        .canonicalize()
        .unwrap_or_else(|err| panic!("canonical: {err}"));
    let set_config = config(
        dir.path(),
        &workspace,
        vec![
            "model=openai/gpt-5".to_owned(),
            "thinking=low".to_owned(),
            "tui.logo_glyph=≈".to_owned(),
        ],
    );
    let set = launch(
        workspace.clone(),
        &identity,
        &set_config,
        tui::ThemeSetting::Follow,
        dir.path(),
    );
    assert_eq!(set.model.as_deref(), Some("openai/gpt-5"));
    assert_eq!(set.thinking.as_deref(), Some("low"));
    assert_eq!(set.logo_glyph, "≈");
    // Unset, the chips show their defaults and the logo its wave.
    let plain_config = config(dir.path(), &workspace, Vec::new());
    let unset = launch(
        workspace,
        &identity,
        &plain_config,
        tui::ThemeSetting::Follow,
        dir.path(),
    );
    assert_eq!(unset.model, None);
    assert_eq!(unset.thinking, None);
    assert_eq!(unset.logo_glyph, "⌇");
}

#[test]
fn tui_hover_defaults_to_on_and_reads_off() {
    let dir = fakes::TempDir::new("fiber-launch-hover");
    let workspace = dir.path().to_path_buf();
    let on = config(dir.path(), &workspace, Vec::new());
    assert!(
        launch(
            workspace.clone(),
            &workspace,
            &on,
            tui::ThemeSetting::Follow,
            dir.path(),
        )
        .hover
    );
    let off = config(dir.path(), &workspace, vec!["tui.hover=false".to_owned()]);
    assert!(
        !launch(
            workspace.clone(),
            &workspace,
            &off,
            tui::ThemeSetting::Follow,
            dir.path(),
        )
        .hover
    );
}

#[test]
fn reduced_motion_comes_from_config_default_off() {
    let dir = fakes::TempDir::new("fiber-launch-reduced");
    let workspace = dir.path().to_path_buf();
    let off = config(dir.path(), &workspace, Vec::new());
    assert!(
        !launch(
            workspace.clone(),
            &workspace,
            &off,
            tui::ThemeSetting::Follow,
            dir.path(),
        )
        .reduced_motion
    );
    let on = config(
        dir.path(),
        &workspace,
        vec!["tui.reduced_motion=true".to_owned()],
    );
    assert!(
        launch(
            workspace.clone(),
            &workspace,
            &on,
            tui::ThemeSetting::Follow,
            dir.path(),
        )
        .reduced_motion
    );
}

#[test]
fn shares_and_cards_come_from_config_with_defaults() {
    let dir = fakes::TempDir::new("fiber-launch-shares");
    let workspace = dir.path().to_path_buf();
    let plain = config(dir.path(), &workspace, Vec::new());
    let unset = launch(
        workspace.clone(),
        &workspace,
        &plain,
        tui::ThemeSetting::Follow,
        dir.path(),
    );
    assert_eq!((unset.rail_share, unset.panel_share), (15.0, 21.0));
    assert_eq!(
        unset.panel_cards,
        ["session", "changed_files", "delegates", "jobs", "quota"]
    );
    let set_config = config(
        dir.path(),
        &workspace,
        vec![
            "tui.rail.width=18.5".to_owned(),
            "tui.panel.width=30".to_owned(),
            r#"tui.panel.cards=["jobs", "session"]"#.to_owned(),
        ],
    );
    let set = launch(
        workspace.clone(),
        &workspace,
        &set_config,
        tui::ThemeSetting::Follow,
        dir.path(),
    );
    assert_eq!((set.rail_share, set.panel_share), (18.5, 30.0));
    assert_eq!(set.panel_cards, ["jobs", "session"]);
}

#[test]
fn an_out_of_range_share_reads_as_the_default() {
    let dir = fakes::TempDir::new("fiber-launch-range");
    let workspace = dir.path().to_path_buf();
    for (rail, panel, expected) in [
        ("-1", "101", (15.0, 21.0)),
        ("0", "100", (0.0, 100.0)),
        ("100.5", "-0.5", (15.0, 21.0)),
    ] {
        let set_config = config(
            dir.path(),
            &workspace,
            vec![
                format!("tui.rail.width={rail}"),
                format!("tui.panel.width={panel}"),
            ],
        );
        let set = launch(
            workspace.clone(),
            &workspace,
            &set_config,
            tui::ThemeSetting::Follow,
            dir.path(),
        );
        assert_eq!(
            (set.rail_share, set.panel_share),
            expected,
            "{rail} {panel}"
        );
    }
}

#[test]
fn without_keys_anywhere_launch_keys_user_is_empty() {
    let dir = fakes::TempDir::new("fiber-launch-keys-empty");
    let workspace = dir.path().to_path_buf();
    let plain = config(dir.path(), &workspace, Vec::new());
    assert!(
        launch(
            workspace.clone(),
            &workspace,
            &plain,
            tui::ThemeSetting::Follow,
            dir.path(),
        )
        .keys
        .user
        .is_empty()
    );
}

#[test]
fn global_and_project_keys_merge_as_written() {
    let dir = fakes::TempDir::new("fiber-launch-keys");
    let workspace = dir.path().to_path_buf();
    std::fs::write(
        dir.path().join("config.json"),
        r#"{"keys": {"send": "ctrl+s"}}"#,
    )
    .unwrap_or_else(|err| panic!("global keys: {err}"));
    let project_dir = dir.path().join("projects").join("-w");
    std::fs::create_dir_all(&project_dir).unwrap_or_else(|err| panic!("mkdir: {err}"));
    std::fs::write(
        project_dir.join("config.json"),
        r#"{"keys": {"copy_focused": ["c"]}}"#,
    )
    .unwrap_or_else(|err| panic!("project keys: {err}"));
    let config = config(dir.path(), &workspace, Vec::new());
    let user = launch(
        workspace.clone(),
        &workspace,
        &config,
        tui::ThemeSetting::Follow,
        dir.path(),
    )
    .keys
    .user;
    assert_eq!(
        user,
        serde_json::json!({"send": "ctrl+s", "copy_focused": ["c"]})
            .as_object()
            .cloned()
            .unwrap_or_default()
    );
}

#[test]
fn the_theme_setting_is_passed_through() {
    let dir = fakes::TempDir::new("fiber-launch-theme");
    let workspace = dir.path().to_path_buf();
    let plain = config(dir.path(), &workspace, Vec::new());
    let setting = tui::ThemeSetting::File {
        name: "solar".to_owned(),
        text: Ok("{}".to_owned()),
    };
    let tui::ThemeSetting::File { name, text } =
        launch(workspace.clone(), &workspace, &plain, setting, dir.path()).theme
    else {
        panic!("not a theme file");
    };
    assert_eq!(name, "solar");
    assert_eq!(text, Ok("{}".to_owned()));
    let light = launch(
        workspace.clone(),
        &workspace,
        &plain,
        tui::ThemeSetting::Light,
        dir.path(),
    );
    assert!(matches!(light.theme, tui::ThemeSetting::Light));
}

#[test]
fn attention_switches_come_from_config_with_defaults() {
    let dir = fakes::TempDir::new("fiber-launch-attention");
    let workspace = dir.path().to_path_buf();
    let plain = config(dir.path(), &workspace, Vec::new());
    let unset = launch(
        workspace.clone(),
        &workspace,
        &plain,
        tui::ThemeSetting::default(),
        dir.path(),
    );
    assert!(unset.attention.notification);
    assert!(unset.attention.bell);
    assert!(unset.attention.title);
    for (key, check) in [
        ("tui.attention.notification=false", (false, true, true)),
        ("tui.attention.bell=false", (true, false, true)),
        ("tui.attention.title=false", (true, true, false)),
    ] {
        let set_config = config(dir.path(), &workspace, vec![key.to_owned()]);
        let set = launch(
            workspace.clone(),
            &workspace,
            &set_config,
            tui::ThemeSetting::default(),
            dir.path(),
        );
        assert_eq!(
            (
                set.attention.notification,
                set.attention.bell,
                set.attention.title
            ),
            check,
            "{key}"
        );
    }
}

#[test]
fn scoped_models_come_from_config() {
    let dir = fakes::TempDir::new("fiber-launch-scoped");
    let workspace = dir.path().to_path_buf();
    let identity = workspace
        .canonicalize()
        .unwrap_or_else(|err| panic!("canonical: {err}"));
    let set_config = config(
        dir.path(),
        &workspace,
        vec!["scoped_models=[\"openai/gpt-5\"]".to_owned()],
    );
    let set = launch(
        workspace.clone(),
        &identity,
        &set_config,
        tui::ThemeSetting::Follow,
        dir.path(),
    );
    assert_eq!(set.scoped_models, ["openai/gpt-5"]);
    assert!(set.models.is_none());
    // Unset, the picker shows every installed model.
    let plain_config = config(dir.path(), &workspace, Vec::new());
    let unset = launch(
        workspace,
        &identity,
        &plain_config,
        tui::ThemeSetting::Follow,
        dir.path(),
    );
    assert!(unset.scoped_models.is_empty());
}

#[test]
fn images_dir_is_under_home_cache() {
    let dir = fakes::TempDir::new("fiber-launch-images");
    let workspace = dir.path().to_path_buf();
    let identity = workspace
        .canonicalize()
        .unwrap_or_else(|err| panic!("canonical: {err}"));
    let config = config(dir.path(), &workspace, Vec::new());
    let launch = launch(
        workspace,
        &identity,
        &config,
        tui::ThemeSetting::Follow,
        dir.path(),
    );
    assert_eq!(launch.images, dir.path().join("cache").join("images"));
}
