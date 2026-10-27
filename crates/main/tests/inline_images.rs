//! Binary-level test of inline images in ledger rows (`docs/tui.md`,
//! "Images"): the real binary on a pixel-reporting pty in truecolour,
//! with a scripted provider. One turn reads a PNG: the ledger shows its
//! placeholder grid and caption, one transmit crosses a redraw and a
//! scroll away and back, a click opens the viewer, and the next session
//! deletes the image before showing its own. With the flag off the same
//! image stays one clickable line and nothing transmits.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use support::pty::{Grid, Run};
use support::{Setup, TRUECOLOUR, write_json};

/// The first row containing `needle`.
fn find_row(rows: &[String], needle: &str) -> Option<u16> {
    rows.iter()
        .position(|row| row.contains(needle))
        .map(|y| u16::try_from(y).expect("the screen fits in a u16"))
}

/// A solid-colour 120 by 80 PNG fixture.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x78, 0x00, 0x00, 0x00, 0x50, 0x08, 0x02, 0x00, 0x00, 0x00, 0x5d, 0xf9, 0x26,
    0xde, 0x00, 0x00, 0x00, 0x82, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0xed, 0xd0, 0x31, 0x0d, 0x00,
    0x00, 0x08, 0x03, 0xb0, 0xbd, 0xb8, 0xc5, 0x27, 0xa6, 0x70, 0x41, 0x38, 0x9a, 0x54, 0x41, 0x53,
    0x3d, 0x1c, 0x88, 0x02, 0xd1, 0xa2, 0x11, 0x2d, 0x5a, 0xb4, 0x05, 0xd1, 0xa2, 0x11, 0x2d, 0x5a,
    0xb4, 0x05, 0xd1, 0xa2, 0x11, 0x2d, 0x5a, 0x34, 0xa2, 0x45, 0x23, 0x5a, 0xb4, 0x68, 0x44, 0x8b,
    0x46, 0xb4, 0x68, 0xd1, 0x88, 0x16, 0x8d, 0x68, 0xd1, 0xa2, 0x11, 0x2d, 0x1a, 0xd1, 0xa2, 0x45,
    0x23, 0x5a, 0x34, 0xa2, 0x45, 0x8b, 0x46, 0xb4, 0x68, 0x44, 0x8b, 0x16, 0x8d, 0x68, 0xd1, 0x88,
    0x16, 0x2d, 0x1a, 0xd1, 0xa2, 0x11, 0x2d, 0x5a, 0x34, 0xa2, 0x45, 0x23, 0x5a, 0xb4, 0x68, 0x44,
    0x8b, 0x46, 0xb4, 0x68, 0xd1, 0x88, 0x16, 0x8d, 0x68, 0xd1, 0xa2, 0x11, 0x2d, 0x1a, 0xd1, 0xa2,
    0x45, 0x23, 0x5a, 0x34, 0xa2, 0x45, 0x8b, 0x46, 0xb4, 0x68, 0x44, 0x8b, 0x16, 0x8d, 0x68, 0xd1,
    0x88, 0x16, 0x2d, 0x1a, 0xd1, 0xa2, 0x11, 0x2d, 0x5a, 0x34, 0xa2, 0x45, 0x23, 0x5a, 0xb4, 0x68,
    0x44, 0x8b, 0x46, 0xb4, 0x68, 0xd1, 0x88, 0x16, 0x8d, 0x68, 0xd1, 0xa2, 0x11, 0x2d, 0x1a, 0xd1,
    0xa2, 0x45, 0x23, 0x5a, 0x34, 0xa2, 0xff, 0x58, 0xd9, 0xb8, 0x78, 0xc2, 0xab, 0x96, 0xaa, 0xe8,
    0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

/// A click at cell (`x`, `y`).
fn click(run: &mut Run, x: u16, y: u16) {
    run.write(format!("\x1b[<0;{};{}M\x1b[<0;{};{}m", x + 1, y + 1, x + 1, y + 1).as_bytes());
}

/// Clicks the first cell of `text` on `grid`.
fn click_text(run: &mut Run, grid: &Grid, text: &str) {
    let row = find_row(&grid.rows, text).expect("the text row");
    let col = u16::try_from(
        grid.rows[usize::from(row)]
            .find(text)
            .expect("the text column"),
    )
    .expect("the row fits in a u16");
    click(run, col, row);
}

/// Scrolls one step at a time until `done` holds on a complete frame.
fn wheel_until(run: &mut Run, up: bool, what: &str, done: impl Fn(&Grid) -> bool) -> Grid {
    let event: &[u8] = if up {
        b"\x1b[<64;51;16M"
    } else {
        b"\x1b[<65;51;16M"
    };
    let mut grid = run.screen();
    for _ in 0..40 {
        if done(&grid) {
            return grid;
        }
        let from = run.output().len();
        run.write(event);
        run.wait_bytes(from, FRAME_END, "the scroll frame");
        grid = run.screen();
    }
    panic!(
        "did not reach {what} after 40 wheel steps; grid:\n{}",
        grid.rows.join("\n")
    );
}

const FRAME_END: &[u8] = b"\x1b[?2026l";

/// The scripted turns: `reads` distinct PNG reads, each its own step so
/// the completions land in order, then closing text, then the same
/// session's text-only turn. With `with_tail` the closing text carries
/// a long tail below the images: the scroll room the single-image
/// journeys scroll through. The many-reads journey needs no scroll
/// room: its group row must stay on screen under the closing text.
fn script_reads(setup: &Setup, inline: bool, reads: usize, with_tail: bool) {
    for n in 0..reads {
        fs::write(setup.workspace().join(format!("pic-{n:03}.png")), PNG).unwrap();
    }
    let mut steps: Vec<Value> = (0..reads)
        .map(|n| {
            json!({"tool_calls": [{"name": "read", "arguments": {"path": format!("pic-{n:03}.png")}}]})
        })
        .collect();
    if with_tail {
        let tail = (1..=30)
            .map(|n| format!("- T{n:04}"))
            .collect::<Vec<_>>()
            .join("\n");
        steps.push(json!({"text": ["Seen.", tail]}));
    } else {
        steps.push(json!({"text": ["Seen."]}));
    }
    steps.push(json!({"text": ["Second."]}));
    write_json(&setup.workspace().join("s.json"), &json!({"steps": steps}));
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 1000},
            "tui": {"inline_images": inline}}),
    );
}

/// The scripted turns: a `read` of one PNG, its closing text with a
/// long tail below the image, then the same session's text-only turn.
fn script(setup: &Setup, inline: bool) {
    script_reads(setup, inline, 1, true);
}

/// A fake `xdg-open`/`open` first on the child's PATH, recording its
/// arguments in `marker`: the viewer the caption click queues.
fn viewer_shim(setup: &Setup, marker: &Path) -> String {
    let bin = setup.root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let body = format!("printf '%s\\n' \"$@\" >> '{}'", marker.to_str().unwrap());
    fakes::script(&bin, "xdg-open", &body);
    fakes::script(&bin, "open", &body);
    std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .unwrap()
    .into_string()
    .unwrap()
}

/// The binary on a 100 by 30 terminal reporting 1000 by 600 pixels, so
/// the cell is 10 by 20, with `env` after the fake key.
fn terminal(setup: &Setup, env: &[(&str, &str)]) -> Run {
    let mut full = vec![("FIBER_TEST_FAKE_KEY", "sk-test")];
    full.extend(env.iter().copied());
    full.extend(TRUECOLOUR.iter().copied());
    Run::spawn_pixels(setup, 100, 30, 1000, 600, &[], &full)
}

/// The started turn finished: approval allowed, the turn done. The
/// done signal is the turn's completed row, which the follow keeps in
/// view no matter how long the turn's tail grows.
fn started(run: &mut Run) {
    let mut from = run.ready();
    run.write(b"go\r");
    loop {
        let grid = run.wait_screen("approval or done", |grid| {
            grid.contents.contains("Allow once") || grid.contents.contains("▣ completed")
        });
        if grid.contents.contains("▣ completed") {
            break;
        }
        let approval = run.wait_bytes(from, b"Allow once", "the approval prompt");
        from = run.wait_bytes(approval, FRAME_END, "the approval frame");
        run.write(b"\r");
    }
    run.turn_finished(from);
}

/// The session directories under the workspace, oldest first.
fn sessions(setup: &Setup) -> Vec<PathBuf> {
    let dir = log::sessions_dir(&setup.home(), &doors::project(&setup.workspace()));
    let mut ids: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.join("events.jsonl").is_file())
        .collect();
    ids.sort();
    ids
}

/// Opens the ledger's `read` call on its settled scroll frame. The
/// closed call draws no placeholder until after this click.
fn open_call(run: &mut Run, grid: &Grid, name: &str) -> usize {
    let click_from = run.output().len();
    click_text(run, grid, name);
    run.wait_bytes(click_from, FRAME_END, "the opened call frame");
    click_from
}

/// One placeholder cell on the wire: the harness grid parser drops the
/// private-use glyph, so the test waits for its bytes instead.
const PLACEHOLDER: &[u8] = "\u{10EEEE}".as_bytes();

/// Counts `needle` in `haystack`.
fn count(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

/// The first offset of `needle` at or after `from`.
fn find_at(haystack: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    haystack[from..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|at| from + at)
}

/// The logged tool image's path and size, read under the setup's
/// deadline: the hub writes the log after the turn's close paints. The
/// read pipeline stores the image under a fresh stem, so the test
/// reads the logged path back instead of guessing it.
fn image_size(setup: &Setup, events: &Path) -> (String, u64, u64) {
    let events = events.to_owned();
    let waiting = events.clone();
    fakes::wait_until("the logged image", setup.deadline, move || {
        fs::read_to_string(&waiting)
            .ok()
            .is_some_and(|text| logged_image(&text).is_some())
    });
    let text = fs::read_to_string(events).expect("the session log");
    logged_image(&text).expect("the logged image")
}

/// The logged path and size of every image part in `events`' tool
/// completions, in log order.
fn logged_images(text: &str) -> Vec<(String, u64, u64)> {
    let mut out = Vec::new();
    for line in support::complete_lines(text) {
        if line.get("kind").and_then(Value::as_str) != Some("tool_call_completed") {
            continue;
        }
        let content = line
            .get("payload")
            .and_then(|payload| payload.get("content"))
            .and_then(Value::as_array);
        for part in content.into_iter().flatten() {
            if part.get("type").and_then(Value::as_str) == Some("image")
                && let (Some(path), Some(width), Some(height)) = (
                    part.get("path").and_then(Value::as_str),
                    part.get("width").and_then(Value::as_u64),
                    part.get("height").and_then(Value::as_u64),
                )
            {
                out.push((path.to_owned(), width, height));
            }
        }
    }
    out
}

/// The logged path and size of the first image part in `events`' tool
/// completions, when one has landed.
fn logged_image(text: &str) -> Option<(String, u64, u64)> {
    logged_images(text).into_iter().next()
}

/// The logged path and size of the last of `reads` image parts in
/// `events`, read under the setup's deadline: the hub logs each read's
/// image as its tool completes, so the last logged image is the one
/// that found the registry exhausted.
fn last_image(setup: &Setup, events: &Path, reads: usize) -> (String, u64, u64) {
    let events = events.to_owned();
    let waiting = events.clone();
    fakes::wait_until("all reads to log their image", setup.deadline, move || {
        fs::read_to_string(&waiting)
            .ok()
            .is_some_and(|text| logged_images(&text).len() >= reads)
    });
    let text = fs::read_to_string(events).expect("the session log");
    logged_images(&text)
        .into_iter()
        .last()
        .expect("the last logged image")
}

/// The caption the ledger draws for the image at `path`: its file name
/// and logged size.
fn caption_of(path: &str, width: u64, height: u64) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    format!("▣ {name} · {width}×{height}")
}

#[test]
fn an_inline_image_transmits_once_then_deletes_on_session_change() {
    let setup = Setup::new();
    script(&setup, true);
    let marker = setup.workspace().join(".viewed");
    let shim = viewer_shim(&setup, &marker);
    let marker_value = marker.to_str().unwrap().to_owned();
    let mut run = terminal(
        &setup,
        &[
            ("PATH", shim.as_str()),
            ("FIBER_TEST_VIEW_MARKER", marker_value.as_str()),
        ],
    );
    started(&mut run);
    let sessions = sessions(&setup);
    assert_eq!(sessions.len(), 1, "one session ran");
    let (path, width, height) = image_size(&setup, &sessions[0].join("events.jsonl"));
    let caption = caption_of(&path, width, height);

    // The turn-finished signal lands with the call off-screen. Scroll
    // until its row is visible on a completed frame, then click it.
    let grid = wheel_until(&mut run, true, "the ledger call", |grid| {
        grid.rows.iter().any(|row| row.contains("Read 1 file"))
    });
    let from = open_call(&mut run, &grid, "Read 1 file");
    // The opened call emits its placeholder grid before its caption.
    let placeholder = run.wait_bytes(from, PLACEHOLDER, "the placeholder grid");
    run.wait_bytes(placeholder, FRAME_END, "the inline image frame");
    run.wait_screen("the inline grid", |grid| grid.contents.contains(&caption));

    // Scroll away and back, then resize. Each input follows the prior
    // frame's end; each redraw still places the image without a second
    // transmit.
    wheel_until(&mut run, false, "the scrolled-away grid", |grid| {
        !grid.contents.contains(&caption)
    });
    let from = run.output().len();
    wheel_until(&mut run, true, "the followed grid", |grid| {
        grid.contents.contains(&caption)
    });
    let placeholder = run.wait_bytes(from, PLACEHOLDER, "the followed grid");
    run.wait_bytes(placeholder, FRAME_END, "the followed frame");
    let resized_from = run.output().len();
    run.resize(80, 24);
    let placeholder = run.wait_bytes(resized_from, PLACEHOLDER, "the recounted grid");
    run.wait_bytes(placeholder, FRAME_END, "the recounted frame");
    let grid = run.wait_screen("the clickable caption", |grid| {
        grid.rows.iter().any(|row| row.contains(&caption))
    });
    let clicked_from = run.output().len();
    click_text(&mut run, &grid, &caption);
    run.wait_bytes(clicked_from, FRAME_END, "the caption click frame");

    // Wait for the viewer's complete path, not just its redirected file:
    // the shell creates that file before printf writes the path.
    let marker_file = marker.clone();
    let session = sessions[0]
        .file_name()
        .expect("the session directory name")
        .to_string_lossy();
    let name = Path::new(&path)
        .file_name()
        .expect("the image file name")
        .to_string_lossy();
    let expected = setup
        .home()
        .join("cache/images")
        .join(format!("{session}-{name}"))
        .to_string_lossy()
        .into_owned();
    let expected_line = expected.clone();
    fakes::wait_until("the viewer to open", setup.deadline, move || {
        fs::read_to_string(&marker_file)
            .is_ok_and(|opened| opened.lines().any(|line| line == expected_line))
    });
    assert_eq!(
        fs::read_to_string(&marker).unwrap().trim(),
        expected,
        "the viewer opened the private image copy"
    );

    // The kitty `CSI 27u` Esc returns the click-landed row focus to the
    // input box. Wait for the typed prompt's frame before Enter starts the
    // session's text-only turn.
    run.write(b"\x1b[27u");
    let prompt_from = run.output().len();
    run.write(b"second");
    let prompt = run.wait_bytes(prompt_from, b"second", "the input box echo");
    run.wait_bytes(prompt, FRAME_END, "the input box frame");
    run.wait_screen("the box holds the prompt", |grid| {
        grid.contents.contains("second")
    });
    let second_turn_from = run.output().len();
    run.write(b"\r");
    let events = sessions[0].join("events.jsonl");
    let waiting = events.clone();
    fakes::wait_until("the second turn to start", setup.deadline, move || {
        fs::read_to_string(&waiting).is_ok_and(|text| {
            support::complete_lines(&text)
                .iter()
                .filter(|line| line.get("kind").and_then(Value::as_str) == Some("turn_started"))
                .count()
                >= 2
        })
    });
    fakes::wait_until("the second turn to complete", setup.deadline, move || {
        fs::read_to_string(&events).is_ok_and(|text| {
            support::complete_lines(&text)
                .iter()
                .filter(|line| line.get("kind").and_then(Value::as_str) == Some("turn_completed"))
                .count()
                >= 2
        })
    });
    // The view sits scrolled up from the image checks. The session log
    // records completion before the terminal renders it; the fresh
    // finished title follows the completed frame.
    run.wait_bytes(
        second_turn_from,
        support::pty::FINISHED_TITLE,
        "the second-turn finished title",
    );
    let grid = run.wait_screen("the new-messages row", |grid| {
        grid.contents.contains("New messages below · End")
    });
    let follow_from = run.output().len();
    click_text(&mut run, &grid, "New messages below");
    run.wait_bytes(follow_from, FRAME_END, "the followed second-turn frame");
    run.wait_screen("the second turn", |grid| grid.contents.contains("Second."));

    // Closing changes the session, deleting the shown image.
    let home_from = run.output().len();
    run.write(b"\x1b[27u/close\r");
    run.wait_screen("home", |grid| {
        !grid.contents.contains("Second.") && grid.contents.contains("›")
    });
    run.wait_bytes(home_from, support::pty::HOME_TITLE, "the home title");
    run.write(b"\x03\x03\r");
    // The closed session has no resume line; quitting restores the
    // primary screen and cursor.
    run.wait_screen("the restored primary screen", |grid| {
        !grid.alternate_screen && !grid.hide_cursor
    });
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));

    // The terminal stream documents this journey's contract: one
    // transmit and place before the session-change delete, then no more
    // image writes.
    let terminal = output.terminal;
    assert_eq!(count(&terminal, b"a=t,"), 1, "one transmit");
    assert_eq!(count(&terminal, b"a=p,"), 1, "one place");
    assert_eq!(count(&terminal, b"a=d,"), 1, "one delete");
    let transmit = find_at(&terminal, 0, b"a=t,").expect("the transmit");
    let placed = find_at(&terminal, transmit, b"a=p,").expect("the place");
    let deleted = find_at(&terminal, placed, b"a=d,").expect("the delete");
    assert!(transmit < placed && placed < deleted);
    let after = &terminal[deleted..];
    assert_eq!(
        count(after, b"a=t,"),
        0,
        "nothing transmits after the delete"
    );
    assert_eq!(count(after, b"a=p,"), 0, "nothing places after the delete");
}

#[test]
fn the_same_image_stays_one_line_with_the_flag_off() {
    let setup = Setup::new();
    script(&setup, false);
    let mut run = terminal(&setup, &[]);
    started(&mut run);
    // The one clickable line names the file and size, with no
    // placeholders and no transmit.
    let sessions = sessions(&setup);
    assert_eq!(sessions.len(), 1, "one session ran");
    let (path, width, height) = image_size(&setup, &sessions[0].join("events.jsonl"));
    let caption = caption_of(&path, width, height);
    // The opened call draws the image as one line, without placeholders.
    // The turn-finished signal permits scrolling back to its off-screen row.
    let from = run.output().len();
    let grid = wheel_until(&mut run, true, "the ledger call", |grid| {
        grid.rows.iter().any(|row| row.contains("Read 1 file"))
    });
    open_call(&mut run, &grid, "Read 1 file");
    run.wait_screen("the one line", |grid| {
        grid.rows.iter().any(|row| row.contains(&caption))
    });
    assert_eq!(
        count(&run.output()[from..], PLACEHOLDER),
        0,
        "no placeholder grid without the flag"
    );
    run.write(b"\x03\x03\r");
    run.wait_screen("the primary screen with the resume line", |grid| {
        !grid.alternate_screen && !grid.hide_cursor && grid.contents.contains("fiber resume")
    });
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(count(&output.terminal, b"a=t,"), 0, "nothing transmits");
}

#[test]
fn image_only_paste_draws_inline_in_its_prompt_bubble() {
    let setup = Setup::new();
    script_reads(&setup, true, 0, false);
    let bin = setup.root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let (clipboard, body) = if cfg!(target_os = "macos") {
        let hex: String = PNG.iter().map(|byte| format!("{byte:02X}")).collect();
        (
            "osascript",
            format!("printf '\\302\\253data PNGf{hex}\\302\\273\\n'"),
        )
    } else {
        let octal: String = PNG.iter().map(|byte| format!("\\{byte:03o}")).collect();
        ("wl-paste", format!("printf '{octal}'"))
    };
    fakes::script(&bin, clipboard, &body);
    let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .unwrap()
    .into_string()
    .unwrap();
    let mut env: Vec<(&str, &str)> = vec![("FIBER_TEST_FAKE_KEY", "sk-test"), ("PATH", &path)];
    if !cfg!(target_os = "macos") {
        env.push(("WAYLAND_DISPLAY", "fiber-test"));
    }
    let mut run = Run::spawn_pixels(&setup, 100, 30, 1000, 600, &[], &env);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    run.write(&[0x16]);
    run.wait_screen("the pasted image", |grid| {
        grid.contents.contains("[Image #1]")
    });
    let from = run.output().len();
    run.write(b"\r");
    let placeholder = run.wait_bytes(from, PLACEHOLDER, "the image-only prompt grid");
    run.wait_bytes(placeholder, FRAME_END, "the image-only prompt frame");
    let grid = run.wait_screen("the image-only prompt bubble", |grid| {
        grid.contents.contains("120×80") && grid.contents.contains("Seen.")
    });
    assert!(
        grid.rows.iter().any(|row| row.contains('▣')),
        "the prompt bubble has an image caption"
    );
    assert!(
        count(&run.output()[from..], PLACEHOLDER) > 0,
        "the image-only prompt draws inline placeholders"
    );
    run.write(b"\x03\x03\r");
    run.wait_screen("the primary screen", |grid| {
        !grid.alternate_screen && !grid.hide_cursor
    });
    assert_eq!(run.wait().status.code(), Some(0));
}

#[test]
fn images_past_the_placeholder_range_keep_a_unique_viewer_target() {
    // 255 distinct PNG reads: the first 254 fill the kitty placeholder
    // colours, so the last image is the one past the range. It keeps its
    // own viewer target without entering the inline display path: a
    // click opens it, and the click itself transmits nothing
    // (`docs/tui.md`, "Images").
    const READS: usize = 255;
    let setup = Setup::new();
    script_reads(&setup, true, READS, false);
    // A standing allow rule for `read`: 255 approval panels would bury
    // the turn, and the reads need no review.
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "allow", "tool": "read", "prefix": ""})
        ),
    )
    .unwrap();
    let marker = setup.workspace().join(".viewed");
    let shim = viewer_shim(&setup, &marker);
    let marker_value = marker.to_str().unwrap().to_owned();
    let mut run = terminal(
        &setup,
        &[
            ("PATH", shim.as_str()),
            ("FIBER_TEST_VIEW_MARKER", marker_value.as_str()),
        ],
    );
    started(&mut run);
    let sessions = sessions(&setup);
    assert_eq!(sessions.len(), 1, "one session ran");
    let (path, width, height) = last_image(&setup, &sessions[0].join("events.jsonl"), READS);
    let caption = caption_of(&path, width, height);

    // The turn's reads share one group row above the closing text; it
    // draws without scrolling. Opening it reveals one row per call
    // with its image's caption, the last carrying the past-range
    // image, so its caption is clickable without opening its call.
    // The turn already finished in `started`, so a grid showing the
    // group row together with the completed paint has nothing left to
    // render: the group row's position in that grid is final
    // (`docs/testing/screens.md`, "Screens").
    let grid = run.wait_screen("the settled reads group", |grid| {
        grid.contents.contains("completed")
            && grid.rows.iter().any(|row| row.contains("Read 255 files"))
    });
    let from = run.output().len();
    click_text(&mut run, &grid, "Read 255 files");
    run.wait_bytes(from, FRAME_END, "the opened group frame");
    // The opened group settles the same way: the past-range caption
    // together with the completed paint proves every call row drew and
    // the caption row's position is final. The click uses this wait's
    // grid, never a fresh read.
    let grid = run.wait_screen("the settled past-range caption", |grid| {
        grid.contents.contains("completed") && grid.rows.iter().any(|row| row.contains(&caption))
    });
    // The caption click opens the viewer, never a display fetch: no
    // transmit may follow it for the past-range image.
    // The viewer open is the click's signal, so the test waits for
    // the marker next.
    click_text(&mut run, &grid, &caption);

    // The click opens the viewer on the private copy, as for an
    // in-range image: the past-range target resolves.
    let marker_file = marker.clone();
    let session = sessions[0]
        .file_name()
        .expect("the session directory name")
        .to_string_lossy();
    let name = Path::new(&path)
        .file_name()
        .expect("the image file name")
        .to_string_lossy();
    let expected = setup
        .home()
        .join("cache/images")
        .join(format!("{session}-{name}"))
        .to_string_lossy()
        .into_owned();
    let expected_line = expected.clone();
    fakes::wait_until("the viewer to open", setup.deadline, move || {
        fs::read_to_string(&marker_file)
            .is_ok_and(|opened| opened.lines().any(|line| line == expected_line))
    });
    assert_eq!(
        fs::read_to_string(&marker).unwrap().trim(),
        expected,
        "the viewer opened the private image copy"
    );
    // The past-range image draws no placeholder grid: the row above
    // its caption is the previous call's row, never a grid row.
    let at = grid
        .rows
        .iter()
        .position(|row| row.contains(&caption))
        .expect("the past-range caption row");
    assert!(at > 0, "the caption is not the first row");
    assert!(
        !grid.rows[at - 1].contains('\u{10EEEE}'),
        "no placeholder grid above the past-range caption"
    );

    run.write(b"\x03\x03\r");
    run.wait_screen("the primary screen with the resume line", |grid| {
        !grid.alternate_screen && !grid.hide_cursor && grid.contents.contains("fiber resume")
    });
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}
