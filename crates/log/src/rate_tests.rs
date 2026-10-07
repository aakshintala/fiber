//! The rate's arithmetic, its preamble size and its fold (`docs/tools.md`,
//! "Seeing the tools").

use super::*;
use contract::events::{
    CacheLifetime, Empty, Event, PreambleBuilt, PreambleReason, SentTool, ToolReplaced,
};
use contract::shapes::Tokens;
use contract::{ActionId, GenerationId, SCHEMA_VERSION, Seq, SessionId, TurnId};
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

fn sized(built: PreambleBuilt) -> u64 {
    preamble_size(&payload(&Event::PreambleBuilt(built)))
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

fn recorded(generation: &str, cache_write: Vec<(&str, u64)>) -> UsageRecorded {
    UsageRecorded {
        generation_id: GenerationId(generation.into()),
        model: "m".into(),
        tokens: Tokens {
            input: 10,
            cache_read: 0,
            cache_write: cache_write
                .into_iter()
                .map(|(lifetime, written)| (lifetime.to_owned(), written))
                .collect(),
            output: 3,
        },
        web_searches: None,
        cost: None,
        subscription: None,
        extension: None,
        origin_session_id: None,
    }
}

fn usage_line(action: Option<&str>, recorded: UsageRecorded) -> Envelope {
    envelope(
        "usage_recorded",
        action,
        payload(&Event::UsageRecorded(recorded)),
    )
}

fn own_usage(generation: &str, cache_write: Vec<(&str, u64)>) -> Envelope {
    usage_line(Some("a_1"), recorded(generation, cache_write))
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
fn tokens_estimates_bytes_from_the_first_requests_cache_write() {
    let cases = [
        (
            Rate {
                preamble: 0,
                written: None,
            },
            12,
            None,
        ),
        (
            Rate {
                preamble: 200,
                written: None,
            },
            12,
            None,
        ),
        (
            Rate {
                preamble: 0,
                written: Some(5),
            },
            12,
            None,
        ),
        (
            Rate {
                preamble: 200,
                written: Some(100),
            },
            12,
            Some(6),
        ),
        (
            Rate {
                preamble: 200,
                written: Some(100),
            },
            3,
            Some(1),
        ),
        (
            Rate {
                preamble: 200,
                written: Some(100),
            },
            1,
            Some(0),
        ),
        (
            Rate {
                preamble: 200,
                written: Some(100),
            },
            0,
            Some(0),
        ),
        (
            Rate {
                preamble: 200,
                written: Some(0),
            },
            12,
            Some(0),
        ),
        (
            Rate {
                preamble: u64::MAX,
                written: Some(u64::MAX),
            },
            u64::MAX,
            Some(u64::MAX),
        ),
        (
            Rate {
                preamble: 1,
                written: Some(u64::MAX),
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
fn preamble_size_counts_the_request_fields_compact_json() {
    // {"cache_lifetime":"5m","model":"m","system_prompt":"hi",
    //  "tool_choice":"pick","tools":[{"type":"object"}]}
    assert_eq!(sized(built(PreambleReason::Start, "hi")), 105);
}

#[test]
fn preamble_size_ignores_the_fields_no_request_sends() {
    let base = sized(built(PreambleReason::Start, "hi"));
    let mut reload = built(PreambleReason::Start, "hi");
    reload.reason = PreambleReason::Reload;
    assert_eq!(sized(reload), base);
    let mut window = built(PreambleReason::Start, "hi");
    window.context_window = 200_000;
    assert_eq!(sized(window), base);
    let mut trigger = built(PreambleReason::Start, "hi");
    trigger.trigger_at = Some(7);
    assert_eq!(sized(trigger), base);
    let mut replaced = built(PreambleReason::Start, "hi");
    replaced.replaced = vec![ToolReplaced {
        name: "read".into(),
        from: "a".into(),
        to: "b".into(),
    }];
    assert_eq!(sized(replaced), base);
    let mut by = built(PreambleReason::Start, "hi");
    by.tools[0].registered_by = "an-extension".into();
    assert_eq!(sized(by), base);
    let mut deferred = built(PreambleReason::Start, "hi");
    deferred.tools[0].deferred = true;
    assert_eq!(sized(deferred), base);
    let mut name = built(PreambleReason::Start, "hi");
    name.tools[0].name = "write-a-longer-name".into();
    assert_eq!(sized(name), base);
    // A tool with no definition contributes nothing to the request.
    let mut bare = payload(&Event::PreambleBuilt(built(PreambleReason::Start, "hi")));
    bare.get_mut("tools")
        .unwrap()
        .as_array_mut()
        .unwrap()
        .push(json!({"name": "ghost"}));
    assert_eq!(preamble_size(&bare), base);
}

#[test]
fn preamble_size_counts_the_fields_each_request_sends() {
    let base = sized(built(PreambleReason::Start, "hi"));
    let mut system = built(PreambleReason::Start, "hi");
    system.system_prompt = "hello".into();
    assert!(sized(system) > base);
    let mut model = built(PreambleReason::Start, "hi");
    model.model = "mm".into();
    assert!(sized(model) > base);
    let mut choice = built(PreambleReason::Start, "hi");
    choice.tool_choice = "choose".into();
    assert!(sized(choice) > base);
    // Both lifetimes serialize to two characters, so a longer stand-in
    // proves the value flows into the size.
    let mut lifetime = payload(&Event::PreambleBuilt(built(PreambleReason::Start, "hi")));
    lifetime.insert("cache_lifetime".into(), Value::from("1hour"));
    assert!(preamble_size(&lifetime) > base);
    let mut thinking = built(PreambleReason::Start, "hi");
    thinking.thinking = Some("high".into());
    assert!(sized(thinking) > base);
    let mut credential = built(PreambleReason::Start, "hi");
    credential.credential = Some("work".into());
    assert!(sized(credential) > base);
    let mut definition = built(PreambleReason::Start, "hi");
    definition.tools[0] = sent(json!({"type": "object", "required": []}));
    assert!(sized(definition) > base);
}

#[test]
fn preamble_size_ignores_the_lines_envelope() {
    let base = built(PreambleReason::Start, "hi");
    let mut line = preamble_line(&base);
    line.ts = 99;
    line.seq = Some(Seq(4));
    line.turn_id = Some(TurnId("t_9".into()));
    assert_tokens(
        vec![line, start_line("a_1"), own_usage("g_1", vec![("5m", 100)])],
        vec![None, None, Some(11)],
    );
}

#[test]
fn a_usage_before_any_preamble_leaves_the_next_build_waiting() {
    assert_tokens(
        vec![
            own_usage("g_0", vec![("5m", 100)]),
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
            own_usage("g_1", vec![("5m", 100)]),
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
            own_usage("g_1", vec![("5m", 100)]),
        ],
        vec![None, None, None, Some(11)],
    );
}

#[test]
fn only_the_builds_first_own_usage_counts() {
    let preamble = preamble_line(&built(PreambleReason::Start, "hi"));
    let start = start_line("a_1");
    let good = recorded("g_1", vec![("5m", 100)]);
    let ignored = [
        usage_line(None, recorded("g_x", vec![("5m", 1000)])),
        usage_line(
            Some("a_1"),
            UsageRecorded {
                extension: Some("x".into()),
                ..recorded("g_x", vec![("5m", 1000)])
            },
        ),
        usage_line(
            Some("a_1"),
            UsageRecorded {
                origin_session_id: Some(SessionId("s_2".into())),
                ..recorded("g_x", vec![("5m", 1000)])
            },
        ),
        usage_line(Some("a_9"), recorded("g_x", vec![("5m", 1000)])),
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
            own_usage("g_1", vec![("5m", 100)]),
            own_usage("g_1", vec![("5m", 555)]),
        ],
        vec![None, None, Some(11), Some(11)],
    );
}

#[test]
fn a_usage_for_a_request_started_before_the_latest_build_never_counts() {
    let build_b = built(PreambleReason::Reload, "a much longer system prompt");
    let rebuilt = Some(12 * 100 / sized(build_b.clone()));
    assert_tokens(
        vec![
            preamble_line(&built(PreambleReason::Start, "hi")),
            start_line("a_1"),
            own_usage("g_1", vec![("5m", 100)]),
            preamble_line(&build_b),
            own_usage("g_1", vec![("5m", 1000)]),
            start_line("a_2"),
            usage_line(Some("a_2"), recorded("g_2", vec![("5m", 40), ("1h", 60)])),
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
            own_usage("g_1", vec![("5m", 100)]),
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
            usage_line(Some("a_2"), recorded("g_2", vec![("5m", 100)])),
        ],
        vec![None, None, None, Some(11)],
    );
}

#[test]
fn a_new_build_resets_the_rate_until_its_first_usage() {
    let build_b = built(PreambleReason::Reload, "hello");
    let rebuilt = Some(12 * 100 / sized(build_b.clone()));
    assert_tokens(
        vec![
            preamble_line(&built(PreambleReason::Start, "hi")),
            start_line("a_1"),
            own_usage("g_1", vec![("5m", 100)]),
            preamble_line(&build_b),
            start_line("a_3"),
            usage_line(Some("a_3"), recorded("g_3", vec![("5m", 100)])),
        ],
        vec![None, None, Some(11), None, None, rebuilt],
    );
}

#[test]
fn the_cache_write_sums_every_lifetime_saturating() {
    let mut fold = RateFold::default();
    fold.fold(&preamble_line(&built(PreambleReason::Start, "hi")));
    fold.fold(&start_line("a_1"));
    fold.fold(&usage_line(
        Some("a_1"),
        recorded("g_1", vec![("5m", 40), ("1h", 60)]),
    ));
    assert_eq!(
        fold.rate(),
        Rate {
            preamble: 105,
            written: Some(100)
        }
    );
    let mut saturated = RateFold::default();
    saturated.fold(&preamble_line(&built(PreambleReason::Start, "hi")));
    saturated.fold(&start_line("a_1"));
    saturated.fold(&usage_line(
        Some("a_1"),
        recorded("g_1", vec![("5m", u64::MAX), ("1h", 1)]),
    ));
    assert_eq!(saturated.rate().tokens(u64::MAX), Some(u64::MAX));
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
        &Event::UsageRecorded(recorded("g_1", vec![("5m", 100)])),
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
