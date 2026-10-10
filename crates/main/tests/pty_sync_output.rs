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

use fakes::{ProviderServer, Response};
use serde_json::{Value, json};
use support::Deadline;
use support::pty::{KITTY_PUSH, TITLE};

/// Installs a provider `fake` with model `m` on `openai-responses` at the
/// fake server, makes `fake/m` the configured model, and idles the hub
/// out a second after its last client leaves, so no hub lingers.
fn provider(setup: &support::Setup, server: &ProviderServer) {
    let source = setup.root.path().join("src");
    write(
        &source.join("extension.json"),
        &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}),
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
        reached.recv_timeout(deadline.left()).is_ok(),
        "waited until the deadline for {what}"
    );
}

/// "Hello." in two deltas.
fn hello() -> Response {
    let events = [
        json!({"type": "response.output_text.delta", "delta": "Hel"}),
        json!({"type": "response.output_text.delta", "delta": "lo."}),
        json!({"type": "response.output_item.done", "item": {
            "type": "message", "content": [{"type": "output_text", "text": "Hello."}]
        }}),
        json!({"type": "response.completed", "response": {
            "id": "resp_1", "status": "completed",
            "usage": {"input_tokens": 10, "input_tokens_details": {"cached_tokens": 4}, "output_tokens": 3}
        }}),
    ];
    let body: String = events
        .iter()
        .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
        .collect();
    Response::stream(body)
}

fn write(file: &std::path::Path, value: &Value) {
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, value.to_string()).unwrap();
}

/// Synchronized output's begin and end markers (DEC mode 2026).
const BEGIN: &[u8] = b"\x1b[?2026h";
const END: &[u8] = b"\x1b[?2026l";
/// The end of the setup writes: the device-attributes query, the last
/// sequence `term::setup` sends.
const SETUP_END: &[u8] = b"\x1b[c";
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
    // The footer's last word proves the last row drew before the resize.
    run.wait_screen("the footer", |grid| grid.contents.contains("quit"));
    let pre_resize = run.output().len();
    run.resize(40, 10);
    // The redrawn home at 40 by 10: the input line sits on row 4
    // with the cursor parked on it, and the footer hint closes row 9.
    // Only a redraw at the new size lays the frame out this way.
    run.wait_screen("the redrawn grid at the new size", |grid| {
        grid.rows.len() == 10
            && grid
                .rows
                .get(4)
                .is_some_and(|row| row.trim_end() == "› █? for shortcuts")
            && grid.rows.get(9).is_some_and(|row| row.contains("key map"))
            && grid.cursor == (4, 2)
    });
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
        .expect("the setup writes end with the device-attributes query");
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
