//! The protected-path baseline: identity, storage, locking and capture
//! (`docs/sandbox.md`, "Protected paths"). Fail closed when discovery or
//! capture is incomplete, so a confined call never runs unprotected.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The baseline file's format version. The format is private Fiber state,
/// never a wire format.
pub(crate) const BASELINE_VERSION: u32 = 1;

/// The project's identity: git's shared directory inside a repository,
/// else the launch directory itself, symlinks resolved (`docs/state.md`,
/// "Projects").
pub(crate) struct Identity {
    /// The identity path.
    pub(crate) path: PathBuf,
    /// Whether `git rev-parse` found a repository.
    pub(crate) in_repository: bool,
}

/// Resolves the identity of `workspace` with one `git rev-parse`, through
/// the scrubbed git command, so the caller's environment cannot redirect
/// the discovery. Outside a repository the workspace itself is the
/// identity. When a `.git` entry is reachable from the workspace but git
/// cannot answer, the discovery is incomplete — the protected config may
/// itself be what broke git — so the lookup fails instead of leaving the
/// shared paths unprotected.
pub(crate) fn identity(workspace: &Path) -> Result<Identity, String> {
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
            Ok(Identity {
                path: path.canonicalize().unwrap_or(path),
                in_repository: true,
            })
        }
        Ok(out) => fallback_or_fail(workspace, &describe(&out.status, &out.stderr)),
        Err(source) => fallback_or_fail(workspace, &source.to_string()),
    }
}

/// The workspace itself when no `.git` entry is reachable from it, else
/// the git failure that leaves discovery incomplete.
fn fallback_or_fail(workspace: &Path, cause: &str) -> Result<Identity, String> {
    if has_git_entry(workspace) {
        return Err(format!(
            "the project identity of {} could not be read: {cause}",
            workspace.display()
        ));
    }
    Ok(Identity {
        path: workspace
            .canonicalize()
            .unwrap_or_else(|_| workspace.to_path_buf()),
        in_repository: false,
    })
}

/// Whether `workspace` or any ancestor holds a `.git` entry: a repository
/// git cannot see is broken discovery, never a plain directory.
fn has_git_entry(workspace: &Path) -> bool {
    let base = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    base.ancestors()
        .any(|ancestor| ancestor.join(".git").symlink_metadata().is_ok())
}

/// Why a git lookup failed: git's own diagnostic when it printed one, else
/// its exit status.
fn describe(status: &std::process::ExitStatus, stderr: &[u8]) -> String {
    let trimmed = String::from_utf8_lossy(stderr);
    let trimmed = trimmed.trim();
    if trimmed.is_empty() {
        format!("git exited with {status}")
    } else {
        trimmed.to_owned()
    }
}

/// The readable key of the identity path: every `/` becomes `-`
/// (`docs/state.md`, "Projects").
pub(crate) fn slug(path: &Path) -> String {
    path.to_string_lossy().replace('/', "-")
}

/// A stable 64-bit FNV-1a hash of the identity path's bytes. The standard
/// hasher's keys are not stable across builds, so the key uses this
/// instead; the suffix keeps distinct project identities distinct even
/// when their readable slugs collide (for example `/a-b` and `/a/b`).
pub(crate) fn fnv1a(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;
    let mut hash = OFFSET;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// The state key for an identity path: the readable slug plus the hash of
/// the full identity, so colliding slugs never share a baseline or a lock
/// (`docs/sandbox.md`, "Protected paths").
pub(crate) fn state_key(identity_path: &Path) -> String {
    let bytes = identity_path.as_os_str().as_encoded_bytes();
    format!("{}-{:016x}", slug(identity_path), fnv1a(bytes))
}

/// The project's state files under
/// `FIBER_HOME/projects/<key>/protected-paths/`: one JSON baseline, one
/// short baseline lock, and one active lock.
#[derive(Debug, Clone)]
pub(crate) struct StatePaths {
    /// The directory holding the three files.
    pub(crate) dir: PathBuf,
    /// The recorded baseline.
    pub(crate) baseline: PathBuf,
    /// The short exclusive lock around baseline reads and writes.
    pub(crate) baseline_lock: PathBuf,
    /// One shared hold per active call.
    pub(crate) active_lock: PathBuf,
}

/// The state files for `workspace`'s project, under the config layout's
/// projects root (`docs/state.md`, "Projects"). The key names one file in
/// `projects/`; anything else fails closed before spawn.
pub(crate) fn state_paths(home: &Path, workspace: &Path) -> Result<StatePaths, String> {
    let key = state_key(&identity(workspace)?.path);
    let key = config::ProjectKey::new(key).map_err(|source| source.to_string())?;
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
#[derive(Debug)]
pub(crate) struct Worktree {
    /// The worktree's root.
    pub(crate) root: PathBuf,
    /// Listed first by `git worktree list`: the main checkout.
    pub(crate) main: bool,
}

/// Lists the main checkout and every registered worktree with one
/// `git worktree list --porcelain`. A repository git just proved through
/// `identity` is listed completely or not at all: a failure, or a listing
/// that names no checkout, fails instead of leaving registered checkouts
/// unprotected.
pub(crate) fn enumerate(workspace: &Path) -> Result<Vec<Worktree>, String> {
    let out = support::git::command("git")
        .arg("-C")
        .arg(workspace)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .map_err(|source| {
            format!(
                "the worktrees of {} could not be listed: {source}",
                workspace.display()
            )
        })?;
    if !out.status.success() {
        return Err(format!(
            "the worktrees of {} could not be listed: {}",
            workspace.display(),
            describe(&out.status, &out.stderr)
        ));
    }
    parse_worktrees(workspace, &out.stdout)
}

/// Reads one `git worktree list --porcelain` output: each `worktree ` line
/// names a checkout, the first one the main checkout. A listing that names
/// none fails, so a repository git proved never snapshots a single
/// checkout as its whole project.
pub(crate) fn parse_worktrees(workspace: &Path, stdout: &[u8]) -> Result<Vec<Worktree>, String> {
    let mut worktrees = Vec::new();
    for line in String::from_utf8_lossy(stdout).lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            let root = PathBuf::from(path);
            let main = worktrees.is_empty();
            worktrees.push(Worktree { root, main });
        }
    }
    if worktrees.is_empty() {
        return Err(format!(
            "the worktrees of {} could not be listed: git listed none",
            workspace.display()
        ));
    }
    Ok(worktrees)
}

/// One protected path: where it is, how the report names it, and what the
/// baseline recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Entry {
    /// The absolute path.
    pub(crate) path: String,
    /// The report name, relative to the owning worktree.
    pub(crate) display: String,
    /// What the baseline recorded.
    pub(crate) kind: EntryKind,
}

/// What one path held: file bytes losslessly as JSON byte values, distinct
/// file, directory, symlink and missing entries, with the permissions a
/// restore reapplies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum EntryKind {
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
pub(crate) struct Baseline {
    /// The format version.
    pub(crate) version: u32,
    /// One entry per protected path.
    pub(crate) entries: Vec<Entry>,
}

/// Snapshots every protected path of `workspace`'s project: the shared
/// `.git/config` and hooks once, and each registered worktree's `.git`
/// pointer and workspace-local `.fiber/`. Worktree membership is captured
/// here and never re-read while calls are active. A protected entry that
/// cannot be read or recorded fails closed with its cause, and incomplete
/// discovery fails the same way, so the call never runs unprotected.
pub(crate) fn snapshot(workspace: &Path) -> Result<Vec<Entry>, String> {
    let found = identity(workspace)?;
    let worktrees = if found.in_repository {
        enumerate(workspace)?
    } else {
        vec![Worktree {
            root: workspace.to_path_buf(),
            main: true,
        }]
    };
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
        )?);
        entries.push(snapshot_entry(
            &common.join("hooks"),
            &display(&main_root, &common.join("hooks")),
        )?);
    }
    for worktree in &worktrees {
        let pointer = worktree.root.join(".git");
        // A directory `.git` on the main checkout is the shared directory
        // itself, whose objects, refs and logs stay writable for commits:
        // only the shared config and hooks above are recorded.
        let is_shared_dir = pointer.is_dir()
            && canonical(&pointer).is_some_and(|resolved| Some(resolved) == canonical(&found.path));
        if !is_shared_dir {
            entries.push(snapshot_entry(
                &pointer,
                &display(&worktree.root, &pointer),
            )?);
        }
        let fiber = worktree.root.join(".fiber");
        entries.push(snapshot_entry(&fiber, &display(&worktree.root, &fiber))?);
    }
    Ok(entries)
}

/// The report name: relative to the owning worktree, else absolute.
pub(crate) fn display(root: &Path, path: &Path) -> String {
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
/// entries, the symlink target, or the missing mark. Anything but a
/// missing path that cannot be read fails closed.
pub(crate) fn snapshot_entry(path: &Path, shown: &str) -> Result<Entry, String> {
    Ok(Entry {
        path: path.to_string_lossy().into_owned(),
        display: shown.to_owned(),
        kind: snapshot_kind(path)?,
    })
}

/// Records what `path` holds now. A path nothing holds is `Missing`; any
/// other read failure names the path and its cause.
pub(crate) fn snapshot_kind(path: &Path) -> Result<EntryKind, String> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(EntryKind::Missing),
        Err(error) => {
            return Err(format!("{} could not be recorded: {error}", path.display()));
        }
    };
    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(path)
            .map_err(|source| format!("{} could not be recorded: {source}", path.display()))?;
        return Ok(EntryKind::Symlink {
            target: target.to_string_lossy().into_owned(),
        });
    }
    if meta.file_type().is_dir() {
        let mut children = BTreeMap::new();
        let entries = std::fs::read_dir(path)
            .map_err(|source| format!("{} could not be recorded: {source}", path.display()))?;
        for entry in entries {
            let entry = entry
                .map_err(|source| format!("{} could not be recorded: {source}", path.display()))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            children.insert(name, snapshot_kind(&entry.path())?);
        }
        return Ok(EntryKind::Dir {
            mode: unix_mode(&meta),
            children,
        });
    }
    if meta.file_type().is_file() {
        let bytes = std::fs::read(path)
            .map_err(|source| format!("{} could not be recorded: {source}", path.display()))?;
        return Ok(EntryKind::File {
            mode: unix_mode(&meta),
            bytes,
        });
    }
    Ok(EntryKind::Missing)
}

/// The canonical path, when it resolves.
pub(crate) fn canonical(path: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(path).ok()
}

/// The Unix permission bits of file metadata.
#[cfg(unix)]
pub(crate) fn unix_mode(meta: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    meta.permissions().mode()
}

/// No permission bits off Unix.
#[cfg(not(unix))]
pub(crate) fn unix_mode(_meta: &std::fs::Metadata) -> u32 {
    0
}

/// Creates the state directory and its parents.
pub(crate) fn create_dir(dir: &Path) -> Result<(), io::Error> {
    std::fs::create_dir_all(dir)
}

/// Opens a lock file, creating it. The lock lives on the descriptor, so
/// the file is never truncated.
pub(crate) fn open_lock(path: &Path) -> Result<std::fs::File, io::Error> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

/// Reads the recorded baseline. A missing file means no baseline was ever
/// taken; a corrupt one fails the call closed rather than running
/// unprotected.
pub(crate) fn read_baseline(path: &Path) -> Result<Baseline, io::Error> {
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
pub(crate) fn write_baseline(path: &Path, entries: &[Entry]) -> Result<(), io::Error> {
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
