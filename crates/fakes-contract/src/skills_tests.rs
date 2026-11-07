//! Tests of [`NoSkills`](super::NoSkills): no name lists a file, and
//! reading one is an error.

use std::path::PathBuf;

use contract::skills::{SkillRead, Skills};

use super::NoSkills;

#[test]
fn no_skills_lists_nothing_and_reading_is_an_error() {
    let skills = NoSkills;
    assert_eq!(skills.file("anything"), None);
    assert_eq!(
        skills.body(
            "anything",
            PathBuf::from("/skills/anything/SKILL.md").as_path()
        ),
        Err(SkillRead::Invalid)
    );
}
