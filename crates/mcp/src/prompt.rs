//! One MCP server's prompts as prompt templates (`docs/mcp.md`, "Prompts
//! and resources"): what `prompts/list` advertised, the rows the session
//! lists beside skills, and the text `prompts/get` answers. A row is the
//! server's name as its tag, so a person runs one by typing its `/name`
//! with arguments.

use contract::events::CommandInfo;
use serde_json::Value;

/// One prompt a server lists: its name, description and arguments, as
/// [`Prompts::commands`] rows them.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ListedPrompt {
    /// The server's own name for it.
    pub name: String,
    /// Its description; `""` when the server gave none.
    pub description: String,
    /// Its arguments, in the order the server lists them.
    pub arguments: Vec<Argument>,
}

/// One argument a prompt takes.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Argument {
    /// The server's own name for it.
    pub name: String,
    /// Whether running the prompt without it is rejected.
    pub required: bool,
}

impl ListedPrompt {
    /// Reads one `prompts/list` entry. A missing description takes `""`,
    /// and an argument without a name is dropped: nothing could fill it.
    pub(crate) fn read(entry: &Value) -> Self {
        let name = entry
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let description = entry
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let arguments = entry
            .get("arguments")
            .and_then(Value::as_array)
            .map(|listed| {
                listed
                    .iter()
                    .filter_map(|argument| {
                        let name = argument.get("name").and_then(Value::as_str)?;
                        if name.is_empty() {
                            return None;
                        }
                        Some(Argument {
                            name: name.to_owned(),
                            required: argument
                                .get("required")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            name,
            description,
            arguments,
        }
    }

    /// Whether `/name` can run it: a prompt whose name is empty or holds
    /// whitespace is left out, because `split_command` could never name it
    /// (`docs/mcp.md`, "Prompts and resources").
    pub(crate) fn runnable(&self) -> bool {
        !self.name.is_empty() && !self.name.chars().any(|char| char.is_whitespace())
    }
}

/// The `commands` row's `argument_hint`: each argument as `<name>` when
/// required and `[name]` when not, space-separated; absent when the prompt
/// takes none (`docs/invocation.md`, "What each command does").
pub(crate) fn hint(arguments: &[Argument]) -> Option<String> {
    if arguments.is_empty() {
        return None;
    }
    Some(
        arguments
            .iter()
            .map(|argument| {
                if argument.required {
                    format!("<{}>", argument.name)
                } else {
                    format!("[{}]", argument.name)
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// One prompt row's source: whose list it came from.
#[derive(Debug, Clone)]
pub(crate) struct PromptSource {
    /// The server's configured name: the row's tag.
    pub server: String,
    /// The prompt, as the server listed it.
    pub prompt: ListedPrompt,
}

/// Every runnable prompt of every server that listed, tagged with its
/// server's name (`docs/mcp.md`, "Prompts and resources"). Entries sort by
/// server name, keeping each server's listed order: between servers, the
/// first in name order wins a shared name.
#[derive(Debug, Clone, Default)]
pub struct Prompts {
    entries: Vec<PromptSource>,
}

impl Prompts {
    /// Collects `sources` into one listing, sorted by server name. The sort
    /// is stable, so each server keeps its listed order.
    pub(crate) fn collect(mut sources: Vec<PromptSource>) -> Self {
        sources.sort_by(|left, right| left.server.cmp(&right.server));
        Self { entries: sources }
    }

    /// The `commands` answer's rows: runnable prompts only, tagged with the
    /// server's name, such as `{"name":"greet","description":"Greets
    /// someone.","argument_hint":"<who> [tone]","tag":"fx"}`.
    pub fn commands(&self) -> Vec<CommandInfo> {
        self.entries
            .iter()
            .filter(|entry| entry.prompt.runnable())
            .map(|entry| CommandInfo {
                name: entry.prompt.name.clone(),
                description: entry.prompt.description.clone(),
                argument_hint: hint(&entry.prompt.arguments),
                tag: entry.server.clone(),
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "prompt_tests.rs"]
mod tests;
