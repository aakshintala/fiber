//! Tests for `/tools`: the rows by source, the two switches, the answer
//! matching its command, and the column fit (`docs/tui.md`, "Swapped
//! views"; `docs/tools.md`, "Seeing the tools").

use std::path::Path;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::{Act, Ctx, Tools, on, size_text};
use crate::configure::{SwitchScope, ToolGroup, ToolLists, ToolSwitches};
use crate::configure_fake::Fake;
use crate::keys::{Edit, Key};
use crate::swapped::{Ink, List, Spot, render};
use crate::text::width;
use contract::events::{ToolInfo, ToolSource, ToolState};

const ID: &str = "c_1";

/// One declared tool.
fn info(
    name: &str,
    source: ToolSource,
    state: ToolState,
    bytes: u64,
    tokens: Option<u64>,
) -> ToolInfo {
    ToolInfo {
        name: name.to_owned(),
        source,
        state,
        bytes,
        tokens,
    }
}

fn builtin(name: &str) -> ToolInfo {
    info(name, ToolSource::Builtin, ToolState::Full, 100, None)
}

fn mcp(name: &str, server: &str, tool: &str) -> ToolInfo {
    info(
        name,
        ToolSource::Mcp {
            server: server.to_owned(),
            tool: tool.to_owned(),
        },
        ToolState::Deferred,
        460,
        None,
    )
}

fn extension(name: &str, extension: &str) -> ToolInfo {
    info(
        name,
        ToolSource::Extension {
            extension: extension.to_owned(),
        },
        ToolState::Full,
        100,
        None,
    )
}

/// A seam answering `switches`.
fn fake_with(switches: Vec<ToolSwitches>) -> Fake {
    let fake = Fake::new(Vec::new());
    if let Ok(mut known) = fake.switches.lock() {
        *known = switches;
    }
    fake
}

fn lists(disabled: &[&str]) -> ToolLists {
    ToolLists {
        enabled: None,
        disabled: disabled.iter().map(|name| (*name).to_owned()).collect(),
    }
}

fn group_switches(group: ToolGroup, project: &[&str], everywhere: &[&str]) -> ToolSwitches {
    ToolSwitches {
        group,
        project: lists(project),
        everywhere: lists(everywhere),
    }
}

fn ctx(fake: &Fake, usage: Option<u64>) -> Ctx<'_> {
    Ctx {
        seam: fake,
        workspace: Path::new("/w"),
        height: 22,
        width: 80,
        usage,
    }
}

/// The view over `fake` with `infos` answered.
fn opened(fake: &Fake, infos: &[ToolInfo], usage: Option<u64>) -> Tools {
    let context = ctx(fake, usage);
    let mut tools = Tools::open(ID.to_owned(), &context);
    tools.answered(ID, infos, &context);
    tools
}

/// The frame at the width the tests draw in.
fn frame(tools: &Tools) -> crate::swapped::Frame {
    tools.frame(80, None)
}

/// Every row's cells joined.
fn names(tools: &Tools) -> Vec<String> {
    frame(tools)
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

/// The tool rows' names: the first cell up to the first ` · `.
fn tool_names(tools: &Tools) -> Vec<String> {
    frame(tools)
        .rows
        .into_iter()
        .filter_map(|cells| {
            cells
                .first()
                .filter(|(_, _, ink)| *ink != Ink::Heading)
                .map(|(text, _, _)| text.split(" · ").next().unwrap_or("").to_owned())
                .filter(|name| !name.is_empty())
        })
        .collect()
}

/// The selected row's index.
fn selected(tools: &Tools) -> usize {
    tools.list.selected()
}

#[test]
fn before_the_answer_it_says_reading_and_has_no_rows() {
    let fake = Fake::new(Vec::new());
    let tools = Tools::open(ID.to_owned(), &ctx(&fake, None));
    let frame = frame(&tools);
    assert!(frame.rows.is_empty());
    assert_eq!(frame.below, vec!["Reading the tools…".to_owned()]);
    assert_eq!(frame.title, "Tools");
}

#[test]
fn each_tool_sits_under_its_own_sources_heading() {
    let fake = Fake::new(Vec::new());
    let tools = opened(
        &fake,
        &[
            mcp("mcp__b__zeta", "b", "zeta"),
            extension("t", "y"),
            builtin("write"),
            mcp("mcp__a__x", "a", "x"),
            builtin("read"),
            extension("u", "c"),
        ],
        None,
    );
    let mut under: Vec<(String, String)> = Vec::new();
    let mut heading = String::new();
    for cells in frame(&tools).rows {
        let Some((text, _, ink)) = cells.first() else {
            continue;
        };
        if *ink == Ink::Heading {
            heading = text.clone();
        } else {
            let name = text.split(" · ").next().unwrap_or("");
            under.push((heading.clone(), name.to_owned()));
        }
    }
    under.sort();
    let want = [
        ("Built-in", "read"),
        ("Built-in", "write"),
        ("Extension · c", "u"),
        ("Extension · y", "t"),
        ("MCP · a", "x"),
        ("MCP · b", "zeta"),
    ];
    let want: Vec<(String, String)> = want
        .iter()
        .map(|(heading, name)| ((*heading).to_owned(), (*name).to_owned()))
        .collect();
    assert_eq!(under, want);
}

#[test]
fn an_mcp_row_shows_the_servers_own_name() {
    let fake = Fake::new(Vec::new());
    let tools = opened(
        &fake,
        &[mcp("mcp__s__abc_1f2e", "s", "abc_long_name")],
        None,
    );
    assert_eq!(tool_names(&tools), vec!["abc_long_name"]);
}

#[test]
fn size_is_tokens_when_given_else_bytes() {
    let fake = Fake::new(Vec::new());
    let tools = opened(
        &fake,
        &[
            info("a", ToolSource::Builtin, ToolState::Full, 460, Some(1840)),
            info("b", ToolSource::Builtin, ToolState::Full, 1234, None),
        ],
        None,
    );
    let rows = names(&tools);
    assert_eq!(rows[1], "a · full · ~1.8k");
    assert_eq!(rows[2], "b · full · 1,234 bytes");
}

#[test]
fn a_token_size_reads_without_its_unit_or_a_zero_decimal() {
    for (tokens, said) in [
        (800, "~800"),
        (999, "~999"),
        (1000, "~1k"),
        (1049, "~1k"),
        (1050, "~1.1k"),
        (9949, "~9.9k"),
        (9950, "~10k"),
        (1200, "~1.2k"),
        (1, "~1"),
        (1_500_000, "~1.5M"),
        (2_000_000, "~2M"),
    ] {
        let tool = info("a", ToolSource::Builtin, ToolState::Full, 1, Some(tokens));
        assert_eq!(size_text(&tool), said, "{tokens}");
    }
}

#[test]
fn each_state_has_its_word() {
    let fake = Fake::new(Vec::new());
    let tools = opened(
        &fake,
        &[
            info("a", ToolSource::Builtin, ToolState::Full, 1, None),
            info("b", ToolSource::Builtin, ToolState::Deferred, 1, None),
            info("c", ToolSource::Builtin, ToolState::Loaded, 1, None),
        ],
        None,
    );
    let rows = names(&tools);
    assert!(rows[1].starts_with("a · full"), "{rows:?}");
    assert!(rows[2].starts_with("b · deferred"), "{rows:?}");
    assert!(rows[3].starts_with("c · loaded"), "{rows:?}");
}

#[test]
fn built_in_rows_have_no_switch() {
    let fake = fake_with(Vec::new());
    let mut tools = opened(&fake, &[builtin("read")], None);
    let frame = frame(&tools);
    assert_eq!(frame.rows.len(), 2);
    assert_eq!(frame.rows[1].len(), 1);
    assert!(!frame.rows[1][0].0.contains('['), "{frame:?}");
    tools.key(&Key::Down, &ctx(&fake, None));
    tools.key(&Key::Char(' '), &ctx(&fake, None));
    assert!(fake.switched.lock().unwrap().is_empty());
}

#[test]
fn a_switch_is_on_unless_the_lists_say_off() {
    let on = |enabled: Option<Vec<String>>, disabled: Vec<String>| {
        on(&ToolLists { enabled, disabled }, "t")
    };
    assert!(on(None, Vec::new()));
    assert!(!on(None, vec!["t".to_owned()]));
    assert!(!on(Some(Vec::new()), Vec::new()));
    assert!(!on(Some(vec!["t".to_owned()]), vec!["t".to_owned()]));
    assert!(on(Some(vec!["t".to_owned()]), Vec::new()));
}

#[test]
fn a_tool_row_reads_as_the_doc_writes_it() {
    let fake = fake_with(vec![group_switches(
        ToolGroup::Extension("memory".to_owned()),
        &["notes"],
        &[],
    )]);
    let tools = opened(&fake, &[extension("notes", "memory")], None);
    let cells = &frame(&tools).rows[1];
    let kinds: Vec<(&str, Option<Spot>, Ink)> = cells
        .iter()
        .map(|(text, spot, ink)| (text.as_str(), *spot, *ink))
        .collect();
    assert_eq!(
        kinds,
        [
            ("notes · full · 100 bytes · this project", None, Ink::Plain),
            ("›", None, Ink::Heading),
            ("[off]", Some(Spot::Switch { row: 1, at: 0 }), Ink::Muted),
            (" · everywhere", None, Ink::Plain),
            (" ", None, Ink::Plain),
            ("[on]", Some(Spot::Switch { row: 1, at: 1 }), Ink::Heading),
        ]
    );
}

#[test]
fn this_project_reads_the_project_lists_and_everywhere_the_global() {
    let fake = fake_with(vec![group_switches(
        ToolGroup::Mcp("gh".to_owned()),
        &["t"],
        &[],
    )]);
    let tools = opened(&fake, &[mcp("mcp__gh__t", "gh", "t")], None);
    let frame = frame(&tools);
    assert_eq!(frame.rows.len(), 2);
    assert_eq!(frame.rows[1][2].0, "[off]");
    assert_eq!(frame.rows[1][5].0, "[on]");
}

#[test]
fn space_switches_the_focused_layer_off_then_on() {
    let fake = fake_with(vec![group_switches(
        ToolGroup::Mcp("gh".to_owned()),
        &[],
        &[],
    )]);
    let mut tools = opened(&fake, &[mcp("mcp__gh__t", "gh", "t")], None);
    tools.key(&Key::Char(' '), &ctx(&fake, None));
    tools.key(&Key::Char(' '), &ctx(&fake, None));
    assert_eq!(
        fake.switched.lock().unwrap().clone(),
        vec![
            (
                ToolGroup::Mcp("gh".to_owned()),
                "t".to_owned(),
                SwitchScope::Project,
                false
            ),
            (
                ToolGroup::Mcp("gh".to_owned()),
                "t".to_owned(),
                SwitchScope::Project,
                true
            ),
        ]
    );
}

#[test]
fn left_and_right_choose_the_switch_and_stop_at_each_end() {
    let fake = fake_with(vec![group_switches(
        ToolGroup::Mcp("gh".to_owned()),
        &[],
        &[],
    )]);
    let mut tools = opened(&fake, &[mcp("mcp__gh__t", "gh", "t")], None);
    for _ in 0..3 {
        tools.edit_key(&Edit::Right);
    }
    tools.key(&Key::Char(' '), &ctx(&fake, None));
    for _ in 0..3 {
        tools.edit_key(&Edit::Left);
    }
    tools.key(&Key::Char(' '), &ctx(&fake, None));
    let switched = fake.switched.lock().unwrap().clone();
    assert_eq!(switched.len(), 2);
    assert_eq!(switched[0].2, SwitchScope::Everywhere);
    assert_eq!(switched[1].2, SwitchScope::Project);
}

/// The mark cells of the tool row at `row`: the project switch's and the
/// everywhere switch's.
fn marks(tools: &Tools, row: usize) -> (String, String) {
    let cells = &frame(tools).rows[row];
    (cells[1].0.clone(), cells[4].0.clone())
}

#[test]
fn a_bold_chevron_marks_the_focused_switch_of_the_focused_row_only() {
    let fake = fake_with(vec![group_switches(
        ToolGroup::Extension("e".to_owned()),
        &[],
        &[],
    )]);
    let mut tools = opened(&fake, &[extension("t", "e"), extension("u", "e")], None);
    // Rows: heading, t, u. The first tool row is focused, on this project.
    assert_eq!(marks(&tools, 1), ("›".to_owned(), " ".to_owned()));
    assert_eq!(frame(&tools).rows[1][1].2, Ink::Heading);
    // The other row has no mark.
    assert_eq!(marks(&tools, 2), (" ".to_owned(), " ".to_owned()));
    // → moves the mark to everywhere, ← moves it back.
    tools.edit_key(&Edit::Right);
    assert_eq!(marks(&tools, 1), (" ".to_owned(), "›".to_owned()));
    assert_eq!(frame(&tools).rows[1][4].2, Ink::Heading);
    tools.edit_key(&Edit::Left);
    assert_eq!(marks(&tools, 1), ("›".to_owned(), " ".to_owned()));
    // ↓ moves the focus to the next row, keeping the switch.
    tools.edit_key(&Edit::Right);
    tools.key(&Key::Down, &ctx(&fake, None));
    assert_eq!(marks(&tools, 1), (" ".to_owned(), " ".to_owned()));
    assert_eq!(marks(&tools, 2), (" ".to_owned(), "›".to_owned()));
}

#[test]
fn the_mark_takes_the_place_of_the_space_so_columns_do_not_move() {
    let fake = fake_with(vec![group_switches(
        ToolGroup::Extension("e".to_owned()),
        &[],
        &[],
    )]);
    let mut tools = opened(&fake, &[extension("t", "e"), extension("u", "e")], None);
    // The columns before each switch's word, on the focused row and on
    // the other one.
    let at = |tools: &Tools, row: usize, switch: usize| -> usize {
        frame(tools).rows[row][..switch]
            .iter()
            .map(|(text, _, _)| width_of(text))
            .sum()
    };
    for _ in 0..2 {
        for switch in [2, 5] {
            assert_eq!(at(&tools, 1, switch), at(&tools, 2, switch), "{switch}");
        }
        tools.edit_key(&Edit::Right);
    }
}

#[test]
fn a_click_on_a_switch_selects_its_row_and_switches_it() {
    let fake = fake_with(vec![group_switches(
        ToolGroup::Extension("e".to_owned()),
        &[],
        &[],
    )]);
    let mut tools = opened(
        &fake,
        &[builtin("a"), builtin("b"), extension("t", "e")],
        None,
    );
    // Heading, a, b, a blank row, the extension's heading, then `t`.
    tools.click(Spot::Switch { row: 5, at: 1 }, &ctx(&fake, None));
    assert_eq!(
        fake.switched.lock().unwrap().clone(),
        vec![(
            ToolGroup::Extension("e".to_owned()),
            "t".to_owned(),
            SwitchScope::Everywhere,
            false
        )]
    );
    assert_eq!(frame(&tools).rows.len(), 6);
    assert_eq!(frame(&tools).rows[5][5].0, "[off]");
}

/// The rows that carry a switch: the ones a click or a key can select.
fn taking(tools: &Tools) -> Vec<usize> {
    frame(tools)
        .rows
        .iter()
        .enumerate()
        .filter(|(_, cells)| cells.iter().any(|cell| cell.1.is_some()))
        .map(|(at, _)| at)
        .collect()
}

/// The name on the selected row.
fn selected_name(tools: &Tools) -> String {
    names(tools)[selected(tools)]
        .split(" · ")
        .next()
        .unwrap_or("")
        .to_owned()
}

#[test]
fn a_click_on_a_row_that_takes_no_switch_selects_nothing() {
    let fake = fake_with(vec![group_switches(
        ToolGroup::Extension("e".to_owned()),
        &[],
        &[],
    )]);
    let mut tools = opened(
        &fake,
        &[builtin("a"), extension("t", "e"), extension("u", "e")],
        None,
    );
    let takes = taking(&tools);
    assert_eq!(takes.len(), 2);
    let on = selected(&tools);
    assert!(takes.contains(&on));
    // Every other row: headings, the built-in row, blank rows.
    for row in (0..frame(&tools).rows.len()).filter(|row| !takes.contains(row)) {
        tools.click(Spot::Row(row), &ctx(&fake, None));
        assert_eq!(selected(&tools), on, "row {row}");
    }
    let other = takes.iter().copied().find(|row| *row != on).unwrap();
    tools.click(Spot::Row(other), &ctx(&fake, None));
    assert_eq!(selected(&tools), other);
}

#[test]
fn up_and_down_move_only_between_the_rows_that_take_a_switch() {
    let fake = fake_with(vec![
        group_switches(ToolGroup::Extension("e".to_owned()), &[], &[]),
        group_switches(ToolGroup::Mcp("m".to_owned()), &[], &[]),
    ]);
    let mut tools = opened(
        &fake,
        &[
            builtin("a"),
            extension("t", "e"),
            mcp("mcp__m__x", "m", "x"),
        ],
        None,
    );
    // The selection starts on a tool with a switch and steps to the
    // other one, never to a heading, the built-in row or a blank row.
    let first = selected_name(&tools);
    assert!(["t", "x"].contains(&first.as_str()), "{first}");
    tools.key(&Key::Down, &ctx(&fake, None));
    let second = selected_name(&tools);
    assert!(["t", "x"].contains(&second.as_str()), "{second}");
    assert_ne!(first, second);
    // At the end Down stays; Up returns, and at the start Up stays.
    tools.key(&Key::Down, &ctx(&fake, None));
    assert_eq!(selected_name(&tools), second);
    tools.key(&Key::Up, &ctx(&fake, None));
    assert_eq!(selected_name(&tools), first);
    tools.key(&Key::Up, &ctx(&fake, None));
    assert_eq!(selected_name(&tools), first);
}

#[test]
fn the_frame_marks_a_focus_only_when_a_row_takes_a_switch() {
    let fake = fake_with(vec![group_switches(
        ToolGroup::Extension("e".to_owned()),
        &[],
        &[],
    )]);
    let with = opened(&fake, &[builtin("a"), extension("t", "e")], None);
    assert!(frame(&with).list.is_gutter());
    assert_eq!(frame(&with).list.selected(), 4);
    let without = opened(&fake, &[builtin("a")], None);
    assert!(frame(&without).list.is_gutter());
    assert_ne!(frame(&without).list.selected(), 0);
    assert_eq!(without.list, List::gutter());
}

#[test]
fn a_view_with_no_switch_row_still_scrolls() {
    let fake = Fake::new(Vec::new());
    let mut tools = opened(&fake, &[builtin("a"), builtin("b")], None);
    tools.key(&Key::Down, &ctx(&fake, None));
    assert_eq!(selected(&tools), 1);
}

/// The view over forty MCP tools, taller than its 20-row window.
fn tall() -> (Fake, Tools) {
    let fake = fake_with(vec![group_switches(
        ToolGroup::Mcp("s".to_owned()),
        &[],
        &[],
    )]);
    let infos: Vec<ToolInfo> = (0..40)
        .map(|n| mcp(&format!("mcp__s__t{n:02}"), "s", &format!("t{n:02}")))
        .collect();
    let tools = opened(&fake, &infos, None);
    (fake, tools)
}

#[test]
fn keys_scroll_the_window_to_keep_the_selection_in_view() {
    let (fake, mut tools) = tall();
    // The heading is row 0, the first tool row 1; the window is 20 rows.
    assert_eq!((selected(&tools), tools.list.top()), (1, 0));
    for _ in 0..30 {
        tools.key(&Key::Down, &ctx(&fake, None));
    }
    assert_eq!((selected(&tools), tools.list.top()), (31, 12));
}

#[test]
fn a_click_scrolls_the_window_to_keep_the_row_in_view() {
    let (fake, mut tools) = tall();
    tools.click(Spot::Row(38), &ctx(&fake, None));
    assert_eq!((selected(&tools), tools.list.top()), (38, 19));
    tools.click(Spot::Switch { row: 40, at: 0 }, &ctx(&fake, None));
    assert_eq!((selected(&tools), tools.list.top()), (40, 21));
}

#[test]
fn space_on_a_heading_does_nothing() {
    let fake = fake_with(Vec::new());
    let mut tools = opened(&fake, &[builtin("read")], None);
    tools.key(&Key::Char(' '), &ctx(&fake, None));
    assert!(fake.switched.lock().unwrap().is_empty());
    assert!(frame(&tools).below.is_empty());
}

#[test]
fn a_disabled_tool_the_answer_lacks_is_an_off_row() {
    let fake = fake_with(vec![group_switches(
        ToolGroup::Mcp("m".to_owned()),
        &["gone"],
        &[],
    )]);
    let tools = opened(&fake, &[], None);
    assert_eq!(tool_names(&tools), vec!["gone"]);
    let rows = names(&tools);
    assert_eq!(rows[1], "gone · off · this project›[off] · everywhere [on]");
    let frame = frame(&tools);
    assert!(matches!(
        frame.rows[1][2].1,
        Some(Spot::Switch { row: 1, at: 0 })
    ));
    assert!(matches!(
        frame.rows[1][5].1,
        Some(Spot::Switch { row: 1, at: 1 })
    ));
}

#[test]
fn two_off_names_in_one_group_are_both_shown() {
    let fake = fake_with(vec![group_switches(
        ToolGroup::Mcp("m".to_owned()),
        &["gone", "missing"],
        &[],
    )]);
    let tools = opened(&fake, &[], None);
    let mut shown = tool_names(&tools);
    shown.sort();
    assert_eq!(shown, ["gone", "missing"]);
}

#[test]
fn a_name_in_both_lists_is_one_row() {
    let fake = fake_with(vec![group_switches(
        ToolGroup::Mcp("m".to_owned()),
        &["t"],
        &["t"],
    )]);
    let tools = opened(&fake, &[], None);
    assert_eq!(tool_names(&tools), vec!["t"]);
}

#[test]
fn a_declared_tool_is_not_doubled_by_its_list() {
    let fake = fake_with(vec![group_switches(
        ToolGroup::Mcp("m".to_owned()),
        &["t"],
        &[],
    )]);
    let tools = opened(&fake, &[mcp("mcp__m__t", "m", "t")], None);
    assert_eq!(tool_names(&tools), vec!["t"]);
    let rows = names(&tools);
    assert!(rows[1].contains("deferred"), "{rows:?}");
}

#[test]
fn an_off_row_stays_after_it_is_switched_on() {
    let fake = fake_with(vec![group_switches(
        ToolGroup::Mcp("m".to_owned()),
        &["gone"],
        &[],
    )]);
    let mut tools = opened(&fake, &[], None);
    tools.key(&Key::Char(' '), &ctx(&fake, None));
    assert_eq!(
        fake.switched.lock().unwrap().clone(),
        vec![(
            ToolGroup::Mcp("m".to_owned()),
            "gone".to_owned(),
            SwitchScope::Project,
            true
        )]
    );
    assert_eq!(tool_names(&tools), vec!["gone"]);
}

#[test]
fn the_last_row_is_the_reload_cost_in_every_state() {
    let fake = fake_with(vec![group_switches(
        ToolGroup::Mcp("gh".to_owned()),
        &[],
        &[],
    )]);
    let context = ctx(&fake, None);
    let mut tools = Tools::open(ID.to_owned(), &context);
    assert_eq!(
        tools.frame(80, None).footer,
        "a change rebuilds the cache on reload"
    );
    tools.answered(ID, &[mcp("mcp__gh__t", "gh", "t")], &context);
    assert_eq!(
        tools.frame(80, Some("~84k tokens")).footer,
        "a change rebuilds the cache (~84k tokens) on reload"
    );
    assert_eq!(
        tools.frame(80, Some("~84k tokens · $0.31")).footer,
        "a change rebuilds the cache (~84k tokens · $0.31) on reload"
    );
    tools.key(&Key::Char(' '), &context);
    assert_eq!(
        tools.frame(80, None).footer,
        "a change rebuilds the cache on reload"
    );
    let mut rejected = Tools::open(ID.to_owned(), &context);
    rejected.rejected(ID, "busy");
    assert_eq!(
        rejected.frame(80, None).footer,
        "a change rebuilds the cache on reload"
    );
}

#[test]
fn a_successful_switch_says_nothing_below() {
    let fake = fake_with(vec![group_switches(
        ToolGroup::Mcp("gh".to_owned()),
        &[],
        &[],
    )]);
    let mut tools = opened(&fake, &[mcp("mcp__gh__t", "gh", "t")], Some(180_000));
    tools.key(&Key::Char(' '), &ctx(&fake, Some(180_000)));
    assert!(frame(&tools).below.is_empty());
    assert_eq!(frame(&tools).rows[1][2].0, "[off]");
}

#[test]
fn moving_clears_a_failed_switchs_message() {
    let fake = fake_with(vec![group_switches(
        ToolGroup::Mcp("gh".to_owned()),
        &[],
        &[],
    )]);
    if let Ok(mut refuse) = fake.refuse.lock() {
        *refuse = Some("config.json: not JSON".to_owned());
    }
    let mut tools = opened(
        &fake,
        &[mcp("mcp__gh__a", "gh", "a"), mcp("mcp__gh__b", "gh", "b")],
        None,
    );
    tools.key(&Key::Char(' '), &ctx(&fake, None));
    assert!(!frame(&tools).below.is_empty());
    tools.key(&Key::Down, &ctx(&fake, None));
    assert!(frame(&tools).below.is_empty());
}

#[test]
fn a_failed_switch_shows_its_message_and_keeps_the_rows() {
    let fake = fake_with(vec![group_switches(
        ToolGroup::Mcp("gh".to_owned()),
        &[],
        &[],
    )]);
    if let Ok(mut refuse) = fake.refuse.lock() {
        *refuse = Some("config.json: not JSON".to_owned());
    }
    let mut tools = opened(&fake, &[mcp("mcp__gh__t", "gh", "t")], None);
    tools.key(&Key::Char(' '), &ctx(&fake, None));
    let frame = frame(&tools);
    assert_eq!(frame.below, vec!["config.json: not JSON".to_owned()]);
    assert_eq!(tool_names(&tools), vec!["t"]);
    assert_eq!(frame.rows[1][2].0, "[on]");
}

#[test]
fn a_switch_that_wrote_then_failed_shows_what_the_files_hold() {
    let fake = fake_with(vec![group_switches(
        ToolGroup::Mcp("gh".to_owned()),
        &[],
        &[],
    )]);
    if let Ok(mut fail) = fake.fail_after_write.lock() {
        *fail = true;
    }
    let mut tools = opened(&fake, &[mcp("mcp__gh__t", "gh", "t")], None);
    tools.key(&Key::Char(' '), &ctx(&fake, None));
    let frame = frame(&tools);
    assert_eq!(frame.rows[1][2].0, "[off]");
    assert!(!frame.below.is_empty());
}

/// The view over one `deferred` MCP tool named `name`, both switches off,
/// so its row is as wide as it gets.
fn widest_row(name: &str) -> Tools {
    let fake = fake_with(vec![group_switches(
        ToolGroup::Mcp("s".to_owned()),
        &[name],
        &[name],
    )]);
    opened(&fake, &[mcp("mcp__s__x", "s", name)], None)
}

/// The row's width, gutter included.
fn row_width(tools: &Tools, width: usize) -> usize {
    2 + tools.frame(width, None).rows[1]
        .iter()
        .map(|(text, _, _)| width_of(text))
        .sum::<usize>()
}

fn width_of(text: &str) -> usize {
    width(text)
}

#[test]
fn a_name_past_its_room_is_cut_so_the_switches_stay_in_view() {
    // ` · deferred · 460 bytes` and both switches at their widest take 63
    // columns; the gutter takes 2; 15 are left for the name at width 80.
    for (len, cut) in [(14, false), (15, false), (16, true)] {
        let name = "e".repeat(len);
        let tools = widest_row(&name);
        let cells = &tools.frame(80, None).rows[1];
        assert_eq!(cut, cells[0].0.contains('…'), "{len}");
        // The row never passes the width, and a name that fits is whole.
        assert!(row_width(&tools, 80) <= 80, "{len}");
        if !cut {
            assert!(cells[0].0.starts_with(&name), "{len}");
        } else {
            assert!(cells[0].0.starts_with(&format!("{}…", "e".repeat(14))));
        }
    }
    // At the room the row fills the width exactly.
    assert_eq!(row_width(&widest_row(&"e".repeat(15)), 80), 80);
    assert_eq!(row_width(&widest_row(&"e".repeat(14)), 80), 79);
    assert_eq!(row_width(&widest_row(&"e".repeat(16)), 80), 80);
}

#[test]
fn a_wide_name_is_cut_by_columns_and_keeps_both_switches_in_view() {
    let name = "\u{3042}".repeat(10);
    let tools = widest_row(&name);
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    render(&tools.frame(80, None), area, &mut buf, &mut targets);
    assert!(row_width(&tools, 80) <= 80);
    for at in 0..2 {
        let target = targets
            .iter()
            .find(|target| target.id == crate::mouse::TargetId::View(Spot::Switch { row: 1, at }))
            .expect("a switch target");
        assert!(target.rect.right() <= 80, "switch {at}");
        assert!(target.rect.width > 0, "switch {at}");
    }
}

#[test]
fn a_narrow_width_clips_without_panicking() {
    for width in [0, 1, 10, 40, 65] {
        let tools = widest_row(&"e".repeat(30));
        let cells = &tools.frame(width, None).rows[1];
        assert_eq!(cells.len(), 6, "width {width}");
        let area = Rect::new(0, 0, u16::try_from(width).unwrap_or(0), 5);
        render(
            &tools.frame(width, None),
            area,
            &mut Buffer::empty(area),
            &mut Vec::new(),
        );
    }
}

#[test]
fn only_the_answer_to_its_id_is_read() {
    let fake = Fake::new(Vec::new());
    let context = ctx(&fake, None);
    let mut tools = Tools::open(ID.to_owned(), &context);
    tools.answered("c_2", &[builtin("read")], &context);
    assert!(frame(&tools).rows.is_empty());
    tools.answered(ID, &[builtin("read")], &context);
    assert_eq!(tool_names(&tools), vec!["read"]);
}

#[test]
fn a_rejection_shows_its_message_only_for_its_id() {
    let fake = Fake::new(Vec::new());
    let context = ctx(&fake, None);
    let mut tools = Tools::open(ID.to_owned(), &context);
    tools.rejected("c_2", "busy");
    assert_eq!(frame(&tools).below, vec!["Reading the tools…".to_owned()]);
    tools.rejected(ID, "busy");
    assert_eq!(frame(&tools).below, vec!["busy".to_owned()]);
}

#[test]
fn esc_closes() {
    let fake = Fake::new(Vec::new());
    let mut tools = opened(&fake, &[builtin("read")], None);
    assert!(matches!(
        tools.key(&Key::Esc, &ctx(&fake, None)),
        Act::Close
    ));
}

#[test]
fn tools_80x24() {
    let fake = fake_with(vec![
        group_switches(ToolGroup::Extension("memory".to_owned()), &[], &[]),
        group_switches(ToolGroup::Mcp("docs".to_owned()), &[], &["gone"]),
    ]);
    let tools = opened(
        &fake,
        &[
            builtin("read"),
            extension("notes", "memory"),
            info(
                "mcp__docs__search",
                ToolSource::Mcp {
                    server: "docs".to_owned(),
                    tool: "search".to_owned(),
                },
                ToolState::Deferred,
                460,
                Some(460),
            ),
        ],
        None,
    );
    // The contract: each source's bold heading, with its tool rows under
    // it, found by content. The focus mark is dropped from the text, since
    // which row holds the focus is not part of it.
    let mut under: Vec<(String, String)> = Vec::new();
    let mut heading = String::new();
    for cells in tools.frame(80, None).rows {
        let Some((first, _, ink)) = cells.first() else {
            continue;
        };
        if *ink == Ink::Heading {
            heading = first.clone();
        } else {
            let row: String = cells.iter().map(|(text, _, _)| text.as_str()).collect();
            under.push((heading.clone(), row.replace('›', " ")));
        }
    }
    under.sort();
    let want = [
        ("Built-in", "read · full · 100 bytes"),
        (
            "Extension · memory",
            "notes · full · 100 bytes · this project [on] · everywhere [on]",
        ),
        (
            "MCP · docs",
            "gone · off · this project [on] · everywhere [off]",
        ),
        (
            "MCP · docs",
            "search · deferred · ~460 · this project [on] · everywhere [on]",
        ),
    ];
    let want: Vec<(String, String)> = want
        .iter()
        .map(|(heading, row)| ((*heading).to_owned(), (*row).to_owned()))
        .collect();
    assert_eq!(under, want);
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    render(
        &tools.frame(80, Some("~84k tokens · $0.31")),
        area,
        &mut buf,
        &mut Vec::new(),
    );
    let text: Vec<String> = (0..24)
        .map(|y| {
            (0..80)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect();
    // The snapshot pins the look only: the blank row between sources is
    // the prototype's (Ruling 17), which the docs leave open.
    insta::assert_snapshot!("tools_80x24", text.join("\n"));
}
