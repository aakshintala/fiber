//! The workspace picker's typed path: splitting the text, matching
//! the hub's one-level listings, queueing one `directories` request per
//! directory, and resolving Enter. Pure state: no `App`, no `Link`. The
//! caller sends what [`Typed::due`] returns while the link is up, and
//! feeds answers to [`Typed::answered`] and [`Typed::rejected`].

use std::collections::HashMap;

use serde_json::Value;

/// One listed directory: the absolute path the hub listed, `~/`
/// expanded, or `None` for a rejected or malformed answer, with its
/// subdirectory names in the order the hub sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Listing {
    /// The absolute path listed, or `None` when the listing failed.
    path: Option<String>,
    /// The subdirectory names, in hub order.
    names: Vec<String>,
}

/// The picker's focused row while typing, held as the row it names,
/// never as a position: an answer never moves a focused recent to
/// another workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Focus {
    /// No row is focused: above the list.
    Nothing,
    /// The completion naming `String`.
    Completion(String),
    /// The recent at the index fixed at open.
    Recent(usize),
}

/// What Enter resolves a typed path to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Resolved {
    /// Open the absolute workspace.
    Open(String),
    /// The path names no directory: show "no such directory".
    Missing,
    /// The listing Enter needs is not held yet: Enter does nothing.
    Loading,
}

/// The typed path's state: its text, whether Enter found no directory,
/// one cached listing per directory text, the directories queued to
/// ask, and the asked ones waiting for their answers, in send order.
#[derive(Debug)]
pub(super) struct Typed {
    /// The typed text, append-only through [`Typed::push`].
    text: String,
    /// Enter found no directory for the text: the list shows one dim
    /// row, "no such directory", until the next keystroke.
    missing: bool,
    /// The answered listings by the directory text as sent, for one
    /// picker opening.
    cache: HashMap<String, Listing>,
    /// The directories to ask for, in the order typed.
    queued: Vec<String>,
    /// The asked directories waiting for their answers, as
    /// `(id, directory)` in send order.
    inflight: Vec<(String, String)>,
    /// The focused row while typing.
    focus: Focus,
}

impl Typed {
    /// An empty typed path, with nothing focused.
    pub(super) fn new() -> Self {
        Self {
            text: String::new(),
            missing: false,
            cache: HashMap::new(),
            queued: Vec::new(),
            inflight: Vec::new(),
            focus: Focus::Nothing,
        }
    }

    /// A typed path already holding `root`, with only the root's own
    /// directory queued and nothing focused.
    pub(super) fn at(root: &str) -> Self {
        let mut typed = Self::new();
        typed.text = root.to_owned();
        if let Some(dir) = split(root).map(|(dir, _)| dir.to_owned()) {
            typed.queue_dir(&dir);
        }
        typed
    }

    /// The typed text.
    pub(super) fn text(&self) -> &str {
        &self.text
    }

    /// Whether Enter found no directory for the text.
    pub(super) fn missing(&self) -> bool {
        self.missing
    }

    /// The focused row while typing.
    pub(super) fn focus(&self) -> &Focus {
        &self.focus
    }

    /// Appends `c`: a keystroke that changes the directory text queues
    /// it, while one inside a name queues nothing. Focus becomes the
    /// first matching completion, else nothing.
    pub(super) fn push(&mut self, c: char) {
        self.text.push(c);
        self.missing = false;
        if let Some(dir) = split(&self.text).map(|(dir, _)| dir.to_owned()) {
            self.queue_dir(&dir);
        }
        self.refocus();
    }

    /// Removes the last character. False when the text became empty:
    /// the picker returns to the recents alone, dropping the queue but
    /// keeping the cache and the in-flight set.
    pub(super) fn pop(&mut self) -> bool {
        self.text.pop();
        self.missing = false;
        if self.text.is_empty() {
            self.queued.clear();
            return false;
        }
        if let Some(dir) = split(&self.text).map(|(dir, _)| dir.to_owned()) {
            self.queue_dir(&dir);
        }
        self.refocus();
        true
    }

    /// Moves focus one row: through the completions, then the recents.
    /// Down from nothing focuses the first row; up from nothing stays
    /// above the list; both ends clamp.
    pub(super) fn move_focus(&mut self, down: bool, recents: usize) {
        let completions = self.completions();
        let total = completions.len().saturating_add(recents);
        if total == 0 {
            self.focus = Focus::Nothing;
            return;
        }
        if matches!(self.focus, Focus::Nothing) {
            // Nothing sits above the list: down enters at the first
            // row, up stays above it.
            if down {
                self.focus = Self::row_focus(0, &completions);
            }
            return;
        }
        let at = match &self.focus {
            Focus::Nothing => None,
            Focus::Completion(name) => completions.iter().position(|next| *next == name),
            Focus::Recent(index) => Some(completions.len().saturating_add(*index)),
        };
        let Some(at) = at else {
            // The focused completion left the list: drop back above it.
            self.focus = Focus::Nothing;
            return;
        };
        let next = if down {
            at.saturating_add(1).min(total.saturating_sub(1))
        } else {
            at.saturating_sub(1)
        };
        self.focus = Self::row_focus(next, &completions);
    }

    /// Puts the focused completion into the path with a trailing `/`
    /// and queues its directory when not held, in flight or queued.
    /// False with a recent or nothing focused: Tab completes only
    /// completions, never recents.
    pub(super) fn tab(&mut self) -> bool {
        let Focus::Completion(name) = &self.focus else {
            return false;
        };
        let name = name.clone();
        let Some(dir) = split(&self.text).map(|(dir, _)| dir.to_owned()) else {
            return false;
        };
        self.text = format!("{dir}{name}/");
        self.missing = false;
        let listed = self.text.clone();
        self.queue_dir(&listed);
        self.refocus();
        true
    }

    /// The queued directories as `(id, directory)` in send order,
    /// moving each to the in-flight set under the id `mint` returned.
    pub(super) fn due(&mut self, mut mint: impl FnMut() -> String) -> Vec<(String, String)> {
        let queued = std::mem::take(&mut self.queued);
        let mut out = Vec::with_capacity(queued.len());
        for dir in queued {
            let id = mint();
            self.inflight.push((id.clone(), dir.clone()));
            out.push((id, dir));
        }
        out
    }

    /// Folds an accepted answer for `id`: a well-formed result stores
    /// its names with its absolute path, anything else an empty failed
    /// listing, held so the directory is never asked again. True when
    /// `id` was in flight.
    pub(super) fn answered(&mut self, id: &str, result: Option<&Value>) -> bool {
        let Some(at) = self.inflight.iter().position(|(sent, _)| sent == id) else {
            return false;
        };
        let (_, dir) = self.inflight.remove(at);
        let listing = result.map(parse_listing).unwrap_or(Listing {
            path: None,
            names: Vec::new(),
        });
        let current = split(&self.text).map(|(dir, _)| dir.to_owned());
        self.cache.insert(dir.clone(), listing);
        if current.as_deref() == Some(dir.as_str()) && matches!(self.focus, Focus::Nothing) {
            self.refocus();
        }
        true
    }

    /// Folds a rejection for `id` as a failed listing, keeping the
    /// recents with no notice. True when `id` was in flight.
    pub(super) fn rejected(&mut self, id: &str) -> bool {
        self.answered(id, None)
    }

    /// Forgets the in-flight ids, queueing their directories again in
    /// send order ahead of any directory already queued: a directory in
    /// flight at a disconnect is asked once more. The cache stays, and
    /// an answer under an old id matches nothing.
    pub(super) fn disconnected(&mut self) {
        let retry: Vec<String> = self.inflight.iter().map(|(_, dir)| dir.clone()).collect();
        self.inflight.clear();
        let mut queued = std::mem::take(&mut self.queued);
        self.queued = retry;
        self.queued.append(&mut queued);
    }

    /// Drops the in-flight ids without queueing them again: with the
    /// recents alone open a disconnect asks nothing more. The cache
    /// stays, and an answer under an old id matches nothing.
    pub(super) fn forget(&mut self) {
        self.inflight.clear();
    }

    /// Resolves the typed text with nothing focused: a completion's
    /// prefix would have focused it, so a name with at least one
    /// character is always missing, and only text ending in `/` reads
    /// its parent listing. Bare `~` queues the hub's home listing when
    /// not held, in flight or queued, and waits for it.
    pub(super) fn resolve(&mut self) -> Resolved {
        if self.text == "~" {
            let home = self
                .cache
                .get("~/")
                .and_then(|listing| listing.path.clone());
            match home {
                Some(path) => return Resolved::Open(trim_home(&path)),
                None if self.cache.contains_key("~/") => {
                    self.missing = true;
                    return Resolved::Missing;
                }
                None => {}
            }
            if self.inflight.iter().any(|(_, dir)| dir == "~/")
                || self.queued.iter().any(|dir| dir == "~/")
            {
                return Resolved::Loading;
            }
            self.queued.push("~/".to_owned());
            return Resolved::Loading;
        }
        let Some((dir, name)) =
            split(&self.text).map(|(dir, name)| (dir.to_owned(), name.to_owned()))
        else {
            self.missing = true;
            return Resolved::Missing;
        };
        if !self.cache.contains_key(dir.as_str()) {
            return Resolved::Loading;
        }
        if !name.is_empty() {
            self.missing = true;
            return Resolved::Missing;
        }
        let stripped = self.text.trim_end_matches('/');
        if stripped.is_empty() {
            return Resolved::Open("/".to_owned());
        }
        if stripped == "~" {
            let home = self
                .cache
                .get("~/")
                .and_then(|listing| listing.path.clone());
            match home {
                Some(path) => return Resolved::Open(trim_home(&path)),
                None => {
                    self.missing = true;
                    return Resolved::Missing;
                }
            }
        }
        let Some(at) = stripped.rfind('/') else {
            self.missing = true;
            return Resolved::Missing;
        };
        let (parent, last) = stripped.split_at(at.saturating_add(1));
        let Some(listing) = self.cache.get(parent) else {
            return Resolved::Loading;
        };
        match &listing.path {
            None => {
                self.missing = true;
                Resolved::Missing
            }
            Some(path) if listing.names.iter().any(|held| held == last) => {
                Resolved::Open(join(path, last))
            }
            Some(_) => {
                self.missing = true;
                Resolved::Missing
            }
        }
    }

    /// The workspace the focused completion opens: its listing's path
    /// joined with its name. `None` without a focused completion or a
    /// listed path.
    pub(super) fn completion_target(&self) -> Option<String> {
        let Focus::Completion(name) = &self.focus else {
            return None;
        };
        let (dir, _) = split(&self.text)?;
        let path = self.cache.get(dir)?.path.as_deref()?;
        Some(join(path, name))
    }

    /// The focused recent's index into the recents fixed at open.
    pub(super) fn recent(&self) -> Option<usize> {
        match &self.focus {
            Focus::Recent(index) => Some(*index),
            Focus::Completion(_) | Focus::Nothing => None,
        }
    }

    /// The current directory's names matching the typed name, in hub
    /// order: one listing of that directory, filtered at each
    /// keystroke, never a walk of the tree.
    pub(super) fn completions(&self) -> Vec<&str> {
        let Some((dir, name)) = split(&self.text) else {
            return Vec::new();
        };
        self.cache
            .get(dir)
            .map(|listing| matching(&listing.names, name).collect())
            .unwrap_or_default()
    }

    /// The focus for row `next`: a completion by its name, else the
    /// recent past the completions.
    fn row_focus(next: usize, completions: &[&str]) -> Focus {
        if let Some(name) = completions.get(next) {
            Focus::Completion((*name).to_owned())
        } else {
            Focus::Recent(next.saturating_sub(completions.len()))
        }
    }

    /// Queues `dir` unless held, in flight or already queued: at most
    /// one request per distinct directory text per picker opening.
    fn queue_dir(&mut self, dir: &str) {
        if self.cache.contains_key(dir) {
            return;
        }
        if self.inflight.iter().any(|(_, sent)| sent == dir) {
            return;
        }
        if self.queued.iter().any(|queued| queued == dir) {
            return;
        }
        self.queued.push(dir.to_owned());
    }

    /// Focuses the first matching completion, else nothing.
    fn refocus(&mut self) {
        let first = self.completions().into_iter().next().map(str::to_owned);
        self.focus = first.map(Focus::Completion).unwrap_or(Focus::Nothing);
    }
}

/// The typed text's directory, up to and including its last `/`, with
/// the name after it: `/a/b` gives (`/a/`, `b`), `~/w` gives (`~/`,
/// `w`), `/` gives (`/`, ``). `~` alone, relative text and an empty
/// text have no directory.
pub(super) fn split(text: &str) -> Option<(&str, &str)> {
    if !(text.starts_with('/') || text.starts_with("~/")) {
        return None;
    }
    let at = text.rfind('/')?;
    // `at` is the last `/`'s byte index, one byte wide: splitting past
    // it lands on the next character's boundary.
    Some(text.split_at(at.saturating_add(1)))
}

/// Joins a listed `path` with a `name`: every trailing `/` of the path
/// goes, so `/` joins to `/name`.
pub(super) fn join(path: &str, name: &str) -> String {
    format!("{}/{}", path.trim_end_matches('/'), name)
}

/// The `names` starting with `name`, case-sensitive, in hub order: a
/// hidden directory lists only when the typed name starts with `.`.
pub(super) fn matching<'a>(names: &'a [String], name: &str) -> impl Iterator<Item = &'a str> {
    names.iter().map(String::as_str).filter(move |held| {
        held.starts_with(name) && (name.starts_with('.') || !held.starts_with('.'))
    })
}

/// A failed listing: answered with no names and no path.
fn failed() -> Listing {
    Listing {
        path: None,
        names: Vec::new(),
    }
}

/// Parses a `directories` result: an array of string names with a
/// string path starting with `/`. Anything else is a failed listing.
fn parse_listing(result: &Value) -> Listing {
    let Value::Object(result) = result else {
        return failed();
    };
    let Some(names) = result.get("directories").and_then(Value::as_array) else {
        return failed();
    };
    let mut held = Vec::with_capacity(names.len());
    for name in names {
        let Some(name) = name.as_str() else {
            return failed();
        };
        held.push(name.to_owned());
    }
    let Some(path) = result.get("path").and_then(Value::as_str) else {
        return failed();
    };
    if !path.starts_with('/') {
        return failed();
    }
    Listing {
        path: Some(path.to_owned()),
        names: held,
    }
}

/// A home path as a workspace: trailing `/`s trimmed, and a path that
/// trims to nothing is `/`, never `""`.
fn trim_home(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".to_owned()
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
#[path = "typed_tests.rs"]
mod tests;
