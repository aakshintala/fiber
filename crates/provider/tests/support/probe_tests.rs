use std::collections::BTreeMap;

use contract::ActionId;
use contract::events::{ReasoningCompleted, TextCompleted, ToolCallRequested};
use contract::provider::{Finish, Input, InputSize};
use contract::shapes::Tokens;
use serde_json::json;

use super::*;

fn words(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn failed_outcome(status: Option<u16>, step: Step, message: &str, spend: (u64, u64)) -> Outcome {
    failed(step, status, message, spend)
}

#[test]
fn refusal_status_is_auth_at_every_step() {
    for step in [Step::Baseline, Step::Deferred, Step::AfterLoad] {
        assert_eq!(classify(Some(401), step), Kind::Auth);
        assert_eq!(classify(Some(403), step), Kind::Auth);
    }
}

#[test]
fn deferral_rejection_fails_only_past_the_baseline() {
    for step in [Step::Deferred, Step::AfterLoad] {
        assert_eq!(classify(Some(400), step), Kind::Fail);
        assert_eq!(classify(Some(422), step), Kind::Fail);
    }
    assert_eq!(classify(Some(400), Step::Baseline), Kind::Inconclusive);
    assert_eq!(classify(Some(422), Step::Baseline), Kind::Inconclusive);
}

#[test]
fn every_other_status_and_no_status_cannot_tell() {
    for status in [
        Some(200),
        Some(402),
        Some(404),
        Some(408),
        Some(413),
        Some(429),
        Some(499),
        Some(500),
        Some(503),
        None,
    ] {
        for step in [Step::Baseline, Step::Deferred, Step::AfterLoad] {
            assert_eq!(
                classify(status, step),
                Kind::Inconclusive,
                "{status:?} {step:?}"
            );
        }
    }
}

#[test]
fn any_cache_read_holds() {
    assert!(!cache_held(0));
    assert!(cache_held(1));
    assert!(cache_held(u64::MAX));
}

#[test]
fn total_input_sums_input_reads_and_every_write_class() {
    let tokens = Tokens {
        input: 300,
        cache_read: 5120,
        cache_write: BTreeMap::from([("1h".to_owned(), 7), ("5m".to_owned(), 11)]),
        output: 999,
    };
    assert_eq!(total_input(&tokens), 300 + 5120 + 7 + 11);
}

#[test]
fn meter_sums_input_and_output_across_replies() {
    let mut spend = (0u64, 0u64);
    for tokens in [
        Tokens {
            input: 12000,
            cache_read: 0,
            cache_write: BTreeMap::new(),
            output: 3,
        },
        Tokens {
            input: 5200,
            cache_read: 0,
            cache_write: BTreeMap::new(),
            output: 5,
        },
        Tokens {
            input: 300,
            cache_read: 5120,
            cache_write: BTreeMap::from([("5m".to_owned(), 11)]),
            output: 7,
        },
    ] {
        meter(&mut spend, &tokens);
    }
    assert_eq!(spend, (12000 + 5200 + 300 + 5120 + 11, 3 + 5 + 7));
}

#[test]
fn exit_code_prefers_refusal_then_failure_then_cannot_tell() {
    assert_eq!(
        exit_code(&Outcome::KeyRefused {
            status: 401,
            spend_input: 0,
            spend_output: 0,
        }),
        4
    );
    assert_eq!(
        exit_code(&Outcome::KeyRefused {
            status: 403,
            spend_input: 0,
            spend_output: 0,
        }),
        4
    );
    let verdicts = [
        Verdict::Fail("no".into()),
        Verdict::Inconclusive("no".into()),
        Verdict::Pass("yes".into()),
        Verdict::NotEstablished("no".into()),
    ];
    for load in &verdicts {
        for cache in &verdicts {
            let outcome = Outcome::Decided {
                load: load.clone(),
                cache: cache.clone(),
                spend_input: 0,
                spend_output: 0,
            };
            let expected = if matches!(load, Verdict::Fail(_)) || matches!(cache, Verdict::Fail(_))
            {
                1
            } else if matches!(load, Verdict::Inconclusive(_))
                || matches!(cache, Verdict::Inconclusive(_))
            {
                3
            } else {
                0
            };
            assert_eq!(exit_code(&outcome), expected, "{load:?} {cache:?}");
        }
    }
}

#[test]
fn parse_args_rejects_too_few_unknown_and_bad_options() {
    assert!(parse_args(&words(&[])).is_err());
    assert!(parse_args(&words(&["anthropic-messages", "https://x", "m"])).is_err());
    let base = ["anthropic-messages", "https://x", "m", "key", "--unknown"];
    let error = parse_args(&words(&base)).unwrap_err();
    assert!(error.contains("--unknown"), "{error}");
    let bad_store = ["openai-responses", "https://x", "m", "key", "--store=maybe"];
    let error = parse_args(&words(&bad_store)).unwrap_err();
    assert!(error.contains("--store"), "{error}");
    let bare = ["anthropic-messages", "https://x", "m", "key", "no-colon"];
    let error = parse_args(&words(&bare)).unwrap_err();
    assert!(error.contains("NAME:VALUE"), "{error}");
    let body_file = ["anthropic-messages", "https://x", "m", "key", "@body.json"];
    let error = parse_args(&words(&body_file)).unwrap_err();
    assert!(error.contains("NAME:VALUE"), "{error}");
    let bad_protocol = ["smtp", "https://x", "m", "key"];
    let error = parse_args(&words(&bad_protocol)).unwrap_err();
    assert!(error.contains("smtp"), "{error}");
}

#[test]
fn parse_args_accepts_options_in_any_order() {
    let args = words(&[
        "openai-responses",
        "https://x",
        "m",
        "env:KEY",
        "X-A: 1",
        "--store=false",
        "--bearer",
    ]);
    assert_eq!(
        parse_args(&args).unwrap(),
        Parsed {
            protocol: "openai-responses".into(),
            base_url: "https://x".into(),
            model: "m".into(),
            key_spec: "env:KEY".into(),
            bearer: true,
            store: Some(false),
            headers: vec![("X-A".into(), "1".into())],
        }
    );
}

#[test]
fn debug_prints_header_names_and_redacts_their_values() {
    let parsed = Parsed {
        protocol: "openai-responses".into(),
        base_url: "https://x".into(),
        model: "m".into(),
        key_spec: "env:KEY".into(),
        bearer: true,
        store: Some(false),
        headers: vec![("Authorization".into(), "Bearer sekrit-value".into())],
    };
    let shown = format!("{parsed:?}");
    assert!(shown.contains("Authorization"), "{shown}");
    assert!(!shown.contains("sekrit-value"), "{shown}");
}

#[test]
fn key_from_reads_env_and_files_and_names_what_is_missing() {
    let env = |name: &str| {
        if name == "PRESENT" {
            Some("  secret \n".to_owned())
        } else {
            None
        }
    };
    assert_eq!(key_from("env:PRESENT", &env).unwrap(), "secret");
    let missing = key_from("env:ABSENT", &env).unwrap_err();
    assert!(missing.contains("ABSENT"), "{missing}");
    assert!(!missing.contains("secret"));
    let dir = fakes::TempDir::new("fiber-probe-key");
    let path = dir.path().join("key");
    std::fs::write(&path, "file-secret\n").unwrap();
    let text = path.display().to_string();
    assert_eq!(key_from(&text, &env).unwrap(), "file-secret");
    let absent = dir.path().join("absent");
    let error = key_from(&absent.display().to_string(), &env).unwrap_err();
    assert!(error.contains("absent"), "{error}");
    std::fs::write(&path, "  \n").unwrap();
    assert!(key_from(&text, &env).unwrap_err().contains("empty"));
}

#[test]
fn baseline_declares_every_tool_in_full() {
    let request = baseline_request("probe/m");
    let names: Vec<&str> = request
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect();
    assert_eq!(
        names,
        vec![
            "tool_search",
            "probe_calendar_create",
            "probe_calendar_list",
            "probe_calendar_delete"
        ]
    );
    assert!(request.tools.iter().all(|tool| !tool.deferred));
    assert!(request.system_prompt.len() >= SYSTEM_MIN_CHARS);
    for tool in &request.tools {
        assert!(tool.description.len() >= TOOL_MIN_CHARS, "{}", tool.name);
    }
    assert_eq!(request.max_output_tokens, Some(2000));
    assert!(request.thinking.is_none());
    assert_eq!(request.tool_choice, "auto");
    assert_eq!(request.cache_key, "probe-probe/m");
    assert!(request.previous_end.is_none());
    assert!(request.sent_tools.is_none());
    assert!(!request.system_prompt.contains("defer_loading"));
    assert!(!request.system_prompt.contains("tool_reference"));
    assert!(!request.system_prompt.contains("tool_search_output"));
}

#[test]
fn first_request_defers_only_the_calendar_tools() {
    let request = first_request("probe/m");
    for tool in &request.tools {
        assert_eq!(tool.deferred, tool.name != "tool_search", "{}", tool.name);
    }
    assert_eq!(request.cache_key, baseline_request("probe/m").cache_key);
}

fn search_call() -> ToolCallRequested {
    ToolCallRequested {
        name: "tool_search".into(),
        arguments: json!({"query": "calendar"}),
        provider_id: None,
        repair: None,
        ran_by: None,
        provider_item: None,
    }
}

fn first_reply(actions: Vec<ReplyAction>) -> Reply {
    Reply {
        actions,
        finish: Finish::Completed,
        generation_id: None,
        tokens: Tokens {
            input: 0,
            cache_read: 0,
            cache_write: BTreeMap::new(),
            output: 0,
        },
        web_searches: None,
        cost: None,
        input_size: InputSize {
            bytes: 0,
            media: false,
        },
    }
}

#[test]
fn after_load_replays_the_first_reply_and_answers_its_search() {
    let first = first_reply(vec![
        ReplyAction::Reasoning(ReasoningCompleted {
            text: "on it".into(),
            provider_item: Some(json!({"type": "reasoning"})),
        }),
        ReplyAction::ToolCall(search_call()),
    ]);
    let after = after_load_request("probe/m", &first);
    assert_eq!(after.previous_end, Some(1));
    assert_eq!(after.cache_key, first_request("probe/m").cache_key);
    assert_eq!(after.tools, first_request("probe/m").tools);
    let conversation: Vec<&Input> = after.conversation.iter().collect();
    assert_eq!(conversation.len(), 4);
    assert!(matches!(conversation[0], Input::User { .. }));
    assert!(matches!(conversation[1], Input::Reasoning { .. }));
    let Input::ToolCall {
        action_id,
        call,
        model,
    } = conversation[2]
    else {
        panic!("{conversation:?}");
    };
    assert_eq!(action_id, &ActionId("probe-2".into()));
    assert_eq!(call.name, "tool_search");
    assert_eq!(model, "probe/m");
    assert_eq!(
        conversation[3],
        &Input::ToolSearchResult {
            action_id: ActionId("probe-2".into()),
            loaded: vec!["probe_calendar_create".into()],
        }
    );
}

#[test]
fn after_load_marks_an_unsearched_call_as_not_run() {
    let mut other = search_call();
    other.name = "probe_calendar_list".into();
    let first = first_reply(vec![
        ReplyAction::Text(TextCompleted {
            text: "hi".into(),
            provider_item: None,
        }),
        ReplyAction::ToolCall(search_call()),
        ReplyAction::ToolCall(other),
    ]);
    let after = after_load_request("probe/m", &first);
    let conversation: Vec<&Input> = after.conversation.iter().collect();
    assert_eq!(conversation.len(), 6);
    assert!(matches!(conversation[1], Input::Assistant { .. }));
    let Input::ToolResult {
        action_id,
        text,
        is_error,
        ..
    } = conversation[5]
    else {
        panic!("{conversation:?}");
    };
    assert_eq!(action_id, &ActionId("probe-3".into()));
    assert_eq!(text, "not run by the probe");
    assert!(is_error);
}

#[test]
fn report_prints_a_passing_run() {
    let outcome = Outcome::Decided {
        load: Verdict::Pass("searched, loaded probe_calendar_create, then called it".into()),
        cache: Verdict::Pass("cache read 5120 of 5420 input tokens".into()),
        spend_input: 22620,
        spend_output: 15,
    };
    assert_eq!(
        report("openai-responses", "gpt-5.4", &outcome),
        vec![
            "probe: openai-responses gpt-5.4",
            "load: pass (searched, loaded probe_calendar_create, then called it)",
            "cache: pass (cache read 5120 of 5420 input tokens)",
            "probe: pass",
            "spend: 22620 input, 15 output tokens",
        ]
    );
    assert_eq!(exit_code(&outcome), 0);
}

#[test]
fn report_prints_a_failed_load() {
    let outcome = Outcome::Decided {
        load: Verdict::Fail("HTTP 400: defer_loading is not supported".into()),
        cache: Verdict::NotEstablished("load rejected".into()),
        spend_input: 12000,
        spend_output: 3,
    };
    assert_eq!(
        report("anthropic-messages", "probe-model", &outcome),
        vec![
            "probe: anthropic-messages probe-model",
            "load: fail (HTTP 400: defer_loading is not supported)",
            "cache: fail (not established: load rejected)",
            "probe: fail",
            "spend: 12000 input, 3 output tokens",
        ]
    );
    assert_eq!(exit_code(&outcome), 1);
}

#[test]
fn report_prints_an_inconclusive_baseline() {
    let outcome = failed_outcome(Some(404), Step::Baseline, "not found", (0, 0));
    assert_eq!(
        report("anthropic-messages", "probe-model", &outcome),
        vec![
            "probe: anthropic-messages probe-model",
            "load: fail (inconclusive: baseline: HTTP 404)",
            "cache: fail (not established: baseline inconclusive)",
            "probe: cannot tell (baseline: HTTP 404)",
            "spend: 0 input, 0 output tokens",
        ]
    );
    assert_eq!(exit_code(&outcome), 3);
}

#[test]
fn report_prints_a_refused_key_and_hides_it() {
    let outcome = Outcome::KeyRefused {
        status: 401,
        spend_input: 0,
        spend_output: 0,
    };
    let lines = report("anthropic-messages", "probe-model", &outcome);
    assert_eq!(
        lines,
        vec![
            "probe: anthropic-messages probe-model",
            "load: fail (not established: key refused)",
            "cache: fail (not established: key refused)",
            "probe: key refused (HTTP 401)",
        ]
    );
    assert_eq!(exit_code(&outcome), 4);
    for line in &lines {
        assert!(!line.contains("sekrit-value"), "{line}");
    }
}

#[test]
fn refusal_after_one_metered_reply_reports_that_reply_spend() {
    let outcome = failed_outcome(Some(403), Step::Deferred, "key refused", (12000, 3));
    assert_eq!(
        outcome,
        Outcome::KeyRefused {
            status: 403,
            spend_input: 12000,
            spend_output: 3,
        }
    );
    assert_eq!(
        report("anthropic-messages", "probe-model", &outcome),
        vec![
            "probe: anthropic-messages probe-model",
            "load: fail (not established: key refused)",
            "cache: fail (not established: key refused)",
            "probe: key refused (HTTP 403)",
            "spend: 12000 input, 3 output tokens",
        ]
    );
    assert_eq!(exit_code(&outcome), 4);
}
