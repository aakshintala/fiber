//! Binary-level tests of `rewind` (`docs/events.md`, "Rewind" and
//! `docs/invocation.md`, "`rewind` starts a new session process"): the
//! built `fiber` runs `hub serve` in its own process group with its own
//! `FIBER_HOME`, holding an ordinary provider whose base URL is the fake
//! server. A rewind closes the old session with `rewound`, the hub starts
//! the named session, and every relayed client follows it there.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use fakes::{ProviderServer, Response};
use serde_json::{Value, json};
use support::*;

/// A command for `session` on the hub socket, with its own id.
fn command(id: &str, session: Option<&str>, command: &str, args: Value) -> String {
    let mut line = json!({"id": id, "command": command, "args": args});
    if let Some(session) = session {
        line["session_id"] = session.into();
    }
    serde_json::to_string(&line).unwrap()
}

fn subscribe(id: &str, session: &str) -> String {
    command(id, Some(session), "subscribe", json!({"level": "full"}))
}

fn prompt(id: &str, session: &str, text: &str) -> String {
    command(
        id,
        Some(session),
        "prompt",
        json!({"content": [{"type": "text", "text": text}]}),
    )
}

fn rewind(id: &str, session: &str, args: Value) -> String {
    command(id, Some(session), "rewind", args)
}

/// The acknowledgement of command `id`: every line until it, the last
/// accepted.
fn until_ack(client: &Socket, what: &str, id: &str) -> Vec<Value> {
    let lines = until(client, what, |line| {
        line["payload"].get("command_id").and_then(Value::as_str) == Some(id)
    });
    assert_eq!(
        lines.last().unwrap()["kind"],
        "command_accepted",
        "{lines:?}"
    );
    lines
}

/// The rejection of command `id`.
fn until_rejected(client: &Socket, what: &str, id: &str) -> Value {
    let lines = until(client, what, |line| {
        line["kind"] == "command_rejected"
            && line["payload"].get("command_id").and_then(Value::as_str) == Some(id)
    });
    lines.last().unwrap().clone()
}

/// The log of session `id`, wherever its project is.
fn session_log(setup: &Setup, id: &str) -> Vec<Value> {
    fs::read_to_string(session_log_path(setup, id))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn session_log_path(setup: &Setup, id: &str) -> PathBuf {
    fs::read_dir(setup.home().join("projects"))
        .unwrap()
        .map(|entry| {
            entry
                .unwrap()
                .path()
                .join("sessions")
                .join(id)
                .join("events.jsonl")
        })
        .find(|log| log.is_file())
        .expect("the session's log exists")
}

/// The `seq` of every `turn_started` in `lines`.
fn turn_starts(lines: &[Value]) -> Vec<u64> {
    lines
        .iter()
        .filter(|line| line["kind"] == "turn_started")
        .map(|line| line["seq"].as_u64().unwrap())
        .collect()
}

/// An `openai-responses` stream answering `Hello.` in two fragments.
fn hello() -> Response {
    stream(&[json!({"type": "response.output_item.done", "item": {
        "type": "message", "content": [{"type": "output_text", "text": "Hello."}]
    }})])
}

fn stream(events: &[Value]) -> Response {
    let mut body = String::new();
    for event in events {
        body.push_str(&format!(
            "event: {}\ndata: {event}\n\n",
            event["type"].as_str().unwrap()
        ));
    }
    let done = json!({"type": "response.completed", "response": {
        "id": "resp_1", "status": "completed",
        "usage": {"input_tokens": 10, "input_tokens_details": {"cached_tokens": 4}, "output_tokens": 3}
    }});
    body.push_str(&format!(
        "event: {}\ndata: {done}\n\n",
        done["type"].as_str().unwrap()
    ));
    Response::stream(body)
}

/// Starts the hub, the session guard and one client: the caller runs the
/// turns.
struct Hubbed {
    hub: Arc<Mutex<Option<HubProc>>>,
    client: Option<Socket>,
    workspace: String,
    guard: SessionGuard,
}

impl Hubbed {
    fn new(setup: &Setup) -> Self {
        let hub = Arc::new(Mutex::new(None));
        let (client, hello) = connect_hub(setup, &hub);
        assert_eq!(hello["kind"], "hub_hello");
        let workspace = setup.workspace().to_string_lossy().into_owned();
        let guard = SessionGuard::arm(setup.deadline, &workspace);
        Self {
            hub,
            client: Some(client),
            workspace,
            guard,
        }
    }

    fn client(&self) -> &Socket {
        self.client.as_ref().expect("the client is connected")
    }

    /// Drops the hub connection: its relays end, so only the feed can
    /// start the next session after this.
    fn disconnect(&mut self) {
        drop(self.client.take());
    }

    /// Starts a session with `content` under command `id`, subscribes
    /// `sub_id` full to it, and waits for its first turn to complete.
    fn start(&self, id: &str, sub_id: &str, content: &str) -> String {
        self.client().send(&command(
            id,
            None,
            "start",
            json!({"workspace": self.workspace, "content": [{"type": "text", "text": content}]}),
        ));
        let ack = until_ack(self.client(), "the start acknowledgement", id);
        let session = ack.last().unwrap()["payload"]["result"]["session_id"]
            .as_str()
            .expect("the start answers with a session id")
            .to_owned();
        self.client().send(&subscribe(sub_id, &session));
        until_ack(self.client(), "the subscribe acknowledgement", sub_id);
        until(self.client(), "the first turn_completed", |line| {
            line["kind"] == "turn_completed" && line["session_id"] == session
        });
        session
    }

    /// Prompts `session` with `text` under command `id`, and waits for the
    /// acknowledgement and the turn to complete.
    fn prompt(&self, id: &str, session: &str, text: &str) {
        self.client().send(&prompt(id, session, text));
        until_ack(self.client(), "the prompt acknowledgement", id);
        until(self.client(), "the turn_completed", |line| {
            line["kind"] == "turn_completed" && line["session_id"] == session
        });
    }

    /// Closes `session` under command `id`, and waits for its `fiber_exited`.
    fn close(&self, id: &str, session: &str) {
        self.client()
            .send(&command(id, Some(session), "close", json!({})));
        until(self.client(), "the fiber_exited", |line| {
            line["kind"] == "fiber_exited" && line["session_id"] == session
        });
    }

    fn finish(self) {
        let Self { hub, guard, .. } = self;
        guard.wait_gone();
        hub.lock()
            .unwrap()
            .take()
            .expect("the hub ran")
            .kill_and_wait();
    }
}

#[test]
fn a_rewind_continues_the_session_from_the_latest_turn() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello(), hello()]).unwrap();
    setup.provider(&server);
    let hubbed = Hubbed::new(&setup);
    let old = hubbed.start("c1_start", "c1_sub", "one");
    let client = hubbed.client();
    hubbed.prompt("c1_p2", &old, "two");

    client.send(&rewind("c1_rw", &old, json!({})));
    let ack = until_ack(client, "the rewind acknowledgement", "c1_rw");
    let next = ack.last().unwrap()["payload"]["result"]["new_session_id"]
        .as_str()
        .expect("the rewind answers with a session id")
        .to_owned();

    // Without sending anything, the client receives the new session's lines.
    until(client, "the new session's start", |line| {
        line["session_id"] == next && line["kind"] == "session_started"
    });

    // The old log ends `turn_completed, rewound`, with no `fiber_exited`.
    let old_log = session_log(&setup, &old);
    let kinds: Vec<&str> = old_log
        .iter()
        .map(|line| line["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds.last().unwrap(), &"rewound", "{kinds:?}");
    let rewound = old_log.last().unwrap();
    let starts = turn_starts(&old_log);
    assert_eq!(starts.len(), 2, "{kinds:?}");
    assert_eq!(rewound["payload"]["seq"], starts[1] - 1);
    assert_eq!(rewound["payload"]["jobs"], json!([]));
    assert_eq!(rewound["payload"]["new_session_id"], next);
    assert!(
        rewound
            .get("payload")
            .unwrap()
            .get("from_session_id")
            .is_none()
    );

    // The new session starts on its own: its `fiber_started` arrives live.
    until(client, "the new session's fiber_started", |line| {
        line["session_id"] == next && line["kind"] == "fiber_started"
    });
    // The new log before any prompt is one fresh start continuing the old.
    let new_log = session_log(&setup, &next);
    let new_kinds: Vec<&str> = new_log
        .iter()
        .map(|line| line["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        new_kinds,
        ["session_started", "fiber_started", "extensions_loaded"],
        "{new_kinds:?}"
    );
    let first = &new_log[0];
    assert_eq!(
        first["payload"]["forked_from"],
        json!({"session_id": old, "seq": starts[1] - 1})
    );
    assert_eq!(first["payload"]["rewind"]["jobs"], json!([]));
    assert!(first["payload"].get("parent").is_none());
    let note = first["payload"]["rewind"]["note"].as_str().unwrap();
    assert!(note.contains("rewound to this point"), "{note}");
    assert!(note.contains("No file was written"), "{note}");
    assert!(
        new_log.iter().all(|line| line["session_id"] == next),
        "no line names the old session"
    );

    // The new session answers, with the old history before the note.
    hubbed.prompt("c1_p3", &next, "three");
    let requests = server.requests();
    assert_eq!(requests.len(), 3, "one request per turn");
    let second: Value = serde_json::from_slice(&requests[1].body).unwrap();
    let third: Value = serde_json::from_slice(&requests[2].body).unwrap();
    let second_keys: Vec<&str> = second
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let third_keys: Vec<&str> = third
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(second_keys, third_keys, "the same top-level keys");
    for key in second_keys {
        if key == "input" {
            continue;
        }
        assert_eq!(
            serde_json::to_string(&second[key]).unwrap(),
            serde_json::to_string(&third[key]).unwrap(),
            "the {key} matches byte for byte"
        );
    }
    // The history rides in `input` behind the opening environment message:
    // the old request's four inputs byte for byte, then the note, then the
    // new prompt.
    let before = second["input"].as_array().unwrap();
    let after = third["input"].as_array().unwrap();
    assert_eq!(after.len(), 5, "{after:?}");
    for (index, item) in before.iter().take(3).enumerate() {
        assert_eq!(
            serde_json::to_string(&after[index]).unwrap(),
            serde_json::to_string(item).unwrap(),
            "input[{index}] matches byte for byte"
        );
    }
    assert!(
        serde_json::to_string(&after[3])
            .unwrap()
            .contains("rewound to this point"),
        "input[3] is the note: {}",
        after[3]
    );
    assert!(
        serde_json::to_string(&after[4]).unwrap().contains("three"),
        "input[4] is the new prompt: {}",
        after[4]
    );

    hubbed.close("c1_close", &next);
    hubbed.finish();
}

#[test]
fn refusals_leave_the_session_running_and_its_log_unchanged() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello(), hello()]).unwrap();
    setup.provider(&server);
    let hubbed = Hubbed::new(&setup);
    let old = hubbed.start("c2_start", "c2_sub", "one");
    let client = hubbed.client();
    let other = hubbed.start("c2_start_other", "c2_sub_other", "other");
    let before = fs::read(session_log_path(&setup, &old)).unwrap();

    // Before the first line's turn: not a step boundary.
    client.send(&rewind("c2_rw_seq", &old, json!({"seq": 0})));
    let refused = until_rejected(client, "the seq refusal", "c2_rw_seq");
    assert_eq!(refused["payload"]["code"], "not_step_boundary");
    assert_eq!(
        fs::read(session_log_path(&setup, &old)).unwrap(),
        before,
        "a refused rewind writes nothing"
    );

    // A session off the chain rewinds through the hub.
    client.send(&rewind(
        "c2_rw_from",
        &old,
        json!({"from_session_id": other}),
    ));
    let refused = until_rejected(client, "the from refusal", "c2_rw_from");
    assert_eq!(refused["payload"]["code"], "invalid_arguments");
    assert!(
        refused["payload"]["message"]
            .as_str()
            .unwrap()
            .contains("through the hub"),
        "{}",
        refused["payload"]["message"]
    );
    assert_eq!(
        fs::read(session_log_path(&setup, &old)).unwrap(),
        before,
        "a refused rewind writes nothing"
    );

    // The session is still running: its next prompt turns.
    hubbed.prompt("c2_p2", &old, "two");
    hubbed.close("c2_close_old", &old);
    hubbed.close("c2_close_other", &other);
    hubbed.finish();
}

#[test]
fn a_rewind_through_the_hub_resumes_an_exited_session() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    let hubbed = Hubbed::new(&setup);
    let old = hubbed.start("c3_start", "c3_sub", "one");
    let client = hubbed.client();
    hubbed.close("c3_close", &old);
    assert!(
        fakes::matching_exits(&hubbed.workspace, setup.deadline.left()),
        "waited until the deadline for the old session process to exit"
    );
    assert!(
        !setup.session_socket(&old).exists(),
        "the closed session unlinked its socket"
    );

    // No process holds the session: the hub resumes it, and it rewinds.
    client.send(&rewind("c3_rw", &old, json!({})));
    let ack = until_ack(client, "the rewind acknowledgement", "c3_rw");
    let next = ack.last().unwrap()["payload"]["result"]["new_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    until(client, "the new session's start", |line| {
        line["session_id"] == next && line["kind"] == "session_started"
    });
    until(client, "the new session's fiber_started", |line| {
        line["session_id"] == next && line["kind"] == "fiber_started"
    });
    let new_log = session_log(&setup, &next);
    assert_eq!(new_log[0]["kind"], "session_started");
    assert_eq!(new_log[0]["payload"]["forked_from"]["session_id"], old);

    hubbed.close("c3_close_next", &next);
    hubbed.finish();
}

#[test]
fn the_feed_starts_a_rewind_no_client_relayed() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    let mut hubbed = Hubbed::new(&setup);
    let old = hubbed.start("c4_start", "c4_sub", "one");
    hubbed.prompt("c4_p2", &old, "two");

    // A second hub client holds a feed subscription only: it relays no
    // session, so only the feed can start the next session. Its status
    // for the old session proves the feed follows it.
    let hub = Arc::clone(&hubbed.hub);
    let (watcher, _) = connect_hub(&setup, &hub);
    watcher.send(&command("c4_watch", None, "feed", json!({})));
    until(&watcher, "the feed following the session", |line| {
        line["kind"] == "session_status" && line["session_id"] == old
    });
    hubbed.disconnect();

    // A client on the session's own socket, not the hub's, rewinds it.
    let direct = Socket::connect(setup.deadline, &setup.session_socket(&old));
    direct.send(&command(
        "d4_sub",
        None,
        "subscribe",
        json!({"level": "full"}),
    ));
    until(&direct, "the direct subscribe acknowledgement", |line| {
        line["payload"].get("command_id").and_then(Value::as_str) == Some("d4_sub")
    });
    direct.send(&command("d4_rw", None, "rewind", json!({})));
    let ack = until(&direct, "the direct rewind acknowledgement", |line| {
        line["payload"].get("command_id").and_then(Value::as_str) == Some("d4_rw")
    });
    assert_eq!(ack.last().unwrap()["kind"], "command_accepted");
    let next = ack.last().unwrap()["payload"]["result"]["new_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    drop(direct);

    // The feed sees the old session leave, then starts the named session:
    // its status arrives, though no hub client relayed the rewind.
    until(&watcher, "the old session's leaving", |line| {
        line["kind"] == "session_left" && line["payload"]["session_id"] == old
    });
    until(&watcher, "the new session's status", |line| {
        line["kind"] == "session_status" && line["session_id"] == next
    });
    let new_log = session_log(&setup, &next);
    assert_eq!(
        new_log[0]["payload"]["forked_from"]["session_id"], old,
        "the feed started the rewind's session"
    );

    // A client on the new session's own socket closes it: the feed sees
    // that too.
    let direct_next = Socket::connect(setup.deadline, &setup.session_socket(&next));
    direct_next.send(&command(
        "d4_sub",
        None,
        "subscribe",
        json!({"level": "full"}),
    ));
    until(
        &direct_next,
        "the direct subscribe acknowledgement",
        |line| line["payload"].get("command_id").and_then(Value::as_str) == Some("d4_sub"),
    );
    direct_next.send(&command("d4_close", None, "close", json!({})));
    until(&direct_next, "the direct close acknowledgement", |line| {
        line["payload"].get("command_id").and_then(Value::as_str) == Some("d4_close")
    });
    drop(direct_next);
    until(&watcher, "the new session's leaving", |line| {
        line["kind"] == "session_left" && line["payload"]["session_id"] == next
    });
    drop(watcher);
    hubbed.finish();
}

#[test]
fn an_unprompted_rewound_session_is_kept_and_resumes() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello(), hello()]).unwrap();
    setup.provider(&server);
    let hubbed = Hubbed::new(&setup);
    let old = hubbed.start("c5_start", "c5_sub", "one");
    let client = hubbed.client();
    hubbed.prompt("c5_p2", &old, "two");
    client.send(&rewind("c5_rw", &old, json!({})));
    let ack = until_ack(client, "the rewind acknowledgement", "c5_rw");
    let next = ack.last().unwrap()["payload"]["result"]["new_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    until(client, "the new session's start", |line| {
        line["session_id"] == next && line["kind"] == "session_started"
    });

    // Never prompted: closing it still keeps its directory.
    hubbed.close("c5_close", &next);
    let dir = session_log_path(&setup, &next).parent().unwrap().to_owned();
    assert!(dir.is_dir(), "the rewound session is kept");
    // The prompt must reach a gone session, not one still shutting down:
    // its socket accepts until the process releases it.
    assert!(
        fakes::matching_exits(&hubbed.workspace, setup.deadline.left()),
        "waited until the deadline for the closed session process to exit"
    );

    // A prompt through the hub resumes it like any exited session.
    hubbed.prompt("c5_p3", &next, "three");
    hubbed.close("c5_close_again", &next);
    hubbed.finish();
}

#[test]
fn a_rewound_session_rewinds_to_an_ancestor_point() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello(), hello(), hello()]).unwrap();
    setup.provider(&server);
    let hubbed = Hubbed::new(&setup);
    let old = hubbed.start("c6_start", "c6_sub", "one");
    let client = hubbed.client();
    hubbed.prompt("c6_p2", &old, "two");
    client.send(&rewind("c6_rw", &old, json!({})));
    let ack = until_ack(client, "the rewind acknowledgement", "c6_rw");
    let middle = ack.last().unwrap()["payload"]["result"]["new_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    until(client, "the middle session's start", |line| {
        line["session_id"] == middle && line["kind"] == "session_started"
    });
    until(client, "the middle session's fiber_started", |line| {
        line["session_id"] == middle && line["kind"] == "fiber_started"
    });
    hubbed.prompt("c6_p3", &middle, "three");

    // The ancestor's first turn boundary, through the middle session.
    let old_log = session_log(&setup, &old);
    let point = turn_starts(&old_log)[0] - 1;
    client.send(&rewind(
        "c6_rw_again",
        &middle,
        json!({"from_session_id": old, "seq": point}),
    ));
    let ack = until_ack(client, "the second rewind acknowledgement", "c6_rw_again");
    let next = ack.last().unwrap()["payload"]["result"]["new_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    until(client, "the third session's start", |line| {
        line["session_id"] == next && line["kind"] == "session_started"
    });
    until(client, "the third session's fiber_started", |line| {
        line["session_id"] == next && line["kind"] == "fiber_started"
    });
    let new_log = session_log(&setup, &next);
    assert_eq!(
        new_log[0]["payload"]["forked_from"],
        json!({"session_id": old, "seq": point}),
        "the new session continues the ancestor"
    );

    // Its first request replays the ancestor's first inputs, then the note.
    hubbed.prompt("c6_p4", &next, "four");
    // The point is before the ancestor turn's own input, which rides in
    // its `turn_started`: the new session replays only what the point
    // holds, then the note, then the new prompt.
    let requests = server.requests();
    assert_eq!(requests.len(), 4, "one request per turn");
    let first: Value = serde_json::from_slice(&requests[0].body).unwrap();
    let last: Value = serde_json::from_slice(&requests[3].body).unwrap();
    let first_input = first["input"].as_array().unwrap();
    let input = last["input"].as_array().unwrap();
    assert_eq!(input.len(), 3, "{input:?}");
    assert_eq!(
        serde_json::to_string(&input[0]).unwrap(),
        serde_json::to_string(&first_input[0]).unwrap(),
        "the ancestor's input at the point"
    );
    assert!(
        serde_json::to_string(&input[1])
            .unwrap()
            .contains("rewound to this point"),
        "input[1] is the note: {}",
        input[1]
    );
    assert!(
        serde_json::to_string(&input[2]).unwrap().contains("four"),
        "input[2] is the new prompt: {}",
        input[2]
    );

    hubbed.close("c6_close_old", &old);
    hubbed.close("c6_close_middle", &middle);
    hubbed.close("c6_close_next", &next);
    hubbed.finish();
}

/// Runs the system `git` in `dir`, in its own process group, to its exit
/// under the test's [`Deadline`].
fn git(deadline: Deadline, dir: &Path, args: &[&str]) -> String {
    use std::os::unix::process::CommandExt as _;
    let mut command = std::process::Command::new("git");
    command
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
        .args(["-c", "init.defaultBranch=main"])
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .process_group(0);
    let out = run_to_exit(deadline, &format!("git {args:?}"), command);
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// A repository with one commit on `main` at `dir`.
fn init_repo(deadline: Deadline, dir: &Path) {
    git(deadline, dir, &["init", "--quiet"]);
    fs::write(dir.join("file.txt"), "x").unwrap();
    git(deadline, dir, &["add", "."]);
    git(deadline, dir, &["commit", "--quiet", "-m", "first"]);
}

#[test]
fn a_rewind_keeps_the_worktree_for_the_next_session() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    init_repo(setup.deadline, &setup.workspace());
    let repo = setup.workspace().to_string_lossy().into_owned();
    let old = "s_7111111111111111";

    // The session runs in a new worktree, as `ask --worktree` does.
    let mut session = setup.fiber(&["session", "--id", old, "--workspace", &repo, "--worktree"]);
    session.current_dir(setup.workspace());
    let mut child = session.spawn().unwrap();
    let stdout = child.stdout.take().unwrap();
    let first: String = support::bounded(setup.deadline, "the session's first line", move || {
        use std::io::BufRead;
        let mut read = std::io::BufReader::new(stdout);
        let mut line = String::new();
        read.read_line(&mut line).unwrap();
        line
    });
    let started: Value = serde_json::from_str(first.trim_end()).unwrap();
    assert_eq!(started["kind"], "session_started", "{started}");
    let worktree = started["payload"]["worktree"]["path"]
        .as_str()
        .expect("the session runs in a worktree")
        .to_owned();
    let branch = started["payload"]["worktree"]["branch"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        started["payload"]["workspace"].as_str().unwrap(),
        worktree,
        "the worktree is the workspace"
    );
    let sessions_guard = SessionGuard::arm(setup.deadline, &worktree);

    let hubbed = Hubbed::new(&setup);
    let client = hubbed.client();
    client.send(&subscribe("c7_sub", old));
    until_ack(client, "the subscribe acknowledgement", "c7_sub");
    hubbed.prompt("c7_p1", old, "hi");
    client.send(&rewind("c7_rw", old, json!({})));
    let ack = until_ack(client, "the rewind acknowledgement", "c7_rw");
    let next = ack.last().unwrap()["payload"]["result"]["new_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    until(client, "the new session's start", |line| {
        line["session_id"] == next && line["kind"] == "session_started"
    });
    until(client, "the new session's fiber_started", |line| {
        line["session_id"] == next && line["kind"] == "fiber_started"
    });

    // The old process is gone, and the worktree is still there.
    assert!(
        fakes::matching_exits(&repo, setup.deadline.left()),
        "waited until the deadline for the old session process to exit"
    );
    support::bounded(setup.deadline, "the old session to be reaped", move || {
        child.wait().unwrap();
    });
    assert!(Path::new(&worktree).is_dir(), "the worktree is kept");
    assert!(
        !git(
            setup.deadline,
            &setup.workspace(),
            &["branch", "--list", &branch]
        )
        .is_empty(),
        "its branch is kept"
    );

    // The new session records the same workspace and worktree, and runs.
    let new_log = session_log(&setup, &next);
    assert_eq!(new_log[0]["payload"]["workspace"], worktree);
    assert_eq!(new_log[0]["payload"]["worktree"]["path"], worktree);
    assert_eq!(new_log[0]["payload"]["worktree"]["branch"], branch);
    drop(Socket::connect(
        setup.deadline,
        &setup.session_socket(&next),
    ));
    // Already subscribed at `full` through the redirect: the new session's
    // status arrives with the worktree it runs in.
    let status = until(client, "the new session's status", |line| {
        line["kind"] == "session_status" && line["session_id"] == next
    });
    assert_eq!(
        status.last().unwrap()["payload"]["workspace"],
        worktree,
        "the new session runs in the worktree"
    );

    // Closing the new session never removes the worktree either.
    hubbed.close("c7_close", &next);
    assert!(Path::new(&worktree).is_dir(), "the worktree is still there");
    assert!(
        !git(
            setup.deadline,
            &setup.workspace(),
            &["branch", "--list", &branch]
        )
        .is_empty(),
        "its branch is still there"
    );
    sessions_guard.wait_gone();
    hubbed.finish();
}
