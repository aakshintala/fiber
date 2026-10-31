//! One temporary directory per test, removed on drop
//! (`docs/testing.md`, "Running tests").

use std::collections::hash_map::RandomState;
use std::fs;
use std::hash::BuildHasher;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use contract::clock::Clock;

use crate::MUST_SUCCEED_WITHIN;
use crate::clock::SystemClock;
use crate::deadline::Deadline;
use crate::process_group::{pids_exit, stop_closed};

/// How many names [`TempDir::new`] tries before giving up.
const ATTEMPTS: usize = 64;

/// How long [`command_line`] waits for its `ps` when the drop names a
/// holder: the test has already failed, so a hung `ps` must not add a
/// must-succeed wait per holder.
const COMMAND_LINE_TIMEOUT: Duration = Duration::from_secs(5);

static NEXT: AtomicU64 = AtomicU64::new(0);
static STATE: LazyLock<RandomState> = LazyLock::new(RandomState::new);

/// A directory under the system temporary directory, owned by the caller.
///
/// Drop removes it, including a nested directory left mode `0o000` or
/// `0o555`, once no live process holds it: a holder still running past the
/// bound fails the test naming the directory and the holder. A directory
/// the caller already removed is left alone.
pub struct TempDir {
    path: PathBuf,
    owner: Option<String>,
}

/// The name of the thread creating the directory, best-effort: the test's
/// name on a test thread, for the drop failure to name.
// Exempt: feeds only the drop failure message text naming the creating test; no consumer reads it.
// (`docs/testing/mutants.md`, "Proving a test bites").
#[cfg_attr(false, mutants::skip)]
fn creating_thread_name() -> Option<String> {
    thread::current().name().map(str::to_owned)
}

impl TempDir {
    /// Creates `<temp>/<prefix>-<8 lowercase hex digits>`.
    ///
    /// The directory did not exist: `create_dir` fails when the path is
    /// taken, and another suffix is drawn. Panics if creating the directory
    /// fails for any other reason, or after [`ATTEMPTS`] names were taken.
    pub fn new(prefix: &str) -> Self {
        Self::under(&std::env::temp_dir(), prefix)
    }

    /// Creates `<parent>/<prefix>-<8 lowercase hex digits>`, as [`TempDir::new`]
    /// but under `parent` instead of the system temporary directory. Root a
    /// directory that holds a socket or a Fiber home a `fiber` process runs in
    /// under a short parent such as `/tmp`: socket paths are limited to SUN_LEN bytes.
    pub fn under(parent: &Path, prefix: &str) -> Self {
        let path = create(parent, prefix, std::iter::from_fn(suffix));
        Self {
            path,
            owner: creating_thread_name(),
        }
    }

    /// Writes `content` to `rel` under the directory, creating parent
    /// directories first.
    ///
    /// # Panics
    ///
    /// When a parent directory cannot be created or the file cannot be
    /// written.
    #[allow(
        clippy::panic,
        clippy::unwrap_used,
        reason = "a test helper; a failure is the test's"
    )]
    pub fn write(&self, rel: &str, content: &str) {
        let path = self.path.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    /// The directory's path.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // A drop while unwinding never waits and never panics: the original
        // failure is already on its way out, so this only tries to remove.
        if thread::panicking() {
            remove(&self.path);
            return;
        }
        let clock = SystemClock;
        let holders = wait_for_holders(&self.path, &clock, clock.now(), MUST_SUCCEED_WITHIN);
        if !holders.is_empty() {
            fail_on_holders(&self.path, self.owner.as_deref(), &holders);
        }
        remove(&self.path);
    }
}

/// Removes `path`, making a read-only tree writable first. A directory the
/// test already removed makes the second removal a no-op. Failure is
/// ignored: only a live holder fails the drop.
fn remove(path: &Path) {
    if fs::remove_dir_all(path).is_ok() {
        return;
    }
    restore_modes(path);
    match fs::remove_dir_all(path) {
        Ok(()) | Err(_) => {}
    }
}

/// Panics naming the directory, the test that created it and every holder
/// pid with its command line, from a final fresh scan.
#[allow(
    clippy::panic,
    reason = "the drop names the holders that outlived the test; a failure past the bound fails the test"
)]
fn fail_on_holders(path: &Path, owner: Option<&str>, holders: &[u32]) -> ! {
    panic!("{}", holders_message(path, owner, holders))
}

/// The drop failure: the directory, the creating test and every holder pid
/// with its command line, from a final fresh scan.
fn holders_message(path: &Path, owner: Option<&str>, holders: &[u32]) -> String {
    let holders = holders
        .iter()
        .map(|pid| {
            let command = command_line(*pid);
            format!("pid {pid} ({command})")
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "temp dir {} for test {} outlived by {holders}",
        path.display(),
        owner.unwrap_or("an unnamed test")
    )
}

/// The command line of one process for the drop failure, or `<gone>` when it
/// cannot be read: a best-effort diagnostic, never a wait.
// Exempt: feeds only the drop failure message text naming each holder pid; no consumer reads it.
// (`docs/testing/mutants.md`, "Proving a test bites").
#[cfg_attr(false, mutants::skip)]
fn command_line(pid: u32) -> String {
    const GONE: &str = "<gone>";
    let mut child = match Command::new("ps")
        .args(["-o", "args=", "-p", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return GONE.to_owned(),
    };
    let mut stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => return GONE.to_owned(),
    };
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let mut out = String::new();
        let read = std::io::Read::read_to_string(&mut stdout, &mut out).map(|_| out);
        match done.send(read) {
            Ok(()) | Err(_) => {}
        }
    });
    match Deadline::after(COMMAND_LINE_TIMEOUT).recv(&finished) {
        Ok(Ok(out)) => {
            let line = out.trim();
            if line.is_empty() {
                GONE.to_owned()
            } else {
                line.to_owned()
            }
        }
        Ok(Err(_)) | Err(_) => {
            match child.kill() {
                Ok(()) | Err(_) => {}
            }
            match child.wait() {
                Ok(_) | Err(_) => {}
            }
            GONE.to_owned()
        }
    }
}

/// Makes every directory under `path` writable, then readable, so a later
/// `remove_dir_all` can finish. A symlink is left alone: following it would
/// change a tree outside `path`.
fn restore_modes(path: &Path) {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return;
    };
    // `symlink_metadata` reports a symlink as not a directory.
    if !meta.is_dir() {
        return;
    }
    let mut perms = meta.permissions();
    perms.set_mode(0o755);
    if fs::set_permissions(path, perms).is_err() {
        return;
    }
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };
        restore_modes(&entry.path());
    }
}

/// The pids still holding `path` once `bound` past `start` runs out on
/// `clock`: scans, waits the listed pids out under what remains of the
/// bound, and rescans until nothing holds it or the bound expires. An
/// expired bound returns the first scan at once, so a post-bound fake clock
/// never waits.
fn wait_for_holders(path: &Path, clock: &dyn Clock, start: Instant, bound: Duration) -> Vec<u32> {
    loop {
        let holders = holder_pids(path);
        if holders.is_empty() {
            return holders;
        }
        let elapsed = clock.now().saturating_duration_since(start);
        if elapsed >= bound {
            return holders;
        }
        match pids_exit(&holders, Deadline::after(bound.saturating_sub(elapsed))) {
            true | false => {}
        }
        pause();
    }
}

/// One poll pause between rescan probes, holding no lock, like the existing
/// liveness loops.
// Exempt: only paces the holder rescan loop; the bound ends it either way, so no consumer
// sees a difference (`docs/testing/mutants.md`, "Proving a test bites").
#[cfg_attr(false, mutants::skip)]
fn pause() {
    let (_stop, stopped) = mpsc::channel::<()>();
    let _closed = stop_closed(&stopped);
}

/// The live holder pids of `path`: processes other than this one whose
/// working directory, executable image or (Linux only) open file resolves
/// under it. Vanished or unreadable processes are skipped, so a process
/// that exits mid-scan never fails it.
fn holder_pids(path: &Path) -> Vec<u32> {
    let Ok(dir) = fs::canonicalize(path) else {
        return Vec::new();
    };
    let own = std::process::id();
    let mut holders = Vec::new();
    for pid in candidate_pids() {
        if !scanned_pid(pid, own) {
            continue;
        }
        if holds(&dir, pid) {
            holders.push(pid);
        }
    }
    holders.sort();
    holders
}

/// Whether `pid` is even a scan candidate: an id of 1 or less reaches
/// processes the test does not own, and the test's own process is never
/// its own holder.
fn scanned_pid(pid: u32, own: u32) -> bool {
    pid > 1 && pid != own
}

/// Every pid the scan filters: the `/proc` entries naming one.
#[cfg(target_os = "linux")]
fn candidate_pids() -> Vec<u32> {
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut pids = Vec::new();
    for entry in entries.flatten() {
        if let Some(pid) = parse_pid(&entry.file_name()) {
            pids.push(pid);
        }
    }
    pids
}

/// The pid a `/proc` entry names: all digits, fitting in a pid.
#[cfg(target_os = "linux")]
fn parse_pid(name: &std::ffi::OsStr) -> Option<u32> {
    let name = name.to_str()?;
    name.parse().ok()
}

/// Whether `pid` holds `dir`: its working directory, its executable image
/// or one of its open files resolves under it.
#[cfg(target_os = "linux")]
fn holds(dir: &Path, pid: u32) -> bool {
    let proc = Path::new("/proc").join(pid.to_string());
    for file in ["cwd", "exe"] {
        if let Ok(target) = fs::read_link(proc.join(file))
            && support::peer::within_dir(dir, &target)
        {
            return true;
        }
    }
    let Ok(fds) = fs::read_dir(proc.join("fd")) else {
        return false;
    };
    for fd in fds.flatten() {
        if let Ok(target) = fs::read_link(fd.path())
            && support::peer::within_dir(dir, &target)
        {
            return true;
        }
    }
    false
}

/// Every pid the scan filters: whatever the kernel lists.
#[cfg(target_os = "macos")]
// Mutants run on Linux, where it is not compiled.
#[cfg_attr(false, mutants::skip)]
fn candidate_pids() -> Vec<u32> {
    support::peer::all_pids()
}

/// Whether `pid` holds `dir`: its working directory or executable image
/// resolves under it.
#[cfg(target_os = "macos")]
// Mutants run on Linux, where it is not compiled.
#[cfg_attr(false, mutants::skip)]
fn holds(dir: &Path, pid: u32) -> bool {
    support::peer::dirs_of(support::peer::Pid(pid))
        .iter()
        .any(|candidate| support::peer::within_dir(dir, candidate))
}

/// Eight lowercase hex digits. A process-wide counter is hashed so two calls
/// in one process differ; [`RandomState`]'s keys differ per process.
fn suffix() -> Option<String> {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let hash = STATE.hash_one(n) & 0xffff_ffff;
    Some(format!("{hash:08x}"))
}

/// Creates `parent`/`prefix`-`suffix` with `create_dir`. An existing path,
/// including one a killed run left behind, is skipped. `suffixes` ends or
/// [`ATTEMPTS`] names are taken, then this panics naming `prefix`.
#[allow(
    clippy::panic,
    reason = "a test directory that cannot be created cannot run its test"
)]
fn create(parent: &Path, prefix: &str, suffixes: impl Iterator<Item = String>) -> PathBuf {
    for suffix in suffixes.take(ATTEMPTS) {
        let path = parent.join(format!("{prefix}-{suffix}"));
        match fs::create_dir(&path) {
            Ok(()) => return path,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => panic!("creating {}: {err}", path.display()),
        }
    }
    panic!("no unique directory for prefix {prefix}");
}

#[cfg(test)]
#[path = "temp_dir_tests.rs"]
mod tests;
