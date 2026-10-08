//! The worker threads one search reads its sessions on (`docs/tools.md`,
//! "Searching past sessions"): scoped threads that claim sessions from one
//! shared queue, all finished before the search returns.

use std::io;
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};
use std::thread::{self, Scope};

use contract::tool::Cancel;

use super::Collect;

/// One worker's body, handed to a [`Spawn`].
pub(super) type Body<'scope> = Box<dyn FnOnce() + Send + 'scope>;

/// Starts one worker on its own thread in `scope`. An error means the thread
/// did not start.
pub(super) type Spawn<'a> = dyn for<'scope, 'env> Fn(&'scope Scope<'scope, 'env>, Body<'scope>) -> io::Result<()>
    + Sync
    + 'a;

/// The [`Spawn`] a search uses: a thread named `session-search`.
pub(super) fn spawn<'scope>(
    scope: &'scope Scope<'scope, '_>,
    body: Body<'scope>,
) -> io::Result<()> {
    thread::Builder::new()
        .name("session-search".to_owned())
        .spawn_scoped(scope, body)
        .map(drop)
}

/// How many workers search `sessions` sessions: as many threads as
/// `parallelism` reports, the calling thread among them, never more than the
/// sessions, and one when the parallelism is unknown.
pub(super) fn count(parallelism: io::Result<NonZeroUsize>, sessions: usize) -> usize {
    if sessions == 0 {
        return 0;
    }
    parallelism.map_or(1, NonZeroUsize::get).min(sessions)
}

/// What the workers found.
#[derive(Default)]
pub(super) struct Searched {
    /// Each worker's hits, every hit counted and the best kept.
    pub(super) hits: Vec<Collect>,
    /// Each session's problems, by its index in the session list: `None`
    /// for a session with none, or one never read.
    pub(super) problems: Vec<Option<Collect>>,
}

/// What one worker found: its hits, and the problems of each session it
/// read that had any, by session index.
type Found = (Collect, Vec<(usize, Collect)>);

/// Searches `sessions` with `search` on `workers` threads, the calling
/// thread one of them, each session into its own [`Collect`] keeping
/// `limit` hits. A worker checks `cancel` after it claims a session and
/// reads no more once it is cancelled. When `spawn` fails, no further
/// thread starts and the running workers read the rest.
pub(super) fn run(
    sessions: &[&Path],
    workers: usize,
    limit: usize,
    cancel: &dyn Cancel,
    search: &(dyn Fn(&Path, &mut Collect) + Sync),
    spawn: &Spawn<'_>,
) -> Searched {
    if sessions.is_empty() {
        return Searched::default();
    }
    let next = AtomicUsize::new(0);
    let found: Mutex<Vec<Found>> = Mutex::new(Vec::new());
    let work = || {
        let mut hits = Collect::new(limit);
        let mut problems = Vec::new();
        let claims = std::iter::repeat_with(|| next.fetch_add(1, Ordering::Relaxed));
        for (index, dir) in claims.map_while(|index| sessions.get(index).map(|dir| (index, *dir))) {
            if cancel.is_cancelled() {
                break;
            }
            let mut session = Collect::new(limit);
            search(dir, &mut session);
            let taken = session.take_problems();
            if !taken.is_empty() {
                problems.push((index, taken));
            }
            hits.merge(session);
        }
        found
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((hits, problems));
    };
    thread::scope(|scope| {
        for _ in 1..workers {
            if spawn(scope, Box::new(&work)).is_err() {
                break;
            }
        }
        work();
    });
    let mut searched = Searched {
        hits: Vec::new(),
        problems: std::iter::repeat_with(|| None)
            .take(sessions.len())
            .collect(),
    };
    let found = found.into_inner().unwrap_or_else(PoisonError::into_inner);
    for (hits, problems) in found {
        searched.hits.push(hits);
        for (index, taken) in problems {
            if let Some(slot) = searched.problems.get_mut(index) {
                *slot = Some(taken);
            }
        }
    }
    searched
}

#[cfg(test)]
#[path = "workers_tests.rs"]
mod tests;
