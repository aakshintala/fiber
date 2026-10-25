//! Tests for the per-page renderer cache: hits, misses, failures,
//! drops and the call counter.

use std::cell::Cell;
use std::sync::Arc;

use super::{Cache, PageKey};
use crate::ext::{Decision, FailureKind};

fn page(index: usize) -> PageKey {
    PageKey {
        session: contract::SessionId("s".into()),
        index,
    }
}
fn decision(call: impl FnOnce() -> Result<Vec<Vec<RawSpan>>, Failure>) -> Decision {
    match call() {
        Ok(lines) => Decision::Draw {
            extension: "test".into(),
            lines,
        },
        Err(error) => Decision::Fail(error),
    }
}

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
        kind: FailureKind::HookFailed,
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
        cache.lines(page(0), "ledger_row", "test", None, &row, width, || {
            decision(|| {
                calls.set(calls.get().saturating_add(1));
                Ok(vec![vec![raw("hi")]])
            })
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
        cache.lines(page(0), "ledger_row", "test", None, &row, width, || {
            decision(|| {
                calls.set(calls.get().saturating_add(1));
                Err(failed("test", "boom"))
            })
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
            .lines(
                page(0),
                "ledger_row",
                "test",
                None,
                &other,
                20,
                || decision(|| {
                    calls.set(calls.get().saturating_add(1));
                    Err(failed("test", "boom"))
                })
            )
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
        cache.lines(page(0), "ledger_row", "test", None, &row, 20, || {
            decision(|| {
                calls.set(calls.get().saturating_add(1));
                Ok(vec![vec![RawSpan {
                    text: "hi".to_owned(),
                    fg: Some("nope".to_owned()),
                    ..RawSpan::default()
                }]])
            })
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
            .lines(page(0), "ledger_row", "test", None, &row, 20, || {
                Decision::Fail(failed("test", "boom"))
            })
            .is_none()
    );
    assert!(
        cache
            .lines(page(0), "ledger_row", "test", None, &row, 20, || {
                Decision::Fail(failed("test", "boom"))
            })
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
    let draw = |at| {
        cache.lines(page(at), "ledger_row", "test", None, &row, 20, || {
            decision(|| {
                calls.set(calls.get().saturating_add(1));
                Ok(vec![vec![raw("hi")]])
            })
        })
    };
    draw(0).expect("a cached line");
    draw(1).expect("a cached line");
    assert_eq!(calls.get(), 2);
    cache.drop_page(page(1));
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
    let draw = |at| {
        cache.lines(page(at), "ledger_row", "test", None, &row, 20, || {
            decision(|| {
                calls.set(calls.get().saturating_add(1));
                Err(failed("test", "boom"))
            })
        })
    };
    assert!(draw(1).is_none());
    assert_eq!(cache.take_failures().len(), 1);
    cache.drop_page(page(1));
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
        cache.lines(page(0), "ledger_row", "test", None, row, width, || {
            decision(|| {
                calls.set(calls.get().saturating_add(1));
                if matches!(row, Input::LedgerRow(ledger) if ledger.name == "read") {
                    return Err(failed("test", "boom"));
                }
                Ok(vec![vec![raw("hi")]])
            })
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
            .lines(page(0), slot, "test", None, &row, 20, || {
                decision(|| {
                    calls.set(calls.get().saturating_add(1));
                    Ok(vec![vec![raw("hi")]])
                })
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
    let drawn = cache.lines(page(0), "ledger_row", "test", None, &row, 20, || {
        decision(|| {
            cache.drop_page(page(1));
            Ok(vec![vec![raw("hi")]])
        })
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
        .lines(page(0), "ledger_row", "test", None, &row, 20, || {
            decision(|| {
                calls.set(calls.get().saturating_add(1));
                Ok(vec![vec![raw("hi")]])
            })
        })
        .expect("a cached line");
    let bad = input("read", "bad", &arguments);
    assert!(
        cache
            .lines(page(0), "ledger_row", "test", None, &bad, 20, || {
                Decision::Fail(failed("test", "boom"))
            })
            .is_none()
    );
    cache.clear();
    assert_eq!(cache.take_failures(), vec![]);
    cache
        .lines(page(0), "ledger_row", "test", None, &row, 20, || {
            decision(|| {
                calls.set(calls.get().saturating_add(1));
                Ok(vec![vec![raw("hi")]])
            })
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
        shared.lines(page(0), "ledger_row", "test", None, row, width, || {
            decision(|| {
                calls.set(calls.get().saturating_add(1));
                Ok(vec![vec![raw("hi")]])
            })
        });
    }
    let third = input("shell", "third", &arguments);
    assert!(
        shared
            .lines(
                page(0),
                "ledger_row",
                "test",
                None,
                &third,
                20,
                || decision(|| {
                    calls.set(calls.get().saturating_add(1));
                    Err(failed("test", "boom"))
                })
            )
            .is_none()
    );
    assert_eq!(calls.get(), 4);
    assert_eq!(crate::work::take().renderer_calls, 4);
}

mod context_regressions {

    use super::{Cache, PageKey};
    use crate::ext::{Decision, Failure, FailureKind, Input, LedgerRow, RawSpan, Status};
    use std::cell::Cell;

    fn page(session: &str, index: usize) -> PageKey {
        PageKey {
            session: contract::SessionId(session.into()),
            index,
        }
    }
    fn input(arguments: &serde_json::Value) -> Input<'_> {
        Input::LedgerRow(LedgerRow {
            name: "shell",
            arguments,
            status: Status::Completed,
            exit: None,
            duration_ms: None,
            lines: 0,
            changes: &[],
            error: None,
            output: "done",
            answer: None,
            flat: false,
        })
    }
    fn draw(extension: &str, text: &str) -> Decision {
        Decision::Draw {
            extension: extension.into(),
            lines: vec![vec![RawSpan {
                text: text.into(),
                ..RawSpan::default()
            }]],
        }
    }
    fn fail(extension: &str) -> Decision {
        Decision::Fail(Failure {
            extension: extension.into(),
            kind: FailureKind::HookFailed,
            message: "failed".into(),
        })
    }

    #[test]
    fn entries_key_page_winner_half_input_and_width() {
        let cache = Cache::default();
        let args = serde_json::json!({});
        let row = input(&args);
        let calls = Cell::new(0);
        for (session, index, winner, half, width) in [
            ("s", 0, "a", "v1", 20),
            ("s", 0, "a", "v1", 20),
            ("t", 0, "a", "v1", 20),
            ("s", 1, "a", "v1", 20),
            ("s", 0, "b", "v1", 20),
            ("s", 0, "a", "v2", 20),
            ("s", 0, "a", "v1", 21),
        ] {
            assert!(
                cache
                    .lines(
                        page(session, index),
                        "ledger_row",
                        winner,
                        Some(half.into()),
                        &row,
                        width,
                        || {
                            calls.set(calls.get() + 1);
                            draw(winner, "hello")
                        }
                    )
                    .is_some()
            );
        }
        // docs/tui.md: only output-relevant page/winner/half/input/width changes rerun.
        assert_eq!(calls.get(), 6);
        let changed = serde_json::json!({"command":"pwd"});
        assert!(
            cache
                .lines(
                    page("s", 0),
                    "ledger_row",
                    "a",
                    Some("v1".into()),
                    &input(&changed),
                    20,
                    || {
                        calls.set(calls.get() + 1);
                        draw("a", "other")
                    }
                )
                .is_some()
        );
        assert_eq!(calls.get(), 7);
    }

    #[test]
    fn failures_ignore_width_but_not_page_winner_or_half() {
        let cache = Cache::default();
        let args = serde_json::json!({});
        let row = input(&args);
        let calls = Cell::new(0);
        for (session, index, winner, half, width) in [
            ("s", 0, "a", "v1", 20),
            ("s", 0, "a", "v1", 30),
            ("s", 1, "a", "v1", 20),
            ("t", 0, "a", "v1", 20),
            ("s", 0, "b", "v1", 20),
            ("s", 0, "a", "v2", 20),
        ] {
            assert!(
                cache
                    .lines(
                        page(session, index),
                        "ledger_row",
                        winner,
                        Some(half.into()),
                        &row,
                        width,
                        || {
                            calls.set(calls.get() + 1);
                            fail(winner)
                        }
                    )
                    .is_none()
            );
        }
        assert_eq!(calls.get(), 5); // docs/tui.md: failure key excludes width.
        cache.queue_notice(("command_conflict".into(), "conflict".into()));
        cache.drop_page(page("s", 0));
        assert_eq!(cache.take_failures().len(), 2); // Only the exact page is pruned.
        assert_eq!(cache.take_notices().len(), 1); // Selection notices have no page lifetime.
        cache.lines(
            page("s", 1),
            "ledger_row",
            "a",
            Some("v1".into()),
            &row,
            20,
            || panic!("other page's memo must survive"),
        );
    }

    #[test]
    fn speculative_trim_preserves_delivery_and_memo_then_landing_allows_reload() {
        let durable = Cache::default();
        let fresh = durable.scratch_sharing_pending();
        let args = serde_json::json!({});
        let row = input(&args);
        fresh.lines(
            page("delegate", 0),
            "ledger_row",
            "a",
            None,
            &row,
            20,
            || fail("a"),
        );
        fresh.queue_notice(("extension_incompatible".into(), "not loaded".into()));
        fresh.drop_page(page("delegate", 0));
        fresh.clear();
        durable.drop_page(page("parent", 0));
        assert_eq!(durable.take_failures().len(), 1);
        assert_eq!(durable.take_notices().len(), 1);
        let scratch = durable.scratch_sharing_pending();
        scratch.lines(
            page("delegate", 0),
            "ledger_row",
            "a",
            None,
            &row,
            40,
            || panic!("shared failure memo"),
        );
        assert!(durable.take_failures().is_empty());
        fresh.promote();
        fresh.clear();
        assert!(
            fresh
                .lines(
                    page("delegate", 0),
                    "ledger_row",
                    "a",
                    None,
                    &row,
                    20,
                    || draw("a", "replacement")
                )
                .is_some()
        );
    }

    #[test]
    fn fresh_successes_are_independent_and_drop_entries_keeps_pending() {
        let durable = Cache::default();
        let args = serde_json::json!({});
        let row = input(&args);
        durable.lines(page("s", 0), "ledger_row", "a", None, &row, 20, || {
            draw("a", "first")
        });
        let fresh = durable.scratch_sharing_pending();
        let called = Cell::new(false);
        fresh.lines(page("s", 0), "ledger_row", "a", None, &row, 20, || {
            called.set(true);
            draw("a", "second")
        });
        assert!(called.get());
        fresh.lines(page("s", 1), "ledger_row", "a", None, &row, 20, || {
            fail("a")
        });
        fresh.queue_notice(("command_conflict".into(), "conflict".into()));
        durable.drop_entries();
        assert_eq!(durable.take_failures().len(), 1);
        assert_eq!(durable.take_notices().len(), 1);
        fresh.queue_notice(("command_conflict".into(), "again".into()));
        durable.clear();
        assert!(durable.take_notices().is_empty());
        assert!(
            durable
                .lines(page("s", 1), "ledger_row", "a", None, &row, 20, || draw(
                    "a", "reset"
                ))
                .is_some()
        );
    }

    #[test]
    fn wrong_shapes_use_actual_identity_and_not_for_session_is_not_memoized() {
        let cache = Cache::default();
        let args = serde_json::json!({});
        let row = input(&args);
        cache.lines(page("s", 0), "ledger_row", "a", None, &row, 20, || {
            draw("a", "bad\ntext")
        });
        let failures = cache.take_failures();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].1.kind, FailureKind::HookFailed);
        assert_eq!(failures[0].1.extension, "a");
        cache.lines(page("s", 1), "ledger_row", "a", None, &row, 20, || {
            Decision::NotForSession {
                extension: "a".into(),
                tui_version: "v1".into(),
                half: crate::ext::SessionVersion::NotLoaded,
            }
        });
        assert!(
            cache
                .lines(page("s", 1), "ledger_row", "a", None, &row, 20, || draw(
                    "a", "eligible"
                ))
                .is_some()
        );
        cache.lines(page("s", 2), "ledger_row", "a", None, &row, 20, || {
            draw("b", "wrong winner")
        });
        assert!(
            cache
                .lines(page("s", 2), "ledger_row", "a", None, &row, 20, || draw(
                    "a",
                    "right winner"
                ))
                .is_some()
        );
    }
}
