//! Tests for `/archive` on the session on screen: the discovery dry
//! run and the confirmed stop/archive correlation (`docs/tui.md`,
//! "Slash commands", "The session list"). Unlike `/delete`, `/archive`
//! asks nothing: the discovery answer stops the held cascade at once.

use super::super::super::{App, Effect};
use super::State;
use crate::app::testkit::{hello, home_with, sent};
use crate::home::Launch;
use crate::keys::Key;
use crate::link::Line;
use contract::SessionId;
use contract::clock::Clock;
use contract::{HubEventKind, events::EventKind};
use serde_json::{Value, json};
use std::path::PathBuf;

const A: &str = "s_aaaaaaaaaaaaaaaa";
const B: &str = "s_bbbbbbbbbbbbbbbb";

fn launch() -> Launch {
    Launch {
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
    }
}

/// Folds `hub_hello`: the feed and recent lines go out, both waiting
/// for their answers.
fn linked(app: &mut App) {
    let _lines: Vec<String> = app.on_line(hello());
}

/// A live `session_status` for `session` named `name`.
fn live(session: &str, name: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: EventKind::SessionStatus,
        session_id: SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: json!({
            "name": name,
            "workspace": "/w",
            "project": "-w",
            "state": "idle",
            "started": 0, "calls": 0, "since": 0,
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

/// A hub `session_left` for `session`, ending `how`.
fn left(session: &str, how: &str) -> Line {
    Line::Hub(contract::HubLine {
        kind: HubEventKind::SessionLeft,
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: json!({"session_id": session, "how": how})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    })
}

/// A hub `command_accepted` for `id` with `result`.
fn accepted(id: &str, result: Value) -> Line {
    Line::Hub(contract::HubLine {
        kind: HubEventKind::CommandAccepted,
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: [
            ("command_id".to_owned(), Value::String(id.to_owned())),
            ("result".to_owned(), result),
        ]
        .into_iter()
        .collect(),
    })
}

/// A hub `command_rejected` for `id` with `code` and `message`.
fn rejected(id: &str, code: &str, message: &str) -> Line {
    Line::Hub(contract::HubLine {
        kind: HubEventKind::CommandRejected,
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: json!({"command_id": id, "code": code, "message": message})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    })
}

/// Types `text` on home and presses Enter, returning the effect.
fn slash(app: &mut App, text: &str) -> Effect {
    let now = fakes::clock::FakeClock::new().now();
    for ch in text.chars() {
        assert_eq!(app.on_key(Key::Char(ch), now), Effect::None);
    }
    app.on_key(Key::Enter, now)
}

/// A linked home attached to `A`, with a live row naming it.
fn attached() -> App {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    app.on_line(live(A, "old work"));
    app.attach(SessionId(A.to_owned()));
    app
}

/// The dry-run command id `slash` sent, after pinning its shape: the
/// same cascading dry run `/delete` sends.
fn discovery(app: &mut App) -> String {
    let lines = sent(slash(app, "/archive"));
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["command"], "delete");
    assert_eq!(
        lines[0]["args"],
        json!({"session": A, "cascade": true, "dry_run": true})
    );
    assert_eq!(app.draft(), "");
    lines[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("discovery id"))
        .to_owned()
}

/// Answers the discovery with the accepted `sessions` and `held`,
/// returning what folding it sent, parsed: the stops, since `/archive`
/// asks nothing.
fn start(app: &mut App, sessions: &[&str], held: &[&str]) -> Vec<Value> {
    let command = discovery(app);
    app.on_line(accepted(
        &command,
        json!({"sessions": sessions, "held": held}),
    ))
    .iter()
    .map(|line| serde_json::from_str(line).unwrap_or_default())
    .collect()
}

/// The `close` lines among the outgoing commands.
fn closes(lines: &[Value]) -> Vec<Value> {
    lines
        .iter()
        .filter(|line| line["command"] == "close")
        .cloned()
        .collect()
}

/// The `archive` lines among the outgoing commands.
fn archives(lines: &[Value]) -> Vec<Value> {
    lines
        .iter()
        .filter(|line| line["command"] == "archive")
        .cloned()
        .collect()
}

/// The lines a fold sends, parsed, with only the command lines kept.
fn fold_lines(app: &mut App, line: Line) -> Vec<Value> {
    app.on_line(line)
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect()
}

/// The running command's generation.
fn generation(app: &App) -> u64 {
    app.home.questions.archive_command.generation
}

#[test]
fn archive_sends_only_the_cascading_dry_run() {
    let mut app = attached();
    let command = discovery(&mut app);
    // Nothing is asked and nothing stops until the hub answers: the
    // session stays.
    assert!(matches!(
        app.home.questions.archive_command.state,
        State::Discovering { .. }
    ));
    assert!(app.home.questions.prompt().is_none());
    assert!(!app.on_home());
    let _ = command;
}

#[test]
fn archive_with_no_session_says_so_and_clears_the_draft() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    assert_eq!(slash(&mut app, "/archive"), Effect::None);
    assert_eq!(app.notice(), Some("No session on screen."));
    assert_eq!(app.draft(), "");
}

#[test]
fn archive_with_the_link_down_sends_nothing_and_keeps_the_draft() {
    let mut app = attached();
    app.disconnected();
    let prior_notice = app.notice().map(str::to_owned);
    assert_eq!(slash(&mut app, "/archive"), Effect::None);
    assert_eq!(app.draft(), "/archive");
    assert_eq!(app.notice(), prior_notice.as_deref());
}

#[test]
fn accepted_discovery_stops_the_held_cascade_without_asking() {
    let mut app = attached();
    app.on_line(live(B, "forked"));
    let lines = start(&mut app, &[A, B], &[A, B]);
    // One `close` with `now` per held id (`docs/tui.md`, "The session
    // list": `/archive` stops the session the way `/delete` does, and
    // `/delete` stops every running one first). The doc leaves the
    // wire order open, so the stopped ids read as a set.
    let died = closes(&lines);
    assert_eq!(died.len(), 2);
    for line in &died {
        assert_eq!(line["command"], "close");
        assert_eq!(line["args"], json!({"now": true}));
    }
    let mut stopped: Vec<&str> = died
        .iter()
        .map(|line| {
            line["session_id"]
                .as_str()
                .unwrap_or_else(|| panic!("stop session"))
        })
        .collect();
    stopped.sort();
    assert_eq!(stopped, [A, B]);
    // No archive goes out before every held id reports its exit, and no
    // question opens at any phase.
    assert!(archives(&lines).is_empty());
    assert!(app.home.questions.prompt().is_none());
    assert!(app.on_home(), "stops go out from home");
    assert_eq!(app.session(), None);
}

#[test]
fn exits_retire_stops_and_send_one_archive_for_the_target() {
    let mut app = attached();
    app.on_line(live(B, "forked"));
    let lines = start(&mut app, &[A, B], &[A, B]);
    assert!(archives(&lines).is_empty());
    // The first exit retires one stop but sends no archive yet: the
    // archive waits for every held stop, so dropping the retirement
    // would send it early.
    let out = fold_lines(&mut app, left(B, "exited"));
    assert!(archives(&out).is_empty());
    // The last exit sends exactly one archive line, for the target
    // alone and never the cascade set.
    let out = fold_lines(&mut app, left(A, "exited"));
    let archived = archives(&out);
    assert_eq!(archived.len(), 1, "{out:?}");
    assert_eq!(archived[0]["args"], json!({"session": A}));
    assert!(out.iter().all(|line| line["command"] != "delete"));
}

#[test]
fn accepted_archive_removes_only_the_target_refetches_and_goes_home() {
    let mut app = attached();
    app.on_line(live(B, "forked"));
    let _lines = start(&mut app, &[A, B], &[A, B]);
    let out = fold_lines(&mut app, left(B, "exited"));
    assert!(archives(&out).is_empty());
    let out = fold_lines(&mut app, left(A, "exited"));
    let archived = archives(&out);
    assert_eq!(archived.len(), 1);
    let id = archived[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("archive id"))
        .to_owned();
    let out = fold_lines(&mut app, accepted(&id, json!({"archived": true})));
    // The target row is gone, the cascade survivor stays, the totals
    // refetch, and the terminal lands home.
    assert!(
        out.iter().any(|line| line["command"] == "recent"),
        "{out:?}"
    );
    assert!(app.on_home());
    assert_eq!(app.session(), None);
    assert!(matches!(
        app.home.questions.archive_command.state,
        State::Idle
    ));
}

#[test]
fn refused_stop_archives_nothing() {
    let mut app = attached();
    let lines = start(&mut app, &[A], &[A]);
    let died = closes(&lines);
    assert_eq!(died.len(), 1);
    let close = died[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("close id"))
        .to_owned();
    let failed = "the hub is busy";
    let out = fold_lines(&mut app, rejected(&close, "session_held", failed));
    assert!(archives(&out).is_empty(), "{out:?}");
    assert_eq!(
        app.notice(),
        Some(format!("Stop refused for old work: {failed}. Nothing was archived.").as_str())
    );
    assert!(matches!(
        app.home.questions.archive_command.state,
        State::Idle
    ));
    // A late exit retires nothing: no archive follows a refused stop.
    assert!(archives(&fold_lines(&mut app, left(A, "exited"))).is_empty());
}

#[test]
fn stop_with_no_answer_shows_stop_status_unknown() {
    let mut app = attached();
    let _lines = start(&mut app, &[A], &[A]);
    // The connection drops with the stop unanswered: its outcome is
    // unknown, so nothing is archived.
    app.disconnected();
    assert_eq!(app.notice(), Some("Stop status unknown"));
    assert!(matches!(
        app.home.questions.archive_command.state,
        State::Idle
    ));
    assert!(archives(&fold_lines(&mut app, left(A, "exited"))).is_empty());
}

#[test]
fn refused_archive_keeps_rows_with_one_notice() {
    let mut app = attached();
    let _lines = start(&mut app, &[A], &[A]);
    let out = fold_lines(&mut app, left(A, "exited"));
    let archived = archives(&out);
    assert_eq!(archived.len(), 1);
    let id = archived[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("archive id"))
        .to_owned();
    let failed = "the hub is busy";
    let _out = fold_lines(&mut app, rejected(&id, "session_held", failed));
    assert_eq!(
        app.notice(),
        Some(format!("Archive failed: {failed}").as_str())
    );
    assert!(matches!(
        app.home.questions.archive_command.state,
        State::Idle
    ));
}

#[test]
fn accepted_archive_without_a_result_archives_and_goes_home() {
    // The hub accepts `archive` with no `result`
    // (`crates/hub/src/archive.rs` returns `Ok(None)`, folded as
    // `command_accepted` with no `result` per `crates/hub/src/feed.rs`):
    // the command treats it as success, removing the row and going
    // home. An `archived`-flag check would report "Not archived" here.
    let mut app = attached();
    let _lines = start(&mut app, &[A], &[A]);
    let out = fold_lines(&mut app, left(A, "exited"));
    let archived = archives(&out);
    assert_eq!(archived.len(), 1);
    let id = archived[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("archive id"))
        .to_owned();
    let missing = Line::Hub(contract::HubLine {
        kind: HubEventKind::CommandAccepted,
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: [("command_id".to_owned(), Value::String(id.clone()))]
            .into_iter()
            .collect(),
    });
    let out = fold_lines(&mut app, missing);
    assert!(
        out.iter().any(|line| line["command"] == "recent"),
        "{out:?}"
    );
    assert!(app.on_home());
    assert_eq!(app.session(), None);
    assert!(app.notice().is_none());
    assert!(app.home.sessions.row(&SessionId(A.to_owned())).is_none());
    assert!(matches!(
        app.home.questions.archive_command.state,
        State::Idle
    ));
}

#[test]
fn malformed_previews_show_the_preview_notice_and_finish() {
    // No `result` in the discovery answer: the missing-result arm
    // finishes the command with the preview notice, so an empty
    // `archive_command_malformed_preview` would leave no notice and
    // keep the discovery running.
    let mut app = attached();
    let command = discovery(&mut app);
    let missing = Line::Hub(contract::HubLine {
        kind: HubEventKind::CommandAccepted,
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: [("command_id".to_owned(), Value::String(command.clone()))]
            .into_iter()
            .collect(),
    });
    let out = fold_lines(&mut app, missing);
    assert!(out.is_empty(), "{out:?}");
    assert_eq!(
        app.notice(),
        Some("Not archived: the hub's preview was invalid. Nothing was archived.")
    );
    assert!(matches!(
        app.home.questions.archive_command.state,
        State::Idle
    ));
    // An invalid preview folds through the validated-sets arm the same
    // way: each of these fails `validated_sets`, so each must finish
    // with the same notice.
    for result in [
        json!({"sessions": [], "held": []}),
        json!({"sessions": [B], "held": []}),
        json!({"sessions": [A]}),
    ] {
        let mut app = attached();
        let command = discovery(&mut app);
        let out = fold_lines(&mut app, accepted(&command, result.clone()));
        assert!(out.is_empty(), "{result}: {out:?}");
        assert_eq!(
            app.notice(),
            Some("Not archived: the hub's preview was invalid. Nothing was archived."),
            "{result}"
        );
        assert!(
            matches!(app.home.questions.archive_command.state, State::Idle),
            "{result}"
        );
    }
}

#[test]
fn unanswered_archive_is_uncertain_and_is_not_retried() {
    let mut app = attached();
    let _lines = start(&mut app, &[A], &[A]);
    let out = fold_lines(&mut app, left(A, "exited"));
    let archived = archives(&out);
    assert_eq!(archived.len(), 1);
    let id = archived[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("archive id"))
        .to_owned();
    // The connection drops with the archive in flight: its answer never
    // arrives, so nothing confirms the archival.
    app.disconnected();
    app.home_reset();
    assert_eq!(app.notice(), Some("Archive status unknown"));
    assert!(app.on_home());
    assert_eq!(app.session(), None);
    assert!(matches!(
        app.home.questions.archive_command.state,
        State::Idle
    ));
    // The lost command no longer owns the state, and reconnect does not
    // resend or retry the archival line.
    assert!(!app.outbox.is_kept(&id));
}

#[test]
fn unsent_archive_settles_the_tagged_command() {
    use crate::app::Waiting;
    let mut app = attached();
    let _lines = start(&mut app, &[A], &[A]);
    let out = fold_lines(&mut app, left(A, "exited"));
    let archived = archives(&out);
    assert_eq!(archived.len(), 1);
    let id = archived[0]["id"].as_str().unwrap_or_default().to_owned();
    assert_eq!(
        app.outbox.waiting(&id),
        Some(Waiting::ArchiveFinal(generation(&app)))
    );
    app.home_unsent(&[out[0].to_string()]);
    assert_eq!(app.notice(), Some("Archive status unknown"));
    assert!(matches!(
        app.home.questions.archive_command.state,
        State::Idle
    ));
    assert_eq!(app.outbox.waiting(&id), None);
}

#[test]
fn unsent_discovery_is_silent_and_leaves_the_state() {
    let mut app = attached();
    let lines = sent(slash(&mut app, "/archive"));
    assert_eq!(lines.len(), 1);
    let id = lines[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("discovery id"))
        .to_owned();
    // The write never went out: no notice fires and the discovery
    // answer can still fold if it ever arrives, as with `/delete`.
    app.home_unsent(&[lines[0].to_string()]);
    assert!(app.notice().is_none());
    assert!(matches!(
        app.home.questions.archive_command.state,
        State::Discovering { .. }
    ));
    assert_eq!(
        app.outbox.waiting(&id),
        Some(crate::app::Waiting::ArchiveDiscovery(generation(&app)))
    );
}

#[test]
fn unsent_stops_show_stop_status_unknown() {
    let mut app = attached();
    let lines = start(&mut app, &[A], &[A]);
    let died = closes(&lines);
    assert_eq!(died.len(), 1);
    app.home_unsent(&[died[0].to_string()]);
    assert_eq!(app.notice(), Some("Stop status unknown"));
    assert!(matches!(
        app.home.questions.archive_command.state,
        State::Idle
    ));
}

#[test]
fn repeat_while_running_keeps_the_first_command() {
    let mut app = attached();
    let _lines = start(&mut app, &[A], &[A]);
    app.draft.paste("/archive");
    assert_eq!(app.archive_on_screen(), Effect::None);
    assert_eq!(app.draft(), "/archive");
    assert_eq!(app.notice(), Some("`/archive` is already in progress."));
    assert!(matches!(
        app.home.questions.archive_command.state,
        State::Stopping { .. }
    ));
}

const C: &str = "s_cccccccccccccccc";

#[test]
fn a_stale_stop_answer_is_ignored() {
    use crate::app::Waiting;
    let mut app = attached();
    let lines = start(&mut app, &[A], &[A]);
    assert_eq!(closes(&lines).len(), 1);
    // A superseded command's stop answers late, while this one still
    // stops: its generation no longer runs, so the fold claims nothing,
    // settles nothing and notices nothing.
    let earlier = generation(&app).wrapping_sub(1);
    app.outbox.wait(
        "c_stale",
        Waiting::ArchiveStop {
            generation: earlier,
            session: SessionId(A.to_owned()),
        },
    );
    let out = fold_lines(&mut app, rejected("c_stale", "io_failed", "Late."));
    assert!(out.is_empty(), "{out:?}");
    assert_eq!(app.notice(), None);
    assert!(matches!(
        app.home.questions.archive_command.state,
        State::Stopping { .. }
    ));
    assert!(app.outbox.waiting("c_stale").is_some());
}

#[test]
fn three_exits_send_one_archive_after_the_last() {
    let mut app = attached();
    app.on_line(live(B, "forked"));
    app.on_line(live(C, "third work"));
    let lines = start(&mut app, &[A, B, C], &[A, B, C]);
    assert_eq!(closes(&lines).len(), 3);
    // The first two exits retire a stop each but send no archive yet:
    // the archive waits for every held stop, so retiring on a middle
    // exit would send it early.
    assert!(archives(&fold_lines(&mut app, left(B, "exited"))).is_empty());
    assert!(archives(&fold_lines(&mut app, left(C, "exited"))).is_empty());
    // The last exit sends exactly one archive line, for the target alone.
    let out = fold_lines(&mut app, left(A, "exited"));
    let archived = archives(&out);
    assert_eq!(archived.len(), 1, "{out:?}");
    assert_eq!(archived[0]["args"], json!({"session": A}));
}

#[test]
fn one_exit_keeps_only_the_other_sessions_close() {
    let mut app = attached();
    app.on_line(live(B, "forked"));
    let lines = start(&mut app, &[A, B], &[A, B]);
    assert_eq!(closes(&lines).len(), 2);
    // The exited stop's close retires with it; the still-waiting
    // session's close stays, so a later quit resumes nothing stopped.
    let out = fold_lines(&mut app, left(B, "exited"));
    assert!(archives(&out).is_empty());
    assert_eq!(app.closing_sessions(), [A.to_owned()]);
}

#[test]
fn a_stale_archive_answer_is_ignored() {
    use crate::app::Waiting;
    let mut app = attached();
    let _lines = start(&mut app, &[A], &[A]);
    let out = fold_lines(&mut app, left(A, "exited"));
    let archived = archives(&out);
    assert_eq!(archived.len(), 1);
    // A superseded command's archive answers while this one archives:
    // its generation no longer runs, so the fold removes nothing,
    // refetches nothing and leaves the running archive waiting.
    let earlier = generation(&app).wrapping_sub(1);
    app.outbox.wait("c_stale", Waiting::ArchiveFinal(earlier));
    let out = fold_lines(&mut app, accepted("c_stale", json!({"archived": true})));
    assert!(out.is_empty(), "{out:?}");
    assert!(matches!(
        app.home.questions.archive_command.state,
        State::Archiving { .. }
    ));
    assert!(app.home.sessions.row(&SessionId(A.to_owned())).is_some());
}

#[test]
fn a_dropped_link_between_the_last_exit_and_the_archive_send_is_uncertain() {
    let mut app = attached();
    let lines = start(&mut app, &[A], &[A]);
    assert_eq!(closes(&lines).len(), 1);
    // The link drops after the last exit retires but before the archive
    // goes out: with nowhere to send it, the command ends in the archive
    // uncertainty instead of archiving blind.
    app.outbox.link = crate::app::Link::Down;
    let out = fold_lines(&mut app, left(A, "exited"));
    assert!(archives(&out).is_empty(), "{out:?}");
    assert_eq!(app.notice(), Some("Archive status unknown"));
    assert!(matches!(
        app.home.questions.archive_command.state,
        State::Idle
    ));
}

#[test]
fn accepted_archive_removes_the_target_row_and_keeps_the_survivor() {
    let mut app = attached();
    app.on_line(live(B, "forked"));
    let _lines = start(&mut app, &[A, B], &[A, B]);
    let out = fold_lines(&mut app, left(B, "exited"));
    assert!(archives(&out).is_empty());
    let out = fold_lines(&mut app, left(A, "exited"));
    let archived = archives(&out);
    assert_eq!(archived.len(), 1);
    let id = archived[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("archive id"))
        .to_owned();
    let out = fold_lines(&mut app, accepted(&id, json!({"archived": true})));
    // The archived target's row is gone, the cascade survivor stays, and
    // the totals refetch: archiving some other id would keep the target
    // while still refetching.
    assert!(
        app.home.sessions.row(&SessionId(A.to_owned())).is_none(),
        "the target row is removed"
    );
    assert!(
        app.home.sessions.row(&SessionId(B.to_owned())).is_some(),
        "the cascade survivor stays"
    );
    assert!(
        out.iter().any(|line| line["command"] == "recent"),
        "{out:?}"
    );
    assert!(matches!(
        app.home.questions.archive_command.state,
        State::Idle
    ));
}

#[test]
fn a_link_loss_drops_a_live_parked_close() {
    let mut app = attached();
    app.on_line(live(B, "forked"));
    // A quit close parked for the live session, as the quit question's
    // closes do: it is no command stop, so the link loss drops it with
    // the waiting stops instead of retaining it.
    app.park_closing(SessionId(B.to_owned()), "c_quit".to_owned());
    let _lines = start(&mut app, &[A], &[A]);
    app.disconnected();
    assert_eq!(app.notice(), Some("Stop status unknown"));
    assert!(matches!(
        app.home.questions.archive_command.state,
        State::Idle
    ));
    assert!(app.closing_sessions().is_empty());
}

#[test]
fn a_link_loss_keeps_an_exited_parked_close() {
    let mut app = attached();
    app.on_line(live(B, "forked"));
    app.park_closing(SessionId(B.to_owned()), "c_quit".to_owned());
    // The parked session exits on its own, with no command running: its
    // close stays parked, and the feed marks it left.
    let _ = fold_lines(&mut app, left(B, "exited"));
    let _lines = start(&mut app, &[A, B], &[A]);
    app.disconnected();
    // The stopped session's close stays, so a later quit never resumes
    // it; the still-waiting stop's close goes with the lost link.
    assert_eq!(app.notice(), Some("Stop status unknown"));
    assert!(matches!(
        app.home.questions.archive_command.state,
        State::Idle
    ));
    assert_eq!(app.closing_sessions(), [B.to_owned()]);
}

#[test]
fn a_finished_archive_keeps_no_close() {
    let mut app = attached();
    app.on_line(live(B, "forked"));
    let _lines = start(&mut app, &[A, B], &[A, B]);
    let out = fold_lines(&mut app, left(B, "exited"));
    assert!(archives(&out).is_empty());
    let out = fold_lines(&mut app, left(A, "exited"));
    let archived = archives(&out);
    assert_eq!(archived.len(), 1);
    let id = archived[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("archive id"))
        .to_owned();
    let _out = fold_lines(&mut app, accepted(&id, json!({"archived": true})));
    // The finished command's closes retire with it: keeping the
    // target's close would resume a stopped session on quit.
    assert!(matches!(
        app.home.questions.archive_command.state,
        State::Idle
    ));
    assert!(app.closing_sessions().is_empty());
}

#[test]
fn reset_during_discovery_drops_the_command_silently() {
    let mut app = attached();
    let command = discovery(&mut app);
    // The connection drops and resets with the discovery unanswered: the
    // command never started, so its tag settles with no notice and the
    // state returns to Idle.
    app.disconnected();
    app.home_reset();
    assert!(matches!(
        app.home.questions.archive_command.state,
        State::Idle
    ));
    assert_eq!(app.outbox.waiting(&command), None);
}

#[test]
fn accepted_archive_refetches_the_targets_project_page() {
    // The attached target lives in project `-w`. Its first status
    // already put one scoped ask for `-w` in flight; answer it here so
    // the asks below are the archive's own. The setup keeps the live
    // fold's lines instead of using `attached`, which drops them.
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    let live_out: Vec<Value> = app
        .on_line(live(A, "old work"))
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    app.attach(SessionId(A.to_owned()));
    let live_scoped = live_out
        .iter()
        .find(|line| {
            line["command"] == "recent"
                && line
                    .get("args")
                    .and_then(|args| args.get("project"))
                    .and_then(Value::as_str)
                    == Some("-w")
        })
        .unwrap_or_else(|| panic!("the live scoped ask: {live_out:?}"));
    let live_id = live_scoped["id"]
        .as_str()
        .unwrap_or_else(|| panic!("live scoped ask id: {live_scoped:?}"));
    let quiet: Vec<Value> = app
        .on_line(accepted(live_id, json!({"sessions": [], "total": 0})))
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    assert!(
        quiet.is_empty(),
        "a clean scoped answer sends no follow-up: {quiet:?}"
    );
    let _lines = start(&mut app, &[A], &[A]);
    let out = fold_lines(&mut app, left(A, "exited"));
    // The exit's project refresh goes out while the archive waits; answer
    // it here so the accept below re-asks the project's page itself.
    for line in out.iter().filter(|line| line["command"] == "recent") {
        let id = line["id"]
            .as_str()
            .unwrap_or_else(|| panic!("exit scoped ask id: {out:?}"));
        let quiet: Vec<Value> = app
            .on_line(accepted(id, json!({"sessions": [], "total": 0})))
            .iter()
            .map(|line| serde_json::from_str(line).unwrap_or_default())
            .collect();
        assert!(
            quiet.is_empty(),
            "a clean scoped answer sends no follow-up: {quiet:?}"
        );
    }
    let archived = archives(&out);
    assert_eq!(archived.len(), 1);
    let id = archived[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("archive id"))
        .to_owned();
    let lines: Vec<Value> = app
        .on_line(accepted(&id, json!({"archived": true})))
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    // The accepted archive refills the unscoped page and the archived
    // target's own project page. A `None` project sends no scoped ask
    // and an empty one is dropped by `scoped_first`, so either mutant
    // fails the scoped assertion while the unscoped refresh holds; a
    // wrong project asks for a page this terminal never shows.
    assert!(
        lines
            .iter()
            .any(|line| line["command"] == "recent" && line.get("args") == Some(&json!({}))),
        "the unscoped page refreshes: {lines:?}"
    );
    assert!(
        lines.iter().any(|line| line["command"] == "recent"
            && line
                .get("args")
                .and_then(|args| args.get("project"))
                .and_then(Value::as_str)
                == Some("-w")),
        "the archived target's project page refreshes: {lines:?}"
    );
    assert!(
        !lines.iter().any(|line| line["command"] == "recent"
            && line
                .get("args")
                .and_then(|args| args.get("project"))
                .and_then(Value::as_str)
                .is_some_and(|project| project != "-w")),
        "no other project's page refreshes on accept: {lines:?}"
    );
}

/// Drives the first command to its refused stop, so the next `/archive`
/// runs as generation 2 and no tag reads 1 by luck.
fn attached_second_command() -> App {
    let mut app = attached();
    let lines = start(&mut app, &[A], &[A]);
    let died = closes(&lines);
    assert_eq!(died.len(), 1);
    let close = died[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("close id"))
        .to_owned();
    let out = fold_lines(&mut app, rejected(&close, "session_held", "Busy."));
    assert!(archives(&out).is_empty(), "{out:?}");
    assert!(matches!(
        app.home.questions.archive_command.state,
        State::Idle
    ));
    // The first answer went home: re-attach for the second command.
    app.attach(SessionId(A.to_owned()));
    app
}

#[test]
fn reset_during_the_second_commands_discovery_settles_its_tag() {
    let mut app = attached_second_command();
    let command = discovery(&mut app);
    assert_eq!(generation(&app), 2);
    app.home_reset();
    assert_eq!(app.outbox.waiting(&command), None);
    assert!(matches!(
        app.home.questions.archive_command.state,
        State::Idle
    ));
}

#[test]
fn an_unsent_archive_of_the_second_command_settles_it() {
    let mut app = attached_second_command();
    let _lines = start(&mut app, &[A], &[A]);
    assert_eq!(generation(&app), 2);
    let out = fold_lines(&mut app, left(A, "exited"));
    let archived = archives(&out);
    assert_eq!(archived.len(), 1);
    let id = archived[0]["id"].as_str().unwrap_or_default().to_owned();
    app.home_unsent(&[out[0].to_string()]);
    assert_eq!(app.notice(), Some("Archive status unknown"));
    assert!(matches!(
        app.home.questions.archive_command.state,
        State::Idle
    ));
    assert_eq!(app.outbox.waiting(&id), None);
}

#[test]
fn an_unsent_stop_of_the_second_command_is_uncertain() {
    let mut app = attached_second_command();
    let lines = start(&mut app, &[A], &[A]);
    assert_eq!(generation(&app), 2);
    let died = closes(&lines);
    assert_eq!(died.len(), 1);
    app.home_unsent(&[died[0].to_string()]);
    assert_eq!(app.notice(), Some("Stop status unknown"));
    assert!(matches!(
        app.home.questions.archive_command.state,
        State::Idle
    ));
}
