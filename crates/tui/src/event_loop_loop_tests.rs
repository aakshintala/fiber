//! Tests for paging inside the frame: the loop fetches the pages a frame
//! needs with `history` before it draws (`docs/tui.md`, "History and
//! paging").

use super::{
    Input, Loop, Screen,
    hub::{Connection, LOST},
};
use crate::app::App;
use crate::keys::{Event, Reply};
use crate::link::Line;
use contract::clock::Clock;
use contract::{Envelope, Seq};
use fakes::{Deadline, MUST_SUCCEED_WITHIN};
use ratatui::backend::TestBackend;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, ErrorKind, Read};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// One named wall-clock deadline for every blocking wait.
const DEADLINE: Duration = Duration::from_secs(10);

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

/// PageUp, as the terminal sends it.
const PAGE_UP: &[u8] = b"\x1b[5~";

/// PageDown, as the terminal sends it.
const PAGE_DOWN: &[u8] = b"\x1b[6~";

/// One line of the session.
fn line(kind: &str, seq: Option<u64>, action: Option<&str>, payload: Value) -> Envelope {
    Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|id| contract::ActionId(id.to_owned())),
        seq: seq.map(contract::Seq),
        payload: payload.as_object().cloned().unwrap_or_default(),
    }
}

/// A session whose first page holds 513 lines, three `history` answers,
/// the last of one line: a turn of 102 tool steps, then six turns of long
/// replies that push it out of the window.
fn session() -> Vec<Envelope> {
    let mut lines = Vec::new();
    let mut seq = 0u64;
    let mut push = |kind: &str, action: Option<&str>, payload: Value| {
        lines.push(line(kind, Some(seq), action, payload));
        seq += 1;
    };
    let prompt = |text: &str| json!({"input": [{"type": "message", "source": "driver", "content": [{"type": "text", "text": text}]}]});
    push("turn_started", None, prompt("first"));
    for step in 0..102 {
        let call = format!("a_t{step}");
        push("step_started", None, json!({}));
        push("assistant_message_started", Some("a_m"), json!({}));
        push(
            "tool_call_requested",
            Some(&call),
            json!({"name": "read", "arguments": {}}),
        );
        push(
            "assistant_message_completed",
            Some("a_m"),
            json!({"outcome": "completed"}),
        );
        push(
            "tool_call_completed",
            Some(&call),
            json!({"status": "completed", "content": []}),
        );
    }
    push("usage_recorded", None, json!({}));
    push("usage_recorded", None, json!({}));
    for turn in 1..7 {
        if turn > 1 {
            push("turn_started", None, prompt(&format!("turn {turn}")));
        }
        for step in 0..20 {
            let message = format!("a_r{turn}_{step}");
            push("step_started", None, json!({}));
            push("assistant_message_started", Some(&message), json!({}));
            let text = format!("reply {turn}.{step} ").repeat(12);
            push("text_completed", Some(&message), json!({"text": text}));
        }
        push("turn_completed", None, json!({"outcome": "completed"}));
    }
    lines
}

/// The durable lines from `from` to `to`, at most 256, as `history`
/// answers.
fn history(lines: &[Envelope], from: u64, to: u64) -> Vec<Envelope> {
    lines
        .iter()
        .filter(|line| line.seq.is_some_and(|seq| from <= seq.0 && seq.0 <= to))
        .take(256)
        .cloned()
        .collect()
}

/// The open's newest or older 256-line history window, oldest first.
fn tail_history(lines: &[Envelope], from: u64, to: u64) -> Vec<Envelope> {
    let mut window: Vec<Envelope> = lines
        .iter()
        .filter(|line| line.seq.is_some_and(|seq| from <= seq.0 && seq.0 <= to))
        .cloned()
        .collect();
    window.drain(..window.len().saturating_sub(256));
    window
}

/// A session line answering command `id`.
fn answer(kind: &str, payload: Value) -> Input {
    Input::Hub(Line::Session(line(kind, None, None, payload)))
}

/// What the fake hub does with each `history` command: the command and
/// the inputs to send back.
type Script = Box<dyn FnMut(&Value) -> Vec<Input> + Send>;

/// A loop at 60x12 with inline images on: home carries the flag and
/// graphics were detected, so the images size to the placeholder grid.
fn inline_opened(
    lines: Vec<Envelope>,
) -> (
    Loop<TestBackend>,
    UnixStream,
    Sender<Input>,
    mpsc::Receiver<Input>,
) {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(60, 24);
    let hello = contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    };
    app.on_line(Line::Hub(hello));
    app.set_home(crate::home::Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        inline_images: true,
        ..Default::default()
    });
    app.set_graphics();
    // The window tests size this image at a 10 by 20 cell: the same
    // grid here, so the scroll keeps a known shape.
    app.set_cell((10, 20));
    app.attach(contract::SessionId(SESSION.to_owned()));
    for envelope in lines {
        app.on_line(Line::Session(envelope));
    }
    // The ledger draws the image grid only while its group is open.
    app.open(crate::app::Target::Group(crate::turn::target_id("a_t")));
    let (ours, theirs) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    // Tall enough that one page keeps the image grid in view.
    let screen =
        Screen::new(TestBackend::new(60, 24), 60, 24).unwrap_or_else(|err| panic!("screen: {err}"));
    let mut lp = Loop::for_test(app, screen);
    lp.hub.store(ours);
    // A writable tty for the image writer's clone: the file is unlinked
    // at once, so no directory outlives the test, while the open file
    // still takes the writer's chunks.
    let tty_dir = fakes::TempDir::new("tui-image-tty");
    lp.tty = std::fs::File::create(tty_dir.path().join("tty"))
        .unwrap_or_else(|err| panic!("tty: {err}"));
    drop(tty_dir);
    let (tx, rx) = mpsc::channel();
    (lp, theirs, tx, rx)
}

/// A session with an image call and the letters under it: the filler
/// turn above puts the grid a page down, and the text below the call
/// scrolls with its placeholders.
fn image_session() -> Vec<Envelope> {
    let mut lines = Vec::new();
    let mut seq = 0u64;
    let mut push = |kind: &str, action: Option<&str>, payload: Value| {
        lines.push(line(kind, Some(seq), action, payload));
        seq += 1;
    };
    let prompt = |text: &str| {
        json!({"input": [{"type": "message", "source": "driver",
        "content": [{"type": "text", "text": text}]}]})
    };
    // A long reply above the image, so one page down still shows it.
    push("turn_started", None, prompt("zeroth"));
    push("step_started", None, json!({}));
    push("assistant_message_started", Some("a_m0"), json!({}));
    let filler = "zeroth filler ".repeat(59);
    push("text_completed", Some("a_m0"), json!({"text": filler}));
    push(
        "assistant_message_completed",
        Some("a_m0"),
        json!({"outcome": "completed"}),
    );
    push("turn_completed", None, json!({"outcome": "completed"}));
    push("turn_started", None, prompt("first"));
    push("step_started", None, json!({}));
    push(
        "tool_call_requested",
        Some("a_t"),
        json!({"name": "read", "arguments": {"path": "a.png"}}),
    );
    push(
        "tool_call_completed",
        Some("a_t"),
        json!({"status": "completed", "content": [
            {"type": "image", "path": "artifacts/a.png",
                "mime_type": "image/png", "width": 1280, "height": 800},
        ]}),
    );
    push("assistant_message_started", Some("a_m"), json!({}));
    push("text_completed", Some("a_m"), json!({"text": "second"}));
    push(
        "assistant_message_completed",
        Some("a_m"),
        json!({"outcome": "completed"}),
    );
    // The turn completes: while a turn runs, Ctrl+C opens the quit
    // question instead of quitting, and the loop below would never end.
    push("turn_completed", None, json!({"outcome": "completed"}));
    lines
}

#[test]
fn letters_under_a_scrolled_image_follow_its_placeholders() {
    // No hub answers: the placeholder grid sizes from the logged width
    // and height, so the loop never fetches to draw it.
    let (lp, _theirs, tx, rx) = inline_opened(image_session());
    // Down one page: enough to move the grid without losing it.
    let downs: Vec<Input> = (0..1).map(|_| Input::Bytes(PAGE_DOWN.to_vec())).collect();
    let lp = run(lp, rx, &tx, downs);
    assert!(lp.app.scroll().0 > 0, "the conversation scrolled");
    let screen = shown(&lp);
    let rows: Vec<&str> = screen.lines().collect();
    let caption = rows
        .iter()
        .position(|row| row.contains("▣ a.png · 1280×800"))
        .unwrap_or_else(|| panic!("the caption scrolled with the grid: {screen}"));
    // The letters under the image moved with it: the reply below the
    // caption reads second, and every row between the call and the
    // caption is a placeholder, so the scrolled grid stays whole.
    let call = rows
        .iter()
        .position(|row| row.contains("✓ read"))
        .unwrap_or_else(|| panic!("the image call scrolled with the grid: {screen}"));
    assert!(
        call + 1 < caption,
        "the grid has placeholder rows: {screen}"
    );
    assert!(
        rows.get(caption + 2)
            .is_some_and(|row| row.contains("second")),
        "{screen}"
    );
    for row in rows.iter().take(caption).skip(call + 1) {
        assert!(
            row.contains('\u{10EEEE}'),
            "the scrolled grid stays whole: {screen}"
        );
    }
    assert_eq!(lp.app.notice(), None);
}

/// The hub's accepted answer to the display `read_file` carrying
/// `command_id`: PNG bytes, as production relays them.
fn display_answer(command_id: &str) -> Input {
    Input::Hub(Line::Hub(contract::HubLine {
        kind: "command_accepted".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: json!({"command_id": command_id,
            "result": {"data": "QUJD", "mime_type": "image/png"}})
        .as_object()
        .cloned()
        .unwrap_or_default(),
    }))
}

/// Reads hub commands until the display `read_file`, returning its id:
/// at most ten lines, each waited on for [`DEADLINE`].
fn read_display_id(theirs: &UnixStream) -> String {
    theirs
        .set_read_timeout(Some(DEADLINE))
        .unwrap_or_else(|err| panic!("timeout: {err}"));
    // The frame's commands first, whole: one read holds everything the
    // joined frame sent, so no line waits on the hub mid-read.
    let mut raw = vec![0u8; 4096];
    let mut ends = theirs;
    let read =
        std::io::Read::read(&mut ends, &mut raw).unwrap_or_else(|err| panic!("hub bytes: {err}"));
    let mut reader = std::io::BufReader::new(&raw[..read]);
    let mut text = String::new();
    for _ in 0..10 {
        text.clear();
        if reader
            .read_line(&mut text)
            .unwrap_or_else(|err| panic!("hub command: {err}"))
            == 0
        {
            break;
        }
        let command: Value = serde_json::from_str(&text).unwrap_or_default();
        if command.get("command").and_then(Value::as_str) == Some("read_file") {
            return command
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("read_file has no id: {command}"))
                .to_owned();
        }
    }
    panic!("no display fetch reached the hub");
}

/// Reads the writer's bytes until every needle arrived, within
/// [`DEADLINE`], returning them with the read end for the next phase: the
/// writer sends after the step returns, so the test waits for the bytes
/// rather than racing the writer thread.
#[track_caller]
fn read_until(read: std::fs::File, needles: &[&str]) -> mpsc::Receiver<(Vec<u8>, std::fs::File)> {
    let needles: Vec<Vec<u8>> = needles
        .iter()
        .map(|needle| needle.as_bytes().to_vec())
        .collect();
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("loop-tty-read".to_owned())
        .spawn(move || {
            let mut read = read;
            let mut bytes = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                match std::io::Read::read(&mut read, &mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        bytes.extend_from_slice(&chunk[..n]);
                        if needles.iter().all(|needle| {
                            bytes
                                .windows(needle.len())
                                .any(|window| window == needle.as_slice())
                        }) {
                            break;
                        }
                    }
                    Err(err) => panic!("tty: {err}"),
                }
            }
            done.send((bytes, read)).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    finished
}

/// The read bytes within [`DEADLINE`].
#[track_caller]
fn recv_wire(rx: &mpsc::Receiver<(Vec<u8>, std::fs::File)>) -> (Vec<u8>, std::fs::File) {
    match Deadline::after(DEADLINE).recv(rx) {
        Ok(done) => done,
        Err(err) => panic!("waited {DEADLINE:?} for tty bytes: {err}"),
    }
}

/// Steps the loop once, on a thread: a frame that waits on the hub
/// would hang the test, so the join carries [`DEADLINE`].
fn stepped(
    mut lp: Loop<TestBackend>,
    rx: mpsc::Receiver<Input>,
    input: Input,
) -> (Loop<TestBackend>, mpsc::Receiver<Input>) {
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("loop-step".to_owned())
        .spawn(move || {
            let code = lp.step(input, &rx);
            done.send((code, lp, rx)).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    let (code, lp, rx) = match Deadline::after(DEADLINE).recv(&finished) {
        Ok(ended) => ended,
        Err(err) => panic!("waited {DEADLINE:?} for the frame: {err}"),
    };
    assert_eq!(code, None);
    (lp, rx)
}

/// The second session the ordering test switches to.
const OTHER: &str = "s_bbbbbbbbbbbbbbbb";

#[test]
fn a_graphics_error_event_fails_the_transmitted_image() {
    let (lp, theirs, tx, rx) = inline_opened(image_session());
    let (lp, rx) = stepped(lp, rx, Input::Tick);
    let command = read_display_id(&theirs);
    let (mut lp, _rx) = stepped(lp, rx, display_answer(&command));
    drop(tx);

    lp.handle_events(vec![Event::Reply(Reply::Graphics { id: 1, ok: false })]);

    assert_eq!(
        lp.app.notice(),
        Some("Could not show a.png: the terminal reported an error.")
    );
}

/// A session change deletes the shown image, and the next session's
/// image reaches the terminal without reusing the old id.
#[test]
fn a_session_change_deletes_the_shown_image_and_the_next_session_shows() {
    let (mut lp, theirs, tx, rx) = inline_opened(image_session());
    // A pipe tty: the writer's bytes arrive in order, and the test reads
    // them with a deadline rather than racing the writer thread.
    let (tty_read, tty_write) = std::io::pipe().unwrap_or_else(|err| panic!("pipe: {err}"));
    lp.tty = std::fs::File::from(std::os::fd::OwnedFd::from(tty_write));
    let mut tty_read = std::fs::File::from(std::os::fd::OwnedFd::from(tty_read));
    // Nothing reaches the hub before the first frame draws: the drain
    // after the draw sends the frame's fetches.
    theirs
        .set_nonblocking(true)
        .unwrap_or_else(|err| panic!("nonblocking: {err}"));
    let mut byte = [0u8; 1];
    match std::io::Read::read(&mut &theirs, &mut byte) {
        Err(err) if err.kind() == ErrorKind::WouldBlock => {}
        read => panic!("the hub heard before the first frame: {read:?}"),
    }
    theirs
        .set_nonblocking(false)
        .unwrap_or_else(|err| panic!("blocking: {err}"));
    // The first frame draws the image and asks for its bytes.
    let (lp, rx) = stepped(lp, rx, Input::Tick);
    let id = read_display_id(&theirs);
    // The PNG answer transmits and places, through the writer thread.
    let (mut lp, rx) = stepped(lp, rx, display_answer(&id));
    drop(tx);
    let (sent, read) = recv_wire(&read_until(tty_read, &["Ga=t,", "Ga=p,"]));
    tty_read = read;
    let text = String::from_utf8_lossy(&sent).into_owned();
    text.find("Ga=t,").expect("the transmit went out");
    text.find("Ga=p,").expect("the place went out");
    // The session changes: the shown image is forgotten, and the next
    // frame draws the new session and deletes the old id. The open
    // group carries over, so the new session's grid draws too.
    lp.app.attach(contract::SessionId(OTHER.to_owned()));
    for envelope in image_session() {
        lp.app.on_line(Line::Session(envelope));
    }
    // The loop drains the queued delete in its pre-draw phase and waits
    // for it to reach the wire, so no frame overtakes the delete.
    lp.drain_image_deletes();
    super::tests::flush_kitty(&lp);
    assert!(
        lp.app.take_image_deletes().is_empty(),
        "the pre-draw drain empties the queue"
    );
    let (lp, rx) = stepped(lp, rx, Input::Tick);
    let other = read_display_id(&theirs);
    let (lp, _) = stepped(lp, rx, display_answer(&other));
    let (later, _read) = recv_wire(&read_until(tty_read, &["Ga=d,", "Ga=t,"]));
    drop(lp);
    let text = String::from_utf8_lossy(&later).into_owned();
    assert!(text.contains("Ga=d,"), "the old image delete went out");
    assert!(text.contains("Ga=t,"), "the next session image transmits");
}

/// An upload item for the boundary test: `full` 4096-byte chunks plus a
/// `tail`-byte last chunk, with its place, and every byte the writer must
/// send for it.
fn kitty_image(id: u32, full: usize, tail: usize) -> (super::kitty_writer::Item, Vec<u8>) {
    use super::kitty_writer::Item;

    let payload = "B".repeat(full * 4096 + tail);
    let chunks = crate::graphics::transmit(id, &payload);
    let place = crate::graphics::place(id, 7, 3);
    let mut bytes = Vec::new();
    for chunk in &chunks {
        bytes.extend_from_slice(chunk);
    }
    bytes.extend_from_slice(&place);
    (Item::Image { id, chunks, place }, bytes)
}

/// Reads exactly `n` bytes off the pipe, returning them with the read end
/// so the test can close it for the failure injection.
#[track_caller]
fn read_exact_bytes(read: std::fs::File, n: usize) -> mpsc::Receiver<(Vec<u8>, std::fs::File)> {
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("loop-tty-read".to_owned())
        .spawn(move || {
            let mut read = read;
            let mut buf = vec![0u8; n];
            std::io::Read::read_exact(&mut read, &mut buf)
                .unwrap_or_else(|err| panic!("read {n}: {err}"));
            done.send((buf, read)).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    finished
}

/// The loop draws frames between whole Kitty chunks: a frame stepped while
/// a payload waits lands on the screen with the image's rows blank, and a
/// delete plus a queued image released together send no further first-image
/// chunk and no first place before the full second transmit and place. A
/// failing tty then reports the failure, and the next step quits.
#[test]
fn kitty_event_loop_draws_at_chunk_boundary() {
    use super::kitty_writer::{Delete, Item, KittyChunkGate, KittyWriter};

    let (mut lp, _theirs, tx, rx) = inline_opened(image_session());
    drop(tx);
    // A held gate over a pipe sink: the writer parks after the first chunk.
    let gate = Arc::new(KittyChunkGate::new(true));
    let (pipe_read, pipe_write) = std::io::pipe().unwrap_or_else(|err| panic!("pipe: {err}"));
    let (live_tx, live_rx) = mpsc::channel();
    lp.files_out = live_tx.clone();
    let writer = KittyWriter::spawn(
        std::fs::File::from(std::os::fd::OwnedFd::from(pipe_write)),
        Arc::clone(&gate),
        Arc::clone(&lp.kitty_failed),
        live_tx,
        Arc::clone(&lp.tty_lock),
    )
    .expect("the writer starts");
    lp.kitty_writer = Some(writer);
    // `inline_opened` attached before any frame ran, so the attach's purge
    // is still queued. No frame the test cares about has drained it, and the
    // key frame below would hand it to the writer as an abort of the test's
    // own upload. Drop that stale handoff before the test's payload.
    let _ = lp.app.take_image_out();
    // A large payload pending: the first chunk reads off the pipe with the
    // deadline, and the held gate parks the writer after it.
    let (first, first_bytes) = kitty_image(1, 2, 10);
    let head = match &first {
        Item::Image { chunks, .. } => chunks[0].len(),
        Item::Delete(_) => panic!("an image, not a delete"),
    };
    let installed = lp.kitty_writer.as_ref().expect("writer installed");
    assert!(installed.push(first));
    let (chunk, read) = recv_wire(&read_exact_bytes(
        std::fs::File::from(std::os::fd::OwnedFd::from(pipe_read)),
        head,
    ));
    // docs/tui.md:2529 ("Images"): images transmit in the order queued.
    assert_eq!(chunk, first_bytes[..head].to_vec());
    // A key's frame is written while the payload is pending: the image's
    // rows stay blank, at their size, until it lands.
    let (mut lp, rx) = stepped(lp, rx, Input::Bytes(b"a".to_vec()));
    assert!(shown(&lp).contains('\u{10EEEE}'));
    // The image's delete plus a queued second image released together send
    // no further first-image chunk and no first place, then the full second
    // transmit and place in order; the barrier ack proves the delete went
    // out.
    let (second, second_bytes) = kitty_image(2, 1, 10);
    let installed = lp.kitty_writer.as_ref().expect("writer installed");
    assert!(installed.push(Item::Delete(Delete {
        id: 1,
        bytes: crate::graphics::delete(1),
    })));
    assert!(installed.push(second));
    gate.release();
    super::tests::flush_kitty(&lp);
    let deleted = crate::graphics::delete(1);
    let (rest, read) = recv_wire(&read_exact_bytes(read, deleted.len() + second_bytes.len()));
    // docs/tui.md:2529-2532 ("Images"): the delete goes at the next chunk
    // boundary with no further first-image chunk and no first place, then
    // the full second transmit and place follow in order.
    assert_eq!(
        rest,
        [deleted, second_bytes].concat(),
        "no further first-image chunk and no first place"
    );
    // An injected write error drops what is pending and writes nothing
    // after it: closing the read end fails the next chunk write.
    drop(read);
    let (third, _) = kitty_image(3, 0, 5);
    let installed = lp.kitty_writer.as_ref().expect("writer installed");
    assert!(installed.push(third));
    match Deadline::after(DEADLINE).recv(&live_rx) {
        Ok(Input::KittyFailed) => {}
        Ok(_) => panic!("a failure, not KittyFailed"),
        Err(err) => panic!("waited {DEADLINE:?} for the failure: {err}"),
    }
    assert!(lp.kitty_failed.load(Ordering::SeqCst));
    // The sink holds exactly the error position with nothing after it, and
    // the next step quits through the failed-draw exit.
    assert_eq!(lp.step(Input::KittyFailed, &rx), Some(1));
}

/// `kitty_failed` sets the flag and wakes an idle loop: with either half
/// missing the terminal failure would never quit the loop.
#[test]
fn kitty_failed_flags_and_wakes() {
    let (mut lp, _theirs, _tx, _rx) = opened();
    let (tx, rx) = mpsc::channel();
    lp.files_out = tx;
    lp.kitty_failed();
    assert!(lp.kitty_failed.load(Ordering::SeqCst));
    match Deadline::after(DEADLINE).recv(&rx) {
        Ok(Input::KittyFailed) => {}
        Ok(_) => panic!("a failure wake, not KittyFailed"),
        Err(err) => panic!("waited {DEADLINE:?} for the wake: {err}"),
    }
}

/// `stop_kitty` takes the writer down: nothing outlives the loop.
#[test]
fn stop_kitty_joins_the_writer() {
    let (mut lp, _theirs, _tx, _rx) = opened();
    let (pipe_read, pipe_write) = std::io::pipe().unwrap_or_else(|err| panic!("pipe: {err}"));
    let gate = Arc::new(super::kitty_writer::KittyChunkGate::new(false));
    let (live_tx, _live_rx) = mpsc::channel();
    let writer = super::kitty_writer::KittyWriter::spawn(
        std::fs::File::from(std::os::fd::OwnedFd::from(pipe_write)),
        Arc::clone(&gate),
        Arc::clone(&lp.kitty_failed),
        live_tx,
        Arc::clone(&lp.tty_lock),
    )
    .expect("the writer starts");
    lp.kitty_writer = Some(writer);
    lp.stop_kitty();
    assert!(lp.kitty_writer.is_none(), "the writer is taken down");
    // The join closed the tty: EOF, not a leaked thread.
    let read = std::fs::File::from(std::os::fd::OwnedFd::from(pipe_read));
    let mut rest = Vec::new();
    std::io::Read::read_to_end(&mut &read, &mut rest)
        .unwrap_or_else(|err| panic!("tty eof: {err}"));
    assert!(rest.is_empty());
}

/// `pause_kitty` retains the writer's ids for the post-resume replay: an
/// empty handoff would drop the shown image without re-fetching it.
#[test]
fn pause_kitty_retains_handover_ids() {
    use super::kitty_writer::{Item, KittyWriter};

    let (mut lp, _theirs, _tx, _rx) = opened();
    let gate = Arc::new(super::kitty_writer::KittyChunkGate::new(true));
    let (pipe_read, pipe_write) = std::io::pipe().unwrap_or_else(|err| panic!("pipe: {err}"));
    let (live_tx, _live_rx) = mpsc::channel();
    lp.files_out = live_tx.clone();
    let writer = KittyWriter::spawn(
        std::fs::File::from(std::os::fd::OwnedFd::from(pipe_write)),
        Arc::clone(&gate),
        Arc::clone(&lp.kitty_failed),
        live_tx,
        Arc::clone(&lp.tty_lock),
    )
    .expect("the writer starts");
    lp.kitty_writer = Some(writer);
    // A partial upload behind the held gate, so the pause meets it at
    // the next chunk boundary with a retained id.
    let (upload, _) = kitty_image(5, 1, 10);
    let head = match &upload {
        Item::Image { chunks, .. } => chunks[0].len(),
        Item::Delete(_) => panic!("an image, not a delete"),
    };
    lp.kitty_writer
        .as_ref()
        .expect("writer installed")
        .push(upload);
    let (_bytes, read) = recv_wire(&read_exact_bytes(
        std::fs::File::from(std::os::fd::OwnedFd::from(pipe_read)),
        head,
    ));
    // The send hook orders the release after the Handover the blocking
    // pause cannot order, so the pause meets the partial upload at the
    // boundary on every run.
    let (sent, sent_rx) = mpsc::channel();
    lp.kitty_writer
        .as_ref()
        .expect("writer installed")
        .set_sent_hook(sent);
    std::thread::scope(|scope| {
        let pausing = scope.spawn(|| lp.pause_kitty());
        match Deadline::after(DEADLINE).recv(&sent_rx) {
            Ok(()) => {}
            Err(err) => panic!("waited {DEADLINE:?} for the handover send: {err}"),
        }
        gate.release();
        pausing.join().expect("pause joined");
    });
    assert_eq!(lp.handover_retained, vec![5]);
    lp.stop_kitty();
    drop(read);
}

/// A second handover in the same batch sends nothing: while Resume pends the
/// writer is still parked, so a second pause would wait for an ack that only
/// the post-frame Resume releases.
#[test]
fn pause_kitty_while_resume_pends_skips_a_second_handover() {
    let (mut lp, _theirs, _tx, _rx) = opened();
    let gate = Arc::new(super::kitty_writer::KittyChunkGate::new(false));
    let (_pipe_read, pipe_write) = std::io::pipe().unwrap_or_else(|err| panic!("pipe: {err}"));
    let (live_tx, _live_rx) = mpsc::channel();
    lp.files_out = live_tx.clone();
    let writer = super::kitty_writer::KittyWriter::spawn(
        std::fs::File::from(std::os::fd::OwnedFd::from(pipe_write)),
        Arc::clone(&gate),
        Arc::clone(&lp.kitty_failed),
        live_tx,
        Arc::clone(&lp.tty_lock),
    )
    .expect("the writer starts");
    lp.kitty_writer = Some(writer);
    // Park the writer holding nothing, as after a first handover.
    let parked = lp
        .kitty_writer
        .as_ref()
        .expect("writer installed")
        .handover_send();
    match Deadline::after(DEADLINE).recv(&parked) {
        Ok(ids) => assert!(ids.is_empty()),
        Err(err) => panic!("waited {DEADLINE:?} for the pause ack: {err}"),
    }
    lp.pending_resume = true;
    lp.handover_retained = vec![5];
    // The second pause returns at once without sending: without the early
    // return it would block here until the Deadline instead.
    let (done, finished) = mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            lp.pause_kitty();
            done.send(()).unwrap_or(());
        });
        match Deadline::after(DEADLINE).recv(&finished) {
            Ok(()) => {}
            Err(err) => panic!("waited {DEADLINE:?} for the second pause: {err}"),
        }
    });
    assert_eq!(lp.handover_retained, vec![5]);
    assert!(!lp.kitty_failed.load(Ordering::SeqCst));
    // The writer is still parked exactly once: one Resume releases it.
    lp.kitty_writer
        .as_ref()
        .expect("writer installed")
        .resume_after_handover();
    super::tests::flush_kitty(&lp);
    lp.stop_kitty();
}

/// Two handovers before the first post-resume frame reset once: the second
/// must not re-ask the retained image the parked writer will replay.
#[test]
fn second_handover_before_resume_keeps_retained_shown() {
    let (mut lp, theirs, tx, rx) = inline_opened(image_session());
    // A pipe tty: the writer's transmit proves the image is shown.
    let (tty_read, tty_write) = std::io::pipe().unwrap_or_else(|err| panic!("pipe: {err}"));
    lp.tty = std::fs::File::from(std::os::fd::OwnedFd::from(tty_write));
    let (lp, rx) = stepped(lp, rx, Input::Tick);
    let id = read_display_id(&theirs);
    let (mut lp, rx) = stepped(lp, rx, display_answer(&id));
    drop(tx);
    drop(rx);
    let (sent, _read) = recv_wire(&read_until(
        std::fs::File::from(std::os::fd::OwnedFd::from(tty_read)),
        &["Ga=t,"],
    ));
    assert!(
        String::from_utf8_lossy(&sent).contains("i=1"),
        "the answer transmitted, so image 1 is shown: {sent:?}"
    );
    if let Some(writer) = lp.kitty_writer.take() {
        writer.stop_and_join();
    }
    assert!(lp.app.take_image_out().items.is_empty());
    // The first post-resume reset keeps the retained image shown
    // (`docs/tui.md`, "Images"): handing the terminal over sends every
    // on-screen image again whole, so the writer-replayed image stays shown.
    lp.handover_retained = vec![1];
    lp.pending_resume = false;
    lp.note_handover_resumed();
    assert!(
        lp.app.take_image_out().display.is_empty(),
        "the retained image stays shown after the first resume"
    );
    assert!(lp.pending_resume);
    // A second handover before the first post-resume frame: without the
    // pending check this takes an empty retained list and re-asks the
    // image the writer will replay (`docs/tui.md`, "Images"): handing
    // the terminal over sends every on-screen image again whole, so the
    // parked writer's replay stays the single resend.
    lp.note_handover_resumed();
    assert!(
        lp.app.take_image_out().display.is_empty(),
        "the second resume before the frame asks nothing again"
    );
}

/// The pre-draw delete drain never waits while a resume pends: the writer
/// is still parked in its handover, so a barrier would hang the frame
/// before it ever reaches the resume.
#[test]
fn drain_image_deletes_skips_the_barrier_while_resume_pends() {
    use super::kitty_writer::{KittyChunkGate, KittyWriter};

    let (mut lp, theirs, tx, rx) = inline_opened(image_session());
    drop(tx);
    // The attach's purge is stale: drop it before the test's own payload.
    let _ = lp.app.take_image_out();
    // A pipe tty with a held gate: the writer parks after the first chunk.
    let (tty_read, tty_write) = std::io::pipe().unwrap_or_else(|err| panic!("pipe: {err}"));
    lp.tty = std::fs::File::from(std::os::fd::OwnedFd::from(tty_write));
    let (live_tx, _live_rx) = mpsc::channel();
    lp.files_out = live_tx.clone();
    let gate = Arc::new(KittyChunkGate::new(true));
    let writer = KittyWriter::spawn(
        lp.tty
            .try_clone()
            .unwrap_or_else(|err| panic!("clone: {err}")),
        Arc::clone(&gate),
        Arc::clone(&lp.kitty_failed),
        live_tx,
        Arc::clone(&lp.tty_lock),
    )
    .expect("the writer starts");
    lp.kitty_writer = Some(writer);
    // The first frame draws the image and asks for its bytes; the
    // answer's frame queues the transmit, whose first chunk lands
    // despite the held gate.
    let (lp, rx) = stepped(lp, rx, Input::Tick);
    let id = read_display_id(&theirs);
    let (mut lp, rx) = stepped(lp, rx, display_answer(&id));
    // The transmit started: its only chunk lands despite the held gate,
    // which parks the writer before its place with the upload complete.
    let (_first, first_read) = recv_wire(&read_until(
        std::fs::File::from(std::os::fd::OwnedFd::from(tty_read)),
        &["Ga=t,"],
    ));
    let paused = lp
        .kitty_writer
        .as_ref()
        .expect("writer installed")
        .handover_send();
    gate.release();
    let ids = match Deadline::after(DEADLINE).recv(&paused) {
        Ok(ids) => ids,
        Err(err) => panic!("waited {DEADLINE:?} for the pause ack: {err}"),
    };
    assert_eq!(ids, vec![1]);
    // The single-chunk upload was already complete before its place, so no
    // upload was open and no abort delete goes out: the ack alone proves
    // the pause. (A partial upload's abort is covered in
    // `handover_pauses_at_boundary_and_restarts_whole`.)
    // The session changes: the shown image is forgotten, queuing its
    // delete, and the resume still pends as after a handover.
    lp.app.attach(contract::SessionId(OTHER.to_owned()));
    for envelope in image_session() {
        lp.app.on_line(Line::Session(envelope));
    }
    lp.pending_resume = true;
    // The drain returns without waiting: the barrier it skips would hang
    // on the parked writer past the deadline.
    let (done, finished) = mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            lp.drain_image_deletes();
            done.send(()).unwrap_or(());
        });
        match Deadline::after(DEADLINE).recv(&finished) {
            Ok(()) => {}
            Err(err) => panic!("waited {DEADLINE:?} for the delete drain: {err}"),
        }
    });
    assert!(
        lp.app.take_image_deletes().is_empty(),
        "the pre-draw drain empties the queue"
    );
    // The queued delete reaches the wire after the resume: the skip is
    // only the wait, never the push.
    lp.kitty_writer
        .as_ref()
        .expect("writer installed")
        .resume_after_handover();
    super::tests::flush_kitty(&lp);
    let deleted = crate::graphics::delete(1);
    let (rest, _read) = recv_wire(&read_until(first_read, &["Ga=d,"]));
    assert_eq!(rest, deleted);
    lp.stop_kitty();
    drop(rx);
}

/// With no writer and a resume pending, the delete drain still hands the
/// deletes over: the barrier skip never drops the push.
#[test]
fn drain_image_deletes_with_pending_resume_and_no_writer_still_hands_over() {
    let (lp, theirs, tx, rx) = inline_opened(image_session());
    let (lp, rx) = stepped(lp, rx, Input::Tick);
    let id = read_display_id(&theirs);
    let (mut lp, _rx) = stepped(lp, rx, display_answer(&id));
    drop(tx);
    // The flow's writer is done with: none exists when the drain runs.
    if let Some(writer) = lp.kitty_writer.take() {
        writer.stop_and_join();
    }
    // The session changes: the shown image is forgotten, queuing its delete.
    lp.app.attach(contract::SessionId(OTHER.to_owned()));
    for envelope in image_session() {
        lp.app.on_line(Line::Session(envelope));
    }
    lp.pending_resume = true;
    // A pipe tty for the fresh writer the drain spawns.
    let (tty_read, tty_write) = std::io::pipe().unwrap_or_else(|err| panic!("pipe: {err}"));
    lp.tty = std::fs::File::from(std::os::fd::OwnedFd::from(tty_write));
    lp.drain_image_deletes();
    assert!(
        lp.app.take_image_deletes().is_empty(),
        "the drain empties the queue"
    );
    assert!(
        lp.kitty_writer.is_some(),
        "the drain hands over even with no writer"
    );
    super::tests::flush_kitty(&lp);
    // The delete reaches the wire: the skip is only the wait, never the push.
    let deleted = crate::graphics::delete(1);
    let (bytes, _read) = recv_wire(&read_until(
        std::fs::File::from(std::os::fd::OwnedFd::from(tty_read)),
        &["Ga=d,"],
    ));
    assert_eq!(bytes, deleted);
    lp.stop_kitty();
}

/// A loop at 60x12, attached to the session, with its lines folded and
/// the hub connected: the loop, the hub's end, and the inputs' sender.
fn opened() -> (
    Loop<TestBackend>,
    UnixStream,
    Sender<Input>,
    mpsc::Receiver<Input>,
) {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(60, 12);
    let hello = contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    };
    app.on_line(Line::Hub(hello));
    app.attach(contract::SessionId(SESSION.to_owned()));
    for envelope in session() {
        app.on_line(Line::Session(envelope));
    }
    let (ours, theirs) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    let screen =
        Screen::new(TestBackend::new(60, 12), 60, 12).unwrap_or_else(|err| panic!("screen: {err}"));
    let mut lp = Loop::for_test(app, screen);
    lp.hub.store(ours);
    let (tx, rx) = mpsc::channel();
    (lp, theirs, tx, rx)
}

/// Serves `history` on the hub's end with `script` on its own thread,
/// recording each command, until the loop hangs up.
fn serve(theirs: UnixStream, tx: Sender<Input>, mut script: Script) -> Arc<Mutex<Vec<Value>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&seen);
    std::thread::Builder::new()
        .name("loop-hub".to_owned())
        .spawn(move || {
            let mut reader = BufReader::new(theirs);
            let mut text = String::new();
            while reader.read_line(&mut text).is_ok_and(|read| read > 0) {
                let command: Value = serde_json::from_str(&text).unwrap_or_default();
                text.clear();
                if let Ok(mut held) = record.lock() {
                    held.push(command.clone());
                }
                for input in script(&command) {
                    if tx.send(input).is_err() {
                        return;
                    }
                }
            }
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    seen
}

/// Answers every `history` command from the session's lines.
fn answers_all() -> Script {
    let lines = session();
    Box::new(move |command| {
        let from = command["args"]["from_seq"].as_u64().unwrap_or(0);
        let to = command["args"]["to_seq"].as_u64().unwrap_or(u64::MAX);
        vec![answer(
            "command_accepted",
            json!({"command_id": command["id"], "result": {"lines": history(&lines, from, to)}}),
        )]
    })
}

/// Runs the loop on a thread over `inputs` then a double Ctrl+C, and hands
/// it back once it quits, waiting at most [`DEADLINE`].
#[track_caller]
fn run(
    mut lp: Loop<TestBackend>,
    rx: mpsc::Receiver<Input>,
    tx: &Sender<Input>,
    inputs: Vec<Input>,
) -> Loop<TestBackend> {
    for input in inputs {
        tx.send(input).unwrap_or_else(|err| panic!("send: {err}"));
    }
    tx.send(Input::Bytes(vec![0x03, 0x03]))
        .unwrap_or_else(|err| panic!("send: {err}"));
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("loop-run".to_owned())
        .spawn(move || {
            let code = lp.run(&rx);
            done.send((code, lp)).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    let (code, lp) = match Deadline::after(DEADLINE).recv(&finished) {
        Ok(ended) => ended,
        Err(err) => panic!("waited {DEADLINE:?} for the loop to quit: {err}"),
    };
    assert_eq!(code, 0);
    lp
}

/// The screen the loop drew last.
fn shown(lp: &Loop<TestBackend>) -> String {
    crate::view::text(lp.screen.backend().buffer())
}

/// Enough PageUps to reach the top.
fn to_the_top() -> Vec<Input> {
    (0..80).map(|_| Input::Bytes(PAGE_UP.to_vec())).collect()
}

/// The `(from_seq, to_seq)` of each history command seen.
fn ranges(seen: &Arc<Mutex<Vec<Value>>>) -> Vec<(u64, u64)> {
    seen.lock()
        .map(|held| {
            held.iter()
                .filter(|command| command["command"] == "history")
                .map(|command| {
                    (
                        command["args"]["from_seq"].as_u64().unwrap_or(u64::MAX),
                        command["args"]["to_seq"].as_u64().unwrap_or(u64::MAX),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn a_frame_fetches_a_dropped_page_in_answers_of_256_and_draws_it() {
    let (lp, theirs, tx, rx) = opened();
    assert!(!lp.app.pages().part(0).is_some());
    let seen = serve(theirs, tx.clone(), answers_all());
    let lp = run(lp, rx, &tx, to_the_top());
    let ranges = ranges(&seen);
    // The first page, lines 0 to 512, in three commands split at 256.
    assert!(ranges.contains(&(0, 255)), "{ranges:?}");
    assert!(ranges.contains(&(256, 511)), "{ranges:?}");
    assert!(ranges.contains(&(512, 512)), "{ranges:?}");
    for (from, to) in &ranges {
        assert!(to - from < 256, "{from}..={to}");
    }
    let screen = shown(&lp);
    let rows: Vec<&str> = screen.lines().collect();
    // To the top: the first turn's bubble starts the screen after its
    // blank row, its text on the row after its edge.
    assert!(
        rows.get(1)
            .is_some_and(|row| row.trim_start().starts_with('▄')),
        "{screen}"
    );
    assert!(
        rows.get(2).is_some_and(|row| row.contains("first")),
        "{screen}"
    );
    assert_eq!(lp.app.scroll().0, 0);
    assert!(lp.app.pages().part(0).is_some());
    assert_eq!(lp.app.notice(), None);
    for command in seen.lock().map(|held| held.clone()).unwrap_or_default() {
        assert_eq!(command["session_id"], SESSION);
    }
}

#[test]
fn inputs_during_the_wait_are_handled_after_the_frame_in_order() {
    let (mut lp, theirs, tx, rx) = opened();
    let mut serve_all = answers_all();
    let mut first = true;
    let script: Script = Box::new(move |command| {
        let mut inputs = Vec::new();
        if first {
            first = false;
            // Typed while the frame waits, and an answer to another
            // command.
            inputs.push(Input::Bytes(b"a".to_vec()));
            inputs.push(answer(
                "command_accepted",
                json!({"command_id": "c_other", "result": {"lines": []}}),
            ));
            inputs.push(Input::Resize);
            inputs.push(Input::Bytes(b"b".to_vec()));
        }
        inputs.extend(serve_all(command));
        inputs
    });
    let seen = serve(theirs, tx, script);
    lp.app.jump(0);
    // One frame, on a thread: it blocks on its fetches.
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("loop-step".to_owned())
        .spawn(move || {
            let code = lp.step(Input::Bytes(Vec::new()), &rx);
            done.send((code, lp, rx)).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    let (code, mut lp, rx) = match Deadline::after(DEADLINE).recv(&finished) {
        Ok(ended) => ended,
        Err(err) => panic!("waited {DEADLINE:?} for the frame: {err}"),
    };
    assert_eq!(code, None);
    assert!(!ranges(&seen).is_empty());
    assert!(lp.app.pages().part(0).is_some());
    // Held in arrival order, not yet handled.
    assert_eq!(lp.app.draft(), "");
    let held: Vec<String> = lp
        .hub
        .stashed()
        .iter()
        .map(|input| match input {
            Input::Bytes(bytes) => String::from_utf8_lossy(bytes).into_owned(),
            Input::Hub(Line::Session(line)) => line.kind.clone(),
            Input::Hub(Line::Hub(_))
            | Input::Connected(..)
            | Input::ConnectFailed(_)
            | Input::Disconnected
            | Input::WriteFailed(_)
            | Input::FindDue(_)
            | Input::Tick
            | Input::Signal(_)
            | Input::Files { .. }
            | Input::Models(_)
            | Input::Install { .. }
            | Input::HooksOfferNames { .. }
            | Input::HooksInstalled { .. }
            | Input::Login { .. }
            | Input::Image { .. }
            | Input::Feedback(_)
            | Input::Viewed { .. }
            | Input::KittyFailed => "other".to_owned(),
            Input::Converted { .. } => "other".to_owned(),
            Input::Resize => "resize".to_owned(),
        })
        .collect();
    assert_eq!(held, ["a", "command_accepted", "resize", "b"]);
    while let Some(input) = lp.hub.pop_stashed() {
        assert_eq!(lp.step(input, &rx), None);
    }
    assert_eq!(lp.app.draft(), "ab");
    assert_eq!(lp.app.notice(), None);
}

#[test]
fn a_lost_connection_ends_the_wait_with_the_notice() {
    let (lp, theirs, tx, rx) = opened();
    let script: Script = Box::new(|_| vec![Input::Disconnected]);
    let seen = serve(theirs, tx.clone(), script);
    // The notice row shortens the conversation, so a few more PageUps
    // reach the top than the eighty that sufficed for the floating
    // notice; a fetch already waiting sends nothing more.
    let mut top = to_the_top();
    top.extend((0..40).map(|_| Input::Bytes(PAGE_UP.to_vec())));
    let lp = run(lp, rx, &tx, top);
    assert_eq!(ranges(&seen).len(), 1);
    assert!(!lp.hub.has_stream());
    assert_eq!(
        lp.app.notice(),
        Some("Could not load history: connection lost")
    );
    assert_eq!(lp.app.scroll().0, 0);
    assert!(!lp.app.pages().part(0).is_some());
    // The rows stay, blank.
    assert!(!shown(&lp).contains("first"));
}

#[test]
fn a_drop_during_a_history_fetch_gives_one_retry() {
    let (mut lp, theirs, tx, rx) = opened();
    let retry = crate::retry::Retry::new(&lp.clock);
    lp.hub.set_retry(Arc::clone(&retry));
    let script: Script = Box::new(|_| vec![Input::Disconnected]);
    serve(theirs, tx.clone(), script);
    let lp = run(lp, rx, &tx, to_the_top());
    // One drop, one permit: the first delay.
    assert_eq!(retry.held(), Some(Duration::from_millis(500)));
    assert_eq!(
        lp.app.banner().as_deref(),
        Some("Connection lost · reconnecting (attempt 1)…")
    );
}

#[test]
fn a_rejected_fetch_sets_the_notice_and_the_frame_goes_on() {
    let (lp, theirs, tx, rx) = opened();
    let mut serve_all = answers_all();
    let script: Script = Box::new(move |command| {
        if command["args"]["from_seq"] == 0 {
            vec![answer(
                "command_rejected",
                json!({"command_id": command["id"], "code": "invalid_arguments", "message": "from_seq is past the latest line"}),
            )]
        } else {
            serve_all(command)
        }
    });
    let seen = serve(theirs, tx.clone(), script);
    let lp = run(lp, rx, &tx, to_the_top());
    assert_eq!(
        lp.app.notice(),
        Some("Could not load history: from_seq is past the latest line")
    );
    // The failed page is asked for once; the other pages still load.
    let ranges = ranges(&seen);
    assert_eq!(ranges.iter().filter(|(from, _)| *from == 0).count(), 1);
    assert!(ranges.iter().any(|(from, _)| *from > 512), "{ranges:?}");
    assert!(!lp.app.pages().part(0).is_some());
    assert!(lp.hub.has_stream());
}

#[test]
fn a_frame_with_no_hub_fetches_nothing() {
    let (mut lp, theirs, tx, rx) = opened();
    drop(theirs);
    lp.hub.hang_up();
    let lp = run(lp, rx, &tx, to_the_top());
    assert!(!lp.app.pages().part(0).is_some());
    assert_eq!(lp.app.scroll().0, 0);
}

#[test]
fn an_answer_with_no_lines_fails_the_page_once() {
    let (lp, theirs, tx, rx) = opened();
    let script: Script = Box::new(|command| {
        vec![answer(
            "command_accepted",
            json!({"command_id": command["id"], "result": {"lines": []}}),
        )]
    });
    let seen = serve(theirs, tx.clone(), script);
    let lp = run(lp, rx, &tx, to_the_top());
    assert_eq!(
        lp.app.notice(),
        Some("Could not load history: the answer held none of its lines")
    );
    // Each page is asked for once.
    let ranges = ranges(&seen);
    let mut froms: Vec<u64> = ranges.iter().map(|(from, _)| *from).collect();
    froms.dedup();
    assert_eq!(froms.len(), ranges.len(), "{ranges:?}");
    assert!(!lp.app.pages().part(0).is_some());
}

#[test]
fn a_history_command_that_cannot_be_written_loses_the_connection() {
    let (lp, _theirs, tx, rx) = opened();
    if let Some(hub) = lp.hub.stream() {
        hub.shutdown(std::net::Shutdown::Write)
            .unwrap_or_else(|err| panic!("shutdown: {err}"));
    }
    let lp = run(lp, rx, &tx, to_the_top());
    assert!(!lp.hub.has_stream());
    assert_eq!(
        lp.app.notice(),
        Some("Could not load history: connection lost")
    );
    assert!(!lp.app.pages().part(0).is_some());
}

#[test]
fn the_paging_jig_opens_the_ended_turns_and_appends_the_running_one() {
    let mut lines = session();
    let next = lines.len() as u64;
    lines.push(line(
        "turn_started",
        Some(next),
        None,
        json!({"input": [{"type": "message", "source": "driver", "content": [{"type": "text", "text": "more"}]}]}),
    ));
    lines.push(line(
        "assistant_message_delta",
        None,
        Some("a_live"),
        json!({"text": "streaming"}),
    ));
    let events: String = lines
        .iter()
        .map(|line| serde_json::to_string(line).unwrap_or_default() + "\n")
        .collect();
    let report = crate::measure_paging(&events, 60, 12, fakes::clock::FakeClock::new());
    let report = report.unwrap_or_else(|error| panic!("{error}"));
    assert!(
        report.starts_with(&format!("lines: {}\n", next + 1)),
        "{report}"
    );
    assert!(report.contains("turns: 6\n"), "{report}");
    assert!(report.contains("calls: 102\n"), "{report}");
    assert!(
        report.contains("slowest append frame: 0.00 ms, of 2;"),
        "{report}"
    );
    assert!(report.contains(", of 6 paging up"), "{report}");
    // An unreadable line names its number.
    let error = crate::measure_paging("{\n", 60, 12, fakes::clock::FakeClock::new());
    assert!(error.is_err_and(|error| error.starts_with("line 1:")));
}

/// `lines` as the paging jig reads them, one envelope per line.
fn events(lines: &[Envelope]) -> String {
    lines
        .iter()
        .map(|line| serde_json::to_string(line).unwrap_or_default() + "\n")
        .collect()
}

#[test]
fn the_paging_report_counts_every_loaded_line() {
    let report = crate::measure_paging(&events(&session()), 60, 12, fakes::clock::FakeClock::new())
        .unwrap_or_else(|error| panic!("{error}"));
    // Every page loaded while paging up keeps its first line: dropping one
    // draws fewer rows. Six turns draw a blank row, two bubble edges, a
    // time row and two card edges each, with their blocks between.
    assert!(report.contains("rows: 536\n"), "{report}");
}

#[test]
fn the_paging_report_counts_the_search_matches_across_the_log() {
    let mut lines = session();
    // "shell" in the first turn and the last: the first turn is dropped by
    // the time the search runs, so its match comes from a `history` fetch.
    for line in &mut lines {
        let action = line.action_id.as_ref().map(|id| id.0.as_str());
        let text = match action {
            Some("a_r1_0") => Some("the shell here"),
            Some("a_r6_0") => Some("shell and shell"),
            _ => None,
        };
        if let (Some(text), "text_completed") = (text, line.kind.as_str()) {
            line.payload.insert("text".to_owned(), json!(text));
        }
    }
    let report = crate::measure_paging(&events(&lines), 60, 12, fakes::clock::FakeClock::new())
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(report.contains(", 3 matches\n"), "{report}");
}

#[test]
fn the_paging_report_searches_twice_around_the_recount() {
    let report = crate::measure_paging(&events(&session()), 60, 12, fakes::clock::FakeClock::new())
        .unwrap_or_else(|error| panic!("{error}"));
    // The matching search stays, and the absent search after the resize
    // completes with no match: a frozen clock prints zeros.
    assert!(
        report.contains("search of the whole log: 0.00 ms, 0 matches\n"),
        "{report}"
    );
    assert!(
        report.contains("search of the whole log with no match: 0.00 ms, 0 matches\n"),
        "{report}"
    );
    // The absent search follows the resize, which follows the matching
    // search: each line's place pins the measurement order.
    let at = |label: &str| {
        report
            .lines()
            .position(|line| line.starts_with(label))
            .unwrap_or_else(|| panic!("no {label:?} in {report}"))
    };
    let shell = at("search of the whole log: ");
    let absent = at("search of the whole log with no match: ");
    let recount = at("slowest re-count at a new width: ");
    assert!(shell < recount && recount < absent, "{report}");
}

#[test]
fn the_paging_report_times_the_resize_and_absent_search() {
    let origin = fakes::clock::FakeClock::new().origin();
    let report = crate::measure_paging(&events(&session()), 60, 12, TickClock::clock(origin))
        .unwrap_or_else(|error| panic!("{error}"));
    // A ticking clock times both: the resize from before `set_size`
    // through its frame, and the absent search after it.
    let ms = |label: &str| {
        report
            .lines()
            .find(|line| line.starts_with(label))
            .and_then(|line| {
                line.trim_start_matches(label)
                    .split_once(" ms")
                    .map(|(ms, _)| ms.parse::<f64>().unwrap_or(-1.0))
            })
            .unwrap_or_else(|| panic!("no {label:?} in {report}"))
    };
    assert!(
        ms("slowest re-count at a new width: ") > 0.0,
        "the resize is timed: {report}"
    );
    assert!(
        ms("search of the whole log with no match: ") > 0.0,
        "the absent search is timed: {report}"
    );
}

/// A clock reading a scripted instant per `now()` call: the frame calls
/// `now()` for `items_due`, the pre-draw sweep and the post-draw sweep
/// in that order, so the script sets each sweep's instant exactly. No
/// fake-clock advance and no real-clock read.
struct ScriptClock {
    script: Vec<std::time::Instant>,
    at: std::sync::atomic::AtomicUsize,
}

impl ScriptClock {
    fn clock(script: Vec<std::time::Instant>) -> Arc<Self> {
        Arc::new(Self {
            script,
            at: std::sync::atomic::AtomicUsize::new(0),
        })
    }
}

impl contract::clock::Clock for ScriptClock {
    fn now(&self) -> std::time::Instant {
        let at = self.at.fetch_add(1, Ordering::SeqCst);
        self.script[at.min(self.script.len() - 1)]
    }

    fn wall(&self) -> std::time::SystemTime {
        std::time::SystemTime::UNIX_EPOCH
    }

    fn sleep(&self, _d: Duration) {}

    fn wait_until(
        &self,
        _until: Option<std::time::Instant>,
        wait: &mut dyn FnMut(Option<Duration>),
    ) {
        wait(Some(Duration::ZERO));
    }

    fn subscribe(&self, _waker: std::sync::Weak<dyn contract::clock::Wake>) {}
}

/// The frame sweeps notices before the draw and again before arming.
/// Frame one's script births the pushed event at the pre-draw sweep,
/// shows it in the draw, then expires it at the post-draw sweep past
/// the fade: the draw saw it, but nothing arms and nothing stacks.
/// Frame two births its event after the fade with a steady clock and
/// arms its expiry; frame three past that expiry removes it before
/// the draw, so it never draws. A missing pre-draw sweep would leave
/// frame one's event unborn at the draw and stacked past the post-draw
/// expiry; a missing post-draw sweep would stack it and arm its wake.
#[test]
fn frames_sweep_notices_before_and_after_the_draw() {
    let origin = fakes::clock::FakeClock::new().origin();
    let past = origin + crate::app::FADE + Duration::from_secs(1);
    // Three `now()` reads per frame: `items_due`, the pre-draw sweep,
    // the post-draw sweep.
    let clock = ScriptClock::clock(vec![
        origin,
        origin,
        past, // frame one: birth, then expiry
        past,
        past,
        past, // frame two: birth after the fade
        past + crate::app::FADE + Duration::from_secs(1),
        past + crate::app::FADE + Duration::from_secs(1),
        past + crate::app::FADE + Duration::from_secs(1),
    ]);
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(60, 12);
    app.on_line(Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    }));
    app.attach(contract::SessionId(SESSION.to_owned()));
    let prompt = json!({"input": [{"type": "message", "source": "driver",
        "content": [{"type": "text", "text": "go"}]}]});
    let mut seq = 0u64;
    for (kind, action, payload) in [
        ("turn_started", None, prompt),
        (
            "tool_call_requested",
            Some("a_t0"),
            json!({"name": "read", "arguments": {}}),
        ),
        (
            "assistant_message_completed",
            Some("a_m"),
            json!({"outcome": "completed"}),
        ),
        (
            "tool_call_completed",
            Some("a_t0"),
            json!({"status": "completed", "content": []}),
        ),
        ("turn_completed", None, json!({"outcome": "completed"})),
    ] {
        seq += 1;
        app.on_line(Line::Session(line(kind, Some(seq), action, payload)));
    }
    let (ours, theirs) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    let screen =
        Screen::new(TestBackend::new(60, 12), 60, 12).unwrap_or_else(|err| panic!("screen: {err}"));
    let mut lp = Loop::for_test(app, screen);
    lp.clock = clock.clone();
    lp.hub.store(ours);
    drop(theirs);
    let (_tx, rx) = mpsc::channel();
    lp.app
        .push_notice("io_failed", "frame fade probe".to_owned());
    // Frame one: the pre-draw sweep births the pushed event at the
    // birth instant, the draw shows it, and the post-draw sweep past
    // the fade expires it before arming: drawn once, stacked never.
    assert_eq!(lp.frame(&rx), None);
    assert!(shown(&lp).contains("frame fade probe"), "{}", shown(&lp));
    assert!(lp.app.notices().is_empty());
    assert_eq!(lp.app.take_wake(), None);
    // Frame two after the fade: the pre-draw sweep births the new push
    // at its instant and the arm covers its expiry.
    lp.app.push_notice("io_failed", "second probe".to_owned());
    assert_eq!(lp.frame(&rx), None);
    assert!(shown(&lp).contains("second probe"), "{}", shown(&lp));
    assert_eq!(notice_codes(&lp.app), ["io_failed"]);
    assert_eq!(lp.app.take_wake(), Some(past + crate::app::FADE));
    // Frame three past that expiry: the pre-draw sweep removes it
    // before the draw, so it never draws and nothing arms.
    assert_eq!(lp.frame(&rx), None);
    assert!(lp.app.notices().is_empty());
    assert!(!shown(&lp).contains("second probe"), "{}", shown(&lp));
    assert_eq!(lp.app.take_wake(), None);
}

/// A clock that ticks one millisecond per `now()`, so the paging jig's
/// report holds nonzero durations. The origin is the fake clock's, since
/// reading the process clock is banned in tests.
struct TickClock {
    origin: std::time::Instant,
    ticks: AtomicU64,
}

impl TickClock {
    fn clock(origin: std::time::Instant) -> Arc<Self> {
        Arc::new(Self {
            origin,
            ticks: AtomicU64::new(0),
        })
    }
}

impl contract::clock::Clock for TickClock {
    fn now(&self) -> std::time::Instant {
        let ticks = self.ticks.fetch_add(1, Ordering::SeqCst);
        self.origin
            .checked_add(Duration::from_millis(ticks))
            .unwrap_or(self.origin)
    }

    fn wall(&self) -> std::time::SystemTime {
        std::time::SystemTime::UNIX_EPOCH
    }

    fn sleep(&self, _d: Duration) {}

    fn wait_until(
        &self,
        _until: Option<std::time::Instant>,
        wait: &mut dyn FnMut(Option<Duration>),
    ) {
        wait(Some(Duration::ZERO));
    }

    fn subscribe(&self, _waker: std::sync::Weak<dyn contract::clock::Wake>) {}
}

#[test]
fn the_paging_report_prints_milliseconds() {
    let origin = fakes::clock::FakeClock::new().origin();
    let report = crate::measure_paging(&events(&session()), 60, 12, TickClock::clock(origin))
        .unwrap_or_else(|error| panic!("{error}"));
    let line = report
        .lines()
        .find(|line| line.starts_with("open pass and first frame: "))
        .unwrap_or_else(|| panic!("no open pass line in {report}"));
    // Seconds per frame would print here as milliseconds: with a ticking
    // clock the open pass takes whole milliseconds, never zero.
    let ms: f64 = line
        .trim_start_matches("open pass and first frame: ")
        .trim_end_matches(" ms")
        .parse()
        .unwrap_or_else(|error| panic!("{error} in {report}"));
    assert!(ms > 0.0, "{report}");
}

#[test]
fn paging_up_ends_at_the_top() {
    let events = events(&session());
    let (done, finished) = mpsc::channel();
    std::thread::spawn(move || {
        done.send(crate::measure_paging(
            &events,
            60,
            12,
            fakes::clock::FakeClock::new(),
        ))
        .unwrap_or(());
    });
    // On a thread with a wall-clock deadline: a loop bound that never ends
    // fails here, instead of hanging the suite.
    let report = Deadline::after(DEADLINE)
        .recv(&finished)
        .unwrap_or_else(|error| panic!("waited {DEADLINE:?} for the paging jig: {error}"));
    assert!(report.is_ok(), "{report:?}");
}

#[test]
fn the_paging_report_names_the_furthest_jump_row() {
    let report = crate::measure_paging(&events(&session()), 60, 12, fakes::clock::FakeClock::new())
        .unwrap_or_else(|error| panic!("{error}"));
    let total: usize = report
        .lines()
        .find_map(|line| line.strip_prefix("rows: "))
        .and_then(|rows| rows.parse().ok())
        .unwrap_or_else(|| panic!("no rows in {report}"));
    // "slowest jump frame: 0.00 ms, of 20 to row 297": the jumps spread
    // across the session, so the furthest is total * (jumps - 1) / jumps.
    // `%` or `*` for `/` lands within a screen or past the session.
    let jump = report
        .lines()
        .find(|line| line.starts_with("slowest jump frame: "))
        .unwrap_or_else(|| panic!("no jump line in {report}"));
    let after = jump
        .split_once(", of ")
        .map(|(_, rest)| rest)
        .unwrap_or_else(|| panic!("no jump count in {report}"));
    let (count, row) = after
        .split_once(" to row ")
        .unwrap_or_else(|| panic!("no jump row in {report}"));
    let (count, row): (usize, usize) = (
        count
            .parse()
            .unwrap_or_else(|error| panic!("{error} in {report}")),
        row.parse()
            .unwrap_or_else(|error| panic!("{error} in {report}")),
    );
    assert_eq!(row, total.saturating_mul(count - 1) / count, "{report}");
}

#[test]
fn a_selection_waiting_on_a_page_copies_in_the_same_step() {
    let (mut lp, theirs, tx, rx) = opened();
    let dir = fakes::TempDir::new("tui-select-page");
    let path = dir.path().join("tty");
    lp.tty = std::fs::File::create(&path).unwrap_or_else(|err| panic!("tty: {err}"));
    let seen = serve(theirs, tx.clone(), answers_all());
    // At the top, page 0 loads; a drag over its first rows, past the
    // right-aligned bubble text, then End, which drops it, then the
    // release: the copy waits on page 0, which the same step fetches,
    // and copies.
    let mut inputs = to_the_top();
    inputs.push(Input::Bytes(b"\x1b[<0;1;1M\x1b[<32;60;3M".to_vec()));
    inputs.push(Input::Bytes(b"\x1b[F".to_vec()));
    inputs.push(Input::Bytes(b"\x1b[<0;60;3m".to_vec()));
    let _lp = run(lp, rx, &tx, inputs);
    // The release waits on page 0, which the same step fetches: the copy
    // below proves the wait resolved.
    let before = ranges(&seen).iter().filter(|range| range.0 == 0).count();
    assert!(
        before >= 2,
        "page 0 was not fetched again: {:?}",
        ranges(&seen)
    );
    let written = std::fs::read(&path).unwrap_or_else(|err| panic!("read tty: {err}"));
    let text = String::from_utf8_lossy(&written).into_owned();
    let start = text.find("\x1b]52;c;").expect("an OSC 52 copy");
    let payload: String = text
        .get(start + 7..)
        .unwrap_or_default()
        .chars()
        .take_while(|ch| *ch != '\x07')
        .collect();
    use base64::Engine as _;
    let copied = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .unwrap_or_else(|err| panic!("base64: {err}"));
    let copied = String::from_utf8_lossy(&copied).into_owned();
    assert!(copied.contains("first"), "{copied:?}");
}

/// A backend wrapping `TestBackend` that counts `flush` calls: one per
/// frame written, since a frame identical to the last writes nothing.
struct CountingBackend {
    inner: TestBackend,
    flushes: std::rc::Rc<std::cell::Cell<usize>>,
}

impl crate::screen::SyncEmit for CountingBackend {
    fn emit(&mut self, _begin: bool) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl ratatui::backend::Backend for CountingBackend {
    type Error = <TestBackend as ratatui::backend::Backend>::Error;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a ratatui::buffer::Cell)>,
    {
        self.inner.draw(content)
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> Result<ratatui::layout::Position, Self::Error> {
        self.inner.get_cursor_position()
    }

    fn set_cursor_position<P: Into<ratatui::layout::Position>>(
        &mut self,
        position: P,
    ) -> Result<(), Self::Error> {
        self.inner.set_cursor_position(position)
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ratatui::backend::ClearType) -> Result<(), Self::Error> {
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> Result<ratatui::layout::Size, Self::Error> {
        self.inner.size()
    }

    fn window_size(&mut self) -> Result<ratatui::backend::WindowSize, Self::Error> {
        self.inner.window_size()
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.flushes.set(self.flushes.get().saturating_add(1));
        self.inner.flush()
    }
}

/// A loop on the counting backend at 60x12, attached to the session as
/// [`opened`] does but with no lines folded yet, and its flush count.
fn counting() -> (
    Loop<CountingBackend>,
    std::rc::Rc<std::cell::Cell<usize>>,
    Sender<Input>,
    mpsc::Receiver<Input>,
) {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(60, 12);
    let hello = contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    };
    app.on_line(Line::Hub(hello));
    app.attach(contract::SessionId(SESSION.to_owned()));
    let flushes = std::rc::Rc::new(std::cell::Cell::new(0));
    let backend = CountingBackend {
        inner: TestBackend::new(60, 12),
        flushes: std::rc::Rc::clone(&flushes),
    };
    let screen = Screen::new(backend, 60, 12).unwrap_or_else(|err| panic!("screen: {err}"));
    let lp = Loop::for_test(app, screen);
    let (tx, rx) = mpsc::channel();
    (lp, flushes, tx, rx)
}

#[test]
fn queued_hub_lines_draw_one_frame() {
    let (mut lp, flushes, tx, rx) = counting();
    let mut seq = 0u64;
    let mut hub = |kind: &str, action: Option<&str>, payload: Value| {
        let envelope = line(kind, Some(seq), action, payload);
        seq += 1;
        tx.send(Input::Hub(Line::Session(envelope)))
            .unwrap_or_else(|err| panic!("send: {err}"));
    };
    for (turn, marker) in ["marker one", "marker two", "marker three"]
        .into_iter()
        .enumerate()
    {
        hub(
            "turn_started",
            None,
            json!({"input": [{"type": "message", "source": "driver",
                "content": [{"type": "text", "text": format!("prompt {turn}")}]}]}),
        );
        hub(
            "text_completed",
            Some(&format!("a_m{turn}")),
            json!({"text": marker}),
        );
        hub("turn_completed", None, json!({"outcome": "completed"}));
    }
    drop(tx);
    // Every line queued before the loop runs: one run of waiting lines
    // draws one frame.
    assert_eq!(lp.run(&rx), 0);
    assert_eq!(flushes.get(), 1, "one frame for the queued lines");
    let screen = crate::view::text(lp.screen.backend().inner.buffer());
    assert!(screen.contains("marker three"), "{screen}");
    assert!(!screen.contains("Esc to interrupt"), "{screen}");
}

/// Runs `lp` over `inputs` queued before it starts, to the end of input.
#[track_caller]
fn run_queued(
    lp: &mut Loop<CountingBackend>,
    rx: mpsc::Receiver<Input>,
    tx: Sender<Input>,
    inputs: Vec<Input>,
) -> i32 {
    for input in inputs {
        tx.send(input).unwrap_or_else(|err| panic!("send: {err}"));
    }
    drop(tx);
    lp.run(&rx)
}

/// One turn's lines with `marker` as its reply, numbered from `seq`.
fn turn_lines(marker: &str, turn: usize, seq: &mut u64) -> Vec<Input> {
    let mut lines = Vec::new();
    let mut push = |kind: &str, action: Option<String>, payload: Value| {
        lines.push(Input::Hub(Line::Session(line(
            kind,
            Some(*seq),
            action.as_deref(),
            payload,
        ))));
        *seq += 1;
    };
    push(
        "turn_started",
        None,
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": format!("prompt {turn}")}]}]}),
    );
    push(
        "text_completed",
        Some(format!("a_m{turn}")),
        json!({"text": marker}),
    );
    push("turn_completed", None, json!({"outcome": "completed"}));
    lines
}

/// The screen a counting loop drew last.
fn counted(lp: &Loop<CountingBackend>) -> String {
    crate::view::text(lp.screen.backend().inner.buffer())
}

#[test]
fn one_batch_counts_its_open_page_once() {
    let (mut lp, _, tx, rx) = counting();
    let mut seq = 0u64;
    let mut inputs = turn_lines("marker one", 0, &mut seq);
    inputs.truncate(2);
    let before = lp.app.pages().recounts;
    assert_eq!(run_queued(&mut lp, rx, tx, inputs), 0);
    // The batch folds both lines before it counts: one recount for the
    // batch, not one per changed line.
    assert_eq!(lp.app.pages().recounts - before, 1);
    let screen = counted(&lp);
    assert!(screen.contains("marker one"), "{screen}");
}

#[test]
fn one_line_alone_counts_its_page_at_once() {
    let (mut lp, _, tx, rx) = counting();
    let mut seq = 0u64;
    let mut inputs = turn_lines("marker one", 0, &mut seq);
    inputs.truncate(1);
    let before = lp.app.pages().recounts;
    assert_eq!(run_queued(&mut lp, rx, tx, inputs), 0);
    // A batch of one counts its page once, at its end.
    assert_eq!(lp.app.pages().recounts - before, 1);
}

#[test]
fn a_batch_with_no_changed_line_counts_nothing() {
    let (mut lp, _, tx, rx) = counting();
    let mut seq = 0u64;
    let mut step = || {
        let envelope = line("step_started", Some(seq), None, json!({}));
        seq += 1;
        Input::Hub(Line::Session(envelope))
    };
    let inputs = vec![step(), step()];
    let before = lp.app.pages().recounts;
    assert_eq!(run_queued(&mut lp, rx, tx, inputs), 0);
    // Neither line changed a card, so the batch marks nothing and
    // counts nothing.
    assert_eq!(lp.app.pages().recounts - before, 0);
}

#[test]
fn lines_a_key_and_lines_draw_three_frames() {
    let (mut lp, flushes, tx, rx) = counting();
    let mut seq = 0u64;
    let mut inputs = turn_lines("marker one", 0, &mut seq);
    inputs.push(Input::Bytes(b"a".to_vec()));
    inputs.extend(turn_lines("marker two", 1, &mut seq));
    // The lines share a frame each, and the key keeps its own.
    assert_eq!(run_queued(&mut lp, rx, tx, inputs), 0);
    assert_eq!(flushes.get(), 3);
    assert_eq!(lp.app.draft(), "a");
    let screen = counted(&lp);
    assert!(screen.contains("marker two"), "{screen}");
    assert!(!screen.contains("Esc to interrupt"), "{screen}");
}

#[test]
fn more_than_a_batch_of_lines_draws_two_frames() {
    let (mut lp, flushes, tx, rx) = counting();
    let full = super::batch::HUB_BATCH / 3;
    let mut seq = 0u64;
    let mut inputs = Vec::new();
    for turn in 0..full {
        inputs.extend(turn_lines(&format!("marker {turn}"), turn, &mut seq));
    }
    assert_eq!(inputs.len(), super::batch::HUB_BATCH - 1);
    inputs.extend(turn_lines(&format!("marker {full}"), full, &mut seq));
    inputs.truncate(super::batch::HUB_BATCH + 1);
    assert_eq!(run_queued(&mut lp, rx, tx, inputs), 0);
    assert_eq!(flushes.get(), 2);
    let screen = counted(&lp);
    assert!(screen.contains(&format!("marker {full}")), "{screen}");
    assert!(!screen.contains("Esc to interrupt"), "{screen}");
}

#[test]
fn a_tick_among_queued_lines_draws_no_frame_of_its_own() {
    let (mut lp, flushes, tx, rx) = counting();
    let mut seq = 0u64;
    let mut inputs = turn_lines("marker one", 0, &mut seq);
    inputs.push(Input::Tick);
    inputs.extend(turn_lines("marker two", 1, &mut seq));
    assert_eq!(run_queued(&mut lp, rx, tx, inputs), 0);
    assert_eq!(flushes.get(), 1);
    let screen = counted(&lp);
    assert!(screen.contains("marker two"), "{screen}");
    assert!(!screen.contains("Esc to interrupt"), "{screen}");
}

#[test]
fn a_disconnect_among_lines_ends_the_batch_with_its_own_frame() {
    let (mut lp, flushes, tx, rx) = counting();
    let (ours, _theirs) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    let hello = contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    };
    let mut seq = 0u64;
    let mut inputs = vec![Input::Connected(ours, hello)];
    inputs.extend(turn_lines("marker one", 0, &mut seq));
    inputs.push(Input::Disconnected);
    inputs.extend(turn_lines("marker two", 1, &mut seq));
    assert_eq!(run_queued(&mut lp, rx, tx, inputs), 0);
    // The connect, the first turn, the disconnect and the second turn
    // each draw their own frame.
    assert_eq!(flushes.get(), 4);
    assert_eq!(lp.app.notice(), Some("Connection lost."));
    let screen = counted(&lp);
    assert!(screen.contains("marker two"), "{screen}");
}

/// One finished `attention` line naming `name`.
fn finished(name: &str) -> Input {
    Input::Hub(Line::Hub(contract::HubLine {
        kind: "attention".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::json!({
            "session_id": "s_aaaaaaaaaaaaaaaa",
            "name": name,
            "reason": "finished",
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    }))
}

#[test]
fn two_finished_lines_in_one_batch_write_both_notifications_in_order() {
    let dir = fakes::TempDir::new("tui-attention-batch");
    let path = dir.path().join("tty");
    let tty = std::fs::File::create(&path).unwrap_or_else(|err| panic!("create: {err}"));
    let (mut lp, _) = super::tests::new_loop(TestBackend::new(60, 12), Some(tty));
    lp.app.set_osc9(true);
    // Both lines fold into one batch, so one frame queues both alerts:
    // neither may be lost.
    assert_eq!(
        super::tests::feed(&mut lp, vec![finished("one"), finished("two")]),
        0
    );
    let bytes = std::fs::read(&path).unwrap_or_else(|err| panic!("read: {err}"));
    let one = "\x1b]9;Fiber: one finished\x07";
    let two = "\x1b]9;Fiber: two finished\x07";
    let count = |needle: &str| {
        bytes
            .windows(needle.len())
            .filter(|w| *w == needle.as_bytes())
            .count()
    };
    assert_eq!(count(one), 1);
    assert_eq!(count(two), 1);
    let at = |needle: &str| {
        bytes
            .windows(needle.len())
            .position(|w| w == needle.as_bytes())
            .unwrap_or(usize::MAX)
    };
    assert!(at(one) < at(two));
}

/// The resume test's hub thread's last step: the last command name it
/// read and whether a socket write is in progress. The hub sets each
/// field and drops the guard before any blocking call, so the panic
/// path can always read it.
#[derive(Clone, Debug, Default)]
struct HubProgress {
    last_command: String,
    writing: bool,
}

/// One helper thread's end state from its done channel: finished, still
/// working, or gone without answering.
fn done_state(done: &Result<(), mpsc::TryRecvError>) -> &'static str {
    match done {
        Ok(()) => "done",
        Err(mpsc::TryRecvError::Empty) => "waiting",
        Err(mpsc::TryRecvError::Disconnected) => "gone",
    }
}

/// The stall report for the resume test's drain window expiring: names
/// whether the run thread is alive (the finished channel is empty) or
/// gone (disconnected), the hub's last step, and both helpers' end states.
/// Pure, so unit tests pin its exact output for the covered end states.
fn stall_report(
    finished: &mpsc::TryRecvError,
    hub: &HubProgress,
    hub_done: &Result<(), mpsc::TryRecvError>,
    reader_done: &Result<(), mpsc::TryRecvError>,
) -> String {
    format!(
        "{finished:?}; hub at {:?} (writing: {}), hub {}; reader {}",
        hub.last_command,
        hub.writing,
        done_state(hub_done),
        done_state(reader_done),
    )
}

#[test]
fn stall_report_names_a_parked_hub_while_the_run_thread_lives() {
    let hub = HubProgress {
        last_command: "subscribe".to_owned(),
        writing: false,
    };
    let waiting: Result<(), mpsc::TryRecvError> = Err(mpsc::TryRecvError::Empty);
    let report = stall_report(&mpsc::TryRecvError::Empty, &hub, &waiting, &waiting);
    assert_eq!(
        report,
        "Empty; hub at \"subscribe\" (writing: false), hub waiting; reader waiting",
    );
}

#[test]
fn stall_report_names_a_gone_run_thread() {
    let hub = HubProgress {
        last_command: "history".to_owned(),
        writing: true,
    };
    let waiting: Result<(), mpsc::TryRecvError> = Err(mpsc::TryRecvError::Empty);
    let report = stall_report(&mpsc::TryRecvError::Disconnected, &hub, &waiting, &waiting);
    assert_eq!(
        report,
        "Disconnected; hub at \"history\" (writing: true), hub waiting; reader waiting",
    );
}

#[test]
fn stall_report_names_finished_helpers() {
    let hub = HubProgress {
        last_command: "subscribe".to_owned(),
        writing: false,
    };
    let report = stall_report(&mpsc::TryRecvError::Empty, &hub, &Ok(()), &Ok(()));
    assert_eq!(
        report,
        "Empty; hub at \"subscribe\" (writing: false), hub done; reader done",
    );
}

/// Pumps folds until `stop` arrives: each fold signal renews the
/// no-progress window and sends one release, so a slow machine folds
/// every chunk within the window; `stop`'s arrival ends the pump, so the
/// window never covers a held answer. A stop that lands while the pump
/// waits still ends it through the recheck: the hub's park arrives with
/// a ping on the fold channel, and the run thread's finish arrives as
/// its fold senders hang up. `idle` names the wait in the error when
/// neither a fold nor the stop arrives.
fn pump_until_stop<T>(
    window: Duration,
    reached: &mpsc::Receiver<()>,
    release: &mpsc::Sender<()>,
    stop: &mpsc::Receiver<T>,
    idle: &str,
) -> Result<T, String> {
    loop {
        if let Ok(stop) = stop.try_recv() {
            return Ok(stop);
        }
        // One fresh window per signal through the shared deadline
        // type: each fold renews the no-progress bound.
        match Deadline::after(window).recv(reached) {
            Ok(()) => {
                release.send(()).unwrap_or(());
            }
            Err(_) => match stop.try_recv() {
                Ok(stop) => return Ok(stop),
                Err(_) => return Err(format!("{idle} for {window:?}")),
            },
        }
    }
}

/// Drains the run thread chunk by chunk: each fold signal renews the
/// no-progress window and sends one release, so a slow machine folds
/// every chunk within the window while the whole log takes what it
/// takes. The window bounds one chunk, never the whole replay.
fn drain_by_chunk<T>(
    window: Duration,
    reached: &mpsc::Receiver<()>,
    release: &mpsc::Sender<()>,
    finished: &mpsc::Receiver<T>,
) -> Result<T, String> {
    pump_until_stop(window, reached, release, finished, "no fold")
}

/// Unpacks a waited run thread return: the value, or a panic naming the
/// run thread's state and each helper thread's last step, so the next
/// sighting shows which stall candidate happened.
#[track_caller]
#[allow(clippy::panic, reason = "a test helper; a failure is the test's")]
fn unpack_wait<T>(
    waited: Result<T, String>,
    prefix: String,
    finished: &mpsc::Receiver<(i32, Loop<TestBackend>)>,
    hub_progress: &Mutex<HubProgress>,
    hub_done: &mpsc::Receiver<()>,
    reader_done: &mpsc::Receiver<()>,
) -> T {
    match waited {
        Ok(done) => done,
        Err(err) => {
            let prefix = format!("{prefix}: {err}");
            match finished.try_recv() {
                Ok(_) => panic!("{prefix}\nrun thread finished after the drain gave up"),
                Err(finished_state) => panic!(
                    "{prefix}\n{}",
                    stall_report(
                        &finished_state,
                        &hub_progress
                            .lock()
                            .map(|held| held.clone())
                            .unwrap_or_default(),
                        &hub_done.try_recv(),
                        &reader_done.try_recv(),
                    ),
                ),
            }
        }
    }
}

#[test]
fn the_park_ends_the_pump_without_a_fold() {
    let (_reached_tx, reached) = mpsc::channel();
    let (release_tx, release) = mpsc::channel();
    let (parked_tx, parked) = mpsc::channel();
    parked_tx
        .send(())
        .unwrap_or_else(|err| panic!("park: {err}"));
    drop(parked_tx);
    pump_until_stop(
        MUST_SUCCEED_WITHIN,
        &reached,
        &release_tx,
        &parked,
        "no fold nor park",
    )
    .unwrap_or_else(|err| panic!("{err}"));
    assert!(release.try_recv().is_err(), "no fold was released");
}

#[test]
fn no_fold_and_no_park_fails_the_pump_window() {
    let (reached_tx, reached) = mpsc::channel();
    let (release_tx, _release) = mpsc::channel();
    let (parked_tx, parked) = mpsc::channel::<()>();
    // No fold and no park will ever arrive: both senders hang up, so
    // the pump fails on the hangup without outlasting any window.
    drop(reached_tx);
    drop(parked_tx);
    let err = pump_until_stop(
        MUST_SUCCEED_WITHIN,
        &reached,
        &release_tx,
        &parked,
        "no fold nor park",
    )
    .expect_err("the pump ended with no fold and no park");
    assert!(err.contains("no fold nor park"), "{err}");
}

#[test]
fn a_held_fold_fails_the_no_progress_window() {
    let (mut lp, _) = super::tests::new_loop(TestBackend::new(60, 12), None);
    lp.app.attach(contract::SessionId(SESSION.to_owned()));
    let (tx, rx) = mpsc::channel();
    let (reached_tx, reached) = mpsc::channel();
    let (release_tx, release) = mpsc::channel();
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("held-fold-run".to_owned())
        .spawn(move || {
            super::pause::install(reached_tx, release);
            let code = lp.run(&rx);
            done.send((code, lp)).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    tx.send(Input::Hub(Line::Session(line(
        "turn_started",
        Some(0),
        None,
        json!({"input": []}),
    ))))
    .unwrap_or_else(|err| panic!("send: {err}"));
    drop(tx);
    // The loop holds before its first fold: one hang guard for the hold.
    Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&reached, "the loop to hold before the fold");
    // No release is sent, so the pump fails its window instead of
    // waiting out the whole drain.
    let window = Duration::from_millis(200);
    let err = match drain_by_chunk(window, &reached, &release_tx, &finished) {
        Ok(_) => panic!("the pump drained a held fold"),
        Err(err) => err,
    };
    assert!(err.contains("no fold"), "{err}");
    // Dropping the release sender lets the held fold through, so the
    // loop thread finishes; one hang guard for its return.
    drop(release_tx);
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv_or_fail(&finished, "the loop to quit after the release");
}

/// Lower-case words, the same for the seed.
fn filler(len: usize, seed: u64) -> String {
    const WORDS: [&str; 8] = [
        "turn", "tool", "call", "file", "session", "context", "model", "log",
    ];
    let mut state = seed;
    let mut text = String::with_capacity(len + 8);
    while text.len() < len {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let pick = usize::try_from(state >> 61).unwrap_or_default();
        text.push_str(WORDS.get(pick).copied().unwrap_or("x"));
        text.push(' ');
    }
    text.truncate(len);
    text
}

/// A log of `turns` handoff turns with 4 KiB replies, the last reply
/// `last`: one screenful per reply, a handoff between turns.
fn handoff_log(turns: u64, last: &str) -> Vec<Envelope> {
    let mut lines = Vec::new();
    let mut seq = 0u64;
    let mut push = |kind: &str, action: Option<&str>, payload: Value| {
        lines.push(line(kind, Some(seq), action, payload));
        seq += 1;
    };
    for turn in 0..turns {
        push(
            "turn_started",
            None,
            json!({"input": [{"type": "message", "source": "driver", "content": [{"type": "text", "text": format!("turn {turn}")}]}]}),
        );
        push("step_started", None, json!({}));
        let message = format!("a_m{turn}");
        push("assistant_message_started", Some(&message), json!({}));
        let text = if turn + 1 == turns {
            last.to_owned()
        } else {
            filler(4096, turn)
        };
        push("text_completed", Some(&message), json!({"text": text}));
        push(
            "assistant_message_completed",
            Some(&message),
            json!({"outcome": "completed"}),
        );
        push("turn_completed", None, json!({"outcome": "completed"}));
        push("usage_recorded", None, json!({}));
        if turn + 1 < turns {
            push("handoff_started", None, json!({"trigger": "manual"}));
            push(
                "handoff_completed",
                None,
                json!({"outcome": "completed", "tokens_before": 1000}),
            );
        }
    }
    lines
}

/// A batch settles once at its end: trimming mid-batch would drop pages
/// the batch's uncounted rows still hold in the window, so the head pages
/// stay resident until the end, and only the exact counts trim them.
#[test]
fn a_batch_settles_once_at_its_end() {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(60, 12);
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.begin_batch();
    assert!(app.pages().holding());
    for envelope in handoff_log(150, "the last reply holds quokkas") {
        app.on_line(Line::Session(envelope));
    }
    assert!(app.pages().part(0).is_some());
    assert!(app.pages().part(1).is_some());
    app.end_batch();
    assert!(!app.pages().holding());
    assert!(app.pages().page_count() > 2);
    // The head leaves once the counts are exact.
    assert!(app.pages().part(0).is_none());
}

/// `fiber resume` opens through the hub: feed, recent, the `summary`
/// subscribe and the session's commands, then the newest `history` window
/// for the first frame. The post-frame count reads older windows through
/// seq zero before `full`; replay then lands the 150 turns / 1,348 envelopes
/// without fetching a page twice.
#[test]
fn resume_open_replays_the_log_to_the_tail() {
    use std::io::{BufRead, BufReader, Write};
    let lines = handoff_log(150, "the last reply holds quokkas");
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(crate::home::Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        hover: true,
        version: "0.0.1".to_owned(),
        model: None,
        thinking: None,
        logo_glyph: "⌇".to_owned(),
        keys: crate::KeysSetup::default(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: Vec::new(),
        open_at: crate::OpenAt::Session(contract::SessionId(SESSION.to_owned()), None),
        ..Default::default()
    });
    app.set_size(60, 12);
    let screen =
        Screen::new(TestBackend::new(60, 12), 60, 12).unwrap_or_else(|err| panic!("screen: {err}"));
    let mut lp = Loop::for_test(app, screen);
    let (tx, rx) = mpsc::channel();
    let (tui_end, hub_end) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    let read_end = tui_end
        .try_clone()
        .unwrap_or_else(|err| panic!("clone: {err}"));
    let read_tx = tx.clone();
    // The reader takes at the loop's credits, so its queue stays
    // bounded while the loop drains.
    let read_feed = crate::link::Feed::new(read_tx, lp.hub.credits());
    // The reader signals when it reaches EOF, so the test waits on the
    // reader's answer instead of the drain window alone.
    let (reader_done_tx, reader_done) = mpsc::channel();
    std::thread::Builder::new()
        .name("resume-read".to_owned())
        .spawn(move || {
            crate::link::read_lines(read_end, &read_feed);
            reader_done_tx.send(()).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    // The hub: drains requests on a read thread while it streams the
    // 150 turns / 1,348 envelopes on subscribe, answers them in order,
    // then hangs up, so the loop drains and returns.
    let stream = lines.clone();
    // Every replayed turn start asks `commands` again: the hub serves
    // those requests, then hangs up once each has been answered.
    let replay_turns = lines
        .iter()
        .filter(|envelope| envelope.kind == "turn_started")
        .count();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&seen);
    // The hub's last step for the stall report: the hub sets each field
    // and drops the guard before any blocking call, so the panic path
    // can always read it.
    let hub_progress = Arc::new(Mutex::new(HubProgress::default()));
    let hub_step = Arc::clone(&hub_progress);
    // The pause point holding the subscribe answer: the hub signals it
    // reached subscribe, then waits on a channel whose sender the test
    // owns, so the hold needs no deadline of its own. The test sends to
    // release it, and dropping the sender on panic releases it too.
    let (subscribed_tx, subscribed) = mpsc::channel();
    let (release_tx, release) = mpsc::channel();
    // The fold seam the pump and the drain share: one `reached` per hub
    // chunk, one `release` per signal. The hub holds a clone to ping the
    // pump awake when it parks at history.
    let (fold_reached_tx, fold_reached) = mpsc::channel();
    let (fold_release_tx, fold_release) = mpsc::channel();
    let fold_ping_tx = fold_reached_tx.clone();
    // The pause point holding the history answer: the hub signals it
    // read history, then waits on a channel whose sender the test owns,
    // so the hold needs no deadline of its own. The pump releases folds
    // until the park, then the test releases the answer, so the drain
    // that follows bounds replay chunks only, never the held answer.
    // Dropping the sender on panic releases the hub the same way.
    let (history_parked_tx, history_parked) = mpsc::channel();
    let (history_release_tx, history_release) = mpsc::channel();
    // The hub signals when it finishes its writes, so the test waits on
    // the hub's answer instead of the drain window alone.
    let (hub_done_tx, hub_done) = mpsc::channel();
    // The drain stays ahead of the stream: each replayed turn start asks
    // `commands` again while the hub still writes, so reading on the
    // hub's thread would fill the socket and stall both sides. The read
    // thread queues requests in arrival order; the hub below handles
    // them in that order once its writes allow.
    let (queue_tx, queue_rx) = mpsc::channel();
    let hub_read = hub_end
        .try_clone()
        .unwrap_or_else(|err| panic!("clone: {err}"));
    let read_step = Arc::clone(&hub_progress);
    std::thread::Builder::new()
        .name("resume-hub-read".to_owned())
        .spawn(move || {
            let mut reader = BufReader::new(hub_read);
            let mut text = String::new();
            while reader.read_line(&mut text).is_ok_and(|read| read > 0) {
                let command: Value = serde_json::from_str(&text).unwrap_or_default();
                text.clear();
                if let Ok(mut held) = record.lock() {
                    held.push(command.clone());
                }
                if let Ok(mut held) = read_step.lock() {
                    held.last_command = command["command"].as_str().unwrap_or("?").to_owned();
                }
                queue_tx.send(command).unwrap_or(());
            }
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    std::thread::Builder::new()
        .name("resume-hub".to_owned())
        .spawn(move || {
            let mut hub_end = hub_end;
            let mut write = |value: Value| {
                if let Ok(mut held) = hub_step.lock() {
                    held.writing = true;
                }
                let mut bytes = serde_json::to_string(&value).unwrap_or_default();
                bytes.push('\n');
                hub_end.write_all(bytes.as_bytes()).unwrap_or(());
                if let Ok(mut held) = hub_step.lock() {
                    held.writing = false;
                }
            };
            let hub_ack = |id: &Value, result: Value| {
                json!({"kind": "command_accepted", "ts": 0,
                    "schema_version": contract::SCHEMA_VERSION,
                    "payload": {"command_id": id, "result": result}})
            };
            let session_ack = |id: &Value, result: Value| {
                let envelope = line("command_accepted", None, None, json!({}));
                let mut value = serde_json::to_value(&envelope).unwrap_or(Value::Null);
                value["payload"] = json!({"command_id": id, "result": result});
                value
            };
            let mut replaying = false;
            let mut replay_commands = 0usize;
            for command in queue_rx {
                let id = command["id"].clone();
                match command["command"].as_str() {
                    Some("feed") => write(hub_ack(&id, json!({}))),
                    Some("recent") => write(hub_ack(&id, json!({"sessions": []}))),
                    Some("subscribe") => {
                        if command["args"]["level"] == "full" {
                            // The raise, released by the first frame: the
                            // last line read ends the preview, so the
                            // replay folds from its first line. The hub
                            // stays up through the replay: every replayed
                            // turn start sends `commands` again.
                            let last = stream
                                .iter()
                                .filter_map(|envelope| envelope.seq.map(|seq| seq.0))
                                .max()
                                .unwrap_or(0);
                            write(session_ack(&id, json!({"last_seq": last})));
                            for envelope in &stream {
                                write(serde_json::to_value(envelope).unwrap_or(Value::Null));
                            }
                            replaying = true;
                        }
                        // The first stage: park until the test releases
                        // it, then answer only. The raise follows the
                        // first frame.
                        subscribed_tx.send(()).unwrap_or(());
                        release.recv().unwrap_or(());
                        write(session_ack(&id, json!({})));
                    }
                    Some("commands") => {
                        write(session_ack(&id, json!({"commands": []})));
                        // The replay's turn starts ask again: hang up once
                        // every one has its answer.
                        if replaying {
                            replay_commands += 1;
                            if replay_commands >= replay_turns {
                                break;
                            }
                        }
                    }
                    Some("history") => {
                        // Parked with the question read: the loop asked
                        // and waits in its answer, so no fold follows
                        // until the test releases the answer. The ping
                        // wakes the fold pump, which sees the park and
                        // stops; the answer goes out only after the
                        // test's release.
                        history_parked_tx.send(()).unwrap_or(());
                        fold_ping_tx.send(()).unwrap_or(());
                        history_release.recv().unwrap_or(());
                        let from = command["args"]["from_seq"].as_u64().unwrap_or(0);
                        let to = command["args"]["to_seq"].as_u64().unwrap_or(u64::MAX);
                        write(session_ack(
                            &id,
                            json!({"lines": tail_history(&stream, from, to)}),
                        ));
                    }
                    _ => write(hub_ack(&id, json!({}))),
                }
            }
            // The hang-up: shut the read half so the drain thread above
            // sees EOF and exits, then report the finished writes.
            hub_end.shutdown(std::net::Shutdown::Both).unwrap_or(());
            hub_done_tx.send(()).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    let hello = contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    };
    tx.send(Input::Connected(tui_end, hello))
        .unwrap_or_else(|err| panic!("send: {err}"));
    drop(tx);
    let (done, finished) = mpsc::channel();
    // The fold seam renewing the drain window chunk by chunk: one
    // `reached` per hub chunk, one `release` per signal, so the window
    // bounds one chunk, never the whole log. Dropping the sender lets
    // every later fold through, and a panicking test releases the loop
    // the same way.
    std::thread::Builder::new()
        .name("resume-run".to_owned())
        .spawn(move || {
            super::pause::install(fold_reached_tx, fold_release);
            let code = lp.run(&rx);
            done.send((code, lp)).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    // The hub parked at subscribe: one deadline for the whole wait.
    let parked = Deadline::after(MUST_SUCCEED_WITHIN);
    parked.recv_or_fail(&subscribed, "the hub to park at subscribe");
    // The drain cannot answer while the subscribe answer is held: a
    // bounded receive of its own that must time out. This is a receive
    // with a bound, not a sleep.
    assert!(
        Deadline::after(Duration::from_millis(200))
            .recv(&finished)
            .is_err(),
        "the loop drained while the subscribe answer was held"
    );
    release_tx
        .send(())
        .unwrap_or_else(|err| panic!("release: {err}"));
    drop(release_tx);
    // The hub parked at history with the loop waiting in its answer:
    // pump folds until the park, so the no-progress window never covers
    // the held answer. The park's ping wakes the pump, which sees the
    // park and stops. Panicking drops the history release sender, which
    // releases the held hub thread too.
    unpack_wait(
        pump_until_stop(
            MUST_SUCCEED_WITHIN,
            &fold_reached,
            &fold_release_tx,
            &history_parked,
            "no fold nor park",
        ),
        format!("the loop neither folded nor parked at history for {MUST_SUCCEED_WITHIN:?}"),
        &finished,
        &hub_progress,
        &hub_done,
        &reader_done,
    );
    history_release_tx
        .send(())
        .unwrap_or_else(|err| panic!("release: {err}"));
    drop(history_release_tx);
    // The drain pumps the fold chunk by chunk: each signal renews the
    // no-progress window, so a slow machine folds every chunk within
    // `MUST_SUCCEED_WITHIN` while the whole log takes what it takes. Panicking drops
    // the release sender, which releases the held loop thread too.
    // The run thread may have finished between the drain's check and
    // now: the drain error still panics, and the panic names the run
    // thread's state and each helper thread's last step, so the next
    // sighting shows which stall candidate happened.
    let (code, lp) = unpack_wait(
        drain_by_chunk(
            MUST_SUCCEED_WITHIN,
            &fold_reached,
            &fold_release_tx,
            &finished,
        ),
        format!("the loop folded no chunk for {MUST_SUCCEED_WITHIN:?}"),
        &finished,
        &hub_progress,
        &hub_done,
        &reader_done,
    );
    drop(fold_release_tx);
    // The hub's writes and the reader's EOF are ready once the loop has
    // returned: hang guards, not drain bounds.
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv_or_fail(&hub_done, "the hub to finish streaming the log");
    Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&reader_done, "the reader to reach EOF");
    assert_eq!(code, 0);
    // The one history command is the tail's first window: the replay
    // lands the whole session, so no page fetch follows.
    let histories: Vec<Value> = seen
        .lock()
        .map(|held| {
            held.iter()
                .filter(|command| command["command"] == "history")
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        histories.len(),
        6,
        // docs/tui.md:2446-2448 ("History and paging"): after the frame the terminal reads older windows to the chain root for the joint total.
        "{histories:?}"
    );
    assert_eq!(
        histories[0]["args"],
        json!({}),
        // docs/tui.md:2438 ("History and paging"): asks `history` with no `from_seq` for the session's last lines.
        "the newest window first"
    );
    assert_eq!(
        histories[1..]
            .iter()
            .map(|command| command["args"]["to_seq"].as_u64().unwrap_or(u64::MAX))
            .collect::<Vec<_>>(),
        [1091, 835, 579, 323, 67],
        // docs/tui.md:2446-2448 ("History and paging"): after the frame the terminal reads older windows down to the session's `seq 0` for the joint total.
        "older count windows reach seq zero before full"
    );
    // Every turn start the terminal applies asks `commands` again. The
    // first request loads the session's rows before the log replay.
    let seen_commands: Vec<Value> = seen.lock().map(|held| held.clone()).unwrap_or_default();
    let commands: Vec<_> = seen_commands
        .iter()
        .filter(|command| command["command"] == "commands")
        .collect();
    // docs/tui.md, "Slash commands": one `commands` request per `turn_started` (docs/testing.md, "Event streams").
    assert_eq!(commands.len(), replay_turns + 1, "{commands:?}");
    let screen = shown(&lp);
    assert!(screen.contains("quokkas"), "{screen}");
}

/// A left click at 0-based `col`, `row`: the press and the release.
fn click(col: u16, row: u16) -> Input {
    let (col, row) = (col + 1, row + 1);
    Input::Bytes(format!("\x1b[<0;{col};{row}M\x1b[<0;{col};{row}m").into_bytes())
}

/// Ctrl+C twice, which quits the loop with code 0.
fn quit() -> Input {
    Input::Bytes(vec![0x03, 0x03])
}

/// A wide loop with the delegates card, attached, holding one running
/// Fiber delegate: the first delegate spot's 0-based cell.
fn delegate_spot() -> (Loop<TestBackend>, (u16, u16)) {
    let (mut lp, _) = super::tests::new_loop(TestBackend::new(160, 40), None);
    lp.app.set_size(160, 40);
    lp.screen
        .resize(160, 40)
        .unwrap_or_else(|err| panic!("resize: {err}"));
    lp.app.set_home(crate::home::Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: ["session", "changed_files", "delegates", "jobs", "quota"]
            .map(str::to_owned)
            .to_vec(),
        ..Default::default()
    });
    lp.app.attach(contract::SessionId(SESSION.to_owned()));
    let hub =
        |kind: &str, payload: Value| Input::Hub(Line::Session(line(kind, None, None, payload)));
    let code = super::tests::feed(
        &mut lp,
        vec![
            hub(
                "job_started",
                json!({"job_id": "j_1", "description": "task one", "output_path": "/tmp/out"}),
            ),
            hub(
                "delegate_started",
                json!({"job_id": "j_1", "delegate_session_id": "s_bbbbbbbbbbbbbbbb",
                    "harness": "fiber", "model": "test/model", "workspace": "/w"}),
            ),
        ],
    );
    assert_eq!(code, 0);
    assert!(!lp.app.item_open());
    // Either delegate row opens the delegate; the description names it.
    let panel = lp
        .app
        .chrome()
        .layout()
        .and_then(|layout| layout.panel)
        .unwrap_or_else(|| panic!("a panel"));
    let row = shown(&lp)
        .lines()
        .position(|text| text.contains("task one"))
        .unwrap_or_else(|| panic!("the delegate row"));
    // The card text starts two columns past the slab.
    (lp, (panel.x + 3, u16::try_from(row).unwrap_or(u16::MAX)))
}

/// A key batch never holds: opening a delegate swaps the shown screen,
/// and a hold would stick to the stashed parent past the batch's end.
/// The click opens the delegate, and the quit ends the run: neither
/// the shown nor the stashed screen is held after.
#[test]
fn a_key_that_opens_a_delegate_leaves_nothing_held() {
    let (mut lp, (col, row)) = delegate_spot();
    let (tx, rx) = mpsc::channel();
    tx.send(click(col, row))
        .unwrap_or_else(|err| panic!("send: {err}"));
    tx.send(quit()).unwrap_or_else(|err| panic!("send: {err}"));
    drop(tx);
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("delegate-run".to_owned())
        .spawn(move || {
            let code = lp.run(&rx);
            done.send((code, lp)).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    let (code, lp) = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&finished)
        .unwrap_or_else(|err| panic!("waited {MUST_SUCCEED_WITHIN:?} for the loop to quit: {err}"));
    assert_eq!(code, 0);
    assert!(lp.app.item_open());
    assert!(!lp.app.pages().holding());
    assert!(!lp.app.stashed_holding());
}

/// Quitting ends the run through the batch's end, so a hold never
/// outlives the loop: a quit in its own batch leaves nothing held.
#[test]
fn a_quit_in_its_own_batch_leaves_nothing_held() {
    let (mut lp, _) = super::tests::new_loop(TestBackend::new(60, 12), None);
    let (tx, rx) = mpsc::channel();
    tx.send(quit()).unwrap_or_else(|err| panic!("send: {err}"));
    drop(tx);
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("quit-run".to_owned())
        .spawn(move || {
            let code = lp.run(&rx);
            done.send((code, lp)).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    let (code, lp) = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&finished)
        .unwrap_or_else(|err| panic!("waited {MUST_SUCCEED_WITHIN:?} for the loop to quit: {err}"));
    assert_eq!(code, 0);
    assert!(!lp.app.pages().holding());
}

/// A hub batch holds while it folds and settles once at its end: two
/// changed lines count the open page once, and the hold is released.
#[test]
fn a_hub_batch_holds_while_folding() {
    let (mut lp, _) = super::tests::new_loop(TestBackend::new(60, 12), None);
    lp.app.attach(contract::SessionId(SESSION.to_owned()));
    let mut seq = 0u64;
    let mut turn = |kind: &str, action: Option<&str>, payload: Value| {
        let envelope = line(kind, Some(seq), action, payload);
        seq += 1;
        Input::Hub(Line::Session(envelope))
    };
    let (tx, rx) = mpsc::channel();
    tx.send(turn(
        "turn_started",
        None,
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "prompt"}]}]}),
    ))
    .unwrap_or_else(|err| panic!("send: {err}"));
    tx.send(turn(
        "text_completed",
        Some("a_m"),
        json!({"text": "marker one"}),
    ))
    .unwrap_or_else(|err| panic!("send: {err}"));
    tx.send(quit()).unwrap_or_else(|err| panic!("send: {err}"));
    drop(tx);
    let before = lp.app.pages().recounts;
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("hub-run".to_owned())
        .spawn(move || {
            let code = lp.run(&rx);
            done.send((code, lp)).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    let (code, lp) = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&finished)
        .unwrap_or_else(|err| panic!("waited {MUST_SUCCEED_WITHIN:?} for the loop to quit: {err}"));
    assert_eq!(code, 0);
    // Both lines folded before the count: one recount for the batch.
    assert_eq!(lp.app.pages().recounts - before, 1);
    assert!(!lp.app.pages().holding());
    let screen = shown(&lp);
    assert!(screen.contains("marker one"), "{screen}");
}

/// Eighty page-shaped groups of identical turns, serialized for the
/// jig: enough dropped pages that a search spans many fetches, each
/// answered from the log. Nine single-sentence turns are the floor for
/// one page per group at 80x10: eight turns report 72 pages, and longer
/// replies add time without adding pages.
fn many_groups() -> String {
    let mut seq = 0u64;
    let mut events = String::new();
    let mut push = |kind: &str, action: Option<&str>, payload: Value| {
        seq += 1;
        let envelope = line(kind, Some(seq), action, payload);
        events.push_str(&serde_json::to_string(&envelope).unwrap_or_default());
        events.push('\n');
    };
    for group in 0..80 {
        for turn in 0..9 {
            let message = format!("a_g{group:02}w{turn}");
            push(
                "turn_started",
                None,
                json!({"input": [{"type": "message", "source": "driver", "content": [{"type": "text", "text": "go"}]}]}),
            );
            push("step_started", None, json!({}));
            push("assistant_message_started", Some(&message), json!({}));
            push(
                "text_completed",
                Some(&message),
                json!({"text": "the session holds every line in order ".to_owned()}),
            );
            push(
                "assistant_message_completed",
                Some(&message),
                json!({"outcome": "completed"}),
            );
            push(
                "usage_recorded",
                Some("a_m"),
                json!({"generation_id": format!("g{group:02}w{turn}"), "model": "fake/m", "tokens": {"input": 100, "cache_read": 0, "cache_write": {}, "output": 40}, "input_bytes": 0, "cost": null}),
            );
            push("turn_completed", None, json!({"outcome": "completed"}));
        }
        push(
            "turn_started",
            None,
            json!({"input": [{"type": "message", "source": "driver", "content": [{"type": "text", "text": "quiet"}]}]}),
        );
        push("step_started", None, json!({}));
        push(
            "assistant_message_started",
            Some(&format!("a_g{group:02}q")),
            json!({}),
        );
        push(
            "assistant_message_completed",
            Some(&format!("a_g{group:02}q")),
            json!({"outcome": "completed"}),
        );
        push(
            "usage_recorded",
            Some("a_m"),
            json!({"generation_id": format!("g{group:02}q"), "model": "fake/m", "tokens": {"input": 100, "cache_read": 0, "cache_write": {}, "output": 40}, "input_bytes": 0, "cost": null}),
        );
        push("turn_completed", None, json!({"outcome": "completed"}));
    }
    events
}

#[test]
fn a_long_search_answers_every_fetch_each_settle_queues() {
    // Eighty pages at 80x10: most drop, so both searches span dozens of
    // fetches. Every settle queues its own fetch past what its line
    // returns; the search drains those too, or an orphaned fetch blocks
    // the wire and the scan never completes.
    let report = crate::measure_paging(&many_groups(), 80, 10, fakes::clock::FakeClock::new())
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(report.contains("pages: 80\n"), "{report}");
    assert!(
        report.contains("search of the whole log with no match: 0.00 ms, 0 matches\n"),
        "{report}"
    );
}

#[test]
fn a_closed_answer_channel_ends_the_wait_hung_up() {
    let (mut lp, _theirs, tx, rx) = opened();
    drop(tx);
    // The channel is closed, so the wait ends at once instead of hanging
    // the suite: no thread and no deadline needed.
    let result = lp.hub.answer(
        &rx,
        "a_1",
        &mut lp.app,
        &lp.tick,
        lp.clock.as_ref(),
        lp.tick.armed(),
    );
    assert_eq!(result, Err(LOST.to_owned()));
    assert!(!lp.hub.has_stream(), "the closed channel hangs the wait up");
}

#[test]
fn an_answer_missing_its_final_seq_is_dropped() {
    let (ours, theirs) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    let mut hub = Connection::new(super::tests::idle_retry());
    hub.store(ours);
    let mut app = App::new(PathBuf::from("/w"));
    let tick = crate::tick::TickThread::idle();
    let session_id = contract::SessionId(SESSION.to_owned());
    let clock = fakes::clock::FakeClock::new();
    let (tx, rx) = mpsc::channel();
    // The hub answers every history command with one line carrying no
    // seq, and counts the commands, until the fetch hangs up.
    let (seen_tx, seen) = mpsc::channel();
    std::thread::Builder::new()
        .name("loop-seq-less-hub".to_owned())
        .spawn(move || {
            let mut reader = BufReader::new(theirs);
            let mut text = String::new();
            let mut count = 0u64;
            while reader.read_line(&mut text).is_ok_and(|read| read > 0) {
                count += 1;
                let command: Value = serde_json::from_str(&text).unwrap_or_default();
                text.clear();
                let id = command["id"].as_str().unwrap_or_default().to_owned();
                let mut first = serde_json::to_value(&session()[0])
                    .unwrap_or_else(|err| panic!("encode: {err}"));
                if let Some(line) = first.as_object_mut() {
                    line.remove("seq");
                }
                tx.send(answer(
                    "command_accepted",
                    json!({"command_id": id, "result": {"lines": [first]}}),
                ))
                .unwrap_or(());
            }
            seen_tx.send(count).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    // One line asked: the fetch returns at once, so it runs on a thread
    // of its own so a fetch that asks again fails here instead of
    // hanging the suite.
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("loop-seq-less-fetch".to_owned())
        .spawn(move || {
            let lines = hub.fetch(
                &rx,
                &session_id,
                &(Seq(0)..=Seq(0)),
                &mut app,
                &tick,
                clock.as_ref(),
                tick.armed(),
            );
            done.send((lines, hub)).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    let (lines, hub) = Deadline::after(DEADLINE)
        .recv(&finished)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the fetch: {err}"));
    // The unsequenced answer is dropped, not kept.
    assert_eq!(lines, Ok(Vec::new()));
    drop(hub);
    let count = Deadline::after(DEADLINE)
        .recv(&seen)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the hub to see the hang-up: {err}"));
    assert_eq!(
        count, 1,
        "the missing seq ends the fetch instead of asking again"
    );
}

#[test]
fn an_empty_fetch_range_asks_nothing() {
    let (ours, mut theirs) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    theirs
        .set_nonblocking(true)
        .unwrap_or_else(|err| panic!("nonblocking: {err}"));
    let mut hub = Connection::new(super::tests::idle_retry());
    hub.store(ours);
    let mut app = App::new(PathBuf::from("/w"));
    let tick = crate::tick::TickThread::idle();
    let session = contract::SessionId(SESSION.to_owned());
    let (_idle_tx, idle) = mpsc::channel();
    let clock = fakes::clock::FakeClock::new();
    // An inverted range asks nothing, so the fetch returns at once; it
    // runs on a thread of its own so an ask that never ends fails here
    // instead of hanging the suite.
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("loop-empty-fetch".to_owned())
        .spawn(move || {
            let lines = hub.fetch(
                &idle,
                &session,
                &(Seq(5)..=Seq(3)),
                &mut app,
                &tick,
                clock.as_ref(),
                tick.armed(),
            );
            done.send((lines, hub)).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    let (lines, hub) = Deadline::after(DEADLINE)
        .recv(&finished)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the empty fetch: {err}"));
    assert_eq!(lines, Ok(Vec::new()));
    // Nothing reached the hub: the peer reads nothing, without waiting.
    // The hub returns first, so its stream is still open for the read.
    let mut byte = [0u8; 1];
    match theirs.read(&mut byte) {
        Err(err) if err.kind() == ErrorKind::WouldBlock => {}
        Ok(_) => panic!("the empty fetch wrote a command"),
        Err(err) => panic!("the empty fetch's read failed: {err}"),
    }
    drop(hub);
}

#[test]
fn a_fetch_with_no_stream_is_lost_at_once() {
    let mut hub = Connection::new(super::tests::idle_retry());
    let mut app = App::new(PathBuf::from("/w"));
    app.on_line(Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    }));
    app.attach(contract::SessionId(SESSION.to_owned()));
    let tick = crate::tick::TickThread::idle();
    let session = contract::SessionId(SESSION.to_owned());
    let (_idle_tx, idle) = mpsc::channel();
    let clock = fakes::clock::FakeClock::new();
    // Nothing is connected, so the fetch fails without waiting; it runs
    // on a thread of its own so a wait that never ends fails here
    // instead of hanging the suite.
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("loop-lost-fetch".to_owned())
        .spawn(move || {
            let lines = hub.fetch(
                &idle,
                &session,
                &(Seq(0)..=Seq(1)),
                &mut app,
                &tick,
                clock.as_ref(),
                tick.armed(),
            );
            done.send((lines, app)).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    let (lines, app) = Deadline::after(DEADLINE)
        .recv(&finished)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the lost fetch: {err}"));
    assert_eq!(lines, Err(LOST.to_owned()));
    assert_eq!(app.notice(), Some("Connection lost."));
}

#[test]
fn an_empty_send_keeps_the_connection_and_writes_nothing() {
    let (ours, mut theirs) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    theirs
        .set_nonblocking(true)
        .unwrap_or_else(|err| panic!("nonblocking: {err}"));
    let mut hub = Connection::new(super::tests::idle_retry());
    hub.store(ours);
    let mut app = App::new(PathBuf::from("/w"));
    assert!(hub.send(&[], &mut app).is_none());
    assert!(hub.has_stream(), "an empty send keeps the stream");
    // Nothing reached the hub: the peer reads nothing, without waiting.
    let mut byte = [0u8; 1];
    match theirs.read(&mut byte) {
        Err(err) if err.kind() == ErrorKind::WouldBlock => {}
        Ok(_) => panic!("the empty send wrote a line"),
        Err(err) => panic!("the empty send's read failed: {err}"),
    }
}

#[test]
fn dropping_the_loop_ends_the_read() {
    let (lp, mut theirs, _tx, _rx) = opened();
    theirs
        .set_read_timeout(Some(DEADLINE))
        .unwrap_or_else(|err| panic!("timeout: {err}"));
    drop(lp);
    // The drop shuts the stream down both ways, so the peer sees the end
    // at once instead of waiting out the deadline.
    let mut byte = [0u8; 1];
    match theirs.read(&mut byte) {
        Ok(0) => {}
        Ok(_) => panic!("a byte instead of the end"),
        Err(err) => {
            panic!("waited {DEADLINE:?} for the dropped loop to end the read: {err}")
        }
    }
}

#[test]
fn hanging_up_twice_clears_the_stream() {
    let (ours, theirs) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    drop(theirs);
    let mut hub = Connection::new(super::tests::idle_retry());
    hub.store(ours);
    // The peer is gone: the shutdown still holds nothing back and clears
    // the stream, and hanging up with nothing held stays quiet.
    hub.hang_up();
    assert!(!hub.has_stream());
    hub.hang_up();
    assert!(!hub.has_stream());
}

#[test]
fn a_signal_during_a_history_wait_keeps_the_connection() {
    use signal_hook::consts::SIGTERM;
    let (lp, _theirs, tx, rx) = opened();
    tx.send(Input::Signal(SIGTERM))
        .unwrap_or_else(|err| panic!("send: {err}"));
    // `answer` blocks in `recv`: it runs on a thread of its own and its
    // result arrives through one deadline, so a wait that never ends
    // fails here instead of hanging the suite.
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("loop-signal-kept".to_owned())
        .spawn(move || {
            let mut lp = lp;
            let result = lp.hub.answer(
                &rx,
                "a_1",
                &mut lp.app,
                &lp.tick,
                lp.clock.as_ref(),
                lp.tick.armed(),
            );
            done.send((result, lp)).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    let (result, lp) = Deadline::after(DEADLINE)
        .recv(&finished)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the history wait to end: {err}"));
    assert_eq!(result, Err(LOST.to_owned()));
    assert!(
        lp.hub.has_stream(),
        "the held signal ends the wait, not the connection"
    );
    assert_eq!(
        lp.hub.stashed().len(),
        1,
        "the signal is held for after the frame"
    );
}

#[test]
fn a_failed_write_grants_no_retry_until_the_reader_disconnects() {
    let (mut lp, _theirs, tx, rx) = opened();
    let retry = crate::retry::Retry::new(&lp.clock);
    lp.hub.set_retry(Arc::clone(&retry));
    // Only the write side fails: the reader stays open, so no permit
    // arrives on its own.
    if let Some(hub) = lp.hub.stream() {
        hub.shutdown(std::net::Shutdown::Write)
            .unwrap_or_else(|err| panic!("shutdown: {err}"));
    } else {
        panic!("the loop holds no stream");
    }
    let mut lp = run(lp, rx, &tx, to_the_top());
    assert!(!lp.hub.has_stream());
    assert_eq!(
        lp.app.notice(),
        Some("Could not load history: connection lost")
    );
    assert_eq!(
        retry.held(),
        None,
        "the write loss grants no retry on its own"
    );
    // The reader's end of the stream gives the permit.
    let (_idle_tx, idle) = mpsc::channel();
    assert_eq!(lp.step(Input::Disconnected, &idle), None);
    assert_eq!(retry.held(), Some(Duration::from_millis(500)));
}

#[test]
fn an_empty_batch_with_no_senders_ends_the_loop() {
    let mut conn = Connection::new(super::tests::idle_retry());
    let (tx, rx) = mpsc::channel::<Input>();
    drop(tx);
    assert!(conn.take_batch(&rx).is_none());
}

#[test]
fn hooks_results_waiting_on_history_are_stashed_then_folded_stale() {
    let (mut lp, theirs, tx, rx) = opened();
    let mut serve_all = answers_all();
    let mut first = true;
    let script: Script = Box::new(move |command| {
        let mut inputs = Vec::new();
        if first {
            first = false;
            // A names answer and an install answer for offers this
            // terminal never opened: no live ticket folds them.
            inputs.push(Input::HooksOfferNames {
                ticket: 7,
                result: Ok(vec!["fmt".to_owned()]),
            });
            inputs.push(Input::HooksInstalled {
                ticket: 9,
                result: Ok(()),
            });
        }
        inputs.extend(serve_all(command));
        inputs
    });
    let _seen = serve(theirs, tx, script);
    lp.app.jump(0);
    // One frame, on a thread: it blocks on its fetches.
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("loop-step".to_owned())
        .spawn(move || {
            let code = lp.step(Input::Bytes(Vec::new()), &rx);
            done.send((code, lp, rx)).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    let (code, mut lp, rx) = match Deadline::after(DEADLINE).recv(&finished) {
        Ok(ended) => ended,
        Err(err) => panic!("waited {DEADLINE:?} for the frame: {err}"),
    };
    assert_eq!(code, None);
    // Held in arrival order, not yet handled.
    let held: Vec<String> = lp
        .hub
        .stashed()
        .iter()
        .map(|input| match input {
            Input::HooksOfferNames { .. } => "names".to_owned(),
            Input::HooksInstalled { .. } => "installed".to_owned(),
            Input::Converted { .. } => "other".to_owned(),
            Input::Bytes(_)
            | Input::Hub(_)
            | Input::Connected(..)
            | Input::ConnectFailed(_)
            | Input::Disconnected
            | Input::WriteFailed(_)
            | Input::Resize
            | Input::Files { .. }
            | Input::FindDue(_)
            | Input::Image { .. }
            | Input::Viewed { .. }
            | Input::Models(_)
            | Input::Feedback(_)
            | Input::Login { .. }
            | Input::Install { .. }
            | Input::Tick
            | Input::Signal(_)
            | Input::KittyFailed => "other".to_owned(),
        })
        .collect();
    assert_eq!(held, ["names", "installed"]);
    while let Some(input) = lp.hub.pop_stashed() {
        assert_eq!(lp.step(input, &rx), None);
    }
    // Stale tickets fold to nothing: no draft, no notice, no session.
    assert_eq!(lp.app.draft(), "");
    assert_eq!(lp.app.notice(), None);
}

#[path = "event_loop/attach_tests.rs"]
mod attach_tests;

/// Appends hub lines with distinct markers until `inputs` holds
/// `count`: whole turns while three fit, a `turn_started` plus its
/// `text_completed` for a tail of two, else one `step_started`, which
/// folds without drawing a row — so the 4096th input shares its frame
/// while the 4097th keeps its own. Returns the last
/// `text_completed` marker pushed, which the final screen shows when
/// every line folded.
fn push_markers(inputs: &mut Vec<Input>, count: usize, seq: &mut u64, turn: &mut usize) -> String {
    let mut last = String::new();
    while inputs.len() < count {
        if count - inputs.len() >= 3 {
            let marker = format!("marker {}", *turn);
            let lines = turn_lines(&marker, *turn, seq);
            *turn += 1;
            last = marker;
            inputs.extend(lines);
        } else if count - inputs.len() == 2 {
            let marker = format!("marker {}", *turn);
            let mut lines = turn_lines(&marker, *turn, seq);
            lines.truncate(2);
            last = marker;
            inputs.extend(lines);
        } else {
            let step = line("step_started", Some(*seq), None, json!({}));
            *seq += 1;
            inputs.push(Input::Hub(Line::Session(step)));
        }
    }
    last
}

#[test]
fn full_batches_draw_one_frame_up_to_4096() {
    // 4095, 4096 and 4097 queued hub inputs draw 1, 1 and 2 frames:
    // at most one frame per batch, the same cadence as before. Each
    // screen shows its own last marker, so every line folded.
    let mut flushes = Vec::new();
    for count in [4095usize, 4096, 4097] {
        let (mut lp, flush, tx, rx) = counting();
        let mut seq = 0u64;
        let mut turn = 0;
        let mut inputs = Vec::new();
        let last = push_markers(&mut inputs, count, &mut seq, &mut turn);
        assert_eq!(run_queued(&mut lp, rx, tx, inputs), 0);
        flushes.push(flush.get());
        let screen = counted(&lp);
        assert!(screen.contains(&last), "{screen}");
    }
    assert_eq!(flushes, [1, 1, 2]);
}

#[test]
fn a_key_after_a_full_batch_draws_its_own_frame() {
    let (mut lp, flushes, tx, rx) = counting();
    let mut seq = 0u64;
    let mut turn = 0;
    let mut inputs = Vec::new();
    push_markers(&mut inputs, 4096, &mut seq, &mut turn);
    inputs.push(Input::Bytes(b"a".to_vec()));
    let tail = inputs.len() + 10;
    push_markers(&mut inputs, tail, &mut seq, &mut turn);
    assert_eq!(run_queued(&mut lp, rx, tx, inputs), 0);
    // The full batch, the key's own, the tail.
    assert_eq!(flushes.get(), 3);
    assert_eq!(lp.app.draft(), "a");
}

#[test]
fn a_disconnect_among_full_batches_ends_the_batch_with_its_own_frame() {
    let (mut lp, flushes, tx, rx) = counting();
    let mut seq = 0u64;
    let mut turn = 0;
    let mut inputs = Vec::new();
    push_markers(&mut inputs, 100, &mut seq, &mut turn);
    inputs.push(Input::Disconnected);
    let tail = inputs.len() + 100;
    push_markers(&mut inputs, tail, &mut seq, &mut turn);
    assert_eq!(run_queued(&mut lp, rx, tx, inputs), 0);
    assert_eq!(flushes.get(), 3);
}

/// A hub line of `kind` for `session`.
fn hub_for(kind: &str, session: &str) -> Line {
    Line::Hub(contract::HubLine {
        kind: kind.to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::json!({"session_id": session})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    })
}

/// The notice rows' codes, oldest first.
fn notice_codes(app: &App) -> Vec<String> {
    app.notices()
        .into_iter()
        .filter_map(|line| match line {
            crate::app::NoticeLine::Notice { code, .. } => Some(code),
            crate::app::NoticeLine::More(_) => None,
        })
        .collect()
}

#[test]
fn take_wake_folds_the_notices_wake() {
    let clock = fakes::clock::FakeClock::new();
    let t0 = clock.now();
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(60, 12);
    app.attach(contract::SessionId(SESSION.to_owned()));
    // Quiet: no motion, no items, no notices.
    assert_eq!(app.take_wake(), None);
    // One event: the wake is its expiry.
    app.push_notice("io_failed", "x".to_owned());
    app.sweep_notices(t0);
    assert_eq!(
        app.take_wake(),
        Some(t0 + std::time::Duration::from_secs(10))
    );
    // Two staggered events: the earlier expiry wins.
    app.push_notice("config_invalid", "y".to_owned());
    app.sweep_notices(t0 + std::time::Duration::from_secs(5));
    assert_eq!(
        app.take_wake(),
        Some(t0 + std::time::Duration::from_secs(10))
    );
    // Past the first notice's fade only it expires: the younger notice
    // stands, and the wake moves to its expiry.
    app.sweep_notices(t0 + std::time::Duration::from_secs(10));
    assert_eq!(notice_codes(&app), ["config_invalid"]);
    assert_eq!(
        app.take_wake(),
        Some(t0 + std::time::Duration::from_secs(15))
    );
    // Past every fade the wake is gone with the rows.
    app.sweep_notices(t0 + std::time::Duration::from_secs(15));
    assert!(app.notices().is_empty());
    assert_eq!(app.take_wake(), None);
}

#[test]
fn reopen_clears_only_the_closed_sessions_stream_closed() {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(60, 12);
    app.attach(contract::SessionId(SESSION.to_owned()));
    // A close for the open session shows; one for another session is not
    // this terminal's cause.
    app.on_line(hub_for("stream_closed", SESSION));
    assert_eq!(notice_codes(&app), ["stream_closed"]);
    app.on_line(hub_for("stream_closed", "s_bbbbbbbbbbbbbbbb"));
    assert_eq!(notice_codes(&app), ["stream_closed"]);
    // Attaching elsewhere drops only the other session's stale row.
    app.attach(contract::SessionId("s_bbbbbbbbbbbbbbbb".to_owned()));
    assert!(app.notices().is_empty());
    // A close for the now-open session shows until its reopen ends it.
    app.on_line(hub_for("stream_closed", "s_bbbbbbbbbbbbbbbb"));
    assert_eq!(notice_codes(&app), ["stream_closed"]);
    app.on_line(hub_for("stream_reopened", "s_bbbbbbbbbbbbbbbb"));
    assert!(app.notices().is_empty());
    // A stale reopen for a session with no row changes nothing.
    app.on_line(hub_for("stream_reopened", SESSION));
    assert!(app.notices().is_empty());
}

#[test]
fn login_url_survives_sweeps_and_falls_to_dismiss() {
    let clock = fakes::clock::FakeClock::new();
    let t0 = clock.now();
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(60, 12);
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.push_notice("login_url", "open the link to log in".to_owned());
    app.push_notice("io_failed", "x".to_owned());
    app.sweep_notices(t0);
    // Past the fade the event is gone and the condition stands: no
    // terminal-visible signal ends it, only the person's ✕ does.
    app.sweep_notices(t0 + std::time::Duration::from_secs(30));
    assert_eq!(notice_codes(&app), ["login_url"]);
    let id = app
        .notices()
        .into_iter()
        .find_map(|line| match line {
            crate::app::NoticeLine::Notice { id, .. } => Some(id),
            crate::app::NoticeLine::More(_) => None,
        })
        .expect("the login row");
    app.on_click(crate::mouse::TargetId::DismissNotice(id));
    assert!(app.notices().is_empty());
}
