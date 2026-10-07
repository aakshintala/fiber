//! Instruction files changing mid-session (`docs/system-prompt.md`, "When
//! something changes", and "The date"): the turn-start check, the own-edit
//! tracking after a call whose declared paths include a known file, the
//! subdirectory files those paths reach, and the resume fold that rebuilds
//! the same state from the log.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use contract::clock::Clock;
use contract::events::{
    DateChanged, Event, InstructionFile, InstructionReason, InstructionSent, Notice, OpeningMessage,
};
use contract::shapes::DeclaredEffects;
use contract::{ActionId, Envelope};

use crate::Error;
use crate::opening;
use crate::prompt::PromptInputs;

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

/// One extension section's files for the turn-start check and the prune
/// check: the extension's name, its files' paths as the manifest names
/// them, and its byte budget, when the manifest gives one.
#[derive(Debug)]
struct Section {
    extension: String,
    paths: Vec<String>,
    budget: Option<u64>,
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
    /// The paths a resume restored from calls' declared paths that are not
    /// known directories: the log does not say whether each was a file or
    /// a directory when its call ran. Each joins `dirs` at the first check
    /// that finds it a directory; until then it is never read, so a file
    /// here names no failure. Canonical.
    maybe_dirs: BTreeSet<PathBuf>,
    /// The date last given, `YYYY-MM-DD`.
    date: String,
    /// Canonical Fiber home: its directory holds only `AGENTS.md`.
    home: PathBuf,
    /// The content the model last had per path, updated as instruction
    /// lines render, live and in `rebuild`.
    pub(crate) had: BTreeMap<String, String>,
    /// Every extension section's files and budget, in send order.
    sections: Vec<Section>,
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
            maybe_dirs: BTreeSet::new(),
            date: String::new(),
            home,
            had: BTreeMap::new(),
            sections: Vec::new(),
            queued: Vec::new(),
        }
    }

    /// The state the opening message just written describes: each file it
    /// sent with its size and time now, the home and chain directories
    /// checked, and the date given. The baseline the next turn's check
    /// compares against.
    pub(crate) fn initial(
        message: &OpeningMessage,
        workspace: &Path,
        prompt: &PromptInputs,
    ) -> Self {
        let workspace = opening::canonical(workspace);
        let (chain, _) = opening::repo_chain(&workspace);
        let mut state = Self::empty(&prompt.home);
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
        state.track_sections(&prompt.extension_sections, true);
        state.date = message.environment.date.clone();
        state
    }

    /// The state the log's lines describe: the content fold gives what the
    /// model last had, the parents of every instruction file restore each
    /// directory that held one, the declared paths of every call the
    /// current context started and completed restore each subdirectory
    /// they reached (each path itself waits in `maybe_dirs` until it is a
    /// directory), and the last date wins. No size or time is
    /// remembered, so the first check reads each file and sends nothing
    /// when its content equals what the model had; a restored subdirectory
    /// queues nothing, and a file created in it later is `created` at the
    /// next check.
    pub(crate) fn resumed(
        lines: &[Envelope],
        workspace: &Path,
        prompt: &PromptInputs,
    ) -> Result<Self, Error> {
        let workspace = opening::canonical(workspace);
        let (chain, _) = opening::repo_chain(&workspace);
        let mut state = Self::empty(&prompt.home);
        state.dirs.extend(chain);
        // Today's manifest only: a path the log's opening message sent
        // that the manifest no longer names is not tracked and sends
        // nothing.
        state.track_sections(&prompt.extension_sections, false);
        // Each started call's declared paths, until its completion: only a
        // call that ran and completed touched its directories, as live.
        let mut running: BTreeMap<ActionId, Vec<String>> = BTreeMap::new();
        // The subdirectories the current context's calls reached.
        let mut touched = BTreeSet::new();
        // The paths those calls declared, of unknown kind.
        let mut declared = BTreeSet::new();
        for line in lines.iter().filter(|l| l.is_durable()) {
            let Some(event) = Event::from_envelope(line).map_err(Error::Unreadable)? else {
                continue;
            };
            // Every other line changes neither the date nor the checked
            // set: only these five restore the tracked state.
            if let Event::OpeningMessage(message) = &event {
                // A new context: the calls before it touched nothing in it.
                touched.clear();
                declared.clear();
                state.date = message.environment.date.clone();
                for file in &message.instruction_files {
                    state.files.entry(file.path.clone()).or_default();
                }
            }
            if let Event::InstructionFile(file) = &event {
                if file.extension.is_some() {
                    // A historical section file: only today's manifest
                    // tracks it, and its directory never becomes checked.
                    if state.extension_of(&file.path).is_some() {
                        state.files.entry(file.path.clone()).or_default();
                    }
                } else {
                    state.files.entry(file.path.clone()).or_default();
                    if let Some(parent) = Path::new(&file.path).parent() {
                        state.dirs.insert(parent.to_path_buf());
                    }
                }
            }
            if let (Event::ToolCallStarted(started), Some(action)) = (&event, &line.action_id)
                && let Some(paths) = &started.declared.paths
            {
                running.insert(action.clone(), paths.clone());
            }
            if let (Event::ToolCallCompleted(_), Some(action)) = (&event, &line.action_id)
                && let Some(paths) = running.remove(action)
            {
                let resolved = resolve(&workspace, &paths);
                touched.extend(reached(&workspace, &resolved));
                // Each path itself, whatever it is today: a directory the
                // call declared may be gone now and made again later.
                declared.extend(
                    resolved
                        .into_iter()
                        .filter(|path| path != &workspace && path.strip_prefix(&workspace).is_ok()),
                );
            }
            if let Event::DateChanged(changed) = &event {
                state.date = changed.date.clone();
            }
            apply(&mut state.had, &event);
        }
        state.dirs.extend(touched);
        state.maybe_dirs = declared.difference(&state.dirs).cloned().collect();
        Ok(state)
    }

    /// Every section path in `sections` is tracked, present or absent: a
    /// path absent at the build stays tracked with `stat: None`, so its
    /// later appearance is `created` with the full text. With `sized` the
    /// size and time are read now (live); on resume nothing is
    /// remembered. Applied after the instruction files: a path in both
    /// roles is tracked once, and reads as the section's.
    fn track_sections(&mut self, sections: &[(String, Vec<PathBuf>, Option<u64>)], sized: bool) {
        for (name, paths, budget) in sections {
            let keys: Vec<String> = paths
                .iter()
                .map(|path| path.display().to_string())
                .collect();
            for key in &keys {
                self.files.entry(key.clone()).or_insert_with(|| Tracked {
                    stat: if sized { stat_of(key) } else { None },
                    noticed: None,
                });
            }
            self.sections.push(Section {
                extension: name.clone(),
                paths: keys,
                budget: *budget,
            });
        }
    }

    /// The section `path` belongs to, when it is a section file.
    fn extension_of(&self, path: &str) -> Option<&str> {
        self.sections
            .iter()
            .find(|section| section.paths.iter().any(|key| key == path))
            .map(|section| section.extension.as_str())
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
        // A restored path that is a directory now is checked from here on.
        let now_dirs: Vec<PathBuf> = self
            .maybe_dirs
            .iter()
            .filter(|path| path.is_dir())
            .cloned()
            .collect();
        for dir in now_dirs {
            self.maybe_dirs.remove(&dir);
            self.dirs.insert(dir);
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
                let extension = self.extension_of(path).map(str::to_owned);
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
                            extension: extension.clone(),
                            content: Some(content.clone()),
                            sent,
                        }
                    }
                    None => InstructionFile {
                        path: path.to_owned(),
                        reason: InstructionReason::Created,
                        extension,
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
                        extension: self.extension_of(path).map(str::to_owned),
                        content: None,
                        sent: InstructionSent::Deleted,
                    });
                }
            }
            Err(e) => {
                // Unreadable: an unknown baseline with the notice named
                // separately, so the next check reads again even when the
                // size and time match. `None` (no notice yet) differs from
                // `Some(None)` (noticed with unknown sizes), so the first
                // failure is always named.
                let now = stat_of(path);
                let noticed = self.files.get(path).and_then(|file| file.noticed);
                let extension = self.extension_of(path).map(str::to_owned);
                self.track(path, None);
                if noticed != Some(now) {
                    if let Some(file) = self.files.get_mut(path) {
                        file.noticed = Some(now);
                    }
                    // A section file's notice names its section, as at the
                    // build.
                    let failed = Path::new(path);
                    out.notices.push(match extension {
                        Some(name) => opening::section_io_failed(failed, &e, &name),
                        None => opening::io_failed(failed, &e),
                    });
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
    /// tracked with an unknown baseline and gives an `io_failed` notice
    /// naming it, so a later read sends it as `created` even when its
    /// size and time match the failure's.
    fn adopt(
        &mut self,
        dir: &Path,
        reason: InstructionReason,
    ) -> (Option<InstructionFile>, Option<Notice>) {
        let candidate = if dir == self.home.as_path() {
            // The global file is `<home>/AGENTS.md` only: absent reads
            // as gone, unreadable reads as a notice, through the same
            // arms below.
            Some(dir.join("AGENTS.md"))
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
                        extension: None,
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
                        stat: None,
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
    /// resolved against the workspace and lexically normalised): every
    /// tracked path is re-read, wherever it is, and new subdirectory files
    /// under the workspace are queued for the next step start. Returns the
    /// session's own edits, written at once; the queued lines wait in
    /// [`State::take_queued`]. A call that declares no paths, and paths that
    /// do not resolve, change nothing here.
    pub(crate) fn call_completed(
        &mut self,
        workspace: &Path,
        declared: &DeclaredEffects,
    ) -> Vec<InstructionFile> {
        let Some(paths) = declared.paths.as_ref() else {
            return Vec::new();
        };
        // Resolved once against the workspace and lexically normalised:
        // every tracked path is re-read, wherever it is. Only the
        // subdirectory walk below stays inside the workspace.
        let resolved = resolve(workspace, paths);
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
                        let extension = self.extension_of(&key).map(str::to_owned);
                        own.push(InstructionFile {
                            path: key,
                            reason: InstructionReason::OwnEdit,
                            extension,
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
                        let extension = self.extension_of(&key).map(str::to_owned);
                        own.push(InstructionFile {
                            path: key,
                            reason: InstructionReason::OwnEdit,
                            extension,
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

    /// The prune lines a completed `write` or `edit` call's result carries
    /// (`docs/system-prompt.md`, "Extension sections"): one per
    /// over-budget section the call's declared paths touch, in
    /// extension-name order. Anything else gives nothing. Sizes only are
    /// read, never content.
    pub(crate) fn prune_lines(
        &self,
        workspace: &Path,
        tool: &str,
        declared: &DeclaredEffects,
    ) -> Vec<String> {
        if tool != "write" && tool != "edit" {
            return Vec::new();
        }
        let Some(paths) = declared.paths.as_ref() else {
            return Vec::new();
        };
        // The same lexical match as the own-edit tracking above.
        let touched: Vec<String> = paths
            .iter()
            .filter_map(|path| clean(&workspace.join(path)))
            .map(|path| path.display().to_string())
            .collect();
        let mut sections: Vec<&Section> = self.sections.iter().collect();
        sections.sort_by(|a, b| a.extension.cmp(&b.extension));
        let mut lines = Vec::new();
        for section in sections {
            let Some(budget) = section.budget else {
                continue;
            };
            if !section.paths.iter().any(|key| touched.contains(key)) {
                continue;
            }
            // Sizes on disk now, absent files skipped: strictly greater
            // means over.
            let size: u64 = section
                .paths
                .iter()
                .filter_map(|key| std::fs::metadata(key).ok())
                .map(|meta| meta.len())
                .sum();
            if size > budget {
                lines.push(opening::budget_line(size, budget));
            }
        }
        lines
    }

    /// Each directory [`reached`] by a call's resolved paths, once per
    /// context: a new directory holding a candidate file queues one
    /// `subdirectory` line with the full text.
    fn touch_subdirs(&mut self, workspace: &Path, resolved: &[PathBuf]) {
        let fresh: BTreeSet<PathBuf> = reached(workspace, resolved)
            .into_iter()
            .filter(|dir| !self.dirs.contains(dir))
            .collect();
        // `BTreeSet` order: the queued lines read in path order.
        for dir in fresh {
            self.maybe_dirs.remove(&dir);
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
                    let noticed = self.files.get(&file.path).and_then(|file| file.noticed);
                    self.track(&file.path, None);
                    if noticed != Some(now) {
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
        // Each section file the message sent: a diff renders the same on
        // resume.
        for section in &message.extension_sections {
            for file in &section.files {
                had.insert(file.path.clone(), file.content.clone());
            }
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

/// A call's declared `paths` resolved against the workspace and lexically
/// normalised; a path that does not resolve is dropped.
fn resolve(workspace: &Path, paths: &[String]) -> Vec<PathBuf> {
    paths
        .iter()
        .filter_map(|declared_path| clean(&workspace.join(declared_path)))
        .collect()
}

/// The subdirectories `resolved` paths reach: for each path under the
/// workspace, the ancestors strictly below the workspace up to its parent,
/// and the path itself when it is a directory. Paths outside the workspace
/// reach none.
fn reached(workspace: &Path, resolved: &[PathBuf]) -> BTreeSet<PathBuf> {
    let mut dirs = BTreeSet::new();
    for resolved in resolved {
        if resolved.strip_prefix(workspace).is_err() {
            continue;
        }
        let mut parent = resolved.parent();
        while let Some(dir) = parent {
            if dir == workspace || dir.strip_prefix(workspace).is_err() {
                break;
            }
            dirs.insert(dir.to_path_buf());
            parent = dir.parent();
        }
        // A shell search declaring `sub/` reaches `sub/AGENTS.md`.
        if resolved.is_dir() {
            dirs.insert(resolved.clone());
        }
    }
    dirs
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
