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

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

use serde_json::{Value, json};
use support::pty::Run;
use support::{Deadline, Setup, TRUECOLOUR, write_json};

/// The first row containing `needle`.
fn find_row(rows: &[String], needle: &str) -> Option<u16> {
    rows.iter()
        .position(|row| row.contains(needle))
        .map(|y| u16::try_from(y).expect("the screen fits in a u16"))
}

/// A click at cell (`x`, `y`).
fn click(run: &mut Run, x: u16, y: u16) {
    run.write(format!("\x1b[<0;{};{}M\x1b[<0;{};{}m", x + 1, y + 1, x + 1, y + 1).as_bytes());
}

/// A solid-colour PNG `width` by `height`: signature, IHDR, stored
/// deflate IDATs, IEND, each with its CRC.
fn png(width: u32, height: u32) -> Vec<u8> {
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFF_u32;
        for byte in bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    0xEDB8_8320 ^ (crc >> 1)
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }
    fn chunk(kind: &[u8; 4], data: &[u8], out: &mut Vec<u8>) {
        out.extend_from_slice(&u32::try_from(data.len()).unwrap().to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    }
    fn adler32(bytes: &[u8]) -> u32 {
        let (mut a, mut b) = (1u32, 0u32);
        for byte in bytes {
            a = (a + u32::from(*byte)) % 65521;
            b = (b + a) % 65521;
        }
        (b << 16) | a
    }
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(b"IHDR", &ihdr, &mut out);
    // One filter byte plus RGB triples per row, in stored blocks.
    let mut raw = Vec::new();
    for _ in 0..height {
        raw.push(0);
        raw.extend_from_slice(&[9, 112, 203].repeat(width as usize));
    }
    let mut stored = vec![0x78, 0x01];
    for (at, block) in raw.chunks(65535).enumerate() {
        let last = at == raw.chunks(65535).len() - 1;
        stored.push(if last { 0x01 } else { 0x00 });
        let len = u16::try_from(block.len()).expect("a chunk holds 65535 bytes");
        stored.extend_from_slice(&len.to_le_bytes());
        stored.extend_from_slice(&(!len).to_le_bytes());
        stored.extend_from_slice(block);
    }
    stored.extend_from_slice(&adler32(&raw).to_be_bytes());
    chunk(b"IDAT", &stored, &mut out);
    chunk(b"IEND", &[], &mut out);
    out
}

/// The scripted turns: a `read` of the PNG, its closing text with a
/// long tail below the image, then the same session's text-only turn.
fn script(setup: &Setup, inline: bool) {
    fs::write(setup.workspace().join("pic.png"), png(120, 80)).unwrap();
    let tail = (1..=30)
        .map(|n| format!("- T{n:04}"))
        .collect::<Vec<_>>()
        .join("\n");
    write_json(
        &setup.workspace().join("s.json"),
        &json!({"steps": [
            {"reasoning": "**Doing**",
             "tool_calls": [{"name": "read", "arguments": {"path": "pic.png"}}]},
            {"text": ["Seen.", tail]},
            {"text": ["Second."]},
        ]}),
    );
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 1000},
            "tui": {"inline_images": inline}}),
    );
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
    let from = run.ready();
    run.write(b"go\r");
    loop {
        let grid = run.wait_screen("approval or done", |grid| {
            grid.contents.contains("Allow once") || grid.contents.contains("▣ completed")
        });
        if grid.contents.contains("▣ completed") {
            break;
        }
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

/// Waits under the setup's [`Deadline`] until `path` exists, showing
/// the terminal on expiry.
fn until_file(run: &mut Run, deadline: Deadline, path: &Path, what: &str) {
    let path = path.to_owned();
    let (done, reached) = mpsc::channel();
    thread::spawn(move || {
        while !path.exists() {
            thread::yield_now();
        }
        done.send(()).unwrap_or(());
    });
    assert!(
        deadline.recv(&reached).is_ok(),
        "waited until the deadline for {what}; screen: {}",
        run.screen().contents
    );
}

/// Opens the ledger's `read` call so its image rows draw.
fn open_call(run: &mut Run, name: &str) {
    let grid = run.wait_screen("the ledger call", |grid| {
        grid.rows.iter().any(|row| row.contains(name))
    });
    let row = find_row(&grid.rows, name).expect("the call row");
    let col = u16::try_from(
        grid.rows[usize::from(row)]
            .find(name)
            .expect("the call row"),
    )
    .expect("the row fits in a u16");
    click(run, col, row);
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
    loop {
        if let Ok(text) = fs::read_to_string(events)
            && let Some(found) = logged_image(&text)
        {
            return found;
        }
        if setup.deadline.left().is_zero() {
            panic!("waited until the deadline for the logged image");
        }
        thread::yield_now();
    }
}

/// The logged path and size of the first image part in `events`' tool
/// completions, when one has landed.
fn logged_image(text: &str) -> Option<(String, u64, u64)> {
    for line in support::complete_lines(text) {
        if line.get("kind").and_then(Value::as_str) != Some("tool_call_completed") {
            continue;
        }
        let content = line
            .get("payload")
            .and_then(|payload| payload.get("content"))
            .and_then(Value::as_array)?;
        for part in content {
            if part.get("type").and_then(Value::as_str) == Some("image")
                && let (Some(path), Some(width), Some(height)) = (
                    part.get("path").and_then(Value::as_str),
                    part.get("width").and_then(Value::as_u64),
                    part.get("height").and_then(Value::as_u64),
                )
            {
                return Some((path.to_owned(), width, height));
            }
        }
    }
    None
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
    // The turn ends following the tail, with the call off-screen: wheel
    // back up so its row draws, then open it.
    run.write(&b"\x1b[<64;51;16M".repeat(10));
    run.wait_screen("the call row", |grid| {
        grid.rows.iter().any(|row| row.contains("Read 1 file"))
    });
    // The opened grid sits above the caption: its rows render at the
    // top, then its placeholders reach the wire.
    let mut from = run.output().len();
    open_call(&mut run, "Read 1 file");
    // The opened grid sits above the caption: wheel up to the top so
    // its rows render, then its placeholders reach the wire.
    run.write(&b"\x1b[<64;51;16M".repeat(10));
    run.wait_bytes(from, PLACEHOLDER, "the placeholder grid");
    run.wait_screen("the inline grid", |grid| grid.contents.contains(&caption));
    // Away to the bottom and back, then a resize: the scrolled and
    // recounted grid fetches and transmits nothing again.
    run.write(&b"\x1b[<65;51;16M".repeat(10));
    run.wait_screen("the scrolled-away grid", |grid| {
        !grid.contents.contains(&caption)
    });
    from = run.output().len();
    run.write(&b"\x1b[<64;51;16M".repeat(10));
    run.wait_bytes(from, PLACEHOLDER, "the followed grid");
    run.wait_screen("the followed grid", |grid| grid.contents.contains(&caption));
    from = run.output().len();
    run.resize(80, 24);
    run.wait_bytes(from, PLACEHOLDER, "the recounted grid");
    run.wait_screen("the recounted grid", |grid| {
        grid.contents.contains(&caption)
    });
    // The caption click queues the viewer with the image.
    let grid = run.wait_screen("the clickable caption", |grid| {
        grid.rows.iter().any(|row| row.contains(&caption))
    });
    let row = find_row(&grid.rows, &caption).expect("the caption row");
    let col = u16::try_from(
        grid.rows[usize::from(row)]
            .find('▣')
            .expect("the caption marker"),
    )
    .expect("the row fits in a u16");
    click(&mut run, col, row);
    until_file(&mut run, setup.deadline, &marker, "the viewer to open");
    // The second turn runs in this session: the kitty `CSI 27u` Esc is
    // one self-delimiting sequence returning the click-landed row focus
    // to the input box (`keys.rs`: a lone Esc ending the read is Esc),
    // and the box echo proves the prompt landed before Enter starts it.
    // Its text-only turn draws nothing inline and transmits nothing.
    run.write(b"\x1b[27u");
    run.write(b"second");
    run.wait_screen("the box holds the prompt", |grid| {
        grid.contents.contains("second")
    });
    let from = run.output().len();
    run.write(b"\r");
    // The view sits scrolled up from the image checks: follow the new
    // turn to its end so its text draws on screen.
    run.write(&b"\x1b[<65;51;16M".repeat(10));
    run.wait_screen("the second turn", |grid| grid.contents.contains("Second."));
    run.turn_finished(from);
    // Closing changes the session, deleting the shown image: exactly
    // one transmit and place went out for it, then its delete, and
    // nothing after the delete.
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
    // Exactly one transmit and place for the shown image, then its
    // delete on the session change: nothing transmits or places after
    // the delete.
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
    // The turn ends following the tail, with the call off-screen: wheel
    // back up so its row draws, then open it.
    run.write(&b"\x1b[<64;51;16M".repeat(10));
    let from = run.output().len();
    open_call(&mut run, "Read 1 file");
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
