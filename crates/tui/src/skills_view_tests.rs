//! Tests for `/skills`: the rows in discovery order with source and
//! visibility, the two switches writing `skills.disabled`, and the answer
//! matching its command (`docs/tui.md`, "Swapped views";
//! `docs/system-prompt.md`, "Skills").

use std::path::{Path, PathBuf};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;

use super::{Skills, Visibility, visibility};
use crate::configure::{SkillsDisabled, SwitchScope};
use crate::configure_fake::{Fake, SkillSwitched};
use crate::keys::{Edit, Key};
use crate::mouse::{Target, TargetId};
use crate::settings_view::{Act, Ctx};
use crate::swapped::{Spot, render, rows_height};
use contract::events::{SkillInfo, SkillSource};

use crate::text;

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

/// A skill of `source` at `path`, with `extension` exactly when the
/// source is an extension.
fn sourced(name: &str, path: &str, source: SkillSource, extension: Option<&str>) -> SkillInfo {
    let mut info = skill(name, "Desc.", path, source);
    info.extension = extension.map(str::to_owned);
    info
}

/// The shadowed-source words for a winner shadowing `paths`.
fn shadowed_by(paths: &[String], infos: &[SkillInfo]) -> Option<Vec<String>> {
    super::shadow_sources(paths, infos)
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
fn rows(skills: &Skills, width: usize, height: usize) -> Vec<String> {
    skills
        .frame(width, height)
        .rows
        .into_iter()
        .map(|cells| {
            cells
                .into_iter()
                .map(|(text, _, _)| text)
                .collect::<Vec<_>>()
                .join("")
        })
        .collect()
}

#[test]
fn before_the_answer_it_says_reading_and_has_no_rows() {
    let fake = Fake::new(Vec::new());
    let skills = Skills::open(ID.to_owned(), &ctx(&fake));
    let frame = skills.frame(80, 22);
    assert_eq!(frame.rows.len(), 0);
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
    let shown = rows(&skills, 80, 22);
    assert_eq!(shown.len(), 4, "no header row: {shown:?}");
    assert!(shown[0].contains("tdd"), "{shown:?}");
    assert!(shown[0].contains("repo"), "{shown:?}");
    assert!(!shown[0].contains("repository"), "{shown:?}");
    assert!(shown[0].contains("seen"), "{shown:?}");
    assert!(!shown[0].contains("model sees it"), "{shown:?}");
    assert!(shown[1].contains("personal"), "{shown:?}");
    assert!(shown[2].contains("memory"), "{shown:?}");
    assert!(!shown[2].contains("extension memory"), "{shown:?}");
    assert!(shown[2].contains("hidden"), "{shown:?}");
    assert!(!shown[2].contains("person only"), "{shown:?}");
    assert!(shown[3].contains("built in"), "{shown:?}");
}

#[test]
fn visibility_by_lists_shadowing_and_invocable() {
    // `off` wins over shadowing, shadowing over the header flag: off
    // lists, then shadowed, then invocable.
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
    let shown = rows(&skills, 80, 22);
    assert!(shown[0].contains(" · seen · "), "{shown:?}");
}

#[test]
fn the_answers_disabled_is_ignored() {
    let fake = fake_with(off(&[], &[]));
    let mut skill = repository("tdd");
    skill.disabled = true;
    let skills = opened(&fake, &[skill]);
    let shown = rows(&skills, 80, 22);
    assert!(shown[0].contains("seen"), "{shown:?}");
    assert!(!shown[0].contains("model sees it"), "{shown:?}");
}

#[test]
fn off_and_shadowed_rows_read_hidden() {
    let fake = fake_with(off(&["tdd"], &[]));
    let skills = opened(&fake, &[repository("tdd")]);
    let shown = rows(&skills, 80, 22);
    assert!(shown[0].contains(" · hidden · "), "{shown:?}");
    assert!(shown[0].contains("[off]"), "{shown:?}");
    let fake = fake_with(off(&[], &[]));
    let mut shadowed = personal("tdd");
    shadowed.shadowed_by = Some("/w/.fiber/skills/tdd/SKILL.md".to_owned());
    let skills = opened(&fake, &[shadowed]);
    let shown = rows(&skills, 80, 22);
    assert!(shown[0].contains(" · hidden · "), "{shown:?}");
    assert!(!shown[0].contains("shadowed"), "{shown:?}");
}

/// Discovery fixtures for the shadow-source resolver: one skill per
/// source, all named `review`.
fn shadow_infos() -> Vec<SkillInfo> {
    vec![
        sourced(
            "review",
            "/fiber/docs/skills/review/SKILL.md",
            SkillSource::Builtin,
            None,
        ),
        sourced(
            "review",
            "/ext/review-kit/skills/review/SKILL.md",
            SkillSource::Extension,
            Some("review-kit"),
        ),
        sourced(
            "review",
            "/home/skills/review/SKILL.md",
            SkillSource::Personal,
            None,
        ),
        sourced(
            "review",
            "/w/.fiber/skills/review/SKILL.md",
            SkillSource::Repository,
            None,
        ),
    ]
}

#[test]
fn shadow_sources_names_one_source() {
    let infos = shadow_infos();
    let paths = vec!["/home/skills/review/SKILL.md".to_owned()];
    assert_eq!(
        shadowed_by(&paths, &infos),
        Some(vec!["personal".to_owned()])
    );
}

#[test]
fn shadow_sources_orders_repo_personal_extensions_then_builtin() {
    // Shuffled discovery order must not move the words: repo, personal,
    // extensions by extension name, then built in.
    let infos = shadow_infos();
    let paths = vec![
        "/fiber/docs/skills/review/SKILL.md".to_owned(),
        "/ext/review-kit/skills/review/SKILL.md".to_owned(),
        "/home/skills/review/SKILL.md".to_owned(),
        "/w/.fiber/skills/review/SKILL.md".to_owned(),
    ];
    assert_eq!(
        shadowed_by(&paths, &infos),
        Some(vec![
            "repo".to_owned(),
            "personal".to_owned(),
            "review-kit".to_owned(),
            "built in".to_owned(),
        ])
    );
}

#[test]
fn shadow_sources_orders_extensions_by_name() {
    let infos = vec![
        sourced(
            "review",
            "/ext/zeta/skills/review/SKILL.md",
            SkillSource::Extension,
            Some("zeta"),
        ),
        sourced(
            "review",
            "/ext/alpha/skills/review/SKILL.md",
            SkillSource::Extension,
            Some("alpha"),
        ),
    ];
    let paths = vec![
        "/ext/zeta/skills/review/SKILL.md".to_owned(),
        "/ext/alpha/skills/review/SKILL.md".to_owned(),
    ];
    assert_eq!(
        shadowed_by(&paths, &infos),
        Some(vec!["alpha".to_owned(), "zeta".to_owned()])
    );
}

#[test]
fn shadow_sources_lists_two_paths_from_one_source_once() {
    let infos = vec![
        sourced(
            "review",
            "/home/skills/review/SKILL.md",
            SkillSource::Personal,
            None,
        ),
        sourced(
            "review",
            "/home/.agents/skills/review/SKILL.md",
            SkillSource::Personal,
            None,
        ),
    ];
    let paths = vec![
        "/home/skills/review/SKILL.md".to_owned(),
        "/home/.agents/skills/review/SKILL.md".to_owned(),
    ];
    assert_eq!(
        shadowed_by(&paths, &infos),
        Some(vec!["personal".to_owned()])
    );
}

#[test]
fn shadow_sources_omits_the_clause_unless_every_path_resolves() {
    let infos = shadow_infos();
    assert_eq!(
        shadowed_by(&["/nowhere/review/SKILL.md".to_owned()], &infos),
        None,
        "an unresolvable path leaves out the whole clause"
    );
    assert_eq!(
        shadowed_by(
            &[
                "/home/skills/review/SKILL.md".to_owned(),
                "/nowhere/review/SKILL.md".to_owned(),
            ],
            &infos,
        ),
        None,
        "one unresolvable path drops even a resolvable list"
    );
    assert_eq!(shadowed_by(&[], &infos), None, "no shadows, no clause");
}

#[test]
fn shadow_sources_treats_a_nameless_extension_as_unresolved() {
    let infos = vec![sourced(
        "review",
        "/ext/nameless/skills/review/SKILL.md",
        SkillSource::Extension,
        None,
    )];
    assert_eq!(
        shadowed_by(&["/ext/nameless/skills/review/SKILL.md".to_owned()], &infos),
        None,
        "a malformed extension entry drops the whole clause"
    );
}

#[test]
fn colliding_extension_names_keep_their_kind() {
    // An extension named after another source keeps its kind, so the two
    // never read the same; any other extension reads as its bare name.
    for name in ["repo", "personal", "built in"] {
        let info = sourced(
            "a",
            &format!("/ext/{name}/skills/a/SKILL.md"),
            SkillSource::Extension,
            Some(name),
        );
        assert_eq!(super::source_word(&info), format!("extension {name}"));
    }
    let plain = sourced(
        "a",
        "/ext/memory/skills/a/SKILL.md",
        SkillSource::Extension,
        Some("memory"),
    );
    assert_eq!(super::source_word(&plain), "memory");
    assert_eq!(super::source_word(&repository("a")), "repo");
    assert_eq!(super::source_word(&personal("a")), "personal");
    assert_eq!(super::source_word(&builtin("a")), "built in");
}

#[test]
fn shadow_sources_lists_a_personal_source_and_a_personal_named_extension() {
    // Deduplication is by source identity, not by display word: a personal
    // skill and an extension named `personal` are two sources.
    let infos = vec![
        sourced(
            "review",
            "/home/skills/review/SKILL.md",
            SkillSource::Personal,
            None,
        ),
        sourced(
            "review",
            "/ext/personal/skills/review/SKILL.md",
            SkillSource::Extension,
            Some("personal"),
        ),
    ];
    let paths = vec![
        "/home/skills/review/SKILL.md".to_owned(),
        "/ext/personal/skills/review/SKILL.md".to_owned(),
    ];
    assert_eq!(
        shadowed_by(&paths, &infos),
        Some(vec!["personal".to_owned(), "extension personal".to_owned()])
    );
}

/// A repository winner shadowing a personal, a long-named extension and a
/// built-in skill, all named `review`.
fn long_shadowed_view(fake: &Fake) -> Skills {
    let mut win = repository("review");
    win.description = "Checks a diff.".to_owned();
    win.shadows = vec![
        "/home/skills/review/SKILL.md".to_owned(),
        "/ext/review-kit/skills/review/SKILL.md".to_owned(),
        "/fiber/docs/skills/review/SKILL.md".to_owned(),
    ];
    let infos = vec![
        win,
        sourced(
            "review",
            "/home/skills/review/SKILL.md",
            SkillSource::Personal,
            None,
        ),
        sourced(
            "review",
            "/ext/review-kit/skills/review/SKILL.md",
            SkillSource::Extension,
            Some("review-kit"),
        ),
        sourced(
            "review",
            "/fiber/docs/skills/review/SKILL.md",
            SkillSource::Builtin,
            None,
        ),
    ];
    opened(fake, &infos)
}

#[test]
fn narrow_rows_cut_the_description_then_the_name_then_the_source() {
    // `repository("review")` is `review · Test first. · repo · seen`.
    let fake = fake_with(off(&[], &[]));
    let skills = opened(&fake, &[repository("review")]);
    // Only the description gives: the name and the source stay whole
    // while the description ends in `…`.
    let row = &rows(&skills, 75, 22)[0];
    assert!(row.contains("review · Test firs… · repo · seen"), "{row:?}");
    assert_eq!(row.matches("[on]").count(), 2, "{row:?}");
    assert!(text::width(row) <= 75, "{row:?}");
    // With the description gone, the name is cut while the source stays
    // whole.
    let row = &rows(&skills, 63, 22)[0];
    assert!(row.contains("rev… ·  · repo · seen"), "{row:?}");
    assert_eq!(row.matches("[on]").count(), 2, "{row:?}");
    assert!(text::width(row) <= 63, "{row:?}");
    // Last the source itself ends in `…`; both switches are still whole.
    let row = &rows(&skills, 58, 22)[0];
    assert!(row.contains("re… · seen"), "{row:?}");
    assert!(!row.contains("repo"), "{row:?}");
    assert_eq!(row.matches("[on]").count(), 2, "{row:?}");
    assert!(text::width(row) <= 58, "{row:?}");
}

#[test]
fn a_long_shadow_list_is_cut_after_the_switches() {
    let fake = fake_with(off(&[], &[]));
    let skills = long_shadowed_view(&fake);
    let row = &rows(&skills, 100, 22)[0];
    // The clause moved past both switches, tail-cut with `…`.
    let everywhere = row.find("everywhere").expect("switches first");
    let shadows = row.find("shadows").expect("the clause");
    assert!(shadows > everywhere, "{row:?}");
    assert!(row.contains("shadows personal,"), "{row:?}");
    assert!(row.ends_with("…"), "{row:?}");
    assert_eq!(row.matches("[on]").count(), 2, "{row:?}");
    assert!(text::width(row) <= 100, "{row:?}");
}

#[test]
fn the_shadows_clause_drops_out_below_its_minimum() {
    let fake = fake_with(off(&[], &[]));
    let skills = long_shadowed_view(&fake);
    // At 67 columns the fixed fields and both switches leave exactly the
    // ` · shadows …` minimum, so the clause stays as that minimum.
    let kept = &rows(&skills, 67, 22)[0];
    assert!(kept.contains("shadows"), "{kept:?}");
    assert!(kept.ends_with("…"), "{kept:?}");
    assert_eq!(kept.matches("[on]").count(), 2, "{kept:?}");
    assert!(text::width(kept) <= 67, "{kept:?}");
    // One column narrower leaves less than the minimum, so the clause is
    // left out and the switches stay whole.
    let dropped = &rows(&skills, 66, 22)[0];
    assert!(!dropped.contains("shadows"), "{dropped:?}");
    assert_eq!(dropped.matches("[on]").count(), 2, "{dropped:?}");
    assert!(text::width(dropped) <= 66, "{dropped:?}");
}

#[test]
fn a_long_own_source_is_cut_last_with_switches_whole() {
    let fake = fake_with(off(&[], &[]));
    let mut info = sourced(
        "review",
        "/ext/a-very-long-extension-name/skills/review/SKILL.md",
        SkillSource::Extension,
        Some("a-very-long-extension-name"),
    );
    info.description = "Checks a diff.".to_owned();
    let skills = opened(&fake, &[info]);
    // The description and the name are gone before the long source is
    // touched.
    let row = &rows(&skills, 81, 22)[0];
    assert!(row.contains("a-very-long-extension-name · seen"), "{row:?}");
    assert!(!row.contains("Checks a"), "{row:?}");
    assert_eq!(row.matches("[on]").count(), 2, "{row:?}");
    assert!(text::width(row) <= 81, "{row:?}");
    // Narrower still, the source itself ends in `…`.
    let row = &rows(&skills, 70, 22)[0];
    assert!(row.contains("a-very-long-ex… · seen"), "{row:?}");
    assert_eq!(row.matches("[on]").count(), 2, "{row:?}");
    assert!(text::width(row) <= 70, "{row:?}");
}

#[test]
fn the_selected_rows_description_and_shadows_show_below() {
    let winner = "/w/.fiber/skills/tdd/SKILL.md";
    let shadowed = "/home/skills/tdd/SKILL.md";
    let fake = fake_with(off(&[], &[]));
    let mut one = repository("tdd");
    one.shadows = vec![shadowed.to_owned()];
    let skills = opened(&fake, &[one]);
    assert_eq!(
        skills.frame(80, 22).below,
        vec!["Test first.".to_owned(), format!("shadows {shadowed}")]
    );
    let many: Vec<String> = (0..3)
        .map(|n| format!("/home/skills/{n}/SKILL.md"))
        .collect();
    let mut three = repository("tdd");
    three.shadows = many.clone();
    let skills = opened(&fake, &[three]);
    assert_eq!(
        skills.frame(80, 22).below,
        vec![
            "Test first.".to_owned(),
            "shadows 3 skills; Enter lists them".to_owned()
        ]
    );
    let mut loser = personal("tdd");
    loser.shadowed_by = Some(winner.to_owned());
    let skills = opened(&fake, &[loser]);
    assert_eq!(
        skills.frame(80, 22).below,
        vec!["Mine.".to_owned(), format!("shadowed by {winner}")]
    );
    let skills = opened(&fake, &[]);
    assert!(
        skills.frame(80, 22).below.is_empty(),
        "an empty view has no details"
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
    let skills = opened(fake, &infos);
    assert_eq!(skills.frame(80, 24).below[0], "Test first.");
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
            let frame = skills.frame(80, height);
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
        let frame = skills.frame(80, 5);
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
    let frame = skills.frame(80, 6);
    assert!(frame_has_row(&frame, 6, winner), "{frame:?}");
    skills.key(&Key::Down, &narrow);
    assert_eq!(skills.list.selected(), winner + 1);
    let frame = skills.frame(80, 6);
    assert!(frame_has_row(&frame, 6, winner + 1), "{frame:?}");
    skills.key(&Key::Up, &narrow);
    assert_eq!(skills.list.selected(), winner);
    let frame = skills.frame(80, 6);
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
    assert_eq!(skills.frame(80, 22).rows.len(), 0);
    skills.answered(ID, &[repository("tdd")], &context);
    assert_eq!(skills.frame(80, 22).rows.len(), 1);
}

#[test]
fn a_rejection_shows_its_message_only_for_its_id() {
    let fake = Fake::new(Vec::new());
    let context = ctx(&fake);
    let mut skills = Skills::open(ID.to_owned(), &context);
    skills.rejected("c_2", "busy");
    assert_eq!(
        skills.frame(80, 22).below,
        vec!["Reading the skills…".to_owned()]
    );
    skills.rejected(ID, "busy");
    assert_eq!(skills.frame(80, 22).below, vec!["busy".to_owned()]);
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
    let frame = skills.frame(80, 22);
    assert_eq!(frame.rows.len(), 1);
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
    skills.click(Spot::Row(6), &ctx(&fake));
    assert_eq!(skills.list.selected(), 3);
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
    assert_eq!(skills.list.selected(), 3);
}

#[test]
fn space_toggles_project_then_everywhere_independently() {
    let fake = fake_with(off(&[], &[]));
    let mut skills = opened(&fake, &[repository("tdd")]);
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
        (Edit::Left, 0, 1, SwitchScope::Everywhere),
        (Edit::Right, 1, 0, SwitchScope::Project),
    ] {
        let fake = fake_with(off(&[], &[]));
        let mut skills = opened(&fake, &[repository("a"), repository("b")]);
        skills.edit_key(&edit);
        skills.click(Spot::Switch { row, at }, &ctx(&fake));
        let asked: Vec<SkillSwitched> = switched(&fake);
        assert_eq!(asked.len(), 1, "{edit:?}");
        assert_eq!(asked[0].2, scope, "{edit:?}");
        assert_eq!(skills.list.selected(), row, "{edit:?}");
        let frame = skills.frame(80, 22);
        let focused = if at == 0 { 1 } else { 4 };
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
    skills.key(&Key::Char(' '), &ctx(&fake));
    let frame = skills.frame(80, 22);
    assert_eq!(
        frame.below,
        vec![
            "Test first.".to_owned(),
            "Reaches the model at the next turn start.".to_owned()
        ]
    );
    assert!(frame.rows[0][2].0.contains("[off]"), "{frame:?}");
    assert!(frame.rows[0][5].0.contains("[on]"), "{frame:?}");
}

#[test]
fn a_failed_switch_rereads_and_shows_the_message() {
    let fake = fake_with(off(&[], &[]));
    if let Ok(mut fail) = fake.fail_after_write.lock() {
        *fail = true;
    }
    let mut skills = opened(&fake, &[repository("tdd")]);
    skills.key(&Key::Char(' '), &ctx(&fake));
    let frame = skills.frame(80, 22);
    assert_eq!(
        frame.below,
        vec![
            "Test first.".to_owned(),
            "the write landed, then syncing failed".to_owned()
        ]
    );
    // The write landed before the error, so the row shows it.
    assert!(frame.rows[0][2].0.contains("[off]"), "{frame:?}");
}

#[test]
fn space_on_an_empty_view_writes_nothing() {
    let fake = fake_with(off(&[], &[]));
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
    // The shadowed row is index 1, past the winner.
    skills.click(Spot::Row(1), &ctx(&fake));
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
    let frame = skills.frame(80, 22);
    assert!(frame.rows[0][2].0.contains("[off]"), "{frame:?}");
    assert!(frame.rows[1][2].0.contains("[off]"), "{frame:?}");
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
    skills.key(&Key::Enter, &ctx(fake));
    skills
}

#[test]
fn enter_shows_the_text_and_esc_returns_then_closes() {
    let fake = text_fake("Test first.\nSecond line.");
    let mut skills = opened(&fake, &[repository("tdd")]);
    assert!(matches!(skills.key(&Key::Enter, &ctx(&fake)), Act::Stay));
    let frame = skills.frame(80, 22);
    assert_eq!(frame.title, "Skill tdd");
    assert!(
        frame.rows.iter().any(|row| row[0].0 == "Test first."),
        "{frame:?}"
    );
    assert!(matches!(skills.key(&Key::Esc, &ctx(&fake)), Act::Stay));
    assert_eq!(skills.frame(80, 22).title, "Skills");
    assert_eq!(skills.list.selected(), 0);
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
    assert!(matches!(skills.key(&Key::Enter, &ctx(&fake)), Act::Stay));
    let frame = skills.frame(80, 22);
    assert_eq!(frame.title, "Skills");
    assert_eq!(frame.rows.len(), 1);
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
    let frame = skills.frame(80, 22);
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

#[test]
fn enter_lists_each_shadow_source_and_path_above_the_text() {
    // Shadow order is the winner's path order, not precedence order, and
    // every path gets its own line, including a repeated source.
    let fake = text_fake("The text.");
    let mut win = repository("review");
    win.shadows = vec![
        "/fiber/docs/skills/review/SKILL.md".to_owned(),
        "/ext/review-kit/skills/review/SKILL.md".to_owned(),
        "/home/skills/review/SKILL.md".to_owned(),
        "/home/other/review/SKILL.md".to_owned(),
    ];
    let infos = vec![
        win,
        sourced(
            "review",
            "/fiber/docs/skills/review/SKILL.md",
            SkillSource::Builtin,
            None,
        ),
        sourced(
            "review",
            "/ext/review-kit/skills/review/SKILL.md",
            SkillSource::Extension,
            Some("review-kit"),
        ),
        sourced(
            "review",
            "/home/skills/review/SKILL.md",
            SkillSource::Personal,
            None,
        ),
        sourced(
            "review",
            "/home/other/review/SKILL.md",
            SkillSource::Personal,
            None,
        ),
    ];
    let skills = entered(&fake, &infos);
    let frame = skills.frame(80, 22);
    assert_eq!(frame.rows.len(), 6);
    let lines: Vec<String> = frame.rows.iter().map(|row| row[0].0.clone()).collect();
    assert_eq!(
        lines,
        vec![
            "shadows built in · /fiber/docs/skills/review/SKILL.md".to_owned(),
            "shadows review-kit · /ext/review-kit/skills/review/SKILL.md".to_owned(),
            "shadows personal · /home/skills/review/SKILL.md".to_owned(),
            "shadows personal · /home/other/review/SKILL.md".to_owned(),
            String::new(),
            "The text.".to_owned(),
        ]
    );
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
    skills.key(&Key::Enter, &narrow);
    assert_eq!(skills.frame(20, 22).rows.len(), 3);
}

#[test]
fn a_new_width_rewraps_on_the_next_key() {
    let fake = text_fake(&"e".repeat(50));
    let narrow = Ctx {
        width: 20,
        ..ctx(&fake)
    };
    let mut skills = opened(&fake, &[repository("tdd")]);
    skills.key(&Key::Enter, &narrow);
    assert_eq!(skills.frame(20, 22).rows.len(), 3);
    let wide = Ctx {
        width: 40,
        ..ctx(&fake)
    };
    skills.key(&Key::Down, &wide);
    assert_eq!(skills.frame(40, 22).rows.len(), 2);
}

#[test]
fn text_scrolls_and_clamps_two_past_the_end() {
    let body: Vec<String> = (0..10).map(|n| format!("line {n}")).collect();
    let fake = text_fake(&body.join("\n"));
    let mut skills = entered(&fake, &[repository("tdd")]);
    assert_eq!(skills.frame(80, 22).rows.len(), 10);
    skills.click(Spot::Row(11), &ctx(&fake));
    assert_eq!(selected_line(&skills), 9);
    skills.key(&Key::PageDown, &ctx(&fake));
    assert_eq!(selected_line(&skills), 9);
    skills.key(&Key::Up, &ctx(&fake));
    assert_eq!(selected_line(&skills), 8);
}

/// The text pane's selected line.
fn selected_line(skills: &Skills) -> usize {
    skills.frame(80, 22).list.selected()
}

#[test]
fn ctrl_g_opens_the_selected_skill_md() {
    let fake = text_fake("Test first.");
    let mut skills = opened(&fake, &[repository("tdd")]);
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
    assert_eq!(skills.list.selected(), 0);
    // Ctrl+G shortens the file to one line; the return re-reads it.
    if let Ok(mut texts) = fake.texts.lock() {
        *texts = Ok("only".to_owned());
    }
    skills.reread(&narrow);
    assert_eq!(skills.list.selected(), 0, "the skill row is unchanged");
    assert_eq!(selected_line(&skills), 0);
    let frame = skills.frame(80, 6);
    assert_eq!(frame.rows.len(), 1);
    assert_eq!(frame.rows[0][0].0, "only");
    assert!(frame_has_row(&frame, 6, 0), "{frame:?}");
}

/// Four skills each with two details lines, so every selection shrinks
/// the frame.
fn detailed_infos() -> Vec<SkillInfo> {
    let mut infos = Vec::new();
    for name in ["a", "b", "c", "d"] {
        let mut info = repository(name);
        info.shadows = vec![format!("/home/skills/{name}/SKILL.md")];
        infos.push(info);
    }
    infos
}

#[test]
fn navigation_keeps_the_selected_row_visible_at_small_heights() {
    for height in [6usize, 5] {
        for key in [Key::PageDown, Key::Down] {
            let fake = fake_with(off(&[], &[]));
            let mut skills = opened(&fake, &detailed_infos());
            let context = Ctx {
                height,
                ..ctx(&fake)
            };
            skills.reread(&context);
            assert_eq!(skills.list.selected(), 0);
            skills.key(&key, &context);
            let selected = skills.list.selected();
            assert!(selected > 0, "height {height} key {key:?}");
            let frame = skills.frame(80, height);
            assert!(
                frame_has_row(&frame, height, selected),
                "height {height} key {key:?} selected {selected} {frame:?}"
            );
            // Two past the last row clamps to it and stays visible.
            let rows = skills.frame(80, height).rows.len();
            skills.click(Spot::Row(rows + 1), &context);
            let selected = skills.list.selected();
            assert_eq!(selected, rows - 1, "height {height} key {key:?}");
            let frame = skills.frame(80, height);
            assert!(
                frame_has_row(&frame, height, selected),
                "height {height} key {key:?} selected {selected} {frame:?}"
            );
        }
    }
}

#[test]
fn skills_text_80x24() {
    let fake = text_fake("Test first, then write the test.\nA second paragraph.");
    let mut win = repository("tdd");
    win.shadows = vec!["/home/skills/tdd/SKILL.md".to_owned()];
    let mut skills = opened(&fake, &[win]);
    skills.key(&Key::Enter, &ctx(&fake));
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    render(&skills.frame(80, 24), area, &mut buf, &mut Vec::new());
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
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    render(&skills.frame(80, 24), area, &mut buf, &mut Vec::new());
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

/// Row `y` of `buf` as text, `width` columns wide.
fn buf_row(buf: &Buffer, y: u16, width: u16) -> String {
    (0..width)
        .map(|x| buf[(x, y)].symbol().to_owned())
        .collect::<String>()
}

/// The column where `needle` starts on row `y` of `buf`, or a panic.
fn buf_col(buf: &Buffer, y: u16, width: u16, needle: &str) -> u16 {
    let row = buf_row(buf, y, width);
    let byte = row
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} on row {y}: {row:?}"));
    u16::try_from(row[..byte].chars().count()).expect("the column fits")
}

#[test]
fn initial_frame_selects_the_first_skill() {
    let winner = "/w/.fiber/skills/review/SKILL.md";
    let mut win = repository("review");
    win.description = "Checks a diff.".to_owned();
    win.shadows = vec![
        "/home/skills/review/SKILL.md".to_owned(),
        "/ext/review-kit/skills/review/SKILL.md".to_owned(),
        "/fiber/docs/skills/review/SKILL.md".to_owned(),
    ];
    let mut shadowed = personal("review");
    shadowed.shadowed_by = Some(winner.to_owned());
    let mut kit = extension("review", "review-kit");
    kit.shadowed_by = Some(winner.to_owned());
    let mut docs = builtin("review");
    docs.shadowed_by = Some(winner.to_owned());
    let fake = fake_with(off(&[], &["notes"]));
    let skills = opened(&fake, &[win, shadowed, kit, docs, repository("notes")]);
    // No header row: the first row is the first skill, selected. The
    // ruling example only fits uncut at pty width, so the frame is 160
    // columns wide.
    let frame = skills.frame(160, 22);
    assert_eq!(frame.rows.len(), 5);
    assert_eq!(frame.list.selected(), 0);
    let head: String = frame.rows[0]
        .iter()
        .map(|(text, _, _)| text.clone())
        .collect::<Vec<_>>()
        .join("");
    assert!(
        head.contains("review · Checks a diff. · repo · seen · this project"),
        "{head:?}"
    );
    // The shadows clause follows both switches, whole at this width.
    assert!(
        head.ends_with(" · shadows personal, review-kit, built in"),
        "{head:?}"
    );
    assert!(head.contains("[on]"), "{head:?}");
    assert_eq!(frame.footer, "Enter shows the text · Ctrl+G opens its file");
    assert_eq!(frame.below[0], "Checks a diff.");
    // A row with no shadows names none.
    assert!(
        !frame.rows[4]
            .iter()
            .any(|(text, _, _)| text.contains("shadows")),
        "{frame:?}"
    );
    // Only the selected row marks its focused switch.
    assert_eq!(frame.rows[0][1].0, "›");
    assert_eq!(frame.rows[1][1].0, " ", "{frame:?}");
    assert_eq!(frame.rows[1][4].0, " ", "{frame:?}");
    // The drawn grid: the `› ` gutter on the focused row only, `[on]`
    // bold, `[off]` dim, the footer dim, and no reverse video.
    let area = Rect::new(0, 0, 160, 22);
    let mut buf = Buffer::empty(area);
    render(&frame, area, &mut buf, &mut Vec::new());
    assert_eq!(buf[(0, 1)].symbol(), "›");
    assert_eq!(buf[(1, 1)].symbol(), " ");
    assert_eq!(buf[(0, 2)].symbol(), " ");
    for x in 0..160 {
        assert!(
            !buf[(x, 1)].modifier.contains(Modifier::REVERSED),
            "no bar on the focused row"
        );
    }
    let on = buf_col(&buf, 1, 160, "[on]");
    for dx in 0..4 {
        let cell = &buf[(on + dx, 1)];
        assert!(cell.modifier.contains(Modifier::BOLD), "[on] is bold");
        assert!(!cell.modifier.contains(Modifier::DIM), "[on] is not dim");
    }
    let off = buf_col(&buf, 5, 160, "[off]");
    for dx in 0..5 {
        let cell = &buf[(off + dx, 5)];
        assert!(cell.modifier.contains(Modifier::DIM), "[off] is dim");
        assert!(!cell.modifier.contains(Modifier::BOLD), "[off] is not bold");
    }
    let footer = buf_row(&buf, 21, 160);
    assert!(
        footer.contains("Enter shows the text · Ctrl+G opens its file"),
        "{footer:?}"
    );
    for x in 0..160 {
        if buf[(x, 21)].symbol() != " " {
            assert!(
                buf[(x, 21)].modifier.contains(Modifier::DIM),
                "the footer is dim"
            );
        }
    }
}

#[test]
fn an_unresolvable_shadow_path_leaves_the_row_without_a_clause() {
    let mut win = repository("review");
    win.shadows = vec!["/nowhere/review/SKILL.md".to_owned()];
    let fake = fake_with(off(&[], &[]));
    let skills = opened(&fake, &[win]);
    let frame = skills.frame(160, 22);
    assert!(
        !frame.rows[0]
            .iter()
            .any(|(text, _, _)| text.contains("shadows")),
        "{frame:?}"
    );
}

#[test]
fn resize_redraws_rows_details_and_text_at_the_new_width() {
    let description = "A description long enough to cut when the view narrows to sixty-four.";
    let fake = fake_with(off(&["b"], &[]));
    let mut a = repository("a");
    a.description = description.to_owned();
    let skills = opened(&fake, &[a, repository("b")]);
    // At full width the details read whole.
    let full = skills.frame(80, 22);
    assert_eq!(full.below[0], description);
    // At a narrower width with no intervening key, the head cuts with
    // `…` while both switches stay reachable, and the details cut.
    let narrow = skills.frame(64, 22);
    let head: String = narrow.rows[0]
        .iter()
        .map(|(text, _, _)| text.clone())
        .collect::<Vec<_>>()
        .join("");
    assert!(head.contains("…"), "{head:?}");
    assert!(head.contains("[on]"), "{head:?}");
    assert!(
        narrow.rows[1]
            .iter()
            .any(|(text, _, _)| text.contains("[off]")),
        "{narrow:?}"
    );
    assert!(narrow.below[0].len() < description.len(), "{narrow:?}");
    // The narrowed row fits its buffer, switches drawn, not clipped.
    let area = Rect::new(0, 0, 64, 22);
    let mut buf = Buffer::empty(area);
    render(&narrow, area, &mut buf, &mut Vec::new());
    let off = buf_col(&buf, 2, 64, "[off]");
    assert!(off + 5 <= 64, "the [off] switch is drawn whole");
    // The text pane rewraps at the redraw's width too.
    let text_fake = text_fake(&"e".repeat(100));
    let mut entered = opened(&text_fake, &[repository("tdd")]);
    entered.key(&Key::Enter, &ctx(&text_fake));
    assert_eq!(entered.frame(80, 22).rows.len(), 2);
    assert_eq!(entered.frame(48, 22).rows.len(), 3);
}
