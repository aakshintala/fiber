//! The Lua half of `host.log` (`docs/extensions.md`, "Host calls"): a
//! string or number reaches the inbox as one `extension_log` delivery, in
//! call order with the extension's other deliveries; anything else raises.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::sync::Arc;
use std::sync::mpsc;

use contract::events::{ExtensionExec, ExtensionLog};
use contract::inbox::Delivery;
use fakes::clock::FakeClock;
use mlua::{Lua, StdLib};

use super::install;
use crate::lua::Hub;

fn installed() -> (Lua, Arc<Hub>) {
    let lua = Lua::new_with(
        StdLib::ALL_SAFE & !StdLib::IO & !StdLib::OS & !StdLib::PACKAGE,
        mlua::LuaOptions::new().catch_rust_panics(false),
    )
    .unwrap();
    let hub = Hub::new(FakeClock::new());
    let host = lua.create_table().unwrap();
    install(&lua, &host, &hub, "fiber.test/notes").unwrap();
    lua.globals().set("host", host).unwrap();
    (lua, hub)
}

/// The next delivery `hub` routes, flushed through a fresh sender: what
/// `host.log` buffered before any `deliver_to`.
fn flushed(hub: &Arc<Hub>) -> Delivery {
    let (tx, rx) = mpsc::channel();
    hub.set_inbox(tx);
    rx.try_recv().expect("the line was buffered")
}

fn logged(delivery: Delivery) -> ExtensionLog {
    match delivery {
        Delivery::ExtensionLog(log) => log,
        other @ (Delivery::ExtensionExec(_)
        | Delivery::Prompt(..)
        | Delivery::Steer(..)
        | Delivery::SteerDrop(..)
        | Delivery::Handoff(..)
        | Delivery::Model(..)
        | Delivery::Reply(..)
        | Delivery::Close(_)
        | Delivery::Job(_)
        | Delivery::JobLine(_)
        | Delivery::Interaction(_)
        | Delivery::Resolved(..)
        | Delivery::Cancelled) => panic!("expected extension_log, got {other:?}"),
    }
}

fn ran(program: &str) -> ExtensionExec {
    ExtensionExec {
        extension: "fiber.test/notes".to_owned(),
        program: program.to_owned(),
        args: Vec::new(),
        cwd: "/tmp".to_owned(),
        process: contract::shapes::Process {
            exit_code: Some(0),
            signal: None,
            timed_out: false,
        },
    }
}

#[test]
fn a_string_message_reaches_the_inbox_with_the_extension_name() {
    let (lua, hub) = installed();
    lua.load(r#"host.log("hello")"#).exec().unwrap();
    let log = logged(flushed(&hub));
    assert_eq!(log.extension, "fiber.test/notes");
    assert_eq!(log.message, "hello");
}

#[test]
fn numbers_coerce_as_lua_coerces_them() {
    let (lua, hub) = installed();
    let (tx, rx) = mpsc::channel();
    hub.set_inbox(tx);
    lua.load("host.log(42)").exec().unwrap();
    lua.load("host.log(1.5)").exec().unwrap();
    assert_eq!(
        logged(rx.try_recv().expect("the integer arrives")).message,
        "42"
    );
    assert_eq!(
        logged(rx.try_recv().expect("the float arrives")).message,
        "1.5"
    );
}

#[test]
fn anything_but_a_string_or_number_raises() {
    let (lua, hub) = installed();
    for code in [
        "host.log({})",
        "host.log(nil)",
        "host.log(true)",
        "host.log(print)",
    ] {
        let err = lua.load(code).exec().expect_err("the call raises");
        assert!(
            err.to_string()
                .contains("host.log: the message must be a string"),
            "{err}"
        );
    }
    let (tx, rx) = mpsc::channel();
    hub.set_inbox(tx);
    assert!(rx.try_recv().is_err(), "nothing was routed");
}

#[test]
fn bytes_that_are_not_utf8_arrive_lossy_without_panic() {
    let (lua, hub) = installed();
    lua.load("host.log(string.char(0xFF, 0xFE))")
        .exec()
        .unwrap();
    assert_eq!(logged(flushed(&hub)).message, "\u{fffd}\u{fffd}");
}

#[test]
fn a_line_before_deliver_to_keeps_order_with_an_exec_that_ends_after_it() {
    let (lua, hub) = installed();
    lua.load(r#"host.log("early")"#).exec().unwrap();
    hub.send(Delivery::ExtensionExec(ran("git")));
    let (tx, rx) = mpsc::channel();
    hub.set_inbox(tx);
    let first = logged(rx.try_recv().expect("the log line arrives first"));
    assert_eq!(first.message, "early");
    match rx.try_recv().expect("the run arrives second") {
        Delivery::ExtensionExec(exec) => assert_eq!(exec.program, "git"),
        other @ (Delivery::ExtensionLog(_)
        | Delivery::Prompt(..)
        | Delivery::Steer(..)
        | Delivery::SteerDrop(..)
        | Delivery::Handoff(..)
        | Delivery::Model(..)
        | Delivery::Reply(..)
        | Delivery::Close(_)
        | Delivery::Job(_)
        | Delivery::JobLine(_)
        | Delivery::Interaction(_)
        | Delivery::Resolved(..)
        | Delivery::Cancelled) => panic!("expected extension_exec, got {other:?}"),
    }
}

#[test]
fn a_line_after_dispose_is_dropped() {
    let (lua, hub) = installed();
    hub.dispose("fiber.test/notes");
    lua.load(r#"host.log("late")"#).exec().unwrap();
    let (tx, rx) = mpsc::channel();
    hub.set_inbox(tx);
    assert!(rx.try_recv().is_err(), "nothing arrives after the drop");
}
