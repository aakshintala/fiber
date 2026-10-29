//! The skills the model may load (`docs/tools.md`, "Skills"), as the
//! session's maintained set holds them. `loop` implements it; `main`
//! injects it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The skills the model may load (`docs/tools.md`, "Skills"), as the
/// session's maintained set holds them. `loop` implements it; `main`
/// injects it.
pub trait Skills: Send + Sync {
    /// The `SKILL.md` discovery opened for the skill the listing holds under
    /// `name` now, lossless. `None` when the listing holds no such name.
    fn file(&self, name: &str) -> Option<PathBuf>;
    /// Reads `file` now. Its header must parse, carry `name`, and allow
    /// model invocation. Returns the body.
    fn body(&self, name: &str, file: &Path) -> Result<String, SkillRead>;
}

/// Tells the import which source skill directories discovery would already
/// load from another personal source (`docs/configuration.md`,
/// "Importing"): the check maps each given source directory to the
/// `SKILL.md` path discovery logged, and leaves the rest absent. `loop`
/// implements it over discovery's own code; `main` injects it; `config`
/// consumes it for the import check, and the import never parses a header
/// itself.
pub trait PersonalSourceCheck: Send + Sync {
    /// Maps each directory in `dirs` discovery would already read to the
    /// `SKILL.md` path it logged; absent entries are not already read.
    fn check(&self, dirs: &[PathBuf]) -> BTreeMap<PathBuf, PathBuf>;
}

impl<F> PersonalSourceCheck for F
where
    F: Fn(&[PathBuf]) -> BTreeMap<PathBuf, PathBuf> + Send + Sync,
{
    fn check(&self, dirs: &[PathBuf]) -> BTreeMap<PathBuf, PathBuf> {
        self(dirs)
    }
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
