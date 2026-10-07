use std::collections::BTreeMap;

use contract::GenerationId;
use contract::provider::{CallUsage, InputSize, Reply};
use contract::shapes::Tokens;

use super::{carried, named};
use crate::Error;

fn usage(generation: Option<&str>) -> CallUsage {
    CallUsage {
        generation_id: generation.map(|id| GenerationId(id.into())),
        tokens: Tokens {
            input: 7,
            cache_read: 1,
            cache_write: BTreeMap::from([("5m".to_owned(), 2)]),
            output: 3,
        },
        web_searches: Some(1),
        input_size: InputSize {
            bytes: 100,
            media: true,
        },
    }
}

fn reply(generation: Option<&str>) -> Reply {
    let usage = usage(generation);
    Reply {
        actions: Vec::new(),
        finish: contract::provider::Finish::Completed,
        generation_id: usage.generation_id.clone(),
        tokens: usage.tokens.clone(),
        web_searches: usage.web_searches,
        cost: Some(0.5),
        input_size: usage.input_size,
    }
}

fn sized(bytes: u64) -> InputSize {
    InputSize {
        bytes,
        media: false,
    }
}

#[allow(
    clippy::result_large_err,
    reason = "the decode's own result shape, which carries its partial beside the error"
)]
fn failed(partial: Option<CallUsage>) -> Result<Reply, (Error, Option<CallUsage>)> {
    Err((Error::StreamIncomplete("ended".into()), partial))
}

/// `usage(generation)` with the call's input size.
fn sent(generation: Option<&str>) -> Box<CallUsage> {
    let mut want = usage(generation);
    want.input_size = sized(900);
    Box::new(want)
}

#[test]
fn each_way_a_call_ends_carries_what_it_saw_with_the_call_s_input_size() {
    let mut named_reply = reply(Some("gen_1"));
    named_reply.input_size = sized(1);
    let mut named_partial = usage(Some("gen_1"));
    named_partial.input_size = sized(1);
    let cases = [
        (
            "a named partial",
            failed(Some(named_partial)),
            sent(Some("gen_1")),
        ),
        ("an unnamed partial", failed(Some(usage(None))), sent(None)),
        (
            "no stream read",
            failed(None),
            Box::new(CallUsage::unnamed(sized(900))),
        ),
        ("a named reply", Ok(named_reply), sent(Some("gen_1"))),
        ("an unnamed reply", Ok(reply(None)), sent(None)),
    ];
    for (case, decoded, want) in cases {
        assert_eq!(carried(&decoded, sized(900)), want, "{case}");
    }
}

#[test]
fn only_a_non_empty_id_names_a_generation() {
    assert_eq!(named(String::new()), None);
    assert_eq!(
        named("gen_1".to_owned()),
        Some(GenerationId("gen_1".into()))
    );
}
