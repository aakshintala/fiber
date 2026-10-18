//! The hub's catalogue through `run` on a pseudo-terminal: a remote
//! home with no model reads the hub's lists and opens the start pick
//! over them, never touching the client's.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, mpsc};

use fakes::Deadline;
use fakes::clock::FakeClock;
use serde_json::Value;

use super::tests::{DEADLINE, hello, launch, once, open};
use crate::catalogue::{Catalogue, ModelEntry, Refresh};
use crate::configure_fake::Fake;

/// The hub's global file: the workspace derives from its parent.
const GLOBAL: &str = "/hub/.fiber/config.json";

/// The client's lists, holding only `zed/z1`: `run` must never read
/// them on a remote terminal.
fn client_models(calls: Arc<Mutex<usize>>) -> crate::ReadModels {
    Arc::new(move |_: Refresh| {
        if let Ok(mut held) = calls.lock() {
            *held += 1;
        }
        Ok(Catalogue {
            models: vec![ModelEntry {
                reference: "zed/z1".to_owned(),
                provider: "zed".to_owned(),
                id: "z1".to_owned(),
                name: None,
                levels: Vec::new(),
                default_level: None,
                configured: None,
                roles: Vec::new(),
                price: None,
            }],
            notices: Vec::new(),
            lists: Vec::new(),
        })
    })
}

/// Answers one hub line: the global file, an empty directory listing,
/// the chip's rows, or the two-model catalogue. `feed` and `recent`
/// are fire-and-forget from the loop and need no answer. Panics when a
/// `models` line arrives before any `global_file` was answered: every
/// workspace derives from the global file first.
fn answer(line: &Value, global_answered: &mut bool) -> Option<Value> {
    let args = line.get("args")?;
    let operation = args.get("operation")?.as_str()?;
    let result = match operation {
        "global_file" => {
            *global_answered = true;
            serde_json::json!({"file": GLOBAL})
        }
        "directories" => serde_json::json!({"directories": [], "path": "/"}),
        "settings" => serde_json::json!({"rows": [
            {"key": "scoped_models", "shown": "value",
             "value": "[\"acme/m1\",\"acme/m2\"]", "layer": "global",
             "file": GLOBAL, "scope": "global_only"},
        ]}),
        "models" => {
            assert!(
                *global_answered,
                "a models line before any global_file answer: {line}"
            );
            serde_json::json!({"models": [
                {"reference": "acme/m1", "provider": "acme", "id": "m1",
                 "levels": ["low", "high"],
                 "default_level": "low", "roles": ["main"],
                 "price": {"micros_per_mtok": 3000000,
                           "input": 3000000, "output": 15000000,
                           "tiers": [{"input_tokens_above": 200000,
                                       "micros_per_mtok": 6000000,
                                       "input": 6000000,
                                       "output": 30000000}]}},
                {"reference": "acme/m2", "provider": "acme", "id": "m2",
                 "levels": [], "roles": []},
            ], "notices": [], "lists": []})
        }
        _ => return None,
    };
    Some(serde_json::json!({"command_id": line["id"], "result": result}))
}

/// Serves the hub's end until the terminal hangs up, reporting every
/// line read on `lines` and answering the recognised ones.
fn serve(hub: UnixStream, lines: mpsc::Sender<Value>) {
    let mut reader = BufReader::new(hub.try_clone().unwrap_or_else(|err| panic!("dup: {err}")));
    let mut hub = hub;
    let mut global_answered = false;
    let mut text = String::new();
    while reader.read_line(&mut text).unwrap_or(0) > 0 {
        let line: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        text.clear();
        lines.send(line.clone()).unwrap_or(());
        let Some(answer) = answer(&line, &mut global_answered) else {
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

/// Receives reported hub lines until `matches`, all under the one
/// `wait` deadline, failing with `what` on expiry.
#[track_caller]
fn until(
    lines: &Receiver<Value>,
    wait: &Deadline,
    what: &str,
    mut matches: impl FnMut(&Value) -> bool,
) {
    loop {
        match wait.recv(lines) {
            Ok(line) => {
                if matches(&line) {
                    return;
                }
            }
            Err(err) => panic!("waited for {what}: {err}"),
        }
    }
}

/// Whether `line` is a `models` command with this `refresh` word.
fn models_refresh(line: &Value, refresh: &str) -> bool {
    line.get("command").and_then(Value::as_str) == Some("configure")
        && line
            .get("args")
            .and_then(|args| args.get("operation"))
            .and_then(Value::as_str)
            == Some("models")
        && line
            .get("args")
            .and_then(|args| args.get("refresh"))
            .and_then(Value::as_str)
            == Some(refresh)
}

#[test]
fn the_start_pick_lists_the_hubs_models() {
    let pair = open();
    rustix::termios::tcsetwinsize(
        &pair.slave,
        rustix::termios::Winsize {
            ws_col: 160,
            ws_row: 12,
            ws_xpixel: 0,
            ws_ypixel: 0,
        },
    )
    .unwrap_or_else(|err| panic!("winsize: {err}"));
    // The tick runs only while something drawn moves, so frames only
    // draw on input: drain every byte the terminal is sent, so no
    // write blocks, and scan them for the hub's models.
    let drained = Arc::new(Mutex::new(Vec::new()));
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
    let (reported, lines) = mpsc::channel();
    std::thread::Builder::new()
        .name("remote-run-hub".to_owned())
        .spawn(move || serve(hub, reported))
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    let calls = Arc::new(Mutex::new(0usize));
    let mut remote = launch();
    remote.location = crate::LaunchLocation::Remote {
        hub: "box".to_owned(),
    };
    remote.model = None;
    // A remote terminal keeps its client-owned rows locally: the seam
    // is what `seams` builds the hub remote over. The client's lists
    // stay installed but unread.
    remote.configure = Some(Arc::new(Fake::new(Vec::new())));
    remote.models = Some(client_models(Arc::clone(&calls)));
    let slave = pair
        .slave
        .try_clone()
        .unwrap_or_else(|err| panic!("dup: {err}"));
    let clock: Arc<dyn contract::clock::Clock> = FakeClock::new();
    let (done, finished) = mpsc::channel();
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
    // The startup read runs at the PB-4 workspace with the cached word.
    until(
        &lines,
        &Deadline::start(),
        "the cached models read",
        |line| {
            line.get("args")
                .and_then(|args| args.get("workspace"))
                .and_then(Value::as_str)
                == Some("/hub/.fiber")
                && models_refresh(line, "cached")
        },
    );
    // No input until the first frame is read; then moving the
    // selection redraws without touching the rows. The rows draw
    // provider and id in separate styled cells, so the halves, not
    // the references, are what the bytes hold.
    let mut pair = pair;
    let wait = Deadline::start();
    let opened = loop {
        {
            let held = drained.lock().unwrap_or_else(|err| panic!("lock: {err}"));
            if !held.is_empty() {
                break true;
            }
        }
        if wait.left().is_zero() {
            break false;
        }
        let (pause, paused) = mpsc::channel::<()>();
        let pause_wait = Deadline::after(std::time::Duration::from_millis(50));
        assert!(pause_wait.recv(&paused).is_err());
        drop(pause);
    };
    assert!(opened, "no first frame");
    let listed = loop {
        pair.main
            .write_all(b"\x1b[B")
            .unwrap_or_else(|err| panic!("write: {err}"));
        // One sanctioned wait between pokes: sleeping is banned, so
        // the pause is a receive that never arrives.
        let (pause, paused) = mpsc::channel::<()>();
        let pause_wait = Deadline::after(std::time::Duration::from_millis(200));
        assert!(pause_wait.recv(&paused).is_err());
        drop(pause);
        let held = drained.lock().unwrap_or_else(|err| panic!("lock: {err}"));
        let held: &[u8] = &held;
        if held.windows(2).any(|w| w == b"m1") && held.windows(2).any(|w| w == b"m2") {
            break true;
        }
        if wait.left().is_zero() {
            break false;
        }
    };
    assert!(listed, "no frame lists the hub's models");
    assert_eq!(
        *calls.lock().unwrap_or_else(|err| panic!("lock: {err}")),
        0,
        "the client's lists were read"
    );
    // A refresh reads every list from the hub, not the client.
    pair.main
        .write_all(b"\x12")
        .unwrap_or_else(|err| panic!("write: {err}"));
    until(
        &lines,
        &Deadline::start(),
        "the every models read",
        |line| models_refresh(line, "every"),
    );
    assert_eq!(
        *calls.lock().unwrap_or_else(|err| panic!("lock: {err}")),
        0,
        "the client's lists were read"
    );
    let held = drained.lock().unwrap_or_else(|err| panic!("lock: {err}"));
    let held: &[u8] = &held;
    assert!(
        !held.windows(2).any(|w| w == b"z1"),
        "a frame lists the client's model"
    );
    pair.main
        .write_all(&[0x03, 0x03])
        .unwrap_or_else(|err| panic!("write: {err}"));
    match Deadline::after(DEADLINE).recv(&finished) {
        Ok(code) => assert_eq!(code, 0),
        Err(err) => panic!("waited {DEADLINE:?} for quit: {err}"),
    }
    assert_eq!(
        *calls.lock().unwrap_or_else(|err| panic!("lock: {err}")),
        0,
        "the client's lists were read"
    );
}
