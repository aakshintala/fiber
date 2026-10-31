//! Tests for the hooks install offer: the refusal it opens on, the
//! query it runs, the choices it takes, and the paths it never reaches
//! (`docs/extensions.md`, "Hooks declared in configuration").
//!
//! Every journey drives the public input paths, the way the loop feeds
//! them: typing and keys through `on_key`, edits through `on_edit`,
//! clicks through `on_click`, and hub lines through `on_line`. Worker
//! results fold through the same `on_hooks_offer_names` and
//! `on_hooks_installed` the loop calls.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};

use crate::app::testkit::{hello, now};
use crate::app::{App, Effect, Kind, NoticeLine, Phase, Surface};
use crate::configure::{Configure, ConfigureError};
use crate::configure_fake::Fake;
use crate::home::Launch;
use crate::keys::{Button, Edit, Key, Mouse, MouseKind};
use crate::link::Line;
use crate::mouse::{Pointer, TargetId};
use contract::ErrorCode;

/// The hub's missing-hooks refusal: the only one that opens the offer.
const MISSING: &str =
    "The `hooks` extension is not installed. Run `fiber extension install hooks`.";

fn fake() -> Arc<Fake> {
    Arc::new(Fake::new(Vec::new()))
}

/// A local home at "/w" with a model on the chip, 80x24, and `seam`.
fn home(seam: Option<Arc<Fake>>) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        hover: true,
        version: "0.0.1".to_owned(),
        model: Some("test/model".to_owned()),
        thinking: None,
        logo_glyph: "⌇".to_owned(),
        keys: crate::KeysSetup::default(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: Vec::new(),
        ..Default::default()
    });
    app.set_size(80, 24);
    app.set_configure(seam.map(|seam| seam as Arc<dyn Configure>));
    app
}

/// A hub `command_rejected` for `id`, with `code` and `message`.
fn refused(id: &str, code: &str, message: &str) -> Line {
    Line::Hub(contract::HubLine {
        kind: "command_rejected".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: [
            ("command_id".to_owned(), Value::String(id.to_owned())),
            ("code".to_owned(), Value::String(code.to_owned())),
            ("message".to_owned(), Value::String(message.to_owned())),
        ]
        .into_iter()
        .collect(),
    })
}

/// A hub `command_accepted` for `id` opening `session`.
fn accepted(id: &str, session: &str) -> Line {
    Line::Hub(contract::HubLine {
        kind: "command_accepted".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: [
            ("command_id".to_owned(), Value::String(id.to_owned())),
            ("result".to_owned(), json!({"session_id": session})),
        ]
        .into_iter()
        .collect(),
    })
}

/// An idle `session_status` for `session`, so `/clear` sends its start.
fn status(session: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: json!({
            "name": "old work", "workspace": "/w", "project": "-w",
            "state": "idle", "started": 0, "calls": 0, "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    })
}

/// Types `text` and presses Enter: the one sent start's id.
#[track_caller]
fn send_start(app: &mut App, text: &str) -> String {
    for ch in text.chars() {
        app.on_key(Key::Char(ch), now());
    }
    let Effect::Send(lines) = app.on_key(Key::Enter, now()) else {
        panic!("Enter sends the start");
    };
    assert_eq!(lines.len(), 1);
    let line: Value = serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("start: {err}"));
    assert_eq!(line["command"], "start");
    line["id"].as_str().expect("the start has an id").to_owned()
}

/// Sends a start, refuses it for missing blocking hooks, and leaves
/// the offer's names query queued: the refused start's id.
#[track_caller]
fn refuse_start(app: &mut App, text: &str) -> String {
    app.refuse_start_for_offer(text)
        .expect("the offer holds the refusal")
}

/// The notices as (code, text) pairs, in order.
/// A session line for `session` with its kind, sequence, action and payload.
fn session_line_for(
    session: &str,
    kind: &str,
    seq: u64,
    action: Option<&str>,
    payload: Value,
) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: seq,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|action| contract::ActionId(action.to_owned())),
        seq: Some(contract::Seq(seq)),
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// A turn's lines with one requested call, from `seq`.
fn call_turn(session: &str, seq: &mut u64) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut push = |kind: &str, action: Option<&str>, payload: Value| {
        lines.push(session_line_for(session, kind, *seq, action, payload));
        *seq += 1;
    };
    push(
        "turn_started",
        None,
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "prompt"}]}]}),
    );
    push("step_started", None, json!({}));
    push("assistant_message_started", Some("m_1"), json!({}));
    push(
        "tool_call_requested",
        Some("c_1"),
        json!({"name": "read", "arguments": {}}),
    );
    push(
        "assistant_message_completed",
        Some("m_1"),
        json!({"outcome": "completed"}),
    );
    push("turn_completed", None, json!({"outcome": "completed"}));
    lines
}

fn notices(app: &App) -> Vec<(String, String)> {
    app.notices()
        .into_iter()
        .filter_map(|line| match line {
            NoticeLine::Notice { code, text, .. } => Some((code, text)),
            NoticeLine::More(_) => None,
        })
        .collect()
}

/// The refused start behind `refuse_start`, and no other pending start.
#[track_caller]
fn pending_start(app: &App, id: &str) {
    assert!(
        matches!(app.outbox.pending.get(id), Some((Kind::Start, _))),
        "the refused start waits"
    );
}

#[test]
fn only_the_exact_missing_hooks_refusal_opens_the_offer() {
    // A provider's missing extension names another install: no offer.
    let mut app = home(Some(fake()));
    app.on_line(hello());
    let id = send_start(&mut app, "hi");
    let lines = app.on_line(refused(
        &id,
        "extension_missing",
        "The `acme` extension is not installed. Run `fiber extension install acme`.",
    ));
    assert!(lines.is_empty());
    assert!(
        !app.hooks_offer_open(),
        "another extension is not the offer"
    );
    assert_eq!(
        notices(&app),
        [(
            "extension_missing".to_owned(),
            "The `acme` extension is not installed. Run `fiber extension install acme`.".to_owned()
        )]
    );

    // The right message under the wrong code is not the offer either.
    let mut app = home(Some(fake()));
    app.on_line(hello());
    let id = send_start(&mut app, "hi");
    app.on_line(refused(&id, "config_invalid", MISSING));
    assert!(!app.hooks_offer_open(), "config_invalid stays a refusal");
    assert_eq!(
        notices(&app),
        [("config_invalid".to_owned(), MISSING.to_owned())]
    );

    // A refused prompt is not a refused start: no offer.
    let mut app = home(Some(fake()));
    app.on_line(hello());
    let id = send_start(&mut app, "hi");
    app.on_line(accepted(&id, "s_aaaaaaaaaaaaaaaa"));
    let line = json!({
        "id": "c_prompt",
        "command": "prompt",
        "session_id": "s_aaaaaaaaaaaaaaaa",
        "args": {},
    })
    .to_string();
    app.outbox.insert(
        "c_prompt".to_owned(),
        Kind::Prompt,
        crate::input::Draft::default(),
    );
    let _ = line;
    app.on_line(refused("c_prompt", "extension_missing", MISSING));
    assert!(!app.hooks_offer_open(), "a refused prompt is not the offer");

    // The exact refusal opens the offer and queues its names query.
    let seam = fake();
    let mut app = home(Some(Arc::clone(&seam)));
    app.on_line(hello());
    let id = refuse_start(&mut app, "hi");
    pending_start(&app, &id);
}

#[test]
fn no_seam_leaves_the_refusal_on_its_existing_path() {
    let mut app = home(None);
    app.on_line(hello());
    let id = send_start(&mut app, "hi");
    app.on_line(refused(&id, "extension_missing", MISSING));
    assert!(!app.hooks_offer_open(), "no seam runs no query");
    assert_eq!(
        notices(&app),
        [("extension_missing".to_owned(), MISSING.to_owned())]
    );
}

#[test]
fn empty_names_and_query_errors_settle_the_original_refusal() {
    for result in [
        Ok(Vec::new()),
        Err(ConfigureError {
            code: ErrorCode::IoFailed,
            message: "the names could not be read".to_owned(),
        }),
    ] {
        let mut app = home(Some(fake()));
        app.on_line(hello());
        let id = refuse_start(&mut app, "hi");
        let job = app.take_hooks_job().expect("the query waits");
        let ticket = match job {
            crate::install_worker::HooksJob::Query(query) => {
                assert_eq!(query.workspace, PathBuf::from("/w"));
                assert!(query.overrides.is_empty());
                query.ticket
            }
            crate::install_worker::HooksJob::Install(_) => panic!("the refusal queues names"),
        };
        app.on_hooks_offer_names(ticket, result);
        assert!(!app.hooks_offer_open(), "no names is no offer");
        assert!(app.take_hooks_job().is_none());
        // The original refusal settles once, through its existing path:
        // one notice, the draft back, and Starting again.
        assert_eq!(
            notices(&app),
            [("extension_missing".to_owned(), MISSING.to_owned())]
        );
        assert_eq!(app.draft(), "hi");
        assert!(matches!(app.phase, Phase::Starting));
        assert!(!app.outbox.pending.contains_key(&id));
        assert!(
            app.outbox.start_line(&id).is_none(),
            "the retained start settles"
        );
    }
}

#[test]
fn names_open_the_offer_and_stale_results_do_nothing() {
    let mut app = home(Some(fake()));
    app.on_line(hello());
    let id = refuse_start(&mut app, "hi");
    let ticket = match app.take_hooks_job().expect("the query waits") {
        crate::install_worker::HooksJob::Query(query) => query.ticket,
        crate::install_worker::HooksJob::Install(_) => panic!("the refusal queues names"),
    };
    app.on_hooks_offer_names(ticket, Ok(vec!["zeta".to_owned(), "alpha".to_owned()]));
    assert!(app.hooks_offer_open());
    assert_eq!(
        app.hooks_offer_names(),
        Some(vec!["zeta".to_owned(), "alpha".to_owned()])
    );
    // A duplicate delivery and a foreign ticket change nothing: no
    // second query, no retry, no close.
    app.on_hooks_offer_names(ticket, Ok(vec!["other".to_owned()]));
    assert_eq!(
        app.hooks_offer_names(),
        Some(vec!["zeta".to_owned(), "alpha".to_owned()])
    );
    app.on_hooks_offer_names(ticket.wrapping_add(1), Ok(vec!["other".to_owned()]));
    assert_eq!(
        app.hooks_offer_names(),
        Some(vec!["zeta".to_owned(), "alpha".to_owned()])
    );
    pending_start(&app, &id);
    assert!(notices(&app).is_empty(), "the open offer shows no refusal");
}

#[test]
fn esc_cancels_without_starting_a_session() {
    let mut app = home(Some(fake()));
    app.on_line(hello());
    let id = refuse_start(&mut app, "hi");
    let ticket = match app.take_hooks_job().expect("the query waits") {
        crate::install_worker::HooksJob::Query(query) => query.ticket,
        crate::install_worker::HooksJob::Install(_) => panic!("the refusal queues names"),
    };
    app.on_hooks_offer_names(ticket, Ok(vec!["fmt".to_owned()]));
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(!app.hooks_offer_open());
    // Exactly one original refusal, the draft back, and Starting again:
    // no session starts, no hook point runs, no close goes out.
    assert_eq!(
        notices(&app),
        [("extension_missing".to_owned(), MISSING.to_owned())]
    );
    assert_eq!(app.draft(), "hi");
    assert!(matches!(app.phase, Phase::Starting));
    assert!(app.session().is_none());
    assert!(!app.outbox.pending.contains_key(&id));
}

#[test]
fn the_cancel_choice_cancels_through_a_click() {
    let mut app = home(Some(fake()));
    app.on_line(hello());
    refuse_start(&mut app, "hi");
    let ticket = match app.take_hooks_job().expect("the query waits") {
        crate::install_worker::HooksJob::Query(query) => query.ticket,
        crate::install_worker::HooksJob::Install(_) => panic!("the refusal queues names"),
    };
    app.on_hooks_offer_names(ticket, Ok(vec!["fmt".to_owned()]));
    assert_eq!(
        app.on_click(TargetId::Home(crate::home::Spot::HooksOffer(
            crate::home::HooksChoice::Cancel
        ))),
        Effect::None
    );
    assert!(!app.hooks_offer_open());
    assert_eq!(
        notices(&app),
        [("extension_missing".to_owned(), MISSING.to_owned())]
    );
    assert_eq!(app.draft(), "hi");
}

#[test]
fn a_click_outside_declines_through_esc() {
    let mut app = home(Some(fake()));
    app.on_line(hello());
    refuse_start(&mut app, "hi");
    let ticket = match app.take_hooks_job().expect("the query waits") {
        crate::install_worker::HooksJob::Query(query) => query.ticket,
        crate::install_worker::HooksJob::Install(_) => panic!("the refusal queues names"),
    };
    app.on_hooks_offer_names(ticket, Ok(vec!["fmt".to_owned()]));
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    let slab = crate::view::frame_of(&app, Surface::HooksOffer);
    assert!(!slab.is_empty(), "the offer draws its frame");
    let mut pointer = Pointer::default();
    let press = Mouse {
        kind: MouseKind::Press(Button::Left),
        col: 0,
        row: 0,
    };
    assert!(!slab.contains(ratatui::layout::Position::new(0, 0)));
    assert_eq!(
        app.on_mouse(&press, &targets, &mut pointer, true, now()),
        Effect::None
    );
    assert!(!app.hooks_offer_open(), "the outside press declines");
    assert_eq!(
        notices(&app),
        [("extension_missing".to_owned(), MISSING.to_owned())]
    );
}

#[test]
fn clear_staged_decline_stays_attached_and_sends_no_close() {
    let session = "s_aaaaaaaaaaaaaaaa";
    let mut app = home(Some(fake()));
    app.on_line(hello());
    app.attach(contract::SessionId(session.to_owned()));
    app.on_line(status(session));
    refuse_start(&mut app, "");
    assert!(app.hooks_offer_open(), "the clear-staged refusal offers");
    // The phase stays attached to the old session while the offer holds.
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(session)
    );
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(!app.hooks_offer_open());
    // Exactly one original refusal through the clear path: attached
    // still, no close, and the draft the stage cleared stays cleared.
    assert_eq!(
        notices(&app),
        [("extension_missing".to_owned(), MISSING.to_owned())]
    );
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(session)
    );
    assert!(
        app.clear_pending_command_id().is_none(),
        "the clear settles"
    );
}

#[test]
fn paste_over_a_clear_staged_offer_keeps_the_retained_draft() {
    let session = "s_aaaaaaaaaaaaaaaa";
    let mut app = home(Some(fake()));
    app.on_line(hello());
    app.attach(contract::SessionId(session.to_owned()));
    app.on_line(status(session));
    let Effect::Send(_) = app.clear() else {
        panic!("clear sends its start");
    };
    // The stage cleared the draft; the person types behind the refusal.
    for ch in "kept".chars() {
        app.on_key(Key::Char(ch), now());
    }
    assert_eq!(app.draft(), "kept");
    refuse_start(&mut app, "");
    assert!(app.hooks_offer_open());
    // The open offer consumes the paste, and the draft stays as it was.
    assert_eq!(app.on_edit(Edit::Paste("pasted".to_owned())), Effect::None);
    assert_eq!(app.draft(), "kept");
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert_eq!(app.draft(), "kept", "cancel keeps the retained draft");
    assert_eq!(
        notices(&app),
        [("extension_missing".to_owned(), MISSING.to_owned())]
    );
}

#[test]
fn keys_edits_and_clicks_hold_while_the_names_query_runs() {
    for clear in [false, true] {
        let session = "s_aaaaaaaaaaaaaaaa";
        let mut app = home(Some(fake()));
        app.on_line(hello());
        if clear {
            app.attach(contract::SessionId(session.to_owned()));
            app.on_line(status(session));
        }
        refuse_start(&mut app, "hi");
        assert!(app.hooks_offer_open(), "clear={clear}: the query runs");
        // A second Enter cannot start, cancel or alter the wait.
        assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
        // The query is still the one job: Enter queued nothing.
        assert!(app.hooks_job.is_some(), "clear={clear}: the query waits");
        let ticket = match app.take_hooks_job().expect("the query waits") {
            crate::install_worker::HooksJob::Query(query) => query.ticket,
            crate::install_worker::HooksJob::Install(_) => panic!("the refusal queues names"),
        };
        // Edits hold too, without reaching the draft.
        assert_eq!(app.on_edit(Edit::Paste("pasted".to_owned())), Effect::None);
        // Clicks on the rows beneath hold as well.
        assert_eq!(
            app.on_click(TargetId::Home(crate::home::Spot::Question)),
            Effect::None
        );
        if clear {
            assert_eq!(
                app.session().map(|session| session.0.as_str()),
                Some(session),
                "clear={clear}: still attached"
            );
        } else {
            assert!(matches!(app.phase, Phase::Pending { .. }));
        }
        // Answering the query still opens the offer afterwards.
        app.on_hooks_offer_names(ticket, Ok(vec!["fmt".to_owned()]));
        assert!(app.hooks_offer_open());
    }
}

#[test]
fn presses_hold_while_the_names_query_runs_over_an_attached_session() {
    let session = "s_aaaaaaaaaaaaaaaa";
    let mut app = home(Some(fake()));
    app.on_line(hello());
    app.attach(contract::SessionId(session.to_owned()));
    app.on_line(status(session));
    // One requested call: the conversation draws a line target.
    let mut seq = 0u64;
    for line in call_turn(session, &mut seq) {
        app.on_line(line);
    }
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    let mut before = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut before, None);
    let target = targets
        .iter()
        .find(|target| matches!(target.id, TargetId::Line(_)))
        .copied()
        .expect("a line target draws");
    // The refusal opens the offer; its query draws no frame, so the
    // frames hold none for it.
    refuse_start(&mut app, "");
    assert!(app.hooks_offer_names().is_none(), "the query runs");
    // Pressing and releasing the line beneath lands nowhere: no output
    // opens, and the next frame draws what this one drew.
    let mut pointer = Pointer::default();
    let col = target.rect.x + target.rect.width / 2;
    let row = target.rect.y + target.rect.height / 2;
    assert_eq!(
        app.on_mouse(
            &Mouse {
                kind: MouseKind::Press(Button::Left),
                col,
                row
            },
            &targets,
            &mut pointer,
            true,
            now()
        ),
        Effect::None
    );
    assert_eq!(
        app.on_mouse(
            &Mouse {
                kind: MouseKind::Release,
                col,
                row
            },
            &targets,
            &mut pointer,
            true,
            now()
        ),
        Effect::None
    );
    let mut after = ratatui::buffer::Buffer::empty(area);
    crate::view::render(&app, area, &mut after, None);
    assert_eq!(
        crate::view::text(&before),
        crate::view::text(&after),
        "the press changes no drawn row"
    );
    assert!(app.hooks_offer_open(), "the query still runs");
}

#[test]
fn accepting_resends_the_retained_start_unchanged() {
    let mut app = home(Some(fake()));
    app.launch_overrides = vec!["z=1".to_owned(), "a=2".to_owned(), "z=1".to_owned()];
    app.on_line(hello());
    let old = refuse_start(&mut app, "hi");
    // The query repeats the start's workspace with its exact ordered
    // overrides, repeated keys kept.
    let ticket = match app.take_hooks_job().expect("the query waits") {
        crate::install_worker::HooksJob::Query(query) => {
            assert_eq!(query.workspace, PathBuf::from("/w"));
            assert_eq!(
                query.overrides,
                vec!["z=1".to_owned(), "a=2".to_owned(), "z=1".to_owned()]
            );
            query.ticket
        }
        crate::install_worker::HooksJob::Install(_) => panic!("the refusal queues names"),
    };
    let first = app.outbox.start_line(&old).expect("the start is retained");
    let first: Value = serde_json::from_str(&first).expect("the retained start");
    app.on_hooks_offer_names(ticket, Ok(vec!["fmt".to_owned()]));
    // An install answer for another ticket while the query's offer
    // waits does nothing: without the ticket check it would send a
    // spare start for an install nobody committed.
    assert!(
        app.on_hooks_installed(ticket.wrapping_add(1), Ok(()))
            .is_empty()
    );
    assert!(app.hooks_offer_open());
    // Enter commits the install: the job waits, and nothing goes out yet.
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(matches!(
        app.take_hooks_job(),
        Some(crate::install_worker::HooksJob::Install(_))
    ));
    assert!(app.hooks_offer_open(), "the offer waits on the install");
    // Success resends the retained args unchanged under a fresh id, and
    // the hub reruns its gate: the args match the first send exactly.
    let lines = app.on_hooks_installed(ticket, Ok(()));
    assert_eq!(lines.len(), 1);
    let retry: Value = serde_json::from_str(&lines[0]).expect("the retry");
    assert_eq!(retry["command"], "start");
    assert_eq!(retry["args"], first["args"]);
    assert_ne!(retry["id"], first["id"]);
    let new = retry["id"]
        .as_str()
        .expect("the retry has an id")
        .to_owned();
    assert!(app.outbox.pending.contains_key(&new));
    assert!(!app.outbox.pending.contains_key(&old));
    assert!(
        app.outbox.start_line(&new).is_some(),
        "the retry is retained"
    );
    assert!(app.outbox.start_line(&old).is_none());
    assert_eq!(
        app.phase,
        Phase::Pending {
            command_id: new.clone(),
        }
    );
    // The new start's answer opens the session, as any start's does:
    // attach, commands, then the pending draft as its first prompt.
    let out = app.on_line(accepted(&new, "s_aaaaaaaaaaaaaaaa"));
    // The new start's answer opens the session, as any start's does,
    // with the pending draft as its first prompt: the lines carry it,
    // with no promised count or order.
    assert!(
        out.iter().any(|line| line.contains("hi")),
        "the draft prompts first"
    );
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some("s_aaaaaaaaaaaaaaaa")
    );
    // No second result for the ticket can retry: the late success and a
    // stale ticket change nothing.
    assert!(app.on_hooks_installed(ticket, Ok(())).is_empty());
    assert!(
        app.on_hooks_installed(ticket.wrapping_add(1), Ok(()))
            .is_empty()
    );
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some("s_aaaaaaaaaaaaaaaa")
    );
}

#[test]
fn clear_staged_accept_closes_the_old_session_with_its_credential() {
    let old_session = "s_aaaaaaaaaaaaaaaa";
    let mut app = home(Some(fake()));
    app.launch_overrides = vec!["providers.\"test\".credential=work".to_owned()];
    app.on_line(hello());
    // The first start carries the credential, so the panel records it.
    let first = send_start(&mut app, "hi");
    app.on_line(accepted(&first, old_session));
    app.on_line(status(old_session));
    app.clear();
    let staged = app
        .clear_pending_command_id()
        .expect("the clear staged a start")
        .to_owned();
    let staged_line = app
        .outbox
        .start_line(&staged)
        .expect("the stage is retained");
    let staged_args: Value = serde_json::from_str(&staged_line).expect("the stage");
    assert!(
        staged_args["args"]["overrides"]
            .as_array()
            .is_some_and(|overrides| overrides
                .iter()
                .any(|override_| override_ == "providers.\"test\".credential=work")),
        "the clear carries the credential"
    );
    app.on_line(refused(&staged, "extension_missing", MISSING));
    assert!(app.hooks_offer_open());
    let ticket = match app.take_hooks_job().expect("the query waits") {
        crate::install_worker::HooksJob::Query(query) => query.ticket,
        crate::install_worker::HooksJob::Install(_) => panic!("the refusal queues names"),
    };
    app.on_hooks_offer_names(ticket, Ok(vec!["fmt".to_owned()]));
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(matches!(
        app.take_hooks_job(),
        Some(crate::install_worker::HooksJob::Install(_))
    ));
    // The retry resends the staged args unchanged, then the accepted
    // retry closes the old session: no extra close went out before.
    let lines = app.on_hooks_installed(ticket, Ok(()));
    assert_eq!(lines.len(), 1);
    let retry: Value = serde_json::from_str(&lines[0]).expect("the retry");
    assert_eq!(retry["args"], staged_args["args"]);
    let new = retry["id"]
        .as_str()
        .expect("the retry has an id")
        .to_owned();
    assert_eq!(
        app.clear_pending_command_id(),
        Some(new.as_str()),
        "the clear follows the retry"
    );
    assert!(
        matches!(app.phase, Phase::Attached { .. }),
        "a clear retry keeps the old phase attached instead of marking the retry pending"
    );
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(old_session),
        "the staged clear stays attached until the retry answers"
    );
    let out = app.on_line(accepted(&new, "s_bbbbbbbbbbbbbbbb"));
    // The accepted retry closes the old session: the close goes out
    // with the lines, with no promised count or order.
    let close: Value = out
        .iter()
        .map(|line| serde_json::from_str::<Value>(line).expect("the close"))
        .find(|close| close["command"] == "close")
        .expect("the accepted retry closes the old session");
    assert_eq!(close["command"], "close");
    assert_eq!(close["session_id"], old_session);
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some("s_bbbbbbbbbbbbbbbb")
    );
}

#[test]
fn install_failure_replaces_the_offer_and_blocks_retry() {
    let mut app = home(Some(fake()));
    app.on_line(hello());
    let old = refuse_start(&mut app, "hi");
    let ticket = match app.take_hooks_job().expect("the query waits") {
        crate::install_worker::HooksJob::Query(query) => query.ticket,
        crate::install_worker::HooksJob::Install(_) => panic!("the refusal queues names"),
    };
    app.on_hooks_offer_names(ticket, Ok(vec!["fmt".to_owned()]));
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(matches!(
        app.take_hooks_job(),
        Some(crate::install_worker::HooksJob::Install(_))
    ));
    // The install's own failure replaces the offer: the error keeps its
    // text, naming the install command it lacked.
    let lines = app.on_hooks_installed(
        ticket,
        Err(ConfigureError {
            code: ErrorCode::ExtensionFailed,
            message: "no network".to_owned(),
        }),
    );
    assert!(lines.is_empty(), "a failure sends no start");
    assert!(!app.hooks_offer_open());
    assert_eq!(
        notices(&app),
        [(
            "extension_failed".to_owned(),
            "no network. Run `fiber extension install hooks`.".to_owned()
        )]
    );
    // An ordinary start restores its draft and phase, with no session.
    assert_eq!(app.draft(), "hi");
    assert!(matches!(app.phase, Phase::Starting));
    assert!(app.session().is_none());
    assert!(!app.outbox.pending.contains_key(&old));
    // No second result for the ticket can retry, late success included.
    assert!(app.on_hooks_installed(ticket, Ok(())).is_empty());
    assert!(
        app.on_hooks_installed(ticket.wrapping_add(1), Ok(()))
            .is_empty()
    );
    assert!(app.session().is_none());
    assert_eq!(notices(&app).len(), 1);
}

/// A worker connection loss that folds before the disconnect: the
/// install's own failure replaces the offer, and the reconnect clears
/// the link's condition while the failure stands, with no retry.
#[test]
fn install_connection_failure_survives_a_reconnect_without_retry() {
    let mut app = home(Some(fake()));
    app.on_line(hello());
    let old = refuse_start(&mut app, "hi");
    let ticket = match app.take_hooks_job().expect("the query waits") {
        crate::install_worker::HooksJob::Query(query) => query.ticket,
        crate::install_worker::HooksJob::Install(_) => panic!("the refusal queues names"),
    };
    app.on_hooks_offer_names(ticket, Ok(vec!["fmt".to_owned()]));
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(matches!(
        app.take_hooks_job(),
        Some(crate::install_worker::HooksJob::Install(_))
    ));
    // The worker's connection loss folds first: the install's own
    // failure keeps the connection detail and names the install
    // command, sending no start.
    let lines = app.on_hooks_installed(
        ticket,
        Err(ConfigureError {
            code: ErrorCode::ConnectionFailed,
            message: "Connection lost while installing hooks.".to_owned(),
        }),
    );
    assert!(lines.is_empty(), "a failure sends no start");
    assert!(!app.hooks_offer_open());
    // The dropped connection lands next, then the hub returns: the
    // reconnect clears the link's condition, but the completed
    // install failure stands, and no start goes out again.
    app.disconnected();
    app.on_line(hello());
    assert_eq!(
        notices(&app),
        [(
            "extension_failed".to_owned(),
            "Connection lost while installing hooks. Run `fiber extension install hooks`."
                .to_owned()
        )]
    );
    assert!(
        app.outbox
            .pending
            .values()
            .all(|(kind, _)| *kind != Kind::Start),
        "a reconnect replays no start"
    );
    assert!(!app.outbox.pending.contains_key(&old));
    assert!(app.session().is_none());
    // A late result for the settled ticket still changes nothing.
    assert!(app.on_hooks_installed(ticket, Ok(())).is_empty());
    assert!(app.session().is_none());
}

/// A worker clone failure that folds while the loop still reads the
/// link up, then the dropped connection: the disconnect upgrades the
/// completed install failure to the connection-loss text both orders
/// share, so the failure names the install command with the
/// connection lost and no start goes out again.
#[test]
fn disconnect_upgrades_a_worker_first_clone_failure() {
    let mut app = home(Some(fake()));
    app.on_line(hello());
    let old = refuse_start(&mut app, "hi");
    let ticket = match app.take_hooks_job().expect("the query waits") {
        crate::install_worker::HooksJob::Query(query) => query.ticket,
        crate::install_worker::HooksJob::Install(_) => panic!("the refusal queues names"),
    };
    app.on_hooks_offer_names(ticket, Ok(vec!["fmt".to_owned()]));
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(matches!(
        app.take_hooks_job(),
        Some(crate::install_worker::HooksJob::Install(_))
    ));
    // The worker's clone failure folds first, before the loop notices
    // the dead link: the install's own failure replaces the offer,
    // sending no start.
    let lines = app.on_hooks_installed(
        ticket,
        Err(ConfigureError {
            code: ErrorCode::FetchFailed,
            message: "`git clone --quiet --depth 1 --branch v0.1.0 https://github.com/aakshintala/fiber /tmp/fiber-fetch-0/hooks` failed: ".to_owned(),
        }),
    );
    assert!(lines.is_empty(), "a failure sends no start");
    assert!(!app.hooks_offer_open());
    // The dropped connection lands next: it upgrades the completed
    // failure to the connection-loss text, dropping the clone's raw
    // command line, and replays no start.
    app.disconnected();
    assert_eq!(
        notices(&app),
        [
            (
                "connection_failed".to_owned(),
                "Connection lost.".to_owned()
            ),
            (
                "extension_failed".to_owned(),
                "Connection lost while installing hooks. Run `fiber extension install hooks`."
                    .to_owned()
            )
        ]
    );
    assert!(
        app.outbox
            .pending
            .values()
            .all(|(kind, _)| *kind != Kind::Start),
        "a disconnect replays no start"
    );
    assert!(!app.outbox.pending.contains_key(&old));
    assert!(app.session().is_none());
    // A late result for the settled ticket still changes nothing.
    assert!(app.on_hooks_installed(ticket, Ok(())).is_empty());
    assert!(app.session().is_none());
}

#[test]
fn disconnect_while_installing_fails_without_retry() {
    let mut app = home(Some(fake()));
    app.on_line(hello());
    let old = refuse_start(&mut app, "hi");
    let ticket = match app.take_hooks_job().expect("the query waits") {
        crate::install_worker::HooksJob::Query(query) => query.ticket,
        crate::install_worker::HooksJob::Install(_) => panic!("the refusal queues names"),
    };
    app.on_hooks_offer_names(ticket, Ok(vec!["fmt".to_owned()]));
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(matches!(
        app.take_hooks_job(),
        Some(crate::install_worker::HooksJob::Install(_))
    ));
    // The dropped connection ends the wait: the existing lost-link
    // notice stays, then the failure names the install command with
    // the connection lost, and no retry follows.
    app.disconnected();
    assert!(!app.hooks_offer_open());
    assert!(app.take_hooks_job().is_none(), "the queued job never runs");
    assert_eq!(
        notices(&app),
        [
            (
                "connection_failed".to_owned(),
                "Connection lost.".to_owned()
            ),
            (
                "extension_failed".to_owned(),
                "Connection lost while installing hooks. Run `fiber extension install hooks`."
                    .to_owned()
            )
        ]
    );
    assert!(!app.outbox.pending.contains_key(&old));
    // A late success after the disconnect changes nothing: no start
    // goes out, no session opens, and the failure stands unchanged.
    assert!(app.on_hooks_installed(ticket, Ok(())).is_empty());
    assert!(app.session().is_none());
    assert_eq!(notices(&app).len(), 2);
}

#[test]
fn disconnect_during_the_names_query_follows_lost_link_handling() {
    let mut app = home(Some(fake()));
    app.on_line(hello());
    let old = refuse_start(&mut app, "hi");
    assert!(app.take_hooks_job().is_some(), "the query runs");
    app.disconnected();
    assert!(!app.hooks_offer_open(), "the offer closes");
    assert!(app.take_hooks_job().is_none(), "the queued job never runs");
    // The dropped link notices through the existing path.
    assert_eq!(
        notices(&app),
        [(
            "connection_failed".to_owned(),
            "Connection lost.".to_owned()
        )]
    );
    // The later answer is stale: no offer, no failure, no session.
    app.on_hooks_offer_names(1, Ok(vec!["fmt".to_owned()]));
    assert!(!app.hooks_offer_open());
    assert_eq!(notices(&app).len(), 1);
    // The refused start still waits: the reconnect settles it through
    // the existing unanswered path.
    assert!(app.outbox.pending.contains_key(&old));
    app.on_line(hello());
    // The reconnect clears the dropped-link notice, then settles the
    // still-waiting start through the existing unanswered path.
    assert_eq!(
        notices(&app),
        [(
            "connection_failed".to_owned(),
            "No answer before the connection was lost.".to_owned()
        )]
    );
    assert!(matches!(app.phase, Phase::Starting));
}

#[test]
fn keys_and_clicks_hold_while_the_install_runs() {
    let mut app = home(Some(fake()));
    app.on_line(hello());
    refuse_start(&mut app, "hi");
    let ticket = match app.take_hooks_job().expect("the query waits") {
        crate::install_worker::HooksJob::Query(query) => query.ticket,
        crate::install_worker::HooksJob::Install(_) => panic!("the refusal queues names"),
    };
    app.on_hooks_offer_names(ticket, Ok(vec!["fmt".to_owned()]));
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(matches!(
        app.take_hooks_job(),
        Some(crate::install_worker::HooksJob::Install(_))
    ));
    // While the install runs, every key and choice holds: no second
    // job, no decline, no close.
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert_eq!(
        app.on_click(TargetId::Home(crate::home::Spot::HooksOffer(
            crate::home::HooksChoice::Install
        ))),
        Effect::None
    );
    assert_eq!(
        app.on_click(TargetId::Home(crate::home::Spot::HooksOffer(
            crate::home::HooksChoice::Cancel
        ))),
        Effect::None
    );
    assert!(app.take_hooks_job().is_none(), "no second job runs");
    assert!(app.hooks_offer_open(), "the offer waits on the install");
    assert!(notices(&app).is_empty(), "nothing settles mid-install");
}

#[test]
fn a_remote_with_its_link_up_commits_the_install() {
    let mut app = home(Some(fake()));
    let router = crate::remote::Router::new();
    let remote = Arc::new(crate::remote::Remote::new(
        Arc::new(Fake::new(Vec::new())),
        "/client".into(),
        router,
    ));
    app.set_remote(Arc::clone(&remote));
    app.on_line(hello());
    let old = refuse_start(&mut app, "hi");
    let ticket = match app.take_hooks_job().expect("the query waits") {
        crate::install_worker::HooksJob::Query(query) => query.ticket,
        crate::install_worker::HooksJob::Install(_) => panic!("the refusal queues names"),
    };
    app.on_hooks_offer_names(ticket, Ok(vec!["fmt".to_owned()]));
    // The hub link is up, so Enter commits the install instead of
    // failing it for a lost connection.
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(matches!(
        app.take_hooks_job(),
        Some(crate::install_worker::HooksJob::Install(_))
    ));
    assert!(app.hooks_offer_open(), "the offer waits on the install");
    assert!(
        app.outbox.pending.contains_key(&old),
        "the refused start waits"
    );
}

#[test]
fn clear_staged_install_failure_settles_the_clear() {
    let session = "s_aaaaaaaaaaaaaaaa";
    let mut app = home(Some(fake()));
    app.on_line(hello());
    app.attach(contract::SessionId(session.to_owned()));
    app.on_line(status(session));
    let old = refuse_start(&mut app, "");
    assert!(
        app.clear_pending_command_id().is_some(),
        "the clear staged a start"
    );
    let ticket = match app.take_hooks_job().expect("the query waits") {
        crate::install_worker::HooksJob::Query(query) => query.ticket,
        crate::install_worker::HooksJob::Install(_) => panic!("the refusal queues names"),
    };
    app.on_hooks_offer_names(ticket, Ok(vec!["fmt".to_owned()]));
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(matches!(
        app.take_hooks_job(),
        Some(crate::install_worker::HooksJob::Install(_))
    ));
    // The install's own failure replaces the offer: the staged clear
    // settles with it, staying attached to the old session with no
    // close and no draft restored.
    let lines = app.on_hooks_installed(
        ticket,
        Err(ConfigureError {
            code: ErrorCode::ExtensionFailed,
            message: "no network".to_owned(),
        }),
    );
    assert!(lines.is_empty(), "a failure sends no start");
    assert!(!app.hooks_offer_open());
    assert_eq!(
        notices(&app),
        [(
            "extension_failed".to_owned(),
            "no network. Run `fiber extension install hooks`.".to_owned()
        )]
    );
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(session)
    );
    assert!(
        app.clear_pending_command_id().is_none(),
        "the clear settles"
    );
    assert!(!app.outbox.pending.contains_key(&old));
}

#[test]
fn disconnect_during_the_names_query_settles_a_clear_staged_start() {
    let session = "s_aaaaaaaaaaaaaaaa";
    let mut app = home(Some(fake()));
    app.on_line(hello());
    app.attach(contract::SessionId(session.to_owned()));
    app.on_line(status(session));
    let old = refuse_start(&mut app, "");
    assert!(
        app.clear_pending_command_id().is_some(),
        "the clear staged a start"
    );
    assert!(app.take_hooks_job().is_some(), "the query runs");
    app.disconnected();
    assert!(!app.hooks_offer_open(), "the offer closes");
    assert!(app.take_hooks_job().is_none(), "the queued job never runs");
    // The dropped link notices through the existing path, and the
    // refused clear-staged start settles silently: still attached, no
    // close, nothing waiting.
    assert_eq!(
        notices(&app),
        [(
            "connection_failed".to_owned(),
            "Connection lost.".to_owned()
        )]
    );
    assert!(
        app.clear_pending_command_id().is_none(),
        "the clear settles"
    );
    assert!(!app.outbox.pending.contains_key(&old));
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(session)
    );
}

#[test]
fn commits_without_a_seam_or_link_fail_at_once_with_no_job() {
    // No seam: the install cannot run, so the offer closes on the
    // failure naming the command instead.
    let mut app = home(Some(fake()));
    app.on_line(hello());
    refuse_start(&mut app, "hi");
    let ticket = match app.take_hooks_job().expect("the query waits") {
        crate::install_worker::HooksJob::Query(query) => query.ticket,
        crate::install_worker::HooksJob::Install(_) => panic!("the refusal queues names"),
    };
    app.on_hooks_offer_names(ticket, Ok(vec!["fmt".to_owned()]));
    app.set_configure(None);
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(app.take_hooks_job().is_none(), "no job runs without a seam");
    assert!(!app.hooks_offer_open());
    assert_eq!(
        notices(&app),
        [(
            "io_failed".to_owned(),
            "Installing is not available in this terminal. Run `fiber extension install hooks`."
                .to_owned()
        )]
    );

    // A remote whose hub link drops fails the same way, with no job.
    let mut app = home(Some(fake()));
    let router = crate::remote::Router::new();
    let remote = Arc::new(crate::remote::Remote::new(
        Arc::new(Fake::new(Vec::new())),
        "/client".into(),
        router,
    ));
    app.set_remote(Arc::clone(&remote));
    app.on_line(hello());
    let before = notices(&app).len();
    let old = refuse_start(&mut app, "hi");
    let ticket = match app.take_hooks_job().expect("the query waits") {
        crate::install_worker::HooksJob::Query(query) => query.ticket,
        crate::install_worker::HooksJob::Install(_) => panic!("the refusal queues names"),
    };
    app.on_hooks_offer_names(ticket, Ok(vec!["fmt".to_owned()]));
    app.outbox.link = crate::app::Link::Down;
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(
        app.take_hooks_job().is_none(),
        "no job runs without the link"
    );
    assert!(!app.hooks_offer_open());
    // Whatever hello through the detached remote already noticed, the
    // commit adds exactly the link failure naming the command.
    assert_eq!(
        notices(&app)[before..],
        [(
            "io_failed".to_owned(),
            "The hub is not connected. Run `fiber extension install hooks`.".to_owned()
        )]
    );
    assert!(!app.outbox.pending.contains_key(&old));
}

#[test]
fn spawn_failures_fold_as_their_own_side() {
    // A query that never starts settles the original refusal: the
    // spawn failure never becomes an install failure.
    let seam = fake();
    let mut app = home(Some(Arc::clone(&seam)));
    app.on_line(hello());
    refuse_start(&mut app, "hi");
    let ticket = match app.take_hooks_job().expect("the query waits") {
        crate::install_worker::HooksJob::Query(query) => query.ticket,
        crate::install_worker::HooksJob::Install(_) => panic!("the refusal queues names"),
    };
    app.on_hooks_offer_names(
        ticket,
        Err(ConfigureError {
            code: ErrorCode::IoFailed,
            message: "terminal: could not start the names query: none left".to_owned(),
        }),
    );
    assert!(!app.hooks_offer_open());
    assert_eq!(
        notices(&app),
        [("extension_missing".to_owned(), MISSING.to_owned())]
    );

    // An install that never starts replaces the offer with the failure
    // naming the command: no seam install runs, no start goes out.
    let seam = fake();
    let mut app = home(Some(Arc::clone(&seam)));
    app.on_line(hello());
    refuse_start(&mut app, "hi");
    let ticket = match app.take_hooks_job().expect("the query waits") {
        crate::install_worker::HooksJob::Query(query) => query.ticket,
        crate::install_worker::HooksJob::Install(_) => panic!("the refusal queues names"),
    };
    app.on_hooks_offer_names(ticket, Ok(vec!["fmt".to_owned()]));
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(matches!(
        app.take_hooks_job(),
        Some(crate::install_worker::HooksJob::Install(_))
    ));
    assert!(
        app.on_hooks_installed(
            ticket,
            Err(ConfigureError {
                code: ErrorCode::IoFailed,
                message: "terminal: could not start the install: none left".to_owned(),
            })
        )
        .is_empty()
    );
    assert_eq!(
        notices(&app),
        [(
            "io_failed".to_owned(),
            "terminal: could not start the install: none left. Run `fiber extension install hooks`.".to_owned()
        )]
    );
    assert_eq!(
        seam.hooks_installs
            .lock()
            .map(|runs| *runs)
            .unwrap_or_default(),
        0,
        "the spawn failure ran no install"
    );
    assert!(app.outbox.pending.is_empty(), "no retry went out");
}

#[test]
fn row_clicks_cannot_clobber_the_offer() {
    let mut app = home(Some(fake()));
    let lines = app.on_line(hello());
    let recent = lines
        .get(1)
        .and_then(|line| serde_json::from_str::<Value>(line).ok())
        .unwrap_or_default();
    let id = recent["id"].as_str().expect("the recent ask").to_owned();
    app.on_line(Line::Hub(contract::HubLine {
        kind: "command_accepted".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: [
            ("command_id".to_owned(), Value::String(id)),
            (
                "result".to_owned(),
                json!({"sessions": [{
                    "session_id": "s_bbbbbbbbbbbbbbbb", "ts": 0, "project": "-w",
                    "workspace": "/w", "name": "old work", "how": "exited",
                }]}),
            ),
        ]
        .into_iter()
        .collect(),
    }));
    refuse_start(&mut app, "hi");
    let ticket = match app.take_hooks_job().expect("the query waits") {
        crate::install_worker::HooksJob::Query(query) => query.ticket,
        crate::install_worker::HooksJob::Install(_) => panic!("the refusal queues names"),
    };
    app.on_hooks_offer_names(ticket, Ok(vec!["fmt".to_owned()]));
    // Deleting the exited row behind the offer ends there: no delete
    // question opens, and the offer keeps the foot.
    let screen = app.home_screen().expect("home draws its list");
    let key = crate::home::home_test_rows(&screen)
        .into_iter()
        .find(|(_, text)| text.contains("old work"))
        .map(|(key, _)| key)
        .expect("the exited row draws");
    assert_eq!(app.home_click(crate::home::Spot::Stop(key)), Effect::None);
    assert!(app.hooks_offer_open(), "the offer keeps the foot");
    assert!(
        !app.top_surfaces().contains(&Surface::DeleteQuestion),
        "no delete question opens"
    );
}

#[test]
fn the_offer_is_not_a_delete_question() {
    let mut app = home(Some(fake()));
    app.on_line(hello());
    refuse_start(&mut app, "hi");
    let ticket = match app.take_hooks_job().expect("the query waits") {
        crate::install_worker::HooksJob::Query(query) => query.ticket,
        crate::install_worker::HooksJob::Install(_) => panic!("the refusal queues names"),
    };
    app.on_hooks_offer_names(ticket, Ok(vec!["fmt".to_owned()]));
    assert!(
        !app.home_question_open(),
        "the offer is not a delete question"
    );
    let listed = app.top_surfaces();
    assert!(listed.contains(&Surface::HooksOffer), "{listed:?}");
    assert!(!listed.contains(&Surface::DeleteQuestion), "{listed:?}");
    assert_eq!(app.top_surface(), Surface::HooksOffer);
    assert_eq!(app.key_context(), crate::keyset::Context::Overlay);
    // The delete question's Enter deletes; the offer's Enter installs.
    // Driving the delete path must not touch the offer.
    assert_eq!(app.question_key(&Key::Enter), Some(Effect::None));
    assert!(app.hooks_offer_open(), "send_delete leaves the offer alone");
}

#[test]
fn the_wheel_over_the_offer_frame_moves_no_list() {
    let seam = fake();
    let mut app = home(Some(Arc::clone(&seam)));
    // Enough exited rows to scroll: the wheel over the list moves it.
    let lines = app.on_line(hello());
    let recent = lines
        .get(1)
        .and_then(|line| serde_json::from_str::<Value>(line).ok())
        .unwrap_or_default();
    let id = recent["id"].as_str().expect("the recent ask").to_owned();
    // One project per session: sections overflow the list window, so
    // the wheel past the frame has rows to move.
    let sessions: Vec<Value> = (0..40)
        .map(|at| {
            json!({
                "session_id": format!("s_{at:016}"),
                "ts": at, "project": format!("-p{at:02}"),
                "workspace": "/w", "name": format!("old {at}"), "how": "exited",
            })
        })
        .collect();
    app.on_line(Line::Hub(contract::HubLine {
        kind: "command_accepted".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: [
            ("command_id".to_owned(), Value::String(id)),
            ("result".to_owned(), json!({"sessions": sessions})),
        ]
        .into_iter()
        .collect(),
    }));
    refuse_start(&mut app, "hi");
    let ticket = match app.take_hooks_job().expect("the query waits") {
        crate::install_worker::HooksJob::Query(query) => query.ticket,
        crate::install_worker::HooksJob::Install(_) => panic!("the refusal queues names"),
    };
    app.on_hooks_offer_names(ticket, Ok(vec!["fmt".to_owned()]));
    let slab = crate::view::frame_of(&app, Surface::HooksOffer);
    assert!(!slab.is_empty(), "the offer draws its frame");
    let wheel = |col: u16, row: u16| Mouse {
        kind: MouseKind::WheelDown,
        col,
        row,
    };
    assert_eq!(app.home_list_scroll(), 0);
    // Over the frame the wheel is the offer's and moves no list.
    app.on_wheel(&wheel(slab.x + slab.width / 2, slab.y + slab.height / 2));
    assert_eq!(app.home_list_scroll(), 0);
    // Past the frame the list still does not scroll: the offer holds
    // the wheel while it is on top, even where no frame catches it.
    let screen = app.home_screen().expect("home draws its list");
    let view_area = ratatui::layout::Rect::new(0, 0, 80, 24);
    let (rows, _) = crate::view::list_window(&app, &screen, view_area);
    let below = ratatui::layout::Position::new(rows.x + 1, rows.bottom().saturating_sub(1));
    assert!(!slab.contains(below), "the control sits past the frame");
    app.on_wheel(&wheel(below.x, below.y));
    assert_eq!(app.home_list_scroll(), 0, "the offer holds the wheel");
}

#[test]
fn the_wheel_while_the_names_query_runs_moves_no_list() {
    let seam = fake();
    let mut app = home(Some(Arc::clone(&seam)));
    // Enough exited rows to scroll: the wheel over the list moves it.
    let lines = app.on_line(hello());
    let recent = lines
        .get(1)
        .and_then(|line| serde_json::from_str::<Value>(line).ok())
        .unwrap_or_default();
    let id = recent["id"].as_str().expect("the recent ask").to_owned();
    // One project per session: sections overflow the list window, so the
    // wheel over the rows has rows to move.
    let sessions: Vec<Value> = (0..40)
        .map(|at| {
            json!({
                "session_id": format!("s_{at:016}"),
                "ts": at, "project": format!("-p{at:02}"),
                "workspace": "/w", "name": format!("old {at}"), "how": "exited",
            })
        })
        .collect();
    app.on_line(Line::Hub(contract::HubLine {
        kind: "command_accepted".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: [
            ("command_id".to_owned(), Value::String(id)),
            ("result".to_owned(), json!({"sessions": sessions})),
        ]
        .into_iter()
        .collect(),
    }));
    refuse_start(&mut app, "hi");
    // The query runs with no frame, and the offer is still on top.
    assert!(app.hooks_offer_names().is_none(), "the query runs");
    assert_eq!(app.top_surface(), Surface::HooksOffer);
    let wheel = |col: u16, row: u16| Mouse {
        kind: MouseKind::WheelDown,
        col,
        row,
    };
    assert_eq!(app.home_list_scroll(), 0);
    // Over the rows the wheel is the offer's and moves no list, even
    // with nothing framed to catch it.
    let screen = app.home_screen().expect("home draws its list");
    let view_area = ratatui::layout::Rect::new(0, 0, 80, 24);
    let (rows, _) = crate::view::list_window(&app, &screen, view_area);
    let over_rows = ratatui::layout::Position::new(rows.x + 1, rows.bottom().saturating_sub(1));
    app.on_wheel(&wheel(over_rows.x, over_rows.y));
    assert_eq!(app.home_list_scroll(), 0, "the query holds the wheel");
}

#[test]
fn install_failure_text_names_the_command_only_when_absent() {
    let error = ConfigureError {
        code: ErrorCode::IoFailed,
        message: "boom".to_owned(),
    };
    assert_eq!(
        super::install_failure_text(&error),
        "boom. Run `fiber extension install hooks`."
    );
    let error = ConfigureError {
        code: ErrorCode::IoFailed,
        message: "boom.".to_owned(),
    };
    assert_eq!(
        super::install_failure_text(&error),
        "boom. Run `fiber extension install hooks`."
    );
    let error = ConfigureError {
        code: ErrorCode::IoFailed,
        message: "run fiber extension install hooks now".to_owned(),
    };
    assert_eq!(
        super::install_failure_text(&error),
        "run fiber extension install hooks now"
    );
}
