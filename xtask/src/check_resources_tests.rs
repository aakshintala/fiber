//! The resource-exhaustion guard's tests (`scripts/check-resources`):
//! `preflight` refuses when the machine has no room to build, and `classify`
//! names a killed compile or link step. The logic stays in the script because
//! a check of whether the machine can build cannot need a build
//! (`docs/testing.md`); these tests run that script on supplied inputs.

use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::process::CommandExt as _;
use std::process::{Command, Stdio};

use fakes::TempDir;

fn script() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../scripts/check-resources")
}

/// The single way these tests run `scripts/check-resources`: spawn in its own
/// process group and wait under a wall-clock bound. `fakes::wait_output`
/// reaps the group with SIGKILL through `fakes::process_group::kill_group`
/// on every exit path (success, failure, timeout).
fn run_check_resources(args: &[&str], envs: &[(&str, &str)], what: &str) -> std::process::Output {
    let mut command = Command::new(script());
    command.args(args);
    for (key, value) in envs {
        command.env(key, value);
    }
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    fakes::wait_output(child, what)
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
    // Every wait has a deadline (`docs/testing.md`, "Waits and timeouts").
    let output = run_check_resources(
        &["preflight"],
        &[
            ("PATH", path.as_str()),
            ("TMPDIR", scratch.to_str().expect("scratch path is UTF-8")),
            (
                "CARGO_TARGET_DIR",
                target.to_str().expect("target path is UTF-8"),
            ),
        ],
        "preflight with too little space",
    );
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
    // Every wait has a deadline (`docs/testing.md`, "Waits and timeouts").
    let output = run_check_resources(
        &["preflight"],
        &[
            ("PATH", path.as_str()),
            ("TMPDIR", scratch.to_str().expect("scratch path is UTF-8")),
            (
                "CARGO_TARGET_DIR",
                target.to_str().expect("target path is UTF-8"),
            ),
        ],
        "preflight with generous space",
    );
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
        "collect2: fatal error: ld terminated with signal 7 [Bus error]\nerror: linking with `cc` failed: exit code: 1\n",
    );
    let log = dir.path().join("build.log");
    let log = log.to_string_lossy().into_owned();
    // Every wait has a deadline (`docs/testing.md`, "Waits and timeouts").
    let output = run_check_resources(
        &["classify", log.as_str()],
        &[],
        "classify a bus-error linker death",
    );
    assert_eq!(output.status.code(), Some(75));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.as_ref(),
        "the build was killed for lack of resources (matched \"terminated with signal 7\"); the code is unproven, not broken; rerun once the machine has memory.\n"
    );
}

#[test]
fn classify_guard_bare_killed_compiler_shape_from_test_output_is_ignored() {
    let dir = TempDir::new("xtask-check-resources");
    dir.write(
        "bare-signal.log",
        "collect2: fatal error: ld terminated with signal 7\n",
    );
    let log = dir.path().join("bare-signal.log");
    let log = log.to_string_lossy().into_owned();
    // Every wait has a deadline (`docs/testing.md`, "Waits and timeouts").
    let output = run_check_resources(
        &["classify", log.as_str()],
        &[],
        "classify a bare killed-compiler shape",
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "bare signal without marker refused: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).is_empty(),
        "bare signal without marker printed: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}
