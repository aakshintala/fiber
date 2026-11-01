//! Binary-level open test (`docs/testing.md`, "Levels"): the built
//! `fiber resume` on a seeded ten-turn session draws the last rows on
//! its first frame — no earlier row shows before the scroll bar's
//! thumb — and a live prompt after the thumb appends below the last
//! rows. Header as `wheel_scroll.rs` (reads).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;

use serde_json::json;
use support::pty::{FINISHED_TITLE, Run};
use support::{Setup, session_dir, write_json};

/// The seeded session: ten turns and a running one, from `tui`.
const ID: &str = "s_0000000000000001";
/// The pty size the open draws on.
const COLS: u16 = 160;
const ROWS: u16 = 48;
/// The scroll bar's thumb: `tui` keeps `view::scroll_bar::THUMB`
/// crate-private, so the test names its glyph.
const THUMB: &str = "┃";
/// The running turn's streamed reply: the last rows hold it.
const LAST: &str = "streamed text, a few words at a time; ";
/// The first turn's prompt: no earlier row may show before the thumb.
const FIRST: &str = "the session log holds every durable line";
/// The seeded model id (`seed` writes it into `config.json`): the box
/// row's model segment, absent before the session status lands.
const MODEL: &str = "scripted/s.json";

/// Splits `part` at its first run of two spaces: the mode or model
/// before the gap from what follows it. Byte-safe: the split lands on
/// ASCII spaces.
fn split_gap(part: &str) -> (&str, &str) {
    let at = part
        .find("  ")
        .unwrap_or_else(|| panic!("a gap in {part:?}"));
    (part[..at].trim(), part[at..].trim())
}

/// The box status row with its two data-dependent segments factored
/// out: the workspace path (a fresh tmp dir per run, absent before the
/// session status lands) and the model. The mode and ctx segments keep
/// their text exactly, rebuilt with the row's own joiner; any other
/// shape is a real change and panics instead of passing silently.
fn steady_status_row(row: &str, workspace: &str) -> String {
    const JOINER: &str = "  │  ";
    let parts: Vec<&str> = row.split(JOINER).collect();
    let (mode, ctx) = match parts.as_slice() {
        // No joiners: the mode alone on the left, the ctx segment on
        // the right, split by the gap between the sides.
        [single] => {
            let (mode, ctx) = split_gap(single.trim());
            if !ctx.starts_with("ctx") {
                panic!("the ctx segment in {row:?}");
            }
            (mode, ctx)
        }
        // One joiner: either the workspace beside the mode, or the
        // model beside the ctx segment.
        [first, second] if first.trim_start() == workspace => {
            let (mode, ctx) = split_gap(second);
            if !ctx.starts_with("ctx") {
                panic!("the ctx segment in {row:?}");
            }
            (mode, ctx)
        }
        [first, second] => {
            let (mode, model) = split_gap(first.trim_start());
            if model != MODEL {
                panic!("the model segment in {row:?}");
            }
            if !second.trim_start().starts_with("ctx") {
                panic!("the ctx segment in {row:?}");
            }
            (mode, second.trim())
        }
        // Both segments present: the workspace, then the mode and the
        // model sharing the middle, then the ctx segment.
        [first, middle, last] => {
            if first.trim_start() != workspace {
                panic!("the workspace segment in {row:?}");
            }
            let mut middle = middle.split_whitespace();
            let mode = middle.next().unwrap_or_else(|| panic!("a mode in {row:?}"));
            if middle.next_back() != Some(MODEL) {
                panic!("the model segment in {row:?}");
            }
            if middle.next().is_some() {
                panic!("only a mode and a model in {row:?}");
            }
            if !last.trim_start().starts_with("ctx") {
                panic!("the ctx segment in {row:?}");
            }
            (mode, last.trim())
        }
        _ => panic!("the box status row's two sides in {row:?}"),
    };
    if mode.is_empty() || ctx.is_empty() {
        panic!("a mode and a ctx segment in {row:?}");
    }
    // The figures after the bar (a size and share, or dashes) arrive a
    // frame late, so the key drops them the way the workspace and model
    // are dropped above. The bar stays exact, and the doc-fixed slot
    // width (17 cells in rule, known or not) stays in their place, so a
    // reflow fails instead of passing.
    let mut head = ctx.split_whitespace();
    let head = format!(
        "{} {}",
        head.next()
            .unwrap_or_else(|| panic!("a ctx bar in {row:?}")),
        head.next()
            .unwrap_or_else(|| panic!("a ctx bar in {row:?}")),
    );
    format!("{mode}{JOINER}{head} 17")
}
/// The scripted reply to the live prompt.
const REPLY: &str = "the scripted reply";
/// A synchronized frame ends here; splitting on it yields whole frames.
const FRAME_END: &[u8] = b"\x1b[?2026l";

/// Seeds the session log and its `recent.jsonl` row: the ten-turn log
/// from `tui` plus one exited row, as a listed session seeds its row.
fn seed(setup: &Setup) {
    let dir = session_dir(setup, ID);
    fs::create_dir_all(&dir).unwrap();
    let workspace = fs::canonicalize(setup.workspace()).unwrap();
    let log = tui::paging_session(1)
        .lines()
        .filter_map(|line| {
            let mut event: serde_json::Value = serde_json::from_str(line).unwrap();
            event.get("seq")?;
            if event.get("kind").and_then(|kind| kind.as_str()) == Some("session_started") {
                event["payload"]["workspace"] =
                    serde_json::Value::String(workspace.to_string_lossy().into_owned());
                // A `session_started` without `variables` does not parse
                // as one, so `resolve` finds no session: the seed writes
                // the schema-valid payload the bench writes in
                // `wheel.rs:create_log`.
                event["payload"]["variables"] = json!({
                    "path": "",
                    "names": [],
                    "source": "inherited",
                });
            }
            // The jig's envelopes name its own session, whose lines the
            // open folds nowhere: the attached session drops every line
            // another session sent, so the pass lands empty and the
            // screen goes blank. The seed renames them to the fixture.
            event["session_id"] = json!(ID);
            if event.get("kind").and_then(|kind| kind.as_str()) == Some("tool_call_started") {
                // The jig's `tool_call_started` holds `{}`, which does
                // not read as `DeclaredEffects`: the seed writes the
                // empty declaration, as a call declaring none does.
                event["payload"]["effects"] = json!([]);
                event["payload"]["reversible"] = json!(true);
            }
            if event.get("kind").and_then(|kind| kind.as_str()) == Some("usage_recorded") {
                // The jig's `usage_recorded` holds bench counts, not a
                // call's record, so the fold refuses the log as missing
                // `generation_id`: the seed rewrites the schema-valid
                // record the model host writes in `model_host.rs:emit`.
                let seq = event.get("seq").cloned().unwrap_or(json!(0));
                let payload = event.get_mut("payload").unwrap();
                payload["generation_id"] = serde_json::Value::String(format!("fiber-seed-{seq}"));
                payload["model"] = json!("scripted/s.json");
                payload["tokens"] = json!({
                    "input": 1200,
                    "cache_read": 0,
                    "cache_write": {},
                    "output": 300,
                });
                payload["input_bytes"] = json!(1200);
                payload["cost"] = serde_json::Value::Null;
                for stale in ["input_tokens", "output_tokens"] {
                    payload.as_object_mut().unwrap().remove(stale);
                }
            }
            Some(serde_json::to_string(&event).unwrap())
        })
        .collect::<Vec<_>>();
    // The fold refuses a line whose payload does not read as its kind,
    // one field at a time, so the seed reads every seeded envelope back
    // with the contract's own event type before writing: a drifted jig
    // payload fails here, naming its kind, not on the first frame.
    for line in &log {
        let envelope: contract::Envelope = serde_json::from_str(line).unwrap();
        match contract::events::Event::from_envelope(&envelope) {
            Ok(Some(_)) => {}
            Ok(None) => panic!("the seed holds kind {}", envelope.kind),
            Err(error) => panic!("the seed's {} does not read: {error}", envelope.kind),
        }
    }
    fs::write(dir.join("events.jsonl"), log.join("\n") + "\n").unwrap();
    let key = dir
        .parent()
        .and_then(|sessions| sessions.parent())
        .and_then(|projects| projects.file_name())
        .and_then(|key| key.to_str())
        .unwrap_or_else(|| panic!("the project key in {dir:?}"));
    let row = json!({"session_id": ID, "ts": 1, "project": key,
        "workspace": workspace, "name": "paging", "how": "exited"});
    fs::write(setup.home().join("recent.jsonl"), format!("{row}\n")).unwrap();
    write_json(
        &setup.workspace().join("s.json"),
        &json!({"steps": [{"text": [REPLY]}]}),
    );
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 200},
            "tui": {"rail": {"shown": false}}}),
    );
}

/// The whole frames in `output`: the bytes through each frame end, in
/// order. A trailing partial frame is still being written, so it is
/// dropped.
fn frames(output: &[u8]) -> Vec<&[u8]> {
    let mut frames = Vec::new();
    let mut at = 0;
    while let Some(end) = output
        .get(at..)
        .and_then(|rest| rest.windows(FRAME_END.len()).position(|w| w == FRAME_END))
    {
        let stop = at + end + FRAME_END.len();
        frames.push(&output[at..stop]);
        at = stop;
    }
    frames
}

/// One frame's screen rows as text, replayed cumulatively: every frame
/// redraws only what changed, so a frame alone never parses.
fn screens(output: &[u8]) -> Vec<Vec<String>> {
    let mut parser = vt100::Parser::new(ROWS, COLS, 0);
    frames(output)
        .iter()
        .map(|frame| {
            parser.process(frame);
            let screen = parser.screen();
            (0..ROWS)
                .map(|y| {
                    (0..COLS)
                        .map(|x| {
                            screen
                                .cell(y, x)
                                .map(|cell| cell.contents())
                                .unwrap_or_default()
                        })
                        .collect::<String>()
                })
                .collect()
        })
        .collect()
}

/// Whether any row holds `needle`.
fn shows(rows: &[String], needle: &str) -> bool {
    rows.iter().any(|row| row.contains(needle))
}

#[test]
fn the_first_frame_shows_the_last_rows_and_no_frame_shows_others_before_the_bar() {
    let setup = Setup::new();
    seed(&setup);
    let mut run = Run::spawn(
        &setup,
        COLS,
        ROWS,
        &["resume", ID],
        &[("FIBER_TEST_FAKE_KEY", "sk-test")],
    );
    // The end of the first frame proves the input reader runs before
    // any typing goes out.
    run.ready();
    run.wait_screen("the last turn's rows", |grid| {
        grid.contents.contains(LAST) && !grid.contents.contains(FIRST)
    });
    run.wait_screen("the scroll bar's thumb", |grid| {
        grid.contents.contains(THUMB)
    });
    let screens = screens(&run.output());
    let first_last = screens
        .iter()
        .position(|rows| shows(rows, LAST))
        .unwrap_or_else(|| panic!("a frame holding the last rows"));
    let first_thumb = screens
        .iter()
        .position(|rows| shows(rows, THUMB))
        .unwrap_or_else(|| panic!("a frame holding the thumb"));
    assert!(
        first_thumb >= first_last,
        "the thumb lands on the last rows, not before them"
    );
    for (at, rows) in screens.iter().enumerate().take(first_thumb + 1) {
        assert!(
            !shows(rows, FIRST),
            "frame {at} shows the first turn's rows before the bar"
        );
        assert!(
            !shows(rows, THUMB) || at == first_thumb,
            "frame {at} shows the thumb early"
        );
    }
    // The thumb overlays an empty bar cell, and the session panel
    // overlays the conversation's top right while the pass folds (its
    // rows and counts move until the bar lands), so without both every
    // frame from the last rows to the bar draws the same conversation.
    // The panel keeps rows 1-11, cols 127+, at this width; the corner
    // below blanks it with room on every side. The bar draws left of
    // it, and the watched rows sit below it, so neither is hidden.
    const PANEL_ROWS: usize = 16;
    const PANEL_AT: usize = 120;
    let clean = |rows: &[String]| {
        rows.iter()
            .enumerate()
            .map(|(y, row)| {
                let row = row.replace(THUMB, " ");
                if y < PANEL_ROWS {
                    row.chars()
                        .enumerate()
                        .map(|(x, cell)| if x >= PANEL_AT { ' ' } else { cell })
                        .collect()
                } else {
                    row
                }
            })
            .collect::<Vec<_>>()
    };
    // The session status lands after the first frame, so the box row's
    // workspace and model segments vary frame to frame; `seed` fixes
    // both values for the run. Canonicalize the same way `seed` does.
    let workspace = fs::canonicalize(setup.workspace())
        .unwrap_or_else(|err| panic!("a canonical workspace: {err}"))
        .to_string_lossy()
        .into_owned();
    // The pass lands the fold under the preview, and the transient
    // `resumed` aside drops out, shifting every row above the composer
    // by one. What must not move is the running turn's tail, so the
    // comparison anchors at its cut-short card and skips the aside row.
    let tail = |rows: &[String]| {
        let cleaned = clean(rows);
        let from = cleaned
            .iter()
            .position(|row| row.contains("cut short"))
            .unwrap_or_else(|| panic!("a frame holding the cut-short card"));
        let mut tail: Vec<String> = cleaned[from..]
            .iter()
            .filter(|row| !row.contains("resumed"))
            .cloned()
            .collect();
        // The status row sits second to last, above the box's bottom
        // edge: factor its two data-dependent segments out, keeping the
        // mode and the ctx bar and width exact. Anything else there fails
        // here.
        let at = tail
            .len()
            .checked_sub(2)
            .unwrap_or_else(|| panic!("a box under the tail"));
        if !tail[at].contains("ctx") {
            panic!("the status row above the box edge: {:?}", tail[at]);
        }
        let row = std::mem::take(&mut tail[at]);
        tail[at] = steady_status_row(&row, &workspace);
        // The box top edge takes the scroll bar's left margin once the
        // bar lands, so the key drops the margin the way the status row
        // drops its figures; the edge must still be an unbroken rule of
        // its own glyph, so a moved or redrawn edge fails here.
        let edge = tail[1].trim();
        if edge.is_empty() || !edge.chars().all(|cell| cell == '▀') {
            panic!("the box top edge in {:?}", tail[1]);
        }
        tail[1] = String::from("box edge");
        tail
    };
    let first = tail(&screens[first_last]);
    for (at, rows) in screens
        .iter()
        .enumerate()
        .take(first_thumb + 1)
        .skip(first_last)
    {
        assert_eq!(
            tail(rows),
            first,
            "frame {at} moves the conversation before the bar"
        );
    }
}

#[test]
fn a_running_session_appends_live_lines_below_the_last_rows() {
    let setup = Setup::new();
    seed(&setup);
    let mut run = Run::spawn(
        &setup,
        COLS,
        ROWS,
        &["resume", ID],
        &[("FIBER_TEST_FAKE_KEY", "sk-test")],
    );
    run.ready();
    run.wait_screen("the last turn's rows", |grid| {
        grid.contents.contains(LAST) && !grid.contents.contains(FIRST)
    });
    run.wait_screen("the scroll bar's thumb", |grid| {
        grid.contents.contains(THUMB)
    });
    let from = run.output().len();
    run.write(b"say hi\r");
    // The finished title after the typing proves the live prompt's own
    // turn completed, not an earlier one.
    run.wait_bytes(from, FINISHED_TITLE, "the live turn's finished title");
    let screen = run.wait_screen("the live reply below the last rows", |grid| {
        let contents = &grid.contents;
        match (contents.find(LAST), contents.find(REPLY)) {
            (Some(last), Some(reply)) => last < reply,
            _ => false,
        }
    });
    let contents = &screen.contents;
    // The resume closes the running turn as cut short (`turn/crash.rs`):
    // the settled view keeps that card, not a `fiber_started` row. It
    // sits below the last rows, and the live reply sits below it.
    let last = contents.find(LAST).unwrap_or(usize::MAX);
    let cut = contents.find("cut short").unwrap_or(usize::MAX);
    let reply = contents.find(REPLY).unwrap_or(0);
    assert!(
        last < cut && cut < reply,
        "the resume's cut-short card sits below the last rows and above the live reply: {contents:?}"
    );
}
