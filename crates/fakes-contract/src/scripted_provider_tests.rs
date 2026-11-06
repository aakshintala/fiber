use contract::events::TextDelta;
use contract::provider::{CallError, CallUsage, Delta, InputSize, ModelRequest, Provider};
use contract::shapes::Failure;
use contract::{ErrorCode, events::CacheLifetime};

use super::{Scripted, ScriptedProvider, reply};

fn request(text: &str) -> ModelRequest {
    ModelRequest {
        system_prompt: text.into(),
        tools: Vec::new(),
        thinking: None,
        tool_choice: "auto".into(),
        cache_lifetime: CacheLifetime::OneHour,
        cache_key: "s_1".into(),
        conversation: (Vec::new()).into(),
        previous_end: None,
        sent_tools: None,
        max_output_tokens: None,
        media: Default::default(),
    }
}

fn text(delta: &Delta) -> &str {
    match delta {
        Delta::Text(TextDelta { text }) => text,
        Delta::Reasoning(_) | Delta::ToolCallArguments(_) => panic!("not text: {delta:?}"),
    }
}

#[test]
fn calls_take_the_script_in_order_and_requests_are_recorded() {
    let provider = ScriptedProvider::new([Scripted::text("Hello"), Scripted::text("Bye")]);
    for (n, expected) in ["Hello", "Bye"].into_iter().enumerate() {
        let mut deltas = Vec::new();
        let got = provider
            .call(&request(&format!("r{n}")))
            .run(&mut |d| deltas.push(d))
            .unwrap();
        assert_eq!(got, reply(expected));
        let streamed: String = deltas.iter().map(text).collect();
        assert_eq!(streamed, expected);
        assert_eq!(deltas.len(), 2);
    }
    let prompts: Vec<String> = provider
        .requests()
        .into_iter()
        .map(|r| r.system_prompt)
        .collect();
    assert_eq!(prompts, ["r0", "r1"]);
}

#[test]
fn text_splits_on_a_character_boundary() {
    let scripted = Scripted::text("héllo");
    let parts: Vec<&str> = scripted.deltas.iter().map(text).collect();
    assert_eq!(parts, ["hé", "llo"]);
    assert_eq!(Scripted::text("").deltas, Vec::new());
}

#[test]
fn a_call_past_the_script_fails_script_exhausted() {
    let provider = ScriptedProvider::new([]);
    let end = provider.call(&request("r")).run(&mut |_| {});
    let Err(CallError::Failed { failure, .. }) = end else {
        panic!("{end:?}");
    };
    assert_eq!(failure.code, ErrorCode::Other("script_exhausted".into()));
}

#[test]
fn a_cancelled_call_returns_cancelled_and_streams_nothing() {
    let provider = ScriptedProvider::new([Scripted::text("Hello")]);
    let call = provider.call(&request("r"));
    call.cancel();
    let mut deltas = Vec::new();
    assert_eq!(
        call.run(&mut |d| deltas.push(d)),
        Err(CallError::Cancelled {
            usage: Box::new(unnamed_1000())
        })
    );
    assert!(deltas.is_empty());
}

/// What a scripted call that ended before any generation carries.
fn unnamed_1000() -> CallUsage {
    CallUsage::unnamed(InputSize {
        bytes: 1000,
        media: false,
    })
}

#[test]
fn a_call_failed_before_any_generation_carries_an_unnamed_usage() {
    let provider = ScriptedProvider::new([Scripted::failed(Failure {
        code: ErrorCode::Timeout,
        message: "slow".into(),
        retry_after_ms: None,
        provider: None,
    })]);
    let end = provider.call(&request("r")).run(&mut |_| {});
    let Err(error) = end else {
        panic!("{end:?}");
    };
    assert_eq!(error.usage(), &unnamed_1000());
}

#[test]
fn a_call_cancelled_mid_stream_carries_an_unnamed_usage() {
    let provider = ScriptedProvider::new([Scripted::text("Hello")]);
    let call = provider.call(&request("r"));
    let mut deltas = Vec::new();
    let end = call.run(&mut |d| {
        deltas.push(d);
        call.cancel();
    });
    assert_eq!(deltas.len(), 1);
    assert_eq!(
        end,
        Err(CallError::Cancelled {
            usage: Box::new(unnamed_1000())
        })
    );
}

#[test]
fn an_unnamed_usage_has_no_generation_and_call_usage_s_counts() {
    let unnamed = super::unnamed_usage();
    assert_eq!(unnamed.generation_id, None);
    assert_eq!(unnamed.tokens.input, 10);
    assert_eq!(unnamed.tokens.output, 3);
    assert_eq!(unnamed.input_size.bytes, 1000);
}

#[test]
fn a_call_runs_once() {
    let provider = ScriptedProvider::new([Scripted::text("Hello")]);
    let call = provider.call(&request("r"));
    assert!(call.run(&mut |_| {}).is_ok());
    assert_eq!(
        call.run(&mut |_| {}),
        Err(CallError::Cancelled {
            usage: Box::new(unnamed_1000())
        })
    );
}

#[test]
fn a_cancelled_call_keeps_a_scripted_cancelled_end_and_drops_any_other() {
    use super::call_usage;
    let usage = call_usage("gen_kept");
    let provider = ScriptedProvider::new([Scripted::cancelled_after(usage.clone())]);
    let call = provider.call(&request("r"));
    call.cancel();
    assert_eq!(
        call.run(&mut |_| {}),
        Err(CallError::Cancelled {
            usage: Box::new(usage)
        })
    );
    let provider = ScriptedProvider::new([Scripted::text("Hello")]);
    let call = provider.call(&request("r"));
    call.cancel();
    assert_eq!(
        call.run(&mut |_| {}),
        Err(CallError::Cancelled {
            usage: Box::new(unnamed_1000())
        })
    );
}

fn definitions() -> Vec<contract::provider::ToolDefinition> {
    ["b", "a"]
        .iter()
        .map(|name| contract::provider::ToolDefinition {
            name: (*name).into(),
            description: format!("the {name} tool"),
            input_schema: serde_json::json!({"type": "object"}),
            deferred: *name == "a",
            hosted: None,
        })
        .collect()
}

#[test]
fn wire_tools_is_each_definition_as_an_object_in_name_order() {
    let tools = definitions();
    let wired = super::wire_tools(&tools);
    let names: Vec<&str> = wired
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["a", "b"]);
    for object in &wired {
        let tool = tools.iter().find(|t| t.name == object["name"]).unwrap();
        assert_eq!(
            serde_json::Value::Object(object.clone()),
            serde_json::to_value(tool).unwrap()
        );
    }
}

#[test]
fn a_before_call_hook_observes_each_request_before_the_script_pop() {
    use std::sync::{Arc, Mutex};
    let seen = Arc::new(Mutex::new(Vec::new()));
    let moved = Arc::clone(&seen);
    let provider = ScriptedProvider::new([Scripted::text("A"), Scripted::text("B")]);
    provider.on_before_call(move |request: &contract::provider::ModelRequest| {
        moved.lock().unwrap().push(request.system_prompt.clone());
    });
    for n in 0..2 {
        let mut deltas = Vec::new();
        let end = provider
            .call(&request(&format!("r{n}")))
            .run(&mut |d| deltas.push(d));
        assert!(end.is_ok(), "{end:?}");
    }
    // The hook sees each request in order, before the script pop
    // (docs/testing/fakes.md, "Fakes").
    assert_eq!(*seen.lock().unwrap(), ["r0", "r1"]);
    let prompts: Vec<String> = provider
        .requests()
        .into_iter()
        .map(|r| r.system_prompt)
        .collect();
    // Requests are recorded in call order (docs/testing/fakes.md, "Fakes").
    assert_eq!(prompts, ["r0", "r1"]);
}

#[test]
fn a_before_call_hook_at_the_second_call_fires_only_there() {
    use std::sync::{Arc, Mutex};
    let seen = Arc::new(Mutex::new(Vec::new()));
    let moved = Arc::clone(&seen);
    let provider = ScriptedProvider::new([
        Scripted::text("A"),
        Scripted::text("B"),
        Scripted::text("C"),
    ]);
    provider.on_before_call_at(2, move |request: &contract::provider::ModelRequest| {
        moved.lock().unwrap().push(request.system_prompt.clone());
    });
    for n in 0..3 {
        let mut deltas = Vec::new();
        let end = provider
            .call(&request(&format!("r{n}")))
            .run(&mut |d| deltas.push(d));
        assert!(end.is_ok(), "{end:?}");
    }
    // The hook fires only on the numbered call (docs/testing/fakes.md, "Fakes").
    assert_eq!(*seen.lock().unwrap(), ["r1"]);
}

#[test]
fn an_after_reply_hook_observes_each_end_and_returns_it_unchanged() {
    use std::sync::{Arc, Mutex};
    let ends = Arc::new(Mutex::new(Vec::new()));
    let moved = Arc::clone(&ends);
    let provider = ScriptedProvider::new([Scripted::text("A"), Scripted::text("B")]);
    provider.on_after_reply(move |end: &Result<contract::provider::Reply, CallError>| {
        moved.lock().unwrap().push(format!("{end:?}"));
    });
    for expected in ["A", "B"] {
        let mut deltas = Vec::new();
        let got = provider
            .call(&request("r"))
            .run(&mut |d| deltas.push(d))
            .unwrap();
        assert_eq!(got, reply(expected));
    }
    // The hook sees each end in call order, once per call
    // (docs/testing/fakes.md, "Fakes").
    assert_eq!(ends.lock().unwrap().len(), 2);
    assert_eq!(
        ends.lock().unwrap()[0],
        format!("{:?}", Ok::<_, CallError>(reply("A")))
    );
    assert_eq!(
        ends.lock().unwrap()[1],
        format!("{:?}", Ok::<_, CallError>(reply("B")))
    );
}

#[test]
fn an_after_reply_hook_at_the_second_call_fires_only_there() {
    use std::sync::{Arc, Mutex};
    let fired = Arc::new(Mutex::new(Vec::new()));
    let moved = Arc::clone(&fired);
    let provider = ScriptedProvider::new([
        Scripted::text("A"),
        Scripted::text("B"),
        Scripted::text("C"),
    ]);
    provider.on_after_reply_at(2, move |_| {
        moved.lock().unwrap().push(());
    });
    for expected in ["A", "B", "C"] {
        let mut deltas = Vec::new();
        let got = provider
            .call(&request("r"))
            .run(&mut |d| deltas.push(d))
            .unwrap();
        assert_eq!(got, reply(expected));
    }
    // The hook fires only on the numbered call (docs/testing/fakes.md, "Fakes").
    assert_eq!(fired.lock().unwrap().len(), 1);
}

#[test]
fn an_after_reply_hook_observes_script_exhausted_and_the_call_still_fails() {
    use std::sync::{Arc, Mutex};
    let seen = Arc::new(Mutex::new(Vec::new()));
    let moved = Arc::clone(&seen);
    let provider = ScriptedProvider::new([Scripted::text("A")]);
    provider.on_after_reply(move |end: &Result<contract::provider::Reply, CallError>| {
        let code = match end {
            Ok(_) => "ok".to_string(),
            Err(CallError::Failed { failure, .. }) => format!("{:?}", failure.code),
            other => panic!("{other:?}"),
        };
        moved.lock().unwrap().push(code);
    });
    let mut deltas = Vec::new();
    assert!(
        provider
            .call(&request("r0"))
            .run(&mut |d| deltas.push(d))
            .is_ok()
    );
    let end = provider.call(&request("r1")).run(&mut |_| {});
    let Err(CallError::Failed { failure, .. }) = &end else {
        panic!("{end:?}");
    };
    assert_eq!(failure.code, ErrorCode::Other("script_exhausted".into()));
    assert!(end.is_err());
    // The hook sees each end in order, exhausted script included
    // (docs/testing/fakes.md, "Fakes").
    assert_eq!(
        *seen.lock().unwrap(),
        [
            "ok".to_string(),
            format!("{:?}", ErrorCode::Other("script_exhausted".into())),
        ]
    );
}

#[test]
fn an_after_reply_hook_passes_a_cancelled_end_through_unchanged() {
    use super::call_usage;
    use std::sync::{Arc, Mutex};
    let usage = call_usage("gen_kept");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let moved = Arc::clone(&seen);
    let provider = ScriptedProvider::new([Scripted::cancelled_after(usage.clone())]);
    provider.on_after_reply(move |end: &Result<contract::provider::Reply, CallError>| {
        moved.lock().unwrap().push(format!("{end:?}"));
    });
    let end = provider.call(&request("r")).run(&mut |_| {});
    assert_eq!(
        end,
        Err(CallError::Cancelled {
            usage: Box::new(usage)
        })
    );
    // The hook sees the cancelled end once, passing it through unchanged
    // (docs/testing/fakes.md, "Fakes").
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert!(seen.lock().unwrap()[0].contains("Cancelled"));
}

#[test]
fn hook_debug_names_the_hook_and_its_numbered_call() {
    use super::{AfterHook, BeforeFire, BeforeHook};
    use std::sync::Arc;
    let fire: Arc<BeforeFire> = Arc::new(|_: &ModelRequest| {});
    assert_eq!(
        format!(
            "{:?}",
            BeforeHook {
                at: Some(2),
                fire: Arc::clone(&fire),
            }
        ),
        "Hook { at: Some(2) }"
    );
    assert_eq!(
        format!("{:?}", BeforeHook { at: None, fire }),
        "Hook { at: None }"
    );
    let end: Arc<super::AfterFire> =
        Arc::new(|_: &Result<contract::provider::Reply, CallError>| {});
    assert_eq!(
        format!(
            "{:?}",
            AfterHook {
                at: Some(2),
                fire: Arc::clone(&end),
            }
        ),
        "Hook { at: Some(2) }"
    );
    assert_eq!(
        format!(
            "{:?}",
            AfterHook {
                at: None,
                fire: end
            }
        ),
        "Hook { at: None }"
    );
}

#[test]
fn both_fake_providers_wire_tools_the_same_way() {
    let tools = definitions();
    let want = super::wire_tools(&tools);
    assert_eq!(ScriptedProvider::new([]).wire_tools(&tools), want);
    assert_eq!(crate::BlockingProvider::default().wire_tools(&tools), want);
}
