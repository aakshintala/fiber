//! The resource-exhaustion guard's tests (`scripts/check-resources`):
//! `preflight` refuses when the machine has no room to build, and `classify`
//! names a killed compile or link step. The logic stays in the script because
//! a check of whether the machine can build cannot need a build
//! (`docs/testing.md`); these tests run that script on supplied inputs.

use std::os::unix::fs::PermissionsExt as _;
use std::process::Command;

use fakes::TempDir;

fn script() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../scripts/check-resources")
}

/// Writes `bin/df`, a stub printing one fixed `df -Pk` table with
/// `avail_kib` free, and returns a `PATH` value with `bin` first.
fn df_path(dir: &TempDir, avail_kib: u64) -> String {
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let stub = format!(
        "#!/usr/bin/env bash\nprintf 'Filesystem  1024-blocks  Used Available Capacity Mounted on\\n'\nprintf 'testfs  200000000  100000000  {avail_kib}  50%%  /testmnt\\n'\n"
    );
    let df = bin.join("df");
    std::fs::write(&df, stub).unwrap();
    std::fs::set_permissions(&df, std::fs::Permissions::from_mode(0o755)).unwrap();
    format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

fn scratch_and_target(dir: &TempDir) -> (std::path::PathBuf, std::path::PathBuf) {
    let scratch = dir.path().join("scratch");
    let target = dir.path().join("target");
    std::fs::create_dir_all(&scratch).unwrap();
    std::fs::create_dir_all(&target).unwrap();
    (scratch, target)
}

#[test]
fn preflight_refuses_when_df_reports_too_little_space() {
    let dir = TempDir::new("xtask-check-resources");
    let (scratch, target) = scratch_and_target(&dir);
    let path = df_path(&dir, 0);
    let output = Command::new(script())
        .arg("preflight")
        .env("PATH", &path)
        .env("TMPDIR", &scratch)
        .env("CARGO_TARGET_DIR", &target)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(75));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("not started:"),
        "refusal names no start: {stdout}"
    );
    assert!(
        stdout.contains("share a filesystem"),
        "refusal names no shared filesystem: {stdout}"
    );
    assert!(
        stdout.contains("(PREFLIGHT_TMPDIR_MIN_MIB + PREFLIGHT_TARGET_MIN_MIB)"),
        "refusal names no summed constants: {stdout}"
    );
    assert!(
        stdout.contains("the code is untested."),
        "refusal holds no untested line: {stdout}"
    );
}

#[test]
fn preflight_passes_when_df_reports_generous_space() {
    let dir = TempDir::new("xtask-check-resources");
    let (scratch, target) = scratch_and_target(&dir);
    let path = df_path(&dir, 100_000_000);
    let output = Command::new(script())
        .arg("preflight")
        .env("PATH", &path)
        .env("TMPDIR", &scratch)
        .env("CARGO_TARGET_DIR", &target)
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "generous preflight failed: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn classify_reports_bus_error_linker_death_as_resource_exhaustion() {
    let dir = TempDir::new("xtask-check-resources");
    dir.write(
        "build.log",
        "collect2: fatal error: ld terminated with signal 7 [Bus error]\n",
    );
    let log = dir.path().join("build.log");
    let output = Command::new(script())
        .args(["classify", &log.to_string_lossy()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(75));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.as_ref(),
        "the build was killed for lack of resources (matched \"terminated with signal 7\"); the code is unproven, not broken; rerun once the machine has memory.\n"
    );
}
