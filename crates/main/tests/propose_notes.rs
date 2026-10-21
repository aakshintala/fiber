//! Binary-level test of driver `propose_notes` (`docs/invocation.md`,
//! "What each command does"): a client proposes reviewer-note lines on a
//! live session built from the `fiber` binary, reading the evidence items
//! and the parsed proposals, while the session's log gains the turn-less
//! usage record. The fake provider serves a scripted reply sequence and
//! records every request it receives, so the test reads the request the
//! provider received: the built-in `scripted` provider takes no notice of
//! request bodies, so it cannot show them. Every run carries a wall-clock
//! deadline.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::sync::{Arc, Mutex};

use fakes::ProviderServer;
use serde_json::{Value, json};
use support::*;

/// An `openai-responses` stream answering `text`.
fn text_reply(text: &str) -> fakes::Response {
    stream(&[json!({"type": "response.output_item.done", "item": {
        "type": "message", "content": [{"type": "output_text", "text": text}]
    }})])
}

/// Subscribes `direct` at `full`: the session socket takes no `session_id`.
#[track_caller]
fn subscribe_full(direct: &Socket) {
    direct.send(r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#);
    let sub = recv(direct, "the subscribe acknowledgement");
    assert_eq!(sub["kind"], "command_accepted", "{sub}");
    assert_eq!(sub["payload"]["command_id"], "c_sub");
}

/// The hub's session over, the direct socket closed, the hub killed and
/// every session gone.
#[track_caller]
fn finish(
    setup: &Setup,
    hub: &Arc<Mutex<Option<support::HubProc>>>,
    session: &str,
    direct: Socket,
    guard: SessionGuard,
) {
    drop(direct);
    let hub = hub.lock().unwrap().take().expect("the starter ran");
    hub.kill_and_wait();
    let closer = Socket::connect(setup.deadline, &setup.session_socket(session));
    close_session(&closer);
    drop(closer);
    guard.wait_gone();
}

#[test]
fn propose_notes_reads_evidence_and_proposals_with_no_token_leaking() {
    let setup = Setup::new();
    let server = ProviderServer::start([
        hello(),
        text_reply("2 | squash-merging my own pull requests once CI passes"),
    ])
    .unwrap();
    setup.provider(&server);
    // The live workspace is a git repository whose remote URL carries a
    // token: the evidence must strip its userinfo.
    let workspace = setup.workspace();
    git(setup.deadline, &workspace, &["init", "--quiet"]);
    git(
        setup.deadline,
        &workspace,
        &[
            "remote",
            "add",
            "origin",
            "https://user:token@github.com/acme/fiber.git",
        ],
    );
    // Another project's session holds two escalations answered allow, a
    // denied one, one without a rule prefix, and a usage record: only the
    // two count, and neither is this project's.
    let other = setup.root.path().join("other");
    fs::create_dir_all(&other).unwrap();
    git(setup.deadline, &other, &["init", "--quiet"]);
    let seed = setup
        .home()
        .join("projects")
        .join("seed")
        .join("sessions")
        .join("s_seed00000001");
    fs::create_dir_all(&seed).unwrap();
    let mut log = json!({"kind": "session_started",
        "payload": {"workspace": other.to_str().unwrap()}})
    .to_string();
    log.push('\n');
    let rule = || json!({"subject": "gh pr merge", "prefix": "gh pr merge"});
    let mut lines: Vec<(u64, &str, Value)> = Vec::new();
    for (number, decision, with_rule) in [
        ("r1", "allow", true),
        ("r2", "allow", true),
        ("r3", "deny", true),
        ("r4", "allow", false),
    ] {
        let mut request = json!({"request_id": number, "step": "review",
            "escalation": {"cause": "consecutive_blocks", "reason": "blocked"}});
        if with_rule {
            request["rule"] = rule();
        }
        let seq = lines.len() as u64 + 1;
        lines.push((seq, "permission_requested", request));
        lines.push((
            seq + 1,
            "permission_resolved",
            json!({"request_id": number, "decision": decision, "decided_by": "person"}),
        ));
    }
    let seq = lines.len() as u64 + 1;
    lines.push((seq, "usage_recorded", json!({"model": "other/m"})));
    for (seq, kind, payload) in &lines {
        log.push_str(
            &json!({"kind": kind, "session_id": "s_seed00000001",
                "ts": seq, "schema_version": 1, "seq": seq, "payload": payload})
            .to_string(),
        );
        log.push('\n');
    }
    fs::write(seed.join("events.jsonl"), log).unwrap();

    let hub = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let workspace = workspace.to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let session = start_session(&client, &workspace, "first");
    drop(client);
    let direct = Socket::connect(setup.deadline, &setup.session_socket(&session));
    subscribe_full(&direct);
    until(&direct, "the first turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    direct.send(
        r#"{"id":"c_notes","command":"propose_notes","args":{"topic":"routine_actions","answer":"I merge my own PRs"}}"#,
    );
    let ack = recv_answer(&direct, "c_notes", "the propose_notes acknowledgement");
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
    let result = &ack["payload"]["result"];
    assert_eq!(
        result["evidence"],
        json!([
            {"kind": "files",
             "text": "git remote origin: https://github.com/acme/fiber.git"},
            {"kind": "session",
             "text": "2 escalations answered allow for `gh pr merge`",
             "prefix": "gh pr merge", "count": 2, "here": 0},
            {"kind": "answer", "text": "I merge my own PRs"},
        ]),
        "{result}"
    );
    assert_eq!(
        result["proposed"],
        json!([{"text": "squash-merging my own pull requests once CI passes",
                "cites": [1]}]),
        "{result}"
    );
    assert!(
        !ack.to_string().contains("token"),
        "no token in the answer: {ack}"
    );
    // The startup turn and the notes request, in order: neither request
    // carries the remote's credential. (`tokens` is a protocol field, so
    // the check names the credential's forms, not the bare word.)
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        let body = String::from_utf8(request.body.clone()).unwrap();
        assert!(
            !body.contains("token@"),
            "no credential in the request: {body}"
        );
        assert!(
            !body.contains("user:token"),
            "no credential in the request: {body}"
        );
    }
    // The session's log gains one usage record with no turn and no action:
    // the startup turn's record carries its turn.
    let log = fs::read_to_string(session_dir(&setup, &session).join("events.jsonl")).unwrap();
    let turnless: Vec<Value> = log
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .filter(|line: &Value| {
            line["kind"] == "usage_recorded"
                && line.get("seq").is_some()
                && line.get("turn_id").is_none()
        })
        .collect();
    assert_eq!(turnless.len(), 1, "{log}");
    assert!(turnless[0].get("action_id").is_none(), "{}", turnless[0]);
    finish(&setup, &hub, &session, direct, guard);
}
