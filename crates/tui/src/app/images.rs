//! The per-session image registry: viewer fetches and the out-queue
//! (`docs/tui.md`, "Images").

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use super::{App, Effect, Link, mint};
use crate::image;

/// One `read_file` out for a viewer open: what its answer needs to
/// queue the open.
struct Fetch {
    /// The image's id.
    id: u32,
    /// The session the image came from, for the viewer's copy.
    session: String,
    /// The logged path, for the `read_file` line.
    path: String,
}

/// What the app holds per session for its images: the viewer fetches
/// out, the views queued for the loop, and the session generation,
/// so a late viewer completion after a session change is dropped.
#[derive(Default)]
pub(crate) struct Images {
    fetches: BTreeMap<String, Fetch>,
    /// Images whose file would not open: missing, too large or
    /// refused. They stay their line, with the notice said once, and
    /// are not asked for again in this session.
    refused: BTreeSet<u32>,
    out: image::Out,
    generation: u64,
}

impl App {
    /// Opens the image `id` names in the system viewer: asks for its
    /// file again with `read_file`, the terminal keeping no bytes.
    /// While one fetch for it is out, another click does nothing;
    /// with the link down nothing goes out; and an image whose file
    /// would not open is not asked for again in this session.
    pub(crate) fn view_image(&mut self, id: u32) -> Effect {
        let Some(part) = self.screen.pages().image(id) else {
            return Effect::None;
        };
        let path = part.path.clone();
        if self.images.refused.contains(&id)
            || self.images.fetches.values().any(|fetch| fetch.id == id)
        {
            return Effect::None;
        }
        let Some(session) = self.session().cloned() else {
            return Effect::None;
        };
        if self.link != Link::Up {
            return Effect::None;
        }
        let command_id = mint();
        let line = json!({
            "id": command_id,
            "command": "read_file",
            "args": {"session": session.0, "path": path},
        })
        .to_string();
        self.images.fetches.insert(
            command_id,
            Fetch {
                id,
                session: session.0.clone(),
                path,
            },
        );
        Effect::Send(vec![line])
    }

    /// Answers our `read_file` for a viewer open: queues the view for
    /// the loop's worker. True when `id` was ours.
    pub(in crate::app) fn image_answered(&mut self, id: &str, result: Option<&Value>) -> bool {
        let Some(fetch) = self.images.fetches.remove(id) else {
            return false;
        };
        let name = file_name(&fetch.path);
        let Some(data) = result
            .and_then(|result| result.get("data"))
            .and_then(Value::as_str)
        else {
            self.images.refused.insert(fetch.id);
            self.cant_open(&name, "the answer carried no file");
            return true;
        };
        self.images.out.view.push(image::View {
            id: fetch.id,
            name,
            session: fetch.session,
            data: data.to_owned(),
            generation: self.images.generation,
        });
        true
    }

    /// A `read_file` for a viewer open was rejected: one notice says
    /// why, and nothing retries. True when `id` was ours.
    pub(in crate::app) fn image_rejected(&mut self, id: &str, message: &str) -> bool {
        let Some(fetch) = self.images.fetches.remove(id) else {
            return false;
        };
        self.images.refused.insert(fetch.id);
        let name = file_name(&fetch.path);
        self.cant_open(&name, message);
        true
    }

    /// A viewer worker finished opening image `id`: nothing on success,
    /// or the notice saying why, and the image refused so it is not
    /// asked for again. A completion from an earlier session is
    /// dropped, even for the same file name.
    pub(crate) fn image_viewed(
        &mut self,
        id: u32,
        name: &str,
        generation: u64,
        result: Result<(), String>,
    ) {
        if generation != self.images.generation {
            return;
        }
        if let Err(reason) = result {
            self.images.refused.insert(id);
            self.cant_open(name, &reason);
        }
    }

    /// The queued viewer opens, for the loop's workers.
    pub(crate) fn take_image_out(&mut self) -> image::Out {
        std::mem::take(&mut self.images.out)
    }

    /// Forgets the session's images on every session change: fetches
    /// and queued views go, and the generation moves, so a late
    /// answer or viewer completion is dropped.
    pub(in crate::app) fn forget_images(&mut self) {
        self.images.fetches.clear();
        self.images.refused.clear();
        self.images.out = image::Out::default();
        self.images.generation = self.images.generation.saturating_add(1);
    }

    /// Drops the viewer fetches with the connection: they are asked
    /// again when drawn with the link up. That is a new connection,
    /// not a retry.
    pub(in crate::app) fn images_disconnected(&mut self) {
        self.images.fetches.clear();
    }

    /// The whole image line `id` names, uncut: what `y` copies.
    pub(in crate::app) fn image_label(&self, id: u32) -> Option<String> {
        self.screen.pages().image(id).map(|part| part.label())
    }

    /// Shows why `name` would not open, once.
    fn cant_open(&mut self, name: &str, reason: &str) {
        let reason = reason.strip_suffix('.').unwrap_or(reason);
        self.notices
            .push(format!("Could not open {name}: {reason}."));
    }
}

/// The file name a notice names: the logged path's last component.
fn file_name(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_owned()
}

#[cfg(test)]
#[path = "images_tests.rs"]
mod tests;
