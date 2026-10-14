//! Approvals and pinned copies in Fiber home (`docs/state.md`, "What each
//! part holds"): `projects/<key>/approvals/<hash>` for an extension or a
//! hook, `approvals/<hash>` for an MCP server, and `pinned/<hash>/` for the
//! copy. The approval file is the last thing an approval writes, so an
//! approval never lacks its copy; a kill before it leaves an unfinished
//! copy, which the next approval removes and builds again. A finished copy
//! with no approval is what `fiber sessions prune` collects.

use std::fmt;
use std::fs::{self, File, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

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

impl fmt::Debug for Fetch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The origin's URL closure is configuration, never diagnostics.
        f.debug_struct("Fetch")
            .field("fiber_version", &self.fiber_version)
            .finish_non_exhaustive()
    }
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

impl Store {
    /// The store in `home` for `project`. The clock bounds an extension's
    /// install step when its copy is built.
    pub fn new(home: &Path, project: &ProjectKey, clock: Arc<dyn Clock>) -> Self {
        Self {
            home: home.to_path_buf(),
            project: project.clone(),
            clock,
            fetch: None,
        }
    }

    /// Fetches a dependency-bearing copy's dependencies from `fetch`, on
    /// its clock. Without it, approving an extension whose manifest names
    /// `depends` fails.
    pub fn fetching(mut self, fetch: Fetch) -> Self {
        self.fetch = Some(fetch);
        self
    }

    /// The clock pinning runs on: a fetch's, so fetching and install steps
    /// share one, else the store's own.
    fn pin_clock(&self) -> Arc<dyn Clock> {
        self.fetch
            .as_ref()
            .map(Fetch::clock)
            .unwrap_or_else(|| Arc::clone(&self.clock))
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

    /// Makes the copy again from the worktree when it is missing, and
    /// holds it: never runs the repository's files. Writes nothing under
    /// `approvals/`. A copy that cannot be made is the caller's error.
    pub fn ensure_copy(&self, item: &RepoItem, hash: &str) -> Result<CopyHold, Error> {
        check(item, hash)?;
        let (_lock, pinned) = self.global_lock(item)?;
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
        let (_lock, pinned) = self.global_lock(item)?;
        self.pin_locked(item, hash, copy, &pinned)
    }

    /// `pinned/.lock`, held: what serialises concurrent approvals. The
    /// pinned root is checked before the lock file is even opened, so a
    /// link where the root goes never receives one. The lock is blocking,
    /// including for finished copies: a concurrent approval waits rather
    /// than racing a rebuild.
    fn global_lock(&self, item: &RepoItem) -> Result<(File, PathBuf), Error> {
        let pinned = self.home.join("pinned");
        check_dirs(&item.name, &self.home, &pinned, true)?;
        fs::create_dir_all(&pinned).map_err(io_error(&pinned))?;
        check_dirs(&item.name, &self.home, &pinned, false)?;
        let lock_path = pinned.join(".lock");
        let lock = File::options()
            .create(true)
            .append(true)
            .open(&lock_path)
            .map_err(io_error(&lock_path))?;
        signal_lock_attempt();
        lock.lock().map_err(io_error(&lock_path))?;
        Ok((lock, pinned))
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
        // A link where the marker goes is refused before it is ever
        // opened: only a regular file can be locked or removed.
        if let Ok(meta) = fs::symlink_metadata(&ready)
            && meta.file_type().is_symlink()
        {
            return Err(refused(&item.name, &ready, "is a symbolic link"));
        }
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
                if ready.is_file() {
                    _held = Some(claim(&ready)?);
                }
                if let Err(e) = fs::remove_file(&ready)
                    && e.kind() != io::ErrorKind::NotFound
                {
                    return Err(io_error(&ready)(e));
                }
            }
            Ok(meta) if meta.file_type().is_dir() => {
                if ready.is_file() {
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
            Ok(_) => fs::remove_file(copy).map_err(io_error(copy))?,
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
            let clock = self.pin_clock();
            self.install_depends(item, copy, &manifest, &clock)?;
            prepare(copy, &manifest, clock.as_ref())?;
            // The install step may have replaced dependency ancestors:
            // validate again before publishing completion.
            if !manifest.depends.is_empty() {
                let complete = self.complete(item, copy)?;
                File::create(&complete).map_err(io_error(&complete))?;
            }
        }
        File::create(&ready).map_err(io_error(&ready))?;
        unfinished.done();
        Ok(())
    }

    /// Whether the copy is finished and kept without rebuilding: a hook
    /// or server (which has no dependencies), or an extension whose
    /// manifest names none, or whose dependency completion marker is
    /// present. A dependency-bearing copy without one is legacy and
    /// rebuilds.
    fn finished(&self, item: &RepoItem, copy: &Path) -> Result<bool, Error> {
        if item.kind != OfferedKind::Extension {
            return Ok(true);
        }
        // The copy root was validated as a real directory by the caller.
        let manifest = config::read_manifest(copy)?;
        if manifest.depends.is_empty() {
            return Ok(true);
        }
        let home = copy.join(".fiber-depends");
        if home_absent(&home)? {
            return Ok(false);
        }
        check_marker(&item.name, &self.home, &home.join(".complete"))
    }

    /// The completion marker for a dependency-bearing copy, with its home
    /// validated again: the dependency home and the extensions root are
    /// real directories, so an install step that replaced either fails
    /// the approval instead of publishing over it.
    fn complete(&self, item: &RepoItem, copy: &Path) -> Result<PathBuf, Error> {
        let home = copy.join(".fiber-depends");
        check_dirs(&item.name, &self.home, &home, false)?;
        check_dirs(&item.name, &self.home, &home.join("extensions"), false)?;
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
        check_dirs(&item.name, &self.home, &home.join("extensions"), true)?;
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
        let dir = self.approvals(item.kind);
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

/// Whether the dependency home is absent, so a legacy copy rebuilds. A
/// present home of any kind reads as present: links and non-directories
/// fail later in [`check_marker`]'s ancestor validation, with the same
/// refusal. Only a missing home is absent here: the copy root is
/// validated as a directory before it, and the manifest reads from that
/// same root first, so any other stat failure fails an earlier step too.
// Exempted from mutation testing: no test can fail this missing/other
// mapping while the later steps still succeed, and the missing-home
// rebuild is pinned by the legacy tests.
#[cfg_attr(false, mutants::skip)]
fn home_absent(home: &Path) -> Result<bool, Error> {
    match fs::symlink_metadata(home) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(io_error(home)(e)),
        Ok(_) => Ok(false),
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

pub(super) fn next() -> usize {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
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

#[cfg(not(test))]
fn signal_lock_attempt() {}
