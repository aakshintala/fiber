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
    AnswerDisposition, CallState, CardPrompt, Failure, Input, LedgerResult, LedgerRow, Prompt,
    PromptImage, RawSpan, ReplyText, ThinkingLine, TurnCard, TurnCardState, TurnEnded,
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
        // docs/tui.md:2674: an unrelated session-half change does not rerun
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
        // docs/tui.md:2673: only output-relevant page/winner/half/input/width changes rerun.
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
        // docs/tui.md:2673: a change to any key input reruns the renderer.
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
        assert_eq!(calls.get(), 5); // docs/tui.md:2674: failure key excludes width.
        cache.queue_notice(("command_conflict".into(), "conflict".into()));
        cache.drop_page(page("s", 0));
        assert_eq!(cache.take_failures().len(), 2); // docs/tui.md:2672: only the exact page is pruned.
        assert_eq!(cache.take_notices().len(), 1); // docs/tui.md:2678: selection notices live until /reload, with no page lifetime.
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
        // docs/tui.md:2734: one failure notice per extension and slot until /reload.
        assert_eq!(durable.take_failures().len(), 1);
        // docs/tui.md:2678: one extension_incompatible per extension and session-half version until /reload.
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
        // docs/tui.md:2734: one failure notice per extension and slot until /reload.
        assert_eq!(durable.take_failures().len(), 1);
        // docs/tui.md:2757: one command_conflict per slot and set of contenders until /reload.
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
        // docs/tui.md:2734: one failure notice per extension and slot until /reload.
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
