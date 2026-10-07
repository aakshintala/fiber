//! File search for the `@` panel: one listing of the files git tracks,
//! searched on a worker thread that drops a search once a newer one is
//! asked for (`docs/tui.md`, "Keys" › "Rules").

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread;

use crate::Input;

/// How many paths a search keeps.
pub(crate) const KEPT: usize = 50;

/// How many paths a search checks between looks at whether it was
/// abandoned.
pub(crate) const CHECK_EVERY: usize = 1024;

/// A generation no search is tagged with: set on drop, it abandons the
/// search in flight.
const DROPPED: u64 = u64::MAX;

/// The files git tracks in the repository holding `workspace`, relative to
/// its root; or why there are none: the first line git wrote to stderr, or
/// the error running it.
pub(crate) fn list(workspace: &Path) -> Result<Vec<String>, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["ls-files", "-z", "--full-name", ":/"])
        .stdin(Stdio::null())
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(stderr
            .lines()
            .next()
            .map_or_else(|| output.status.to_string(), str::to_owned));
    }
    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect())
}

/// The first [`KEPT`] paths containing `query`, ignoring case: those whose
/// file name contains it first, then the shorter, then by path. `None`
/// when `cancelled` says so; it is asked every [`CHECK_EVERY`] paths.
pub(crate) fn rank(
    paths: &[String],
    query: &str,
    cancelled: impl Fn() -> bool,
) -> Option<Vec<String>> {
    let query = query.to_lowercase();
    let mut found: Vec<(bool, usize, &str)> = Vec::new();
    for (at, path) in paths.iter().enumerate() {
        if at % CHECK_EVERY == 0 && cancelled() {
            return None;
        }
        let lower = path.to_lowercase();
        if !lower.contains(&query) {
            continue;
        }
        let name = lower.rsplit('/').next().unwrap_or_default();
        found.push((!name.contains(&query), path.len(), path));
    }
    if found.len() > KEPT {
        found.select_nth_unstable(KEPT.saturating_sub(1));
        found.truncate(KEPT);
    }
    found.sort_unstable();
    Some(
        found
            .into_iter()
            .map(|(_, _, path)| path.to_owned())
            .collect(),
    )
}

/// The search worker: it lists once, then runs each search asked for,
/// skipping to the newest when several wait. Dropping it ends the worker
/// and abandons the search in flight.
pub(crate) struct Search {
    jobs: Sender<(u64, String)>,
    /// The newest generation asked for.
    current: Arc<AtomicU64>,
}

impl Search {
    /// Starts the worker on its own thread: `listing` runs there, and each
    /// result goes to `out` as [`Input::Files`]. The thread is detached;
    /// it ends once this handle is dropped or `out` hangs up.
    pub(crate) fn spawn(
        listing: impl FnOnce() -> Result<Vec<String>, String> + Send + 'static,
        out: Sender<Input>,
    ) -> Self {
        let (jobs, inbox) = mpsc::channel::<(u64, String)>();
        let current = Arc::new(AtomicU64::new(0));
        let newest = Arc::clone(&current);
        let worker = thread::Builder::new()
            .name("tui-files".to_owned())
            .spawn(move || {
                let listed = listing();
                while let Ok(mut job) = inbox.recv() {
                    while let Ok(next) = inbox.try_recv() {
                        job = next;
                    }
                    let (generation, query) = job;
                    let result = match &listed {
                        Ok(paths) => {
                            let cancelled = || newest.load(Ordering::Relaxed) != generation;
                            let Some(found) = rank(paths, &query, cancelled) else {
                                continue;
                            };
                            Ok(found)
                        }
                        Err(error) => Err(error.clone()),
                    };
                    if out.send(Input::Files { generation, result }).is_err() {
                        return;
                    }
                }
            });
        // A worker that cannot start leaves the panel empty.
        drop(worker);
        Self { jobs, current }
    }

    /// Asks for `query` at `generation`, abandoning the search before it.
    pub(crate) fn search(&self, generation: u64, query: String) {
        self.current.store(generation, Ordering::Relaxed);
        // A worker gone has nothing left to search.
        drop(self.jobs.send((generation, query)));
    }
}

impl Drop for Search {
    fn drop(&mut self) {
        self.current.store(DROPPED, Ordering::Relaxed);
    }
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;
