//! Binary-level tests of the panel's look (`docs/tui.md`, "The panel",
//! "The narrow layout"): the real binary with a scripted session holding
//! three edited files, a running delegate, a running job and 268k of a
//! 400k handoff. The 160x48 truecolour run pins the slab's surface,
//! edges, padding and bar; the 100x40 run pins the narrow status rows;
//! the 256-colour run pins the panel tint's quantisation.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;

use fakes::{ProviderServer, Response, Watchdog};
use serde_json::json;
use support::pty::{Colour, Grid, Run};
use support::{Deadline, Setup, TRUECOLOUR, write_json};

/// The dark theme's surface, panel, attention and info in truecolour
/// (`crates/tui/src/theme.rs`).
const SURFACE: Colour = Colour::Rgb(26, 26, 34);
const PANEL: Colour = Colour::Rgb(12, 12, 17);
const ATTENTION: Colour = Colour::Rgb(255, 159, 67);
const INFO: Colour = Colour::Rgb(125, 211, 252);

/// The panel pinned to 34 columns on a 160-column screen: 21.25% rounds
/// to 34, kept from the 30-column floor to the 60-column ceiling
/// (`docs/tui.md`, "Layout").
const PANEL_SHARE: f64 = 21.25;

/// Quits the run's terminal and waits for it.
fn quit(mut run: Run) {
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

/// The held background job's release: writing a line lets its read
/// finish. Dropping the guard kills the job's group, so a failure leaks
/// nothing.
struct Release {
    file: fs::File,
    guard: Watchdog,
}

/// The cell index of `needle` in `row`, counting cells.
fn locate(row: &str, needle: &str) -> u16 {
    let chars: Vec<char> = row.chars().collect();
    let need: Vec<char> = needle.chars().collect();
    chars
        .windows(need.len().max(1))
        .position(|window| window == need.as_slice())
        .map(|at| at as u16)
        .unwrap_or_else(|| panic!("{needle:?} in {row:?}"))
}

/// The scripted turn: three edits, a delegate whose model hangs
/// mid-stream, a background shell call waiting on the hold FIFO, and a
/// text step reporting 268k of context, so the panel holds three changed
/// files, a running delegate, a running job and 268k of the 400k handoff
/// at once. The background call moves to a job at once, so no timing
/// decides when the count appears; writing a line to the FIFO releases
/// it, so nothing leaks.
fn script(job: &str) -> serde_json::Value {
    json!({"steps": [
        {"tool_calls": [
            {"name": "edit", "arguments": {"path": "a.txt",
                "edits": [{"old_text": "A1\n", "new_text": "A1\nA2\n"}]}},
            {"name": "edit", "arguments": {"path": "b.txt",
                "edits": [{"old_text": "B1\n", "new_text": "B1\nB2\n"}]}},
            {"name": "edit", "arguments": {"path": "c.txt",
                "edits": [{"old_text": "C1\n", "new_text": "C1\nC2\n"}]}},
        ]},
        {"tool_calls": [
            {"name": "delegate_spawn", "arguments": {
                "description": "review the diff",
                "prompt": "say hi",
                "model": "fiber:fake/m",
            }},
        ], "usage": {"input": 267000, "output": 1000}},
        {"tool_calls": [
            {"name": "shell", "arguments": {"command": job, "run_in_background": true}},
        ]},
        {"text": ["Done."]},
    ]})
}

/// A setup with the scripted session above: the files, the scripts, the
/// 400k handoff, the pinned panel, the fake provider serving the
/// delegate's hung stream, and a reviewer allowing every call, so no
/// approval stalls the turn. Returns the setup with its provider
/// server, held to the test's end, and the held job's release.
fn panel_setup() -> (Setup, ProviderServer, Release) {
    // A short root: the status line shows the whole workspace path, and
    // the narrow row must still reach the `ctx` segment in 99 columns.
    let root = fakes::TempDir::under(std::path::Path::new("/tmp"), "pl");
    fs::create_dir_all(root.path().join("h")).unwrap();
    fs::create_dir_all(root.path().join("w")).unwrap();
    let setup = Setup {
        root,
        deadline: Deadline::start(),
    };
    let workspace = setup.workspace();
    fs::write(workspace.join("a.txt"), "A1\n").unwrap();
    fs::write(workspace.join("b.txt"), "B1\n").unwrap();
    fs::write(workspace.join("c.txt"), "C1\n").unwrap();
    // The held shell call runs in the background at once and waits on
    // the FIFO: the test holds it open, so neither open blocks, and the
    // job outlives every assertion until the test writes its release.
    let hold = workspace.join("hold");
    fakes::fifo(&hold);
    let waiter = workspace.join("wait.sh");
    fs::write(&waiter, "read -r _ < hold\necho done\n").unwrap();
    let release = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&hold)
        .unwrap();
    let command = format!("sh {}", waiter.display());
    write_json(&workspace.join("s.json"), &script(&command));
    // The reviewer allows far more calls than the turn makes, so every
    // call runs without asking.
    write_json(
        &workspace.join("r.json"),
        &json!({"steps": [
            {"text": ["allow"]}, {"text": ["allow"]},
            {"text": ["allow"]}, {"text": ["allow"]},
            {"text": ["allow"]}, {"text": ["allow"]},
            {"text": ["allow"]}, {"text": ["allow"]},
            {"text": ["allow"]}, {"text": ["allow"]},
        ]}),
    );
    // The delegate's model hangs mid-stream: the delegate runs until
    // the test quits. Installed first: installing the fake provider
    // rewrites the config, so the session's own config lands after.
    let server = ProviderServer::start([Response::stall(
        200,
        "event: response.output_text.delta\ndata: {\"type\": \"response.output_text.delta\", \"delta\": \"working\"}\n\n",
        1000000,
    )])
    .unwrap();
    setup.provider(&server);
    write_json(
        &setup.home().join("config.json"),
        &json!({
            "model": "scripted/s.json",
            "reviewer": {"model": "scripted/r.json"},
            "handoff": {"tokens": 400000},
            "tui": {"panel": {"width": PANEL_SHARE}},
            "hub": {"idle_exit_ms": 200},
        }),
    );
    let rules = [
        json!({"decision": "allow", "tool": "delegate_spawn", "prefix": ""}),
        json!({"decision": "allow", "tool": "shell", "prefix": command}),
    ];
    fs::write(
        setup.home().join("rules"),
        rules
            .iter()
            .map(|rule| format!("{rule}\n"))
            .collect::<String>(),
    )
    .unwrap();
    // The job's guard kills its group if the test fails before the
    // release lets the read finish.
    let guard = Watchdog::matching(waiter.to_str().unwrap());
    (
        setup,
        server,
        Release {
            file: release,
            guard,
        },
    )
}

/// Starts the scripted turn and waits for the panel's whole state: both
/// running counts, the changed files and the Session card's own context-bar
/// row. The wait looks for the bar row itself because the turn footer also
/// reads "268.0k tokens" and lands earlier.
fn wait_panel_state(run: &mut Run) -> Grid {
    run.ready();
    run.write(b"go\r");
    run.wait_screen("the panel's whole state", |grid| {
        grid.contents.contains("review the diff")
            && grid.contents.matches("1 running").count() >= 2
            && grid.contents.contains("Changed files")
            && grid
                .rows
                .iter()
                .any(|row| row.contains("268.0k") && row.contains(&"▆".repeat(17)))
    })
}

/// Whether row `y` opens a slab: a run of 33 half-block top edges at
/// or past column 120, past any conversation card's edges.
fn slab_edge_row(grid: &Grid, y: u16) -> Option<(u16, u16)> {
    let chars: Vec<char> = grid.rows[y as usize].chars().collect();
    chars
        .windows(33)
        .position(|window| window.iter().all(|cell| *cell == '▄'))
        .filter(|at| *at >= 120)
        .map(|at| (at as u16, at as u16 + 33))
}

/// Duration text is normalised rather than compared to the wall clock.
fn delegate_duration(text: &str) -> bool {
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    match text.split(' ').collect::<Vec<_>>().as_slice() {
        [seconds] => seconds.strip_suffix('s').is_some_and(digits),
        [minutes, seconds] => {
            minutes.strip_suffix('m').is_some_and(digits)
                && seconds
                    .strip_suffix('s')
                    .is_some_and(|s| s.len() == 2 && digits(s))
        }
        _ => false,
    }
}

/// A whole dim progress row below the delegate's description, in the panel.
fn delegate_progress(grid: &Grid, tail: &str, finished: bool) -> bool {
    grid.rows.iter().enumerate().any(|(y, row)| {
        let Some(at) = row.find("review the diff") else {
            return false;
        };
        if at < 100 || (finished && !row.contains("✓ review the diff")) {
            return false;
        }
        let Some(next) = grid.rows.get(y + 1) else {
            return false;
        };
        let Some(start) = next.find("2 calls · ") else {
            return false;
        };
        let text = next[start..].trim_end();
        let parts: Vec<_> = text.split(" · ").collect();
        if parts.len() != 3
            || parts[0] != "2 calls"
            || !delegate_duration(parts[1])
            || parts[2] != tail
        {
            return false;
        }
        let x = u16::try_from(next[..start].chars().count()).unwrap();
        let y = u16::try_from(y + 1).unwrap();
        (0..u16::try_from(text.chars().count()).unwrap()).all(|offset| {
            let cell = grid.cell(x + offset, y);
            cell.dim && cell.fg == Colour::Default
        })
    })
}

#[test]
fn delegates_show_two_calls_and_the_last_subject_then_frozen_done_figures() {
    for (text, expected) in [
        ("0s", true),
        ("9s", true),
        ("12s", true),
        ("2m 10s", true),
        ("", false),
        ("s", false),
        ("-1s", false),
        ("1ms", false),
        ("2m 1s", false),
        ("2m 100s", false),
        ("m 10s", false),
        ("2m 1xs", false),
        ("2m 10s extra", false),
    ] {
        assert_eq!(delegate_duration(text), expected, "{text:?}");
    }
    let setup = Setup::new();
    let workspace = setup.workspace();
    fs::write(workspace.join("a.txt"), "hello\n").unwrap();
    let hold = workspace.join("hold");
    fakes::fifo(&hold);
    let mut release = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&hold)
        .unwrap();
    write_json(
        &workspace.join("s.json"),
        &json!({"steps": [
            {"tool_calls": [{"name": "delegate_spawn", "arguments": {
                "description": "review the diff", "prompt": "go", "model": "fiber:scripted/d.json"}}]},
            {"text": ["Waiting."]}, {"text": ["Done."]}
        ]}),
    );
    write_json(
        &workspace.join("d.json"),
        &json!({"steps": [
            {"tool_calls": [{"name": "read", "arguments": {"path": "a.txt"}}]},
            {"tool_calls": [{"name": "shell", "arguments": {"command": "head -n 1 hold"}}]},
            {"text": ["Reviewed."]}
        ]}),
    );
    write_json(
        &workspace.join("r.json"),
        &json!({"steps": [{"text": ["allow"]}, {"text": ["allow"]}, {"text": ["allow"]}]}),
    );
    write_json(
        &setup.home().join("config.json"),
        &json!({
            "model": "scripted/s.json", "reviewer": {"model": "scripted/r.json"},
            "roles": {"delegate": "fiber:scripted/d.json"},
            "tui": {"panel": {"width": 37.5}}, "hub": {"idle_exit_ms": 200}
        }),
    );
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n{}\n",
            json!({"decision": "allow", "tool": "delegate_spawn", "prefix": ""}),
            json!({"decision": "allow", "tool": "shell", "prefix": "head -n 1 hold"})
        ),
    )
    .unwrap();
    let guard = Watchdog::matching("head -n 1 hold");
    let mut run = Run::spawn(&setup, 160, 48, &[], &TRUECOLOUR);
    run.ready();
    run.write(b"go\r");
    run.wait_screen("the delegate's two calls and shell subject, dim", |grid| {
        delegate_progress(grid, "shell head -n 1 hold", false)
    });
    std::io::Write::write_all(&mut release, b"done\n").unwrap();
    run.wait_screen(
        "the finished delegate's calls, elapsed and done, dim",
        |grid| delegate_progress(grid, "done", true),
    );
    guard.stand_down(setup.deadline.cleanup_phase());
    quit(run);
}

#[test]
fn panel_cards_match_the_doc() {
    let (setup, _server, release) = panel_setup();
    // The session shows the canonical workspace, symlinks resolved.
    let workspace = fs::canonicalize(setup.workspace())
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let env: Vec<(&str, &str)> = [("FIBER_TEST_FAKE_KEY", "sk-test")]
        .into_iter()
        .chain(TRUECOLOUR)
        .collect();
    let mut run = Run::spawn(&setup, 160, 48, &[], &env);
    let grid = wait_panel_state(&mut run);
    // The first slab on screen is the Session card's: the pinned panel
    // ends 34 columns wide at the screen's right edge.
    let top = (0u16..48)
        .find(|y| slab_edge_row(&grid, *y).is_some())
        .unwrap();
    let (slab, _) = slab_edge_row(&grid, top).unwrap();
    let right = slab + 33;
    assert_eq!(right, 160, "the slab ends at the screen's edge");
    // The pinned 34-column panel with the slab one past its drag edge.
    assert_eq!(slab - 1, 160 - 34, "the pinned 34-column panel");

    // The edges: half blocks in the surface colour over the panel
    // colour, across the slab past the drag edge.
    for x in slab..right {
        let cell = grid.cell(x, top);
        assert_eq!(cell.symbol.as_str(), "▄", "edge cell ({x}, {top})");
        assert_eq!(cell.fg, SURFACE, "edge cell ({x}, {top})");
        assert_eq!(cell.bg, PANEL, "edge cell ({x}, {top})");
    }
    let bottom = (top..48)
        .find(|y| {
            grid.rows[*y as usize]
                .chars()
                .skip(slab as usize)
                .take(33)
                .all(|cell| cell == '▀')
        })
        .unwrap_or_else(|| panic!("a slab bottom edge"));
    for x in slab..right {
        let cell = grid.cell(x, bottom);
        assert_eq!(cell.symbol.as_str(), "▀");
        assert_eq!(cell.fg, SURFACE);
        assert_eq!(cell.bg, PANEL);
    }

    // The Session title: the directory with `auto` in attention ending
    // one column before the slab's right edge, the text two columns past
    // the slab.
    let title = top + 1;
    let text: String = (slab + 2..right - 1)
        .map(|x| grid.cell(x, title).symbol)
        .collect();
    let cells = workspace.chars().count();
    let want = if cells + 5 > 30 {
        let kept: String = workspace
            .chars()
            .rev()
            .take(24)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        format!("…{kept}")
    } else {
        workspace.clone()
    };
    assert!(text.starts_with(&want), "{text:?}");
    assert!(text.trim_end().ends_with("auto"), "{text:?}");
    let row = &grid.rows[title as usize];
    let auto = locate(row, "auto");
    assert_eq!(auto, right - 5, "auto ends one past: {row:?}");
    for (n, expected) in "auto".chars().enumerate() {
        let cell = grid.cell(auto + n as u16, title);
        assert_eq!(cell.symbol, expected.to_string());
        assert_eq!(cell.fg, ATTENTION, "auto cell {n}");
    }
    // Every text row sits on the surface tint.
    for y in top + 1..bottom {
        for x in slab..right {
            assert_eq!(grid.cell(x, y).bg, SURFACE, "slab cell ({x}, {y})");
        }
    }

    // The blank row after the card shows the panel tint.
    let blank = bottom + 1;
    assert!(
        (slab - 1..right).all(|x| grid.cell(x, blank).symbol.as_str() == " "),
        "a blank panel row"
    );
    for x in slab - 1..right {
        assert_eq!(grid.cell(x, blank).bg, PANEL, "blank cell ({x}, {blank})");
    }

    // The context bar: 17 cells of `▆`, eleven in info toward the 400k
    // handoff, then the marker.
    let bar = (top + 1..bottom)
        .find(|y| {
            grid.rows[*y as usize].contains("268.0k")
                && (0..17).all(|n| grid.cell(slab + 2 + n, *y).symbol.as_str() == "▆")
                && grid.cell(slab + 2 + 17, *y).symbol.as_str() == "│"
        })
        .unwrap_or_else(|| panic!("the context bar"));
    let fill = (0..17)
        .filter(|n| grid.cell(slab + 2 + n, bar).fg == INFO)
        .count();
    assert_eq!(fill, 11, "eleven cells toward the handoff");

    // No empty track anywhere in the panel's columns.
    for y in 0..48 {
        for x in slab - 1..right {
            assert_ne!(
                grid.cell(x, y).symbol.as_str(),
                "░",
                "track cell ({x}, {y})"
            );
        }
    }

    let Release {
        file: release,
        guard,
    } = release;
    std::io::Write::write_all(&mut &release, b"\n").unwrap();
    guard.stand_down(setup.deadline.cleanup_phase());
    quit(run);
}

#[test]
fn narrow_status_rows_match_the_doc() {
    let (setup, _server, release) = panel_setup();
    let env: Vec<(&str, &str)> = [("FIBER_TEST_FAKE_KEY", "sk-test")]
        .into_iter()
        .chain(TRUECOLOUR)
        .collect();
    let mut run = Run::spawn(&setup, 100, 40, &[], &env);
    run.ready();
    run.write(b"go\r");
    // Both status rows: the session's segments, then the running counts.
    let grid = run.wait_screen("the narrow status rows", |grid| {
        grid.contents.contains("ctx")
            && grid.contents.contains("1 delegate running")
            && grid.contents.contains("1 job running")
    });
    // The last two content rows: the session's segments, then the
    // running counts.
    let second = (0u16..40)
        .rev()
        .find(|y| grid.rows[*y as usize].contains("1 job running"))
        .unwrap_or_else(|| panic!("a counts row"));
    let first = second - 1;
    assert!(
        grid.rows[first as usize].contains("ctx"),
        "the segments row"
    );
    // Each status row sits on the panel tint one column in: the middle
    // of the row carries the tint, past the grip and the cut edge.
    for y in [first, second] {
        for x in 1..90 {
            assert_eq!(grid.cell(x, y).bg, PANEL, "tint cell ({x}, {y})");
        }
    }
    // The first row runs to the terminal's right edge: cut at 99.
    let line: String = (1..100).map(|x| grid.cell(x, first).symbol).collect();
    let line = line.trim_end();
    assert_eq!(line.chars().count(), 99, "{line:?}");
    assert!(line.contains("  │  "), "{line:?}");
    let at = locate(line, "  │  ");
    for x in 1 + at..1 + at + 5 {
        assert!(grid.cell(x, first).dim, "joiner cell ({x}, {first})");
    }
    let ctx = locate(line, "ctx");
    assert_eq!(
        (0..8)
            .filter(|n| grid.cell(1 + ctx + 4 + n, first).symbol.as_str() == "▆")
            .count(),
        8,
        "{line:?}"
    );
    let Release {
        file: release,
        guard,
    } = release;
    std::io::Write::write_all(&mut &release, b"\n").unwrap();
    guard.stand_down(setup.deadline.cleanup_phase());
    quit(run);
}

#[test]
fn panel_tint_quantises_to_233() {
    // 256 colours: xterm without COLORTERM.
    let (setup, _server, release) = panel_setup();
    let mut run = Run::spawn(
        &setup,
        160,
        48,
        &[],
        &[
            ("FIBER_TEST_FAKE_KEY", "sk-test"),
            ("TERM_PROGRAM", "ghostty"),
        ],
    );
    let grid = wait_panel_state(&mut run);
    // The blank row after a card carries the panel tint, indexed 233.
    let blank = (0..48)
        .find(|y| {
            let row = &grid.rows[*y as usize];
            row.chars().skip(126).all(|cell| cell == ' ')
                && (126..160).any(|x| grid.cell(x, *y).bg == Colour::Indexed(233))
        })
        .unwrap_or_else(|| panic!("a blank panel row"));
    for x in 126..160 {
        assert_eq!(
            grid.cell(x, blank).bg,
            Colour::Indexed(233),
            "blank cell ({x}, {blank})"
        );
    }
    let Release {
        file: release,
        guard,
    } = release;
    std::io::Write::write_all(&mut &release, b"\n").unwrap();
    guard.stand_down(setup.deadline.cleanup_phase());
    quit(run);
}
