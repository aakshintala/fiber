//! The rate's arithmetic, its preamble size and its fold (`docs/tools.md`,
//! "Seeing the tools").

use super::*;
use contract::events::{CacheLifetime, Empty, Event, PreambleBuilt, PreambleReason, SentTool};
use contract::shapes::Tokens;
use contract::{ActionId, GenerationId, SCHEMA_VERSION, SessionId};
use serde_json::{Map, Value, json};

fn envelope(kind: &str, action: Option<&str>, payload: Map<String, Value>) -> Envelope {
    Envelope {
        kind: kind.into(),
        session_id: SessionId("s_1".into()),
        ts: 1,
        schema_version: SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|action| ActionId(action.into())),
        seq: None,
        payload,
    }
}

fn sent(definition: Value) -> SentTool {
    SentTool {
        name: "read".into(),
        registered_by: "builtin".into(),
        deferred: false,
        definition: serde_json::from_value(definition).unwrap(),
    }
}

fn built(reason: PreambleReason, system_prompt: &str) -> PreambleBuilt {
    PreambleBuilt {
        reason,
        model: "m".into(),
        context_window: 7,
        trigger_at: None,
        thinking: None,
        tool_choice: "pick".into(),
        cache_lifetime: CacheLifetime::FiveMinutes,
        credential: None,
        system_prompt: system_prompt.into(),
        tools: vec![sent(json!({"type": "object"}))],
        replaced: Vec::new(),
    }
}

fn payload(event: &Event) -> Map<String, Value> {
    event.payload().unwrap()
}

fn preamble_line(built: &PreambleBuilt) -> Envelope {
    envelope(
        "preamble_built",
        None,
        payload(&Event::PreambleBuilt(built.clone())),
    )
}

fn start_line(action: &str) -> Envelope {
    envelope(
        "assistant_message_started",
        Some(action),
        payload(&Event::AssistantMessageStarted(Empty {})),
    )
}

fn recorded(generation: &str) -> UsageRecorded {
    UsageRecorded {
        generation_id: GenerationId(generation.into()),
        model: "m".into(),
        tokens: Tokens {
            input: 10,
            cache_read: 0,
            cache_write: [("5m".to_owned(), 90)].into_iter().collect(),
            output: 3,
        },
        web_searches: None,
        cost: None,
        subscription: None,
        extension: None,
        origin_session_id: None,
        input_bytes: 105,
        input_media: None,
    }
}

fn usage_line(action: Option<&str>, recorded: UsageRecorded) -> Envelope {
    envelope(
        "usage_recorded",
        action,
        payload(&Event::UsageRecorded(recorded)),
    )
}

fn own_usage(generation: &str) -> Envelope {
    usage_line(Some("a_1"), recorded(generation))
}

/// `recorded` with its token counts replaced; the byte size and media flag
/// stay at their defaults.
fn counted(generation: &str, input: u64, cache_read: u64, pairs: &[(&str, u64)]) -> UsageRecorded {
    UsageRecorded {
        tokens: Tokens {
            input,
            cache_read,
            cache_write: pairs
                .iter()
                .map(|(lifetime, written)| ((*lifetime).to_owned(), *written))
                .collect(),
            output: 3,
        },
        ..recorded(generation)
    }
}

/// Folds `lines` one at a time, asserting `tokens(12)` after every line.
#[track_caller]
fn assert_tokens(lines: Vec<Envelope>, expected: Vec<Option<u64>>) {
    let mut fold = RateFold::default();
    assert_eq!(lines.len(), expected.len());
    for (line, want) in lines.into_iter().zip(expected) {
        fold.fold(&line);
        assert_eq!(fold.rate().tokens(12), want, "after {}", line.kind);
    }
}

#[test]
fn tokens_estimates_bytes_from_the_first_requests_input() {
    let cases = [
        (
            Rate {
                input_tokens: None,
                input_bytes: 0,
            },
            12,
            None,
        ),
        (
            Rate {
                input_tokens: None,
                input_bytes: 200,
            },
            12,
            None,
        ),
        (
            Rate {
                input_tokens: Some(5),
                input_bytes: 0,
            },
            12,
            None,
        ),
        (
            Rate {
                input_tokens: Some(100),
                input_bytes: 200,
            },
            12,
            Some(6),
        ),
        (
            Rate {
                input_tokens: Some(100),
                input_bytes: 200,
            },
            3,
            Some(1),
        ),
        (
            Rate {
                input_tokens: Some(100),
                input_bytes: 200,
            },
            1,
            Some(0),
        ),
        (
            Rate {
                input_tokens: Some(100),
                input_bytes: 200,
            },
            0,
            Some(0),
        ),
        (
            Rate {
                input_tokens: Some(0),
                input_bytes: 200,
            },
            12,
            Some(0),
        ),
        (
            Rate {
                input_tokens: Some(u64::MAX),
                input_bytes: u64::MAX,
            },
            u64::MAX,
            Some(u64::MAX),
        ),
        (
            Rate {
                input_tokens: Some(u64::MAX),
                input_bytes: 1,
            },
            2,
            Some(u64::MAX),
        ),
    ];
    for (rate, bytes, expected) in cases {
        assert_eq!(rate.tokens(bytes), expected);
    }
}

#[test]
fn a_usage_before_any_preamble_leaves_the_next_build_waiting() {
    assert_tokens(
        vec![
            own_usage("g_0"),
            preamble_line(&built(PreambleReason::Start, "hi")),
        ],
        vec![None, None],
    );
}

#[test]
fn a_build_its_start_and_its_usage_give_a_rate() {
    assert_tokens(
        vec![
            preamble_line(&built(PreambleReason::Start, "hi")),
            start_line("a_1"),
            own_usage("g_1"),
        ],
        vec![None, None, Some(11)],
    );
}

#[test]
fn a_line_of_another_kind_changes_nothing() {
    assert_tokens(
        vec![
            preamble_line(&built(PreambleReason::Start, "hi")),
            envelope("step_started", None, Map::new()),
            start_line("a_1"),
            own_usage("g_1"),
        ],
        vec![None, None, None, Some(11)],
    );
}

#[test]
fn only_the_builds_first_own_usage_counts() {
    let preamble = preamble_line(&built(PreambleReason::Start, "hi"));
    let start = start_line("a_1");
    let good = recorded("g_1");
    let ignored = [
        usage_line(None, recorded("g_x")),
        usage_line(
            Some("a_1"),
            UsageRecorded {
                extension: Some("x".into()),
                ..recorded("g_x")
            },
        ),
        usage_line(
            Some("a_1"),
            UsageRecorded {
                origin_session_id: Some(SessionId("s_2".into())),
                ..recorded("g_x")
            },
        ),
        usage_line(Some("a_9"), recorded("g_x")),
        usage_line(
            Some("a_1"),
            UsageRecorded {
                model: "other".into(),
                ..recorded("g_x")
            },
        ),
        usage_line(
            Some("a_1"),
            UsageRecorded {
                input_media: Some(true),
                ..recorded("g_x")
            },
        ),
        usage_line(
            Some("a_1"),
            UsageRecorded {
                input_bytes: 0,
                ..recorded("g_x")
            },
        ),
        envelope("usage_recorded", Some("a_1"), Map::new()),
    ];
    for bad in ignored {
        assert_tokens(
            vec![
                preamble.clone(),
                start.clone(),
                bad,
                usage_line(Some("a_1"), good.clone()),
            ],
            vec![None, None, None, Some(11)],
        );
    }
}

#[test]
fn the_first_usage_for_a_request_wins() {
    assert_tokens(
        vec![
            preamble_line(&built(PreambleReason::Start, "hi")),
            start_line("a_1"),
            own_usage("g_1"),
            own_usage("g_1"),
        ],
        vec![None, None, Some(11), Some(11)],
    );
}

#[test]
fn a_usage_for_a_request_started_before_the_latest_build_never_counts() {
    let rebuilt = Some(2);
    assert_tokens(
        vec![
            preamble_line(&built(PreambleReason::Start, "hi")),
            start_line("a_1"),
            own_usage("g_1"),
            preamble_line(&built(
                PreambleReason::Reload,
                "a much longer system prompt",
            )),
            own_usage("g_1"),
            start_line("a_2"),
            usage_line(
                Some("a_2"),
                UsageRecorded {
                    tokens: Tokens {
                        input: 50,
                        cache_read: 0,
                        cache_write: Default::default(),
                        output: 1,
                    },
                    input_bytes: 300,
                    ..recorded("g_2")
                },
            ),
        ],
        vec![None, None, Some(11), None, None, None, rebuilt],
    );
    assert!(rebuilt.is_some_and(|tokens| tokens != 11));
}

#[test]
fn a_request_in_flight_across_a_build_never_counts_for_the_new_build() {
    assert_tokens(
        vec![
            start_line("a_1"),
            preamble_line(&built(PreambleReason::Reload, "hi")),
            own_usage("g_1"),
        ],
        vec![None, None, None],
    );
}

#[test]
fn a_request_that_recorded_nothing_leaves_the_rate_waiting() {
    assert_tokens(
        vec![
            preamble_line(&built(PreambleReason::Start, "hi")),
            start_line("a_1"),
            start_line("a_2"),
            usage_line(Some("a_2"), recorded("g_2")),
        ],
        vec![None, None, None, Some(11)],
    );
}

#[test]
fn a_new_build_resets_the_rate_until_its_first_usage() {
    let rebuilt = Some(2);
    assert_tokens(
        vec![
            preamble_line(&built(PreambleReason::Start, "hi")),
            start_line("a_1"),
            own_usage("g_1"),
            preamble_line(&built(PreambleReason::Reload, "hello")),
            start_line("a_3"),
            usage_line(
                Some("a_3"),
                UsageRecorded {
                    tokens: Tokens {
                        input: 50,
                        cache_read: 0,
                        cache_write: Default::default(),
                        output: 1,
                    },
                    input_bytes: 300,
                    ..recorded("g_3")
                },
            ),
        ],
        vec![None, None, Some(11), None, None, rebuilt],
    );
}

#[test]
fn every_input_token_counts_whether_cached_or_not() {
    for (input, cache_read, pairs) in [
        (100, 0, &[("5m", 0)][..]),
        (10, 0, &[("5m", 90)][..]),
        (1, 99, &[][..]),
        (1, 9, &[("5m", 40), ("1h", 50)][..]),
    ] {
        assert_tokens(
            vec![
                preamble_line(&built(PreambleReason::Start, "hi")),
                start_line("a_1"),
                usage_line(Some("a_1"), counted("g_1", input, cache_read, pairs)),
            ],
            vec![None, None, Some(11)],
        );
    }
}

#[test]
fn a_first_request_with_no_cache_write_gives_a_rate() {
    assert_tokens(
        vec![
            preamble_line(&built(PreambleReason::Start, "hi")),
            start_line("a_1"),
            usage_line(
                Some("a_1"),
                UsageRecorded {
                    input_bytes: 16_000,
                    ..counted("g_1", 4000, 0, &[])
                },
            ),
        ],
        vec![None, None, Some(3)],
    );
}

#[test]
fn a_request_with_media_is_skipped_for_the_next_without() {
    assert_tokens(
        vec![
            preamble_line(&built(PreambleReason::Start, "hi")),
            start_line("a_1"),
            usage_line(
                Some("a_1"),
                UsageRecorded {
                    tokens: Tokens {
                        input: 10,
                        cache_read: 0,
                        cache_write: [("5m".to_owned(), 5000)].into_iter().collect(),
                        output: 3,
                    },
                    input_media: Some(true),
                    ..recorded("g_1")
                },
            ),
            start_line("a_2"),
            usage_line(Some("a_2"), recorded("g_2")),
        ],
        vec![None, None, None, None, Some(11)],
    );
}

#[test]
fn input_media_false_counts_like_absent() {
    assert_tokens(
        vec![
            preamble_line(&built(PreambleReason::Start, "hi")),
            start_line("a_1"),
            usage_line(
                Some("a_1"),
                UsageRecorded {
                    input_media: Some(false),
                    ..recorded("g_1")
                },
            ),
        ],
        vec![None, None, Some(11)],
    );
}

#[test]
fn a_zero_byte_request_is_skipped_for_the_next() {
    assert_tokens(
        vec![
            preamble_line(&built(PreambleReason::Start, "hi")),
            start_line("a_1"),
            usage_line(
                Some("a_1"),
                UsageRecorded {
                    input_bytes: 0,
                    ..recorded("g_1")
                },
            ),
            start_line("a_2"),
            usage_line(Some("a_2"), recorded("g_2")),
        ],
        vec![None, None, None, None, Some(11)],
    );
}

#[test]
fn a_request_that_reported_no_input_tokens_is_skipped_for_the_next() {
    // A call that failed before its provider named a generation reports no
    // input tokens; output alone does not count.
    assert_tokens(
        vec![
            preamble_line(&built(PreambleReason::Start, "hi")),
            start_line("a_1"),
            usage_line(
                Some("a_1"),
                UsageRecorded {
                    generation_id: GenerationId("fiber-0123456789abcdef".into()),
                    input_bytes: 900,
                    ..counted("g_1", 0, 0, &[])
                },
            ),
            start_line("a_2"),
            usage_line(
                Some("a_2"),
                UsageRecorded {
                    input_bytes: 900,
                    ..counted("g_2", 300, 0, &[])
                },
            ),
        ],
        vec![None, None, None, None, Some(4)],
    );
}

#[test]
fn another_models_usage_never_counts() {
    assert_tokens(
        vec![
            preamble_line(&built(PreambleReason::Start, "hi")),
            start_line("a_1"),
            usage_line(
                Some("a_1"),
                UsageRecorded {
                    model: "other".into(),
                    ..recorded("g_1")
                },
            ),
            start_line("a_2"),
            usage_line(Some("a_2"), recorded("g_2")),
        ],
        vec![None, None, None, None, Some(11)],
    );
}

#[test]
fn input_tokens_sums_every_kind_saturating() {
    let full = UsageRecorded {
        tokens: Tokens {
            input: 1,
            cache_read: 2,
            cache_write: [("5m".to_owned(), 4), ("1h".to_owned(), 8)]
                .into_iter()
                .collect(),
            output: 3,
        },
        ..recorded("g_1")
    };
    assert_eq!(input_tokens(&full), 15);
    let saturated = UsageRecorded {
        tokens: Tokens {
            input: u64::MAX,
            cache_read: 1,
            cache_write: Default::default(),
            output: 3,
        },
        ..recorded("g_1")
    };
    assert_eq!(input_tokens(&saturated), u64::MAX);
    let mut fold = RateFold::default();
    fold.fold(&preamble_line(&built(PreambleReason::Start, "hi")));
    fold.fold(&start_line("a_1"));
    fold.fold(&usage_line(Some("a_1"), recorded("g_1")));
    assert_eq!(
        fold.rate(),
        Rate {
            input_tokens: Some(100),
            input_bytes: 105
        }
    );
}

#[test]
fn the_rate_folds_appended_lines_and_survives_a_reopen() {
    let sessions = fakes::TempDir::new("log-unit-rate");
    let id = SessionId("s_1".into());
    let log =
        crate::write::Log::create(sessions.path(), id.clone(), fakes::clock::FakeClock::new())
            .unwrap();
    let build = built(PreambleReason::Start, "hi");
    log.append(&Event::PreambleBuilt(build), None, None)
        .unwrap();
    log.append(
        &Event::AssistantMessageStarted(Empty {}),
        None,
        Some(ActionId("a_1".into())),
    )
    .unwrap();
    log.append(
        &Event::UsageRecorded(recorded("g_1")),
        None,
        Some(ActionId("a_1".into())),
    )
    .unwrap();
    let rate = log.rate();
    assert_eq!(rate.tokens(12), Some(11));
    drop(log);
    let opened =
        crate::write::Log::open(sessions.path(), id, fakes::clock::FakeClock::new()).unwrap();
    assert_eq!(opened.rate(), rate);
}
