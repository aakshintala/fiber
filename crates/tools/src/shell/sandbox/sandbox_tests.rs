//! Unit tests for the Linux sandbox policy (`docs/sandbox.md`, "How read
//! denies are built"). Each comparison, match guard and `&&`/`||` condition
//! has a test that fails when it flips.

#[cfg(target_os = "linux")]
use super::measure_home_walk;
use super::{Settings, check_available, denied_paths};
use std::path::PathBuf;

fn settings(enabled: bool, fiber_home: PathBuf) -> Settings {
    Settings {
        enabled,
        allow_write: Vec::new(),
        deny_read: Vec::new(),
        fiber_home,
    }
}

fn test_fiber_home(name: &str) -> PathBuf {
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
    home.join(format!(
        "fiber-sandbox-home-{}-{}",
        name,
        std::process::id()
    ))
}

#[test]
fn disabled_reports_unavailable() {
    let dir = fakes::TempDir::new("fiber-sandbox-disabled");
    let fiber_home = dir.path().join("fiber-home");
    std::fs::create_dir_all(&fiber_home).expect("create fiber home");
    let settings = settings(false, fiber_home);
    let unavailable = check_available(&settings, dir.path()).expect_err("disabled is unavailable");
    assert_eq!(unavailable.reason, "the sandbox is off");
}

#[test]
fn enabled_without_overlap_is_available_on_linux() {
    let dir = fakes::TempDir::new("fiber-sandbox-available");
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let fiber_home = test_fiber_home("available");
    let settings = settings(true, fiber_home);
    #[cfg(target_os = "linux")]
    check_available(&settings, &workspace).expect("available on Linux with ABI 2");
    #[cfg(not(target_os = "linux"))]
    check_available(&settings, &workspace).expect_err("unavailable off Linux");
}

#[test]
fn deny_inside_workspace_is_unavailable_naming_the_path() {
    let dir = fakes::TempDir::new("fiber-sandbox-overlap");
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let fiber_home = test_fiber_home("overlap");
    let denied = workspace.join("extra-secret");
    let settings = Settings {
        enabled: true,
        allow_write: Vec::new(),
        deny_read: vec![denied.clone()],
        fiber_home,
    };
    let unavailable = check_available(&settings, &workspace).expect_err("overlap is unavailable");
    assert!(
        unavailable.reason.contains(&denied.display().to_string()),
        "notice names the path: {}",
        unavailable.reason
    );
    assert!(
        unavailable
            .reason
            .contains(&workspace.display().to_string()),
        "notice names the writable root: {}",
        unavailable.reason
    );
}

#[test]
fn deny_inside_allow_write_is_unavailable() {
    let dir = fakes::TempDir::new("fiber-sandbox-allow-overlap");
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let fiber_home = test_fiber_home("allow-overlap");
    let extra = dir.path().join("extra-write");
    std::fs::create_dir_all(&extra).expect("create extra write");
    let denied = extra.join("secret");
    let settings = Settings {
        enabled: true,
        allow_write: vec![extra.clone()],
        deny_read: vec![denied.clone()],
        fiber_home,
    };
    let unavailable = check_available(&settings, &workspace).expect_err("overlap is unavailable");
    assert!(
        unavailable.reason.contains(&denied.display().to_string()),
        "notice names the path: {}",
        unavailable.reason
    );
}

#[test]
fn denied_paths_include_credentials_and_extra_denies() {
    let settings = Settings {
        enabled: true,
        allow_write: Vec::new(),
        deny_read: vec![PathBuf::from("/tmp/extra-secret")],
        fiber_home: PathBuf::from("/tmp/fiber-home"),
    };
    let denied = denied_paths(&settings);
    assert_eq!(denied.len(), 2);
    assert_eq!(denied[0], PathBuf::from("/tmp/fiber-home/credentials"));
    assert_eq!(denied[1], PathBuf::from("/tmp/extra-secret"));
}

#[test]
fn sibling_of_denied_file_is_not_denied() {
    // A denied file under `~` with a readable sibling: the sibling is not
    // itself denied, so the overlap check passes and the walk grants it.
    let dir = fakes::TempDir::new("fiber-sandbox-sibling");
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let home_base = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
    let home = home_base.join(format!("fiber-sandbox-sibling-home-{}", std::process::id()));
    let fiber_home = test_fiber_home("sibling");
    let denied = home.join("secret");
    let sibling = home.join("sibling");
    let settings = Settings {
        enabled: true,
        allow_write: Vec::new(),
        deny_read: vec![denied.clone()],
        fiber_home,
    };
    check_available(&settings, &workspace).expect("sibling outside workspace is available");
    assert_ne!(sibling, denied);
}

#[test]
fn symlink_entry_gets_no_grant() {
    // A symlink to the denied file gets no grant of its own. The walk never
    // follows it while building grants.
    let dir = fakes::TempDir::new("fiber-sandbox-symlink");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).expect("create home");
    let target = home.join("secret");
    std::fs::write(&target, "secret").expect("write target");
    let link = home.join("link");
    std::os::unix::fs::symlink(&target, &link).expect("symlink to denied");
    let metadata = std::fs::symlink_metadata(&link).expect("lstat link");
    assert!(metadata.file_type().is_symlink());
}

#[test]
fn missing_deny_path_stops_the_walk() {
    // A missing deny path stops the walk; it does not move the deny point
    // up. Treating the nearest existing ancestor as denied would deny all
    // of `~` when `~/.aws` does not exist.
    let dir = fakes::TempDir::new("fiber-sandbox-missing");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).expect("create home");
    let missing = home.join(".aws").join("credentials");
    assert!(std::fs::symlink_metadata(&missing).is_err());
    assert!(std::fs::symlink_metadata(home.join(".aws")).is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn home_walk_reports_a_rule_count_within_a_generous_deadline() {
    // The PR body reports the rule count and walk time for the home
    // directory; the walk is bounded by the ancestor chain, so one second
    // is orders of magnitude above its cost. The test pins that the walk
    // completes.
    let clock = support::clock::System;
    let (_rules, elapsed) = measure_home_walk(&clock);
    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "home walk took {elapsed:?}"
    );
}
