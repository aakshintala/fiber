//! Binary-level test of the `reviewer-setup` skill's save
//! (`docs/permissions.md`, "Setting up the notes"): a scripted-model
//! session loads `reviewer-setup`, calls `reviewer_notes_save` with two
//! lines, and with the confirm answered yes both layer files hold the lines
//! under their headings with outside text unchanged, while answered no
//! leaves both files unchanged. The adversarial exit case (exit with the
//! confirm pending, an external change, resume, an attempted yes) shows no
//! pending question, rejects the stale reply, and writes nothing. The fake
//! provider serves a scripted reply sequence. Every run carries a
//! wall-clock deadline.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use crate::support;

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use fakes::ProviderServer;
use serde_json::{Value, json};
use support::*;

/// The global notes the run starts from: prose, one block, prose.
const GLOBAL_START: &str =
    "Notes for the reviewer.\n\n### Source control\n- corp.example is ours\n\nProse below.\n";

/// The project notes the run starts from: prose and one block.
const PROJECT_START: &str = "Project notes.\n\n### Routine actions\n- merge my own PRs after CI\n";

/// The two lines the scripted model sends: one per layer.
fn save_lines() -> Value {
    json!([
        {"topic": "source_control", "layer": "global", "text": "github.com/acme/* are ours"},
        {"topic": "routine_actions", "layer": "project", "text": "squash-merging my own pull requests once CI passes"},
    ])
}

/// An `openai-responses` stream answering `text`.
fn text_reply(text: &str) -> fakes::Response {
    stream(&[json!({"type": "response.output_item.done", "item": {
        "type": "message", "content": [{"type": "output_text", "text": text}]
    }})])
}

/// An `openai-responses` stream calling the `skill` tool for `name`.
fn skill_call(name: &str) -> fakes::Response {
    stream(&[json!({"type": "response.output_item.done", "item": {
        "type": "function_call",
        "id": "fc_skill",
        "call_id": "call_skill",
        "name": "skill",
        "arguments": json!({"name": name}).to_string(),
    }})])
}

/// An `openai-responses` stream calling `reviewer_notes_save` with the two
/// lines.
fn save_call() -> fakes::Response {
    stream(&[json!({"type": "response.output_item.done", "item": {
        "type": "function_call",
        "id": "fc_save",
        "call_id": "call_save",
        "name": "reviewer_notes_save",
        "arguments": json!({"lines": save_lines()}).to_string(),
    }})])
}

/// The global notes file: Fiber home's `config.json`.
fn global_file(home: &std::path::Path) -> PathBuf {
    home.join("config.json")
}

/// The project notes file: the project's `config.json`.
fn project_file(home: &std::path::Path, key: &str) -> PathBuf {
    home.join("projects").join(key).join("config.json")
}

/// Writes both layers' files with the starting notes, keeping installed
/// keys such as the test model: the save must preserve them too.
fn seed(home: &std::path::Path, key: &str) {
    let project = project_file(home, key);
    fs::create_dir_all(project.parent().unwrap()).unwrap();
    set_context(&global_file(home), GLOBAL_START);
    set_context(&project, PROJECT_START);
}

/// Installs the `reviewer-setup` skill where sessions list it.
fn install_skill(home: &std::path::Path) {
    let dir = home.join("docs/skills/reviewer-setup");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("SKILL.md"),
        include_str!("../../../docs/skills/reviewer-setup/SKILL.md"),
    )
    .unwrap();
}

/// Sets a notes file's `reviewer.context`, keeping its other keys.
fn set_context(file: &PathBuf, notes: &str) {
    let mut root: Value = fs::read(file)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Object(Default::default()));
    root["reviewer"]["context"] = Value::String(notes.to_owned());
    fs::write(file, root.to_string()).unwrap();
}

/// The `reviewer.context` of a notes file.
fn context(file: &PathBuf) -> String {
    let root: Value = serde_json::from_str(&fs::read_to_string(file).unwrap()).unwrap();
    root["reviewer"]["context"].as_str().unwrap().to_owned()
}

/// A started hub session with its direct socket subscribed: the model
/// loads the skill, then calls the save, then says it is done.
fn begin(setup: &Setup, hub: &Arc<Mutex<Option<HubProc>>>) -> (Socket, String, SessionGuard) {
    let (client, _) = connect_hub(setup, hub);
    let workspace = setup.workspace().to_str().unwrap().to_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let session = start_session(&client, &workspace, "set up my reviewer notes");
    drop(client);
    let direct = Socket::connect(setup.deadline, &setup.session_socket(&session));
    subscribe_direct(&direct);
    (direct, session, guard)
}

/// The save confirm's request id, after asserting the prompt names both
/// files and both lines.
fn confirm_requested(direct: &Socket, global: &str, project: &str, lines: &[&str]) -> String {
    let asked = until(direct, "the save confirm", |line| {
        line["kind"] == "interaction_requested"
    });
    let asked = asked.last().unwrap();
    for needle in ["Save these reviewer notes?", global, project]
        .into_iter()
        .chain(lines.iter().copied())
    {
        assert!(
            asked.to_string().contains(needle),
            "the confirm lacks {needle}: {asked}"
        );
    }
    asked["payload"]["request_id"].as_str().unwrap().to_owned()
}

/// Replies to the confirm, then waits for its person-resolved resolution.
fn reply(direct: &Socket, id: &str, request: &str, confirmed: bool) {
    direct.send(
        &json!({"id": id, "command": "reply",
            "args": {"request_id": request, "confirmed": confirmed}})
        .to_string(),
    );
    let resolved = until(direct, "the confirm resolution", |line| {
        line["kind"] == "interaction_resolved" && line["payload"]["request_id"] == request
    });
    let resolved = resolved.last().unwrap();
    assert_eq!(resolved["payload"]["by"], "person", "{resolved}");
}

/// Whether the session's log shows the `skill` call for `reviewer-setup`
/// completing.
fn skill_loaded(setup: &Setup, session: &str) -> bool {
    let log = fs::read_to_string(session_dir(setup, session).join("events.jsonl")).unwrap();
    log.lines()
        .any(|line| line.contains("\"name\":\"skill\"") && line.contains("\"reviewer-setup\""))
        && !log.contains("No skill named `reviewer-setup`")
}

/// The hub, the session's log, and every session gone.
fn finish(
    setup: &Setup,
    hub: &Arc<Mutex<Option<HubProc>>>,
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
fn yes_saves_both_layers_with_outside_text_unchanged() {
    let setup = Setup::new();
    let server = ProviderServer::start([
        skill_call("reviewer-setup"),
        save_call(),
        text_reply("saved"),
    ])
    .unwrap();
    setup.provider(&server);
    let home = setup.home();
    install_skill(&home);
    let key = log::project_key(&worktree::project(&setup.workspace()));
    seed(&home, &key);
    let global = global_file(&home).display().to_string();
    let project = project_file(&home, &key).display().to_string();

    let hub = Arc::new(Mutex::new(None));
    let (direct, session, guard) = begin(&setup, &hub);
    let request = confirm_requested(
        &direct,
        &global,
        &project,
        &[
            "github.com/acme/* are ours",
            "squash-merging my own pull requests once CI passes",
        ],
    );
    reply(&direct, "c_yes", &request, true);
    until(&direct, "the save turn", |line| {
        line["kind"] == "turn_completed"
    });

    let global_notes = context(&global_file(&home));
    let source = global_notes
        .find("### Source control")
        .expect("heading kept");
    let bullet = global_notes
        .find("- github.com/acme/* are ours")
        .expect("line saved");
    assert!(source < bullet, "the line sits under its heading");
    assert!(
        !global_notes.contains("corp.example is ours"),
        "the unkept bullet is struck on a sent topic"
    );
    assert!(global_notes.contains("Prose below."), "prose kept");
    let project_notes = context(&project_file(&home, &key));
    let heading = project_notes
        .find("### Routine actions")
        .expect("heading kept");
    let added = project_notes
        .find("- squash-merging my own pull requests once CI passes")
        .expect("line saved");
    assert!(heading < added, "the line sits under its heading");
    assert!(
        !project_notes.contains("merge my own PRs after CI"),
        "the unkept bullet is struck on a sent topic"
    );
    assert!(
        skill_loaded(&setup, &session),
        "the session loaded reviewer-setup"
    );
    finish(&setup, &hub, &session, direct, guard);
}

#[test]
fn no_leaves_both_files_unchanged() {
    let setup = Setup::new();
    let server = ProviderServer::start([
        skill_call("reviewer-setup"),
        save_call(),
        text_reply("left as it was"),
    ])
    .unwrap();
    setup.provider(&server);
    install_skill(&setup.home());
    let home = setup.home();
    let key = log::project_key(&worktree::project(&setup.workspace()));
    seed(&home, &key);
    let before = (
        fs::read(global_file(&home)).unwrap(),
        fs::read(project_file(&home, &key)).unwrap(),
    );

    let hub = Arc::new(Mutex::new(None));
    let (direct, session, guard) = begin(&setup, &hub);
    let global = global_file(&home).display().to_string();
    let project = project_file(&home, &key).display().to_string();
    let request = confirm_requested(&direct, &global, &project, &[]);
    reply(&direct, "c_no", &request, false);
    until(&direct, "the refused turn", |line| {
        line["kind"] == "turn_completed"
    });

    assert_eq!(fs::read(global_file(&home)).unwrap(), before.0);
    assert_eq!(fs::read(project_file(&home, &key)).unwrap(), before.1);
    finish(&setup, &hub, &session, direct, guard);
}

/// Exit with the confirm pending, change the notes outside Fiber, resume,
/// attempt yes: the resume asks nothing, the stale reply is rejected, and
/// the files hold only the external change.
#[test]
fn an_exited_confirm_resumes_asking_nothing_and_rejects_yes_as_stale() {
    let setup = Setup::new();
    let server = ProviderServer::start([
        skill_call("reviewer-setup"),
        save_call(),
        text_reply("resumed"),
    ])
    .unwrap();
    setup.provider(&server);
    install_skill(&setup.home());
    let home = setup.home();
    let key = log::project_key(&worktree::project(&setup.workspace()));
    seed(&home, &key);
    let before = (
        fs::read(global_file(&home)).unwrap(),
        fs::read(project_file(&home, &key)).unwrap(),
    );

    let hub = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let workspace = setup.workspace().to_str().unwrap().to_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let session = start_session(&client, &workspace, "set up my reviewer notes");
    client.send(
        &json!({"id": "c_sub", "session_id": session, "command": "subscribe",
            "args": {"level": "full"}})
        .to_string(),
    );
    let ack = recv_answer(&client, "c_sub", "the subscribe acknowledgement");
    assert_eq!(ack["kind"], "command_accepted", "{ack}");

    let asked = until(&client, "the save confirm", |line| {
        line["kind"] == "interaction_requested"
    });
    let request = asked.last().unwrap()["payload"]["request_id"]
        .as_str()
        .unwrap()
        .to_owned();

    // Exit with the confirm pending: closing resolves the unanswered
    // confirm as declined and the session writes its farewell. The save
    // never runs, so the files below still match the seeds.
    client.send(&json!({"id": "c_close", "session_id": session, "command": "close"}).to_string());
    until(&client, "the session exit", |line| {
        line["kind"] == "fiber_exited"
            && line.get("session_id") == Some(&Value::String(session.clone()))
    });
    assert_eq!(fs::read(global_file(&home)).unwrap(), before.0);
    assert_eq!(fs::read(project_file(&home, &key)).unwrap(), before.1);

    let edited = context(&project_file(&home, &key)) + "- the person's own edit\n";
    set_context(&project_file(&home, &key), &edited);

    client.send(
        &json!({"id": "c_again", "session_id": session, "command": "prompt",
            "args": {"content": [{"type": "text", "text": "go on"}]}})
        .to_string(),
    );
    let resumed = until(&client, "the resumed turn", |line| {
        line["kind"] == "turn_completed"
    });
    // A resume replays the log first: only a question past the reopen
    // bound would be the session asking again. Without a reopen every
    // line is new, so the bound stays zero.
    let last = resumed
        .iter()
        .find(|line| line["kind"] == "stream_reopened")
        .and_then(|line| line["payload"]["last_seq"].as_u64())
        .unwrap_or(0);
    assert!(
        resumed
            .iter()
            .filter(|line| line["kind"] == "interaction_requested")
            .all(|line| line
                .get("seq")
                .and_then(Value::as_u64)
                .is_some_and(|seq| seq <= last)),
        "the resume asks nothing new: {resumed:?}"
    );

    client.send(
        &json!({"id": "c_stale", "session_id": session, "command": "reply",
            "args": {"request_id": request, "confirmed": true}})
        .to_string(),
    );
    let replies = until(&client, "the stale rejection", |line| {
        line["kind"] == "command_rejected" && line["payload"]["command_id"] == "c_stale"
    });
    let rejected = replies.last().expect("the stale reply is rejected");
    assert_eq!(rejected["payload"]["code"], "stale_request", "{rejected}");

    assert_eq!(fs::read(global_file(&home)).unwrap(), before.0);
    assert_eq!(context(&project_file(&home, &key)), edited);

    client.send(&json!({"id": "c_close2", "session_id": session, "command": "close"}).to_string());
    until(&client, "the final exit", |line| {
        line["kind"] == "fiber_exited"
            && line.get("session_id") == Some(&Value::String(session.clone()))
    });
    drop(client);
    let hub = hub.lock().unwrap().take().expect("the starter ran");
    hub.kill_and_wait();
    guard.wait_gone();
}
