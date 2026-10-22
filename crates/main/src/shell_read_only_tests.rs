//! The `shell.read_only` reader: the checked merged configuration's complete
//! string-array entries, name ordered (`docs/configuration.md`, "Keys").

use super::ShellReadOnly;
use super::{refresh_shell_read_only, shell_read_only};
use crate::test_support::Rig;
use fakes::Deadline;

fn read(rig: &Rig, overrides: &[&str]) -> Vec<(String, Vec<String>)> {
    let config = rig.config(overrides.iter().copied());
    shell_read_only(&config)
}

#[test]
fn an_absent_list_reads_empty() {
    let rig = Rig::new("fiber-shell-read-only-absent");
    assert_eq!(read(&rig, &[]), Vec::new());
}

#[test]
fn a_run_override_adds_an_entry() {
    let rig = Rig::new("fiber-shell-read-only-override");
    assert_eq!(
        read(&rig, &[r#"shell.read_only.jq.flags=["-r"]"#]),
        vec![("jq".to_owned(), vec!["-r".to_owned()])]
    );
}

#[test]
fn an_explicit_empty_array_is_an_entry_with_no_flags() {
    let rig = Rig::new("fiber-shell-read-only-empty");
    rig.write_global(&serde_json::json!({"shell": {"read_only": {"jq": {"flags": []}}}}));
    assert_eq!(read(&rig, &[]), vec![("jq".to_owned(), Vec::new())]);
}

#[test]
fn an_entry_without_flags_is_absent_not_empty() {
    let rig = Rig::new("fiber-shell-read-only-no-flags");
    rig.write_global(&serde_json::json!({"shell": {"read_only": {"jq": {}}}}));
    assert_eq!(read(&rig, &[]), Vec::new());
}

#[test]
fn a_run_override_replaces_a_global_commands_flags() {
    let rig = Rig::new("fiber-shell-read-only-replace");
    rig.write_global(&serde_json::json!({"shell": {"read_only": {"jq": {"flags": ["-r"]}}}}));
    assert_eq!(
        read(&rig, &[r#"shell.read_only.jq.flags=["-c"]"#]),
        vec![("jq".to_owned(), vec!["-c".to_owned()])]
    );
}

#[test]
fn entries_from_every_layer_appear_in_name_order() {
    let rig = Rig::new("fiber-shell-read-only-layers");
    rig.write_global(
        &serde_json::json!({"shell": {"read_only": {"yq": {"flags": []}, "jq": {"flags": ["-r"]}}}}),
    );
    assert_eq!(
        read(&rig, &[r#"shell.read_only.zq.flags=["-e"]"#]),
        vec![
            ("jq".to_owned(), vec!["-r".to_owned()]),
            ("yq".to_owned(), Vec::new()),
            ("zq".to_owned(), vec!["-e".to_owned()]),
        ]
    );
}

#[test]
fn a_project_layer_entry_is_read() {
    let rig = Rig::new("fiber-shell-read-only-project");
    std::fs::create_dir_all(rig.home.join("projects/test")).unwrap();
    std::fs::write(
        rig.home.join("projects/test/config.json"),
        r#"{"shell": {"read_only": {"jq": {"flags": ["-r"]}}}}"#,
    )
    .unwrap();
    assert_eq!(
        read(&rig, &[]),
        vec![("jq".to_owned(), vec!["-r".to_owned()])]
    );
}

#[test]
fn a_quoted_dotted_command_key_is_not_split() {
    let rig = Rig::new("fiber-shell-read-only-dotted");
    rig.write_global(&serde_json::json!({"shell": {"read_only": {"my.tool": {"flags": ["-r"]}}}}));
    assert_eq!(
        read(&rig, &[]),
        vec![("my.tool".to_owned(), vec!["-r".to_owned()])]
    );
}

fn load_error(rig: &Rig) -> config::ConfigError {
    config::Config::load(config::Sources {
        home: rig.home.clone(),
        workspace: rig.workspace.clone(),
        project: config::ProjectKey::new("test").unwrap(),
        overrides: Vec::new(),
    })
    .unwrap_err()
}

#[test]
fn a_non_array_flags_is_config_invalid() {
    let rig = Rig::new("fiber-shell-read-only-bad-shape");
    rig.write_global(&serde_json::json!({"shell": {"read_only": {"jq": {"flags": "-r"}}}}));
    let error = load_error(&rig);
    assert_eq!(error.code(), contract::ErrorCode::ConfigInvalid);
    assert!(
        error.to_string().contains("shell.read_only.jq.flags"),
        "{error}"
    );
}

#[test]
fn mixed_type_flags_are_config_invalid() {
    let rig = Rig::new("fiber-shell-read-only-mixed");
    rig.write_global(&serde_json::json!({"shell": {"read_only": {"jq": {"flags": ["-r", 3]}}}}));
    let error = load_error(&rig);
    assert_eq!(error.code(), contract::ErrorCode::ConfigInvalid);
    assert!(
        error.to_string().contains("shell.read_only.jq.flags"),
        "{error}"
    );
}

fn write_repository(rig: &Rig, text: &str) -> std::path::PathBuf {
    std::fs::create_dir_all(rig.workspace.join(".fiber")).unwrap();
    let path = rig.workspace.join(".fiber/config.json");
    std::fs::write(&path, text).unwrap();
    path
}

#[test]
fn a_repository_only_value_is_ignored_with_a_notice() {
    let rig = Rig::new("fiber-shell-read-only-repository");
    let path = write_repository(
        &rig,
        r#"{"shell": {"read_only": {"jq": {"flags": ["-r"]}}}}"#,
    );
    let config = rig.config(Vec::<String>::new());
    assert_eq!(shell_read_only(&config), Vec::new());
    assert!(
        config.notices().iter().any(|notice| {
            notice.code == contract::ErrorCode::ConfigKeyIgnored
                && notice.message.contains(&path.display().to_string())
                && notice.message.contains("shell.read_only.jq.flags")
        }),
        "{:?}",
        config.notices()
    );
}

#[test]
fn a_repository_override_leaves_the_personal_list_intact() {
    let rig = Rig::new("fiber-shell-read-only-repository-override");
    rig.write_global(&serde_json::json!({"shell": {"read_only": {"jq": {"flags": ["-r"]}}}}));
    write_repository(
        &rig,
        r#"{"shell": {"read_only": {"jq": {"flags": ["-c"]}}}}"#,
    );
    let config = rig.config(Vec::<String>::new());
    assert_eq!(
        shell_read_only(&config),
        vec![("jq".to_owned(), vec!["-r".to_owned()])]
    );
}

#[test]
fn reading_again_after_the_file_changes_returns_the_new_list() {
    let rig = Rig::new("fiber-shell-read-only-reread");
    rig.write_global(&serde_json::json!({"shell": {"read_only": {"jq": {"flags": ["-r"]}}}}));
    assert_eq!(
        read(&rig, &[]),
        vec![("jq".to_owned(), vec!["-r".to_owned()])]
    );
    rig.write_global(&serde_json::json!({"shell": {"read_only": {"yq": {"flags": []}}}}));
    assert_eq!(read(&rig, &[]), vec![("yq".to_owned(), Vec::new())]);
}

#[test]
fn refresh_replaces_the_held_list_with_the_fresh_config() {
    let rig = Rig::new("fiber-shell-read-only-refresh");
    let handle: ShellReadOnly = std::sync::Arc::new(std::sync::Mutex::new(vec![(
        "jq".to_owned(),
        vec!["-r".to_owned()],
    )]));
    rig.write_global(&serde_json::json!({"shell": {"read_only": {"yq": {"flags": []}}}}));
    let config = rig.config(Vec::<String>::new());
    refresh_shell_read_only(&handle, &config);
    assert_eq!(*handle.lock().unwrap(), vec![("yq".to_owned(), Vec::new())]);
}

#[test]
fn a_clone_taken_before_refresh_sees_the_new_list() {
    let rig = Rig::new("fiber-shell-read-only-refresh-clone");
    let handle: ShellReadOnly = std::sync::Arc::new(std::sync::Mutex::new(vec![(
        "jq".to_owned(),
        vec!["-r".to_owned()],
    )]));
    let before = handle.clone();
    rig.write_global(&serde_json::json!({"shell": {"read_only": {"yq": {"flags": []}}}}));
    let config = rig.config(Vec::<String>::new());
    refresh_shell_read_only(&handle, &config);
    assert_eq!(*before.lock().unwrap(), vec![("yq".to_owned(), Vec::new())]);
}

#[test]
fn a_second_refresh_replaces_again() {
    let rig = Rig::new("fiber-shell-read-only-refresh-twice");
    let handle: ShellReadOnly = Default::default();
    rig.write_global(&serde_json::json!({"shell": {"read_only": {"yq": {"flags": []}}}}));
    refresh_shell_read_only(&handle, &rig.config(Vec::<String>::new()));
    rig.write_global(&serde_json::json!({"shell": {"read_only": {"zq": {"flags": ["-e"]}}}}));
    refresh_shell_read_only(&handle, &rig.config(Vec::<String>::new()));
    assert_eq!(
        *handle.lock().unwrap(),
        vec![("zq".to_owned(), vec!["-e".to_owned()])]
    );
}

#[test]
fn refresh_from_a_config_with_no_entry_empties_the_list() {
    let rig = Rig::new("fiber-shell-read-only-refresh-empty");
    let handle: ShellReadOnly = std::sync::Arc::new(std::sync::Mutex::new(vec![(
        "jq".to_owned(),
        vec!["-r".to_owned()],
    )]));
    rig.write_global(&serde_json::json!({"model": "scripted/s.json"}));
    refresh_shell_read_only(&handle, &rig.config(Vec::<String>::new()));
    assert_eq!(*handle.lock().unwrap(), Vec::new());
}

#[test]
fn refresh_recovers_a_poisoned_lock() {
    let rig = Rig::new("fiber-shell-read-only-refresh-poison");
    let handle: ShellReadOnly = std::sync::Arc::new(std::sync::Mutex::new(vec![(
        "jq".to_owned(),
        vec!["-r".to_owned()],
    )]));
    let victim = handle.clone();
    let poisoned = fakes::within(
        "shell.read_only lock poisoned",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            std::thread::spawn(move || {
                let _guard = victim.lock().unwrap();
                panic!("poison the shell.read_only lock");
            })
            .join()
        },
    );
    assert!(poisoned.is_err(), "the lock was not poisoned");
    rig.write_global(&serde_json::json!({"shell": {"read_only": {"yq": {"flags": []}}}}));
    refresh_shell_read_only(&handle, &rig.config(Vec::<String>::new()));
    assert_eq!(
        *handle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        vec![("yq".to_owned(), Vec::new())]
    );
}
