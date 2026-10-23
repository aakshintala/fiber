//! Binary-level tests of the first-party `export` extension
//! (`docs/extensions.md`, "Commands and screens"): the built `fiber` runs
//! in its own process group with its own `FIBER_HOME`, a fake provider,
//! and the real `extensions/export` package installed from the repository.

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
use std::path::PathBuf;
use std::process::ExitStatus;

use extension_harness::*;
use fakes::ProviderServer;
use serde_json::{Value, json};
use support::{Setup, function_call, stream, text_reply};

/// The first-party package under test.
const EXPORT_NAME: &str = "github.com/aakshintala/fiber/extensions/export";

impl Setup {
    /// Installs the real `export` package from the repository.
    fn install_export(&self) {
        extensions::plan(
            &self.home(),
            &extensions::Request::Path(
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../extensions/export"),
            ),
            "0.0.0",
            &extensions::Origin::github(),
            &*fakes::clock::FakeClock::new(),
        )
        .unwrap()
        .commit()
        .unwrap();
    }
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
        Self::start_with_id(setup, server, &id)
    }

    fn start_with_id(setup: Setup, server: ProviderServer, id: &str) -> Self {
        let mut running = setup.start_session(id, &[]);
        let client = running.connect(&setup.socket(id));
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

    /// Sends a `prompt` and returns the lines up to its turn's end.
    #[track_caller]
    fn prompt(&self, id: &str, text: &str) -> Vec<Value> {
        send(
            &self.client,
            &json!({ "id": id, "command": "prompt",
                "args": { "content": [{ "type": "text", "text": text }] } })
            .to_string(),
        );
        until_session(&self.client, id, |line| line["kind"] == "turn_completed")
    }

    #[track_caller]
    fn run(&self, id: &str, name: &str, text: &str) -> Value {
        self.ask(id, "command", &json!({ "name": name, "text": text }))
            .pop()
            .unwrap()
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

    /// Closes the session and checks the process exits cleanly.
    #[track_caller]
    fn finish(self) -> (ExitStatus, Vec<Value>, String) {
        send(&self.client, r#"{"id":"c_close","command":"close"}"#);
        let _tail = until_session_close(&self.client);
        drop(self.client);
        let (status, out, stderr) = self.running.wait();
        assert!(status.success(), "stderr: {stderr}");
        // The server outlives the session; reading its log keeps the
        // field that holds it alive until here.
        let _ = self.server.requests();
        (status, out, stderr)
    }
}

/// The export extension's status line after a run, waited for on the
/// session's stream.
#[track_caller]
fn export_status(session: &Session) -> String {
    let seen = until_session(&session.client, "the export status", |line| {
        line["kind"] == "extension_ui" && line["payload"]["extension"] == EXPORT_NAME
    });
    seen.last().unwrap()["payload"]["status"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn export_is_listed_with_no_vm_and_writes_the_transcript_to_the_default_path() {
    let setup = Setup::new_with_fm_root();
    let server = ProviderServer::start([
        stream(&[function_call(
            "call_read_1",
            "read",
            &json!({"path": "note.txt"}),
        )]),
        text_reply("Done."),
    ])
    .unwrap();
    setup.provider(&server);
    setup.install_export();
    fs::write(
        setup.workspace().join("note.txt"),
        "line 1\nline 2\nline 3\nline 4\nline 5\n",
    )
    .unwrap();
    let id = doors::mint("s_");
    let session = Session::start_with_id(setup, server, &id);
    assert_eq!(
        session.listed(EXPORT_NAME),
        [json!({
            "name": "export", "description": "Write the transcript as Markdown",
            "argument_hint": "[path]", "tag": EXPORT_NAME, "extension": true
        })]
    );
    session.prompt("c_prompt", "fix it");
    let accepted = session.ask("c_export", "command", &json!({ "name": "export" }));
    assert_eq!(accepted.last().unwrap()["kind"], "command_accepted");
    assert_eq!(export_status(&session), format!("wrote {id}.md"));
    let written = fs::read_to_string(session.setup.workspace().join(format!("{id}.md"))).unwrap();
    // docs/testing.md:74-75: the test asserts the file left behind. Its order
    // is the log's: `history` answers durable lines in `seq` order
    // (docs/events.md:1367), and the extension renders each page in order.
    assert_eq!(
        written,
        format!(
            "# Session {id}\n\n> fix it\n\n- `read` {{\"path\":\"note.txt\"}}\n    line 1\n    line 2\n    line 3\n\nDone.\n\n"
        )
    );
    session.finish();
}

#[test]
fn an_explicit_path_is_written_and_a_second_run_is_refused() {
    let setup = Setup::new_with_fm_root();
    let server = ProviderServer::start([text_reply("Hello.")]).unwrap();
    setup.provider(&server);
    setup.install_export();
    let session = Session::start(setup, server);
    session.prompt("c_prompt", "hi");
    fs::create_dir_all(session.setup.workspace().join("out")).unwrap();
    let accepted = session.run("c_export1", "export", "out/a.md");
    assert_eq!(accepted["kind"], "command_accepted", "{accepted}");
    assert_eq!(export_status(&session), "wrote out/a.md");
    let before = fs::read(session.setup.workspace().join("out/a.md")).unwrap();
    let accepted = session.run("c_export2", "export", "out/a.md");
    assert_eq!(accepted["kind"], "command_accepted", "{accepted}");
    let refused = until_session(&session.client, "the refusal notice", |line| {
        line["kind"] == "notice"
            && line["payload"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("already exists"))
    });
    let notice = refused.last().unwrap();
    let message = notice["payload"]["message"].as_str().unwrap();
    assert!(message.contains("out/a.md"), "{message}");
    assert_eq!(notice["payload"]["extension"], EXPORT_NAME);
    let after = fs::read(session.setup.workspace().join("out/a.md")).unwrap();
    assert_eq!(before, after);
    session.finish();
}

#[test]
fn a_missing_parent_directory_surfaces_the_host_error() {
    let setup = Setup::new_with_fm_root();
    let server = ProviderServer::start([text_reply("Hello.")]).unwrap();
    setup.provider(&server);
    setup.install_export();
    let session = Session::start(setup, server);
    session.prompt("c_prompt", "hi");
    let accepted = session.run("c_export", "export", "nodir/a.md");
    assert_eq!(accepted["kind"], "command_accepted", "{accepted}");
    let failed = until_session(&session.client, "the failure notice", |line| {
        line["kind"] == "notice" && line["payload"]["extension"] == EXPORT_NAME
    });
    let notice = failed.last().unwrap();
    let message = notice["payload"]["message"].as_str().unwrap();
    assert!(message.contains("nodir/a.md"), "{message}");
    assert!(
        !session.setup.workspace().join("nodir/a.md").exists(),
        "no file was written"
    );
    session.finish();
}

#[test]
fn a_transcript_longer_than_one_page_is_paged_whole() {
    let setup = Setup::new_with_fm_root();
    let server =
        ProviderServer::start((0..150).map(|_| text_reply("ok")).collect::<Vec<_>>()).unwrap();
    setup.provider(&server);
    setup.install_export();
    let id = doors::mint("s_");
    let session = Session::start_with_id(setup, server, &id);
    let mut asked = Vec::new();
    let mut highest = 0;
    for i in 0..150 {
        let text = format!("page-prompt-{i:03}");
        let lines = session.prompt(&format!("c_prompt_{i}"), &text);
        asked.push(text);
        for line in &lines {
            if let Some(seq) = line["seq"].as_u64() {
                highest = highest.max(seq);
            }
        }
        if highest > 300 {
            break;
        }
    }
    // A setup guard, not a doc count: past seq 300 with 256-line pages
    // (docs/invocation.md:452) the transcript spans more than one page.
    assert!(highest > 300, "the log never passed seq 300");
    let accepted = session.ask("c_export", "command", &json!({ "name": "export" }));
    assert_eq!(accepted.last().unwrap()["kind"], "command_accepted");
    assert_eq!(export_status(&session), format!("wrote {id}.md"));
    let written = fs::read_to_string(session.setup.workspace().join(format!("{id}.md"))).unwrap();
    let quotes: Vec<&str> = written
        .lines()
        .filter(|line| line.starts_with("> page-prompt-"))
        .collect();
    let expected: Vec<String> = asked.iter().map(|text| format!("> {text}")).collect();
    // docs/events.md:360 and :1367: each prompt enters one `turn_started`
    // `input` in arrival order, and `history` returns those lines in `seq`
    // order, so the quotes run in prompt order across pages.
    assert_eq!(
        quotes,
        expected.iter().map(String::as_str).collect::<Vec<_>>()
    );
    // Every rendered line is a prompt quote or its one-word reply, so the
    // whole file is fixed: no rendered line may go missing or repeat.
    let mut whole = format!("# Session {id}\n\n");
    for text in &asked {
        whole.push_str(&format!("> {text}\n\nok\n\n"));
    }
    assert_eq!(written, whole);
    session.finish();
}

#[test]
fn steering_and_a_failed_call_are_written() {
    let setup = Setup::new_with_fm_root();
    let server = ProviderServer::start([
        stream(&[function_call(
            "call_shell",
            "shell",
            &json!({"command": "echo hi"}),
        )]),
        text_reply("after the steer"),
        stream(&[function_call(
            "call_missing",
            "read",
            &json!({"path": "missing.txt"}),
        )]),
        text_reply("second done"),
        stream(&[function_call(
            "call_odd",
            "read",
            &json!({"a": null, "b": [], "c": {}}),
        )]),
        text_reply("third done"),
        text_reply("fourth done"),
    ])
    .unwrap();
    setup.provider(&server);
    setup.install_export();
    // The first turn waits on an approval, so the steer lands mid-turn.
    fs::write(
        setup.home().join("rules"),
        "{\"decision\":\"ask\",\"tool\":\"shell\",\"prefix\":\"echo hi\"}\n",
    )
    .unwrap();
    // A second extension whose `prompt` drive sends a message the person
    // did not type: its turn's source is `extension`, never `driver`.
    setup.lua(
        "nudge",
        "fiber.command(\"nudge\", { timeout = 8000, description = \"Nudge.\", run = function() host.drive(\"prompt\", { content = {{ type = \"text\", text = \"extension said\" }} }) end })\n",
    );
    let id = doors::mint("s_");
    let session = Session::start_with_id(setup, server, &id);
    send(
        &session.client,
        r#"{"id":"c_prompt1","command":"prompt","args":{"content":[{"type":"text","text":"run it"}]}}"#,
    );
    let asked = until_session(&session.client, "permission_requested", |line| {
        line["kind"] == "permission_requested"
    });
    let request_id = asked.last().unwrap()["payload"]["request_id"]
        .as_str()
        .unwrap()
        .to_owned();
    send(
        &session.client,
        r#"{"id":"c_steer","command":"steer","args":{"content":[{"type":"text","text":"use the other file"}]}}"#,
    );
    let queued = until_session(&session.client, "the steer queued", |line| {
        line["kind"] == "steering_queue"
            && line["payload"]["messages"]
                .as_array()
                .is_some_and(|messages| !messages.is_empty())
    });
    // The acceptance may land after the queue line, so a second wait
    // covers it; when it already arrived, there is nothing to wait for.
    if !queued.iter().any(|line| {
        line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_steer"
    }) {
        until_session(&session.client, "the steer admitted", |line| {
            line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_steer"
        });
    }
    send(
        &session.client,
        &format!(
            r#"{{"id":"c_reply","command":"reply","args":{{"request_id":"{request_id}","decision":"allow"}}}}"#
        ),
    );
    until_session(&session.client, "steering_applied", |line| {
        line["kind"] == "steering_applied"
    });
    until_session(&session.client, "the first turn to complete", |line| {
        line["kind"] == "turn_completed"
    });
    // The missing file needs no approval: the read runs and fails.
    session.prompt("c_prompt2", "missing file");
    // The odd call's arguments match no rule, so a reviewer may hold it;
    // an approval admits it either way.
    send(
        &session.client,
        r#"{"id":"c_prompt3","command":"prompt","args":{"content":[{"type":"text","text":"odd args"}]}}"#,
    );
    loop {
        let lines = until_session(&session.client, "the second turn to settle", |line| {
            line["kind"] == "turn_completed" || line["kind"] == "permission_requested"
        });
        let last = lines.last().unwrap();
        if last["kind"] == "turn_completed" {
            break;
        }
        let request_id = last["payload"]["request_id"].as_str().unwrap().to_owned();
        send(
            &session.client,
            &format!(
                r#"{{"id":"c_reply2","command":"reply","args":{{"request_id":"{request_id}","decision":"allow"}}}}"#
            ),
        );
    }
    let nudged = session.run("c_nudge", "nudge", "");
    assert_eq!(nudged["kind"], "command_accepted", "{nudged}");
    until_session(&session.client, "the nudged turn to complete", |line| {
        line["kind"] == "turn_completed"
    });
    let accepted = session.ask("c_export", "command", &json!({ "name": "export" }));
    assert_eq!(accepted.last().unwrap()["kind"], "command_accepted");
    assert_eq!(export_status(&session), format!("wrote {id}.md"));
    let written = fs::read_to_string(session.setup.workspace().join(format!("{id}.md"))).unwrap();
    let prompt_at = written.find("> run it").expect("the prompt is quoted");
    let steer_at = written
        .find("> use the other file")
        .expect("the steering message is quoted");
    // docs/events.md:389 and :1367: the steer is a durable line after the
    // prompt's `turn_started`, and `history` returns lines in `seq` order.
    assert!(prompt_at < steer_at);
    assert!(
        written.contains("- `read` {\"path\":\"missing.txt\"} (failed)"),
        "{written}"
    );
    let reads: Vec<&str> = written
        .lines()
        .filter(|line| line.starts_with("- `read` {"))
        .collect();
    // The extension writes one line per completed model tool call and nothing
    // for the inner `ran_by` call (init.lua header); two `read` calls ran.
    assert_eq!(reads.len(), 2, "{written}");
    let odd = reads[1];
    assert!(odd.contains("null"), "{odd}");
    assert!(odd.contains("{}"), "{odd}");
    assert!(
        !written.lines().any(|line| line == "> extension said"),
        "an extension-sent message is not quoted as a prompt: {written}"
    );
    session.finish();
}

#[test]
fn an_inner_tool_call_is_not_listed() {
    let setup = Setup::new_with_fm_root();
    let server = ProviderServer::start([text_reply("hi there")]).unwrap();
    setup.provider(&server);
    setup.install_export();
    fs::write(setup.workspace().join("note.txt"), "the file\n").unwrap();
    setup.lua(
        "peek",
        "fiber.command('peek', {timeout=5000,description='Peek.',run=function() local o=host.tool('read',{path='note.txt'}) host.status('peeked ' .. o.status) end})\n",
    );
    let id = doors::mint("s_");
    let session = Session::start_with_id(setup, server, &id);
    session.prompt("c_prompt", "hi");
    let peeked = session.run("c_peek", "peek", "");
    assert_eq!(peeked["kind"], "command_accepted", "{peeked}");
    let seen = until_session(&session.client, "the peek status", |line| {
        line["kind"] == "extension_ui" && line["payload"]["extension"] == "fiber.test/peek"
    });
    assert_eq!(
        seen.last().unwrap()["payload"]["status"],
        "peeked completed"
    );
    let accepted = session.ask("c_export", "command", &json!({ "name": "export" }));
    assert_eq!(accepted.last().unwrap()["kind"], "command_accepted");
    assert_eq!(export_status(&session), format!("wrote {id}.md"));
    let written = fs::read_to_string(session.setup.workspace().join(format!("{id}.md"))).unwrap();
    assert!(written.contains("> hi"), "{written}");
    assert!(written.contains("hi there"), "{written}");
    assert!(
        !written.lines().any(|line| line.starts_with("- `read`")),
        "the inner call is not listed: {written}"
    );
    session.finish();
}
