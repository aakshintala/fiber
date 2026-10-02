//! `fiber install <local path>` (`docs/extensions.md`, "Installing"): the
//! extension's directory is copied whole into `extensions/<name>/` in Fiber
//! home, and renamed into place only once every check has passed, so a
//! failed install leaves nothing behind.

use std::fs;
use std::io::{self, ErrorKind};
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use config::{Manifest, ProviderData};

use crate::resolve::version;
use crate::{API, Error};

/// Where a staged copy waits, and where it goes.
#[derive(Clone)]
pub(crate) struct Paths {
    pub(crate) fresh: PathBuf,
    target: PathBuf,
    old: PathBuf,
}

/// What `extensions/<name>/.fiber.json` records about an install.
pub(crate) struct Record {
    /// A local path, or the extension's name.
    pub(crate) source: String,
    pub(crate) version: String,
    /// The exact commit; none for a local path.
    pub(crate) commit: Option<String>,
    /// Asked for, rather than pulled in as a dependency.
    pub(crate) requested: bool,
}

/// The file beside the manifest that holds the [`Record`], so removing the
/// directory removes it too.
pub(crate) const RECORD: &str = ".fiber.json";

/// Installs the extension at `source` into Fiber home, replacing an installed
/// copy of the same name, and returns its name. It refuses an extension whose
/// manifest needs a newer Fiber than `fiber_version` or a different API, and
/// one whose provider data does not read.
pub fn install(home: &Path, source: &Path, fiber_version: &str) -> Result<String, Error> {
    let manifest = config::read_manifest(source)?;
    let record = Record {
        source: source.display().to_string(),
        version: manifest.version,
        commit: None,
        requested: true,
    };
    let (manifest, _, paths) = stage(home, source, fiber_version, &record)?;
    commit_all(&[paths], |from, to| fs::rename(from, to))?;
    Ok(manifest.name)
}

/// Checks the extension at `source` and copies it, with its record, to a
/// fresh directory beside where it will live; nothing installed changes.
pub(crate) fn stage(
    home: &Path,
    source: &Path,
    fiber_version: &str,
    record: &Record,
) -> Result<(Manifest, Vec<ProviderData>, Paths), Error> {
    let manifest = config::read_manifest(source)?;
    if version(&manifest.fiber)? > version(fiber_version)? {
        return Err(Error::NeedsNewerFiber {
            name: manifest.name,
            needs: manifest.fiber,
            running: fiber_version.into(),
        });
    }
    if manifest.api != API {
        return Err(Error::ApiVersion {
            name: manifest.name,
            api: manifest.api,
        });
    }
    let slug = slug(&manifest.name)?;
    let root = home.join("extensions");
    fs::create_dir_all(&root).map_err(io(&root))?;
    overlap(source, &root)?;
    let paths = Paths {
        target: root.join(&slug),
        fresh: root.join(format!(".{slug}.{}.new", std::process::id())),
        old: root.join(format!(".{slug}.{}.old", std::process::id())),
    };
    remove(&paths.fresh)?;
    remove(&paths.old)?;
    let copied = copy(source, &paths.fresh)
        .and_then(|()| write_record(&paths.fresh, record))
        .and_then(|()| config::read_providers(&paths.fresh).map_err(Error::from));
    match copied {
        Ok(providers) => Ok((manifest, providers, paths)),
        Err(e) => {
            remove(&paths.fresh)?;
            Err(e)
        }
    }
}

/// Puts every staged copy in place, or none: a failed move puts back the
/// copies already moved.
pub(crate) fn commit_all(
    staged: &[Paths],
    rename: impl Fn(&Path, &Path) -> io::Result<()>,
) -> Result<(), Error> {
    let mut done: Vec<&Paths> = Vec::new();
    for p in staged {
        if let Err(e) = swap(&p.fresh, &p.target, &p.old, &rename) {
            for d in done.iter().rev() {
                // Best effort: the installed copy is the one that matters.
                remove(&d.target).unwrap_or(());
                if fs::symlink_metadata(&d.old).is_ok() {
                    rename(&d.old, &d.target).unwrap_or(());
                }
            }
            for p in staged {
                remove(&p.fresh).unwrap_or(());
            }
            return Err(e);
        }
        done.push(p);
    }
    // The new copies are in place: a stale copy left behind is skipped by
    // loading and removed by the next install of this extension.
    for p in staged {
        remove(&p.old).unwrap_or(());
    }
    Ok(())
}

fn write_record(dir: &Path, record: &Record) -> Result<(), Error> {
    let file = dir.join(RECORD);
    let json = serde_json::json!({
        "source": record.source,
        "version": record.version,
        "commit": record.commit,
        "requested": record.requested,
    });
    fs::write(&file, json.to_string()).map_err(io(&file))
}

/// Moves `fresh` to `target`. An installed copy is moved to `old` first and
/// moved back if `fresh` cannot take its place, so a failure leaves the
/// installed copy as it was.
fn swap(
    fresh: &Path,
    target: &Path,
    old: &Path,
    rename: impl Fn(&Path, &Path) -> io::Result<()>,
) -> Result<(), Error> {
    let replacing = fs::symlink_metadata(target).is_ok();
    if replacing {
        rename(target, old).map_err(io(target))?;
    }
    if let Err(e) = rename(fresh, target) {
        if replacing {
            rename(old, target).map_err(io(old))?;
        }
        return Err(io(target)(e));
    }
    Ok(())
}

/// Refuses a source that holds `extensions/` or lies inside it: the copy
/// would read what it writes, or replace what it reads.
fn overlap(source: &Path, root: &Path) -> Result<(), Error> {
    let source = fs::canonicalize(source).map_err(io(source))?;
    let root = fs::canonicalize(root).map_err(io(root))?;
    if root.starts_with(&source) || source.starts_with(&root) {
        return Err(Error::Overlaps { path: source });
    }
    Ok(())
}

/// `extensions/<name>/`'s directory name: every `/` becomes `-`, as for a
/// project key (`docs/state.md`, "Extensions").
pub(crate) fn slug(name: &str) -> Result<String, Error> {
    let slug = name.replace('/', "-");
    if slug.is_empty() || slug.starts_with('.') || slug.contains('\0') {
        return Err(Error::BadName { name: name.into() });
    }
    Ok(slug)
}

/// Copies a directory tree, keeping symbolic links as links.
fn copy(from: &Path, to: &Path) -> Result<(), Error> {
    fs::create_dir_all(to).map_err(io(to))?;
    for entry in fs::read_dir(from).map_err(io(from))? {
        let entry = entry.map_err(io(from))?;
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        let kind = entry.file_type().map_err(io(&src))?;
        if kind.is_dir() {
            copy(&src, &dst)?;
        } else if kind.is_symlink() {
            symlink(fs::read_link(&src).map_err(io(&src))?, &dst).map_err(io(&dst))?;
        } else {
            fs::copy(&src, &dst).map_err(io(&dst))?;
        }
    }
    Ok(())
}

pub(crate) fn remove(dir: &Path) -> Result<(), Error> {
    match fs::remove_dir_all(dir) {
        Err(e) if e.kind() != ErrorKind::NotFound => Err(io(dir)(e)),
        Ok(()) | Err(_) => Ok(()),
    }
}

pub(crate) fn io(path: &Path) -> impl Fn(io::Error) -> Error {
    let path: PathBuf = path.to_path_buf();
    move |source| Error::Io {
        path: path.clone(),
        source,
    }
}

#[cfg(test)]
#[path = "install_tests.rs"]
mod tests;
