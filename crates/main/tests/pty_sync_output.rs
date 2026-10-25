//! Binary-level tests of synchronized output (`docs/tui.md`,
//! "Performance"): every frame the terminal writes sits inside a matched
//! mode-2026 pair, and the harness snapshots only whole frames.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::sync::mpsc;
use std::thread;

use fakes::ProviderServer;
use serde_json::{Value, json};
use support::pty::{KITTY_PUSH, TITLE};
use support::{
    Deadline, function_call, hello as hello_text, hello_inline_completed as hello, stream,
};

/// Installs a provider `fake` with model `m` on `openai-responses` at the
/// fake server, makes `fake/m` the configured model, and idles the hub
/// out a second after its last client leaves, so no hub lingers.
fn provider(setup: &support::Setup, server: &ProviderServer) {
    let source = setup.root.path().join("src");
    write(
        &source.join("extension.json"),
        &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "providers": {"fake": [format!("{}/v1", server.url())]}}),
    );
    write(
        &source.join("providers/fake.json"),
        &json!({
            "name": "fake",
            "credential": {"env": "FIBER_TEST_FAKE_KEY"},
            "models": [{"id": "m", "protocol": "openai-responses",
                "base_url": format!("{}/v1", server.url()), "context_window": 100000}]
        }),
    );
    extensions::plan(
        &setup.home(),
        &extensions::Request::Path(source),
        "0.0.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    write(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "hub": {"idle_exit_ms": 1000}}),
    );
}

/// Spawns `fiber` with `args` on a `cols` by `rows` terminal through the
/// shared driver, with `env` after the fake key in the child's
/// environment, so every run keeps today's key.
fn terminal(
    setup: &support::Setup,
    cols: u16,
    rows: u16,
    args: &[&str],
    env: &[(&str, &str)],
) -> support::pty::Run {
    let mut full = vec![("FIBER_TEST_FAKE_KEY", "sk-test")];
    full.extend(env.iter().copied());
    support::pty::Run::spawn(setup, cols, rows, args, &full)
}

/// Waits under the test's [`Deadline`] until `socket` exists or not, as
/// `present` says, naming `what` on expiry.
#[track_caller]
fn until_socket(deadline: Deadline, socket: &std::path::Path, present: bool, what: &str) {
    let socket = socket.to_owned();
    let (done, reached) = mpsc::channel();
    thread::spawn(move || {
        while socket.exists() != present {
            thread::yield_now();
        }
        done.send(()).unwrap_or(());
    });
    assert!(
        deadline.recv(&reached).is_ok(),
        "waited until the deadline for {what}"
    );
}

fn write(file: &std::path::Path, value: &Value) {
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, value.to_string()).unwrap();
}

/// Synchronized output's begin and end markers (DEC mode 2026).
const BEGIN: &[u8] = b"\x1b[?2026h";
const END: &[u8] = b"\x1b[?2026l";
/// The end of the setup writes: the graphics probe, the last sequence
/// `term::setup` sends.
const SETUP_END: &[u8] = b"\x1b_Gi=31,a=q,s=1,v=1,f=24,m=0;AAAA\x1b\\";
/// The start of the restore writes: popping kitty's keyboard flags, the
/// first sequence the quit path sends.
const RESTORE_START: &[u8] = b"\x1b[<u";
/// Kitty's keyboard-flags push: written once the loop sees the harness's
/// reply, wherever the frames are.
const PUSH: &[u8] = KITTY_PUSH;

#[test]
fn every_frame_is_wrapped_in_synchronized_output() {
    let setup = support::Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    provider(&setup, &server);
    let mut run = terminal(&setup, 120, 32, &[], &[]);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    // The footer's first words prove the last row drew before the resize.
    run.wait_screen("the footer", |grid| {
        grid.contents.contains("Enter opens the focused session")
    });
    let pre_resize = run.output().len();
    run.resize(40, 10);
    // The redrawn home at 40 by 10: the box moves to the bottom, with
    // the input line on row 5 and the footer hint on row 9. Only a redraw
    // at the new size lays the frame out this way.
    let grid = run.wait_screen("the redrawn grid at the new size", |grid| {
        grid.rows.len() == 10
            && grid
                .rows
                .get(5)
                .is_some_and(|row| row.trim() == "› █? for shortcuts")
            && grid
                .rows
                .get(9)
                .is_some_and(|row| row.contains("Enter opens the focused session"))
    });
    assert_eq!(grid.cursor, (5, 4));
    // The hub `fiber` started is up before the quit, so `wait` sees it
    // idle out rather than start after the home is gone.
    until_socket(
        setup.deadline,
        &setup.hub_socket(),
        true,
        "the hub to start",
    );
    run.write(b"\x03\x03");
    // No session runs, so no resume line follows: the restored primary
    // screen is the assertion.
    run.wait_screen("the restored primary screen", |grid| {
        !grid.alternate_screen && !grid.hide_cursor
    });
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
    assert_frames_wrapped(&output.terminal, pre_resize);
}

/// Asserts every frame write in `terminal` sits inside a matched
/// `?2026h`/`?2026l` pair: pairs never nest, the first frame owns the
/// first pair, and a pair completes after `pre_resize`, the output length
/// when the resize went out. A frame write is any byte between the end of
/// the setup sequences and the start of the restore writes other than an
/// OSC sequence (the title, the pointer shape), a bell, or kitty's
/// keyboard-flags push.
fn assert_frames_wrapped(terminal: &[u8], pre_resize: usize) {
    let setup_end = terminal
        .windows(SETUP_END.len())
        .position(|window| window == SETUP_END)
        .map(|at| at + SETUP_END.len())
        .expect("the setup writes end with the graphics probe");
    assert!(
        terminal[..setup_end]
            .windows(b"\x1b[?1049h".len())
            .any(|window| window == b"\x1b[?1049h"),
        "the setup writes start on the alternate screen"
    );
    let restore_start = terminal[setup_end..]
        .windows(RESTORE_START.len())
        .position(|window| window == RESTORE_START)
        .map(|at| setup_end + at)
        .expect("the quit path leaves the alternate screen");
    let frame = &terminal[setup_end..restore_start];
    let mut depth = 0;
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    let mut begin_at: Option<usize> = None;
    let mut first_title = None;
    let mut at = 0;
    while at < frame.len() {
        let rest = &frame[at..];
        if rest.starts_with(BEGIN) {
            assert_eq!(depth, 0, "synchronized-output pairs never nest");
            depth = 1;
            begin_at = Some(setup_end + at);
            at += BEGIN.len();
        } else if rest.starts_with(END) {
            assert_eq!(depth, 1, "every end marker closes a block");
            depth = 0;
            pairs.push((begin_at.take().unwrap_or(setup_end + at), setup_end + at));
            at += END.len();
        } else if rest.starts_with(b"\x1b]") {
            let end = osc_end(rest).expect("every OSC sequence is terminated");
            if rest.starts_with(TITLE) && first_title.is_none() {
                first_title = Some(setup_end + at);
            }
            at += end;
        } else if rest.starts_with(b"\x07") {
            at += 1;
        } else if rest.starts_with(PUSH) {
            at += PUSH.len();
        } else {
            assert_eq!(
                depth, 1,
                "every frame write sits inside a synchronized-output pair"
            );
            at += 1;
        }
    }
    assert_eq!(depth, 0, "no synchronized-output block is left open");
    assert!(
        pairs.len() >= 2,
        "the first frame and the redraw each take a pair"
    );
    let first_title = first_title.expect("the first frame writes its title");
    assert_eq!(
        pairs.iter().filter(|(_, end)| *end <= first_title).count(),
        1,
        "the first frame owns the first pair"
    );
    assert!(
        pairs.first().is_some_and(|(begin, _)| *begin >= setup_end),
        "the first pair starts after the setup writes"
    );
    assert!(
        pairs.iter().any(|(_, end)| *end > pre_resize),
        "a pair completes after the resize went out"
    );
}

/// The length of the OSC sequence `bytes` starts with, through its
/// terminator: BEL, or ESC backslash.
fn osc_end(bytes: &[u8]) -> Option<usize> {
    let mut at = 2;
    while at < bytes.len() {
        if bytes[at] == 0x07 {
            return Some(at + 1);
        }
        if bytes[at] == 0x5c && bytes.get(at.wrapping_sub(1)) == Some(&0x1b) {
            return Some(at + 1);
        }
        at += 1;
    }
    None
}

/// A stream split inside a synchronized block yields no snapshot until
/// the block closes: the grid still shows the pre-block screen after the
/// begin marker and more text, and shows the whole block once it ends.
#[test]
fn the_harness_snapshots_only_whole_frames() {
    let mut shared = support::pty::Shared::new(20, 5);
    let blank = shared.grid().contents.clone();
    shared.publish(b"\x1b[?2026hold");
    assert_eq!(
        shared.grid().contents,
        blank,
        "no snapshot lands while the block is open"
    );
    shared.publish(b"more");
    assert_eq!(
        shared.grid().contents,
        blank,
        "no snapshot lands while the block is open"
    );
    shared.publish(b"\x1b[?2026l");
    assert!(
        shared.grid().contents.contains("oldmore"),
        "the closed block snapshots at once: {:?}",
        shared.grid().contents,
    );
}

/// A begin marker split across two reads still opens the block: at every
/// split offset the grid waits for the end marker.
#[test]
fn the_harness_joins_a_marker_split_across_reads() {
    for split in 1..BEGIN.len() {
        let mut shared = support::pty::Shared::new(20, 5);
        let blank = shared.grid().contents.clone();
        shared.publish(&BEGIN[..split]);
        shared.publish(&[&BEGIN[split..], b"x"].concat());
        assert_eq!(
            shared.grid().contents,
            blank,
            "no snapshot lands with the block open at split {split}"
        );
        shared.publish(END);
        assert!(
            shared.grid().contents.contains('x'),
            "the closed block snapshots at split {split}"
        );
    }
}

/// A stream without mode 2026 snapshots after every read, as before.
#[test]
fn the_harness_snapshots_a_stream_without_markers_after_each_read() {
    let mut shared = support::pty::Shared::new(20, 5);
    shared.publish(b"a");
    assert!(
        shared.grid().contents.contains('a'),
        "the first read snapshots: {:?}",
        shared.grid().contents,
    );
    shared.publish(b"b");
    assert!(
        shared.grid().contents.contains("ab"),
        "the second read snapshots: {:?}",
        shared.grid().contents,
    );
}

/// Kitty's graphics-probe reply the test writes once the probe is on the
/// wire: the harness answers the keyboard, attributes and theme queries,
/// but nothing kitty-specific, so the test speaks for the terminal
/// (`docs/tui.md`, "Images").
const PROBE_REPLY: &[u8] = b"\x1b_Gi=31;OK\x07";

/// The logo's bytes, compiled in: the read tool passes a PNG through at
/// its own bytes, so the display transmit chunks them over the wire.
const LOGO: &[u8] = include_bytes!("../../../research/site-prototype/fiber-logo.png");

/// The logo's bytes: big enough to chunk once stored.
fn logo_bytes() -> Vec<u8> {
    LOGO.to_vec()
}

/// The provider answering a `read` of `pic.png`, then closing the turn.
fn image_read_script() -> [fakes::Response; 2] {
    [
        stream(&[function_call(
            "call_read",
            "read",
            &json!({"path": "pic.png"}),
        )]),
        hello_text(),
    ]
}

/// Standard base64 of `bytes`: the test's only encoder, checked by the
/// reassembly below against the hub's own encoding of the same file.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let word = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(ALPHABET[((word >> 18) & 63) as usize] as char);
        out.push(ALPHABET[((word >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[((word >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(word & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// The transmit payloads on `wire` in order, asserting the chunk flags:
/// the first chunk's control data carries the PNG format, every chunk
/// but the last says more follow, and the last says none.
fn transmit_payloads(wire: &str) -> Vec<&str> {
    let first = wire.find("Ga=t,").expect("the transmit went out");
    assert!(
        wire[first..].starts_with("Ga=t,f=100,"),
        "the transmit carries the PNG format: {wire:?}"
    );
    let mut at = first + wire[first..].find(';').expect("the control data ends") + 1;
    assert!(
        wire[first..at].ends_with("m=1;"),
        "the first chunk says more: {wire:?}"
    );
    let mut payloads = Vec::new();
    loop {
        let tail = &wire[at..];
        let end = at
            + tail.find("\x1b\\").unwrap_or_else(|| {
                panic!("each chunk ends: {at} in {} bytes: {tail:?}", wire.len())
            });
        payloads.push(&wire[at..end]);
        at = end + "\x1b\\".len();
        if wire[at..].starts_with("\x1b_Gm=0;") {
            at += "\x1b_Gm=0;".len();
            let end = at + wire[at..].find("\x1b\\").expect("the last chunk ends");
            payloads.push(&wire[at..end]);
            break;
        }
        assert!(
            wire[at..].starts_with("\x1b_Gm=1;"),
            "a middle chunk says more: {wire:?}"
        );
        at += "\x1b_Gm=1;".len();
    }
    payloads
}

/// An inline image's journey to the wire: with `tui.inline_images` on, a
/// tool result carrying a PNG draws its placeholder grid, and the
/// answered display fetch transmits the PNG in chunks and places it,
/// with the placeholder cells in the frame (`docs/tui.md`, "Images").
#[test]
fn an_inline_image_transmits_places_and_draws_placeholders() {
    let setup = support::Setup::new();
    // Big enough to chunk: its thousands of bytes span more than one
    // 4096-byte kitty chunk once stored.
    let logo = logo_bytes();
    assert!(
        logo.len() > 3 * 1024,
        "the logo spans chunks: {} bytes",
        logo.len()
    );
    std::fs::write(setup.workspace().join("pic.png"), &logo).unwrap();
    let server = ProviderServer::start(image_read_script()).unwrap();
    provider(&setup, &server);
    std::fs::write(
        setup.home().join("config.json"),
        json!({
            "model": "fake/m",
            "hub": {"idle_exit_ms": 1000},
            "tui": {"inline_images": true},
        })
        .to_string(),
    )
    .unwrap();
    let mut run = terminal(&setup, 60, 24, &[], &[]);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    // The probe went out with setup: answer it, and the images size to
    // the placeholder grid.
    run.wait_bytes(0, SETUP_END, "the graphics probe");
    run.write(PROBE_REPLY);
    // The turn's tool result carries the PNG: its grid draws inline.
    let from = run.output().len();
    run.write(b"look at the logo\r");
    run.turn_finished(from);
    // The call's group opens, and the image draws its grid under the
    // ledger row: only a drawn grid queues its display fetch. The grid,
    // not the raw wire, carries the caption: the wire splits short
    // mixed-style rows into positioned runs.
    run.write(b"\x0f");
    let grid = run.wait_screen("the inline grid", |grid| {
        grid.contents.contains("· 640×160")
    });
    assert!(
        grid.contents.contains("· 640×160"),
        "the caption names the logged size"
    );
    // The place follows the whole transmit out of the post-draw drain, so
    // waiting for it sees every chunk.
    run.wait_bytes(from, b"Ga=p,", "the display place");
    let wire = String::from_utf8_lossy(&run.output()).into_owned();
    // Exactly one transmit goes out, however often the frame redraws.
    assert_eq!(wire.matches("Ga=t,").count(), 1, "one transmit: {wire:?}");
    // The transmit payload is the stored artifact's bytes in base64,
    // chunked at 4096: reassemble the wire chunks and compare.
    let id = support::journey::only_session(&setup);
    let stored: Vec<std::path::PathBuf> =
        std::fs::read_dir(support::session_dir(&setup, &id).join("artifacts"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
    assert_eq!(stored.len(), 1, "one stored image: {stored:?}");
    let expected = base64(&std::fs::read(&stored[0]).unwrap());
    let payloads = transmit_payloads(&wire);
    assert!(payloads.len() >= 2, "the payload chunks: {wire:?}");
    assert_eq!(
        payloads.concat(),
        expected,
        "the wire carries the artifact's bytes"
    );
    // The place follows the transmit, bound to the placeholders under
    // the image's id: a 56-column conversation at a 10 by 20 cell grids
    // 640 by 160 at 22 by 2.
    let transmit = wire.find("Ga=t,").expect("the transmit went out");
    let placed = wire.find("Ga=p,").expect("the place went out");
    assert!(transmit < placed, "the place follows the transmit");
    assert!(
        wire[placed..].starts_with("Ga=p,U=1,i=1,c=22,r=2"),
        "the place binds the placeholders: {wire:?}"
    );
    // The frame's placeholder cells drew: the image moves with the text
    // through Unicode placeholders.
    assert!(wire.contains("\u{10EEEE}"), "placeholder cells drew");
    // Closing the session quits cleanly off the alternate screen.
    run.write(b"/close\r");
    support::pty::until_hub_exits(
        setup.deadline,
        &setup.session_socket(&id),
        std::path::Path::new(""),
        "the closed session to exit",
    );
    run.write(b"\x03\x03\r");
    run.wait_screen("the restored primary screen", |screen| {
        !screen.alternate_screen && !screen.hide_cursor
    });
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}
