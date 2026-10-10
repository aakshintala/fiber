//! Searching past sessions (`docs/tools.md`, "Searching past sessions"):
//! one raw pass per log with the ripgrep crates, no index, nothing written.
//! [`SessionScan::scan`] is the one entry every search goes through.

mod fields;
mod read;
mod session;
mod text;
mod workers;

use std::cmp::Reverse;
use std::collections::HashMap;
use std::fs::{DirEntry, File};
use std::io::{self, BufRead, BufReader};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::thread;

use contract::SessionId;
use contract::session_search::{Found, Hit, Label, Query, Scan};
use contract::tool::Cancel;

use crate::scan::started_from;
use crate::{EVENTS, project_key};
use read::Cancelling;
use session::Session;
use text::Text;

/// A project's identity path for a workspace, as resume looks a session up
/// (`docs/state.md`, "Projects").
pub type Identity = Arc<dyn Fn(&Path) -> PathBuf + Send + Sync>;

/// The scan behind `session_search` and `fiber sessions search`, for one
/// session's workspace.
pub struct SessionScan {
    /// Fiber home.
    home: PathBuf,
    /// The calling session's workspace.
    workspace: PathBuf,
    /// The project identity of a workspace.
    identity: Identity,
    /// The calling session's own project identity, found at first use and
    /// kept for the scanner's life.
    own: OnceLock<PathBuf>,
}

impl SessionScan {
    /// A scanner over `home` for a session in `workspace`. Nothing runs
    /// until the first call: `identity` may run git.
    pub fn new(home: &Path, workspace: &Path, identity: Identity) -> Self {
        Self {
            home: home.to_owned(),
            workspace: workspace.to_owned(),
            identity,
            own: OnceLock::new(),
        }
    }

    /// The calling session's own project identity.
    fn own(&self) -> &Path {
        self.own.get_or_init(|| (self.identity)(&self.workspace))
    }

    /// `projects/` in Fiber home.
    fn projects(&self) -> PathBuf {
        self.home.join("projects")
    }
}

impl Scan for SessionScan {
    fn scope(&self, all_projects: bool) -> PathBuf {
        let dir = if all_projects {
            self.projects()
        } else {
            self.projects().join(project_key(self.own()))
        };
        let mut dir = dir.into_os_string();
        dir.push("/");
        PathBuf::from(dir)
    }

    fn scan(&self, query: &Query, cancel: &dyn Cancel) -> Found {
        self.search(query, cancel, thread::available_parallelism())
    }
}

impl SessionScan {
    /// Runs `query` on as many threads as `parallelism` allows.
    fn search(
        &self,
        query: &Query,
        cancel: &dyn Cancel,
        parallelism: io::Result<NonZeroUsize>,
    ) -> Found {
        self.search_spawning(query, cancel, parallelism, &workers::spawn)
    }

    /// Runs `query`, starting each worker thread with `spawn`. The sessions
    /// are found first, on the calling thread; the workers then read them,
    /// and the answer is merged in the order the sessions were found.
    fn search_spawning(
        &self,
        query: &Query,
        cancel: &dyn Cancel,
        parallelism: io::Result<NonZeroUsize>,
        spawn: &workers::Spawn<'_>,
    ) -> Found {
        if query.text.is_empty() || cancel.is_cancelled() {
            return Found::default();
        }
        let mut out = Collect::new(query.limit);
        let text = match Text::new(&query.text) {
            Ok(text) => text,
            Err(problem) => {
                out.problem(problem);
                return out.found();
            }
        };
        let steps = self.discover(query.all_projects, cancel);
        let sessions: Vec<&Path> = steps
            .iter()
            .filter_map(|step| match step {
                Step::Session(dir) => Some(dir.as_path()),
                Step::Problem(_) => None,
            })
            .collect();
        let context = Context {
            scan: self,
            text: &text,
            all_projects: query.all_projects,
            cancel,
            ours: Mutex::new(HashMap::new()),
        };
        let searched = workers::run(
            &sessions,
            workers::count(parallelism, sessions.len()),
            query.limit,
            cancel,
            &|dir: &Path, out: &mut Collect| context.session(dir, out),
            spawn,
        );
        for hits in searched.hits {
            out.merge(hits);
        }
        let mut problems = searched.problems.into_iter();
        for step in steps {
            match step {
                Step::Problem(problem) => out.problem(problem),
                Step::Session(_) => {
                    if let Some(Some(session)) = problems.next() {
                        out.merge(session);
                    }
                }
            }
        }
        out.found()
    }

    /// The sessions in scope and the problems met finding them, in the
    /// order found: projects sorted, then each project's sessions sorted.
    /// A cancelled call stops before the next project. Every step below
    /// `projects/` refuses a link, so nothing outside the declared
    /// directory is read.
    fn discover(&self, all_projects: bool, cancel: &dyn Cancel) -> Vec<Step> {
        let mut steps = Vec::new();
        let projects = self.projects();
        if !all_projects {
            project(&projects.join(project_key(self.own())), &mut steps);
            return steps;
        }
        let Some(dirs) = list(&projects, &mut |problem| steps.push(Step::Problem(problem))) else {
            return steps;
        };
        for dir in dirs {
            if cancel.is_cancelled() {
                break;
            }
            project(&dir, &mut steps);
        }
        steps
    }
}

/// One step of a search, in the order the sessions were found.
enum Step {
    /// A problem met while finding the sessions.
    Problem(String),
    /// A session directory to read.
    Session(PathBuf),
}

/// Adds the sessions of project directory `dir` to `steps`.
fn project(dir: &Path, steps: &mut Vec<Step>) {
    let mut problem = |problem| steps.push(Step::Problem(problem));
    if !directory(dir, &mut problem) {
        return;
    }
    let sessions = dir.join("sessions");
    if !directory(&sessions, &mut problem) {
        return;
    }
    let Some(dirs) = list(&sessions, &mut problem) else {
        return;
    };
    steps.extend(dirs.into_iter().map(Step::Session));
}

/// What every worker of one search shares. Every step below `projects/`
/// refuses a link, so nothing outside the declared directory is read.
struct Context<'a> {
    /// The scanner.
    scan: &'a SessionScan,
    /// The query.
    text: &'a Text,
    /// Whether every project is searched.
    all_projects: bool,
    /// The call's signal.
    cancel: &'a dyn Cancel,
    /// Whether each recorded workspace seen in this call is in the
    /// calling session's project.
    ours: Mutex<HashMap<String, bool>>,
}

impl Context<'_> {
    /// Searches one session directory into `out`: one whose first line is
    /// a `session_started` in scope.
    fn session(&self, dir: &Path, out: &mut Collect) {
        if !directory(dir, &mut |problem| out.problem(problem)) {
            return;
        }
        let Some(name) = dir.file_name() else {
            return;
        };
        let id = SessionId(name.to_string_lossy().into_owned());
        let path = dir.join(EVENTS);
        let Some(kind) = kind_of(&path, &mut |problem| out.problem(problem)) else {
            return;
        };
        if !kind.is_file() {
            return;
        }
        let log = match File::open(&path) {
            Ok(log) => log,
            Err(error) => return out.problem(unreadable(&path, &error)),
        };
        let mut first = Vec::new();
        let read = BufReader::new(Cancelling::new(&log, self.cancel)).read_until(b'\n', &mut first);
        if let Err(error) = read {
            if !self.cancel.is_cancelled() {
                out.problem(unreadable(&path, &error));
            }
            return;
        }
        // A first line that is not a `session_started` is not a session.
        let Some((workspace, _)) = started_from(&first) else {
            return;
        };
        if !self.all_projects && !workspace.is_some_and(|w| self.ours(&w)) {
            return;
        }
        let session = Session {
            id: &id,
            dir,
            log: &log,
        };
        session::search(self.text, &session, self.cancel, out);
    }

    /// Whether a session recorded in `workspace` is in the calling
    /// session's project, as resume checks it (`docs/state.md`, "A slug
    /// can collide"). Each workspace's identity is found once per call: the
    /// lock is held while it is found, so a worker asking about the same
    /// workspace waits for that answer.
    fn ours(&self, workspace: &str) -> bool {
        let own = self.scan.own();
        let mut cache = self.ours.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(ours) = cache.get(workspace) {
            return *ours;
        }
        let ours = (self.scan.identity)(Path::new(workspace)) == own;
        cache.insert(workspace.to_owned(), ours);
        ours
    }
}

/// Whether `path` is a directory to walk. A link is a problem; a path that
/// is missing or not a directory is passed over.
fn directory(path: &Path, problem: &mut dyn FnMut(String)) -> bool {
    kind_of(path, problem).is_some_and(|kind| kind.is_dir())
}

/// The file type of `path`: `None` for a missing path, an unreadable one
/// or a link. An unreadable path or a link is a problem; a missing one is
/// passed over silently, like every other missing path in a search.
pub(super) fn kind_of(path: &Path, problem: &mut dyn FnMut(String)) -> Option<std::fs::FileType> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            problem(unreadable(path, &error));
            None
        }
        Ok(meta) if meta.file_type().is_symlink() => {
            problem(link(path));
            None
        }
        Ok(meta) => Some(meta.file_type()),
    }
}

/// The entries of directory `path`, sorted; `None` when it cannot be
/// listed, which is a problem unless it is missing.
fn list(path: &Path, problem: &mut dyn FnMut(String)) -> Option<Vec<PathBuf>> {
    match std::fs::read_dir(path) {
        Ok(entries) => {
            let mut paths: Vec<PathBuf> = entries
                .filter_map(|next| entry(path, next, problem))
                .map(|entry| entry.path())
                .collect();
            paths.sort();
            Some(paths)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            problem(unreadable(path, &error));
            None
        }
    }
}

/// The problem of `path` being unreadable.
pub(super) fn unreadable(path: &Path, error: &dyn std::fmt::Display) -> String {
    format!("Could not read: {}: {error}", path.display())
}

/// The problem of `path` being a link, which the search does not follow.
pub(super) fn link(path: &Path) -> String {
    format!("{} is a link", path.display())
}

/// One entry of the `read_dir` of `dir`: the entry, or `None` when the
/// entry cannot be read, which is passed to `problem`. Every directory
/// listing routes through here, so one test covers them.
pub(super) fn entry(
    dir: &Path,
    next: io::Result<DirEntry>,
    problem: &mut dyn FnMut(String),
) -> Option<DirEntry> {
    match next {
        Ok(entry) => Some(entry),
        Err(error) => {
            problem(unreadable(dir, &error));
            None
        }
    }
}

/// The most problems a search lists; the rest are counted.
const PROBLEMS: usize = 20;

/// What one search keeps while it runs: the best `limit` hits, the count of
/// every hit, and the first [`PROBLEMS`] problems.
pub(super) struct Collect {
    /// The most hits kept.
    limit: usize,
    /// The kept hits, at most twice `limit`; `found` sorts them.
    hits: Vec<Hit>,
    /// Every hit found.
    total: u64,
    /// The problems kept.
    problems: Vec<String>,
    /// The problems past [`PROBLEMS`].
    more_problems: u64,
}

impl Collect {
    /// Keeps at most `limit` hits.
    pub(super) fn new(limit: usize) -> Self {
        Self {
            limit,
            hits: Vec::new(),
            total: 0,
            problems: Vec::new(),
            more_problems: 0,
        }
    }

    /// Counts `hit`, and keeps it while it is among the best `limit`.
    pub(super) fn hit(&mut self, hit: Hit) {
        self.total += 1;
        self.keep(hit);
    }

    /// Keeps `hit` while it is among the best `limit`. Past twice `limit`
    /// hits only the best `limit` are kept, so memory stays bounded while
    /// the answer depends only on which hits were found, never on the
    /// order they arrived.
    fn keep(&mut self, hit: Hit) {
        if self.limit == 0 {
            return;
        }
        self.hits.push(hit);
        let cap = self.limit.saturating_mul(2);
        if self.hits.len() > cap {
            self.hits.select_nth_unstable_by(self.limit, |a, b| key(a).cmp(&key(b)));
            self.hits.truncate(self.limit);
        }
    }

    /// Adds what `other` found after what this one found: its count, its
    /// kept hits under this one's limit, and its problems in order.
    pub(super) fn merge(&mut self, other: Self) {
        self.total += other.total;
        for hit in other.hits {
            self.keep(hit);
        }
        for problem in other.problems {
            self.problem(problem);
        }
        self.more_problems += other.more_problems;
    }

    /// Moves this one's problems out, into one that holds only them.
    pub(super) fn take_problems(&mut self) -> Self {
        let mut taken = Self::new(self.limit);
        std::mem::swap(&mut taken.problems, &mut self.problems);
        taken.more_problems = std::mem::take(&mut self.more_problems);
        taken
    }

    /// Lists `problem`, or counts it once [`PROBLEMS`] are listed.
    pub(super) fn problem(&mut self, problem: String) {
        if self.problems.len() < PROBLEMS {
            self.problems.push(problem);
        } else {
            self.more_problems += 1;
        }
    }

    /// Gives the kept hits of session `id` its name, known once its whole
    /// log was read.
    pub(super) fn name(&mut self, id: &SessionId, name: &str) {
        for hit in self.hits.iter_mut().filter(|hit| hit.session_id == *id) {
            name.clone_into(&mut hit.name);
        }
    }

    /// The kept hits, best first, with the counts.
    pub(super) fn found(mut self) -> Found {
        self.hits.sort_by(|a, b| key(a).cmp(&key(b)));
        self.hits.truncate(self.limit);
        Found {
            hits: self.hits,
            total: self.total,
            problems: self.problems,
            more_problems: self.more_problems,
        }
    }
}

/// A hit's rank key: the smaller is the better. Messages and tool inputs
/// come before tool outputs, then newer before older; ties go to the
/// smaller session id, then the larger `seq`, then the label's order. Hits
/// that still tie order by log path, snippet and artifact, so the kept hits
/// depend only on which hits were found, never on the order they arrived.
fn key(hit: &Hit) -> Key<'_> {
    (
        hit.label == Label::ToolOutput,
        Reverse(hit.ts),
        hit.session_id.0.as_str(),
        Reverse(hit.seq.0),
        hit.label,
        &hit.log,
        &hit.snippet,
        hit.artifact.as_deref(),
    )
}

/// The rank's key.
type Key<'a> = (
    bool,
    Reverse<u64>,
    &'a str,
    Reverse<u64>,
    Label,
    &'a Path,
    &'a str,
    Option<&'a Path>,
);

#[cfg(test)]
#[path = "search_tests.rs"]
mod tests;
