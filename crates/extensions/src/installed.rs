//! What is installed, and `fiber remove` (`docs/extensions.md`, "Installing").

use std::collections::BTreeMap;
use std::fs::{self, File, TryLockError};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use crate::Error;
use crate::git::full_name;
use crate::install::{Provenance, Record, io, remove, slug};

/// An installed extension.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    /// Its name.
    pub name: String,
    /// Its version: the tag it was fetched at, or a local path's manifest
    /// version.
    pub version: String,
    /// Where it came from.
    pub provenance: Provenance,
    /// Asked for, rather than pulled in as a dependency.
    pub requested: bool,
    /// The extensions it depends on, each with a minimum version.
    pub depends: BTreeMap<String, String>,
}

/// How long a second operation waits for the lock before it fails. A
/// process another thread just started can hold a copy of the lock's file
/// for a moment after its owner let go.
const LOCK_WAIT: Duration = Duration::from_millis(500);

/// Takes the one lock over `extensions/` that every install, update and
/// remove holds from its first read to its last write. A second operation
/// waits [`LOCK_WAIT`], then fails with [`Error::Busy`].
pub(crate) fn lock(home: &Path) -> Result<File, Error> {
    fs::create_dir_all(home).map_err(io(home))?;
    let path = home.join(".extensions.lock");
    let file = File::options()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(io(&path))?;
    let start = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(TryLockError::WouldBlock) if start.elapsed() < LOCK_WAIT => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(TryLockError::WouldBlock) => return Err(Error::Busy),
            Err(TryLockError::Error(e)) => return Err(io(&path)(e)),
        }
    }
}

/// The installed extensions, by name. A manifest or record that cannot be
/// read is an error naming the file.
pub fn list(home: &Path) -> Result<Vec<Installed>, Error> {
    let root = home.join("extensions");
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io(&root)(e)),
    };
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry.map_err(io(&root))?;
        let dir = entry.path();
        if entry.file_name().to_string_lossy().starts_with('.') || !dir.is_dir() {
            continue;
        }
        let manifest = config::read_manifest(&dir)?;
        let record = Record::read(&dir)?;
        found.push(Installed {
            name: manifest.name,
            version: record.version,
            provenance: record.provenance,
            requested: record.requested,
            depends: manifest.depends,
        });
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(found)
}

/// What `fiber remove` will delete, worked out under the lock. Dropping it
/// deletes nothing.
pub struct Removal {
    /// The extensions that go: the one asked for, then each dependency
    /// nothing left needs.
    pub names: Vec<String>,
    /// Their data directories and settings files that exist.
    pub data: Vec<PathBuf>,
    dirs: Vec<PathBuf>,
    _lock: File,
}

impl Removal {
    /// Deletes the extensions, their data directories and their settings.
    pub fn commit(self) -> Result<(), Error> {
        for dir in &self.dirs {
            remove(dir)?;
        }
        for path in &self.data {
            let gone = if path.is_dir() {
                fs::remove_dir_all(path)
            } else {
                fs::remove_file(path)
            };
            match gone {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(io(path)(e)),
                Ok(()) | Err(_) => {}
            }
        }
        Ok(())
    }
}

/// Works out what removing `typed` deletes (`docs/state.md`, "Extension
/// data"; `docs/configuration.md`, "Extension settings").
pub fn removal(home: &Path, typed: &str) -> Result<Removal, Error> {
    let lock = lock(home)?;
    let name = full_name(typed);
    let mut left = list(home)?;
    if !left.iter().any(|i| i.name == name) {
        return Err(Error::NotInstalled { name });
    }
    let mut names = Vec::new();
    let mut next = Some(name);
    while let Some(name) = next {
        left.retain(|i| i.name != name);
        names.push(name);
        next = left
            .iter()
            .find(|i| !i.requested && !left.iter().any(|other| other.depends.contains_key(&i.name)))
            .map(|i| i.name.clone());
    }
    let mut layers = vec![home.to_path_buf()];
    let projects = home.join("projects");
    match fs::read_dir(&projects) {
        Ok(entries) => {
            for entry in entries {
                layers.push(entry.map_err(io(&projects))?.path());
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(io(&projects)(e)),
    }
    let mut dirs = Vec::new();
    let mut data = Vec::new();
    for name in &names {
        let slug = slug(name)?;
        dirs.push(home.join("extensions").join(&slug));
        for layer in &layers {
            for path in [
                layer.join("data").join(&slug),
                layer.join("config").join(format!("{slug}.json")),
            ] {
                if fs::symlink_metadata(&path).is_ok() {
                    data.push(path);
                }
            }
        }
    }
    Ok(Removal {
        names,
        data,
        dirs,
        _lock: lock,
    })
}
