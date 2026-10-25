//! Quota notices in turn input, and the session's durable re-arm fold.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code, helpers included"
)]
mod support;

use contract::Envelope;
use contract::clock::Clock;
use contract::events::{QuotaInfo, TurnOutcome};
use contract::provider::Input;
use fakes::Scripted;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use support::{Session, delivery, kinds};

type Reading = Arc<Mutex<(Vec<QuotaInfo>, f64)>>;

fn reading(percent: f64, reset: Option<u64>) -> Reading {
    Arc::new(Mutex::new((vec![serde_json::from_value(json!({"provider":"codex","credential":"work","windows":[{"name":"weekly","percent_used":percent,"resets_at":reset}]})).unwrap()], 80.0)))
}

fn wire(session: &mut Session, reading: &Reading) {
    let reading = reading.clone();
    session.looped = session
        .looped
        .take()
        .map(|looped| looped.quota(Arc::new(move || reading.lock().unwrap().clone())));
}

fn script(turns: usize) -> Vec<Scripted> {
    (0..turns).map(|_| Scripted::text("Done.")).collect()
}

fn turn(session: &mut Session) -> Vec<Envelope> {
    session.inbox.send(delivery("next")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    session.lines()
}

fn quota_events(lines: &[Envelope]) -> Vec<&Envelope> {
    lines
        .iter()
        .filter(|line| line.kind.starts_with("quota_"))
        .collect()
}

fn check_turn(lines: &[Envelope], quota: &[&str], fiber: Option<&str>) {
    let at = lines
        .iter()
        .position(|line| line.kind == "turn_started")
        .unwrap();
    assert!(matches!(
        kinds(&lines[..at]).as_slice(),
        [] | ["session_started", "preamble_built", "opening_message"]
            | ["fiber_started", "preamble_built"]
    ));
    let mut expected = vec!["turn_started"];
    expected.extend_from_slice(quota);
    expected.extend([
        "step_started",
        "assistant_message_started",
        "assistant_message_delta",
        "assistant_message_delta",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
    ]);
    assert_eq!(kinds(&lines[at..]), expected);
    let input = lines[at].payload["input"].as_array().unwrap();
    assert_eq!(input.len(), if fiber.is_some() { 2 } else { 1 });
    if let Some(text) = fiber {
        assert_eq!(
            input.last().unwrap(),
            &json!({"type":"message","source":"fiber","content":[{"type":"text","text":text}]})
        );
    }
    for event in quota_events(lines) {
        assert_eq!(event.turn_id, lines[at].turn_id);
    }
}

#[test]
fn a_capped_credit_notices_on_the_first_turn_and_not_the_next() {
    let mut session = Session::new(script(2), None);
    let cache = Arc::new(Mutex::new((vec![serde_json::from_value(json!({"provider":"openrouter","credential":"work","credit":{"remaining":1.0,"limit":10.0}})).unwrap()],80.0)));
    wire(&mut session, &cache);
    let lines = turn(&mut session);
    check_turn(
        &lines,
        &["quota_noticed"],
        Some("Fiber: openrouter credential work has used 90% of its credit quota."),
    );
    assert_eq!(quota_events(&lines)[0].payload, serde_json::from_value::<serde_json::Map<String,Value>>(json!({"provider":"openrouter","credential":"work","window":"credit","percent_used":90.0,"notice_at":80.0})).unwrap());
    check_turn(&turn(&mut session), &[], None);
}

const LINE: &str = "Fiber: codex credential work has used 84% of its weekly quota, which resets at 2025-10-09T08:53:20Z.";

#[test]
fn quota_notice_is_input_not_preamble_and_fires_once_until_a_low_reading() {
    let mut baseline = Session::new(script(1), None);
    let plain = turn(&mut baseline);
    let mut session = Session::new(script(5), None);
    let cache = reading(83.5, Some(1760000000000));
    wire(&mut session, &cache);
    let lines = turn(&mut session);
    check_turn(&lines, &["quota_noticed"], Some(LINE));
    assert_eq!(
        kinds(&plain),
        kinds(&lines)
            .into_iter()
            .filter(|kind| *kind != "quota_noticed")
            .collect::<Vec<_>>()
    );
    assert_eq!(quota_events(&lines)[0].payload, serde_json::from_value::<serde_json::Map<String,Value>>(json!({"provider":"codex","credential":"work","window":"weekly","percent_used":83.5,"resets_at":1760000000000_u64,"notice_at":80.0})).unwrap());
    let request = session.requests().pop().unwrap();
    let plain_request = baseline.requests().pop().unwrap();
    assert_eq!(request.system_prompt, plain_request.system_prompt);
    assert_eq!(request.tools, plain_request.tools);
    assert_eq!(
        request.conversation.last(),
        Some(&Input::User {
            text: LINE.into(),
            images: vec![]
        })
    );
    assert_eq!(
        lines
            .iter()
            .find(|l| l.kind == "preamble_built")
            .unwrap()
            .payload,
        plain
            .iter()
            .find(|l| l.kind == "preamble_built")
            .unwrap()
            .payload
    );
    check_turn(&turn(&mut session), &[], None);
    cache.lock().unwrap().0[0].windows.as_mut().unwrap()[0].percent_used = Some(40.0);
    let low = turn(&mut session);
    check_turn(&low, &["quota_rearmed"], None);
    assert_eq!(quota_events(&low)[0].payload, serde_json::from_value::<serde_json::Map<String,Value>>(json!({"provider":"codex","credential":"work","window":"weekly","percent_used":40.0,"notice_at":80.0})).unwrap());
    check_turn(&turn(&mut session), &[], None);
    cache.lock().unwrap().0[0].windows.as_mut().unwrap()[0].percent_used = Some(83.5);
    check_turn(&turn(&mut session), &["quota_noticed"], Some(LINE));
}

#[test]
fn quota_notice_resume_keeps_fired_and_value_rearm_state() {
    for rearm in [false, true] {
        let mut session = Session::new(script(3), None);
        let cache = reading(83.5, Some(1760000000000));
        wire(&mut session, &cache);
        check_turn(&turn(&mut session), &["quota_noticed"], Some(LINE));
        if rearm {
            cache.lock().unwrap().0[0].windows.as_mut().unwrap()[0].percent_used = Some(40.0);
            check_turn(&turn(&mut session), &["quota_rearmed"], None);
        }
        session.resume(vec![]);
        wire(&mut session, &cache);
        cache.lock().unwrap().0[0].windows.as_mut().unwrap()[0].percent_used = Some(83.5);
        let lines = turn(&mut session);
        check_turn(
            &lines,
            if rearm { &["quota_noticed"] } else { &[] },
            if rearm { Some(LINE) } else { None },
        );
    }
}

#[test]
fn quota_notice_passed_reset_rearms_live_and_resumed_without_an_event() {
    for resume in [false, true] {
        let mut session = Session::advancing(script(2), Duration::from_secs(2));
        let now = contract::clock::wall_ms(session.clock.wall());
        let cache = reading(83.5, Some(now + 1000));
        wire(&mut session, &cache);
        let first_text = format!(
            "Fiber: codex credential work has used 84% of its weekly quota, which resets at {}.",
            contract::clock::utc_time((now + 1000) / 1000)
        );
        check_turn(&turn(&mut session), &["quota_noticed"], Some(&first_text));
        if resume {
            session.resume(vec![]);
            wire(&mut session, &cache);
        }
        cache.lock().unwrap().0[0].windows.as_mut().unwrap()[0].resets_at = Some(now + 3000);
        let next_text = format!(
            "Fiber: codex credential work has used 84% of its weekly quota, which resets at {}.",
            contract::clock::utc_time((now + 3000) / 1000)
        );
        check_turn(&turn(&mut session), &["quota_noticed"], Some(&next_text));
    }
}

#[test]
fn quota_notice_without_a_reader_or_without_a_credit_limit_is_silent() {
    let mut session = Session::new(script(2), None);
    check_turn(&turn(&mut session), &[], None);
    let cache = Arc::new(Mutex::new((
        vec![
            serde_json::from_value(
                json!({"provider":"openrouter","credential":"work","credit":{"remaining":0.0}}),
            )
            .unwrap(),
        ],
        80.0,
    )));
    wire(&mut session, &cache);
    check_turn(&turn(&mut session), &[], None);
    assert_eq!(session.requests().len(), 2);
}

#[test]
fn quota_notice_fold_reads_only_the_sessions_own_log() {
    use contract::events::{Event, SessionStarted, Variables, VariablesSource};
    use contract::{SessionId, shapes::Point};
    let mut session = Session::new(script(2), None);
    let cache = reading(83.5, Some(1760000000000));
    wire(&mut session, &cache);
    let lines = turn(&mut session);
    check_turn(&lines, &["quota_noticed"], Some(LINE));
    let home = session.dir.parent().unwrap().to_path_buf();
    let log = Arc::new(
        log::Log::create(&home, SessionId("s_child".into()), session.clock.clone()).unwrap(),
    );
    log.append(
        &Event::SessionStarted(SessionStarted {
            workspace: session.workspace.display().to_string(),
            variables: Variables {
                path: "/usr/bin:/bin".into(),
                names: vec![],
                source: VariablesSource::Inherited,
            },
            parent: None,
            forked_from: Some(Point {
                session_id: SessionId("s_test".into()),
                seq: lines.last().unwrap().seq.unwrap(),
            }),
            rewind: None,
            worktree: None,
            supervised: false,
        }),
        None,
        None,
    )
    .unwrap();
    let folded = r#loop::resumed(log.dir()).unwrap();
    let (inbox, rx) = mpsc::channel();
    let mut prompt = r#loop::PromptInputs::new(
        home,
        "/bin/sh".into(),
        log.dir().join("events.jsonl").display().to_string(),
        session.clock.clone(),
        fakes::CONTEXT_WINDOW,
    );
    prompt.credential = Some("work".into());
    let child_dir = log.dir().to_path_buf();
    let child = r#loop::Loop::resume(
        r#loop::Session {
            log,
            provider: session.provider.clone(),
            model: r#loop::Model {
                reference: support::MODEL.into(),
                cost: None,
                subscription: false,
            },
            prompt,
            inbox: rx,
            tools: vec![],
            permissions: r#loop::Permissions {
                workspace: session.workspace.display().to_string(),
                credentials: session.credentials.clone(),
                credential_files: vec![],
                rules: session.rules.clone(),
            },
        },
        folded,
    )
    .unwrap();
    session.looped = Some(child);
    session.inbox = inbox;
    wire(&mut session, &cache);
    session.inbox.send(delivery("child")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let child_lines = log::lines(&child_dir)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(quota_events(&child_lines).len(), 1);
    assert_eq!(quota_events(&child_lines)[0].kind, "quota_noticed");
}
