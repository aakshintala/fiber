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

/// One `ran_by` row of the shared admission table: the anchor and the
/// three permission bits.
fn admission_row(ran_by: Option<&str>) -> (Option<admission::ToolAnchor>, bool, bool, bool) {
    (
        admission::tool_anchor(ran_by),
        admission::model_permitted(ran_by),
        admission::delegate_permitted(ran_by),
        admission::exec_logged(ran_by),
    )
}

#[test]
fn admission_shares_the_attribution_table() {
    use admission::ToolAnchor::{Command, Tool};
    // A tool request anchors an inner call and skips the exec line.
    assert_eq!(
        admission_row(Some("tool t")),
        (Some(Tool("t".into())), true, true, false)
    );
    // A command request anchors an inner call but still logs exec.
    assert_eq!(
        admission_row(Some("command c")),
        (Some(Command("c".into())), true, true, true)
    );
    // The effects function is not the run: it anchors nothing and loses
    // the model and the delegate. One letter short of " effects" keeps
    // every permission, so the suffix match reads the whole word.
    assert_eq!(
        admission_row(Some("tool t effects")),
        (None, false, false, true)
    );
    assert_eq!(
        admission_row(Some("tool t effect")),
        (None, true, true, true)
    );
    // A stream hook runs everything but an inner tool.
    assert_eq!(admission_row(Some("hook x")), (None, true, true, true));
    // A watcher loses the delegate but keeps the model.
    assert_eq!(admission_row(Some("watcher 0")), (None, true, false, true));
    // A provider loses the model, the delegate and the anchor.
    assert_eq!(
        admission_row(Some("provider p.f")),
        (None, false, false, true)
    );
    // A search backend loses the model, the delegate and the anchor.
    assert_eq!(admission_row(Some("search s")), (None, false, false, true));
    // A harness function keeps what a delegate run keeps.
    assert_eq!(admission_row(Some("harness h.k")), (None, true, true, true));
    // A timer anchors nothing but may use every service.
    assert_eq!(admission_row(None), (None, true, true, true));
}

#[test]
fn entry_refusals_name_the_lua_sentence() {
    for (call, code, message) in [
        (
            "host.tool",
            contract::ErrorCode::InvalidArguments,
            "host.tool: not available while init.lua runs",
        ),
        (
            "host.exec",
            contract::ErrorCode::InvalidArguments,
            "host.exec: not available while init.lua runs",
        ),
        (
            "host.model",
            contract::ErrorCode::Closing,
            "host.model: not available while init.lua runs",
        ),
        (
            "host.delegate",
            contract::ErrorCode::Closing,
            "host.delegate: not available while init.lua runs",
        ),
        (
            "host.drive",
            contract::ErrorCode::Closing,
            "host.drive: not available while init.lua runs",
        ),
        (
            "host.ask",
            contract::ErrorCode::InvalidArguments,
            "host.ask: not available while init.lua runs",
        ),
        (
            "host.oauth.callback",
            contract::ErrorCode::InvalidArguments,
            "host.oauth.callback: needs a running callback, not the entry script",
        ),
        (
            "host.oauth.poll",
            contract::ErrorCode::InvalidArguments,
            "host.oauth.poll: needs a running callback, not the entry script",
        ),
        (
            "host.oauth.refresh",
            contract::ErrorCode::InvalidArguments,
            "host.oauth.refresh: needs a running callback, not the entry script",
        ),
    ] {
        assert_eq!(
            admission::entry_refusal(call),
            Some((code, message.to_owned()))
        );
    }
    for call in [
        "host.http",
        "host.fs.read",
        "host.config.get",
        "host.secret",
        "host.data_dir",
        "host.sha256",
        "host.status",
        "host.log",
        "state.get",
        "host.oauth.open",
        "host.oauth.show",
        "host.oauth.pkcs",
    ] {
        assert_eq!(
            admission::entry_refusal(call),
            None,
            "{call} runs in the entry script"
        );
    }
}

#[test]
fn lock_failures_name_the_callback() {
    let call = "host.oauth.refresh";
    assert_eq!(
        admission::lock_arg_error(call, None),
        "host.oauth.refresh: a timer has no provider credential to refresh"
    );
    assert_eq!(
        admission::lock_arg_error(call, Some("tool t")),
        "host.oauth.refresh: a tool has no provider credential to refresh"
    );
    assert_eq!(
        admission::lock_arg_error(call, Some("command c")),
        "host.oauth.refresh: a command has no provider credential to refresh"
    );
    assert_eq!(
        admission::lock_arg_error(call, Some("hook x")),
        "host.oauth.refresh: a hook has no provider credential to refresh"
    );
    assert_eq!(
        admission::lock_arg_error(call, Some("watcher 0")),
        "host.oauth.refresh: a watcher has no provider credential to refresh"
    );
    assert_eq!(
        admission::lock_arg_error(call, Some("search s")),
        "host.oauth.refresh: a search backend has no provider credential to refresh"
    );
    assert_eq!(
        admission::lock_arg_error(call, Some("provider p.credential")),
        "host.oauth.refresh: p.credential has no credential to refresh; only credential() refreshes"
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
    table.mint(
        RequestKind::Stream,
        "e",
        ran_by.to_owned(),
        None,
        None,
        None,
    )
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
    let second = table.mint(RequestKind::Tool, "e", "tool b".into(), None, None, None);
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
    let next = table.mint(RequestKind::Stream, "e", "hook b".into(), None, None, None);
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
    let tool = table.mint(RequestKind::Tool, "e", "tool t".into(), None, None, None);
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
    let next = table.mint(RequestKind::Stream, "e", "hook b".into(), None, None, None);
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
    let minted = table.mint(RequestKind::Tool, "e", "tool t".into(), None, None, None);
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

// Process host/state decoding: every wire call decodes with the Lua
// half's validation and sentences, or refuses naming the problem. Each
// row pins one Lua sentence beside its process twin.

use super::context::HostOperation;

/// Decodes `call` with JSON `args`, as the wire carries them.
fn decode(
    call: &str,
    args: serde_json::Value,
) -> Result<calls::DecodedCall, (contract::ErrorCode, String)> {
    calls::decode_host(call, &args.as_array().cloned().unwrap_or_default())
}

/// Decodes `call` with JSON `args`, refusing with the problem.
fn refused(call: &str, args: serde_json::Value) -> (contract::ErrorCode, String) {
    decode(call, args).err().expect("the call refuses")
}

/// The refused sentence, for rows that share a code. Every decode
/// refusal is `invalid_arguments`, as in Lua.
fn refused_message(call: &str, args: serde_json::Value) -> String {
    let (code, message) = refused(call, args);
    assert_eq!(code, contract::ErrorCode::InvalidArguments);
    message
}

#[test]
fn http_decodes_a_full_request() {
    let body = crate::host::admission::encode_bytes(b"hello");
    let decoded = decode(
        "host.http",
        json!([{"url": "https://x.test", "method": "POST", "headers": {"a": "b"}, "body": body}]),
    )
    .expect("a full request decodes");
    let calls::DecodedCall::Service(HostOperation::Http {
        method,
        url,
        headers,
        body,
    }) = decoded
    else {
        panic!("a full request is a service operation");
    };
    assert_eq!(method, "POST");
    assert_eq!(url, "https://x.test");
    assert_eq!(headers, vec![("a".to_owned(), "b".to_owned())]);
    assert_eq!(body, Some(b"hello".to_vec()));
}

#[test]
fn http_decodes_a_bare_url_with_defaults() {
    let decoded = decode("host.http", json!([{"url": "https://x.test"}])).expect("decodes");
    let calls::DecodedCall::Service(HostOperation::Http {
        method,
        url,
        headers,
        body,
    }) = decoded
    else {
        panic!("a bare URL is a service operation");
    };
    assert_eq!(method, "GET");
    assert_eq!(url, "https://x.test");
    assert!(headers.is_empty());
    assert_eq!(body, None);
}

#[test]
fn http_names_a_missing_url_and_bad_shapes() {
    for args in [
        json!([]),
        json!([null]),
        json!([{}]),
        json!([{"method": "GET"}]),
    ] {
        assert_eq!(
            refused_message("host.http", args),
            "host.http: `url` must be a string"
        );
    }
    assert_eq!(
        refused_message("host.http", json!([{"url": "u", "method": 7}])),
        "host.http: `method` must be a string"
    );
    assert_eq!(
        refused_message("host.http", json!([{"url": "u", "headers": [["a", "b"]]}])),
        "host.http: `headers` must be a table"
    );
    assert_eq!(
        refused_message("host.http", json!([{"url": "u", "headers": {"a": 7}}])),
        "host.http: `headers` must be a table of strings"
    );
    assert_eq!(
        refused_message("host.http", json!([{"url": "u", "body": 7}])),
        "host.http: `body` must be a base64 string"
    );
    assert_eq!(
        refused_message("host.http", json!([{"url": "u", "body": "!!!"}])),
        "host.http: `body` is not valid base64"
    );
}

#[test]
fn exec_decodes_a_full_run() {
    let input = crate::host::admission::encode_bytes(b"in");
    let decoded = decode(
        "host.exec",
        json!(["prog", ["a", "b"], {"cwd": "sub", "env": {"K": "V"}, "input": input}]),
    )
    .expect("a full run decodes");
    let calls::DecodedCall::Service(HostOperation::Exec {
        program,
        args,
        cwd,
        env,
        input,
    }) = decoded
    else {
        panic!("a full run is a service operation");
    };
    assert_eq!(program, "prog");
    assert_eq!(args, vec!["a".to_owned(), "b".to_owned()]);
    assert_eq!(cwd, std::path::PathBuf::from("sub"));
    assert_eq!(env.get("K"), Some(&"V".to_owned()));
    assert_eq!(input, Some(b"in".to_vec()));
}

#[test]
fn exec_names_every_bad_shape() {
    assert_eq!(
        refused_message("host.exec", json!([7])),
        "host.exec: `program` must be a string"
    );
    assert_eq!(
        refused_message("host.exec", json!(["p", "x"])),
        "host.exec: every argument must be a string"
    );
    assert_eq!(
        refused_message("host.exec", json!(["p", ["a", 7]])),
        "host.exec: every argument must be a string"
    );
    assert_eq!(
        refused_message("host.exec", json!(["p", [], 7])),
        "host.exec: `opts` must be a table"
    );
    assert_eq!(
        refused_message("host.exec", json!(["p", [], {"cwd": 7}])),
        "host.exec: `cwd` must be a string"
    );
    assert_eq!(
        refused_message("host.exec", json!(["p", [], {"env": ["K"]}])),
        "host.exec: `env` must be a table of strings"
    );
    assert_eq!(
        refused_message("host.exec", json!(["p", [], {"input": 7}])),
        "host.exec: `input` must be a string"
    );
}

#[test]
fn exec_refuses_unspawnable_names_and_values() {
    for name in ["", "a=b", "a\0b"] {
        assert_eq!(
            refused_message("host.exec", json!(["p", [], {"env": {name: "v"}}])),
            "host.exec: `env` names must not be empty or contain `=` or NUL"
        );
    }
    assert_eq!(
        refused_message("host.exec", json!(["p", [], {"env": {"K": "a\0b"}}])),
        "host.exec: `env` values must not contain NUL"
    );
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

#[test]
fn tool_decodes_absent_and_empty_arguments_as_none() {
    for args in [
        json!(["t"]),
        json!(["t", null]),
        json!(["t", []]),
        json!(["t", {}]),
    ] {
        let decoded = decode("host.tool", args).expect("no arguments decode");
        let calls::DecodedCall::Service(HostOperation::Tool { name, arguments }) = decoded else {
            panic!("a tool call is a service operation");
        };
        assert_eq!(name, "t");
        assert_eq!(arguments, json!({}));
    }
}

#[test]
fn tool_keeps_a_non_empty_array_for_the_runner() {
    // An empty Lua table reads as an empty array, so `[]` is no
    // arguments; a non-empty array stays an array, and the runner
    // refuses it `invalid_arguments` as an ordinary return.
    let decoded = decode("host.tool", json!(["t", [7]])).expect("an array decodes");
    let calls::DecodedCall::Service(HostOperation::Tool { arguments, .. }) = decoded else {
        panic!("an array is a service operation");
    };
    assert_eq!(arguments, json!([7]));
}

#[test]
fn tool_names_a_bad_name_and_bad_arguments() {
    for args in [json!([]), json!([7]), json!([null])] {
        assert_eq!(
            refused_message("host.tool", args),
            "host.tool: `name` must be a string"
        );
    }
    for args in [json!(["t", 7]), json!(["t", "x"])] {
        assert_eq!(
            refused_message("host.tool", args),
            "host.tool: `args` must be a table"
        );
    }
}

#[test]
fn tool_refuses_past_125_nested_containers() {
    fn nested(wraps: usize) -> serde_json::Value {
        let mut value = json!({});
        for _ in 0..wraps {
            value = json!({"n": value});
        }
        value
    }
    // 124 wraps hold 125 containers with the innermost table; the 125th
    // wrap is the 126th container and refuses.
    assert!(decode("host.tool", json!(["t", nested(124)])).is_ok());
    assert_eq!(
        refused_message("host.tool", json!(["t", nested(125)])),
        "host.tool: `args` nests deeper than 125"
    );
}

#[test]
fn model_decodes_a_full_request() {
    let decoded = decode(
        "host.model",
        json!([{
            "model": "fiber/test",
            "system": "sys",
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": [{"type": "text", "text": "yo"}]},
            ],
            "max_tokens": 8,
        }]),
    )
    .expect("a full request decodes");
    let calls::DecodedCall::Service(HostOperation::Model { request }) = decoded else {
        panic!("a model call is a service operation");
    };
    assert_eq!(request.model, "fiber/test");
    assert_eq!(request.system.as_deref(), Some("sys"));
    assert_eq!(request.messages.len(), 2);
    assert_eq!(request.max_tokens, 8);
}

#[test]
fn model_names_every_bad_shape() {
    let full = || {
        json!([{
            "model": "fiber/test",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 8,
        }])
    };
    assert_eq!(
        refused_message("host.model", json!([null])),
        "host.model: `opts` must be a table"
    );
    let mut no_model = full();
    no_model[0].as_object_mut().unwrap().remove("model");
    assert_eq!(
        refused_message("host.model", no_model),
        "host.model: `model` must be a model reference, not a role"
    );
    assert_eq!(
        refused_message(
            "host.model",
            json!([{"model": "role", "messages": [{"role": "user", "content": "hi"}], "max_tokens": 8}]),
        ),
        "host.model: `model` must be a model reference, not a role"
    );
    assert_eq!(
        refused_message(
            "host.model",
            json!([{"model": "a/b", "system": 7, "messages": [{"role": "user", "content": "hi"}], "max_tokens": 8}]),
        ),
        "host.model: `system` must be a string"
    );
    for messages in [json!([]), json!("x")] {
        assert_eq!(
            refused_message(
                "host.model",
                json!([{"model": "a/b", "messages": messages, "max_tokens": 8}]),
            ),
            "host.model: `messages` must be a non-empty list"
        );
    }
    assert_eq!(
        refused_message(
            "host.model",
            json!([{"model": "a/b", "messages": [7], "max_tokens": 8}]),
        ),
        "host.model: `messages` must hold `{ role, content }` tables"
    );
    assert_eq!(
        refused_message(
            "host.model",
            json!([{"model": "a/b", "messages": [{"role": "x", "content": "hi"}], "max_tokens": 8}]),
        ),
        "host.model: a message `role` is `user` or `assistant`"
    );
    assert_eq!(
        refused_message(
            "host.model",
            json!([{"model": "a/b", "messages": [{"role": "user", "content": 7}], "max_tokens": 8}]),
        ),
        "host.model: a message `content` is a string or a list of parts"
    );
    assert_eq!(
        refused_message(
            "host.model",
            json!([{"model": "a/b", "messages": [{"role": "user", "content": [7]}], "max_tokens": 8}]),
        ),
        "host.model: a content part is a table"
    );
    assert_eq!(
        refused_message(
            "host.model",
            json!([{"model": "a/b", "messages": [{"role": "user", "content": [{"type": "x"}]}], "max_tokens": 8}]),
        ),
        "host.model: a content part `type` is `text`, `image` or `pdf`"
    );
    assert_eq!(
        refused_message(
            "host.model",
            json!([{"model": "a/b", "messages": [{"role": "user", "content": [{"type": "text"}]}], "max_tokens": 8}]),
        ),
        "host.model: a text part holds `text`, a string"
    );
    for tokens in [json!(0), json!(-1), json!(1.5), json!("8")] {
        assert_eq!(
            refused_message(
                "host.model",
                json!([{"model": "a/b", "messages": [{"role": "user", "content": "hi"}], "max_tokens": tokens}]),
            ),
            "host.model: `max_tokens` must be a positive integer"
        );
    }
}

#[test]
fn delegate_decodes_spec_timeout_and_run() {
    let decoded = decode(
        "host.delegate",
        json!([{"description": "d", "prompt": "p"}, {"timeout": 9, "run": true}]),
    )
    .expect("a delegate call decodes");
    let calls::DecodedCall::Service(HostOperation::Delegate {
        arguments,
        timeout,
        run,
    }) = decoded
    else {
        panic!("a delegate call is a service operation");
    };
    assert_eq!(arguments.get("description"), Some(&json!("d")));
    assert_eq!(timeout, std::time::Duration::from_millis(9));
    assert!(run);
}

#[test]
fn delegate_names_a_bad_spec_and_bad_options() {
    for args in [json!([]), json!([null])] {
        assert_eq!(
            refused_message("host.delegate", args),
            "host.delegate: `spec` must be a table"
        );
    }
    assert_eq!(
        refused_message("host.delegate", json!([["a"]])),
        "host.delegate: `spec` must be a table of named arguments"
    );
    // An empty table reads as `{}`, which the service refuses for its
    // missing fields; decoding passes it through.
    assert!(decode("host.delegate", json!([[], {"timeout": 1, "run": true}])).is_ok());
    for args in [
        json!([{}]),
        json!([{}, null]),
        json!([{}, {}]),
        json!([{}, {"timeout": 0, "run": true}]),
        json!([{}, {"timeout": -1, "run": true}]),
        json!([{}, {"timeout": 1.5, "run": true}]),
    ] {
        assert_eq!(
            refused_message("host.delegate", args),
            "host.delegate: `timeout` must be a whole number of milliseconds above 0"
        );
    }
    assert_eq!(
        refused_message("host.delegate", json!([{}, {"timeout": 1}])),
        "host.delegate: `run` must be a function"
    );
    assert_eq!(
        refused_message("host.delegate", json!([{}, {"timeout": 1, "run": false}])),
        "host.delegate: `run` must be a function"
    );
}

#[test]
fn ask_decodes_a_closed_interaction() {
    let decoded = decode("host.ask", json!(["confirm", {"prompt": "go?"}])).expect("decodes");
    assert!(
        matches!(decoded, calls::DecodedCall::Ask(_)),
        "an ask parks"
    );
}

#[test]
fn ask_names_a_bad_kind_and_spec() {
    assert_eq!(
        refused_message("host.ask", json!([7])),
        "host.ask: `kind` must be a string"
    );
    assert_eq!(
        refused_message("host.ask", json!(["nope", {}])),
        "host.ask: unknown kind \"nope\"; one of confirm, select, multi_select, text_input, form"
    );
    assert_eq!(
        refused_message("host.ask", json!(["confirm", []])),
        "host.ask: spec must be a table"
    );
}

#[test]
fn oauth_callback_decodes_port_with_optional_path() {
    let decoded = decode("host.oauth.callback", json!([8080, "/cb"])).expect("decodes");
    let calls::DecodedCall::Service(HostOperation::OAuthCallback { port, path }) = decoded else {
        panic!("a callback is a service operation");
    };
    assert_eq!(port, 8080);
    assert_eq!(path.as_deref(), Some("/cb"));
    let decoded = decode("host.oauth.callback", json!([1])).expect("decodes");
    let calls::DecodedCall::Service(HostOperation::OAuthCallback { port, path }) = decoded else {
        panic!("a bare port is a service operation");
    };
    assert_eq!(port, 1);
    assert_eq!(path, None);
}

#[test]
fn oauth_callback_names_a_bad_port_and_path() {
    for port in [json!(0), json!(65536), json!(-1), json!(1.5), json!("8080")] {
        assert_eq!(
            refused_message("host.oauth.callback", json!([port])),
            "host.oauth.callback: `port` must be a whole number from 1 to 65535"
        );
    }
    assert_eq!(
        refused_message("host.oauth.callback", json!([])),
        "host.oauth.callback: `port` must be a whole number from 1 to 65535"
    );
    assert_eq!(
        refused_message("host.oauth.callback", json!([8080, "cb"])),
        "host.oauth.callback: `path` must be a string starting with `/`"
    );
    assert_eq!(
        refused_message("host.oauth.callback", json!([8080, 7])),
        "host.oauth.callback: `path` must be a string starting with `/`"
    );
}

#[test]
fn oauth_open_show_and_pkce_decode() {
    let decoded = decode("host.oauth.open", json!(["https://x.test"])).expect("decodes");
    assert!(matches!(
        decoded,
        calls::DecodedCall::Service(HostOperation::OAuthOpen { .. })
    ));
    assert_eq!(
        refused_message("host.oauth.open", json!([7])),
        "host.oauth.open: url must be a string"
    );
    let decoded = decode("host.oauth.show", json!(["https://x.test", "code-1"])).expect("decodes");
    let calls::DecodedCall::Service(HostOperation::OAuthShow { url, code }) = decoded else {
        panic!("a show is a service operation");
    };
    assert_eq!(url, "https://x.test");
    assert_eq!(code, "code-1");
    for args in [
        json!([]),
        json!(["u"]),
        json!(["u", null]),
        json!([null, "c"]),
    ] {
        assert_eq!(
            refused_message("host.oauth.show", args),
            "host.oauth.show: `url` and `code` must be strings"
        );
    }
    let decoded = decode("host.oauth.pkce", json!([])).expect("decodes");
    assert!(matches!(
        decoded,
        calls::DecodedCall::Service(HostOperation::OAuthPkcs)
    ));
}

#[test]
fn oauth_poll_decodes_full_and_bare_requests() {
    let body = crate::host::admission::encode_bytes(b"b");
    let decoded = decode(
        "host.oauth.poll",
        json!(["https://x.test", "PUT", {"a": "b"}, body, [403, 404], 7]),
    )
    .expect("decodes");
    let calls::DecodedCall::Service(HostOperation::OAuthPoll {
        url,
        method,
        headers,
        body,
        pending,
        interval,
    }) = decoded
    else {
        panic!("a poll is a service operation");
    };
    assert_eq!(url, "https://x.test");
    assert_eq!(method, "PUT");
    assert_eq!(headers, vec![("a".to_owned(), "b".to_owned())]);
    assert_eq!(body, Some(b"b".to_vec()));
    assert_eq!(pending, Some(vec![403, 404]));
    assert_eq!(interval, 7);
    let decoded = decode("host.oauth.poll", json!(["https://x.test"])).expect("decodes");
    let calls::DecodedCall::Service(HostOperation::OAuthPoll {
        method,
        pending,
        interval,
        ..
    }) = decoded
    else {
        panic!("a bare poll is a service operation");
    };
    assert_eq!(method, "POST");
    assert_eq!(pending, None);
    assert_eq!(interval, 5);
}

#[test]
fn oauth_poll_names_bad_shapes_and_clamps_a_short_interval() {
    assert_eq!(
        refused_message("host.oauth.poll", json!([7])),
        "host.oauth.poll: `url` must be a string"
    );
    assert_eq!(
        refused_message("host.oauth.poll", json!(["u", 7])),
        "host.oauth.poll: `method` must be a string"
    );
    assert_eq!(
        refused_message("host.oauth.poll", json!(["u", null, 7])),
        "host.oauth.poll: `headers` must be a table"
    );
    assert_eq!(
        refused_message("host.oauth.poll", json!(["u", null, null, 7])),
        "host.oauth.poll: `body` must be a base64 string"
    );
    for pending in [json!(99), json!(600), json!(1.5), json!("x")] {
        assert_eq!(
            refused_message("host.oauth.poll", json!(["u", null, null, null, [pending]])),
            "host.oauth.poll: `pending` must be a list of whole-number statuses from 100 to 599"
        );
    }
    assert_eq!(
        refused_message("host.oauth.poll", json!(["u", null, null, null, 7])),
        "host.oauth.poll: `pending` must be a list of whole-number statuses from 100 to 599"
    );
    for interval in [json!(3601), json!(1.5), json!("x")] {
        assert_eq!(
            refused_message(
                "host.oauth.poll",
                json!(["u", null, null, null, null, interval])
            ),
            "host.oauth.poll: `interval` must be a whole number of seconds, at most 3600"
        );
    }
    // A shorter wait reads as one second, as the Lua half clamps.
    for interval in [json!(0), json!(1)] {
        let decoded = decode(
            "host.oauth.poll",
            json!(["u", null, null, null, null, interval]),
        )
        .expect("a short interval decodes");
        let calls::DecodedCall::Service(HostOperation::OAuthPoll { interval, .. }) = decoded else {
            panic!("a clamped poll is a service operation");
        };
        assert_eq!(interval, 1);
    }
    let decoded = decode(
        "host.oauth.poll",
        json!(["u", null, null, null, null, 3600]),
    )
    .expect("the longest interval decodes");
    let calls::DecodedCall::Service(HostOperation::OAuthPoll { interval, .. }) = decoded else {
        panic!("the longest poll is a service operation");
    };
    assert_eq!(interval, 3600);
}

#[test]
fn oauth_refresh_takes_only_its_function_marker() {
    let decoded = decode("host.oauth.refresh", json!([true])).expect("decodes");
    assert!(matches!(decoded, calls::DecodedCall::Refresh));
    for args in [json!([]), json!([false]), json!([null]), json!([7])] {
        assert_eq!(
            refused_message("host.oauth.refresh", args),
            "host.oauth.refresh: takes a function"
        );
    }
}

#[test]
fn fs_read_list_and_stat_decode_a_path() {
    for call in ["host.fs.read", "host.fs.list", "host.fs.stat"] {
        let decoded = decode(call, json!(["a.txt"])).expect("a path decodes");
        assert!(
            matches!(
                decoded,
                calls::DecodedCall::Service(
                    HostOperation::FsRead { .. }
                        | HostOperation::FsList { .. }
                        | HostOperation::FsStat { .. }
                )
            ),
            "{call} is a service operation"
        );
        assert_eq!(
            refused_message(call, json!([7])),
            format!("{call}: path must be a string")
        );
        assert_eq!(
            refused_message(call, json!([])),
            format!("{call}: path must be a string")
        );
    }
}

#[test]
fn fs_write_decodes_contents_with_its_lock() {
    let contents = crate::host::admission::encode_bytes(b"data");
    let decoded =
        decode("host.fs.write", json!(["a.txt", contents, {"lock": true}])).expect("decodes");
    let calls::DecodedCall::Service(HostOperation::FsWrite {
        path,
        contents,
        lock,
    }) = decoded
    else {
        panic!("a write is a service operation");
    };
    assert_eq!(path, "a.txt");
    assert_eq!(contents, b"data".to_vec());
    assert!(lock);
    let decoded = decode(
        "host.fs.write",
        json!(["a.txt", crate::host::admission::encode_bytes(b"")]),
    )
    .expect("a missing lock reads as none");
    let calls::DecodedCall::Service(HostOperation::FsWrite { lock, .. }) = decoded else {
        panic!("a lockless write is a service operation");
    };
    assert!(!lock);
    assert_eq!(
        refused_message("host.fs.write", json!(["a.txt", 7])),
        "host.fs.write: data must be a string"
    );
    assert_eq!(
        refused_message("host.fs.write", json!(["a.txt", "!!!"])),
        "host.fs.write: `data` is not valid base64"
    );
    assert_eq!(
        refused_message("host.fs.write", json!(["a.txt", "Zg==", {"lock": "yes"}])),
        "host.fs: `lock` must be a boolean"
    );
    assert_eq!(
        refused_message("host.fs.write", json!(["a.txt", "Zg==", 7])),
        "host.fs: options must be a table"
    );
}

#[test]
fn fs_mutations_carry_the_lock_at_their_position() {
    for (call, args) in [
        ("host.fs.mkdir", json!(["d", {"lock": true}])),
        ("host.fs.remove", json!(["d", {"lock": true}])),
    ] {
        let decoded = decode(call, args).expect("a lock decodes");
        let locked = matches!(
            decoded,
            calls::DecodedCall::Service(HostOperation::FsMkdir { lock: true, .. })
                | calls::DecodedCall::Service(HostOperation::FsRemove { lock: true, .. })
        );
        assert!(locked, "{call} carries its lock");
    }
    let decoded = decode("host.fs.rename", json!(["a", "b", {"lock": true}])).expect("decodes");
    let calls::DecodedCall::Service(HostOperation::FsRename { from, to, lock }) = decoded else {
        panic!("a rename is a service operation");
    };
    assert_eq!((from.as_str(), to.as_str(), lock), ("a", "b", true));
    assert_eq!(
        refused_message("host.fs.rename", json!([[7], "b"])),
        "host.fs.rename: path must be a string"
    );
    assert_eq!(
        refused_message("host.fs.rename", json!(["a", null])),
        "host.fs.rename: path must be a string"
    );
    assert_eq!(
        refused_message("host.fs.mkdir", json!([7])),
        "host.fs.mkdir: path must be a string"
    );
}

#[test]
fn config_get_and_set_decode_key_value_and_scope() {
    let decoded = decode("host.config.get", json!(["a.b"])).expect("decodes");
    let calls::DecodedCall::Service(HostOperation::ConfigGet { key }) = decoded else {
        panic!("a get is a service operation");
    };
    assert_eq!(key, "a.b");
    assert_eq!(
        refused_message("host.config.get", json!([7])),
        "host.config.get: key must be a string"
    );
    let decoded = decode("host.config.set", json!(["a.b", 7, "machine"])).expect("decodes");
    let calls::DecodedCall::Service(HostOperation::ConfigSet { key, value, scope }) = decoded
    else {
        panic!("a set is a service operation");
    };
    assert_eq!(key, "a.b");
    assert_eq!(value, json!(7));
    assert!(matches!(scope, super::context::ConfigScope::Machine));
    let decoded = decode("host.config.set", json!(["a.b", false, "project"])).expect("decodes");
    assert!(matches!(
        decoded,
        calls::DecodedCall::Service(HostOperation::ConfigSet { .. })
    ));
    assert_eq!(
        refused_message("host.config.set", json!([7, 7, "machine"])),
        "host.config.set: key must be a string"
    );
    assert_eq!(
        refused_message("host.config.set", json!(["a.b", null, "machine"])),
        "host.config.set: value must not be nil"
    );
    assert_eq!(
        refused_message("host.config.set", json!(["a.b", 7, "everywhere"])),
        "host.config.set: scope must be \"machine\" or \"project\""
    );
    assert_eq!(
        refused_message("host.config.set", json!(["a.b", 7])),
        "host.config.set: scope must be \"machine\" or \"project\""
    );
}

#[test]
fn secret_and_data_dir_decode_their_single_argument() {
    let decoded = decode("host.secret", json!(["token"])).expect("decodes");
    let calls::DecodedCall::Service(HostOperation::Secret { name }) = decoded else {
        panic!("a secret read is a service operation");
    };
    assert_eq!(name, "token");
    assert_eq!(
        refused_message("host.secret", json!([7])),
        "host.secret: name must be a string"
    );
    let decoded = decode("host.data_dir", json!(["project"])).expect("decodes");
    assert!(matches!(
        decoded,
        calls::DecodedCall::Service(HostOperation::DataDir { .. })
    ));
    for scope in [json!(["everywhere"]), json!([]), json!([7])] {
        assert_eq!(
            refused_message("host.data_dir", scope),
            "host.data_dir: scope must be \"machine\" or \"project\""
        );
    }
}

#[test]
fn sha256_and_hmac_answer_bytes_positions() {
    // The empty string's digest is the well-known one, pinning the
    // hash to its algorithm instead of the test's own math.
    let empty = crate::host::admission::encode_bytes(b"");
    let decoded = decode("host.sha256", json!([empty])).expect("decodes");
    let calls::DecodedCall::Service(HostOperation::Sha256 { data }) = decoded else {
        panic!("a digest is a service operation");
    };
    assert!(data.is_empty());
    assert_eq!(
        refused_message("host.sha256", json!([[7]])),
        "host.sha256: `data` must be a base64 string"
    );
    let key = crate::host::admission::encode_bytes(b"k");
    let data = crate::host::admission::encode_bytes(b"d");
    let decoded = decode("host.hmac_sha256", json!([key.clone(), data.clone()])).expect("decodes");
    let calls::DecodedCall::Service(HostOperation::HmacSha256 {
        key: raw_key,
        data: raw_data,
    }) = decoded
    else {
        panic!("a tag is a service operation");
    };
    assert_eq!((raw_key, raw_data), (b"k".to_vec(), b"d".to_vec()));
    assert_eq!(
        refused_message("host.hmac_sha256", json!([key, "!!!"])),
        "host.hmac_sha256: `data` is not valid base64"
    );
    assert_eq!(
        refused_message("host.hmac_sha256", json!([7, data])),
        "host.hmac_sha256: `key` must be a base64 string"
    );
}

#[test]
fn status_widget_emit_and_log_decode() {
    let decoded = decode("host.status", json!(["line"])).expect("decodes");
    let calls::DecodedCall::Service(HostOperation::Status { text }) = decoded else {
        panic!("a status is a service operation");
    };
    assert_eq!(text, "line");
    assert_eq!(
        refused_message("host.status", json!([7])),
        "host.status: `text` must be a string"
    );
    let decoded = decode("host.widget", json!(["w", ["a", "b"]])).expect("decodes");
    let calls::DecodedCall::Service(HostOperation::Widget { id, lines }) = decoded else {
        panic!("a widget is a service operation");
    };
    assert_eq!(id, "w");
    assert_eq!(lines, vec!["a".to_owned(), "b".to_owned()]);
    assert_eq!(
        refused_message("host.widget", json!(["w", ["a", 7]])),
        "host.widget: `lines` must be a list of strings"
    );
    assert_eq!(
        refused_message("host.widget", json!(["w", "ab"])),
        "host.widget: `lines` must be a list of strings"
    );
    let decoded = decode("host.emit", json!([{"k": "v"}])).expect("decodes");
    let calls::DecodedCall::Service(HostOperation::Emit { data }) = decoded else {
        panic!("an emit is a service operation");
    };
    assert_eq!(data, json!({"k": "v"}));
    let decoded = decode("host.log", json!(["line"])).expect("decodes");
    let calls::DecodedCall::Service(HostOperation::Log { message }) = decoded else {
        panic!("a log is a service operation");
    };
    assert_eq!(message, "line");
    // A number logs as its text, as the Lua half converts.
    let decoded = decode("host.log", json!([7])).expect("a number logs");
    let calls::DecodedCall::Service(HostOperation::Log { message }) = decoded else {
        panic!("a numbered log is a service operation");
    };
    assert_eq!(message, "7");
    assert_eq!(
        refused_message("host.log", json!([true])),
        "host.log: the message must be a string"
    );
}

#[test]
fn state_calls_decode_key_value_and_rule() {
    let decoded = decode("state.get", json!(["k"])).expect("decodes");
    assert!(matches!(
        decoded,
        calls::DecodedCall::Service(HostOperation::StateGet { .. })
    ));
    let decoded = decode("state.keys", json!([])).expect("decodes");
    assert!(matches!(
        decoded,
        calls::DecodedCall::Service(HostOperation::StateKeys)
    ));
    let decoded = decode("state.set", json!(["k", 7])).expect("decodes");
    let calls::DecodedCall::Service(HostOperation::StateSet {
        key,
        value,
        on_fork,
    }) = decoded
    else {
        panic!("a set is a service operation");
    };
    assert_eq!(key, "k");
    assert_eq!(value, json!(7));
    assert!(matches!(on_fork, contract::events::OnFork::AtPoint));
    for (rule, fork) in [
        ("latest", contract::events::OnFork::Latest),
        ("fresh", contract::events::OnFork::Fresh),
    ] {
        let decoded = decode("state.set", json!(["k", 7, {"on_fork": rule}])).expect("decodes");
        let calls::DecodedCall::Service(HostOperation::StateSet { on_fork: read, .. }) = decoded
        else {
            panic!("a ruled set is a service operation");
        };
        assert_eq!(read, fork);
    }
    assert_eq!(
        refused_message("state.set", json!([7, 7])),
        "state: key must be a string"
    );
    assert_eq!(
        refused_message("state.get", json!([7])),
        "state: key must be a string"
    );
    assert_eq!(
        refused_message("state.set", json!(["k", null])),
        "state.set: value is nil; use state.unset to remove the key"
    );
    assert_eq!(
        refused_message("state.set", json!(["k", 7, 7])),
        "state.set: options must be a table"
    );
    assert_eq!(
        refused_message("state.set", json!(["k", 7, {"on_fork": "never"}])),
        "state.set: on_fork must be at_point, latest or fresh"
    );
    assert_eq!(
        refused_message("state.set", json!(["k", 7, {"on_fork": 7}])),
        "state.set: on_fork must be at_point, latest or fresh"
    );
    let decoded = decode("state.unset", json!(["k"])).expect("decodes");
    assert!(matches!(
        decoded,
        calls::DecodedCall::Service(HostOperation::StateUnset { .. })
    ));
}

#[test]
fn excluded_calls_name_their_exclusion() {
    for call in ["host.after", "host.every"] {
        assert_eq!(
            refused_message(call, json!([100])),
            format!("{call} is not a process call; the program keeps its own timers")
        );
    }
    for call in ["host.drive", "host.json"] {
        assert_eq!(
            refused_message(call, json!([])),
            format!("{call} is not a process call")
        );
    }
    assert_eq!(
        refused_message("host.nope", json!([])),
        "unknown call `host.nope`"
    );
}
