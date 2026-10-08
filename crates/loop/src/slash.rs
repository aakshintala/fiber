//! A `prompt` whose first word is `/name` (`docs/invocation.md`, "Getting
//! a prompt in"): a skill's text with the rest of the prompt as its
//! arguments, or an MCP server's prompt's text asked with the typed
//! arguments; anything else is sent as written. A steer never expands.

use std::sync::Arc;

use contract::events::{CommandInfo, Event};
use contract::inbox::{Message, Rejection};
use contract::shapes::ContentPart;
use contract::tool::{Cancel, Output, ServerRecord};

use crate::skills;
use crate::{Error, Loop};

/// Asks `server`'s `prompt` for its text with the words after `/name`:
/// what `main` builds from the session's MCP servers (`docs/mcp.md`,
/// "Prompts and resources"). Reads `content`, `error` and `servers` only,
/// so `loop` never depends on `mcp` (`docs/architecture.md`, "The call
/// rules").
pub type FetchPrompt = Arc<dyn Fn(&str, &str, &str, &dyn Cancel) -> Output + Send + Sync>;

/// The session's MCP prompt rows and how to run one.
pub struct ServerPrompts {
    /// The `commands` rows, tagged with each server's name.
    pub rows: Vec<CommandInfo>,
    /// Asks a server for a prompt's text.
    pub fetch: FetchPrompt,
}

/// The first prompt row of `name`: between servers, the first in name
/// order wins, because the rows sort by server name
/// (`docs/system-prompt.md`, "Skills").
pub(crate) fn prompt_row<'a>(rows: &'a [CommandInfo], name: &str) -> Option<&'a CommandInfo> {
    rows.iter().find(|row| row.name == name)
}

impl Loop {
    /// Runs the session's MCP prompts by `/name` through `prompts`
    /// (`docs/mcp.md`, "Prompts and resources"). Without it every `/name`
    /// that names no skill is sent as written.
    pub fn server_prompts(mut self, prompts: ServerPrompts) -> Self {
        self.server_prompts = Some(prompts);
        self
    }

    /// Switches off the skills and prompts these names name, as
    /// `skills.disabled` does (`docs/system-prompt.md`, "Skills").
    pub fn skills_disabled(mut self, names: Vec<String>) -> Self {
        self.prompt.skills_disabled = names;
        self
    }

    /// Resolves a `prompt` whose first word is `/name`: `Ok(Ok)` carries
    /// the message expanded or as written, `Ok(Err)` is the prompt's
    /// rejection, and the outer `Err` is a log failure
    /// (`docs/invocation.md`, "Getting a prompt in"). A skill, from every
    /// source, and a name in `skills.disabled` are checked before any
    /// fetch; the fetch runs only for a `prompt`, only when the first part
    /// is text whose first word names a row. Server lines the fetch
    /// returns are written as durable lines with no turn and no action, in
    /// the order observed, before the prompt joins the turn or its
    /// rejection.
    pub(crate) fn slash(&mut self, message: Message) -> Result<Result<Message, Rejection>, Error> {
        let Some(ContentPart::Text { text }) = message.content.first() else {
            return Ok(Ok(message));
        };
        let Some((name, args)) = skills::split_command(text) else {
            return Ok(Ok(message));
        };
        let (name, args) = (name.to_owned(), args.to_owned());
        // A name in `skills.disabled` expands nothing, an MCP prompt of
        // that name included (`docs/system-prompt.md`, "Skills").
        if self.prompt.skills_disabled.iter().any(|off| off == &name) {
            return Ok(Ok(message));
        }
        // The repository's top level, as the opening message reads it
        // (`opening::collect`): a skill under a parent repository is found
        // from a subdirectory workspace.
        let (chain, _) = crate::opening::repo_chain(&self.workspace);
        let top = chain.first().unwrap_or(&self.workspace);
        // Any skill wins over an MCP prompt, from every source
        // (`docs/system-prompt.md`, "Skills").
        let skill = skills::discover(&self.prompt, top)
            .skills
            .into_iter()
            .find(|found| found.listed.name == name);
        if skill.is_some() {
            let mut message = message;
            if let Some(content) = skills::expand(&self.prompt, top, &message.content) {
                message.content = content;
            }
            return Ok(Ok(message));
        }
        let Some(prompts) = self.server_prompts.as_ref() else {
            return Ok(Ok(message));
        };
        let Some(row) = prompt_row(&prompts.rows, &name) else {
            return Ok(Ok(message));
        };
        let Output {
            content,
            error,
            servers,
            ..
        } = (prompts.fetch)(&row.tag, &row.name, &args, self.cancel.as_ref());
        for record in servers {
            let event = match record {
                ServerRecord::Failed(failed) => Event::McpServerFailed(failed),
                ServerRecord::Ready(ready) => Event::McpServerReady(ready),
            };
            self.log.append(&event, None, None)?;
        }
        if let Some(error) = error {
            return Ok(Err(Rejection {
                code: error.code,
                message: error.message,
            }));
        }
        let Some(ContentPart::Text { text: fetched }) = content.first() else {
            return Ok(Err(Rejection {
                code: contract::ErrorCode::McpPromptFailed,
                message: format!(
                    "The MCP server `{}`'s prompt `/{}` returned no text, which Fiber cannot send as a message.",
                    row.tag, row.name
                ),
            }));
        };
        // The fetch keeps the contract: one text part. Later parts of the
        // person's message, such as a pasted image, stay after the
        // prompt's text, as `skills::expand` keeps them
        // (`docs/invocation.md`, "Getting a prompt in").
        let mut message = message;
        if let Some(ContentPart::Text { text }) = message.content.first_mut() {
            *text = fetched.clone();
        }
        Ok(Ok(message))
    }
}
