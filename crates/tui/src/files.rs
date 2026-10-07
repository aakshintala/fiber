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
    // Past the first KEPT, a partial select keeps the smallest without
    // sorting the rest.
    if found.get(KEPT).is_some() {
        found.select_nth_unstable(KEPT);
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
    /// Why the worker could not start, and where each search is answered
    /// with it.
    unstarted: Option<(String, Sender<Input>)>,
}

impl Search {
    /// Starts the worker on its own thread: `listing` runs there, and each
    /// result goes to `out` as [`Input::Files`]. The thread is detached;
    /// it ends once this handle is dropped or `out` hangs up. A worker
    /// that cannot start answers every search with why, as [`unstarted`].
    ///
    /// [`unstarted`]: Self::unstarted
    pub(crate) fn spawn(
        listing: impl FnOnce() -> Result<Vec<String>, String> + Send + 'static,
        out: Sender<Input>,
    ) -> Self {
        let (jobs, inbox) = mpsc::channel::<(u64, String)>();
        let current = Arc::new(AtomicU64::new(0));
        let newest = Arc::clone(&current);
        let failed = out.clone();
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
        match worker {
            Ok(_) => Self {
                jobs,
                current,
                unstarted: None,
            },
            Err(error) => Self::unstarted(error.to_string(), failed),
        }
    }

    /// A worker that never started: every search is answered at once on
    /// `out` with `error`, which the panel shows as its error row.
    pub(crate) fn unstarted(error: String, out: Sender<Input>) -> Self {
        Self {
            jobs: mpsc::channel().0,
            current: Arc::new(AtomicU64::new(0)),
            unstarted: Some((error, out)),
        }
    }

    /// Asks for `query` at `generation`, abandoning the search before it.
    pub(crate) fn search(&self, generation: u64, query: String) {
        if let Some((error, out)) = &self.unstarted {
            let result = Err(error.clone());
            // A loop gone has no panel to show the error in.
            drop(out.send(Input::Files { generation, result }));
            return;
        }
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
