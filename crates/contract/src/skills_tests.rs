use std::collections::BTreeMap;
use std::path::PathBuf;

use super::*;

#[test]
fn the_blanket_check_forwards_the_closure_result() {
    // The blanket `PersonalSourceCheck for F` implementation exists so
    // callers and tests can pass a closure: `check` must return whatever
    // the closure returns for the directories it was given.
    let dir = PathBuf::from("/source/skills/review");
    let logged = PathBuf::from("/person/.agents/skills/review/SKILL.md");
    let wanted = BTreeMap::from([(dir.clone(), logged)]);
    let probed = dir.clone();
    let check = |dirs: &[PathBuf]| {
        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0], dir);
        wanted.clone()
    };
    assert_eq!(check.check(std::slice::from_ref(&probed)), wanted);
}
