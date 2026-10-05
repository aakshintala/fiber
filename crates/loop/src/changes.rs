//! Instruction files changing mid-session (`docs/system-prompt.md`, "When
//! something changes", and "The date"): the turn-start check, the own-edit
//! tracking after a call whose declared paths include a known file, the
//! subdirectory files those paths reach, and the resume fold that rebuilds
//! the same state from the log.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use contract::Envelope;
use contract::clock::Clock;
use contract::events::{
    DateChanged, Event, InstructionFile, InstructionReason, InstructionSent, Notice, OpeningMessage,
};
use contract::shapes::DeclaredEffects;

use crate::Error;
use crate::opening;

/// A file's size and modification time when last read.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Stat {
    size: u64,
    mtime: Option<SystemTime>,
}

/// `path`'s size and modification time now. `None` when they cannot be
/// read: the read below says whether the file is gone or unreadable.
fn stat_of(path: &str) -> Option<Stat> {
    let meta = std::fs::metadata(path).ok()?;
    Some(Stat {
        size: meta.len(),
        mtime: meta.modified().ok(),
    })
}

/// What the loop remembers per instruction file path: its size and time
/// when last read, and the size and time of the last `io_failed` notice
/// naming it. The content the model last had lives beside it in
/// [`State::had`], updated as instruction lines render.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct Tracked {
    stat: Option<Stat>,
    /// The failing size and time of the last `io_failed` notice naming
    /// the path: `Some(None)` when even the sizes could not be read.
    /// `None` means no notice yet, which differs from a failure whose
    /// sizes are unknown, so the first failure is always named.
    noticed: Option<Option<Stat>>,
}

/// The instruction files the model was sent and the directories checked,
/// carried across turns and rebuilt from the log on resume.
#[derive(Debug)]
pub(crate) struct State {
    /// One entry per path the model was sent or a check met, in path
    /// order. A path read as absent stays tracked with `stat: None`: when
    /// it exists again it is `created`, never a diff against the version
    /// before deletion.
    files: BTreeMap<String, Tracked>,
    /// Every directory checked: Fiber home and the repository chain, plus
    /// each subdirectory a call's paths reached. Canonical.
    dirs: BTreeSet<PathBuf>,
    /// The date last given, `YYYY-MM-DD`.
    date: String,
    /// Canonical Fiber home: its directory holds only `AGENTS.md`.
    home: PathBuf,
    /// The content the model last had per path, updated as instruction
    /// lines render, live and in `rebuild`.
    pub(crate) had: BTreeMap<String, String>,
    /// The subdirectory lines a call queued, written at the next step
    /// start.
    queued: Vec<Event>,
}

/// What one turn-start check found.
#[derive(Debug)]
pub(crate) struct Check {
    /// One `instruction_file` per change, in path order.
    pub(crate) files: Vec<InstructionFile>,
    /// One `io_failed` per unreadable tracked file.
    pub(crate) notices: Vec<Notice>,
    /// The new date, when a turn starts on a later date.
    pub(crate) date: Option<DateChanged>,
}

impl State {
    /// Nothing read yet: a `Loop::start` before its opening message, or a
    /// resume over a log holding none. The first turn writes the opening
    /// message and rebuilds the state from it, and never checks this one.
    pub(crate) fn empty(home: &Path) -> Self {
        let home = opening::canonical(home);
        Self {
            files: BTreeMap::new(),
            dirs: BTreeSet::from([home.clone()]),
            date: String::new(),
            home,
            had: BTreeMap::new(),
            queued: Vec::new(),
        }
    }

    /// The state the opening message just written describes: each file it
    /// sent with its size and time now, the home and chain directories
    /// checked, and the date given. The baseline the next turn's check
    /// compares against.
    pub(crate) fn initial(message: &OpeningMessage, workspace: &Path, home: &Path) -> Self {
        let workspace = opening::canonical(workspace);
        let (chain, _) = opening::repo_chain(&workspace);
        let mut state = Self::empty(home);
        state.dirs.extend(chain);
        for file in &message.instruction_files {
            state.files.insert(
                file.path.clone(),
                Tracked {
                    stat: stat_of(&file.path),
                    noticed: None,
                },
            );
        }
        state.date = message.environment.date.clone();
        state
    }

    /// The state the log's lines describe: the content fold gives what the
    /// model last had, the parents of every instruction file restore each
    /// directory that held one, and the last date wins. No size or time is
    /// remembered, so the first check reads each file and sends nothing
    /// when its content equals what the model had. A subdirectory touched
    /// before the resume that held no instruction file is not remembered:
    /// a call touching it again checks it.
    pub(crate) fn resumed(
        lines: &[Envelope],
        workspace: &Path,
        home: &Path,
    ) -> Result<Self, Error> {
        let workspace = opening::canonical(workspace);
        let (chain, _) = opening::repo_chain(&workspace);
        let mut state = Self::empty(home);
        state.dirs.extend(chain);
        for line in lines.iter().filter(|l| l.is_durable()) {
            let Some(event) = Event::from_envelope(line).map_err(Error::Unreadable)? else {
                continue;
            };
            // Every other line changes neither the date nor the checked
            // set: only these three restore the tracked state.
            if let Event::OpeningMessage(message) = &event {
                state.date = message.environment.date.clone();
                for file in &message.instruction_files {
                    state.files.entry(file.path.clone()).or_default();
                }
            }
            if let Event::InstructionFile(file) = &event {
                state.files.entry(file.path.clone()).or_default();
                if let Some(parent) = Path::new(&file.path).parent() {
                    state.dirs.insert(parent.to_path_buf());
                }
            }
            if let Event::DateChanged(changed) = &event {
                state.date = changed.date.clone();
            }
            apply(&mut state.had, &event);
        }
        Ok(state)
    }

    /// The turn-start check (`docs/system-prompt.md`, "When something
    /// changes" and "The date"): every tracked file, then every checked
    /// directory's candidate, in path order, with the date change last.
    pub(crate) fn check(&mut self, clock: &dyn Clock) -> Check {
        let mut out = Check {
            files: Vec::new(),
            notices: Vec::new(),
            date: None,
        };
        // Tracked files first, in path order (`BTreeMap` iteration).
        for path in self.files.keys().cloned().collect::<Vec<_>>() {
            self.check_file(&path, &mut out);
        }
        // Then the candidates of checked directories: a path not yet
        // tracked is `created` with the full text.
        for dir in self.dirs.iter().cloned().collect::<Vec<_>>() {
            self.check_dir(&dir, &mut out);
        }
        out.files.sort_by(|a, b| a.path.cmp(&b.path));
        let today = opening::date_of(clock.wall());
        // A plain string comparison of `YYYY-MM-DD`: only a later date
        // appends a line.
        if today > self.date {
            self.date = today.clone();
            out.date = Some(DateChanged { date: today });
        }
        out
    }

    /// One tracked file against what the model last had: equal size and
    /// time means unchanged; otherwise the content decides. A file read
    /// as absent stays tracked: existing again, it is `created`, never a
    /// diff against the version before deletion.
    fn check_file(&mut self, path: &str, out: &mut Check) {
        let now = stat_of(path);
        let known = self.files.get(path).and_then(|file| file.stat);
        // The size-and-time shortcut: unchanged without reading.
        if let (Some(now), Some(known)) = (now, known)
            && now == known
        {
            return;
        }
        match std::fs::read(path) {
            Ok(bytes) => {
                let content = String::from_utf8_lossy(&bytes).into_owned();
                self.track(path, now);
                let old = self.had.get(path).cloned();
                if old.as_deref() == Some(content.as_str()) {
                    // Touched but identical: sizes move on, nothing sent.
                    return;
                }
                out.files.push(match old {
                    Some(old) => {
                        let diff = unified_diff(&old, &content, path);
                        // The full text when the diff is longer in bytes
                        // than the new file.
                        let sent = if diff.len() > content.len() {
                            InstructionSent::Full
                        } else {
                            InstructionSent::Diff
                        };
                        InstructionFile {
                            path: path.to_owned(),
                            reason: InstructionReason::Changed,
                            content: Some(content.clone()),
                            sent,
                        }
                    }
                    None => InstructionFile {
                        path: path.to_owned(),
                        reason: InstructionReason::Created,
                        content: Some(content),
                        sent: InstructionSent::Full,
                    },
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.track(path, None);
                // Gone: one line saying its instructions no longer apply.
                // Still tracked as absent: existing again, it is `created`.
                if self.had.contains_key(path) {
                    out.files.push(InstructionFile {
                        path: path.to_owned(),
                        reason: InstructionReason::Deleted,
                        content: None,
                        sent: InstructionSent::Deleted,
                    });
                }
            }
            Err(e) => {
                // Left as it was, with an `io_failed` notice naming it:
                // once per change of size and time. `None` (no notice
                // yet) differs from `Some(None)` (noticed with unknown
                // sizes), so the first failure is always named.
                if self.files.get(path).and_then(|file| file.noticed) != Some(now) {
                    if let Some(file) = self.files.get_mut(path) {
                        file.noticed = Some(now);
                    }
                    out.notices.push(opening::io_failed(Path::new(path), &e));
                }
            }
        }
    }

    /// One checked directory's candidate: a path not yet tracked is
    /// `created` with the full text, and one that cannot be read is
    /// named once per change of size and time.
    fn check_dir(&mut self, dir: &Path, out: &mut Check) {
        let (file, notice) = self.adopt(dir, InstructionReason::Created);
        if let Some(file) = file {
            out.files.push(file);
        }
        if let Some(notice) = notice {
            out.notices.push(notice);
        }
    }

    /// Reads `dir`'s candidate into tracking: the global file is
    /// `<home>/AGENTS.md` only, elsewhere [`opening::candidate`]. A
    /// path already tracked, or with no candidate, gives nothing. A
    /// readable candidate gives its `reason` line with the full text;
    /// one that is gone gives nothing, and one that cannot be read is
    /// tracked with its failing sizes and gives an `io_failed` notice
    /// naming it.
    fn adopt(
        &mut self,
        dir: &Path,
        reason: InstructionReason,
    ) -> (Option<InstructionFile>, Option<Notice>) {
        let candidate = if dir == self.home.as_path() {
            // The global file is `<home>/AGENTS.md` only.
            home_candidate(dir)
        } else {
            opening::candidate(dir)
        };
        let Some(candidate) = candidate else {
            return (None, None);
        };
        let path = candidate.display().to_string();
        if self.files.contains_key(&path) {
            return (None, None);
        }
        match std::fs::read(&candidate) {
            Ok(bytes) => {
                let content = String::from_utf8_lossy(&bytes).into_owned();
                self.files.insert(
                    path.clone(),
                    Tracked {
                        stat: stat_of(&path),
                        noticed: None,
                    },
                );
                (
                    Some(InstructionFile {
                        path,
                        reason,
                        content: Some(content),
                        sent: InstructionSent::Full,
                    }),
                    None,
                )
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (None, None),
            Err(e) => {
                let now = stat_of(&path);
                self.files.insert(
                    path,
                    Tracked {
                        stat: now,
                        noticed: Some(now),
                    },
                );
                (None, Some(opening::io_failed(&candidate, &e)))
            }
        }
    }

    /// Records `stat` for a tracked `path`, keeping its notice mark.
    fn track(&mut self, path: &str, stat: Option<Stat>) {
        if let Some(file) = self.files.get_mut(path) {
            file.stat = stat;
        }
    }

    /// A completed call's declared paths (`DeclaredEffects.paths`,
    /// resolved against the workspace and lexically normalised): tracked
    /// instruction files are re-read, and new subdirectory files are queued
    /// for the next step start. Returns the session's own edits, written at
    /// once; the queued lines wait in [`State::take_queued`]. A call that
    /// declares no paths, and paths outside the workspace or that do not
    /// resolve, change nothing here.
    pub(crate) fn call_completed(
        &mut self,
        workspace: &Path,
        declared: &DeclaredEffects,
    ) -> Vec<InstructionFile> {
        let Some(paths) = declared.paths.as_ref() else {
            return Vec::new();
        };
        // Resolved once against the workspace and lexically normalised:
        // paths outside it, or that do not resolve, change nothing here.
        let resolved: Vec<PathBuf> = paths
            .iter()
            .filter_map(|declared_path| clean(&workspace.join(declared_path)))
            .filter(|resolved| resolved.strip_prefix(workspace).is_ok())
            .collect();
        let mut own = Vec::new();
        for resolved in &resolved {
            let key = resolved.display().to_string();
            if !self.files.contains_key(&key) {
                continue;
            }
            match std::fs::read(resolved) {
                Ok(bytes) => {
                    let content = String::from_utf8_lossy(&bytes).into_owned();
                    self.track(&key, stat_of(&key));
                    // Content different from what the model had: the model
                    // saw its own edit, so nothing is sent.
                    if self.had.get(&key).is_none_or(|old| old != &content) {
                        own.push(InstructionFile {
                            path: key,
                            reason: InstructionReason::OwnEdit,
                            content: Some(content),
                            sent: InstructionSent::None,
                        });
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    self.track(&key, None);
                    // The session's own call deleted a tracked file: an
                    // `own_edit` with no content, tracked as absent.
                    if self.had.contains_key(&key) {
                        own.push(InstructionFile {
                            path: key,
                            reason: InstructionReason::OwnEdit,
                            content: None,
                            sent: InstructionSent::None,
                        });
                    }
                }
                // Unreadable: left as it was.
                Err(_) => {}
            }
        }
        self.touch_subdirs(workspace, &resolved);
        own
    }

    /// Each directory a call's declared paths reach, once per context: the
    /// ancestors strictly below the workspace up to each path's parent, and
    /// the path itself when it is a directory. A new directory holding a
    /// candidate file queues one `subdirectory` line with the full text.
    fn touch_subdirs(&mut self, workspace: &Path, paths: &[PathBuf]) {
        let mut fresh = BTreeSet::new();
        for resolved in paths {
            let mut parent = resolved.parent();
            while let Some(dir) = parent {
                if dir == workspace || dir.strip_prefix(workspace).is_err() {
                    break;
                }
                if !self.dirs.contains(dir) {
                    fresh.insert(dir.to_path_buf());
                }
                parent = dir.parent();
            }
            // A shell search declaring `sub/` reaches `sub/AGENTS.md`.
            if resolved.is_dir() && !self.dirs.contains(resolved) {
                fresh.insert(resolved.clone());
            }
        }
        // `BTreeSet` order: the queued lines read in path order.
        for dir in fresh {
            self.dirs.insert(dir.clone());
            let (file, notice) = self.adopt(&dir, InstructionReason::Subdirectory);
            if let Some(file) = file {
                self.queued.push(Event::InstructionFile(file));
            }
            if let Some(notice) = notice {
                self.queued.push(Event::Notice(notice));
            }
        }
    }

    /// The subdirectory lines queued since the last step start, re-read
    /// before they are emitted: a later call in the same batch may have
    /// edited or deleted a queued file, and its own edit already moved
    /// what the model had. A file matching what the model had, or gone,
    /// is dropped; a changed one goes out with its content now; one that
    /// cannot be read is named once per change of size and time.
    pub(crate) fn take_queued(&mut self) -> Vec<Event> {
        let mut out = Vec::new();
        for event in std::mem::take(&mut self.queued) {
            let Event::InstructionFile(file) = event else {
                out.push(event);
                continue;
            };
            match std::fs::read(&file.path) {
                Ok(bytes) => {
                    let content = String::from_utf8_lossy(&bytes).into_owned();
                    self.track(&file.path, stat_of(&file.path));
                    if self.had.get(&file.path) == Some(&content) {
                        // An own edit already moved what the model had.
                        continue;
                    }
                    out.push(Event::InstructionFile(InstructionFile {
                        content: Some(content),
                        ..file
                    }));
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    self.track(&file.path, None);
                    // Gone: an own deletion already recorded it, and a
                    // file never sent needs no farewell.
                }
                Err(e) => {
                    let now = stat_of(&file.path);
                    if self.files.get(&file.path).and_then(|file| file.noticed) != Some(now) {
                        if let Some(tracked) = self.files.get_mut(&file.path) {
                            tracked.noticed = Some(now);
                        }
                        out.push(Event::Notice(opening::io_failed(Path::new(&file.path), &e)));
                    }
                }
            }
        }
        out
    }
}

/// The unified diff from what the model had to what is in force now, with
/// the path as both header lines.
pub(crate) fn unified_diff(old: &str, new: &str, path: &str) -> String {
    similar::TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(path, path)
        .to_string()
}

/// Folds `event` into what the model last had: the opening message sets
/// every file it sent, an instruction file sets or clears its path, and
/// every other line changes nothing. Live rendering and `rebuild` go
/// through this one function, so a diff renders identically on resume.
pub(crate) fn apply(had: &mut BTreeMap<String, String>, event: &Event) {
    // Every other line changes nothing: only these two move what the
    // model last had.
    if let Event::OpeningMessage(message) = event {
        had.clear();
        for file in &message.instruction_files {
            had.insert(file.path.clone(), file.content.clone());
        }
    } else if let Event::InstructionFile(file) = event {
        match &file.content {
            Some(content) => {
                had.insert(file.path.clone(), content.clone());
            }
            None => {
                had.remove(&file.path);
            }
        }
    }
}

/// The global file's candidate: `<home>/AGENTS.md` only, when present or
/// unreadable.
fn home_candidate(home: &Path) -> Option<PathBuf> {
    let agents = home.join("AGENTS.md");
    match std::fs::metadata(&agents) {
        Ok(_) => Some(agents),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => Some(agents),
    }
}

/// `path` with `.` dropped and `..` applied lexically, without touching
/// the file system. `None` when `..` climbs past the root: the path does
/// not resolve.
fn clean(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(Component::RootDir.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            Component::Normal(name) => out.push(name),
        }
    }
    Some(out)
}

#[cfg(test)]
#[path = "changes_tests.rs"]
mod tests;
