//! `host.ask` (`docs/extensions.md`, "Commands and screens"): the Lua half
//! and the scheduler arm, with a fake clock and an `mpsc` inbox standing in
//! for the loop, which acks each `Resolved` it receives. Every receive has
//! one named wall-clock deadline.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::sync::{Arc, mpsc};
use std::time::Duration;

use contract::RequestId;
use contract::commands::{Reply, ReplyAnswer};
use contract::events::{Interaction, InteractionRequested, ResolvedBy};
use contract::inbox::{Ack, Delivery};
use fakes::clock::FakeClock;

use crate::{Error, LuaExtension};

/// Wall-clock bound on a wait for the extension's thread.
const WAIT: Duration = Duration::from_secs(5);

/// Runs the blocking call `f` on a thread and receives its result with a
/// deadline: calling code that blocks is a wait too (`docs/testing.md`,
/// "Waits and timeouts").
fn within<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || done_tx.send(f()));
    done_rx
        .recv_timeout(WAIT)
        .unwrap_or_else(|_| panic!("the call did not return within {WAIT:?}"))
}

/// `clock` as the extension's clock.
fn clocked(clock: &Arc<FakeClock>) -> Arc<dyn contract::clock::Clock> {
    Arc::clone(clock) as _
}

/// The extension `init`, delivering to `inbox`, answered when `answerable`.
fn extension(
    init: &str,
    clock: &Arc<FakeClock>,
    inbox: mpsc::Sender<Delivery>,
    answerable: bool,
) -> (fakes::TempDir, Arc<LuaExtension>) {
    let dir = fakes::TempDir::new("fiber-ask");
    std::fs::write(dir.path().join("init.lua"), init).unwrap();
    let ext = Arc::new(LuaExtension::new(
        "ext",
        dir.path(),
        "/nonexistent-fiber-home",
        clocked(clock),
    ));
    ext.deliver_to(inbox);
    ext.set_answerable(answerable);
    (dir, ext)
}

fn command(run: &str) -> String {
    format!(
        "fiber.command(\"go\", {{ timeout = 5000, run = function() {run} end }})\n",
        run = run
    )
}

fn interaction(rx: &mpsc::Receiver<Delivery>) -> InteractionRequested {
    let Delivery::Interaction(requested) = rx.recv_timeout(WAIT).expect("the Interaction arrives")
    else {
        panic!("an unexpected delivery arrives");
    };
    requested
}

/// Answers `id` and acks the `Resolved`, so the parked call resumes; the
/// driver's answer follows.
fn answer(
    ext: &Arc<LuaExtension>,
    rx: &mpsc::Receiver<Delivery>,
    reply: Reply,
) -> mpsc::Receiver<bool> {
    let (tx, accepted) = mpsc::channel();
    let ack = Ack(Box::new(move |answer| {
        let _sent = tx.send(answer.is_ok());
    }));
    assert!(
        ext.answer(reply, ack).is_none(),
        "a held ask takes the answer"
    );
    let Delivery::Resolved(_, ack) = rx.recv_timeout(WAIT).expect("the Resolved arrives") else {
        panic!("a Resolved arrives");
    };
    (ack.0)(Ok(None));
    accepted
}

#[test]
fn not_answerable_returns_declined_at_once() {
    let clock = FakeClock::new();
    let (inbox_tx, rx) = mpsc::channel();
    let (_dir, ext) = extension(
        &command("return json.encode(host.ask(\"confirm\", { prompt = \"go?\" }))"),
        &clock,
        inbox_tx,
        false,
    );
    let held = Arc::clone(&ext);
    assert_eq!(
        within(move || held.command("go", "")).unwrap(),
        "{\"declined\":true}"
    );
    assert!(rx.try_recv().is_err(), "nothing reaches the inbox");
}

#[test]
fn an_answerable_confirm_round_trips() {
    let clock = FakeClock::new();
    let (inbox_tx, rx) = mpsc::channel();
    let (_dir, ext) = extension(
        &command(
            "local answer = host.ask(\"confirm\", { prompt = \"go?\" }) return tostring(answer.confirmed)",
        ),
        &clock,
        inbox_tx,
        true,
    );
    let held = Arc::clone(&ext);
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _sent = done_tx.send(held.command("go", ""));
    });
    let requested = interaction(&rx);
    assert_eq!(requested.extension.as_deref(), Some("ext"));
    assert!(
        requested.request_id.0.starts_with("r_"),
        "the ask mints its request id"
    );
    let accepted = answer(
        &ext,
        &rx,
        Reply {
            request_id: requested.request_id,
            answer: ReplyAnswer::Confirmed { confirmed: true },
        },
    );
    assert!(
        accepted.recv_timeout(WAIT).expect("the reply is answered"),
        "the reply is accepted once the line is in the log"
    );
    assert_eq!(
        done_rx
            .recv_timeout(WAIT)
            .expect("the command returns")
            .unwrap(),
        "true"
    );
}

#[test]
fn each_kinds_answer_table_round_trips_to_lua() {
    let cases = [
        (
            "select",
            "host.ask(\"select\", { prompt = \"go?\", options = {{ label = \"a\" }, { label = \"b\" }} })",
            ReplyAnswer::Labels {
                labels: vec!["b".into()],
            },
            "answer.labels[1]",
            "b",
        ),
        (
            "multi_select",
            "host.ask(\"multi_select\", { prompt = \"go?\", options = {{ label = \"a\" }, { label = \"b\" }} })",
            ReplyAnswer::Labels { labels: vec![] },
            "tostring(#answer.labels)",
            "0",
        ),
        (
            "text_input",
            "host.ask(\"text_input\", { prompt = \"go?\" })",
            ReplyAnswer::Text { text: "hi".into() },
            "answer.text",
            "hi",
        ),
    ];
    for (kind, ask, answered, read, want) in cases {
        let clock = FakeClock::new();
        let (inbox_tx, rx) = mpsc::channel();
        let (_dir, ext) = extension(
            &command(&format!("local answer = {ask} return {read}")),
            &clock,
            inbox_tx,
            true,
        );
        let held = Arc::clone(&ext);
        let (done_tx, done_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _sent = done_tx.send(held.command("go", ""));
        });
        let requested = interaction(&rx);
        let accepted = answer(
            &ext,
            &rx,
            Reply {
                request_id: requested.request_id,
                answer: answered,
            },
        );
        assert!(accepted.recv_timeout(WAIT).is_ok());
        assert_eq!(
            done_rx
                .recv_timeout(WAIT)
                .expect("the command returns")
                .unwrap(),
            want,
            "{kind} round-trips"
        );
    }
}

#[test]
fn a_form_answer_with_a_note_round_trips() {
    let clock = FakeClock::new();
    let (inbox_tx, rx) = mpsc::channel();
    let (_dir, ext) = extension(
        &command(
            "local answer = host.ask(\"form\", { fields = {\
             { header = \"A\", question = \"First?\", options = {{ label = \"a\" }, { label = \"b\" }} },\
             { header = \"B\", question = \"Second?\", options = {{ label = \"c\" }} } } })\
             return answer.answers[1].labels[1] .. \"/\" .. (answer.answers[1].text or \"-\") .. \"/\" .. \
             tostring(answer.answers[2].skipped) .. \"/\" .. (answer.note or \"-\")",
        ),
        &clock,
        inbox_tx,
        true,
    );
    let held = Arc::clone(&ext);
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _sent = done_tx.send(held.command("go", ""));
    });
    let requested = interaction(&rx);
    let accepted = answer(
        &ext,
        &rx,
        Reply {
            request_id: requested.request_id,
            answer: ReplyAnswer::Form {
                answers: vec![
                    contract::commands::SentFormAnswer::Answered {
                        labels: vec!["a".into()],
                        text: Some("extra".into()),
                    },
                    contract::commands::SentFormAnswer::Skipped {
                        skipped: contract::shapes::True,
                    },
                ],
                note: Some("n".into()),
            },
        },
    );
    assert!(accepted.recv_timeout(WAIT).is_ok());
    assert_eq!(
        done_rx
            .recv_timeout(WAIT)
            .expect("the command returns")
            .unwrap(),
        "a/extra/true/n"
    );
}

#[test]
fn a_timed_out_ask_is_declined_by_fiber_and_late_answers_hand_back() {
    let clock = FakeClock::new();
    let (inbox_tx, rx) = mpsc::channel();
    let dir = fakes::TempDir::new("fiber-ask");
    std::fs::write(
        dir.path().join("init.lua"),
        "fiber.command(\"go\", { timeout = 200, run = function() return host.ask(\"confirm\", { prompt = \"go?\" }) end })\n",
    )
    .unwrap();
    let ext = Arc::new(LuaExtension::new(
        "ext",
        dir.path(),
        "/nonexistent-fiber-home",
        clocked(&clock),
    ));
    ext.deliver_to(inbox_tx);
    ext.set_answerable(true);
    let held = Arc::clone(&ext);
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _sent = done_tx.send(held.command("go", ""));
    });
    let requested = interaction(&rx);
    clock.advance(Duration::from_millis(1000));
    let Delivery::Resolved(resolved, _) = rx.recv_timeout(WAIT).expect("the decline arrives")
    else {
        panic!("a decline arrives");
    };
    assert_eq!(resolved.request_id, requested.request_id);
    assert_eq!(resolved.by, ResolvedBy::Fiber);
    let Err(Error::Timeout { .. }) = done_rx.recv_timeout(WAIT).expect("the command ends") else {
        panic!("the command fails timed out");
    };
    let (tx, _) = mpsc::channel();
    let ack = Ack(Box::new(move |_| {
        let _sent = tx.send(());
    }));
    assert!(
        ext.answer(
            Reply {
                request_id: requested.request_id,
                answer: ReplyAnswer::Confirmed { confirmed: true },
            },
            ack
        )
        .is_some(),
        "a late answer hands back, for the loop's stale_request"
    );
}

#[test]
fn a_timer_that_asks_and_expires_is_declined_the_same_way() {
    let clock = FakeClock::new();
    let (inbox_tx, rx) = mpsc::channel();
    let (_dir, ext) = extension(
        "fiber.command(\"go\", { timeout = 5000, run = function()\n\
         host.after(50, function() host.ask(\"confirm\", { prompt = \"late?\" }) end, { timeout = 100 })\n\
         return \"set\" end })\n",
        &clock,
        inbox_tx,
        true,
    );
    let held = Arc::clone(&ext);
    assert_eq!(within(move || held.command("go", "")).unwrap(), "set");
    clock.advance(Duration::from_millis(60));
    let requested = interaction(&rx);
    clock.advance(Duration::from_millis(200));
    let Delivery::Resolved(resolved, _) = rx.recv_timeout(WAIT).expect("the decline arrives")
    else {
        panic!("a decline arrives");
    };
    assert_eq!(resolved.request_id, requested.request_id);
    assert_eq!(resolved.by, ResolvedBy::Fiber);
}

#[test]
fn two_commands_asking_in_sequence_keep_the_stream() {
    let clock = FakeClock::new();
    let (inbox_tx, rx) = mpsc::channel();
    let dir = fakes::TempDir::new("fiber-ask");
    std::fs::write(
        dir.path().join("init.lua"),
        "fiber.command(\"first\", { timeout = 5000, run = function()\n\
         local answer = host.ask(\"confirm\", { prompt = \"one?\" })\n\
         return \"first-\" .. tostring(answer.confirmed) end })\n\
         fiber.command(\"second\", { timeout = 5000, run = function()\n\
         local answer = host.ask(\"confirm\", { prompt = \"two?\" })\n\
         return \"second-\" .. tostring(answer.confirmed) end })\n",
    )
    .unwrap();
    let ext = Arc::new(LuaExtension::new(
        "ext",
        dir.path(),
        "/nonexistent-fiber-home",
        clocked(&clock),
    ));
    ext.deliver_to(inbox_tx);
    ext.set_answerable(true);
    let (first_tx, first_rx) = mpsc::channel();
    let (second_tx, second_rx) = mpsc::channel();
    let first_ext = Arc::clone(&ext);
    std::thread::spawn(move || {
        let _sent = first_tx.send(first_ext.command("first", ""));
    });
    let first = interaction(&rx);
    assert!(
        matches!(&first.interaction, Interaction::Confirm { prompt } if prompt == "one?"),
        "the first ask's request is observed before the second starts"
    );
    let second_ext = Arc::clone(&ext);
    std::thread::spawn(move || {
        let _sent = second_tx.send(second_ext.command("second", ""));
    });
    assert!(
        rx.recv_timeout(Duration::from_millis(200)).is_err(),
        "the second command does not start while the first is parked"
    );
    let accepted = answer(
        &ext,
        &rx,
        Reply {
            request_id: first.request_id,
            answer: ReplyAnswer::Confirmed { confirmed: true },
        },
    );
    assert!(accepted.recv_timeout(WAIT).is_ok());
    assert_eq!(
        first_rx
            .recv_timeout(WAIT)
            .expect("the first returns")
            .unwrap(),
        "first-true"
    );
    let second = interaction(&rx);
    let accepted = answer(
        &ext,
        &rx,
        Reply {
            request_id: second.request_id,
            answer: ReplyAnswer::Confirmed { confirmed: false },
        },
    );
    assert!(accepted.recv_timeout(WAIT).is_ok());
    assert_eq!(
        second_rx
            .recv_timeout(WAIT)
            .expect("the second returns")
            .unwrap(),
        "second-false"
    );
}

#[test]
fn disposing_with_an_ask_held_routes_nothing() {
    let clock = FakeClock::new();
    let (inbox_tx, rx) = mpsc::channel();
    let (_dir, ext) = extension(
        &command("return host.ask(\"confirm\", { prompt = \"go?\" })"),
        &clock,
        inbox_tx,
        true,
    );
    let held = Arc::clone(&ext);
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _sent = done_tx.send(held.command("go", ""));
    });
    let _ = interaction(&rx);
    ext.dispose();
    assert!(
        done_rx
            .recv_timeout(WAIT)
            .expect("the command ends")
            .is_err(),
        "the parked command ends with the dispose"
    );
    assert!(rx.try_recv().is_err(), "the decline is dropped");
}

#[test]
fn sealing_declines_new_asks_and_hands_answers_back() {
    let clock = FakeClock::new();
    let (inbox_tx, rx) = mpsc::channel();
    let (_dir, ext) = extension(
        &command("return json.encode(host.ask(\"confirm\", { prompt = \"go?\" }))"),
        &clock,
        inbox_tx,
        true,
    );
    ext.seal();
    let held = Arc::clone(&ext);
    assert_eq!(
        within(move || held.command("go", "")).unwrap(),
        "{\"declined\":true}"
    );
    assert!(rx.try_recv().is_err(), "a sealed ask writes nothing");
    let (tx, _) = mpsc::channel();
    let ack = Ack(Box::new(move |_| {
        let _sent = tx.send(());
    }));
    assert!(
        ext.answer(
            Reply {
                request_id: RequestId("r_held".into()),
                answer: ReplyAnswer::Confirmed { confirmed: true },
            },
            ack
        )
        .is_some(),
        "a sealed answer hands back"
    );
}

#[test]
fn bad_specs_raise_strings_and_nothing_reaches_the_inbox() {
    let clock = FakeClock::new();
    let (inbox_tx, rx) = mpsc::channel();
    let (_dir, ext) = extension(
        "fiber.command(\"go\", { timeout = 5000, run = function()\n\
         local function trial(kind, spec)\n\
         local ok, err = pcall(host.ask, kind, spec)\n\
         return tostring(ok) .. \":\" .. type(err) .. \":\" .. tostring(err)\n\
         end\n\
         return trial(\"approve\", { prompt = \"x\" }) .. \"|\" ..\n\
         trial(\"confirm\", { prompt = \"x\", options = {} }) .. \"|\" ..\n\
         trial(\"select\", { prompt = \"x\", options = {} }) .. \"|\" ..\n\
         trial(\"form\", { fields = {} }) .. \"|\" ..\n\
         trial(\"select\", { prompt = \"x\", options = {{ label = \"a\" }, { label = \"a\" }} }) .. \"|\" ..\n\
         trial(\"confirm\", {})\n\
         end })\n",
        &clock,
        inbox_tx,
        true,
    );
    let held = Arc::clone(&ext);
    let got = within(move || held.command("go", "")).unwrap();
    for (part, key) in got
        .split('|')
        .zip(["approve", "options", "option", "field", "\"a\"", "prompt"])
    {
        assert!(
            part.starts_with("false:string:host.ask: "),
            "a bad spec raises a string: {part}"
        );
        assert!(part.contains(key), "the error names {key:?}: {part}");
    }
    assert!(rx.try_recv().is_err(), "nothing reaches the inbox");
}

#[test]
fn host_ask_in_init_lua_raises_the_entry_string() {
    let clock = FakeClock::new();
    let dir = fakes::TempDir::new("fiber-ask");
    std::fs::write(
        dir.path().join("init.lua"),
        "host.ask(\"confirm\", { prompt = \"go?\" })\n\
         fiber.command(\"go\", { timeout = 5000, run = function() end })\n",
    )
    .unwrap();
    let ext = LuaExtension::new(
        "ext",
        dir.path(),
        "/nonexistent-fiber-home",
        clocked(&clock),
    );
    let ext = Arc::new(ext);
    let held = Arc::clone(&ext);
    let Err(Error::Lua { message, .. }) = within(move || held.commands()) else {
        panic!("the entry script fails to load");
    };
    assert_eq!(message, "host.ask: not available while init.lua runs");
}
