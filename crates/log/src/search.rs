//! Searching past sessions (`docs/tools.md`, "Searching past sessions"):
//! one raw pass per log with the ripgrep crates, no index, nothing written.
//! [`SessionScan::scan`] is the one entry every search goes through.

mod fields;
mod read;
mod session;
mod text;

use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashMap};
use std::fs::{DirEntry, File};
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

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
        if query.text.is_empty() || cancel.is_cancelled() {
            return Found::default();
        }
        let mut out = Collect::new(query.limit);
        match Text::new(&query.text) {
            Ok(text) => Walk {
                scan: self,
                text: &text,
                all_projects: query.all_projects,
                cancel,
                out: &mut out,
                ours: HashMap::new(),
            }
            .run(),
            Err(problem) => out.problem(problem),
        }
        out.found()
    }
}

/// One search's walk over the sessions it reads. Every step below
/// `projects/` refuses a link, so nothing outside the declared directory is
/// read.
struct Walk<'a> {
    /// The scanner.
    scan: &'a SessionScan,
    /// The query.
    text: &'a Text,
    /// Whether every project is searched.
    all_projects: bool,
    /// The call's signal.
    cancel: &'a dyn Cancel,
    /// Where hits and problems go.
    out: &'a mut Collect,
    /// Whether each recorded workspace seen in this call is in the
    /// calling session's project.
    ours: HashMap<String, bool>,
}

impl Walk<'_> {
    /// Searches every project in scope, one at a time.
    fn run(&mut self) {
        let projects = self.scan.projects();
        if !self.all_projects {
            let own = projects.join(project_key(self.scan.own()));
            return self.project(&own);
        }
        let Some(dirs) = self.list(&projects) else {
            return;
        };
        for dir in dirs {
            if self.cancel.is_cancelled() {
                return;
            }
            self.project(&dir);
        }
    }

    /// Searches the sessions of one project directory.
    fn project(&mut self, dir: &Path) {
        if !self.directory(dir) {
            return;
        }
        let sessions = dir.join("sessions");
        if !self.directory(&sessions) {
            return;
        }
        let Some(dirs) = self.list(&sessions) else {
            return;
        };
        for dir in dirs {
            if self.cancel.is_cancelled() {
                return;
            }
            self.session(&dir);
        }
    }

    /// Searches one session directory: one whose first line is a
    /// `session_started` in scope.
    fn session(&mut self, dir: &Path) {
        if !self.directory(dir) {
            return;
        }
        let Some(name) = dir.file_name() else {
            return;
        };
        let id = SessionId(name.to_string_lossy().into_owned());
        let path = dir.join(EVENTS);
        match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return,
            Err(error) => return self.unreadable(&path, &error),
            Ok(meta) if meta.is_symlink() => return self.link(&path),
            Ok(meta) if !meta.is_file() => return,
            Ok(_) => {}
        }
        let log = match File::open(&path) {
            Ok(log) => log,
            Err(error) => return self.unreadable(&path, &error),
        };
        let mut first = Vec::new();
        let read = BufReader::new(Cancelling::new(&log, self.cancel)).read_until(b'\n', &mut first);
        if let Err(error) = read {
            if !self.cancel.is_cancelled() {
                self.unreadable(&path, &error);
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
        session::search(self.text, &session, self.cancel, self.out);
    }

    /// Whether a session recorded in `workspace` is in the calling
    /// session's project, as resume checks it (`docs/state.md`, "A slug
    /// can collide"). Each workspace's identity is found once per call.
    fn ours(&mut self, workspace: &str) -> bool {
        if let Some(ours) = self.ours.get(workspace) {
            return *ours;
        }
        let ours = (self.scan.identity)(Path::new(workspace)) == self.scan.own();
        self.ours.insert(workspace.to_owned(), ours);
        ours
    }

    /// Whether `path` is a directory to walk. A link is a problem; a path
    /// that is missing or not a directory is passed over.
    fn directory(&mut self, path: &Path) -> bool {
        match std::fs::symlink_metadata(path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => {
                self.unreadable(path, &error);
                false
            }
            Ok(meta) if meta.is_symlink() => {
                self.link(path);
                false
            }
            Ok(meta) => meta.is_dir(),
        }
    }

    /// The entries of directory `path`, sorted; `None` when it cannot be
    /// listed, which is a problem unless it is missing.
    fn list(&mut self, path: &Path) -> Option<Vec<PathBuf>> {
        match std::fs::read_dir(path) {
            Ok(entries) => {
                let mut paths: Vec<PathBuf> = entries
                    .filter_map(|next| entry(path, next, self.out))
                    .map(|entry| entry.path())
                    .collect();
                paths.sort();
                Some(paths)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => {
                self.unreadable(path, &error);
                None
            }
        }
    }

    /// Lists `path` as unreadable.
    fn unreadable(&mut self, path: &Path, error: &io::Error) {
        self.out
            .problem(format!("Could not read: {}: {error}", path.display()));
    }

    /// Lists `path` as a link, which the search does not follow.
    fn link(&mut self, path: &Path) {
        self.out.problem(format!("{} is a link", path.display()));
    }
}

/// One entry of the `read_dir` of `dir`: the entry, or `None` when the
/// entry cannot be read, which is listed as a discovery problem. Both
/// directory listings route through here, so one test covers them.
pub(super) fn entry(dir: &Path, next: io::Result<DirEntry>, out: &mut Collect) -> Option<DirEntry> {
    match next {
        Ok(entry) => Some(entry),
        Err(error) => {
            out.problem(format!("Could not read: {}: {error}", dir.display()));
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
    /// The kept hits, the worst on top.
    heap: BinaryHeap<Ranked>,
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
            heap: BinaryHeap::new(),
            total: 0,
            problems: Vec::new(),
            more_problems: 0,
        }
    }

    /// Counts `hit`, and keeps it while it is among the best `limit`.
    pub(super) fn hit(&mut self, hit: Hit) {
        self.total += 1;
        self.heap.push(Ranked(hit));
        if self.heap.len() > self.limit {
            self.heap.pop();
        }
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
        if !self.heap.iter().any(|kept| kept.0.session_id == *id) {
            return;
        }
        let mut kept = std::mem::take(&mut self.heap).into_vec();
        for ranked in kept.iter_mut().filter(|kept| kept.0.session_id == *id) {
            name.clone_into(&mut ranked.0.name);
        }
        self.heap = BinaryHeap::from(kept);
    }

    /// The kept hits, best first, with the counts.
    pub(super) fn found(self) -> Found {
        Found {
            hits: self
                .heap
                .into_sorted_vec()
                .into_iter()
                .map(|ranked| ranked.0)
                .collect(),
            total: self.total,
            problems: self.problems,
            more_problems: self.more_problems,
        }
    }
}

/// A hit ordered by rank: the smaller is the better. Messages and tool
/// inputs come before tool outputs, then newer before older; ties go to the
/// smaller session id, then the larger `seq`, then the label's order.
struct Ranked(Hit);

impl Ranked {
    /// The rank's key.
    fn key(&self) -> (bool, Reverse<u64>, &str, Reverse<u64>, Label) {
        let hit = &self.0;
        (
            hit.label == Label::ToolOutput,
            Reverse(hit.ts),
            hit.session_id.0.as_str(),
            Reverse(hit.seq.0),
            hit.label,
        )
    }
}

impl PartialEq for Ranked {
    fn eq(&self, other: &Self) -> bool {
        self.key() == other.key()
    }
}

impl Eq for Ranked {}

impl PartialOrd for Ranked {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Ranked {
    fn cmp(&self, other: &Self) -> Ordering {
        self.key().cmp(&other.key())
    }
}

#[cfg(test)]
#[path = "search_tests.rs"]
mod tests;
