//! Staging an extension and putting it in place (`docs/extensions.md`,
//! "Installing"): the extension's directory is copied whole into `extensions/<name>/` in Fiber
//! home, and renamed into place only once every check has passed, so a
//! failed install leaves nothing behind.

use std::fs;
use std::io::{self, ErrorKind, Write};
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use config::{Manifest, ProviderData};

use crate::resolve::version;
use crate::{API, Error};

/// Where an installed extension came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Provenance {
    /// A directory on this machine, as an absolute path.
    Path(PathBuf),
    /// A git repository, at this exact commit.
    Git {
        /// The commit.
        commit: String,
    },
}

/// What `extensions/<name>/.fiber.json` records about an install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Record {
    /// The extension's full name: its directory name is a slug of it.
    pub(crate) name: String,
    pub(crate) provenance: Provenance,
    pub(crate) version: String,
    /// Asked for, rather than pulled in as a dependency.
    pub(crate) requested: bool,
}

/// The file beside the manifest that holds the [`Record`], so removing the
/// directory removes it too.
pub(crate) const RECORD: &str = ".fiber.json";

impl Record {
    fn to_json(&self) -> serde_json::Value {
        let source = match &self.provenance {
            Provenance::Path(path) => serde_json::json!({ "path": path }),
            Provenance::Git { commit } => serde_json::json!({ "commit": commit }),
        };
        serde_json::json!({
            "name": self.name,
            "version": self.version,
            "requested": self.requested,
            "source": source,
        })
    }

    /// Reads the record in `dir`; every way it can be wrong names the file.
    pub(crate) fn read(dir: &Path) -> Result<Self, Error> {
        let file = dir.join(RECORD);
        let bad = |why: &str| Error::BadRecord {
            path: file.clone(),
            why: why.into(),
        };
        let text = fs::read_to_string(&file).map_err(|e| bad(&e.to_string()))?;
        let json: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| bad(&e.to_string()))?;
        let text = |v: &serde_json::Value, key: &str| {
            v.get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        };
        let source = json.get("source").ok_or_else(|| bad("no `source`"))?;
        let provenance = match (text(source, "commit"), text(source, "path")) {
            (Some(commit), None) => Provenance::Git { commit },
            (None, Some(path)) => Provenance::Path(PathBuf::from(path)),
            _ => return Err(bad("`source` is neither a commit nor a path")),
        };
        Ok(Self {
            name: text(&json, "name").ok_or_else(|| bad("no `name`"))?,
            version: text(&json, "version").ok_or_else(|| bad("no `version`"))?,
            requested: json
                .get("requested")
                .and_then(serde_json::Value::as_bool)
                .ok_or_else(|| bad("no `requested`"))?,
            provenance,
        })
    }
}

/// Where a staged copy waits, and where it goes.
#[derive(Clone)]
pub(crate) struct Paths {
    pub(crate) fresh: PathBuf,
    target: PathBuf,
    old: PathBuf,
}

/// Checks the extension at `source` and copies it, with its record, to a
/// fresh directory beside where it will live; nothing installed changes.
/// `id` is unique to the plan, so two plans never share a directory.
pub(crate) fn stage(
    home: &Path,
    id: usize,
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
    let pid = std::process::id();
    let paths = Paths {
        target: root.join(&slug),
        fresh: root.join(format!(".{slug}.{pid}.{id}.new")),
        old: root.join(format!(".{slug}.{pid}.{id}.old")),
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
/// copies already moved, and a copy that cannot be put back is an error
/// naming it.
pub(crate) fn commit_all(
    staged: &[Paths],
    rename: impl Fn(&Path, &Path) -> io::Result<()>,
) -> Result<(), Error> {
    let mut done: Vec<&Paths> = Vec::new();
    for p in staged {
        if let Err(e) = swap(&p.fresh, &p.target, &p.old, &rename) {
            let mut stuck = Vec::new();
            for d in done.iter().rev() {
                let restored = remove(&d.target).and_then(|()| {
                    if fs::symlink_metadata(&d.old).is_ok() {
                        rename(&d.old, &d.target).map_err(io(&d.old))
                    } else {
                        Ok(())
                    }
                });
                if restored.is_err() {
                    stuck.push(d.target.clone());
                }
            }
            for p in staged {
                remove(&p.fresh).unwrap_or(());
            }
            return Err(if stuck.is_empty() {
                e
            } else {
                Error::Rollback {
                    why: e.to_string(),
                    stuck,
                }
            });
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

/// Writes the record as a new file: a link a package carries under that name
/// is replaced, never written through.
fn write_record(dir: &Path, record: &Record) -> Result<(), Error> {
    let file = dir.join(RECORD);
    if fs::symlink_metadata(&file).is_ok() {
        fs::remove_file(&file).map_err(io(&file))?;
    }
    let mut out = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&file)
        .map_err(io(&file))?;
    out.write_all(record.to_json().to_string().as_bytes())
        .map_err(io(&file))
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
