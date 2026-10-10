//! Reads one listed skill's body (`docs/tools.md`, "Skills"): the file
//! the session's maintained set holds for its name, and the body's
//! revalidated read. A free-standing reader over the shared set, with no
//! loop state and no events.

use std::path::Path;

use contract::skills::{SkillRead, Skills};

use crate::skill_header;
use crate::skill_set::SkillSet;

/// Reads one listed skill's body (`docs/tools.md`, "Skills").
pub struct SkillReader {
    set: SkillSet,
}

impl SkillReader {
    /// Reads skills from `set`: the session's maintained set.
    pub(crate) fn new(set: SkillSet) -> Self {
        Self { set }
    }
}

impl Skills for SkillReader {
    fn file(&self, name: &str) -> Option<std::path::PathBuf> {
        self.set.listed_file(name)
    }

    fn body(&self, name: &str, file: &Path) -> Result<String, SkillRead> {
        let bytes = std::fs::read(file).map_err(|error| {
            SkillRead::Io(format!("Could not read skill {}: {error}.", file.display()))
        })?;
        let text = String::from_utf8_lossy(&bytes);
        let header = skill_header::parse(&text).map_err(|_| SkillRead::Invalid)?;
        if header.name != name || !header.model_invocable {
            return Err(SkillRead::Invalid);
        }
        skill_header::body(&text).ok_or(SkillRead::Invalid)
    }
}

#[cfg(test)]
#[path = "skill_reader_tests.rs"]
mod tests;
