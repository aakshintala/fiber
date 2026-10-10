//! One MCP server's prompts as prompt templates (`docs/mcp.md`, "Prompts
//! and resources"): what `prompts/list` advertised, the rows the session
//! lists beside skills, and the text `prompts/get` answers. A row is the
//! server's name as its tag, so a person runs one by typing its `/name`
//! with arguments.

use std::sync::Weak;
use std::time::Duration;

use contract::ErrorCode;
use contract::events::CommandInfo;
use contract::shapes::{ContentPart, Failure};
use contract::tool::{Cancel, Output, ServerRecord};
use serde_json::{Map, Value};

use crate::server::CallError;
use crate::server_json::{Argument, Content, ListedPrompt, PromptResult};
use crate::slot::{self, Served, Slot};

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

/// How the words after `/name` fill a prompt's arguments (`docs/mcp.md`,
/// "Prompts and resources"): the named arguments for the `prompts/get`
/// call, and what follows the prompt's own text.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Filled {
    /// One entry per argument the text fills, by the server's name.
    pub named: Map<String, Value>,
    /// What follows the prompt's text after a blank line: the rest of the
    /// text for a prompt with no arguments.
    pub appended: Option<String>,
}

/// Fills `arguments`, in the server's order, from the words after `/name`
/// (`docs/mcp.md`, "Prompts and resources"): one whitespace-separated
/// word each, and the last argument takes the rest of the text, trimmed
/// at both ends. An argument with no text is left out of the map. `Err`
/// lists the missing required names in order. A prompt with no arguments
/// puts the text in `appended`, so it follows the prompt's text as a
/// skill's arguments do; no text means no `appended`.
pub(crate) fn fill(arguments: &[Argument], text: &str) -> Result<Filled, Vec<String>> {
    if arguments.is_empty() {
        let trimmed = text.trim();
        return Ok(Filled {
            named: Map::new(),
            appended: if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_owned())
            },
        });
    }
    let mut named = Map::new();
    let mut missing = Vec::new();
    let mut rest = text;
    for (index, argument) in arguments.iter().enumerate() {
        if index + 1 == arguments.len() {
            let trimmed = rest.trim();
            if trimmed.is_empty() {
                if argument.required {
                    missing.push(argument.name.clone());
                }
            } else {
                named.insert(argument.name.clone(), Value::String(trimmed.to_owned()));
            }
        } else {
            let (word, remaining) = take_word(rest);
            rest = remaining;
            match word {
                Some(found) => {
                    named.insert(argument.name.clone(), Value::String(found.to_owned()));
                }
                None => {
                    if argument.required {
                        missing.push(argument.name.clone());
                    }
                }
            }
        }
    }
    if missing.is_empty() {
        Ok(Filled {
            named,
            appended: None,
        })
    } else {
        Err(missing)
    }
}

/// The next whitespace-separated word of `rest`, and what follows it.
fn take_word(rest: &str) -> (Option<&str>, &str) {
    let words = rest.trim_start();
    if words.is_empty() {
        return (None, "");
    }
    let end = words
        .find(|char: char| char.is_whitespace())
        .unwrap_or(words.len());
    (Some(&words[..end]), &words[end..])
}

/// The person's message from a `prompts/get` result (`docs/mcp.md`,
/// "Prompts and resources"): the text of every message in order, joined
/// by one blank line, whatever its role; an embedded text resource counts
/// as text. `Err` is the reason for the rejection's sentence: any image,
/// audio, binary resource or resource link, or a result with no text.
pub(crate) fn text(result: &PromptResult) -> Result<String, String> {
    let mut texts = Vec::new();
    for message in &result.messages {
        for part in &message.content {
            match part {
                Content::Text { text: Some(found) } if !found.is_empty() => {
                    texts.push(found.clone());
                }
                Content::Text { .. } => {}
                Content::Image => return Err("an image".to_owned()),
                Content::Audio => return Err("audio".to_owned()),
                Content::Resource { resource } => match &resource.text {
                    Some(found) if !found.is_empty() => texts.push(found.clone()),
                    Some(_) => {}
                    None if resource.blob.is_some() => {
                        return Err("a binary resource".to_owned());
                    }
                    None => return Err("an unreadable resource".to_owned()),
                },
                Content::ResourceLink => return Err("a resource link".to_owned()),
                Content::Other => return Err("unsupported content".to_owned()),
                Content::Unreadable => return Err("unreadable content".to_owned()),
            }
        }
    }
    if texts.is_empty() {
        return Err("no text".to_owned());
    }
    Ok(texts.join("\n\n"))
}

/// One prompt row's source: whose list it came from, and what running it
/// needs.
#[derive(Debug, Clone)]
pub(crate) struct PromptSource {
    /// The server's configured name: the row's tag.
    pub server: String,
    /// The prompt, as the server listed it.
    pub prompt: ListedPrompt,
    /// The server's slot: running it starts a lazy server.
    pub slot: Weak<Slot>,
    /// Each `prompts/get` call's timeout, from the spec.
    pub timeout: Duration,
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

    /// Runs `server`'s `prompt` with the words after `/name` as `arguments`
    /// (`docs/mcp.md`, "Prompts and resources"): the prompt's text, then
    /// `\n\n` and the rest of the text when the prompt takes no arguments,
    /// as the person's message. `content` holds that one text part on
    /// success; `error` is `invalid_arguments` for a missing required
    /// argument and `mcp_prompt_failed` for every server-side cause, with
    /// Fiber's sentence naming the server, the prompt and the cause; and
    /// `servers` holds what this run observed. Running starts a lazy
    /// server, as a first tool call does, and a request whose server dies
    /// is never replayed.
    pub fn get(&self, server: &str, prompt: &str, arguments: &str, cancel: &dyn Cancel) -> Output {
        let Some(entry) = self
            .entries
            .iter()
            .find(|entry| entry.server == server && entry.prompt.name == prompt)
        else {
            return failed(
                ErrorCode::McpPromptFailed,
                format!("The MCP server `{server}` has no prompt `/{prompt}`."),
            );
        };
        let filled = match fill(&entry.prompt.arguments, arguments) {
            Ok(filled) => filled,
            Err(missing) => {
                let needs = missing
                    .iter()
                    .map(|name| format!("<{name}>"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let follow = match hint(&entry.prompt.arguments) {
                    Some(hint) => format!(" Run it as `/{prompt} {hint}`."),
                    None => String::new(),
                };
                return failed(
                    ErrorCode::InvalidArguments,
                    format!(
                        "The MCP server `{server}`'s prompt `/{prompt}` needs {needs}.{follow}"
                    ),
                );
            }
        };
        let Some(slot) = entry.slot.upgrade() else {
            return failed(
                ErrorCode::McpPromptFailed,
                not_run(server, prompt, &slot::unavailable(server)),
            );
        };
        match slot.serve() {
            Served::Failed(failed) => Output {
                error: Some(Failure {
                    code: ErrorCode::McpPromptFailed,
                    message: not_run(server, prompt, &failed.error.message),
                    retry_after_ms: None,
                    provider: None,
                }),
                servers: failed.records,
                ..Output::default()
            },
            Served::Up(running, mut servers) => {
                let mut output = match running.get_prompt(
                    prompt,
                    &filled.named,
                    entry.timeout,
                    cancel,
                ) {
                    Ok(result) => match serde_json::from_value::<PromptResult>(result) {
                        Err(_) => failed(
                            ErrorCode::McpPromptFailed,
                            format!(
                                "The MCP server `{server}`'s prompt `/{prompt}` returned no messages, \
                                 which Fiber cannot send as a message."
                            ),
                        ),
                        Ok(result) => match text(&result) {
                        Ok(body) => {
                            let mut full = body;
                            if let Some(appended) = filled.appended {
                                full.push_str("\n\n");
                                full.push_str(&appended);
                            }
                            Output {
                                content: vec![ContentPart::Text { text: full }],
                                ..Output::default()
                            }
                        }
                        Err(reason) => failed(
                            ErrorCode::McpPromptFailed,
                            format!(
                                "The MCP server `{server}`'s prompt `/{prompt}` returned {reason}, \
                                 which Fiber cannot send as a message."
                            ),
                        ),
                        },
                    },
                    Err(CallError::Timeout) => failed(
                        ErrorCode::McpPromptFailed,
                        format!(
                            "The MCP server `{server}` did not answer the prompt `/{prompt}` \
                             within {} ms.",
                            entry.timeout.as_millis(),
                        ),
                    ),
                    Err(CallError::Cancelled) => failed(
                        ErrorCode::McpPromptFailed,
                        format!(
                            "The run of the prompt `/{prompt}` on the MCP server `{server}` was \
                             cancelled; the server may still act on it."
                        ),
                    ),
                    // The run is never replayed: the next run restarts
                    // the server, if a restart is left.
                    Err(CallError::Gone) => match slot.died(&running) {
                        Some(record) => {
                            let output = failed(
                                ErrorCode::McpPromptFailed,
                                not_run(server, prompt, &record.error.message),
                            );
                            servers.push(ServerRecord::Failed(record));
                            output
                        }
                        None => failed(
                            ErrorCode::McpPromptFailed,
                            not_run(server, prompt, &slot::unavailable(server)),
                        ),
                    },
                    Err(CallError::JsonRpc { message, .. }) => failed(
                        ErrorCode::McpPromptFailed,
                        format!(
                            "The MCP server `{server}` refused the prompt `/{prompt}`: {message}."
                        ),
                    ),
                };
                output.servers = servers;
                output
            }
        }
    }
}

fn failed(code: ErrorCode, message: String) -> Output {
    Output {
        error: Some(Failure {
            code,
            message,
            retry_after_ms: None,
            provider: None,
        }),
        ..Output::default()
    }
}

/// The sentence when the prompt's text cannot be had for a server-side
/// cause (`docs/errors.md`, `mcp_prompt_failed`): the server, the prompt
/// and the cause. Every cause the slot reports starts with "The ", so it
/// reads as the sentence's end.
fn not_run(server: &str, prompt: &str, cause: &str) -> String {
    let cause = cause.strip_prefix("The ").unwrap_or(cause);
    format!("The MCP server `{server}`'s prompt `/{prompt}` was not run: the {cause}")
}

#[cfg(test)]
#[path = "prompt_tests.rs"]
mod tests;
