//! `host.model` (`docs/extensions.md`, "Host calls"): the Lua half's checks,
//! the entry-script refusal, the yielded spec and the reply shape.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::path::Path;
use std::sync::Arc;

use contract::shapes::{ContentPart, Tokens};
use fakes::Deadline;
use fakes::clock::FakeClock;
use mlua::{Lua, Value as LuaValue};

use crate::{Error, LuaExtension};

/// `clock` as the extension's clock.
fn clocked(clock: &Arc<FakeClock>) -> Arc<dyn contract::clock::Clock> {
    Arc::clone(clock) as _
}

/// Runs `body` as a `fiber.command` and returns what it returned.
fn run(body: &str) -> String {
    let clock = FakeClock::new();
    let dir = fakes::TempDir::new("fiber-model");
    std::fs::write(
        dir.path().join("init.lua"),
        format!("fiber.command(\"go\", {{ timeout = 5000, run = function()\n{body}\nend }})\n"),
    )
    .unwrap();
    let ext = Arc::new(LuaExtension::new(
        "ext",
        dir.path(),
        "/nonexistent-fiber-home",
        clocked(&clock),
    ));
    let held = Arc::clone(&ext);
    fakes::within(
        "held.command",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || held.command("go", ""),
    )
    .unwrap()
}

/// The `pcall` verdict for `call`: `ok` with the returned text, or the
/// error's type and message.
fn probed(call: &str) -> String {
    run(&format!(
        "local ok, err = pcall({call})\n\
         if ok then return \"ok/\" .. tostring(err) end\n\
         return type(err) .. \"/\" .. tostring(err)"
    ))
}

#[test]
fn model_rejects_a_non_table() {
    assert_eq!(
        probed("host.model"),
        "string/host.model: `opts` must be a table"
    );
    assert_eq!(
        probed("host.model, 42"),
        "string/host.model: `opts` must be a table"
    );
}

#[test]
fn model_rejects_a_role_shaped_model() {
    assert_eq!(
        probed(
            "host.model, { model = \"fast\", messages = { { role = \"user\", content = \"hi\" } }, max_tokens = 16 }"
        ),
        "string/host.model: `model` must be a model reference, not a role"
    );
}

#[test]
fn model_rejects_a_missing_model() {
    assert_eq!(
        probed(
            "host.model, { messages = { { role = \"user\", content = \"hi\" } }, max_tokens = 16 }"
        ),
        "string/host.model: `model` must be a model reference, not a role"
    );
}

#[test]
fn model_rejects_a_non_string_system() {
    assert_eq!(
        probed(
            "host.model, { model = \"p/m\", system = 42, messages = { { role = \"user\", content = \"hi\" } }, max_tokens = 16 }"
        ),
        "string/host.model: `system` must be a string"
    );
}

#[test]
fn model_rejects_empty_messages() {
    assert_eq!(
        probed("host.model, { model = \"p/m\", messages = {}, max_tokens = 16 }"),
        "string/host.model: `messages` must be a non-empty list"
    );
    assert_eq!(
        probed("host.model, { model = \"p/m\", max_tokens = 16 }"),
        "string/host.model: `messages` must be a non-empty list"
    );
}

#[test]
fn model_rejects_a_wrong_role() {
    assert_eq!(
        probed(
            "host.model, { model = \"p/m\", messages = { { role = \"system\", content = \"hi\" } }, max_tokens = 16 }"
        ),
        "string/host.model: a message `role` is `user` or `assistant`"
    );
}

#[test]
fn model_rejects_a_part_with_an_unknown_type() {
    assert_eq!(
        probed(
            "host.model, { model = \"p/m\", messages = { { role = \"user\", content = { { type = \"video\", url = \"x\" } } } }, max_tokens = 16 }"
        ),
        "string/host.model: a content part `type` is `text`, `image` or `pdf`"
    );
}

#[test]
fn model_rejects_a_missing_or_non_positive_max_tokens() {
    assert_eq!(
        probed(
            "host.model, { model = \"p/m\", messages = { { role = \"user\", content = \"hi\" } }, max_tokens = 0 }"
        ),
        "string/host.model: `max_tokens` must be a positive integer"
    );
    assert_eq!(
        probed(
            "host.model, { model = \"p/m\", messages = { { role = \"user\", content = \"hi\" } }, max_tokens = -1 }"
        ),
        "string/host.model: `max_tokens` must be a positive integer"
    );
    assert_eq!(
        probed(
            "host.model, { model = \"p/m\", messages = { { role = \"user\", content = \"hi\" } } }"
        ),
        "string/host.model: `max_tokens` must be a positive integer"
    );
}

#[test]
fn model_accepts_a_limit_of_one() {
    // A limit of 1 passes the checks, so the call yields: with no model
    // wired yet the stub answers `closing`, raised as the failure table.
    let verdict = run(
        "local ok, err = pcall(host.model, { model = \"p/m\", system = \"terse\",\n\
         messages = { { role = \"user\", content = \"hi\" }, { role = \"assistant\", content = { { type = \"text\", text = \"yo\" } } } },\n\
         max_tokens = 1 })\n\
         if ok then return \"yielded\" end\n\
         return type(err) .. \"/\" .. tostring(err.code)",
    );
    assert_eq!(verdict, "table/closing");
}

#[test]
fn model_in_the_entry_script_raises_closing() {
    let clock = FakeClock::new();
    let dir = fakes::TempDir::new("fiber-model");
    std::fs::write(
        dir.path().join("init.lua"),
        "host.model({ model = \"p/m\", messages = { { role = \"user\", content = \"hi\" } }, max_tokens = 16 })\n\
         fiber.command(\"go\", { timeout = 5000, run = function() end })\n",
    )
    .unwrap();
    let ext = LuaExtension::new(
        "ext",
        dir.path(),
        "/nonexistent-fiber-home",
        clocked(&clock),
    );
    let ext = Arc::new(ext);
    let held = Arc::clone(&ext);
    match fakes::within(
        "held.commands",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || held.commands(),
    ) {
        Err(Error::Lua { message, .. }) => {
            assert_eq!(message, "host.model: not available while init.lua runs");
        }
        other => panic!("the entry script fails to load: {other:?}"),
    }
}

#[test]
fn request_from_reads_a_valid_spec() {
    let lua = Lua::new();
    let spec = lua.create_table().unwrap();
    spec.set("model", "p/m").unwrap();
    spec.set("system", "terse").unwrap();
    let messages = lua.create_table().unwrap();
    let first = lua.create_table().unwrap();
    first.set("role", "user").unwrap();
    first.set("content", "hi").unwrap();
    messages.raw_set(1, first).unwrap();
    let second = lua.create_table().unwrap();
    second.set("role", "assistant").unwrap();
    let parts = lua.create_table().unwrap();
    let part = lua.create_table().unwrap();
    part.set("type", "text").unwrap();
    part.set("text", "yo").unwrap();
    parts.raw_set(1, part).unwrap();
    second.set("content", parts).unwrap();
    messages.raw_set(2, second).unwrap();
    spec.set("messages", messages).unwrap();
    spec.set("max_tokens", 16).unwrap();
    match super::super::request_from(
        "model",
        Some(&LuaValue::Table(spec)),
        None,
        Path::new("/tmp"),
        1 << 20,
    )
    .unwrap()
    {
        Some(super::super::Request::Model(read)) => {
            assert_eq!(read.model, "p/m");
            assert_eq!(read.system.as_deref(), Some("terse"));
            assert_eq!(read.max_tokens, 16);
            assert_eq!(read.messages.len(), 2);
            assert!(matches!(
                read.messages[0].role,
                contract::model::HostRole::User
            ));
            assert_eq!(
                read.messages[0].content,
                vec![ContentPart::Text {
                    text: "hi".to_owned()
                }]
            );
            assert!(matches!(
                read.messages[1].role,
                contract::model::HostRole::Assistant
            ));
            assert_eq!(
                read.messages[1].content,
                vec![ContentPart::Text {
                    text: "yo".to_owned()
                }]
            );
        }
        _ => panic!("a valid spec reads as a model request"),
    }
}

fn tokens() -> Tokens {
    Tokens {
        input: 10,
        cache_read: 1,
        cache_write: [("5m".to_owned(), 2)].into_iter().collect(),
        output: 3,
    }
}

fn answered(lua: &Lua, cost: Option<f64>) -> LuaValue {
    let clock = FakeClock::new();
    let reply = super::super::Reply::Model(Ok(contract::model::HostModelReply {
        text: "ok".to_owned(),
        tokens: tokens(),
        cost,
    }));
    let mut values = super::super::resume_values(lua, &clocked(&clock), reply).unwrap();
    values.pop_front().unwrap()
}

#[test]
fn resume_values_reports_a_nil_cost_as_nil() {
    let lua = Lua::new();
    let LuaValue::Table(result) = answered(&lua, None) else {
        panic!("the answer is one table");
    };
    assert_eq!(result.get::<String>("text").unwrap(), "ok".to_owned());
    let usage: mlua::Table = result.get("usage").unwrap();
    assert_eq!(usage.get::<LuaValue>("cost").unwrap(), LuaValue::Nil);
    let seen: mlua::Table = usage.get("tokens").unwrap();
    assert_eq!(seen.get::<u64>("input").unwrap(), 10);
    assert_eq!(seen.get::<u64>("output").unwrap(), 3);
}

#[test]
fn resume_values_reports_a_known_cost() {
    let lua = Lua::new();
    let LuaValue::Table(result) = answered(&lua, Some(0.01)) else {
        panic!("the answer is one table");
    };
    let usage: mlua::Table = result.get("usage").unwrap();
    assert_eq!(usage.get::<f64>("cost").unwrap(), 0.01);
}

#[test]
fn resume_values_reports_a_failure_as_code_and_message() {
    let lua = Lua::new();
    let clock = FakeClock::new();
    let reply = super::super::Reply::Model(Err((
        contract::ErrorCode::BudgetExceeded,
        "host.model: the budget of 1.00 USD is reached".to_owned(),
    )));
    let values = super::super::resume_values(&lua, &clocked(&clock), reply).unwrap();
    let mut values = values.into_vec();
    assert_eq!(values.len(), 3);
    assert_eq!(values.remove(0), LuaValue::Nil);
    assert_eq!(
        values.remove(0),
        LuaValue::String(lua.create_string("budget_exceeded").unwrap())
    );
    assert_eq!(
        values.remove(0),
        LuaValue::String(
            lua.create_string("host.model: the budget of 1.00 USD is reached")
                .unwrap()
        )
    );
}
