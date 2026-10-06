//! What is installed, and `fiber extension remove` (`docs/extensions.md`, "Installing").

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File, TryLockError};
use std::path::{Path, PathBuf};
use std::time::Duration;

use contract::clock::Clock;

use crate::Error;
use crate::git::{full_name, short_name};
use crate::install::{Provenance, Record, io, recover, remove, slug};

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

/// How many times, ten milliseconds apart, a second operation tries for the
/// lock before it fails. A process another thread just started can hold a
/// copy of the lock's file for a moment after its owner let go.
const LOCK_TRIES: u32 = 50;

/// The file lock over `extensions/.lock`. A plan keeps one across approval.
pub(crate) struct Lock {
    /// Kept so dropping the lock releases it.
    #[allow(dead_code, reason = "dropping the file releases the advisory lock")]
    file: File,
}

/// Takes the one lock over `extensions/` that every install, update, remove
/// and list holds from its first read to its last write. The file is
/// `extensions/.lock`. A second operation waits half a second on `clock`,
/// then fails with [`Error::Busy`]. A commit that stopped halfway is
/// finished before the lock is returned.
pub(crate) fn lock(home: &Path, clock: &dyn Clock) -> Result<Lock, Error> {
    let root = home.join("extensions");
    fs::create_dir_all(&root).map_err(io(&root))?;
    let path = root.join(".lock");
    let file = File::options()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(io(&path))?;
    for _ in 0..LOCK_TRIES {
        match file.try_lock() {
            Ok(()) => {
                recover(&root)?;
                return Ok(Lock { file });
            }
            Err(TryLockError::WouldBlock) => clock.sleep(Duration::from_millis(10)),
            Err(TryLockError::Error(e)) => return Err(io(&path)(e)),
        }
    }
    Err(Error::Busy)
}

/// An `extensions/<dir>/` whose install record is missing or unreadable.
///
/// Any failure of the record's read is damage: a missing file, an I/O
/// error, invalid JSON, or a missing or mistyped key. The record is read
/// before the manifest, so a damaged directory never surfaces a manifest
/// error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Damaged {
    /// The manifest's name, or the directory's name when the manifest
    /// does not read.
    pub name: String,
    /// The directory's file name (its slug).
    pub(crate) dir: String,
}

impl fmt::Display for Damaged {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let shown = short_name(&self.name);
        write!(
            f,
            "`{shown}` is damaged; run `fiber extension remove {shown}`, then install it again."
        )
    }
}

impl Damaged {
    /// The skip line, without the `fiber: ` prefix: a damaged extension's
    /// dependency minimums are unknown, so the versions chosen did not
    /// count them.
    pub fn skipped(&self) -> String {
        let shown = short_name(&self.name);
        format!(
            "`{shown}` is damaged, so its dependency minimums are unknown and the versions chosen did not count them; run `fiber extension remove {shown}`, then install it again."
        )
    }
}

/// What is installed: the healthy extensions and the damaged directories.
/// A manifest that cannot be read beside a healthy record stays a hard
/// error, as today.
#[derive(Debug, Default)]
pub struct Listing {
    /// The healthy extensions, sorted by name, as today.
    pub installed: Vec<Installed>,
    /// The damaged directories, sorted by name.
    pub damaged: Vec<Damaged>,
}

/// The installed extensions, by name. A directory whose install record is
/// missing or cannot be read is listed as damaged instead of failing the
/// command. A manifest that cannot be read beside a healthy record is an
/// error naming the file. A commit that stopped halfway is finished
/// first, under the same lock every other operation takes.
pub fn list(home: &Path, clock: &dyn Clock) -> Result<Listing, Error> {
    let root = home.join("extensions");
    if !root.try_exists().map_err(io(&root))? {
        return Ok(Listing::default());
    }
    let _lock = lock(home, clock)?;
    read(home)
}

/// The installed extensions, without taking the lock. The caller holds it
/// and has already finished any commit that stopped halfway.
pub(crate) fn read(home: &Path) -> Result<Listing, Error> {
    let root = home.join("extensions");
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Listing::default()),
        Err(e) => return Err(io(&root)(e)),
    };
    let mut installed = Vec::new();
    let mut damaged = Vec::new();
    for entry in entries {
        let entry = entry.map_err(io(&root))?;
        let dir = entry.path();
        let dir_name = entry.file_name().to_string_lossy().into_owned();
        if dir_name.starts_with('.') || !dir.is_dir() {
            continue;
        }
        let record = match Record::read(&dir) {
            Ok(record) => record,
            Err(_) => {
                let name = config::read_manifest(&dir)
                    .map(|manifest| manifest.name)
                    .unwrap_or_else(|_| dir_name.clone());
                damaged.push(Damaged {
                    name,
                    dir: dir_name,
                });
                continue;
            }
        };
        let manifest = config::read_manifest(&dir)?;
        installed.push(Installed {
            name: manifest.name,
            version: record.version,
            provenance: record.provenance,
            requested: record.requested,
            depends: manifest.depends,
        });
    }
    installed.sort_by(|a, b| a.name.cmp(&b.name));
    damaged.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Listing { installed, damaged })
}

/// What `fiber extension remove` will delete, worked out under the lock. Dropping it
/// deletes nothing.
pub struct Removal {
    /// The extensions that go: the one asked for, then each dependency
    /// nothing left needs.
    pub names: Vec<String>,
    /// Their data directories and settings files that exist.
    pub data: Vec<PathBuf>,
    dirs: Vec<PathBuf>,
    _lock: Lock,
}

impl Removal {
    /// Deletes the extensions, their data directories and their settings.
    pub fn commit(self) -> Result<(), Error> {
        for path in &self.data {
            present(path)?;
        }
        for dir in &self.dirs {
            remove(dir)?;
        }
        for path in &self.data {
            // `present` already ran. A path that vanished since then is
            // already gone; any other failure names the path.
            let gone = if path.is_dir() {
                fs::remove_dir_all(path)
            } else {
                fs::remove_file(path)
            };
            match gone {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(io(path)(e)),
            }
        }
        Ok(())
    }
}

/// Whether `path` is there. [`std::io::ErrorKind::NotFound`] is absence;
/// any other metadata error fails, naming `path`.
fn present(path: &Path) -> Result<bool, Error> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(io(path)(e)),
    }
}

/// Works out what removing `typed` deletes (`docs/state.md`, "Extension
/// data"; `docs/configuration.md`, "Extension settings").
pub fn removal(home: &Path, typed: &str, clock: &dyn Clock) -> Result<Removal, Error> {
    let lock = lock(home, clock)?;
    let name = full_name(typed);
    let listing = read(home)?;
    // A damaged directory is removed by its directory name, without
    // reading its record, so removal always works.
    if let Some(hit) = listing
        .damaged
        .iter()
        .find(|hit| slug(&name).is_ok_and(|mine| mine == hit.dir))
    {
        let layers = layers(home)?;
        return Ok(Removal {
            names: vec![hit.name.clone()],
            data: existing_data(&layers, &hit.dir)?,
            dirs: vec![home.join("extensions").join(&hit.dir)],
            _lock: lock,
        });
    }
    let mut left = listing.installed;
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
    let layers = layers(home)?;
    let mut dirs = Vec::new();
    let mut data = Vec::new();
    for name in &names {
        let dir = slug(name)?;
        dirs.push(home.join("extensions").join(&dir));
        data.extend(existing_data(&layers, &dir)?);
    }
    Ok(Removal {
        names,
        data,
        dirs,
        _lock: lock,
    })
}

/// Fiber home and each project in it: the layers a removal reads data
/// and settings from.
fn layers(home: &Path) -> Result<Vec<PathBuf>, Error> {
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
    Ok(layers)
}

/// The data directories and settings files that exist for the extension
/// directory `dir`.
fn existing_data(layers: &[PathBuf], dir: &str) -> Result<Vec<PathBuf>, Error> {
    let mut data = Vec::new();
    for layer in layers {
        for path in [
            layer.join("data").join(dir),
            layer.join("config").join(format!("{dir}.json")),
        ] {
            if present(&path)? {
                data.push(path);
            }
        }
    }
    Ok(data)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "a failure is the test's")]
mod tests {
    use std::fs;

    use super::read;

    #[test]
    fn a_missing_extensions_directory_lists_nothing() {
        let home = fakes::TempDir::new("fiber-read-missing");
        let listing = read(home.path()).unwrap();
        assert!(listing.installed.is_empty());
        assert!(listing.damaged.is_empty());
    }

    #[test]
    fn an_extensions_path_that_is_not_a_directory_is_an_error() {
        let home = fakes::TempDir::new("fiber-read-file");
        fs::write(home.path().join("extensions"), "nope").unwrap();
        let err = read(home.path()).unwrap_err();
        assert!(err.to_string().contains("extensions"), "{err}");
    }
}
