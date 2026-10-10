//! Approvals and pinned copies in Fiber home (`docs/state.md`, "What each
//! part holds"): `projects/<key>/approvals/<hash>` for an extension or a
//! hook, `approvals/<hash>` for an MCP server, and `pinned/<hash>/` for the
//! copy. The approval file is the last thing an approval writes, so an
//! approval never lacks its copy; a kill before it leaves an unfinished
//! copy, which the next approval removes and builds again. A finished copy
//! with no approval is what `fiber sessions prune` collects.

use std::fmt;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use config::ProjectKey;
use contract::clock::Clock;
use contract::events::OfferedKind;
use serde_json::Value;

use super::content::{Index, hash_paths};
use super::declared::RepoItem;
use super::kind_name;
use crate::Error;
use crate::install::{io as io_error, remove};
use crate::prepare::prepare;

/// What a person decided about one content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Approved: the copy may run.
    Approve,
    /// Never: not offered again until the content changes.
    Never,
}

/// The approvals and copies in one Fiber home, for one project.
#[derive(Clone)]
pub struct Store {
    home: PathBuf,
    project: ProjectKey,
    clock: Arc<dyn Clock>,
}

impl fmt::Debug for Store {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Store")
            .field("home", &self.home)
            .field("project", &self.project)
            .finish_non_exhaustive()
    }
}

/// An earlier approved version of an item.
#[derive(Debug)]
pub(crate) struct Previous {
    pub(crate) hash: String,
    /// A hook's or server's declaration then.
    pub(crate) declaration: Option<Value>,
    /// The files its hash covered.
    pub(crate) files: Vec<String>,
}

impl Store {
    /// The store in `home` for `project`. The clock bounds an extension's
    /// install step when its copy is built.
    pub fn new(home: &Path, project: &ProjectKey, clock: Arc<dyn Clock>) -> Self {
        Self {
            home: home.to_path_buf(),
            project: project.clone(),
            clock,
        }
    }

    /// An extension's and a hook's approvals are per project; a server's are
    /// per machine.
    fn approvals(&self, kind: OfferedKind) -> PathBuf {
        match kind {
            OfferedKind::Extension | OfferedKind::Hook => self
                .home
                .join("projects")
                .join(self.project.as_str())
                .join("approvals"),
            OfferedKind::McpServer => self.home.join("approvals"),
        }
    }

    /// Where the copy of a content is kept.
    pub fn copy_dir(&self, hash: &str) -> PathBuf {
        self.home.join("pinned").join(hash)
    }

    /// The decision recorded for this content, if any.
    pub fn decision(&self, kind: OfferedKind, hash: &str) -> Option<Decision> {
        if !is_hash(hash) {
            return None;
        }
        let text = fs::read_to_string(self.approvals(kind).join(hash)).ok()?;
        match serde_json::from_str::<Value>(&text)
            .ok()?
            .get("decision")?
            .as_str()?
        {
            "approve" => Some(Decision::Approve),
            "never" => Some(Decision::Never),
            _ => None,
        }
    }

    /// Copies the item into `pinned/<hash>/`, runs an extension's install
    /// step there, and records the approval, replacing a never. A finished
    /// copy already there is kept; an unfinished one is removed and built
    /// again. Nothing is recorded when a step fails: the copy is removed
    /// and an earlier approval stays.
    pub fn approve(&self, item: &RepoItem, hash: &str) -> Result<(), Error> {
        check(item, hash)?;
        let copy = self.copy_dir(hash);
        self.pin(item, hash, &copy)?;
        self.record(item, hash, "approve")
    }

    /// Records never for this content, replacing an approval. No copy is
    /// made.
    pub fn never(&self, item: &RepoItem, hash: &str) -> Result<(), Error> {
        check(item, hash)?;
        self.record(item, hash, "never")
    }

    /// Builds the copy at its final path, under the lock in
    /// `pinned/.lock`, which serialises concurrent approvals. The path
    /// `pinned/<hash>/` is known before the step runs, so the copy is
    /// built in place there: a step such as creating a Python virtualenv
    /// records its own location. The empty file `pinned/<hash>.ready` is
    /// written last, so a copy with no `.ready` is an unfinished attempt
    /// and is removed and built again, while a copy with one is kept. On
    /// any error after the directory was made, the copy is removed, so a
    /// failing step leaves no copy. A step failure is the step's own
    /// error, unchanged.
    fn pin(&self, item: &RepoItem, hash: &str, copy: &Path) -> Result<(), Error> {
        let pinned = self.home.join("pinned");
        fs::create_dir_all(&pinned).map_err(io_error(&pinned))?;
        let lock_path = pinned.join(".lock");
        let lock = File::options()
            .create(true)
            .append(true)
            .open(&lock_path)
            .map_err(io_error(&lock_path))?;
        lock.lock().map_err(io_error(&lock_path))?;
        let ready = pinned.join(format!("{hash}.ready"));
        if copy.is_dir() && ready.is_file() {
            return Ok(());
        }
        // A missing copy can leave its marker behind. Remove it before
        // rebuilding: a kill mid-rebuild would otherwise leave a partial
        // directory beside the old marker, which the next approval accepts.
        if let Err(e) = fs::remove_file(&ready)
            && e.kind() != io::ErrorKind::NotFound
        {
            return Err(io_error(&ready)(e));
        }
        match fs::symlink_metadata(copy) {
            // Nothing there, or unreadable: `create_dir` below fails naming
            // the path when it is the second.
            Err(_) => {}
            Ok(meta) if meta.file_type().is_dir() => remove(copy)?,
            Ok(_) => fs::remove_file(copy).map_err(io_error(copy))?,
        }
        fs::create_dir(copy).map_err(io_error(copy))?;
        let unfinished = Unfinished(Some(copy.to_path_buf()));
        let mut copied = Vec::new();
        for file in &item.files {
            let to = copy.join(&file.rel);
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent).map_err(io_error(parent))?;
            }
            // `fs::copy` keeps the file's mode, so a script stays executable.
            fs::copy(&file.path, &to).map_err(io_error(&file.path))?;
            copied.push(to);
        }
        let again = hash_paths(
            &mut Index::scratch(),
            &item.name,
            item.kind,
            item.declaration.as_ref(),
            item.files
                .iter()
                .map(|f| f.rel.as_str())
                .zip(copied.iter().map(PathBuf::as_path)),
        )?;
        if again != hash {
            return Err(Error::ChangedWhileCopying {
                item: item.name.clone(),
            });
        }
        if item.kind == OfferedKind::Extension {
            let manifest = config::read_manifest(copy)?;
            prepare(copy, &manifest, self.clock.as_ref())?;
        }
        File::create(&ready).map_err(io_error(&ready))?;
        unfinished.done();
        Ok(())
    }

    fn record(&self, item: &RepoItem, hash: &str, decision: &str) -> Result<(), Error> {
        let dir = self.approvals(item.kind);
        fs::create_dir_all(&dir).map_err(io_error(&dir))?;
        let name = Value::String(item.name.clone()).to_string();
        let declaration = item
            .declaration
            .as_ref()
            .map(|d| format!(",\"declaration\":{d}"))
            .unwrap_or_default();
        let files: Vec<&str> = item.files.iter().map(|f| f.rel.as_str()).collect();
        let files = Value::from(files);
        let body = format!(
            "{{\"decision\":\"{decision}\",\"kind\":\"{}\",\"name\":{name}{declaration},\"files\":{files}}}\n",
            kind_name(item.kind)
        );
        let target = dir.join(hash);
        let temp = dir.join(format!(".tmp-{}-{}", std::process::id(), next()));
        let written = fs::write(&temp, body).and_then(|()| fs::rename(&temp, &target));
        if let Err(e) = written {
            fs::remove_file(&temp).unwrap_or(());
            return Err(io_error(&target)(e));
        }
        Ok(())
    }

    /// The newest approved version of this item, other than `hash`, whose
    /// copy is still there.
    pub(crate) fn previous(&self, kind: OfferedKind, name: &str, hash: &str) -> Option<Previous> {
        let (_, hash, body) = fs::read_dir(self.approvals(kind))
            .ok()?
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let file = entry.file_name().to_string_lossy().into_owned();
                if !is_hash(&file) || file == hash || !self.copy_dir(&file).is_dir() {
                    return None;
                }
                let body: Value =
                    serde_json::from_str(&fs::read_to_string(entry.path()).ok()?).ok()?;
                let same = body.get("decision").and_then(Value::as_str) == Some("approve")
                    && body.get("kind").and_then(Value::as_str) == Some(kind_name(kind))
                    && body.get("name").and_then(Value::as_str) == Some(name);
                let modified = entry.metadata().and_then(|m| m.modified()).ok()?;
                same.then_some((modified, file, body))
            })
            .max_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)))?;
        Some(Previous {
            hash,
            declaration: body.get("declaration").cloned(),
            files: body
                .get("files")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|f| f.as_str().map(str::to_owned))
                .collect(),
        })
    }
}

/// A copy being built: dropped unfinished, the directory goes with it, so
/// a failing step leaves no copy. [`Option::take`] disarms it once the
/// `.ready` marker is written.
struct Unfinished(Option<PathBuf>);

impl Unfinished {
    fn done(mut self) {
        self.0 = None;
    }
}

impl Drop for Unfinished {
    fn drop(&mut self) {
        if let Some(dir) = self.0.take() {
            remove(&dir).unwrap_or(());
        }
    }
}

/// A name in Fiber home comes from a hash, never from the repository's text.
fn check(item: &RepoItem, hash: &str) -> Result<(), Error> {
    if is_hash(hash) {
        Ok(())
    } else {
        Err(Error::Pin {
            item: item.name.clone(),
            why: "that is not a content hash".to_owned(),
        })
    }
}

fn is_hash(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

pub(super) fn next() -> usize {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}
