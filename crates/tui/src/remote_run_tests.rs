//! The hub's catalogue through `run` on a pseudo-terminal: a remote
//! home with no model opens the start pick over the hub's models, and
//! the picker lists them.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::Arc;

use fakes::Deadline;
use fakes::clock::FakeClock;

use super::tests::{DEADLINE, hello, launch, once, open};
use crate::configure_fake::Fake;

/// Answers one hub line: the global file, the chip's rows, or the
/// one-model catalogue. Any other command is fire-and-forget from the
/// loop (`feed`, `recent`) and needs no answer.
fn answer(line: &serde_json::Value) -> Option<serde_json::Value> {
    let args = line.get("args")?;
    let operation = args.get("operation")?.as_str()?;
    let result = match operation {
        "global_file" => serde_json::json!({"file": "/hub/config.json"}),
        "directories" => serde_json::json!({"directories": [], "path": "/"}),
        "settings" => serde_json::json!({"rows": [
            {"key": "scoped_models", "shown": "value",
             "value": "[\"hub/m1\"]", "layer": "global",
             "file": "/hub/config.json", "scope": "global_only"},
        ]}),
        "models" => serde_json::json!({"models": [
            {"reference": "hub/m1", "provider": "hub", "id": "m1",
             "levels": [], "roles": []},
        ], "notices": [], "lists": []}),
        _ => return None,
    };
    Some(serde_json::json!({"command_id": line["id"], "result": result}))
}

/// Serves the hub's end until the terminal hangs up: one answer per
/// recognised command line.
fn serve(hub: UnixStream) {
    let mut reader = BufReader::new(hub.try_clone().unwrap_or_else(|err| panic!("dup: {err}")));
    let mut hub = hub;
    let mut text = String::new();
    while reader.read_line(&mut text).unwrap_or(0) > 0 {
        let line: serde_json::Value =
            serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
        text.clear();
        let Some(answer) = answer(&line) else {
            continue;
        };
        let accepted = contract::HubLine {
            kind: "command_accepted".to_owned(),
            ts: 0,
            schema_version: contract::SCHEMA_VERSION,
            payload: answer.as_object().cloned().unwrap_or_default(),
        };
        let mut bytes = serde_json::to_string(&accepted).unwrap_or_default();
        bytes.push('\n');
        if hub.write_all(bytes.as_bytes()).is_err() {
            return;
        }
    }
}

#[test]
fn the_start_pick_lists_the_hubs_models() {
    let pair = open();
    // The tick runs only while something drawn moves, so frames only
    // draw on input: drain every byte the terminal is sent, so no
    // write blocks, and scan them for the hub's model.
    let drained = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = Arc::clone(&drained);
    let mut main = pair
        .main
        .try_clone()
        .unwrap_or_else(|err| panic!("dup: {err}"));
    std::thread::Builder::new()
        .name("remote-run-drain".to_owned())
        .spawn(move || {
            use std::io::Read as _;
            let mut buf = [0u8; 4096];
            loop {
                match main.read(&mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => {
                        if let Ok(mut held) = sink.lock() {
                            held.extend_from_slice(&buf[..n]);
                        }
                    }
                }
            }
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    let (terminal, hub) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    std::thread::Builder::new()
        .name("remote-run-hub".to_owned())
        .spawn(move || serve(hub))
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    let mut remote = launch();
    remote.location = crate::LaunchLocation::Remote {
        hub: "test".to_owned(),
    };
    remote.model = None;
    // A remote terminal keeps its client-owned rows locally: the seam
    // is what `seams` builds the hub remote over.
    remote.configure = Some(Arc::new(Fake::new(Vec::new())));
    let slave = pair
        .slave
        .try_clone()
        .unwrap_or_else(|err| panic!("dup: {err}"));
    let clock: Arc<dyn contract::clock::Clock> = FakeClock::new();
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("remote-run".to_owned())
        .spawn(move || {
            let code = super::run(
                slave,
                remote,
                once(terminal, hello()),
                Box::new(|_| {}),
                clock,
            )
            .unwrap_or_else(|error| panic!("run: {error}"));
            done.send(code).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    // Moving the selection redraws without touching the rows: poll
    // the drained bytes until a frame lists the hub's model. The row
    // draws its provider and id in separate styled cells, so neither
    // the reference nor the name is contiguous: both halves present
    // means the row drew.
    let mut pair = pair;
    let wait = Deadline::after(DEADLINE);
    let listed = loop {
        pair.main
            .write_all(b"\x1b[B")
            .unwrap_or_else(|err| panic!("write: {err}"));
        // One sanctioned wait between pokes: sleeping is banned, so
        // the pause is a receive that never arrives.
        let (pause, paused) = std::sync::mpsc::channel::<()>();
        let pause_wait = fakes::Deadline::after(std::time::Duration::from_millis(200));
        assert!(pause_wait.recv(&paused).is_err());
        drop(pause);
        let held = drained.lock().unwrap_or_else(|err| panic!("lock: {err}"));
        let held: &[u8] = &held;
        if held.windows(3).any(|w| w == b"hub") && held.windows(2).any(|w| w == b"m1") {
            break true;
        }
        if wait.left().is_zero() {
            break false;
        }
    };
    assert!(listed, "no frame lists the hub's model");
    pair.main
        .write_all(&[0x03, 0x03])
        .unwrap_or_else(|err| panic!("write: {err}"));
    match Deadline::after(DEADLINE).recv(&finished) {
        Ok(code) => assert_eq!(code, 0),
        Err(err) => panic!("waited {DEADLINE:?} for quit: {err}"),
    }
}
