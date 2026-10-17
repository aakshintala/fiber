//! `host.model` (`docs/extensions.md`, "Host calls"): one model request
//! through the session's routing and credentials. It suspends the callback
//! like `host.http`: the worker runs the call and resumes the coroutine with
//! its reply. A wrong argument is an error in the calling code and raises a
//! string, which `pcall` catches. Refused in the entry script before it
//! yields, like `host.exec`.

use std::cell::Cell;
use std::rc::Rc;

use contract::model::{HostMessage, HostModelRequest, HostRole};
use contract::shapes::ContentPart;
use mlua::{Lua, Table, Value as LuaValue};

/// `host.model` yields this tag, `"model"` and the call's spec. The
/// extension's thread runs the model call off to the side and resumes the
/// coroutine with the reply (`docs/extensions.md`, "A host call suspends the
/// code that made it"). Refused in the entry script before it yields, like
/// `host.exec`.
const MODEL: &str = r#"
local host, tag, in_entry, failure = ...
function host.model(opts)
  if in_entry() then
    error(failure("closing", "host.model: not available while init.lua runs"), 0)
  end
  if type(opts) ~= "table" then
    error("host.model: `opts` must be a table", 2)
  end
  local model = opts.model
  if type(model) ~= "string" or model:find("/", 1, true) == nil then
    error("host.model: `model` must be a model reference, not a role", 2)
  end
  local system = opts.system
  if system ~= nil and type(system) ~= "string" then
    error("host.model: `system` must be a string", 2)
  end
  local messages = opts.messages
  if type(messages) ~= "table" or #messages == 0 then
    error("host.model: `messages` must be a non-empty list", 2)
  end
  for i = 1, #messages do
    local message = messages[i]
    if type(message) ~= "table" then
      error("host.model: `messages` must hold `{ role, content }` tables", 2)
    end
    local role = message.role
    if role ~= "user" and role ~= "assistant" then
      error("host.model: a message `role` is `user` or `assistant`", 2)
    end
    local content = message.content
    if type(content) == "string" then
    elseif type(content) == "table" then
      for j = 1, #content do
        local part = content[j]
        if type(part) ~= "table" then
          error("host.model: a content part is a table", 2)
        end
        local kind = part.type
        if kind ~= "text" and kind ~= "image" and kind ~= "pdf" then
          error("host.model: a content part `type` is `text`, `image` or `pdf`", 2)
        end
        if kind == "text" and type(part.text) ~= "string" then
          error("host.model: a text part holds `text`, a string", 2)
        end
      end
    else
      error("host.model: a message `content` is a string or a list of parts", 2)
    end
  end
  local max_tokens = opts.max_tokens
  if type(max_tokens) ~= "number" or math.type(max_tokens) ~= "integer" or max_tokens <= 0 then
    error("host.model: `max_tokens` must be a positive integer", 2)
  end
  local result, code, message = coroutine.yield(tag, "model", { model = model, system = system, messages = messages, max_tokens = max_tokens })
  if result == nil then error(failure(code, message), 0) end
  return result
end
"#;

/// Sets `host.model` on `host`. `entry` is true while the entry script runs.
pub(crate) fn install(
    lua: &Lua,
    host: &Table,
    tag: &Table,
    entry: Rc<Cell<bool>>,
    failure: mlua::Function,
) -> mlua::Result<()> {
    let in_entry = super::failure::in_entry(lua, &entry)?;
    lua.load(MODEL).set_name("=host.model").call::<()>((
        host.clone(),
        tag.clone(),
        in_entry,
        failure.clone(),
    ))
}

/// Reads a `host.model` yield: the model, system, messages and token limit.
/// It cannot fail after the Lua half's checks passed; an `Err` is an error
/// in the calling code, raised as the string. It builds the contract's
/// request directly, so the admit arm passes it to `HostModel::call`
/// without a second mapping.
pub(crate) fn request(spec: &Table) -> mlua::Result<HostModelRequest> {
    let fail = |message: &str| mlua::Error::RuntimeError(message.to_owned());
    let model: String = spec
        .get("model")
        .map_err(|_| fail("host.model: `model` must be a model reference, not a role"))?;
    if model.find('/').is_none() {
        return Err(fail(
            "host.model: `model` must be a model reference, not a role",
        ));
    }
    let system: Option<String> = spec
        .get("system")
        .map_err(|_| fail("host.model: `system` must be a string"))?;
    let messages: Table = spec
        .get("messages")
        .map_err(|_| fail("host.model: `messages` must be a non-empty list"))?;
    let len = messages.len().unwrap_or(0);
    if len == 0 {
        return Err(fail("host.model: `messages` must be a non-empty list"));
    }
    let mut read = Vec::new();
    for i in 1..=len {
        let message: Table = messages
            .get(i)
            .map_err(|_| fail("host.model: `messages` must hold `{ role, content }` tables"))?;
        read.push(message_from(&message)?);
    }
    let max_tokens: i64 = spec
        .get("max_tokens")
        .map_err(|_| fail("host.model: `max_tokens` must be a positive integer"))?;
    let max_tokens = u64::try_from(max_tokens)
        .map_err(|_| fail("host.model: `max_tokens` must be a positive integer"))?;
    if max_tokens == 0 {
        return Err(fail("host.model: `max_tokens` must be a positive integer"));
    }
    Ok(HostModelRequest {
        model,
        system,
        messages: read,
        max_tokens,
    })
}

/// Reads one `{ role, content }` message table.
fn message_from(message: &Table) -> mlua::Result<HostMessage> {
    let fail = |message: &str| mlua::Error::RuntimeError(message.to_owned());
    let role: String = message
        .get("role")
        .map_err(|_| fail("host.model: a message `role` is `user` or `assistant`"))?;
    let role = match role.as_str() {
        "user" => HostRole::User,
        "assistant" => HostRole::Assistant,
        _ => {
            return Err(fail(
                "host.model: a message `role` is `user` or `assistant`",
            ));
        }
    };
    let content: LuaValue = message
        .get("content")
        .map_err(|_| fail("host.model: a message `content` is a string or a list of parts"))?;
    let content = match content {
        LuaValue::String(text) => vec![ContentPart::Text {
            text: text.to_str()?.to_owned(),
        }],
        LuaValue::Table(parts) => {
            let mut read = Vec::new();
            for j in 1..=parts.len().unwrap_or(0) {
                let part: LuaValue = parts.get(j)?;
                read.push(part_from(&part)?);
            }
            read
        }
        LuaValue::Nil
        | LuaValue::Boolean(_)
        | LuaValue::LightUserData(_)
        | LuaValue::Integer(_)
        | LuaValue::Number(_)
        | LuaValue::Function(_)
        | LuaValue::Thread(_)
        | LuaValue::UserData(_)
        | LuaValue::Error(_)
        | LuaValue::Other(_) => {
            return Err(fail(
                "host.model: a message `content` is a string or a list of parts",
            ));
        }
    };
    Ok(HostMessage { role, content })
}

/// Reads one content part table, as `docs/events.md` names it. An unknown
/// part reads as an error in the calling code, never as an `Unknown` part
/// the model would then echo as a placeholder.
fn part_from(part: &LuaValue) -> mlua::Result<ContentPart> {
    let fail = |message: String| mlua::Error::RuntimeError(message);
    let json = super::to_json(part)
        .map_err(|_| fail("host.model: a content part is a table".to_owned()))?;
    let read: ContentPart = serde_json::from_value(json)
        .map_err(|e| fail(format!("host.model: a content part does not read: {e}")))?;
    if matches!(read, ContentPart::Unknown) {
        return Err(fail(
            "host.model: a content part `type` is `text`, `image` or `pdf`".to_owned(),
        ));
    }
    Ok(read)
}

/// The successful reply as Lua sees it: `{ text, usage = { tokens, cost } }`.
/// A cost the provider did not know reads as `nil` (`docs/events.md`,
/// `usage_recorded`: `cost` "null when neither exists").
pub(crate) fn reply(
    lua: &Lua,
    text: &str,
    tokens: &contract::shapes::Tokens,
    cost: Option<f64>,
) -> mlua::Result<Table> {
    let seen = lua.create_table()?;
    seen.set("input", tokens.input)?;
    seen.set("cache_read", tokens.cache_read)?;
    let written = lua.create_table()?;
    for (lifetime, count) in &tokens.cache_write {
        written.raw_set(lifetime.as_str(), *count)?;
    }
    seen.set("cache_write", written)?;
    seen.set("output", tokens.output)?;
    let usage = lua.create_table()?;
    usage.set("tokens", seen)?;
    match cost {
        Some(cost) => usage.set("cost", cost)?,
        None => usage.set("cost", LuaValue::Nil)?,
    }
    let result = lua.create_table()?;
    result.set("text", text)?;
    result.set("usage", usage)?;
    Ok(result)
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;
