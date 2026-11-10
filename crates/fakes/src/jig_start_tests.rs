use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::{START_BOUND, WarmedJig, stale_evidence, warm_up, warm_up_within};
use crate::{MUST_SUCCEED_WITHIN, TempDir, fifo};

#[test]
fn start_bound_covers_the_stall() {
    assert!(
        START_BOUND >= Duration::from_secs(30),
        "the start bound covers the seconds-long first-exec stall: {START_BOUND:?}"
    );
    assert!(
        START_BOUND > MUST_SUCCEED_WITHIN,
        "the start bound exceeds the must-succeed bound: {START_BOUND:?} vs {MUST_SUCCEED_WITHIN:?}"
    );
    assert!(
        2 * START_BOUND <= Duration::from_secs(120),
        "twice the start bound fits nextest's 4 x 30 s timeout (`docs/testing.md`, \"Waits and timeouts\"): {START_BOUND:?}"
    );
}

#[test]
fn a_warm_up_with_the_expected_exit_returns() {
    warm_up(PathBuf::from("/bin/sh").as_path(), &["-c", "exit 3"], 3);
}

#[test]
fn a_wrong_exit_panics_naming_the_jig_and_the_start() {
    let binary = PathBuf::from("/bin/sh");
    let shown = binary.display().to_string();
    let failed = std::panic::catch_unwind(|| warm_up(&binary, &["-c", "exit 3"], 2))
        .expect_err("a warm-up with the wrong exit fails");
    let message = panic_message(failed);
    assert!(
        message.contains(&shown),
        "the failure names the jig: {message}"
    );
    assert!(
        message.contains("start"),
        "the failure names the start wait: {message}"
    );
    assert!(
        message.contains('2') && message.contains('3'),
        "the failure names both codes: {message}"
    );
}

#[test]
fn a_missing_binary_panics_naming_the_jig_and_the_start() {
    let binary = PathBuf::from("/bin/sh-no-such-binary");
    let shown = binary.display().to_string();
    let failed = std::panic::catch_unwind(|| warm_up(&binary, &[], 0))
        .expect_err("a warm-up with a missing binary fails");
    let message = panic_message(failed);
    assert!(
        message.contains(&shown),
        "the failure names the jig: {message}"
    );
    assert!(
        message.contains("start"),
        "the failure names the start wait: {message}"
    );
}

#[test]
fn a_start_that_misses_the_bound_names_the_start_not_the_exchange() {
    let dir = TempDir::new("fiber-jig-start");
    let pipe = dir.path().join("pipe");
    fifo(&pipe);
    let command = format!("read x < {}", pipe.display());
    let binary = PathBuf::from("/bin/sh");
    let shown = binary.display().to_string();
    let failed = std::panic::catch_unwind(|| {
        warm_up_within(&binary, &["-c", &command], 0, Duration::from_millis(100));
    })
    .expect_err("a start past its bound fails");
    let message = panic_message(failed);
    assert!(
        message.contains(&shown),
        "the failure names the jig: {message}"
    );
    assert!(
        message.contains("start"),
        "the failure names the start wait, not the exchange: {message}"
    );
}

#[test]
fn warm_warms_before_it_returns() {
    let binary = PathBuf::from("/bin/sh");
    let shown = binary.display().to_string();
    let failed = std::panic::catch_unwind(|| {
        WarmedJig::warm(binary, &["-c", "exit 3"], 2);
    })
    .expect_err("a warm with the wrong exit fails");
    let message = panic_message(failed);
    assert!(
        message.contains(&shown),
        "the failure names the jig: {message}"
    );
    assert!(
        message.contains("start"),
        "the failure names the start wait: {message}"
    );
}

#[test]
fn path_yields_the_warmed_path() {
    let binary = PathBuf::from("/bin/sh");
    let warmed = WarmedJig::warm(binary.clone(), &["-c", "exit 3"], 3);
    assert_eq!(
        warmed.path(),
        binary.as_path(),
        "the accessor yields the path the warm covered"
    );
}

fn binary_mtime() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
}

fn set_mtime(path: &Path, mtime: SystemTime) {
    fs::File::open(path).unwrap().set_modified(mtime).unwrap();
}

/// A non-executable binary stand-in with an explicit mtime: the freshness
/// tests assert the refusal, so the stand-in never runs.
fn stand_in(dir: &TempDir, mtime: SystemTime) -> PathBuf {
    let binary = dir.path().join("jig");
    fs::write(&binary, "not a binary").unwrap();
    set_mtime(&binary, mtime);
    binary
}

fn dep_info_path(binary: &Path) -> PathBuf {
    let mut name = binary.file_name().unwrap().to_owned();
    name.push(".d");
    binary.with_file_name(name)
}

fn write_dep_info(binary: &Path, deps: &[String]) {
    let rule = format!("{}: {}\n", binary.display(), deps.join(" "));
    fs::write(dep_info_path(binary), rule).unwrap();
}

fn wrote_source(dir: &TempDir, name: &str, mtime: SystemTime) -> PathBuf {
    let source = dir.path().join(name);
    fs::write(&source, "source").unwrap();
    set_mtime(&source, mtime);
    source
}

fn escaped(path: &Path) -> String {
    path.display().to_string().replace(' ', "\\ ")
}

#[test]
fn inputs_older_than_the_binary_pass_the_check() {
    let dir = TempDir::new("fiber-jig-fresh");
    let binary = stand_in(&dir, binary_mtime());
    let spaced = wrote_source(&dir, "my src.rs", binary_mtime() - Duration::from_secs(60));
    let older = wrote_source(&dir, "older.rs", binary_mtime() - Duration::from_secs(3600));
    write_dep_info(&binary, &[escaped(&spaced), escaped(&older)]);
    assert!(
        stale_evidence(&binary).is_none(),
        "a binary newer than every listed input is fresh"
    );
}

#[test]
fn a_newer_source_fails_naming_the_binary_the_source_and_the_rebuild() {
    let dir = TempDir::new("fiber-jig-stale");
    let binary = stand_in(&dir, binary_mtime());
    let source = wrote_source(&dir, "source.rs", binary_mtime() + Duration::from_secs(60));
    write_dep_info(&binary, &[escaped(&source)]);
    let shown = binary.display().to_string();
    let failed = std::panic::catch_unwind(|| {
        WarmedJig::warm(binary, &["--bogus"], 2);
    })
    .expect_err("a warm with a stale binary fails before it spawns");
    let message = panic_message(failed);
    assert!(
        message.contains(&shown),
        "the failure names the stale binary: {message}"
    );
    assert!(
        message.contains("source.rs"),
        "the failure names the newer source: {message}"
    );
    assert!(
        message.contains("cargo build --examples"),
        "the failure names the rebuild command: {message}"
    );
}

#[test]
fn a_source_from_the_same_second_as_the_binary_fails_closed() {
    let dir = TempDir::new("fiber-jig-equal");
    let binary = stand_in(&dir, binary_mtime());
    let source = wrote_source(&dir, "source.rs", binary_mtime());
    write_dep_info(&binary, &[escaped(&source)]);
    let stale = stale_evidence(&binary).expect("an equal mtime is stale");
    assert_eq!(
        stale, source,
        "a source no older than the binary fails closed"
    );
}

#[test]
fn a_missing_source_fails_naming_it() {
    let dir = TempDir::new("fiber-jig-missing-source");
    let binary = stand_in(&dir, binary_mtime());
    let missing = dir.path().join("gone.rs");
    write_dep_info(&binary, &[escaped(&missing)]);
    let stale = stale_evidence(&binary).expect("a missing source is stale");
    assert_eq!(stale, missing, "the refusal names the missing source");
}

#[test]
fn a_dep_info_with_no_rule_fails_naming_the_dep_info() {
    let dir = TempDir::new("fiber-jig-malformed");
    let binary = stand_in(&dir, binary_mtime());
    let dep_info = dep_info_path(&binary);
    fs::write(&dep_info, "no colon here\n").unwrap();
    let stale = stale_evidence(&binary).expect("a malformed dep-info is stale");
    assert_eq!(stale, dep_info, "the refusal names the dep-info file");
}

#[test]
fn an_unreadable_dep_info_fails_naming_the_dep_info() {
    let dir = TempDir::new("fiber-jig-unreadable");
    let binary = stand_in(&dir, binary_mtime());
    let dep_info = dep_info_path(&binary);
    fs::create_dir(&dep_info).unwrap();
    let stale = stale_evidence(&binary).expect("an unreadable dep-info is stale");
    assert_eq!(stale, dep_info, "the refusal names the dep-info file");
}

#[test]
fn a_missing_dep_info_skips_the_check() {
    let dir = TempDir::new("fiber-jig-no-dep-info");
    let binary = stand_in(&dir, binary_mtime());
    assert!(
        stale_evidence(&binary).is_none(),
        "no dep-info file means unchecked, never stale"
    );
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_owned()
    } else {
        "a non-string panic".to_owned()
    }
}
