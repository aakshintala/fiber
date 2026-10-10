//! `host.ask`'s spec table (`docs/extensions.md`, "Commands and screens"):
//! each kind reads to its [`Interaction`], and each bad spec raises a string
//! naming the key or label. The fit table is `contract`'s.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use contract::events::Interaction;
use contract::shapes::{Choice, Question};
use serde_json::json;

use super::*;

fn choice(label: &str) -> Choice {
    Choice {
        label: label.into(),
        description: None,
    }
}

fn field(multi: bool) -> Question {
    Question {
        header: "Model".into(),
        question: "Which?".into(),
        options: vec![choice("a"), choice("b")],
        multi_select: multi.then_some(true),
    }
}

fn asked(kind: &str) -> Interaction {
    match kind {
        "confirm" => Interaction::Confirm {
            prompt: "go?".into(),
        },
        "select" => Interaction::Select {
            prompt: "go?".into(),
            options: vec![
                choice("a"),
                Choice {
                    label: "b".into(),
                    description: Some("d".into()),
                },
            ],
        },
        "multi_select" => Interaction::MultiSelect {
            prompt: "go?".into(),
            options: vec![choice("a"), choice("b")],
        },
        "text_input" => Interaction::TextInput {
            prompt: "go?".into(),
        },
        "form" => Interaction::Form {
            fields: vec![field(false)],
        },
        _ => unreachable!(),
    }
}

#[test]
fn each_kind_with_a_valid_spec_reads_to_its_interaction() {
    let cases = [
        ("confirm", json!({"prompt": "go?"})),
        (
            "select",
            json!({"prompt": "go?", "options": [{"label": "a"}, {"label": "b", "description": "d"}]}),
        ),
        (
            "multi_select",
            json!({"prompt": "go?", "options": [{"label": "a"}, {"label": "b"}]}),
        ),
        ("text_input", json!({"prompt": "go?"})),
        (
            "form",
            json!({"fields": [{"header": "Model", "question": "Which?", "options": [{"label": "a"}, {"label": "b"}]}]}),
        ),
    ];
    for (kind, spec) in cases {
        assert_eq!(interaction(kind, &spec).unwrap(), asked(kind));
    }
}

#[test]
fn each_allowed_key_set_with_one_extra_key_raises_naming_it() {
    let cases = [
        (
            "confirm",
            json!({"prompt": "go?", "options": []}),
            "options",
        ),
        (
            "select",
            json!({"prompt": "go?", "options": [{"label": "a"}], "fields": []}),
            "fields",
        ),
        (
            "form",
            json!({"fields": [{"header": "h", "question": "q"}], "prompt": "go?"}),
            "prompt",
        ),
        (
            "select",
            json!({"prompt": "go?", "options": [{"label": "a", "value": "a"}]}),
            "value",
        ),
        (
            "form",
            json!({"fields": [{"header": "h", "question": "q", "multi_select": true}]}),
            "multi_select",
        ),
    ];
    for (kind, spec, key) in cases {
        let error = interaction(kind, &spec).expect_err("an extra key is refused");
        assert!(error.contains(key), "the error names {key:?}: {error}");
    }
}

#[test]
fn a_form_field_option_takes_only_label_and_description() {
    let field_with = |option: serde_json::Value| serde_json::json!({"header": "h", "question": "q", "options": [option]});
    // Each operand of the `key != "label" && key != "description"`
    // check: an option with only `label`, only `description`, or both
    // is allowed.
    let allowed = [
        json!({"label": "a"}),
        json!({"description": "d"}),
        json!({"label": "a", "description": "d"}),
    ];
    for option in &allowed {
        assert!(
            check_field(&field_with(option.clone()), 0).is_ok(),
            "option {option} is allowed"
        );
    }
    let refused = [
        (json!({"label": "a", "value": "v"}), "value"),
        (json!({"description": "d", "name": "n"}), "name"),
        (json!({"value": "v"}), "value"),
    ];
    for (option, key) in refused {
        let error =
            check_field(&field_with(option), 0).expect_err("an extra option key is refused");
        assert!(error.contains(key), "the error names {key:?}: {error}");
    }
    // The spec level wires the same check: a form field option with an
    // extra key is refused naming it, while a `label`-only option reads.
    let error = interaction(
        "form",
        &json!({"fields": [{"header": "h", "question": "q", "options": [{"label": "a", "value": "v"}]}]}),
    )
    .expect_err("a form field option with an extra key is refused");
    assert!(error.contains("value"), "the error names the key: {error}");
    assert!(
        interaction(
            "form",
            &json!({"fields": [{"header": "h", "question": "q", "options": [{"label": "a"}]}]}),
        )
        .is_ok(),
        "a `label`-only form field option reads"
    );
}

#[test]
fn a_missing_prompt_options_or_fields_raises_naming_it() {
    let cases = [
        ("confirm", json!({}), "prompt"),
        ("text_input", json!({}), "prompt"),
        ("select", json!({"prompt": "go?"}), "options"),
        ("multi_select", json!({"prompt": "go?"}), "options"),
        ("form", json!({}), "fields"),
    ];
    for (kind, spec, key) in cases {
        let error = interaction(kind, &spec).expect_err("a missing key is refused");
        assert!(error.contains(key), "the error names {key:?}: {error}");
    }
}

#[test]
fn a_non_string_prompt_raises_naming_it() {
    let error = interaction("confirm", &json!({"prompt": 1})).expect_err("a number is refused");
    assert!(
        error.contains("prompt"),
        "the error names `prompt`: {error}"
    );
}

#[test]
fn an_unknown_kind_raises_naming_it() {
    let error = interaction("approve", &json!({"prompt": "go?"})).expect_err("the kind is refused");
    assert!(
        error.contains("approve"),
        "the error names the kind: {error}"
    );
}

#[test]
fn a_spec_that_is_not_a_table_raises() {
    interaction("confirm", &json!("go?")).expect_err("a string is refused");
}

#[test]
fn empty_options_and_empty_fields_raise() {
    for kind in ["select", "multi_select"] {
        let error = interaction(kind, &json!({"prompt": "go?", "options": []}))
            .expect_err("no option is refused");
        assert!(error.contains("option"), "the error names options: {error}");
    }
    let error = interaction("form", &json!({"fields": []})).expect_err("no field is refused");
    assert!(error.contains("field"), "the error names fields: {error}");
}

#[test]
fn a_duplicate_label_raises_naming_it() {
    let cases = [
        (
            "select",
            json!({"prompt": "go?", "options": [{"label": "a"}, {"label": "a"}]}),
        ),
        (
            "multi_select",
            json!({"prompt": "go?", "options": [{"label": "a"}, {"label": "a"}]}),
        ),
        (
            "form",
            json!({"fields": [{"header": "h", "question": "q", "options": [{"label": "a"}, {"label": "a"}]}]}),
        ),
    ];
    for (kind, spec) in cases {
        let error = interaction(kind, &spec).expect_err("a duplicate label is refused");
        assert!(
            error.contains("\"a\""),
            "the error names the label: {error}"
        );
    }
}

// The registry and routing (`Shared::raise`, `Hub::answer`,
// `Shared::decline` and `Shared::stop`): one `Resolved` per ask, removed
// and routed under one hub lock, and nothing dropped while it is held.

use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::Duration;

use contract::events::{InteractionRequested, InteractionResolved};
use contract::inbox::{Ack, Delivery};
use contract::{ErrorCode, RequestId};
use fakes::Deadline;
use fakes::clock::FakeClock;

/// Wall-clock bound on a wait for a delivery.
const WAIT: Duration = Duration::from_secs(5);

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn requested(id: &str) -> InteractionRequested {
    InteractionRequested {
        request_id: RequestId(id.into()),
        interaction: asked("confirm"),
        action_ids: None,
        extension: Some("ext".into()),
        resumes: false,
    }
}

/// Registers `id` for the parked call `call`, dropping what routing could
/// not send only after the hub lock is released.
fn raise(hub: &Hub, call: u64, id: &str) {
    let unsent = hub.lock().raise(call, requested(id));
    drop(unsent);
}

fn decline(hub: &Hub, id: &str) {
    let unsent = hub.lock().decline(&RequestId(id.into()));
    drop(unsent);
}

fn reply(id: &str, answer: contract::commands::ReplyAnswer) -> contract::commands::Reply {
    contract::commands::Reply {
        request_id: RequestId(id.into()),
        answer,
    }
}

fn answered() -> (Ack, mpsc::Receiver<bool>) {
    let (tx, rx) = mpsc::channel();
    (
        Ack(Box::new(move |answer| {
            let _sent = tx.send(answer.is_ok());
        })),
        rx,
    )
}

#[track_caller]
fn take_interaction(rx: &mpsc::Receiver<Delivery>, wait: &Deadline) -> InteractionRequested {
    let Delivery::Interaction(requested) = wait.recv(rx).expect("the Interaction arrives") else {
        panic!("an unexpected delivery arrives");
    };
    requested
}

#[track_caller]
fn resolved(rx: &mpsc::Receiver<Delivery>, wait: &Deadline) -> (InteractionResolved, Ack) {
    let Delivery::Resolved(resolved, ack) = wait.recv(rx).expect("the Resolved arrives") else {
        panic!("a Resolved arrives");
    };
    (resolved, ack)
}

fn hub_with_inbox() -> (Arc<Hub>, mpsc::Receiver<Delivery>) {
    let hub = Hub::new(FakeClock::new());
    // Ready, as a loaded extension is: `deliver` takes no reply else.
    hub.lock().phase = crate::lua::Phase::Ready(crate::lua::CallbackTimeouts::default());
    let (tx, rx) = mpsc::channel();
    hub.set_inbox(tx);
    (hub, rx)
}

#[test]
fn raise_routes_the_interaction_and_holds_the_request() {
    let (hub, rx) = hub_with_inbox();
    raise(&hub, 7, "r_1");
    let requested = take_interaction(&rx, &Deadline::after(WAIT));
    assert_eq!(requested.request_id, RequestId("r_1".into()));
    assert_eq!(requested.extension.as_deref(), Some("ext"));
    assert!(hub.lock().asks.contains_key(&RequestId("r_1".into())));
}

#[test]
fn a_fitting_answer_resolves_by_person_then_resumes_the_call() {
    let (hub, rx) = hub_with_inbox();
    raise(&hub, 7, "r_1");
    let _ = take_interaction(&rx, &Deadline::after(WAIT));
    let order = Arc::new(Mutex::new(Vec::new()));
    let recording = Arc::clone(&order);
    let answering = Arc::clone(&hub);
    let ack = Ack(Box::new(move |answer| {
        assert!(answer.is_ok(), "the reply is accepted");
        // The driver's reply is answered before the call resumes.
        assert!(
            answering.lock().replies.iter().all(|(id, _)| *id != 7),
            "command_accepted comes before the resume"
        );
        recording.lock().unwrap().push("ack");
    }));
    let answer = reply(
        "r_1",
        contract::commands::ReplyAnswer::Confirmed { confirmed: true },
    );
    assert!(
        hub.answer(answer, ack).is_none(),
        "a fitting answer is taken"
    );
    assert!(
        !hub.lock().asks.contains_key(&RequestId("r_1".into())),
        "the ask leaves the registry"
    );
    let (resolved, ack) = resolved(&rx, &Deadline::after(WAIT));
    assert_eq!(resolved.request_id, RequestId("r_1".into()));
    assert_eq!(resolved.by, contract::events::ResolvedBy::Person);
    assert_eq!(
        resolved.answer,
        contract::events::Answer::Confirmed { confirmed: true }
    );
    (ack.0)(Ok(None));
    let delivered = hub
        .lock()
        .replies
        .iter()
        .any(|(id, reply)| *id == 7 && matches!(reply, crate::host::Reply::Ask(_)));
    if delivered {
        order.lock().unwrap().push("delivered");
    }
    assert_eq!(lock(&order).as_slice(), ["ack", "delivered"]);
}

#[test]
fn an_unfit_answer_is_rejected_and_stays_pending() {
    let (hub, rx) = hub_with_inbox();
    raise(&hub, 7, "r_1");
    let _ = take_interaction(&rx, &Deadline::after(WAIT));
    let (ack, frequently) = answered();
    let answer = reply(
        "r_1",
        contract::commands::ReplyAnswer::Text { text: "x".into() },
    );
    assert!(
        hub.answer(answer, ack).is_none(),
        "an unfit answer is taken, not handed back"
    );
    assert!(
        !Deadline::after(WAIT)
            .recv(&frequently)
            .expect("the rejection arrives"),
        "the reply is rejected"
    );
    assert!(
        hub.lock().asks.contains_key(&RequestId("r_1".into())),
        "the ask stays held"
    );
    assert!(rx.try_recv().is_err(), "nothing is routed");
}

#[test]
fn an_unfit_answer_rejects_with_the_loops_sentence() {
    let (hub, rx) = hub_with_inbox();
    raise(&hub, 7, "r_1");
    let _ = take_interaction(&rx, &Deadline::after(WAIT));
    let (tx, rejected) = mpsc::channel();
    let ack = Ack(Box::new(move |answer| {
        let _sent = tx.send(answer);
    }));
    let answer = reply(
        "r_1",
        contract::commands::ReplyAnswer::Text { text: "x".into() },
    );
    assert!(hub.answer(answer, ack).is_none());
    match Deadline::after(WAIT)
        .recv(&rejected)
        .expect("the rejection arrives")
    {
        Err(rejection) => {
            assert_eq!(rejection.code, ErrorCode::InvalidArguments);
            assert_eq!(
                rejection.message,
                "That answer does not fit the pending request."
            );
        }
        ok => panic!("the reply is rejected, got {ok:?}"),
    }
}

#[test]
fn an_answer_for_an_id_not_held_hands_back() {
    let (hub, rx) = hub_with_inbox();
    raise(&hub, 7, "r_1");
    let _ = take_interaction(&rx, &Deadline::after(WAIT));
    let (ack, _) = answered();
    let answer = reply(
        "r_other",
        contract::commands::ReplyAnswer::Confirmed { confirmed: true },
    );
    assert!(
        hub.answer(answer, ack).is_some(),
        "an unknown id is handed back for the loop"
    );
    assert!(rx.try_recv().is_err(), "nothing is routed");
}

#[test]
fn decline_after_a_fitting_answer_routes_nothing() {
    let (hub, rx) = hub_with_inbox();
    raise(&hub, 7, "r_1");
    let _ = take_interaction(&rx, &Deadline::after(WAIT));
    let (ack, _) = answered();
    let answer = reply(
        "r_1",
        contract::commands::ReplyAnswer::Confirmed { confirmed: true },
    );
    assert!(hub.answer(answer, ack).is_none());
    let (_, ack) = resolved(&rx, &Deadline::after(WAIT));
    (ack.0)(Ok(None));
    decline(&hub, "r_1");
    assert!(rx.try_recv().is_err(), "the loser routes nothing");
}

#[test]
fn an_answer_after_a_decline_hands_back() {
    let (hub, rx) = hub_with_inbox();
    raise(&hub, 7, "r_1");
    let _ = take_interaction(&rx, &Deadline::after(WAIT));
    decline(&hub, "r_1");
    let (_, ack) = resolved(&rx, &Deadline::after(WAIT));
    (ack.0)(Ok(None));
    let (back, _) = answered();
    let answer = reply(
        "r_1",
        contract::commands::ReplyAnswer::Confirmed { confirmed: true },
    );
    assert!(
        hub.answer(answer, back).is_some(),
        "a declined ask is handed back, for the loop's stale_request"
    );
}

#[test]
fn stop_declines_every_held_ask_once() {
    for held in [1, 2] {
        let (hub, rx) = hub_with_inbox();
        for n in 0..held {
            raise(&hub, n, &format!("r_{n}"));
        }
        // One deadline for the whole wait: the loop drains one stream.
        let wait = Deadline::after(WAIT);
        for _ in 0..held {
            let _ = take_interaction(&rx, &wait);
        }
        let unsent = hub.lock().stop(super::super::hub::StopReason::Abandoned {
            extension: "ext".into(),
            callback: "go".into(),
        });
        drop(unsent);
        assert!(
            hub.lock().asks.is_empty(),
            "no ask stays held past the stop"
        );
        // One deadline for the whole wait: the loop drains one stream.
        let wait = Deadline::after(WAIT);
        for _ in 0..held {
            let (resolved, ack) = resolved(&rx, &wait);
            assert_eq!(resolved.by, contract::events::ResolvedBy::Fiber);
            assert_eq!(
                resolved.answer,
                contract::events::Answer::Declined {
                    declined: contract::shapes::True
                }
            );
            (ack.0)(Ok(None));
        }
        assert!(rx.try_recv().is_err(), "one Resolved per held ask");
    }
}

#[test]
fn after_seal_raise_routes_nothing_answer_hands_back_and_decline_routes_nothing() {
    let (hub, rx) = hub_with_inbox();
    hub.seal();
    raise(&hub, 7, "r_1");
    assert!(rx.try_recv().is_err(), "nothing is routed after seal");
    let (ack, _) = answered();
    let answer = reply(
        "r_1",
        contract::commands::ReplyAnswer::Confirmed { confirmed: true },
    );
    assert!(
        hub.answer(answer, ack).is_some(),
        "a sealed ask is handed back; the loop has gone"
    );
    decline(&hub, "r_1");
    assert!(rx.try_recv().is_err(), "a sealed decline routes nothing");
}

#[test]
fn without_an_inbox_both_lines_buffer_and_flush_in_order() {
    let hub = Hub::new(FakeClock::new());
    raise(&hub, 7, "r_1");
    let (ack, _) = answered();
    let answer = reply(
        "r_1",
        contract::commands::ReplyAnswer::Confirmed { confirmed: true },
    );
    assert!(hub.answer(answer, ack).is_none());
    let (tx, rx) = mpsc::channel();
    hub.set_inbox(tx);
    // One deadline for the whole wait: the pair drains the flushed buffer.
    let wait = Deadline::after(WAIT);
    let requested = take_interaction(&rx, &wait);
    assert_eq!(requested.request_id, RequestId("r_1".into()));
    let (resolved, ack) = resolved(&rx, &wait);
    assert_eq!(resolved.request_id, RequestId("r_1".into()));
    (ack.0)(Ok(None));
}

#[test]
fn a_buffer_sealed_before_its_inbox_reaches_no_inbox() {
    let hub = Hub::new(FakeClock::new());
    raise(&hub, 7, "r_1");
    hub.seal();
    let (tx, rx) = mpsc::channel();
    hub.set_inbox(tx);
    assert!(rx.try_recv().is_err(), "nothing flushes after seal");
}

/// A door ack whose drop answers, as the doors guard does: dropping it
/// uncalled answers `closing`, which re-enters the hub the way a
/// same-extension `host.drive("reply", ..)` would.
struct DropDeliver {
    hub: Arc<Hub>,
    id: u64,
    closed: mpsc::Sender<&'static str>,
}

impl Drop for DropDeliver {
    fn drop(&mut self) {
        self.hub
            .deliver(self.id, crate::host::Reply::Drive(Ok(None)));
        let _sent = self.closed.send("closing");
    }
}

#[test]
fn a_disconnected_inbox_drops_a_fitting_answer_after_the_lock() {
    fakes::within("a fitting answer past a disconnect", WAIT, || {
        let hub = Hub::new(FakeClock::new());
        let (tx, rx) = mpsc::channel();
        hub.set_inbox(tx);
        drop(rx);
        raise(&hub, 7, "r_1");
        let (closed_tx, closed_rx) = mpsc::channel();
        let guard = DropDeliver {
            hub: Arc::clone(&hub),
            id: 7,
            closed: closed_tx,
        };
        let ack = Ack(Box::new(move |_| {
            let _guard = guard;
        }));
        let answer = reply(
            "r_1",
            contract::commands::ReplyAnswer::Confirmed { confirmed: true },
        );
        assert!(hub.answer(answer, ack).is_none());
        assert_eq!(
            Deadline::after(WAIT)
                .recv(&closed_rx)
                .expect("the drop answers"),
            "closing",
            "the dropped Resolved is dropped after the lock"
        );
    });
}

#[test]
fn a_disconnected_flush_drops_the_buffer_after_the_lock() {
    fakes::within("a flush past a disconnect", WAIT, || {
        let hub = Hub::new(FakeClock::new());
        raise(&hub, 7, "r_1");
        let (closed_tx, closed_rx) = mpsc::channel();
        let guard = DropDeliver {
            hub: Arc::clone(&hub),
            id: 7,
            closed: closed_tx,
        };
        let ack = Ack(Box::new(move |_| {
            let _guard = guard;
        }));
        let answer = reply(
            "r_1",
            contract::commands::ReplyAnswer::Confirmed { confirmed: true },
        );
        assert!(hub.answer(answer, ack).is_none());
        let (tx, rx) = mpsc::channel();
        drop(rx);
        hub.set_inbox(tx);
        assert_eq!(
            Deadline::after(WAIT)
                .recv(&closed_rx)
                .expect("the drop answers"),
            "closing",
            "the unsent buffer is dropped after the lock"
        );
    });
}

#[test]
fn dispose_with_a_held_ask_routes_nothing() {
    let (hub, rx) = hub_with_inbox();
    raise(&hub, 7, "r_1");
    let _ = take_interaction(&rx, &Deadline::after(WAIT));
    hub.dispose("ext");
    assert!(rx.try_recv().is_err(), "the decline is dropped");
    let (ack, _) = answered();
    assert!(
        hub.answer(
            reply(
                "r_1",
                contract::commands::ReplyAnswer::Confirmed { confirmed: true }
            ),
            ack
        )
        .is_some(),
        "a disposed ask hands back"
    );
}
