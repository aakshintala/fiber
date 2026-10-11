//! Run-level tests for opening an image: the click reads the file
//! over the hub, and the worker opens the copy.

use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use ratatui::backend::TestBackend;
use serde_json::{Value, json};

use super::Loop;
use crate::Input;
use crate::app::{App, Effect, Target};
use crate::link::Line;
use crate::mouse::TargetId;
use crate::screen::Screen;
use fakes::Deadline;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

/// One deadline per receive: the hub and the worker answer promptly.
const DEADLINE: Duration = Duration::from_secs(5);

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
    let lp = Loop {
        app,
        parser: crate::keys::Parser::default(),
        screen,
        hub: Some(ours),
        tty: None,
        on_attach: Box::new(|_| {}),
        clock: fakes::clock::FakeClock::new(),
        wakeups: 0,
        stash: std::collections::VecDeque::new(),
        files_out: Some(out),
        search: None,
        reader: None,
        model_reader: crate::catalogue::Reader::new(None),
        paste_reader: None,
        pointer: crate::mouse::Pointer::default(),
        hover: true,
        var: Box::new(|_| None),
        copy_command: None,
        open_command: None,
        viewer,
        images_dir: images.clone(),
        title: crate::osc::Title::default(),
        shape: crate::osc::Shape::default(),
        retry: None,
        tick: crate::tick::TickThread::idle(),
    };
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
        .set_read_timeout(Some(DEADLINE))
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
            | Input::Files { .. }
            | Input::FindDue(_)
            | Input::Image { .. }
            | Input::Models(_)
            | Input::Login { .. }
            | Input::Tick,
        ) => panic!("the worker answered something else"),
        Err(err) => panic!("waited {DEADLINE:?} for the worker: {err}"),
    }
}

#[test]
fn a_click_on_an_image_line_reads_the_file_and_opens_it() {
    // The viewer prints the copy's path to the marker file.
    let (mut lp, theirs, rx, images, marker, _dir) = loop_with_viewer("echo \"$2\" > \"$1\"\n");
    let (_idle_tx, idle) = mpsc::channel();
    let lines = click(&mut lp);
    lp.send(&lines);
    let id = answer(&theirs);
    assert_eq!(lp.step(accepted(&id, "Ynl0ZXM="), &idle), None);
    match completion(&rx, &Deadline::after(DEADLINE)) {
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
        | Input::Files { .. }
        | Input::FindDue(_)
        | Input::Image { .. }
        | Input::Models(_)
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
    lp.send(&lines);
    let id = answer(&theirs);
    assert_eq!(lp.step(accepted(&id, "Ynl0ZXM="), &idle), None);
    // The worker's completion arrives as its own input: stepping it
    // draws the notice.
    let done = completion(&rx, &Deadline::after(DEADLINE));
    assert!(matches!(done, Input::Viewed { result: Err(_), .. }));
    assert_eq!(lp.step(done, &idle), None);
    assert!(
        lp.app
            .notice()
            .is_some_and(|notice| notice.starts_with("Could not open shot.png: "))
    );
}
