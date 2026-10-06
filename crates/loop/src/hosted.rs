//! A call the provider ran itself, such as a hosted web search
//! (`docs/tools.md`, "Hosted by the provider"): Fiber writes what happened
//! and never reviews, runs or answers it.

use contract::events::{Event, ToolCallStarted};
use contract::provider::HostedCall;
use contract::shapes::DeclaredEffects;
use contract::{ActionId, TurnId};
use serde_json::Map;

use crate::{Error, Loop, mint};

impl Loop {
    /// Writes `hosted` as `tool_call_requested`, `tool_call_started` and
    /// `tool_call_completed` under one new action, in that order. The
    /// provider already ran it, so no permission, hook, schema check or run
    /// comes between the lines. `tool_call_started` carries the registered
    /// tool's effects for the call; an absent tool or an effects error
    /// declares none.
    pub(crate) fn write_hosted(&mut self, hosted: &HostedCall, turn: &TurnId) -> Result<(), Error> {
        let id = ActionId(mint("a_"));
        let none = Map::new();
        let arguments = hosted.call.arguments.as_object().unwrap_or(&none);
        let declared = self
            .tools
            .get(&hosted.call.name)
            .and_then(|(_, tool, _)| tool.effects(arguments).ok())
            .map_or_else(
                || DeclaredEffects {
                    effects: Vec::new(),
                    reversible: true,
                    paths: None,
                },
                |effects| effects.declared,
            );
        self.append(
            &Event::ToolCallRequested(hosted.call.clone()),
            turn,
            Some(&id),
        )?;
        self.append(
            &Event::ToolCallStarted(ToolCallStarted {
                declared,
                arguments: None,
                changed_by: None,
            }),
            turn,
            Some(&id),
        )?;
        self.append(
            &Event::ToolCallCompleted(hosted.completed.clone()),
            turn,
            Some(&id),
        )?;
        Ok(())
    }
}
