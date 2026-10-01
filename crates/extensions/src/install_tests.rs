//! The swap that puts a fresh copy in place, with renames that fail on
//! purpose.

use std::cell::Cell;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::swap;

struct Dirs {
    root: PathBuf,
}

impl Dirs {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("fiber-swap-{}-{name}", std::process::id()));
        fs::remove_dir_all(&root).unwrap_or(());
        fs::create_dir_all(root.join("fresh")).unwrap();
        fs::write(root.join("fresh/v"), "new").unwrap();
        fs::create_dir_all(root.join("target")).unwrap();
        fs::write(root.join("target/v"), "old").unwrap();
        Self { root }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn swap(&self, fail_on: usize) -> Result<(), crate::Error> {
        let calls = Cell::new(0);
        swap(
            &self.path("fresh"),
            &self.path("target"),
            &self.path("old"),
            |from: &Path, to: &Path| {
                calls.set(calls.get() + 1);
                if calls.get() == fail_on {
                    Err(io::Error::other("injected"))
                } else {
                    fs::rename(from, to)
                }
            },
        )
    }
}

impl Drop for Dirs {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap_or(());
    }
}

#[test]
fn a_swap_puts_the_fresh_copy_in_place() {
    let dirs = Dirs::new("ok");
    dirs.swap(0).unwrap();
    assert_eq!(fs::read_to_string(dirs.path("target/v")).unwrap(), "new");
    assert!(!dirs.path("fresh").exists());
}

#[test]
fn a_failed_move_aside_leaves_the_installed_copy() {
    let dirs = Dirs::new("aside");
    dirs.swap(1).unwrap_err();
    assert_eq!(fs::read_to_string(dirs.path("target/v")).unwrap(), "old");
}

#[test]
fn a_failed_promotion_restores_the_installed_copy() {
    let dirs = Dirs::new("promote");
    let err = dirs.swap(2).unwrap_err();
    assert_eq!(err.code(), contract::ErrorCode::IoFailed);
    assert_eq!(fs::read_to_string(dirs.path("target/v")).unwrap(), "old");
    assert!(!dirs.path("old").exists());
    assert_eq!(fs::read_to_string(dirs.path("fresh/v")).unwrap(), "new");
}

#[test]
fn a_first_install_has_nothing_to_move_aside() {
    let dirs = Dirs::new("first");
    fs::remove_dir_all(dirs.path("target")).unwrap();
    dirs.swap(2).unwrap();
    assert_eq!(fs::read_to_string(dirs.path("target/v")).unwrap(), "new");
}
