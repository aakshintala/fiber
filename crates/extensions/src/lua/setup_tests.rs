#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]
#![allow(clippy::expect_used, reason = "test code; a failure is the test's")]

use std::sync::mpsc;

use super::super::MEMORY_CAP;
use super::*;

#[test]
fn a_load_observer_fires_once_before_the_read() {
    let dir = fakes::TempDir::new("fiber-load-observer");
    let lua = Lua::new();
    // Canonicalized, as the loader's directory check compares against
    // the canonicalized file: on macOS `/var` resolves to
    // `/private/var`.
    let dir = dir.path().canonicalize().unwrap();
    let watched = dir.join("go_signal.lua");
    std::fs::write(&watched, "").unwrap();
    std::fs::write(dir.join("other.lua"), "return 8").unwrap();
    let (tx, rx) = mpsc::channel();
    let writing = watched.clone();
    observe_load(
        watched.canonicalize().unwrap(),
        Box::new(move || {
            // The read below sees this write, which proves the observer
            // ran before the loader opened the file.
            std::fs::write(&writing, "return 7").unwrap();
            tx.send(()).unwrap();
        }),
    );
    // A load of another file leaves the registration alone.
    let untouched: i64 = load_file(&lua, &dir, "other.lua", MEMORY_CAP)
        .unwrap()
        .unwrap()
        .call(())
        .unwrap();
    assert_eq!(untouched, 8);
    assert!(rx.try_recv().is_err(), "no signal for another file");
    // The first load of the watched file fires the observer, and its
    // read sees the observer's write.
    let loaded = load_file(&lua, &dir, "go_signal.lua", MEMORY_CAP)
        .unwrap()
        .unwrap();
    rx.try_recv().expect("the observer fires before the read");
    let value: i64 = loaded.call(()).unwrap();
    assert_eq!(value, 7);
    // A second load finds the registration spent.
    let again: i64 = load_file(&lua, &dir, "go_signal.lua", MEMORY_CAP)
        .unwrap()
        .unwrap()
        .call(())
        .unwrap();
    assert_eq!(again, 7);
    assert!(rx.try_recv().is_err(), "the observer fires once");
}
