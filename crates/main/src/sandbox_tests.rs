//! The `sandbox.*` reader: person-only keys with defaults, repository
//! rejection and refresh (`docs/configuration.md`, "Keys").

use super::{SandboxHandle, refresh_sandbox, sandbox_settings};
use crate::test_support::Rig;
use fakes::Deadline;

fn read(rig: &Rig) -> tools::SandboxSettings {
    let config = rig.config(Vec::<String>::new());
    sandbox_settings(&config, &rig.home, &[])
}

#[test]
fn an_absent_sandbox_reads_off_with_empty_lists() {
    let rig = Rig::new("fiber-sandbox-absent");
    let settings = read(&rig);
    assert!(!settings.enabled);
    assert!(settings.allow_write.is_empty());
    assert!(settings.deny_read.is_empty());
}

#[test]
fn a_global_setting_turns_the_sandbox_on() {
    let rig = Rig::new("fiber-sandbox-global");
    rig.write_global(&serde_json::json!({"sandbox": {"enabled": true}}));
    assert!(read(&rig).enabled);
}

#[test]
fn a_run_override_turns_the_sandbox_on() {
    let rig = Rig::new("fiber-sandbox-override");
    let config = rig.config(vec![r#"sandbox.enabled=true"#.to_owned()]);
    let settings = sandbox_settings(&config, &rig.home, &[]);
    assert!(settings.enabled);
}

#[test]
fn write_and_deny_lists_expand_home() {
    let rig = Rig::new("fiber-sandbox-lists");
    rig.write_global(&serde_json::json!({"sandbox": {
        "enabled": true,
        "allow_write": ["~/.pi/agent"],
        "deny_read": ["~/extra-secret"],
    }}));
    let settings = read(&rig);
    let home = std::env::var_os("HOME").map_or_else(
        || std::path::PathBuf::from("/tmp"),
        std::path::PathBuf::from,
    );
    assert_eq!(settings.allow_write, vec![home.join(".pi/agent")]);
    assert_eq!(settings.deny_read, vec![home.join("extra-secret")]);
}

fn write_repository(rig: &Rig, text: &str) -> std::path::PathBuf {
    std::fs::create_dir_all(rig.workspace.join(".fiber")).unwrap();
    let path = rig.workspace.join(".fiber/config.json");
    std::fs::write(&path, text).unwrap();
    path
}

#[test]
fn a_repository_value_is_ignored_with_a_notice() {
    let rig = Rig::new("fiber-sandbox-repository");
    let path = write_repository(&rig, r#"{"sandbox": {"enabled": true}}"#);
    let config = rig.config(Vec::<String>::new());
    assert!(!sandbox_settings(&config, &rig.home, &[]).enabled);
    assert!(
        config.notices().iter().any(|notice| {
            notice.code == contract::ErrorCode::ConfigKeyIgnored
                && notice.message.contains(&path.display().to_string())
                && notice.message.contains("sandbox.enabled")
        }),
        "{:?}",
        config.notices()
    );
}

/// A deny inside a writable grant is a bad configuration: the startup
/// notice carries the configured path with no arrow for a lexical overlap.
#[cfg(target_os = "linux")]
#[test]
fn deny_inside_the_workspace_is_unavailable_naming_the_path() {
    let rig = Rig::new("fiber-sandbox-overlap");
    let workspace = rig.workspace.clone();
    let home_base = std::env::var_os("HOME").map_or_else(
        || std::path::PathBuf::from("/tmp"),
        std::path::PathBuf::from,
    );
    let fiber_home = home_base.join(format!("fiber-main-sandbox-overlap-{}", std::process::id()));
    let denied = workspace.join("secret");
    let settings = tools::SandboxSettings {
        enabled: true,
        allow_write: Vec::new(),
        deny_read: vec![denied.clone()],
        fiber_home,
    };
    let unavailable = tools::sandbox_check_available(&settings, &workspace).expect_err("overlap");
    assert_eq!(
        unavailable,
        tools::SandboxUnavailable::DenyInsideWrite {
            configured: denied.clone(),
            overlapping: denied,
            root: workspace.clone(),
        }
    );
    assert_eq!(unavailable.code(), contract::ErrorCode::InvalidArguments);
    let notice = unavailable.to_string();
    assert!(
        notice.contains("secret") && notice.contains(&workspace.display().to_string()),
        "the notice names the configured path: {notice}"
    );
    assert!(
        !notice.contains('\u{2192}'),
        "a lexical overlap carries no arrow: {notice}"
    );
}

#[test]
fn refresh_replaces_the_held_settings_with_the_fresh_config() {
    let rig = Rig::new("fiber-sandbox-refresh");
    let handle: SandboxHandle =
        std::sync::Arc::new(std::sync::Mutex::new(tools::SandboxSettings {
            enabled: false,
            allow_write: Vec::new(),
            deny_read: Vec::new(),
            fiber_home: rig.home.clone(),
        }));
    rig.write_global(&serde_json::json!({"sandbox": {"enabled": true}}));
    let config = rig.config(Vec::<String>::new());
    refresh_sandbox(&handle, &config, &[]);
    assert!(support::lock(&handle).enabled);
}

#[test]
fn a_clone_taken_before_refresh_sees_the_new_settings() {
    let rig = Rig::new("fiber-sandbox-refresh-clone");
    let handle: SandboxHandle =
        std::sync::Arc::new(std::sync::Mutex::new(tools::SandboxSettings {
            enabled: false,
            allow_write: Vec::new(),
            deny_read: Vec::new(),
            fiber_home: rig.home.clone(),
        }));
    let before = handle.clone();
    rig.write_global(&serde_json::json!({"sandbox": {"enabled": true}}));
    let config = rig.config(Vec::<String>::new());
    refresh_sandbox(&handle, &config, &[]);
    assert!(support::lock(&before).enabled);
}

#[test]
fn credential_files_merge_into_the_deny_set() {
    let rig = Rig::new("fiber-sandbox-credentials");
    rig.write_global(&serde_json::json!({"sandbox": {
        "enabled": true,
        "deny_read": ["~/extra-secret"],
    }}));
    let credential = rig.home.join("credentials").join("token");
    std::fs::create_dir_all(credential.parent().unwrap()).unwrap();
    std::fs::write(&credential, "shh").unwrap();
    let config = rig.config(Vec::<String>::new());
    let settings = sandbox_settings(&config, &rig.home, &[credential.clone()]);
    let home = std::env::var_os("HOME").map_or_else(
        || std::path::PathBuf::from("/tmp"),
        std::path::PathBuf::from,
    );
    assert_eq!(
        settings.deny_read,
        vec![home.join("extra-secret"), credential]
    );
}

/// A credential path inside a writable grant disables the sandbox for the
/// session: the merged deny set trips the same overlap rule, and the notice
/// carries the generic read-deny label with no `sandbox.deny_read`
/// attribution.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn a_credential_path_inside_the_workspace_is_unavailable() {
    let rig = Rig::new("fiber-sandbox-credential-overlap");
    rig.write_global(&serde_json::json!({"sandbox": {"enabled": true}}));
    // The rig home sits under the temp directory, which is a writable
    // root: point the fiber home at the real home, as production does, so
    // only the workspace credential trips the overlap rule.
    let home_base = std::env::var_os("HOME").map_or_else(
        || std::path::PathBuf::from("/tmp"),
        std::path::PathBuf::from,
    );
    let fiber_home = home_base.join(format!(
        "fiber-main-sandbox-credential-{}",
        std::process::id()
    ));
    let denied = rig.workspace.join("secret");
    std::fs::write(&denied, "shh").expect("write secret");
    let config = rig.config(Vec::<String>::new());
    let settings = sandbox_settings(&config, &fiber_home, &[denied.clone()]);
    let unavailable =
        tools::sandbox_check_available(&settings, &rig.workspace).expect_err("overlap");
    assert_eq!(
        unavailable,
        tools::SandboxUnavailable::DenyInsideWrite {
            configured: denied.clone(),
            overlapping: denied.clone(),
            root: rig.workspace.clone(),
        }
    );
    assert_eq!(unavailable.code(), contract::ErrorCode::InvalidArguments);
    let notice = unavailable.to_string();
    assert!(
        notice.contains("sandbox read-deny path"),
        "the label names no source: {notice}"
    );
    assert!(
        !notice.contains("sandbox.deny_read"),
        "a credential path gets no deny_read attribution: {notice}"
    );
}

#[test]
fn refresh_forwards_credential_files_into_the_held_settings() {
    let rig = Rig::new("fiber-sandbox-refresh-credentials");
    let handle: SandboxHandle =
        std::sync::Arc::new(std::sync::Mutex::new(tools::SandboxSettings {
            enabled: false,
            allow_write: Vec::new(),
            deny_read: Vec::new(),
            fiber_home: rig.home.clone(),
        }));
    let credential = rig.home.join("credentials").join("token");
    std::fs::create_dir_all(credential.parent().unwrap()).unwrap();
    std::fs::write(&credential, "shh").unwrap();
    let config = rig.config(Vec::<String>::new());
    refresh_sandbox(&handle, &config, &[credential.clone()]);
    assert!(support::lock(&handle).deny_read.contains(&credential));
}

/// A credential path with no file behind it merges nothing: there is no
/// secret to protect yet, so the sandbox stays available.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn a_missing_credential_file_merges_nothing() {
    let rig = Rig::new("fiber-sandbox-missing-credential");
    rig.write_global(&serde_json::json!({"sandbox": {"enabled": true}}));
    // The rig home sits under the temp directory, which is a writable
    // root: point the fiber home at the real home, as production does, so
    // the sandbox's own credentials entry trips no overlap.
    let home_base = std::env::var_os("HOME").map_or_else(
        || std::path::PathBuf::from("/tmp"),
        std::path::PathBuf::from,
    );
    let fiber_home = home_base.join(format!("fiber-main-sandbox-missing-{}", std::process::id()));
    let missing = fiber_home.join("credentials").join("token");
    assert!(!missing.exists());
    let config = rig.config(Vec::<String>::new());
    let settings = sandbox_settings(&config, &fiber_home, &[missing]);
    assert!(settings.deny_read.is_empty());
    assert!(
        tools::sandbox_check_available(&settings, &rig.workspace).is_ok(),
        "a fresh install without credentials stays available"
    );
}

#[test]
fn a_missing_sandbox_exec_reports_the_binary_it_looked_for() {
    let missing = std::path::PathBuf::from("/nonexistent-fiber-sandbox-exec");
    let unavailable = tools::SandboxUnavailable::SandboxExecUnavailable {
        path: missing.clone(),
    };
    assert_eq!(unavailable.code(), contract::ErrorCode::ToolError);
    assert!(
        unavailable
            .to_string()
            .contains(&*missing.to_string_lossy()),
        "the notice names the binary: {unavailable}"
    );
}

#[test]
fn refresh_recovers_a_poisoned_lock() {
    let rig = Rig::new("fiber-sandbox-refresh-poison");
    let handle: SandboxHandle =
        std::sync::Arc::new(std::sync::Mutex::new(tools::SandboxSettings {
            enabled: false,
            allow_write: Vec::new(),
            deny_read: Vec::new(),
            fiber_home: rig.home.clone(),
        }));
    let victim = handle.clone();
    let poisoned = fakes::within(
        "sandbox lock poisoned",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            std::thread::spawn(move || {
                let _guard = victim.lock().unwrap();
                panic!("poison the sandbox lock");
            })
            .join()
        },
    );
    assert!(poisoned.is_err(), "the lock was not poisoned");
    rig.write_global(&serde_json::json!({"sandbox": {"enabled": true}}));
    refresh_sandbox(&handle, &rig.config(Vec::<String>::new()), &[]);
    assert!(support::lock(&handle).enabled);
}
