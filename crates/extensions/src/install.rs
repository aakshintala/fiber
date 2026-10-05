//! Staging an extension and putting it in place (`docs/extensions.md`,
//! "Installing"): the extension's directory is copied whole into `extensions/<name>/` in Fiber
//! home, renamed into place only once every check has passed, and only
//! then given its manifest's install step, which runs at that final path.
//! A failed step puts every previous version back, so a failed install
//! leaves nothing behind.

use std::fs::{self, File};
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

/// One directory a commit moves. `had_old` is whether `target` existed
/// before the commit, so a later process can tell a new install from a
/// replacement after the backup is gone.
#[derive(Clone)]
struct Step {
    target: PathBuf,
    old: PathBuf,
    fresh: PathBuf,
    had_old: bool,
}

/// Puts every staged copy in place, then runs each item's step at its
/// target, or puts nothing in place. `run` receives each staged item's
/// index and target path, after every swap has succeeded and before the
/// journal is marked committed. A swap failure and a step failure both roll
/// every swapped copy back: each target is removed and its previous version
/// moved back, so the install stays all or none. The journal
/// `extensions/.commit` is written first. A restore that fails leaves that
/// journal; the next install, update, remove or list finishes it
/// (`recover`).
pub(crate) fn commit_all(
    staged: &[Paths],
    rename: impl Fn(&Path, &Path) -> io::Result<()>,
    run: impl Fn(usize, &Path) -> Result<(), Error>,
) -> Result<(), Error> {
    let Some(dir) = staged
        .first()
        .and_then(|p| p.target.parent())
        .map(Path::to_path_buf)
    else {
        return Ok(());
    };
    let mut steps = Vec::new();
    for p in staged {
        let had_old = match fs::symlink_metadata(&p.target) {
            Ok(_) => true,
            Err(e) if e.kind() == ErrorKind::NotFound => false,
            Err(e) => return Err(io(&p.target)(e)),
        };
        steps.push(Step {
            target: p.target.clone(),
            old: p.old.clone(),
            fresh: p.fresh.clone(),
            had_old,
        });
    }
    write_journal(&dir, false, &steps)?;
    let mut done = Vec::new();
    for (p, step) in staged.iter().zip(&steps) {
        // The step that fails is rolled back too: its own put-back may
        // have failed.
        done.push(step.clone());
        if let Err(e) = swap(&p.fresh, &p.target, &p.old, &rename) {
            return rollback(&dir, staged, &done, &rename, e);
        }
    }
    // Every swap is done, so each step runs at the final path its files
    // keep. A step that fails puts every swapped copy back, including the
    // ones whose steps already ran.
    for (i, p) in staged.iter().enumerate() {
        if let Err(e) = run(i, &p.target) {
            return rollback(&dir, staged, &steps, &rename, e);
        }
    }
    write_journal(&dir, true, &steps)?;
    recover(&dir)
}

/// Finishes `dir/.commit` if a commit stopped halfway. An uncommitted
/// journal is rolled back; a committed one drops the backups.
pub(crate) fn recover(dir: &Path) -> Result<(), Error> {
    let Some((committed, steps)) = read_journal(dir)? else {
        return Ok(());
    };
    if committed {
        for step in &steps {
            remove(&step.old)?;
            remove(&step.fresh)?;
        }
    } else {
        for step in &steps {
            abort_step(step, &|from, to| fs::rename(from, to))?;
        }
    }
    remove_journal(dir)
}

/// Puts every swapped copy back after a failed swap or a failed step.
/// `done` holds the steps that reached their target, including the one
/// that failed. A step's own error is returned unchanged; only a restore
/// that fails becomes [`Error::Rollback`], leaving the journal for the
/// next operation.
fn rollback(
    dir: &Path,
    staged: &[Paths],
    done: &[Step],
    rename: &impl Fn(&Path, &Path) -> io::Result<()>,
    error: Error,
) -> Result<(), Error> {
    let mut stuck = Vec::new();
    for step in done {
        if abort_step(step, rename).is_err() {
            stuck.push(step.target.clone());
        }
    }
    for p in staged {
        remove(&p.fresh).unwrap_or(());
    }
    if stuck.is_empty() {
        remove_journal(dir)?;
        return Err(error);
    }
    Err(Error::Rollback {
        why: error.to_string(),
        stuck,
    })
}

fn abort_step(step: &Step, rename: &impl Fn(&Path, &Path) -> io::Result<()>) -> Result<(), Error> {
    if exists(&step.old)? {
        if exists(&step.target)? {
            remove(&step.target)?;
        }
        rename(&step.old, &step.target).map_err(io(&step.old))?;
    } else if !step.had_old && exists(&step.target)? {
        // A new install has no backup. Whatever landed at `target` goes.
        remove(&step.target)?;
    }
    if exists(&step.fresh)? {
        remove(&step.fresh)?;
    }
    Ok(())
}

fn exists(path: &Path) -> Result<bool, Error> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(false),
        Err(e) => Err(io(path)(e)),
    }
}

fn write_journal(dir: &Path, committed: bool, steps: &[Step]) -> Result<(), Error> {
    let path = dir.join(".commit");
    let tmp = dir.join(".commit.new");
    let body: Vec<serde_json::Value> = steps
        .iter()
        .map(|step| {
            serde_json::json!({
                "target": step.target.display().to_string(),
                "old": step.old.display().to_string(),
                "fresh": step.fresh.display().to_string(),
                "had_old": step.had_old,
            })
        })
        .collect();
    let text = serde_json::json!({ "committed": committed, "steps": body }).to_string();
    let mut file = File::create(&tmp).map_err(io(&tmp))?;
    file.write_all(text.as_bytes()).map_err(io(&tmp))?;
    file.sync_all().map_err(io(&tmp))?;
    fs::rename(&tmp, &path).map_err(io(&path))
}

fn read_journal(dir: &Path) -> Result<Option<(bool, Vec<Step>)>, Error> {
    let path = dir.join(".commit");
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io(&path)(e)),
    };
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| io(&path)(io::Error::other(e)))?;
    let bad = |why: &str| io(&path)(io::Error::other(why));
    let committed = value
        .get("committed")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| bad("no `committed`"))?;
    let raw = value
        .get("steps")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| bad("no `steps`"))?;
    let mut steps = Vec::new();
    for step in raw {
        let field = |key: &str| {
            step.get(key)
                .and_then(serde_json::Value::as_str)
                .map(PathBuf::from)
                .ok_or_else(|| bad("a step is missing a path"))
        };
        steps.push(Step {
            target: field("target")?,
            old: field("old")?,
            fresh: field("fresh")?,
            had_old: step
                .get("had_old")
                .and_then(serde_json::Value::as_bool)
                .ok_or_else(|| bad("no `had_old`"))?,
        });
    }
    Ok(Some((committed, steps)))
}

fn remove_journal(dir: &Path) -> Result<(), Error> {
    let path = dir.join(".commit");
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io(&path)(e)),
    }
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
    let replacing = exists(target)?;
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
