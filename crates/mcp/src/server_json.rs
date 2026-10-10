//! Typed reads of the server's JSON: tools, prompts, results and content.
//! Every field tolerates what the old hand readers accepted: a missing
//! value, a `null` or a value of the wrong type reads as the default.

use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value};

use crate::effects::Hints;

/// The default input schema, when the server gives none.
fn object_schema() -> Value {
    serde_json::json!({"type": "object"})
}

/// Reads any JSON value, giving the default when it does not deserialize
/// as `T`: missing, `null` or the wrong type all read as default, exactly
/// as the old hand readers did. It never errors on JSON input.
fn lenient<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    let value = Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

/// One tool the server lists.
#[derive(Debug, Clone, PartialEq, Deserialize, serde::Serialize)]
pub(crate) struct ListedTool {
    /// The server's own name for it.
    #[serde(default, deserialize_with = "lenient")]
    pub name: String,
    /// Its description; `""` when the server gave none.
    #[serde(default, deserialize_with = "lenient")]
    pub description: String,
    /// Its input schema; `{"type":"object"}` when the server gave none.
    /// A present value keeps its value, `null` included.
    #[serde(rename = "inputSchema", default = "object_schema")]
    pub schema: Value,
    /// Its hints; absent when the server gave none.
    #[serde(default, deserialize_with = "lenient")]
    pub annotations: Annotations,
    /// Unknown fields, kept so the cache rewrites on any change.
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

impl ListedTool {
    /// The hints this tool declares.
    pub(crate) fn hints(&self) -> Hints {
        Hints {
            read_only: self.annotations.read_only,
            destructive: self.annotations.destructive,
            open_world: self.annotations.open_world,
        }
    }
}

/// One tool's hints.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, serde::Serialize)]
pub(crate) struct Annotations {
    /// `readOnlyHint`.
    #[serde(
        rename = "readOnlyHint",
        default,
        deserialize_with = "lenient",
        skip_serializing_if = "Option::is_none"
    )]
    pub read_only: Option<bool>,
    /// `destructiveHint`.
    #[serde(
        rename = "destructiveHint",
        default,
        deserialize_with = "lenient",
        skip_serializing_if = "Option::is_none"
    )]
    pub destructive: Option<bool>,
    /// `openWorldHint`.
    #[serde(
        rename = "openWorldHint",
        default,
        deserialize_with = "lenient",
        skip_serializing_if = "Option::is_none"
    )]
    pub open_world: Option<bool>,
    /// Unknown fields, kept so the cache rewrites on any change.
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

/// One prompt the server lists.
#[derive(Debug, Clone, PartialEq, Deserialize, serde::Serialize)]
pub(crate) struct ListedPrompt {
    /// The server's own name for it.
    #[serde(default, deserialize_with = "lenient")]
    pub name: String,
    /// Its description; `""` when the server gave none.
    #[serde(default, deserialize_with = "lenient")]
    pub description: String,
    /// Its arguments, in the order the server lists them.
    #[serde(default, deserialize_with = "arguments")]
    pub arguments: Vec<Argument>,
    /// Unknown fields, kept so the cache rewrites on any change.
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

impl ListedPrompt {
    /// Whether `/name` can run it: a prompt whose name is empty or holds
    /// whitespace is left out, because `split_command` could never name it.
    pub(crate) fn runnable(&self) -> bool {
        !self.name.is_empty() && !self.name.chars().any(|char| char.is_whitespace())
    }
}

/// One argument a prompt takes.
#[derive(Debug, Clone, PartialEq, Deserialize, serde::Serialize)]
pub(crate) struct Argument {
    /// The server's own name for it.
    #[serde(default, deserialize_with = "lenient")]
    pub name: String,
    /// Whether running the prompt without it is rejected.
    #[serde(default, deserialize_with = "lenient")]
    pub required: bool,
    /// Unknown fields, kept so the cache rewrites on any change.
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

/// Reads the argument list: a non-array reads as empty, and each entry
/// that is not an object or has an empty name is dropped, as before.
fn arguments<'de, D>(deserializer: D) -> Result<Vec<Argument>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    let listed = match value.as_array() {
        Some(listed) => listed,
        None => return Ok(Vec::new()),
    };
    let mut kept = Vec::new();
    for entry in listed {
        let argument: Argument = match serde_json::from_value(entry.clone()) {
            Ok(argument) => argument,
            Err(_) => continue,
        };
        if argument.name.is_empty() {
            continue;
        }
        kept.push(argument);
    }
    Ok(kept)
}

/// A `tools/call` result.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct CallResult {
    /// Its content parts.
    pub content: Vec<Content>,
    /// Whether the server marked it as an error.
    pub is_error: bool,
}

impl<'de> Deserialize<'de> for CallResult {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let object = match value.as_object() {
            Some(object) => object,
            None => return Ok(CallResult::default()),
        };
        let content = match object.get("content") {
            Some(Value::Array(listed)) => listed
                .iter()
                .map(|part| {
                    serde_json::from_value(part.clone()).unwrap_or(Content::Unreadable)
                })
                .collect(),
            Some(_) | None => Vec::new(),
        };
        let is_error = match object.get("isError") {
            Some(value) => serde_json::from_value(value.clone()).unwrap_or_default(),
            None => false,
        };
        Ok(CallResult { content, is_error })
    }
}

/// A `prompts/get` result.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub(crate) struct PromptResult {
    /// Its messages.
    pub messages: Vec<Message>,
}

/// One message in a prompt result.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Message {
    /// Its content parts.
    pub content: Vec<Content>,
}

impl<'de> Deserialize<'de> for Message {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let object = match value.as_object() {
            Some(object) => object,
            None => {
                return Ok(Message {
                    content: Vec::new(),
                });
            }
        };
        let content = match object.get("content") {
            None => Vec::new(),
            Some(Value::Array(listed)) => listed
                .iter()
                .map(|part| {
                    serde_json::from_value(part.clone()).unwrap_or(Content::Unreadable)
                })
                .collect(),
            Some(single) => {
                vec![serde_json::from_value(single.clone()).unwrap_or(Content::Unreadable)]
            }
        };
        Ok(Message { content })
    }
}

/// One content part.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Content {
    /// Text.
    Text {
        /// Its text; a non-string reads as `None`.
        #[serde(default, deserialize_with = "lenient")]
        text: Option<String>,
    },
    /// An image.
    Image,
    /// Audio.
    Audio,
    /// An embedded resource.
    Resource {
        /// The resource; a missing, `null` or non-object resource reads
        /// as the default, giving "an unreadable resource".
        #[serde(default, deserialize_with = "lenient")]
        resource: Resource,
    },
    /// A resource link.
    ResourceLink,
    /// A part that failed to deserialize: no `type`, a non-string
    /// `type`, or a non-object. Never serialized.
    #[serde(skip)]
    Unreadable,
    /// Any other `type`.
    #[serde(other)]
    Other,
}

/// An embedded resource.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct Resource {
    /// Its text; a non-string reads as `None`.
    #[serde(default, deserialize_with = "lenient")]
    pub text: Option<String>,
    /// Its blob; a `null` counts as absent.
    pub blob: Option<Value>,
}

/// Drops the entries that do not deserialize, which are exactly the
/// non-objects, given lenient fields.
pub(crate) fn entries<T: serde::de::DeserializeOwned>(listed: Vec<Value>) -> Vec<T> {
    listed
        .into_iter()
        .filter_map(|entry| serde_json::from_value(entry).ok())
        .collect()
}

#[cfg(test)]
#[path = "server_json_tests.rs"]
mod tests;
