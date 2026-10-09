//! The skills the model may load (`docs/tools.md`, "Skills"), as discovery
//! finds them at the call. `loop` implements it; `main` injects it.

use std::path::{Path, PathBuf};

/// The skills the model may load (`docs/tools.md`, "Skills"), as discovery
/// finds them at the call. `loop` implements it; `main` injects it.
pub trait Skills: Send + Sync {
    /// The `SKILL.md` discovery opened for the skill the listing holds under
    /// `name` now, lossless. `None` when the listing holds no such name.
    fn file(&self, name: &str) -> Option<PathBuf>;
    /// Reads `file` now. Its header must parse, carry `name`, and allow
    /// model invocation. Returns the body.
    fn body(&self, name: &str, file: &Path) -> Result<String, SkillRead>;
}

/// Why `body` failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillRead {
    /// The read failed; the message names the path.
    Io(String),
    /// The header does not parse, names another skill, or disables model
    /// invocation.
    Invalid,
}
