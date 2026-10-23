//! Binary-level tests of a lazy extension (`docs/extensions.md`, "Commands
//! and screens"): its manifest's `commands` are listed without starting its
//! VM, the first run starts it before `command_accepted`, and an
//! undeclared registration fails it with `extension_failed`.

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
use std::path::Path;
use std::sync::mpsc;
use std::thread;

use extension_harness::*;
use fakes::ProviderServer;
use serde_json::{Value, json};
use support::Setup;

/// Lua that adds one `x` to the counter file `path`.
fn bump(path: &Path) -> String {
    let path = serde_json::to_string(&path.to_string_lossy()).unwrap();
    format!(
        "do local ok, old = pcall(host.fs.read, {path}); host.fs.write({path}, (ok and old or '') .. 'x') end\n"
    )
}

/// How many times the script that bumps `path` ran.
fn count(path: &Path) -> usize {
    fs::read_to_string(path).map_or(0, |text| text.len())
}

const EXPORT: &str = "fiber.command('export', { timeout = 5000, description = 'from lua', run = function(text) host.status('ran ' .. text) end })\n";
const TOOL: &str = "fiber.tool('t', { description = 'd', input_schema = { type = 'object', properties = {} }, effects = { effects = { 'reads' }, paths = { 'note.txt' }, reversible = true }, timeout = 1000, run = function() return 'x' end })\n";

fn declared() -> Value {
    json!({ "commands": { "export": {
        "description": "Write the transcript as Markdown", "argument_hint": "[path]"
    } } })
}

/// A started session with a subscribed client.
struct Session {
    setup: Setup,
    running: Running,
    client: Socket,
    server: ProviderServer,
}

impl Session {
    fn start(setup: Setup, server: ProviderServer) -> Self {
        let id = doors::mint("s_");
        let mut running = setup.start_session(&id, &[]);
        let client = running.connect(&setup.socket(&id));
        running.wait_for("extensions_loaded");
        send(
            &client,
            r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
        );
        Self {
            setup,
            running,
            client,
            server,
        }
    }

    /// Sends `command` and returns the lines up to its answer.
    #[track_caller]
    fn ask(&self, id: &str, command: &str, args: &Value) -> Vec<Value> {
        let line = json!({ "id": id, "command": command, "args": args });
        send(&self.client, &line.to_string());
        until_session(&self.client, id, |line| {
            (line["kind"] == "command_accepted" || line["kind"] == "command_rejected")
                && line["payload"]["command_id"] == id
        })
    }

    /// The `commands` answer's entry for the extension tagged `tag`.
    #[track_caller]
    fn listed(&self, tag: &str) -> Vec<Value> {
        let lines = self.ask("c_list", "commands", &json!({}));
        let result = lines.last().unwrap()["payload"]["result"].clone();
        result["commands"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["tag"] == tag)
            .cloned()
            .collect()
    }

    #[track_caller]
    fn run(&self, id: &str, name: &str, text: &str) -> Value {
        self.ask(id, "command", &json!({ "name": name, "text": text }))
            .pop()
            .unwrap()
    }

    /// Closes the session and checks the process exits cleanly.
    #[track_caller]
    fn finish(self) {
        send(&self.client, r#"{"id":"c_close","command":"close"}"#);
        let _tail = until_session_close(&self.client);
        drop(self.client);
        let (status, _out, stderr) = self.running.wait();
        assert!(status.success(), "stderr: {stderr}");
        assert!(self.server.requests().is_empty());
    }
}

/// A setup holding `fiber.test/lazy`, whose entry script `init` builds from
/// the setup, with `manifest` merged into its `extension.json`.
fn lazy_setup(init: impl FnOnce(&Setup) -> String, manifest: Value) -> (Setup, ProviderServer) {
    let setup = Setup::new_with_fm_root();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    let init = init(&setup);
    setup.lua_with("lazy", &init, manifest);
    (setup, server)
}

#[test]
fn a_declared_command_is_listed_with_no_vm_and_starts_on_its_first_run() {
    let (setup, server) = lazy_setup(
        |setup| format!("{}{EXPORT}", bump(&setup.workspace().join("started"))),
        declared(),
    );
    let started = setup.workspace().join("started");
    let session = Session::start(setup, server);
    assert_eq!(
        session.listed("fiber.test/lazy"),
        [json!({
            "name": "export", "description": "Write the transcript as Markdown",
            "argument_hint": "[path]", "tag": "fiber.test/lazy", "extension": true
        })]
    );
    // docs/extensions.md:1356-57: listing starts no VM.
    assert_eq!(count(&started), 0, "listing started the VM");
    // docs/extensions.md:1375-79: the first run starts the extension.
    let accepted = session.run("c_run", "export", "a.md");
    assert_eq!(accepted["kind"], "command_accepted", "{accepted}");
    assert_eq!(count(&started), 1, "the run did not start the extension");
    let ui = until_session(&session.client, "extension_ui", |line| {
        line["kind"] == "extension_ui" && line["payload"]["extension"] == "fiber.test/lazy"
    });
    assert_eq!(ui.last().unwrap()["payload"]["status"], "ran a.md");
    // docs/extensions.md:1381-83: one `init.lua` run.
    assert_eq!(count(&started), 1);
    session.finish();
}

#[test]
fn the_first_run_is_accepted_only_after_the_start() {
    let (setup, server) = lazy_setup(
        |setup| {
            let fifo = setup.workspace().join("hold.fifo");
            fakes::fifo(&fifo);
            let fifo = serde_json::to_string(&fifo.to_string_lossy()).unwrap();
            // The status comes last, so it is in the stream only once the
            // whole entry script has run.
            format!("host.fs.read({fifo})\n{EXPORT}host.status('init done')\n")
        },
        declared(),
    );
    let fifo = setup.workspace().join("hold.fifo");
    let deadline = setup.deadline;
    let session = Session::start(setup, server);
    // The entry script blocks reading the fifo until `release` fires; the
    // writer opening it is the proof that the start is under way.
    let (held, opened) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let holder = thread::spawn(move || {
        let writer = fs::OpenOptions::new().write(true).open(fifo).unwrap();
        held.send(()).unwrap();
        // A dropped `release` (the test failed) frees the reader too.
        released.recv().unwrap_or(());
        drop(writer);
    });
    send(
        &session.client,
        r#"{"id":"c_run","command":"command","args":{"name":"export","text":"a.md"}}"#,
    );
    deadline.recv_or_fail(&opened, "the entry script to open the fifo");
    release.send(()).unwrap();
    let lines = until_session(&session.client, "c_run", |line| {
        line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_run"
    });
    // docs/extensions.md:1375-79: the start comes first. The entry script
    // cannot finish before the release, so `command_accepted` before its
    // closing status would mean the command was answered while it was held.
    assert!(
        lines
            .iter()
            .any(|line| line["kind"] == "extension_ui" && line["payload"]["status"] == "init done"),
        "{lines:?}"
    );
    holder.join().unwrap();
    session.finish();
}

#[test]
fn an_undeclared_registration_fails_with_extension_failed() {
    let (setup, server) = lazy_setup(
        |setup| format!("{}{EXPORT}{TOOL}", bump(&setup.workspace().join("started"))),
        declared(),
    );
    let started = setup.workspace().join("started");
    let session = Session::start(setup, server);
    for id in ["c_run1", "c_run2"] {
        let rejected = session.run(id, "export", "a.md");
        assert_eq!(rejected["kind"], "command_rejected", "{rejected}");
        assert_eq!(rejected["payload"]["code"], "extension_failed");
        let message = rejected["payload"]["message"].as_str().unwrap();
        assert!(
            message.starts_with("Command `export` did not run: "),
            "{message}"
        );
        assert!(message.contains("the tool `t`"), "{message}");
    }
    // docs/extensions.md:1377-80: a failed start stays failed for the session.
    assert_eq!(count(&started), 1);
    session.finish();
}

#[test]
fn after_a_failed_start_the_extension_stays_failed_until_reload_starts_it_again() {
    let (setup, server) = lazy_setup(
        |setup| format!("{}{EXPORT}{TOOL}", bump(&setup.workspace().join("started"))),
        declared(),
    );
    let started = setup.workspace().join("started");
    let fixed = setup.workspace().join("fixed");
    let session = Session::start(setup, server);
    let rejected = session.run("c_run1", "export", "a.md");
    assert_eq!(
        rejected["payload"]["code"], "extension_failed",
        "{rejected}"
    );
    fs::write(
        session
            .setup
            .home()
            .join("extensions/fiber.test-lazy/init.lua"),
        format!("{}{EXPORT}", bump(&fixed)),
    )
    .unwrap();
    let still = session.run("c_run2", "export", "a.md");
    assert_eq!(still["payload"]["code"], "extension_failed", "{still}");
    let reloaded = session.ask("c_reload", "reload", &json!({}));
    assert_eq!(reloaded.last().unwrap()["kind"], "command_accepted");
    // docs/extensions.md:1379 and 1520-22: the reload starts it again.
    assert_eq!(
        session.listed("fiber.test/lazy").len(),
        1,
        "the reloaded session lists the command"
    );
    assert_eq!(count(&fixed), 1, "the reload did not start the extension");
    // docs/extensions.md:1381-83: the run shares that start.
    let accepted = session.run("c_run3", "export", "b.md");
    assert_eq!(accepted["kind"], "command_accepted", "{accepted}");
    assert_eq!(count(&fixed), 1);
    assert_eq!(count(&started), 1);
    session.finish();
}

#[test]
fn an_empty_commands_key_is_lazy_and_never_starts() {
    let (setup, server) = lazy_setup(
        |setup| format!("{}{EXPORT}", bump(&setup.workspace().join("started"))),
        json!({ "commands": {} }),
    );
    let started = setup.workspace().join("started");
    let session = Session::start(setup, server);
    assert!(session.listed("fiber.test/lazy").is_empty());
    // docs/extensions.md:70-71: a `commands` key, `{}` included, is lazy.
    assert_eq!(count(&started), 0);
    session.finish();
}

#[test]
fn an_extension_without_a_commands_key_still_starts_at_session_start() {
    let setup = Setup::new_with_fm_root();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    let started = setup.workspace().join("started");
    setup.lua("plain", &format!("{}{EXPORT}", bump(&started)));
    let session = Session::start(setup, server);
    // docs/extensions.md:313-16: an extension without `commands` runs at session start.
    assert_eq!(count(&started), 1);
    assert_eq!(session.listed("fiber.test/plain").len(), 1);
    session.finish();
}
