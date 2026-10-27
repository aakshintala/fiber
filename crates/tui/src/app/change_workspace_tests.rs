//! Tests for `/cd` (#3452): the command bytes it sends, the usage it
//! notes, and the pending move's correlation — an accept for the pending
//! id on the displayed session attaches and subscribes, while refusal,
//! loss, duplicate, stale, and unrelated answers never switch.

use contract::SessionId;
use serde_json::{Value, json};

use super::super::commands::commands_testkit::{
    SESSION, attached, connected, enter, hub_line, painted, sent, session_line,
};
use crate::app::NoticeLine;
use crate::link::Line;

const NEW: &str = "s_bbbbbbbbbbbbbbbb";
const OTHER: &str = "s_cccccccccccccccc";

/// The notices on screen as code and text.
fn notices(app: &crate::app::App) -> Vec<(String, String)> {
    app.notices()
        .into_iter()
        .filter_map(|line| match line {
            NoticeLine::Notice { code, text, .. } => Some((code, text)),
            NoticeLine::More(_) => None,
        })
        .collect()
}

/// One session envelope from `session`.
fn from(session: &str, kind: &str, payload: Value) -> Line {
    let mut line = session_line(kind, payload);
    if let Line::Session(envelope) = &mut line {
        envelope.session_id = SessionId(session.to_owned());
    }
    line
}

/// Sends `/cd <path>`: the one command line out, parsed.
fn send_cd(app: &mut crate::app::App, path: &str) -> Value {
    let lines = sent(enter(app, &format!("/cd {path}")));
    assert_eq!(lines.len(), 1, "one command line goes out");
    assert_eq!(lines[0]["command"], "change_workspace");
    assert_eq!(lines[0]["session_id"], SESSION);
    lines.into_iter().next().unwrap()
}

/// The accept for `id` naming the moved child.
fn accept(id: &Value, child: &str) -> Line {
    session_line(
        "command_accepted",
        json!({"command_id": id, "result": {"new_session_id": child}}),
    )
}

/// The rejection for `id` with `code` and `message`.
fn reject(id: &Value, code: &str, message: &str) -> Line {
    session_line(
        "command_rejected",
        json!({"command_id": id, "code": code, "message": message}),
    )
}

#[test]
fn cd_sends_the_path_unchanged_as_a_json_string() {
    let mut app = attached();
    let line = send_cd(&mut app, "../other");
    assert_eq!(line["args"], json!({"path": "../other"}));
    assert_eq!(app.draft(), "");
    // Spaces inside the path ride along, and nothing is shell-expanded.
    let mut app = attached();
    let line = send_cd(&mut app, "a  b/$(rm -rf ~)");
    assert_eq!(line["args"], json!({"path": "a  b/$(rm -rf ~)"}));
    // A home-relative path goes out as typed; the loop expands it.
    let mut app = attached();
    let line = send_cd(&mut app, "~/work");
    assert_eq!(line["args"], json!({"path": "~/work"}));
}

#[test]
fn cd_with_no_path_notes_usage_and_sends_nothing() {
    let mut app = attached();
    assert!(sent(enter(&mut app, "/cd")).is_empty());
    assert_eq!(
        notices(&app),
        vec![("usage".to_owned(), "/cd needs a path.".to_owned())]
    );
    assert_eq!(app.draft(), "/cd", "the draft stays for the path");
    // Only spaces after the command is still no path.
    let mut app = attached();
    assert!(sent(enter(&mut app, "/cd   ")).is_empty());
    assert_eq!(
        notices(&app),
        vec![("usage".to_owned(), "/cd needs a path.".to_owned())]
    );
    // The whole frame names the usage and no moved status.
    let (shown, _) = painted(&mut app, 160, 48);
    assert!(shown.contains("/cd needs a path."), "{shown}");
    assert!(!shown.contains("moved"), "{shown}");
}

#[test]
fn cd_with_no_session_names_the_missing_session() {
    let mut app = connected();
    assert!(sent(enter(&mut app, "/cd /x")).is_empty());
    assert_eq!(
        notices(&app),
        vec![(
            "session_not_found".to_owned(),
            "No session on screen.".to_owned()
        )]
    );
}

#[test]
fn refused_cd_reports_and_keeps_the_old_attachment() {
    let mut app = attached();
    let line = send_cd(&mut app, "../other");
    let out = app.on_line(reject(&line["id"], "busy", "A turn is running."));
    assert!(out.is_empty(), "a refusal sends nothing");
    assert_eq!(
        notices(&app),
        vec![("busy".to_owned(), "A turn is running.".to_owned())]
    );
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(SESSION),
        "the screen stays on the old session"
    );
    assert_eq!(app.draft(), "/cd ../other", "the draft comes back");
    // The refusal settles the claim: a late accept for it never attaches.
    let out = app.on_line(accept(&line["id"], NEW));
    assert!(out.is_empty(), "a late accept sends nothing");
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(SESSION)
    );
    // The whole frame names the refusal and no moved status.
    let (shown, _) = painted(&mut app, 160, 48);
    assert!(shown.contains("A turn is running."), "{shown}");
    assert!(!shown.contains("moved"), "{shown}");
}

#[test]
fn accepted_cd_attaches_subscribes_and_asks_without_a_first_prompt() {
    let mut app = attached();
    let line = send_cd(&mut app, "../other");
    let out: Vec<Value> = app
        .on_line(accept(&line["id"], NEW))
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    assert_eq!(out.len(), 2, "subscribe full and ask commands, {out:?}");
    assert_eq!(out[0]["command"], "subscribe");
    assert_eq!(out[0]["session_id"], NEW);
    assert_eq!(out[0]["args"], json!({"level": "full"}));
    assert_eq!(out[1]["command"], "commands");
    assert_eq!(out[1]["session_id"], NEW);
    assert!(
        out.iter().all(|line| line["command"] != "prompt"),
        "no first prompt goes out, {out:?}"
    );
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(NEW),
        "the screen switches to the child"
    );
    assert!(notices(&app).is_empty());
    // A duplicate accept finds no pending claim and sends nothing.
    let again: Vec<String> = app.on_line(accept(&line["id"], NEW));
    assert!(again.is_empty(), "a duplicate accept sends nothing");
    assert_eq!(app.session().map(|session| session.0.as_str()), Some(NEW));
}

#[test]
fn stale_accept_after_a_switch_elsewhere_sends_nothing() {
    let mut app = attached();
    let line = send_cd(&mut app, "../other");
    // The person attaches elsewhere before the answer lands.
    app.attach(SessionId(OTHER.to_owned()));
    let out: Vec<String> = app.on_line(from(
        SESSION,
        "command_accepted",
        json!({"command_id": line["id"], "result": {"new_session_id": NEW}}),
    ));
    assert!(out.is_empty(), "a stale accept sends nothing");
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(OTHER),
        "the screen stays where the person put it"
    );
}

#[test]
fn unrelated_new_session_id_switches_nothing() {
    let mut app = attached();
    let line = send_cd(&mut app, "../other");
    // Another command's accept names a new session: the move's claim
    // stands, and the screen stays.
    let out: Vec<String> = app.on_line(accept(&serde_json::json!("c_other"), NEW));
    assert!(out.is_empty());
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(SESSION)
    );
    // The pending move still answers: the accept switches then.
    let out: Vec<Value> = app
        .on_line(accept(&line["id"], NEW))
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    assert_eq!(out.len(), 2, "{out:?}");
    assert_eq!(app.session().map(|session| session.0.as_str()), Some(NEW));
}

#[test]
fn hub_refused_cd_reports_and_settles_the_claim() {
    let mut app = attached();
    let line = send_cd(&mut app, "../other");
    // The hub relays the start's failure for the sent command.
    let out: Vec<String> = app.on_line(hub_line(
        "command_rejected",
        json!({"command_id": line["id"], "code": "io_failed",
            "message": "the target is gone"}),
    ));
    assert!(out.is_empty(), "a hub refusal sends nothing itself");
    assert_eq!(
        notices(&app),
        vec![("io_failed".to_owned(), "the target is gone".to_owned())]
    );
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(SESSION)
    );
    // The refusal settles the claim: a late accept never attaches.
    let out: Vec<String> = app.on_line(accept(&line["id"], NEW));
    assert!(out.is_empty(), "a late accept sends nothing");
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(SESSION)
    );
}

#[test]
fn lost_cd_sends_nothing_later() {
    let mut app = attached();
    let line = send_cd(&mut app, "../other");
    let written = serde_json::to_string(&line).unwrap();
    // The line never reached the hub, so the hub never answers it: the
    // claim drops with the draft's return, and only the lost connection
    // is named.
    app.write_failed(&[written]);
    assert_eq!(
        notices(&app),
        vec![(
            "connection_failed".to_owned(),
            "Connection lost.".to_owned()
        )]
    );
    assert_eq!(app.draft(), "/cd ../other", "the draft comes back");
    let out: Vec<String> = app.on_line(accept(&line["id"], NEW));
    assert!(out.is_empty(), "a late accept sends nothing");
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(SESSION)
    );
}

#[test]
fn duplicate_rejection_keeps_the_claim_for_the_first_answer() {
    let mut app = attached();
    let line = send_cd(&mut app, "../other");
    // A resent copy the session already holds: silent, and the first
    // answer still attaches when it lands.
    let out: Vec<String> = app.on_line(reject(&line["id"], "duplicate_command", "again"));
    assert!(out.is_empty());
    assert!(notices(&app).is_empty(), "a duplicate stays silent");
    let out: Vec<Value> = app
        .on_line(accept(&line["id"], NEW))
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    assert_eq!(out.len(), 2, "the first answer still switches, {out:?}");
    assert_eq!(app.session().map(|session| session.0.as_str()), Some(NEW));
}

#[test]
fn accept_without_a_new_session_id_switches_nothing() {
    let mut app = attached();
    let line = send_cd(&mut app, "../other");
    // An accept of another shape for the pending id settles nothing and
    // sends nothing; the claim drops with the unreadable answer.
    let out: Vec<String> = app.on_line(session_line(
        "command_accepted",
        json!({"command_id": line["id"], "result": {"commands": []}}),
    ));
    assert!(out.is_empty());
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(SESSION)
    );
}
