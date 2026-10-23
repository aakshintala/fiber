//! A command's runner, cancellation and synchronous serving-window edges.

use super::*;
use contract::tool::{Cancel, InnerCalls};
use fakes::Deadline;
use fakes::MUST_SUCCEED_WITHIN;
use std::sync::mpsc;

fn completion() -> Box<ToolCallCompleted> {
    Box::new(ToolCallCompleted {
        status: CallStatus::Completed,
        reason: None,
        provider_item: None,
        error: None,
        process: None,
        details: None,
        changes: None,
        control: None,
        artifact: None,
        kept: None,
        changed_by: None,
        content: vec![contract::shapes::ContentPart::Text {
            text: "answer".into(),
        }],
        hosts_surfaced: Vec::new(),
        loaded: None,
    })
}

fn runner(hub: &Arc<Hub>, call: u64) -> Arc<CommandCalls> {
    let runner = Arc::new(CommandCalls {
        hub: Arc::downgrade(hub),
        command: contract::CommandId(format!("c_{call}")),
        call,
        ended: Arc::new(Ended::default()),
        pause: Mutex::default(),
    });
    hub.lock().runners.insert(call, runner.clone());
    runner
}

fn start(runner: &Arc<CommandCalls>) -> mpsc::Receiver<Box<ToolCallCompleted>> {
    let runner = runner.clone();
    let (done, result) = mpsc::channel();
    std::thread::spawn(move || {
        let _sent = done.send(
            runner
                .run("ext", "read", serde_json::json!({"path":"x"}))
                .unwrap(),
        );
    });
    result
}

fn received_request(
    rx: &mpsc::Receiver<contract::inbox::Delivery>,
) -> contract::inbox::InnerCallRequest {
    let contract::inbox::Delivery::InnerCall(request) =
        Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(rx, "command inner request")
    else {
        panic!("expected an inner request");
    };
    request
}

#[test]
fn the_runner_sends_the_command_anchor_and_returns_the_answer() {
    let hub = Hub::new(fakes::clock::FakeClock::new());
    let (send, rx) = mpsc::channel();
    hub.set_inbox(send);
    let runner = runner(&hub, 1);
    let result = start(&runner);
    let request = received_request(&rx);
    assert_eq!(request.extension, "ext");
    assert_eq!(request.command.0, "c_1");
    assert_eq!(request.name, "read");
    assert_eq!(request.arguments, serde_json::json!({"path":"x"}));
    assert_eq!(hub.lock().serving, None);
    (request.taken)();
    assert_eq!(hub.lock().serving, Some(1));
    (request.answer)(completion());
    assert_eq!(hub.lock().serving, None);
    assert_eq!(
        Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&result, "command tool answer"),
        completion()
    );
}

#[test]
fn unanswered_drop_end_and_seal_release_the_runner_as_cancelled() {
    for edge in ["drop", "end", "seal"] {
        let hub = Hub::new(fakes::clock::FakeClock::new());
        let (send, rx) = mpsc::channel();
        hub.set_inbox(send);
        let runner = runner(&hub, 1);
        if edge == "seal" {
            hub.seal();
        }
        let result = start(&runner);
        if edge != "seal" {
            let request = received_request(&rx);
            if edge == "end" {
                runner.ended.end();
                assert_eq!(
                    Deadline::after(MUST_SUCCEED_WITHIN)
                        .recv_or_fail(&result, "ended command runner")
                        .status,
                    CallStatus::Cancelled
                );
                (request.taken)();
                assert_eq!(hub.lock().serving, None);
                (request.answer)(completion());
                continue;
            }
            (request.taken)();
            assert_eq!(hub.lock().serving, Some(1));
            drop(request.answer);
            assert_eq!(hub.lock().serving, None);
        }
        assert_eq!(
            Deadline::after(MUST_SUCCEED_WITHIN)
                .recv_or_fail(&result, "cancelled command runner")
                .status,
            CallStatus::Cancelled
        );
    }
}

#[test]
fn a_buffered_request_releases_at_command_end_even_when_sealed() {
    let hub = Hub::new(fakes::clock::FakeClock::new());
    let (send, selected) = mpsc::channel();
    hub.pause_at_windows(Arc::new(move |_, window| {
        if window == super::super::hub::Window::Selected {
            let _sent = send.send(());
        }
    }));
    let runner = runner(&hub, 1);
    let result = start(&runner);
    Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&selected, "buffered command request");
    assert_eq!(hub.lock().router.buffer().len(), 1);
    hub.seal();
    assert!(result.try_recv().is_err());
    runner.ended.end();
    assert_eq!(
        Deadline::after(MUST_SUCCEED_WITHIN)
            .recv_or_fail(&result, "sealed buffered command end")
            .status,
        CallStatus::Cancelled
    );
    let (send, rx) = mpsc::channel();
    hub.set_inbox(send);
    assert!(hub.lock().router.buffer().is_empty());
    assert!(rx.try_recv().is_err());
}

#[test]
fn a_runner_whose_hub_is_gone_returns_cancelled() {
    let hub = Hub::new(fakes::clock::FakeClock::new());
    let runner = runner(&hub, 1);
    drop(hub);
    assert_eq!(
        Deadline::after(MUST_SUCCEED_WITHIN)
            .recv_or_fail(&start(&runner), "disposed command hub")
            .status,
        CallStatus::Cancelled
    );
}

#[test]
fn an_already_ended_runner_sends_nothing() {
    let hub = Hub::new(fakes::clock::FakeClock::new());
    let (send, rx) = mpsc::channel();
    hub.set_inbox(send);
    let runner = runner(&hub, 1);
    runner.ended.end();
    assert_eq!(
        Deadline::after(MUST_SUCCEED_WITHIN)
            .recv_or_fail(&start(&runner), "already ended runner")
            .status,
        CallStatus::Cancelled
    );
    assert!(rx.try_recv().is_err());
}

#[test]
fn answer_and_drop_clear_synchronously_and_a_late_backstop_keeps_the_next_command() {
    for answered in [false, true] {
        let hub = Hub::new(fakes::clock::FakeClock::new());
        let (send, rx) = mpsc::channel();
        hub.set_inbox(send);
        let first = runner(&hub, 1);
        let (entered, paused) = mpsc::channel();
        let (release, held) = mpsc::channel();
        *first.pause.lock().unwrap() = Some((entered, held));
        let result = start(&first);
        let request = received_request(&rx);
        (request.taken)();
        assert_eq!(hub.lock().serving, Some(1));
        if answered {
            (request.answer)(completion());
        } else {
            drop(request.answer);
        }
        assert_eq!(hub.lock().serving, None);
        Deadline::after(MUST_SUCCEED_WITHIN)
            .recv_or_fail(&paused, "first runner paused before backstop");
        let second = runner(&hub, 2);
        let second_result = start(&second);
        let request = received_request(&rx);
        (request.taken)();
        assert_eq!(hub.lock().serving, Some(2));
        release.send(()).unwrap();
        let completed =
            Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&result, "first backstop complete");
        assert_eq!(
            completed.status,
            if answered {
                CallStatus::Completed
            } else {
                CallStatus::Cancelled
            }
        );
        assert_eq!(hub.lock().serving, Some(2));
        (request.answer)(completion());
        assert_eq!(
            Deadline::after(MUST_SUCCEED_WITHIN)
                .recv_or_fail(&second_result, "second command answer"),
            completion()
        );
    }
}

#[test]
fn a_taken_closure_after_an_answer_or_runner_removal_sets_no_window() {
    for removed in [false, true] {
        let hub = Hub::new(fakes::clock::FakeClock::new());
        let (send, rx) = mpsc::channel();
        hub.set_inbox(send);
        let runner = runner(&hub, 1);
        let result = start(&runner);
        let request = received_request(&rx);
        let (entered, paused) = mpsc::channel();
        let (release, held) = mpsc::channel();
        if removed {
            hub.lock().runners.remove(&1);
        } else {
            // Hold the runner after its wait returns and before it clears
            // `serving`, so the clear cannot race the `taken` call below.
            *runner.pause.lock().unwrap() = Some((entered, held));
            (request.answer)(completion());
            Deadline::after(MUST_SUCCEED_WITHIN)
                .recv_or_fail(&paused, "runner paused after its answer");
        }
        (request.taken)();
        assert_eq!(hub.lock().serving, None);
        if removed {
            runner.ended.end();
        } else {
            release.send(()).unwrap();
        }
        assert_eq!(
            Deadline::after(MUST_SUCCEED_WITHIN)
                .recv_or_fail(&result, "closed serving window")
                .status,
            if removed {
                CallStatus::Cancelled
            } else {
                CallStatus::Completed
            }
        );
    }
}

#[test]
fn queued_return_and_drop_clear_the_runner_and_window_before_waking_it() {
    use super::super::{Queued, Target};
    for ending in ["success", "failure", "drop"] {
        let hub = Hub::new(fakes::clock::FakeClock::new());
        let (send, rx) = mpsc::channel();
        hub.set_inbox(send);
        let runner = runner(&hub, 1);
        let (entered, paused) = mpsc::channel();
        let (release, held) = mpsc::channel();
        *runner.pause.lock().unwrap() = Some((entered, held));
        let result = start(&runner);
        let request = received_request(&rx);
        (request.taken)();
        assert_eq!(hub.lock().serving, Some(1));
        let queued = Queued {
            hub: hub.clone(),
            name: "ext".into(),
            id: 1,
            target: Target::Command("go".into()),
            asked: hub.clock().now(),
            ended: runner.ended.clone(),
        };
        if ending == "drop" {
            drop(queued);
        } else {
            if ending == "success" {
                hub.lock().calls.insert(
                    1,
                    super::super::hub::Progress::Done(Ok(serde_json::Value::Null)),
                );
            }
            assert_eq!(queued.wait().is_ok(), ending == "success");
        }
        assert_eq!(hub.lock().serving, None);
        assert_eq!(hub.lock().runners.len(), 0);
        assert!(runner.ended.is_cancelled());
        Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&paused, "queued runner before backstop");
        release.send(()).unwrap();
        assert_eq!(
            Deadline::after(MUST_SUCCEED_WITHIN)
                .recv_or_fail(&result, "queued cleanup wake")
                .status,
            CallStatus::Cancelled
        );
    }
}

#[test]
fn dropping_an_old_queued_call_keeps_a_later_serving_window() {
    let hub = Hub::new(fakes::clock::FakeClock::new());
    let first = runner(&hub, 1);
    let second = runner(&hub, 2);
    hub.lock().serving = Some(2);
    let queued = super::super::Queued {
        hub: hub.clone(),
        name: "ext".into(),
        id: 1,
        target: super::super::Target::Command("go".into()),
        asked: hub.clock().now(),
        ended: first.ended.clone(),
    };
    drop(queued);
    assert_eq!(hub.lock().serving, Some(2));
    assert!(first.ended.is_cancelled());
    assert!(!second.ended.is_cancelled());
    assert!(hub.lock().runners.contains_key(&2));
}

#[test]
fn ended_wakes_current_subscribers_once_and_is_set_before_the_wake() {
    struct Notice {
        source: Arc<Ended>,
        send: mpsc::Sender<bool>,
    }
    impl Wake for Notice {
        fn wake(&self) {
            self.send.send(self.source.is_cancelled()).unwrap();
        }
    }
    let ended = Arc::new(Ended::default());
    let (send, rx) = mpsc::channel();
    let notice: Arc<dyn Wake> = Arc::new(Notice {
        source: ended.clone(),
        send,
    });
    ended.subscribe(Arc::downgrade(&notice));
    assert!(!ended.is_cancelled());
    ended.end();
    assert!(Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&rx, "command ended wake"));
    ended.subscribe(Arc::downgrade(&notice));
    ended.end();
    assert!(rx.try_recv().is_err());
}
