//! Tests for the notice stack, its rows and overlays.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::{NoticeLine, NoticeOverlay, Notices};
use crate::app::App;
use crate::keys::Key;
use crate::link::Line;
use contract::ErrorCode;
use contract::clock::Clock;
use contract::events::Notice;

const S_A: &str = "s_aaaaaaaaaaaaaaaa";

/// Notices of `texts` under `code`, oldest first.
fn stack(code: &str, texts: &[&str]) -> Notices {
    let mut notices = Notices::default();
    for text in texts {
        notices.push(code, (*text).to_owned());
    }
    notices
}

/// `base` plus `ms` milliseconds: the fake-clock instants sweeps read.
fn at(base: Instant, ms: u64) -> Instant {
    base + Duration::from_millis(ms)
}

/// An event under `code` with `text`, born at the first sweep.
fn event(code: &str, text: &str, born: Instant) -> Notices {
    let mut notices = Notices::default();
    notices.push(code, text.to_owned());
    notices.sweep(born);
    notices
}

/// Each line's row: `notice <id>`, oldest first, or `more <n>`.
fn ids(lines: &[NoticeLine]) -> Vec<String> {
    lines
        .iter()
        .map(|line| match line {
            NoticeLine::Notice { id, .. } => format!("notice {id}"),
            NoticeLine::More(n) => format!("more {n}"),
        })
        .collect()
}

#[test]
fn two_notices_draw_two_rows_and_no_more() {
    assert_eq!(
        ids(&stack("extension_failed", &["a", "b"]).lines()),
        ["notice 0", "notice 1"]
    );
}

#[test]
fn three_oldest_then_more() {
    // At the cap three rows show, oldest first, and no "+N more".
    assert_eq!(
        ids(&stack("extension_failed", &["a", "b", "c"]).lines()),
        ["notice 0", "notice 1", "notice 2"]
    );
    // Past it the three oldest show, then one "+N more" row last.
    assert_eq!(
        ids(&stack("extension_failed", &["a", "b", "c", "d"]).lines()),
        ["notice 0", "notice 1", "notice 2", "more 1"]
    );
    assert_eq!(
        ids(&stack("extension_failed", &["a", "b", "c", "d", "e"]).lines()),
        ["notice 0", "notice 1", "notice 2", "more 2"]
    );
    assert!(Notices::default().lines().is_empty());
}

#[test]
fn dismiss_reveals_the_next_oldest() {
    let mut notices = stack("extension_failed", &["a", "b", "c", "d"]);
    notices.dismiss(0);
    // Dismissing a shown row reveals the next oldest; the "+N more" row
    // goes, since three show again.
    assert_eq!(ids(&notices.lines()), ["notice 1", "notice 2", "notice 3"]);
    notices.dismiss(3);
    assert_eq!(ids(&notices.lines()), ["notice 1", "notice 2"]);
}

#[test]
fn dismissing_a_hidden_notice_drops_the_more_count() {
    let mut notices = stack("extension_failed", &["a", "b", "c", "d", "e"]);
    notices.dismiss(4);
    assert_eq!(
        ids(&notices.lines()),
        ["notice 0", "notice 1", "notice 2", "more 1"]
    );
}

#[test]
fn code_is_kept() {
    let lines = stack("extension_failed", &["x"]).lines();
    assert_eq!(
        lines,
        [NoticeLine::Notice {
            id: 0,
            code: "extension_failed".to_owned(),
            text: "x".to_owned(),
        }]
    );
}

#[test]
fn empty_has_no_lines() {
    assert!(Notices::default().lines().is_empty());
}

#[test]
fn dismiss_and_the_overlays() {
    let mut notices = stack("extension_failed", &["one", "two", "three", "four"]);
    assert_eq!(notices.overlay(), None);
    notices.open(1);
    assert_eq!(notices.overlay(), Some(vec!["two".to_owned()]));
    notices.dismiss(1);
    assert_eq!(notices.overlay(), None);
    assert_eq!(
        notices
            .lines()
            .iter()
            .map(|line| match line {
                NoticeLine::Notice { text, .. } => text.clone(),
                NoticeLine::More(n) => format!("+{n} more"),
            })
            .collect::<Vec<_>>(),
        ["one", "three", "four"]
    );
    // Opening a notice that is gone does nothing.
    notices.open(1);
    assert_eq!(notices.overlay(), None);
    notices.open_all();
    assert_eq!(
        notices.overlay(),
        Some(vec![
            "four".to_owned(),
            "three".to_owned(),
            "one".to_owned()
        ])
    );
    // Dismissing another notice keeps the list open.
    notices.dismiss(0);
    assert_eq!(
        notices.overlay(),
        Some(vec!["four".to_owned(), "three".to_owned()])
    );
    assert!(notices.close());
    assert!(!notices.close());
    let mut empty = Notices::default();
    empty.open_all();
    assert_eq!(empty.overlay(), None);
}

#[test]
fn held_notices_wait_for_the_release() {
    let mut notices = stack("extension_failed", &["one", "two", "three"]);
    notices.hold();
    notices.push("extension_failed", "during".to_owned());
    notices.push("extension_failed", "later".to_owned());
    // Held notices are not drawn, not listed by "+N more" and not in the
    // overlay; the ones from before the hold still show.
    let first = |notices: &Notices| -> Vec<String> {
        notices
            .lines()
            .iter()
            .map(|line| match line {
                NoticeLine::Notice { text, .. } => text.clone(),
                NoticeLine::More(n) => format!("+{n} more"),
            })
            .collect()
    };
    assert_eq!(first(&notices), ["one", "two", "three"]);
    notices.open_all();
    assert_eq!(
        notices.overlay(),
        Some(vec!["three".to_owned(), "two".to_owned(), "one".to_owned()])
    );
    notices.close();
    // A second hold keeps where the first began.
    notices.hold();
    notices.release();
    assert_eq!(first(&notices), ["one", "two", "three", "+2 more"]);
}

/// An app attached to [`S_A`], connected, 100 columns wide.
fn attached() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(100, 24);
    app.on_line(Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::default(),
    }));
    app.attach(contract::SessionId(S_A.to_owned()));
    app
}

/// A `notice` line for `session`.
fn notice(session: &str, message: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "notice".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::json!({"code": "extension_failed", "message": message})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    })
}

/// The first line's code and text.
fn first(app: &App) -> (String, String) {
    match app.notices().into_iter().next() {
        Some(NoticeLine::Notice { code, text, .. }) => (code, text),
        line => panic!("a notice row, not {line:?}"),
    }
}

/// A hub `command_rejected` for `id`, with `code` when `Some`.
fn hub_refused(id: &str, code: Option<&str>, message: &str) -> Line {
    let mut payload = serde_json::Map::new();
    payload.insert(
        "command_id".to_owned(),
        serde_json::Value::String(id.to_owned()),
    );
    if let Some(code) = code {
        payload.insert(
            "code".to_owned(),
            serde_json::Value::String(code.to_owned()),
        );
    }
    payload.insert(
        "message".to_owned(),
        serde_json::Value::String(message.to_owned()),
    );
    Line::Hub(contract::HubLine {
        kind: "command_rejected".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload,
    })
}

/// A session `command_rejected` for `id` on `session` with `code`.
fn session_refused(session: &str, id: &str, code: &str, message: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "command_rejected".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: [
            ("command_id".to_owned(), serde_json::json!(id)),
            ("code".to_owned(), serde_json::json!(code)),
            ("message".to_owned(), serde_json::json!(message)),
        ]
        .into_iter()
        .collect(),
    })
}

#[test]
fn connection_lost_is_connection_failed() {
    let mut app = attached();
    app.disconnected();
    assert_eq!(
        first(&app),
        (
            "connection_failed".to_owned(),
            "Connection lost.".to_owned()
        )
    );
}

#[test]
fn connect_failed_is_connection_failed() {
    let mut app = attached();
    app.connect_failed("Could not reach the hub: refused.".to_owned());
    assert_eq!(
        first(&app),
        (
            "connection_failed".to_owned(),
            "Could not reach the hub: refused.".to_owned()
        )
    );
}

#[test]
fn session_notice_keeps_its_code() {
    let mut app = attached();
    app.on_line(notice(S_A, "An extension failed to load."));
    assert_eq!(
        first(&app),
        (
            "extension_failed".to_owned(),
            "An extension failed to load.".to_owned()
        )
    );
    assert_eq!(app.notice(), Some("An extension failed to load."));
}

#[test]
fn the_sessions_notices_and_the_terminals_own_share_the_stack() {
    let mut app = attached();
    app.on_line(notice(S_A, "An extension failed to load."));
    app.on_line(notice("s_bbbbbbbbbbbbbbbb", "Not ours."));
    app.on_key(Key::AltA, fakes::clock::FakeClock::new().now());
    app.disconnected();
    let firsts: Vec<String> = app
        .notices()
        .iter()
        .filter_map(|line| match line {
            NoticeLine::Notice { text, .. } => Some(text.clone()),
            NoticeLine::More(_) => None,
        })
        .collect();
    assert_eq!(
        firsts,
        [
            "An extension failed to load.",
            "No requests waiting.",
            "Connection lost.",
        ]
    );
}

#[test]
fn a_hub_refusal_keeps_its_code() {
    let mut app = attached();
    // A refused `reply` carries its draft back with the hub's code.
    app.outbox.pending.insert(
        "c_1".to_owned(),
        (crate::app::Kind::Reply, crate::input::Draft::default()),
    );
    app.on_line(hub_refused("c_1", Some("busy"), "A turn is running."));
    assert_eq!(
        first(&app),
        ("busy".to_owned(), "A turn is running.".to_owned())
    );
}

#[test]
fn a_hub_refusal_with_no_code_is_malformed() {
    let mut app = attached();
    app.outbox.pending.insert(
        "c_1".to_owned(),
        (crate::app::Kind::Reply, crate::input::Draft::default()),
    );
    app.on_line(hub_refused("c_1", None, "No code."));
    assert_eq!(first(&app), ("malformed".to_owned(), "No code.".to_owned()));
}

#[test]
fn a_session_refusal_keeps_its_code() {
    let mut app = attached();
    app.outbox.pending.insert(
        "c_1".to_owned(),
        (crate::app::Kind::Reply, crate::input::Draft::default()),
    );
    app.on_line(session_refused(S_A, "c_1", "busy", "A turn is running."));
    assert_eq!(
        first(&app),
        ("busy".to_owned(), "A turn is running.".to_owned())
    );
}

#[test]
fn a_history_refusal_keeps_its_code() {
    // Up sends `prompt_history`; its refusal is the notice with the
    // refusal's code.
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(100, 24);
    app.set_project("key".to_owned());
    app.on_line(Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::default(),
    }));
    let now = fakes::clock::FakeClock::new().now();
    let crate::app::Effect::Send(sent) = app.on_key(Key::Up, now) else {
        panic!("a send");
    };
    let id = sent
        .iter()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap_or_default())
        .find(|line| line["command"] == "prompt_history")
        .and_then(|line| line["id"].as_str().map(str::to_owned))
        .expect("a prompt_history send");
    app.on_line(hub_refused(&id, Some("busy"), "A turn is running."));
    assert_eq!(
        first(&app),
        ("busy".to_owned(), "A turn is running.".to_owned())
    );
}

#[test]
fn keys_notices_are_config_invalid() {
    let mut app = attached();
    // An action Fiber has no name for is ignored with a notice.
    let setup = crate::keyset::KeysSetup {
        user: [("no_action".to_owned(), serde_json::json!("ctrl+a"))]
            .into_iter()
            .collect(),
    };
    app.set_keys(setup);
    let codes: Vec<String> = app
        .notices()
        .iter()
        .filter_map(|line| match line {
            NoticeLine::Notice { code, .. } => Some(code.clone()),
            NoticeLine::More(_) => None,
        })
        .collect();
    assert!(!codes.is_empty());
    assert!(
        codes.iter().all(|code| code == "config_invalid"),
        "{codes:?}"
    );
}

#[test]
fn catalogue_notices_show_under_their_own_codes_and_a_read_error_is_io_failed() {
    let mut app = attached();
    app.on_models(Ok(crate::catalogue::Catalogue {
        installable: Vec::new(),
        models: Vec::new(),
        notices: vec![
            Notice {
                code: ErrorCode::ModelUnconfigured,
                message: "a host is missing".to_owned(),
                extension: None,
            },
            Notice {
                code: ErrorCode::ExtensionFailed,
                message: "the refresh failed".to_owned(),
                extension: Some("acme".to_owned()),
            },
            Notice {
                code: ErrorCode::ModelInvalid,
                message: "a cached list is bad".to_owned(),
                extension: None,
            },
        ],
        lists: Vec::new(),
        left_out: Vec::new(),
    }));
    let codes: Vec<String> = app
        .notices()
        .iter()
        .map(|line| match line {
            NoticeLine::Notice { code, .. } => code.clone(),
            line @ NoticeLine::More(_) => panic!("a more row, not a notice: {line:?}"),
        })
        .collect();
    // Catalogue notices keep their push order, one row each, oldest first
    // (`docs/tui.md`, "Notices").
    assert_eq!(
        codes,
        ["model_unconfigured", "extension_failed", "model_invalid"]
    );
    assert_eq!(
        first(&app),
        (
            "model_unconfigured".to_owned(),
            "a host is missing".to_owned()
        )
    );
    app.on_models(Err("the lists could not be read".to_owned()));
    // Three rows already show, so the read error waits behind one
    // `+N more` row; dismissing the oldest reveals it with its code
    // (`docs/tui.md`, "Notices": at most 3 show, then "+N more").
    assert_eq!(app.notices()[3], NoticeLine::More(1));
    app.dismiss_notice(0);
    assert_eq!(
        app.notices()[2],
        NoticeLine::Notice {
            id: 3,
            code: "io_failed".to_owned(),
            text: "the lists could not be read".to_owned(),
        }
    );
}

#[test]
fn the_app_dismisses_and_opens_notices_and_esc_closes_the_overlay() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = attached();
    for n in 0..4 {
        app.on_line(notice(S_A, &format!("notice {n}")));
    }
    app.open_notice(2);
    assert_eq!(app.notice_overlay(), Some(vec!["notice 2".to_owned()]));
    assert_eq!(app.on_key(Key::Esc, now), crate::app::Effect::None);
    assert_eq!(app.notice_overlay(), None);
    app.open_more_notices();
    assert_eq!(app.notice_overlay().map(|all| all.len()), Some(4));
    app.on_key(Key::Esc, now);
    app.dismiss_notice(3);
    assert_eq!(app.notice(), Some("notice 2"));
    assert_eq!(app.notices().len(), 3);
}

#[test]
fn overlay_entries_names_the_open_single_notice() {
    let mut notices = stack("extension_failed", &["one", "two"]);
    assert_eq!(notices.overlay_entries(), None);
    notices.open(1);
    assert_eq!(
        notices.overlay_entries(),
        Some(NoticeOverlay {
            all: false,
            entries: vec![("extension_failed".to_owned(), "two".to_owned())],
        })
    );
}

#[test]
fn overlay_entries_lists_every_notice_newest_first() {
    let mut notices = Notices::default();
    notices.push("extension_failed", "one".to_owned());
    notices.push("config_invalid", "two".to_owned());
    notices.open_all();
    assert_eq!(
        notices.overlay_entries(),
        Some(NoticeOverlay {
            all: true,
            entries: vec![
                ("config_invalid".to_owned(), "two".to_owned()),
                ("extension_failed".to_owned(), "one".to_owned()),
            ],
        })
    );
}

#[test]
fn overlay_entries_covers_the_cap_edges_and_the_emptied_list() {
    for count in [2, 3, 4] {
        let texts: Vec<String> = (0..count).map(|n| format!("notice {n}")).collect();
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        let mut notices = stack("extension_failed", &refs);
        notices.open_all();
        let entries = notices.overlay_entries().unwrap();
        assert!(entries.all);
        assert_eq!(entries.entries.len(), count, "{count} notices");
    }
    // Dismissing the open single notice closes the overlay.
    let mut notices = stack("extension_failed", &["one", "two"]);
    notices.open(1);
    notices.dismiss(1);
    assert_eq!(notices.overlay_entries(), None);
    // Dismissing every notice while the list is open empties it, not closes.
    let mut notices = stack("extension_failed", &["one", "two"]);
    notices.open_all();
    notices.dismiss(0);
    notices.dismiss(1);
    assert_eq!(
        notices.overlay_entries(),
        Some(NoticeOverlay {
            all: true,
            entries: vec![],
        })
    );
}

/// Every event code the kind table names.
const EVENTS: [&str; 28] = [
    "cache_rebuild",
    "closing",
    "command_conflict",
    "config_invalid",
    "config_key_ignored",
    "credential_failed",
    "credential_missing",
    "extension_failed",
    "extension_incompatible",
    "extension_shadowed",
    "hook_failed",
    "instructions_large",
    "invalid_arguments",
    "io_failed",
    "malformed",
    "model_invalid",
    "model_unconfigured",
    "no_model",
    "repository_code_skipped",
    "reviewer_selection_failed",
    "session_held",
    "session_not_found",
    "skill_invalid",
    "skill_shadowed",
    "skills_large",
    "stale_request",
    "tool_definitions_large",
    "web_search_unavailable",
];

/// Every condition code the kind table names.
const CONDITIONS: [&str; 6] = [
    "connection_failed",
    "history_loading",
    "login_url",
    "models_loading",
    "stream_closed",
    "switch_pending",
];

/// The notice rows' texts, oldest first, without the "+N more" row.
fn texts(lines: &[NoticeLine]) -> Vec<String> {
    lines
        .iter()
        .filter_map(|line| match line {
            NoticeLine::Notice { text, .. } => Some(text.clone()),
            NoticeLine::More(_) => None,
        })
        .collect()
}

/// The notice rows' codes, oldest first.
fn codes(lines: &[NoticeLine]) -> Vec<String> {
    lines
        .iter()
        .filter_map(|line| match line {
            NoticeLine::Notice { code, .. } => Some(code.clone()),
            NoticeLine::More(_) => None,
        })
        .collect()
}

#[test]
fn an_event_fades_ten_seconds_after_its_first_sweep() {
    let t0 = fakes::clock::FakeClock::new().now();
    let mut notices = event("io_failed", "x", t0);
    assert_eq!(ids(&notices.lines()), ["notice 0"]);
    // Just below the edge the row stands.
    notices.sweep(at(t0, 9_999));
    assert_eq!(ids(&notices.lines()), ["notice 0"]);
    // At exactly ten seconds it is gone: expiry is now minus birth at
    // least the fade, not past it.
    notices.sweep(at(t0, 10_000));
    assert!(notices.lines().is_empty());
}

#[test]
fn expiry_counts_from_the_first_sweep_after_the_push() {
    let t0 = fakes::clock::FakeClock::new().now();
    let mut notices = Notices::default();
    notices.push("io_failed", "x".to_owned());
    // The first sweep births the notice, however late it runs.
    notices.sweep(at(t0, 60_000));
    assert_eq!(ids(&notices.lines()), ["notice 0"]);
    notices.sweep(at(t0, 69_999));
    assert_eq!(ids(&notices.lines()), ["notice 0"]);
    notices.sweep(at(t0, 70_000));
    assert!(notices.lines().is_empty());
}

#[test]
fn every_event_fades_and_every_condition_stands() {
    let t0 = fakes::clock::FakeClock::new().now();
    let mut notices = Notices::default();
    for code in EVENTS {
        notices.push(code, format!("{code} happened"));
    }
    for code in CONDITIONS {
        if code == "stream_closed" {
            notices.push_identified(code, S_A.to_owned(), "closed".to_owned());
        } else {
            notices.push(code, format!("{code} stands"));
        }
    }
    notices.sweep(t0);
    // Past the fade every event is gone and every condition stands, in
    // push order: drains batch by batch, since a sweep births only the
    // shown rows and each expiry reveals the next batch to birth.
    let mut now = t0;
    for _ in 0..10 {
        now = at(now, 60_000);
        notices.sweep(now);
        notices.sweep(now);
    }
    notices.open_all();
    let entries = notices.overlay_entries().expect("the open list");
    assert_eq!(entries.entries.len(), CONDITIONS.len());
    let stood: Vec<String> = entries.entries.into_iter().map(|(code, _)| code).collect();
    assert_eq!(
        stood,
        CONDITIONS
            .iter()
            .rev()
            .map(|code| (*code).to_owned())
            .collect::<Vec<_>>()
    );
    notices.close();
    // Conditions never fade on their own, however late the sweep runs.
    notices.sweep(at(t0, 3_600_000));
    notices.open_all();
    assert_eq!(
        notices
            .overlay_entries()
            .expect("the open list")
            .entries
            .len(),
        CONDITIONS.len()
    );
}

#[test]
fn unknown_codes_fade_as_events() {
    // Hub refusals and session notices carry codes the table does not
    // name — `busy`, `not_subscribed` and the like: they fade.
    let t0 = fakes::clock::FakeClock::new().now();
    let mut notices = Notices::default();
    notices.push("busy", "A turn is running.".to_owned());
    notices.push("not_subscribed", "Send `subscribe` first.".to_owned());
    notices.sweep(t0);
    assert_eq!(texts(&notices.lines()).len(), 2);
    notices.sweep(at(t0, 10_000));
    assert!(notices.lines().is_empty());
}

#[test]
fn repeat_pushes_dedupe_by_code_and_identity() {
    // The same code with the same text pushed twice keeps one row.
    let notices = stack("io_failed", &["same", "same"]);
    assert_eq!(texts(&notices.lines()), ["same"]);
    // The same code with another text stacks beside it.
    let notices = stack("io_failed", &["same", "other"]);
    assert_eq!(texts(&notices.lines()), ["same", "other"]);
    // A repeat `stream_closed` for the same session keeps one row, while
    // another session's stacks beside it.
    let mut notices = Notices::default();
    notices.push_identified("stream_closed", S_A.to_owned(), "closed".to_owned());
    notices.push_identified("stream_closed", S_A.to_owned(), "closed".to_owned());
    assert_eq!(texts(&notices.lines()), ["closed"]);
    notices.push_identified(
        "stream_closed",
        "s_bbbbbbbbbbbbbbbb".to_owned(),
        "closed".to_owned(),
    );
    assert_eq!(texts(&notices.lines()).len(), 2);
}

#[test]
fn identical_switch_pending_choices_stack_and_dismiss_singly() {
    // Two identical pending choices are two notices worth of state, not one.
    let mut notices = Notices::default();
    notices.push("switch_pending", "x applies when this turn ends".to_owned());
    notices.push("switch_pending", "x applies when this turn ends".to_owned());
    let ids: Vec<usize> = notices
        .lines()
        .iter()
        .filter_map(|line| match line {
            NoticeLine::Notice { id, .. } => Some(*id),
            NoticeLine::More(_) => None,
        })
        .collect();
    assert_eq!(ids.len(), 2);
    // Dismissing one share leaves the other standing.
    notices.dismiss(ids[0]);
    assert_eq!(texts(&notices.lines()), ["x applies when this turn ends"]);
    notices.dismiss(ids[1]);
    assert!(notices.lines().is_empty());
}

#[test]
fn dismissing_an_absent_id_changes_nothing() {
    let mut notices = stack("io_failed", &["x"]);
    notices.open(0);
    // Dismissing an id that is gone, or never was, keeps the stack and a
    // single overlay showing another notice.
    notices.dismiss(99);
    assert_eq!(texts(&notices.lines()), ["x"]);
    assert!(notices.overlay().is_some());
    notices.dismiss(0);
    assert!(notices.lines().is_empty());
    assert!(notices.overlay().is_none());
    notices.dismiss(0);
    assert!(notices.lines().is_empty());
    // Clearing an absent code changes nothing either.
    notices.clear_code("stream_closed", S_A);
    assert!(notices.lines().is_empty());
}

#[test]
fn expired_rows_never_occupy_a_shown_slot() {
    let t0 = fakes::clock::FakeClock::new().now();
    // One expired row beside two, three and four live ones: the live rows
    // show oldest first, and "+N more" counts only live rows past three.
    for live in [2, 3, 4] {
        let mut notices = Notices::default();
        notices.push("io_failed", "old".to_owned());
        notices.sweep(t0);
        for n in 0..live {
            notices.push("io_failed", format!("live {n}"));
        }
        notices.sweep(at(t0, 10_000));
        let kept: Vec<String> = (0..live).map(|n| format!("live {n}")).collect();
        let shown = texts(&notices.lines());
        assert_eq!(shown, kept[..shown.len()], "with {live} live rows");
        let more = notices
            .lines()
            .iter()
            .filter_map(|line| match line {
                NoticeLine::More(n) => Some(*n),
                NoticeLine::Notice { .. } => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(more, if live > 3 { vec![live - 3] } else { vec![] });
    }
}

#[test]
fn an_empty_sweep_is_a_no_op_with_no_wake() {
    let t0 = fakes::clock::FakeClock::new().now();
    let mut notices = Notices::default();
    notices.sweep(t0);
    assert!(notices.lines().is_empty());
    assert_eq!(notices.notices_wake(), None);
}

#[test]
fn the_wake_is_the_earliest_unpaused_event_expiry() {
    let t0 = fakes::clock::FakeClock::new().now();
    // No event stacked arms nothing.
    let mut notices = Notices::default();
    notices.push("stream_closed", "closed".to_owned());
    notices.sweep(t0);
    assert_eq!(notices.notices_wake(), None);
    // An unborn event contributes no wake: births come from sweeps, and
    // the frame sweeps before it arms.
    notices.push("io_failed", "x".to_owned());
    assert_eq!(notices.notices_wake(), None);
    notices.sweep(t0);
    assert_eq!(notices.notices_wake(), Some(at(t0, 10_000)));
    // The earlier of two staggered expiries wins.
    notices.push("config_invalid", "y".to_owned());
    notices.sweep(at(t0, 5_000));
    assert_eq!(notices.notices_wake(), Some(at(t0, 10_000)));
    // Rows showing at the hold wait out their fade with no wake until
    // the release; a pushed event contributes none either, unborn.
    notices.hold();
    notices.push("malformed", "z".to_owned());
    notices.sweep(at(t0, 6_000));
    assert_eq!(notices.notices_wake(), None);
    notices.release();
    notices.sweep(at(t0, 6_000));
    assert_eq!(notices.notices_wake(), Some(at(t0, 10_000)));
    // An open overlay pauses every entry's wake until it closes.
    notices.open_all();
    assert_eq!(notices.notices_wake(), None);
    notices.close();
    assert_eq!(notices.notices_wake(), Some(at(t0, 10_000)));
    // Past the first fade its row is gone, and the wake moves to the
    // next expiry; the standing condition row shows throughout
    // (`docs/tui.md`, "Notices": one row each until dismissed, oldest
    // first).
    notices.sweep(at(t0, 10_000));
    assert_eq!(texts(&notices.lines()), ["closed", "y", "z"]);
    assert_eq!(notices.notices_wake(), Some(at(t0, 15_000)));
}

#[test]
fn a_row_showing_at_hold_keeps_its_row_past_fade() {
    let t0 = fakes::clock::FakeClock::new().now();
    // A drag holds every visible notice against expiry, not just the
    // ones pushed after the press: the row showing at hold time waits
    // out its fade, then expires on the first sweep past the release
    // (`docs/tui.md:1204`, "Notices": one row each until dismissed; a
    // notice that arrives during a drag waits for the release).
    let mut notices = Notices::default();
    notices.push("io_failed", "before".to_owned());
    notices.sweep(t0);
    assert_eq!(texts(&notices.lines()), ["before"]);
    notices.hold();
    // Past the fade the row still shows, and holds no wake either.
    notices.sweep(at(t0, 30_000));
    assert_eq!(texts(&notices.lines()), ["before"]);
    assert_eq!(notices.notices_wake(), None);
    // The release resumes expiry: the next sweep drops the row.
    notices.release();
    assert_eq!(notices.notices_wake(), Some(at(t0, 10_000)));
    notices.sweep(at(t0, 30_000));
    assert!(notices.lines().is_empty());
}

#[test]
fn a_held_notice_and_an_overlay_notice_pause_expiry() {
    let t0 = fakes::clock::FakeClock::new().now();
    // One pushed during the hold is retained but waits, hidden until
    // the release.
    let mut notices = Notices::default();
    notices.push("io_failed", "before".to_owned());
    notices.sweep(t0);
    notices.hold();
    notices.push("io_failed", "during".to_owned());
    notices.sweep(at(t0, 10_000));
    // The visible row waits out its fade still showing; the pushed one
    // stays unborn and hidden until the release (`docs/tui.md`,
    // "Notices": one row each until dismissed, oldest first).
    assert_eq!(texts(&notices.lines()), ["before"]);
    // The held notice gained no birth while hidden, so it contributes
    // no wake either: expiry counts from showing, not from the push.
    notices.release();
    assert_eq!(texts(&notices.lines()), ["before", "during"]);
    assert_eq!(notices.notices_wake(), Some(at(t0, 10_000)));
    // The first sweep showing the pushed one births it; it fades ten
    // seconds after that sweep, while the before row is already gone.
    notices.sweep(at(t0, 10_000));
    assert_eq!(texts(&notices.lines()), ["during"]);
    assert_eq!(notices.notices_wake(), Some(at(t0, 20_000)));
    notices.sweep(at(t0, 19_999));
    assert_eq!(texts(&notices.lines()), ["during"]);
    notices.sweep(at(t0, 20_000));
    assert!(notices.lines().is_empty());
}

#[test]
fn a_notice_past_the_shown_cap_is_born_when_it_shows() {
    let t0 = fakes::clock::FakeClock::new().now();
    let mut notices = stack("io_failed", &["a", "b", "c", "d"]);
    notices.sweep(t0);
    // Only the shown rows gain a birth; the fourth waits past the cap.
    notices.sweep(at(t0, 10_000));
    // Past the fade the shown three are gone and the fourth shows,
    // still unborn: this sweep removed the rows, so it did not show
    // it yet. The next sweep births it, ten seconds to its own fade.
    assert_eq!(texts(&notices.lines()), ["d"]);
    assert_eq!(notices.notices_wake(), None);
    notices.sweep(at(t0, 10_000));
    assert_eq!(notices.notices_wake(), Some(at(t0, 20_000)));
    notices.sweep(at(t0, 19_999));
    assert_eq!(texts(&notices.lines()), ["d"]);
    notices.sweep(at(t0, 20_000));
    assert!(notices.lines().is_empty());
    assert_eq!(notices.notices_wake(), None);
}

#[test]
fn a_held_boundary_notice_contributes_no_wake_until_released() {
    let t0 = fakes::clock::FakeClock::new().now();
    let mut notices = Notices::default();
    notices.hold();
    // The first notice pushed under the hold sits on the boundary:
    // hidden, unborn, and contributing no wake.
    notices.push("io_failed", "x".to_owned());
    notices.sweep(at(t0, 1_000));
    assert!(notices.lines().is_empty());
    assert_eq!(notices.notices_wake(), None);
    // The release shows it with a fresh birth: its wake starts then.
    notices.release();
    notices.sweep(at(t0, 1_000));
    assert_eq!(texts(&notices.lines()), ["x"]);
    assert_eq!(notices.notices_wake(), Some(at(t0, 11_000)));
}

#[test]
fn an_open_overlay_births_notices_past_the_cap() {
    let t0 = fakes::clock::FakeClock::new().now();
    // The open list shows every unheld notice past the cap, so its
    // sweep births them all: past the fade every row is gone.
    let mut notices = stack("io_failed", &["a", "b", "c", "d"]);
    notices.open_all();
    notices.sweep(t0);
    notices.close();
    notices.sweep(at(t0, 10_000));
    assert!(notices.lines().is_empty());
}

#[test]
fn an_open_single_notice_past_the_cap_is_born() {
    let t0 = fakes::clock::FakeClock::new().now();
    // The open single notice shows whole past the cap, so its sweep
    // births it: past the fade it expires with the shown rows.
    let mut notices = stack("io_failed", &["a", "b", "c", "d"]);
    notices.open(3);
    notices.sweep(t0);
    notices.close();
    notices.sweep(at(t0, 10_000));
    assert!(notices.lines().is_empty());
}

#[test]
fn an_open_single_notice_on_a_held_notice_gains_no_birth() {
    let t0 = fakes::clock::FakeClock::new().now();
    let mut notices = Notices::default();
    notices.push("io_failed", "shown".to_owned());
    notices.hold();
    notices.push("io_failed", "held".to_owned());
    // The open single notice names the held row: the sweep must not
    // birth what it does not show, however the guard reads.
    notices.open(1);
    notices.sweep(t0);
    notices.close();
    // Past the fade the born row waits out the hold still showing,
    // while the held row — never born — stands waiting for the
    // release with a fresh life (`docs/tui.md`, "Notices": one row
    // each until dismissed, oldest first).
    notices.sweep(at(t0, 10_000));
    assert_eq!(texts(&notices.lines()), ["shown"]);
    notices.release();
    assert_eq!(texts(&notices.lines()), ["shown", "held"]);
    // The shown row's fade waited out the hold: its wake is due, and
    // the next sweep drops it while birthing the held row, which is
    // still unborn and contributes no wake of its own.
    assert_eq!(notices.notices_wake(), Some(at(t0, 10_000)));
    notices.sweep(at(t0, 10_000));
    assert_eq!(texts(&notices.lines()), ["held"]);
    assert_eq!(notices.notices_wake(), Some(at(t0, 20_000)));
    notices.sweep(at(t0, 19_999));
    assert_eq!(texts(&notices.lines()), ["held"]);
    notices.sweep(at(t0, 20_000));
    assert!(notices.lines().is_empty());
}

#[test]
fn an_open_single_notice_pauses_only_its_row() {
    let t0 = fakes::clock::FakeClock::new().now();
    let mut notices = stack("io_failed", &["one", "two"]);
    notices.sweep(t0);
    notices.open(0);
    // Past the fade the shown notice stands while the other expires.
    notices.sweep(at(t0, 30_000));
    assert_eq!(texts(&notices.lines()), ["one"]);
    assert_eq!(notices.notices_wake(), None);
    // Closing the overlay lets the next sweep take it.
    assert!(notices.close());
    assert_eq!(notices.notices_wake(), Some(at(t0, 10_000)));
    notices.sweep(at(t0, 30_000));
    assert!(notices.lines().is_empty());
}

#[test]
fn clear_code_drops_only_that_notice() {
    let t0 = fakes::clock::FakeClock::new().now();
    let mut notices = Notices::default();
    notices.push_identified("stream_closed", S_A.to_owned(), "closed".to_owned());
    notices.push_identified(
        "stream_closed",
        "s_bbbbbbbbbbbbbbbb".to_owned(),
        "closed".to_owned(),
    );
    notices.sweep(t0);
    // Clearing one session's notice leaves the other's standing.
    notices.clear_code("stream_closed", S_A);
    assert_eq!(codes(&notices.lines()), ["stream_closed"]);
    notices.clear_code("stream_closed", "s_bbbbbbbbbbbbbbbb");
    assert!(notices.lines().is_empty());
}

#[test]
fn clearing_the_open_single_notice_closes_the_overlay() {
    let mut notices = stack("io_failed", &["one", "two"]);
    notices.open(1);
    // Clearing the notice the single overlay shows closes it.
    notices.clear_code("io_failed", "two");
    assert_eq!(notices.overlay_entries(), None);
    assert_eq!(texts(&notices.lines()), ["one"]);
}

#[test]
fn clearing_another_notice_leaves_the_single_overlay_open() {
    let mut notices = stack("io_failed", &["one", "two"]);
    notices.open(0);
    // Clearing a row the single overlay does not show leaves it open.
    notices.clear_code("io_failed", "two");
    assert_eq!(notices.overlay(), Some(vec!["one".to_owned()]));
}

#[test]
fn clear_code_except_keeps_only_the_kept_identity() {
    let search = "Could not search all of history: connection lost".to_owned();
    let mut notices = Notices::default();
    notices.push("connection_failed", "Could not reach the hub.".to_owned());
    notices.push("connection_failed", search.clone());
    notices.push("io_failed", "a read failed".to_owned());
    // The reconnect fold drops both connection texts but never the
    // search-incomplete text, and never another code's row.
    notices.clear_code_except("connection_failed", &search);
    assert_eq!(
        texts(&notices.lines()),
        [search, "a read failed".to_owned()]
    );
}

#[test]
fn attach_drops_only_other_sessions_stream_closed() {
    let mut app = attached();
    app.notices.push_identified(
        "stream_closed",
        S_A.to_owned(),
        "this session's log can't be read past here".to_owned(),
    );
    app.notices.push("io_failed", "a read failed".to_owned());
    // Attaching elsewhere drops only the other session's stale row:
    // the unrelated event row stands.
    app.attach(contract::SessionId("s_bbbbbbbbbbbbbbbb".to_owned()));
    assert_eq!(texts(&app.notices()), ["a read failed"]);
}

const ERRORS: &str = include_str!("../../../docs/errors.md");
const TUI: &str = include_str!("../../../docs/tui.md");

/// Every notice class the docs state: the Registry "Notice" column and
/// the second table's "Class" column of `docs/errors.md`, with the
/// `docs/tui.md` "Notices" per-code list. Sources stating a code twice
/// must agree.
fn doc_classes() -> std::collections::BTreeMap<&'static str, &'static str> {
    let mut classes = std::collections::BTreeMap::new();
    let mut take = |code: &'static str, class: &'static str| {
        let class = class.trim();
        assert!(
            class == "event" || class == "condition",
            "{code} classes as {class}"
        );
        assert!(
            classes.insert(code, class).is_none_or(|held| held == class),
            "{code} classes differ across the docs"
        );
    };
    let registry = ERRORS
        .split("## Registry")
        .nth(1)
        .unwrap()
        .split("Notices, for a failure")
        .next()
        .unwrap();
    for row in registry.lines().filter(|line| line.starts_with("| `")) {
        let cells: Vec<&str> = row.split('|').collect();
        let code = cells[1].trim().trim_matches('`');
        let class = cells.get(4).map(|cell| cell.trim()).unwrap_or("—");
        if class != "—" {
            take(code, class);
        }
    }
    let second = ERRORS
        .split("Notices, for a failure")
        .nth(1)
        .unwrap()
        .split("\n## ")
        .next()
        .unwrap();
    for row in second.lines().filter(|line| line.starts_with("| `")) {
        let cells: Vec<&str> = row.split('|').collect();
        take(cells[1].trim().trim_matches('`'), cells[2].trim());
    }
    let notices = TUI
        .split("### Notices")
        .nth(1)
        .unwrap()
        .split("\n### ")
        .next()
        .unwrap();
    for row in notices.lines().filter(|line| line.starts_with("| `")) {
        let cells: Vec<&str> = row.split('|').collect();
        take(cells[1].trim().trim_matches('`'), cells[2].trim());
    }
    classes
}

/// Every code `kind` covers, mapped to its production class name.
fn kind_table() -> std::collections::BTreeMap<&'static str, &'static str> {
    super::KINDS
        .iter()
        .map(|(code, kind)| {
            (
                *code,
                match kind {
                    super::Kind::Event => "event",
                    super::Kind::Condition => "condition",
                },
            )
        })
        .collect()
}

/// The docs class every notice code as the code table does.
#[test]
fn the_docs_class_every_notice_code_as_the_code_table_does() {
    assert_eq!(doc_classes(), kind_table());
}

/// A seam whose renderer hides every notice it sees.
fn hiding() -> (
    std::rc::Rc<crate::ext::tests::SlotRenderer>,
    crate::ext::Bound,
) {
    use crate::ext::tests::{SlotRenderer, SlotScript};
    let fake = std::rc::Rc::new(SlotRenderer::filling(
        &["notice"],
        SlotScript::Replace(Vec::new()),
    ));
    let bound = crate::ext::tests::seam(&fake, "s", 0, false);
    (fake, bound)
}

#[test]
fn hidden_notices_leave_no_rows_counts_or_overlays() {
    // Four stacked past the cap of three, all hidden: no rows and no
    // "+N more" (`docs/tui.md`, "Notices": one row each; "The slots": a
    // replacement that returns no lines draws no row).
    let (fake, bound) = hiding();
    let notices = stack("io_failed", &["a", "b", "c", "d"]);
    assert!(notices.visible_lines(&bound, 40).is_empty());
    let mut notices = notices;
    notices.open_all();
    let overlay = notices
        .visible_overlay_entries(&bound, 40)
        .expect("the overlay still opens");
    assert!(overlay.entries.is_empty());
    // One cached visibility result serves rows, counts and overlays:
    // each distinct notice calls the renderer once.
    // (`docs/tui.md`, "How a TUI extension runs": each drawn item is cached by input and width.)
    assert_eq!(fake.calls.borrow().len(), 4);
}

#[test]
fn an_extension_failed_notice_always_shows_built_in() {
    // A notice extension cannot hide its own switch-off failure: the
    // empty replacement hides the I/O notice but never the
    // `extension_failed` row beside it (`docs/tui.md`, "The slots",
    // `notice`: a notice extension cannot hide its own switch-off failure).
    let (_, bound) = hiding();
    let mut notices = stack("extension_failed", &["off"]);
    notices.push("io_failed", "a read failed".to_owned());
    let lines = notices.visible_lines(&bound, 40);
    assert_eq!(ids(&lines), ["notice 0"]);
}

#[test]
fn a_notice_shown_once_does_not_show_again_in_the_same_run() {
    // In one terminal run, a notice whose code and text match one already
    // shown does not show again (`docs/tui.md`, "Notices"): after the row
    // shows and fades, pushing the identical notice stacks no new row.
    let t0 = fakes::clock::FakeClock::new().now();
    let mut notices = Notices::default();
    notices.push("skill_shadowed", "25 skills are shadowed".to_owned());
    notices.sweep(t0);
    assert_eq!(texts(&notices.lines()), ["25 skills are shadowed"]);
    notices.sweep(at(t0, 10_000));
    assert!(notices.lines().is_empty());
    notices.push("skill_shadowed", "25 skills are shadowed".to_owned());
    assert!(
        notices.lines().is_empty(),
        "an identical notice after its first display shows no row"
    );
}

#[test]
fn a_notice_cleared_before_display_suppresses_nothing() {
    // Marked only on first display, never on push: a notice cleared
    // before any sweep shows it stacks again on its next push.
    let t0 = fakes::clock::FakeClock::new().now();
    let mut notices = Notices::default();
    notices.push("skill_shadowed", "gone".to_owned());
    notices.clear_code("skill_shadowed", "gone");
    notices.push("skill_shadowed", "gone".to_owned());
    notices.sweep(t0);
    assert_eq!(texts(&notices.lines()), ["gone"]);
}

#[test]
fn a_changed_notice_text_shows_again() {
    // A changed set is a new notice: the same code with another text
    // stacks after the first text showed and faded.
    let t0 = fakes::clock::FakeClock::new().now();
    let mut notices = Notices::default();
    notices.push("skill_shadowed", "25 skills are shadowed".to_owned());
    notices.sweep(t0);
    notices.sweep(at(t0, 10_000));
    assert!(notices.lines().is_empty());
    notices.push("skill_shadowed", "26 skills are shadowed".to_owned());
    assert_eq!(texts(&notices.lines()), ["26 skills are shadowed"]);
}

#[test]
fn identical_switch_pending_choices_show_once_per_text() {
    // Each sent choice stacks its own row, but an identical choice the
    // run already showed stacks none; another wording still shows.
    let t0 = fakes::clock::FakeClock::new().now();
    let mut notices = Notices::default();
    notices.push("switch_pending", "x applies when this turn ends".to_owned());
    notices.push("switch_pending", "x applies when this turn ends".to_owned());
    assert_eq!(texts(&notices.lines()).len(), 2);
    notices.sweep(t0);
    // The turn ends and the applied shares go with it; the run keeps
    // what it showed.
    notices.clear_code_all("switch_pending");
    assert!(notices.lines().is_empty());
    notices.push("switch_pending", "x applies when this turn ends".to_owned());
    assert!(
        notices.lines().is_empty(),
        "the shown identical choice shows no row again"
    );
    notices.push("switch_pending", "y applies when this turn ends".to_owned());
    assert_eq!(texts(&notices.lines()), ["y applies when this turn ends"]);
}

#[test]
fn visible_views_pass_through_without_a_notice_renderer() {
    // Nothing fills the slot: every shown notice draws built-in, oldest
    // first with "+N more" (`docs/tui.md`, "Notices"), and the overlay
    // lists them.
    use crate::ext::tests::{SlotRenderer, SlotScript};
    let fake = std::rc::Rc::new(SlotRenderer::filling(&[], SlotScript::Replace(Vec::new())));
    let bound = crate::ext::tests::seam(&fake, "s", 0, false);
    let notices = stack("io_failed", &["a", "b", "c", "d"]);
    assert_eq!(
        ids(&notices.visible_lines(&bound, 40)),
        ["notice 0", "notice 1", "notice 2", "more 1"]
    );
    let mut notices = notices;
    notices.open_all();
    let overlay = notices
        .visible_overlay_entries(&bound, 40)
        .expect("open_all opens the list overlay");
    assert_eq!(overlay.entries.len(), 4);
    assert!(fake.calls.borrow().is_empty());
}
