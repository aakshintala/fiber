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
        let full = crate::conversation::text(&ran.text);
        let Some(hooks) = self.hooks.clone() else {
            return Ok(self
                .capped(ran.text, full, ran.details, None, bound, id)?
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
            self.append(&Event::Notice(notice), turn, Some(id))?;
        }
        let changed_by = (!answer.changed_by.is_empty()).then_some(answer.changed_by);
        let mut shaped = match answer.outcome {
            AfterToolOutcome::Unchanged => self
                .capped(ran.text, full, ran.details, None, bound, id)?
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
                let (text, full) = match content {
                    Some(text) if text.is_empty() => (Vec::new(), text),
                    Some(text) => (vec![ContentPart::Text { text: text.clone() }], text),
                    None => (ran.text, full),
                };
                self.capped(text, full, details, artifact, bound, id)?
                    .with(ran.images)
            }
        };
        shaped.changed_by = changed_by;
        Ok(shaped)
    }

    /// `text`, whose joined text is `full`, cut to `bound`. The artifact
    /// holds `artifact` when a hook returned it, written even when `full`
    /// fits, or else `full` when it was cut. A hook's artifact that cannot
    /// be written fails the turn, as a log write does.
    fn capped(
        &self,
        text: Vec<ContentPart>,
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
            (text, None)
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
