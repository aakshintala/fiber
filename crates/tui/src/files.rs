//! File search for the `@` panel: one listing of the files git tracks,
//! or one bounded walk under a typed path, searched on a worker thread
//! that drops a search once a newer one is asked for (`docs/tui.md`,
//! "Keys" › "Rules").

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};

use crate::Input;

/// How many paths a search keeps.
pub(crate) const KEPT: usize = 50;

/// How many entries a path search examines across the base listing and
/// the walk before it stops.
pub(crate) const VISITS: usize = 100_000;

/// The bounds a search runs under: rows kept, entries examined, and
/// entries between looks at whether the search was abandoned.
/// `check_every` must be at least 1: the search looks when
/// `examined % check_every == 0`, which panics on 0.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    pub(crate) kept: usize,
    pub(crate) visits: usize,
    pub(crate) check_every: usize,
}

impl Limits {
    /// The bounds a live search runs under.
    pub(crate) const SEARCH: Limits = Limits {
        kept: KEPT,
        visits: VISITS,
        check_every: CHECK_EVERY,
    };
}

/// What a path search resolves against: the workspace for `../`, and
/// the home directory for `~/`.
pub struct Roots {
    pub(crate) workspace: PathBuf,
    pub(crate) home: Option<PathBuf>,
}

impl Roots {
    /// Keeps `home` only when it names an absolute directory, as the
    /// launch and the prompt files rule: an empty or relative `HOME`
    /// is no home, so a `~/` search answers `Err("no home directory")`.
    pub fn new(workspace: PathBuf, home: Option<OsString>) -> Self {
        let home = home.and_then(|home| {
            let home = PathBuf::from(home);
            home.is_absolute().then_some(home)
        });
        Self { workspace, home }
    }
}

/// What an `@` query searches: the files git tracks, or the entries
/// under one base directory.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Query<'a> {
    /// `typed` does not start with `/`, `~/` or `../`.
    Workspace(&'a str),
    /// `base_text` is `typed` up to and including its last `/`, and
    /// `query` the rest.
    Path { base_text: &'a str, query: &'a str },
}

/// Splits `typed`, the text after the `@`, into a workspace query or a
/// path search: `/`, `~/` or `../` starts a path search, whose base is
/// the text up to its last `/`; anything else searches the files git
/// tracks.
pub(crate) fn split(typed: &str) -> Query<'_> {
    let path = typed.starts_with('/') || typed.starts_with("~/") || typed.starts_with("../");
    if !path {
        return Query::Workspace(typed);
    }
    match typed.rfind('/') {
        Some(at) => Query::Path {
            base_text: &typed[..at + 1],
            query: &typed[at + 1..],
        },
        // `/`, `~/` and `../` all hold a `/`, so this never runs.
        None => Query::Workspace(typed),
    }
}

/// Resolves a path search's `base_text` against `roots`: `~/rest` joins
/// the home directory, `/…` is itself, and `../…` joins the workspace.
pub(crate) fn resolve(base_text: &str, roots: &Roots) -> Result<PathBuf, String> {
    if let Some(rest) = base_text.strip_prefix("~/") {
        return match &roots.home {
            Some(home) => Ok(home.join(rest)),
            None => Err("no home directory".to_owned()),
        };
    }
    if base_text.starts_with('/') {
        return Ok(PathBuf::from(base_text));
    }
    Ok(roots.workspace.join(base_text))
}

/// Entries deeper under `base` whose paths hold `query`: one walk with
/// the `ignore` crate, honouring `.gitignore` even outside a repository,
/// skipping `.git`, and stopping with at most `room` rows or `limits`
/// examined entries in total. Rows read `base_text` plus the path
/// relative to the base, a directory ending in `/`. Every entry the
/// walk reaches at depth 2 and below counts as examined, matching or
/// not; `.gitignore`-skipped entries never reach the walk and are not
/// counted. `None` when `cancelled` says so: asked, with the shared
/// `examined` count, at every `check_every`-th examined entry.
fn walk(
    base: &Path,
    base_text: &str,
    query: &str,
    room: usize,
    limits: &Limits,
    examined: &mut usize,
    cancelled: impl Fn() -> bool,
) -> Option<Vec<String>> {
    if room == 0 {
        return Some(Vec::new());
    }
    let query = query.to_lowercase();
    let mut aborted = false;
    let mut builder = ignore::WalkBuilder::new(base);
    builder
        .hidden(false)
        .ignore(false)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(false)
        .parents(false)
        .require_git(false)
        .follow_links(false)
        .filter_entry(|entry| entry.depth() == 0 || entry.file_name().to_string_lossy() != ".git");
    let rows: Vec<String> = builder
        .build()
        .take_while(|item| {
            // The base listing owns depth 0 and 1; only deeper entries
            // and errors the walk yields count against the visit bound.
            let countable = match item {
                Err(_) => true,
                Ok(entry) => entry.depth() >= 2,
            };
            if !countable {
                return true;
            }
            if *examined >= limits.visits {
                return false;
            }
            let at = *examined;
            *examined += 1;
            if at.is_multiple_of(limits.check_every) && cancelled() {
                aborted = true;
                return false;
            }
            true
        })
        .filter_map(|item| {
            let entry = item.ok()?;
            if entry.depth() < 2 {
                return None;
            }
            let rel = entry.path().strip_prefix(base).ok()?;
            let rel = rel
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            if !rel.to_lowercase().contains(&query) {
                return None;
            }
            if !listable(&rel) {
                return None;
            }
            let mut row = format!("{base_text}{rel}");
            let dir = entry
                .file_type()
                .map(|kind| is_dir(kind, entry.path()))
                .unwrap_or(false);
            if dir {
                row.push('/');
            }
            Some(row)
        })
        .take(room)
        .collect();
    if aborted {
        return None;
    }
    Some(rows)
}

/// The rows a path search shows for `query` under `base`: the base
/// directory's own matching entries first, then the walk's, at most
/// `limits.kept` rows over at most `limits.visits` examined entries.
/// `None` when `cancelled` says so: before the first read, at the first
/// examined entry and every `check_every`-th one after, and once more
/// before returning rows. `Some(Err)` when the base cannot be read.
pub(crate) fn find(
    base: &Path,
    base_text: &str,
    query: &str,
    limits: &Limits,
    cancelled: impl Fn() -> bool,
) -> Option<Result<Vec<String>, String>> {
    if cancelled() {
        return None;
    }
    let mut examined = 0;
    let mut rows = match entries(base, base_text, query, limits, &mut examined, &cancelled)? {
        Ok(rows) => rows,
        Err(error) => return Some(Err(error)),
    };
    let room = limits.kept.saturating_sub(rows.len());
    rows.extend(walk(
        base,
        base_text,
        query,
        room,
        limits,
        &mut examined,
        &cancelled,
    )?);
    if cancelled() {
        return None;
    }
    Some(Ok(rows))
}

/// Whether `path` with `file_type` is a directory: its type is one, or
/// a symlink whose target is one, so a linked directory stays
/// navigable. The walk never follows links, so a symlinked directory
/// inside it is still a row ending in `/` but is not descended.
fn is_dir(file_type: std::fs::FileType, path: &Path) -> bool {
    file_type.is_dir()
        || (file_type.is_symlink() && std::fs::metadata(path).is_ok_and(|meta| meta.is_dir()))
}

/// Whether `name` can be a row: a row draws on one panel line and is
/// inserted into the draft, where a control character such as a newline
/// or escape would break the panel and the draft. The lossy U+FFFD form
/// of a non-UTF-8 name is not a control character and is listed.
fn listable(name: &str) -> bool {
    !name.chars().any(char::is_control)
}

/// The base directory's own entries whose names hold `query`, ignoring
/// case: directories first, then by lower-cased name, then by name. A
/// directory row ends in `/`. Reads at most what the `examined` budget
/// under `limits` still allows, in `read_dir` order before the sort, and
/// keeps at most `limits.kept` rows; every entry read counts as
/// examined, listed or not. `None` when `cancelled` says so, asked at
/// the first examined entry and every `check_every`-th one after.
fn entries(
    base: &Path,
    base_text: &str,
    query: &str,
    limits: &Limits,
    examined: &mut usize,
    cancelled: impl Fn() -> bool,
) -> Option<Result<Vec<String>, String>> {
    let room = limits.visits.saturating_sub(*examined);
    let read = match std::fs::read_dir(base) {
        Ok(read) => read,
        Err(error) => return Some(Err(error.to_string())),
    };
    let query = query.to_lowercase();
    let mut found: Vec<(bool, String, String)> = Vec::new();
    for entry in read.take(room) {
        if (*examined).is_multiple_of(limits.check_every) && cancelled() {
            return None;
        }
        *examined += 1;
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().into_owned();
        if !listable(&name) {
            continue;
        }
        if !name.to_lowercase().contains(&query) {
            continue;
        }
        let is_dir = entry
            .file_type()
            .map(|kind| is_dir(kind, &entry.path()))
            .unwrap_or(false);
        let mut row = format!("{base_text}{name}");
        if is_dir {
            row.push('/');
        }
        found.push((!is_dir, name.to_lowercase(), row));
    }
    found.sort_unstable();
    found.truncate(limits.kept);
    Some(Ok(found.into_iter().map(|(_, _, row)| row).collect()))
}

/// How many paths a search checks between looks at whether it was
/// abandoned.
pub(crate) const CHECK_EVERY: usize = 1024;

/// A generation no search is tagged with: set on drop, it abandons the
/// search in flight.
const DROPPED: u64 = u64::MAX;

/// The files git tracks in the repository holding `workspace`, relative to
/// its root; or why there are none: the first line git wrote to stderr, or
/// the error running it.
pub fn list(workspace: &Path) -> Result<Vec<String>, String> {
    let output = support::git::command("git")
        .arg("-C")
        .arg(workspace)
        .args(["ls-files", "-z", "--full-name", ":/"])
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

/// A workspace listing with each path's lowercase form, made once when the
/// listing arrives so a search never lowercases a path.
pub(crate) struct Paths {
    /// Each path and its lowercase form; `None` when that is the path
    /// itself, so an already lowercase path keeps no second copy.
    paths: Vec<(String, Option<Box<str>>)>,
}

impl Paths {
    /// Lowercases each of `paths` once.
    pub(crate) fn new(paths: Vec<String>) -> Self {
        crate::work::add(|work| work.path_lowercases += paths.len());
        let paths = paths
            .into_iter()
            .map(|path| {
                let lower = path.to_lowercase();
                let lower = (lower != path).then(|| lower.into_boxed_str());
                (path, lower)
            })
            .collect();
        Self { paths }
    }
}

/// The first [`KEPT`] paths containing `query`, ignoring case: those whose
/// file name contains it first, then the shorter, then by path. `None`
/// when `cancelled` says so; it is asked every [`CHECK_EVERY`] paths.
pub(crate) fn rank(
    paths: &Paths,
    query: &str,
    cancelled: impl Fn() -> bool,
) -> Option<Vec<String>> {
    let query = query.to_lowercase();
    let mut found: Vec<(bool, usize, &str)> = Vec::new();
    for (at, (path, lower)) in paths.paths.iter().enumerate() {
        if at % CHECK_EVERY == 0 && cancelled() {
            return None;
        }
        let lower = lower.as_deref().unwrap_or(path);
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

type ListFiles = Box<dyn FnOnce() -> Result<Paths, String> + Send>;

/// A lazy workspace listing, retained across queries until its owner drops it.
pub struct Listing {
    paths: std::sync::LazyLock<Result<Paths, String>, ListFiles>,
}

impl Listing {
    /// Defers `listing` until the first workspace query; path queries never run it.
    pub fn new(listing: impl FnOnce() -> Result<Vec<String>, String> + Send + 'static) -> Self {
        Self {
            paths: std::sync::LazyLock::new(Box::new(move || listing().map(Paths::new))),
        }
    }
}

/// Searches the retained workspace listing or a typed path under `roots`.
/// Returns no answer when cancelled, otherwise at most fifty rows or an error row.
pub fn search(
    listing: &mut Listing,
    roots: &Roots,
    typed: &str,
    cancelled: impl Fn() -> bool,
) -> Option<Result<Vec<String>, String>> {
    if cancelled() {
        return None;
    }
    match split(typed) {
        Query::Workspace(seek) => match &*listing.paths {
            Ok(paths) => rank(paths, seek, cancelled).map(Ok),
            Err(error) => Some(Err(error.clone())),
        },
        Query::Path { base_text, query } => match resolve(base_text, roots) {
            Err(reason) => Some(Err(reason)),
            Ok(base) => find(&base, base_text, query, &Limits::SEARCH, cancelled),
        },
    }
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
    /// Starts the worker on its own thread: each result goes to `out` as
    /// [`Input::Files`]. The thread is detached; it ends once this handle
    /// is dropped or `out` hangs up. A worker that cannot start answers
    /// every search with why, as [`unstarted`].
    ///
    /// A workspace query searches the listing, which runs on the worker
    /// on the first workspace job only, so a path query typed at once
    /// never waits on `git ls-files`; a path query resolves its base
    /// against `roots` and walks it instead, ignoring the listing even
    /// when it failed.
    ///
    /// [`unstarted`]: Self::unstarted
    pub(crate) fn spawn(
        listing: impl FnOnce() -> Result<Vec<String>, String> + Send + 'static,
        roots: Roots,
        out: Sender<Input>,
    ) -> Self {
        let (jobs, inbox) = mpsc::channel::<(u64, String)>();
        let current = Arc::new(AtomicU64::new(0));
        let newest = Arc::clone(&current);
        let failed = out.clone();
        let worker = crate::sources::builder("tui-files").spawn(move || {
            let mut listing = Listing::new(listing);
            while let Ok(mut job) = inbox.recv() {
                while let Ok(next) = inbox.try_recv() {
                    job = next;
                }
                let (generation, query) = job;
                let cancelled = || newest.load(Ordering::Relaxed) != generation;
                let Some(result) = search(&mut listing, &roots, &query, cancelled) else {
                    continue;
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
