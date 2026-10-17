//! Whether a person can answer is the live full-client count, except in ask.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::sync::Arc;

use fakes::Client;
use support::{Opened, kind, subscribe, until};

#[test]
fn no_connection_is_unattended() {
    let opened = Opened::open(vec![]);
    let attended = opened.session.attended();
    opened
        .session
        .run(vec![], Arc::new(|| false), |_| {
            assert!(!attended());
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn full_connections_count_at_call_time() {
    let opened = Opened::open(vec![]);
    let attended = opened.session.attended();
    opened
        .session
        .run(vec![], Arc::new(|| false), |_| {
            let first = Client::connect(&opened.socket).unwrap();
            subscribe(&first, "first", "full");
            until(&first, |line| {
                kind(line) == "clients" && line["payload"]["count"] == 1
            });
            assert!(attended());
            let second = Client::connect(&opened.socket).unwrap();
            subscribe(&second, "second", "full");
            until(&second, |line| {
                kind(line) == "clients" && line["payload"]["count"] == 2
            });
            assert!(attended());
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn summary_connections_do_not_count() {
    let opened = Opened::open(vec![]);
    let attended = opened.session.attended();
    opened
        .session
        .run(vec![], Arc::new(|| false), |_| {
            let client = Client::connect(&opened.socket).unwrap();
            subscribe(&client, "summary", "summary");
            assert!(!attended());
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn lowering_a_full_connection_stops_counting_it() {
    let opened = Opened::open(vec![]);
    let attended = opened.session.attended();
    opened
        .session
        .run(vec![], Arc::new(|| false), |_| {
            let client = Client::connect(&opened.socket).unwrap();
            subscribe(&client, "full", "full");
            until(&client, |line| {
                kind(line) == "clients" && line["payload"]["count"] == 1
            });
            assert!(attended());
            support::send(&client, &support::subscribe_line("lower", "summary"));
            until(&client, |line| support::command_id(line) == Some("lower"));
            assert!(!attended());
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn ask_never_counts_a_full_connection() {
    let opened = Opened::open(vec![]);
    let attended = opened.session.attended();
    opened
        .session
        .ask("hello".into(), Arc::new(|| false), |_| {
            let client = Client::connect(&opened.socket).unwrap();
            subscribe(&client, "full", "full");
            until(&client, |line| {
                kind(line) == "clients" && line["payload"]["count"] == 1
            });
            assert!(!attended());
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn an_attendance_reader_does_not_keep_a_closed_session_alive() {
    let opened = Opened::open(vec![]);
    let attended = opened.session.attended();
    opened.close();
    assert!(!attended());
}
