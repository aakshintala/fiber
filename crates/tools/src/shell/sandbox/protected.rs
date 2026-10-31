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
//!
//! The baseline half lives in [`baseline`]: identity, storage, locking and
//! capture, failing closed when discovery or capture is incomplete. The
//! restore half lives in [`restore`]: one recursive reconciler with
//! symlink-safe comparison.

pub(crate) mod baseline;
mod restore;

use std::collections::HashMap;
use std::fs::{File, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

type PendingBaselines = Arc<Mutex<HashMap<PathBuf, Vec<baseline::Entry>>>>;

/// One session's handle on the protected paths of its project. The handle
/// carries only Fiber home; the project is derived from the workspace of
/// each call, so calls from another registered worktree of the same
/// repository share the one baseline (`docs/state.md`, "Projects").
#[derive(Debug)]
pub struct ProtectedPaths {
    home: PathBuf,
    pending: PendingBaselines,
}

/// What `begin` hands the call: the guard that owns the end-of-call
/// compare, restore and report, carrying any stale baseline the call
/// recovered on its way in.
#[cfg(any(target_os = "linux", test))]
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
    dir: baseline::StatePaths,
    /// The active lock, shared for the call's life.
    active: File,
    /// What a stale baseline left behind, recovered on the way in and
    /// reported with the call's own end.
    pub(crate) recovered: Report,
    /// Approved writes whose baseline update could not be persisted. Their
    /// in-memory entries keep active calls from restoring stale bytes.
    pending: PendingBaselines,
}

/// What changed while a confined call ran: the shared protected-path
/// report (`docs/events.md`, "Payload types"). Empty means nothing
/// changed: every surface omits it, and the warning lines name each
/// changed path for the model and the person.
pub use contract::shapes::{ProtectedPathFailure as RestoreFailure, ProtectedPaths as Report};

/// Why a confined command never started: the baseline could not be read or
/// recorded. The caller fails the call closed with this cause before spawn.
#[cfg(any(target_os = "linux", test))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SetupError {
    /// What could not be read or recorded.
    pub message: String,
}

#[cfg(any(target_os = "linux", test))]
impl std::fmt::Display for SetupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

#[cfg(any(target_os = "linux", test))]
impl std::error::Error for SetupError {}

/// Why session startup could not open protected-path state.
#[derive(Debug, thiserror::Error)]
pub enum ProtectedPathsOpenError {
    /// The project's identity or state key could not be resolved.
    #[error("{0}")]
    Project(String),
    /// Fiber home exists but cannot hold protected-path state.
    #[error("{path} is not a directory, so protected-path state cannot be recorded")]
    HomeNotDirectory {
        /// The Fiber home path that is not a directory.
        path: PathBuf,
    },
    /// A stale protected-path baseline could not be recovered.
    #[error("protected-path recovery failed: {0}")]
    Recovery(#[source] io::Error),
}

fn pending_lock(
    pending: &Mutex<HashMap<PathBuf, Vec<baseline::Entry>>>,
) -> MutexGuard<'_, HashMap<PathBuf, Vec<baseline::Entry>>> {
    pending
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn pending_entries(
    pending: &PendingBaselines,
    dir: &baseline::StatePaths,
) -> Option<Vec<baseline::Entry>> {
    let entries = pending_lock(pending).get(&dir.dir).cloned()?;
    baseline::belongs_to_project(&entries, &dir.identity_marker).then_some(entries)
}

fn set_pending(pending: &PendingBaselines, dir: &Path, entries: Vec<baseline::Entry>) {
    pending_lock(pending).insert(dir.to_path_buf(), entries);
}

fn clear_pending(pending: &PendingBaselines, dir: &Path) {
    pending_lock(pending).remove(dir);
}

impl ProtectedPaths {
    /// Opens the handle for `workspace`'s project and recovers a stale
    /// baseline: when no active holder exists, compares the recorded paths
    /// against the baseline, restores any difference, reports it and drops
    /// the baseline. A live baseline is left untouched. Returns the handle
    /// and the startup report, when a stale baseline changed a path.
    pub fn open(
        home: &Path,
        workspace: &Path,
    ) -> Result<(Arc<Self>, Option<Report>), ProtectedPathsOpenError> {
        let handle = Arc::new(Self {
            home: home.to_path_buf(),
            pending: Arc::default(),
        });
        let dir = baseline::state_paths(&handle.home, workspace)
            .map_err(ProtectedPathsOpenError::Project)?;
        if !dir.dir.exists() {
            // No state yet: no stale baseline can exist, and the first
            // confined call's begin records it. A session that ends
            // before any call leaves the home untouched. A home that
            // exists but is no directory can never hold state, so
            // opening fails closed instead of deferring the failure.
            if std::fs::metadata(&handle.home).is_ok_and(|meta| !meta.is_dir()) {
                return Err(ProtectedPathsOpenError::HomeNotDirectory {
                    path: handle.home.clone(),
                });
            }
            return Ok((handle, None));
        }
        let recovered = recover(&dir).map_err(ProtectedPathsOpenError::Recovery)?;
        Ok((handle, recovered))
    }

    /// Takes or reuses the project baseline for a confined call in
    /// `workspace` and returns the guard that owns the call's end. Fails
    /// before spawn when protection cannot be established, so the call
    /// never runs unprotected.
    #[cfg(any(target_os = "linux", test))]
    pub(crate) fn begin(&self, workspace: &Path) -> Result<Begin, SetupError> {
        let dir = baseline::state_paths(&self.home, workspace)
            .map_err(|message| SetupError { message })?;
        baseline::create_dir(&dir.dir).map_err(setup)?;
        let baseline_file = baseline::open_lock(&dir.baseline_lock).map_err(setup)?;
        baseline_file.lock().map_err(setup)?;
        let active_file = baseline::open_lock(&dir.active_lock).map_err(setup)?;
        let guard = match active_file.try_lock() {
            Ok(()) => {
                // No active holder: recover a stale baseline left by a dead
                // session, then snapshot fresh. The recovered report joins
                // the call's own report below. Discovery or capture that
                // is incomplete fails before spawn.
                let recovered = match pending_entries(&self.pending, &dir) {
                    Some(entries) => {
                        let report = restore::compare_restore_entries(&entries);
                        baseline::write_baseline(&dir.baseline, &entries).map_err(setup)?;
                        clear_pending(&self.pending, &dir.dir);
                        report
                    }
                    None => restore::compare_restore_report(&dir).map_err(setup)?,
                };
                let entries =
                    baseline::snapshot(workspace).map_err(|message| SetupError { message })?;
                baseline::write_baseline(&dir.baseline, &entries).map_err(setup)?;
                active_file.lock_shared().map_err(setup)?;
                drop(baseline_file);
                Begin {
                    guard: Guard {
                        dir,
                        active: active_file,
                        recovered,
                        pending: self.pending.clone(),
                    },
                }
            }
            Err(TryLockError::WouldBlock) => {
                // Calls are active: reuse the fixed baseline. Validate its
                // bytes too, so an approved-write persistence failure that
                // invalidated it makes this call fail closed. The shared take
                // is compatible with the ending calls' own shared holds, and
                // the holders of a brief exclusive take never wait, so this
                // cannot deadlock.
                if let Some(entries) = pending_entries(&self.pending, &dir) {
                    baseline::write_baseline(&dir.baseline, &entries).map_err(setup)?;
                    clear_pending(&self.pending, &dir.dir);
                } else {
                    match baseline::read_baseline(&dir.baseline) {
                        Ok(baseline)
                            if baseline::belongs_to_project(
                                &baseline.entries,
                                &dir.identity_marker,
                            ) => {}
                        Ok(_) => {
                            return Err(SetupError {
                                message: "the protected-path baseline belongs to another project identity".to_owned(),
                            });
                        }
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {
                            return Err(SetupError {
                                message: format!(
                                    "the protected-path baseline is missing while confined calls are active in {}",
                                    dir.dir.display()
                                ),
                            });
                        }
                        Err(source) => return Err(setup(source)),
                    }
                }
                active_file.lock_shared().map_err(setup)?;
                drop(baseline_file);
                Begin {
                    guard: Guard {
                        dir,
                        active: active_file,
                        recovered: Report {
                            restored: Vec::new(),
                            failed: Vec::new(),
                        },
                        pending: self.pending.clone(),
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
    /// protected entry holds, or a project with no baseline yet, is a
    /// no-op. Any other failure fails the tool: the caller reports it, so
    /// the approved write is never silently restored away.
    pub(crate) fn note_approved_write(&self, workspace: &Path, path: &Path) -> Result<(), String> {
        let dir = baseline::state_paths(&self.home, workspace)?;
        if !dir.dir.exists() {
            // No state yet: no baseline can hold the path, and the first
            // confined call's begin will snapshot it.
            return Ok(());
        }
        let baseline_file =
            baseline::open_lock(&dir.baseline_lock).map_err(|source| source.to_string())?;
        baseline_file.lock().map_err(|source| source.to_string())?;
        let mut entries = match pending_entries(&self.pending, &dir) {
            Some(entries) => entries,
            None => match baseline::read_baseline(&dir.baseline) {
                Ok(baseline)
                    if baseline::belongs_to_project(&baseline.entries, &dir.identity_marker) =>
                {
                    baseline.entries
                }
                Ok(_) => {
                    return Err(
                        "the protected-path baseline belongs to another project identity"
                            .to_owned(),
                    );
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(error.to_string()),
            },
        };
        let path = baseline::canonical(path).unwrap_or_else(|| path.to_path_buf());
        let mut changed = false;
        for entry in &mut entries {
            let entry_path = Path::new(&entry.path);
            let comparable_entry =
                baseline::canonical(entry_path).unwrap_or_else(|| entry_path.to_path_buf());
            // The entry holds the path itself, or the directory holding
            // it: re-snapshotting the entry records the approved landing.
            if comparable_entry == path
                || (matches!(entry.kind, baseline::EntryKind::Dir { .. })
                    && path.starts_with(&comparable_entry))
            {
                *entry = baseline::snapshot_entry(entry_path, &entry.display)?;
                changed = true;
            }
        }
        if changed {
            // Publish the new entry to in-process guards before trying disk.
            // If persistence fails, they must not read and restore the old
            // baseline over the approved landing.
            set_pending(&self.pending, &dir.dir, entries.clone());
            if let Err(source) = baseline::write_baseline(&dir.baseline, &entries) {
                let message = match baseline::invalidate_baseline(&dir.baseline) {
                    Ok(()) => source.to_string(),
                    Err(invalidation) => {
                        format!("{source}; stale baseline could not be invalidated: {invalidation}")
                    }
                };
                return Err(message);
            }
            clear_pending(&self.pending, &dir.dir);
        }
        Ok(())
    }
}

impl Guard {
    /// Compares the protected paths against the baseline, restores any
    /// difference and reports it, then releases both locks. Runs once, after
    /// the call's final output is collected. The recovered stale baseline
    /// joins the call's own end, each path named once. A compare or
    /// baseline-read failure is never an empty report: it lands as a
    /// `failed` entry naming the baseline and its cause.
    pub(crate) fn finish(self) -> Report {
        let Self {
            dir,
            active,
            recovered,
            pending,
        } = self;
        let end = match baseline::open_lock(&dir.baseline_lock) {
            Ok(file) => {
                if file.lock().is_err() {
                    finish_failure("the baseline lock could not be taken")
                } else {
                    let report = match pending_entries(&pending, &dir) {
                        Some(entries) => {
                            let mut report = restore::compare_restore_entries(&entries);
                            match baseline::write_baseline(&dir.baseline, &entries) {
                                Ok(()) => clear_pending(&pending, &dir.dir),
                                Err(source) => {
                                    let message = match baseline::invalidate_baseline(&dir.baseline)
                                    {
                                        Ok(()) => source.to_string(),
                                        Err(invalidation) => format!(
                                            "{source}; stale baseline could not be invalidated: {invalidation}"
                                        ),
                                    };
                                    report.failed.push(RestoreFailure {
                                        path: "baseline.json".to_owned(),
                                        message,
                                    });
                                }
                            }
                            report
                        }
                        None if !dir.baseline.exists() => {
                            // No baseline while a holder exists: restoring
                            // is impossible, so report the missing state.
                            finish_failure(&format!(
                                "the protected-path baseline is missing from {}",
                                dir.baseline.display()
                            ))
                        }
                        None => match restore::compare_restore_report(&dir) {
                            Ok(report) => report,
                            Err(source) => finish_failure(&source.to_string()),
                        },
                    };
                    drop(file);
                    report
                }
            }
            Err(source) => finish_failure(&source.to_string()),
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
fn recover(dir: &baseline::StatePaths) -> Result<Option<Report>, io::Error> {
    let baseline_file = baseline::open_lock(&dir.baseline_lock)?;
    baseline_file.lock()?;
    let active_file = baseline::open_lock(&dir.active_lock)?;
    match active_file.try_lock() {
        Ok(()) => {
            // No holder: the last call died before its end ran.
            let report = restore::compare_restore_report(dir)?;
            if dir.baseline.exists() {
                std::fs::remove_file(&dir.baseline)?;
            }
            drop(baseline_file);
            drop(active_file);
            Ok((!report.is_empty()).then_some(report))
        }
        Err(TryLockError::WouldBlock) => {
            // Calls are live: the baseline stays fixed for them. A colliding
            // project key must not join another identity's active baseline.
            if let Ok(baseline) = baseline::read_baseline(&dir.baseline)
                && !baseline::belongs_to_project(&baseline.entries, &dir.identity_marker)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "the protected-path baseline belongs to another project identity",
                ));
            }
            drop(baseline_file);
            drop(active_file);
            Ok(None)
        }
        Err(TryLockError::Error(source)) => Err(source),
    }
}

/// A finish that never compared: one `failed` entry naming the baseline
/// file and its cause, never an empty report.
fn finish_failure(cause: &str) -> Report {
    Report {
        restored: Vec::new(),
        failed: vec![RestoreFailure {
            path: "baseline.json".to_owned(),
            message: cause.to_owned(),
        }],
    }
}

/// A setup failure naming what could not be read or recorded.
#[cfg(any(target_os = "linux", test))]
fn setup(source: io::Error) -> SetupError {
    SetupError {
        message: source.to_string(),
    }
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

/// The mutually exclusive Linux signal policies. Existing socket and
/// `io_uring_setup` denials stay in both cases.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LinuxPolicy {
    /// Landlock signal scoping from ABI 6; no seccomp signal rule is added.
    LandlockSignalScope,
    /// Deny signals to the supervisor with `EPERM` below ABI 6.
    SupervisorSeccomp(Vec<(i64, Vec<seccompiler::SeccompRule>)>),
}

/// Selects the Linux signal policy: the seccomp supervisor rule when the
/// kernel is below ABI 6, Landlock signal scope from ABI 6. One call from
/// the sandbox setup, keeping every other policy behavior unchanged.
#[cfg(target_os = "linux")]
pub(crate) fn linux_policy() -> Result<LinuxPolicy, String> {
    use landlock::{Access as _, Compatible as _, RulesetAttr as _};
    if landlock::Ruleset::default()
        .set_compatibility(landlock::CompatLevel::HardRequirement)
        .handle_access(landlock::AccessFs::from_all(landlock::ABI::V6))
        .and_then(|ruleset| ruleset.create())
        .is_ok()
    {
        return Ok(LinuxPolicy::LandlockSignalScope);
    }
    Ok(LinuxPolicy::SupervisorSeccomp(supervisor_rules(
        &supervisor(),
    )?))
}

/// Collects the supervisor pids: the session host itself, and the spawning
/// hub when the hub marker says one spawned this process. A hub-spawned
/// session is the hub's direct child, so the parent pid names the hub;
/// without the marker the hub slot names the session host, adding no rule.
#[cfg(target_os = "linux")]
pub(crate) fn supervisor() -> Supervisor {
    let session_pid = std::process::id();
    let session_pgid = process_group(None).unwrap_or(session_pid);
    if std::env::var_os(contract::HUB_SESSION_MARKER).is_some() {
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
pub(crate) fn process_group(pid: Option<u32>) -> Option<u32> {
    let raw = match pid {
        Some(pid) => Some(rustix::process::Pid::from_raw(i32::try_from(pid).ok()?)?),
        None => None,
    };
    let group = rustix::process::getpgid(raw).ok()?;
    u32::try_from(group.as_raw_pid()).ok()
}

/// This process's parent pid.
#[cfg(target_os = "linux")]
pub(crate) fn parent_pid() -> Option<u32> {
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
