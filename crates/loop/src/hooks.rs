//! The `after_tool` call site (`docs/extensions.md`, "The hook points" and
//! "When a hook fails"): a call that ran has returned, and the session's
//! hooks see its output before it is cut, written as an artifact or logged.
//! The loop treats the tool's name and arguments as opaque data, and an
//! extension's name reaches it only on the answer (`docs/architecture.md`,
//! "The call rules").

use std::sync::Arc;

use contract::events::{CallStatus, Event};
use contract::hook::{AfterToolCall, AfterToolOutcome, Hooks};
use contract::shapes::{ContentPart, Process};
use contract::tool::Bound;
use contract::{ActionId, TurnId};
use serde_json::{Map, Value};

use crate::{Error, Loop};

/// A call that ran, as its completion is about to be built: the text parts
/// of its output, and what the hook is shown beside them.
pub(crate) struct Ran<'a> {
    /// The tool's name, as the model called it.
    pub(crate) tool: &'a str,
    /// The arguments it ran with.
    pub(crate) arguments: &'a Map<String, Value>,
    /// The status the completion carries.
    pub(crate) status: CallStatus,
    /// The output's text parts.
    pub(crate) text: Vec<ContentPart>,
    /// Its other parts, such as images, which no hook sees.
    pub(crate) images: Vec<ContentPart>,
    /// The tool's `details`.
    pub(crate) details: Option<Value>,
    /// How its process ended.
    pub(crate) process: Option<&'a Process>,
}

/// What the completion carries of the output, once the hooks have answered
/// and the cap is applied.
pub(crate) struct Shaped {
    pub(crate) content: Vec<ContentPart>,
    pub(crate) details: Option<Value>,
    pub(crate) artifact: Option<String>,
    pub(crate) changed_by: Option<Vec<String>>,
}

impl Loop {
    /// Runs the session's `after_tool` hooks on every call that ran
    /// (`docs/extensions.md`, "The hook points"). Without hooks the tool's
    /// output is the output.
    pub fn hooks(mut self, hooks: Arc<dyn Hooks>) -> Self {
        self.hooks = Some(hooks);
        self
    }

    /// Asks the hooks about `ran`, writes each notice they raised under
    /// `id`, and applies the answer and the size cap `bound`
    /// (`docs/tools.md`, "Bounded results"). What a hook replaced is never
    /// written: the cap and the artifact see only what it returned.
    pub(crate) fn shape(
        &mut self,
        ran: Ran<'_>,
        bound: Bound,
        id: &ActionId,
        turn: &TurnId,
    ) -> Result<Shaped, Error> {
        // Under a shutdown no hook runs, so nothing is left to redact the
        // output: the completion carries none of it (`docs/invocation.md`,
        // "Shutdown").
        if self.shutting_down() {
            return Ok(Shaped {
                content: Vec::new(),
                details: None,
                artifact: None,
                changed_by: None,
            });
        }
        let (parts, full) = joined(ran.text);
        let Some(hooks) = self.hooks.clone() else {
            return Ok(self
                .capped(parts, full, ran.details, None, bound, id)?
                .with(ran.images));
        };
        let answer = hooks.after_tool(&AfterToolCall {
            tool: ran.tool,
            arguments: ran.arguments,
            status: ran.status,
            content: &full,
            details: ran.details.as_ref(),
            process: ran.process,
        });
        for notice in answer.notices {
            self.append(&Event::Notice(notice), turn, None)?;
        }
        let changed_by = (!answer.changed_by.is_empty()).then_some(answer.changed_by);
        let mut shaped = match answer.outcome {
            AfterToolOutcome::Unchanged => self
                .capped(parts, full, ran.details, None, bound, id)?
                .with(ran.images),
            // Its only content is the line: images go too.
            AfterToolOutcome::Withheld { extension } => Shaped {
                content: vec![ContentPart::Text {
                    text: format!(
                        "Output withheld: the `after_tool` hook of extension {extension} failed."
                    ),
                }],
                details: None,
                artifact: None,
                changed_by: None,
            },
            AfterToolOutcome::Changed {
                content,
                details,
                artifact,
            } => {
                let details = details.or(ran.details);
                let (parts, full) = match content {
                    Some(text) if text.is_empty() => (Some(Vec::new()), text),
                    Some(text) => (None, text),
                    None => (parts, full),
                };
                self.capped(parts, full, details, artifact, bound, id)?
                    .with(ran.images)
            }
        };
        shaped.changed_by = changed_by;
        Ok(shaped)
    }

    /// The text parts whose joined text is `full`, cut to `bound`: `parts`,
    /// or `full` as the one part when `parts` is `None`. The artifact
    /// holds `artifact` when a hook returned it, written even when `full`
    /// fits, or else `full` when it was cut. A hook's artifact that cannot
    /// be written fails the turn, as a log write does.
    fn capped(
        &self,
        parts: Option<Vec<ContentPart>>,
        full: String,
        details: Option<Value>,
        artifact: Option<String>,
        bound: Bound,
        id: &ActionId,
    ) -> Result<Shaped, Error> {
        let name = format!("{}.txt", id.0);
        let cap = bound.start.saturating_add(bound.end);
        let (content, cut) = if full.len() > cap {
            let (kept, artifact) = self.log.cut_output(&full, bound, &name);
            (vec![ContentPart::Text { text: kept }], artifact)
        } else {
            (
                parts.unwrap_or_else(|| vec![ContentPart::Text { text: full }]),
                None,
            )
        };
        let artifact = match artifact {
            // The hook's text replaces what the cut wrote under the same
            // name, so the cut's notice points at it.
            Some(artifact) => Some(self.log.write_artifact(&name, artifact.as_bytes())?.0),
            None => cut,
        };
        Ok(Shaped {
            content,
            details,
            artifact,
            changed_by: None,
        })
    }
}

/// The joined text of `parts`, and the parts when they are not that text
/// as one part. A lone text part is moved into the joined text, so a large
/// result is never copied; several are joined with `\n`.
fn joined(parts: Vec<ContentPart>) -> (Option<Vec<ContentPart>>, String) {
    let parts = match <[ContentPart; 1]>::try_from(parts) {
        Ok([ContentPart::Text { text }]) => return (None, text),
        Ok(one) => Vec::from(one),
        Err(parts) => parts,
    };
    let full = crate::conversation::text(&parts);
    (Some(parts), full)
}

impl Shaped {
    /// The output's other parts, after its text.
    fn with(mut self, images: Vec<ContentPart>) -> Self {
        self.content.extend(images);
        self
    }
}

#[cfg(test)]
#[path = "hooks_tests.rs"]
mod tests;
