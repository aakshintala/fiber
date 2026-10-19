//! Tests for the per-page renderer cache: hits, misses, failures,
//! drops and the call counter.

use std::cell::Cell;
use std::sync::Arc;

use super::Cache;
use crate::ext::{Failure, Input, LedgerRow, RawSpan, Status};

fn raw(text: &str) -> RawSpan {
    RawSpan {
        text: text.to_owned(),
        ..RawSpan::default()
    }
}

fn input<'a>(name: &'a str, output: &'a str, arguments: &'a serde_json::Value) -> Input<'a> {
    Input::LedgerRow(LedgerRow {
        name,
        arguments,
        status: Status::Completed,
        exit: None,
        duration_ms: None,
        lines: 0,
        changes: &[],
        error: None,
        output,
        answer: None,
        flat: false,
    })
}

fn failed(extension: &str, message: &str) -> Failure {
    Failure {
        extension: extension.to_owned(),
        message: message.to_owned(),
    }
}

#[test]
fn a_second_draw_reads_the_cache() {
    crate::work::take();
    let cache = Cache::default();
    let calls = Cell::new(0usize);
    let arguments = serde_json::json!({"command": "ls"});
    let row = input("shell", "first", &arguments);
    let draw = |width| {
        cache.lines(0, "ledger_row", "test", &row, width, || {
            calls.set(calls.get().saturating_add(1));
            Ok(vec![vec![raw("hi")]])
        })
    };
    let first = draw(20).expect("a cached line");
    let second = draw(20).expect("a cached line");
    let one = first.first().expect("one fitted line");
    let two = second.first().expect("one fitted line");
    assert_eq!(calls.get(), 1);
    assert_eq!(one.line.to_string(), two.line.to_string());
    assert_eq!(one.line.to_string(), format!("hi{}", " ".repeat(18)));
    draw(24).expect("a cached line");
    assert_eq!(calls.get(), 2);
    draw(20).expect("a cached line");
    draw(24).expect("a cached line");
    assert_eq!(calls.get(), 2);
}

#[test]
fn a_failure_falls_back_and_calls_nothing_again() {
    crate::work::take();
    let cache = Cache::default();
    let calls = Cell::new(0usize);
    let arguments = serde_json::json!({"command": "ls"});
    let row = input("shell", "first", &arguments);
    let draw = |width| {
        cache.lines(0, "ledger_row", "test", &row, width, || {
            calls.set(calls.get().saturating_add(1));
            Err(failed("test", "boom"))
        })
    };
    assert!(draw(20).is_none());
    assert_eq!(calls.get(), 1);
    assert!(draw(20).is_none());
    assert!(draw(24).is_none());
    assert_eq!(calls.get(), 1, "the failure key holds no width");
    let other = input("shell", "second", &arguments);
    assert!(
        cache
            .lines(0, "ledger_row", "test", &other, 20, || {
                calls.set(calls.get().saturating_add(1));
                Err(failed("test", "boom"))
            })
            .is_none()
    );
    assert_eq!(calls.get(), 2);
}

#[test]
fn a_wrong_shape_is_a_cached_failure_listed_once() {
    crate::work::take();
    let cache = Cache::default();
    let calls = Cell::new(0usize);
    let arguments = serde_json::json!({"command": "ls"});
    let row = input("shell", "first", &arguments);
    let draw = || {
        cache.lines(0, "ledger_row", "test", &row, 20, || {
            calls.set(calls.get().saturating_add(1));
            Ok(vec![vec![RawSpan {
                text: "hi".to_owned(),
                fg: Some("nope".to_owned()),
                ..RawSpan::default()
            }]])
        })
    };
    assert!(draw().is_none());
    assert!(draw().is_none());
    assert_eq!(calls.get(), 1);
    let listed = cache.take_failures();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed.first().expect("one failure").0, "ledger_row");
    let failure = listed.first().expect("one failure").1.clone();
    assert_eq!(failure.extension, "test", "{failure:?}");
    assert!(failure.message.contains("fg"), "{listed:?}");
    assert_eq!(cache.take_failures(), vec![]);
}

#[test]
fn an_error_is_listed_once_and_a_hit_lists_nothing() {
    crate::work::take();
    let cache = Cache::default();
    let arguments = serde_json::json!({"command": "ls"});
    let row = input("shell", "first", &arguments);
    assert!(
        cache
            .lines(0, "ledger_row", "test", &row, 20, || Err(failed(
                "test", "boom"
            )))
            .is_none()
    );
    assert!(
        cache
            .lines(0, "ledger_row", "test", &row, 20, || Err(failed(
                "test", "boom"
            )))
            .is_none()
    );
    let listed = cache.take_failures();
    assert_eq!(listed.len(), 1);
    assert_eq!(
        listed.first().expect("one failure"),
        &("ledger_row".to_owned(), failed("test", "boom"))
    );
}

#[test]
fn dropping_a_page_calls_that_page_again_only() {
    crate::work::take();
    let cache = Cache::default();
    let calls = Cell::new(0usize);
    let arguments = serde_json::json!({"command": "ls"});
    let row = input("shell", "first", &arguments);
    let draw = |page| {
        cache.lines(page, "ledger_row", "test", &row, 20, || {
            calls.set(calls.get().saturating_add(1));
            Ok(vec![vec![raw("hi")]])
        })
    };
    draw(0).expect("a cached line");
    draw(1).expect("a cached line");
    assert_eq!(calls.get(), 2);
    cache.drop_page(1);
    draw(0).expect("a cached line");
    assert_eq!(calls.get(), 2);
    draw(1).expect("a cached line");
    assert_eq!(calls.get(), 3);
}

#[test]
fn dropping_a_page_calls_a_failed_input_again() {
    crate::work::take();
    let cache = Cache::default();
    let calls = Cell::new(0usize);
    let arguments = serde_json::json!({"command": "ls"});
    let row = input("shell", "first", &arguments);
    let draw = |page| {
        cache.lines(page, "ledger_row", "test", &row, 20, || {
            calls.set(calls.get().saturating_add(1));
            Err(failed("test", "boom"))
        })
    };
    assert!(draw(1).is_none());
    assert_eq!(cache.take_failures().len(), 1);
    cache.drop_page(1);
    assert_eq!(cache.take_failures(), vec![]);
    assert!(draw(1).is_none());
    assert_eq!(calls.get(), 2);
}

#[test]
fn dropping_entries_calls_every_width_again_and_keeps_a_failure() {
    crate::work::take();
    let cache = Cache::default();
    let calls = Cell::new(0usize);
    let arguments = serde_json::json!({"command": "ls"});
    let row = input("shell", "good", &arguments);
    let bad = input("read", "bad", &arguments);
    let draw = |row: &Input<'_>, width| {
        cache.lines(0, "ledger_row", "test", row, width, || {
            calls.set(calls.get().saturating_add(1));
            if matches!(row, Input::LedgerRow(ledger) if ledger.name == "read") {
                return Err(failed("test", "boom"));
            }
            Ok(vec![vec![raw("hi")]])
        })
    };
    draw(&row, 20).expect("a cached line");
    draw(&row, 24).expect("a cached line");
    assert!(draw(&bad, 20).is_none());
    assert_eq!(calls.get(), 3);
    assert_eq!(cache.take_failures().len(), 1);
    cache.drop_entries();
    draw(&row, 20).expect("a cached line");
    draw(&row, 24).expect("a cached line");
    assert!(draw(&bad, 20).is_none());
    assert_eq!(calls.get(), 5, "the failure stays cached");
}

#[test]
fn the_same_input_in_two_slots_calls_twice() {
    crate::work::take();
    let cache = Cache::default();
    let calls = Cell::new(0usize);
    let arguments = serde_json::json!({"command": "ls"});
    let row = input("shell", "first", &arguments);
    for slot in ["ledger_row", "ledger_row:shell"] {
        cache
            .lines(0, slot, "test", &row, 20, || {
                calls.set(calls.get().saturating_add(1));
                Ok(vec![vec![raw("hi")]])
            })
            .expect("a cached line");
    }
    assert_eq!(calls.get(), 2);
}

#[test]
fn a_renderer_may_drop_another_page_while_drawing() {
    crate::work::take();
    let cache = Cache::default();
    let arguments = serde_json::json!({"command": "ls"});
    let row = input("shell", "first", &arguments);
    let drawn = cache.lines(0, "ledger_row", "test", &row, 20, || {
        cache.drop_page(1);
        Ok(vec![vec![raw("hi")]])
    });
    assert!(drawn.is_some());
    assert_eq!(cache.take_failures(), vec![]);
}

#[test]
fn clear_drops_everything() {
    crate::work::take();
    let cache = Cache::default();
    let calls = Cell::new(0usize);
    let arguments = serde_json::json!({"command": "ls"});
    let row = input("shell", "first", &arguments);
    cache
        .lines(0, "ledger_row", "test", &row, 20, || {
            calls.set(calls.get().saturating_add(1));
            Ok(vec![vec![raw("hi")]])
        })
        .expect("a cached line");
    let bad = input("read", "bad", &arguments);
    assert!(
        cache
            .lines(0, "ledger_row", "test", &bad, 20, || Err(failed(
                "test", "boom"
            )))
            .is_none()
    );
    cache.clear();
    assert_eq!(cache.take_failures(), vec![]);
    cache
        .lines(0, "ledger_row", "test", &row, 20, || {
            calls.set(calls.get().saturating_add(1));
            Ok(vec![vec![raw("hi")]])
        })
        .expect("a cached line");
    assert_eq!(calls.get(), 2);
}

#[test]
fn the_counter_matches_the_renderer_calls() {
    crate::work::take();
    let cache = Cache::default();
    let calls = Cell::new(0usize);
    let shared = Arc::new(cache);
    let arguments = serde_json::json!({"command": "ls"});
    let first = input("shell", "first", &arguments);
    let second = input("shell", "second", &arguments);
    for (row, width) in [(&first, 20), (&first, 20), (&second, 20), (&first, 24)] {
        shared.lines(0, "ledger_row", "test", row, width, || {
            calls.set(calls.get().saturating_add(1));
            Ok(vec![vec![raw("hi")]])
        });
    }
    let third = input("shell", "third", &arguments);
    assert!(
        shared
            .lines(0, "ledger_row", "test", &third, 20, || {
                calls.set(calls.get().saturating_add(1));
                Err(failed("test", "boom"))
            })
            .is_none()
    );
    assert_eq!(calls.get(), 4);
    assert_eq!(crate::work::take().renderer_calls, 4);
}
