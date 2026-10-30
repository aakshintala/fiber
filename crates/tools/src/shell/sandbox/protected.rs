//! The protected paths across confined shell calls (`docs/sandbox.md`,
//! "What it cannot do" and the restore the sandbox promises): the shared
//! `.git/config` and hooks, each registered worktree's `.git` pointer and
//! workspace-local `.fiber/`. One baseline per project lives under Fiber
//! home; each confined call ends by comparing the paths against it,
//! restoring any difference and reporting it. Execution never holds the
//! baseline lock, and the baseline stays fixed while any call is active
//! (`docs/state.md`, "Projects").
//!
//! The lock order is the short exclusive `baseline.lock`, then the
//! non-blocking exclusive probe of `active.lock`. A call that finds no
//! active holder snapshots the baseline and converts its active take to
//! shared; a call that finds holders takes a shared active lock and reuses
//! the baseline. The baseline lock is never held across execution; the
//! active shared lock lasts until the call's final report, including after
//! adoption. Every active probe is non-blocking, so an ending call waiting
//! on the baseline lock cannot deadlock a beginning call.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// The marker the hub sets beside the startup snapshot in a session it
/// spawned (`crates/hub/src/spawn.rs`, `HUB_SESSION_MARKER`): present means
/// this process's parent is the hub that spawned it, so the parent pid below
/// names the hub. An in-process `fiber ask` carries no marker, and no hub
/// pid reaches the filter; the hub slot then names the session host.
const HUB_SESSION_MARKER: &str = "FIBER_HUB_SESSION";

/// The baseline file's format version. The format is private Fiber state,
/// never a wire format.
const BASELINE_VERSION: u32 = 1;

/// One session's handle on the protected paths of its project. The handle
/// carries only Fiber home; the project is derived from the workspace of
/// each call, so calls from another registered worktree of the same
/// repository share the one baseline (`docs/state.md`, "Projects").
#[derive(Debug)]
pub struct ProtectedPaths {
    home: PathBuf,
}

/// What `begin` hands the call: the guard that owns the end-of-call
/// compare, restore and report, carrying any stale baseline the call
/// recovered on its way in.
#[derive(Debug)]
pub(crate) struct Begin {
    /// The active-lock owner, consumed exactly once by the call's end.
    pub guard: Guard,
}

/// Owns the call's active shared lock until its end. Consumed exactly once:
/// the foreground path and the background job's common finish path each
/// take it once, and dropping it without finishing only releases the lock.
#[derive(Debug)]
pub(crate) struct Guard {
    /// The project's state files.
    dir: StatePaths,
    /// The active lock, shared for the call's life.
    active: File,
    /// What a stale baseline left behind, recovered on the way in and
    /// reported with the call's own end.
    pub(crate) recovered: Report,
}

/// What changed while a confined call ran: the paths restored to the
/// baseline, and the restores that failed. Empty means nothing changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// Workspace-relative paths restored to the recorded baseline.
    pub restored: Vec<String>,
    /// The restores that did not succeed, with the cause.
    pub failed: Vec<RestoreFailure>,
}

/// One restore that did not succeed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreFailure {
    /// The workspace-relative path.
    pub path: String,
    /// Why the restore failed.
    pub message: String,
}

impl Report {
    /// No path changed and no restore failed.
    pub fn is_empty(&self) -> bool {
        self.restored.is_empty() && self.failed.is_empty()
    }

    /// One notice line per changed path for the model and the person. A
    /// line says the path changed while a confined call ran; it never says
    /// this call changed it.
    pub fn notice_lines(&self) -> Vec<String> {
        // One wording for every surface: the model text, the person's
        // warning line and the completion notice all read the shared
        // report's lines.
        self.contract_paths()
            .map(|report| report.warning_lines())
            .unwrap_or_default()
    }

    /// The shared contract report, for the tool result and the completion
    /// events: absent when nothing changed.
    pub(crate) fn contract_paths(&self) -> Option<contract::shapes::ProtectedPaths> {
        if self.is_empty() {
            return None;
        }
        Some(contract::shapes::ProtectedPaths {
            restored: self.restored.clone(),
            failed: self
                .failed
                .iter()
                .map(|failure| contract::shapes::ProtectedPathFailure {
                    path: failure.path.clone(),
                    message: failure.message.clone(),
                })
                .collect(),
        })
    }

    /// The structured report, for the call result's `details`: absent when
    /// nothing changed.
    pub(crate) fn details_value(&self) -> Option<serde_json::Value> {
        if self.is_empty() {
            return None;
        }
        let failed: Vec<serde_json::Value> = self
            .failed
            .iter()
            .map(|failure| serde_json::json!({"path": failure.path, "message": failure.message}))
            .collect();
        Some(serde_json::json!({"restored": self.restored, "failed": failed}))
    }
}

/// Why a confined command never started: the baseline could not be read or
/// recorded. The caller fails the call closed with this cause before spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SetupError {
    /// What could not be read or recorded.
    pub message: String,
}

impl std::fmt::Display for SetupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for SetupError {}

/// The project's state files under
/// `FIBER_HOME/projects/<key>/protected-paths/`: one JSON baseline, one
/// short baseline lock, and one active lock.
#[derive(Debug, Clone)]
struct StatePaths {
    /// The directory holding the three files.
    dir: PathBuf,
    /// The recorded baseline.
    baseline: PathBuf,
    /// The short exclusive lock around baseline reads and writes.
    baseline_lock: PathBuf,
    /// One shared hold per active call.
    active_lock: PathBuf,
}

impl ProtectedPaths {
    /// Opens the handle for `workspace`'s project and recovers a stale
    /// baseline: when no active holder exists, compares the recorded paths
    /// against the baseline, restores any difference, reports it and drops
    /// the baseline. A live baseline is left untouched. Returns the handle
    /// and the startup report, when a stale baseline changed a path.
    pub fn open(home: &Path, workspace: &Path) -> Result<(Arc<Self>, Option<Report>), String> {
        let handle = Arc::new(Self {
            home: home.to_path_buf(),
        });
        let dir = state_paths(&handle.home, workspace)?;
        if !dir.dir.exists() {
            // No state yet: no stale baseline can exist, and the first
            // confined call's begin records it. A session that ends
            // before any call leaves the home untouched. A home that
            // exists but is no directory can never hold state, so
            // opening fails closed instead of deferring the failure.
            if std::fs::metadata(&handle.home).is_ok_and(|meta| !meta.is_dir()) {
                return Err(format!(
                    "{} is not a directory, so protected-path state cannot be recorded",
                    handle.home.display()
                ));
            }
            return Ok((handle, None));
        }
        let recovered = recover(&dir)?;
        Ok((handle, recovered))
    }

    /// Takes or reuses the project baseline for a confined call in
    /// `workspace` and returns the guard that owns the call's end. Fails
    /// before spawn when protection cannot be established, so the call
    /// never runs unprotected.
    pub(crate) fn begin(&self, workspace: &Path) -> Result<Begin, SetupError> {
        let dir = state_paths(&self.home, workspace).map_err(|message| SetupError { message })?;
        create_dir(&dir.dir).map_err(setup)?;
        let baseline_file = open_lock(&dir.baseline_lock).map_err(setup)?;
        baseline_file.lock().map_err(setup)?;
        let active_file = open_lock(&dir.active_lock).map_err(setup)?;
        let guard = match active_file.try_lock() {
            Ok(()) => {
                // No active holder: recover a stale baseline left by a dead
                // session, then snapshot fresh. The recovered report joins
                // the call's own report below.
                let recovered = compare_restore_report(&dir).map_err(setup)?;
                let entries = snapshot(workspace);
                write_baseline(&dir.baseline, &entries).map_err(setup)?;
                active_file.lock_shared().map_err(setup)?;
                drop(baseline_file);
                Begin {
                    guard: Guard {
                        dir,
                        active: active_file,
                        recovered,
                    },
                }
            }
            Err(TryLockError::WouldBlock) => {
                // Calls are active: reuse the fixed baseline. The shared
                // take is compatible with the ending calls' own shared
                // holds, and the holders of a brief exclusive take never
                // wait, so this cannot deadlock.
                active_file.lock_shared().map_err(setup)?;
                if !dir.baseline.exists() {
                    return Err(SetupError {
                        message: format!(
                            "the protected-path baseline is missing while confined calls are active in {}",
                            dir.dir.display()
                        ),
                    });
                }
                drop(baseline_file);
                Begin {
                    guard: Guard {
                        dir,
                        active: active_file,
                        recovered: Report {
                            restored: Vec::new(),
                            failed: Vec::new(),
                        },
                    },
                }
            }
            Err(TryLockError::Error(source)) => {
                return Err(setup(source));
            }
        };
        Ok(guard)
    }

    /// Advances the baseline entry holding `path` after an approved Fiber
    /// `write` or `edit` landed, under the short baseline lock. A path no
    /// protected entry holds is a no-op. Failures leave the baseline as it
    /// is; the next confined call still compares against the recorded one.
    pub(crate) fn note_approved_write(&self, workspace: &Path, path: &Path) {
        let Ok(dir) = state_paths(&self.home, workspace) else {
            return;
        };
        let Ok(baseline_file) = open_lock(&dir.baseline_lock) else {
            return;
        };
        if baseline_file.lock().is_err() {
            return;
        }
        let Ok(mut baseline) = read_baseline(&dir.baseline) else {
            return;
        };
        let mut changed = false;
        for entry in &mut baseline.entries {
            let entry_path = Path::new(&entry.path);
            // The entry holds the path itself, or the directory holding
            // it: re-snapshotting the entry records the approved landing.
            if entry_path == path
                || (matches!(entry.kind, EntryKind::Dir { .. }) && path.starts_with(entry_path))
            {
                *entry = snapshot_entry(entry_path, &entry.display);
                changed = true;
            }
        }
        if changed {
            let _written = write_baseline(&dir.baseline, &baseline.entries);
        }
    }
}

impl Guard {
    /// Compares the protected paths against the baseline, restores any
    /// difference and reports it, then releases both locks. Runs once, after
    /// the call's final output is collected. The recovered stale baseline
    /// joins the call's own end, each path named once.
    pub(crate) fn finish(self) -> Report {
        let Self {
            dir,
            active,
            recovered,
        } = self;
        let end = match open_lock(&dir.baseline_lock) {
            Ok(file) => {
                if file.lock().is_err() {
                    failed_report(&[], "the baseline lock could not be taken")
                } else {
                    let report = compare_restore_report(&dir).unwrap_or_else(failed_report_empty);
                    drop(file);
                    report
                }
            }
            Err(source) => failed_report(&[], &source.to_string()),
        };
        let mut restored = recovered.restored;
        for path in end.restored {
            if !restored.contains(&path) {
                restored.push(path);
            }
        }
        let mut failed = recovered.failed;
        for failure in end.failed {
            if !failed.iter().any(|held| held.path == failure.path) {
                failed.push(failure);
            }
        }
        let report = Report { restored, failed };
        // Releasing the descriptor releases the shared active take.
        drop(active);
        report
    }
}

/// A stale-baseline recovery at session start: with no active holder,
/// compares, restores, reports and drops the baseline. With holders, or
/// with no baseline, recovers nothing.
fn recover(dir: &StatePaths) -> Result<Option<Report>, String> {
    let baseline_file = open_lock(&dir.baseline_lock).map_err(to_string)?;
    baseline_file.lock().map_err(to_string)?;
    let active_file = open_lock(&dir.active_lock).map_err(to_string)?;
    match active_file.try_lock() {
        Ok(()) => {
            // No holder: the last call died before its end ran.
            let report = compare_restore_report(dir).map_err(to_string)?;
            if dir.baseline.exists() {
                std::fs::remove_file(&dir.baseline).map_err(to_string)?;
            }
            drop(baseline_file);
            drop(active_file);
            Ok((!report.is_empty()).then_some(report))
        }
        Err(TryLockError::WouldBlock) => {
            // Calls are live: the baseline stays fixed for them.
            drop(baseline_file);
            drop(active_file);
            Ok(None)
        }
        Err(TryLockError::Error(source)) => Err(to_string(source)),
    }
}

/// The project's identity: git's shared directory inside a repository,
/// else the launch directory itself, symlinks resolved (`docs/state.md`,
/// "Projects").
struct Identity {
    /// The identity path.
    path: PathBuf,
    /// Whether `git rev-parse` found a repository.
    in_repository: bool,
}

/// Resolves the identity of `workspace` with one `git rev-parse`, through
/// the scrubbed git command, so the caller's environment cannot redirect
/// the discovery.
fn identity(workspace: &Path) -> Identity {
    let common = support::git::command("git")
        .arg("-C")
        .arg(workspace)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output();
    match common {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            let raw = text.trim_end_matches('\n');
            let path = PathBuf::from(raw);
            Identity {
                path: path.canonicalize().unwrap_or(path),
                in_repository: true,
            }
        }
        Ok(_) | Err(_) => Identity {
            path: workspace
                .canonicalize()
                .unwrap_or_else(|_| workspace.to_path_buf()),
            in_repository: false,
        },
    }
}

/// The readable key of the identity path: every `/` becomes `-`
/// (`docs/state.md`, "Projects").
fn slug(path: &Path) -> String {
    path.to_string_lossy().replace('/', "-")
}

/// The state files for `workspace`'s project, under the config layout's
/// projects root (`docs/state.md`, "Projects"). The slug must name one
/// file in `projects/`; anything else fails closed before spawn.
fn state_paths(home: &Path, workspace: &Path) -> Result<StatePaths, String> {
    let slug = slug(&identity(workspace).path);
    let key = config::ProjectKey::new(slug).map_err(|source| source.to_string())?;
    let dir = config::projects_dir(home)
        .join(key.as_str())
        .join("protected-paths");
    Ok(StatePaths {
        baseline: dir.join("baseline.json"),
        baseline_lock: dir.join("baseline.lock"),
        active_lock: dir.join("active.lock"),
        dir,
    })
}

/// One registered worktree: its root, and whether git lists it first.
struct Worktree {
    /// The worktree's root.
    root: PathBuf,
    /// Listed first by `git worktree list`: the main checkout.
    main: bool,
}

/// Lists the main checkout and every registered worktree with one
/// `git worktree list --porcelain`. A failure, or a repository `git` cannot
/// see, leaves the single workspace: the baseline then holds only the
/// workspace-local entries.
fn enumerate(workspace: &Path) -> Vec<Worktree> {
    let out = support::git::command("git")
        .arg("-C")
        .arg(workspace)
        .args(["worktree", "list", "--porcelain"])
        .output();
    let Ok(out) = out else {
        return vec![Worktree {
            root: workspace.to_path_buf(),
            main: true,
        }];
    };
    if !out.status.success() {
        return vec![Worktree {
            root: workspace.to_path_buf(),
            main: true,
        }];
    }
    let mut worktrees = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            let root = PathBuf::from(path);
            let main = worktrees.is_empty();
            worktrees.push(Worktree { root, main });
        }
    }
    if worktrees.is_empty() {
        return vec![Worktree {
            root: workspace.to_path_buf(),
            main: true,
        }];
    }
    worktrees
}

/// One protected path: where it is, how the report names it, and what the
/// baseline recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    /// The absolute path.
    path: String,
    /// The report name, relative to the owning worktree.
    display: String,
    /// What the baseline recorded.
    kind: EntryKind,
}

/// What one path held: file bytes losslessly as JSON byte values, distinct
/// file, directory, symlink and missing entries, with the permissions a
/// restore reapplies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum EntryKind {
    /// A regular file: its bytes and its permission bits.
    File {
        /// The Unix permission bits.
        mode: u32,
        /// The file's exact bytes.
        bytes: Vec<u8>,
    },
    /// A directory: its permission bits and its entries by file name.
    Dir {
        /// The Unix permission bits.
        mode: u32,
        /// The entries by file name.
        children: BTreeMap<String, EntryKind>,
    },
    /// A symlink: its target, exactly as `read_link` read it.
    Symlink {
        /// The link's target.
        target: String,
    },
    /// The path held nothing when the baseline was taken.
    Missing,
}

/// The baseline file: the recorded entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Baseline {
    /// The format version.
    version: u32,
    /// One entry per protected path.
    entries: Vec<Entry>,
}

/// Snapshots every protected path of `workspace`'s project: the shared
/// `.git/config` and hooks once, and each registered worktree's `.git`
/// pointer and workspace-local `.fiber/`. Worktree membership is captured
/// here and never re-read while calls are active.
fn snapshot(workspace: &Path) -> Vec<Entry> {
    let found = identity(workspace);
    let worktrees = enumerate(workspace);
    let main_root = worktrees
        .iter()
        .find(|worktree| worktree.main)
        .map(|worktree| worktree.root.clone())
        .unwrap_or_else(|| workspace.to_path_buf());
    let mut entries = Vec::new();
    if found.in_repository {
        let common = &found.path;
        entries.push(snapshot_entry(
            &common.join("config"),
            &display(&main_root, &common.join("config")),
        ));
        entries.push(snapshot_entry(
            &common.join("hooks"),
            &display(&main_root, &common.join("hooks")),
        ));
    }
    for worktree in &worktrees {
        let pointer = worktree.root.join(".git");
        // A directory `.git` on the main checkout is the shared directory
        // itself, whose objects, refs and logs stay writable for commits:
        // only the shared config and hooks above are recorded.
        let is_shared_dir = pointer.is_dir()
            && canonical(&pointer).is_some_and(|resolved| Some(resolved) == canonical(&found.path));
        if !is_shared_dir {
            entries.push(snapshot_entry(&pointer, &display(&worktree.root, &pointer)));
        }
        let fiber = worktree.root.join(".fiber");
        entries.push(snapshot_entry(&fiber, &display(&worktree.root, &fiber)));
    }
    entries
}

/// The report name: relative to the owning worktree, else absolute.
fn display(root: &Path, path: &Path) -> String {
    if let Ok(relative) = path.strip_prefix(root) {
        let text = relative.to_string_lossy().into_owned();
        if text.is_empty() {
            return path.to_string_lossy().into_owned();
        }
        return text;
    }
    path.to_string_lossy().into_owned()
}

/// Snapshots one path: file bytes and mode, directory mode and recursive
/// entries, the symlink target, or the missing mark.
fn snapshot_entry(path: &Path, shown: &str) -> Entry {
    Entry {
        path: path.to_string_lossy().into_owned(),
        display: shown.to_owned(),
        kind: snapshot_kind(path),
    }
}

/// Records what `path` holds now.
fn snapshot_kind(path: &Path) -> EntryKind {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(_) => return EntryKind::Missing,
    };
    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(path)
            .map(|target| target.to_string_lossy().into_owned())
            .unwrap_or_default();
        return EntryKind::Symlink { target };
    }
    if meta.file_type().is_dir() {
        let mut children = BTreeMap::new();
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                children.insert(name, snapshot_kind(&entry.path()));
            }
        }
        return EntryKind::Dir {
            mode: unix_mode(&meta),
            children,
        };
    }
    if meta.file_type().is_file() {
        let bytes = std::fs::read(path).unwrap_or_default();
        return EntryKind::File {
            mode: unix_mode(&meta),
            bytes,
        };
    }
    EntryKind::Missing
}

/// The canonical path, when it resolves.
fn canonical(path: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(path).ok()
}

/// The Unix permission bits of file metadata.
#[cfg(unix)]
fn unix_mode(meta: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    meta.permissions().mode()
}

/// No permission bits off Unix.
#[cfg(not(unix))]
fn unix_mode(_meta: &std::fs::Metadata) -> u32 {
    0
}

/// Creates the state directory and its parents.
fn create_dir(dir: &Path) -> Result<(), io::Error> {
    std::fs::create_dir_all(dir)
}

/// Opens a lock file, creating it. The lock lives on the descriptor, so
/// the file is never truncated.
fn open_lock(path: &Path) -> Result<File, io::Error> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

/// A setup failure naming what could not be read or recorded.
fn setup(source: io::Error) -> SetupError {
    SetupError {
        message: source.to_string(),
    }
}

/// An I/O failure as text.
fn to_string(source: io::Error) -> String {
    source.to_string()
}

/// Reads the recorded baseline. A missing file means no baseline was ever
/// taken; a corrupt one fails the call closed rather than running
/// unprotected.
fn read_baseline(path: &Path) -> Result<Baseline, io::Error> {
    let bytes = std::fs::read(path)?;
    let baseline: Baseline = serde_json::from_slice(&bytes)
        .map_err(|source| io::Error::new(io::ErrorKind::InvalidData, source))?;
    if baseline.version != BASELINE_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "unsupported protected-path baseline version {}",
                baseline.version
            ),
        ));
    }
    Ok(baseline)
}

/// Records the baseline atomically: the bytes go to a temporary file in
/// the same directory, then rename onto the baseline, so a concurrent
/// reader never sees a half-written file.
fn write_baseline(path: &Path, entries: &[Entry]) -> Result<(), io::Error> {
    let baseline = Baseline {
        version: BASELINE_VERSION,
        entries: entries.to_vec(),
    };
    let bytes = serde_json::to_vec(&baseline)
        .map_err(|source| io::Error::new(io::ErrorKind::InvalidData, source))?;
    let parent = path.parent().unwrap_or_else(|| Path::new("/"));
    let temp = parent.join(format!(".baseline-{}.tmp", std::process::id()));
    std::fs::write(&temp, &bytes)?;
    if let Err(source) = std::fs::rename(&temp, path) {
        let _removed = std::fs::remove_file(&temp);
        return Err(source);
    }
    Ok(())
}

/// Compares every protected path against the baseline, restores any
/// difference and reports it. A missing baseline records nothing, so a
/// recovery with no baseline restores nothing. A failed entry is reported
/// with its cause and never as restored.
fn compare_restore_report(dir: &StatePaths) -> Result<Report, io::Error> {
    let baseline = match read_baseline(&dir.baseline) {
        Ok(baseline) => baseline,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(Report {
                restored: Vec::new(),
                failed: Vec::new(),
            });
        }
        Err(error) => return Err(error),
    };
    let mut report = Report {
        restored: Vec::new(),
        failed: Vec::new(),
    };
    for entry in &baseline.entries {
        if let Err(source) = reconcile(entry, &mut report) {
            report.failed.push(RestoreFailure {
                path: entry.display.clone(),
                message: source,
            });
        }
    }
    Ok(report)
}

/// Compares one entry and restores it when it differs. Returns the cause
/// when the compare or the restore fails.
fn reconcile(entry: &Entry, report: &mut Report) -> Result<(), String> {
    let path = Path::new(&entry.path);
    match &entry.kind {
        EntryKind::File { mode, bytes } => {
            let same = std::fs::read(path)
                .map(|current| current == *bytes)
                .unwrap_or(false)
                && mode_matches(path, *mode);
            if same {
                return Ok(());
            }
            remove_any(path)?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|source| source.to_string())?;
            }
            std::fs::write(path, bytes).map_err(|source| source.to_string())?;
            apply_mode(path, *mode)?;
            report.restored.push(entry.display.clone());
            Ok(())
        }
        EntryKind::Dir { mode, children } => {
            let is_dir = std::fs::symlink_metadata(path)
                .map(|meta| meta.file_type().is_dir())
                .unwrap_or(false);
            let mut changed = false;
            if !is_dir {
                remove_any(path)?;
                std::fs::create_dir_all(path).map_err(|source| source.to_string())?;
                changed = true;
            }
            apply_mode(path, *mode)?;
            if !mode_matches(path, *mode) {
                changed = true;
            }
            reconcile_children(path, children, &mut changed)?;
            if changed {
                report.restored.push(entry.display.clone());
            }
            Ok(())
        }
        EntryKind::Symlink { target } => {
            let same = std::fs::read_link(path)
                .map(|current| current.to_string_lossy() == *target)
                .unwrap_or(false);
            if same {
                return Ok(());
            }
            remove_any(path)?;
            std::os::unix::fs::symlink(target, path).map_err(|source| source.to_string())?;
            report.restored.push(entry.display.clone());
            Ok(())
        }
        EntryKind::Missing => {
            if std::fs::symlink_metadata(path).is_err() {
                return Ok(());
            }
            remove_any(path)?;
            report.restored.push(entry.display.clone());
            Ok(())
        }
    }
}

/// Whether the path's permission bits equal the recorded ones.
fn mode_matches(path: &Path, mode: u32) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(path)
            .map(|meta| (meta.permissions().mode() & 0o7777) == (mode & 0o7777))
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        true
    }
}

/// Reapplies the recorded permission bits.
fn apply_mode(path: &Path, mode: u32) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & 0o7777))
            .map_err(|source| source.to_string())
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Ok(())
    }
}

/// Reconciles a recorded directory's children: restores each recorded
/// child, and removes every current entry the baseline never recorded, so
/// a rename, a new file or a new directory entry cannot escape detection.
fn reconcile_children(
    dir: &Path,
    children: &BTreeMap<String, EntryKind>,
    changed: &mut bool,
) -> Result<(), String> {
    for (name, kind) in children {
        let path = dir.join(name);
        if reconcile_child(&path, kind)? {
            *changed = true;
        }
    }
    let current: Vec<String> = std::fs::read_dir(dir)
        .map_err(|source| source.to_string())?
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    for name in current {
        if !children.contains_key(&name) {
            remove_any(&dir.join(&name))?;
            *changed = true;
        }
    }
    Ok(())
}

/// Compares and restores one recorded directory child.
fn reconcile_child(path: &Path, kind: &EntryKind) -> Result<bool, String> {
    match kind {
        EntryKind::File { mode, bytes } => {
            let same = std::fs::read(path)
                .map(|current| current == *bytes)
                .unwrap_or(false)
                && mode_matches(path, *mode);
            if same {
                return Ok(false);
            }
            remove_any(path)?;
            std::fs::write(path, bytes).map_err(|source| source.to_string())?;
            apply_mode(path, *mode)?;
            Ok(true)
        }
        EntryKind::Dir { mode, children } => {
            let is_dir = std::fs::symlink_metadata(path)
                .map(|meta| meta.file_type().is_dir())
                .unwrap_or(false);
            if !is_dir {
                remove_any(path)?;
                std::fs::create_dir(path).map_err(|source| source.to_string())?;
            }
            apply_mode(path, *mode)?;
            let mut changed = !mode_matches(path, *mode) || !is_dir;
            reconcile_children(path, children, &mut changed)?;
            Ok(changed)
        }
        EntryKind::Symlink { target } => {
            let same = std::fs::read_link(path)
                .map(|current| current.to_string_lossy() == *target)
                .unwrap_or(false);
            if same {
                return Ok(false);
            }
            remove_any(path)?;
            std::os::unix::fs::symlink(target, path).map_err(|source| source.to_string())?;
            Ok(true)
        }
        EntryKind::Missing => {
            if std::fs::symlink_metadata(path).is_err() {
                return Ok(false);
            }
            remove_any(path)?;
            Ok(true)
        }
    }
}

/// Removes whatever `path` holds: a file or symlink, or a directory tree.
/// Absent is already removed.
fn remove_any(path: &Path) -> Result<(), String> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    if meta.file_type().is_dir() {
        std::fs::remove_dir_all(path).map_err(|source| source.to_string())
    } else {
        std::fs::remove_file(path).map_err(|source| source.to_string())
    }
}

/// The whole compare as failed: every recorded display path with one cause.
fn failed_report(displays: &[String], cause: &str) -> Report {
    Report {
        restored: Vec::new(),
        failed: displays
            .iter()
            .map(|path| RestoreFailure {
                path: path.clone(),
                message: cause.to_owned(),
            })
            .collect(),
    }
}

/// A compare that never recorded a path, failed with one cause.
fn failed_report_empty(cause: io::Error) -> Report {
    failed_report(&[], &cause.to_string())
}

/// The processes a confined call must not signal below Landlock ABI 6:
/// the session host, whose death would skip the end-of-call restore, and
/// the hub that spawned it, when one did. The pids are known when the
/// filter is built; every other same-user process stays open below ABI 6
/// (`docs/sandbox.md`, "What it cannot do").
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Supervisor {
    /// The session host's pid.
    pub session_pid: u32,
    /// Its process group.
    pub session_pgid: u32,
    /// The spawning hub's pid, or the session host when no hub spawned it.
    pub hub_pid: u32,
    /// Its process group.
    pub hub_pgid: u32,
}

/// The Linux signal policy: Landlock signal scoping from ABI 6, or the
/// narrow seccomp supervisor rule below it. Existing socket and
/// `io_uring_setup` denials stay in both cases.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LinuxPolicy {
    /// Request Landlock signal scope; no seccomp signal rule is added.
    pub signal_scope: bool,
    /// The supervisor rules to deny with `EPERM`, empty under scope.
    pub supervisor_rules: Vec<(i64, Vec<seccompiler::SeccompRule>)>,
}

/// Selects the Linux signal policy: the seccomp supervisor rule when the
/// kernel is below ABI 6, Landlock signal scope from ABI 6. One call from
/// the sandbox setup, keeping every other policy behavior unchanged.
#[cfg(target_os = "linux")]
pub(crate) fn linux_policy() -> Result<LinuxPolicy, String> {
    if supports_signal_scope() {
        return Ok(LinuxPolicy {
            signal_scope: true,
            supervisor_rules: Vec::new(),
        });
    }
    Ok(LinuxPolicy {
        signal_scope: false,
        supervisor_rules: supervisor_rules(&supervisor())?,
    })
}

/// Whether the kernel honours Landlock ABI 6: a version-6 ruleset creates
/// only there. Probed, never assumed.
#[cfg(target_os = "linux")]
pub(crate) fn supports_signal_scope() -> bool {
    use landlock::{Access as _, Compatible as _, RulesetAttr as _};
    landlock::Ruleset::default()
        .set_compatibility(landlock::CompatLevel::HardRequirement)
        .handle_access(landlock::AccessFs::from_all(landlock::ABI::V6))
        .and_then(|ruleset| ruleset.create())
        .is_ok()
}

/// Collects the supervisor pids: the session host itself, and the spawning
/// hub when the hub marker says one spawned this process. A hub-spawned
/// session is the hub's direct child, so the parent pid names the hub;
/// without the marker the hub slot names the session host, adding no rule.
#[cfg(target_os = "linux")]
pub(crate) fn supervisor() -> Supervisor {
    let session_pid = std::process::id();
    let session_pgid = process_group(None).unwrap_or(session_pid);
    if std::env::var_os(HUB_SESSION_MARKER).is_some() {
        let hub_pid = parent_pid().unwrap_or(session_pid);
        let hub_pgid = process_group(Some(hub_pid)).unwrap_or(hub_pid);
        Supervisor {
            session_pid,
            session_pgid,
            hub_pid,
            hub_pgid,
        }
    } else {
        Supervisor {
            session_pid,
            session_pgid,
            hub_pid: session_pid,
            hub_pgid: session_pgid,
        }
    }
}

/// The process group of `pid`, or of this process when absent.
#[cfg(target_os = "linux")]
fn process_group(pid: Option<u32>) -> Option<u32> {
    let raw = match pid {
        Some(pid) => Some(rustix::process::Pid::from_raw(i32::try_from(pid).ok()?)?),
        None => None,
    };
    let group = rustix::process::getpgid(raw).ok()?;
    u32::try_from(group.as_raw_pid()).ok()
}

/// This process's parent pid.
#[cfg(target_os = "linux")]
fn parent_pid() -> Option<u32> {
    let parent = rustix::process::getppid()?;
    u32::try_from(parent.as_raw_pid()).ok()
}

/// The narrow supervisor rule: `EPERM` for `kill`, `tkill` and `tgkill`
/// aimed at the session or hub process, either one's process group, or pid
/// -1, and for `pidfd_open` of the session or hub pid. Signals to the
/// call's own children and process group still work: pid 0 is never denied.
#[cfg(target_os = "linux")]
pub(crate) fn supervisor_rules(
    supervisor: &Supervisor,
) -> Result<Vec<(i64, Vec<seccompiler::SeccompRule>)>, String> {
    use seccompiler::{SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompRule};
    let targeted =
        |pids: &[u64], arg: u8| -> Result<Vec<SeccompRule>, String> {
            let mut rules = Vec::new();
            for pid in pids {
                let condition =
                    SeccompCondition::new(arg, SeccompCmpArgLen::Qword, SeccompCmpOp::Eq, *pid)
                        .map_err(|source| {
                            format!("sandbox setup failed compiling seccomp: {source:?}")
                        })?;
                rules.push(SeccompRule::new(vec![condition]).map_err(|source| {
                    format!("sandbox setup failed compiling seccomp: {source:?}")
                })?);
            }
            Ok(rules)
        };
    let session = u64::from(supervisor.session_pid);
    let hub = u64::from(supervisor.hub_pid);
    // Aimed at a process group, or at every process: the sign-extended
    // 64-bit form of the negative pid, and all-bits for -1.
    let session_group = 0u64.wrapping_sub(u64::from(supervisor.session_pgid));
    let hub_group = 0u64.wrapping_sub(u64::from(supervisor.hub_pgid));
    let everyone = u64::MAX;
    let mut rules = vec![
        (
            libc::SYS_kill,
            targeted(
                &sorted(&[session, hub, session_group, hub_group, everyone]),
                0,
            )?,
        ),
        (libc::SYS_tkill, targeted(&sorted(&[session, hub]), 0)?),
        (libc::SYS_pidfd_open, targeted(&sorted(&[session, hub]), 0)?),
    ];
    // Aimed at the session or hub through either argument: the thread group
    // or the thread.
    let mut tgkill = targeted(&sorted(&[session, hub]), 0)?;
    tgkill.extend(targeted(&sorted(&[session, hub]), 1)?);
    rules.push((libc::SYS_tgkill, tgkill));
    Ok(rules)
}

/// The values sorted and deduplicated: the session and hub slots name one
/// process when no hub spawned the session.
#[cfg(target_os = "linux")]
fn sorted(values: &[u64]) -> Vec<u64> {
    let mut ordered = values.to_vec();
    ordered.sort_unstable();
    ordered.dedup();
    ordered
}
