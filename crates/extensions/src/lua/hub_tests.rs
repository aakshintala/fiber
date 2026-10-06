//! `send_exec` against `set_exec_inbox`: buffered runs flush in order,
//! and a `deliver_to` racing a run's end strands nothing
//! (`docs/extensions.md`, "Host calls").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]
use std::sync::Barrier;

use fakes::clock::FakeClock;

use super::*;

fn exec(tag: &str) -> ExtensionExec {
    ExtensionExec {
        extension: "ext".to_owned(),
        program: tag.to_owned(),
        args: Vec::new(),
        cwd: "/tmp".to_owned(),
        process: contract::shapes::Process {
            exit_code: Some(0),
            signal: None,
            timed_out: false,
        },
    }
}

fn received(rx: &std::sync::mpsc::Receiver<Delivery>) -> Option<String> {
    match rx.try_recv().ok()? {
        Delivery::ExtensionExec(exec) => Some(exec.program),
        Delivery::Prompt(..)
        | Delivery::Steer(..)
        | Delivery::SteerDrop(..)
        | Delivery::Handoff(..)
        | Delivery::Reply(..)
        | Delivery::Close(_)
        | Delivery::Job(_)
        | Delivery::JobLine(_)
        | Delivery::Cancelled => None,
    }
}

/// A run that ends before any sender is buffered and flushed in order
/// on the first sender.
#[test]
fn a_run_before_any_sender_flushes_on_the_first_one() {
    let clock = FakeClock::new();
    let hub = Hub::new(clock);
    hub.send_exec(exec("first"));
    hub.send_exec(exec("second"));
    let (tx, rx) = std::sync::mpsc::channel();
    hub.set_exec_inbox(tx);
    assert_eq!(received(&rx).as_deref(), Some("first"));
    assert_eq!(received(&rx).as_deref(), Some("second"));
    assert!(hub.lock().exec_buffer.is_empty());
}

/// `deliver_to` racing a run's end strands nothing: sender choice,
/// buffering and the flush serialize under one lock, so every run is
/// either received or still buffered, in order. The barrier starts both
/// threads together, forcing the overlap without timing.
#[test]
fn deliver_to_racing_a_run_end_strands_nothing() {
    for i in 0..50 {
        let clock = FakeClock::new();
        let hub = Hub::new(clock);
        let tag = format!("run-{i}");
        let run = exec(&tag);
        let barrier = Arc::new(Barrier::new(2));
        let (tx, rx) = std::sync::mpsc::channel();
        let (hub_run, barrier_run) = (Arc::clone(&hub), Arc::clone(&barrier));
        let sending = std::thread::spawn(move || {
            barrier_run.wait();
            hub_run.send_exec(run);
        });
        let (hub_deliver, barrier_deliver) = (Arc::clone(&hub), Arc::clone(&barrier));
        let delivering = std::thread::spawn(move || {
            barrier_deliver.wait();
            hub_deliver.set_exec_inbox(tx);
        });
        sending.join().expect("the run thread joins");
        delivering.join().expect("the deliver thread joins");
        // Either order leaves the run received: buffered-then-flushed,
        // or sent direct to the inbox. Stranded is an empty inbox with
        // the run still buffered.
        let got = received(&rx).as_deref() == Some(tag.as_str());
        let stranded = !got && !hub.lock().exec_buffer.is_empty();
        assert!(!stranded, "iteration {i}: the racing run was stranded");
        assert!(got, "iteration {i}: the racing run was lost");
    }
}
