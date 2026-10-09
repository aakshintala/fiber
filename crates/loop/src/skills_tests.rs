//! Tests for skill discovery, shadowing, the listing and the
//! `skills_large` check, over places built in a temporary directory.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use contract::ErrorCode;
use contract::events::{CommandInfo, SkillListed, SkillSource};
use contract::shapes::ContentPart;
use fakes::clock::FakeClock;

use super::{Found, commands, discover, entry, expand, listing, size_notice, split_command};
use crate::prompt::PromptInputs;

/// A temporary tree: `top` is the repository's top level, `home` is
/// Fiber home, `person` the person's home.
struct Tree {
    _held: fakes::TempDir,
    root: PathBuf,
}

impl Tree {
    fn new() -> Self {
        let held = fakes::TempDir::new("fiber-skills");
        let root = held.path().canonicalize().unwrap();
        for dir in ["top", "home", "person"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        Self { _held: held, root }
    }

    fn top(&self) -> PathBuf {
        self.root.join("top")
    }

    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    fn person(&self) -> PathBuf {
        self.root.join("person")
    }

    fn inputs(&self) -> PromptInputs {
        let clock: Arc<dyn contract::clock::Clock> = FakeClock::new();
        let mut inputs = PromptInputs::new(
            self.home(),
            "/bin/sh".into(),
            self.root.join("events.jsonl").display().to_string(),
            clock,
            fakes::CONTEXT_WINDOW,
        );
        inputs.agents_home = Some(self.person());
        inputs
    }

    fn discover(&self) -> super::Discovered {
        discover(&self.inputs(), &self.top())
    }
}

/// Writes `<place>/<entry>/SKILL.md` with `name` and `description`, and
/// returns its path.
fn skill(place: &Path, entry: &str, name: &str, description: &str) -> PathBuf {
    write(
        place,
        entry,
        &format!("---\nname: {name}\ndescription: {description}\n---\nBody.\n"),
    )
}

fn write(place: &Path, entry: &str, text: &str) -> PathBuf {
    let dir = place.join(entry);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("SKILL.md");
    std::fs::write(&path, text).unwrap();
    path
}

fn names(found: &super::Discovered) -> Vec<&str> {
    found
        .skills
        .iter()
        .map(|found| found.listed.name.as_str())
        .collect()
}

fn messages(found: &super::Discovered) -> Vec<(ErrorCode, String)> {
    found
        .notices
        .iter()
        .map(|notice| (notice.code.clone(), notice.message.clone()))
        .collect()
}

fn path_of(found: &super::Discovered, name: &str) -> String {
    found
        .skills
        .iter()
        .find(|found| found.listed.name == name)
        .unwrap()
        .listed
        .path
        .clone()
}

#[test]
fn every_place_in_the_table_is_read() {
    let tree = Tree::new();
    let top = tree.top();
    let paths = [
        skill(&top.join(".fiber/skills"), "a", "fiber-repo", "d"),
        skill(&top.join(".agents/skills"), "a", "agents-repo", "d"),
        skill(&tree.home().join("skills"), "a", "home", "d"),
        skill(&tree.person().join(".agents/skills"), "a", "person", "d"),
    ];
    let extension = tree.root.join("ext/acme");
    let ext_skill = skill(&extension.join("skills"), "a", "ext-skill", "d");
    let ext_prompt = skill(&extension.join("prompts"), "a", "ext-prompt", "d");
    let mut inputs = tree.inputs();
    inputs.extension_dirs = vec![("acme".into(), extension)];
    let found = discover(&inputs, &top);
    assert!(found.notices.is_empty(), "{:?}", messages(&found));
    let got: Vec<(String, String, SkillSource)> = found
        .skills
        .into_iter()
        .map(|found| (found.listed.name, found.listed.path, found.listed.source))
        .collect();
    let want =
        |name: &str, path: &Path, source| (name.to_owned(), path.display().to_string(), source);
    assert_eq!(
        got,
        [
            want("fiber-repo", &paths[0], SkillSource::Repository),
            want("agents-repo", &paths[1], SkillSource::Repository),
            want("home", &paths[2], SkillSource::Personal),
            want("person", &paths[3], SkillSource::Personal),
            want("ext-skill", &ext_skill, SkillSource::Extension),
            want("ext-prompt", &ext_prompt, SkillSource::Extension),
        ]
    );
}

#[test]
fn no_place_means_no_skills_and_no_notice() {
    let tree = Tree::new();
    let found = tree.discover();
    assert!(found.skills.is_empty());
    assert!(found.notices.is_empty());
}

#[test]
fn without_a_person_home_that_place_is_skipped() {
    let tree = Tree::new();
    skill(&tree.person().join(".agents/skills"), "a", "person", "d");
    let mut inputs = tree.inputs();
    inputs.agents_home = None;
    assert!(discover(&inputs, &tree.top()).skills.is_empty());
}

#[test]
fn only_the_entrys_skill_md_is_read() {
    let tree = Tree::new();
    let place = tree.top().join(".agents/skills");
    skill(&place, "real", "real", "d");
    // Two levels down: never opened.
    skill(&place.join("group"), "deep", "deep", "d");
    // A plain file in the place, and a directory without a SKILL.md.
    std::fs::write(place.join("notes.md"), "x").unwrap();
    std::fs::create_dir_all(place.join("empty")).unwrap();
    std::fs::write(place.join("empty/README.md"), "x").unwrap();
    let found = tree.discover();
    assert_eq!(names(&found), ["real"]);
    assert!(found.notices.is_empty(), "{:?}", messages(&found));
}

#[test]
fn a_symlinked_skill_directory_is_followed_and_keeps_its_listed_path() {
    let tree = Tree::new();
    let target = tree.root.join("elsewhere");
    skill(&target, "real", "linked", "d");
    let place = tree.top().join(".agents/skills");
    std::fs::create_dir_all(&place).unwrap();
    std::os::unix::fs::symlink(target.join("real"), place.join("link")).unwrap();
    // A link to nothing is no skill.
    std::os::unix::fs::symlink(tree.root.join("gone"), place.join("broken")).unwrap();
    let found = tree.discover();
    assert_eq!(names(&found), ["linked"]);
    assert_eq!(
        path_of(&found, "linked"),
        place.join("link/SKILL.md").display().to_string()
    );
    assert!(found.notices.is_empty(), "{:?}", messages(&found));
}

#[test]
fn entries_are_read_in_byte_order_of_their_directory_names() {
    let tree = Tree::new();
    let place = tree.top().join(".fiber/skills");
    skill(&place, "c", "third", "d");
    skill(&place, "B", "first", "d");
    skill(&place, "a", "second", "d");
    assert_eq!(names(&tree.discover()), ["first", "second", "third"]);
}

#[test]
fn the_name_is_the_headers_not_the_directorys() {
    let tree = Tree::new();
    skill(
        &tree.top().join(".fiber/skills"),
        "dir-name",
        "Header Name",
        "d",
    );
    assert_eq!(names(&tree.discover()), ["Header Name"]);
}

/// The six places in order, most specific first, under `tree`.
fn six_places(tree: &Tree) -> [PathBuf; 6] {
    let extension = tree.root.join("ext/acme");
    [
        tree.top().join(".fiber/skills"),
        tree.top().join(".agents/skills"),
        tree.home().join("skills"),
        tree.person().join(".agents/skills"),
        extension.join("skills"),
        extension.join("prompts"),
    ]
}

fn with_extension(tree: &Tree) -> PromptInputs {
    let mut inputs = tree.inputs();
    inputs.extension_dirs = vec![("acme".into(), tree.root.join("ext/acme"))];
    inputs
}

#[test]
fn a_clash_names_the_loser_and_the_winner_from_the_more_specific_place() {
    for winner in 0..6 {
        for loser in winner + 1..6 {
            let tree = Tree::new();
            let places = six_places(&tree);
            let won = skill(&places[winner], "a", "same", "first");
            let lost = skill(&places[loser], "a", "same", "second");
            let found = discover(&with_extension(&tree), &tree.top());
            assert_eq!(names(&found), ["same"], "{winner} over {loser}");
            assert_eq!(found.skills[0].listed.description, "first");
            assert_eq!(
                messages(&found),
                [(
                    ErrorCode::SkillShadowed,
                    format!(
                        "Skill same at {} is shadowed by {}, which is used.",
                        lost.display(),
                        won.display()
                    )
                )],
                "{winner} over {loser}"
            );
        }
    }
}

#[test]
fn a_clash_inside_one_place_is_won_by_the_first_directory_name() {
    let tree = Tree::new();
    let place = tree.top().join(".fiber/skills");
    let won = skill(&place, "a", "same", "first");
    let lost = skill(&place, "b", "same", "second");
    let found = tree.discover();
    assert_eq!(found.skills[0].listed.description, "first");
    assert_eq!(
        messages(&found),
        [(
            ErrorCode::SkillShadowed,
            format!(
                "Skill same at {} is shadowed by {}, which is used.",
                lost.display(),
                won.display()
            )
        )]
    );
}

#[test]
fn a_three_way_clash_raises_two_notices_naming_the_one_winner() {
    let tree = Tree::new();
    let won = skill(&tree.top().join(".fiber/skills"), "a", "same", "d");
    let second = skill(&tree.home().join("skills"), "a", "same", "d");
    let third = skill(&tree.person().join(".agents/skills"), "a", "same", "d");
    let found = tree.discover();
    assert_eq!(names(&found), ["same"]);
    let want = |loser: &Path| {
        (
            ErrorCode::SkillShadowed,
            format!(
                "Skill same at {} is shadowed by {}, which is used.",
                loser.display(),
                won.display()
            ),
        )
    };
    assert_eq!(messages(&found), [want(&second), want(&third)]);
}

#[test]
fn a_place_reached_twice_is_read_once_and_shadows_nothing() {
    // The repository's top level is the person's home: `~/.agents/skills`
    // is the repository's `.agents/skills`.
    let tree = Tree::new();
    skill(&tree.top().join(".agents/skills"), "a", "same", "d");
    let mut inputs = tree.inputs();
    inputs.agents_home = Some(tree.top());
    let found = discover(&inputs, &tree.top());
    assert_eq!(names(&found), ["same"]);
    assert!(found.notices.is_empty(), "{:?}", messages(&found));
    // Fiber home is `~/.agents`: `skills/` is `~/.agents/skills`.
    let tree = Tree::new();
    skill(&tree.person().join(".agents/skills"), "a", "same", "d");
    let mut inputs = tree.inputs();
    inputs.home = tree.person().join(".agents");
    let found = discover(&inputs, &tree.top());
    assert_eq!(names(&found), ["same"]);
    assert!(found.notices.is_empty(), "{:?}", messages(&found));
    // A symlink to an earlier place is the same place too.
    let tree = Tree::new();
    skill(&tree.top().join(".fiber/skills"), "a", "same", "d");
    std::fs::create_dir_all(tree.home()).unwrap();
    std::os::unix::fs::symlink(tree.top().join(".fiber/skills"), tree.home().join("skills"))
        .unwrap();
    let found = tree.discover();
    assert_eq!(names(&found), ["same"]);
    assert!(found.notices.is_empty(), "{:?}", messages(&found));
}

#[test]
fn two_distinct_places_with_one_name_raise_exactly_one_notice() {
    let tree = Tree::new();
    skill(&tree.top().join(".fiber/skills"), "a", "same", "d");
    skill(&tree.home().join("skills"), "a", "same", "d");
    assert_eq!(tree.discover().notices.len(), 1);
}

#[test]
fn a_header_that_does_not_parse_or_lacks_a_field_is_left_out_naming_its_path() {
    let tree = Tree::new();
    let place = tree.top().join(".fiber/skills");
    let broken = write(&place, "a-broken", "no header at all\n");
    let nameless = write(&place, "b-nameless", "---\ndescription: d\n---\n");
    let blank = write(&place, "c-blank", "---\nname: n\ndescription: '  '\n---\n");
    skill(&place, "d-fine", "fine", "d");
    let found = tree.discover();
    assert_eq!(names(&found), ["fine"]);
    let invalid = |path: &Path, reason: &str| {
        (
            ErrorCode::SkillInvalid,
            format!("Skill {} was left out: {reason}.", path.display()),
        )
    };
    assert_eq!(
        messages(&found),
        [
            invalid(&broken, "its header does not parse"),
            invalid(&nameless, "it has no name"),
            invalid(&blank, "it has no description"),
        ]
    );
}

#[test]
fn an_invalid_skill_shadows_nothing() {
    let tree = Tree::new();
    write(
        &tree.top().join(".fiber/skills"),
        "a",
        "---\nname: same\n---\n",
    );
    skill(&tree.home().join("skills"), "a", "same", "d");
    let found = tree.discover();
    assert_eq!(names(&found), ["same"]);
    assert_eq!(found.notices.len(), 1);
    assert_eq!(found.notices[0].code, ErrorCode::SkillInvalid);
}

#[test]
fn non_utf8_bytes_are_read_lossily() {
    let tree = Tree::new();
    let place = tree.top().join(".fiber/skills");
    std::fs::create_dir_all(place.join("a")).unwrap();
    std::fs::write(
        place.join("a/SKILL.md"),
        b"---\nname: n\ndescription: bad \xff byte\n---\n",
    )
    .unwrap();
    let found = tree.discover();
    assert_eq!(found.skills[0].listed.description, "bad \u{fffd} byte");
}

#[test]
fn an_unreadable_skill_md_is_an_io_failed_notice_naming_it() {
    let tree = Tree::new();
    let place = tree.top().join(".fiber/skills");
    // A directory where the file should be cannot be read as one.
    std::fs::create_dir_all(place.join("a/SKILL.md")).unwrap();
    skill(&place, "b", "fine", "d");
    let found = tree.discover();
    assert_eq!(names(&found), ["fine"]);
    let [notice] = found.notices.as_slice() else {
        panic!("{:?}", messages(&found));
    };
    assert_eq!(notice.code, ErrorCode::IoFailed);
    assert!(
        notice.message.starts_with(&format!(
            "Could not read skill {}",
            place.join("a/SKILL.md").display()
        )),
        "{}",
        notice.message
    );
    assert!(notice.message.ends_with('.'), "{}", notice.message);
}

#[test]
fn a_place_that_cannot_be_listed_is_an_io_failed_notice_naming_it() {
    let tree = Tree::new();
    let place = tree.top().join(".fiber/skills");
    std::fs::create_dir_all(place.parent().unwrap()).unwrap();
    std::fs::write(&place, "not a directory").unwrap();
    skill(&tree.home().join("skills"), "a", "fine", "d");
    let found = tree.discover();
    assert_eq!(names(&found), ["fine"]);
    let [notice] = found.notices.as_slice() else {
        panic!("{:?}", messages(&found));
    };
    assert_eq!(notice.code, ErrorCode::IoFailed);
    assert!(
        notice
            .message
            .starts_with(&format!("Could not read skill {}", place.display())),
        "{}",
        notice.message
    );
}

fn listed_names<'a>(found: &'a [Found], disabled: &[&str]) -> Vec<&'a str> {
    let disabled: Vec<String> = disabled.iter().map(|name| (*name).to_owned()).collect();
    listing(found, &disabled)
        .into_iter()
        .map(|found| found.listed.name.as_str())
        .collect()
}

#[test]
fn a_skill_with_disable_model_invocation_true_is_not_listed_and_false_is() {
    let tree = Tree::new();
    let place = tree.top().join(".fiber/skills");
    write(
        &place,
        "a",
        "---\nname: hidden\ndescription: d\ndisable-model-invocation: true\n---\n",
    );
    write(
        &place,
        "b",
        "---\nname: shown\ndescription: d\ndisable-model-invocation: false\n---\n",
    );
    skill(&place, "c", "plain", "d");
    let found = tree.discover();
    assert_eq!(names(&found), ["hidden", "shown", "plain"]);
    assert_eq!(listed_names(&found.skills, &[]), ["plain", "shown"]);
}

#[test]
fn a_skill_in_an_extensions_prompts_is_never_listed() {
    let tree = Tree::new();
    let extension = tree.root.join("ext/acme");
    let path = skill(&extension.join("prompts"), "a", "prompt", "d");
    skill(&extension.join("skills"), "a", "listed", "d");
    let mut inputs = tree.inputs();
    inputs.extension_dirs = vec![("acme".into(), extension)];
    let found = discover(&inputs, &tree.top());
    assert_eq!(names(&found), ["listed", "prompt"]);
    assert_eq!(path_of(&found, "prompt"), path.display().to_string());
    assert_eq!(listed_names(&found.skills, &[]), ["listed"]);
}

#[test]
fn a_prompts_skill_wins_over_a_skill_found_after_it() {
    // Only built-in skills come after an extension; the order inside an
    // extension is `skills/` then `prompts/`, so a `prompts/` skill wins
    // over a later extension's `skills/` of the same name.
    let tree = Tree::new();
    let first = tree.root.join("ext/aaa");
    let second = tree.root.join("ext/bbb");
    let won = skill(&first.join("prompts"), "a", "same", "d");
    let lost = skill(&second.join("skills"), "a", "same", "d");
    let mut inputs = tree.inputs();
    inputs.extension_dirs = vec![("bbb".into(), second), ("aaa".into(), first)];
    let found = discover(&inputs, &tree.top());
    assert!(listed_names(&found.skills, &[]).is_empty());
    assert_eq!(
        messages(&found),
        [(
            ErrorCode::SkillShadowed,
            format!(
                "Skill same at {} is shadowed by {}, which is used.",
                lost.display(),
                won.display()
            )
        )]
    );
}

#[test]
fn extensions_are_read_in_name_order() {
    let tree = Tree::new();
    skill(&tree.root.join("ext/zed/skills"), "a", "from-zed", "d");
    skill(&tree.root.join("ext/abe/skills"), "a", "from-abe", "d");
    let mut inputs = tree.inputs();
    inputs.extension_dirs = vec![
        ("zed".into(), tree.root.join("ext/zed")),
        ("abe".into(), tree.root.join("ext/abe")),
    ];
    let found = discover(&inputs, &tree.top());
    assert_eq!(names(&found), ["from-abe", "from-zed"]);
}

#[test]
fn a_switched_off_name_is_not_listed_even_when_it_won() {
    let tree = Tree::new();
    skill(&tree.top().join(".fiber/skills"), "a", "same", "d");
    skill(&tree.home().join("skills"), "a", "same", "d");
    skill(&tree.home().join("skills"), "b", "other", "d");
    let found = tree.discover();
    // The clash is still reported.
    assert_eq!(found.notices.len(), 1);
    assert_eq!(listed_names(&found.skills, &["same"]), ["other"]);
    assert_eq!(listed_names(&found.skills, &["nothing"]), ["other", "same"]);
    assert!(listed_names(&found.skills, &["same", "other"]).is_empty());
}

#[test]
fn the_listing_is_sorted_by_name_in_byte_order() {
    let tree = Tree::new();
    let place = tree.top().join(".fiber/skills");
    skill(&place, "1", "b", "d");
    skill(&place, "2", "B", "d");
    skill(&place, "3", "a", "d");
    skill(&tree.home().join("skills"), "1", "A", "d");
    let found = tree.discover();
    assert_eq!(listed_names(&found.skills, &[]), ["A", "B", "a", "b"]);
}

#[test]
fn an_entry_is_name_description_and_path() {
    let line = entry(&SkillListed {
        name: "review".into(),
        description: "Reviews a diff.".into(),
        path: "/repo/.agents/skills/review/SKILL.md".into(),
        source: SkillSource::Repository,
    });
    assert_eq!(
        line,
        "- review: Reviews a diff. (/repo/.agents/skills/review/SKILL.md)"
    );
}

/// A found skill whose listing line is exactly `bytes` long.
fn sized(name: &str, place: &str, bytes: usize) -> Found {
    // The line is `- {name}: {description} (p)`.
    let description = "x".repeat(bytes - name.len() - 8);
    let listed = SkillListed {
        name: name.into(),
        description,
        path: "p".into(),
        source: SkillSource::Repository,
    };
    assert_eq!(entry(&listed).len(), bytes);
    Found {
        listed,
        model_invocable: true,
        argument_hint: None,
        place: place.into(),
        extension: None,
    }
}

fn large(found: &[Found], window: u64) -> Option<String> {
    let listed = listing(found, &[]);
    size_notice(&listed, window).map(|notice| {
        assert_eq!(notice.code, ErrorCode::SkillsLarge);
        notice.message
    })
}

#[test]
fn a_listing_of_exactly_ten_percent_is_not_large_and_one_byte_over_is() {
    // 40 bytes at four bytes a token is 10 tokens; 10% of 100 is 10.
    let one = [sized("a", "/p", 40)];
    assert_eq!(large(&one, 100), None);
    assert!(large(&one, 99).is_some());
    // Two entries and the line break between them: 20 + 1 + 19.
    let two = [sized("a", "/p", 20), sized("b", "/p", 19)];
    assert_eq!(large(&two, 100), None);
    assert!(large(&two, 99).is_some());
    // A window where `*` and `+` read differently.
    assert_eq!(large(&one, 2000), None);
}

#[test]
fn an_empty_listing_is_never_large() {
    assert_eq!(large(&[], 1), None);
}

#[test]
fn the_notice_names_the_three_largest_places_with_an_extensions_places_summed() {
    let found = [
        sized("a", "/small", 20),
        sized("b", "/big", 100),
        sized("c", "extension acme", 40),
        sized("d", "extension acme", 50),
        sized("e", "/mid", 60),
    ];
    // 20 + 100 + 40 + 50 + 60 and four line breaks: 274 bytes.
    let message = large(&found, 10).unwrap();
    assert_eq!(
        message,
        "The skills listing is about 68 tokens, over 10% of the 10-token context window. \
         Largest: /big (100 bytes), extension acme (90 bytes), /mid (60 bytes)."
    );
}

#[test]
fn a_skill_the_model_may_not_load_adds_nothing_to_the_size() {
    let mut hidden = sized("hidden", "/p", 400);
    hidden.model_invocable = false;
    let found = [sized("a", "/p", 40), hidden];
    assert_eq!(large(&found, 100), None);
}

fn builtin_dir(tree: &Tree) -> PathBuf {
    tree.home().join("docs/skills")
}

#[test]
fn a_built_in_skill_is_found_with_its_real_path() {
    let tree = Tree::new();
    let path = skill(&builtin_dir(&tree), "a", "built", "d");
    let found = tree.discover();
    assert_eq!(names(&found), ["built"]);
    assert!(found.notices.is_empty(), "{:?}", messages(&found));
    let skill = &found.skills[0];
    assert_eq!(skill.listed.source, SkillSource::Builtin);
    assert_eq!(skill.listed.path, path.display().to_string());
    assert!(skill.model_invocable);
    assert_eq!(skill.place, builtin_dir(&tree).display().to_string());
}

#[test]
fn without_docs_or_docs_skills_there_are_no_built_ins_and_no_notice() {
    let tree = Tree::new();
    let found = tree.discover();
    assert!(found.skills.is_empty());
    assert!(found.notices.is_empty());
    std::fs::create_dir_all(tree.home().join("docs")).unwrap();
    let found = tree.discover();
    assert!(found.skills.is_empty());
    assert!(found.notices.is_empty(), "{:?}", messages(&found));
}

#[test]
fn a_skill_of_any_other_place_wins_over_a_built_in_of_its_name() {
    for winner in 0..6 {
        let tree = Tree::new();
        let places = six_places(&tree);
        let won = skill(&places[winner], "a", "same", "mine");
        let lost = skill(&builtin_dir(&tree), "b", "same", "built-in");
        let found = discover(&with_extension(&tree), &tree.top());
        assert_eq!(names(&found), ["same"], "place {winner}");
        assert_eq!(found.skills[0].listed.description, "mine", "place {winner}");
        assert_ne!(
            found.skills[0].listed.source,
            SkillSource::Builtin,
            "place {winner}"
        );
        assert_eq!(
            messages(&found),
            [(
                ErrorCode::SkillShadowed,
                format!(
                    "Skill same at {} is shadowed by {}, which is used.",
                    lost.display(),
                    won.display()
                )
            )],
            "place {winner}"
        );
    }
}

#[test]
fn a_switched_off_built_in_is_left_out_and_the_other_stays() {
    let tree = Tree::new();
    skill(&builtin_dir(&tree), "a", "alpha", "d");
    skill(&builtin_dir(&tree), "b", "beta", "d");
    let found = tree.discover();
    assert_eq!(listed_names(&found.skills, &["alpha"]), ["beta"]);
    assert_eq!(listed_names(&found.skills, &[]), ["alpha", "beta"]);
}

#[test]
fn the_size_notice_names_the_built_in_directory() {
    let tree = Tree::new();
    skill(&builtin_dir(&tree), "a", "alpha", "d");
    skill(&builtin_dir(&tree), "b", "beta", "d");
    let found = tree.discover();
    let message = large(&found.skills, 10).unwrap();
    assert!(
        message.contains(&format!("Largest: {} (", builtin_dir(&tree).display())),
        "{message}"
    );
}

/// The shipped texts, checked where they ship (`docs/skills/`): each
/// parses, its name is its directory's, the model may load it and its
/// description is non-empty. A change to either text runs `loop` in CI
/// (`docs/ci.md`, "Selection").
const SHIPPED: [(&str, &str); 2] = [
    (
        "cache-warming",
        include_str!("../../../docs/skills/cache-warming/SKILL.md"),
    ),
    (
        "using-fiber",
        include_str!("../../../docs/skills/using-fiber/SKILL.md"),
    ),
];

#[test]
fn both_shipped_texts_parse_with_the_name_of_their_directory() {
    for (directory, text) in SHIPPED {
        let header = crate::skill_header::parse(text).unwrap();
        assert_eq!(&header.name, directory);
        assert!(header.model_invocable);
        assert!(!header.description.is_empty());
    }
}

/// Writes the `review-pr` skill with body `Review it.` into the
/// repository's `.agents/skills/`.
fn review_skill(tree: &Tree) {
    write(
        &tree.top().join(".agents/skills"),
        "review-pr",
        "---\nname: review-pr\ndescription: Reviews.\n---\nReview it.\n",
    );
}

fn text_only(text: &str) -> Vec<ContentPart> {
    vec![ContentPart::Text { text: text.into() }]
}

fn expand_text(
    tree: &Tree,
    inputs: &PromptInputs,
    content: &[ContentPart],
) -> Option<Vec<ContentPart>> {
    expand(inputs, &tree.top(), content)
}

fn expanded(content: &[ContentPart]) -> &str {
    let [ContentPart::Text { text }, ..] = content else {
        panic!("the first part is text: {content:?}");
    };
    text
}

#[test]
fn a_slash_command_expands_to_the_body_then_the_arguments() {
    let tree = Tree::new();
    review_skill(&tree);
    let out = expand_text(&tree, &tree.inputs(), &text_only("/review-pr 42")).unwrap();
    assert_eq!(expanded(&out), "Review it.\n\n42");
}

#[test]
fn a_bare_slash_command_sends_the_body_alone() {
    let tree = Tree::new();
    review_skill(&tree);
    for prompt in ["/review-pr", "/review-pr   "] {
        let out = expand_text(&tree, &tree.inputs(), &text_only(prompt)).unwrap();
        assert_eq!(expanded(&out), "Review it.", "{prompt:?}");
    }
}

#[test]
fn leading_whitespace_is_skipped_and_arguments_span_lines() {
    let tree = Tree::new();
    review_skill(&tree);
    let out = expand_text(&tree, &tree.inputs(), &text_only("  /review-pr\n42 more")).unwrap();
    assert_eq!(expanded(&out), "Review it.\n\n42 more");
    // Tabs are whitespace too, not only spaces.
    let out = expand_text(&tree, &tree.inputs(), &text_only("\t/review-pr\t42")).unwrap();
    assert_eq!(expanded(&out), "Review it.\n\n42");
}

#[test]
fn a_prompt_naming_no_skill_expands_nothing() {
    let tree = Tree::new();
    review_skill(&tree);
    let inputs = tree.inputs();
    for prompt in [
        "/nope x",
        "/",
        "/ x",
        "review-pr 42",
        "/Review-pr 42",
        "",
        "   ",
    ] {
        assert_eq!(
            expand_text(&tree, &inputs, &text_only(prompt)),
            None,
            "{prompt:?}"
        );
    }
}

#[test]
fn the_split_names_no_command_without_a_slash_or_a_name() {
    assert_eq!(split_command("/review-pr 42"), Some(("review-pr", "42")));
    assert_eq!(
        split_command("  /review-pr\n42 more"),
        Some(("review-pr", "42 more"))
    );
    assert_eq!(split_command("/review-pr"), Some(("review-pr", "")));
    assert_eq!(split_command("/review-pr   "), Some(("review-pr", "")));
    assert_eq!(split_command("/"), None);
    assert_eq!(split_command("/ x"), None);
    assert_eq!(split_command("review-pr 42"), None);
    assert_eq!(split_command(""), None);
}

#[test]
fn a_prompt_template_expands() {
    let tree = Tree::new();
    write(
        &tree.top().join(".agents/skills"),
        "template",
        "---\nname: template\ndescription: d\ndisable-model-invocation: true\n---\nFill this in.\n",
    );
    let out = expand_text(&tree, &tree.inputs(), &text_only("/template 42")).unwrap();
    assert_eq!(expanded(&out), "Fill this in.\n\n42");
}

#[test]
fn an_extension_prompt_expands() {
    let tree = Tree::new();
    let extension = tree.root.join("ext/acme");
    write(
        &extension.join("prompts"),
        "deploy",
        "---\nname: deploy\ndescription: d\n---\nDeploy it.\n",
    );
    let mut inputs = tree.inputs();
    inputs.extension_dirs = vec![("acme".into(), extension)];
    let out = expand_text(&tree, &inputs, &text_only("/deploy now")).unwrap();
    assert_eq!(expanded(&out), "Deploy it.\n\nnow");
}

#[test]
fn a_switched_off_skill_does_not_expand() {
    let tree = Tree::new();
    review_skill(&tree);
    let mut inputs = tree.inputs();
    inputs.skills_disabled = vec!["review-pr".into()];
    assert_eq!(
        expand_text(&tree, &inputs, &text_only("/review-pr 42")),
        None
    );
}

#[test]
fn a_shared_name_expands_the_winner() {
    let tree = Tree::new();
    write(
        &tree.top().join(".agents/skills"),
        "a",
        "---\nname: same\ndescription: d\n---\nRepository body.\n",
    );
    write(
        &tree.home().join("skills"),
        "a",
        "---\nname: same\ndescription: d\n---\nHome body.\n",
    );
    let out = expand_text(&tree, &tree.inputs(), &text_only("/same 42")).unwrap();
    assert_eq!(expanded(&out), "Repository body.\n\n42");
}

#[test]
fn only_the_first_text_part_expands_and_the_rest_is_kept() {
    let tree = Tree::new();
    review_skill(&tree);
    // A first part that is no text expands nothing.
    let image = ContentPart::Image {
        path: "artifacts/shot.png".into(),
        mime_type: "image/png".into(),
        width: 1,
        height: 1,
    };
    assert_eq!(
        expand_text(&tree, &tree.inputs(), std::slice::from_ref(&image)),
        None
    );
    // Later parts stay as they are, in order.
    let content = vec![
        ContentPart::Text {
            text: "/review-pr 42".into(),
        },
        ContentPart::Text {
            text: "kept".into(),
        },
        image.clone(),
    ];
    let out = expand_text(&tree, &tree.inputs(), &content).unwrap();
    assert_eq!(out.len(), 3);
    assert_eq!(expanded(&out), "Review it.\n\n42");
    assert_eq!(
        out[1],
        ContentPart::Text {
            text: "kept".into()
        }
    );
    assert_eq!(out[2], image);
}

fn row(name: &str, description: &str, hint: Option<&str>, tag: &str) -> CommandInfo {
    CommandInfo {
        name: name.into(),
        description: description.into(),
        argument_hint: hint.map(str::to_owned),
        tag: tag.into(),
    }
}

#[test]
fn rows_tag_a_model_skill_skill_and_a_template_template() {
    let tree = Tree::new();
    let place = tree.top().join(".fiber/skills");
    write(
        &place,
        "a",
        "---\nname: review\ndescription: Review a diff.\nargument-hint: [base]\n\
         disable-model-invocation: true\n---\n",
    );
    skill(&place, "b", "tdd", "Test first.");
    let extension = tree.root.join("ext/acme");
    write(
        &extension.join("prompts"),
        "a",
        "---\nname: ship\ndescription: Ship it.\nargument-hint: <tag>\n---\n",
    );
    let mut inputs = tree.inputs();
    inputs.extension_dirs = vec![("acme".into(), extension)];
    let found = discover(&inputs, &tree.top());
    assert_eq!(
        commands(&found.skills, &[]),
        [
            row("review", "Review a diff.", Some("[base]"), "template"),
            row("tdd", "Test first.", None, "skill"),
            row("ship", "Ship it.", Some("<tag>"), "template"),
        ]
    );
}

#[test]
fn a_switched_off_skill_or_template_has_no_row() {
    let tree = Tree::new();
    let place = tree.top().join(".fiber/skills");
    skill(&place, "a", "off", "d");
    write(
        &place,
        "b",
        "---\nname: off-template\ndescription: d\ndisable-model-invocation: true\n---\n",
    );
    skill(&place, "c", "on", "d");
    let found = tree.discover();
    let disabled = vec!["off".to_owned(), "off-template".to_owned()];
    assert_eq!(
        commands(&found.skills, &disabled),
        [row("on", "d", None, "skill")]
    );
}

#[test]
fn a_shared_name_has_one_row_the_winners() {
    let tree = Tree::new();
    skill(&tree.top().join(".fiber/skills"), "a", "same", "winner");
    write(
        &tree.home().join("skills"),
        "a",
        "---\nname: same\ndescription: loser\ndisable-model-invocation: true\n---\n",
    );
    let found = tree.discover();
    assert_eq!(
        commands(&found.skills, &[]),
        [row("same", "winner", None, "skill")]
    );
}

#[test]
fn the_session_commands_are_read_from_the_repository_top_level() {
    let tree = Tree::new();
    std::fs::create_dir_all(tree.top().join(".git")).unwrap();
    std::fs::write(tree.top().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    let workspace = tree.top().join("sub");
    std::fs::create_dir_all(&workspace).unwrap();
    skill(&tree.top().join(".fiber/skills"), "a", "top", "d");
    write(&workspace.join(".fiber/skills"), "a", "---\nname: broken\n");
    let mut inputs = tree.inputs();
    inputs.skills_disabled = vec!["gone".into()];
    skill(&tree.home().join("skills"), "a", "gone", "d");
    assert_eq!(
        crate::commands(&inputs, &workspace, &[]).rows,
        [row("top", "d", None, "skill")]
    );
}
