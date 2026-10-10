//! The session's maintained skill set (`docs/system-prompt.md`, "Skills"
//! and "Added and removed skills"): the one discovery every `/name`
//! expansion and every `skill` tool call answers from. The set reads each
//! place's directory once, at the first lookup, and the opening message's
//! `write_opening` refreshes it with what that message sent, so a skill
//! written after the first read stays unknown until the refresh hands it
//! over. At each turn start the check compares the current listing
//! against what the model was last given, and appends one line per skill
//! added or removed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use contract::Envelope;
use contract::events::{Event, Notice, SkillListed, SkillsChanged};

use crate::Error;
use crate::opening;
use crate::prompt::PromptInputs;
use crate::skill_reader::SkillReader;
use crate::skills;

/// The session's maintained skill set: the winners of one discovery, and
/// the names `skills.disabled` switches off. `None` means nothing read
/// yet: the first lookup discovers once, stores the result, then answers.
#[derive(Clone)]
pub struct SkillSet(std::sync::Arc<Mutex<Inner>>);

/// What one turn-start check found (`docs/system-prompt.md`, "Added and
/// removed skills"): the notices the turn appends, and the
/// `skills_changed` line, when a skill was added or removed.
pub(crate) struct SkillCheck {
    /// The skill notices the turn appends: the discovery's notices in
    /// discovery order, then the `skills.disabled` failure. Each is
    /// raised once while its problem stays.
    pub(crate) notices: Vec<Notice>,
    /// The skills added or removed since the listing was last given, when
    /// any: each added entry, then each removed name, in name order.
    pub(crate) changed: Option<SkillsChanged>,
}

/// What the set holds: the inputs the opening message reads, the
/// repository's top level above the workspace, and the one discovery, if
/// any.
#[derive(Clone)]
struct Inner {
    /// What the opening message reads, once.
    inputs: PromptInputs,
    /// The repository's top level, or the workspace outside git.
    top: PathBuf,
    /// The one discovery, once read.
    known: Option<Known>,
    /// What the model was last given, per name: the opening listing, then
    /// each `added` (`docs/system-prompt.md`, "Recording"). A lazy read
    /// never touches it, so the turn after a resume still announces what
    /// changed while the session was away.
    listed: BTreeMap<String, SkillListed>,
    /// The previous check's notice messages: a notice is raised again
    /// only when its message is new
    /// (`docs/system-prompt.md`, "Added and removed skills").
    noticed: BTreeSet<String>,
    /// A reader call already in flight: a re-entrant call skips the
    /// reader and keeps the last-known list, so a reader that calls back
    /// into this set cannot deadlock.
    reading: bool,
    /// The reader's last failure, for the next check's notice.
    pending_notice: Option<Notice>,
}

/// One discovery's winners, in discovery order.
#[derive(Clone)]
struct Known {
    /// The winners, in discovery order.
    found: Vec<skills::Found>,
    /// What the last check read, per `SKILL.md`.
    cache: skills::Cache,
}

impl SkillSet {
    /// The set for `inputs` above `workspace`: the repository's top
    /// level, as the opening message reads it. Reads nothing.
    pub fn new(inputs: PromptInputs, workspace: &Path) -> Self {
        let canonical = opening::canonical(workspace);
        let (chain, _) = opening::repo_chain(&canonical);
        let top = chain.first().cloned().unwrap_or(canonical);
        Self(std::sync::Arc::new(Mutex::new(Inner {
            inputs,
            top,
            known: None,
            listed: BTreeMap::new(),
            noticed: BTreeSet::new(),
            reading: false,
            pending_notice: None,
        })))
    }

    /// A reader answering from this set, not from a fresh discovery.
    pub fn reader(&self) -> SkillReader {
        SkillReader::new(self.clone())
    }

    /// The given inputs with the set's current disabled list applied:
    /// what a fresh opening message collects, so a handoff sizes its
    /// notices by the model's current window, not the set's startup
    /// snapshot (`docs/system-prompt.md`, "Size").
    pub(crate) fn with_disabled(&self, inputs: &PromptInputs) -> PromptInputs {
        let mut out = inputs.clone();
        out.skills_disabled = lock(&self.0).inputs.skills_disabled.clone();
        out
    }

    /// The inputs with the current disabled list: after one
    /// `skills.disabled` re-read, when one is set
    /// (`docs/configuration.md`, "When Fiber reads configuration").
    pub(crate) fn inputs_now(&self) -> PromptInputs {
        self.refresh_disabled();
        lock(&self.0).inputs.clone()
    }

    /// Refreshes the set with what the opening message sent: the
    /// discovery's winners, the disabled list in force with them, the
    /// listing as the new baseline, and the opening's notices, which seed
    /// the check's notice dedupe. The opening's own discovery neither
    /// uses nor seeds the cache, so the first turn-start check reads each
    /// `SKILL.md` once (`docs/system-prompt.md`, "Added and removed
    /// skills").
    pub(crate) fn opened(
        &self,
        found: Vec<skills::Found>,
        disabled: Vec<String>,
        listed: &[SkillListed],
        notices: &[Notice],
    ) {
        let mut inner = lock(&self.0);
        inner.inputs.skills_disabled = disabled;
        inner.known = Some(Known {
            found,
            cache: skills::Cache::default(),
        });
        inner.listed = listed
            .iter()
            .map(|entry| (entry.name.clone(), entry.clone()))
            .collect();
        inner.noticed = notices
            .iter()
            .map(|notice| notice.message.clone())
            .collect();
    }

    /// The turn-start check (`docs/system-prompt.md`, "Added and removed
    /// skills"): the current listing against what the model was last
    /// given. One `skills_changed`, written only when a skill was added
    /// or removed.
    pub(crate) fn check(&self) -> SkillCheck {
        let inputs = self.inputs_now();
        let mut inner = lock(&self.0);
        let pending = inner.pending_notice.take();
        let disabled = inner.inputs.skills_disabled.clone();
        let top = inner.top.clone();
        let mut cache = inner
            .known
            .as_mut()
            .map(|known| std::mem::take(&mut known.cache))
            .unwrap_or_default();
        // With `known: None` (a resume) there are no old winners to
        // retain.
        let old: Vec<skills::Found> = inner
            .known
            .as_ref()
            .map(|known| known.found.clone())
            .unwrap_or_default();
        let discovered = skills::discover_cached(&inputs, &top, &mut cache);
        // The listing by the listing's rules.
        let mut current: BTreeMap<String, SkillListed> = BTreeMap::new();
        let mut winners: BTreeMap<String, skills::Found> = BTreeMap::new();
        for found in discovered.skills {
            if found.model_invocable && !disabled.iter().any(|off| off == &found.listed.name) {
                current.insert(found.listed.name.clone(), found.listed.clone());
            }
            winners.insert(found.listed.name.clone(), found);
        }
        // A listed skill the check could not read keeps its last-known
        // entry: no removed line, and the `skill` tool still resolves it.
        // Not-found means removed. A switch still counts while unreadable:
        // a name `skills.disabled` names is removed, as the tool refuses
        // it (`docs/system-prompt.md`, "Added and removed skills").
        for (name, entry) in &inner.listed {
            if current.contains_key(name) {
                continue;
            }
            let unreadable = discovered
                .unread
                .iter()
                .any(|unread| Path::new(&entry.path).starts_with(unread));
            if !unreadable || disabled.iter().any(|off| off == name) {
                continue;
            }
            if let Some(found) = old.iter().find(|found| found.listed.name == *name) {
                current.insert(name.clone(), found.listed.clone());
                winners.insert(name.clone(), found.clone());
            }
        }
        // The set differences, both in name order (`BTreeMap`
        // iteration).
        let mut added = Vec::new();
        for (name, entry) in &current {
            if !inner.listed.contains_key(name) {
                added.push(entry.clone());
            }
        }
        let mut removed = Vec::new();
        for name in inner.listed.keys() {
            if !current.contains_key(name) {
                removed.push(name.clone());
            }
        }
        // Written only when `added` or `removed` is non-empty.
        let changed = if added.is_empty() && removed.is_empty() {
            None
        } else {
            Some(SkillsChanged { added, removed })
        };
        inner.listed = current;
        inner.known = Some(Known {
            found: winners.into_values().collect(),
            cache,
        });
        // Each candidate notice is raised when its message was not among
        // the previous check's (or the opening's) notices; the set then
        // becomes this check's candidates, so a problem staying raises
        // nothing until it is fixed and broken again.
        let previous = std::mem::take(&mut inner.noticed);
        let mut notices = Vec::new();
        let candidates: Vec<&Notice> = discovered.notices.iter().chain(pending.as_ref()).collect();
        for notice in &candidates {
            if !previous.contains(&notice.message) {
                notices.push((*notice).clone());
            }
        }
        inner.noticed = candidates
            .iter()
            .map(|notice| notice.message.clone())
            .collect();
        SkillCheck { notices, changed }
    }

    /// Folds the durable `opening_message` and `skills_changed` lines into
    /// the baseline: from the last opening message, then each later change
    /// (`docs/system-prompt.md`, "Recording"). It never reads the disk
    /// and leaves the winners as they are: none on a fresh resume, so the
    /// first lookup reads them lazily.
    pub(crate) fn resumed(&self, context: &[Envelope]) -> Result<(), Error> {
        // From the last opening message: each opening resets the fold,
        // so earlier contexts fall away on their own.
        let mut listed = BTreeMap::new();
        for line in context {
            let Some(event) = Event::from_envelope(line).map_err(Error::Unreadable)? else {
                continue;
            };
            // Every other line changes nothing: only these two move the
            // baseline.
            if let Event::OpeningMessage(message) = &event {
                listed = message
                    .skills
                    .iter()
                    .map(|entry| (entry.name.clone(), entry.clone()))
                    .collect();
            }
            if let Event::SkillsChanged(changed) = event {
                for entry in changed.added {
                    listed.insert(entry.name.clone(), entry);
                }
                for name in changed.removed {
                    listed.remove(&name);
                }
            }
        }
        lock(&self.0).listed = listed;
        Ok(())
    }

    /// Renders a `skills_changed` from the event alone, so a resume, a
    /// fork and a rewind render what the live turn sent
    /// (`docs/system-prompt.md`, "Recording"): each added line, then each
    /// removed line, both in the name order the check wrote.
    pub(crate) fn changed_text(changed: &SkillsChanged) -> String {
        let added = crate::prompt::body(crate::conversation::MESSAGES_MD, "skill-added");
        let removed = crate::prompt::body(crate::conversation::MESSAGES_MD, "skill-removed");
        let mut lines = Vec::with_capacity(changed.added.len() + changed.removed.len());
        for entry in &changed.added {
            lines.push(crate::prompt::fill(
                &added,
                &[
                    ("name", one_line(&entry.name).as_str()),
                    ("description", one_line(&entry.description).as_str()),
                ],
            ));
        }
        for name in &changed.removed {
            lines.push(crate::prompt::fill(
                &removed,
                &[("name", one_line(name).as_str())],
            ));
        }
        lines.join("\n")
    }

    /// The winner's `SKILL.md` for `name`, when it is not switched off,
    /// whatever its invocability: templates and extension `prompts/`
    /// skills count, as `/name` expands them
    /// (`docs/system-prompt.md`, "Skills").
    pub(crate) fn command(&self, name: &str) -> Option<PathBuf> {
        self.ensure_read();
        let mut inner = lock(&self.0);
        ensure(&mut inner);
        if inner.inputs.skills_disabled.iter().any(|off| off == name) {
            return None;
        }
        inner
            .known
            .as_ref()?
            .found
            .iter()
            .find(|found| found.listed.name == name)
            .map(|found| found.file.clone())
    }

    /// The winner's `SKILL.md` for `name`, when the model may load it
    /// and it is not switched off, as the `skill` tool loads it
    /// (`docs/tools.md`, "Skills").
    pub(crate) fn listed_file(&self, name: &str) -> Option<PathBuf> {
        self.ensure_read();
        let mut inner = lock(&self.0);
        ensure(&mut inner);
        if inner.inputs.skills_disabled.iter().any(|off| off == name) {
            return None;
        }
        inner
            .known
            .as_ref()?
            .found
            .iter()
            .find(|found| found.listed.name == name && found.model_invocable)
            .map(|found| found.file.clone())
    }

    /// Whether `name` is switched off.
    pub(crate) fn is_disabled(&self, name: &str) -> bool {
        self.ensure_read();
        let mut inner = lock(&self.0);
        ensure(&mut inner);
        inner.inputs.skills_disabled.iter().any(|off| off == name)
    }

    /// Switches off `names`, as `skills.disabled` does
    /// (`docs/system-prompt.md`, "Skills").
    pub(crate) fn set_disabled(&self, names: Vec<String>) {
        lock(&self.0).inputs.skills_disabled = names;
    }

    /// Moves `from`'s state into `self`: after it, `self` answers what
    /// `from` held. No-op when both are one set.
    pub(crate) fn adopt(&self, from: &SkillSet) {
        if std::sync::Arc::ptr_eq(&self.0, &from.0) {
            return;
        }
        let cloned = lock(&from.0).clone();
        *lock(&self.0) = cloned;
    }

    /// Discovers once when nothing is read yet, after one
    /// `skills.disabled` re-read: the resumed process's one read at
    /// start. Any later lookup answers from the maintained state, so a
    /// switch made mid-turn changes nothing before the turn-start check
    /// announces it (`docs/configuration.md`, "When Fiber reads
    /// configuration").
    fn ensure_read(&self) {
        if lock(&self.0).known.is_some() {
            return;
        }
        self.refresh_disabled();
        ensure(&mut lock(&self.0));
    }

    /// One `skills.disabled` re-read with no lock held, applied when it
    /// succeeds: without a reader the static list stays in force, and a
    /// failure keeps the last-known list for the next check's notice
    /// (`docs/configuration.md`, "When Fiber reads configuration").
    fn refresh_disabled(&self) {
        match self.read_disabled() {
            Some(Ok(names)) => {
                let mut inner = lock(&self.0);
                inner.inputs.skills_disabled = names;
                inner.pending_notice = None;
            }
            Some(Err(notice)) => {
                lock(&self.0).pending_notice = Some(notice);
            }
            None => {}
        }
    }

    /// One call of the injected `skills.disabled` reader with no lock
    /// held: `None` when no reader is set, or a call is already in flight
    /// on this set (the reader may call back into it).
    fn read_disabled(&self) -> Option<Result<Vec<String>, Notice>> {
        let reader = {
            let mut inner = lock(&self.0);
            if inner.reading {
                return None;
            }
            let reader = inner.inputs.skills_disabled_now.clone()?;
            inner.reading = true;
            reader
        };
        let read = reader();
        lock(&self.0).reading = false;
        Some(read)
    }
}

impl crate::Loop {
    /// Answers `/name` and the `skill` tool from `shared`: `shared`
    /// takes over this loop's one discovery, then this loop holds it.
    pub fn skill_set(mut self, shared: SkillSet) -> Self {
        shared.adopt(&self.skills);
        self.skills = shared;
        self
    }
}

/// Discovers once when nothing is read yet, and stores the result. The
/// caller holds the lock across it.
fn ensure(inner: &mut Inner) {
    if inner.known.is_some() {
        return;
    }
    let mut cache = skills::Cache::default();
    let found = skills::discover_cached(&inner.inputs, &inner.top, &mut cache);
    inner.known = Some(Known {
        found: found.skills,
        cache,
    });
}

/// One line of a name or description: each run of carriage returns and
/// line feeds as one space, so an added or removed line stays one line
/// (`docs/system-prompt.md`, "Added and removed skills").
fn one_line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut gap = false;
    for ch in text.chars() {
        if ch == '\r' || ch == '\n' {
            gap = true;
        } else if gap {
            out.push(' ');
            gap = false;
            out.push(ch);
        } else {
            out.push(ch);
        }
    }
    if gap {
        out.push(' ');
    }
    out
}

fn lock<T>(inner: &Mutex<T>) -> MutexGuard<'_, T> {
    inner.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "skill_set_tests.rs"]
mod tests;
