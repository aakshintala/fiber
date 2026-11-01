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

#[test]
fn home_key_never_equals_a_real_page_key() {
    let home = PageKey::home();
    for key in [
        page(0),
        PageKey {
            session: contract::SessionId(String::new()),
            index: 0,
        },
        PageKey {
            session: contract::SessionId("s".into()),
            index: usize::MAX,
        },
    ] {
        assert_ne!(home, key);
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

use crate::ext::{
    AnswerDisposition, BranchName, CallState, Card, CardPrompt, ChangedFile, ChangedFiles,
    Delegate, DelegatesCard, Failure, Input, Job, JobsCard, LedgerResult, LedgerRow, Prompt,
    PromptImage, Quota, RawSpan, ReplyText, SessionCard, SessionContext, Spend, StatusRows,
    SteeringMessage, SteeringQueue, ThinkingLine, TurnCard, TurnCardState, TurnEnded, WidgetCard,
    WorkingLine, WorkingRetry,
};

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
        state: CallState::Completed(LedgerResult {
            exit: None,
            duration_ms: None,
            lines: 0,
            changes: &[],
            output,
        }),
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
    // (`docs/tui.md`, "When an extension fails": the failure is cached against the item's input.)
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
    // (`docs/tui.md`, "When an extension fails": the failure is cached against the item's input.)
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
    use crate::ext::{
        CallState, Decision, Failure, FailureKind, Input, LedgerResult, LedgerRow, RawSpan,
    };
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
            state: CallState::Completed(LedgerResult {
                exit: None,
                duration_ms: None,
                lines: 0,
                changes: &[],
                output: "done",
            }),
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
    fn a_renderer_without_a_session_half_ignores_halves_map_changes() {
        use crate::ext::{Renderer, SessionCtx};

        /// A client-only renderer: no `half` override, so the default
        /// reports no session half for every context.
        #[derive(Debug)]
        struct ClientOnly {
            calls: Cell<usize>,
        }
        impl Renderer for ClientOnly {
            fn name(&self) -> &str {
                "client"
            }
            fn fills(&self, slot: &str) -> bool {
                slot == "ledger_row"
            }
            fn render(
                &self,
                _ctx: &SessionCtx,
                _slot: &str,
                _input: &Input<'_>,
                _width: u16,
                _builtin: &dyn Fn(&Input<'_>, u16) -> Vec<Vec<RawSpan>>,
            ) -> Decision {
                self.calls.set(self.calls.get() + 1);
                Decision::Draw {
                    extension: "client".into(),
                    lines: vec![vec![RawSpan {
                        text: "hi".into(),
                        ..RawSpan::default()
                    }]],
                }
            }
        }

        let renderer = ClientOnly {
            calls: Cell::new(0),
        };
        let cache = Cache::default();
        let args = serde_json::json!({});
        let row = input(&args);
        for version in ["v1", "v2"] {
            let ctx = SessionCtx {
                session_id: Some(contract::SessionId("s".into())),
                halves: Some(
                    [("other".to_owned(), version.to_owned())]
                        .into_iter()
                        .collect(),
                ),
            };
            // The default reports no half even as the map around it changes.
            assert_eq!(renderer.half(&ctx, "client"), None);
            let half = renderer.half(&ctx, "client");
            assert!(
                cache
                    .lines(page("s", 0), "ledger_row", "client", half, &row, 20, || {
                        renderer.render(&ctx, "ledger_row", &row, 20, &|_, _| Vec::new())
                    })
                    .is_some()
            );
        }
        // docs/tui.md:2684: an unrelated session-half change does not rerun
        // the renderer, so the second context hits the first entry.
        assert_eq!(renderer.calls.get(), 1);
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
        // docs/tui.md:2683: only output-relevant page/winner/half/input/width changes rerun.
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
        // docs/tui.md:2683: a change to any key input reruns the renderer.
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
        assert_eq!(calls.get(), 5); // docs/tui.md:2684: failure key excludes width.
        cache.queue_notice(("command_conflict".into(), "conflict".into()));
        cache.drop_page(page("s", 0));
        assert_eq!(cache.take_failures().len(), 2); // docs/tui.md:2682: only the exact page is pruned.
        assert_eq!(cache.take_notices().len(), 1); // docs/tui.md:2688: selection notices live until /reload, with no page lifetime.
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
        // docs/tui.md:2745: one failure notice per extension and slot until /reload.
        assert_eq!(durable.take_failures().len(), 1);
        // docs/tui.md:2688: one extension_incompatible per extension and session-half version until /reload.
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
        // docs/tui.md:2745: one failure notice per extension and slot until /reload.
        assert_eq!(durable.take_failures().len(), 1);
        // docs/tui.md:2772: one command_conflict per slot and set of contenders until /reload.
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
        // docs/tui.md:2745: one failure notice per extension and slot until /reload.
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

/// A card input with one reply entry: the scroll/search cache tests draw
/// this twice and count one renderer call.
fn card(running: bool) -> TurnCard<'static> {
    TurnCard {
        prompts: vec![CardPrompt {
            text: "go",
            images: vec![PromptImage {
                path: "a.png",
                width: 10,
                height: 8,
            }],
        }],
        entries: vec![crate::ext::TurnEntry::Reply { text: "ok" }],
        started: 1,
        calls: 0,
        usage: contract::shapes::Usage {
            tokens: contract::shapes::Tokens {
                input: 0,
                cache_read: 0,
                cache_write: std::collections::BTreeMap::new(),
                output: 0,
            },
            cost: None,
            subscription_cost: 0.0,
        },
        state: if running {
            TurnCardState::Running
        } else {
            TurnCardState::Ended(TurnEnded::Completed { ended_ts: 0 })
        },
        flat: false,
    }
}

fn prompt_input(text: &'static str) -> Input<'static> {
    Input::Prompt(Prompt {
        text,
        images: Vec::new(),
        flat: false,
    })
}

fn reply_input(text: &'static str) -> Input<'static> {
    Input::Reply(ReplyText { text, flat: false })
}

fn thought_input(text: &'static str, open: bool) -> Input<'static> {
    Input::ThinkingLine(ThinkingLine {
        text,
        started: 1,
        ended: Some(2),
        open,
        flat: false,
    })
}

fn card_input(running: bool) -> Input<'static> {
    Input::TurnCard(card(running))
}

fn group_line_input(running: bool) -> Input<'static> {
    Input::GroupLine(crate::ext::GroupLine {
        running,
        calls: Vec::new(),
        streaming: Vec::new(),
        thoughts: Vec::new(),
        failed: Vec::new(),
        first: 1,
        last: 2,
        open: false,
        flat: false,
    })
}

fn steer_input(text: &'static str) -> Input<'static> {
    Input::Steer(crate::ext::Steer {
        text,
        images: Vec::new(),
        ts: 1,
        flat: false,
    })
}

fn answers_input(note: Option<&'static str>) -> Input<'static> {
    Input::Answers(crate::ext::Answers {
        answers: crate::ext::TurnAnswers::Given {
            parts: vec![crate::ext::TurnAnswerPart {
                header: "Base",
                disposition: AnswerDisposition::Answered { text: "main" },
            }],
            note,
            ts: 1,
        },
        flat: false,
    })
}

fn handoff_input(trigger: &'static str) -> Input<'static> {
    Input::HandoffBand(crate::ext::HandoffBand {
        trigger,
        ts: 1,
        state: crate::ext::TurnBandState::Done(crate::ext::TurnSizes {
            before: 1,
            after: Some(2),
        }),
        flat: false,
    })
}

fn error_input(message: &'static str) -> Input<'static> {
    Input::ErrorLine(crate::ext::ErrorLine {
        code: contract::ErrorCode::IoFailed,
        message,
        provider: None,
        flat: false,
    })
}

fn notice_input(text: &'static str) -> Input<'static> {
    Input::Notice(crate::ext::Notice {
        code: "io_failed",
        text,
        flat: false,
    })
}

fn working_line(started: Option<u64>, flat: bool) -> Input<'static> {
    Input::WorkingLine(WorkingLine {
        started,
        retry: None,
        flat,
    })
}

fn working_retry() -> Input<'static> {
    Input::WorkingLine(WorkingLine {
        started: Some(7),
        retry: Some(WorkingRetry {
            code: contract::ErrorCode::RateLimited,
            attempt: 2,
            last_attempt: 4,
            delay_ms: 4000,
            ts: 9,
        }),
        flat: false,
    })
}

fn steering_message(text: &'static str, selected: bool) -> SteeringMessage<'static> {
    SteeringMessage {
        text,
        images: Vec::new(),
        droppable: true,
        selectable: true,
        selected,
    }
}

fn steering_queue(messages: Vec<SteeringMessage<'static>>, flat: bool) -> Input<'static> {
    Input::SteeringQueue(SteeringQueue { messages, flat })
}

fn usage(cost: Option<f64>) -> contract::shapes::Usage {
    contract::shapes::Usage {
        tokens: contract::shapes::Tokens {
            input: 1,
            output: 2,
            cache_read: 3,
            cache_write: std::collections::BTreeMap::new(),
        },
        cost,
        subscription_cost: 0.5,
    }
}

fn session_card<'a>(
    model: Option<&'a str>,
    spend: Option<Spend<'a>>,
    tools_down: &'a std::collections::BTreeSet<String>,
    waiting: Option<usize>,
) -> SessionCard<'a> {
    SessionCard {
        workspace: Some("~/work/fiber"),
        branch: Some(BranchName::Named("main")),
        model,
        thinking: Some("high"),
        thinking_now: false,
        context: Some(SessionContext {
            tokens: 268000,
            window: 1000000,
            trigger_at: Some(400000),
        }),
        spend,
        cache_expires: None,
        budget: Some(20.0),
        cache_expires: None,
        speed: Some(64),
        turns: 12,
        tools_down,
        waiting,
        flat: false,
    }
}

fn status_rows<'a>(
    session: SessionCard<'a>,
    files: &'a [ChangedFile<'a>],
    waiting: Option<usize>,
    quota: Option<Quota<'a>>,
) -> Input<'a> {
    Input::StatusRows(StatusRows {
        cards: vec!["session".to_owned(), "changed_files".to_owned()],
        session,
        changed_files: ChangedFiles { files, flat: false },
        delegates_running: 1,
        jobs_running: 0,
        widgets: Vec::new(),
        waiting,
        quota,
        flat: false,
    })
}

fn quota_info(provider: &str) -> contract::events::QuotaInfo {
    contract::events::QuotaInfo {
        provider: provider.to_owned(),
        credential: "work".to_owned(),
        windows: Some(vec![contract::events::QuotaWindow {
            name: "weekly".to_owned(),
            percent_used: Some(35.0),
            reset: None,
            resets_at: None,
        }]),
        credit: None,
        unreachable: None,
    }
}

fn delegate(
    job_id: &'static str,
    outcome: Option<contract::events::Outcome>,
    ended: Option<u64>,
) -> Delegate<'static> {
    Delegate {
        job_id,
        description: "review the diff",
        harness: "fiber",
        model: "m",
        calls: 14,
        step: Some("read app.rs"),
        started: Some(1720000000000),
        outcome,
        ended,
    }
}

#[test]
fn each_chrome_slot_caches_by_input_and_width() {
    crate::work::take();
    let cache = Cache::default();
    let calls = Cell::new(0usize);
    let tools_down = std::collections::BTreeSet::new();
    let spend = usage(Some(4.12));
    let files = [
        ChangedFile {
            path: "crates/tui/src/app.rs",
            added: 38,
            removed: 9,
        },
        ChangedFile {
            path: "crates/tui/src/view.rs",
            added: 1,
            removed: 2,
        },
    ];
    let jobs = [Job {
        job_id: "7",
        description: "bench",
    }];
    let lines: Vec<String> = vec!["one".to_owned()];
    let cases: Vec<(&str, Input<'_>, Input<'_>)> = vec![
        (
            "working_line",
            working_line(Some(1), false),
            working_line(Some(2), false),
        ),
        (
            "working_line",
            working_line(Some(7), false),
            working_retry(),
        ),
        (
            "steering_queue",
            steering_queue(vec![steering_message("fix the flake", false)], false),
            steering_queue(vec![steering_message("fix the flake", true)], false),
        ),
        (
            "status_rows",
            status_rows(
                session_card(Some("m"), Some(Spend::Status(&spend)), &tools_down, None),
                &files,
                None,
                None,
            ),
            status_rows(
                session_card(Some("m"), Some(Spend::Status(&spend)), &tools_down, None),
                &files,
                Some(2),
                None,
            ),
        ),
        (
            "card:session",
            Input::Card(Card::Session(session_card(
                Some("m"),
                Some(Spend::Status(&spend)),
                &tools_down,
                None,
            ))),
            Input::Card(Card::Session(session_card(
                Some("n"),
                Some(Spend::Status(&spend)),
                &tools_down,
                None,
            ))),
        ),
        (
            "card:changed_files",
            Input::Card(Card::ChangedFiles(ChangedFiles {
                files: &files,
                flat: false,
            })),
            Input::Card(Card::ChangedFiles(ChangedFiles {
                files: &files[1..],
                flat: false,
            })),
        ),
        (
            "card:delegates",
            Input::Card(Card::Delegates(DelegatesCard {
                delegates: vec![delegate("3", None, None)],
                flat: false,
            })),
            Input::Card(Card::Delegates(DelegatesCard {
                delegates: vec![delegate(
                    "3",
                    Some(contract::events::Outcome::Completed),
                    Some(1720000001000),
                )],
                flat: false,
            })),
        ),
        (
            "card:jobs",
            Input::Card(Card::Jobs(JobsCard {
                jobs: &jobs,
                open: false,
                flat: false,
            })),
            Input::Card(Card::Jobs(JobsCard {
                jobs: &jobs,
                open: true,
                flat: false,
            })),
        ),
        (
            "card:e/w",
            Input::Card(Card::Widget(WidgetCard {
                extension: "e",
                widget: "w",
                lines: &lines,
                open: false,
                flat: false,
            })),
            Input::Card(Card::Widget(WidgetCard {
                extension: "e",
                widget: "w",
                lines: &lines,
                open: true,
                flat: false,
            })),
        ),
    ];
    for (slot, first, second) in &cases {
        let before = calls.get();
        assert!(
            draw_counted(&cache, slot, first, 20, &calls, drawn("hi")).is_some(),
            "{slot} draws"
        );
        assert!(
            draw_counted(&cache, slot, first, 20, &calls, drawn("hi")).is_some(),
            "{slot} hits"
        );
        // A renderer runs again only when its input or the width
        // changes (`docs/tui.md`, "How a TUI extension runs").
        assert_eq!(calls.get(), before + 1, "{slot} hits the cache");
        assert!(
            draw_counted(&cache, slot, first, 24, &calls, drawn("hi")).is_some(),
            "{slot} draws at a new width"
        );
        // The width rides the entry key beside the input
        // (`docs/tui.md`, "How a TUI extension runs").
        assert_eq!(calls.get(), before + 2, "{slot} keys the width");
        assert!(
            draw_counted(&cache, slot, second, 20, &calls, drawn("hi")).is_some(),
            "{slot} draws a new input"
        );
        // The key covers the slot and every field
        // (`docs/tui.md`, "How a TUI extension runs").
        assert_eq!(calls.get(), before + 3, "{slot} keys the input");
    }
}

/// The session card's key covers the cache expiry: the same card with
/// a new expiry misses, while an unchanged expiry hits, so a re-pinned
/// countdown never shares one cache entry with the old text
/// (`docs/tui.md`, "How a TUI extension runs").
#[test]
fn the_session_card_key_covers_cache_expires() {
    let tools_down = std::collections::BTreeSet::new();
    let spend = usage(Some(4.12));
    let card = session_card(Some("m"), Some(Spend::Status(&spend)), &tools_down, None);
    let first = card.key("card:session");
    assert_eq!(first, card.key("card:session"), "same expiry hits");
    let mut repinned = session_card(Some("m"), Some(Spend::Status(&spend)), &tools_down, None);
    repinned.cache_expires = Some(1_754_000_000_000);
    assert_ne!(first, repinned.key("card:session"), "new expiry misses");
}

/// The session card's key covers the thinking flag: the same card with
/// the flag flipped misses, so a shown and a hidden `∴` never share one
/// cache entry (`docs/tui.md`, "How a TUI extension runs").
#[test]
fn the_session_card_key_covers_thinking_now() {
    let tools_down = std::collections::BTreeSet::new();
    let spend = usage(Some(4.12));
    let mut card = session_card(Some("m"), Some(Spend::Status(&spend)), &tools_down, None);
    card.thinking_now = false;
    let calm = card.key("card:session");
    card.thinking_now = true;
    assert_ne!(calm, card.key("card:session"), "flipped flag misses");
}

#[test]
fn chrome_keys_cover_flat_order_slot_and_spend_shape() {
    crate::work::take();
    let cache = Cache::default();
    let calls = Cell::new(0usize);
    // Flat rides the key: the same working line draws again flat.
    let before = calls.get();
    assert!(
        draw_counted(
            &cache,
            "working_line",
            &working_line(Some(1), false),
            20,
            &calls,
            drawn("hi")
        )
        .is_some()
    );
    assert!(
        draw_counted(
            &cache,
            "working_line",
            &working_line(Some(1), false),
            20,
            &calls,
            drawn("hi")
        )
        .is_some()
    );
    assert_eq!(calls.get(), before + 1, "same input hits");
    assert!(
        draw_counted(
            &cache,
            "working_line",
            &working_line(Some(1), true),
            20,
            &calls,
            drawn("hi")
        )
        .is_some()
    );
    assert_eq!(calls.get(), before + 2, "flat misses");
    // Nested order matters: the same messages newest-first miss.
    let before = calls.get();
    let oldest = steering_queue(
        vec![
            steering_message("one", false),
            steering_message("two", false),
        ],
        false,
    );
    let newest = steering_queue(
        vec![
            steering_message("two", false),
            steering_message("one", false),
        ],
        false,
    );
    assert!(draw_counted(&cache, "steering_queue", &oldest, 20, &calls, drawn("hi")).is_some());
    assert!(draw_counted(&cache, "steering_queue", &newest, 20, &calls, drawn("hi")).is_some());
    assert_eq!(calls.get(), before + 2, "message order misses");
    // Images ride the key: the same text with an image misses.
    let before = calls.get();
    let imaged = SteeringMessage {
        text: "one",
        images: vec![PromptImage {
            path: "artifacts/a.png",
            width: 8,
            height: 8,
        }],
        droppable: true,
        selectable: false,
        selected: false,
    };
    let with_image = steering_queue(vec![imaged], false);
    assert!(
        draw_counted(
            &cache,
            "steering_queue",
            &with_image,
            20,
            &calls,
            drawn("hi")
        )
        .is_some()
    );
    assert_eq!(calls.get(), before + 1, "images miss");
    // The slot rides the key: the same working line under another slot
    // misses.
    let before = calls.get();
    let line = working_line(Some(1), false);
    assert!(draw_counted(&cache, "working_line", &line, 20, &calls, drawn("hi")).is_some());
    assert_eq!(calls.get(), before, "same slot hits");
    assert!(draw_counted(&cache, "card:session", &line, 20, &calls, drawn("hi")).is_some());
    assert_eq!(calls.get(), before + 1, "another slot misses");
    // The two spend shapes key apart: the status spend whole against
    // the attached fallback's cost alone.
    let before = calls.get();
    let tools_down = std::collections::BTreeSet::new();
    let spend = usage(Some(4.12));
    let whole = Input::Card(Card::Session(session_card(
        Some("m"),
        Some(Spend::Status(&spend)),
        &tools_down,
        None,
    )));
    let fallback = Input::Card(Card::Session(session_card(
        Some("m"),
        Some(Spend::Attached { cost: 4.12 }),
        &tools_down,
        None,
    )));
    assert!(draw_counted(&cache, "card:session", &whole, 20, &calls, drawn("hi")).is_some());
    assert!(draw_counted(&cache, "card:session", &fallback, 20, &calls, drawn("hi")).is_some());
    assert_eq!(calls.get(), before + 2, "spend shapes miss");
    // Quota presence keys, and its rows keep their order.
    let before = calls.get();
    let files: [ChangedFile<'_>; 0] = [];
    let codex = [quota_info("codex")];
    let both = [quota_info("codex"), quota_info("openrouter")];
    let swapped = [quota_info("openrouter"), quota_info("codex")];
    let bare = status_rows(
        session_card(Some("m"), None, &tools_down, None),
        &files,
        None,
        None,
    );
    let quoted = status_rows(
        session_card(Some("m"), None, &tools_down, None),
        &files,
        None,
        Some(Quota {
            rows: &codex,
            notice_at: 80.0,
        }),
    );
    let ordered = status_rows(
        session_card(Some("m"), None, &tools_down, None),
        &files,
        None,
        Some(Quota {
            rows: &both,
            notice_at: 80.0,
        }),
    );
    let reordered = status_rows(
        session_card(Some("m"), None, &tools_down, None),
        &files,
        None,
        Some(Quota {
            rows: &swapped,
            notice_at: 80.0,
        }),
    );
    assert!(draw_counted(&cache, "status_rows", &bare, 20, &calls, drawn("hi")).is_some());
    assert!(draw_counted(&cache, "status_rows", &quoted, 20, &calls, drawn("hi")).is_some());
    assert!(draw_counted(&cache, "status_rows", &ordered, 20, &calls, drawn("hi")).is_some());
    assert!(draw_counted(&cache, "status_rows", &reordered, 20, &calls, drawn("hi")).is_some());
    assert_eq!(calls.get(), before + 4, "quota presence and order miss");
}

#[test]
fn chrome_spend_cost_is_in_the_key() {
    crate::work::take();
    let cache = Cache::default();
    let calls = Cell::new(0usize);
    let tools_down = std::collections::BTreeSet::new();
    let low = usage(Some(4.12));
    let high = usage(Some(4.13));
    let cheap = Input::Card(Card::Session(session_card(
        Some("m"),
        Some(Spend::Status(&low)),
        &tools_down,
        None,
    )));
    let dear = Input::Card(Card::Session(session_card(
        Some("m"),
        Some(Spend::Status(&high)),
        &tools_down,
        None,
    )));
    let first = draw_counted(&cache, "card:session", &cheap, 20, &calls, drawn("hi"))
        .expect("a cached line");
    assert!(text_of(&first).starts_with("hi"));
    assert!(draw_counted(&cache, "card:session", &cheap, 20, &calls, drawn("hi")).is_some());
    assert_eq!(calls.get(), 1, "same spend hits");
    assert!(draw_counted(&cache, "card:session", &dear, 20, &calls, drawn("hi")).is_some());
    // Only the session spend's cost differs, so a miss proves the key
    // carries the float (`docs/tui.md`, "How a TUI extension runs").
    assert_eq!(calls.get(), 2, "a changed cost misses");
}

#[test]
fn chrome_delegate_outcomes_key_apart() {
    crate::work::take();
    let cache = Cache::default();
    let calls = Cell::new(0usize);
    let ended = Some(1720000001000);
    for outcome in [
        contract::events::Outcome::Completed,
        contract::events::Outcome::Failed,
        contract::events::Outcome::Cancelled,
    ] {
        let card = Input::Card(Card::Delegates(DelegatesCard {
            delegates: vec![delegate("3", Some(outcome), ended)],
            flat: false,
        }));
        assert!(
            draw_counted(&cache, "card:delegates", &card, 20, &calls, drawn("hi")).is_some(),
            "{outcome:?} draws"
        );
    }
    // Every field but the outcome matches, and every outcome is `Some`,
    // so three misses prove the key reaches the variant
    // (`docs/tui.md`, "How a TUI extension runs").
    assert_eq!(calls.get(), 3, "each outcome misses");
}

#[test]
fn chrome_card_flat_drops_the_style_through_the_closed_path() {
    crate::work::take();
    let cache = Cache::default();
    let calls = Cell::new(0usize);
    let lines = vec!["hi".to_owned()];
    let card = |flat: bool| {
        Input::Card(Card::Widget(WidgetCard {
            extension: "e",
            widget: "w",
            lines: &lines,
            open: false,
            flat,
        }))
    };
    let styled = vec![vec![RawSpan {
        text: "hi".to_owned(),
        fg: Some("accent".to_owned()),
        ..RawSpan::default()
    }]];
    let draw = |flat: bool| {
        cache
            .lines_animated_closed(
                page(0),
                "card:e/w",
                "test",
                None,
                &card(flat),
                20,
                |frame| {
                    calls.set(calls.get() + 1);
                    assert_eq!(frame, 0);
                    (
                        Decision::Draw {
                            extension: "test".into(),
                            lines: styled.clone(),
                        },
                        true,
                    )
                },
            )
            .expect("a fitted line")
    };
    let vivid = draw(false);
    assert!(text_of(&vivid).starts_with("hi"));
    assert_ne!(
        vivid
            .first()
            .expect("one line")
            .line
            .spans
            .first()
            .expect("one span")
            .style,
        ratatui::style::Style::default(),
        "a styled span keeps its style"
    );
    let flat = draw(true);
    assert!(text_of(&flat).starts_with("hi"));
    // (`docs/tui.md`, "The slots": `flat` is true when the terminal drops every style.)
    assert_eq!(
        flat.first()
            .expect("one line")
            .line
            .spans
            .first()
            .expect("one span")
            .style,
        ratatui::style::Style::default(),
        "flat drops the style"
    );
    assert_eq!(calls.get(), 2, "flat misses");
}

/// Draws `row` at `width`, counting renderer calls.
fn draw_counted<'a>(
    cache: &Cache,
    slot: &str,
    row: &'a Input<'a>,
    width: u16,
    calls: &Cell<usize>,
    answer: Result<Vec<Vec<RawSpan>>, Failure>,
) -> Option<std::sync::Arc<Vec<crate::ext::spans::FittedLine>>> {
    cache.lines(page(0), slot, "test", None, row, width, || {
        decision(|| {
            calls.set(calls.get().saturating_add(1));
            answer.clone()
        })
    })
}

fn drawn(text: &str) -> Result<Vec<Vec<RawSpan>>, Failure> {
    Ok(vec![vec![raw(text)]])
}

#[test]
fn each_new_slot_caches_by_input_and_width() {
    crate::work::take();
    let cache = Cache::default();
    let calls = Cell::new(0usize);
    let arguments = serde_json::json!({});
    let _ = &arguments;
    let cases: Vec<(&str, Input<'_>, Input<'_>)> = vec![
        ("prompt", prompt_input("go"), prompt_input("stop")),
        ("reply", reply_input("ok"), reply_input("changed")),
        (
            "thinking_line",
            thought_input("plan", false),
            thought_input("plan", true),
        ),
        ("turn_card", card_input(false), card_input(true)),
        (
            "group_line",
            group_line_input(false),
            group_line_input(true),
        ),
        ("steer", steer_input("wait"), steer_input("stop")),
        ("answers", answers_input(Some("go")), answers_input(None)),
        (
            "handoff_band",
            handoff_input("automatic"),
            handoff_input("the model handed off"),
        ),
        ("error_line", error_input("down"), error_input("up")),
        ("notice", notice_input("down"), notice_input("up")),
    ];
    for (slot, first, second) in &cases {
        let before = calls.get();
        assert!(
            draw_counted(&cache, slot, first, 20, &calls, drawn("hi")).is_some(),
            "{slot} draws"
        );
        assert!(
            draw_counted(&cache, slot, first, 20, &calls, drawn("hi")).is_some(),
            "{slot} hits"
        );
        // A renderer runs again only when its input or the width
        // changes (`docs/tui.md`, "How a TUI extension runs").
        assert_eq!(calls.get(), before + 1, "{slot} hits the cache");
        assert!(
            draw_counted(&cache, slot, first, 24, &calls, drawn("hi")).is_some(),
            "{slot} draws at a new width"
        );
        // The width rides the entry key beside the input
        // (`docs/tui.md`, "How a TUI extension runs").
        assert_eq!(calls.get(), before + 2, "{slot} keys the width");
        assert!(
            draw_counted(&cache, slot, second, 20, &calls, drawn("hi")).is_some(),
            "{slot} draws a new input"
        );
        // The key covers the slot and every field
        // (`docs/tui.md`, "How a TUI extension runs").
        assert_eq!(calls.get(), before + 3, "{slot} keys the input");
    }
}

#[test]
fn each_new_slot_failure_holds_without_width() {
    crate::work::take();
    let cache = Cache::default();
    let calls = Cell::new(0usize);
    let cases: Vec<(&str, Input<'_>)> = vec![
        ("prompt", prompt_input("go")),
        ("reply", reply_input("ok")),
        ("thinking_line", thought_input("plan", false)),
        ("turn_card", card_input(false)),
        ("group_line", group_line_input(false)),
        ("steer", steer_input("wait")),
        ("answers", answers_input(Some("go"))),
        ("handoff_band", handoff_input("automatic")),
        ("error_line", error_input("down")),
        ("notice", notice_input("down")),
    ];
    for (slot, row) in &cases {
        let before = calls.get();
        let fail = || draw_counted(&cache, slot, row, 20, &calls, Err(failed("test", "boom")));
        assert!(fail().is_none(), "{slot} falls back");
        assert!(fail().is_none(), "{slot} recalls nothing");
        assert!(
            draw_counted(&cache, slot, row, 24, &calls, Err(failed("test", "boom"))).is_none(),
            "{slot} holds across widths"
        );
        // The failure key holds without the width
        // (`docs/tui.md`, "How a TUI extension runs").
        assert_eq!(calls.get(), before + 1, "{slot} calls once");
        let listed = cache.take_failures();
        // (`docs/tui.md`, "When an extension fails": one notice names the extension and slot once until `/reload`.)
        assert_eq!(listed.len(), 1, "{slot} lists its failure once");
        assert_eq!(listed[0].0, *slot);
    }
}

#[test]
fn a_new_slot_wrong_shape_is_listed_once() {
    crate::work::take();
    let cache = Cache::default();
    let row = prompt_input("go");
    let wrong = || {
        cache.lines(page(0), "prompt", "test", None, &row, 20, || {
            decision(|| {
                Ok(vec![vec![RawSpan {
                    text: "hi".to_owned(),
                    fg: Some("nope".to_owned()),
                    ..RawSpan::default()
                }]])
            })
        })
    };
    assert!(wrong().is_none());
    assert!(wrong().is_none());
    let listed = cache.take_failures();
    // (`docs/tui.md`, "When an extension fails": one notice names the extension and slot once until `/reload`.)
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].0, "prompt");
}

#[test]
fn scroll_and_search_redraws_hit() {
    crate::work::take();
    let cache = Cache::default();
    let calls = Cell::new(0usize);
    let row = card_input(false);
    for _ in 0..2 {
        assert!(
            draw_counted(&cache, "turn_card", &row, 20, &calls, drawn("hi")).is_some(),
            "a redraw hits"
        );
    }
    // Scrolling or searching costs what it costs for built-in rows:
    // no new renderer call (`docs/tui.md`, "How a TUI extension runs").
    assert_eq!(calls.get(), 1, "scrolling and searching call Lua once");
    assert_eq!(crate::work::take().renderer_calls, 1);
}

/// The animated answers a test draws: every frame it drew, whether the
/// next answer stops, and whether it fails instead.
struct AnimAnswers {
    frames: std::cell::RefCell<Vec<u64>>,
    stop: Cell<bool>,
    fail: Cell<bool>,
}

impl AnimAnswers {
    fn new() -> Self {
        Self {
            frames: std::cell::RefCell::new(Vec::new()),
            stop: Cell::new(false),
            fail: Cell::new(false),
        }
    }
}

const ANIM_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

fn origin() -> std::time::Instant {
    fakes::clock::FakeClock::new().origin()
}

fn at(origin: std::time::Instant, millis: u64) -> std::time::Instant {
    origin
        .checked_add(std::time::Duration::from_millis(millis))
        .expect("after the origin")
}

/// Paints `row` through the animation schedule at `width`, recording
/// each frame drawn.
fn paint(
    cache: &Cache,
    row: &Input<'_>,
    width: u16,
    answers: &AnimAnswers,
) -> Option<Arc<Vec<super::super::spans::FittedLine>>> {
    cache.lines_animated(
        page(0),
        "ledger_row",
        "test",
        None,
        row,
        width,
        ANIM_INTERVAL,
        |frame| {
            answers.frames.borrow_mut().push(frame);
            if answers.fail.get() {
                return (Decision::Fail(failed("test", "boom")), false);
            }
            let lines = vec![vec![raw(&format!("f{frame}"))]];
            (
                Decision::Draw {
                    extension: "test".into(),
                    lines,
                },
                !answers.stop.get(),
            )
        },
    )
}

/// The animated item's key from its painted lines.
fn tag_of(lines: &Arc<Vec<super::super::spans::FittedLine>>) -> super::super::anim::AnimKey {
    lines
        .first()
        .expect("one fitted line")
        .anim
        .clone()
        .expect("an animated line is tagged")
}

fn text_of(lines: &Arc<Vec<super::super::spans::FittedLine>>) -> String {
    lines.first().expect("one fitted line").line.to_string()
}

#[test]
fn an_animated_paint_starts_at_frame_zero() {
    crate::work::take();
    let cache = Cache::default();
    let answers = AnimAnswers::new();
    let arguments = serde_json::json!({"command": "ls"});
    let row = input("shell", "first", &arguments);
    let t0 = origin();
    cache.begin_anim_frame(Some(t0), false);
    let fitted = paint(&cache, &row, 20, &answers).expect("a painted line");
    assert_eq!(crate::work::take().renderer_calls, 1); // docs/tui.md:2924: one call per cache miss.
    assert_eq!(answers.frames.borrow().clone(), vec![0]); // docs/tui.md:2989: frame 0 is the still form.
    assert!(text_of(&fitted).starts_with("f0"), "{fitted:?}");
    assert!(fitted.first().expect("one line").anim.is_some());
    // A repaint in the same frame hits: no call before the paint and
    // the due both arrive.
    cache.mark_painted(&[tag_of(&fitted)]);
    assert_eq!(cache.anim_dues(), vec![(t0, ANIM_INTERVAL)]);
    cache.begin_anim_frame(Some(t0), false);
    let again = paint(&cache, &row, 20, &answers).expect("a painted line");
    assert_eq!(answers.frames.borrow().clone(), vec![0]); // docs/tui.md:2989: frame 0 is the still form.
    assert_eq!(text_of(&again), text_of(&fitted));
}

#[test]
fn an_animated_item_advances_only_when_due_and_painted() {
    crate::work::take();
    let cache = Cache::default();
    let answers = AnimAnswers::new();
    let arguments = serde_json::json!({"command": "ls"});
    let row = input("shell", "first", &arguments);
    let t0 = origin();
    cache.begin_anim_frame(Some(t0), false);
    let first = paint(&cache, &row, 20, &answers).expect("frame 0");
    cache.mark_painted(&[tag_of(&first)]);
    assert_eq!(cache.anim_dues(), vec![(t0, ANIM_INTERVAL)]);
    // Before the interval the paint hits its stored frame: no call.
    cache.begin_anim_frame(Some(at(t0, 100)), false);
    let early = paint(&cache, &row, 20, &answers).expect("stored frame 0");
    assert_eq!(answers.frames.borrow().clone(), vec![0]); // docs/tui.md:2989: frame 0 is the still form.
    assert_eq!(text_of(&early), text_of(&first));
    cache.mark_painted(&[tag_of(&early)]);
    assert_eq!(cache.anim_dues(), vec![(t0, ANIM_INTERVAL)]);
    // At the interval the paint advances: frame 1, last moves.
    cache.begin_anim_frame(Some(at(t0, 500)), false);
    let second = paint(&cache, &row, 20, &answers).expect("frame 1");
    assert_eq!(answers.frames.borrow().clone(), vec![0, 1]); // docs/tui.md:2983: calls follow paint in the previous frame.
    assert!(text_of(&second).starts_with("f1"), "{second:?}");
    cache.mark_painted(&[tag_of(&second)]);
    assert_eq!(cache.anim_dues(), vec![(at(t0, 500), ANIM_INTERVAL)]);
}

#[test]
fn an_unmarked_draw_never_advances_or_schedules() {
    crate::work::take();
    let cache = Cache::default();
    let answers = AnimAnswers::new();
    let arguments = serde_json::json!({"command": "ls"});
    let row = input("shell", "first", &arguments);
    let t0 = origin();
    cache.begin_anim_frame(Some(t0), false);
    paint(&cache, &row, 20, &answers).expect("frame 0");
    // Nothing painted: no due, and the next frame holds still.
    assert!(cache.anim_dues().is_empty());
    cache.begin_anim_frame(Some(at(t0, 500)), false);
    let still = paint(&cache, &row, 20, &answers).expect("stored frame 0");
    assert_eq!(answers.frames.borrow().clone(), vec![0]); // docs/tui.md:2989: frame 0 is the still form.
    assert!(text_of(&still).starts_with("f0"), "{still:?}");
    assert!(cache.anim_dues().is_empty());
}

#[test]
fn an_absent_item_holds_no_due_then_resumes_without_restarting() {
    crate::work::take();
    let cache = Cache::default();
    let answers = AnimAnswers::new();
    let arguments = serde_json::json!({"command": "ls"});
    let row = input("shell", "first", &arguments);
    let t0 = origin();
    cache.begin_anim_frame(Some(t0), false);
    let first = paint(&cache, &row, 20, &answers).expect("frame 0");
    cache.mark_painted(&[tag_of(&first)]);
    assert_eq!(cache.anim_dues(), vec![(t0, ANIM_INTERVAL)]);
    // Two frames absent: no paint, no due.
    for millis in [500, 1000] {
        cache.begin_anim_frame(Some(at(t0, millis)), false);
        assert!(cache.anim_dues().is_empty(), "absent at {millis} ms");
    }
    // Back on screen: hits its stored frame with no recall, and the
    // counter resumes instead of restarting.
    cache.begin_anim_frame(Some(at(t0, 1500)), false);
    let back = paint(&cache, &row, 20, &answers).expect("stored frame 0");
    assert_eq!(answers.frames.borrow().clone(), vec![0]); // docs/tui.md:2989: frame 0 is the still form.
    assert_eq!(text_of(&back), text_of(&first));
    cache.mark_painted(&[tag_of(&back)]);
    // The stored paint changes nothing: the due still names the last
    // tick.
    assert_eq!(cache.anim_dues(), vec![(t0, ANIM_INTERVAL)]);
    cache.begin_anim_frame(Some(at(t0, 2000)), false);
    let second = paint(&cache, &row, 20, &answers).expect("frame 1");
    assert_eq!(answers.frames.borrow().clone(), vec![0, 1]); // docs/tui.md:2985: one scroll-off call, then resume at the stored frame.
    assert!(text_of(&second).starts_with("f1"), "{second:?}");
}

#[test]
fn a_stopped_item_hits_stored_lines_until_its_input_changes() {
    crate::work::take();
    let cache = Cache::default();
    let answers = AnimAnswers::new();
    answers.stop.set(true);
    let arguments = serde_json::json!({"command": "ls"});
    let row = input("shell", "first", &arguments);
    let t0 = origin();
    cache.begin_anim_frame(Some(t0), false);
    let first = paint(&cache, &row, 20, &answers).expect("the stopping draw");
    assert_eq!(answers.frames.borrow().clone(), vec![0]); // docs/tui.md:2989: frame 0 is the still form.
    // Stopped: repaints hit with no recall, and ask for no wake.
    for millis in [500, 1000, 1500] {
        cache.begin_anim_frame(Some(at(t0, millis)), false);
        let still = paint(&cache, &row, 20, &answers).expect("stored lines");
        assert_eq!(text_of(&still), text_of(&first));
        cache.mark_painted(&[tag_of(&still)]);
        assert!(cache.anim_dues().is_empty(), "stopped at {millis} ms");
    }
    assert_eq!(answers.frames.borrow().clone(), vec![0]); // docs/tui.md:2991: `animate = false` stays stopped until the input changes.
    // A changed input starts a fresh item at frame 0.
    let other = input("shell", "second", &arguments);
    cache.begin_anim_frame(Some(at(t0, 2000)), false);
    paint(&cache, &other, 20, &answers).expect("a fresh frame 0");
    assert_eq!(answers.frames.borrow().clone(), vec![0, 0]); // docs/tui.md:2991: `animate = false` stays stopped until the input changes.
}

#[test]
fn a_stopped_item_is_not_recalled_for_a_width_change_alone() {
    crate::work::take();
    let cache = Cache::default();
    let answers = AnimAnswers::new();
    answers.stop.set(true);
    let arguments = serde_json::json!({"command": "ls"});
    let row = input("shell", "first", &arguments);
    let t0 = origin();
    cache.begin_anim_frame(Some(t0), false);
    paint(&cache, &row, 20, &answers).expect("the stopping draw");
    // The width change itself recalls nothing.
    cache.drop_entries();
    assert_eq!(answers.frames.borrow().clone(), vec![0]); // docs/tui.md:2989: frame 0 is the still form.
    // On demand it re-renders at its stored frame without advancing.
    cache.begin_anim_frame(Some(at(t0, 500)), false);
    let narrow = paint(&cache, &row, 24, &answers).expect("stored frame 0");
    assert_eq!(answers.frames.borrow().clone(), vec![0, 0]); // docs/tui.md:2992: a width change redraws a stopped item at its stored frame.
    assert!(text_of(&narrow).starts_with("f0"), "{narrow:?}");
    cache.begin_anim_frame(Some(at(t0, 1000)), false);
    paint(&cache, &row, 24, &answers).expect("stored lines");
    assert_eq!(answers.frames.borrow().clone(), vec![0, 0]); // docs/tui.md:2992: a width change redraws a stopped item at its stored frame.
}

#[test]
fn a_stopped_item_redraws_at_its_stored_frame_after_advancing() {
    crate::work::take();
    let cache = Cache::default();
    let answers = AnimAnswers::new();
    let arguments = serde_json::json!({"command": "ls"});
    let row = input("shell", "first", &arguments);
    let t0 = origin();
    cache.begin_anim_frame(Some(t0), false);
    let first = paint(&cache, &row, 20, &answers).expect("frame 0");
    cache.mark_painted(&[tag_of(&first)]);
    assert_eq!(cache.anim_dues(), vec![(t0, ANIM_INTERVAL)]);
    cache.begin_anim_frame(Some(at(t0, 500)), false);
    let second = paint(&cache, &row, 20, &answers).expect("frame 1");
    // docs/tui.md:2977: the terminal calls the renderer again when the
    // frame advances, with the count in its input.
    assert_eq!(answers.frames.borrow().clone(), vec![0, 1]);
    cache.mark_painted(&[tag_of(&second)]);
    assert_eq!(cache.anim_dues(), vec![(at(t0, 500), ANIM_INTERVAL)]);
    // The renderer stops at the next tick: frame 2 draws once, then stills.
    answers.stop.set(true);
    cache.begin_anim_frame(Some(at(t0, 1000)), false);
    let third = paint(&cache, &row, 20, &answers).expect("frame 2 stops");
    // docs/tui.md:2991: a renderer that returns `animate = false` stays
    // stopped until its input changes.
    assert_eq!(answers.frames.borrow().clone(), vec![0, 1, 2]);
    assert!(text_of(&third).starts_with("f2"), "{third:?}");
    cache.mark_painted(&[tag_of(&third)]);
    assert!(cache.anim_dues().is_empty());
    // A width change re-renders at the stored frame 2, not at frame 0.
    // docs/tui.md:2992: a width change redraws a stopped item once at its
    // stored frame.
    cache.begin_anim_frame(Some(at(t0, 1500)), false);
    let narrow = paint(&cache, &row, 24, &answers).expect("stored frame 2");
    assert_eq!(answers.frames.borrow().clone(), vec![0, 1, 2, 2]); // docs/tui.md:2992: a width change redraws a stopped item at its stored frame.
    assert!(text_of(&narrow).starts_with("f2"), "{narrow:?}");
}

#[test]
fn a_width_change_recalls_at_the_stored_frame_then_advances() {
    crate::work::take();
    let cache = Cache::default();
    let answers = AnimAnswers::new();
    let arguments = serde_json::json!({"command": "ls"});
    let row = input("shell", "first", &arguments);
    let t0 = origin();
    cache.begin_anim_frame(Some(t0), false);
    paint(&cache, &row, 20, &answers).expect("frame 0");
    cache.drop_entries();
    // The recall reuses the stored frame with the counter, last,
    // markers and due unchanged.
    cache.begin_anim_frame(Some(at(t0, 100)), false);
    let recalled = paint(&cache, &row, 24, &answers).expect("stored frame 0");
    assert_eq!(answers.frames.borrow().clone(), vec![0, 0]); // docs/tui.md:2977: a width change alone advances nothing; the stored frame draws again.
    assert!(text_of(&recalled).starts_with("f0"), "{recalled:?}");
    cache.mark_painted(&[tag_of(&recalled)]);
    assert_eq!(cache.anim_dues(), vec![(t0, ANIM_INTERVAL)]);
    // At the interval the recalled item advances.
    cache.begin_anim_frame(Some(at(t0, 500)), false);
    let second = paint(&cache, &row, 24, &answers).expect("frame 1");
    assert_eq!(answers.frames.borrow().clone(), vec![0, 0, 1]); // docs/tui.md:2977: a width change alone advances nothing; the stored frame draws again.
    assert!(text_of(&second).starts_with("f1"), "{second:?}");
}

#[test]
fn a_new_frame_evicts_older_entries_at_every_width() {
    crate::work::take();
    let cache = Cache::default();
    let answers = AnimAnswers::new();
    let arguments = serde_json::json!({"command": "ls"});
    let row = input("shell", "first", &arguments);
    let t0 = origin();
    cache.begin_anim_frame(Some(t0), false);
    let wide = paint(&cache, &row, 20, &answers).expect("frame 0");
    let narrow = paint(&cache, &row, 24, &answers).expect("frame 0 again");
    assert_eq!(answers.frames.borrow().clone(), vec![0, 0]); // docs/tui.md:2989: frame 0 is the still form.
    cache.mark_painted(&[tag_of(&wide), tag_of(&narrow)]);
    assert_eq!(cache.anim_dues(), vec![(t0, ANIM_INTERVAL)]);
    assert!(
        cache
            .animated_lines(page(0), "ledger_row", "test", None, &row, 24)
            .is_some()
    );
    // Advancing stores the new frame and evicts every older same-item
    // entry at every width: only the latest frame is kept.
    cache.begin_anim_frame(Some(at(t0, 500)), false);
    paint(&cache, &row, 20, &answers).expect("frame 1");
    assert_eq!(answers.frames.borrow().clone(), vec![0, 0, 1]); // docs/tui.md:2977: the renderer is called again when the frame advances.
    assert!(
        cache
            .animated_lines(page(0), "ledger_row", "test", None, &row, 20)
            .is_some()
    );
    assert!(
        cache
            .animated_lines(page(0), "ledger_row", "test", None, &row, 24)
            .is_none()
    );
}

#[test]
fn a_changed_animated_line_count_queues_its_page_once() {
    crate::work::take();
    let cache = Cache::default();
    let arguments = serde_json::json!({"command": "ls"});
    let row = input("shell", "first", &arguments);
    let t0 = origin();
    let draw = |now, want| {
        cache.begin_anim_frame(Some(now), false);
        let lines = cache
            .lines_animated(
                page(0),
                "ledger_row",
                "test",
                None,
                &row,
                20,
                ANIM_INTERVAL,
                |frame| {
                    let count = if frame == 0 { 1 } else { want };
                    (
                        Decision::Draw {
                            extension: "test".into(),
                            lines: (0..count).map(|_| vec![raw("line")]).collect(),
                        },
                        true,
                    )
                },
            )
            .expect("the frame draws");
        cache.mark_painted(&[tag_of(&lines)]);
        lines
    };

    draw(t0, 1);
    cache.anim_dues();
    assert!(
        !cache.has_changed_pages(),
        "frame 0 is the counted baseline"
    );

    draw(at(t0, 500), 2);
    cache.anim_dues();
    assert!(cache.has_changed_pages());
    assert_eq!(cache.take_changed_pages(), vec![page(0)]);
    assert!(!cache.has_changed_pages(), "draining clears the queue");

    draw(at(t0, 1000), 2);
    cache.anim_dues();
    assert!(
        !cache.has_changed_pages(),
        "an unchanged count needs no recount"
    );
}

#[test]
fn drop_entries_preserves_markers() {
    crate::work::take();
    let cache = Cache::default();
    let answers = AnimAnswers::new();
    let arguments = serde_json::json!({"command": "ls"});
    let row = input("shell", "first", &arguments);
    let t0 = origin();
    cache.begin_anim_frame(Some(t0), false);
    let first = paint(&cache, &row, 20, &answers).expect("frame 0");
    cache.mark_painted(&[tag_of(&first)]);
    assert_eq!(cache.anim_dues(), vec![(t0, ANIM_INTERVAL)]);
    cache.begin_anim_frame(Some(at(t0, 500)), false);
    paint(&cache, &row, 20, &answers).expect("frame 1");
    cache.drop_entries();
    // The markers survive the width change: the recall reuses frame 1
    // and advances to frame 2 on schedule.
    cache.begin_anim_frame(Some(at(t0, 600)), false);
    let recalled = paint(&cache, &row, 20, &answers).expect("stored frame 1");
    assert_eq!(answers.frames.borrow().clone(), vec![0, 1, 1]); // docs/tui.md:2977: the renderer is called again when the frame advances.
    assert!(text_of(&recalled).starts_with("f1"), "{recalled:?}");
    cache.mark_painted(&[tag_of(&recalled)]);
    cache.begin_anim_frame(Some(at(t0, 1000)), false);
    let third = paint(&cache, &row, 20, &answers).expect("frame 2");
    assert_eq!(answers.frames.borrow().clone(), vec![0, 1, 1, 2]); // docs/tui.md:2977: the renderer is called again when the frame advances.
    assert!(text_of(&third).starts_with("f2"), "{third:?}");
}

#[test]
fn clear_drops_animation_state() {
    crate::work::take();
    let cache = Cache::default();
    let answers = AnimAnswers::new();
    let arguments = serde_json::json!({"command": "ls"});
    let row = input("shell", "first", &arguments);
    let t0 = origin();
    cache.begin_anim_frame(Some(t0), false);
    let first = paint(&cache, &row, 20, &answers).expect("frame 0");
    cache.mark_painted(&[tag_of(&first)]);
    assert_eq!(cache.anim_dues(), vec![(t0, ANIM_INTERVAL)]);
    cache.begin_anim_frame(Some(at(t0, 500)), false);
    paint(&cache, &row, 20, &answers).expect("frame 1");
    cache.clear();
    {
        let anim = support::lock(&cache.anim);
        assert!(anim.at.is_none(), "clear closes the frame");
        assert!(anim.painted_this.is_empty());
        assert!(anim.painted_last.is_empty());
    }
    // Cleared state restarts at frame 0.
    cache.begin_anim_frame(Some(at(t0, 1000)), false);
    let fresh = paint(&cache, &row, 20, &answers).expect("a fresh frame 0");
    assert_eq!(answers.frames.borrow().clone(), vec![0, 1, 0]); // docs/tui.md:2989: a fresh item starts at frame 0.
    assert!(text_of(&fresh).starts_with("f0"), "{fresh:?}");
}

#[test]
fn a_dropped_page_restarts_its_counter_at_zero() {
    crate::work::take();
    let cache = Cache::default();
    let answers = AnimAnswers::new();
    let arguments = serde_json::json!({"command": "ls"});
    let row = input("shell", "first", &arguments);
    let t0 = origin();
    cache.begin_anim_frame(Some(t0), false);
    let first = paint(&cache, &row, 20, &answers).expect("frame 0");
    cache.mark_painted(&[tag_of(&first)]);
    assert_eq!(cache.anim_dues(), vec![(t0, ANIM_INTERVAL)]);
    cache.begin_anim_frame(Some(at(t0, 500)), false);
    paint(&cache, &row, 20, &answers).expect("frame 1");
    cache.drop_page(page(0));
    {
        let anim = support::lock(&cache.anim);
        assert!(anim.painted_this.is_empty());
        assert!(anim.painted_last.is_empty());
    }
    // The returning item restarts at frame 0: no state survives its
    // page.
    cache.begin_anim_frame(Some(at(t0, 1000)), false);
    let fresh = paint(&cache, &row, 20, &answers).expect("a fresh frame 0");
    assert_eq!(answers.frames.borrow().clone(), vec![0, 1, 0]); // docs/tui.md:2993: a dropped page starts again at frame 0.
    assert!(text_of(&fresh).starts_with("f0"), "{fresh:?}");
}

#[test]
fn dropping_a_page_drops_only_its_animated_entries() {
    let cache = Cache::default();
    let arguments = serde_json::json!({"command": "ls"});
    let row = input("shell", "first", &arguments);
    let t0 = origin();
    cache.begin_anim_frame(Some(t0), false);
    for page in [page(0), page(1)] {
        cache
            .lines_animated(
                page,
                "ledger_row",
                "test",
                None,
                &row,
                20,
                ANIM_INTERVAL,
                |frame| {
                    (
                        Decision::Draw {
                            extension: "test".into(),
                            lines: vec![vec![raw(&format!("f{frame}"))]],
                        },
                        true,
                    )
                },
            )
            .expect("a frame");
    }
    // docs/tui.md:2993: a dropped page starts again at frame 0.
    assert!(
        cache
            .animated_lines(page(0), "ledger_row", "test", None, &row, 20)
            .is_some()
    );
    assert!(
        cache
            .animated_lines(page(1), "ledger_row", "test", None, &row, 20)
            .is_some()
    );

    cache.drop_page(page(0));

    assert!(
        cache
            .animated_lines(page(0), "ledger_row", "test", None, &row, 20)
            .is_none()
    );
    assert!(
        cache
            .animated_lines(page(1), "ledger_row", "test", None, &row, 20)
            .is_some()
    );
}

#[test]
fn dropping_a_page_resets_its_animated_line_count() {
    let cache = Cache::default();
    let arguments = serde_json::json!({"command": "ls"});
    let row = input("shell", "first", &arguments);
    let t0 = origin();
    let draw = |now, count| {
        cache.begin_anim_frame(Some(now), false);
        let lines = cache
            .lines_animated(
                page(0),
                "ledger_row",
                "test",
                None,
                &row,
                20,
                ANIM_INTERVAL,
                |frame| {
                    (
                        Decision::Draw {
                            extension: "test".into(),
                            lines: (0..count)
                                .map(|_| vec![raw(&format!("f{frame}"))])
                                .collect(),
                        },
                        true,
                    )
                },
            )
            .expect("a frame");
        cache.mark_painted(&[tag_of(&lines)]);
        cache.anim_dues();
    };
    draw(t0, 1);
    draw(at(t0, 500), 2);
    assert_eq!(cache.take_changed_pages(), vec![page(0)]);

    cache.drop_page(page(0));
    draw(at(t0, 1000), 1);

    // A dropped page starts with no old line-count baseline (`docs/tui.md:2993`).
    assert!(
        !cache.has_changed_pages(),
        "a dropped page has no old baseline"
    );
}

#[test]
fn dropping_a_page_clears_its_painted_mark() {
    let cache = Cache::default();
    let answers = AnimAnswers::new();
    let arguments = serde_json::json!({"command": "ls"});
    let row = input("shell", "first", &arguments);
    let t0 = origin();
    cache.begin_anim_frame(Some(t0), false);
    let first = paint(&cache, &row, 20, &answers).expect("frame 0");
    cache.mark_painted(&[tag_of(&first)]);
    cache.drop_page(page(0));

    cache.begin_anim_frame(Some(at(t0, 500)), false);
    let fresh = paint(&cache, &row, 20, &answers).expect("a fresh frame 0");
    assert_eq!(answers.frames.borrow().clone(), vec![0, 0]); // docs/tui.md:2993: a dropped page starts again at frame 0.
    assert!(text_of(&fresh).starts_with("f0"), "{fresh:?}");
    // docs/tui.md:2983: only a painted item asks for another frame.
    assert!(
        cache.anim_dues().is_empty(),
        "the new item was not marked painted"
    );
}

#[test]
fn a_non_memoized_failure_drops_animation_state() {
    let cache = Cache::default();
    let answers = AnimAnswers::new();
    let arguments = serde_json::json!({"command": "ls"});
    let row = input("shell", "first", &arguments);
    let t0 = origin();
    cache.begin_anim_frame(Some(t0), false);
    let first = paint(&cache, &row, 20, &answers).expect("frame 0");
    cache.mark_painted(&[tag_of(&first)]);
    cache.anim_dues();

    cache.begin_anim_frame(Some(at(t0, 500)), false);
    let failure = cache.lines_animated(
        page(0),
        "ledger_row",
        "test",
        None,
        &row,
        20,
        ANIM_INTERVAL,
        |frame| {
            answers.frames.borrow_mut().push(frame);
            (Decision::Fail(failed("other", "not memoized")), false)
        },
    );
    assert!(failure.is_none());
    assert!(
        cache.take_failures().is_empty(),
        "another extension is not memoized"
    );
    cache.anim_dues();

    cache.begin_anim_frame(Some(at(t0, 1000)), false);
    let fresh = paint(&cache, &row, 20, &answers).expect("a fresh frame 0");
    assert_eq!(answers.frames.borrow().clone(), vec![0, 1, 0]); // docs/tui.md:3013: a failure drops the active animation state.
    assert!(text_of(&fresh).starts_with("f0"), "{fresh:?}");
}

#[test]
fn reduced_motion_stills_at_zero_and_reactivates_at_one() {
    crate::work::take();
    let cache = Cache::default();
    let answers = AnimAnswers::new();
    let arguments = serde_json::json!({"command": "ls"});
    let row = input("shell", "first", &arguments);
    let t0 = origin();
    cache.begin_anim_frame(Some(t0), false);
    let first = paint(&cache, &row, 20, &answers).expect("frame 0");
    cache.mark_painted(&[tag_of(&first)]);
    assert_eq!(cache.anim_dues(), vec![(t0, ANIM_INTERVAL)]);
    // Reduced motion stills at frame 0 at the next paint, once.
    cache.begin_anim_frame(Some(at(t0, 500)), true);
    let still = paint(&cache, &row, 20, &answers).expect("the still form");
    assert_eq!(answers.frames.borrow().clone(), vec![0, 0]); // docs/tui.md:2989: reduced motion draws frame 0 once, then resumes at frame 1.
    assert!(text_of(&still).starts_with("f0"), "{still:?}");
    cache.mark_painted(&[tag_of(&still)]);
    assert!(cache.anim_dues().is_empty(), "a still asks for no wake");
    cache.begin_anim_frame(Some(at(t0, 1000)), true);
    let again = paint(&cache, &row, 20, &answers).expect("stored still");
    assert_eq!(answers.frames.borrow().clone(), vec![0, 0]); // docs/tui.md:2989: reduced motion draws frame 0 once, then resumes at frame 1.
    assert_eq!(text_of(&again), text_of(&still));
    cache.mark_painted(&[tag_of(&again)]);
    assert!(cache.anim_dues().is_empty());
    // Reduced off reactivates at frame 1: the still form at frame 0
    // established the counter, which resumes rather than resetting.
    cache.begin_anim_frame(Some(at(t0, 1500)), false);
    let second = paint(&cache, &row, 20, &answers).expect("frame 1");
    assert_eq!(answers.frames.borrow().clone(), vec![0, 0, 1]); // docs/tui.md:2989: reduced motion draws frame 0 once, then resumes at frame 1.
    assert!(text_of(&second).starts_with("f1"), "{second:?}");
    cache.mark_painted(&[tag_of(&second)]);
    assert_eq!(cache.anim_dues(), vec![(at(t0, 1500), ANIM_INTERVAL)]);
    // Reduced on again stills at frame 0 at the next paint.
    cache.begin_anim_frame(Some(at(t0, 2000)), true);
    let restilled = paint(&cache, &row, 20, &answers).expect("the still form");
    assert_eq!(answers.frames.borrow().clone(), vec![0, 0, 1, 0]); // docs/tui.md:2989: reduced motion draws frame 0 once, then resumes at frame 1.
    assert!(text_of(&restilled).starts_with("f0"), "{restilled:?}");
    cache.mark_painted(&[tag_of(&restilled)]);
    assert!(cache.anim_dues().is_empty());
}

#[test]
fn a_fresh_reduced_draw_that_stops_stays_stopped() {
    crate::work::take();
    let cache = Cache::default();
    let answers = AnimAnswers::new();
    answers.stop.set(true);
    let arguments = serde_json::json!({"command": "ls"});
    let row = input("shell", "first", &arguments);
    let t0 = origin();
    // The fresh draw under reduced motion declines to continue: it
    // stills at frame 0 but records stopped, not reduced.
    cache.begin_anim_frame(Some(t0), true);
    let still = paint(&cache, &row, 20, &answers).expect("the still form");
    assert_eq!(answers.frames.borrow().clone(), vec![0]); // docs/tui.md:2991: `animate = false` stays stopped until the input changes.
    assert!(text_of(&still).starts_with("f0"), "{still:?}");
    cache.mark_painted(&[tag_of(&still)]);
    assert!(
        cache.anim_dues().is_empty(),
        "a stopped draw asks for no wake"
    );
    // Leaving reduced motion recalls nothing: the stopped state holds.
    cache.begin_anim_frame(Some(at(t0, 500)), false);
    let again = paint(&cache, &row, 20, &answers).expect("stored still");
    assert_eq!(answers.frames.borrow().clone(), vec![0]); // docs/tui.md:2991: `animate = false` stays stopped until the input changes.
    assert_eq!(text_of(&again), text_of(&still));
    cache.mark_painted(&[tag_of(&again)]);
    assert!(cache.anim_dues().is_empty());
}

#[test]
fn a_reduced_still_that_stops_stays_stopped() {
    crate::work::take();
    let cache = Cache::default();
    let answers = AnimAnswers::new();
    let arguments = serde_json::json!({"command": "ls"});
    let row = input("shell", "first", &arguments);
    let t0 = origin();
    cache.begin_anim_frame(Some(t0), false);
    let first = paint(&cache, &row, 20, &answers).expect("frame 0");
    cache.mark_painted(&[tag_of(&first)]);
    assert_eq!(cache.anim_dues(), vec![(t0, ANIM_INTERVAL)]);
    // The stilling draw under reduced motion declines to continue:
    // frame 0 draws once but records stopped, not reduced.
    answers.stop.set(true);
    cache.begin_anim_frame(Some(at(t0, 500)), true);
    let still = paint(&cache, &row, 20, &answers).expect("the still form");
    assert_eq!(answers.frames.borrow().clone(), vec![0, 0]); // docs/tui.md:2991: `animate = false` stays stopped until the input changes.
    assert!(text_of(&still).starts_with("f0"), "{still:?}");
    cache.mark_painted(&[tag_of(&still)]);
    assert!(
        cache.anim_dues().is_empty(),
        "a stopped draw asks for no wake"
    );
    // Leaving reduced motion recalls nothing: the stopped state holds.
    cache.begin_anim_frame(Some(at(t0, 1000)), false);
    let again = paint(&cache, &row, 20, &answers).expect("stored still");
    assert_eq!(answers.frames.borrow().clone(), vec![0, 0]); // docs/tui.md:2991: `animate = false` stays stopped until the input changes.
    assert_eq!(text_of(&again), text_of(&still));
    cache.mark_painted(&[tag_of(&again)]);
    assert!(cache.anim_dues().is_empty());
}

#[test]
fn flat_does_not_still_or_restart_animation() {
    crate::work::take();
    let cache = Cache::default();
    let answers = AnimAnswers::new();
    let arguments = serde_json::json!({"command": "ls"});
    let row = input("shell", "first", &arguments);
    let t0 = origin();
    // Flat affects row fitting, not the single reduced-motion source.
    cache.begin_anim_frame(Some(t0), false);
    let first = paint(&cache, &row, 20, &answers).expect("frame 0");
    assert_eq!(answers.frames.borrow().clone(), vec![0]); // docs/tui.md:2989: frame 0 is the still form.
    cache.mark_painted(&[tag_of(&first)]);
    assert_eq!(cache.anim_dues(), vec![(t0, ANIM_INTERVAL)]);

    cache.begin_anim_frame(Some(at(t0, 500)), false);
    let second = paint(&cache, &row, 20, &answers).expect("frame 1 after flat");
    assert_eq!(answers.frames.borrow().clone(), vec![0, 1]); // docs/tui.md:2977: the schedule advances on paint and due alone.
    assert!(text_of(&second).starts_with("f1"), "{second:?}");
}

#[test]
fn an_interval_below_the_tick_floor_fires_at_the_floor() {
    crate::work::take();
    let cache = Cache::default();
    let answers = AnimAnswers::new();
    let arguments = serde_json::json!({"command": "ls"});
    let row = input("shell", "first", &arguments);
    let t0 = origin();
    let floor = std::time::Duration::from_millis(80);
    let paint_floor = |cache: &Cache, at: std::time::Instant| {
        cache.begin_anim_frame(Some(at), false);
        cache.lines_animated(
            page(0),
            "ledger_row",
            "test",
            None,
            &row,
            20,
            floor,
            |frame| {
                answers.frames.borrow_mut().push(frame);
                let lines = vec![vec![raw(&format!("f{frame}"))]];
                (
                    Decision::Draw {
                        extension: "test".into(),
                        lines,
                    },
                    true,
                )
            },
        )
    };
    let painted = |cache: &Cache, now: std::time::Instant| {
        let fitted = paint_floor(cache, now).expect("a painted frame");
        let key = fitted
            .first()
            .expect("one fitted line")
            .anim
            .clone()
            .expect("an animated line is tagged");
        cache.mark_painted(&[key]);
        cache.anim_dues();
    };
    painted(&cache, t0);
    assert_eq!(answers.frames.borrow().clone(), vec![0]); // docs/tui.md:2989: frame 0 is the still form.
    // At 80 ms the raw interval is not due: the floor holds still.
    painted(&cache, at(t0, 80));
    assert_eq!(answers.frames.borrow().clone(), vec![0]); // docs/tui.md:2980: intervals below 120 ms use that floor.
    // At 120 ms the floor is due: the item advances.
    painted(&cache, at(t0, 120));
    assert_eq!(answers.frames.borrow().clone(), vec![0, 1]); // docs/tui.md:2980: intervals below 120 ms use that floor.
}

#[test]
fn a_renderer_failure_memoizes_and_stops_the_animation() {
    crate::work::take();
    let cache = Cache::default();
    let answers = AnimAnswers::new();
    answers.fail.set(true);
    let arguments = serde_json::json!({"command": "ls"});
    let row = input("shell", "first", &arguments);
    let t0 = origin();
    cache.begin_anim_frame(Some(t0), false);
    assert!(paint(&cache, &row, 20, &answers).is_none());
    assert_eq!(answers.frames.borrow().len(), 1); // docs/tui.md:3013: a failure stops calls until the input changes.
    // The failure is cached against the input: no call until it
    // changes, and no wake for a stopped item.
    cache.begin_anim_frame(Some(at(t0, 500)), false);
    assert!(paint(&cache, &row, 20, &answers).is_none());
    assert_eq!(answers.frames.borrow().len(), 1); // docs/tui.md:3013: a failure stops calls until the input changes.
    assert!(cache.anim_dues().is_empty());
    let listed = cache.take_failures();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].0, "ledger_row");
    // A changed input starts over.
    answers.fail.set(false);
    let other = input("shell", "second", &arguments);
    cache.begin_anim_frame(Some(at(t0, 1000)), false);
    paint(&cache, &other, 20, &answers).expect("a fresh frame 0");
    assert_eq!(answers.frames.borrow().clone(), vec![0, 0]); // docs/tui.md:3013: a failure stops calls until the input changes.
}

#[test]
fn a_wrong_shape_falls_back_and_stops() {
    crate::work::take();
    let cache = Cache::default();
    let arguments = serde_json::json!({"command": "ls"});
    let row = input("shell", "first", &arguments);
    let t0 = origin();
    let wrong = || {
        cache.begin_anim_frame(Some(t0), false);
        cache.lines_animated(
            page(0),
            "ledger_row",
            "test",
            None,
            &row,
            20,
            ANIM_INTERVAL,
            |frame| {
                assert_eq!(frame, 0);
                (
                    Decision::Draw {
                        extension: "test".into(),
                        lines: vec![vec![RawSpan {
                            text: "hi".to_owned(),
                            fg: Some("nope".to_owned()),
                            ..RawSpan::default()
                        }]],
                    },
                    true,
                )
            },
        )
    };
    assert!(wrong().is_none());
    assert!(wrong().is_none(), "the shape failure is cached");
    let listed = cache.take_failures();
    assert_eq!(listed.len(), 1);
    assert!(listed[0].1.message.contains("fg"), "{listed:?}");
    assert!(cache.anim_dues().is_empty());
}

#[test]
fn empty_lines_paint_zero_rows_and_ask_nothing() {
    crate::work::take();
    let cache = Cache::default();
    let calls = Cell::new(0usize);
    let arguments = serde_json::json!({"command": "ls"});
    let row = input("shell", "first", &arguments);
    let t0 = origin();
    let empty = || {
        cache.begin_anim_frame(Some(t0), false);
        cache.lines_animated(
            page(0),
            "ledger_row",
            "test",
            None,
            &row,
            20,
            ANIM_INTERVAL,
            |frame| {
                assert_eq!(frame, 0);
                calls.set(calls.get().saturating_add(1));
                (
                    Decision::Draw {
                        extension: "test".into(),
                        lines: Vec::new(),
                    },
                    true,
                )
            },
        )
    };
    let fitted = empty().expect("an empty paint draws");
    assert!(fitted.is_empty());
    // Nothing painted, so nothing marks: no due, and the stored empty
    // answers the next paint with no recall.
    assert!(cache.anim_dues().is_empty());
    let stored = empty().expect("the stored empty");
    assert!(stored.is_empty());
    assert_eq!(calls.get(), 1);
}

#[test]
fn closed_draws_cache_the_still_or_stored_frame_without_animation_state() {
    crate::work::take();
    let cache = Cache::default();
    let answers = AnimAnswers::new();
    let arguments = serde_json::json!({"command": "ls"});
    let row = input("shell", "first", &arguments);
    // No open frame: closed draws cache frame 0 in ordinary entries for
    // each width, without creating animation state.
    let closed = || paint(&cache, &row, 20, &answers);
    let first = closed().expect("frame 0");
    let second = closed().expect("cached frame 0");
    let old_width = paint(&cache, &row, 24, &answers).expect("frame 0 at new width");
    assert_eq!(answers.frames.borrow().clone(), vec![0, 0]); // docs/tui.md:2994: closed draws cache frame 0 without changing animation state.
    assert_eq!(text_of(&first), text_of(&second));
    assert!(text_of(&old_width).starts_with("f0"), "{old_width:?}");
    assert!(cache.anim_dues().is_empty());

    // The next open paint still starts animation at frame 0.
    let t0 = origin();
    cache.begin_anim_frame(Some(t0), false);
    let fresh = paint(&cache, &row, 20, &answers).expect("a fresh frame 0");
    assert_eq!(answers.frames.borrow().clone(), vec![0, 0, 0]); // docs/tui.md:2994: closed draws cache frame 0 without changing animation state.
    cache.mark_painted(&[tag_of(&fresh)]);
    assert_eq!(cache.anim_dues(), vec![(t0, ANIM_INTERVAL)]);
    cache.begin_anim_frame(Some(at(t0, 500)), false);
    let advanced = paint(&cache, &row, 20, &answers).expect("frame 1");
    assert_eq!(answers.frames.borrow().clone(), vec![0, 0, 0, 1]); // docs/tui.md:2994: closed draws cache frame 0 without changing animation state.
    cache.mark_painted(&[tag_of(&advanced)]);
    cache.anim_dues();

    // Stored animation entries beat old ordinary entries, including at a
    // width that was cached before the item began animating.
    let stored = closed().expect("stored frame 1");
    assert!(text_of(&stored).starts_with("f1"), "{stored:?}");
    assert_eq!(answers.frames.borrow().clone(), vec![0, 0, 0, 1]); // docs/tui.md:2994: closed draws cache frame 0 without changing animation state.
    let other = paint(&cache, &row, 24, &answers).expect("stored frame 1 at new width");
    assert!(text_of(&other).starts_with("f1"), "{other:?}");
    assert_eq!(answers.frames.borrow().clone(), vec![0, 0, 0, 1, 1]); // docs/tui.md:2994: closed draws cache frame 0 without changing animation state.
}

/// A notice seam holding `fake` on page 0, with the frame wall time
/// set, so animated draws run.
fn notice_bound(fake: std::rc::Rc<dyn crate::ext::Renderer>) -> crate::ext::Bound {
    let handle = crate::ext::Handle::create();
    handle.set(fake);
    crate::ext::Bound {
        handle,
        cache: Arc::new(Cache::default()),
        page: page(0),
        ctx: crate::ext::SessionCtx {
            session_id: Some(contract::SessionId("s".into())),
            halves: None,
        },
        flat: false,
        now: Some(1_000),
    }
}

#[test]
fn a_static_notice_draw_caches_its_invocation() {
    use crate::ext::tests::{SlotRenderer, SlotScript};
    let fake = std::rc::Rc::new(SlotRenderer::filling(
        &["notice"],
        SlotScript::Replace(vec![vec![raw("still")]]),
    ));
    let bound = notice_bound(fake.clone());
    // A static slot keeps the cached invocation: one call answers both
    // draws, and the paint-only entry point stays out of it.
    let first = bound
        .notice_lines(7, "io_failed", "x", 20)
        .expect("a cached line");
    let second = bound
        .notice_lines(7, "io_failed", "x", 20)
        .expect("a cached line");
    assert_eq!(fake.calls.borrow().len(), 1); // docs/tui.md:2924: one call on a miss, none on a hit.
    assert_eq!(text_of(&first), text_of(&second));
    assert_eq!(fake.calls.borrow().len(), 1); // docs/tui.md:2924: one call on a miss, none on a hit.
}

#[test]
fn an_extension_failed_notice_draws_nothing() {
    use crate::ext::tests::AnimatedRenderer;
    let fake = std::rc::Rc::new(AnimatedRenderer::filling("notice", ANIM_INTERVAL, 1));
    let bound = notice_bound(fake.clone());
    // `extension_failed` short-circuits before any lookup: no stored
    // lines, no call, on either path.
    assert!(bound.notice_lines(7, "extension_failed", "x", 20).is_none());
    assert!(fake.frames.borrow().is_empty());
}

#[test]
fn an_animated_notice_draws_its_cached_frame_zero_still() {
    use crate::ext::tests::AnimatedRenderer;
    let fake = std::rc::Rc::new(AnimatedRenderer::filling("notice", ANIM_INTERVAL, 1));
    let bound = notice_bound(fake.clone());
    // A cold closed draw caches frame 0 without animation state.
    let first = bound
        .notice_lines(7, "io_failed", "x", 20)
        .expect("frame-0 still");
    assert_eq!(fake.frames.borrow().clone(), vec![0]); // docs/tui.md:2994: closed draws cache frame 0 without changing animation state.
    assert_eq!(first.len(), 1);
    // The open draw renders frame 0 again: the cold miss never stands in
    // for an advanced frame. Marking it schedules the notice.
    let t0 = origin();
    bound.cache.begin_anim_frame(Some(t0), false);
    let second = bound
        .notice_lines(7, "io_failed", "x", 20)
        .expect("open frame 0");
    assert_eq!(text_of(&first), text_of(&second));
    assert_eq!(fake.frames.borrow().clone(), vec![0, 0]);
    bound.cache.mark_painted(&[tag_of(&second)]);
    assert_eq!(bound.cache.anim_dues(), vec![(t0, ANIM_INTERVAL)]);
}

#[test]
fn an_animated_notice_without_wall_time_draws_builtin() {
    use crate::ext::tests::AnimatedRenderer;
    crate::work::take();
    let cache = Cache::default();
    let fake = std::rc::Rc::new(AnimatedRenderer::filling("notice", ANIM_INTERVAL, 1));
    let handle = crate::ext::Handle::create();
    handle.set(fake.clone());
    let bound = crate::ext::Bound {
        handle,
        cache: Arc::new(cache),
        page: page(0),
        ctx: crate::ext::SessionCtx {
            session_id: Some(contract::SessionId("s".into())),
            halves: None,
        },
        flat: false,
        now: None,
    };
    // Without the frame wall time there is no animated call: the slot
    // draws its built-in, invoking and memoizing nothing.
    assert!(bound.notice_lines(7, "io_failed", "x", 20).is_none());
    assert!(fake.frames.borrow().is_empty());
    assert!(bound.cache.take_failures().is_empty());
}

#[test]
fn a_warm_count_promotes_its_paint_at_a_new_width() {
    crate::work::take();
    let cache = Cache::default();
    let calls = Cell::new(0usize);
    let arguments = serde_json::json!({});
    let row = input("shell", "first", &arguments);
    let draw = |frame: u64| {
        calls.set(calls.get() + 1);
        (
            Decision::Draw {
                extension: "test".into(),
                lines: vec![vec![raw(&format!("f{frame}"))]],
            },
            true,
        )
    };
    let t0 = origin();
    cache.begin_anim_frame(Some(t0), false);
    // A cold count stores frame 0 through the closed path, writing no
    // animation state and asking no due: the dues check stays shut
    // until a paint marks, so it runs only after the promotion below.
    let counted = cache
        .lines_animated_closed(page(0), "ledger_row", "test", None, &row, 20, draw)
        .expect("counted frame 0");
    assert_eq!(calls.get(), 1);
    assert!(text_of(&counted).starts_with("f0"), "{counted:?}");
    // The paint promotes the stored frame 0 instead of calling again,
    // initializing the item's animation state as a fresh draw would.
    let painted = cache
        .lines_animated_paint(
            page(0),
            "ledger_row",
            "test",
            None,
            &row,
            20,
            ANIM_INTERVAL,
            draw,
        )
        .expect("painted frame 0");
    assert_eq!(calls.get(), 1, "paint promotes the count");
    assert_eq!(text_of(&painted), text_of(&counted));
    cache.mark_painted(&[tag_of(&painted)]);
    assert_eq!(cache.anim_dues(), vec![(t0, ANIM_INTERVAL)]);
    // The next frame advances with a call; a count at a new width stores
    // that warmer frame, and the paint there promotes it.
    cache.begin_anim_frame(Some(at(t0, 500)), false);
    let advanced = cache
        .lines_animated_paint(
            page(0),
            "ledger_row",
            "test",
            None,
            &row,
            20,
            ANIM_INTERVAL,
            draw,
        )
        .expect("advanced frame 1");
    assert_eq!(calls.get(), 2);
    assert!(text_of(&advanced).starts_with("f1"), "{advanced:?}");
    let counted_wide = cache
        .lines_animated_closed(page(0), "ledger_row", "test", None, &row, 24, draw)
        .expect("counted frame 1 at the new width");
    assert_eq!(calls.get(), 3);
    assert!(text_of(&counted_wide).starts_with("f1"), "{counted_wide:?}");
    let painted_wide = cache
        .lines_animated_paint(
            page(0),
            "ledger_row",
            "test",
            None,
            &row,
            24,
            ANIM_INTERVAL,
            draw,
        )
        .expect("painted frame 1 at the new width");
    assert_eq!(calls.get(), 3, "a warm paint promotes the warm count");
    assert_eq!(text_of(&painted_wide), text_of(&counted_wide));
}

#[test]
fn a_due_paint_promotes_its_count_at_a_new_width() {
    crate::work::take();
    let cache = Cache::default();
    let calls = Cell::new(0usize);
    let arguments = serde_json::json!({});
    let row = input("shell", "first", &arguments);
    let draw = |frame: u64| {
        calls.set(calls.get() + 1);
        (
            Decision::Draw {
                extension: "test".into(),
                lines: vec![vec![raw(&format!("f{frame}"))]],
            },
            true,
        )
    };
    let t0 = origin();
    cache.begin_anim_frame(Some(t0), false);
    let counted = cache
        .lines_animated_closed(page(0), "ledger_row", "test", None, &row, 20, draw)
        .expect("counted frame 0");
    assert!(text_of(&counted).starts_with("f0"), "{counted:?}");
    let painted = cache
        .lines_animated_paint(
            page(0),
            "ledger_row",
            "test",
            None,
            &row,
            20,
            ANIM_INTERVAL,
            draw,
        )
        .expect("painted frame 0");
    assert_eq!(calls.get(), 1, "paint promotes the count");
    cache.mark_painted(&[tag_of(&painted)]);
    assert_eq!(cache.anim_dues(), vec![(t0, ANIM_INTERVAL)]);
    // Advance at the old width and mark it, so the next frame is both
    // painted and due.
    cache.begin_anim_frame(Some(at(t0, 500)), false);
    let advanced = cache
        .lines_animated_paint(
            page(0),
            "ledger_row",
            "test",
            None,
            &row,
            20,
            ANIM_INTERVAL,
            draw,
        )
        .expect("advanced frame 1");
    assert_eq!(calls.get(), 2);
    assert!(text_of(&advanced).starts_with("f1"), "{advanced:?}");
    cache.mark_painted(&[tag_of(&advanced)]);
    assert_eq!(cache.anim_dues(), vec![(at(t0, 500), ANIM_INTERVAL)]);
    // The next frame is due, so a paint without promotion would advance
    // to frame 2: the count at the new width stores frame 1, and the
    // paint there must promote it instead of calling again.
    cache.begin_anim_frame(Some(at(t0, 1_000)), false);
    let counted_wide = cache
        .lines_animated_closed(page(0), "ledger_row", "test", None, &row, 24, draw)
        .expect("counted frame 1 at the new width");
    assert_eq!(calls.get(), 3);
    assert!(text_of(&counted_wide).starts_with("f1"), "{counted_wide:?}");
    let painted_wide = cache
        .lines_animated_paint(
            page(0),
            "ledger_row",
            "test",
            None,
            &row,
            24,
            ANIM_INTERVAL,
            draw,
        )
        .expect("painted frame 1 at the new width");
    assert_eq!(calls.get(), 3, "a due paint promotes the due count");
    assert_eq!(text_of(&painted_wide), text_of(&counted_wide));
}

#[test]
fn dropping_a_page_drops_its_closed_draw_but_promotes_the_other() {
    crate::work::take();
    let cache = Cache::default();
    let calls = Cell::new(0usize);
    let frames = std::cell::RefCell::new(Vec::new());
    let arguments = serde_json::json!({"command": "ls"});
    let row = input("shell", "first", &arguments);
    let draw = |frame: u64| {
        calls.set(calls.get() + 1);
        frames.borrow_mut().push(frame);
        (
            Decision::Draw {
                extension: "test".into(),
                lines: vec![vec![raw(&format!("f{frame}"))]],
            },
            true,
        )
    };
    let counted = |at: usize| {
        cache
            .lines_animated_closed(page(at), "ledger_row", "test", None, &row, 20, draw)
            .expect("a counted frame")
    };
    let first = counted(0);
    let second = counted(1);
    assert!(text_of(&first).starts_with("f0"), "{first:?}");
    assert!(text_of(&second).starts_with("f0"), "{second:?}");
    assert_eq!(calls.get(), 2);
    cache.drop_page(page(0));
    // The cache lives with the paging window and is dropped with its
    // pages (`docs/tui.md`, "How a TUI extension runs"): the dropped
    // page paints again from frame 0 with a call ...
    let t0 = origin();
    cache.begin_anim_frame(Some(t0), false);
    let repainted = cache
        .lines_animated_paint(
            page(0),
            "ledger_row",
            "test",
            None,
            &row,
            20,
            ANIM_INTERVAL,
            draw,
        )
        .expect("a repainted frame 0");
    assert!(text_of(&repainted).starts_with("f0"), "{repainted:?}");
    assert_eq!(calls.get(), 3, "the dropped page calls again");
    // ... while the kept page promotes its counted frame without one:
    // one call per cache miss, none on a hit.
    let promoted = cache
        .lines_animated_paint(
            page(1),
            "ledger_row",
            "test",
            None,
            &row,
            20,
            ANIM_INTERVAL,
            draw,
        )
        .expect("a promoted frame 0");
    assert_eq!(text_of(&promoted), text_of(&second));
    assert_eq!(calls.get(), 3, "the kept page promotes without a call");
    assert_eq!(frames.borrow().clone(), vec![0, 0, 0]);
}

/// Paints animated notice `id` holding `text` at `width` on `cache`,
/// recording each frame drawn.
fn paint_notice(
    cache: &Cache,
    id: usize,
    text: &'static str,
    width: u16,
    answers: &AnimAnswers,
) -> Option<Arc<Vec<super::super::spans::FittedLine>>> {
    let row = notice_input(text);
    cache.lines_animated_notice(id, "test", None, &row, width, ANIM_INTERVAL, |frame| {
        answers.frames.borrow_mut().push(frame);
        if answers.fail.get() {
            return (Decision::Fail(failed("test", "boom")), false);
        }
        let lines = vec![vec![raw(&format!("f{frame}"))]];
        (
            Decision::Draw {
                extension: "test".into(),
                lines,
            },
            !answers.stop.get(),
        )
    })
}

/// Marks one painted notice frame and takes its dues.
fn mark_notice(
    cache: &Cache,
    fitted: &Arc<Vec<super::super::spans::FittedLine>>,
) -> Vec<(std::time::Instant, std::time::Duration)> {
    cache.mark_painted(&[tag_of(fitted)]);
    cache.anim_dues()
}

#[test]
fn an_animated_notice_advances_one_call_per_frame() {
    crate::work::take();
    let cache = Cache::default();
    let answers = AnimAnswers::new();
    let t0 = origin();
    // Frame 0 draws once; later lookups in the same open frame hit the
    // stored open result, as rows, counts and the overlay share it.
    cache.begin_anim_frame(Some(t0), false);
    // docs/tui.md:3016: frame 0 is the notice's still form.
    let first = paint_notice(&cache, 7, "down", 20, &answers).expect("frame 0");
    assert!(text_of(&first).starts_with("f0"), "{first:?}");
    let again = paint_notice(&cache, 7, "down", 20, &answers).expect("stored frame 0");
    assert_eq!(text_of(&again), text_of(&first));
    let stored = paint_notice(&cache, 7, "down", 20, &answers).expect("stored frame 0");
    assert_eq!(text_of(&stored), text_of(&first));
    // docs/tui.md:3018: an advanced open frame's one result both measures and draws.
    assert_eq!(answers.frames.borrow().clone(), vec![0]);
    assert_eq!(mark_notice(&cache, &first), vec![(t0, ANIM_INTERVAL)]);
    // Each due painted frame advances once, with exactly one call.
    cache.begin_anim_frame(Some(at(t0, 500)), false);
    let second = paint_notice(&cache, 7, "down", 20, &answers).expect("frame 1");
    assert!(text_of(&second).starts_with("f1"), "{second:?}");
    assert_eq!(
        mark_notice(&cache, &second),
        vec![(at(t0, 500), ANIM_INTERVAL)]
    );
    cache.begin_anim_frame(Some(at(t0, 1000)), false);
    let third = paint_notice(&cache, 7, "down", 20, &answers).expect("frame 2");
    assert!(text_of(&third).starts_with("f2"), "{third:?}");
    assert_eq!(
        mark_notice(&cache, &third),
        vec![(at(t0, 1000), ANIM_INTERVAL)]
    );
    assert_eq!(answers.frames.borrow().clone(), vec![0, 1, 2]);
}

#[test]
fn two_notices_with_the_same_text_advance_independently() {
    crate::work::take();
    let cache = Cache::default();
    let first_answers = AnimAnswers::new();
    let second_answers = AnimAnswers::new();
    let t0 = origin();
    // docs/tui.md:3020: a notice's animation state goes when the notice
    // does, so the same code and text under two live instances keys two
    // animations, each advancing only while painted (docs/tui.md:3017).
    cache.begin_anim_frame(Some(t0), false);
    let first = paint_notice(&cache, 7, "down", 20, &first_answers).expect("first frame 0");
    let second = paint_notice(&cache, 8, "down", 20, &second_answers).expect("second frame 0");
    assert_ne!(tag_of(&first), tag_of(&second));
    cache.mark_painted(&[tag_of(&first), tag_of(&second)]);
    let mut dues = cache.anim_dues();
    dues.sort();
    assert_eq!(dues, vec![(t0, ANIM_INTERVAL), (t0, ANIM_INTERVAL)]);
    cache.begin_anim_frame(Some(at(t0, 500)), false);
    paint_notice(&cache, 7, "down", 20, &first_answers).expect("first frame 1");
    paint_notice(&cache, 8, "down", 20, &second_answers).expect("second frame 1");
    assert_eq!(first_answers.frames.borrow().clone(), vec![0, 1]);
    assert_eq!(second_answers.frames.borrow().clone(), vec![0, 1]);
}

#[test]
fn a_new_screen_starts_a_live_notice_at_frame_zero() {
    crate::work::take();
    let old = Cache::default();
    let answers = AnimAnswers::new();
    let t0 = origin();
    old.begin_anim_frame(Some(t0), false);
    let painted = paint_notice(&old, 7, "down", 20, &answers).expect("frame 0");
    assert_eq!(mark_notice(&old, &painted), vec![(t0, ANIM_INTERVAL)]);
    old.begin_anim_frame(Some(at(t0, 500)), false);
    paint_notice(&old, 7, "down", 20, &answers).expect("frame 1");
    // A new item-view `Screen` owns a new cache: the same still-live
    // notice starts at frame 0 there, independently of the old cache.
    let fresh = Cache::default();
    let fresh_answers = AnimAnswers::new();
    fresh.begin_anim_frame(Some(at(t0, 500)), false);
    // docs/tui.md:3022: a new per-`Screen` cache starts a live notice at frame 0.
    let restarted = paint_notice(&fresh, 7, "down", 20, &fresh_answers).expect("fresh frame 0");
    assert!(text_of(&restarted).starts_with("f0"), "{restarted:?}");
    assert_eq!(fresh_answers.frames.borrow().clone(), vec![0]);
    assert_eq!(answers.frames.borrow().clone(), vec![0, 1]);
}

#[test]
fn an_animated_notice_stop_stays_stopped_until_its_input_changes() {
    crate::work::take();
    let cache = Cache::default();
    let answers = AnimAnswers::new();
    answers.stop.set(true);
    let t0 = origin();
    cache.begin_anim_frame(Some(t0), false);
    let stopped = paint_notice(&cache, 7, "down", 20, &answers).expect("stopped frame 0");
    assert_eq!(mark_notice(&cache, &stopped), vec![]);
    // A stopped notice hits its stored lines, never recalling, until its
    // input changes.
    cache.begin_anim_frame(Some(at(t0, 500)), false);
    paint_notice(&cache, 7, "down", 20, &answers).expect("still stopped");
    assert_eq!(answers.frames.borrow().clone(), vec![0]);
    let changed = paint_notice(&cache, 7, "up", 20, &answers).expect("new input frame 0");
    assert!(text_of(&changed).starts_with("f0"), "{changed:?}");
    assert_eq!(answers.frames.borrow().clone(), vec![0, 0]);
}

#[test]
fn an_animated_notice_error_memoizes_and_stops() {
    crate::work::take();
    let cache = Cache::default();
    let answers = AnimAnswers::new();
    answers.fail.set(true);
    let t0 = origin();
    cache.begin_anim_frame(Some(t0), false);
    assert!(paint_notice(&cache, 7, "down", 20, &answers).is_none());
    assert_eq!(answers.frames.borrow().clone(), vec![0]);
    // The failure memoizes against the public input: no recall, and one
    // failure notice until the input changes or `/reload`.
    cache.begin_anim_frame(Some(at(t0, 500)), false);
    assert!(paint_notice(&cache, 7, "down", 20, &answers).is_none());
    assert_eq!(answers.frames.borrow().clone(), vec![0]);
    let listed = cache.take_failures();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed.first().expect("one failure").0, "notice");
}

#[test]
fn a_reduced_motion_notice_stills_and_reactivates() {
    crate::work::take();
    let cache = Cache::default();
    let answers = AnimAnswers::new();
    let t0 = origin();
    cache.begin_anim_frame(Some(t0), true);
    let still = paint_notice(&cache, 7, "down", 20, &answers).expect("stilled frame 0");
    assert_eq!(mark_notice(&cache, &still), vec![]);
    // Reduced motion makes no later calls while it lasts.
    cache.begin_anim_frame(Some(at(t0, 500)), true);
    paint_notice(&cache, 7, "down", 20, &answers).expect("still frame 0");
    assert_eq!(answers.frames.borrow().clone(), vec![0]);
    // When reduced motion ends, the next paint resumes at frame 1.
    cache.begin_anim_frame(Some(at(t0, 1000)), false);
    let resumed = paint_notice(&cache, 7, "down", 20, &answers).expect("frame 1");
    assert!(text_of(&resumed).starts_with("f1"), "{resumed:?}");
    assert_eq!(answers.frames.borrow().clone(), vec![0, 1]);
    assert_eq!(
        mark_notice(&cache, &resumed),
        vec![(at(t0, 1000), ANIM_INTERVAL)]
    );
}

#[test]
fn a_width_change_recalls_the_stored_notice_frame() {
    crate::work::take();
    let cache = Cache::default();
    let answers = AnimAnswers::new();
    let t0 = origin();
    cache.begin_anim_frame(Some(t0), false);
    let first = paint_notice(&cache, 7, "down", 20, &answers).expect("frame 0");
    assert_eq!(mark_notice(&cache, &first), vec![(t0, ANIM_INTERVAL)]);
    // A new width before the due tick recalls the stored frame without
    // advancing the counter or changing `last`.
    cache.begin_anim_frame(Some(at(t0, 200)), false);
    let wide = paint_notice(&cache, 7, "down", 24, &answers).expect("stored frame 0");
    assert!(text_of(&wide).starts_with("f0"), "{wide:?}");
    assert_eq!(answers.frames.borrow().clone(), vec![0, 0]);
    assert_eq!(mark_notice(&cache, &wide), vec![(t0, ANIM_INTERVAL)]);
    // The due tick then advances from the stored frame.
    cache.begin_anim_frame(Some(at(t0, 500)), false);
    let advanced = paint_notice(&cache, 7, "down", 24, &answers).expect("frame 1");
    assert!(text_of(&advanced).starts_with("f1"), "{advanced:?}");
    assert_eq!(answers.frames.borrow().clone(), vec![0, 0, 1]);
}

#[test]
fn a_dropped_page_restarts_items_but_not_notices() {
    crate::work::take();
    let cache = Cache::default();
    let arguments = serde_json::json!({"command": "ls"});
    let row = input("shell", "first", &arguments);
    let answers = AnimAnswers::new();
    let t0 = origin();
    cache.begin_anim_frame(Some(t0), false);
    paint(&cache, &row, 20, &answers).expect("item frame 0");
    let notice = paint_notice(&cache, 7, "down", 20, &answers).expect("notice frame 0");
    assert_eq!(mark_notice(&cache, &notice), vec![(t0, ANIM_INTERVAL)]);
    cache.drop_page(page(0));
    // The item restarts at frame 0 on its dropped page ...
    cache.begin_anim_frame(Some(at(t0, 500)), false);
    paint(&cache, &row, 20, &answers).expect("item frame 0 again");
    // ... while the notice stays keyed to its instance and stable page.
    // docs/tui.md:3021: the animation key is stable across page changes.
    let advanced = paint_notice(&cache, 7, "down", 20, &answers).expect("notice frame 1");
    assert!(text_of(&advanced).starts_with("f1"), "{advanced:?}");
    assert_eq!(answers.frames.borrow().clone(), vec![0, 0, 0, 1]);
}

/// A `notice` renderer that always fails, for failure-memo tests.
#[derive(Debug)]
struct FailingNotice;

impl crate::ext::Renderer for FailingNotice {
    fn name(&self) -> &str {
        "failing-test"
    }
    fn fills(&self, slot: &str) -> bool {
        slot == "notice"
    }
    fn render(
        &self,
        _ctx: &crate::ext::SessionCtx,
        _slot: &str,
        _input: &crate::ext::Input<'_>,
        _width: u16,
        _builtin: &dyn Fn(&crate::ext::Input<'_>, u16) -> Vec<Vec<crate::ext::RawSpan>>,
    ) -> Decision {
        Decision::Fail(failed("failing-test", "boom"))
    }
    fn animate_interval(
        &self,
        _ctx: &crate::ext::SessionCtx,
        _slot: &str,
    ) -> Option<std::time::Duration> {
        Some(ANIM_INTERVAL)
    }
    #[allow(
        clippy::too_many_arguments,
        reason = "the frame and its wall time travel with the draw context"
    )]
    fn render_animated(
        &self,
        _ctx: &crate::ext::SessionCtx,
        _slot: &str,
        _input: &crate::ext::Input<'_>,
        _width: u16,
        frame: u64,
        _now: u64,
        _builtin: &dyn Fn(&crate::ext::Input<'_>, u16) -> Vec<Vec<crate::ext::RawSpan>>,
    ) -> (Decision, bool) {
        let _ = frame;
        (Decision::Fail(failed("failing-test", "boom")), false)
    }
}

/// A failing `notice` bound on shared `cache` at page `index`, with
/// the frame wall time set, so animated draws run.
fn failing_bound(cache: Arc<Cache>, index: usize) -> crate::ext::Bound {
    let handle = crate::ext::Handle::create();
    handle.set(std::rc::Rc::new(FailingNotice));
    crate::ext::Bound {
        handle,
        cache,
        page: page(index),
        ctx: crate::ext::SessionCtx {
            session_id: Some(contract::SessionId("s".into())),
            halves: None,
        },
        flat: false,
        now: Some(1_000),
    }
}

#[test]
fn a_failed_notice_stays_failed_across_pages_and_re_raise() {
    crate::work::take();
    // One cache serves every page for notices: two bounds on different
    // pages share it, while notices key home.
    let cache = Arc::new(Cache::default());
    let before = failing_bound(Arc::clone(&cache), 0);
    let after = failing_bound(Arc::clone(&cache), 3);
    let t0 = origin();
    cache.begin_anim_frame(Some(t0), false);
    assert!(before.notice_lines(7, "io_failed", "x", 20).is_none());
    // A page change recalls nothing: the failure memo is page-stable.
    // docs/tui.md:3021: the animation key is stable across page changes.
    assert!(after.notice_lines(7, "io_failed", "x", 20).is_none());
    // A re-raise with the same input under a new instance stays failed.
    assert!(after.notice_lines(8, "io_failed", "x", 20).is_none());
    let listed = cache.take_failures();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed.first().expect("one failure").0, "notice");
}

#[test]
fn closed_entries_prune_with_the_instance_and_re_raise_starts_at_zero() {
    crate::work::take();
    let cache = Cache::default();
    let answers = AnimAnswers::new();
    let t0 = origin();
    cache.begin_anim_frame(Some(t0), false);
    let first = paint_notice(&cache, 7, "down", 20, &answers).expect("frame 0");
    assert_eq!(mark_notice(&cache, &first), vec![(t0, ANIM_INTERVAL)]);
    cache.begin_anim_frame(Some(at(t0, 500)), false);
    let second = paint_notice(&cache, 7, "down", 20, &answers).expect("frame 1");
    assert_eq!(
        mark_notice(&cache, &second),
        vec![(at(t0, 500), ANIM_INTERVAL)]
    );
    cache.begin_anim_frame(Some(at(t0, 1000)), false);
    let third = paint_notice(&cache, 7, "down", 20, &answers).expect("frame 2");
    assert_eq!(
        mark_notice(&cache, &third),
        vec![(at(t0, 1000), ANIM_INTERVAL)]
    );
    // The frame closed with the dues: a closed probe at a new width
    // recalls once at the stored frame without advancing or marking.
    let wide = paint_notice(&cache, 7, "down", 24, &answers).expect("stored frame 2");
    assert!(text_of(&wide).starts_with("f2"), "{wide:?}");
    assert_eq!(answers.frames.borrow().clone(), vec![0, 1, 2, 2]);
    // Pruning the dismissed instance drops every per-notice store.
    assert!(cache.has_notice_state(7));
    cache.retain_notices(&[]);
    assert!(!cache.has_notice_state(7));
    // The same text raised again is a new item: its first closed and
    // open draws both show frame 0.
    // docs/tui.md:3019: a re-raised notice is a new item and starts at frame 0.
    let fresh_answers = AnimAnswers::new();
    let cold = paint_notice(&cache, 8, "down", 20, &fresh_answers).expect("cold frame 0");
    assert!(text_of(&cold).starts_with("f0"), "{cold:?}");
    cache.begin_anim_frame(Some(at(t0, 1500)), false);
    let reopened = paint_notice(&cache, 8, "down", 20, &fresh_answers).expect("new frame 0");
    assert!(text_of(&reopened).starts_with("f0"), "{reopened:?}");
    assert_eq!(fresh_answers.frames.borrow().clone(), vec![0, 0]);
}
