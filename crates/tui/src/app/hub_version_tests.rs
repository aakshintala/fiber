//! Tests for the hub's build against this terminal's: the comparison, the
//! notice, and the reason an unreadable row gives.

use serde_json::{Value, json};

use super::super::notices::NoticeLine;
use super::super::{App, Link};
use super::{build_text, differs};
use crate::home::{Launch, LaunchLocation};
use crate::link::Line;
use std::path::PathBuf;

const SHA: &str = "2121f38d1e4f0a9b6c3d2e1f0a9b8c7d6e5f4a3b";
const OTHER: &str = "d13fc453846698c07c1dfd3f313d480f097a624e";

#[test]
fn the_build_text_is_fiber_versions_form() {
    assert_eq!(build_text("0.0.0", Some(SHA)), "0.0.0 (2121f38)");
    assert_eq!(build_text("0.0.0", Some("abc")), "0.0.0 (abc)");
    assert_eq!(build_text("0.0.0", Some("1234567")), "0.0.0 (1234567)");
    assert_eq!(build_text("0.0.0", Some("")), "0.0.0");
    assert_eq!(build_text("0.0.0", None), "0.0.0");
}

#[test]
fn differing_versions_differ() {
    assert!(differs(("0.0.0", None), ("9.9.9", None)));
    assert!(differs(("0.0.0", Some(SHA)), ("9.9.9", Some(SHA))));
}

#[test]
fn equal_versions_differ_only_when_both_commits_are_present_and_unequal() {
    assert!(differs(("0.0.0", Some(SHA)), ("0.0.0", Some(OTHER))));
    assert!(!differs(("0.0.0", Some(SHA)), ("0.0.0", Some(SHA))));
    assert!(!differs(("0.0.0", Some(SHA)), ("0.0.0", None)));
    assert!(!differs(("0.0.0", None), ("0.0.0", Some(SHA))));
    assert!(!differs(("0.0.0", None), ("0.0.0", None)));
}

#[test]
fn an_empty_commit_counts_as_none() {
    assert!(!differs(("0.0.0", Some("")), ("0.0.0", Some(SHA))));
    assert!(!differs(("0.0.0", Some(SHA)), ("0.0.0", Some(""))));
    assert!(!differs(("0.0.0", Some("")), ("0.0.0", Some(""))));
}

fn app_for(location: LaunchLocation, commit: Option<&str>) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        location,
        version: "0.0.0".to_owned(),
        commit: commit.map(str::to_owned),
        rail_share: 15.0,
        panel_share: 21.0,
        ..Default::default()
    });
    app.set_size(200, 40);
    app
}

fn local() -> LaunchLocation {
    LaunchLocation::Local {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        git: false,
    }
}

/// A terminal built from `0.0.0` at `OTHER`.
fn app() -> App {
    app_for(local(), Some(OTHER))
}

fn hello(schema: u32, payload: Value) -> Line {
    Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: schema,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

fn readable(payload: Value) -> Line {
    hello(contract::SCHEMA_VERSION, payload)
}

/// The `hub_version` notices on screen.
fn notices(app: &App) -> Vec<String> {
    app.notices()
        .into_iter()
        .filter_map(|line| match line {
            NoticeLine::Notice { code, text, .. } if code == "hub_version" => Some(text),
            _ => None,
        })
        .collect()
}

const NOTICE: &str = "The hub runs Fiber 9.9.9 (2121f38) (this terminal: 0.0.0 (d13fc45)). `fiber stop` restarts it on the next launch.";

#[test]
fn another_version_pushes_one_notice_and_the_link_goes_up() {
    let mut app = app();
    app.on_line(readable(
        json!({"fiber_version": "9.9.9", "fiber_commit": SHA}),
    ));
    assert_eq!(notices(&app), [NOTICE]);
    assert_eq!(app.outbox.link, Link::Up);
}

#[test]
fn the_same_version_and_another_commit_pushes_the_notice() {
    let mut app = app();
    app.on_line(readable(
        json!({"fiber_version": "0.0.0", "fiber_commit": SHA}),
    ));
    assert_eq!(
        notices(&app),
        [
            "The hub runs Fiber 0.0.0 (2121f38) (this terminal: 0.0.0 (d13fc45)). `fiber stop` restarts it on the next launch."
        ]
    );
}

#[test]
fn the_same_build_pushes_nothing() {
    let mut app = app();
    app.on_line(readable(
        json!({"fiber_version": "0.0.0", "fiber_commit": OTHER}),
    ));
    assert!(notices(&app).is_empty());
}

#[test]
fn a_hello_with_no_or_an_empty_or_a_non_string_version_pushes_nothing() {
    for payload in [
        json!({}),
        json!({"fiber_version": ""}),
        json!({"fiber_version": 9}),
    ] {
        let mut app = app();
        app.on_line(readable(payload.clone()));
        assert!(notices(&app).is_empty(), "{payload}");
        assert_eq!(
            app.unreadable_reason(),
            "Cannot attach: its status did not parse"
        );
    }
}

#[test]
fn a_non_string_or_empty_commit_reads_as_absent() {
    for commit in [json!(5), json!("")] {
        let mut app = app();
        app.on_line(readable(
            json!({"fiber_version": "0.0.0", "fiber_commit": commit}),
        ));
        assert!(notices(&app).is_empty(), "{commit}");
    }
}

#[test]
fn a_terminal_with_no_home_pushes_nothing() {
    let mut app = App::new(PathBuf::from("/w"));
    app.on_line(readable(json!({"fiber_version": "9.9.9"})));
    assert!(notices(&app).is_empty());
}

#[test]
fn a_refused_schema_shows_both_notices_and_stays_refused() {
    let mut app = app();
    app.on_line(hello(
        contract::SCHEMA_VERSION + 1,
        json!({"fiber_version": "9.9.9", "fiber_commit": SHA}),
    ));
    assert_eq!(notices(&app), [NOTICE]);
    assert_eq!(app.outbox.link, Link::Refused);
    assert!(
        app.notices().iter().any(|line| matches!(
            line,
            NoticeLine::Notice { code, .. } if code == "connection_failed"
        )),
        "the schema notice shows too"
    );
}

#[test]
fn each_connection_pushes_once() {
    let mut app = app();
    let hello = || readable(json!({"fiber_version": "9.9.9", "fiber_commit": SHA}));
    app.on_line(hello());
    assert_eq!(notices(&app).len(), 1);
    app.disconnected();
    app.on_line(hello());
    assert_eq!(
        notices(&app).len(),
        1,
        "a notice still standing is not stacked twice"
    );
    let ids: Vec<usize> = app
        .notices()
        .into_iter()
        .filter_map(|line| match line {
            NoticeLine::Notice { id, .. } => Some(id),
            NoticeLine::More(_) => None,
        })
        .collect();
    for id in ids {
        app.dismiss_notice(id);
    }
    app.disconnected();
    app.on_line(hello());
    assert_eq!(notices(&app), [NOTICE]);
}

#[test]
fn a_listed_hub_names_its_stop_command_and_no_restart() {
    let mut app = app_for(
        LaunchLocation::Remote {
            hub: "work".to_owned(),
        },
        Some(OTHER),
    );
    app.on_line(readable(
        json!({"fiber_version": "9.9.9", "fiber_commit": SHA}),
    ));
    assert_eq!(
        notices(&app),
        [
            "The hub runs Fiber 9.9.9 (2121f38) (this terminal: 0.0.0 (d13fc45)); `fiber stop --hub work` stops it."
        ]
    );
}

#[test]
fn a_hub_build_with_control_characters_is_stripped_in_the_notice() {
    let mut app = app_for(local(), None);
    app.on_line(readable(json!({
        "fiber_version": "9.\u{1b}[31m9\n.9",
        "fiber_commit": "ab\u{1b}cd\nef",
    })));
    let text = notices(&app).remove(0);
    assert_eq!(
        text,
        "The hub runs Fiber 9.[31m9.9 (abcdef) (this terminal: 0.0.0). `fiber stop` restarts it on the next launch."
    );
    assert!(!text.chars().any(char::is_control));
}

#[test]
fn a_later_same_build_hello_drops_the_difference() {
    let mut app = app();
    app.on_line(readable(json!({"fiber_version": "9.9.9"})));
    assert_eq!(
        app.unreadable_reason(),
        "Cannot attach: this session runs Fiber 9.9.9"
    );
    app.disconnected();
    app.on_line(readable(json!({"fiber_version": "0.0.0"})));
    assert_eq!(
        app.unreadable_reason(),
        "Cannot attach: its status did not parse"
    );
}

#[test]
fn the_reason_names_the_hubs_build_after_a_differing_hello() {
    let mut app = app();
    assert_eq!(
        app.unreadable_reason(),
        "Cannot attach: its status did not parse"
    );
    app.on_line(readable(
        json!({"fiber_version": "9.9.9", "fiber_commit": SHA}),
    ));
    assert_eq!(
        app.unreadable_reason(),
        "Cannot attach: this session runs Fiber 9.9.9 (2121f38)"
    );
}

fn status(id: &str, payload: Value) -> Line {
    Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: contract::SessionId(id.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

fn readable_status() -> Value {
    json!({
        "name": "fix the parser", "workspace": "/w", "project": "-w",
        "state": "idle", "started": 0, "calls": 0, "since": 0,
        "spend": {"tokens": {"input": 1, "cache_read": 0,
            "cache_write": {}, "output": 2},
            "cost": 0.0, "subscription_cost": 0.0},
        "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
    })
}

/// Each live row's id and second-line text on home's screen.
fn waits(app: &App) -> Vec<(String, Option<String>)> {
    app.home_screen()
        .unwrap_or_else(|| panic!("home on screen"))
        .list
        .into_iter()
        .filter_map(|line| match line {
            crate::home::ListLine::Live { id, wait, .. } => Some((id, wait)),
            _ => None,
        })
        .collect()
}

const GOOD: &str = "s_aaaaaaaaaaaaaaaa";
const BAD: &str = "s_bbbbbbbbbbbbbbbb";

#[test]
fn an_unreadable_row_carries_the_reason_and_a_readable_row_none() {
    let mut app = app();
    app.on_line(readable(
        json!({"fiber_version": "9.9.9", "fiber_commit": SHA}),
    ));
    app.on_line(status(GOOD, readable_status()));
    app.on_line(status(BAD, json!({})));
    assert_eq!(
        waits(&app),
        [
            (GOOD.to_owned(), None),
            (
                BAD.to_owned(),
                Some("Cannot attach: this session runs Fiber 9.9.9 (2121f38)".to_owned())
            )
        ]
    );
}

#[test]
fn an_unreadable_row_says_it_did_not_parse_after_a_same_build_hello() {
    let mut app = app();
    app.on_line(readable(
        json!({"fiber_version": "0.0.0", "fiber_commit": OTHER}),
    ));
    app.on_line(status(BAD, json!({})));
    assert_eq!(
        waits(&app),
        [(
            BAD.to_owned(),
            Some("Cannot attach: its status did not parse".to_owned())
        )]
    );
}

#[test]
fn opening_an_unreadable_row_names_the_hubs_build() {
    let mut app = app();
    app.on_line(readable(
        json!({"fiber_version": "9.9.9", "fiber_commit": SHA}),
    ));
    app.on_line(status(BAD, json!({})));
    let key = app
        .home_screen()
        .and_then(|screen| {
            screen.list.into_iter().find_map(|line| match line {
                crate::home::ListLine::Live { key, .. } => Some(key),
                _ => None,
            })
        })
        .unwrap_or_else(|| panic!("a live row"));
    app.home_click(crate::home::Spot::Entry(key));
    assert_eq!(
        app.notice(),
        Some("Cannot attach: this session runs Fiber 9.9.9 (2121f38).")
    );
}
