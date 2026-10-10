//! The server's prompt list through the fixture and a fake clock: every
//! wait carries a named deadline, and the clock advances only after the
//! log proves the handshake is paging on it.

use serde_json::{Value, json};

use crate::test_support::Setup;

#[test]
fn runnable_names_hold_no_whitespace() {
    use crate::server_json::ListedPrompt;
    let runnable = |name: &str| {
        serde_json::from_value::<ListedPrompt>(json!({"name": name}))
            .expect("a prompt reads")
            .runnable()
    };
    assert!(!runnable(""));
    assert!(!runnable("a b"));
    assert!(!runnable("a\tb"));
    assert!(runnable("greet"));
    assert!(runnable("a/b"));
}

#[test]
fn the_hint_marks_required_arguments() {
    use super::hint;
    use crate::server_json::Argument;
    assert_eq!(hint(&[]), None);
    assert_eq!(
        hint(&[
            Argument {
                name: "who".to_owned(),
                required: true,
                rest: Default::default(),
            },
            Argument {
                name: "tone".to_owned(),
                required: false,
                rest: Default::default(),
            },
        ]),
        Some("<who> [tone]".to_owned())
    );
}

#[test]
fn fill_gives_one_word_per_argument_with_the_rest_to_the_last() {
    use super::fill;
    use crate::server_json::Argument;
    let args = || {
        vec![
            Argument {
                name: "who".to_owned(),
                required: true,
                rest: Default::default(),
            },
            Argument {
                name: "tone".to_owned(),
                required: false,
                rest: Default::default(),
            },
        ]
    };
    let filled = fill(&args(), "Ada warm and kind").expect("filled");
    assert_eq!(
        filled.named,
        serde_json::Map::from_iter([
            ("who".to_owned(), json!("Ada")),
            ("tone".to_owned(), json!("warm and kind")),
        ])
    );
    assert_eq!(filled.appended, None);
    // The last argument takes the rest trimmed at both ends; an empty
    // last is absent.
    let filled = fill(&args(), "Ada  ").expect("filled");
    assert_eq!(
        filled.named,
        serde_json::Map::from_iter([("who".to_owned(), json!("Ada"))])
    );
    // Missing required names list in order.
    let missing = fill(&args(), "  \t ").expect_err("missing");
    assert_eq!(missing, ["who"]);
    let missing = fill(
        &[
            Argument {
                name: "a".to_owned(),
                required: true,
                rest: Default::default(),
            },
            Argument {
                name: "b".to_owned(),
                required: true,
                rest: Default::default(),
            },
        ],
        "",
    )
    .expect_err("missing");
    assert_eq!(missing, ["a", "b"]);
}

#[test]
fn fill_with_no_arguments_appends_the_text() {
    use super::fill;
    let filled = fill(&[], "extra words").expect("filled");
    assert!(filled.named.is_empty());
    assert_eq!(filled.appended, Some("extra words".to_owned()));
    let filled = fill(&[], "   ").expect("filled");
    assert_eq!(filled.appended, None);
    let filled = fill(&[], "").expect("filled");
    assert_eq!(filled.appended, None);
}

fn read_text(result: Value) -> Result<String, String> {
    use super::text;
    use crate::server_json::{PromptResult, from_object};
    match from_object::<PromptResult>(result) {
        Err(_) => Err("no messages".to_owned()),
        Ok(result) => text(&result),
    }
}

#[test]
fn text_joins_every_message_in_order() {
    assert_eq!(
        read_text(json!({"messages": [
            {"role": "user", "content": {"type": "text", "text": "First."}},
            {"role": "assistant", "content": {"type": "text", "text": "Second."}},
        ]})),
        Ok("First.\n\nSecond.".to_owned())
    );
    assert_eq!(
        read_text(json!({"messages": [
            {"role": "user", "content": {"type": "resource", "resource": {"text": "From a file."}}},
        ]})),
        Ok("From a file.".to_owned())
    );
    for (result, kind) in [
        (
            json!({"messages": [{"role": "user", "content": {"type": "image", "data": "aGk="}}]}),
            "an image",
        ),
        (
            json!({"messages": [{"role": "user", "content": {"type": "audio", "data": "aGk="}}]}),
            "audio",
        ),
        (
            json!({"messages": [{"role": "user", "content": {"type": "resource", "resource": {"blob": "aGk="}}}]}),
            "a binary resource",
        ),
        (
            json!({"messages": [{"role": "user", "content": {"type": "resource_link", "uri": "file:///x"}}]}),
            "a resource link",
        ),
        (json!({}), "no messages"),
        (json!({"messages": []}), "no text"),
        (
            json!({"messages": [{"role": "user", "content": {"type": "text", "text": ""}}, {"role": "user"}]}),
            "no text",
        ),
    ] {
        assert_eq!(read_text(result), Err(kind.to_owned()), "kind: {kind}");
    }
}

#[test]
fn an_empty_text_part_adds_nothing_to_the_joined_text() {
    assert_eq!(
        read_text(json!({"messages": [
            {"role": "user", "content": {"type": "text", "text": ""}},
            {"role": "user", "content": {"type": "text", "text": "Only this."}},
        ]})),
        Ok("Only this.".to_owned())
    );
}

#[test]
fn embedded_resource_text_and_blob_guard_are_distinct() {
    assert_eq!(
        read_text(
            json!({"messages": [{"content": {"type": "resource", "resource": {"text": "Available text"}}}]})
        ),
        Ok("Available text".to_owned()),
    );
    assert_eq!(
        read_text(
            json!({"messages": [{"content": {"type": "resource", "resource": {"blob": "aGk="}}}]})
        ),
        Err("a binary resource".to_owned()),
    );
    assert_eq!(
        read_text(json!({"messages": [{"content": {"type": "resource", "resource": {}}}]})),
        Err("an unreadable resource".to_owned()),
    );
}

fn greet_entry() -> Value {
    json!({
        "name": "greet",
        "description": "Greets someone.",
        "arguments": [{"name": "who", "required": true}, {"name": "tone"}],
    })
}

fn greet_prompt() -> crate::server_json::ListedPrompt {
    serde_json::from_value(greet_entry()).expect("a prompt reads")
}

fn entry_with(
    server: &str,
    prompt: crate::server_json::ListedPrompt,
    slot: std::sync::Weak<crate::slot::Slot>,
    timeout: std::time::Duration,
) -> crate::prompt::PromptSource {
    crate::prompt::PromptSource {
        server: server.to_owned(),
        prompt,
        slot,
        timeout,
    }
}

fn prompts_with(entry: crate::prompt::PromptSource) -> crate::prompt::Prompts {
    crate::prompt::Prompts::collect(vec![entry])
}

#[test]
#[allow(
    clippy::type_complexity,
    reason = "one table pins every prompt outcome"
)]
fn each_outcome_maps_to_its_sentence() {
    use crate::slot::Fault;
    use contract::ErrorCode;
    use contract::shapes::ContentPart;
    use contract::tool::ServerRecord;
    let timeout = std::time::Duration::from_secs(60);
    let entry = entry_with("fx", greet_prompt(), std::sync::Weak::new(), timeout);
    let ok: Value =
        json!({"messages": [{"role": "user", "content": {"type": "text", "text": "Hi."}}]});
    let ok_appended: Value =
        json!({"messages": [{"role": "user", "content": {"type": "text", "text": "Body."}}]});
    let no_text: Value = json!({"messages": []});
    let refused = Fault::Refused("Unknown prompt.".to_owned());
    let died = Fault::Died(contract::events::McpServerFailed {
        server: "fx".to_owned(),
        reason: contract::events::ServerFailure::Died,
        will_restart: true,
        error: crate::fail::failure(
            ErrorCode::McpServerUnavailable,
            "The MCP server `fx` exited; Fiber restarts it on the next call.".to_owned(),
        ),
    });
    let rows: Vec<(
        &str,
        Option<String>,
        Result<Value, Fault>,
        Option<ErrorCode>,
        &str,
        usize,
    )> = vec![
        ("ok", None, Ok(ok), None, "", 0),
        (
            "ok with appended",
            Some("extra".to_owned()),
            Ok(ok_appended),
            None,
            "",
            0,
        ),
        (
            "ok with no text",
            None,
            Ok(no_text),
            Some(ErrorCode::McpPromptFailed),
            "returned no text, which Fiber cannot send as a message.",
            0,
        ),
        (
            "timeout",
            None,
            Err(Fault::Timeout),
            Some(ErrorCode::McpPromptFailed),
            "did not answer",
            0,
        ),
        (
            "cancelled",
            None,
            Err(Fault::Cancelled),
            Some(ErrorCode::McpPromptFailed),
            "was cancelled",
            0,
        ),
        (
            "refused",
            None,
            Err(refused),
            Some(ErrorCode::McpPromptFailed),
            "refused",
            0,
        ),
        (
            "died",
            None,
            Err(died),
            Some(ErrorCode::McpPromptFailed),
            "was not run",
            1,
        ),
        (
            "gone",
            None,
            Err(Fault::Gone),
            Some(ErrorCode::McpPromptFailed),
            "was not run",
            0,
        ),
    ];
    for (name, appended, called, code, fragment, records) in rows {
        let out = super::output(&entry, appended.clone(), called, vec![]);
        match code {
            None => {
                assert!(out.error.is_none(), "row: {name}");
                let expected = if appended.is_some() {
                    "Body.\n\nextra"
                } else {
                    "Hi."
                };
                assert_eq!(
                    out.content,
                    vec![ContentPart::Text {
                        text: expected.to_owned()
                    }],
                    "row: {name}"
                );
            }
            Some(code) => {
                let failure = out.error.expect("failed");
                assert_eq!(failure.code, code, "row: {name}");
                assert!(
                    failure.message.contains(fragment),
                    "row {name}: {}",
                    failure.message
                );
                assert_eq!(out.servers.len(), records, "row: {name}");
                if name == "died" {
                    match &out.servers[0] {
                        ServerRecord::Failed(_) => {}
                        ServerRecord::Ready(_) => panic!("one death record, got ready"),
                    }
                }
            }
        }
    }
}

#[test]
fn a_dead_link_is_not_run() {
    use contract::ErrorCode;
    let entry = entry_with(
        "fx",
        greet_prompt(),
        std::sync::Weak::new(),
        std::time::Duration::from_secs(30),
    );
    let prompts = prompts_with(entry);
    let out = prompts.get("fx", "greet", "Ada", &fakes::CancelToken::new());
    let failure = out.error.expect("failed");
    assert_eq!(failure.code, ErrorCode::McpPromptFailed);
    assert!(
        failure.message.contains("was not run"),
        "{}",
        failure.message
    );
}

#[test]
fn a_dead_slot_is_not_run_without_spawning() {
    use contract::ErrorCode;
    let setup = Setup::new();
    let mut spec = setup.spec("fx");
    spec.command = "/no/such/command".to_owned();
    spec.args = Vec::new();
    let slot = crate::slot::Slot::lazy(
        spec,
        setup.dir.path(),
        &setup.cache(),
        &setup.clock(),
        "0.0.0",
        crate::cache::Cached::default(),
    );
    slot.stop();
    let entry = entry_with(
        "fx",
        greet_prompt(),
        std::sync::Arc::downgrade(&slot),
        std::time::Duration::from_secs(30),
    );
    let prompts = prompts_with(entry);
    let out = prompts.get("fx", "greet", "Ada", &fakes::CancelToken::new());
    let failure = out.error.expect("failed");
    assert_eq!(failure.code, ErrorCode::McpPromptFailed);
    assert!(!setup.spawned(), "a dead slot never spawns");
}

#[test]
fn a_prompt_no_server_lists_is_not_run() {
    use contract::ErrorCode;
    let entry = entry_with(
        "fx",
        greet_prompt(),
        std::sync::Weak::new(),
        std::time::Duration::from_secs(30),
    );
    let prompts = prompts_with(entry);
    let out = prompts.get("fx", "missing", "", &fakes::CancelToken::new());
    let failure = out.error.expect("failed");
    assert_eq!(failure.code, ErrorCode::McpPromptFailed);
    assert_eq!(
        failure.message,
        "The MCP server `fx` has no prompt `/missing`."
    );
}

#[test]
fn a_missing_required_argument_is_invalid() {
    use contract::ErrorCode;
    let entry = entry_with(
        "fx",
        greet_prompt(),
        std::sync::Weak::new(),
        std::time::Duration::from_secs(30),
    );
    let prompts = prompts_with(entry);
    let out = prompts.get("fx", "greet", "", &fakes::CancelToken::new());
    let failure = out.error.expect("failed");
    assert_eq!(failure.code, ErrorCode::InvalidArguments);
    assert_eq!(
        failure.message,
        "The MCP server `fx`'s prompt `/greet` needs <who>. Run it as `/greet <who> [tone]`."
    );
}

#[test]
fn a_failed_start_is_not_run_with_its_record() {
    use contract::ErrorCode;
    let setup = Setup::new();
    let mut spec = setup.spec("fx");
    spec.command = "/no/such/command".to_owned();
    spec.args = Vec::new();
    let slot = crate::slot::Slot::lazy(
        spec,
        setup.dir.path(),
        &setup.cache(),
        &setup.clock(),
        "0.0.0",
        crate::cache::Cached {
            tools: vec![],
            prompts: vec![greet_prompt()],
        },
    );
    let entry = entry_with(
        "fx",
        greet_prompt(),
        std::sync::Arc::downgrade(&slot),
        std::time::Duration::from_secs(30),
    );
    let prompts = prompts_with(entry);
    let out = prompts.get("fx", "greet", "Ada", &fakes::CancelToken::new());
    let failure = out.error.expect("failed");
    assert_eq!(failure.code, ErrorCode::McpPromptFailed);
    assert!(
        failure.message.contains("was not run"),
        "{}",
        failure.message
    );
    assert_eq!(out.servers.len(), 1);
    assert!(!setup.spawned(), "a failed spawn never leaves a child");
}
