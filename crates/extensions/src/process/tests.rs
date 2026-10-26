//! Private process tests: codec, request table, transport cancellation
//! and lifecycle boundaries (`super`). Public package behavior rides
//! `process_tests`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code; a failure is the test's"
)]

use std::sync::{Arc, mpsc};
use std::time::Duration;

use serde_json::{Map, json};

use contract::clock::Clock as _;
use contract::tool::Cancel as _;

use super::calls::{self, Route};
use super::requests::{self, Answer, Outcome, RequestKind, Settled, Table};
use super::transport::{
    self, CloseReason, Incoming, Limits, ParsedLine, ReplyAnswer, SendError, Transport,
};
use crate::host::admission::{self, CallbackContext, CallbackKind};

/// The bound a positive wait keeps: fast on success, failing instead of
/// hanging.
const WITHIN: Duration = Duration::from_secs(10);

/// The fake clock one wait parks on: frozen unless a test advances it.
fn clock() -> Arc<fakes::clock::FakeClock> {
    fakes::clock::FakeClock::new()
}

/// What a minted request's cell holds: the settled outcome, if any.
fn outcome_of(minted: &requests::Minted) -> Option<Outcome> {
    support::lock(&minted.wait.mutex).clone()
}

/// A process-context scratch dir, kept alive for the transport's `cwd`.
fn scratch() -> fakes::TempDir {
    fakes::TempDir::new("fiber-process-tests")
}

/// Spawns `sh -c script` in `dir` with `limits`, shared: reads run on
/// workers holding their own share.
fn spawn_shell(dir: &std::path::Path, script: &str, limits: Limits) -> Arc<Transport> {
    Arc::new(
        Transport::spawn_with(
            std::path::Path::new("sh"),
            &["-c".to_owned(), script.to_owned()],
            dir,
            limits,
        )
        .unwrap(),
    )
}

/// Stops the pipes, kills the transport's child and reaps it. The caller
/// joins the threads: the dead child fails a blocked write, and the stop
/// ends the reader and the drain, so every thread ends.
fn kill_and_join(transport: &Transport) {
    transport.stop();
    match transport.signal(rustix::process::Signal::KILL) {
        Ok(()) | Err(_) => {}
    }
    match transport.wait_exit() {
        Ok(_) | Err(_) => {}
    }
}

/// Joins the transport's threads on a worker, so a wedged thread fails
/// the test at the bound instead of hanging it.
fn join_within(transport: Arc<Transport>, what: &'static str) {
    fakes::within(what, fakes::Deadline::after(WITHIN), move || {
        transport.join();
    });
}

/// The next arrival, or panics naming `what` when nothing arrives within
/// the bound. Reads on a worker, so the bound fails the test instead of
/// hanging it.
fn next_incoming(transport: &Arc<Transport>, what: &str) -> Incoming {
    let (done_tx, done_rx) = mpsc::channel();
    let owned = Arc::clone(transport);
    std::thread::spawn(move || match done_tx.send(owned.recv()) {
        Ok(()) | Err(_) => {}
    });
    fakes::Deadline::after(WITHIN)
        .recv(&done_rx)
        .unwrap_or_else(|_| panic!("waited {WITHIN:?} for {what}"))
}

/// The first line, or panics naming `what` when nothing arrives within
/// the bound.
fn next_line(transport: &Arc<Transport>, what: &str) -> Vec<u8> {
    match next_incoming(transport, what) {
        Incoming::Line(line) => line,
        other @ Incoming::Closed(_) => panic!("{what}: no line arrived, saw {other:?}"),
    }
}

/// The close, or panics when the pipe stays open past the bound.
fn next_close(transport: &Arc<Transport>, what: &str) -> CloseReason {
    match next_incoming(transport, &format!("the {what} close")) {
        Incoming::Closed(reason) => reason,
        other @ Incoming::Line(_) => panic!("{what}: the pipe stayed open, saw {other:?}"),
    }
}

// The supervised codec: one row per shape the pipe carries.

#[test]
fn call_with_request_id_parses() {
    let line = br#"{"id":"c1","command":"call","args":{"call":"fiber.tool","args":["t",{}],"request_id":"r_1"}}"#;
    let ParsedLine::Call {
        id,
        call,
        args,
        request_id,
    } = transport::parse_line(line)
    else {
        panic!("a supervised call must parse");
    };
    assert_eq!(id.0, "c1");
    assert_eq!(call, "fiber.tool");
    assert_eq!(args, vec![json!("t"), json!({})]);
    assert_eq!(request_id, Some(contract::RequestId("r_1".into())));
}

#[test]
fn call_without_request_id_has_none() {
    let line = br#"{"id":"c1","command":"call","args":{"call":"state.get","args":["k"]}}"#;
    let ParsedLine::Call { request_id, .. } = transport::parse_line(line) else {
        panic!("a call must parse");
    };
    assert_eq!(request_id, None);
}

#[test]
fn ready_parses() {
    assert_eq!(
        transport::parse_line(br#"{"id":"r1","command":"ready"}"#),
        ParsedLine::Ready {
            id: contract::CommandId("r1".into())
        }
    );
}

#[test]
fn reply_value_and_failure_parse() {
    let value = transport::parse_line(
        br#"{"id":"p1","command":"reply","args":{"request_id":"r_1","result":42}}"#,
    );
    assert_eq!(
        value,
        ParsedLine::Reply {
            id: contract::CommandId("p1".into()),
            request_id: contract::RequestId("r_1".into()),
            answer: ReplyAnswer::Value(json!(42)),
        }
    );
    let failure = transport::parse_line(
        br#"{"id":"p2","command":"reply","args":{"request_id":"r_2","error":{"code":"tool_error","message":"no"}}}"#,
    );
    assert_eq!(
        failure,
        ParsedLine::Reply {
            id: contract::CommandId("p2".into()),
            request_id: contract::RequestId("r_2".into()),
            answer: ReplyAnswer::Failure {
                code: contract::ErrorCode::ToolError,
                message: "no".into(),
            }
        }
    );
}

#[test]
fn reply_with_an_interaction_answer_is_a_bad_reply() {
    // Only what the Lua function returns, or its error, answers a
    // request: an approval-shaped answer refuses `invalid_arguments`.
    let parsed = transport::parse_line(
        br#"{"id":"p1","command":"reply","args":{"request_id":"r_1","confirmed":true}}"#,
    );
    assert_eq!(
        parsed,
        ParsedLine::BadReply {
            id: contract::CommandId("p1".into())
        }
    );
}

#[test]
fn ordinary_driver_command_forwards() {
    let parsed = transport::parse_line(br#"{"id":"d1","command":"tools","args":{}}"#);
    assert_eq!(
        parsed,
        ParsedLine::Forward {
            id: contract::CommandId("d1".into()),
            command: "tools".into(),
            args: Map::new(),
        }
    );
}

#[test]
fn unknown_name_without_args_is_unknown() {
    let parsed = transport::parse_line(br#"{"id":"u1","command":"frob"}"#);
    assert_eq!(
        parsed,
        ParsedLine::Unknown {
            id: contract::CommandId("u1".into()),
            command: "frob".into(),
        }
    );
}

#[test]
fn unknown_name_with_args_is_unknown() {
    // The name alone decides: with args the full parse fails, and the
    // driver re-reads the name to answer `unknown_command`.
    let parsed = transport::parse_line(br#"{"id":"u1","command":"frob","args":{"x":1}}"#);
    assert_eq!(
        parsed,
        ParsedLine::Unknown {
            id: contract::CommandId("u1".into()),
            command: "frob".into(),
        }
    );
}

#[test]
fn built_command_with_unfit_args_is_unfit() {
    // `call` is built and its `args` is an object, but the call's shape
    // does not parse: the name is known, so the answer is
    // `invalid_arguments`, not `unknown_command`.
    let parsed = transport::parse_line(br#"{"id":"c1","command":"call","args":{"call":42}}"#);
    assert_eq!(
        parsed,
        ParsedLine::Unfit {
            id: contract::CommandId("c1".into())
        }
    );
}

#[test]
fn garbage_is_ignored() {
    assert_eq!(transport::parse_line(b"not json"), ParsedLine::Ignored);
    assert_eq!(transport::parse_line(b"[1,2]"), ParsedLine::Ignored);
    assert_eq!(transport::parse_line(b"null"), ParsedLine::Ignored);
    assert_eq!(transport::parse_line(&[0xff, 0xfe]), ParsedLine::Ignored);
}

#[test]
fn json_without_an_id_is_ignored() {
    assert_eq!(
        transport::parse_line(br#"{"command":"ready"}"#),
        ParsedLine::Ignored
    );
    assert_eq!(
        transport::parse_line(br#"{"id":42,"command":"ready"}"#),
        ParsedLine::Ignored
    );
}

#[test]
fn extra_keys_are_malformed() {
    let parsed = transport::parse_line(br#"{"id":"c1","command":"ready","args":{},"extra":true}"#);
    assert_eq!(
        parsed,
        ParsedLine::Malformed {
            id: contract::CommandId("c1".into())
        }
    );
}

#[test]
fn non_object_args_are_malformed() {
    let parsed = transport::parse_line(br#"{"id":"c1","command":"ready","args":[1]}"#);
    assert_eq!(
        parsed,
        ParsedLine::Malformed {
            id: contract::CommandId("c1".into())
        }
    );
}

#[test]
fn null_args_are_malformed() {
    // An optional key is absent, never null: explicit `null` reads as no
    // command, answered `malformed` while the id names the answer.
    let parsed = transport::parse_line(br#"{"id":"c1","command":"ready","args":null}"#);
    assert_eq!(
        parsed,
        ParsedLine::Malformed {
            id: contract::CommandId("c1".into())
        }
    );
}

// The shared admission context and helpers.

#[test]
fn entry_registers_while_timer_and_request_do_not() {
    assert!(CallbackContext::entry("e").registration_allowed());
    assert!(!CallbackContext::timer("e").registration_allowed());
    let request = CallbackContext::request(
        "e",
        contract::RequestId("r_1".into()),
        "tool t".into(),
        None,
        support::cancel::Token::new(),
        None,
    );
    assert!(!request.registration_allowed());
    assert_eq!(
        request.request_id(),
        Some(&contract::RequestId("r_1".into()))
    );
    assert_eq!(request.ran_by(), Some("tool t"));
    assert_eq!(CallbackContext::timer("e").request_id(), None);
    assert_eq!(CallbackContext::entry("e").ran_by(), None);
}

#[test]
fn request_kind_matches_constructor() {
    let timer = CallbackContext::timer("e");
    assert!(matches!(timer.kind, CallbackKind::Timer));
    let entry = CallbackContext::entry("e");
    assert!(matches!(entry.kind, CallbackKind::Entry));
}

#[test]
fn decode_bytes_round_trips_and_names_failures() {
    assert_eq!(
        admission::decode_bytes("host.exec", "input", "aGk="),
        Ok(b"hi".to_vec())
    );
    assert_eq!(
        admission::decode_bytes("host.exec", "input", ""),
        Ok(vec![])
    );
    assert_eq!(
        admission::decode_bytes("host.exec", "input", "!!!"),
        Err((
            contract::ErrorCode::InvalidArguments,
            "host.exec: `input` is not valid base64".into(),
        ))
    );
}

#[test]
fn fits_framing_holds_at_the_edge() {
    assert!(admission::fits_framing(8, 8));
    assert!(admission::fits_framing(7, 8));
    assert!(!admission::fits_framing(9, 8));
}

// Call routing and handler keys.

#[test]
fn fiber_calls_register() {
    let route = calls::route_call("fiber.tool", vec![json!("t")]).unwrap();
    assert_eq!(
        route,
        Route::Register {
            call: "fiber.tool".into(),
            args: vec![json!("t")],
        }
    );
}

#[test]
fn drive_routes_with_its_arguments() {
    let route =
        calls::route_call("host.drive", vec![json!("steer"), json!({"text": "t"})]).unwrap();
    assert_eq!(
        route,
        Route::Drive {
            command: "steer".into(),
            args: [("text".to_owned(), json!("t"))].into_iter().collect(),
        }
    );
}

#[test]
fn drive_without_args_reads_empty() {
    let route = calls::route_call("host.drive", vec![json!("tools")]).unwrap();
    assert_eq!(
        route,
        Route::Drive {
            command: "tools".into(),
            args: Map::new(),
        }
    );
}

#[test]
fn drive_with_an_empty_array_reads_empty() {
    let route = calls::route_call("host.drive", vec![json!("tools"), json!([])]).unwrap();
    assert_eq!(
        route,
        Route::Drive {
            command: "tools".into(),
            args: Map::new(),
        }
    );
}

#[test]
fn drive_names_a_non_string_command() {
    assert_eq!(
        calls::route_call("host.drive", vec![json!(42)]),
        Err((
            contract::ErrorCode::InvalidArguments,
            "host.drive: `command` must be a string".into(),
        ))
    );
}

#[test]
fn drive_names_a_non_table_args() {
    assert_eq!(
        calls::route_call("host.drive", vec![json!("tools"), json!([1])]),
        Err((
            contract::ErrorCode::InvalidArguments,
            "host.drive: `args` must be a table".into(),
        ))
    );
}

#[test]
fn host_and_state_calls_dispatch() {
    let route = calls::route_call("host.http", vec![json!({})]).unwrap();
    assert_eq!(
        route,
        Route::Host {
            call: "host.http".into(),
            args: vec![json!({})],
        }
    );
    let route = calls::route_call("state.get", vec![json!("k")]).unwrap();
    assert_eq!(
        route,
        Route::Host {
            call: "state.get".into(),
            args: vec![json!("k")],
        }
    );
}

#[test]
fn handler_keys_cover_every_family() {
    assert_eq!(
        calls::handler_keys("fiber.tool", &[json!("t")]),
        vec!["run"]
    );
    assert_eq!(
        calls::handler_keys("fiber.tool", &[json!("t"), json!({"run": true})]),
        vec!["run"]
    );
    // A table `effects` runs no function; a `true` one does.
    assert_eq!(
        calls::handler_keys(
            "fiber.tool",
            &[json!("t"), json!({"run": true, "effects": {"effects": []}})]
        ),
        vec!["run"]
    );
    assert_eq!(
        calls::handler_keys(
            "fiber.tool",
            &[json!("t"), json!({"run": true, "effects": true})]
        ),
        vec!["run", "effects"]
    );
    for call in [
        "fiber.hook",
        "fiber.watch",
        "fiber.command",
        "fiber.search_backend",
    ] {
        assert_eq!(
            calls::handler_keys(call, &[json!("x")]),
            vec!["run"],
            "{call}"
        );
    }
    assert_eq!(
        calls::handler_keys(
            "fiber.provider",
            &[json!("p"), json!({"models": {"timeout": 1, "run": true}})]
        ),
        vec!["models"]
    );
    assert_eq!(
        calls::handler_keys(
            "fiber.harness",
            &[json!("h"), json!({"auto": {"timeout": 1, "run": true}})]
        ),
        vec!["auto"]
    );
    // A function value beside `run` enables nothing extra.
    assert_eq!(
        calls::handler_keys(
            "fiber.provider",
            &[json!("p"), json!({"models": {"timeout": 1}})]
        ),
        Vec::<String>::new()
    );
    assert_eq!(calls::handler_keys("host.http", &[]), Vec::<String>::new());
}

// The request table: correlation, one completion and generations.

/// Mints one stream request, returning the table with it.
fn minted(table: &Table, ran_by: &str) -> requests::Minted {
    table.mint(RequestKind::Stream, "e", ran_by.to_owned(), None, None)
}

#[test]
fn minted_ids_are_unique_within_a_generation() {
    let table = Table::new();
    let mut ids = std::collections::BTreeSet::new();
    for _ in 0..100 {
        let minted = minted(&table, "tool t");
        assert!(ids.insert(minted.id), "a minted id repeats");
    }
}

#[test]
fn settle_delivers_once_then_reads_stale() {
    let table = Table::new();
    let minted = minted(&table, "tool t");
    assert!(table.is_live(&minted.id));
    assert_eq!(table.kind(&minted.id), Some(RequestKind::Stream));
    assert_eq!(
        table
            .context(&minted.id)
            .as_ref()
            .and_then(|ctx| ctx.ran_by()),
        Some("tool t")
    );
    assert_eq!(
        table.settle(&minted.id, Outcome::Answered(Answer::Value(json!(1)))),
        Settled::Completed
    );
    assert_eq!(
        outcome_of(&minted),
        Some(Outcome::Answered(Answer::Value(json!(1))))
    );
    assert!(!table.is_live(&minted.id));
    assert_eq!(table.kind(&minted.id), None);
    assert!(table.context(&minted.id).is_none());
    assert_eq!(
        table.settle(&minted.id, Outcome::Answered(Answer::Value(json!(2)))),
        Settled::Stale
    );
}

#[test]
fn settling_an_unknown_id_is_stale() {
    let table = Table::new();
    assert_eq!(
        table.settle(
            &contract::RequestId("r_dead".into()),
            Outcome::Answered(Answer::Value(json!(null)))
        ),
        Settled::Stale
    );
}

#[test]
fn cancel_fires_the_signal_and_completes_cancelled() {
    let table = Table::new();
    let minted = minted(&table, "tool t");
    assert_eq!(table.cancel(&minted.id), Settled::Completed);
    assert!(minted.cancel.is_cancelled());
    assert_eq!(outcome_of(&minted), Some(Outcome::Cancelled));
    assert_eq!(table.cancel(&minted.id), Settled::Stale);
}

#[test]
fn timeout_does_not_fire_the_cancel_signal() {
    // A missed hook timeout stops nothing: the hook has failed, and the
    // hook's own machinery applies `on_failure`.
    let table = Table::new();
    let minted = minted(&table, "hook x");
    assert_eq!(table.timeout(&minted.id), Settled::Completed);
    assert!(!minted.cancel.is_cancelled());
    assert_eq!(outcome_of(&minted), Some(Outcome::TimedOut));
    assert_eq!(table.settle(&minted.id, Outcome::Cancelled), Settled::Stale);
}

#[test]
fn retire_cancels_everything_and_bumps_the_generation() {
    let table = Table::new();
    let first = minted(&table, "tool a");
    let second = table.mint(RequestKind::Tool, "e", "tool b".into(), None, None);
    let before = table.generation();
    let after = table.retire();
    assert_eq!(after, before + 1, "retirement bumps the generation once");
    assert!(first.cancel.is_cancelled());
    assert!(second.cancel.is_cancelled());
    assert_eq!(outcome_of(&first), Some(Outcome::Cancelled));
    assert_eq!(outcome_of(&second), Some(Outcome::Cancelled));
    assert!(!table.is_live(&first.id));
    assert_eq!(table.settle(&first.id, Outcome::Cancelled), Settled::Stale);
    // No id resolves across generations: the next mint stands alone.
    let third = minted(&table, "tool c");
    assert_ne!(third.id, first.id);
    assert_ne!(third.id, second.id);
    assert!(table.is_live(&third.id));
}

#[test]
fn retire_with_nothing_pending_still_bumps() {
    let table = Table::new();
    let before = table.generation();
    assert_eq!(table.retire(), before + 1);
}

#[test]
fn stream_items_wait_for_the_last_reply() {
    let table = Arc::new(Table::new());
    let held = minted(&table, "hook a");
    assert!(table.acquire_stream(&held.id, table.generation()));
    // The next stream item blocks while the held one waits.
    let waiter = Arc::clone(&table);
    let (done_tx, done_rx) = mpsc::channel();
    let next = table.mint(RequestKind::Stream, "e", "hook b".into(), None, None);
    std::thread::spawn(move || {
        let acquired = waiter.acquire_stream(&next.id, waiter.generation());
        match done_tx.send((acquired, next.id.clone())) {
            Ok(()) | Err(_) => {}
        }
        if acquired {
            waiter.cancel(&next.id);
        }
    });
    // Tools never take the gate: one mints and settles while the stream waits.
    let tool = table.mint(RequestKind::Tool, "e", "tool t".into(), None, None);
    assert_eq!(
        table.settle(&tool.id, Outcome::Answered(Answer::Value(json!(true)))),
        Settled::Completed
    );
    assert!(
        fakes::Deadline::after(Duration::from_millis(200))
            .recv(&done_rx)
            .is_err()
    );
    // The held reply releases the waiter.
    assert_eq!(
        table.settle(&held.id, Outcome::Answered(Answer::Value(json!(null)))),
        Settled::Completed
    );
    let (acquired, next_id) = fakes::Deadline::after(WITHIN)
        .recv(&done_rx)
        .expect("the waiter acquires once the stream releases");
    assert!(acquired);
    table.cancel(&next_id);
}

#[test]
fn retirement_releases_a_waiting_stream_item_stale() {
    let table = Arc::new(Table::new());
    let held = minted(&table, "hook a");
    assert!(table.acquire_stream(&held.id, table.generation()));
    let waiter = Arc::clone(&table);
    let (done_tx, done_rx) = mpsc::channel();
    let next = table.mint(RequestKind::Stream, "e", "hook b".into(), None, None);
    let next_id = next.id.clone();
    std::thread::spawn(move || {
        let acquired = waiter.acquire_stream(&next.id, 0);
        match done_tx.send(acquired) {
            Ok(()) | Err(_) => {}
        }
    });
    table.retire();
    assert_eq!(
        fakes::Deadline::after(WITHIN).recv(&done_rx),
        Ok(false),
        "a retired waiter ends stale instead of hanging"
    );
    table.cancel(&next_id);
}

#[test]
fn wait_for_reads_the_answer_deadline_cancel_and_guard() {
    let clock = clock();
    let table = Table::new();
    let answered = minted(&table, "tool t");
    table.settle(
        &answered.id,
        Outcome::Answered(Answer::Value(json!("done"))),
    );
    assert_eq!(
        Table::wait_for(
            &answered.wait,
            None,
            &support::cancel::Token::new(),
            &*clock,
            WITHIN
        ),
        Some(Outcome::Answered(Answer::Value(json!("done"))))
    );
    // A past deadline ends the wait without settling the table.
    let late = minted(&table, "hook x");
    let past = Some(
        clock
            .now()
            .checked_sub(Duration::from_secs(1))
            .expect("the fake clock starts after the suite's first second"),
    );
    assert_eq!(
        Table::wait_for(
            &late.wait,
            past,
            &support::cancel::Token::new(),
            &*clock,
            WITHIN
        ),
        Some(Outcome::TimedOut)
    );
    assert!(table.is_live(&late.id), "the wait settles nothing itself");
    table.cancel(&late.id);
    // A fired cancel ends the wait cancelled.
    let stopped = minted(&table, "tool s");
    stopped.cancel.cancel();
    assert_eq!(
        Table::wait_for(&stopped.wait, None, &stopped.cancel, &*clock, WITHIN),
        Some(Outcome::Cancelled)
    );
    table.cancel(&stopped.id);
    // A zero hang reads `None` at once, and the test fails instead of
    // hanging when nothing answers.
    let hung = minted(&table, "tool h");
    assert_eq!(
        Table::wait_for(
            &hung.wait,
            None,
            &support::cancel::Token::new(),
            &*clock,
            Duration::ZERO
        ),
        None
    );
    table.cancel(&hung.id);
}

#[test]
fn a_settle_wakes_a_parked_wait() {
    // The waiter parks on a frozen clock, so only the settle's wake can
    // end it: the answer arrives without any clock advance.
    let clock = clock();
    let table = Table::new();
    let minted = table.mint(RequestKind::Tool, "e", "tool t".into(), None, None);
    let waiter = minted.wait.clone();
    let parked = clock.clone();
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let outcome = Table::wait_for(
            &waiter,
            None,
            &support::cancel::Token::new(),
            &*parked,
            Duration::from_secs(60),
        );
        match done_tx.send(outcome) {
            Ok(()) | Err(_) => {}
        }
    });
    table.settle(&minted.id, Outcome::TimedOut);
    assert_eq!(
        fakes::Deadline::after(WITHIN).recv(&done_rx),
        Ok(Some(Outcome::TimedOut)),
        "the settle's wake ends the parked wait"
    );
}

#[test]
fn wait_for_on_a_dropped_table_keeps_nothing() {
    // The table is gone, so nothing can settle the cell: a zero hang
    // reads `None` instead of hanging on the orphaned wait.
    let wait = {
        let table = Table::new();
        minted(&table, "tool t").wait
    };
    let clock = clock();
    assert_eq!(
        Table::wait_for(
            &wait,
            None,
            &support::cancel::Token::new(),
            &*clock,
            Duration::ZERO
        ),
        None
    );
}

#[test]
fn end_race_lets_an_arrived_reply_win() {
    // A reply that arrived first still wins, even when the local clock
    // just passed the deadline: the settler lost, so the channel's
    // answer stands.
    let table = Table::new();
    let minted = minted(&table, "hook x");
    table.settle(
        &minted.id,
        Outcome::Answered(Answer::Failure {
            code: contract::ErrorCode::ToolError,
            message: "no".into(),
        }),
    );
    let outcome = table.end_race(&minted.id, &minted.wait, Outcome::TimedOut);
    assert_eq!(
        outcome,
        Outcome::Answered(Answer::Failure {
            code: contract::ErrorCode::ToolError,
            message: "no".into(),
        })
    );
}

#[test]
fn end_race_returns_the_hang_outcome_when_nothing_arrived() {
    // Nothing settled the cell before the hang's own outcome: the
    // hang's settle wins, ending the request timed out, and a late
    // reply reads stale.
    let table = Table::new();
    let minted = minted(&table, "hook x");
    assert_eq!(
        table.end_race(&minted.id, &minted.wait, Outcome::TimedOut),
        Outcome::TimedOut
    );
    assert!(!table.is_live(&minted.id));
    assert_eq!(
        outcome_of(&minted),
        Some(Outcome::TimedOut),
        "the hang's outcome is what the waiter reads"
    );
    assert_eq!(table.settle(&minted.id, Outcome::Cancelled), Settled::Stale);
}

#[test]
fn watch_caller_fires_at_once_when_already_cancelled() {
    let table = Arc::new(Table::new());
    let minted = minted(&table, "hook x");
    let caller = support::cancel::Token::new();
    caller.cancel();
    table.watch_caller(&caller, &minted.id);
    assert_eq!(outcome_of(&minted), Some(Outcome::Cancelled));
}

#[test]
fn watch_caller_fires_when_the_caller_cancels() {
    let table = Arc::new(Table::new());
    let minted = minted(&table, "hook x");
    let caller = support::cancel::Token::new();
    table.watch_caller(&caller, &minted.id);
    caller.cancel();
    assert_eq!(outcome_of(&minted), Some(Outcome::Cancelled));
    assert!(minted.cancel.is_cancelled());
}

// The pipes: framed I/O, cancellation and process groups.

#[test]
fn default_limits_share_the_wire_cap() {
    assert_eq!(transport::default_max_line(), support::framing::MAX_LINE);
    assert_eq!(Limits::default().max_line, support::framing::MAX_LINE);
}

#[test]
fn lines_round_trip_through_cat() {
    let dir = scratch();
    let transport = spawn_shell(dir.path(), "exec cat", Limits::default());
    transport.send(br#"{"id":"c1"}"#.to_vec()).unwrap();
    assert_eq!(next_line(&transport, "the echo"), br#"{"id":"c1"}"#);
    kill_and_join(&transport);
    join_within(transport, "the echo joins");
}

#[test]
fn multiple_lines_and_a_partial_tail_arrive() {
    // Two complete lines in one read, then a tail with no line feed:
    // the partial posts before the close.
    let dir = scratch();
    let transport = spawn_shell(dir.path(), "printf 'a\\nb\\npartial'", Limits::default());
    assert_eq!(next_line(&transport, "first"), b"a");
    assert_eq!(next_line(&transport, "second"), b"b");
    assert_eq!(next_line(&transport, "partial tail"), b"partial");
    assert_eq!(next_close(&transport, "exit"), CloseReason::Eof);
    join_within(transport, "the exited pipes join");
}

#[test]
fn crlf_reads_as_its_text() {
    let dir = scratch();
    let transport = spawn_shell(dir.path(), "printf 'x\\r\\n'", Limits::default());
    assert_eq!(next_line(&transport, "crlf"), b"x");
    assert_eq!(next_close(&transport, "exit"), CloseReason::Eof);
    join_within(transport, "the exited pipes join");
}

#[test]
fn a_quick_exit_closes_with_eof() {
    let dir = scratch();
    let transport = spawn_shell(dir.path(), "exit 0", Limits::default());
    assert_eq!(next_close(&transport, "quick exit"), CloseReason::Eof);
    // The child is gone, but the writer still parks on its queue: the
    // stop ends it, and every thread joins.
    transport.stop();
    join_within(transport, "the quick exit joins");
}

#[test]
fn a_line_at_the_cap_arrives_while_one_past_closes() {
    let limits = Limits {
        max_line: 8,
        max_queued: 1024,
        writer_queue: 8,
    };
    let dir = scratch();
    let ok = spawn_shell(dir.path(), "printf '12345678\\n'", limits);
    assert_eq!(next_line(&ok, "at the cap"), b"12345678");
    assert_eq!(next_close(&ok, "exit after"), CloseReason::Eof);
    join_within(ok, "the capped pipe joins");
    // One byte past the cap closes the pipe: the extension counts as dead.
    let over = spawn_shell(dir.path(), "printf '123456789\\n'", limits);
    assert_eq!(next_close(&over, "over the cap"), CloseReason::TooLong);
    over.stop();
    join_within(over, "the over-cap pipes join");
}

#[test]
fn send_refuses_a_line_past_the_cap_without_queueing() {
    let limits = Limits {
        max_line: 8,
        max_queued: 1024,
        writer_queue: 8,
    };
    let dir = scratch();
    let transport = spawn_shell(dir.path(), "exec cat", limits);
    assert_eq!(transport.send(vec![b'x'; 9]), Err(SendError::TooLarge));
    // Nothing queued: the next line still round-trips.
    transport.send(b"ok".to_vec()).unwrap();
    assert_eq!(next_line(&transport, "after refusal"), b"ok");
    kill_and_join(&transport);
    join_within(transport, "the refused pipes join");
}

#[test]
fn a_full_writer_queue_reads_gone() {
    // The child never reads: the pipe buffer fills, the writer parks,
    // and the queue fills behind it. Every send is a `try_send`, so the
    // caller never blocks: the pool keeps the bound.
    let limits = Limits {
        max_line: 4096,
        max_queued: 1 << 20,
        writer_queue: 1,
    };
    let dir = scratch();
    let transport = spawn_shell(dir.path(), "sleep 30", limits);
    let mut gone = false;
    for _ in 0..4000 {
        match transport.send(vec![b'x'; 1024]) {
            Ok(()) => {}
            Err(SendError::Gone) => {
                gone = true;
                break;
            }
            Err(SendError::TooLarge) => panic!("a 1 KiB line fits"),
        }
    }
    assert!(gone, "the queue fills while the child never reads");
    kill_and_join(&transport);
    // The dead child fails the parked write: every thread ends promptly.
    join_within(transport, "the unwedged pipes join");
}

#[test]
fn stop_wakes_a_blocked_reader_without_eof() {
    // No descendant closes the pipe and none ever will: stopping still
    // ends the read at once, instead of waiting for an escaped holder.
    let dir = scratch();
    let transport = spawn_shell(dir.path(), "sleep 30", Limits::default());
    let stopped = Arc::clone(&transport);
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || match done_tx.send(stopped.recv()) {
        Ok(()) | Err(_) => {}
    });
    transport.stop();
    assert!(
        matches!(
            fakes::Deadline::after(WITHIN).recv(&done_rx),
            Ok(Incoming::Closed(CloseReason::Stopped))
        ),
        "the stop ends the blocked read"
    );
    kill_and_join(&transport);
    join_within(transport, "the stopped pipes join");
}

#[test]
fn full_stderr_drains_and_keeps_its_tail() {
    // Two megabytes on stderr with stdout closed: the drain keeps the
    // child moving, and keeps the tail naming the failure.
    let dir = scratch();
    let transport = spawn_shell(
        dir.path(),
        "head -c 2000000 /dev/zero >&2",
        Limits::default(),
    );
    assert_eq!(next_close(&transport, "dump exit"), CloseReason::Eof);
    join_within(Arc::clone(&transport), "the dump drain joins");
    let tail = transport.take_stderr();
    assert_eq!(tail.len(), transport::STDERR_TAIL);
    assert!(tail.iter().all(|byte| *byte == 0));
}

#[test]
fn a_term_reaches_the_child_and_everything_it_started() {
    // The child starts a grandchild and waits: one TERM to the group
    // ends both, proving the stop reaches past the leader.
    let dir = scratch();
    let transport = spawn_shell(dir.path(), "sleep 60 & wait", Limits::default());
    let pgid = transport.pgid();
    assert!(pgid > 1, "a listed group is never a refused id");
    assert!(transport.group_alive());
    transport
        .signal(rustix::process::Signal::TERM)
        .expect("TERM reaches the group");
    // Reap the leader first: its zombie alone keeps the group id alive,
    // and the wait builtin ends once its grandchild dies.
    let status = transport.wait_exit().expect("the child reaps");
    assert!(!status.success(), "a TERM kill exits uncleanly");
    assert!(
        fakes::group_empties(transport.pgid(), fakes::Deadline::after(WITHIN)),
        "the group empties after TERM, leader and grandchild"
    );
    join_within(transport, "the terminated pipes join");
}
