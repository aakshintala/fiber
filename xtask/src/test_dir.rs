//! A temporary directory for tests, removed when dropped.

use std::fs;
use std::hash::BuildHasher;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

pub(crate) struct TestDir(PathBuf);

impl TestDir {
    #[allow(
        clippy::panic,
        reason = "a test directory that cannot be created cannot run its test"
    )]
    pub(crate) fn new(name: &str) -> Self {
        let parent = std::env::temp_dir();
        for _ in 0..64 {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let suffix = format!(
                "{:08x}",
                std::collections::hash_map::RandomState::new().hash_one(n) & 0xffff_ffff
            );
            let path = parent.join(format!("xtask-{name}-{suffix}"));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(err) => panic!("creating {}: {err}", path.display()),
            }
        }
        panic!("no unique directory for prefix xtask-{name}");
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }

    pub(crate) fn write(&self, rel: &str, content: &str) {
        let path = self.0.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap_or(());
    }
}
