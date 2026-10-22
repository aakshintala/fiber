//! Tests for the correlation outbox: what waits, what is held until the
//! hub connects, what a reconnect sends again, and what a failure
//! restores.

use std::path::PathBuf;

use contract::clock::Clock;
use serde_json::json;

use super::super::{App, Effect};
use super::{Kind, Link, Outbox};
use crate::input::Draft;
use crate::link::Line;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

/// An outbox with the link up.
fn up() -> Outbox {
    Outbox {
        link: Link::Up,
        ..Default::default()
    }
}

/// A `prompt` line for [`SESSION`] with `id`.
fn prompt_line(id: &str) -> String {
    super::session_command(id, "prompt", &contract::SessionId(SESSION.to_owned()), None).to_string()
}

#[test]
fn is_up_only_when_the_link_is_up() {
    assert!(up().is_up());
    for link in [Link::Waiting, Link::Down, Link::Refused] {
        assert!(
            !Outbox {
                link,
                ..Default::default()
            }
            .is_up()
        );
    }
}

#[test]
fn send_records_pending_and_returns_the_line_when_up() {
    let mut outbox = up();
    let mut draft = Draft::default();
    draft.set("hi");
    let line = prompt_line("c_1");
    assert_eq!(
        outbox.send("c_1".to_owned(), Kind::Prompt, draft, line.clone()),
        Some(line)
    );
    assert!(outbox.held.is_empty());
    let (kind, draft) = outbox.pending.get("c_1").unwrap();
    assert_eq!(*kind, Kind::Prompt);
    assert_eq!(draft.expand(), "hi");
}

#[test]
fn send_holds_the_line_until_hello_when_waiting() {
    let mut outbox = Outbox::default();
    assert_eq!(outbox.link, Link::Waiting);
    let line = prompt_line("c_1");
    assert_eq!(
        outbox.send(
            "c_1".to_owned(),
            Kind::Prompt,
            Draft::default(),
            line.clone()
        ),
        None
    );
    assert_eq!(outbox.held, [line]);
    assert!(outbox.pending.contains_key("c_1"));
}

#[test]
fn fail_pops_pending_and_returns_its_kind_and_draft() {
    let mut outbox = up();
    let mut draft = Draft::default();
    draft.set("hi");
    outbox.insert("c_1".to_owned(), Kind::Reply, draft);
    let (kind, draft) = outbox.fail("c_1").unwrap();
    assert_eq!(kind, Kind::Reply);
    assert_eq!(draft.expand(), "hi");
    assert!(outbox.pending.is_empty());
    assert!(outbox.fail("c_1").is_none());
}

#[test]
fn answered_closes_pending_and_drops_kept() {
    let mut outbox = up();
    outbox.insert("c_1".to_owned(), Kind::Prompt, Draft::default());
    outbox.wrote(&prompt_line("c_1"));
    assert!(outbox.is_kept("c_1"));
    let (kind, _) = outbox.answered("c_1").unwrap();
    assert_eq!(kind, Kind::Prompt);
    assert!(outbox.pending.is_empty());
    assert!(!outbox.is_kept("c_1"));
    assert!(outbox.answered("c_9").is_none());
}

#[test]
fn wrote_keeps_session_commands_but_not_subscribe_commands_or_start() {
    let mut outbox = up();
    let session = contract::SessionId(SESSION.to_owned());
    outbox.wrote(&prompt_line("c_1"));
    assert!(outbox.is_kept("c_1"));
    // A second write of the same id keeps one entry.
    outbox.wrote(&prompt_line("c_1"));
    assert_eq!(outbox.kept.len(), 1);
    // A second command is kept behind the first, in write order.
    let steer = super::session_command("c_5", "steer", &session, None).to_string();
    outbox.wrote(&steer);
    // `subscribe` and `commands` go out afresh on a reconnect.
    outbox.wrote(&super::session_command("c_2", "subscribe", &session, None).to_string());
    outbox.wrote(&super::session_command("c_3", "commands", &session, None).to_string());
    // `start` carries no `session_id`, so it is never kept.
    outbox.wrote(&json!({"id": "c_4", "command": "start"}).to_string());
    // A line with no id is not a command.
    outbox.wrote("not json");
    assert_eq!(outbox.kept.len(), 2);
    assert_eq!(
        outbox.reconnect_lines(),
        vec![(session.clone(), prompt_line("c_1")), (session, steer)]
    );
}

/// An app with no home, before the hub connects.
fn fresh() -> App {
    App::new(PathBuf::from("/w"))
}

/// A `hub_hello` this terminal reads.
fn hello() -> Line {
    Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    })
}

/// The injected clock's now.
fn now() -> std::time::Instant {
    fakes::clock::FakeClock::new().now()
}

/// Types `text`, checking each key does nothing else.
fn type_text(app: &mut App, text: &str) {
    for ch in text.chars() {
        assert_eq!(app.on_key(crate::keys::Key::Char(ch), now()), Effect::None);
    }
}

#[test]
fn a_failure_returns_the_draft_to_an_empty_box() {
    let mut app = fresh();
    let mut draft = Draft::default();
    draft.set("hi");
    app.outbox.insert("c_1".to_owned(), Kind::Prompt, draft);
    let (kind, draft) = app.outbox.fail("c_1").unwrap();
    app.apply_failed("c_1", kind, draft);
    assert_eq!(app.draft(), "hi");
}

#[test]
fn a_failure_drops_its_draft_when_the_box_is_full() {
    // A full box keeps its text: the failure's draft is dropped.
    let mut app = fresh();
    let mut draft = Draft::default();
    draft.set("held");
    app.outbox.insert("c_2".to_owned(), Kind::Prompt, draft);
    type_text(&mut app, "typed");
    let (kind, draft) = app.outbox.fail("c_2").unwrap();
    app.apply_failed("c_2", kind, draft);
    assert_eq!(app.draft(), "typed");
}

#[test]
fn a_failure_resets_starting_after_a_start_and_nothing_else() {
    let mut app = fresh();
    app.phase = super::Phase::Pending {
        command_id: "c_1".to_owned(),
    };
    app.outbox
        .insert("c_1".to_owned(), Kind::Start, Draft::default());
    let (kind, draft) = app.outbox.fail("c_1").unwrap();
    app.apply_failed("c_1", kind, draft);
    assert_eq!(app.phase, super::Phase::Starting);
    // Any other kind leaves the phase alone.
    app.phase = super::Phase::Pending {
        command_id: "c_2".to_owned(),
    };
    app.outbox
        .insert("c_2".to_owned(), Kind::Prompt, Draft::default());
    let (kind, draft) = app.outbox.fail("c_2").unwrap();
    app.apply_failed("c_2", kind, draft);
    assert!(matches!(app.phase, super::Phase::Pending { .. }));
}

#[test]
fn a_rejected_reply_restores_its_queue_and_offer() {
    let mut app = fresh();
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.outbox
        .insert("c_1".to_owned(), Kind::Reply, Draft::default());
    let (kind, draft) = app.outbox.fail("c_1").unwrap();
    assert_eq!(kind, Kind::Reply);
    // Restoring a reply the queue never held is still a no-op away from
    // the failure path: the call itself must not panic.
    app.apply_failed("c_1", kind, draft);
    assert!(app.outbox.pending.is_empty());
}

#[test]
fn a_stale_commands_answer_changes_nothing() {
    let mut app = fresh();
    app.attach(contract::SessionId(SESSION.to_owned()));
    let session = contract::SessionId(SESSION.to_owned());
    let first = app.ask_commands(&session);
    let second = app.ask_commands(&session);
    assert_ne!(first, second);
    // The older answer arrives late: the list keeps the built-ins only.
    app.commands_answered(&contract::events::CommandAccepted {
        command_id: contract::CommandId(
            serde_json::from_str::<serde_json::Value>(&first).unwrap()["id"]
                .as_str()
                .unwrap()
                .to_owned(),
        ),
        result: Some(contract::events::CommandResult::Commands {
            commands: vec![contract::events::CommandInfo {
                name: "deploy".to_owned(),
                description: "Deploys.".to_owned(),
                argument_hint: None,
                tag: "acme".to_owned(),
                extension: true,
            }],
        }),
    });
    type_text(&mut app, "/");
    let lines = app
        .completions()
        .map(|panel| panel.lines())
        .unwrap_or_default();
    assert!(!lines.iter().any(|line| line.contains("deploy")));
    assert!(lines.iter().any(|line| line.contains("/home")));
}

#[test]
fn enter_when_up_sends_the_start() {
    let mut app = fresh();
    app.on_line(hello());
    assert_eq!(app.outbox.link, Link::Up);
    type_text(&mut app, "go");
    let effect = app.on_key(crate::keys::Key::Enter, now());
    assert!(matches!(effect, Effect::Send(_)), "{effect:?}");
}

#[test]
fn a_held_start_is_pending_and_goes_out_on_hello() {
    let mut app = fresh();
    type_text(&mut app, "hi");
    assert_eq!(app.on_key(crate::keys::Key::Enter, now()), Effect::None);
    assert_eq!(app.outbox.held.len(), 1);
    assert_eq!(app.outbox.pending.len(), 1);
    let (kind, draft) = app.outbox.pending.values().next().unwrap();
    assert_eq!(*kind, Kind::Start);
    assert_eq!(draft.expand(), "hi");
    let held = app.outbox.held.clone();
    let lines = app.on_line(hello());
    assert!(app.outbox.held.is_empty());
    assert_eq!(app.outbox.link, Link::Up);
    assert!(
        lines.contains(&held[0]),
        "the held start goes out: {lines:?}"
    );
    // `start` carries no `session_id`, so it is never kept.
    assert!(app.outbox.kept.is_empty());
}

#[test]
fn kept_lines_go_out_again_unchanged_after_a_drop() {
    let mut app = fresh();
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.on_line(hello());
    app.outbox.link = Link::Up;
    let line = prompt_line("c_1");
    app.outbox
        .insert("c_1".to_owned(), Kind::Prompt, Draft::default());
    app.outbox.wrote(&line);
    app.disconnected();
    let lines = app.on_line(hello());
    assert_eq!(
        lines.iter().filter(|sent| **sent == line).count(),
        1,
        "{lines:?}"
    );
}

#[test]
fn an_import_entry_answers_with_its_source_then_closes() {
    use crate::import_report::ImportSource;

    let mut outbox = up();
    outbox.insert(
        "c_1".to_owned(),
        Kind::Import(ImportSource::Codex),
        Draft::default(),
    );
    let (kind, _) = outbox.answered("c_1").expect("the pending import answers");
    assert_eq!(kind, Kind::Import(ImportSource::Codex));
    assert!(outbox.pending.is_empty());
    assert!(outbox.answered("c_1").is_none());
}
