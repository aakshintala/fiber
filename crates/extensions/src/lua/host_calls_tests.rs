//! The `host.model` cancel a call registers and the hub ends
//! (`docs/extensions.md`, "Host calls"): ending sets the flag before waking
//! subscribers, a late subscriber is never woken, and registering, ending
//! and finishing move the hub's entry.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::sync::{Arc, mpsc};

use contract::clock::Wake;
use contract::tool::Cancel;
use fakes::Deadline;
use fakes::clock::FakeClock;

use super::super::hub::Hub;
use super::*;

/// How long a test waits for a wake before it fails.
const WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// A subscriber reporting whether the cancel was already set when woken.
struct Notice {
    source: Arc<ModelCancel>,
    send: mpsc::Sender<bool>,
}

impl Wake for Notice {
    fn wake(&self) {
        self.send.send(self.source.is_cancelled()).unwrap();
    }
}

fn subscribed(source: &Arc<ModelCancel>) -> (mpsc::Receiver<bool>, Arc<dyn Wake>) {
    let (send, rx) = mpsc::channel();
    let notice: Arc<dyn Wake> = Arc::new(Notice {
        source: Arc::clone(source),
        send,
    });
    source.subscribe(Arc::downgrade(&notice));
    (rx, notice)
}

/// Ending sets the flag and wakes every subscriber once, with the flag
/// already set at the wake.
#[test]
fn end_sets_cancelled_and_wakes_subscribers_once() {
    let cancel = Arc::new(ModelCancel::default());
    assert!(!cancel.is_cancelled());
    let (first, _hold_first) = subscribed(&cancel);
    let (second, _hold_second) = subscribed(&cancel);
    cancel.end();
    assert!(cancel.is_cancelled());
    assert!(Deadline::after(WAIT).recv_or_fail(&first, "first subscriber to wake"));
    assert!(Deadline::after(WAIT).recv_or_fail(&second, "second subscriber to wake"));
    cancel.end();
    assert!(first.try_recv().is_err(), "no second wake");
    assert!(second.try_recv().is_err(), "no second wake");
}

/// A subscriber after the end is never woken: the end is idempotent, so no
/// later end fires for it.
#[test]
fn a_subscriber_after_the_end_is_never_woken() {
    let cancel = Arc::new(ModelCancel::default());
    let (before, _hold_before) = subscribed(&cancel);
    cancel.end();
    assert!(Deadline::after(WAIT).recv_or_fail(&before, "earlier subscriber to wake"));
    let (after, _hold_after) = subscribed(&cancel);
    cancel.end();
    assert!(after.try_recv().is_err(), "nothing wakes a late subscriber");
}

/// Registering records the call's cancel; ending it through the hub sets
/// the flag.
#[test]
fn register_model_registers_end_model_ends() {
    let hub = Hub::new(FakeClock::new());
    let cancel = {
        let mut shared = hub.lock();
        assert!(!shared.models.contains_key(&7));
        let cancel = shared.register_model(7);
        assert!(!cancel.is_cancelled());
        cancel
    };
    assert!(hub.lock().models.contains_key(&7));
    hub.lock().end_model(7);
    assert!(cancel.is_cancelled());
}

/// Finishing drops the registration: a later end sets nothing.
#[test]
fn finish_model_removes_the_registration() {
    let hub = Hub::new(FakeClock::new());
    let cancel = {
        let mut shared = hub.lock();
        let cancel = shared.register_model(7);
        shared.finish_model(7);
        assert!(!shared.models.contains_key(&7));
        cancel
    };
    hub.lock().end_model(7);
    assert!(!cancel.is_cancelled());
}

/// Scripted `host.exec` entries match `input` when the case names it.
/// Matching is a subset: a case without `input` matches any run, a case
/// with `input` matches only that stdin, and a mismatch consumes nothing.
#[test]
fn scripted_exec_matches_input_when_given() {
    use crate::host::exec::ExecRequest;
    use crate::host::script::{ExecEnd, ExecEntry, ExecReply, HostScript};
    use serde_json::{Value, json};

    fn run(input: Option<&[u8]>) -> Value {
        ExecRequest {
            program: "sh".to_owned(),
            args: vec!["-c".to_owned(), "cat".to_owned()],
            cwd: std::path::PathBuf::from("/workspace"),
            env: std::collections::BTreeMap::new(),
            input: input.map(|bytes| bytes.to_vec()),
            cap: 1024,
            own_group: true,
            #[cfg(test)]
            stdout_read: None,
        }
        .case_value()
    }

    fn entry(request: Value) -> ExecEntry {
        ExecEntry {
            request,
            reply: Ok(ExecReply::Completed {
                end: ExecEnd::Code(0),
                stdout: String::new(),
                stderr: String::new(),
            }),
        }
    }

    // A case without `input` matches any run, as today.
    let script = HostScript::new(
        vec![],
        vec![entry(json!({"program": "sh", "cwd": "/workspace"}))],
        vec![],
    );
    assert!(script.exec(run(Some(b"abc"))).1.is_ok());
    assert_eq!(script.unmet().len(), 0, "the entry was consumed");

    // A case with `input` matches only that stdin.
    let script = HostScript::new(
        vec![],
        vec![entry(json!({
            "program": "sh",
            "cwd": "/workspace",
            "input": "abc",
        }))],
        vec![],
    );
    let miss = script.exec(run(Some(b"xyz")));
    assert!(miss.1.is_err(), "other stdin is a miss");
    assert!(script.exec(run(None)).1.is_err(), "null stdin is a miss");
    assert!(script.exec(run(Some(b"abc"))).1.is_ok());
    assert_eq!(script.unmet().len(), 2, "only the two misses remain");
}
