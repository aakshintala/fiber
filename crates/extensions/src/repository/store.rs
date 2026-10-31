//! Approvals and pinned copies in Fiber home (`docs/state.md`, "What each
//! part holds"): `projects/<key>/approvals/<hash>` for an extension or a
//! hook, `approvals/<hash>` for an MCP server, `projects/<key>/servers/<hash>`
//! for each project that approved that server copy, and `pinned/<hash>/`
//! for the copy. An extension or a hook approval writes its approval file
//! last, so an approval never lacks its copy; a kill before it leaves an
//! unfinished copy, which the next approval removes and builds again. A
//! server approval writes the machine approval first and its project
//! record after it. `fiber sessions prune` collects a finished copy only when at least one project's
//! `approvals/<hash>` or `servers/<hash>` names it; a copy with no project
//! record anywhere is kept.

use std::collections::{BTreeMap, BTreeSet};

use std::fmt;
use std::fs::{self, File, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use config::ProjectKey;
use contract::clock::Clock;
use contract::events::OfferedKind;
use serde_json::{Value, json};

use super::content::{Index, hash_paths};
use super::declared::RepoItem;
use super::kind_name;
use crate::Error;
use crate::git::Origin;
use crate::install::{io as io_error, remove};
use crate::installed::LOCK_TRIES;
use crate::manage::plan_depends;
use crate::prepare::prepare;

/// What a person decided about one content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Approved: the copy may run.
    Approve,
    /// Never: not offered again until the content changes.
    Never,
}

/// How a dependency-bearing copy fetches its manifest's dependencies:
/// where they come from, the running Fiber version they must accept, and
/// the one clock both fetching and their install steps run on.
#[derive(Clone)]
pub struct Fetch {
    origin: Arc<Origin>,
    fiber_version: String,
    clock: Arc<dyn Clock>,
}

impl Fetch {
    /// Fetches from `origin`, accepting manifests for `fiber_version`, on
    /// `clock`.
    pub fn new(origin: Origin, fiber_version: impl Into<String>, clock: Arc<dyn Clock>) -> Self {
        Self {
            origin: Arc::new(origin),
            fiber_version: fiber_version.into(),
            clock,
        }
    }

    pub(crate) fn clock(&self) -> Arc<dyn Clock> {
        Arc::clone(&self.clock)
    }
}

/// The approvals and copies in one Fiber home, for one project.
#[derive(Clone)]
pub struct Store {
    home: PathBuf,
    project: ProjectKey,
    clock: Arc<dyn Clock>,
    fetch: Option<Fetch>,
    /// What a repository extension's binary download reports on: the
    /// sink its `extension_release` line is written to. `None` stays
    /// silent. Set with [`Store::with_diagnostics`].
    debug: Option<Arc<dyn contract::diag::DebugLog>>,
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

/// A finished copy, held: the shared lock on `pinned/<hash>.ready`,
/// taken while `pinned/.lock` was held and released on drop. Keeping it
/// for the session's life stops `fiber sessions prune` removing a copy
/// a session runs. Lock order is `pinned/.lock`, then the marker.
#[derive(Debug)]
pub struct CopyHold {
    _ready: File,
}

/// What `remove_copy` found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyRemoval {
    /// The copy is gone, or would be with `dry_run`.
    Removed,
    /// A session holds the copy: nothing was removed.
    Held,
    /// No copy was there: nothing was removed.
    Absent,
    /// The caller's revalidation refused the removal, with why: the copy
    /// stays.
    Refused(String),
}

/// A copy in `pinned/` and who approved it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptCopy {
    /// The content hash: the copy is `pinned/<hash>/`.
    pub hash: String,
    /// The copy's directory.
    pub dir: PathBuf,
    /// The project keys with `projects/<key>/approvals/<hash>` or
    /// `projects/<key>/servers/<hash>`, sorted.
    pub owners: Vec<String>,
    /// Whether the per-machine `approvals/<hash>` names it.
    pub machine_wide: bool,
}

impl Store {
    /// The store in `home` for `project`. The clock bounds an extension's
    /// install step when its copy is built; [`Store::fetching`] replaces
    /// it with the fetch's, so fetching and install steps share one.
    pub fn new(home: &Path, project: &ProjectKey, clock: Arc<dyn Clock>) -> Self {
        Self {
            home: home.to_path_buf(),
            project: project.clone(),
            clock,
            fetch: None,
            debug: None,
        }
    }

    /// Fetches a dependency-bearing copy's dependencies from `fetch`, on
    /// its clock: pinning runs on the fetch's clock, so fetching and
    /// install steps share one. Without it, approving an extension whose
    /// manifest names `depends` fails.
    pub fn fetching(mut self, fetch: Fetch) -> Self {
        self.clock = fetch.clock();
        self.fetch = Some(fetch);
        self
    }

    /// Reports a repository extension's binary download on `debug`: the
    /// sink its `extension_release` line is written to. `None` stays
    /// silent.
    pub fn with_diagnostics(mut self, debug: Option<Arc<dyn contract::diag::DebugLog>>) -> Self {
        self.debug = debug;
        self
    }

    /// An extension's and a hook's approvals are per project; a server's are
    /// per machine.
    fn approvals(&self, kind: OfferedKind) -> PathBuf {
        Self::approvals_in(&self.home, &self.project, kind)
    }

    /// The approvals directory for `home` and `project`: what listing
    /// reads without building a store.
    fn approvals_in(home: &Path, project: &ProjectKey, kind: OfferedKind) -> PathBuf {
        match kind {
            OfferedKind::Extension | OfferedKind::Hook => {
                config::project_approvals_dir(home, project)
            }
            OfferedKind::McpServer => config::machine_approvals_dir(home),
        }
    }

    /// Where the copy of a content is kept.
    pub fn copy_dir(&self, hash: &str) -> PathBuf {
        config::pinned_dir(&self.home).join(hash)
    }

    /// The decision recorded for this content, if any.
    pub fn decision(&self, kind: OfferedKind, hash: &str) -> Option<Decision> {
        Self::decision_in(&self.home, &self.project, kind, hash)
    }

    /// The decision recorded for `home` and `project` about this content,
    /// if any: what listing reads without building a store.
    pub(crate) fn decision_in(
        home: &Path,
        project: &ProjectKey,
        kind: OfferedKind,
        hash: &str,
    ) -> Option<Decision> {
        if !is_hash(hash) {
            return None;
        }
        let text = fs::read_to_string(Self::approvals_in(home, project, kind).join(hash)).ok()?;
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
        // One `pinned/.lock` from the copy through the record: `global_lock`
        // is the only place that takes it, so `approve` and `never` share
        // its order with `ensure_copy` and `remove_copy`. A prune running
        // under the same lock cannot miss the record being written.
        let (_lock, pinned) = self.global_lock(item, Wait::Block)?;
        let copy = self.copy_dir(hash);
        self.pin_locked(item, hash, &copy, &pinned)?;
        pause_before_record(self);
        self.record(item, hash, "approve")
    }

    /// Records never for this content, replacing an approval. No copy is
    /// made. The lock is still held across the record, as in `approve`, so
    /// a prune revalidation running under `pinned/.lock` sees the decision.
    pub fn never(&self, item: &RepoItem, hash: &str) -> Result<(), Error> {
        Self::never_in(&self.home, &self.project, item, hash)
    }

    /// Records `never` for this content without a store: the CLI's
    /// remove, which holds only `&dyn Clock`, never blocks on a copy.
    pub fn never_in(
        home: &Path,
        project: &ProjectKey,
        item: &RepoItem,
        hash: &str,
    ) -> Result<(), Error> {
        check(item, hash)?;
        let (_lock, _pinned) = global_lock_in(home, item, Wait::Block)?;
        Self::record_in(home, project, item, hash, "never")
    }

    /// Under `pinned/.lock`: checks the copy `pinned/<hash>`, runs `check`
    /// (never with `dry_run`), and removes the copy and its marker unless
    /// a session holds it or `check` refused it with a reason. With
    /// `dry_run` nothing is removed, the check never runs, and `Removed`
    /// means the copy would be. Needs no project and no clock.
    pub fn remove_copy(
        home: &Path,
        hash: &str,
        dry_run: bool,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<CopyRemoval, Error> {
        // No path is built from a name that is not a hash.
        if !is_hash(hash) {
            return Err(Error::Pin {
                item: hash.to_owned(),
                why: "that is not a content hash".to_owned(),
            });
        }
        let Some((_lock, pinned)) = pinned_lock(home, hash, false, Wait::Block)? else {
            return Ok(CopyRemoval::Absent);
        };
        let copy = pinned.join(hash);
        let Some(meta) = copy_metadata(&copy)? else {
            return Ok(CopyRemoval::Absent);
        };
        let ready = pinned.join(format!("{hash}.ready"));
        if !marker_state(hash, &ready)? {
            // An unfinished copy has no marker, so no session holds it:
            // only the caller's check can still refuse it.
            if dry_run {
                return Ok(CopyRemoval::Removed);
            }
            if let Err(reason) = check() {
                return Ok(CopyRemoval::Refused(reason));
            }
            remove_copy_dir(&copy, &meta)?;
            return Ok(CopyRemoval::Removed);
        }
        // Read-write without create: macOS refuses `O_CREAT` on a read-only
        // open, and create would forge what ownership is being proved.
        let marker = File::options()
            .read(true)
            .write(true)
            .open(&ready)
            .map_err(io_error(&ready))?;
        // A session running the copy holds the marker shared, so a held
        // copy fails here, changing nothing.
        match marker.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Ok(CopyRemoval::Held),
            Err(TryLockError::Error(e)) => return Err(io_error(&ready)(e)),
        }
        if dry_run {
            return Ok(CopyRemoval::Removed);
        }
        if let Err(reason) = check() {
            return Ok(CopyRemoval::Refused(reason));
        }
        // Still under `pinned/.lock` and the marker lock: nothing renamed
        // the copy or took a hold between the checks above and this.
        fs::remove_file(&ready).map_err(io_error(&ready))?;
        remove_copy_dir(&copy, &meta)?;
        Ok(CopyRemoval::Removed)
    }

    /// Every `pinned/<hash>` that has an approval record: `owners` are the
    /// project keys with `projects/<key>/approvals/<hash>` or
    /// `projects/<key>/servers/<hash>`, sorted, and `machine_wide` is the
    /// per-machine `approvals/<hash>`. An approval
    /// without its copy is not listed. Takes no lock. Any listing error
    /// other than `NotFound` is `Err` naming the path.
    pub fn kept_copies(home: &Path) -> Result<Vec<KeptCopy>, Error> {
        let mut owners: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let projects = home.join("projects");
        match fs::read_dir(&projects) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry.map_err(io_error(&projects))?;
                    let path = entry.path();
                    // Only a real directory is a project: a file or a link
                    // under `projects/` names no owner.
                    if !fs::symlink_metadata(&path)
                        .map_err(io_error(&path))?
                        .file_type()
                        .is_dir()
                    {
                        continue;
                    }
                    let key = entry.file_name().to_string_lossy().into_owned();
                    // A server copy's project records live beside the
                    // extension and hook approvals: each names one more
                    // project that approved the copy.
                    for sub in ["approvals", "servers"] {
                        let dir = path.join(sub);
                        match fs::read_dir(&dir) {
                            Ok(records) => {
                                for record in records {
                                    let record = record.map_err(io_error(&dir))?;
                                    let name = record.file_name().to_string_lossy().into_owned();
                                    if is_hash(&name) {
                                        owners.entry(name).or_default().push(key.clone());
                                    }
                                }
                            }
                            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                            Err(e) => return Err(io_error(&dir)(e)),
                        }
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_error(&projects)(e)),
        }
        let mut machine_wide = BTreeSet::new();
        let global = home.join("approvals");
        match fs::read_dir(&global) {
            Ok(records) => {
                for record in records {
                    let record = record.map_err(io_error(&global))?;
                    let name = record.file_name().to_string_lossy().into_owned();
                    if is_hash(&name) {
                        machine_wide.insert(name);
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_error(&global)(e)),
        }
        let pinned = home.join("pinned");
        let mut hashes: BTreeSet<String> = owners.keys().cloned().collect();
        hashes.extend(machine_wide.iter().cloned());
        let mut kept = Vec::new();
        for hash in hashes {
            let dir = pinned.join(&hash);
            if fs::symlink_metadata(&dir).is_err() {
                continue;
            }
            let mut owned = owners.remove(&hash).unwrap_or_default();
            owned.sort();
            kept.push(KeptCopy {
                dir,
                machine_wide: machine_wide.contains(&hash),
                owners: owned,
                hash,
            });
        }
        Ok(kept)
    }

    /// Makes the copy again from the worktree when it is missing, and
    /// holds it: never runs the repository's files. Writes nothing under
    /// `approvals/`. A copy that cannot be made is the caller's error.
    pub fn ensure_copy(&self, item: &RepoItem, hash: &str) -> Result<CopyHold, Error> {
        self.copy_wait(item, hash, Wait::Block)
    }

    /// As `ensure_copy`, but `pinned/.lock` is tried `installed::LOCK_TRIES` times,
    /// 10 ms apart on the store's clock, then `Error::Busy`: session start and reload never wait
    /// on another process's approval.
    pub fn try_ensure_copy(&self, item: &RepoItem, hash: &str) -> Result<CopyHold, Error> {
        self.copy_wait(item, hash, Wait::Tries(&*self.clock))
    }

    /// Holds a finished copy without building it: only a real directory
    /// with a regular-file `.ready` marker is held, through a shared lock
    /// on the marker tried `installed::LOCK_TRIES` times, 10 ms apart on
    /// the store's clock, then `Error::Busy`. Nothing is created or
    /// written: no `pinned/` directory, no `.lock` file, no copy and no
    /// approval. A missing copy, an unfinished copy, a link or an
    /// unholdable marker is the caller's error to skip.
    pub fn try_hold_finished(&self, hash: &str) -> Result<CopyHold, Error> {
        if !is_hash(hash) {
            return Err(Error::Pin {
                item: hash.to_owned(),
                why: "that is not a content hash".to_owned(),
            });
        }
        let copy = self.copy_dir(hash);
        // Read-only checks before any lock: only a real directory is a
        // copy, so a link or a file where it goes is refused, never
        // followed; only a regular-file marker proves it finished.
        match fs::symlink_metadata(&copy) {
            Ok(meta) if meta.file_type().is_dir() => {}
            Ok(_) => return Err(refused(hash, &copy, "is a symbolic link or a file")),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(refused(hash, &copy, "has no pinned copy"));
            }
            Err(e) => return Err(io_error(&copy)(e)),
        }
        let marker = config::pinned_dir(&self.home).join(format!("{hash}.ready"));
        if !marker_state(hash, &marker)? {
            return Err(refused(hash, &marker, "has no finished copy"));
        }
        // Read-write without create: macOS refuses `O_CREAT` on a
        // read-only open, and create would forge what ownership is being
        // proved.
        let ready = File::options()
            .read(true)
            .write(true)
            .open(&marker)
            .map_err(io_error(&marker))?;
        // The shared hold, tried without ever blocking: another session
        // holding the copy shares the marker, while an approval
        // rebuilding it holds the marker exclusively, so a held marker
        // waits on the store's clock instead of on another process.
        for _ in 0..LOCK_TRIES {
            match ready.try_lock_shared() {
                Ok(()) => return Ok(CopyHold { _ready: ready }),
                Err(TryLockError::WouldBlock) => self.clock.sleep(Duration::from_millis(10)),
                Err(TryLockError::Error(e)) => return Err(io_error(&marker)(e)),
            }
        }
        Err(Error::Busy)
    }

    /// One copy through one wait for `pinned/.lock`: blocking for an
    /// approval or a person-waiting caller, bounded for a session start
    /// or reload that must never wait on another process.
    fn copy_wait(&self, item: &RepoItem, hash: &str, wait: Wait<'_>) -> Result<CopyHold, Error> {
        check(item, hash)?;
        let (_lock, pinned) = self.global_lock(item, wait)?;
        let copy = self.copy_dir(hash);
        self.pin_locked(item, hash, &copy, &pinned)?;
        // Still under `pinned/.lock`: the shared hold on a finished copy.
        // The marker exists: `pin_locked` writes it last, and keeps a
        // finished copy. Read-write without create: macOS refuses
        // `O_CREAT` on a read-only open.
        let marker = pinned.join(format!("{hash}.ready"));
        let ready = File::options()
            .read(true)
            .write(true)
            .open(&marker)
            .map_err(io_error(&marker))?;
        ready.lock_shared().map_err(io_error(&marker))?;
        Ok(CopyHold { _ready: ready })
    }

    /// `pinned/.lock`, held: what serialises concurrent approvals. The
    /// pinned root is checked before the lock file is even opened, so a
    /// link where the root goes never receives one. The wait is blocking
    /// for an approval, including for finished copies, so a concurrent
    /// approval waits rather than racing a rebuild; bounded for a session
    /// start or reload, so it never waits on another process.
    fn global_lock(&self, item: &RepoItem, wait: Wait<'_>) -> Result<(File, PathBuf), Error> {
        global_lock_in(&self.home, item, wait)
    }

    /// The copy itself, with `pinned/.lock` already held: keeping a
    /// finished copy, rebuilding a legacy dependency-bearing one under
    /// exclusive marker ownership, and building anything else in place.
    /// The empty file `pinned/<hash>.ready` is written last, after the
    /// dependency completion marker, so a copy with no `.ready` is an
    /// unfinished attempt and is removed and built again. On any error
    /// after the directory was made, the copy is removed, so a failing
    /// step leaves no copy. A step failure is the step's own error,
    /// unchanged.
    fn pin_locked(
        &self,
        item: &RepoItem,
        hash: &str,
        copy: &Path,
        pinned: &Path,
    ) -> Result<(), Error> {
        let ready = pinned.join(format!("{hash}.ready"));
        // A link or any other non-file where the marker goes is refused
        // before it is ever opened or anything is deleted: only a regular
        // file can be locked or removed.
        let marked = marker_state(&item.name, &ready)?;
        // The exclusive marker hold, kept until the build below is done:
        // proving ownership before deleting or replacing anything with a
        // ready file leaves a held copy alone.
        let mut _held: Option<File> = None;
        match fs::symlink_metadata(copy) {
            // Nothing there, or unreadable: `create_dir` below fails naming
            // the path when it is the second.
            Err(_) => {
                // A missing copy can leave its marker behind. Proving
                // ownership first leaves a held copy alone; removing it
                // before rebuilding means a kill mid-rebuild cannot leave
                // a partial directory beside the old marker, which the
                // next approval would accept.
                if marked {
                    _held = Some(claim(&ready)?);
                }
                if let Err(e) = fs::remove_file(&ready)
                    && e.kind() != io::ErrorKind::NotFound
                {
                    return Err(io_error(&ready)(e));
                }
            }
            Ok(meta) if meta.file_type().is_dir() => {
                if marked {
                    if self.finished(item, copy)? {
                        return Ok(());
                    }
                    // A legacy dependency-bearing copy: rebuild under
                    // exclusive ownership, keeping the hold until the new
                    // marker is written.
                    _held = Some(claim(&ready)?);
                    remove(copy)?;
                    fs::remove_file(&ready).map_err(io_error(&ready))?;
                } else {
                    remove(copy)?;
                }
            }
            Ok(_) => {
                // A stray file or link where the copy goes: claim a stale
                // marker exactly as the legacy rebuild does, leaving a
                // held copy alone, or the rebuild below fails on it.
                if marked {
                    _held = Some(claim(&ready)?);
                    fs::remove_file(&ready).map_err(io_error(&ready))?;
                }
                fs::remove_file(copy).map_err(io_error(copy))?
            }
        }
        fs::create_dir(copy).map_err(io_error(copy))?;
        let unfinished = Unfinished(Some(copy.to_path_buf()));
        let mut copied = Vec::new();
        for file in &item.files {
            // The first path component `.fiber-depends` holds what the
            // install commits, with its completion marker: a package
            // file there, or under it, could forge either, while a
            // similarly prefixed name cannot.
            if file.rel == ".fiber-depends" || file.rel.starts_with(".fiber-depends/") {
                return Err(refused(
                    &item.name,
                    &file.path,
                    "is reserved for installed dependencies",
                ));
            }
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
            let clock = Arc::clone(&self.clock);
            self.install_depends(item, copy, &manifest, &clock)?;
            let observe = crate::prepare::Observe {
                debug: self.debug.clone(),
                clock: clock.as_ref(),
            };
            prepare(copy, &manifest, clock.as_ref(), &observe)?;
            // The install step may have replaced dependency ancestors or
            // planted either marker: validate again and publish without
            // ever following a link.
            if !manifest.depends.is_empty() {
                let complete = self.complete(item, copy)?;
                publish(&item.name, &complete)?;
            }
        }
        publish(&item.name, &ready)?;
        unfinished.done();
        Ok(())
    }

    /// Whether the copy is finished and kept without rebuilding: a hook
    /// or server (which has no dependencies), or an extension whose
    /// manifest names none, or whose dependency completion marker is
    /// present. A dependency-bearing copy without one is legacy and
    /// rebuilds. Any failure reads as unfinished to a read-only caller,
    /// which skips the copy.
    pub(crate) fn finished(&self, item: &RepoItem, copy: &Path) -> Result<bool, Error> {
        if item.kind != OfferedKind::Extension {
            return Ok(true);
        }
        // The copy root was validated as a real directory by the caller.
        // Confine the manifest before reading it: ancestors already
        // validated, the file itself a regular file, so a link to an
        // outside manifest is refused, never read through.
        let manifest_path = copy.join("extension.json");
        check_dirs(&item.name, &self.home, copy, false)?;
        match fs::symlink_metadata(&manifest_path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(refused(&item.name, &manifest_path, "is a symbolic link"));
            }
            Ok(meta) if !meta.file_type().is_file() => {
                return Err(refused(&item.name, &manifest_path, "is not a regular file"));
            }
            Ok(_) => {}
            // Missing or unreadable: the read below fails naming the path.
            Err(_) => {}
        }
        let manifest = config::read_manifest(copy)?;
        if manifest.depends.is_empty() {
            return Ok(true);
        }
        let home = copy.join(".fiber-depends");
        // A missing home rebuilds through the missing-ok ancestor walk
        // below; every other state refuses or errors the same way.
        check_marker(&item.name, &self.home, &home.join(".complete"))
    }

    /// The completion marker for a dependency-bearing copy, with its home
    /// validated again: the dependency home and the extensions root are
    /// real directories, so an install step that replaced either fails
    /// the approval instead of publishing over it.
    fn complete(&self, item: &RepoItem, copy: &Path) -> Result<PathBuf, Error> {
        let home = copy.join(".fiber-depends");
        check_dirs(&item.name, &self.home, &home, false)?;
        check_dirs(
            &item.name,
            &self.home,
            &config::extensions_dir(&home),
            false,
        )?;
        Ok(home.join(".complete"))
    }

    /// Resolves the manifest's `depends` against an empty installed set and
    /// commits each inside the copy, before its own install step runs, so
    /// two versions never share one (`docs/extensions.md`, "Pinning").
    /// With no dependencies nothing is fetched and no directory is made.
    fn install_depends(
        &self,
        item: &RepoItem,
        copy: &Path,
        manifest: &config::Manifest,
        clock: &Arc<dyn Clock>,
    ) -> Result<(), Error> {
        if manifest.depends.is_empty() {
            return Ok(());
        }
        let Some(fetch) = &self.fetch else {
            return Err(Error::Pin {
                item: item.name.clone(),
                why: "its manifest names dependencies, which need fetching".to_owned(),
            });
        };
        let home = copy.join(".fiber-depends");
        // Checked before creating anything under it, so a link where the
        // home goes never receives a directory.
        check_dirs(&item.name, &self.home, &home, true)?;
        fs::create_dir_all(&home).map_err(io_error(&home))?;
        check_dirs(&item.name, &self.home, &home, false)?;
        check_dirs(&item.name, &self.home, &config::extensions_dir(&home), true)?;
        let planned = plan_depends(
            &home,
            manifest,
            &fetch.fiber_version,
            fetch.origin.as_ref(),
            clock.as_ref(),
        )?;
        planned.commit().map(|_| ())
    }

    fn record(&self, item: &RepoItem, hash: &str, decision: &str) -> Result<(), Error> {
        Self::record_in(&self.home, &self.project, item, hash, decision)
    }

    fn record_in(
        home: &Path,
        project: &ProjectKey,
        item: &RepoItem,
        hash: &str,
        decision: &str,
    ) -> Result<(), Error> {
        let dir = Self::approvals_in(home, project, item.kind);
        fs::create_dir_all(&dir).map_err(io_error(&dir))?;
        let files: Vec<&str> = item.files.iter().map(|f| f.rel.as_str()).collect();
        let files = Value::from(files);
        // Built with `json!` so the key order is the serializer's, never
        // hand-placed text.
        let body = match &item.declaration {
            Some(declaration) => json!({
                "decision": decision,
                "kind": kind_name(item.kind),
                "name": item.name,
                "declaration": declaration,
                "files": files,
            }),
            None => json!({
                "decision": decision,
                "kind": kind_name(item.kind),
                "name": item.name,
                "files": files,
            }),
        }
        .to_string()
            + "\n";
        // The machine approval is written before a server's project record
        // below (for an extension or a hook it is the last write), and
        // the write is atomic: a reader sees the old file or the new
        // one, never half. A directory-sync failure reports `Unsynced`
        // with the file already in place; rewriting the same bytes is
        // idempotent, so a retry is safe.
        let target = dir.join(hash);
        support::write_atomic(&target, body.as_bytes(), 0o666)
            .map_err(|error| config::atomic_error(&target, error))?;
        if item.kind == OfferedKind::McpServer {
            if decision == "approve" {
                // The machine approval above is already in place, written
                // first: a project-record failure returns the error and
                // removes nothing, leaving a copy with no project record,
                // which prune keeps.
                let servers = config::project_servers_dir(home, project);
                fs::create_dir_all(&servers).map_err(io_error(&servers))?;
                let record = servers.join(hash);
                support::write_atomic(&record, body.as_bytes(), 0o666)
                    .map_err(|error| config::atomic_error(&record, error))?;
            } else {
                // A never writes no project record: it removes one an
                // earlier approval left, when present.
                let record = config::project_servers_dir(home, project).join(hash);
                match fs::remove_file(&record) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(io_error(&record)(e)),
                }
            }
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

/// A refusal naming `path`: what it is instead of what was wanted.
fn refused(item: &str, path: &Path, why: &str) -> Error {
    Error::Pin {
        item: item.into(),
        why: format!("{}: {why}", path.display()),
    }
}

/// Every component of `path` from `home` down, `path` included, is a real
/// directory, or missing when `missing_ok` lets a rebuild create it.
/// Symbolic links and files fail naming the component; `home` itself is
/// trusted as given, so a home reached through a link still anchors the
/// walk. An untrusted candidate is never canonicalized into its own root.
fn check_dirs(item: &str, home: &Path, path: &Path, missing_ok: bool) -> Result<(), Error> {
    let rel = path
        .strip_prefix(home)
        .map_err(|_| refused(item, path, "is outside Fiber home"))?;
    let mut prefix = home.to_path_buf();
    for part in rel.components() {
        prefix.push(part);
        match fs::symlink_metadata(&prefix) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(refused(item, &prefix, "is a symbolic link"));
            }
            Ok(meta) if !meta.file_type().is_dir() => {
                return Err(refused(item, &prefix, "is not a directory"));
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound && missing_ok => return Ok(()),
            Err(e) => return Err(io_error(&prefix)(e)),
        }
    }
    Ok(())
}

/// Whether the marker at `path` is present as a regular file: links and
/// non-files fail naming it before it is ever opened, while a missing
/// marker is absent. Ancestors under `home` must be real directories,
/// missing allowed, for a rebuild.
fn check_marker(item: &str, home: &Path, path: &Path) -> Result<bool, Error> {
    if let Some(parent) = path.parent() {
        check_dirs(item, home, parent, true)?;
    }
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(refused(item, path, "is a symbolic link")),
        Ok(meta) if !meta.file_type().is_file() => {
            Err(refused(item, path, "is not a regular file"))
        }
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(io_error(path)(e)),
    }
}

/// Under `pinned/.lock`, for callers without a store.
fn global_lock_in(home: &Path, item: &RepoItem, wait: Wait<'_>) -> Result<(File, PathBuf), Error> {
    // `pinned_lock` with `create` makes the root, so `None` would mean
    // a concurrent removal of `pinned/` itself, which no code does.
    pinned_lock(home, &item.name, true, wait)?.ok_or(Error::Pin {
        item: item.name.clone(),
        why: "pinned/ is missing after it was created".to_owned(),
    })
}

/// How `pinned_lock` waits for `pinned/.lock`: an approval or removal
/// blocks, since a person is waiting on it; a session start or reload
/// tries a bounded number of times, then fails rather than waiting on
/// another process's approval.
#[derive(Clone, Copy)]
enum Wait<'c> {
    Block,
    Tries(&'c dyn Clock),
}

/// `pinned/.lock`, held: what serialises approvals and prune's removals.
/// With `create` the root is made first, as approvals need; without it a
/// missing root is `None` and nothing is created, as removals need. Either
/// way the root is checked before the lock file is even opened, so a link
/// or file where the root goes never receives one. The wait is blocking
/// for an approval or removal, which waits rather than racing a rebuild;
/// bounded for a session start or reload, which fails with [`Error::Busy`]
/// after `LOCK_TRIES` tries, ten milliseconds apart on the wait's clock.
fn pinned_lock(
    home: &Path,
    name: &str,
    create: bool,
    wait: Wait<'_>,
) -> Result<Option<(File, PathBuf)>, Error> {
    let pinned = home.join("pinned");
    check_dirs(name, home, &pinned, true)?;
    if create {
        fs::create_dir_all(&pinned).map_err(io_error(&pinned))?;
    } else if let Err(e) = fs::symlink_metadata(&pinned) {
        if e.kind() == io::ErrorKind::NotFound {
            return Ok(None);
        }
        return Err(io_error(&pinned)(e));
    }
    check_dirs(name, home, &pinned, false)?;
    let lock_path = pinned.join(".lock");
    let lock = File::options()
        .create(true)
        .append(true)
        .open(&lock_path)
        .map_err(io_error(&lock_path))?;
    signal_lock_attempt();
    match wait {
        Wait::Block => {
            lock.lock().map_err(io_error(&lock_path))?;
        }
        Wait::Tries(clock) => {
            let mut held = false;
            for _ in 0..LOCK_TRIES {
                match lock.try_lock() {
                    Ok(()) => {
                        held = true;
                        break;
                    }
                    Err(TryLockError::WouldBlock) => clock.sleep(Duration::from_millis(10)),
                    Err(TryLockError::Error(e)) => return Err(io_error(&lock_path)(e)),
                }
            }
            if !held {
                return Err(Error::Busy);
            }
        }
    }
    Ok(Some((lock, pinned)))
}

/// The copy's metadata, or `None` when no copy is there. Any other error
/// fails naming the copy: `pinned/` is verified searchable under the lock
/// just above, so only a missing copy surfaces without a concurrent
/// permission change no test can stage.
#[cfg_attr(false, mutants::skip)]
fn copy_metadata(copy: &Path) -> Result<Option<fs::Metadata>, Error> {
    match fs::symlink_metadata(copy) {
        Ok(meta) => Ok(Some(meta)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_error(copy)(e)),
    }
}

/// Whether the `.ready` marker is present as a regular file: a link or a
/// non-file fails naming it before it is ever opened, while a missing
/// marker is an unfinished copy. Any other error fails naming the marker:
/// its parent is verified searchable under the lock, so no test can stage
/// another error without a concurrent permission change.
#[cfg_attr(false, mutants::skip)]
fn marker_state(item: &str, ready: &Path) -> Result<bool, Error> {
    match fs::symlink_metadata(ready) {
        Ok(meta) if meta.file_type().is_symlink() => {
            Err(refused(item, ready, "is a symbolic link"))
        }
        Ok(meta) if !meta.file_type().is_file() => {
            Err(refused(item, ready, "is not a regular file"))
        }
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(io_error(ready)(e)),
    }
}

/// Removes the copy: a directory goes through `install::remove`, while a
/// link or file is unlinked, so a link is never followed.
fn remove_copy_dir(copy: &Path, meta: &fs::Metadata) -> Result<(), Error> {
    if meta.file_type().is_dir() {
        remove(copy)
    } else {
        fs::remove_file(copy).map_err(io_error(copy))
    }
}

/// Publishes an empty marker without ever following a link: a marker path
/// that already exists in any form (an install step may have planted either
/// marker as a link) fails the approval with nothing published, and
/// exclusive creation keeps a link planted between the check and the write
/// from ever being followed.
fn publish(item: &str, marker: &Path) -> Result<(), Error> {
    if fs::symlink_metadata(marker).is_ok() {
        return Err(refused(item, marker, "already exists"));
    }
    File::options()
        .write(true)
        .create_new(true)
        .open(marker)
        .map(|_| ())
        .map_err(|e| {
            if e.kind() == io::ErrorKind::AlreadyExists {
                refused(item, marker, "already exists")
            } else {
                io_error(marker)(e)
            }
        })
}

/// Opens the existing marker without creating it and takes it exclusively:
/// a session running the copy holds it shared, so a held copy fails with
/// the existing [`Error::Busy`], changing no byte, inode or approval.
/// Other lock failures propagate. The returned file stays alive across
/// the removal and rebuild it guards.
fn claim(ready: &Path) -> Result<File, Error> {
    // Read-write without create: macOS refuses `O_CREAT` on a read-only
    // open, and create would forge what ownership is being proved.
    let marker = File::options()
        .read(true)
        .write(true)
        .open(ready)
        .map_err(io_error(ready))?;
    match marker.try_lock() {
        Ok(()) => Ok(marker),
        Err(TryLockError::WouldBlock) => Err(Error::Busy),
        Err(TryLockError::Error(e)) => Err(io_error(ready)(e)),
    }
}

fn is_hash(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
static LOCK_ATTEMPT: std::sync::Mutex<Option<std::sync::mpsc::Sender<()>>> =
    std::sync::Mutex::new(None);

/// Sets the test-only signal fired immediately before taking
/// `pinned/.lock`. Only tests set it; production never does.
#[cfg(test)]
pub(crate) fn set_lock_attempt(tx: Option<std::sync::mpsc::Sender<()>>) {
    *LOCK_ATTEMPT.lock().unwrap() = tx;
}

#[cfg(test)]
fn signal_lock_attempt() {
    if let Some(tx) = LOCK_ATTEMPT.lock().unwrap().as_ref() {
        tx.send(()).unwrap_or(());
    }
}

/// A pause armed for one home: the gate is the home, and the channels
/// carry the parked signal and the release.
#[cfg(test)]
type ArmedPause = (
    PathBuf,
    std::sync::mpsc::Sender<()>,
    std::sync::mpsc::Receiver<()>,
);

/// Approvals parked between pinning and recording, keyed by home: only the
/// store whose test armed one pauses, so concurrent approvals in other
/// tests never park on it.
#[cfg(test)]
static RECORD_PAUSES: std::sync::Mutex<Vec<ArmedPause>> = std::sync::Mutex::new(Vec::new());

/// Arms the pause between pinning and recording in `approve` for `home`:
/// while the file `home/approve-pause` exists, the approving thread signals
/// `entered` and blocks until the test releases `release` (dropping the
/// test's sender releases it too). Passing `None` disarms it. Only tests
/// set it; production never pauses. The gate is a file, the same shape as
/// the #2521 repro, because a test cannot pass it through the environment.
#[cfg(test)]
pub(crate) fn set_record_pause(
    home: &Path,
    pause: Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>,
) {
    let mut pauses = RECORD_PAUSES.lock().unwrap();
    pauses.retain(|(registered, _, _)| registered != home);
    if let Some((entered, release)) = pause {
        pauses.push((home.to_owned(), entered, release));
    }
}

/// The file-gated pause in `approve`, between the copy and the record. The
/// lock in `pinned/.lock` is held across it, so a prune running under the
/// same lock waits rather than missing the record.
#[cfg(test)]
fn pause_before_record(store: &Store) {
    let taken = {
        let mut pauses = RECORD_PAUSES.lock().unwrap();
        pauses
            .iter()
            .position(|(registered, _, _)| registered == &store.home)
            .map(|at| pauses.remove(at))
    };
    let Some((_, entered, release)) = taken else {
        return;
    };
    if !store.home.join("approve-pause").exists() {
        return;
    }
    entered.send(()).unwrap_or(());
    release.recv().unwrap_or(());
}

#[cfg(not(test))]
fn pause_before_record(_store: &Store) {}

#[cfg(not(test))]
fn signal_lock_attempt() {}

#[cfg(test)]
#[path = "store_remove_tests.rs"]
mod remove_tests;
