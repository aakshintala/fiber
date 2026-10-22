//! A package's manifest-named binary through the public API
//! (`docs/extensions.md`, "What a package holds" and "Host calls"): the
//! platform binary installs as `bin/<last segment of the manifest name>`,
//! and a `/`-relative `host.exec` program resolves inside the package.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use common::{Setup, install_lua};
use contract::shapes::ContentPart;
use contract::tool::{Output, Tool};
use extensions::{ExecEnd, ExecEntry, ExecReply, HostScript, LuaExtension, LuaTool};
use fakes::Deadline;
use fakes::clock::FakeClock;
use fakes::{CancelToken, Recorder};
use serde_json::{Map, Value, json};

/// How long a test waits for a signal or an answer before failing.
const WAIT: Duration = Duration::from_secs(10);

/// An extension named `fiber.test/<short>` with the entry script `init`.
fn extension(dir: &Path, short: &str, init: &str, clock: Arc<FakeClock>) -> Arc<LuaExtension> {
    std::fs::write(dir.join("init.lua"), init).unwrap();
    Arc::new(LuaExtension::new(
        format!("fiber.test/{short}"),
        dir,
        dir.join("home"),
        clock,
    ))
}

// debt: duplicate test helpers with lua_tools.rs and git_install.rs (they differ: wait 10 s vs 5 s, failed().0 use), upgrade when a fourth test file needs them.
/// The extension's tools, read on a thread under `WAIT`.
#[track_caller]
fn tools(ext: &Arc<LuaExtension>) -> Vec<LuaTool> {
    let (tx, rx) = mpsc::channel();
    let ext = Arc::clone(ext);
    std::thread::spawn(move || tx.send(ext.tools()));
    Deadline::after(WAIT)
        .recv(&rx)
        .expect("waited for the tools")
        .expect("the entry script ran")
}

/// The extension's tools by name, each shareable with a calling thread.
#[track_caller]
fn by_name(ext: &Arc<LuaExtension>) -> BTreeMap<String, Arc<LuaTool>> {
    tools(ext)
        .into_iter()
        .map(|tool| (tool.definition().name, Arc::new(tool)))
        .collect()
}

fn args(value: Value) -> Map<String, Value> {
    let Value::Object(map) = value else {
        panic!("arguments are an object, not {value}");
    };
    map
}

/// Runs `tool` on `arguments` on its own thread.
fn run(tool: &Arc<LuaTool>, arguments: Value) -> mpsc::Receiver<Output> {
    let (tx, rx) = mpsc::channel();
    let tool = Arc::clone(tool);
    std::thread::spawn(move || {
        let output = tool.run(&args(arguments), &CancelToken::new(), &Recorder::default());
        match tx.send(output) {
            Ok(()) | Err(mpsc::SendError(_)) => {}
        }
    });
    rx
}

/// The call's result, under `WAIT`.
#[track_caller]
fn ran(rx: &mpsc::Receiver<Output>, wait: &Deadline) -> Output {
    wait.recv(rx).expect("waited for the tool call")
}

/// The result's text parts, joined.
fn text(output: &Output) -> String {
    output
        .content
        .iter()
        .map(|part| {
            let ContentPart::Text { text } = part else {
                panic!("a non-text part {part:?}");
            };
            text.clone()
        })
        .collect()
}

/// The failure's message.
fn failed(output: &Output) -> String {
    output.error.clone().expect("the call failed").message
}

/// Serves `body` once at `/tool-1.0`, like the binary download expects.
fn serve_once(body: &'static [u8]) -> fakes::ProviderServer {
    fakes::ProviderServer::start([fakes::Response::status(200, body.to_vec())]).unwrap()
}

fn sha256(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

const INIT: &str = r#"
fiber.tool("run_it", {
  description = "d", input_schema = { type = "object" },
  effects = { effects = { "executes" }, reversible = false }, timeout = 10000,
  run = function() return host.exec("bin/mybin", {}).stdout end,
})
fiber.tool("escape", {
  description = "d", input_schema = { type = "object" },
  effects = { effects = { "executes" }, reversible = false }, timeout = 10000,
  run = function() return host.exec("../mybin", {}).stdout end,
})
fiber.tool("bare", {
  description = "d", input_schema = { type = "object" },
  effects = { effects = { "executes" }, reversible = false }, timeout = 10000,
  run = function() return host.exec("sh", { "-c", "echo path" }).stdout end,
})
"#;

/// Installs `fiber.test/mybin` into `setup` with a real `binaries` entry for
/// this platform, serving `body`, and returns the server and installed
/// directory.
fn install_into(setup: &Setup, body: &'static [u8]) -> (fakes::ProviderServer, std::path::PathBuf) {
    let server = serve_once(body);
    let url = format!("{}/tool-1.0", server.url());
    let dir = install_lua(
        setup,
        "mybin",
        &json!({"binaries": { extensions::platform(): { "url": url, "sha256": sha256(body) } }}),
        INIT,
    );
    assert!(
        server.await_requests(1, Deadline::after(WAIT)),
        "waited for the binary to be downloaded"
    );
    (server, dir)
}

/// Installs `fiber.test/mybin` with `body` and returns its installed directory.
fn installed(body: &'static [u8]) -> (Setup, fakes::ProviderServer, std::path::PathBuf) {
    let setup = Setup::new();
    let (server, dir) = install_into(&setup, body);
    (setup, server, dir)
}

/// What a `ran` timeout leaves to read: the installed binary, the call's
/// callback output (none arrives before the deadline), and the stages the
/// script's marker file records.
fn timeout_report(dir: &Path, marker: &Path) -> String {
    use std::os::unix::fs::PermissionsExt;
    let binary = dir.join("bin").join("mybin");
    let mode = match std::fs::metadata(&binary) {
        Ok(meta) => format!("{:o}", meta.permissions().mode() & 0o7777),
        Err(e) => format!("<{e}>"),
    };
    let stages = match std::fs::read_to_string(marker) {
        Ok(text) => text,
        Err(e) => format!("<{e}>"),
    };
    let started = stages.lines().any(|line| line == "started");
    let eof = stages.lines().any(|line| line == "eof");
    format!(
        "tool=run_it program=bin/mybin\n\
         binary={} exists={} mode={mode}\n\
         callback output: none, no result arrived before the deadline\n\
         marker={} started={started} eof={eof} raw={stages:?}",
        binary.display(),
        binary.exists(),
        marker.display(),
    )
}

#[test]
fn the_manifest_named_binary_runs_through_host_exec() {
    let setup = Setup::new();
    let marker = setup.root().join("marker");
    let script: &'static str = format!(
        "#!/bin/sh\necho started >> '{}'\necho from-binary\necho eof >> '{}'\n",
        marker.display(),
        marker.display(),
    )
    .leak();
    let (_server, dir) = install_into(&setup, script.as_bytes());
    let ext = extension(&dir, "mybin", INIT, FakeClock::new());
    let tools = by_name(&ext);
    let rx = run(&tools["run_it"], json!({}));
    let output = match Deadline::after(WAIT).recv(&rx) {
        Ok(output) => output,
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("{}", timeout_report(&dir, &marker)),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("the call's thread ended without a result")
        }
    };
    assert_eq!(text(&output), "from-binary\n");
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap(),
        "started\neof\n",
        "the script reached its first and last lines"
    );
}

#[test]
fn a_dot_dot_program_is_refused_naming_the_path() {
    let (_setup, _server, dir) = installed(b"#!/bin/sh\necho from-binary\n");
    let ext = extension(&dir, "mybin", INIT, FakeClock::new());
    let tools = by_name(&ext);
    let message = failed(&ran(
        &run(&tools["escape"], json!({})),
        &Deadline::after(WAIT),
    ));
    assert!(
        message.contains("../mybin"),
        "the refusal names the path: {message}"
    );
    assert!(
        message.contains("leaves the extension directory"),
        "the refusal is the resolver's, not the spawn's: {message}"
    );
}

#[test]
fn a_bare_name_still_resolves_on_path() {
    let (_setup, _server, dir) = installed(b"#!/bin/sh\necho from-binary\n");
    let ext = extension(&dir, "mybin", INIT, FakeClock::new());
    let tools = by_name(&ext);
    assert_eq!(
        text(&ran(
            &run(&tools["bare"], json!({})),
            &Deadline::after(WAIT)
        )),
        "path\n"
    );
}

#[test]
fn a_scripted_case_sees_the_program_as_written() {
    let (_setup, _server, dir) = installed(b"#!/bin/sh\necho from-binary\n");
    let script = HostScript::new(
        Vec::new(),
        vec![ExecEntry {
            request: json!({ "program": "bin/mybin", "args": [] }),
            reply: Ok(ExecReply::Completed {
                end: ExecEnd::Code(0),
                stdout: "scripted\n".to_owned(),
                stderr: String::new(),
            }),
        }],
        Vec::new(),
    );
    let ext = Arc::new(
        LuaExtension::new("fiber.test/mybin", &dir, dir.join("home"), FakeClock::new())
            .with_host_script(script.clone()),
    );
    let tools = by_name(&ext);
    assert_eq!(
        text(&ran(
            &run(&tools["run_it"], json!({})),
            &Deadline::after(WAIT)
        )),
        "scripted\n"
    );
    assert!(script.unmet().is_empty(), "{:?}", script.unmet());
}

#[test]
fn an_escape_fails_even_under_a_host_script() {
    let (_setup, _server, dir) = installed(b"#!/bin/sh\necho from-binary\n");
    let script = HostScript::new(Vec::new(), Vec::new(), Vec::new());
    let ext = Arc::new(
        LuaExtension::new("fiber.test/mybin", &dir, dir.join("home"), FakeClock::new())
            .with_host_script(script),
    );
    let tools = by_name(&ext);
    let message = failed(&ran(
        &run(&tools["escape"], json!({})),
        &Deadline::after(WAIT),
    ));
    assert!(
        message.contains("../mybin"),
        "the refusal names the path: {message}"
    );
    assert!(
        message.contains("leaves the extension directory"),
        "the refusal is the resolver's, not the spawn's: {message}"
    );
}
