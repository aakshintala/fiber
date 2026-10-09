//! The session's image ids: every image part is noted when its line
//! is folded, live or on a page reload, so each path draws under one
//! id (`docs/tui.md`, "Images"). A child of `window`, so it reads the
//! pages' fields.

use contract::events::{SteeringApplied, ToolCallCompleted, TurnStarted};
use contract::{Envelope, events::InputItem};
use serde_json::Value;

use super::Pages;
use crate::image;

impl Pages {
    /// Notes every image part `envelope` carries, handing each unseen
    /// path the next id; a path already seen keeps its id, so a page
    /// dropped and folded again draws the same id.
    pub(super) fn note_images(&mut self, envelope: &Envelope) {
        let payload = Value::Object(envelope.payload.clone());
        let parts: Vec<image::Part> = match envelope.kind.as_str() {
            "turn_started" => serde_json::from_value::<TurnStarted>(payload)
                .map(|started| {
                    started
                        .input
                        .iter()
                        .filter_map(|input| {
                            if let InputItem::Message { content, .. } = input {
                                Some(image::parts(content))
                            } else {
                                None
                            }
                        })
                        .flatten()
                        .collect()
                })
                .unwrap_or_default(),
            "tool_call_completed" => serde_json::from_value::<ToolCallCompleted>(payload)
                .map(|done| image::parts(&done.content))
                .unwrap_or_default(),
            "steering_applied" => serde_json::from_value::<SteeringApplied>(payload)
                .map(|applied| image::parts(&applied.content))
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        for part in &parts {
            self.images.note(part);
        }
    }

    /// The image `id` names, on any page.
    pub(crate) fn image(&self, id: u32) -> Option<&image::Part> {
        self.images.parts.get(&id)
    }

    /// The conversation's width in columns: what an image's line is cut
    /// to (`docs/tui.md`, "Images").
    pub(crate) fn column_width(&self) -> u16 {
        self.width
    }
}

#[cfg(test)]
#[path = "images_tests.rs"]
mod tests;
