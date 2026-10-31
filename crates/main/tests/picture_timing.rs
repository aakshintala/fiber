//! Binary-level `picture.run` journeys (see #1212, Part 12): the built
//! `fiber` runs `ask` against the fake provider server, and sessions over
//! their sockets, through the installed composition, which forwards the
//! debug timing to both image drivers. At debug one entered image writes
//! exactly one `picture.run` with only `op` and `ms`; without an entered
//! image, or below debug, nothing is written. A paste the child never
//! enters emits nothing.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod extension_harness;
mod support;

use std::fs;
use std::path::{Path, PathBuf};

use extension_harness::{Socket, send, until_session, until_session_close};
use fakes::ProviderServer;
use serde_json::{Value, json};
use support::{
    PIXEL, PIXEL_BASE64, Run, Setup, function_call, hello, is_status, read_kinds, run_to_exit,
    stream, write_json,
};

/// Runs `fiber ask <prompt>` in the workspace to exit and parses its stdout.
#[track_caller]
fn ask(setup: &Setup, prompt: &str) -> Run {
    let mut command = setup.fiber(&["ask", prompt]);
    command.current_dir(setup.workspace());
    let output = run_to_exit(setup.deadline, "fiber ask", command);
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines = stdout
        .lines()
        .filter(|l| !is_status(l))
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    Run {
        code: output.status.code(),
        lines,
        stderr: String::from_utf8(output.stderr).unwrap(),
    }
}

/// A `read` of `note.txt` answered, then a closing answer.
fn read_script() -> [fakes::Response; 2] {
    [
        stream(&[function_call(
            "call_read",
            "read",
            &json!({"path": "note.txt"}),
        )]),
        hello(),
    ]
}

/// Every diagnostic line under the home's `logs/`, parsed.
fn log_lines(setup: &Setup) -> Vec<Value> {
    let mut lines = Vec::new();
    let logs = setup.home().join("logs");
    if !logs.exists() {
        return lines;
    }
    for entry in fs::read_dir(&logs).unwrap() {
        let text = fs::read_to_string(entry.unwrap().path()).unwrap();
        for line in text.lines() {
            lines.push(serde_json::from_str(line).unwrap());
        }
    }
    lines
}

/// The `picture.run` lines among `lines`: never an argument, a path or any
/// content, only the operation and the milliseconds.
fn picture_runs(lines: &[Value]) -> Vec<&Value> {
    lines
        .iter()
        .filter(|line| line["code"] == "op_timing" && line["data"]["op"] == "picture.run")
        .collect()
}

#[test]
fn at_default_a_text_read_writes_no_picture_run() {
    let setup = Setup::new();
    fs::write(setup.workspace().join("note.txt"), "alpha line\n").unwrap();
    let server = ProviderServer::start(read_script()).unwrap();
    setup.provider(&server);

    let run = ask(&setup, "read the note");

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), read_kinds());
    assert_eq!(server.requests().len(), 2, "the read really ran");
    assert!(
        picture_runs(&log_lines(&setup)).is_empty(),
        "no picture.run without an image"
    );
}

#[test]
fn at_info_a_text_read_writes_no_picture_run() {
    let setup = Setup::new();
    write_json(
        &setup.home().join("config.json"),
        &json!({"diagnostics": {"level": "info"}}),
    );
    fs::write(setup.workspace().join("note.txt"), "alpha line\n").unwrap();
    let server = ProviderServer::start(read_script()).unwrap();
    setup.provider(&server);

    let run = ask(&setup, "read the note");

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), read_kinds());
    assert_eq!(server.requests().len(), 2, "the read really ran");
    assert!(
        picture_runs(&log_lines(&setup)).is_empty(),
        "no picture.run without an image"
    );
}

#[test]
fn an_ignored_repository_debug_level_writes_no_picture_run() {
    let setup = Setup::new();
    write_json(
        &setup.workspace().join(".fiber/config.json"),
        &json!({"diagnostics": {"level": "debug"}}),
    );
    fs::write(setup.workspace().join("note.txt"), "alpha line\n").unwrap();
    let server = ProviderServer::start(read_script()).unwrap();
    setup.provider(&server);

    let run = ask(&setup, "read the note");

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(
        run.all("notice")
            .iter()
            .any(|notice| notice["payload"]["code"] == "config_key_ignored"),
        "the repository value stays ignored: {:?}",
        run.kinds()
    );
    assert!(
        run.lines
            .iter()
            .any(|line| line["kind"] == "tool_call_completed"),
        "the read really ran"
    );
    assert!(
        picture_runs(&log_lines(&setup)).is_empty(),
        "no picture.run without an image"
    );
}

#[test]
fn at_debug_a_text_read_writes_no_picture_run() {
    let setup = Setup::new();
    write_json(
        &setup.home().join("config.json"),
        &json!({"diagnostics": {"level": "debug"}}),
    );
    fs::write(setup.workspace().join("note.txt"), "alpha line\n").unwrap();
    let server = ProviderServer::start(read_script()).unwrap();
    setup.provider(&server);

    let run = ask(&setup, "read the note");

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), read_kinds());
    assert!(
        run.lines
            .iter()
            .any(|line| line["kind"] == "tool_call_completed"),
        "the read really ran"
    );
    // Other operations may time at debug; the imageless read times none.
    assert!(
        picture_runs(&log_lines(&setup)).is_empty(),
        "no picture.run without an image"
    );
}

impl Setup {
    /// Installs the provider with the fake model declaring `input`
    /// `["text", "image"]`, so a pasted image is sent as an image part.
    fn provider_with_images(&self, server: &ProviderServer) {
        let source = self.root.path().join("src");
        write_json(
            &source.join("extension.json"),
            &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "providers": {"fake": [format!("{}/v1", server.url())]}}),
        );
        write_json(
            &source.join("providers/fake.json"),
            &json!({
                "name": "fake",
                "credential": {"env": "FIBER_TEST_FAKE_KEY"},
                "models": [{"id": "m", "protocol": "openai-responses", "base_url": format!("{}/v1", server.url()), "context_window": 100000, "input": ["text", "image"]}]
            }),
        );
        extensions::plan(
            &self.home(),
            &extensions::Request::Path(source),
            "0.0.0",
            &extensions::Origin::github(),
            &*fakes::clock::FakeClock::new(),
        )
        .unwrap()
        .commit()
        .unwrap();
        write_json(
            &self.home().join("config.json"),
            &json!({"model": "fake/m"}),
        );
    }

    /// The session's directory, from its id.
    fn session_dir(&self, id: &str) -> PathBuf {
        log::sessions_dir(&self.home(), &worktree::project(&self.workspace())).join(id)
    }
}

/// One socket line, such as the subscribe acknowledgement: the first line
/// [`until_session`] takes within the test's deadline.
fn recv(client: &Socket, what: &str) -> Value {
    until_session(client, what, |_| true)
        .into_iter()
        .next()
        .unwrap()
}

fn answer(client: &Socket, id: &str) -> Value {
    until_session(client, "the answer", |line| {
        line["payload"].get("command_id") == Some(&json!(id))
    })
    .into_iter()
    .next_back()
    .unwrap()
}

#[test]
fn an_invalid_pasted_image_is_refused_with_no_picture_run() {
    let setup = Setup::new_with_fm_root();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider_with_images(&server);
    // At debug, so the zero-picture assertion below is sensitive to an
    // accidental image-child entry: info would suppress timing regardless.
    debug_home(&setup);
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);

    let client = running.connect(&setup.socket(&id));
    running.wait_for("extensions_loaded");
    send(
        &client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    recv(&client, "the subscribe acknowledgement");
    // Not base64 at all: the paste is refused before the child runs.
    send(
        &client,
        r#"{"id":"c_1","command":"prompt","args":{"content":[{"type":"image","data":"!!!not-base64!!!","mime_type":"image/png"}]}}"#,
    );
    let refused = answer(&client, "c_1");
    assert_eq!(refused["kind"], "command_rejected", "{refused}");
    assert_eq!(refused["payload"]["code"], "invalid_arguments", "{refused}");
    assert!(
        refused["payload"]["message"]
            .as_str()
            .unwrap()
            .contains("not base64"),
        "{refused}"
    );
    assert!(server.requests().is_empty(), "no turn ran for the refusal");
    // A text turn after the refusal: the session persists events and
    // answers, proving the run below scans logs the session really wrote.
    send(
        &client,
        r#"{"id":"c_2","command":"prompt","args":{"content":[{"type":"text","text":"hi"}]}}"#,
    );
    until_session(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    assert_eq!(server.requests().len(), 1, "the text turn really ran");

    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let _tail = until_session_close(&client);
    drop(client);
    let (status, _out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");

    let mut lines = log_lines(&setup);
    let events = fs::read_to_string(setup.session_dir(&id).join("events.jsonl")).unwrap();
    for line in events.lines() {
        if line.is_empty() {
            continue;
        }
        lines.push(serde_json::from_str(line).unwrap());
    }
    assert!(
        picture_runs(&lines).is_empty(),
        "a refused paste enters no scope"
    );
}

/// A `read` of `pic.png` answered, then a closing answer.
fn image_read_script() -> [fakes::Response; 2] {
    [
        stream(&[function_call(
            "call_read",
            "read",
            &json!({"path": "pic.png"}),
        )]),
        hello(),
    ]
}

/// Runs `fiber ask <prompt>` in the workspace to exit, returning the run
/// and its raw non-status stdout lines for the durable-log comparison.
#[track_caller]
fn ask_raw(setup: &Setup, prompt: &str) -> (Run, Vec<String>) {
    let mut command = setup.fiber(&["ask", prompt]);
    command.current_dir(setup.workspace());
    let output = run_to_exit(setup.deadline, "fiber ask", command);
    let stdout = String::from_utf8(output.stdout).unwrap();
    let raw: Vec<String> = stdout
        .lines()
        .filter(|l| !is_status(l))
        .map(str::to_owned)
        .collect();
    let lines = raw
        .iter()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    (
        Run {
            code: output.status.code(),
            lines,
            stderr: String::from_utf8(output.stderr).unwrap(),
        },
        raw,
    )
}

/// Stdout filtered to this session's durable lines is the session log,
/// byte for byte.
#[track_caller]
fn assert_durable_stdout(run: &Run, raw: &[String], setup: &Setup) {
    let durable: String = raw
        .iter()
        .zip(&run.lines)
        .filter(|(_, l)| l.get("seq").is_some() && l["session_id"] == run.session_id())
        .map(|(raw, _)| format!("{raw}\n"))
        .collect();
    assert_eq!(
        fs::read_to_string(support::session_dir(setup, run.session_id()).join("events.jsonl"))
            .unwrap(),
        durable
    );
}

/// The wire promise on one `picture.run` line: the session's identity,
/// exactly the keys `op` and `ms` with an unsigned `ms`, and none of the
/// image's bytes, a path, the prompt, the output text, a credential or a
/// null anywhere in the line. `ms` is never asserted exactly.
#[track_caller]
fn assert_picture_wire(line: &Value, process: &str, session_id: &str, extra: &[&str]) {
    assert_eq!(line["code"], "op_timing", "{line}");
    assert_eq!(line["level"], "debug", "{line}");
    assert_eq!(line["process"], process, "{line}");
    assert_eq!(line["session_id"], session_id, "{line}");
    assert_eq!(line["message"], "An operation ended.", "{line}");
    let data = line["data"].as_object().expect("data is an object");
    assert_eq!(data.keys().collect::<Vec<_>>(), vec!["ms", "op"], "{line}");
    assert_eq!(line["data"]["op"], "picture.run", "{line}");
    assert!(line["data"]["ms"].as_u64().is_some(), "{line}");
    let raw = line.to_string();
    let mut sentinels = vec![PIXEL_BASE64, "artifacts/", "sk-test", "null"];
    sentinels.extend(extra.iter().copied());
    for sentinel in sentinels {
        assert!(
            !raw.contains(sentinel),
            "no bytes, path, prompt, output, credential or null in {raw}"
        );
    }
}

#[test]
fn at_debug_an_image_read_writes_one_picture_run() {
    let setup = Setup::new();
    fs::write(setup.workspace().join("pic.png"), PIXEL).unwrap();
    let server = ProviderServer::start(image_read_script()).unwrap();
    setup.provider_with_images(&server);
    debug_home(&setup);

    let (run, raw) = ask_raw(&setup, "look at the picture");

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), read_kinds());
    assert_eq!(server.requests().len(), 2, "the read really ran");
    // The provider's image result: the second request forwards the bytes.
    assert!(
        String::from_utf8_lossy(&server.requests()[1].body).contains(PIXEL_BASE64),
        "the tool result carries the image"
    );
    let completed = run.first("tool_call_completed");
    assert_eq!(completed["payload"]["status"], "completed");
    let content = completed["payload"]["content"].as_array().unwrap();
    assert_eq!(content[0]["text"], "Image: 1x1 image/png.\n");
    assert_eq!(content[1]["type"], "image");
    let path = content[1]["path"].as_str().unwrap();
    assert!(
        path.starts_with("artifacts/i_") && path.ends_with(".png"),
        "{path}"
    );
    // The artifact is the processed file: here, the input byte for byte.
    let session = support::session_dir(&setup, run.session_id());
    assert_eq!(fs::read(session.join(path)).unwrap(), PIXEL);
    // The log names the file and never holds its bytes.
    let log = fs::read(session.join("events.jsonl")).unwrap();
    assert!(
        !log.windows(PIXEL.len()).any(|window| window == PIXEL),
        "the log holds the image's bytes"
    );
    assert_durable_stdout(&run, &raw, &setup);
    let lines = log_lines(&setup);
    let runs = picture_runs(&lines);
    assert_eq!(runs.len(), 1, "one scope per entered image: {runs:?}");
    assert_picture_wire(
        runs[0],
        "ask",
        run.session_id(),
        &["look at the", "Image: 1x1"],
    );
}

#[test]
fn at_default_an_image_read_writes_no_picture_run() {
    let setup = Setup::new();
    fs::write(setup.workspace().join("pic.png"), PIXEL).unwrap();
    let server = ProviderServer::start(image_read_script()).unwrap();
    setup.provider_with_images(&server);

    let run = ask(&setup, "look at the picture");

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), read_kinds());
    assert_eq!(server.requests().len(), 2, "the read really ran");
    let session = support::session_dir(&setup, run.session_id());
    let stored: Vec<PathBuf> = fs::read_dir(session.join("artifacts"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(stored.len(), 1, "the image is still processed");
    assert_eq!(fs::read(&stored[0]).unwrap(), PIXEL);
    let lines = log_lines(&setup);
    assert!(
        picture_runs(&lines).is_empty(),
        "below debug nothing is timed"
    );
    assert!(
        lines.iter().all(|line| line["level"] != "debug"),
        "the default stays silent"
    );
}

#[test]
fn at_info_an_image_read_writes_no_picture_run() {
    let setup = Setup::new();
    fs::write(setup.workspace().join("pic.png"), PIXEL).unwrap();
    let server = ProviderServer::start(image_read_script()).unwrap();
    setup.provider_with_images(&server);
    // After the provider install, which rewrites the home config: info
    // must still process the image while timing nothing.
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "diagnostics": {"level": "info"}}),
    );

    let run = ask(&setup, "look at the picture");

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), read_kinds());
    assert_eq!(server.requests().len(), 2, "the read really ran");
    let completed = run.first("tool_call_completed");
    assert_eq!(completed["payload"]["status"], "completed");
    let session = support::session_dir(&setup, run.session_id());
    let stored: Vec<PathBuf> = fs::read_dir(session.join("artifacts"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(stored.len(), 1, "the image is still processed");
    assert_eq!(fs::read(&stored[0]).unwrap(), PIXEL);
    let lines = log_lines(&setup);
    assert!(
        picture_runs(&lines).is_empty(),
        "at info a processed image times nothing"
    );
    assert!(
        lines.iter().all(|line| line["level"] != "debug"),
        "explicit info stays silent"
    );
}

#[test]
fn a_repository_debug_image_read_stays_silent_with_its_notice() {
    let setup = Setup::new();
    write_json(
        &setup.workspace().join(".fiber/config.json"),
        &json!({"diagnostics": {"level": "debug"}}),
    );
    fs::write(setup.workspace().join("pic.png"), PIXEL).unwrap();
    let server = ProviderServer::start(image_read_script()).unwrap();
    setup.provider_with_images(&server);

    let run = ask(&setup, "look at the picture");

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    // The notice rides along after the extensions load, before the
    // preamble; every other kind is the one successful read's.
    let mut expected = read_kinds();
    expected.insert(3, "notice");
    assert_eq!(run.kinds(), expected);
    assert!(
        run.all("notice")
            .iter()
            .any(|notice| notice["payload"]["code"] == "config_key_ignored"),
        "the repository value stays ignored: {:?}",
        run.kinds()
    );
    assert_eq!(server.requests().len(), 2, "the read really ran");
    let lines = log_lines(&setup);
    assert!(
        picture_runs(&lines).is_empty(),
        "an ignored level times nothing"
    );
}

#[test]
fn at_debug_an_unreadable_image_read_writes_one_picture_run() {
    let setup = Setup::new();
    // PNG magic with a truncated body: detected as an image, so the
    // child runs and refuses it.
    fs::write(setup.workspace().join("pic.png"), &PIXEL[..40]).unwrap();
    let server = ProviderServer::start(image_read_script()).unwrap();
    setup.provider_with_images(&server);
    debug_home(&setup);

    let run = ask(&setup, "look at the picture");

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), read_kinds());
    assert_eq!(server.requests().len(), 2, "the read really ran");
    let completed = run.first("tool_call_completed");
    assert_eq!(completed["payload"]["status"], "failed");
    assert_eq!(completed["payload"]["error"]["code"], "unsupported_file");
    let session = support::session_dir(&setup, run.session_id());
    assert!(
        fs::read_dir(session.join("artifacts"))
            .unwrap()
            .next()
            .is_none(),
        "a refused image stores nothing"
    );
    let lines = log_lines(&setup);
    let runs = picture_runs(&lines);
    assert_eq!(runs.len(), 1, "a refused read still entered: {runs:?}");
    assert_picture_wire(
        runs[0],
        "ask",
        run.session_id(),
        &["look at the", "cannot be read"],
    );
}

/// A pasted image prompt: text, then the base64 image.
fn image_prompt(id: &str, text: &str, data: &str) -> String {
    format!(
        "{{\"id\":\"{id}\",\"command\":\"prompt\",\"args\":{{\"content\":[{{\"type\":\"text\",\"text\":\"{text}\"}},{{\"type\":\"image\",\"data\":\"{data}\",\"mime_type\":\"image/png\"}}]}}}}"
    )
}

/// Debug home configuration keeping the installed model.
fn debug_home(setup: &Setup) {
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "diagnostics": {"level": "debug"}}),
    );
}

/// The home's diagnostic lines plus the session's durable events, parsed.
fn session_lines(setup: &Setup, id: &str) -> Vec<Value> {
    let mut lines = log_lines(setup);
    let events = fs::read_to_string(setup.session_dir(id).join("events.jsonl")).unwrap();
    for line in events.lines() {
        if line.is_empty() {
            continue;
        }
        lines.push(serde_json::from_str(line).unwrap());
    }
    lines
}

#[test]
fn at_debug_a_pasted_image_writes_one_picture_run() {
    let setup = Setup::new_with_fm_root();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider_with_images(&server);
    debug_home(&setup);
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);

    let client = running.connect(&setup.socket(&id));
    running.wait_for("extensions_loaded");
    send(
        &client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    recv(&client, "the subscribe acknowledgement");
    send(
        &client,
        &image_prompt("c_prompt", "describe this", PIXEL_BASE64),
    );
    let rest = until_session(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    let turn_started = rest
        .iter()
        .find(|line| line["kind"] == "turn_started")
        .expect("the prompt started a turn");
    let image = &turn_started["payload"]["input"][0]["content"][1];
    assert_eq!(image["type"], "image");
    let path = image["path"].as_str().unwrap();
    assert!(
        path.starts_with("artifacts/i_") && path.ends_with(".png"),
        "{path}"
    );
    let session = setup.session_dir(&id);
    assert_eq!(fs::read(session.join(path)).unwrap(), PIXEL);
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert!(
        String::from_utf8_lossy(&requests[0].body).contains(PIXEL_BASE64),
        "the request carries the pasted image: {body}"
    );

    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let _tail = until_session_close(&client);
    drop(client);
    let (status, _out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");

    let lines = session_lines(&setup, &id);
    let runs = picture_runs(&lines);
    assert_eq!(runs.len(), 1, "one scope per pasted image: {runs:?}");
    assert_picture_wire(runs[0], "session", &id, &["describe this"]);
    let kinds: Vec<String> = fs::read_to_string(session.join("events.jsonl"))
        .unwrap()
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let event: Value = serde_json::from_str(line).unwrap();
            event["kind"].as_str().unwrap().to_owned()
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
        .map(str::to_owned)
        .to_vec(),
        "the paste ran a full turn"
    );
}

#[test]
fn at_debug_a_decoded_unreadable_paste_enters_process_once() {
    let setup = Setup::new_with_fm_root();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider_with_images(&server);
    debug_home(&setup);
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);

    let client = running.connect(&setup.socket(&id));
    running.wait_for("extensions_loaded");
    send(
        &client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    recv(&client, "the subscribe acknowledgement");
    // Valid base64, but not an image: the paste enters the child and fails.
    send(
        &client,
        r#"{"id":"c_1","command":"prompt","args":{"content":[{"type":"image","data":"bm90IGFuIGltYWdl","mime_type":"image/png"}]}}"#,
    );
    let refused = answer(&client, "c_1");
    assert_eq!(refused["kind"], "command_rejected", "{refused}");
    assert_eq!(refused["payload"]["code"], "invalid_arguments", "{refused}");
    assert!(
        refused["payload"]["message"]
            .as_str()
            .unwrap()
            .starts_with("Image 1 cannot be read: "),
        "{refused}"
    );
    assert!(server.requests().is_empty(), "no turn ran for the refusal");
    // A text turn after the refusal: the session persists events and
    // answers, proving the run below scans logs the session really wrote.
    send(
        &client,
        r#"{"id":"c_2","command":"prompt","args":{"content":[{"type":"text","text":"hi"}]}}"#,
    );
    until_session(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    assert_eq!(server.requests().len(), 1, "the text turn really ran");

    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let _tail = until_session_close(&client);
    drop(client);
    let (status, _out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");

    let lines = session_lines(&setup, &id);
    let runs = picture_runs(&lines);
    assert_eq!(runs.len(), 1, "decoded bytes enter one scope: {runs:?}");
    assert_picture_wire(runs[0], "session", &id, &["not an image"]);
}

/// One read-only offline MCP tool, so its calls take the permission fast path.
fn echo_tool() -> Value {
    json!({
        "name": "echo",
        "description": "Echoes.",
        "inputSchema": {"type": "object"},
        "annotations": {"readOnlyHint": true, "openWorldHint": false},
    })
}

/// A stdio fixture server whose `echo` call answers with `result`.
fn mcp_fixture(setup: &Setup, result: &str) -> PathBuf {
    let dir = setup.root.path().join("fx");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("tools.json"), json!([echo_tool()]).to_string()).unwrap();
    fs::write(dir.join("call-echo.json"), result).unwrap();
    dir
}

/// Home configuration with the fixture server `fx` and the model, at
/// debug when asked.
fn mcp_configure(setup: &Setup, dir: &Path, debug: bool) {
    let mut config = json!({
        "model": "fake/m",
        "mcp": {"servers": {"fx": {
            "command": "/bin/bash",
            "args": [fakes::mcp_fixture().display().to_string(), dir.display().to_string()],
            "startup_timeout_ms": 600000,
        }}},
    });
    if debug {
        config["diagnostics"] = json!({"level": "debug"});
    }
    write_json(&setup.home().join("config.json"), &config);
}

#[test]
fn at_debug_an_mcp_image_result_writes_one_picture_run() {
    let setup = Setup::new_with_mcp_fixture();
    let dir = mcp_fixture(
        &setup,
        &format!(
            "{{\"content\":[{{\"type\":\"image\",\"data\":\"{PIXEL_BASE64}\",\"mimeType\":\"image/png\"}}]}}"
        ),
    );
    let server = ProviderServer::start([
        stream(&[function_call(
            "call_echo",
            "mcp__fx__echo",
            &json!({"text": "hi"}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider_with_images(&server);
    mcp_configure(&setup, &dir, true);

    let (run, raw) = ask_raw(&setup, "echo hi");

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), read_kinds());
    let requests = server.requests();
    assert_eq!(requests.len(), 2, "the tool call lands and the turn ends");
    // The provider's image result: the second request forwards the bytes.
    assert!(
        String::from_utf8_lossy(&requests[1].body).contains(PIXEL_BASE64),
        "the tool result carries the image"
    );
    let completed = run.first("tool_call_completed");
    assert_eq!(completed["payload"]["status"], "completed");
    let content = completed["payload"]["content"].as_array().unwrap();
    assert_eq!(content[0]["text"], "Image: 1x1 image/png.\n");
    assert_eq!(content[1]["type"], "image");
    let path = content[1]["path"].as_str().unwrap();
    assert!(
        path.starts_with("artifacts/i_") && path.ends_with(".png"),
        "{path}"
    );
    let session = support::session_dir(&setup, run.session_id());
    assert_eq!(fs::read(session.join(path)).unwrap(), PIXEL);
    assert_durable_stdout(&run, &raw, &setup);
    let lines = log_lines(&setup);
    let runs = picture_runs(&lines);
    assert_eq!(runs.len(), 1, "one scope per tool image: {runs:?}");
    assert_picture_wire(runs[0], "ask", run.session_id(), &["echo hi", "Image: 1x1"]);
}
