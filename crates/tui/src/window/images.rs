//! The chain's image ids: every image part is noted under its segment
//! session when its line is folded, live or on a page reload, so each
//! `(session, path)` draws under one id (`docs/tui.md`, "Images"). A
//! child of `window`, so it reads the pages' fields.

use contract::events::{SteeringApplied, ToolCallCompleted, TurnStarted};
use contract::{Envelope, events::InputItem};
use serde_json::Value;

use super::Pages;
use crate::image;

impl Pages {
    /// Shares the app's process-lifetime image ids with this page layout.
    pub(crate) fn set_image_ids(&mut self, ids: image::IdAllocator) {
        self.images.set_allocator(ids);
    }

    /// Notes every image part `envelope` carries under its segment
    /// session, handing each unseen `(session, path)` the next id; a
    /// pair already seen keeps its id, so a page dropped and folded
    /// again draws the same id.
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
                                Some(image::parts(&envelope.session_id, content))
                            } else {
                                None
                            }
                        })
                        .flatten()
                        .collect()
                })
                .unwrap_or_default(),
            "tool_call_completed" => serde_json::from_value::<ToolCallCompleted>(payload)
                .map(|done| image::parts(&envelope.session_id, &done.content))
                .unwrap_or_default(),
            "steering_applied" => serde_json::from_value::<SteeringApplied>(payload)
                .map(|applied| image::parts(&envelope.session_id, &applied.content))
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

    /// How the images size: the one line, or the inline grid with its
    /// cell size (`docs/tui.md`, "Images"). A flat page reports text:
    /// flat rendering never fetches (`docs/tui.md`, "Screen readers").
    pub(crate) fn image_sizing(&self) -> image::Sizing {
        if self.ext.flat {
            image::Sizing::Text
        } else {
            self.images.sizing
        }
    }

    /// Sets the image sizing, recounting when it changes, so paging
    /// counts the inline rows from the logged size without decoding any
    /// image (`docs/tui.md`, "Images").
    pub(crate) fn set_image_sizing(&mut self, sizing: image::Sizing) {
        if self.images.sizing == sizing {
            return;
        }
        self.images.sizing = sizing;
        self.recount();
    }

    /// Marks the image at `path` in `session` failed and recounts: its
    /// rows shrink to the one-line form, and no fetch asks for it again
    /// while the session's layout lives (`docs/tui.md`, "Images").
    pub(crate) fn fail_image(&mut self, session: &contract::SessionId, path: &str) {
        self.images.fail(session, path);
        self.recount();
    }

    /// Whether the image at `path` in `session` failed: one line, no
    /// refetch (`docs/tui.md`, "Images").
    pub(crate) fn image_failed(&self, session: &contract::SessionId, path: &str) -> bool {
        self.images.failed(session, path)
    }
}

#[cfg(test)]
#[path = "images_tests.rs"]
mod tests;
