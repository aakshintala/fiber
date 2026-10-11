//! Tests for the `skill` tool, with an in-file fake `Skills` over real
//! temporary files.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use contract::ErrorCode;
use contract::events::SkillLoad;
use contract::shapes::{ContentPart, Effect};
use contract::skills::{SkillRead, Skills};
use contract::tool::{Bound, Tool};
use fakes::{CancelToken, Recorder, TempDir};
use serde_json::{Map, Value, json};

use super::Skill;

/// A fake `Skills`: `file` returns the mapped path, and `body` reads the
/// file's text unless an error is scripted for the name.
struct Fake {
    files: BTreeMap<String, PathBuf>,
    errors: BTreeMap<String, SkillRead>,
    panic_body: bool,
}

impl Fake {
    fn mapping(files: BTreeMap<String, PathBuf>) -> Self {
        Self {
            files,
            errors: BTreeMap::new(),
            panic_body: false,
        }
    }
}

impl Skills for Fake {
    fn file(&self, name: &str) -> Option<PathBuf> {
        self.files.get(name).cloned()
    }

    fn body(&self, name: &str, file: &Path) -> Result<String, SkillRead> {
        if self.panic_body {
            panic!("the tool must not read the body");
        }
        if let Some(error) = self.errors.get(name) {
            return Err(error.clone());
        }
        Ok(std::fs::read_to_string(file).unwrap())
    }
}

fn tool(fake: Fake) -> Skill {
    Skill::new(Arc::new(fake))
}

fn args(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => {
            panic!("arguments are an object")
        }
    }
}

fn name(value: &str) -> Map<String, Value> {
    args(json!({"name": value}))
}

/// Writes `SKILL.md` holding `text` under `dir`, creating it.
fn write_skill(dir: &Path, text: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join("SKILL.md");
    std::fs::write(&path, text).unwrap();
    path
}

fn effects_of(skill: &Skill, arguments: &Map<String, Value>) -> contract::tool::Effects {
    skill.effects(arguments).unwrap()
}

fn run_of(skill: &Skill, arguments: &Map<String, Value>) -> contract::tool::Output {
    skill.run(arguments, &CancelToken::new(), &Recorder::default())
}

fn code(output: &contract::tool::Output) -> Option<ErrorCode> {
    output.error.as_ref().map(|error| error.code.clone())
}

fn text_of(output: &contract::tool::Output) -> String {
    match output.content.first() {
        Some(ContentPart::Text { text }) => text.clone(),
        _ => panic!("one text part, got {:?}", output.content),
    }
}

#[test]
fn the_definition_does_not_depend_on_the_skills() {
    let one = tool(Fake::mapping(BTreeMap::from([(
        "tdd".into(),
        PathBuf::from("/w/tdd/SKILL.md"),
    )])));
    let other = tool(Fake::mapping(BTreeMap::from([(
        "review".into(),
        PathBuf::from("/w/review/SKILL.md"),
    )])));
    assert_eq!(
        serde_json::to_vec(&one.definition()).unwrap(),
        serde_json::to_vec(&other.definition()).unwrap()
    );
    assert!(!one.definition().description.contains("tdd"));
    assert!(!one.definition().description.contains("review"));
}

#[test]
fn effects_on_a_symlinked_skill_declares_reads_on_its_target() {
    let held = TempDir::new("fiber-skill-symlink");
    let root = held.path().canonicalize().unwrap();
    let real = root.join("a/tdd");
    write_skill(&real, "Write the failing test first.\n");
    std::os::unix::fs::symlink(root.join("a/tdd"), root.join("skills-tdd")).unwrap();
    let link = root.join("skills-tdd/SKILL.md");
    let skill = tool(Fake::mapping(BTreeMap::from([("tdd".into(), link)])));
    let declared = effects_of(&skill, &name("tdd"));
    let target = std::fs::canonicalize(real.join("SKILL.md")).unwrap();
    assert_eq!(declared.declared.effects, [Effect::Reads]);
    assert_eq!(
        declared.declared.paths,
        Some(vec![target.display().to_string()])
    );
    assert!(declared.declared.reversible);
    assert_eq!(declared.subject, Some(String::new()));
    assert_eq!(declared.prefix, None);
    assert!(!declared.always_reviewed);
}

#[test]
fn effects_on_an_unlisted_name_or_a_missing_file_declares_nothing() {
    let held = TempDir::new("fiber-skill-no-effect");
    let root = held.path().canonicalize().unwrap();
    let skill = tool(Fake::mapping(BTreeMap::from([(
        "gone".into(),
        root.join("gone/SKILL.md"),
    )])));
    for arguments in [name("nope"), name("gone")] {
        let declared = effects_of(&skill, &arguments);
        assert!(declared.declared.effects.is_empty());
        assert_eq!(declared.declared.paths, None);
    }
}

#[test]
fn effects_then_run_returns_the_body_under_its_path() {
    let held = TempDir::new("fiber-skill-load");
    let root = held.path().canonicalize().unwrap();
    let path = write_skill(&root.join("tdd"), "Write the failing test first.\n");
    let skill = tool(Fake::mapping(BTreeMap::from([(
        "tdd".into(),
        path.clone(),
    )])));
    let arguments = name("tdd");
    effects_of(&skill, &arguments);
    let output = run_of(&skill, &arguments);
    let listing = path.display().to_string();
    assert_eq!(
        output.content,
        [ContentPart::Text {
            text: format!("{listing}\n\nWrite the failing test first.\n"),
        }]
    );
    assert_eq!(output.error, None);
    assert_eq!(
        output.control,
        Some(contract::events::Control {
            handoff: None,
            questions: None,
            skill: Some(SkillLoad {
                name: "tdd".into(),
                path: listing,
            }),
        })
    );
}

#[test]
fn an_empty_body_gives_the_path_and_a_blank_line() {
    let held = TempDir::new("fiber-skill-empty");
    let root = held.path().canonicalize().unwrap();
    let path = write_skill(&root.join("tdd"), "");
    let skill = tool(Fake::mapping(BTreeMap::from([(
        "tdd".into(),
        path.clone(),
    )])));
    let arguments = name("tdd");
    effects_of(&skill, &arguments);
    let output = run_of(&skill, &arguments);
    assert_eq!(text_of(&output), format!("{}\n\n", path.display()));
}

#[test]
fn a_link_repointed_between_effects_and_run_fails_path_changed() {
    let held = TempDir::new("fiber-skill-swap");
    let root = held.path().canonicalize().unwrap();
    write_skill(&root.join("a/tdd"), "Old body.\n");
    write_skill(&root.join("b/tdd"), "New body that must never be read.\n");
    std::fs::create_dir_all(root.join("skills")).unwrap();
    std::os::unix::fs::symlink(root.join("a/tdd"), root.join("skills/tdd")).unwrap();
    let link = root.join("skills/tdd/SKILL.md");
    let skill = tool(Fake::mapping(BTreeMap::from([("tdd".into(), link)])));
    let arguments = name("tdd");
    effects_of(&skill, &arguments);
    std::fs::remove_file(root.join("skills/tdd")).unwrap();
    std::os::unix::fs::symlink(root.join("b/tdd"), root.join("skills/tdd")).unwrap();
    let output = run_of(&skill, &arguments);
    assert_eq!(code(&output), Some(ErrorCode::PathChanged));
    assert!(!text_of(&output).contains("must never be read"));
    assert_eq!(output.control, None);
}

#[test]
fn a_failed_resolution_clears_the_judged_target() {
    let held = TempDir::new("fiber-skill-clear");
    let root = held.path().canonicalize().unwrap();
    let path = write_skill(&root.join("tdd"), "Secret body that must never be read.\n");
    let skill = tool(Fake::mapping(BTreeMap::from([(
        "tdd".into(),
        path.clone(),
    )])));
    let arguments = name("tdd");
    effects_of(&skill, &arguments);
    std::fs::remove_file(&path).unwrap();
    let declared = effects_of(&skill, &arguments);
    assert!(declared.declared.effects.is_empty());
    assert_eq!(declared.declared.paths, None);
    write_skill(&root.join("tdd"), "Secret body that must never be read.\n");
    let output = run_of(&skill, &arguments);
    assert_eq!(code(&output), Some(ErrorCode::PathChanged));
    assert!(!text_of(&output).contains("must never be read"));
    assert_eq!(output.control, None);
}

#[test]
fn run_with_no_prior_effects_fails_path_changed() {
    let held = TempDir::new("fiber-skill-no-judgement");
    let root = held.path().canonicalize().unwrap();
    let path = write_skill(&root.join("tdd"), "Body.\n");
    let skill = tool(Fake::mapping(BTreeMap::from([("tdd".into(), path)])));
    let output = run_of(&skill, &name("tdd"));
    assert_eq!(code(&output), Some(ErrorCode::PathChanged));
}

#[test]
fn run_for_an_unlisted_name_fails_invalid_arguments() {
    let skill = tool(Fake::mapping(BTreeMap::new()));
    let output = run_of(&skill, &name("nope"));
    assert_eq!(code(&output), Some(ErrorCode::InvalidArguments));
    assert!(text_of(&output).contains("nope"));
    assert_eq!(output.control, None);
}

#[test]
fn a_file_deleted_after_effects_fails_io_failed() {
    let held = TempDir::new("fiber-skill-deleted");
    let root = held.path().canonicalize().unwrap();
    let path = write_skill(&root.join("tdd"), "Body.\n");
    let skill = tool(Fake::mapping(BTreeMap::from([(
        "tdd".into(),
        path.clone(),
    )])));
    let arguments = name("tdd");
    effects_of(&skill, &arguments);
    std::fs::remove_file(&path).unwrap();
    assert_eq!(code(&run_of(&skill, &arguments)), Some(ErrorCode::IoFailed));
}

#[test]
fn a_failing_body_maps_to_its_error_code() {
    // A scripted read failure still needs the judged entry `effects`
    // records: point the name at a real file first.
    let held = TempDir::new("fiber-skill-body-error");
    let root = held.path().canonicalize().unwrap();
    let path = write_skill(&root.join("tdd"), "Body.\n");
    let invalid = tool(Fake {
        files: BTreeMap::from([("tdd".into(), path.clone())]),
        errors: BTreeMap::from([("tdd".into(), SkillRead::Invalid)]),
        panic_body: false,
    });
    let arguments = name("tdd");
    effects_of(&invalid, &arguments);
    assert_eq!(
        code(&run_of(&invalid, &arguments)),
        Some(ErrorCode::InvalidArguments)
    );
    let io = tool(Fake {
        files: BTreeMap::from([("tdd".into(), path)]),
        errors: BTreeMap::from([(
            "tdd".into(),
            SkillRead::Io("Could not read skill /w: gone.".into()),
        )]),
        panic_body: false,
    });
    effects_of(&io, &arguments);
    assert_eq!(code(&run_of(&io, &arguments)), Some(ErrorCode::IoFailed));
}

#[test]
fn run_with_no_name_fails_invalid_arguments() {
    let skill = tool(Fake::mapping(BTreeMap::new()));
    assert_eq!(
        code(&run_of(&skill, &args(json!({})))),
        Some(ErrorCode::InvalidArguments)
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_skill_whose_path_is_not_utf8_declares_nothing_and_fails() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let held = TempDir::new("fiber-skill-non-utf8");
    let root = held.path().canonicalize().unwrap();
    let dir = root.join(Path::new(OsStr::from_bytes(b"sk\xffill")));
    let path = write_skill(&dir, "Body.\n");
    let fake = Fake {
        files: BTreeMap::from([("weird".into(), path)]),
        errors: BTreeMap::new(),
        panic_body: true,
    };
    let skill = tool(fake);
    let declared = effects_of(&skill, &name("weird"));
    assert!(declared.declared.effects.is_empty());
    assert_eq!(declared.declared.paths, None);
    assert_eq!(
        code(&run_of(&skill, &name("weird"))),
        Some(ErrorCode::InvalidArguments)
    );
}

#[test]
fn the_guidelines_name_skill() {
    let skill = tool(Fake::mapping(BTreeMap::new()));
    let guidelines = skill.guidelines().unwrap();
    assert!(guidelines.contains("`skill`"), "{guidelines}");
}

#[test]
fn the_bound_is_the_default() {
    let skill = tool(Fake::mapping(BTreeMap::new()));
    assert_eq!(skill.bound(), Bound::DEFAULT);
}
