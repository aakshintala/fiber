//! Tests for the correlation outbox: what waits, what is held until the
//! hub connects, what a reconnect sends again, and what a failure
//! restores.

use std::path::PathBuf;

use serde_json::{Value, json};

use super::super::{App, Effect};
use super::{Kind, Link, Outbox, RecentAsk, Waiting};
use crate::app::testkit::{SESSION, hello, now};
use crate::input::Draft;
use contract::commands::{Command, CommandLine, ContentArgs, SubscribeArgs, SubscribeLevel};

#[test]
fn wait_then_settled_returns_the_tag_once() {
    let mut outbox = up();
    let tag = super::Waiting::Dismiss(contract::SessionId(SESSION.to_owned()));
    assert_eq!(outbox.wait("c_wait", tag.clone()), None);
    assert_eq!(outbox.waiting("c_wait"), Some(tag.clone()));
    assert_eq!(outbox.settled("c_other"), None);
    assert_eq!(outbox.settled("c_wait"), Some(tag));
    assert_eq!(outbox.waiting("c_wait"), None);
    assert_eq!(outbox.settled("c_wait"), None);
}

#[test]
fn a_second_wait_replaces_and_returns_the_first() {
    let mut outbox = up();
    let first = super::Waiting::Dismiss(contract::SessionId(SESSION.to_owned()));
    let second = super::Waiting::Dismiss(contract::SessionId("s_other".to_owned()));
    assert_eq!(outbox.wait("c_wait", first.clone()), None);
    assert_eq!(outbox.wait("c_wait", second.clone()), Some(first));
    assert_eq!(outbox.waiting("c_wait"), Some(second.clone()));
    assert_eq!(outbox.settled("c_wait"), Some(second));
}

#[test]
fn answered_and_fail_leave_the_tag() {
    let mut outbox = up();
    let tag = super::Waiting::Dismiss(contract::SessionId(SESSION.to_owned()));
    for id in ["c_answer", "c_fail"] {
        outbox.wait(id, tag.clone());
        outbox.insert(id.to_owned(), Kind::Command, Draft::default());
    }
    assert!(outbox.answered("c_answer").is_some());
    assert_eq!(outbox.waiting("c_answer"), Some(tag.clone()));
    assert!(outbox.fail("c_fail").is_some());
    assert_eq!(outbox.waiting("c_fail"), Some(tag));
}

#[test]
fn waiting_any_and_forget_match_variants_not_payloads() {
    let mut outbox = up();
    let first = super::Waiting::ChangeWorkspace(contract::SessionId("s_first".to_owned()));
    let other = super::Waiting::ChangeWorkspace(contract::SessionId("s_other".to_owned()));
    assert!(!outbox.waiting_any(&other));
    outbox.wait("c_first", first);
    outbox.wait("c_fork", super::Waiting::Fork);
    assert!(outbox.waiting_any(&other));
    outbox.wait("c_other", other.clone());
    outbox.forget(&other);
    assert!(!outbox.waiting_any(&other));
    assert_eq!(outbox.waiting("c_first"), None);
    assert_eq!(outbox.waiting("c_other"), None);
    assert_eq!(outbox.waiting("c_fork"), Some(super::Waiting::Fork));
}

#[test]
fn tags_come_back_in_send_order() {
    let mut outbox = up();
    outbox.wait("c_b", Waiting::Fork);
    outbox.wait("c_a", Waiting::Sessions);
    outbox.wait("c_c", Waiting::Feed);
    let ids: Vec<&str> = outbox.tags().map(|(id, _)| id).collect();
    assert_eq!(ids, ["c_b", "c_a", "c_c"]);
}

#[test]
fn a_repeat_wait_keeps_its_place() {
    let mut outbox = up();
    outbox.wait("c_b", Waiting::Fork);
    outbox.wait("c_a", Waiting::Sessions);
    assert_eq!(outbox.wait("c_b", Waiting::Feed), Some(Waiting::Fork));
    let tags: Vec<(&str, &Waiting)> = outbox.tags().collect();
    assert_eq!(tags, [("c_b", &Waiting::Feed), ("c_a", &Waiting::Sessions)]);
    assert_eq!(outbox.settled("c_b"), Some(Waiting::Feed));
    assert_eq!(outbox.settled("c_b"), None);
}

#[test]
fn settle_where_removes_only_matches() {
    let mut outbox = up();
    outbox.wait("c_s1", Waiting::Recent(RecentAsk::Scoped("p1".to_owned())));
    outbox.wait("c_first", Waiting::Recent(RecentAsk::First));
    outbox.wait("c_s2", Waiting::Recent(RecentAsk::Scoped("p2".to_owned())));
    outbox.settle_where(|owner| matches!(owner, Waiting::Recent(RecentAsk::Scoped(_))));
    let tags: Vec<(&str, &Waiting)> = outbox.tags().collect();
    assert_eq!(tags, [("c_first", &Waiting::Recent(RecentAsk::First))]);
}

#[test]
fn forget_ignores_the_payload_but_keeps_the_order_of_the_rest() {
    let mut outbox = up();
    outbox.wait("c_1", Waiting::Directories("/a/".to_owned()));
    outbox.wait("c_2", Waiting::Fork);
    outbox.wait("c_3", Waiting::Directories("/b/".to_owned()));
    outbox.wait("c_4", Waiting::Feed);
    outbox.forget(&Waiting::Directories(String::new()));
    let ids: Vec<&str> = outbox.tags().map(|(id, _)| id).collect();
    assert_eq!(ids, ["c_2", "c_4"]);
}

/// An outbox with the link up.
fn up() -> Outbox {
    Outbox {
        link: Link::Up,
        ..Default::default()
    }
}

/// A `prompt` line for [`SESSION`] with `id`.
fn prompt_line(id: &contract::CommandId) -> String {
    super::session_line(
        id,
        &contract::SessionId(SESSION.to_owned()),
        Command::Prompt(ContentArgs { content: vec![] }),
    )
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
    let line = prompt_line(&contract::CommandId("c_1".to_owned()));
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
    let line = prompt_line(&contract::CommandId("c_1".to_owned()));
    assert_eq!(
        outbox.send(
            "c_1".to_owned(),
            Kind::Prompt,
            Draft::default(),
            line.clone()
        ),
        None
    );
    assert_eq!(
        outbox.held,
        [crate::app::Sent {
            id: contract::CommandId("c_1".to_owned()),
            line,
        }]
    );
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
    outbox.wrote(&prompt_line(&contract::CommandId("c_1".to_owned())));
    assert!(outbox.is_kept("c_1"));
    let (kind, _) = outbox.answered("c_1").unwrap();
    assert_eq!(kind, Kind::Prompt);
    assert!(outbox.pending.is_empty());
    assert!(!outbox.is_kept("c_1"));
    assert!(outbox.answered("c_9").is_none());
}

#[test]
fn wrote_keeps_session_lines_but_not_subscribe_or_start() {
    let mut outbox = up();
    let session = contract::SessionId(SESSION.to_owned());
    outbox.wrote(&prompt_line(&contract::CommandId("c_1".to_owned())));
    assert!(outbox.is_kept("c_1"));
    // A second write of the same id keeps one entry.
    outbox.wrote(&prompt_line(&contract::CommandId("c_1".to_owned())));
    assert_eq!(outbox.kept.len(), 1);
    // A second command is kept behind the first, in write order.
    let steer = super::session_line(
        &contract::CommandId("c_5".to_owned()),
        &session,
        Command::Steer(ContentArgs { content: vec![] }),
    );
    outbox.wrote(&steer);
    // `subscribe` and `commands` go out afresh on a reconnect.
    outbox.wrote(&super::session_line(
        &contract::CommandId("c_2".to_owned()),
        &session,
        Command::Subscribe(SubscribeArgs {
            level: SubscribeLevel::Summary,
            credential: None,
        }),
    ));
    outbox.wrote(&super::session_line(
        &contract::CommandId("c_3".to_owned()),
        &session,
        Command::Commands,
    ));
    // `start` carries no `session_id`, so it is never kept.
    outbox.wrote(&json!({"id": "c_4", "command": "start"}).to_string());
    // A line with no id is not a command.
    outbox.wrote("not json");
    assert_eq!(outbox.kept.len(), 2);
    assert_eq!(
        outbox.reconnect_lines(),
        vec![
            (
                session.clone(),
                prompt_line(&contract::CommandId("c_1".to_owned()))
            ),
            (session, steer)
        ]
    );
}

#[test]
fn session_line_round_trips_a_line_with_args_and_one_without() {
    use contract::commands::SentPart;
    let session = contract::SessionId(SESSION.to_owned());
    let with_args = Command::Prompt(ContentArgs {
        content: vec![SentPart::Text {
            text: "hi".to_owned(),
        }],
    });
    for (id, command) in [("c_1", with_args), ("c_2", Command::Cancel)] {
        let line = super::session_line(
            &contract::CommandId(id.to_owned()),
            &session,
            command.clone(),
        );
        let mut value: Value =
            serde_json::from_str(&line).unwrap_or_else(|err| panic!("{line}: {err}"));
        // The injected top-level `session_id` is not a `CommandLine`
        // key, so it comes out before the line parses as one.
        let session_id = value
            .as_object_mut()
            .and_then(|map| map.remove("session_id"))
            .and_then(|id| id.as_str().map(str::to_owned))
            .unwrap_or_else(|| panic!("a session_id: {line}"));
        assert_eq!(session_id, SESSION);
        let parsed: CommandLine =
            serde_json::from_value(value).unwrap_or_else(|err| panic!("{line}: {err}"));
        assert_eq!(parsed.id, contract::CommandId(id.to_owned()));
        assert_eq!(parsed.command, command);
    }
    // A hand-built line with a wrong key is refused once the injected
    // `session_id` is removed.
    let mut bad = json!({"id": "c_9", "command": "cancel", "bogus": true});
    bad.as_object_mut()
        .unwrap_or_else(|| panic!("an object"))
        .remove("session_id");
    assert!(serde_json::from_value::<CommandLine>(bad).is_err());
}

/// An app with no home, before the hub connects.
fn fresh() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    // A home with a model chosen, so Enter sends a `start` instead of
    // opening the model picker.
    app.set_home(crate::home::Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: String::new(),
            git: false,
        },
        model: Some("test/model".to_owned()),
        ..Default::default()
    });
    app
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
    assert!(matches!(effect, Effect::SendTracked(_)), "{effect:?}");
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
        lines.contains(&held[0].line),
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
    let line = prompt_line(&contract::CommandId("c_1".to_owned()));
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
    use contract::commands::ImportSource;

    let mut outbox = up();
    outbox.insert(
        "c_1".to_owned(),
        Kind::Import(Some(ImportSource::Codex)),
        Draft::default(),
    );
    let (kind, _) = outbox.answered("c_1").expect("the pending import answers");
    assert_eq!(kind, Kind::Import(Some(ImportSource::Codex)));
    assert!(outbox.pending.is_empty());
    assert!(outbox.answered("c_1").is_none());
}

#[test]
fn find_wait_then_settled_returns_the_tag_once() {
    let mut outbox = up();
    assert_eq!(outbox.wait("c_wait", Waiting::Find), None);
    assert_eq!(outbox.waiting("c_wait"), Some(Waiting::Find));
    assert_eq!(outbox.settled("c_other"), None);
    assert_eq!(outbox.settled("c_wait"), Some(Waiting::Find));
    assert_eq!(outbox.waiting("c_wait"), None);
    assert_eq!(outbox.settled("c_wait"), None);
}

#[test]
fn commands_wait_then_settled_returns_the_tag_once() {
    let mut outbox = up();
    assert_eq!(outbox.wait("c_wait", Waiting::Commands), None);
    assert_eq!(outbox.waiting("c_wait"), Some(Waiting::Commands));
    assert_eq!(outbox.settled("c_other"), None);
    assert_eq!(outbox.settled("c_wait"), Some(Waiting::Commands));
    assert_eq!(outbox.waiting("c_wait"), None);
    assert_eq!(outbox.settled("c_wait"), None);
}

#[test]
fn model_wait_then_settled_returns_the_tag_once() {
    let mut outbox = up();
    assert_eq!(outbox.wait("c_wait", Waiting::Model), None);
    assert_eq!(outbox.waiting("c_wait"), Some(Waiting::Model));
    assert_eq!(outbox.settled("c_other"), None);
    assert_eq!(outbox.settled("c_wait"), Some(Waiting::Model));
    assert_eq!(outbox.waiting("c_wait"), None);
    assert_eq!(outbox.settled("c_wait"), None);
}

#[test]
fn reply_wait_then_settled_returns_the_tag_once() {
    let mut outbox = up();
    assert_eq!(outbox.wait("c_wait", Waiting::Reply), None);
    assert_eq!(outbox.waiting("c_wait"), Some(Waiting::Reply));
    assert_eq!(outbox.settled("c_other"), None);
    assert_eq!(outbox.settled("c_wait"), Some(Waiting::Reply));
    assert_eq!(outbox.waiting("c_wait"), None);
    assert_eq!(outbox.settled("c_wait"), None);
}

#[test]
fn offer_file_wait_then_settled_returns_the_tag_once() {
    let mut outbox = up();
    assert_eq!(outbox.wait("c_wait", Waiting::OfferFile), None);
    assert_eq!(outbox.waiting("c_wait"), Some(Waiting::OfferFile));
    assert_eq!(outbox.settled("c_other"), None);
    assert_eq!(outbox.settled("c_wait"), Some(Waiting::OfferFile));
    assert_eq!(outbox.waiting("c_wait"), None);
    assert_eq!(outbox.settled("c_wait"), None);
}

#[test]
fn steer_wait_then_settled_returns_the_tag_once() {
    let mut outbox = up();
    assert_eq!(outbox.wait("c_wait", Waiting::Steer), None);
    assert_eq!(outbox.waiting("c_wait"), Some(Waiting::Steer));
    assert_eq!(outbox.settled("c_other"), None);
    assert_eq!(outbox.settled("c_wait"), Some(Waiting::Steer));
    assert_eq!(outbox.waiting("c_wait"), None);
    assert_eq!(outbox.settled("c_wait"), None);
}

#[test]
fn shell_wait_then_settled_returns_the_tag_once() {
    let mut outbox = up();
    assert_eq!(outbox.wait("c_wait", Waiting::Shell), None);
    assert_eq!(outbox.waiting("c_wait"), Some(Waiting::Shell));
    assert_eq!(outbox.settled("c_other"), None);
    assert_eq!(outbox.settled("c_wait"), Some(Waiting::Shell));
    assert_eq!(outbox.waiting("c_wait"), None);
    assert_eq!(outbox.settled("c_wait"), None);
}

#[test]
fn a_second_wait_replaces_the_first_across_variants() {
    let mut outbox = up();
    assert_eq!(outbox.wait("c_wait", Waiting::Model), None);
    assert_eq!(outbox.wait("c_wait", Waiting::Shell), Some(Waiting::Model));
    assert_eq!(outbox.waiting("c_wait"), Some(Waiting::Shell));
    assert_eq!(outbox.settled("c_wait"), Some(Waiting::Shell));
}

/// One session envelope from [`SESSION`], with no action id.
fn session_envelope(kind: &str, payload: serde_json::Value) -> crate::link::Line {
    crate::link::Line::Session(contract::Envelope {
        kind: contract::events::EventKind::from_name(kind),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// Folds one session line, answering the `commands` ask a fold may send
/// with no rows.
fn fold_session_line(app: &mut App, line: crate::link::Line) {
    let mut out = app.on_line(line);
    assert!(out.len() <= 1, "{out:?}");
    let Some(first) = out.pop() else {
        return;
    };
    let asked: serde_json::Value = serde_json::from_str(&first).unwrap();
    assert_eq!(asked["command"], "commands", "{asked}");
    let answer = session_envelope(
        "command_accepted",
        json!({"command_id": asked["id"], "result": {"commands": []}}),
    );
    assert!(app.on_line(answer).is_empty());
}

/// The id of the single line `effect` sends, parsed.
fn sent_id(effect: Effect) -> (String, serde_json::Value) {
    let Effect::Send(lines) = effect else {
        panic!("expected one line, got {effect:?}");
    };
    assert_eq!(lines.len(), 1, "{lines:?}");
    let line: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    let id = line["id"].as_str().unwrap_or_default().to_owned();
    assert!(id.starts_with("c_"), "{id}");
    (id, line)
}

#[test]
fn a_find_send_registers_find() {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(40, 10);
    app.on_line(hello());
    app.attach(contract::SessionId(SESSION.to_owned()));
    let mut seq = 0u64;
    find_text_turn(&mut app, &mut seq, &["a needle here"]);
    for turn in 0..16 {
        let filler: Vec<String> = (0..4).map(|line| format!("filler {turn} {line}")).collect();
        let refs: Vec<&str> = filler.iter().map(String::as_str).collect();
        find_text_turn(&mut app, &mut seq, &refs);
    }
    assert!(app.pages().page_count() > 1);
    assert!(app.pages().part(0).is_none(), "the first page dropped");
    assert_eq!(app.on_key(crate::keys::Key::CtrlF, now()), Effect::None);
    let mut generation = 0u64;
    for ch in "needle".chars() {
        generation += 1;
        assert_eq!(
            app.on_key(crate::keys::Key::Char(ch), now()),
            Effect::FindPause {
                generation,
                after: crate::app::find::FIND_PAUSE,
            }
        );
    }
    let mut first = app.find_due(generation);
    first.extend(app.find_outgoing());
    assert_eq!(first.len(), 1, "one fetch on the wire");
    let fetch: serde_json::Value = serde_json::from_str(&first[0]).unwrap();
    assert_eq!(fetch["command"], "history");
    let id = fetch["id"].as_str().unwrap_or_default();
    assert_eq!(app.outbox.waiting(id), Some(Waiting::Find));
}

/// A turn of `replies` text replies, seqs from `seq`.
fn find_text_turn(app: &mut App, seq: &mut u64, replies: &[&str]) {
    *seq += 1;
    find_numbered(
        app,
        seq,
        "turn_started",
        json!({"input": [{"type": "message", "source": "driver",
        "content": [{"type": "text", "text": "go"}]}]}),
        true,
    );
    for reply in replies {
        *seq += 1;
        find_numbered(app, seq, "text_completed", json!({"text": reply}), false);
    }
    *seq += 1;
    find_numbered(
        app,
        seq,
        "turn_completed",
        json!({"outcome": "completed"}),
        false,
    );
}

/// Folds one durable session line, answering a `commands` ask with no rows.
fn find_numbered(
    app: &mut App,
    seq: &mut u64,
    kind: &str,
    payload: serde_json::Value,
    commands: bool,
) {
    let line = crate::link::Line::Session(contract::Envelope {
        kind: contract::events::EventKind::from_name(kind),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: Some(contract::Seq(*seq)),
        payload: payload.as_object().cloned().unwrap_or_default(),
    });
    let mut rest = Vec::new();
    let mut asked = Vec::new();
    for sent in app.on_line(line) {
        let value: serde_json::Value = serde_json::from_str(&sent).unwrap();
        if value["command"] == "commands" {
            asked.push(value);
        } else {
            rest.push(sent);
        }
    }
    assert!(rest.is_empty(), "{rest:?}");
    if commands {
        assert_eq!(asked.len(), 1, "{asked:?}");
        let answer = crate::link::Line::Session(contract::Envelope {
            kind: contract::events::EventKind::from_name("command_accepted"),
            session_id: contract::SessionId(SESSION.to_owned()),
            ts: 0,
            schema_version: contract::SCHEMA_VERSION,
            turn_id: None,
            action_id: None,
            seq: None,
            payload: json!({"command_id": asked[0]["id"], "result": {"commands": []}})
                .as_object()
                .cloned()
                .unwrap_or_default(),
        });
        assert!(app.on_line(answer).is_empty());
    } else {
        assert!(asked.is_empty(), "{asked:?}");
    }
}

#[test]
fn a_commands_send_registers_commands() {
    let mut app = fresh();
    app.attach(contract::SessionId(SESSION.to_owned()));
    let session = contract::SessionId(SESSION.to_owned());
    let line: serde_json::Value = serde_json::from_str(&app.ask_commands(&session)).unwrap();
    assert_eq!(line["command"], "commands");
    let id = line["id"].as_str().unwrap_or_default();
    assert_eq!(app.outbox.waiting(id), Some(Waiting::Commands));
}

#[test]
fn a_model_send_registers_model() {
    let mut app = fresh();
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.on_line(hello());
    let choice = crate::model_picker::Choice {
        reference: "acme/m1".to_owned(),
        level: None,
        level_chosen: false,
        save_model: false,
        session_only: true,
    };
    let (id, line) = sent_id(app.choose(choice));
    assert_eq!(line["command"], "model");
    assert_eq!(app.outbox.waiting(&id), Some(Waiting::Model));
}

#[test]
fn a_reply_send_registers_reply() {
    use crate::app::commands::commands_testkit::attached;
    let mut app = attached();
    fold_session_line(
        &mut app,
        session_envelope(
            "interaction_requested",
            json!({"request_id": "r_c", "kind": "confirm", "prompt": "Continue?",
                "action_ids": ["a_1"]}),
        ),
    );
    let (id, line) = sent_id(app.on_key(crate::keys::Key::Enter, now()));
    assert_eq!(line["command"], "reply");
    assert_eq!(app.outbox.waiting(&id), Some(Waiting::Reply));
}

#[test]
fn an_offer_file_send_registers_offer_file() {
    use crate::app::commands::commands_testkit::attached;
    let mut app = attached();
    app.set_size(80, 24);
    fold_session_line(
        &mut app,
        session_envelope(
            "repository_code_offered",
            json!({"request_id": "r_1", "items": [{"kind": "hook", "name": "db",
                "hash": "h", "required": false, "summary": "hook",
                "files": ["tools/db.sh", "tools/b.sh"]}]}),
        ),
    );
    assert!(app.offer_open());
    let (id, line) = sent_id(app.on_key(crate::keys::Key::Enter, now()));
    assert_eq!(line["command"], "offered_file");
    assert_eq!(app.outbox.waiting(&id), Some(Waiting::OfferFile));
}

#[test]
fn an_offer_send_registers_reply() {
    use crate::app::commands::commands_testkit::attached;
    let mut app = attached();
    fold_session_line(
        &mut app,
        session_envelope(
            "repository_code_offered",
            json!({"request_id": "r_1", "items": [
                {"kind": "mcp_server", "name": "a", "hash": "h", "required": false,
                    "summary": "MCP server: a"},
                {"kind": "mcp_server", "name": "b", "hash": "h", "required": false,
                    "summary": "MCP server: b"},
                {"kind": "mcp_server", "name": "c", "hash": "h", "required": false,
                    "summary": "MCP server: c"}]}),
        ),
    );
    assert!(app.offer_open());
    assert_eq!(app.on_edit(crate::keys::Edit::Left), Effect::None);
    assert_eq!(app.on_key(crate::keys::Key::Down, now()), Effect::None);
    assert_eq!(app.on_key(crate::keys::Key::Down, now()), Effect::None);
    assert_eq!(app.on_edit(crate::keys::Edit::Right), Effect::None);
    assert_eq!(app.on_key(crate::keys::Key::Down, now()), Effect::None);
    let (id, line) = sent_id(app.on_key(crate::keys::Key::Enter, now()));
    assert_eq!(line["command"], "reply");
    assert_eq!(app.outbox.waiting(&id), Some(Waiting::Reply));
}

#[test]
fn a_steer_send_registers_steer() {
    use crate::app::commands::commands_testkit::attached;
    let mut app = attached();
    fold_session_line(
        &mut app,
        session_envelope(
            "steering_queue",
            json!({"messages": [
                {"content": [{"type": "text", "text": "one"}],
                    "source": "driver", "command_id": "c_1"},
                {"content": [{"type": "text", "text": "two"}],
                    "source": "driver", "command_id": "c_2"}]}),
        ),
    );
    app.select_steering(0);
    let Effect::Send(lines) = app.on_key(crate::keys::Key::Enter, now()) else {
        panic!("Enter amends");
    };
    assert_eq!(lines.len(), 2);
    let lines: Vec<serde_json::Value> = lines
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    assert_eq!(lines[0]["command"], "steer_drop");
    assert_eq!(lines[1]["command"], "steer");
    let id = lines[1]["id"].as_str().unwrap_or_default();
    assert_eq!(app.outbox.waiting(id), Some(Waiting::Steer));
}

#[test]
fn a_delegate_steer_send_registers_steer() {
    use crate::app::items::testkit::DELEGATE_A;
    use crate::app::items::testkit::{commands, home, opened, opened_item, start_delegate};
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    opened_item(&mut app, "j_1");
    app.draft.set("focus on the tests");
    let out = match app.on_key(crate::keys::Key::Enter, now()) {
        Effect::Send(lines) => commands(lines),
        Effect::SendTracked(sents) => commands(sents.into_iter().map(|sent| sent.line).collect()),
        Effect::None
        | Effect::Quit
        | Effect::ListFiles
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::ExitTracked(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_)
        | Effect::OpenFile(_)
        | Effect::ReadImage(_)
        | Effect::Feedback { .. } => panic!("Enter steers the delegate"),
    };
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["command"], "steer");
    assert_eq!(out[0]["session_id"], DELEGATE_A);
    let id = out[0]["id"].as_str().unwrap_or_default();
    assert_eq!(app.outbox.waiting(id), Some(Waiting::Steer));
}

#[test]
fn a_shell_send_registers_shell() {
    use crate::app::commands::commands_testkit::{attached, enter, sent};
    let mut app = attached();
    let out = sent(enter(&mut app, "!ls"));
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["command"], "shell");
    let id = out[0]["id"].as_str().unwrap_or_default();
    assert_eq!(app.outbox.waiting(id), Some(Waiting::Shell));
    // A plain prompt registers no tag: the `Shell` registration fires
    // only on the `!` send.
    let out = sent(enter(&mut app, "hi"));
    let id = out[0]["id"].as_str().unwrap_or_default();
    assert_eq!(app.outbox.waiting(id), None);
}

#[test]
fn an_unknown_id_falls_through_to_the_generic_path() {
    use crate::app::commands::commands_testkit::{attached, hub_line, session_line};
    let mut app = attached();
    // An accept and a refusal for ids nobody waits on settle their
    // pending entries and fold nothing: the routers run their owners'
    // handlers only for registered waits.
    app.outbox
        .pending
        .insert("c_unknown".to_owned(), (Kind::Command, Draft::default()));
    let out = app.on_line(hub_line(
        "command_accepted",
        json!({"command_id": "c_unknown", "session_id": SESSION}),
    ));
    assert!(out.is_empty());
    assert!(!app.outbox.pending.contains_key("c_unknown"));
    app.outbox
        .pending
        .insert("c_quiet".to_owned(), (Kind::Command, Draft::default()));
    let out = app.on_line(session_line(
        "command_accepted",
        json!({"command_id": "c_quiet"}),
    ));
    assert!(out.is_empty());
    assert!(!app.outbox.pending.contains_key("c_quiet"));
    app.outbox
        .pending
        .insert("c_noisy".to_owned(), (Kind::Command, Draft::default()));
    let out = app.on_line(session_line(
        "command_rejected",
        json!({"command_id": "c_noisy", "code": "busy", "message": "no"}),
    ));
    assert!(out.is_empty());
    assert!(!app.outbox.pending.contains_key("c_noisy"));
    assert_eq!(app.notice(), Some("no"));
}

#[test]
fn a_second_answer_for_one_id_changes_nothing() {
    use crate::app::commands::commands_testkit::attached;
    let mut app = attached();
    fold_session_line(
        &mut app,
        session_envelope(
            "interaction_requested",
            json!({"request_id": "r_c", "kind": "confirm", "prompt": "Continue?",
                "action_ids": ["a_1"]}),
        ),
    );
    // Declining answers with a `reply` the accept then cancels.
    let effect = app.on_click(crate::mouse::TargetId::Form(
        crate::approvals::form::Spot::Chat,
    ));
    let (id, line) = sent_id(effect);
    assert_eq!(line["command"], "reply");
    // The first answer runs the reply handler: its cancel goes out
    // and the wait settles with the entry.
    let answer = || session_envelope("command_accepted", json!({"command_id": id}));
    let first = app.on_line(answer());
    assert_eq!(first.len(), 1);
    assert_eq!(app.outbox.waiting(&id), None);
    // The wait is consumed: a second answer for the id finds no tag
    // and falls through without sending another cancel.
    let second = app.on_line(answer());
    assert!(second.is_empty());
    assert_eq!(app.outbox.waiting(&id), None);
}

#[test]
fn hub_pairs_live_exactly_as_long_as_their_waits() {
    let mut outbox = up();
    let id = contract::CommandId("c_1".to_owned());
    outbox.wait("c_1", Waiting::Feed);
    outbox.note_hub_sent(&id, "line");
    assert_eq!(outbox.hub_id("line"), Some(id.clone()));
    // Settling the wait purges the pair: a later failure parses.
    outbox.settled("c_1");
    assert_eq!(outbox.hub_id("line"), None);
}

#[test]
fn settle_where_and_forget_purge_their_hub_pairs() {
    let mut outbox = up();
    let scoped = contract::CommandId("c_s1".to_owned());
    outbox.wait("c_s1", Waiting::Recent(RecentAsk::Scoped("p1".to_owned())));
    outbox.note_hub_sent(&scoped, "scoped one");
    let first = contract::CommandId("c_first".to_owned());
    outbox.wait("c_first", Waiting::Recent(RecentAsk::First));
    outbox.note_hub_sent(&first, "first");
    // The disconnect path forgets abandoned waits the same way.
    let abandoned = contract::CommandId("c_gone".to_owned());
    outbox.wait("c_gone", Waiting::Abandoned);
    outbox.note_hub_sent(&abandoned, "gone");
    outbox.settle_where(|owner| matches!(owner, Waiting::Recent(RecentAsk::Scoped(_))));
    assert_eq!(outbox.hub_id("scoped one"), None);
    assert_eq!(outbox.hub_id("first"), Some(first));
    outbox.forget(&Waiting::Recent(RecentAsk::First));
    assert_eq!(outbox.hub_id("first"), None);
    outbox.forget(&Waiting::Abandoned);
    assert_eq!(outbox.hub_id("gone"), None);
}
