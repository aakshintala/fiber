//! `WeakEmit`: emits while the log lives and does nothing after the last
//! `Arc<Log>` drops, and does not keep the log alive.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::sync::Arc;

use contract::emit::Emit;
use contract::events::{Event, ExtensionUi, Ui};
use fakes::clock::FakeClock;

use super::WeakEmit;

fn status() -> Event {
    Event::ExtensionUi(ExtensionUi {
        extension: "fiber.test/a".to_owned(),
        ui: Ui::Status {
            status: "syncing".to_owned(),
        },
    })
}

fn log_in(temp: &fakes::TempDir) -> Arc<crate::Log> {
    let sessions = temp.path().join("sessions");
    let clock = FakeClock::new();
    let timed: Arc<dyn contract::clock::Clock> = clock;
    Arc::new(crate::Log::create(&sessions, contract::SessionId("s_1".into()), timed).unwrap())
}

#[test]
fn emits_while_the_log_lives() {
    let temp = fakes::TempDir::new("fiber-weak-emit-live");
    let log = log_in(&temp);
    let emit = WeakEmit::new(&log);
    let mut watcher = log.watch();
    emit.emit(&status());
    let line = watcher
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("the status line arrives within 5s")
        .unwrap()
        .expect("the status line arrives");
    assert_eq!(line.kind, "extension_ui");
}

#[test]
fn does_nothing_after_the_last_log_drops() {
    let temp = fakes::TempDir::new("fiber-weak-emit-gone");
    let log = log_in(&temp);
    let emit = WeakEmit::new(&log);
    drop(log);
    // Must not panic; there is no log to write to.
    emit.emit(&status());
}

#[test]
fn does_not_keep_the_log_alive() {
    let temp = fakes::TempDir::new("fiber-weak-emit-weak");
    let log = log_in(&temp);
    let weak = Arc::downgrade(&log);
    let emit = WeakEmit::new(&log);
    drop(log);
    assert!(
        weak.upgrade().is_none(),
        "the emitter holds no strong handle"
    );
    emit.emit(&status());
}
