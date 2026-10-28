//! Run-level tests for opening an image: the click reads the file
//! over the hub, and the worker opens the copy. A stored JPEG converts
//! through the image child before it reaches Kitty.

use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};

use ratatui::backend::TestBackend;
use serde_json::{Value, json};

use super::Loop;
use crate::Input;
use crate::app::{App, Effect, Target};
use crate::link::Line;
use crate::mouse::TargetId;
use crate::screen::Screen;
use fakes::{Deadline, MUST_SUCCEED_WITHIN};

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
    let waiting = drain_tty(&mut tty);
    assert!(
        !String::from_utf8_lossy(&waiting).contains("Ga=t"),
        "nothing transmits while the completion waits"
    );
    // Stepping the completion transmits under the placeholders.
    let (lp, _rx) = stepped(lp, rx, done);
    let sent = drain_tty(&mut tty);
    let sent = String::from_utf8_lossy(&sent);
    assert!(
        sent.contains("Ga=t,f=100,i=1,"),
        "the converted PNG transmits: {sent:?}"
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
