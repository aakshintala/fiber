//! Tests for `/tools`: the rows by source, the two switches, the answer
//! matching its command, and the column fit (`docs/tui.md`, "Swapped
//! views"; `docs/tools.md`, "Seeing the tools").

use std::path::Path;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::{Act, Ctx, Tools, fit, on};
use crate::configure::{SwitchScope, ToolGroup, ToolLists, ToolSwitches};
use crate::configure_fake::Fake;
use crate::format::width;
use crate::keys::{Edit, Key};
use crate::swapped::{Spot, render};
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

/// Every row's cells joined.
fn names(tools: &Tools) -> Vec<String> {
    tools
        .frame(None)
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

/// The tool rows' names: the first cell trimmed.
fn tool_names(tools: &Tools) -> Vec<String> {
    tools
        .frame(None)
        .rows
        .into_iter()
        .filter_map(|cells| {
            cells
                .first()
                .map(|(text, _, _)| text.trim().to_owned())
                .filter(|name| {
                    !name.starts_with("Built-in")
                        && !name.starts_with("Extension")
                        && !name.starts_with("MCP server")
                })
        })
        .collect()
}

#[test]
fn before_the_answer_it_says_reading_and_has_no_rows() {
    let fake = Fake::new(Vec::new());
    let tools = Tools::open(ID.to_owned(), &ctx(&fake, None));
    let frame = tools.frame(None);
    assert!(frame.rows.is_empty());
    assert_eq!(frame.below, vec!["Reading the tools…".to_owned()]);
    assert_eq!(frame.title, "Tools");
}

#[test]
fn rows_group_built_in_then_extensions_then_servers_each_by_name() {
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
    assert_eq!(
        tool_names(&tools),
        vec!["read", "write", "u", "t", "x", "zeta"]
    );
    let headings: Vec<String> = names(&tools)
        .into_iter()
        .filter(|row| {
            row.starts_with("Built-in")
                || row.starts_with("Extension")
                || row.starts_with("MCP server")
        })
        .collect();
    assert_eq!(headings.len(), 5);
    assert!(headings[0].starts_with("Built-in"), "{headings:?}");
    assert!(headings[1].starts_with("Extension c"), "{headings:?}");
    assert!(headings[2].starts_with("Extension y"), "{headings:?}");
    assert!(headings[3].starts_with("MCP server a"), "{headings:?}");
    assert!(headings[4].starts_with("MCP server b"), "{headings:?}");
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
            info("b", ToolSource::Builtin, ToolState::Full, 460, None),
        ],
        None,
    );
    let rows = names(&tools);
    assert!(rows[1].contains("about 1,840 tokens"), "{rows:?}");
    assert!(rows[2].contains("460 bytes"), "{rows:?}");
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
    assert!(rows[1].contains("full"), "{rows:?}");
    assert!(rows[2].contains("deferred"), "{rows:?}");
    assert!(rows[3].contains("loaded"), "{rows:?}");
}

#[test]
fn built_in_rows_have_no_switch() {
    let fake = fake_with(Vec::new());
    let mut tools = opened(&fake, &[builtin("read")], None);
    let frame = tools.frame(None);
    assert_eq!(frame.rows.len(), 2);
    for (_, spot, _) in &frame.rows[1] {
        assert!(!matches!(spot, Some(Spot::Switch { .. })), "{frame:?}");
    }
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
fn this_project_reads_the_project_lists_and_everywhere_the_global() {
    let fake = fake_with(vec![group_switches(
        ToolGroup::Mcp("gh".to_owned()),
        &["t"],
        &[],
    )]);
    let tools = opened(&fake, &[mcp("mcp__gh__t", "gh", "t")], None);
    let frame = tools.frame(None);
    assert_eq!(frame.rows.len(), 2);
    // `[ ]` under this project, `[x]` under everywhere.
    assert!(frame.rows[1][1].0.contains("[ ]"), "{frame:?}");
    assert!(frame.rows[1][3].0.contains("[x]"), "{frame:?}");
}

#[test]
fn space_switches_the_focused_layer_off_then_on() {
    let fake = fake_with(vec![group_switches(
        ToolGroup::Mcp("gh".to_owned()),
        &[],
        &[],
    )]);
    let mut tools = opened(&fake, &[mcp("mcp__gh__t", "gh", "t")], None);
    tools.key(&Key::Down, &ctx(&fake, None));
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
    tools.key(&Key::Down, &ctx(&fake, None));
    for _ in 0..3 {
        tools.edit_key(&Edit::Right);
    }
    let frame = tools.frame(None);
    assert!(frame.rows[1][3].0.contains('›'), "{frame:?}");
    tools.key(&Key::Char(' '), &ctx(&fake, None));
    for _ in 0..3 {
        tools.edit_key(&Edit::Left);
    }
    let frame = tools.frame(None);
    assert!(frame.rows[1][1].0.contains('›'), "{frame:?}");
    tools.key(&Key::Char(' '), &ctx(&fake, None));
    let switched = fake.switched.lock().unwrap().clone();
    assert_eq!(switched.len(), 2);
    assert_eq!(switched[0].2, SwitchScope::Everywhere);
    assert_eq!(switched[1].2, SwitchScope::Project);
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
    tools.click(Spot::Switch { row: 4, at: 1 }, &ctx(&fake, None));
    assert_eq!(
        fake.switched.lock().unwrap().clone(),
        vec![(
            ToolGroup::Extension("e".to_owned()),
            "t".to_owned(),
            SwitchScope::Everywhere,
            false
        )]
    );
    assert_eq!(tools.frame(None).rows.len(), 5);
}

#[test]
fn space_on_a_heading_does_nothing() {
    let fake = fake_with(Vec::new());
    let mut tools = opened(&fake, &[builtin("read")], None);
    tools.key(&Key::Char(' '), &ctx(&fake, None));
    assert!(fake.switched.lock().unwrap().is_empty());
    assert!(tools.frame(None).below.is_empty());
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
    assert!(rows[1].contains("off"), "{rows:?}");
    assert!(!rows[1].contains("bytes"), "{rows:?}");
    let frame = tools.frame(None);
    assert!(matches!(
        frame.rows[1][1].1,
        Some(Spot::Switch { row: 1, at: 0 })
    ));
    assert!(matches!(
        frame.rows[1][3].1,
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
    assert_eq!(tool_names(&tools), ["gone", "missing"]);
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
    tools.key(&Key::Down, &ctx(&fake, None));
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
fn a_switch_says_the_reload_cost() {
    for (usage, said) in [
        (
            Some(180_000),
            "Applies on /reload, which rebuilds the cache: about 180,000 tokens.",
        ),
        (None, "Applies on each session's next /reload."),
    ] {
        let fake = fake_with(vec![group_switches(
            ToolGroup::Mcp("gh".to_owned()),
            &[],
            &[],
        )]);
        let mut tools = opened(&fake, &[mcp("mcp__gh__t", "gh", "t")], usage);
        tools.key(&Key::Down, &ctx(&fake, usage));
        tools.key(&Key::Char(' '), &ctx(&fake, usage));
        assert_eq!(tools.frame(usage).below, vec![said.to_owned()]);
    }
}

#[test]
fn moving_clears_the_switch_line() {
    let fake = fake_with(vec![group_switches(
        ToolGroup::Mcp("gh".to_owned()),
        &[],
        &[],
    )]);
    let mut tools = opened(
        &fake,
        &[mcp("mcp__gh__a", "gh", "a"), mcp("mcp__gh__b", "gh", "b")],
        None,
    );
    tools.key(&Key::Down, &ctx(&fake, None));
    tools.key(&Key::Char(' '), &ctx(&fake, None));
    assert!(!tools.frame(None).below.is_empty());
    tools.key(&Key::Down, &ctx(&fake, None));
    assert!(tools.frame(None).below.is_empty());
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
    tools.key(&Key::Down, &ctx(&fake, None));
    tools.key(&Key::Char(' '), &ctx(&fake, None));
    let frame = tools.frame(None);
    assert_eq!(frame.below, vec!["config.json: not JSON".to_owned()]);
    assert_eq!(tool_names(&tools), vec!["t"]);
    assert!(frame.rows[1][1].0.contains("[x]"), "{frame:?}");
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
    tools.key(&Key::Down, &ctx(&fake, None));
    tools.key(&Key::Char(' '), &ctx(&fake, None));
    let frame = tools.frame(None);
    assert!(frame.rows[1][1].0.contains("[ ]"), "{frame:?}");
    assert!(frame.below != vec!["Applies on each session's next /reload.".to_owned()]);
}

#[test]
fn a_name_past_its_column_is_cut() {
    let fake = Fake::new(Vec::new());
    let tools = opened(
        &fake,
        &[
            builtin(&"e".repeat(20)),
            builtin(&"e".repeat(21)),
            builtin(&"e".repeat(22)),
        ],
        None,
    );
    let frame = tools.frame(None);
    assert_eq!(width(&frame.rows[1][0].0), 22);
    assert_eq!(width(&frame.rows[2][0].0), 22);
    assert_eq!(width(&frame.rows[3][0].0), 22);
    assert!(frame.rows[1][0].0.contains(&"e".repeat(20)), "{frame:?}");
    assert!(
        frame.rows[2][0].0.contains(&format!("{}…", "e".repeat(19))),
        "{frame:?}"
    );
    assert!(
        frame.rows[3][0].0.contains(&format!("{}…", "e".repeat(19))),
        "{frame:?}"
    );
}

#[test]
fn fit_counts_terminal_columns() {
    assert_eq!(width(&fit(&"\u{3042}".repeat(10), 20)), 20);
    assert_eq!(fit(&"\u{3042}".repeat(10), 20), "\u{3042}".repeat(10));
    assert_eq!(
        fit(&"\u{3042}".repeat(11), 20),
        format!("{}… ", "\u{3042}".repeat(9))
    );
    assert_eq!(width(&fit(&"\u{3042}".repeat(11), 20)), 20);
    let combining = "e\u{301}".repeat(20);
    assert_eq!(width(&combining), 20);
    assert_eq!(fit(&combining, 20), combining);
    assert_eq!(width(&fit("\u{1F389}", 20)), 20);
}

#[test]
fn wide_names_keep_the_switch_columns_aligned() {
    let fake = fake_with(vec![group_switches(
        ToolGroup::Mcp("s".to_owned()),
        &[],
        &[],
    )]);
    let tools = opened(
        &fake,
        &[
            mcp("mcp__s__abc", "s", "abc"),
            mcp(
                "mcp__s__\u{3042}\u{3044}\u{3046}",
                "s",
                "\u{3042}\u{3044}\u{3046}",
            ),
            mcp(
                &format!("mcp__s__{}", "e\u{301}".repeat(5)),
                "s",
                &"e\u{301}".repeat(5),
            ),
        ],
        None,
    );
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    render(&tools.frame(None), area, &mut buf, &mut Vec::new());
    let bracket = |y: u16| (0..80).find(|x| buf[(*x, y)].symbol() == "[");
    // Tool rows are y 2, 3 and 4: the heading is y 1.
    let second = bracket(2).expect("row 2 renders its project switch");
    let third = bracket(3).expect("row 3 renders its project switch");
    let fourth = bracket(4).expect("row 4 renders its project switch");
    assert_eq!(second, third);
    assert_eq!(third, fourth);
    assert!(second > 0);
}

#[test]
fn only_the_answer_to_its_id_is_read() {
    let fake = Fake::new(Vec::new());
    let context = ctx(&fake, None);
    let mut tools = Tools::open(ID.to_owned(), &context);
    tools.answered("c_2", &[builtin("read")], &context);
    assert!(tools.frame(None).rows.is_empty());
    tools.answered(ID, &[builtin("read")], &context);
    assert_eq!(tool_names(&tools), vec!["read"]);
}

#[test]
fn a_rejection_shows_its_message_only_for_its_id() {
    let fake = Fake::new(Vec::new());
    let context = ctx(&fake, None);
    let mut tools = Tools::open(ID.to_owned(), &context);
    tools.rejected("c_2", "busy");
    assert_eq!(
        tools.frame(None).below,
        vec!["Reading the tools…".to_owned()]
    );
    tools.rejected(ID, "busy");
    assert_eq!(tools.frame(None).below, vec!["busy".to_owned()]);
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
    let mut tools = opened(
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
    for _ in 0..6 {
        tools.key(&Key::Down, &ctx(&fake, None));
    }
    tools.edit_key(&Edit::Right);
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    render(&tools.frame(None), area, &mut buf, &mut Vec::new());
    let text: Vec<String> = (0..24)
        .map(|y| {
            (0..80)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect();
    insta::assert_snapshot!("tools_80x24", text.join("\n"));
}
