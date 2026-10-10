//! The session's maintained skill set (`docs/system-prompt.md`, "Skills"
//! and "Added and removed skills"): the one discovery every `/name`
//! expansion and every `skill` tool call answers from. The set reads each
//! place's directory once, at the first lookup, and the opening message's
//! `write_opening` refreshes it with what that message sent, so a skill
//! written after the first read stays unknown until the refresh hands it
//! over.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::opening;
use crate::prompt::PromptInputs;
use crate::skill_reader::SkillReader;
use crate::skills;

/// The session's maintained skill set: the winners of one discovery, and
/// the names `skills.disabled` switches off. `None` means nothing read
/// yet: the first lookup discovers once, stores the result, then answers.
#[derive(Clone)]
pub struct SkillSet(std::sync::Arc<Mutex<Inner>>);

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
}

/// One discovery's winners, in discovery order.
#[derive(Clone)]
struct Known {
    /// The winners, in discovery order.
    found: Vec<skills::Found>,
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

    /// Refreshes the set with what the opening message sent: the
    /// discovery's winners and the disabled list in force with them.
    pub(crate) fn opened(&self, found: Vec<skills::Found>, disabled: Vec<String>) {
        let mut inner = lock(&self.0);
        inner.inputs.skills_disabled = disabled;
        inner.known = Some(Known { found });
    }

    /// The winner's `SKILL.md` for `name`, when it is not switched off,
    /// whatever its invocability: templates and extension `prompts/`
    /// skills count, as `/name` expands them
    /// (`docs/system-prompt.md`, "Skills").
    pub(crate) fn command(&self, name: &str) -> Option<PathBuf> {
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
    let found = skills::discover(&inner.inputs, &inner.top);
    inner.known = Some(Known {
        found: found.skills,
    });
}

fn lock<T>(inner: &Mutex<T>) -> MutexGuard<'_, T> {
    inner.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "skill_set_tests.rs"]
mod tests;
