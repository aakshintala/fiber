//! Approvals and pinned copies in Fiber home (`docs/state.md`, "What each
//! part holds"): `projects/<key>/approvals/<hash>` for an extension or a
//! hook, `approvals/<hash>` for an MCP server, and `pinned/<hash>/` for the
//! copy. The approval file is the last thing an approval writes, so an
//! approval never lacks its copy; a kill before it leaves a copy nothing
//! refers to, which `fiber sessions prune` collects.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use config::ProjectKey;
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
#[derive(Debug, Clone)]
pub struct Store {
    home: PathBuf,
    project: ProjectKey,
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
    /// The store in `home` for `project`.
    pub fn new(home: &Path, project: &ProjectKey) -> Self {
        Self {
            home: home.to_path_buf(),
            project: project.clone(),
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
    /// step there, and records the approval, replacing a never. A copy
    /// already there is kept. Nothing is recorded when a step fails: the
    /// scratch copy is removed and an earlier approval stays.
    pub fn approve(&self, item: &RepoItem, hash: &str) -> Result<(), Error> {
        check(item, hash)?;
        let copy = self.copy_dir(hash);
        if !copy.is_dir() {
            self.pin(item, hash, &copy)?;
        }
        self.record(item, hash, "approve")
    }

    /// Records never for this content, replacing an approval. No copy is
    /// made.
    pub fn never(&self, item: &RepoItem, hash: &str) -> Result<(), Error> {
        check(item, hash)?;
        self.record(item, hash, "never")
    }

    fn pin(&self, item: &RepoItem, hash: &str, copy: &Path) -> Result<(), Error> {
        let pinned = self.home.join("pinned");
        fs::create_dir_all(&pinned).map_err(io_error(&pinned))?;
        let scratch = Scratch::new(&pinned)?;
        let mut copied = Vec::new();
        for file in &item.files {
            let to = scratch.0.join(&file.rel);
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
            let manifest = config::read_manifest(&scratch.0)?;
            prepare(&scratch.0, &manifest)?;
        }
        settle(&scratch.0, copy)
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
        let mut found: Vec<(std::time::SystemTime, String, Value)> = Vec::new();
        for entry in fs::read_dir(self.approvals(kind))
            .ok()?
            .filter_map(Result::ok)
        {
            let file = entry.file_name().to_string_lossy().into_owned();
            if !is_hash(&file) || file == hash || !self.copy_dir(&file).is_dir() {
                continue;
            }
            let Ok(text) = fs::read_to_string(entry.path()) else {
                continue;
            };
            let Ok(body) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            let same = body.get("decision").and_then(Value::as_str) == Some("approve")
                && body.get("kind").and_then(Value::as_str) == Some(kind_name(kind))
                && body.get("name").and_then(Value::as_str) == Some(name);
            if let (true, Ok(modified)) = (same, entry.metadata().and_then(|m| m.modified())) {
                found.push((modified, file, body));
            }
        }
        found.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
        let (_, hash, body) = found.pop()?;
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

/// Moves a finished scratch copy to its place. When another `fiber approve`
/// put the same content there first, the copy already there stays: it is the
/// same content, and the scratch is removed with its guard.
pub(super) fn settle(scratch: &Path, copy: &Path) -> Result<(), Error> {
    match fs::rename(scratch, copy) {
        Ok(()) => Ok(()),
        Err(_) if copy.is_dir() => Ok(()),
        Err(e) => Err(io_error(copy)(e)),
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

fn next() -> usize {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// `pinned/.tmp-<pid>-<n>/`, removed when dropped: after the rename there is
/// nothing to remove, and on every error path the half-built copy goes.
struct Scratch(PathBuf);

impl Scratch {
    fn new(pinned: &Path) -> Result<Self, Error> {
        let dir = pinned.join(format!(".tmp-{}-{}", std::process::id(), next()));
        remove(&dir)?;
        fs::create_dir(&dir).map_err(|e: io::Error| io_error(&dir)(e))?;
        Ok(Self(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        remove(&self.0).unwrap_or(());
    }
}
