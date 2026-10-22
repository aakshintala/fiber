//! Elicitation over the wire: the fixture elicits, `FakeAsk` answers,
//! and the server gets the answers (`docs/mcp.md`, "Elicitation,
//! sampling and roots"). Callers run on detached threads holding an
//! `Arc<Server>`, as the cancel test does; every blocking call carries a
//! deadline naming its wait, and the fake clock advances only after the
//! ask carrying its `until` is observed.

use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use contract::ActionId;
use contract::clock::Clock;
use contract::events::{Answer, Interaction};
use contract::tool::Answered;
use fakes::Deadline;
use serde_json::{Value, json};

use super::{CallError, Server};
use crate::elicit::Asker;
use crate::elicit::tests::FakeAsk;
use crate::rpc::{Named, TOOLS_CALL};
use crate::test_support::{Setup, WITHIN};

/// One `tools/call` through `call_asking`, with `ask` for elicitations.
fn call_asking(
    server: &Server,
    tool: &str,
    timeout: Duration,
    cancel: &dyn contract::tool::Cancel,
    ask: &dyn contract::tool::Ask,
) -> Result<Value, CallError> {
    let empty = serde_json::Map::new();
    let params = Named {
        arguments: &empty,
        name: tool,
    };
    server.call_asking(
        TOOLS_CALL,
        &params,
        timeout,
        cancel,
        Some(Asker { ask, server: "fx" }),
    )
}

/// One ordinary `tools/call` with no asker: it never takes an
/// elicitation and is never listed in its `action_ids`.
fn call_plain(server: &Server, tool: &str, timeout: Duration) -> Result<Value, CallError> {
    let empty = serde_json::Map::new();
    let params = Named {
        arguments: &empty,
        name: tool,
    };
    server.call(TOOLS_CALL, &params, timeout, &fakes::CancelToken::new())
}

/// Makes `name` a FIFO under the fixture directory.
fn mkfifo(setup: &Setup, name: &str) {
    fakes::fifo(setup.dir.path().join(name));
}

/// Reads one FIFO's line on a thread, with a deadline naming the wait:
/// opening a FIFO blocks until the fixture opens the other end.
fn read_fifo(path: std::path::PathBuf, what: &str) -> String {
    let (done, read) = mpsc::channel();
    thread::spawn(move || {
        let content = std::fs::read_to_string(&path).expect("the fifo reads");
        done.send(content).expect("collected");
    });
    Deadline::after(WITHIN).recv_or_fail(&read, what)
}

/// Writes one line to a FIFO on a thread, with a deadline naming the
/// wait: opening blocks until the fixture reads.
fn write_fifo(path: std::path::PathBuf, what: &str) {
    let (done, wrote) = mpsc::channel();
    thread::spawn(move || {
        std::fs::write(&path, "go\n").expect("the fifo writes");
        done.send(()).expect("collected");
    });
    Deadline::after(WITHIN).recv_or_fail(&wrote, what);
}

/// The two-field elicitation: `name` (a required string) then `approve`
/// (a boolean). `name` first is the reverse of sorted order, so the order
/// proves the server's order end to end.
fn two_field_elicitation() -> String {
    r#"{"message":"Deploy settings","requestedSchema":{"type":"object","properties":{"name":{"type":"string","title":"Name"},"approve":{"type":"boolean"}},"required":["name"]}}"#.to_owned()
}

/// Waits, at most `WITHIN`, until request `id` has a response in its slot:
/// the second signal the elicitation tests wait for before releasing the
/// asker. Each turn reads the slot through `view`, then parks until `seq`
/// moves past the value that `view` read, so a response landing between the
/// two is not missed. The test takes `id` as `next_id() + 1` before its call
/// starts, with no other request issued until this returns.
fn await_response(shared: &super::Shared, id: u64) {
    let wait = Deadline::after(WITHIN);
    loop {
        let view = shared.view(id, &super::NoCancel);
        if view.response.is_some() {
            return;
        }
        let state = crate::registry::lock(&shared.inner);
        let (state, timeout) = shared
            .cv
            .wait_timeout_while(state, wait.left(), |state| state.seq == view.seq)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        drop(state);
        if timeout.timed_out() {
            // The slot can fill between the timeout and the relock, so the
            // slot, not the timeout, decides.
            assert!(
                shared.view(id, &super::NoCancel).response.is_some(),
                "a response lands within {WITHIN:?}"
            );
            return;
        }
    }
}

/// One ordinary echo call on `server`: the fixture reads stdin in order
/// and logs each line first, so the echo's answer proves every earlier
/// line is logged.
fn barrier(server: &Arc<Server>) {
    let echoed = Arc::clone(server);
    let answered = fakes::within(
        "the echo barrier call",
        Deadline::after(WITHIN),
        move || call_plain(&echoed, "echo", Duration::from_secs(30)),
    );
    answered.expect("the echo barrier answers");
}

/// Every `requests.log` line naming an elicitation answer.
fn elicitation_answers(log: &str) -> Vec<&str> {
    log.lines().filter(|line| line.contains("\"el-")).collect()
}

#[test]
fn an_answered_elicitation_asks_each_field_and_accepts() {
    let setup = Setup::with_tools(&json!([{"name": "ask"}]));
    setup.result("ask", r#"{"content":[{"type":"text","text":"done"}]}"#);
    setup.write("elicit-ask.json", &two_field_elicitation());
    let opened = setup.start_expect(Duration::from_secs(5));
    let server = Arc::new(opened.server);
    let (ask, seen) = FakeAsk::scripted(
        "a_1",
        vec![
            Answered::Reply(Answer::Text {
                text: "prod".to_owned(),
            }),
            Answered::Reply(Answer::Confirmed { confirmed: true }),
        ],
    );
    let (done, result) = mpsc::channel();
    {
        let server = Arc::clone(&server);
        thread::spawn(move || {
            let answer = call_asking(
                &server,
                "ask",
                Duration::from_secs(60),
                &fakes::CancelToken::new(),
                &ask,
            );
            done.send(answer).expect("collected");
        });
    }
    // Two asks in field order, each with this call's action alone.
    let wait = Deadline::after(WITHIN);
    let first = wait.recv_or_fail(&seen, "the first field's ask");
    assert!(
        matches!(first.interaction, Interaction::TextInput { .. }),
        "the string field asks a text input",
    );
    assert_eq!(
        first.action_ids,
        vec![ActionId("a_1".to_owned())],
        "one call in flight lists its action alone",
    );
    let second = wait.recv_or_fail(&seen, "the second field's ask");
    assert!(
        matches!(second.interaction, Interaction::Confirm { .. }),
        "the boolean field asks a confirm",
    );
    assert_eq!(
        Deadline::after(WITHIN)
            .recv(&result)
            .unwrap_or_else(|_| { panic!("the answered call returns within {WITHIN:?}") }),
        Ok(json!({"content": [{"type": "text", "text": "done"}]})),
    );
    // The fixture answers the call only on reading the `accept`, so the
    // result proves it is logged: no barrier.
    let log = std::fs::read_to_string(setup.dir.path().join("requests.log")).expect("requests");
    let accept = log
        .lines()
        .find(|line| line.contains("\"el-") && line.contains("\"action\":\"accept\""))
        .unwrap_or_else(|| panic!("the accept reaches the wire: {log}"));
    assert!(
        accept.contains(r#""approve":true"#) && accept.contains(r#""name":"prod""#),
        "the server gets the answers: {accept}",
    );
}

#[test]
fn an_unanswered_elicitation_is_declined_at_the_calls_deadline() {
    let setup = Setup::with_tools(&json!([{"name": "ask"}, {"name": "echo"}]));
    setup.result("ask", r#"{"content":[{"type":"text","text":"done"}]}"#);
    setup.result("echo", r#"{"content":[]}"#);
    setup.write("elicit-ask.json", &two_field_elicitation());
    let opened = setup.start_expect(Duration::from_secs(5));
    let server = Arc::new(opened.server);
    let timeout = Duration::from_secs(60);
    let deadline = setup.fake.now().checked_add(timeout).expect("deadline");
    let (ask, seen, send) = FakeAsk::held("a_1");
    let (done, result) = mpsc::channel();
    {
        let server = Arc::clone(&server);
        thread::spawn(move || {
            let answer = call_asking(&server, "ask", timeout, &fakes::CancelToken::new(), &ask);
            done.send(answer).expect("collected");
        });
    }
    // The ask carries the call's own deadline: the elicitation never
    // outlives the first call it belongs to.
    let first = Deadline::after(WITHIN).recv_or_fail(&seen, "the first field's ask");
    assert_eq!(
        first.until,
        Some(deadline),
        "the ask ends at the call's deadline"
    );
    // The clock advances only now that the ask (with its `until`) is
    // observed. The send below models the loop releasing the ask at
    // `until` with `NoAnswer`: the handler declines, then its own
    // deadline wins over any response delivered meanwhile.
    // No code under test waits on the clock here; the call thread is blocked in the held ask, past its stale park at the deadline (ruling 6179406086).
    setup.fake.advance(timeout);
    send.send(Answered::NoAnswer).expect("the ask ends");
    assert_eq!(
        Deadline::after(WITHIN)
            .recv(&result)
            .unwrap_or_else(|_| { panic!("the unanswered call ends within {WITHIN:?}") }),
        Err(CallError::Timeout),
    );
    barrier(&server);
    let log = std::fs::read_to_string(setup.dir.path().join("requests.log")).expect("requests");
    let answers = elicitation_answers(&log);
    assert_eq!(answers.len(), 1, "one answer to the elicitation: {log}");
    assert!(
        answers[0].contains("\"action\":\"decline\""),
        "the unanswered elicitation is declined: {log}",
    );
}

#[test]
fn two_calls_in_flight_list_both_actions_with_the_first_deadline() {
    let setup = Setup::with_tools(&json!([{"name": "hold"}, {"name": "ask"}, {"name": "third"}]));
    setup.result("hold", r#"{"content":[{"type":"text","text":"held"}]}"#);
    setup.result("ask", r#"{"content":[{"type":"text","text":"asked"}]}"#);
    setup.result("third", r#"{"content":[{"type":"text","text":"third"}]}"#);
    setup.write("elicit-ask.json", &two_field_elicitation());
    for name in ["held-hold", "release-hold", "held-third", "release-third"] {
        mkfifo(&setup, name);
    }
    let opened = setup.start_expect(Duration::from_secs(5));
    let server = Arc::new(opened.server);
    let hold_deadline = setup
        .fake
        .now()
        .checked_add(Duration::from_secs(30))
        .expect("deadline");
    // One script for whichever call takes it: either may, so both draw
    // from one channel.
    let (hold_ask, seen) = FakeAsk::scripted(
        "a_hold",
        vec![
            Answered::Reply(Answer::Text {
                text: "prod".to_owned(),
            }),
            Answered::Reply(Answer::Confirmed { confirmed: true }),
        ],
    );
    let ask_ask = hold_ask.sharing("a_ask");
    // `hold` first: the acknowledgement proves its line was sent, so its
    // request id is lower than every call started after it.
    let (hold_done, hold_result) = mpsc::channel();
    {
        let server = Arc::clone(&server);
        thread::spawn(move || {
            let answer = call_asking(
                &server,
                "hold",
                Duration::from_secs(30),
                &fakes::CancelToken::new(),
                &hold_ask,
            );
            hold_done.send(answer).expect("collected");
        });
    }
    assert_eq!(
        read_fifo(setup.dir.path().join("held-hold"), "the hold is held"),
        "held\n",
    );
    // A third call with no asker is in flight too and is never listed.
    let (third_done, third_result) = mpsc::channel();
    {
        let server = Arc::clone(&server);
        thread::spawn(move || {
            third_done
                .send(call_plain(&server, "third", Duration::from_secs(60)))
                .expect("collected");
        });
    }
    assert_eq!(
        read_fifo(
            setup.dir.path().join("held-third"),
            "the third call is held"
        ),
        "held\n",
    );
    let (ask_done, ask_result) = mpsc::channel();
    {
        let server = Arc::clone(&server);
        thread::spawn(move || {
            let answer = call_asking(
                &server,
                "ask",
                Duration::from_secs(60),
                &fakes::CancelToken::new(),
                &ask_ask,
            );
            ask_done.send(answer).expect("collected");
        });
    }
    // The one ask lists both asking calls in request-id order, with the
    // first call's deadline: never later than the handler's own.
    let wait = Deadline::after(WITHIN);
    let asking = wait.recv_or_fail(&seen, "the elicitation's ask");
    assert_eq!(
        asking.action_ids,
        vec![ActionId("a_hold".to_owned()), ActionId("a_ask".to_owned())],
        "both asking calls, in request-id order",
    );
    assert_eq!(
        asking.until,
        Some(hold_deadline),
        "the first call's deadline"
    );
    let again = wait.recv_or_fail(&seen, "the second field's ask");
    assert_eq!(
        again.action_ids, asking.action_ids,
        "every field lists both"
    );
    // The fixture answers the elicited call on reading the `accept`,
    // whichever call took it; the held calls answer once released.
    assert_eq!(
        Deadline::after(WITHIN)
            .recv(&ask_result)
            .unwrap_or_else(|_| { panic!("the elicited call answers within {WITHIN:?}") }),
        Ok(json!({"content": [{"type": "text", "text": "asked"}]})),
    );
    write_fifo(
        setup.dir.path().join("release-hold"),
        "the hold is released",
    );
    assert_eq!(
        Deadline::after(WITHIN)
            .recv(&hold_result)
            .unwrap_or_else(|_| { panic!("the held call answers within {WITHIN:?}") }),
        Ok(json!({"content": [{"type": "text", "text": "held"}]})),
    );
    write_fifo(
        setup.dir.path().join("release-third"),
        "the third call is released",
    );
    assert_eq!(
        Deadline::after(WITHIN)
            .recv(&third_result)
            .unwrap_or_else(|_| { panic!("the third call answers within {WITHIN:?}") }),
        Ok(json!({"content": [{"type": "text", "text": "third"}]})),
    );
    assert!(seen.try_recv().is_err(), "only one elicitation is asked");
}

#[test]
fn a_cancel_after_an_elicitation_cancels_despite_the_response() {
    let setup = Setup::with_tools(&json!([{"name": "ask"}, {"name": "echo"}]));
    setup.result("ask", r#"{"content":[{"type":"text","text":"done"}]}"#);
    setup.result("echo", r#"{"content":[]}"#);
    setup.write("elicit-ask.json", &two_field_elicitation());
    // The fixture writes the elicitation request, then the result: the
    // response is in its slot before the asker is released. The release
    // FIFO is read twice, so the ask is observed before the response can
    // land: no take, no ruling-10 precedence to prove.
    setup.write("result-after-elicit-ask", "");
    mkfifo(&setup, "held-ask");
    mkfifo(&setup, "release-ask");
    let opened = setup.start_expect(Duration::from_secs(5));
    let server = Arc::new(opened.server);
    let id = server.inner.shared.next_id() + 1;
    let (ask, seen, send) = FakeAsk::held("a_1");
    let cancel = fakes::CancelToken::new();
    let (done, result) = mpsc::channel();
    {
        let server = Arc::clone(&server);
        let cancel = cancel.clone();
        thread::spawn(move || {
            let answer = call_asking(&server, "ask", Duration::from_secs(60), &cancel, &ask);
            done.send(answer).expect("collected");
        });
    }
    assert_eq!(
        read_fifo(
            setup.dir.path().join("held-ask"),
            "the answer subshell starts"
        ),
        "held\n",
    );
    write_fifo(
        setup.dir.path().join("release-ask"),
        "the elicitation is sent",
    );
    Deadline::after(WITHIN).recv_or_fail(&seen, "the first field's ask");
    write_fifo(setup.dir.path().join("release-ask"), "the result is sent");
    await_response(&server.inner.shared, id);
    cancel.cancel();
    send.send(Answered::NoAnswer).expect("the ask ends");
    assert_eq!(
        Deadline::after(WITHIN)
            .recv(&result)
            .unwrap_or_else(|_| { panic!("the cancelled call ends within {WITHIN:?}") }),
        Err(CallError::Cancelled),
    );
    barrier(&server);
    let log = std::fs::read_to_string(setup.dir.path().join("requests.log")).expect("requests");
    let decline = log
        .find("\"action\":\"decline\"")
        .expect("the decline is logged");
    let cancelled = log
        .find("notifications/cancelled")
        .expect("the cancel is logged");
    assert!(
        decline < cancelled,
        "the decline goes out before the cancel: {log}",
    );
}

#[test]
fn a_deadline_after_an_elicitation_times_out_despite_the_response() {
    let setup = Setup::with_tools(&json!([{"name": "ask"}, {"name": "echo"}]));
    setup.result("ask", r#"{"content":[{"type":"text","text":"done"}]}"#);
    setup.result("echo", r#"{"content":[]}"#);
    setup.write("elicit-ask.json", &two_field_elicitation());
    setup.write("result-after-elicit-ask", "");
    mkfifo(&setup, "held-ask");
    mkfifo(&setup, "release-ask");
    let opened = setup.start_expect(Duration::from_secs(5));
    let server = Arc::new(opened.server);
    let id = server.inner.shared.next_id() + 1;
    let timeout = Duration::from_secs(60);
    let (ask, seen, send) = FakeAsk::held("a_1");
    let (done, result) = mpsc::channel();
    {
        let server = Arc::clone(&server);
        thread::spawn(move || {
            let answer = call_asking(&server, "ask", timeout, &fakes::CancelToken::new(), &ask);
            done.send(answer).expect("collected");
        });
    }
    assert_eq!(
        read_fifo(
            setup.dir.path().join("held-ask"),
            "the answer subshell starts"
        ),
        "held\n",
    );
    write_fifo(
        setup.dir.path().join("release-ask"),
        "the elicitation is sent",
    );
    Deadline::after(WITHIN).recv_or_fail(&seen, "the first field's ask");
    write_fifo(setup.dir.path().join("release-ask"), "the result is sent");
    await_response(&server.inner.shared, id);
    // The clock advances only now that the ask and the response are both
    // observed: the handler declines, then its own deadline wins over the
    // response delivered meanwhile.
    // No code under test waits on the clock here; the call thread is blocked in the held ask, past its stale park at the deadline (ruling 6179406086).
    setup.fake.advance(timeout);
    send.send(Answered::NoAnswer).expect("the ask ends");
    assert_eq!(
        Deadline::after(WITHIN)
            .recv(&result)
            .unwrap_or_else(|_| { panic!("the timed-out call ends within {WITHIN:?}") }),
        Err(CallError::Timeout),
    );
    barrier(&server);
    let log = std::fs::read_to_string(setup.dir.path().join("requests.log")).expect("requests");
    let answers = elicitation_answers(&log);
    assert_eq!(answers.len(), 1, "one answer to the elicitation: {log}");
    assert!(
        answers[0].contains("\"action\":\"decline\""),
        "the elicitation is declined: {log}",
    );
}

#[test]
fn an_answered_elicitation_with_a_response_already_delivered_succeeds() {
    let setup = Setup::with_tools(&json!([{"name": "ask"}]));
    setup.result("ask", r#"{"content":[{"type":"text","text":"done"}]}"#);
    setup.write("elicit-ask.json", &two_field_elicitation());
    setup.write("result-after-elicit-ask", "");
    mkfifo(&setup, "held-ask");
    mkfifo(&setup, "release-ask");
    let opened = setup.start_expect(Duration::from_secs(5));
    let server = Arc::new(opened.server);
    let (ask, seen) = FakeAsk::scripted(
        "a_1",
        vec![
            Answered::Reply(Answer::Text {
                text: "prod".to_owned(),
            }),
            Answered::Reply(Answer::Confirmed { confirmed: true }),
        ],
    );
    let (done, result) = mpsc::channel();
    {
        let server = Arc::clone(&server);
        thread::spawn(move || {
            let answer = call_asking(
                &server,
                "ask",
                Duration::from_secs(60),
                &fakes::CancelToken::new(),
                &ask,
            );
            done.send(answer).expect("collected");
        });
    }
    assert_eq!(
        read_fifo(
            setup.dir.path().join("held-ask"),
            "the answer subshell starts"
        ),
        "held\n",
    );
    write_fifo(
        setup.dir.path().join("release-ask"),
        "the elicitation is sent",
    );
    // Both asks land before the result can: the response is still gated
    // on the second release read.
    let wait = Deadline::after(WITHIN);
    wait.recv_or_fail(&seen, "the first field's ask");
    wait.recv_or_fail(&seen, "the second field's ask");
    write_fifo(setup.dir.path().join("release-ask"), "the result is sent");
    assert_eq!(
        Deadline::after(WITHIN)
            .recv(&result)
            .unwrap_or_else(|_| { panic!("the answered call returns within {WITHIN:?}") }),
        Ok(json!({"content": [{"type": "text", "text": "done"}]})),
    );
}

#[test]
fn the_last_remover_declines_what_is_still_queued() {
    let setup = Setup::with_tools(&json!([{"name": "ask"}, {"name": "echo"}]));
    setup.result("ask", r#"{"content":[{"type":"text","text":"done"}]}"#);
    setup.result("echo", r#"{"content":[]}"#);
    setup.write("elicit-ask.json", &two_field_elicitation());
    // The fixture writes the elicitation request, then the result: the
    // reader queues the first while the call is registered, and the call
    // sees the second first.
    setup.write("result-after-elicit-ask", "");
    let opened = setup.start_expect(Duration::from_secs(5));
    // Installed after the start: the handshake's own passes run free.
    let (entered_tx, entered) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel();
    super::tests::install_before_pass(move || {
        entered_tx.send(()).expect("the test waits");
        Deadline::after(WITHIN)
            .recv(&go_rx)
            .expect("the test opens the gate");
    });
    let server = Arc::new(opened.server);
    let id = server.inner.shared.next_id() + 1;
    // Refusing: the call must never ask, or the test fails.
    let (ask, seen) = FakeAsk::refusing("a_1");
    let (done, result) = mpsc::channel();
    {
        let server = Arc::clone(&server);
        thread::spawn(move || {
            let answer = call_asking(
                &server,
                "ask",
                Duration::from_secs(60),
                &fakes::CancelToken::new(),
                &ask,
            );
            done.send(answer).expect("collected");
        });
    }
    // The gate holds the registered call before its first view; the
    // result lands while it waits, so the queue-then-remove order the
    // remover must handle happens on every run.
    Deadline::after(WITHIN).recv_or_fail(&entered, "the call registers");
    await_response(&server.inner.shared, id);
    go_tx.send(()).expect("the gate opens");
    assert_eq!(
        Deadline::after(WITHIN)
            .recv(&result)
            .unwrap_or_else(|_| { panic!("the call answers its result within {WITHIN:?}") }),
        Ok(json!({"content": [{"type": "text", "text": "done"}]})),
    );
    barrier(&server);
    let log = std::fs::read_to_string(setup.dir.path().join("requests.log")).expect("requests");
    let answers = elicitation_answers(&log);
    assert_eq!(
        answers.len(),
        1,
        "exactly one answer to the elicitation: {log}"
    );
    assert!(
        answers[0].contains("\"action\":\"decline\""),
        "the remover declines it: {log}",
    );
    assert!(seen.try_recv().is_err(), "nothing was asked");
}

#[test]
fn the_reader_declines_what_arrives_after_the_last_removal() {
    let setup = Setup::with_tools(&json!([{"name": "ask"}]));
    setup.result("ask", r#"{"content":[{"type":"text","text":"done"}]}"#);
    setup.write("elicit-ask.json", &two_field_elicitation());
    mkfifo(&setup, "late-elicit-ask");
    mkfifo(&setup, "seen-el");
    let opened = setup.start_expect(Duration::from_secs(5));
    let server = Arc::new(opened.server);
    // Refusing: the call must never ask, or the test fails.
    let (ask, seen) = FakeAsk::refusing("a_1");
    let (done, result) = mpsc::channel();
    {
        let server = Arc::clone(&server);
        thread::spawn(move || {
            let answer = call_asking(
                &server,
                "ask",
                Duration::from_secs(60),
                &fakes::CancelToken::new(),
                &ask,
            );
            done.send(answer).expect("collected");
        });
    }
    assert_eq!(
        Deadline::after(WITHIN)
            .recv(&result)
            .unwrap_or_else(|_| { panic!("the call answers its result within {WITHIN:?}") }),
        Ok(json!({"content": [{"type": "text", "text": "done"}]})),
    );
    // `remove` has run, so no call is registered. Releasing the
    // subshell sends the elicitation; the reader declines it, and the
    // fixture's `seen-el` acknowledgement proves the decline is logged.
    // An echo barrier cannot serve here: the echo could be written before
    // the reader's decline.
    let seen_path = setup.dir.path().join("seen-el");
    let (seen_done, seen_ack) = mpsc::channel();
    thread::spawn(move || {
        let content = std::fs::read_to_string(&seen_path).expect("seen reads");
        seen_done.send(content).expect("collected");
    });
    write_fifo(
        setup.dir.path().join("late-elicit-ask"),
        "the elicitation is sent",
    );
    Deadline::after(WITHIN).recv_or_fail(&seen_ack, "the reader's decline");
    let log = std::fs::read_to_string(setup.dir.path().join("requests.log")).expect("requests");
    let answers = elicitation_answers(&log);
    assert_eq!(
        answers.len(),
        1,
        "exactly one answer to the elicitation: {log}"
    );
    assert!(
        answers[0].contains("\"action\":\"decline\""),
        "the reader declines it: {log}",
    );
    assert!(seen.try_recv().is_err(), "nothing was asked");
}
