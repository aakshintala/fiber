//! Forced interleavings for the validated reader: a committed pause brackets
//! the open, the test swaps the file while the reader waits, and the
//! descriptor check after the open decides. Every call runs on a worker
//! under one 10 s wall guard; the test owns the release sender and drops it
//! before any join, and never joins a worker that did not report.

#![allow(clippy::unwrap_used, reason = "test setup; a failure is the test's")]
#![allow(clippy::panic, reason = "a hang is the test's failure")]

use std::fs;
use std::os::unix::fs::symlink;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fakes::{Deadline, TempDir};

use super::seam;
use super::{Presence, read_validated};
use crate::{Config, ConfigError, ProjectKey, ProviderData, Sources};
use contract::ErrorCode;

/// One wall guard for every signal and result in a race.
const HANG: Duration = Duration::from_secs(10);

/// A home, a workspace and the config read from them.
struct Home {
    /// The temporary root; dropped at the test's end, after every join.
    _root: TempDir,
    /// The loaded configuration.
    config: Config,
}

impl Home {
    /// A home whose `path` file holds `text`, and the config read from it.
    fn new(write: impl FnOnce(&TempDir)) -> Self {
        let root = TempDir::new("fiber-origin-race");
        fs::create_dir_all(root.path().join("home")).unwrap();
        fs::create_dir_all(root.path().join("workspace")).unwrap();
        write(&root);
        let config = Config::load(Sources {
            home: root.path().join("home"),
            workspace: root.path().join("workspace"),
            project: ProjectKey::new("race").unwrap(),
            overrides: Vec::new(),
        })
        .unwrap();
        Self {
            _root: root,
            config,
        }
    }

    /// The home directory.
    fn home(&self) -> std::path::PathBuf {
        self._root.path().join("home")
    }
}

/// A provider with no declared source, reading stored label `default`.
fn stored_provider() -> ProviderData {
    ProviderData {
        name: "fake".into(),
        credential_from: crate::CredentialFrom::Stored {
            credential: None,
            login: None,
        },
        credential_name: None,
        headers: Default::default(),
        placeholders: Default::default(),
        models: Vec::new(),
        reviewer_model: None,
    }
}

/// Runs the reader for `label` on an unscoped worker while the test swaps
/// `path`, and returns what the worker reported. Joins the worker only
/// after its report arrived; a worker that never reports is never joined,
/// so a blocking-open mutant fails at the wall guard instead of hanging
/// the join.
fn swapped(
    config: Config,
    provider: ProviderData,
    label: &str,
    path: &std::path::Path,
    swap: impl FnOnce(&std::path::Path),
) -> Result<Presence, ConfigError> {
    let (arrived, release, _pause) = seam::arm_pre_open(path);
    let _cleared = seam::take_reads();
    let (done, finished) = mpsc::channel();
    let label = label.to_owned();
    let worker = thread::Builder::new()
        .name("origin-race".to_owned())
        .spawn(move || {
            let outcome = config
                .credentials()
                .key_presence(&provider, &label, &|_: &str| None::<String>);
            done.send(outcome).unwrap_or(());
        })
        .unwrap();
    Deadline::after(HANG)
        .recv(&arrived)
        .expect("the reader did not reach its pre-open pause");
    swap(path);
    drop(release);
    let outcome = Deadline::after(HANG)
        .recv(&finished)
        .expect("the reader did not return after its release");
    worker.join().expect("the reader panicked");
    outcome
}

/// The paths the reader actually read from since the pause was armed.
fn reads(path: &std::path::Path) -> bool {
    seam::take_reads().contains(&path.to_path_buf())
}

#[test]
fn a_configured_file_swapped_for_a_fifo_is_absent_and_unread() {
    let key = Home::new(|root| {
        let file = root.path().join("key");
        fs::write(&file, "from-file\n").unwrap();
        fs::write(
            root.path().join("home/config.json"),
            format!(
                r#"{{"providers": {{"fake": {{"credentials": {{"key": {{"file": "{}"}}}}}}}}}}"#,
                file.display(),
            ),
        )
        .unwrap();
    });
    let path = key._root.path().join("key");
    let outcome = swapped(
        key.config.clone(),
        stored_provider(),
        "key",
        &path,
        |path| {
            fs::remove_file(path).unwrap();
            fakes::fifo(path);
        },
    );
    assert!(matches!(outcome, Ok(Presence::Absent)), "{outcome:?}");
    assert!(
        !reads(&path),
        "a FIFO swapped in after the listing was read"
    );
}

#[test]
fn a_stored_file_swapped_for_a_fifo_is_not_plain_and_unread() {
    let key = Home::new(|root| {
        let dir = root.path().join("home/credentials/fake");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("default"), "stored\n").unwrap();
    });
    let path = key.home().join("credentials/fake/default");
    let outcome = swapped(
        key.config.clone(),
        stored_provider(),
        "default",
        &path,
        |path| {
            fs::remove_file(path).unwrap();
            fakes::fifo(path);
        },
    );
    assert!(
        matches!(outcome, Err(ConfigError::NotPlain { .. })),
        "{outcome:?}"
    );
    assert_eq!(outcome.unwrap_err().code(), ErrorCode::ConfigInvalid);
    assert!(
        !reads(&path),
        "a FIFO swapped in after the listing was read"
    );
}

#[test]
fn a_configured_file_swapped_for_a_directory_is_an_io_error() {
    let key = Home::new(|root| {
        let file = root.path().join("key");
        fs::write(&file, "from-file\n").unwrap();
        fs::write(
            root.path().join("home/config.json"),
            format!(
                r#"{{"providers": {{"fake": {{"credentials": {{"key": {{"file": "{}"}}}}}}}}}}"#,
                file.display(),
            ),
        )
        .unwrap();
    });
    let path = key._root.path().join("key");
    let outcome = swapped(
        key.config.clone(),
        stored_provider(),
        "key",
        &path,
        |path| {
            fs::remove_file(path).unwrap();
            fs::create_dir(path).unwrap();
        },
    );
    assert!(
        matches!(outcome, Err(ConfigError::Io { .. })),
        "{outcome:?}"
    );
    assert_eq!(outcome.unwrap_err().code(), ErrorCode::IoFailed);
    assert!(
        !reads(&path),
        "a directory swapped in after the listing was read"
    );
}

#[test]
fn a_stored_file_swapped_for_a_directory_is_not_plain() {
    let key = Home::new(|root| {
        let dir = root.path().join("home/credentials/fake");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("default"), "stored\n").unwrap();
    });
    let path = key.home().join("credentials/fake/default");
    let outcome = swapped(
        key.config.clone(),
        stored_provider(),
        "default",
        &path,
        |path| {
            fs::remove_file(path).unwrap();
            fs::create_dir(path).unwrap();
        },
    );
    assert!(
        matches!(outcome, Err(ConfigError::NotPlain { .. })),
        "{outcome:?}"
    );
    assert!(
        !reads(&path),
        "a directory swapped in after the listing was read"
    );
}

#[test]
fn a_stored_file_swapped_for_a_link_is_refused_without_following() {
    let key = Home::new(|root| {
        let dir = root.path().join("home/credentials/fake");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("default"), "stored\n").unwrap();
        fs::write(root.path().join("elsewhere"), "elsewhere\n").unwrap();
    });
    let path = key.home().join("credentials/fake/default");
    let outcome = swapped(
        key.config.clone(),
        stored_provider(),
        "default",
        &path,
        |path| {
            fs::remove_file(path).unwrap();
            symlink(key._root.path().join("elsewhere"), path).unwrap();
        },
    );
    assert!(
        matches!(outcome, Err(ConfigError::NotPlain { .. })),
        "{outcome:?}"
    );
    assert!(
        !reads(&path),
        "a link swapped in after the listing was read"
    );
}

#[test]
fn a_file_swapped_for_another_regular_file_still_reads() {
    let key = Home::new(|root| {
        let dir = root.path().join("home/credentials/fake");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("default"), "stored\n").unwrap();
    });
    let path = key.home().join("credentials/fake/default");
    let outcome = swapped(
        key.config.clone(),
        stored_provider(),
        "default",
        &path,
        |path| {
            fs::write(path, "replacement\n").unwrap();
        },
    );
    assert!(matches!(outcome, Ok(Presence::Present)), "{outcome:?}");
    assert!(reads(&path), "the replacement regular file was not read");
}

#[test]
fn a_regular_file_reads_through_the_validated_reader() {
    let root = TempDir::new("fiber-origin-direct");
    let file = root.path().join("key");
    fs::write(&file, "  spaced  \n").unwrap();
    let text = read_validated(&file, false).unwrap();
    assert_eq!(text, "  spaced  \n");
}

#[test]
fn a_configured_file_loop_is_an_io_error_not_an_absence() {
    // A symlink loop never resolves, so following the configured path
    // fails with `FilesystemLoop`, not `NotFound`: the validated read
    // reports it, and the key is an error, never a silent absence.
    let key = Home::new(|root| {
        symlink(root.path().join("loop-b"), root.path().join("loop-a")).unwrap();
        symlink(root.path().join("loop-a"), root.path().join("loop-b")).unwrap();
        let file = root.path().join("loop-a");
        fs::write(
            root.path().join("home/config.json"),
            format!(
                r#"{{"providers": {{"fake": {{"credentials": {{"key": {{"file": "{}"}}}}}}}}}}"#,
                file.display(),
            ),
        )
        .unwrap();
    });
    let outcome = key
        .config
        .credentials()
        .key_presence(&stored_provider(), "key", &|_: &str| None::<String>);
    assert!(
        matches!(outcome, Err(ConfigError::Io { .. })),
        "a loop is an error, not an absence: {outcome:?}"
    );
    assert_eq!(outcome.unwrap_err().code(), ErrorCode::IoFailed);
}

#[test]
fn a_configured_file_deleted_before_its_open_is_absent() {
    // Listed, then gone before the open: the open reports the operating
    // system's missing file, which holds no key.
    let key = Home::new(|root| {
        let file = root.path().join("key");
        fs::write(&file, "from-file\n").unwrap();
        fs::write(
            root.path().join("home/config.json"),
            format!(
                r#"{{"providers": {{"fake": {{"credentials": {{"key": {{"file": "{}"}}}}}}}}}}"#,
                file.display(),
            ),
        )
        .unwrap();
    });
    let path = key._root.path().join("key");
    let outcome = swapped(
        key.config.clone(),
        stored_provider(),
        "key",
        &path,
        |path| {
            fs::remove_file(path).unwrap();
        },
    );
    assert!(matches!(outcome, Ok(Presence::Absent)), "{outcome:?}");
    assert!(!reads(&path), "a deleted file was never read");
}

#[test]
fn a_configured_file_swapped_for_a_loop_is_an_io_error() {
    // A regular file at listing time, a symlink loop at open time: the
    // following open fails instead of reading through the loop, and the
    // key is an error, never a silent absence.
    let key = Home::new(|root| {
        let file = root.path().join("key");
        fs::write(&file, "from-file\n").unwrap();
        fs::write(
            root.path().join("home/config.json"),
            format!(
                r#"{{"providers": {{"fake": {{"credentials": {{"key": {{"file": "{}"}}}}}}}}}}"#,
                file.display(),
            ),
        )
        .unwrap();
    });
    let path = key._root.path().join("key");
    let outcome = swapped(
        key.config.clone(),
        stored_provider(),
        "key",
        &path,
        |path| {
            let other = path.with_extension("loop");
            fs::remove_file(path).unwrap();
            symlink(&other, path).unwrap();
            symlink(path, &other).unwrap();
        },
    );
    assert!(
        matches!(outcome, Err(ConfigError::Io { .. })),
        "a loop is an error, not an absence: {outcome:?}"
    );
    assert_eq!(outcome.unwrap_err().code(), ErrorCode::IoFailed);
    assert!(
        !reads(&path),
        "a loop swapped in after the listing was read"
    );
}
