//! Run-level tests for opening an image: the click reads the file
//! over the hub, and the worker opens the copy. A stored JPEG converts
//! through the image child before it reaches Kitty.

use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};

use base64::Engine as _;

use ratatui::backend::TestBackend;
use serde_json::{Value, json};

use super::Loop;
use crate::Input;
use crate::app::{App, Effect, Target};
use crate::link::Line;
use crate::mouse::TargetId;
use crate::screen::Screen;
use fakes::{Deadline, MUST_SUCCEED_WITHIN};

fn bounded_sixel_test(name: &str, body: fn()) {
    const CHILD: &str = "FIBER_SIXEL_TEST_CHILD";
    if std::env::var_os(CHILD).is_some() {
        body();
        return;
    }
    use std::os::unix::process::CommandExt as _;
    let child = std::process::Command::new(std::env::current_exe().expect("the test binary"))
        .arg("--exact")
        .arg(name)
        .arg("--nocapture")
        .env(CHILD, "1")
        .process_group(0)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the Sixel test process spawns");
    let output = fakes::wait_output(child, name);
    assert!(
        output.status.success(),
        "{name} passed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

use crate::convert::tests::PNG as CONVERTED_PNG;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

/// A loop on a `TestBackend` with a paired hub, the viewer's script
/// writing the copy's path to the marker file, and the worker answers
/// on `rx`. Returns the loop, the hub's end, the answers, the images
/// directory, the marker file, and the directories' owner, oldest
/// first.
fn loop_with_viewer(
    script: &str,
) -> (
    Loop<TestBackend>,
    UnixStream,
    Receiver<Input>,
    PathBuf,
    PathBuf,
    fakes::TempDir,
) {
    let dir = fakes::TempDir::new("fiber-loop-images");
    let images = dir.path().join("images");
    let marker = dir.path().join("opened");
    let program = fakes::script(dir.path(), "viewer", script);
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(60, 12);
    app.on_line(Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    }));
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.on_line(Line::Session(contract::Envelope {
        kind: "turn_started".to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: json!({"input": [{
            "type": "message", "source": "driver",
            "content": [
                {"type": "text", "text": "look"},
                {"type": "image", "path": "artifacts/shot.png",
                    "mime_type": "image/png",
                    "width": 1280, "height": 800},
            ],
        }]})
        .as_object()
        .cloned()
        .unwrap_or_default(),
    }));
    let (ours, theirs) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    let screen =
        Screen::new(TestBackend::new(60, 12), 60, 12).unwrap_or_else(|err| panic!("screen: {err}"));
    let (out, rx) = mpsc::channel();
    let viewer = vec![
        "/bin/sh".to_owned(),
        program.to_str().unwrap_or_default().to_owned(),
        marker.to_str().unwrap_or_default().to_owned(),
    ];
    let mut lp = Loop::for_test(app, screen);
    lp.files_out = out;
    lp.programs.viewer = viewer;
    lp.programs.images_dir = images.clone();
    lp.hub.store(ours);
    // The writer clones this tty when the first image hands over: a
    // writable temp file, so its chunks land instead of failing on the
    // read-only `/dev/null` the test loop starts with.
    lp.tty =
        std::fs::File::create(dir.path().join("tty")).unwrap_or_else(|err| panic!("tty: {err}"));
    (lp, theirs, rx, images, marker, dir)
}

/// Clicks the image's line, sending its `read_file`.
fn click(lp: &mut Loop<TestBackend>) -> Vec<String> {
    match lp.app.on_click(TargetId::Line(Target::Image(1))) {
        Effect::Send(lines) => lines,
        Effect::None
        | Effect::Quit
        | Effect::Exit(_)
        | Effect::ListFiles
        | Effect::ReadImage(_)
        | Effect::Feedback { .. }
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::OpenFile(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_) => panic!("expected a send"),
    }
}

/// Reads the `read_file` line the click sent, returning the command's
/// id; the hub answers it with the file in base64.
fn answer(theirs: &UnixStream) -> String {
    theirs
        .set_read_timeout(Some(MUST_SUCCEED_WITHIN))
        .unwrap_or_else(|err| panic!("timeout: {err}"));
    let reader = theirs
        .try_clone()
        .unwrap_or_else(|err| panic!("clone: {err}"));
    let mut text = String::new();
    BufReader::new(reader)
        .read_line(&mut text)
        .unwrap_or_else(|err| panic!("read: {err}"));
    let asked: Value = serde_json::from_str(&text).unwrap_or_else(|err| panic!("a line: {err}"));
    assert_eq!(asked.get("command"), Some(&json!("read_file")));
    assert_eq!(
        asked.get("args"),
        Some(&json!({"session": SESSION, "path": "artifacts/shot.png"}))
    );
    let id = asked
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    assert!(!id.is_empty());
    id
}

/// The hub's answer carrying `id`'s file in base64.
fn accepted(id: &str, data: &str) -> Input {
    Input::Hub(Line::Hub(contract::HubLine {
        kind: "command_accepted".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: json!({"command_id": id,
            "result": {"data": data, "mime_type": "image/png"}})
        .as_object()
        .cloned()
        .unwrap_or_default(),
    }))
}

/// The worker's completion within the deadline.
#[track_caller]
fn completion(rx: &Receiver<Input>, wait: &Deadline) -> Input {
    match wait.recv(rx) {
        Ok(done @ Input::Viewed { .. }) => done,
        Ok(
            Input::Bytes(_)
            | Input::Hub(_)
            | Input::Connected(..)
            | Input::ConnectFailed(_)
            | Input::Disconnected
            | Input::Resize
            | Input::Signal(_)
            | Input::Files { .. }
            | Input::FindDue(_)
            | Input::Image { .. }
            | Input::Feedback(_)
            | Input::Converted { .. }
            | Input::Models(_)
            | Input::Install { .. }
            | Input::HooksOfferNames { .. }
            | Input::HooksInstalled { .. }
            | Input::Login { .. }
            | Input::KittyFailed
            | Input::Tick,
        ) => panic!("the worker answered something else"),
        Err(err) => panic!("waited {MUST_SUCCEED_WITHIN:?} for the worker: {err}"),
    }
}

#[test]
fn a_click_on_an_image_line_reads_the_file_and_opens_it() {
    // The viewer prints the copy's path to the marker file.
    let (mut lp, theirs, rx, images, marker, _dir) = loop_with_viewer("echo \"$2\" > \"$1\"\n");
    let (_idle_tx, idle) = mpsc::channel();
    let lines = click(&mut lp);
    lp.hub.send(&lines, &mut lp.app);
    let id = answer(&theirs);
    assert_eq!(lp.step(accepted(&id, "Ynl0ZXM="), &idle), None);
    match completion(&rx, &Deadline::after(MUST_SUCCEED_WITHIN)) {
        Input::Viewed { name, result, .. } => {
            assert_eq!(name, "shot.png");
            assert_eq!(result, Ok(()));
        }
        Input::Bytes(_)
        | Input::Hub(_)
        | Input::Connected(..)
        | Input::ConnectFailed(_)
        | Input::Disconnected
        | Input::Resize
        | Input::Signal(_)
        | Input::Files { .. }
        | Input::FindDue(_)
        | Input::Image { .. }
        | Input::Feedback(_)
        | Input::Converted { .. }
        | Input::Models(_)
        | Input::Install { .. }
        | Input::HooksOfferNames { .. }
        | Input::HooksInstalled { .. }
        | Input::Login { .. }
        | Input::KittyFailed
        | Input::Tick => panic!("the worker answered something else"),
    }
    // The copy holds the file's bytes, and the viewer ran over it.
    let path = images.join("s_aaaaaaaaaaaaaaaa-shot.png");
    assert_eq!(
        std::fs::read(&path).unwrap_or_else(|err| panic!("read: {err}")),
        b"bytes"
    );
    assert_eq!(
        std::fs::read_to_string(&marker)
            .unwrap_or_else(|err| panic!("read: {err}"))
            .trim(),
        path.to_str().unwrap_or_default()
    );
    assert_eq!(lp.app.notice(), None);
}

#[test]
fn a_failed_viewer_draws_the_notice() {
    let (mut lp, theirs, rx, _images, _marker, _dir) = loop_with_viewer("exit 3\n");
    let (_idle_tx, idle) = mpsc::channel();
    let lines = click(&mut lp);
    lp.hub.send(&lines, &mut lp.app);
    let id = answer(&theirs);
    assert_eq!(lp.step(accepted(&id, "Ynl0ZXM="), &idle), None);
    // The worker's completion arrives as its own input: stepping it
    // draws the notice.
    let done = completion(&rx, &Deadline::after(MUST_SUCCEED_WITHIN));
    assert!(matches!(done, Input::Viewed { result: Err(_), .. }));
    assert_eq!(lp.step(done, &idle), None);
    assert!(
        lp.app
            .notice()
            .is_some_and(|notice| notice.starts_with("Could not open shot.png: "))
    );
}

/// A turn whose prompt pasted a JPEG image: drawn inline while the
/// converter is available.
fn turn_started_jpeg() -> contract::Envelope {
    contract::Envelope {
        kind: "turn_started".to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: json!({"input": [{
            "type": "message", "source": "driver",
            "content": [
                {"type": "text", "text": "look"},
                {"type": "image", "path": "artifacts/shot.jpg",
                    "mime_type": "image/jpeg",
                    "width": 1280, "height": 800},
            ],
        }]})
        .as_object()
        .cloned()
        .unwrap_or_default(),
    }
}

/// A loop with inline images, the converter's executable path and a
/// piped tty: the scripted converter prints [`CONVERTED_PNG`] for every
/// start, and the test reads what reaches the terminal.
fn converting_loop() -> (
    Loop<TestBackend>,
    UnixStream,
    Receiver<Input>,
    std::io::PipeReader,
    fakes::TempDir,
) {
    let dir = fakes::TempDir::new("fiber-loop-convert");
    let png = dir.path().join("out.png");
    std::fs::write(&png, CONVERTED_PNG).unwrap_or_else(|err| panic!("png: {err}"));
    let script = fakes::script(dir.path(), "convert", "cat \"$1\"\n");
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(60, 12);
    app.on_line(Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    }));
    app.set_home(crate::home::Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        inline_images: true,
        fiber: Some(PathBuf::from("/fiber")),
        ..Default::default()
    });
    app.set_graphics();
    app.set_cell((10, 20));
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.on_line(Line::Session(turn_started_jpeg()));
    let (ours, theirs) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    let (tty_out, tty_in) = std::io::pipe().unwrap_or_else(|err| panic!("pipe: {err}"));
    let (mut lp, _) = super::tests::new_loop(
        TestBackend::new(60, 12),
        Some(std::fs::File::from(std::os::fd::OwnedFd::from(tty_in))),
    );
    lp.app = app;
    lp.hub.store(ours);
    let (out, rx) = mpsc::channel();
    lp.files_out = out;
    lp.programs.converter = Some(vec![
        OsString::from("/bin/sh"),
        script.as_os_str().to_owned(),
        png.as_os_str().to_owned(),
    ]);
    (lp, theirs, rx, tty_out, dir)
}

/// Steps the loop once, on a thread: a frame that waits on the hub
/// would hang the test, so the join carries the deadline.
#[track_caller]
fn stepped(
    mut lp: Loop<TestBackend>,
    rx: Receiver<Input>,
    input: Input,
) -> (Loop<TestBackend>, Receiver<Input>) {
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("loop-step".to_owned())
        .spawn(move || {
            let code = lp.step(input, &rx);
            done.send((code, lp, rx)).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    let (code, lp, rx) = match Deadline::after(MUST_SUCCEED_WITHIN).recv(&finished) {
        Ok(ended) => ended,
        Err(err) => panic!("waited {MUST_SUCCEED_WITHIN:?} for the frame: {err}"),
    };
    assert_eq!(code, None);
    (lp, rx)
}

/// Reads hub commands until the display `read_file`, returning its id.
#[track_caller]
fn read_display_id(theirs: &UnixStream) -> String {
    theirs
        .set_read_timeout(Some(MUST_SUCCEED_WITHIN))
        .unwrap_or_else(|err| panic!("timeout: {err}"));
    let reader = theirs
        .try_clone()
        .unwrap_or_else(|err| panic!("clone: {err}"));
    let mut reader = BufReader::new(reader);
    for _ in 0..10 {
        let mut text = String::new();
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

/// The hub's accepted answer to the display `read_file` carrying `id`:
/// JPEG bytes, as production relays them.
fn jpeg_answer(id: &str) -> Input {
    use base64::Engine as _;
    let data = base64::engine::general_purpose::STANDARD.encode(b"jpeg-bytes");
    Input::Hub(Line::Hub(contract::HubLine {
        kind: "command_accepted".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: json!({"command_id": id,
            "result": {"data": data, "mime_type": "image/jpeg"}})
        .as_object()
        .cloned()
        .unwrap_or_default(),
    }))
}

/// The worker's conversion completion within the deadline.
#[track_caller]
fn converted(rx: &Receiver<Input>, wait: &Deadline) -> Input {
    match wait.recv(rx) {
        Ok(done @ Input::Converted { .. }) => done,
        Ok(_) => panic!("the worker answered something else"),
        Err(err) => panic!("waited {MUST_SUCCEED_WITHIN:?} for the worker: {err}"),
    }
}

/// The screen the loop drew last.
fn shown(lp: &Loop<TestBackend>) -> String {
    crate::view::text(lp.screen.backend().buffer())
}

/// The writer's bytes until `terms` escape terminators arrived, within
/// the deadline: the transmit's chunks plus the place. The writer sends
/// after the step returns, so the test waits for the bytes rather than
/// draining what arrived so far.
#[track_caller]
fn read_wire(read: std::io::PipeReader, terms: usize) -> Vec<u8> {
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
                        if bytes
                            .windows(2)
                            .filter(|window| *window == b"\x1b\\")
                            .count()
                            >= terms
                        {
                            break;
                        }
                    }
                    Err(err) => panic!("tty: {err}"),
                }
            }
            done.send(bytes).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    match Deadline::after(MUST_SUCCEED_WITHIN).recv(&finished) {
        Ok(bytes) => bytes,
        Err(err) => panic!("waited {MUST_SUCCEED_WITHIN:?} for the wire: {err}"),
    }
}

/// Reads the loop's tty on a thread until `marker` arrives, returning
/// the wait for the bytes and the reader: the loop writes a large logo
/// emission while its step runs, so no step with one can drain after it
/// returns. The wait is bounded by the named deadline, never the clock
/// (`docs/testing.md`, "Waits and timeouts").
fn spawn_read(
    read: std::io::PipeReader,
    marker: Vec<u8>,
) -> Receiver<(Vec<u8>, std::io::PipeReader)> {
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("loop-tty-read".to_owned())
        .spawn(move || {
            let mut read = read;
            rustix::fs::fcntl_setfl(&mut read, rustix::fs::OFlags::empty())
                .unwrap_or_else(|err| panic!("blocking: {err}"));
            let mut bytes = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                match std::io::Read::read(&mut read, &mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        bytes.extend_from_slice(&chunk[..n]);
                        if bytes
                            .windows(marker.len())
                            .any(|window| window == marker.as_slice())
                        {
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

/// Steps the loop while a reader drains its tty until `marker`
/// arrives, returning what the step wrote and the reader: a large logo
/// emission fills the pipe, so the step runs beside the reader.
#[track_caller]
fn step_emission(
    lp: &mut Loop<TestBackend>,
    rx: &Receiver<Input>,
    input: Input,
    tty: std::io::PipeReader,
    marker: Vec<u8>,
) -> (Vec<u8>, std::io::PipeReader) {
    // The last step's trailing title bytes stay in the pipe past the
    // marker: clear them, so what returns is this step's write.
    let mut tty = tty;
    drain_tty(&mut tty);
    let wait = spawn_read(tty, marker.clone());
    assert_eq!(lp.step(input, rx), None);
    match Deadline::after(MUST_SUCCEED_WITHIN).recv(&wait) {
        Ok((mut got, tty)) => {
            // The reader may hold the next write's first bytes past the
            // marker: only this step's emission returns.
            if let Some(at) = got
                .windows(marker.len())
                .position(|window| window == marker)
            {
                got.truncate(at + marker.len());
            }
            (got, tty)
        }
        Err(err) => panic!("waited {MUST_SUCCEED_WITHIN:?} for the emission: {err}"),
    }
}

/// The positioned write's tail: the payload's BEL, then the cursor
/// restore. No frame writes one before the emission.
const POSITIONED_TAIL: &[u8] = b"\x07\x1b8";

/// A Sixel write's terminator: the emission's only ST.
const SIXEL_TAIL: &[u8] = b"\x1b\\";

/// All bytes the loop has written to its tty so far: the pipe reads
/// nonblocking, so the drain ends where the writes do.
#[track_caller]
fn drain_tty(reader: &mut std::io::PipeReader) -> Vec<u8> {
    rustix::fs::fcntl_setfl(&mut *reader, rustix::fs::OFlags::NONBLOCK)
        .unwrap_or_else(|err| panic!("nonblocking: {err}"));
    let mut out = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match std::io::Read::read(reader, &mut chunk) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(chunk.split_at(n).0),
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(err) => panic!("tty: {err}"),
        }
    }
    out
}

#[test]
fn a_jpeg_converts_then_transmits() {
    let (lp, theirs, rx, mut tty, _dir) = converting_loop();
    // The first frame draws at once: the caption counts its row from
    // the logged size, with no conversion and no fetch wait.
    let (lp, rx) = stepped(lp, rx, Input::Tick);
    assert!(
        shown(&lp).contains("▣ shot"),
        "the first frame draws the caption: {}",
        shown(&lp)
    );
    super::tests::flush_kitty(&lp);
    let before = drain_tty(&mut tty);
    assert!(
        !String::from_utf8_lossy(&before).contains("Ga=t"),
        "nothing transmits before the conversion"
    );
    let id = read_display_id(&theirs);
    // The JPEG answer queues a conversion, not a transmit.
    let (lp, rx) = stepped(lp, rx, jpeg_answer(&id));
    let done = converted(&rx, &Deadline::after(MUST_SUCCEED_WITHIN));
    // The completion waits in the loop's inputs: still nothing on the
    // terminal, even though the child already finished.
    super::tests::flush_kitty(&lp);
    let waiting = drain_tty(&mut tty);
    assert!(
        !String::from_utf8_lossy(&waiting).contains("Ga=t"),
        "nothing transmits while the completion waits"
    );
    // Stepping the completion hands the transmit to the writer: its bytes
    // reach the tty after the step, so the test reads them with a
    // deadline.
    let (lp, _rx) = stepped(lp, rx, done);
    super::tests::flush_kitty(&lp);
    let png = base64::engine::general_purpose::STANDARD.encode(CONVERTED_PNG);
    let transmit = crate::graphics::transmit(1, &png);
    let sent = read_wire(tty, transmit.len() + 1);
    let sent = String::from_utf8_lossy(&sent);
    for chunk in &transmit {
        let text = String::from_utf8_lossy(chunk);
        assert!(
            sent.contains(text.as_ref()),
            "the converted PNG transmits whole: {sent:?}"
        );
    }
    assert!(
        sent.contains("Ga=p,U=1,i=1,"),
        "the place binds the placeholders: {sent:?}"
    );
    assert!(shown(&lp).contains('\u{10EEEE}'));
    assert!(shown(&lp).contains("▣ shot"));
    assert_eq!(lp.app.notice(), None);
}

#[test]
fn a_queued_conversion_without_an_executable_fails_without_a_child() {
    let (mut lp, theirs, rx, mut tty, _dir) = converting_loop();
    lp.programs.converter = None;
    let (lp, rx) = stepped(lp, rx, Input::Tick);
    let id = read_display_id(&theirs);
    // Without an executable path the queued conversion fails at once:
    // no child runs, and the image keeps its one line with a notice.
    let (lp, _rx) = stepped(lp, rx, jpeg_answer(&id));
    assert_eq!(
        lp.app.notice(),
        Some("Could not show shot.jpg: image conversion failed.")
    );
    super::tests::flush_kitty(&lp);
    let sent = drain_tty(&mut tty);
    assert!(
        !String::from_utf8_lossy(&sent).contains("Ga=t"),
        "no child ran, so nothing transmits"
    );
    assert!(lp.app.pages().image_failed(
        &contract::SessionId(SESSION.to_owned()),
        "artifacts/shot.jpg"
    ));
}

/// Detection replies land without waiting: an iTerm2-family XTVERSION
/// names its protocol, any other text changes nothing, and the Sixel
/// bit needs a measured cell inside the bound.
#[test]
fn detection_replies_land_without_a_frame_wait() {
    let (mut lp, _) = super::tests::new_loop(TestBackend::new(60, 12), None);
    let (_, rx) = mpsc::channel();
    assert!(!lp.app.iterm());
    assert!(!lp.app.sixel());
    // Other version text names no protocol.
    assert_eq!(
        lp.step(Input::Bytes(b"\x1bP>|ghostty 1.0\x1b\\".to_vec()), &rx),
        None
    );
    assert!(!lp.app.iterm());
    assert_eq!(
        lp.step(Input::Bytes(b"\x1bP>|iTerm2 3.5.0\x1b\\".to_vec()), &rx),
        None
    );
    assert!(lp.app.iterm());
    // The Sixel bit with no measured cell keeps the pixels.
    assert_eq!(lp.step(Input::Bytes(b"\x1b[?62;4c".to_vec()), &rx), None);
    assert!(!lp.app.sixel());
    // A measured cell inside the bound lets Sixel through.
    lp.app.set_measured_cell(Some((10, 20)));
    assert_eq!(lp.step(Input::Bytes(b"\x1b[?62;4c".to_vec()), &rx), None);
    assert!(lp.app.sixel());
}

/// The Sixel bit with an out-of-bound measured cell keeps the pixels.
#[test]
fn sixel_with_an_out_of_bound_cell_keeps_the_pixels() {
    let (mut lp, _) = super::tests::new_loop(TestBackend::new(60, 12), None);
    let (_, rx) = mpsc::channel();
    lp.app.set_measured_cell(Some((65, 128)));
    assert_eq!(lp.step(Input::Bytes(b"\x1b[?4c".to_vec()), &rx), None);
    assert!(!lp.app.sixel());
}

/// The home app the logo loops draw.
fn logo_app() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(80, 24);
    app.set_home(crate::home::Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        hover: true,
        version: "v9.9.9".to_owned(),
        model: None,
        thinking: None,
        logo_glyph: "\u{2307}".to_owned(),
        keys: crate::KeysSetup::default(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: Vec::new(),
        ..Default::default()
    });
    app
}

/// A loop on home with its tty on a pipe, so positioned writes and the
/// kitty writer's chunks land where the test reads them.
fn logo_loop() -> (Loop<TestBackend>, std::io::PipeReader, Receiver<Input>) {
    let (tty_out, tty_in) = std::io::pipe().unwrap_or_else(|err| panic!("pipe: {err}"));
    let (mut lp, _) = super::tests::new_loop(
        TestBackend::new(80, 24),
        Some(std::fs::File::from(std::os::fd::OwnedFd::from(tty_in))),
    );
    lp.app = logo_app();
    let (_, rx) = mpsc::channel();
    (lp, tty_out, rx)
}

/// A loop on home with its tty on a temp file: an unexpected emission
/// cannot fill a pipe and wedge the test, so a mutant that re-emits
/// fails on the assertions instead of hanging.
fn logo_file_loop() -> (Loop<TestBackend>, PathBuf, Receiver<Input>, fakes::TempDir) {
    let dir = fakes::TempDir::new("fiber-logo-reject");
    let tty_path = dir.path().join("tty");
    let tty = std::fs::File::create(&tty_path).unwrap_or_else(|err| panic!("tty: {err}"));
    let (mut lp, _) = super::tests::new_loop(TestBackend::new(80, 24), Some(tty));
    lp.app = logo_app();
    let (_, rx) = mpsc::channel();
    (lp, tty_path, rx, dir)
}

/// What the loop has written to its file tty so far.
fn read_tty(path: &PathBuf) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|err| panic!("tty: {err}"))
}

/// The drawn TestBackend buffer.
fn buffer_of(lp: &Loop<TestBackend>) -> ratatui::buffer::Buffer {
    lp.screen.backend().buffer().clone()
}

/// Whether any cell holds a kitty placeholder.
fn buffer_has_placeholder(buf: &ratatui::buffer::Buffer) -> bool {
    buf.content
        .iter()
        .any(|cell| cell.symbol().starts_with('\u{10EEEE}'))
}

/// The first frame draws pixels: no reply has arrived, so no image
/// writes, and the dim version stays.
#[test]
fn logo_first_frame_draws_pixels() {
    let (mut lp, mut tty, rx) = logo_loop();
    assert_eq!(lp.step(Input::Bytes(Vec::new()), &rx), None);
    let buf = buffer_of(&lp);
    assert!(!buffer_has_placeholder(&buf));
    assert_eq!(buf[(35, 1)].symbol(), "v");
    assert!(
        buf.content
            .iter()
            .take(80 * 4)
            .any(|cell| cell.symbol() != " "),
        "the pixel logo draws"
    );
    let sent = drain_tty(&mut tty);
    for marker in [b"i=30".as_slice(), b"1337", b"\x1bP0;1"] {
        assert!(
            !sent.windows(marker.len()).any(|window| *window == *marker),
            "no image writes before any reply: {sent:?}"
        );
    }
}

/// Still no replies after more frames: the pixels stay.
#[test]
fn logo_without_replies_stays_pixels() {
    let (mut lp, mut tty, rx) = logo_loop();
    for _ in 0..3 {
        assert_eq!(lp.step(Input::Bytes(Vec::new()), &rx), None);
    }
    let buf = buffer_of(&lp);
    assert!(!buffer_has_placeholder(&buf));
    assert_eq!(buf[(35, 1)].symbol(), "v");
    assert!(
        !drain_tty(&mut tty)
            .windows(4)
            .any(|window| window == b"1337"),
        "no image writes without replies"
    );
}

/// Without colour the pixel logo stays, even where a protocol replied.
#[test]
fn logo_no_colour_draws_pixels() {
    let (mut lp, mut tty, rx) = logo_loop();
    lp.app.set_iterm();
    lp.app.set_logo_depth(crate::look::Depth::NoColour);
    assert_eq!(lp.step(Input::Bytes(Vec::new()), &rx), None);
    let buf = buffer_of(&lp);
    assert!(!buffer_has_placeholder(&buf));
    assert_eq!(buf[(35, 1)].symbol(), "v");
    assert!(
        !drain_tty(&mut tty)
            .windows(4)
            .any(|window| window == b"1337"),
        "no colour writes no image"
    );
}

/// Kitty needs no measured geometry: the flags reply is enough for
/// placeholders, one transmit, and the place bytes.
#[test]
fn logo_kitty_swap_transmits_once() {
    let (mut lp, tty, rx) = logo_loop();
    lp.app.set_graphics();
    // The place closes the emission: the transmit's chunks precede it.
    let place = format!(
        "\x1b_Ga=p,U=1,i={},c={},r={}\x1b\\",
        crate::graphics::logo::ID,
        crate::graphics::logo::COLS,
        crate::graphics::logo::ROWS
    );
    let (sent, mut tty) = step_emission(
        &mut lp,
        &rx,
        Input::Bytes(Vec::new()),
        tty,
        place.into_bytes(),
    );
    let buf = buffer_of(&lp);
    assert_eq!(
        buf.content
            .iter()
            .filter(|cell| cell.symbol().starts_with('\u{10EEEE}'))
            .count(),
        128,
        "the logo's 32-by-4 cells hold placeholders"
    );
    super::tests::flush_kitty(&lp);
    let sent = String::from_utf8_lossy(&sent);
    for marker in [
        "i=30", "f=32", "s=640", "v=160", "a=t", "a=p", "c=32", "r=4",
    ] {
        assert!(
            sent.contains(marker),
            "the transmit carries {marker}: {sent:?}"
        );
    }
    // One more frame: no second transmit.
    assert_eq!(lp.step(Input::Bytes(Vec::new()), &rx), None);
    super::tests::flush_kitty(&lp);
    assert!(
        !drain_tty(&mut tty)
            .windows(3)
            .any(|window| window == b"a=t"),
        "the transmit runs once per theme per visit"
    );
}

/// A rejected kitty transmit leaves the pixel logo with no notice and
/// no retry this session.
#[test]
fn logo_kitty_rejection_draws_pixels_without_retry() {
    let (mut lp, tty, rx, _dir) = logo_file_loop();
    lp.app.set_graphics();
    assert_eq!(
        lp.step(Input::Bytes(b"\x1b_Gi=30;EINVAL\x1b\\".to_vec()), &rx),
        None
    );
    assert!(lp.app.logo_rejected());
    assert_eq!(lp.step(Input::Bytes(Vec::new()), &rx), None);
    let buf = buffer_of(&lp);
    assert!(!buffer_has_placeholder(&buf));
    assert_eq!(buf[(35, 1)].symbol(), "v");
    super::tests::flush_kitty(&lp);
    assert!(
        !read_tty(&tty).windows(4).any(|window| window == b"i=30"),
        "no transmit after a rejection"
    );
    // Another frame with the same tint: still no transmit.
    assert_eq!(lp.step(Input::Bytes(Vec::new()), &rx), None);
    super::tests::flush_kitty(&lp);
    assert!(
        !read_tty(&tty).windows(4).any(|window| window == b"i=30"),
        "a rejected session never re-asks"
    );
}

/// A dead writer still returns None: the logo never takes the
/// terminal-failure path itself; the writer's own KittyFailed quits
/// the loop.
#[test]
fn logo_kitty_dead_writer_returns_none() {
    let (mut lp, tty, rx) = logo_loop();
    let (out, failures) = mpsc::channel();
    lp.files_out = out;
    lp.app.set_graphics();
    // The place closes the emission: the transmit's chunks precede it.
    let place = format!(
        "\x1b_Ga=p,U=1,i={},c={},r={}\x1b\\",
        crate::graphics::logo::ID,
        crate::graphics::logo::COLS,
        crate::graphics::logo::ROWS
    );
    let (_sent, tty) = step_emission(
        &mut lp,
        &rx,
        Input::Bytes(Vec::new()),
        tty,
        place.clone().into_bytes(),
    );
    super::tests::flush_kitty(&lp);
    // No reader: the writer's next write fails, like a dead terminal.
    drop(tty);
    // One chunk the writer must write: its failure kills the writer
    // and reports KittyFailed.
    assert!(
        lp.kitty_writer
            .as_ref()
            .expect("the emission spawned the writer")
            .push(super::kitty_writer::Item::Image {
                id: crate::graphics::logo::ID,
                chunks: vec![b"doomed".to_vec()],
                place: Vec::new(),
            }),
        "the live writer queues"
    );
    match Deadline::after(MUST_SUCCEED_WITHIN).recv(&failures) {
        Ok(Input::KittyFailed) => {}
        Ok(_) => panic!("a failure, not KittyFailed"),
        Err(err) => panic!("waited {MUST_SUCCEED_WITHIN:?} for the failure: {err}"),
    }
    // The logo write still returns None: no quit, no retry.
    assert_eq!(
        lp.logo_write(
            crate::graphics::logo::Protocol::Kitty,
            crate::graphics::logo::After {
                bytes: place.into_bytes(),
                repaint: false,
            }
        ),
        None
    );
}

/// iTerm2's image writes positioned after the frame: the cursor saved,
/// moved to the logo rect, restored after the File.
#[test]
fn logo_iterm_writes_positioned_after_the_frame() {
    let (mut lp, tty, rx) = logo_loop();
    lp.app.set_iterm();
    let (sent, _tty) = step_emission(
        &mut lp,
        &rx,
        Input::Bytes(Vec::new()),
        tty,
        POSITIONED_TAIL.to_vec(),
    );
    let buf = buffer_of(&lp);
    assert!(
        !buffer_has_placeholder(&buf),
        "iTerm2 places by coordinates"
    );
    assert_eq!(buf[(35, 1)].symbol(), "v", "the version stays dim text");
    // The rect's own cells stay blank under the positioned image; the
    // version beside it is dim text, not a blank.
    let rect = crate::view::logo_rect(&lp.app, lp.screen.area()).expect("the logo rect");
    for y in rect.y..rect.y + rect.height {
        for x in rect.x..rect.x + rect.width {
            assert_eq!(
                buf[(x, y)].symbol(),
                " ",
                "blank cells draw under the positioned image"
            );
        }
    }
    let at = sent
        .windows(2)
        .position(|window| window == b"\x1b7")
        .expect("the cursor saves");
    assert_eq!(&sent[at + 2..at + 8], b"\x1b[1;1H", "at the logo rect");
    assert!(
        sent.windows(4).any(|window| window == b"1337"),
        "the File writes"
    );
    assert_eq!(&sent[sent.len() - 2..], b"\x1b8", "the cursor restores");
}

/// A scrolled frame rewrites the iTerm2 image: one File per touch.
#[test]
fn logo_iterm_rewrites_after_touch() {
    let (mut lp, tty, rx) = logo_loop();
    lp.app.set_iterm();
    let (first, tty) = step_emission(
        &mut lp,
        &rx,
        Input::Bytes(Vec::new()),
        tty,
        POSITIONED_TAIL.to_vec(),
    );
    assert_eq!(
        first.windows(4).filter(|window| *window == b"1337").count(),
        1
    );
    // Typing rewrites the frame, and the image with it.
    let (second, _tty) = step_emission(
        &mut lp,
        &rx,
        Input::Bytes(b"x".to_vec()),
        tty,
        POSITIONED_TAIL.to_vec(),
    );
    assert_eq!(
        second
            .windows(4)
            .filter(|window| *window == b"1337")
            .count(),
        1,
        "a touched frame rewrites the image"
    );
}

/// Sixel scales to the measured cell: 10 by 20 pixels draws a 320 by
/// 80 image.
#[test]
fn logo_sixel_scales_to_the_measured_cell() {
    bounded_sixel_test(
        "event_loop::image_tests::logo_sixel_scales_to_the_measured_cell",
        logo_sixel_scales_to_the_measured_cell_body,
    );
}

fn logo_sixel_scales_to_the_measured_cell_body() {
    let (mut lp, mut tty, rx) = logo_loop();
    lp.app.set_measured_cell(Some((10, 20)));
    assert_eq!(lp.step(Input::Bytes(b"\x1b[?62;4c".to_vec()), &rx), None);
    assert_eq!(lp.step(Input::Bytes(Vec::new()), &rx), None);
    let sent = drain_tty(&mut tty);
    let sent = String::from_utf8_lossy(&sent);
    assert!(sent.contains("\"1;1;320;80"), "320 by 80: {sent:?}");
    assert!(sent.contains('P'), "the Sixel data writes");
}

/// The cell bound at run level: zero, past the bound, and absurd cells
/// keep pixels with no Sixel bytes; 64 by 128 draws 2048 by 512.
#[test]
fn logo_cell_bound_rows() {
    bounded_sixel_test(
        "event_loop::image_tests::logo_cell_bound_rows",
        logo_cell_bound_rows_body,
    );
}

fn logo_cell_bound_rows_body() {
    for cell in [None, Some((0, 0)), Some((65, 128)), Some((65535, 65535))] {
        let (mut lp, mut tty, rx) = logo_loop();
        lp.app.set_measured_cell(cell);
        assert_eq!(lp.step(Input::Bytes(b"\x1b[?4c".to_vec()), &rx), None);
        assert_eq!(lp.step(Input::Bytes(Vec::new()), &rx), None);
        let sent = drain_tty(&mut tty);
        assert!(
            !sent.windows(4).any(|window| window == b"\x1bP0"),
            "{cell:?} keeps the pixels"
        );
        assert!(!buffer_has_placeholder(&buffer_of(&lp)));
    }
    let (mut lp, tty, rx) = logo_loop();
    lp.app.set_measured_cell(Some((64, 128)));
    let (wire, mut tty) = step_emission(
        &mut lp,
        &rx,
        Input::Bytes(b"\x1b[?4c".to_vec()),
        tty,
        SIXEL_TAIL.to_vec(),
    );
    let sent = String::from_utf8_lossy(&wire);
    assert!(sent.contains("\"1;1;2048;512"), "64 by 128: {sent:?}");
    // A held frame rewrites nothing.
    assert_eq!(lp.step(Input::Bytes(Vec::new()), &rx), None);
    assert!(
        !drain_tty(&mut tty)
            .windows(4)
            .any(|window| window == b"\x1bP0"),
        "a held frame rewrites nothing"
    );
}

/// Re-tinting re-emits: a new theme's bytes differ from the old.
#[test]
fn logo_retints_with_the_theme() {
    let (mut lp, tty, rx) = logo_loop();
    lp.app.set_iterm();
    let (first, tty) = step_emission(
        &mut lp,
        &rx,
        Input::Bytes(Vec::new()),
        tty,
        POSITIONED_TAIL.to_vec(),
    );
    assert_eq!(
        first.windows(4).filter(|window| *window == b"1337").count(),
        1
    );
    let light = crate::look::Look::new(crate::ThemeSetting::Light, &|_: &str| -> Option<String> {
        None
    })
    .0;
    lp.app.set_logo_depth(light.depth());
    lp.screen.set_look(light);
    let (second, _tty) = step_emission(
        &mut lp,
        &rx,
        Input::Bytes(Vec::new()),
        tty,
        POSITIONED_TAIL.to_vec(),
    );
    assert_eq!(
        second
            .windows(4)
            .filter(|window| *window == b"1337")
            .count(),
        1
    );
    assert_ne!(first, second, "the re-tint writes new bytes");
}

/// Leaving home repaints and redraws at once: the stale image clears
/// with the frame, never a blank screen.
#[test]
fn logo_leave_home_repaints_then_redraws() {
    let (mut lp, tty, rx) = logo_loop();
    lp.app.set_iterm();
    let (first, tty) = step_emission(
        &mut lp,
        &rx,
        Input::Bytes(Vec::new()),
        tty,
        POSITIONED_TAIL.to_vec(),
    );
    assert_eq!(
        first.windows(4).filter(|window| *window == b"1337").count(),
        1,
        "home emits the image"
    );
    lp.app.attach(contract::SessionId(SESSION.to_owned()));
    assert_eq!(lp.step(Input::Bytes(Vec::new()), &rx), None);
    let mut tty = tty;
    let sent = drain_tty(&mut tty);
    assert!(
        !sent.windows(4).any(|window| window == b"1337"),
        "a gone rect emits nothing"
    );
    assert!(!lp.app.on_home(), "the loop left home");
    // Settled on the session, further frames emit nothing.
    assert_eq!(lp.step(Input::Bytes(Vec::new()), &rx), None);
    assert!(
        !drain_tty(&mut tty)
            .windows(4)
            .any(|window| window == b"1337"),
        "a non-home screen emits nothing"
    );
}

/// A session that never showed the logo clears nothing on any frame.
#[test]
fn logo_non_home_never_clears() {
    let (mut lp, mut tty, rx) = logo_loop();
    lp.app.attach(contract::SessionId(SESSION.to_owned()));
    for _ in 0..2 {
        assert_eq!(lp.step(Input::Bytes(Vec::new()), &rx), None);
    }
    assert!(
        !drain_tty(&mut tty)
            .windows(4)
            .any(|window| window == b"\x1b[2J"),
        "a non-home screen is never cleared"
    );
}

/// A flat home keeps pixels, and unflattening re-emits the image.
#[test]
fn logo_flat_home_keeps_pixels_then_reemits() {
    let (mut lp, tty, rx) = logo_loop();
    lp.app.set_iterm();
    let (first, tty) = step_emission(
        &mut lp,
        &rx,
        Input::Bytes(Vec::new()),
        tty,
        POSITIONED_TAIL.to_vec(),
    );
    assert_eq!(
        first.windows(4).filter(|window| *window == b"1337").count(),
        1
    );
    // Flat: pixels, no image, and the rect-gone repaint.
    lp.app.set_flat(true);
    assert_eq!(lp.step(Input::Bytes(Vec::new()), &rx), None);
    let mut tty = tty;
    let buf = buffer_of(&lp);
    assert_eq!(buf[(35, 1)].symbol(), "v");
    let flat = drain_tty(&mut tty);
    assert!(
        !flat.windows(4).any(|window| window == b"1337"),
        "flat writes no image"
    );
    // Unflattening re-emits into the returned rect, on the same theme.
    lp.app.set_flat(false);
    let (second, _tty) = step_emission(
        &mut lp,
        &rx,
        Input::Bytes(Vec::new()),
        tty,
        POSITIONED_TAIL.to_vec(),
    );
    assert_eq!(
        second
            .windows(4)
            .filter(|window| *window == b"1337")
            .count(),
        1,
        "the returned rect re-emits"
    );
    assert_eq!(first, second, "the same theme re-emits the same bytes");
}

/// The session's flags: attaching keeps an armed Sixel (the terminal
/// did not change) but clears a rejection, so the next session
/// re-attempts the transmit.
#[test]
fn logo_session_change_keeps_armed_sixel_clears_rejection() {
    let (mut lp, _) = super::tests::new_loop(TestBackend::new(60, 12), None);
    lp.app.set_measured_cell(Some((10, 20)));
    lp.app.set_sixel();
    assert!(lp.app.logo_cell_ok());
    lp.app.attach(contract::SessionId(SESSION.to_owned()));
    assert!(lp.app.sixel(), "the Sixel reply persists");
    assert_eq!(lp.app.measured_cell(), Some((10, 20)));
    assert!(lp.app.logo_cell_ok(), "the gate persists with the cell");
    let (mut lp, _) = super::tests::new_loop(TestBackend::new(60, 12), None);
    lp.app.set_graphics();
    lp.app.set_logo_rejected();
    assert!(lp.app.logo_rejected());
    lp.app.attach(contract::SessionId(SESSION.to_owned()));
    assert!(!lp.app.logo_rejected(), "the next session re-attempts");
}
