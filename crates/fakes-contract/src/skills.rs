//! Skills that list nothing (`contract::skills::Skills`).

use std::path::{Path, PathBuf};

use contract::skills::{SkillRead, Skills};

/// Skills with no entry: `file` is `None`, and `body` is an error.
pub struct NoSkills;

impl Skills for NoSkills {
    fn file(&self, _name: &str) -> Option<PathBuf> {
        None
    }

    fn body(&self, _name: &str, _file: &Path) -> Result<String, SkillRead> {
        Err(SkillRead::Invalid)
    }
}

#[cfg(test)]
#[path = "skills_tests.rs"]
mod tests;
