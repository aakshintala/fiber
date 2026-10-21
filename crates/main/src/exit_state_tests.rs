//! The exit boundary closes, drains and only then writes the last line.

use contract::events::{CallStatus, Event};
use contract::hook::{AfterToolCall, ContextOutcome, Hooks, TurnEnd};
use extensions::SessionExtensions;
use serde_json::json;
use std::sync::Arc;

fn load(
    init: &str,
    clock: Arc<fakes::clock::FakeClock>,
) -> (fakes::TempDir, Arc<SessionExtensions>) {
    let root = fakes::TempDir::new("fiber-exit-state");
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    let source = root.path().join("source");
    for path in [&home, &workspace, &source] {
        std::fs::create_dir_all(path).unwrap();
    }
    std::fs::write(
        source.join("extension.json"),
        json!({"name":"fiber.test/goal","version":"v1.0.0","fiber":"0.1.0","api":1}).to_string(),
    )
    .unwrap();
    std::fs::write(source.join("init.lua"), init).unwrap();
    extensions::plan(
        &home,
        &extensions::Request::Path(source),
        "0.1.0",
        &extensions::Origin::github(),
        clock.as_ref(),
    )
    .unwrap()
    .commit()
    .unwrap();
    let config = config::Config::load(config::Sources {
        home: home.clone(),
        workspace,
        project: config::ProjectKey::new("state").unwrap(),
        overrides: Vec::new(),
    })
    .unwrap();
    let session = fakes::within(
        "extension state loaded",
        fakes::MUST_SUCCEED_WITHIN,
        move || {
            Arc::new(SessionExtensions::load(
                &home,
                &config,
                clock,
                Arc::new(fakes::FakeLock::new()),
                None,
            ))
        },
    );
    (root, session)
}
fn make_log(root: &fakes::TempDir) -> log::Log {
    log::Log::create(
        root.path(),
        contract::SessionId("s_state".into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap()
}
fn closing(session: Arc<SessionExtensions>) -> String {
    fakes::within(
        "closed state hook answered",
        fakes::MUST_SUCCEED_WITHIN,
        move || {
            let answer = session.turn_end(&TurnEnd {
                reply: "done",
                cancel: &fakes::CancelToken::new(),
            });
            let ContextOutcome::Added(contexts) = answer.outcome else {
                panic!("hook failed")
            };
            contexts[0].text.clone()
        },
    )
}
const CLOSE_HOOK: &str = "fiber.hook('turn_end',{timeout=5000,on_failure='blocking',run=function() local ok,e=pcall(state.set,'x',1); return {message=ok and 'open' or e.code} end})";

#[test]
fn exit_state_closes_drains_once_and_precedes_fiber_exited() {
    let (root, session) = load(
        &format!("state.set('boot',1);{CLOSE_HOOK}"),
        fakes::clock::FakeClock::new(),
    );
    let log = make_log(&root);
    let dir = root.path().join("s_state");
    super::write_state_at_exit(&log, &dir, &session).unwrap();
    r#loop::fiber_exited(&log, &dir, Ok(()), false, None).unwrap();
    super::write_state_at_exit(&log, &dir, &session).unwrap();
    let lines = log::read(&dir).unwrap();
    assert_eq!(
        lines
            .iter()
            .map(|line| line.kind.as_str())
            .collect::<Vec<_>>(),
        ["extension_state_set", "fiber_exited"]
    );
    assert_eq!(lines[0].payload["key"], "boot");
    assert_eq!(closing(session.clone()), "closing");
    assert!(session.take_state_lines().is_empty());
}

#[test]
fn exit_state_empty_outbox_still_closes() {
    let (root, session) = load(CLOSE_HOOK, fakes::clock::FakeClock::new());
    let log = make_log(&root);
    let dir = root.path().join("s_state");
    super::write_state_at_exit(&log, &dir, &session).unwrap();
    assert!(log::read(&dir).unwrap().is_empty());
    assert_eq!(closing(session.clone()), "closing");
    assert!(session.take_state_lines().is_empty());
}

#[test]
fn exit_state_never_writes_a_dropped_changes_lines() {
    let (root, session) = load(
        "fiber.hook('after_tool',{timeout=5000,on_failure='blocking',run=function() state.set('changed',1);return {content='changed'} end})",
        fakes::clock::FakeClock::new(),
    );
    let held = session.clone();
    fakes::within(
        "changed hook returned",
        fakes::MUST_SUCCEED_WITHIN,
        move || {
            held.after_tool(&AfterToolCall {
                tool: "read",
                arguments: &serde_json::Map::new(),
                status: CallStatus::Completed,
                content: "raw",
                details: None,
                process: None,
                spilled: None,

                not_regular: false,
                delivery: None,
            })
        },
    );
    assert_eq!(session.take_change_state().len(), 1);
    let log = make_log(&root);
    let dir = root.path().join("s_state");
    super::write_state_at_exit(&log, &dir, &session).unwrap();
    assert!(log::read(&dir).unwrap().is_empty());
}

#[test]
fn exit_state_rewound_log_gets_no_new_line_but_the_store_closes() {
    let (root, session) = load(
        &format!("state.set('boot',1);{CLOSE_HOOK}"),
        fakes::clock::FakeClock::new(),
    );
    let log = make_log(&root);
    let dir = root.path().join("s_state");
    // The exit helper needs only the discriminator, not a rewind payload.
    std::fs::write(dir.join("events.jsonl"),format!("{}\n",json!({"kind":"rewound","session_id":"s_state","schema_version":1,"ts":0,"seq":0,"payload":{}}))).unwrap();
    let before = std::fs::read(dir.join("events.jsonl")).unwrap();
    super::write_state_at_exit(&log, &dir, &session).unwrap();
    assert_eq!(std::fs::read(dir.join("events.jsonl")).unwrap(), before);
    assert_eq!(closing(session.clone()), "closing");
    assert!(session.take_state_lines().is_empty());
}

#[test]
fn exit_state_timer_write_uses_an_injected_clock_and_survives_clean_exit() {
    struct Signal(std::sync::mpsc::Sender<()>);
    impl contract::emit::Emit for Signal {
        fn emit(&self, event: &Event) {
            if matches!(event, Event::ExtensionUi(_)) {
                let _sent = self.0.send(());
            }
        }
    }
    let clock = fakes::clock::FakeClock::new();
    let (sent, ready) = std::sync::mpsc::channel();
    let (root, session) = load(
        &format!(
            "{CLOSE_HOOK};host.after(100,function() state.set('late',1);host.status('written') end,{{timeout=5000}})"
        ),
        clock.clone(),
    );
    session.emit_to(Arc::new(Signal(sent)));
    let deadline = fakes::deadline::Deadline::start();
    let delay = std::time::Duration::from_millis(100);
    let _mark = clock.advance_when_parked(Some(clock.origin() + delay), None, delay, &deadline);
    deadline
        .recv(&ready)
        .expect("timer signaled after writing state");
    let log = make_log(&root);
    let dir = root.path().join("s_state");
    super::write_state_at_exit(&log, &dir, &session).unwrap();
    r#loop::fiber_exited(&log, &dir, Ok(()), false, None).unwrap();
    let lines = log::read(&dir).unwrap();
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.kind == "extension_state_set" && line.payload["key"] == "late")
            .count(),
        1
    );
    assert_eq!(lines[0].kind, "extension_state_set");
    assert_eq!(lines.last().unwrap().kind, "fiber_exited");
}

/// The drain exit keeps the `fiber_exited` code normally and the shutdown
/// code once a signal stops the drain (Ruling 19). Flipping the branch
/// would return the pre-signal code after a signal.
#[test]
fn drain_exit_prefers_the_shutdown_code() {
    assert_eq!(super::drain_exit_code(0, false, None), 0);
    assert_eq!(super::drain_exit_code(0, true, Some(143)), 143);
    // A stop with no shutdown recorded (an abandonment, an idle hub)
    // keeps `exited`.
    assert_eq!(super::drain_exit_code(7, true, None), 7);
}

/// A shutdown that arrives after the drain already finished changes
/// nothing: the drain was not stopped, so the exit stays the
/// `fiber_exited` code even though a shutdown code is recorded.
#[test]
fn drain_exit_ignores_a_shutdown_after_the_drain_finished() {
    assert_eq!(super::drain_exit_code(0, false, Some(129)), 0);
}
