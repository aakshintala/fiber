//! Tests for `/skills`: the rows in discovery order with source and
//! visibility, the two switches writing `skills.disabled`, and the answer
//! matching its command (`docs/tui.md`, "Swapped views";
//! `docs/system-prompt.md`, "Skills").

use std::path::{Path, PathBuf};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::{Skills, Visibility, visibility};
use crate::configure::{SkillsDisabled, SwitchScope};
use crate::configure_fake::{Fake, SkillSwitched};
use crate::keys::{Edit, Key};
use crate::mouse::{Target, TargetId};
use crate::settings_view::{Act, Ctx};
use crate::swapped::{Spot, render, rows_height};
use contract::events::{SkillInfo, SkillSource};

const ID: &str = "c_1";

/// One discovered skill, invocable and shadowing nothing.
fn skill(name: &str, description: &str, path: &str, source: SkillSource) -> SkillInfo {
    SkillInfo {
        name: name.to_owned(),
        description: description.to_owned(),
        path: path.to_owned(),
        source,
        extension: None,
        model_invocable: true,
        disabled: false,
        shadows: Vec::new(),
        shadowed_by: None,
    }
}

fn repository(name: &str) -> SkillInfo {
    skill(
        name,
        "Test first.",
        &format!("/w/.fiber/skills/{name}/SKILL.md"),
        SkillSource::Repository,
    )
}

fn personal(name: &str) -> SkillInfo {
    skill(
        name,
        "Mine.",
        &format!("/home/skills/{name}/SKILL.md"),
        SkillSource::Personal,
    )
}

/// An extension skill from `extension` that the model cannot load.
fn extension(name: &str, extension: &str) -> SkillInfo {
    let mut skill = skill(
        name,
        "Remember.",
        &format!("/ext/{extension}/skills/{name}/SKILL.md"),
        SkillSource::Extension,
    );
    skill.extension = Some(extension.to_owned());
    skill.model_invocable = false;
    skill
}

/// A built-in skill.
fn builtin(name: &str) -> SkillInfo {
    skill(
        name,
        "Built in.",
        &format!("/fiber/docs/skills/{name}/SKILL.md"),
        SkillSource::Builtin,
    )
}

/// A seam answering `off` for `skills.disabled`.
fn fake_with(off: SkillsDisabled) -> Fake {
    let fake = Fake::new(Vec::new());
    if let Ok(mut lists) = fake.skills_off.lock() {
        *lists = Ok(off);
    }
    fake
}

fn off(project: &[&str], everywhere: &[&str]) -> SkillsDisabled {
    SkillsDisabled {
        project: project.iter().map(|name| (*name).to_owned()).collect(),
        everywhere: everywhere.iter().map(|name| (*name).to_owned()).collect(),
    }
}

fn ctx(fake: &Fake) -> Ctx<'_> {
    Ctx {
        seam: fake,
        workspace: Path::new("/w"),
        height: 22,
        width: 80,
        usage: None,
    }
}

/// The view over `fake` with `infos` answered.
fn opened(fake: &Fake, infos: &[SkillInfo]) -> Skills {
    let context = ctx(fake);
    let mut skills = Skills::open(ID.to_owned(), &context);
    skills.answered(ID, infos, &context);
    skills
}

/// Every row's cells joined.
fn rows(skills: &Skills, height: usize) -> Vec<String> {
    skills
        .frame(height)
        .rows
        .into_iter()
        .map(|cells| {
            cells
                .into_iter()
                .map(|(text, _)| text)
                .collect::<Vec<_>>()
                .join("")
        })
        .collect()
}

#[test]
fn before_the_answer_it_says_reading_and_has_only_the_header() {
    let fake = Fake::new(Vec::new());
    let skills = Skills::open(ID.to_owned(), &ctx(&fake));
    let frame = skills.frame(22);
    assert_eq!(frame.rows.len(), 1);
    assert_eq!(frame.below, vec!["Reading the skills…".to_owned()]);
    assert_eq!(frame.title, "Skills");
}

#[test]
fn rows_keep_the_answers_order_with_source_and_visibility() {
    let fake = fake_with(off(&[], &[]));
    let skills = opened(
        &fake,
        &[
            repository("tdd"),
            personal("mine"),
            extension("recall", "memory"),
            builtin("built"),
        ],
    );
    let shown = rows(&skills, 22);
    assert_eq!(shown.len(), 5);
    assert!(shown[0].contains("skill"), "{shown:?}");
    assert!(shown[1].contains("tdd"), "{shown:?}");
    assert!(shown[1].contains("repository"), "{shown:?}");
    assert!(shown[1].contains("model sees it"), "{shown:?}");
    assert!(shown[2].contains("personal"), "{shown:?}");
    assert!(shown[3].contains("extension memory"), "{shown:?}");
    assert!(shown[3].contains("person only"), "{shown:?}");
    assert!(shown[4].contains("built in"), "{shown:?}");
}

#[test]
fn visibility_by_lists_shadowing_and_header() {
    // `off` wins over shadowing, shadowing over the header: off lists,
    // then shadowed, then invocable.
    for (invocable, listed, shadowed_by, want) in [
        (true, &[][..], false, Visibility::Model),
        (true, &["tdd"][..], false, Visibility::Off),
        (false, &[][..], false, Visibility::PersonOnly),
        (true, &[][..], true, Visibility::Shadowed),
        (true, &["tdd"][..], true, Visibility::Off),
    ] {
        let mut skill = repository("tdd");
        skill.model_invocable = invocable;
        if shadowed_by {
            skill.shadowed_by = Some("/home/skills/tdd/SKILL.md".to_owned());
        }
        for scope in [SwitchScope::Project, SwitchScope::Everywhere] {
            let off = match scope {
                SwitchScope::Project => off(listed, &[]),
                SwitchScope::Everywhere => off(&[], listed),
            };
            assert_eq!(visibility(&skill, &off), want, "{scope:?}");
        }
    }
    let fake = fake_with(off(&[], &[]));
    let skills = opened(&fake, &[repository("tdd")]);
    let shown = rows(&skills, 22);
    assert!(shown[0].contains("visibility"), "{shown:?}");
}

#[test]
fn the_answers_disabled_is_ignored() {
    let fake = fake_with(off(&[], &[]));
    let mut skill = repository("tdd");
    skill.disabled = true;
    let skills = opened(&fake, &[skill]);
    let shown = rows(&skills, 22);
    assert!(shown[1].contains("model sees it"), "{shown:?}");
}

#[test]
fn the_selected_rows_description_and_shadows_show_below() {
    let winner = "/w/.fiber/skills/tdd/SKILL.md";
    let shadowed = "/home/skills/tdd/SKILL.md";
    let fake = fake_with(off(&[], &[]));
    let mut one = repository("tdd");
    one.shadows = vec![shadowed.to_owned()];
    let mut skills = opened(&fake, &[one]);
    skills.key(&Key::Down, &ctx(&fake));
    assert_eq!(
        skills.frame(22).below,
        vec!["Test first.".to_owned(), format!("shadows {shadowed}")]
    );
    let many: Vec<String> = (0..3)
        .map(|n| format!("/home/skills/{n}/SKILL.md"))
        .collect();
    let mut three = repository("tdd");
    three.shadows = many.clone();
    let mut skills = opened(&fake, &[three]);
    skills.key(&Key::Down, &ctx(&fake));
    assert_eq!(
        skills.frame(22).below,
        vec![
            "Test first.".to_owned(),
            "shadows 3 skills; Enter lists them".to_owned()
        ]
    );
    let mut loser = personal("tdd");
    loser.shadowed_by = Some(winner.to_owned());
    let mut skills = opened(&fake, &[loser]);
    skills.key(&Key::Down, &ctx(&fake));
    assert_eq!(
        skills.frame(22).below,
        vec!["Mine.".to_owned(), format!("shadowed by {winner}")]
    );
    let skills = opened(&fake, &[repository("tdd")]);
    assert!(
        skills.frame(22).below.is_empty(),
        "the header has no details"
    );
}

/// A winner shadowing `n` skills, selected.
fn shadowed_view(fake: &Fake, n: usize) -> Skills {
    let winner = "/w/.fiber/skills/tdd/SKILL.md";
    let mut infos = vec![repository("tdd")];
    let mut shadows = Vec::new();
    for index in 0..n {
        let path = format!("/home/skills/tdd-{index}/SKILL.md");
        shadows.push(path);
    }
    infos[0].shadows = shadows.clone();
    for (index, path) in shadows.iter().enumerate() {
        let mut shadowed = skill(
            &format!("tdd-{index}"),
            "Mine.",
            path,
            SkillSource::Personal,
        );
        shadowed.shadowed_by = Some(winner.to_owned());
        infos.push(shadowed);
    }
    let mut skills = opened(fake, &infos);
    skills.key(&Key::Down, &ctx(fake));
    assert_eq!(skills.frame(24).below[0], "Test first.");
    skills
}

#[test]
fn many_shadows_keep_rows_shown_and_navigable() {
    for said in [
        vec!["Reaches the model at the next turn start.".to_owned()],
        vec!["config.json: not JSON".to_owned()],
    ] {
        let fake = fake_with(off(&[], &[]));
        let mut skills = shadowed_view(&fake, 30);
        skills.said = said.clone();
        for height in [24, 6, 5, 4, 3] {
            let frame = skills.frame(height);
            assert!(rows_height(&frame, height) >= 1, "height {height}");
            let area = Rect::new(0, 0, 80, u16::try_from(height).unwrap_or(u16::MAX));
            let mut buf = Buffer::empty(area);
            let mut targets = Vec::new();
            render(&frame, area, &mut buf, &mut targets);
            assert!(
                targets
                    .iter()
                    .any(|target| matches!(target.id, TargetId::View(Spot::Row(_)))),
                "height {height}"
            );
        }
        let frame = skills.frame(5);
        assert_eq!(frame.below.len(), 2, "{frame:?}");
        assert_eq!(frame.below[1], said[0], "{frame:?}");
    }
    let fake = fake_with(off(&[], &[]));
    let mut skills = shadowed_view(&fake, 30);
    skills.said = vec!["Reaches the model at the next turn start.".to_owned()];
    let before = skills.list.selected();
    skills.key(&Key::Down, &ctx(&fake));
    assert_eq!(skills.list.selected(), before + 1);
    skills.key(&Key::Up, &ctx(&fake));
    assert_eq!(skills.list.selected(), before);
    // At a height fitting one row the frames follow the selection: the
    // navigation happens at that height, so the list scrolls to it.
    let mut skills = shadowed_view(&fake, 30);
    skills.said = vec!["Reaches the model at the next turn start.".to_owned()];
    let narrow = Ctx {
        height: 6,
        ..ctx(&fake)
    };
    skills.reread(&narrow);
    let winner = skills.list.selected();
    let frame = skills.frame(6);
    assert!(frame_has_row(&frame, 6, winner), "{frame:?}");
    skills.key(&Key::Down, &narrow);
    assert_eq!(skills.list.selected(), winner + 1);
    let frame = skills.frame(6);
    assert!(frame_has_row(&frame, 6, winner + 1), "{frame:?}");
    skills.key(&Key::Up, &narrow);
    assert_eq!(skills.list.selected(), winner);
    let frame = skills.frame(6);
    assert!(frame_has_row(&frame, 6, winner), "{frame:?}");
}

/// Whether `frame` built for `height` draws row `at` with its click
/// target: rendered at that height, so paging applies.
fn frame_has_row(frame: &crate::swapped::Frame, height: usize, at: usize) -> bool {
    let area = Rect::new(0, 0, 80, u16::try_from(height).unwrap_or(u16::MAX));
    let mut buf = Buffer::empty(area);
    let mut targets: Vec<Target> = Vec::new();
    render(frame, area, &mut buf, &mut targets);
    targets
        .iter()
        .any(|target| target.id == TargetId::View(Spot::Row(at)))
}

#[test]
fn only_the_answer_to_its_id_fills_the_view() {
    let fake = Fake::new(Vec::new());
    let context = ctx(&fake);
    let mut skills = Skills::open(ID.to_owned(), &context);
    skills.answered("c_2", &[repository("tdd")], &context);
    assert_eq!(skills.frame(22).rows.len(), 1);
    skills.answered(ID, &[repository("tdd")], &context);
    assert_eq!(skills.frame(22).rows.len(), 2);
}

#[test]
fn a_rejection_shows_its_message_only_for_its_id() {
    let fake = Fake::new(Vec::new());
    let context = ctx(&fake);
    let mut skills = Skills::open(ID.to_owned(), &context);
    skills.rejected("c_2", "busy");
    assert_eq!(
        skills.frame(22).below,
        vec!["Reading the skills…".to_owned()]
    );
    skills.rejected(ID, "busy");
    assert_eq!(skills.frame(22).below, vec!["busy".to_owned()]);
}

#[test]
fn a_failed_lists_read_says_why_and_the_rows_still_show() {
    let fake = Fake::new(Vec::new());
    if let Ok(mut lists) = fake.skills_off.lock() {
        *lists = Err(crate::configure::ConfigureError {
            code: contract::ErrorCode::IoFailed,
            message: "config.json: not JSON".to_owned(),
        });
    }
    let context = ctx(&fake);
    let mut skills = Skills::open(ID.to_owned(), &context);
    skills.answered(ID, &[repository("tdd")], &context);
    let frame = skills.frame(22);
    assert_eq!(frame.rows.len(), 2);
    assert!(frame.below.contains(&"config.json: not JSON".to_owned()));
}

#[test]
fn esc_closes() {
    let fake = Fake::new(Vec::new());
    let mut skills = opened(&fake, &[repository("tdd")]);
    assert!(matches!(skills.key(&Key::Esc, &ctx(&fake)), Act::Close));
}

#[test]
fn selection_clamps_two_past_the_end() {
    let fake = Fake::new(Vec::new());
    let mut skills = opened(
        &fake,
        &[
            repository("a"),
            repository("b"),
            repository("c"),
            repository("d"),
        ],
    );
    skills.click(Spot::Row(7), &ctx(&fake));
    assert_eq!(skills.list.selected(), 4);
    let context = Ctx {
        height: 4,
        ..ctx(&fake)
    };
    skills.click(Spot::Row(0), &context);
    for _ in 0..3 {
        skills.key(&Key::Down, &context);
    }
    assert_eq!(skills.list.selected(), 3);
    skills.key(&Key::PageDown, &context);
    assert_eq!(skills.list.selected(), 4);
}

#[test]
fn space_toggles_project_then_everywhere_independently() {
    let fake = fake_with(off(&[], &[]));
    let mut skills = opened(&fake, &[repository("tdd")]);
    skills.key(&Key::Down, &ctx(&fake));
    skills.key(&Key::Char(' '), &ctx(&fake));
    skills.edit_key(&Edit::Right);
    skills.key(&Key::Char(' '), &ctx(&fake));
    skills.key(&Key::Char(' '), &ctx(&fake));
    assert_eq!(
        switched(&fake),
        vec![
            (
                Path::new("/w").to_path_buf(),
                "tdd".to_owned(),
                SwitchScope::Project,
                false
            ),
            (
                Path::new("/w").to_path_buf(),
                "tdd".to_owned(),
                SwitchScope::Everywhere,
                false
            ),
            (
                Path::new("/w").to_path_buf(),
                "tdd".to_owned(),
                SwitchScope::Everywhere,
                true
            ),
        ]
    );
}

/// The switches asked for so far.
fn switched(fake: &Fake) -> Vec<(std::path::PathBuf, String, SwitchScope, bool)> {
    fake.skill_switched
        .lock()
        .map(|switched| {
            switched
                .iter()
                .map(|(workspace, name, scope, on)| (workspace.clone(), name.clone(), *scope, *on))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn a_switch_click_selects_focuses_and_flips() {
    for (edit, row, at, scope) in [
        (Edit::Left, 1, 1, SwitchScope::Everywhere),
        (Edit::Right, 2, 0, SwitchScope::Project),
    ] {
        let fake = fake_with(off(&[], &[]));
        let mut skills = opened(&fake, &[repository("a"), repository("b")]);
        skills.edit_key(&edit);
        skills.click(Spot::Switch { row, at }, &ctx(&fake));
        let asked: Vec<SkillSwitched> = switched(&fake);
        assert_eq!(asked.len(), 1, "{edit:?}");
        assert_eq!(asked[0].2, scope, "{edit:?}");
        assert_eq!(skills.list.selected(), row, "{edit:?}");
        let frame = skills.frame(22);
        let focused = if at == 0 { 1 } else { 3 };
        assert!(
            frame.rows[row][focused].0.contains('›'),
            "{edit:?} {frame:?}"
        );
    }
}

#[test]
fn right_then_left_then_space_writes_this_project() {
    let fake = fake_with(off(&[], &[]));
    let mut skills = opened(&fake, &[repository("tdd")]);
    skills.key(&Key::Down, &ctx(&fake));
    skills.edit_key(&Edit::Right);
    skills.edit_key(&Edit::Left);
    skills.key(&Key::Char(' '), &ctx(&fake));
    assert_eq!(
        switched(&fake),
        vec![(
            Path::new("/w").to_path_buf(),
            "tdd".to_owned(),
            SwitchScope::Project,
            false
        )]
    );
    assert!(
        fake.skills_off
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .everywhere
            .is_empty()
    );
}

#[test]
fn a_switch_rereads_and_says_next_turn_start() {
    let fake = fake_with(off(&[], &[]));
    let mut skills = opened(&fake, &[repository("tdd")]);
    skills.key(&Key::Down, &ctx(&fake));
    skills.key(&Key::Char(' '), &ctx(&fake));
    let frame = skills.frame(22);
    assert_eq!(
        frame.below,
        vec![
            "Test first.".to_owned(),
            "Reaches the model at the next turn start.".to_owned()
        ]
    );
    assert!(frame.rows[1][1].0.contains("[ ]"), "{frame:?}");
    assert!(frame.rows[1][3].0.contains("[x]"), "{frame:?}");
}

#[test]
fn a_failed_switch_rereads_and_shows_the_message() {
    let fake = fake_with(off(&[], &[]));
    if let Ok(mut fail) = fake.fail_after_write.lock() {
        *fail = true;
    }
    let mut skills = opened(&fake, &[repository("tdd")]);
    skills.key(&Key::Down, &ctx(&fake));
    skills.key(&Key::Char(' '), &ctx(&fake));
    let frame = skills.frame(22);
    assert_eq!(
        frame.below,
        vec![
            "Test first.".to_owned(),
            "the write landed, then syncing failed".to_owned()
        ]
    );
    // The write landed before the error, so the row shows it.
    assert!(frame.rows[1][1].0.contains("[ ]"), "{frame:?}");
}

#[test]
fn space_on_the_header_or_an_empty_view_writes_nothing() {
    let fake = fake_with(off(&[], &[]));
    let mut skills = opened(&fake, &[repository("tdd")]);
    skills.key(&Key::Char(' '), &ctx(&fake));
    assert!(switched(&fake).is_empty());
    let mut skills = opened(&fake, &[]);
    skills.key(&Key::Char(' '), &ctx(&fake));
    assert!(switched(&fake).is_empty());
}

#[test]
fn a_shadowed_rows_switch_writes_its_name() {
    let winner = "/w/.fiber/skills/tdd/SKILL.md";
    let fake = fake_with(off(&[], &[]));
    let mut shadowed = personal("tdd");
    shadowed.shadowed_by = Some(winner.to_owned());
    let mut win = repository("tdd");
    win.shadows = vec!["/home/skills/tdd/SKILL.md".to_owned()];
    let mut skills = opened(&fake, &[win, shadowed]);
    // The shadowed row is index 2, past the header and the winner.
    skills.click(Spot::Row(2), &ctx(&fake));
    skills.key(&Key::Char(' '), &ctx(&fake));
    assert_eq!(
        switched(&fake),
        vec![(
            Path::new("/w").to_path_buf(),
            "tdd".to_owned(),
            SwitchScope::Project,
            false
        )]
    );
    let frame = skills.frame(22);
    assert!(frame.rows[1][1].0.contains("[ ]"), "{frame:?}");
    assert!(frame.rows[2][1].0.contains("[ ]"), "{frame:?}");
}

#[test]
fn other_edits_do_nothing() {
    let fake = fake_with(off(&[], &[]));
    let mut skills = opened(&fake, &[repository("tdd")]);
    for edit in [
        Edit::ShiftEnter,
        Edit::CtrlJ,
        Edit::WordLeft,
        Edit::WordRight,
        Edit::DeleteWord,
        Edit::LineStart,
        Edit::LineEnd,
        Edit::Delete,
        Edit::Paste("x".to_owned()),
    ] {
        skills.edit_key(&edit);
    }
    skills.key(&Key::Down, &ctx(&fake));
    skills.key(&Key::Char(' '), &ctx(&fake));
    assert_eq!(switched(&fake).len(), 1);
    assert_eq!(switched(&fake)[0].2, SwitchScope::Project);
}

/// A seam answering `body` for every skill text.
fn text_fake(body: &str) -> Fake {
    let fake = fake_with(off(&[], &[]));
    if let Ok(mut texts) = fake.texts.lock() {
        *texts = Ok(body.to_owned());
    }
    fake
}

/// The view over `fake` with `infos` answered and the selected row's
/// text open.
fn entered(fake: &Fake, infos: &[SkillInfo]) -> Skills {
    let mut skills = opened(fake, infos);
    skills.key(&Key::Down, &ctx(fake));
    skills.key(&Key::Enter, &ctx(fake));
    skills
}

#[test]
fn enter_shows_the_text_and_esc_returns_then_closes() {
    let fake = text_fake("Test first.\nSecond line.");
    let mut skills = opened(&fake, &[repository("tdd")]);
    skills.key(&Key::Down, &ctx(&fake));
    assert!(matches!(skills.key(&Key::Enter, &ctx(&fake)), Act::Stay));
    let frame = skills.frame(22);
    assert_eq!(frame.title, "Skill tdd");
    assert!(
        frame.rows.iter().any(|row| row[0].0 == "Test first."),
        "{frame:?}"
    );
    assert!(matches!(skills.key(&Key::Esc, &ctx(&fake)), Act::Stay));
    assert_eq!(skills.frame(22).title, "Skills");
    assert_eq!(skills.list.selected(), 1);
    assert!(matches!(skills.key(&Key::Esc, &ctx(&fake)), Act::Close));
}

#[test]
fn enter_on_a_read_error_stays_on_the_rows_with_the_message() {
    let fake = fake_with(off(&[], &[]));
    if let Ok(mut texts) = fake.texts.lock() {
        *texts = Err(crate::configure::ConfigureError {
            code: contract::ErrorCode::IoFailed,
            message: "Could not read /tdd/SKILL.md: gone.".to_owned(),
        });
    }
    let mut skills = opened(&fake, &[repository("tdd")]);
    skills.key(&Key::Down, &ctx(&fake));
    assert!(matches!(skills.key(&Key::Enter, &ctx(&fake)), Act::Stay));
    let frame = skills.frame(22);
    assert_eq!(frame.title, "Skills");
    assert_eq!(frame.rows.len(), 2);
    assert!(
        frame
            .below
            .contains(&"Could not read /tdd/SKILL.md: gone.".to_owned()),
        "{frame:?}"
    );
}

#[test]
fn enter_lists_every_shadow_path_before_the_text() {
    let fake = text_fake("The text.");
    let skills = entered(&fake, shadowed_infos(30).as_slice());
    let frame = skills.frame(22);
    assert_eq!(frame.rows.len(), 32);
    for (index, row) in frame.rows.iter().take(30).enumerate() {
        assert_eq!(
            row[0].0,
            format!("shadows /home/skills/tdd-{index}/SKILL.md")
        );
    }
    // No shadow path dropped: the block, a blank line, then the text.
    assert_eq!(frame.rows[30][0].0, "");
    assert_eq!(frame.rows[31][0].0, "The text.");
}

/// A winner shadowing `n` skills, without opening the view.
fn shadowed_infos(n: usize) -> Vec<SkillInfo> {
    let mut infos = vec![repository("tdd")];
    let mut shadows = Vec::new();
    for index in 0..n {
        shadows.push(format!("/home/skills/tdd-{index}/SKILL.md"));
    }
    infos[0].shadows = shadows;
    infos
}

#[test]
fn the_text_wraps_to_the_width() {
    let fake = text_fake(&"e".repeat(50));
    let narrow = Ctx {
        width: 20,
        ..ctx(&fake)
    };
    let mut skills = opened(&fake, &[repository("tdd")]);
    skills.key(&Key::Down, &narrow);
    skills.key(&Key::Enter, &narrow);
    assert_eq!(skills.frame(22).rows.len(), 3);
}

#[test]
fn a_new_width_rewraps_on_the_next_key() {
    let fake = text_fake(&"e".repeat(50));
    let narrow = Ctx {
        width: 20,
        ..ctx(&fake)
    };
    let mut skills = opened(&fake, &[repository("tdd")]);
    skills.key(&Key::Down, &narrow);
    skills.key(&Key::Enter, &narrow);
    assert_eq!(skills.frame(22).rows.len(), 3);
    let wide = Ctx {
        width: 40,
        ..ctx(&fake)
    };
    skills.key(&Key::Down, &wide);
    assert_eq!(skills.frame(22).rows.len(), 2);
}

#[test]
fn text_scrolls_and_clamps_two_past_the_end() {
    let body: Vec<String> = (0..10).map(|n| format!("line {n}")).collect();
    let fake = text_fake(&body.join("\n"));
    let mut skills = entered(&fake, &[repository("tdd")]);
    assert_eq!(skills.frame(22).rows.len(), 10);
    skills.click(Spot::Row(11), &ctx(&fake));
    assert_eq!(selected_line(&skills), 9);
    skills.key(&Key::PageDown, &ctx(&fake));
    assert_eq!(selected_line(&skills), 9);
    skills.key(&Key::Up, &ctx(&fake));
    assert_eq!(selected_line(&skills), 8);
}

/// The text pane's selected line.
fn selected_line(skills: &Skills) -> usize {
    skills.frame(22).list.selected()
}

#[test]
fn ctrl_g_opens_the_selected_skill_md() {
    let fake = text_fake("Test first.");
    let mut skills = opened(&fake, &[repository("tdd")]);
    skills.key(&Key::Down, &ctx(&fake));
    let file = PathBuf::from("/w/.fiber/skills/tdd/SKILL.md");
    assert!(matches!(
        skills.key(&Key::CtrlG, &ctx(&fake)),
        Act::Open(path) if path == file
    ));
    skills.key(&Key::Enter, &ctx(&fake));
    assert!(matches!(
        skills.key(&Key::CtrlG, &ctx(&fake)),
        Act::Open(path) if path == file
    ));
}

#[test]
fn ctrl_g_with_no_rows_opens_nothing() {
    let fake = Fake::new(Vec::new());
    let mut skills = opened(&fake, &[]);
    assert!(matches!(skills.key(&Key::CtrlG, &ctx(&fake)), Act::Stay));
}

#[test]
fn shortening_the_open_text_clamps_the_pane_and_keeps_the_row() {
    let body: Vec<String> = (0..10).map(|n| format!("line {n}")).collect();
    let fake = text_fake(&body.join("\n"));
    let mut skills = entered(&fake, &[repository("tdd")]);
    let narrow = Ctx {
        height: 6,
        ..ctx(&fake)
    };
    // Two past the last line scrolls to it.
    skills.click(Spot::Row(11), &narrow);
    assert_eq!(selected_line(&skills), 9);
    assert_eq!(skills.list.selected(), 1);
    // Ctrl+G shortens the file to one line; the return re-reads it.
    if let Ok(mut texts) = fake.texts.lock() {
        *texts = Ok("only".to_owned());
    }
    skills.reread(&narrow);
    assert_eq!(skills.list.selected(), 1, "the skill row is unchanged");
    assert_eq!(selected_line(&skills), 0);
    let frame = skills.frame(6);
    assert_eq!(frame.rows.len(), 1);
    assert_eq!(frame.rows[0][0].0, "only");
    assert!(frame_has_row(&frame, 6, 0), "{frame:?}");
}

#[test]
fn skills_text_80x24() {
    let fake = text_fake("Test first, then write the test.\nA second paragraph.");
    let mut win = repository("tdd");
    win.shadows = vec!["/home/skills/tdd/SKILL.md".to_owned()];
    let mut skills = opened(&fake, &[win]);
    skills.key(&Key::Down, &ctx(&fake));
    skills.key(&Key::Enter, &ctx(&fake));
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    render(&skills.frame(24), area, &mut buf, &mut Vec::new());
    let text: Vec<String> = (0..24)
        .map(|y| {
            (0..80)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect();
    insta::assert_snapshot!("skills_text_80x24", text.join("\n"));
}

#[test]
fn skills_80x24() {
    let fake = fake_with(off(&[], &["built"]));
    let winner = "/w/.fiber/skills/tdd/SKILL.md";
    let mut win = repository("tdd");
    win.shadows = vec!["/home/skills/tdd/SKILL.md".to_owned()];
    let mut lost = personal("tdd");
    lost.shadowed_by = Some(winner.to_owned());
    let mut skills = opened(
        &fake,
        &[win, lost, extension("recall", "memory"), builtin("built")],
    );
    skills.key(&Key::Down, &ctx(&fake));
    skills.key(&Key::Down, &ctx(&fake));
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    render(&skills.frame(24), area, &mut buf, &mut Vec::new());
    let text: Vec<String> = (0..24)
        .map(|y| {
            (0..80)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect();
    insta::assert_snapshot!("skills_80x24", text.join("\n"));
}
