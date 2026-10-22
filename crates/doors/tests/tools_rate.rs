//! The `tools` rate on a resumed session (`docs/tools.md`, "Seeing the
//! tools"): the events the log replayed, then every event appended after.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::io;
use std::sync::Arc;

use contract::clock::Clock;
use contract::events::{
    CacheLifetime, Empty, Event, PreambleBuilt, PreambleReason, SentTool, ToolInfo, ToolSource,
    ToolState, UsageRecorded,
};
use contract::shapes::Tokens;
use contract::{ActionId, GenerationId, SessionId};
use doors::{Session, mint};
use fakes::Client;
use fakes::clock::FakeClock;
use log::Log;
use serde_json::Value;
use support::*;

fn tool() -> ToolInfo {
    ToolInfo {
        name: "read".into(),
        source: ToolSource::Builtin,
        state: ToolState::Full,
        bytes: 12,
        tokens: None,
    }
}

fn preamble(reason: PreambleReason) -> Event {
    Event::PreambleBuilt(PreambleBuilt {
        reason,
        model: "fake/m".into(),
        context_window: 200_000,
        trigger_at: None,
        budget: None,
        thinking: None,
        tool_choice: "auto".into(),
        cache_lifetime: CacheLifetime::FiveMinutes,
        credential: None,
        system_prompt: "system".into(),
        tools: vec![SentTool {
            name: "read".into(),
            registered_by: "builtin".into(),
            deferred: false,
            definition: serde_json::from_value(serde_json::json!({"type": "object"})).unwrap(),
        }],
        replaced: Vec::new(),
    })
}

fn started() -> Event {
    Event::AssistantMessageStarted(Empty {})
}

/// A first own request of `input` uncached tokens and `written` written to
/// the 5-minute cache, over 500 bytes.
fn usage(generation: &str, input: u64, written: u64) -> Event {
    Event::UsageRecorded(UsageRecorded {
        generation_id: GenerationId(generation.into()),
        model: "fake/m".into(),
        tokens: Tokens {
            input,
            cache_read: 0,
            cache_write: [("5m".to_owned(), written)].into_iter().collect(),
            output: 3,
        },
        web_searches: None,
        cost: None,
        subscription: None,
        extension: None,
        origin_session_id: None,
        reviewer: None,
        input_bytes: 500,
        input_media: None,
    })
}

/// The first tool of the `tools` answer to the command `id`.
#[track_caller]
fn first_tool(client: &Client, id: &str) -> Value {
    send(client, &format!(r#"{{"id":"{id}","command":"tools"}}"#));
    let answer = response(client, id);
    assert_eq!(kind(&answer), "command_accepted", "{answer}");
    answer["payload"]["result"]["tools"][0].clone()
}

#[test]
fn tools_after_a_resume_use_the_replayed_rate_then_follow_new_events() {
    let temp = Temp::new();
    let home = temp.0.join("h");
    let sessions = home.join("projects/p/sessions");
    let id = SessionId(mint("s_"));
    let dir = sessions.join(&id.0);
    let clock: Arc<dyn Clock> = FakeClock::new();
    let a1 = Some(ActionId("a_1".into()));
    let created = Log::create(&sessions, id.clone(), Arc::clone(&clock)).unwrap();
    created
        .append(&preamble(PreambleReason::Start), None, None)
        .unwrap();
    created.append(&started(), None, a1.clone()).unwrap();
    created
        .append(&usage("g_1", 10, 90), None, a1.clone())
        .unwrap();
    drop(created);

    let log = Arc::new(Log::open(&sessions, id.clone(), Arc::clone(&clock)).unwrap());
    let session = Session::resume(
        &home,
        &dir,
        &log,
        Arc::clone(&clock),
        vec![tool()],
        Box::new(io::sink()),
    )
    .unwrap();
    let socket = home.join("run").join(&id.0);
    let appended = Arc::clone(&log);
    session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");

            // 12 bytes at 100 tokens over 500 bytes: the replayed fold.
            let replayed = first_tool(&client, "c_t1");
            assert_eq!(replayed["bytes"], 12);
            assert_eq!(replayed["tokens"], 2, "{replayed}");

            appended
                .append(&preamble(PreambleReason::Reload), None, None)
                .unwrap();
            let reset = first_tool(&client, "c_t2");
            assert_eq!(reset["bytes"], 12);
            assert!(reset.get("tokens").is_none(), "{reset}");

            let a2 = Some(ActionId("a_2".into()));
            appended.append(&started(), None, a2.clone()).unwrap();
            appended
                .append(&usage("g_2", 10, 240), None, a2.clone())
                .unwrap();
            // 12 bytes at 250 tokens over 500 bytes.
            let followed = first_tool(&client, "c_t3");
            assert_eq!(followed["bytes"], 12);
            assert_eq!(followed["tokens"], 6, "{followed}");
            Ok(())
        })
        .unwrap();
    close_within(session, log);
}
