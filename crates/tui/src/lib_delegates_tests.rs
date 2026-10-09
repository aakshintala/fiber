//! A loop-level journey for the Delegates card: four running Fiber
//! delegates are subscribed at `summary`, one delegate's status draws its
//! state word, and the wheel over the card scrolls it (`docs/tui.md`,
//! "The panel").

use super::Input;
use super::tests::{feed, hello, new_loop};
use crate::home::Launch;
use crate::link::Line;
use ratatui::backend::TestBackend;
use std::io::Read;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";
const CARDS: [&str; 5] = ["session", "changed_files", "delegates", "jobs", "quota"];

/// The screen's text.
fn shown(lp: &super::Loop<TestBackend>) -> String {
    crate::view::text(lp.screen.backend().buffer())
}

/// A wheel-down report at 0-based `col`, `row`.
fn wheel(col: u16, row: u16) -> Input {
    Input::Bytes(format!("\x1b[<65;{};{}M", col + 1, row + 1).into_bytes())
}

/// One envelope of `session`.
fn envelope(session: &str, kind: &str, payload: serde_json::Value) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId("a_1".to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// The `job_started` and `delegate_started` lines for job `job` with
/// delegate session `delegate`.
fn started(job: &str, delegate: &str) -> Vec<Line> {
    vec![
        envelope(
            SESSION,
            "job_started",
            serde_json::json!({"job_id": job, "description": format!("task {job}"),
                "output_path": "/tmp/out"}),
        ),
        envelope(
            SESSION,
            "delegate_started",
            serde_json::json!({"job_id": job,
                "delegate_session_id": delegate,
                "harness": "fiber", "model": "test/model", "workspace": "/w"}),
        ),
    ]
}

/// A live `session_status` for `session` in `state`.
fn live(session: &str, state: &str) -> Line {
    envelope(
        session,
        "session_status",
        serde_json::json!({
            "name": "fix the parser", "workspace": "/w", "project": "-w",
            "state": state, "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 4, "jobs": 0, "clients": 0,
        }),
    )
}

/// A delegate's `session_status` in `state`, naming its parent.
fn delegate_status(session: &str, state: &str) -> Line {
    envelope(
        session,
        "session_status",
        serde_json::json!({
            "name": "delegate one", "workspace": "/w", "project": "-w",
            "state": state, "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
            "parent": SESSION,
        }),
    )
}

/// Every command line buffered on `stream`, without waiting: the writes
/// finish inside `run`.
fn buffered(mut stream: UnixStream) -> Vec<serde_json::Value> {
    stream
        .set_nonblocking(true)
        .unwrap_or_else(|err| panic!("nonblocking: {err}"));
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => bytes.extend_from_slice(chunk.get(..n).unwrap_or_else(|| panic!("a chunk"))),
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(err) => panic!("read: {err}"),
        }
    }
    let text = String::from_utf8(bytes).unwrap_or_else(|err| panic!("utf8: {err}"));
    text.lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line:?}: {err}")))
        .collect()
}

#[test]
fn a_delegate_is_subscribed_drawn_and_scrolled_in_the_loop() {
    let (mut lp, _) = new_loop(TestBackend::new(160, 40), None);
    lp.app.set_size(160, 40);
    lp.screen
        .resize(160, 40)
        .unwrap_or_else(|err| panic!("resize: {err}"));
    lp.app.set_home(Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: CARDS.map(str::to_owned).to_vec(),
        ..Default::default()
    });
    lp.app.attach(contract::SessionId(SESSION.to_owned()));
    let (ours, theirs) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    feed(&mut lp, vec![Input::Connected(ours, hello())]);
    let delegates = [
        "s_d111111111111111",
        "s_d222222222222222",
        "s_d333333333333333",
        "s_d444444444444444",
    ];
    let mut hub = Vec::new();
    for (at, delegate) in delegates.iter().enumerate() {
        let job = format!("j_{}", at + 1);
        hub.extend(started(&job, delegate).into_iter().map(Input::Hub));
    }
    hub.push(Input::Hub(live(SESSION, "streaming")));
    feed(&mut lp, hub);
    let subscribes: Vec<serde_json::Value> = buffered(theirs)
        .into_iter()
        .filter(|line| line["command"] == "subscribe")
        .collect();
    assert_eq!(subscribes.len(), 4);
    for (subscribed, delegate) in subscribes.iter().zip(delegates.iter()) {
        assert_eq!(subscribed["session_id"], *delegate);
        assert_eq!(subscribed["args"]["level"], "summary");
    }
    feed(
        &mut lp,
        vec![Input::Hub(delegate_status(delegates[0], "streaming"))],
    );
    let screen = shown(&lp);
    assert!(screen.contains("⠋ WORKING"), "{screen}");
    assert!(!screen.contains("delegate one"), "{screen}");
    let panel = lp
        .app
        .chrome()
        .layout()
        .and_then(|layout| layout.panel)
        .unwrap_or_else(|| panic!("a panel"));
    let (rows, span) = crate::view::panel::rows_and_delegates(&lp.app, panel.width);
    let height = usize::from(panel.height.saturating_sub(1));
    let skip = lp
        .app
        .panel_state()
        .scroll()
        .min(rows.len().saturating_sub(height));
    let start = span.unwrap_or_else(|| panic!("the card's rows")).start;
    let row = panel
        .y
        .saturating_add(1)
        .saturating_add(u16::try_from(start.saturating_sub(skip)).unwrap_or(u16::MAX));
    feed(&mut lp, vec![wheel(panel.x.saturating_add(4), row)]);
    let screen = shown(&lp);
    assert!(screen.contains("task j_4"), "{screen}");
    assert!(!screen.contains("task j_1"), "{screen}");
}
