use contract::events::TextDelta;
use contract::provider::{CallError, Delta, ModelRequest, Provider};
use contract::{ErrorCode, events::CacheLifetime};

use super::{Scripted, ScriptedProvider, reply};

fn request(text: &str) -> ModelRequest {
    ModelRequest {
        system_prompt: text.into(),
        tools: Vec::new(),
        effort: None,
        tool_choice: "auto".into(),
        cache_lifetime: CacheLifetime::OneHour,
        cache_key: "s_1".into(),
        conversation: Vec::new(),
        previous_end: None,
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
    assert_eq!(call.run(&mut |d| deltas.push(d)), Err(CallError::Cancelled));
    assert!(deltas.is_empty());
}

#[test]
fn a_call_runs_once() {
    let provider = ScriptedProvider::new([Scripted::text("Hello")]);
    let call = provider.call(&request("r"));
    assert!(call.run(&mut |_| {}).is_ok());
    assert_eq!(call.run(&mut |_| {}), Err(CallError::Cancelled));
}
