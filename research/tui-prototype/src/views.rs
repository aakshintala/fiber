//! The swapped views, for #1633.
//!
//! A throwaway look prototype: each case draws one static frame from the
//! fixtures in this file, reusing the builders in `super` (the palette, the
//! `slab` surface, its roles and the swapped-view `view_header`), and the
//! program waits for a key; Esc, q or Ctrl+C quits. The fixture replay never
//! draws under a case. The ticket's cases are the minimum; `running` is drawn
//! only as docs/tui.md describes it.
//!
//! Where docs/tui.md names no command for a view (a delegate's or job's view
//! opens from its card row, not a slash command), the header's dim slot
//! carries what opened it, so the top row keeps its shape: name bold,
//! dim context, "Esc returns" dim right-aligned.

use super::input::{Ev, Key};
use super::{Args, Term};
use super::{BC, BI, BK, BW, GREEN, ORANGE, RED};
use super::{bold, dim, fg, paint, rgb, row, slab, sp, view_header};
use crate::cases::{Case, Surface};
use ratatui::style::{Color, Style};
use ratatui::text::Span;
use serde_json::json;
use std::io::{self, Write};
use unicode_width::UnicodeWidthStr;

/// Every `--view-case` case, named in README.md.
const CASES: &[Case<Look>] = &[
    Case {
        name: "delegate-view",
        help: "a delegate's breadcrumb, status card, transcript and input box",
        check: "the breadcrumb `main › ◆ review: …`, a status card naming harness, model, calls, elapsed, session and the stop target, the transcript in turn cards, and the input box sending steering to the delegate.",
        build: || Look {
            kind: Kind::Delegate,
        },
    },
    Case {
        name: "job-pty",
        help: "a job's 80x24 pseudo-terminal screen with its stop target",
        check: "the live 80x24 screen from attach with applied output, the job's status with its stop target, and the input box typing into the job.",
        build: || Look { kind: Kind::JobPty },
    },
    Case {
        name: "running",
        help: "the running delegates and jobs in start order",
        check: "two rows per running delegate, the second ending in its spend and its context bar with the percent, and one row per running job, in start order, each opening that item's view.",
        build: || Look {
            kind: Kind::Running,
        },
    },
    Case {
        name: "usage",
        help: "usage by turn, model and delegate with billed cost",
        check: "usage broken down by turn, by model and by delegate, each with tokens by kind, billed and subscription cost, and the budget left.",
        build: || Look { kind: Kind::Usage },
    },
    Case {
        name: "changed-files",
        help: "the files Fiber changed with one file's hunks",
        check: "the changed files with the chosen file's hunks as a guttered diff against HEAD, differing words in reverse video on paired lines, the untracked file wholly added, removals red and additions green.",
        build: || Look {
            kind: Kind::ChangedFiles,
        },
    },
    Case {
        name: "search-results",
        help: "every search match with its surrounding lines",
        check: "every match one row each with the lines around it, each match's header row ending in a dim `turn N`, the current match brighter, and the jump hint.",
        build: || Look {
            kind: Kind::SearchResults,
        },
    },
    Case {
        name: "tools",
        help: "every tool by source with its switches",
        check: "tools by source with state and size, two on/off switches per MCP or extension tool, built-ins without a switch, and the reload cost.",
        build: || Look { kind: Kind::Tools },
    },
    Case {
        name: "rules",
        help: "standing rules by scope with their revoke marks",
        check: "global rules then this project's, each with its prefix, when and from which session it was added, and its red ✕.",
        build: || Look { kind: Kind::Rules },
    },
    Case {
        name: "settings",
        help: "configuration keys with values and layers",
        check: "the keys as aligned columns under a dim header row, key, effective value and the layer it comes from (global, project, repository or default), then the reload cost and the file hint.",
        build: || Look {
            kind: Kind::Settings,
        },
    },
    Case {
        name: "skills",
        help: "skills with sources, visibility and switches",
        check: "one row per skill with its source, visibility, shadowed skill and switches, and the text and file hints.",
        build: || Look { kind: Kind::Skills },
    },
    Case {
        name: "opened-edit",
        help: "an edit opened inline with its guttered diff and word pairs",
        check: "the ledger row with its counts, the diff under it with a dim gutter and dim @@ lines, removals red and additions green, one removed and added line with their differing words in reverse video, and the dim overflow line past twenty rows.",
        build: || Look {
            kind: Kind::OpenedEdit,
        },
    },
    Case {
        name: "opened-read",
        help: "a read opened inline with its text numbered from its offset",
        check: "the ledger row with its lines, the text under it with a dim gutter numbered from the read's offset.",
        build: || Look {
            kind: Kind::OpenedRead,
        },
    },
    Case {
        name: "opened-search",
        help: "a search opened inline with its matches grouped under each path",
        check: "the ledger row with its matches, the matches under it grouped under each file's path with line numbers in the gutter and the matched text on the match tint.",
        build: || Look {
            kind: Kind::OpenedSearch,
        },
    },
    Case {
        name: "opened-shell",
        help: "shell calls opened inline with their commands and closings",
        check: "each ledger row, the full command and output under it, one closing line in removed for the failure and one for the killed command.",
        build: || Look {
            kind: Kind::OpenedShell,
        },
    },
    Case {
        name: "opened-mcp",
        help: "an MCP and an unknown tool opened inline as text and JSON",
        check: "each ledger row, the MCP content as plain text and the unknown tool's JSON indented under theirs.",
        build: || Look {
            kind: Kind::OpenedMcp,
        },
    },
    Case {
        name: "cut-shell",
        help: "a cut shell call's whole output with the marker where the model's 16 KiB view ended",
        check: "the swapped-view title row, the whole output paged from the session, and the dim marker where the model's 16 KiB view ended.",
        build: || Look {
            kind: Kind::CutShell,
        },
    },
];

/// `--view-case`, for `--help` and `check/views.md`.
pub(crate) const SURFACE: Surface = Surface {
    flag: "--view-case",
    file: "views",
    title: "Swapped views (#1633)",
    docs: || crate::cases::docs(CASES),
};

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Delegate,
    JobPty,
    Running,
    Usage,
    ChangedFiles,
    SearchResults,
    Tools,
    Rules,
    Settings,
    Skills,
    OpenedEdit,
    OpenedRead,
    OpenedSearch,
    OpenedShell,
    OpenedMcp,
    CutShell,
}

struct Look {
    kind: Kind,
}

/// A fixed tick is unnecessary: no swapped-view still animates.
/// Narrow width the README documents beside the 160x48 captures.
#[cfg(test)]
const NARROW: usize = 100;
#[cfg(test)]
const NARROW_ROWS: usize = 40;

/// A row painted at its own offset and width.
struct Placed {
    x: u16,
    w: u16,
    row: super::Row,
}

struct Frame {
    screen: Vec<Vec<Placed>>,
}

/// The delegate's status card: harness, model, calls, elapsed and session,
/// with the stop target that ends it.
fn delegate_status(w: usize) -> Vec<super::Row> {
    slab(
        vec![
            row(vec![
                sp("harness fiber", Style::new()),
                sp(" · model opus-5-5 · high", dim()),
            ]),
            row(vec![sp("12 calls · elapsed 4m · session s_d417a0", dim())]),
            row(vec![sp("■ stop s_d417a0", fg(RED))]),
        ],
        BC,
        None,
        w,
    )
}

/// The delegate's transcript in the conversation's turn cards.
fn delegate_transcript(w: usize) -> Vec<super::Row> {
    slab(
        vec![
            row(vec![sp("◆ review · 14:02", dim())]),
            row(vec![sp(
                "The sweep is done: 3 call sites, all covered.",
                Style::new(),
            )]),
            row(vec![sp("you · 14:03", dim())]),
            row(vec![sp("Ship it.", Style::new())]),
        ],
        BW,
        None,
        w,
    )
}

/// The job's pseudo-terminal screen: 80 columns by 24 rows of output since
/// the terminal attached, with cursor movement and erasing already applied.
fn pty_screen_rows() -> Vec<super::Row> {
    let lines = [
        "$ cargo test --watch",
        "running 42 tests",
        "test lock::flaky_close ... ok",
        "test lock::reopen ... ok",
        "test lock::held ... ok",
        "test serve::attach ... FAILED",
        "test serve::resume ... ok",
        "test tui::paint ... ok",
        "test tui::fit ... ok",
        "test tui::wrap ... ok",
        "test panel::cards ... ok",
        "test rail::bars ... ok",
        "test input::keys ... ok",
        "test input::mouse ... ok",
        "test lua::rows ... ok",
        "test paged::pages ... ok",
        "test home::list ... ok",
        "test login::steps ... ok",
        "",
        "failures:",
        "    serve::attach",
        "",
        "test result: FAILED. 41 passed; 1 failed",
        "waiting for file changes …",
    ];
    lines
        .iter()
        .map(|l| {
            if l.contains("FAILED") {
                row(vec![sp(l.to_string(), fg(RED))])
            } else {
                row(vec![sp(l.to_string(), Style::new())])
            }
        })
        .collect()
}

/// The input box, sending steering messages to the delegate.
fn delegate_input(w: usize) -> Vec<super::Row> {
    slab(
        vec![row(vec![
            sp("› █", Style::new()),
            sp(" steering goes to ◆ review", dim()),
        ])],
        BI,
        None,
        w,
    )
}

/// The running delegates and jobs, in the order they started: two rows per
/// delegate, one row per job. A row opens that item's view.
fn running_rows() -> Vec<super::Row> {
    // A delegate's detail row: the dim facts, then its spend and its
    // context use as the rail cards draw them (bar, then percent).
    let detail = |facts: &str, spend: &str, ctx: u8| {
        let mut spans = vec![
            sp(format!("  {facts} · "), dim()),
            sp(format!("{spend} "), Style::new()),
        ];
        spans.extend(super::bar(
            ctx as f64 / 100.0,
            10,
            super::rail_ctx_color(ctx),
        ));
        spans.push(sp(format!(" {ctx}%"), dim()));
        row(spans)
    };
    vec![
        row(vec![sp("Delegates · 2 running", bold())]),
        row(vec![sp("◆ docs: rail spec · 9m", Style::new())]),
        detail("opus-5-5 · high · 3 calls · s_b82c1d", "$0.84", 34),
        row(vec![sp(
            "◆ review: the wait_for_path sweep · 4m",
            Style::new(),
        )]),
        detail("opus-5-5 · high · 12 calls · s_d417a0", "$2.31", 72),
        super::Row::default(),
        row(vec![sp("Jobs · 1 running", bold())]),
        row(vec![sp("◆ cargo test --watch · 2m · s_j42", Style::new())]),
        super::Row::default(),
        row(vec![sp("a row opens that item's view", dim())]),
    ]
}

/// The usage view: the session's usage broken down by turn, by model and
/// by delegate, each with tokens by kind, cost billed and cost on
/// subscription, and the budget left while `budget.usd` is set.
fn usage_rows() -> Vec<super::Row> {
    vec![
        row(vec![sp("By turn", bold())]),
        row(vec![sp(
            "turn 3 · 120k in · 15k out · $1.35 billed · $0.00 subscription",
            Style::new(),
        )]),
        row(vec![sp(
            "turn 2 · 200k in · 30k out · $0.90 billed · $0.00 subscription",
            Style::new(),
        )]),
        super::Row::default(),
        row(vec![sp("By model", bold())]),
        row(vec![sp(
            "opus-5-5 · 400k in · 60k out · $2.10 billed · $0.00 subscription",
            Style::new(),
        )]),
        row(vec![sp(
            "gpt-6.1-sol · 80k in · 10k out · $0.45 billed · $0.00 subscription",
            Style::new(),
        )]),
        super::Row::default(),
        row(vec![sp("By delegate", bold())]),
        row(vec![sp(
            "◆ review · 60k in · 8k out · $0.40 billed · $0.00 subscription",
            Style::new(),
        )]),
        super::Row::default(),
        row(vec![sp("budget left $17.90 of $20.00", dim())]),
    ]
}

/// Changed files: the files Fiber changed, with the chosen file's hunks as
/// the workspace's current diff against HEAD, through the shared diff
/// renderer: a dim gutter, removals red, additions green, differing words
/// in reverse video on paired lines.
fn changed_files_rows(w: usize) -> Vec<super::Row> {
    let mut out = vec![
        row(vec![
            sp("crates/tui/src/view.rs", bold()),
            sp(" · ", dim()),
            sp("+41", fg(GREEN)),
            sp(" ", dim()),
            sp("−12", fg(RED)),
        ]),
        row(vec![
            sp("notes/scratch.md", Style::new()),
            sp(" · untracked, wholly added", dim()),
        ]),
        super::Row::default(),
    ];
    let hunks = [
        "@@ -12,7 +12,9 @@ fn paint(",
        "     let spans = fit(&r.spans, w as usize);",
        "-    buf.set_line(x, y, &Line::from(spans), w);",
        "+    buf.set_line(x, y, &line, w);",
        "     if let Some(bg) = r.bg {",
        "+    let line = Line::from(spans);",
        "-    let old = Line::from(spans);",
        "+    let new = Line::from(spans);",
    ];
    out.extend(slab(super::diff_rows(&hunks), BK, None, w));
    out
}

/// A marked search hit: the current one brighter, the rest on the dim
/// mark. These are the main loop's real colours: docs/tui.md's `match` and
/// `match_current` tokens name no palette entry in the code.
fn mark(word: &str, current: bool) -> Span<'static> {
    if current {
        Span::styled(word.to_string(), Style::new().bg(ORANGE).fg(Color::Black))
    } else {
        Span::styled(
            word.to_string(),
            Style::new().bg(rgb(0x5a4a1a)).fg(Color::White),
        )
    }
}

/// One search match's header row: the path bold, with the owning turn's
/// number dim and right-aligned after it. With no owning turn, or without
/// room for the path, a one-cell gap and the label, the path stands alone
/// and is never cut to make room: `fit` would cut the row's left side
/// rather than squeeze the label in.
fn match_header(path: &str, turn: Option<usize>, w: usize) -> super::Row {
    let label = turn.map(|n| format!("turn {}", n + 1));
    match label {
        Some(label) if path.width() + 1 + label.width() <= w => row(vec![
            sp(path.to_string(), bold()),
            super::t(),
            sp(label, dim()),
        ]),
        _ => row(vec![sp(path.to_string(), bold())]),
    }
}

/// Search results: every match one row each, with the lines around it.
/// The first match is the current one.
fn search_results_rows(w: usize) -> Vec<super::Row> {
    vec![
        row(vec![sp("3 of 5 · ⌕ lock", dim())]),
        super::Row::default(),
        match_header("crates/log/tests/lock.rs:41", Some(2), w),
        row(vec![
            sp("    let _guard = ", Style::new()),
            mark("lock", true),
            sp("();", Style::new()),
        ]),
        row(vec![sp("    map.insert(path, _guard);", Style::new())]),
        super::Row::default(),
        match_header("crates/tui/src/main.rs:1905", Some(6), w),
        row(vec![sp("    // the handoff point", dim())]),
        row(vec![
            sp("    holds the ", Style::new()),
            mark("lock", false),
            sp(" for the turn", Style::new()),
        ]),
        row(vec![sp("    while running {", Style::new())]),
        super::Row::default(),
        match_header("crates/panel/src/cards.rs:88", Some(13), w),
        row(vec![sp("    let cards = vec![];", Style::new())]),
        row(vec![
            sp("    for c in ", Style::new()),
            mark("lock", false),
            sp(" { lock held }", Style::new()),
        ]),
        row(vec![sp("        cards.push(c);", Style::new())]),
        super::Row::default(),
        row(vec![sp("Enter jumps to the match", dim())]),
    ]
}

/// One on/off switch: on bold, off dim.
fn switch(on: bool) -> Span<'static> {
    if on {
        sp("[on]", bold())
    } else {
        sp("[off]", dim())
    }
}

/// The tools view: every tool by source with its state and approximate
/// size. Each MCP or extension tool has two switches, this project and
/// everywhere; built-ins are always declared, so they have none.
fn tools_rows() -> Vec<super::Row> {
    vec![
        row(vec![sp("Built-in", bold())]),
        row(vec![sp("read · full · ~1.2k", Style::new())]),
        row(vec![sp("shell · full · ~800", Style::new())]),
        super::Row::default(),
        row(vec![sp("MCP · linear", bold())]),
        row(vec![
            sp(
                "linear_search · deferred · ~800 · this project ",
                Style::new(),
            ),
            switch(true),
            sp(" · everywhere ", Style::new()),
            switch(false),
        ]),
        super::Row::default(),
        row(vec![sp("Extension · notes", bold())]),
        row(vec![
            sp("notes_save · full · ~400 · this project", Style::new()),
            // The focused row marks its focused switch with a bold "›"
            // in place of the space before it.
            sp("›", bold()),
            switch(false),
            sp(" · everywhere ", Style::new()),
            switch(true),
        ]),
        super::Row::default(),
        row(vec![sp(
            "a change rebuilds the cache (~84k tokens · $0.31) on reload",
            dim(),
        )]),
    ]
}

/// `/rules`: every standing rule by scope, global then this project. Each
/// shows the prefix it allows, when and from which session it was added,
/// and a ✕ that revokes it.
fn rules_rows() -> Vec<super::Row> {
    vec![
        row(vec![sp("Global", bold())]),
        row(vec![
            sp(
                "allow shell:git status · added 2d ago in review #688 · ",
                Style::new(),
            ),
            sp("✕", fg(RED)),
        ]),
        super::Row::default(),
        row(vec![sp("This project", bold())]),
        row(vec![
            sp(
                "ask shell:cargo publish · added 1h ago in main · ",
                Style::new(),
            ),
            sp("✕", fg(RED)),
        ]),
        super::Row::default(),
        row(vec![sp("Ctrl+G opens the rules file", dim())]),
    ]
}

/// `/settings`: the configuration keys, their effective values and the
/// layer each comes from. Editing follows `fiber config set`; the view says
/// when a change needs a reload and what that costs.
fn settings_rows() -> Vec<super::Row> {
    // key | value | layer, each padded to its column so they line up.
    let line = |key: &str, value: &str, layer: &str, style: Style| {
        row(vec![sp(format!("{key:<20}{value:<14}{layer}"), style)])
    };
    vec![
        line("key", "value", "layer", dim()),
        line("model", "opus-5-5", "global", Style::new()),
        line("thinking", "high", "project", Style::new()),
        line("budget.usd", "20", "project", Style::new()),
        line("tui.theme", "dark", "global", Style::new()),
        line("permissions.mode", "auto", "repository", Style::new()),
        line("tools.timeout_s", "120", "default", Style::new()),
        super::Row::default(),
        row(vec![sp(
            "changing model needs a reload (~84k tokens)",
            dim(),
        )]),
        row(vec![sp("Ctrl+G opens the file", dim())]),
    ]
}

/// `/skills`: one row per skill with its name, one-line description, where
/// it comes from, whether the model can see it, any skill it shadows, and
/// its off/on switches for this project and everywhere.
fn skills_rows() -> Vec<super::Row> {
    vec![
        row(vec![
            sp(
                "review · checks a diff · repo · seen · this project ",
                Style::new(),
            ),
            switch(true),
            sp(" · everywhere ", Style::new()),
            switch(true),
        ]),
        row(vec![
            sp(
                "notes · personal notes · personal · hidden · shadows review · this project ",
                Style::new(),
            ),
            switch(false),
            sp(" · everywhere ", Style::new()),
            switch(true),
        ]),
        super::Row::default(),
        row(vec![sp(
            "Enter shows the text · Ctrl+G opens its file",
            dim(),
        )]),
    ]
}

/// One group with its calls opened, as the conversation draws it: the
/// summary line, each ledger row, and each opened call under its row.
/// Static fixtures only; the rows come from the ledger builders.
fn opened_frame(calls: Vec<super::Call>, open: &[usize], cols: usize) -> Vec<Placed> {
    let mut v = super::View::default();
    for ii in open {
        v.exp.insert((0, 0, *ii));
    }
    let g = super::Group {
        items: calls.into_iter().map(super::Item::C).collect(),
        steps: 1,
        step_closed: true,
        open: true,
        last_ts: 2500,
    };
    placed(
        super::group_lines(&g, super::Act::Group(0, 0), cols, &v),
        cols as u16,
    )
}

fn edit_fixture() -> super::Call {
    let mut diff = vec![
        "@@ -12,5 +12,6 @@ fn paint(".to_string(),
        "     let spans = fit(&r.spans, w as usize);".to_string(),
        "-    buf.set_line(x, y, &line, w);".to_string(),
        "+    buf.set_line(x, y, &Line::from(spans), w);".to_string(),
        "     if let Some(bg) = r.bg {".to_string(),
        "+    let line = Line::from(spans);".to_string(),
    ];
    diff.extend((7..=24).map(|i| format!("     filler line {i}")));
    super::Call {
        name: "edit".into(),
        args: json!({ "path": "crates/tui/src/view.rs" }),
        st: super::St::Completed,
        start: 0,
        end: Some(2500),
        err: None,
        lines: 0,
        exit: None,
        changes: vec![("crates/tui/src/view.rs".into(), 6, 2)],
        content: diff.join("\n"),
        step: 1,
        asked: None,
    }
}

fn read_fixture() -> super::Call {
    super::Call {
        name: "read".into(),
        args: json!({ "path": "crates/hub/src/lock.rs", "offset": 41 }),
        st: super::St::Completed,
        start: 0,
        end: Some(2500),
        err: None,
        lines: 8,
        exit: None,
        changes: vec![],
        content: [
            "pub fn try_lock(path: &Path) -> Result<Lock> {",
            "    // one holder at a time",
            "    let file = File::create(path)?;",
            "    let _guard = lock(&file)?;",
            "    map.insert(path, _guard);",
            "    // the map owns the guard",
            "    Ok(Lock { file })",
            "}",
        ]
        .join("\n"),
        step: 1,
        asked: None,
    }
}

fn search_fixture() -> super::Call {
    super::Call {
        name: "shell".into(),
        args: json!({ "command": "grep -rn lock crates" }),
        st: super::St::Completed,
        start: 0,
        end: Some(2500),
        err: None,
        lines: 3,
        exit: Some(0),
        changes: vec![],
        content: [
            "crates/hub/src/lock.rs:41:    let _guard = lock(path);",
            "crates/hub/src/lock.rs:42:    lock twice lock;",
            "--",
            "crates/tui/src/main.rs:1905:    holds the lock for the turn",
        ]
        .join("\n"),
        step: 1,
        asked: None,
    }
}

fn shell_fixtures() -> Vec<super::Call> {
    vec![
        super::Call {
            name: "shell".into(),
            args: json!({ "command": "cargo test -p log" }),
            st: super::St::Failed,
            start: 0,
            end: Some(2500),
            err: Some(("nonzero_exit".into(), "exit code 1".into())),
            lines: 2,
            exit: Some(1),
            changes: vec![],
            content: "running 42 tests\ntest serve::attach ... FAILED".into(),
            step: 1,
            asked: None,
        },
        super::Call {
            name: "shell".into(),
            args: json!({ "command": "cargo test -- --nocapture" }),
            st: super::St::Failed,
            start: 0,
            end: Some(2500),
            err: Some(("signal".into(), "SIGTERM".into())),
            lines: 1,
            exit: None,
            changes: vec![],
            content: "running 42 tests".into(),
            step: 1,
            asked: None,
        },
    ]
}

fn mcp_fixtures() -> Vec<super::Call> {
    vec![
        super::Call {
            name: "linear_search".into(),
            args: json!({}),
            st: super::St::Completed,
            start: 0,
            end: Some(2500),
            err: None,
            lines: 0,
            exit: None,
            changes: vec![],
            content:
                "3 results for \"lock\"\ncrates/hub/src/lock.rs:41\ncrates/tui/src/main.rs:1905"
                    .into(),
            step: 1,
            asked: None,
        },
        super::Call {
            name: "notes_save".into(),
            args: json!({}),
            st: super::St::Completed,
            start: 0,
            end: Some(2500),
            err: None,
            lines: 0,
            exit: None,
            changes: vec![],
            content: "{\"saved\": true, \"path\": \"notes/scratch.md\"}".into(),
            step: 1,
            asked: None,
        },
    ]
}

/// A cut shell call's whole output, paged from the session's artifact: the
/// lines the model saw, a dim marker where its 16 KiB view ended, then the
/// rest with the closing line. A still shows one screen of the paging.
fn cut_shell_rows() -> Vec<super::Row> {
    let mut out = vec![row(vec![sp("running 42 tests", Style::new())])];
    for t in [
        "test lock::flaky_close ... ok",
        "test lock::reopen ... ok",
        "test lock::held ... ok",
        "test serve::attach ... ok",
        "test serve::resume ... ok",
        "test tui::paint ... ok",
        "test tui::fit ... ok",
        "test tui::wrap ... ok",
        "test panel::cards ... ok",
        "test rail::bars ... ok",
        "test input::keys ... ok",
    ] {
        out.push(row(vec![sp(t, Style::new())]));
    }
    out.push(row(vec![sp("── end of the model's 16 KiB view ──", dim())]));
    for t in [
        "test input::mouse ... ok",
        "test lua::rows ... ok",
        "test paged::pages ... ok",
        "test home::list ... ok",
        "test login::steps ... ok",
        "",
        "test result: ok. 42 passed; 0 failed",
    ] {
        out.push(row(vec![sp(t, Style::new())]));
    }
    out.push(row(vec![sp("exit 0", dim())]));
    out
}

/// Rows painted at `w`: the terminal width, except a slab that keeps its
/// own width, such as the job's 80-column screen.
fn placed(rows: Vec<super::Row>, w: u16) -> Vec<Placed> {
    rows.into_iter()
        .map(|row| Placed { x: 0, w, row })
        .collect()
}

fn body_rows(c: &Look, cols: usize) -> Vec<Placed> {
    let full = cols as u16;
    let blank = || Placed {
        x: 0,
        w: full,
        row: super::Row::default(),
    };
    match c.kind {
        Kind::Delegate => {
            let mut out = placed(vec![view_header("Delegate", "◆ review")], full);
            out.push(blank());
            out.extend(placed(
                vec![row(vec![sp(
                    "main › ◆ review: the wait_for_path sweep",
                    bold(),
                )])],
                full,
            ));
            out.push(blank());
            out.extend(placed(delegate_status(cols), full));
            out.push(blank());
            out.extend(placed(delegate_transcript(cols), full));
            out.push(blank());
            out.extend(placed(delegate_input(cols), full));
            out
        }
        Kind::Running => {
            let mut out = placed(vec![view_header("Running", "delegates · jobs")], full);
            out.push(blank());
            out.extend(placed(running_rows(), full));
            out
        }
        Kind::Usage => {
            let mut out = placed(vec![view_header("Usage", "/usage")], full);
            out.push(blank());
            out.extend(placed(usage_rows(), full));
            out
        }
        Kind::ChangedFiles => {
            let mut out = placed(
                vec![view_header("Changed files", "diff against HEAD")],
                full,
            );
            out.push(blank());
            out.extend(placed(changed_files_rows(cols), full));
            out
        }
        Kind::SearchResults => {
            let mut out = placed(vec![view_header("Search", "⌕ lock")], full);
            out.push(blank());
            out.extend(placed(search_results_rows(cols), full));
            out
        }
        Kind::Tools => {
            let mut out = placed(vec![view_header("Tools", "/tools")], full);
            out.push(blank());
            out.extend(placed(tools_rows(), full));
            out
        }
        Kind::Rules => {
            let mut out = placed(vec![view_header("Rules", "/rules")], full);
            out.push(blank());
            out.extend(placed(rules_rows(), full));
            out
        }
        Kind::Settings => {
            let mut out = placed(vec![view_header("Settings", "/settings")], full);
            out.push(blank());
            out.extend(placed(settings_rows(), full));
            out
        }
        Kind::Skills => {
            let mut out = placed(vec![view_header("Skills", "/skills")], full);
            out.push(blank());
            out.extend(placed(skills_rows(), full));
            out
        }
        Kind::OpenedEdit => opened_frame(vec![edit_fixture()], &[0], cols),
        Kind::OpenedRead => opened_frame(vec![read_fixture()], &[0], cols),
        Kind::OpenedSearch => opened_frame(vec![search_fixture()], &[0], cols),
        Kind::OpenedShell => opened_frame(shell_fixtures(), &[0, 1], cols),
        Kind::OpenedMcp => opened_frame(mcp_fixtures(), &[0, 1], cols),
        Kind::CutShell => {
            let mut out = placed(vec![view_header("Shell", "cargo test -p log")], full);
            out.push(blank());
            out.extend(placed(
                vec![row(vec![sp(
                    "$ cargo test -p log -- --nocapture",
                    Style::new(),
                )])],
                full,
            ));
            out.push(blank());
            out.extend(placed(cut_shell_rows(), full));
            out.push(blank());
            out.extend(placed(
                vec![row(vec![sp("y copies the whole call", dim())])],
                full,
            ));
            out
        }
        Kind::JobPty => {
            let mut out = placed(vec![view_header("Job", "◆ cargo test")], full);
            out.push(blank());
            out.extend(placed(
                vec![row(vec![
                    sp("job s_j42 · cargo test --watch · running 2m", Style::new()),
                    sp(" · ", dim()),
                    sp("■ stop ends it", fg(RED)),
                ])],
                full,
            ));
            out.push(blank());
            out.extend(placed(
                vec![row(vec![sp(
                    "live screen · 80 columns × 24 rows from attach",
                    dim(),
                )])],
                full,
            ));
            // The screen keeps its own 80 columns: painted at the terminal
            // width, its background would run past its edges.
            out.extend(placed(slab(pty_screen_rows(), BW, None, 80), 80));
            out.push(blank());
            out.extend(placed(
                slab(
                    vec![row(vec![
                        sp("› █", Style::new()),
                        sp(" type into the job (job_input)", dim()),
                    ])],
                    BI,
                    None,
                    cols,
                ),
                full,
            ));
            out
        }
    }
}

fn frame(c: &Look, cols: usize, rows: usize) -> Frame {
    let mut body = body_rows(c, cols);
    body.truncate(rows);
    while body.len() < rows {
        body.push(Placed {
            x: 0,
            w: cols as u16,
            row: super::Row::default(),
        });
    }
    Frame {
        screen: body.into_iter().map(|p| vec![p]).collect(),
    }
}

fn draw(term: &mut Term, c: &Look) -> io::Result<()> {
    let size = term.size()?;
    let (cols, rows) = (size.width.max(1), size.height.max(1));
    let fr = frame(c, cols as usize, rows as usize);
    term.backend_mut().write_all(b"\x1b[?2026h")?;
    term.draw(|f| {
        let buf = f.buffer_mut();
        for (y, placements) in fr.screen.iter().enumerate() {
            for p in placements {
                paint(buf, p.x, y as u16, p.w, &p.row);
            }
        }
    })?;
    let be = term.backend_mut();
    be.write_all(b"\x1b[?2026l")?;
    be.flush()?;
    Ok(())
}

/// Draws the `--view-case` case and waits for a key. Mutually exclusive with
/// the conversation view: the fixture replay never draws.
pub(crate) fn run_view(a: &Args, term: &mut Term) -> io::Result<String> {
    let name = a.view_case.clone().unwrap_or_default();
    let Some(c) = crate::cases::lookup(CASES, &name) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "unknown --view-case case {name:?}; one of: {}",
                crate::cases::names(CASES)
            ),
        ));
    };
    draw(term, &c)?;
    let mut rd = super::input::Reader::new()?;
    loop {
        let (evs, resized) = rd.wait(None)?;
        if resized {
            draw(term, &c)?;
        }
        for ev in evs {
            match ev {
                Ev::Key(Key::Char('c'), m) if m.ctrl => return Ok(format!("view {name}\n")),
                Ev::Key(Key::Esc, _) => return Ok(format!("view {name}\n")),
                Ev::Key(Key::Char('q'), m) if !m.ctrl && !m.alt && !m.sup => {
                    return Ok(format!("view {name}\n"));
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Modifier;
    use unicode_width::UnicodeWidthStr;

    fn parse(name: &str) -> Option<Look> {
        crate::cases::lookup(CASES, name)
    }

    fn text(c: &Look, cols: usize, rows: usize) -> String {
        frame(c, cols, rows)
            .screen
            .into_iter()
            .map(|ps| {
                ps.iter()
                    .map(|p| {
                        p.row
                            .spans
                            .iter()
                            .map(|s| s.content.as_ref())
                            .collect::<String>()
                    })
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The buffer's text, one line per row, as `draw` painted it.
    fn buf_text(buf: &Buffer, cols: usize, rows: usize) -> String {
        (0..rows as u16)
            .map(|y| {
                (0..cols as u16)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Paints a case into a buffer, as `draw` does.
    fn buffer(c: &Look, cols: usize, rows: usize) -> Buffer {
        let mut buf = Buffer::empty(Rect::new(0, 0, cols as u16, rows as u16));
        let fr = frame(c, cols, rows);
        for (y, ps) in fr.screen.iter().enumerate() {
            for p in ps {
                crate::paint(&mut buf, p.x, y as u16, p.w, &p.row);
            }
        }
        buf
    }

    /// The swapped-view top row: the surface tint, the name bold, the command
    /// dim, and "Esc returns" dim right-aligned.
    fn assert_header(buf: &Buffer, name: &str, cmd: &str, cols: usize) {
        // The surface tint runs the whole row.
        for x in [0, cols as u16 / 2, cols as u16 - 1] {
            assert_eq!(buf[(x, 0)].bg, BI, "no surface tint at {x}");
        }
        // The name is bold from column 1; the command follows it, dim.
        let nx = 1u16;
        for (i, ch) in name.chars().enumerate() {
            let cell = &buf[(nx + i as u16, 0)];
            assert_eq!(cell.symbol(), ch.to_string().as_str());
            assert!(
                cell.modifier.contains(Modifier::BOLD),
                "the name is not bold at {}",
                nx as usize + i
            );
        }
        let cx = nx + name.width() as u16 + 2;
        for (i, ch) in cmd.chars().enumerate() {
            let cell = &buf[(cx + i as u16, 0)];
            assert_eq!(cell.symbol(), ch.to_string().as_str());
            assert!(
                cell.modifier.contains(Modifier::DIM),
                "the command is not dim at {}",
                cx as usize + i
            );
        }
        // "Esc returns" ends the row, dim.
        let ex = cols as u16 - "Esc returns ".width() as u16;
        assert_eq!(buf[(ex, 0)].symbol(), "E");
        for (i, ch) in "Esc returns ".chars().enumerate() {
            let cell = &buf[(ex + i as u16, 0)];
            assert_eq!(cell.symbol(), ch.to_string().as_str());
            assert!(
                cell.modifier.contains(Modifier::DIM),
                "Esc is not dim at {}",
                ex as usize + i
            );
        }
    }

    fn parse_case(name: &str) -> Look {
        parse(name).unwrap()
    }

    #[test]
    fn every_case_parses_and_unknown_does_not() {
        assert!(
            CASES
                .iter()
                .all(|c| crate::cases::lookup(CASES, c.name).is_some())
        );
        assert_eq!(CASES.len(), 16);
        assert!(parse("nope").is_none());
    }

    #[test]
    fn delegate_view_header_row() {
        for cols in [160, NARROW] {
            let c = parse_case("delegate-view");
            let buf = buffer(&c, cols, 48);
            assert_header(&buf, "Delegate", "◆ review", cols);
        }
    }

    #[test]
    fn delegate_view_shows_breadcrumb_status_transcript_and_input() {
        for cols in [160, NARROW] {
            let c = parse_case("delegate-view");
            let t = text(&c, cols, 48);
            assert!(t.contains("main › ◆ review:"), "no breadcrumb");
            assert!(t.contains("harness fiber"), "no harness");
            assert!(t.contains("opus-5-5"), "no model");
            assert!(t.contains("12 calls"), "no calls");
            assert!(t.contains("4m"), "no elapsed");
            assert!(t.contains("s_d417a0"), "no session");
            assert!(t.contains("■ stop"), "no stop target");
            assert!(t.contains("The sweep is done"), "no transcript");
            assert!(t.contains("steering goes to ◆ review"), "no input box");
        }
    }

    #[test]
    fn job_pty_header_row() {
        for cols in [160, NARROW] {
            let c = parse_case("job-pty");
            let buf = buffer(&c, cols, 48);
            assert_header(&buf, "Job", "◆ cargo test", cols);
        }
    }

    #[test]
    fn job_pty_shows_status_screen_and_input() {
        for cols in [160, NARROW] {
            let c = parse_case("job-pty");
            let t = text(&c, cols, 48);
            assert!(t.contains("s_j42"), "no job id");
            assert!(t.contains("cargo test --watch"), "no command");
            assert!(t.contains("running 2m"), "no elapsed");
            assert!(t.contains("■ stop"), "no stop target");
            assert!(t.contains("job_input"), "no input hint");
            assert!(t.contains("running 42 tests"), "no screen output");
            assert!(t.contains("80 columns × 24 rows"), "no screen caption");
        }
    }

    #[test]
    fn job_pty_screen_is_80_by_24() {
        use crate::width;
        let rows = pty_screen_rows();
        assert_eq!(rows.len(), 24, "the screen is not 24 rows");
        for (y, r) in rows.iter().enumerate() {
            assert!(
                width(&r.spans) <= 80,
                "screen row {y} is wider than 80 columns"
            );
        }
    }

    #[test]
    fn job_pty_screen_background_ends_at_column_80() {
        use ratatui::style::Color;
        let c = parse_case("job-pty");
        let buf = buffer(&c, 160, 48);
        let t = buf_text(&buf, 160, 48);
        let lines: Vec<&str> = t.lines().collect();
        let top = lines
            .iter()
            .position(|l| l.contains("$ cargo test --watch"))
            .unwrap();
        // The slab's edge rows frame the 24 screen rows.
        assert!(lines[top - 1].starts_with("▄"), "no top edge");
        assert!(lines[top + 24].starts_with("▀"), "no bottom edge");
        for (i, y) in (top..top + 24).enumerate() {
            for x in [0, 39, 79] {
                assert_eq!(buf[(x, y as u16)].bg, BW, "no screen tint at {x},{y}");
            }
            for x in [80, 100, 159] {
                assert_eq!(
                    buf[(x, y as u16)].bg,
                    Color::Reset,
                    "screen row {i} background runs past column 80"
                );
            }
        }
    }

    #[test]
    fn job_pty_failed_lines_read_red() {
        let rows = pty_screen_rows();
        let failed: Vec<_> = rows
            .iter()
            .filter(|r| r.spans.iter().any(|s| s.content.contains("FAILED")))
            .collect();
        assert!(!failed.is_empty(), "no FAILED line to mark");
        for r in failed {
            assert_eq!(r.spans[0].style.fg, Some(crate::RED));
        }
        let ok = rows
            .iter()
            .find(|r| r.spans.iter().any(|s| s.content.contains("flaky_close")))
            .unwrap();
        assert_eq!(ok.spans[0].style.fg, None);
    }

    #[test]
    fn running_header_row() {
        for cols in [160, NARROW] {
            let c = parse_case("running");
            let buf = buffer(&c, cols, 48);
            assert_header(&buf, "Running", "delegates · jobs", cols);
        }
    }

    #[test]
    fn running_lists_delegates_and_jobs_in_start_order() {
        for cols in [160, NARROW] {
            let c = parse_case("running");
            let t = text(&c, cols, 48);
            assert!(t.contains("2 running"), "no delegate count");
            assert!(t.contains("1 running"), "no job count");
            // Two rows per delegate: the row, then its dim detail.
            let review = t.find("◆ review:").unwrap();
            let detail = t.find("12 calls").unwrap();
            assert!(
                detail > review,
                "the delegate's detail is not under its row"
            );
            // One row per job.
            assert!(t.contains("◆ cargo test --watch · 2m"), "no job row");
            // Start order: docs started 9m ago, review 4m ago.
            assert!(
                t.find("◆ docs:").unwrap() < t.find("◆ review:").unwrap(),
                "delegates are not in start order"
            );
            assert!(t.contains("opens that item's view"), "no row hint");
        }
    }

    #[test]
    fn running_buffer_lists_delegates_in_start_order() {
        let c = parse_case("running");
        let buf = buffer(&c, 160, 48);
        let t = buf_text(&buf, 160, 48);
        assert!(
            t.find("◆ docs:").unwrap() < t.find("◆ review:").unwrap(),
            "delegates are not in start order"
        );
    }

    #[test]
    fn usage_header_row() {
        for cols in [160, NARROW] {
            let c = parse_case("usage");
            let buf = buffer(&c, cols, 48);
            assert_header(&buf, "Usage", "/usage", cols);
        }
    }

    #[test]
    fn usage_breaks_down_turns_models_and_delegates() {
        for cols in [160, NARROW] {
            let c = parse_case("usage");
            let t = text(&c, cols, 48);
            assert!(t.contains("By turn"), "no turn section");
            assert!(t.contains("By model"), "no model section");
            assert!(t.contains("By delegate"), "no delegate section");
            assert!(t.contains("turn 3"), "no turn row");
            assert!(t.contains("opus-5-5"), "no model row");
            assert!(t.contains("◆ review"), "no delegate row");
            assert!(t.contains("billed"), "no billed cost");
            assert!(t.contains("subscription"), "no subscription cost");
            assert!(t.contains("budget left"), "no budget left");
        }
    }

    #[test]
    fn changed_files_header_row() {
        for cols in [160, NARROW] {
            let c = parse_case("changed-files");
            let buf = buffer(&c, cols, 48);
            assert_header(&buf, "Changed files", "diff against HEAD", cols);
        }
    }

    #[test]
    fn changed_files_shows_files_and_hunks() {
        for cols in [160, NARROW] {
            let c = parse_case("changed-files");
            let t = text(&c, cols, 48);
            assert!(t.contains("crates/tui/src/view.rs"), "no file row");
            assert!(t.contains("+41"), "no additions");
            assert!(t.contains("−12"), "no deletions");
            assert!(t.contains("notes/scratch.md"), "no untracked file");
            assert!(t.contains("untracked"), "untracked not marked");
            assert!(t.contains("@@ -12,7 +12,9 @@"), "no hunk header");
            assert!(t.contains("-    buf.set_line"), "no removal");
            assert!(t.contains("+    let line"), "no addition");
        }
    }

    #[test]
    fn changed_files_marks_removals_red_and_hunks_dim() {
        let rows = changed_files_rows(160);
        let plain = |r: &crate::Row| {
            r.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        };
        let rev = |r: &crate::Row| {
            r.spans
                .iter()
                .filter(|s| s.style.add_modifier.contains(Modifier::REVERSED))
                .map(|s| s.content.to_string())
                .collect::<Vec<_>>()
        };
        // the paired removal numbers the old side and reads red, with only
        // its differing words in reverse video
        let removal = rows
            .iter()
            .find(|r| plain(r).contains("-    buf.set_line"))
            .unwrap();
        assert!(
            removal.spans.iter().any(|s| s.style.fg == Some(crate::RED)),
            "the removal is not red"
        );
        assert!(
            plain(removal).contains("  13 -"),
            "no old-side gutter: {}",
            plain(removal)
        );
        assert_eq!(rev(removal), ["&Line::from(spans),"]);
        // its pair numbers the new side in the added role
        let addition = rows
            .iter()
            .find(|r| plain(r).contains("+    buf.set_line"))
            .unwrap();
        assert!(
            addition
                .spans
                .iter()
                .any(|s| s.style.fg == Some(crate::GREEN)),
            "the addition is not green"
        );
        assert!(
            plain(addition).contains("  13 +"),
            "no new-side gutter: {}",
            plain(addition)
        );
        assert_eq!(rev(addition), ["&line,"]);
        // an unpaired addition keeps its colour with nothing reversed
        let lone = rows
            .iter()
            .find(|r| plain(r).contains("+    let line"))
            .unwrap();
        assert!(lone.spans.iter().any(|s| s.style.fg == Some(crate::GREEN)));
        assert!(rev(lone).is_empty(), "an unpaired line reverses");
        let hunk = rows
            .iter()
            .find(|r| r.spans.iter().any(|s| s.content.starts_with("@@")))
            .unwrap();
        assert!(
            hunk.spans
                .iter()
                .any(|s| s.style.add_modifier.contains(Modifier::DIM)),
            "the @@ line is not dim"
        );
    }

    #[test]
    fn search_results_header_row() {
        for cols in [160, NARROW] {
            let c = parse_case("search-results");
            let buf = buffer(&c, cols, 48);
            assert_header(&buf, "Search", "⌕ lock", cols);
        }
    }

    #[test]
    fn search_results_shows_matches_with_context() {
        for cols in [160, NARROW] {
            let c = parse_case("search-results");
            let t = text(&c, cols, 48);
            assert!(t.contains("3 of 5"), "no count");
            assert!(t.contains("crates/log/tests/lock.rs:41"), "no first match");
            assert!(t.contains("turn 3"), "no first turn label");
            assert!(t.contains("let _guard = "), "no surrounding line");
            assert!(t.contains("crates/tui/src/main.rs:1905"), "no second match");
            assert!(t.contains("turn 7"), "no second turn label");
            assert!(t.contains("crates/panel/src/cards.rs:88"), "no third match");
            assert!(t.contains("turn 14"), "no third turn label");
            assert!(t.contains("Enter jumps to the match"), "no jump hint");
        }
    }

    #[test]
    fn search_results_marks_the_current_match_brighter() {
        use ratatui::style::Color;
        let rows = search_results_rows(160);
        let mut current = 0;
        let mut other = 0;
        for r in &rows {
            for s in &r.spans {
                if s.content == "lock" {
                    if s.style.bg == Some(crate::ORANGE) {
                        assert_eq!(s.style.fg, Some(Color::Black));
                        current += 1;
                    } else {
                        assert_eq!(s.style.bg, Some(crate::rgb(0x5a4a1a)));
                        other += 1;
                    }
                }
            }
        }
        assert_eq!(current, 1, "not exactly one current match");
        assert_eq!(other, 2, "not two other matches");
    }

    #[test]
    fn search_label_is_dim_and_right_aligned() {
        for cols in [160, NARROW] {
            let c = parse_case("search-results");
            let buf = buffer(&c, cols, 48);
            for (path, label) in [
                ("crates/log/tests/lock.rs:41", "turn 3"),
                ("crates/tui/src/main.rs:1905", "turn 7"),
                ("crates/panel/src/cards.rs:88", "turn 14"),
            ] {
                let mut found = None;
                for y in 0..48u16 {
                    let line: String = (0..cols as u16)
                        .map(|x| buf[(x, y)].symbol().to_string())
                        .collect();
                    if line.contains(path) && line.contains(label) {
                        found = Some(y);
                        break;
                    }
                }
                let y =
                    found.unwrap_or_else(|| panic!("no {path} header with {label} at {cols} cols"));
                // the label ends at the last column, dim
                let start = cols - label.len();
                for (i, ch) in label.chars().enumerate() {
                    let cell = &buf[(start as u16 + i as u16, y)];
                    assert_eq!(
                        cell.symbol(),
                        ch.to_string().as_str(),
                        "label char at {cols} cols"
                    );
                    assert!(
                        cell.modifier.contains(Modifier::DIM),
                        "the label is not dim at column {}",
                        start + i
                    );
                }
                // the path starts at column 0, still bold
                for (i, ch) in path.chars().enumerate() {
                    let cell = &buf[(i as u16, y)];
                    assert_eq!(cell.symbol(), ch.to_string().as_str());
                    assert!(
                        cell.modifier.contains(Modifier::BOLD),
                        "the path is not bold at column {i}"
                    );
                }
            }
        }
    }

    #[test]
    fn search_label_is_left_out_without_a_turn_or_room() {
        let path = "crates/log/tests/lock.rs:41";
        let label = "turn 3";
        // no owning turn: the path alone
        let r = match_header(path, None, 160);
        assert_eq!(r.spans.len(), 1);
        assert_eq!(r.spans[0].content, path);
        // the exact boundary both sides, and one cell of room to spare
        let w = path.width() + 1 + label.width();
        let kept = match_header(path, Some(2), w);
        assert!(
            kept.spans.iter().any(|s| s.content == label),
            "the label is missing at the boundary"
        );
        let dropped = match_header(path, Some(2), w - 1);
        assert!(
            dropped.spans.iter().all(|s| s.content != label),
            "the label is drawn without room"
        );
        assert_eq!(dropped.spans.len(), 1);
        assert_eq!(dropped.spans[0].content, path);
        let wide = match_header(path, Some(2), w + 1);
        assert!(
            wide.spans.iter().any(|s| s.content == label),
            "the label is missing with room to spare"
        );
        // the turn number counts from 1, dim; the path stays bold and whole
        let label_span = kept.spans.iter().find(|s| s.content == label).unwrap();
        assert!(
            label_span.style.add_modifier.contains(Modifier::DIM),
            "the label is not dim"
        );
        let path_span = &kept.spans[0];
        assert_eq!(path_span.content, path);
        assert!(
            path_span.style.add_modifier.contains(Modifier::BOLD),
            "the path is not bold"
        );
    }

    #[test]
    fn tools_header_row() {
        for cols in [160, NARROW] {
            let c = parse_case("tools");
            let buf = buffer(&c, cols, 48);
            assert_header(&buf, "Tools", "/tools", cols);
        }
    }

    #[test]
    fn tools_shows_sources_states_and_switches() {
        for cols in [160, NARROW] {
            let c = parse_case("tools");
            let t = text(&c, cols, 48);
            assert!(t.contains("Built-in"), "no built-in section");
            assert!(t.contains("read · full"), "no built-in row");
            assert!(t.contains("MCP · linear"), "no MCP section");
            assert!(t.contains("linear_search · deferred"), "no MCP row");
            assert!(t.contains("Extension · notes"), "no extension section");
            assert!(t.contains("notes_save · full"), "no extension row");
            assert!(t.contains("this project"), "no project switch");
            assert!(t.contains("everywhere"), "no global switch");
            assert_eq!(t.matches("[on]").count(), 2, "wrong on count");
            assert_eq!(t.matches("[off]").count(), 2, "wrong off count");
            assert!(t.contains("on reload"), "no reload note");
        }
    }

    #[test]
    fn tools_every_tool_row_shows_full_or_deferred() {
        for r in tools_rows() {
            let plain: String = r.spans.iter().map(|s| s.content.as_ref()).collect();
            if plain.contains("· ~") {
                assert!(
                    plain.contains("full") || plain.contains("deferred"),
                    "tool row without a full/deferred state: {plain}"
                );
            }
        }
    }

    #[test]
    fn tools_buffer_shows_full_or_deferred_states() {
        let c = parse_case("tools");
        let buf = buffer(&c, 160, 48);
        let t = buf_text(&buf, 160, 48);
        assert!(t.contains("notes_save · full"), "no extension row");
        assert!(!t.contains("loaded"), "a tool still shows a loaded state");
    }

    #[test]
    fn tools_focused_switch_is_marked_with_a_bold_chevron() {
        let rows = tools_rows();
        let marked: Vec<_> = rows
            .iter()
            .filter(|r| r.spans.iter().any(|s| s.content == "›"))
            .collect();
        assert_eq!(marked.len(), 1, "one focused row");
        let plain: String = marked[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            plain.contains("this project›[off] · everywhere [on]"),
            "{plain}"
        );
        let chevron = marked[0].spans.iter().find(|s| s.content == "›");
        assert!(chevron.is_some_and(|s| s.style.add_modifier.contains(Modifier::BOLD)));
    }

    #[test]
    fn tools_switch_marks_on_bold_and_off_dim() {
        let on = switch(true);
        assert_eq!(on.content, "[on]");
        assert!(on.style.add_modifier.contains(Modifier::BOLD));
        let off = switch(false);
        assert_eq!(off.content, "[off]");
        assert!(off.style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn tools_builtin_rows_carry_no_switch() {
        for r in tools_rows() {
            let plain: String = r.spans.iter().map(|s| s.content.as_ref()).collect();
            if plain.contains("read · full") {
                assert!(
                    !plain.contains("[on]") && !plain.contains("[off]"),
                    "built-in row carries a switch: {plain}"
                );
            }
        }
    }

    #[test]
    fn rules_header_row() {
        for cols in [160, NARROW] {
            let c = parse_case("rules");
            let buf = buffer(&c, cols, 48);
            assert_header(&buf, "Rules", "/rules", cols);
        }
    }

    #[test]
    fn rules_shows_scopes_and_revoke_marks() {
        for cols in [160, NARROW] {
            let c = parse_case("rules");
            let t = text(&c, cols, 48);
            assert!(t.contains("Global"), "no global scope");
            assert!(t.contains("allow shell:git status"), "no global rule");
            assert!(t.contains("This project"), "no project scope");
            assert!(t.contains("ask shell:cargo publish"), "no project rule");
            assert!(t.contains("added 2d ago in review #688"), "no rule origin");
            assert!(t.contains("✕"), "no revoke mark");
            assert!(t.contains("Ctrl+G opens the rules file"), "no file hint");
            // Global reads before this project.
            assert!(
                t.find("Global").unwrap() < t.find("This project").unwrap(),
                "scopes out of order"
            );
        }
    }

    #[test]
    fn rules_revoke_marks_read_red() {
        let mut marks = 0;
        for r in rules_rows() {
            for s in &r.spans {
                if s.content == "✕" {
                    assert_eq!(s.style.fg, Some(crate::RED));
                    marks += 1;
                }
            }
        }
        assert_eq!(marks, 2, "not one ✕ per rule");
    }

    #[test]
    fn settings_header_row() {
        for cols in [160, NARROW] {
            let c = parse_case("settings");
            let buf = buffer(&c, cols, 48);
            assert_header(&buf, "Settings", "/settings", cols);
        }
    }

    #[test]
    fn settings_shows_keys_values_and_layers() {
        for cols in [160, NARROW] {
            let c = parse_case("settings");
            let t = text(&c, cols, 48);
            assert!(t.contains("key"), "no header row");
            assert!(
                t.contains("model               opus-5-5      global"),
                "no model row"
            );
            assert!(
                t.contains("thinking            high          project"),
                "no thinking row"
            );
            assert!(
                t.contains("budget.usd          20            project"),
                "no budget row"
            );
            assert!(
                t.contains("permissions.mode    auto          repository"),
                "no repository row"
            );
            assert!(
                t.contains("tools.timeout_s     120           default"),
                "no default row"
            );
            assert!(t.contains("needs a reload"), "no reload note");
            assert!(t.contains("Ctrl+G opens the file"), "no file hint");
        }
    }

    #[test]
    fn skills_header_row() {
        for cols in [160, NARROW] {
            let c = parse_case("skills");
            let buf = buffer(&c, cols, 48);
            assert_header(&buf, "Skills", "/skills", cols);
        }
    }

    #[test]
    fn skills_shows_sources_visibility_and_switches() {
        for cols in [160, NARROW] {
            let c = parse_case("skills");
            let t = text(&c, cols, 48);
            assert!(
                t.contains("review · checks a diff · repo · seen"),
                "no repo row"
            );
            assert!(t.contains("notes · personal notes"), "no personal name");
            assert!(t.contains("personal · hidden"), "no personal row");
            assert!(t.contains("shadows review"), "no shadowed skill");
            assert!(t.contains("[off]"), "no off switch");
            assert!(t.contains("Enter shows the text"), "no text hint");
            assert!(t.contains("Ctrl+G opens its file"), "no file hint");
        }
    }

    /// The cell of the first occurrence of `needle` in the buffer's text.
    fn cell_of(buf: &Buffer, cols: usize, rows: usize, needle: &str) -> (u16, u16) {
        let t = buf_text(buf, cols, rows);
        let y = t.lines().position(|l| l.contains(needle)).unwrap();
        let x = t.lines().nth(y).unwrap().find(needle).unwrap();
        (x as u16, y as u16)
    }

    #[test]
    fn opened_edit_shows_gutter_pairs_and_truncation() {
        for cols in [160, NARROW] {
            let c = parse_case("opened-edit");
            let t = text(&c, cols, 48);
            assert!(t.contains("Edited 1 file"), "no summary");
            assert!(t.contains("@@ -12,5 +12,6 @@"), "no hunk header");
            assert!(t.contains("  13 -    buf.set_line"), "no old-side gutter");
            assert!(t.contains("  13 +    buf.set_line"), "no new-side gutter");
            assert!(
                t.contains("… 4 more lines (Enter to view)"),
                "no overflow line"
            );
        }
        let c = parse_case("opened-edit");
        let buf = buffer(&c, 160, 48);
        for (needle, fg) in [
            ("&line,", crate::RED),
            ("&Line::from(spans),", crate::GREEN),
        ] {
            let (x, y) = cell_of(&buf, 160, 48, needle);
            let cell = &buf[(x, y)];
            assert!(
                cell.modifier.contains(Modifier::REVERSED),
                "{needle} is not reversed"
            );
            assert_eq!(cell.fg, fg, "{needle} lost its colour");
        }
    }

    #[test]
    fn opened_read_numbers_from_its_offset() {
        for cols in [160, NARROW] {
            let c = parse_case("opened-read");
            let t = text(&c, cols, 48);
            assert!(t.contains("Read 1 file"), "no summary");
            assert!(
                t.contains("  41 pub fn try_lock"),
                "the text is not numbered from the offset"
            );
            assert!(t.contains("  48 }"), "the gutter does not run on");
        }
    }

    #[test]
    fn opened_search_groups_matches_and_highlights() {
        for cols in [160, NARROW] {
            let c = parse_case("opened-search");
            let t = text(&c, cols, 48);
            let (h0, h1) = (
                t.find("crates/hub/src/lock.rs").unwrap(),
                t.find("crates/tui/src/main.rs").unwrap(),
            );
            assert!(h0 < h1, "matches are not grouped under each path");
            assert!(t.contains("  41 "), "no gutter");
            assert!(t.contains(" 1905 "), "no gutter");
        }
        let c = parse_case("opened-search");
        let buf = buffer(&c, 160, 48);
        let (x, y) = cell_of(&buf, 160, 48, "lock(path)");
        assert_eq!(buf[(x, y)].bg, crate::rgb(0x5a4a1a), "no match tint");
    }

    #[test]
    fn opened_shell_shows_both_closings() {
        for cols in [160, NARROW] {
            let c = parse_case("opened-shell");
            let t = text(&c, cols, 48);
            assert!(t.contains("$ cargo test -p log"), "no command");
            assert!(t.contains("exit 1"), "no failure closing");
            assert!(t.contains("killed by SIGTERM"), "no signal closing");
        }
        let c = parse_case("opened-shell");
        let buf = buffer(&c, 160, 48);
        let (x, y) = cell_of(&buf, 160, 48, "exit 1");
        assert_eq!(buf[(x, y)].fg, crate::RED, "exit 1 is not removed");
        let (x, y) = cell_of(&buf, 160, 48, "killed by SIGTERM");
        assert_eq!(buf[(x, y)].fg, crate::RED, "the signal is not removed");
    }

    #[test]
    fn opened_mcp_shows_text_and_indented_json() {
        for cols in [160, NARROW] {
            let c = parse_case("opened-mcp");
            let t = text(&c, cols, 48);
            assert!(t.contains("3 results for"), "no plain content");
            assert!(
                t.contains("  \"path\": \"notes/scratch.md\""),
                "no indented JSON"
            );
            assert!(t.contains("  \"saved\": true"), "no indented JSON");
        }
    }

    #[test]
    fn cut_shell_header_row() {
        for cols in [160, NARROW] {
            let c = parse_case("cut-shell");
            let buf = buffer(&c, cols, 48);
            assert_header(&buf, "Shell", "cargo test -p log", cols);
        }
    }

    #[test]
    fn cut_shell_shows_the_whole_output_around_its_marker() {
        for cols in [160, NARROW] {
            let c = parse_case("cut-shell");
            let t = text(&c, cols, 48);
            assert!(t.contains("$ cargo test -p log"), "no command");
            assert!(t.contains("running 42 tests"), "no output start");
            assert!(
                t.contains("end of the model's 16 KiB view"),
                "no cut marker"
            );
            assert!(t.contains("test result: ok"), "no output past the marker");
            assert!(t.contains("exit 0"), "no closing");
            // the marker reads dim
            let marker = cut_shell_rows()
                .into_iter()
                .find(|r| r.spans.iter().any(|s| s.content.contains("16 KiB")))
                .unwrap();
            assert!(
                marker
                    .spans
                    .iter()
                    .all(|s| s.style.add_modifier.contains(Modifier::DIM)),
                "the cut marker is not dim"
            );
        }
    }

    #[test]
    fn no_row_overflows_its_screen() {
        use crate::width;
        for name in CASES.iter().map(|c| c.name) {
            for (cols, rows) in [(160, 48), (NARROW, NARROW_ROWS)] {
                let c = crate::cases::lookup(CASES, name).unwrap();
                let fr = frame(&c, cols, rows);
                assert_eq!(fr.screen.len(), rows, "{name} is not {rows} rows");
                for (y, ps) in fr.screen.iter().enumerate() {
                    for p in ps {
                        assert!(
                            width(&p.row.spans) <= p.w as usize,
                            "{name} row {y} overflows its {w} columns",
                            w = p.w
                        );
                        assert!(
                            p.x + p.w <= cols as u16,
                            "{name} row {y} runs past the edge"
                        );
                    }
                }
            }
        }
    }
}
