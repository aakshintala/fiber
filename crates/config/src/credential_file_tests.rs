use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fakes::Deadline;
use fakes::TempDir;
use serde_json::json;

use super::*;

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn open(home: &Path) -> CredentialFile {
    CredentialFile::new(home, "acme", "default").unwrap()
}

#[test]
fn a_stored_object_reads_back() {
    let home = TempDir::new("cred-file");
    let lock = open(home.path()).try_lock().unwrap().unwrap();
    assert_eq!(lock.read().unwrap(), None);

    let value = json!({ "token": "t", "expires_at": 1_700_000_000, "refresh": "r" });
    lock.write(&value).unwrap();

    assert_eq!(lock.read().unwrap(), Some(value));
}

#[test]
fn the_file_is_0600_in_0700_directories() {
    let home = TempDir::new("cred-file");
    let lock = open(home.path()).try_lock().unwrap().unwrap();
    lock.write(&json!({ "token": "t" })).unwrap();

    let dir = home.path().join("credentials");
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&dir.join("acme")), 0o700);
    assert_eq!(mode(&dir.join("acme/default")), 0o600);
    assert_eq!(mode(&dir.join("acme/default.lock")), 0o600);
}

#[test]
fn an_existing_directory_keeps_its_mode() {
    let home = TempDir::new("cred-file");
    let dir = home.path().join("credentials/acme");
    fs::create_dir_all(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o750)).unwrap();

    let lock = open(home.path()).try_lock().unwrap().unwrap();
    lock.write(&json!({ "token": "t" })).unwrap();

    assert_eq!(mode(&dir), 0o750);
}

#[test]
fn a_writer_never_exposes_a_file_wider_than_0600() {
    let home = TempDir::new("cred-file");
    let file = open(home.path());
    let lock = file.try_lock().unwrap().unwrap();
    lock.write(&json!({ "token": "first" })).unwrap();
    let dir = home.path().join("credentials/acme");
    const DEADLINE: Duration = Duration::from_secs(5);
    let (paused_tx, paused) = mpsc::channel();

    thread::scope(|scope| {
        // Owned inside the scope: unwinding drops the sender before the
        // scope's implicit join, disconnecting the held writer.
        let (release_tx, release) = mpsc::channel();
        scope.spawn(|| {
            crate::write::before_rename(move || {
                paused_tx.send(()).unwrap();
                release.recv().expect("the test released the writer");
            });
            lock.write(&json!({ "token": "t" })).unwrap();
        });
        Deadline::after(DEADLINE)
            .recv(&paused)
            .expect("the writer paused between creating the temporary file and renaming it");
        let mut temporaries = 0;
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "tmp") {
                temporaries += 1;
            }
            assert_eq!(mode(&path), 0o600, "{}", path.display());
        }
        assert_eq!(
            temporaries,
            1,
            "expected exactly one temporary file in {}",
            dir.display()
        );
        release_tx.send(()).unwrap();
    });

    let names: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert!(
        names.iter().all(|n| !n.to_string_lossy().ends_with(".tmp")),
        "a temporary file remains in {}",
        dir.display()
    );
    assert_eq!(lock.read().unwrap(), Some(json!({ "token": "t" })));
}

#[test]
fn two_handles_on_one_path_exclude_each_other() {
    let home = TempDir::new("cred-file");
    let first = open(home.path());
    let second = open(home.path());

    let held = first.try_lock().unwrap().unwrap();
    assert!(second.try_lock().unwrap().is_none());
    drop(held);
    assert!(second.try_lock().unwrap().is_some());
}

#[test]
fn a_write_that_fails_before_the_rename_leaves_the_old_bytes() {
    let home = TempDir::new("cred-file");
    let lock = open(home.path()).try_lock().unwrap().unwrap();
    lock.write(&json!({ "token": "old" })).unwrap();
    let dir = home.path().join("credentials/acme");
    let before = fs::read(dir.join("default")).unwrap();

    fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
    let failed = lock.write(&json!({ "token": "new" }));
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();

    assert!(matches!(failed, Err(ConfigError::Io { .. })));
    assert_eq!(fs::read(dir.join("default")).unwrap(), before);
    let names: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert!(names.iter().all(|n| !n.to_string_lossy().ends_with(".tmp")));
}

#[test]
fn a_name_that_is_not_one_file_name_is_refused() {
    let home = TempDir::new("cred-file");
    for (provider, label) in [
        ("", "default"),
        (".", "default"),
        ("..", "default"),
        ("a/b", "default"),
        ("acme", ""),
        ("acme", ".."),
        ("acme", "x/y"),
        ("acme", "nul\0"),
        ("acme", "default.lock"),
        ("acme", "x.lock"),
        ("acme", "x.tmp"),
    ] {
        assert!(
            matches!(
                CredentialFile::new(home.path(), provider, label),
                Err(ConfigError::SecretName { .. })
            ),
            "{provider:?} {label:?}"
        );
    }
}

#[test]
fn a_symbolic_link_directory_is_refused() {
    let home = TempDir::new("cred-file");
    let elsewhere = TempDir::new("cred-elsewhere");
    std::os::unix::fs::symlink(elsewhere.path(), home.path().join("credentials")).unwrap();
    assert!(matches!(
        CredentialFile::new(home.path(), "acme", "default"),
        Err(ConfigError::NotPlain { .. })
    ));
    fs::remove_file(home.path().join("credentials")).unwrap();

    fs::create_dir(home.path().join("credentials")).unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), home.path().join("credentials/acme")).unwrap();
    assert!(matches!(
        CredentialFile::new(home.path(), "acme", "default"),
        Err(ConfigError::NotPlain { .. })
    ));
    assert_eq!(fs::read_dir(elsewhere.path()).unwrap().count(), 0);
}

#[test]
fn a_link_planted_after_new_is_refused_at_the_lock() {
    let home = TempDir::new("cred-file");
    let elsewhere = TempDir::new("cred-elsewhere");
    let file = open(home.path());
    std::os::unix::fs::symlink(elsewhere.path(), home.path().join("credentials")).unwrap();

    assert!(matches!(file.try_lock(), Err(ConfigError::NotPlain { .. })));
    assert_eq!(fs::read_dir(elsewhere.path()).unwrap().count(), 0);
}

#[test]
fn a_credential_file_that_is_a_symbolic_link_is_not_read() {
    let home = TempDir::new("cred-file");
    let elsewhere = TempDir::new("cred-elsewhere");
    fs::write(elsewhere.path().join("secret"), br#"{"token":"x"}"#).unwrap();
    let dir = home.path().join("credentials/acme");
    fs::create_dir_all(&dir).unwrap();
    std::os::unix::fs::symlink(elsewhere.path().join("secret"), dir.join("default")).unwrap();

    let lock = open(home.path()).try_lock().unwrap().unwrap();
    assert!(matches!(lock.read(), Err(ConfigError::NotPlain { .. })));
}

#[test]
fn a_stored_file_that_is_not_json_is_an_error() {
    let home = TempDir::new("cred-file");
    let dir = home.path().join("credentials/acme");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("default"), b"not json").unwrap();

    let lock = open(home.path()).try_lock().unwrap().unwrap();
    assert!(matches!(lock.read(), Err(ConfigError::Json { .. })));
}

#[test]
fn a_directory_replaced_by_a_link_under_a_live_guard_is_refused() {
    let home = TempDir::new("cred-file");
    let elsewhere = TempDir::new("cred-elsewhere");
    let lock = open(home.path()).try_lock().unwrap().unwrap();
    lock.write(&json!({ "token": "t" })).unwrap();
    let provider = home.path().join("credentials/acme");
    fs::rename(&provider, home.path().join("moved")).unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), &provider).unwrap();

    assert!(matches!(lock.read(), Err(ConfigError::NotPlain { .. })));
    assert!(matches!(
        lock.write(&json!({ "token": "u" })),
        Err(ConfigError::NotPlain { .. })
    ));
    assert_eq!(fs::read_dir(elsewhere.path()).unwrap().count(), 0);
}

#[test]
fn a_credential_removed_under_the_guard_reads_as_nothing() {
    let home = TempDir::new("cred-file");
    let lock = open(home.path()).try_lock().unwrap().unwrap();
    let value = json!({ "token": "t" });
    lock.write(&value).unwrap();
    assert_eq!(lock.read().unwrap(), Some(value));

    fs::remove_file(home.path().join("credentials/acme/default")).unwrap();
    assert_eq!(lock.read().unwrap(), None);
}
