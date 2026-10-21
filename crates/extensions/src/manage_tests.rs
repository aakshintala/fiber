//! The superseded-copy cleanup: staging a replacement removes the previous
//! copy first, and a failure there keeps the item for `Plan::drop`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use super::{Item, empty, retire};
use crate::install::{Provenance, Record};

/// A file mode restored on drop, so a failure still leaves the tree
/// removable.
struct RestoreMode {
    path: PathBuf,
    mode: u32,
}

impl RestoreMode {
    fn set(path: &std::path::Path, mode: u32) -> Self {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
        Self {
            path: path.to_owned(),
            mode: 0o755,
        }
    }
}

impl Drop for RestoreMode {
    fn drop(&mut self) {
        fs::set_permissions(&self.path, fs::Permissions::from_mode(self.mode)).unwrap_or(());
    }
}

#[test]
fn retiring_an_absent_name_changes_nothing() {
    let held = fakes::TempDir::new("fiber-retire-absent");
    let clock = fakes::clock::FakeClock::new();
    let mut plan = empty(&held.path().join("home"), &*clock).unwrap();
    retire(&mut plan, "example.com/acme/missing").unwrap();
    assert!(plan.items.is_empty());
}

/// `unlock` releases the lock over `extensions/`: while the plan holds
/// it a second lock attempt fails at once, and after `unlock` it
/// succeeds. `StagedUpdate::commit` releases the lock here, before the
/// docs swap, so the swap and any later operation take it freely.
#[test]
fn unlock_releases_the_extensions_lock() {
    let held = fakes::TempDir::new("fiber-unlock");
    let clock = fakes::clock::FakeClock::new();
    let home = held.path().join("home");
    let mut plan = empty(&home, &*clock).unwrap();
    let lock_path = config::extensions_dir(&home).join(".lock");
    let probe = || {
        std::fs::File::options()
            .read(true)
            .open(&lock_path)
            .unwrap()
            .try_lock()
    };
    // The plan holds the lock, so the release below is what frees it,
    // not the plan's later drop.
    assert!(
        matches!(probe(), Err(std::fs::TryLockError::WouldBlock)),
        "the plan holds the lock"
    );
    plan.unlock();
    assert!(probe().is_ok(), "unlock releases the lock");
}

#[test]
fn a_superseded_copy_that_cannot_be_removed_keeps_its_item() {
    let held = fakes::TempDir::new("fiber-retire");
    let clock = fakes::clock::FakeClock::new();
    let home = held.path().join("home");
    let mut plan = empty(&home, &*clock).unwrap();
    // A real staged extension, so the item holds a manifest and providers.
    let source = held.path().join("src");
    fs::create_dir_all(&source).unwrap();
    fs::write(
        source.join("extension.json"),
        r#"{"name":"acme","version":"v1","fiber":"0.1.0","api":1}"#,
    )
    .unwrap();
    let record = Record {
        name: "acme".into(),
        provenance: Provenance::Path(source.clone()),
        version: "v1".into(),
        requested: true,
        floor: None,
    };
    let (manifest, providers, paths) =
        crate::install::stage(&home, &source, "0.1.0", &record).unwrap();
    let name = manifest.name.clone();
    let fresh = paths.fresh.clone();
    plan.items.insert(
        name.clone(),
        Item {
            name: name.clone(),
            version: "v1".into(),
            provenance: record.provenance.clone(),
            changes: None,
            manifest,
            providers,
            paths,
        },
    );
    // A subdirectory holding files, made unwriteable, so removing the
    // staged copy fails with `PermissionDenied`.
    let sub = fresh.join("sub");
    fs::create_dir(&sub).unwrap();
    fs::write(sub.join("v"), "x").unwrap();
    fs::write(sub.join("probe"), "x").unwrap();
    let restore = RestoreMode::set(&sub, 0o500);
    // Precondition: without write permission the probe cannot go, so a
    // root runner fails here loudly instead of passing vacuously.
    let probe_err = fs::remove_file(sub.join("probe")).unwrap_err();
    assert_eq!(
        probe_err.kind(),
        std::io::ErrorKind::PermissionDenied,
        "{probe_err}"
    );
    let err = retire(&mut plan, &name).unwrap_err();
    assert!(matches!(&err, crate::Error::Io { .. }), "{err}");
    assert!(
        err.to_string().contains(&fresh.display().to_string()),
        "{err}"
    );
    // The previous item stays in the plan, so `Plan::drop` still owns it.
    assert_eq!(
        plan.items.get(&name).map(|item| item.paths.fresh.clone()),
        Some(fresh.clone())
    );
    drop(restore);
    drop(plan);
    assert!(!fresh.exists());
}
