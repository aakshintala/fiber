//! Tests for `/skills` through the seam: each person file's own
//! `skills.disabled` and the file a switch writes (`docs/tui.md`,
//! "Swapped views"; `docs/configuration.md`, "When Fiber writes"). The
//! switch's edit rule lives in `config` (`config::switch_skill`); one
//! wiring test here proves the seam calls through.

use std::fs;

use serde_json::{Value, json};
use tui::{Configure, SwitchScope};

use super::Seam;
use crate::configure::tests::{Dirs, write};

/// The seam's `skills.disabled` for `workspace`.
fn disabled(seam: &Seam, workspace: &std::path::Path) -> tui::SkillsDisabled {
    seam.skills_disabled(workspace)
        .unwrap_or_else(|e| panic!("disabled: {e}"))
}

fn read(file: &std::path::Path) -> Value {
    serde_json::from_str(&fs::read_to_string(file).unwrap_or_else(|e| panic!("read: {e}")))
        .unwrap_or_else(|e| panic!("json: {e}"))
}

#[test]
fn skills_disabled_reads_each_layer_alone() {
    let dirs = Dirs::new();
    let workspace = dirs.workspace("one");
    write(
        &dirs.home().join("config.json"),
        r#"{"skills": {"disabled": ["a"]}}"#,
    );
    write(
        &dirs.project_file(&workspace),
        r#"{"skills": {"disabled": ["b"]}}"#,
    );
    write(
        &workspace.join(".fiber/config.json"),
        r#"{"skills": {"disabled": ["c"]}}"#,
    );
    let seam = Seam::new(dirs.home());
    let found = disabled(&seam, &workspace);
    assert_eq!(found.everywhere, vec!["a".to_owned()]);
    assert_eq!(found.project, vec!["b".to_owned()]);
}

#[test]
fn no_file_reads_two_empty_lists() {
    let dirs = Dirs::new();
    let workspace = dirs.workspace("one");
    let seam = Seam::new(dirs.home());
    let found = disabled(&seam, &workspace);
    assert!(found.everywhere.is_empty());
    assert!(found.project.is_empty());
}

#[test]
fn skills_disabled_reads_the_session_workspaces_project() {
    let dirs = Dirs::new();
    let (one, two) = (dirs.workspace("one"), dirs.workspace("two"));
    assert_ne!(dirs.project_file(&one), dirs.project_file(&two));
    write(
        &dirs.project_file(&one),
        r#"{"skills": {"disabled": ["a"]}}"#,
    );
    write(
        &dirs.project_file(&two),
        r#"{"skills": {"disabled": ["b"]}}"#,
    );
    let seam = Seam::new(dirs.home());
    assert_eq!(disabled(&seam, &one).project, vec!["a".to_owned()]);
    assert_eq!(disabled(&seam, &two).project, vec!["b".to_owned()]);
}

#[test]
fn switching_off_everywhere_writes_only_the_global_file() {
    let dirs = Dirs::new();
    let workspace = dirs.workspace("one");
    let seam = Seam::new(dirs.home());
    seam.switch_skill(&workspace, "a", SwitchScope::Everywhere, false)
        .unwrap_or_else(|e| panic!("switch: {e}"));
    assert_eq!(
        read(&dirs.home().join("config.json"))["skills"]["disabled"],
        json!(["a"])
    );
    assert!(!dirs.project_file(&workspace).exists());
}

const TEXT_LIMIT: usize = 64 * 1024;

/// `size` `a` bytes as a skill file, read through the seam.
fn skill_text(seam: &Seam, file: &std::path::Path) -> Result<String, tui::ConfigureError> {
    seam.skill_text(file)
}

#[test]
fn skill_text_cuts_at_64_kib() {
    let dirs = Dirs::new();
    let seam = Seam::new(dirs.home());
    for (size, cut) in [
        (TEXT_LIMIT - 1, false),
        (TEXT_LIMIT, false),
        (TEXT_LIMIT + 1, true),
        (TEXT_LIMIT + 2, true),
    ] {
        let file = dirs.workspace("one").join(format!("skill-{size}.md"));
        write(&file, &"a".repeat(size));
        let text = skill_text(&seam, &file).unwrap_or_else(|e| panic!("text: {e}"));
        if cut {
            assert_eq!(text, format!("{}…", "a".repeat(TEXT_LIMIT)));
        } else {
            assert_eq!(text, "a".repeat(size));
            assert!(!text.ends_with('…'));
        }
    }
}

#[test]
fn a_character_across_the_limit_is_left_out() {
    let dirs = Dirs::new();
    let file = dirs.workspace("one").join("SKILL.md");
    let mut bytes = vec![b'a'; TEXT_LIMIT - 1];
    bytes.extend_from_slice("é".as_bytes());
    bytes.extend_from_slice(&[b'a'; 100]);
    std::fs::write(&file, &bytes).unwrap_or_else(|e| panic!("write: {e}"));
    let seam = Seam::new(dirs.home());
    let text = skill_text(&seam, &file).unwrap_or_else(|e| panic!("text: {e}"));
    assert_eq!(text, format!("{}…", "a".repeat(TEXT_LIMIT - 1)));
}

#[test]
fn a_missing_file_is_an_error_naming_it() {
    let dirs = Dirs::new();
    let file = dirs.workspace("one").join("SKILL.md");
    let seam = Seam::new(dirs.home());
    let error = skill_text(&seam, &file).unwrap_err();
    assert!(
        error.message.contains(&file.display().to_string()),
        "{error:?}"
    );
}

#[test]
fn invalid_utf8_before_the_limit_is_an_error() {
    let dirs = Dirs::new();
    let file = dirs.workspace("one").join("SKILL.md");
    let mut bytes = vec![b'a'; 70_000];
    bytes[100] = 0xFF;
    std::fs::write(&file, &bytes).unwrap_or_else(|e| panic!("write: {e}"));
    let seam = Seam::new(dirs.home());
    assert!(skill_text(&seam, &file).is_err());
}

#[test]
fn an_incomplete_character_at_the_real_end_is_an_error() {
    let dirs = Dirs::new();
    let seam = Seam::new(dirs.home());
    for size in [10, TEXT_LIMIT - 1] {
        let file = dirs.workspace("one").join(format!("skill-{size}.md"));
        let mut bytes = vec![b'a'; size];
        bytes.push(0xC3);
        std::fs::write(&file, &bytes).unwrap_or_else(|e| panic!("write: {e}"));
        assert!(skill_text(&seam, &file).is_err(), "size {size}");
    }
}
