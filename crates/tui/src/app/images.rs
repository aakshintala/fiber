//! The chain's image viewer state: fetches and the out-queue, keyed by
//! each part's segment session (`docs/tui.md`, "Images").

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde_json::{Value, json};

use super::{App, Effect, Target, mint};
use crate::event_loop::kitty_writer;
use crate::image;

/// One `read_file` out for a viewer open: what its answer needs to
/// queue the open.
struct Fetch {
    /// The image's id on the screen that drew it.
    id: u32,
    /// The session the image came from, for the viewer's copy.
    session: String,
    /// The logged path, for the `read_file` line: the fetch's
    /// identity, so a refusal survives the swap's new ids.
    path: String,
}

/// Whether an inline image's bytes are asked for, converting, shown,
/// or failed: a converting, shown or failed image is not asked for
/// again while its session's layout lives (`docs/tui.md`, "Images").
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DisplayState {
    Asked,
    Converting,
    Shown,
    Failed,
}

/// A display fetch for an inline image, by its `(session, path)`: the
/// `read_file` command out, the placeholder id its answer draws under,
/// and the session generation that asked, so a late answer draws
/// nowhere stale.
#[derive(Clone)]
struct Display {
    /// The `read_file` command's id: the answer's dispatch key.
    command: String,
    /// The placeholder id the answer transmits under.
    id: u32,
    /// The session generation that asked.
    generation: u64,
    /// Whether its bytes are asked for, shown, or failed.
    state: DisplayState,
    /// The logged pixel size: the answer grids it under the current
    /// width and cell.
    width: u32,
    height: u32,
}

/// What the app holds per session for its images: the viewer fetches
/// out, the views queued for the loop, and the session generation,
/// so a late viewer completion after a session change is dropped.
#[derive(Default)]
pub(crate) struct Images {
    fetches: BTreeMap<String, Fetch>,
    /// Images whose file would not open, by `(session, path)`: missing,
    /// too large or refused. They stay their line, with the notice said
    /// once, and are not asked for again in that segment.
    refused: BTreeSet<(String, String)>,
    /// The inline images' display fetches, by `(session, path)`.
    display: BTreeMap<(String, String), Display>,
    /// The generation that last queued a transmit for each placeholder
    /// id: a kitty error answers a transmit, so an error for an id
    /// nothing transmitted in this generation is an older session's and
    /// drops (`docs/tui.md`, "Images").
    transmitted: BTreeMap<u32, u64>,
    /// The process-lifetime image ids, shared with every page layout.
    pub(super) id_allocator: image::IdAllocator,
    /// The session-change deletes queued for the loop's pre-draw drain:
    /// shown images leave the terminal before the next session draws.
    deletes: Vec<kitty_writer::Delete>,
    out: image::Out,
    generation: u64,
    /// The queued conversions in asking order, holding their base64
    /// until the one worker starts: dropped on a session change
    /// (`docs/tui.md`, "Images").
    convert_queue: VecDeque<image::Convert>,
    /// Whether the one conversion child runs, across sessions: only an
    /// `Input::Converted` completion clears it, so a session change
    /// never starts a second child (`docs/tui.md`, "Images").
    convert_busy: bool,
}

impl App {
    /// Opens the image `id` names in the system viewer: asks for its
    /// file again with `read_file`, the terminal keeping no bytes.
    /// While one fetch for it is out, another click does nothing;
    /// with the link down nothing goes out; and an image whose file
    /// would not open is not asked for again in its segment.
    pub(crate) fn view_image(&mut self, id: u32) -> Effect {
        let Some(part) = self.screen.pages().image(id) else {
            return Effect::None;
        };
        // The read runs under the part's segment session, not the
        // attached one: an ancestor image resolves under its own log.
        let session = part.session.clone();
        let path = part.path.clone();
        if self
            .images
            .refused
            .contains(&(session.0.clone(), path.clone()))
            || self
                .images
                .fetches
                .values()
                .any(|fetch| fetch.session == session.0 && fetch.path == path)
        {
            return Effect::None;
        }
        if self.session().is_none() {
            return Effect::None;
        };
        if !self.outbox.is_up() {
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
    /// the loop's worker. A display fetch answers here too: PNG bytes
    /// transmit under the placeholders, JPEG or WebP bytes queue a
    /// conversion, and anything else fails its image with a notice
    /// (`docs/tui.md`, "Images"). True when `id` was ours.
    pub(in crate::app) fn image_answered(&mut self, id: &str, result: Option<&Value>) -> bool {
        let display = self
            .images
            .display
            .iter()
            .find(|(_, fetch)| fetch.command == id)
            .map(|(key, _)| key.clone());
        if let Some(key) = display {
            return self.display_answered(key, result);
        }
        let Some(fetch) = self.images.fetches.remove(id) else {
            return false;
        };
        let name = file_name(&fetch.path);
        let Some(data) = result
            .and_then(|result| result.get("data"))
            .and_then(Value::as_str)
        else {
            let name = file_name(&fetch.path);
            self.images
                .refused
                .insert((fetch.session.clone(), fetch.path));
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
    /// why, and nothing retries. A display fetch answers here too: one
    /// notice says why its image failed, which keeps its one line and is
    /// not asked for again (`docs/tui.md`, "Images"). True when `id`
    /// was ours.
    /// Answers our `read_file` for a display fetch: a PNG answer queues
    /// its transmit and its place over the placeholders, and marks the
    /// image shown; a JPEG or WebP answer queues a conversion of its
    /// bytes instead. Missing or empty bytes or any other answer fail
    /// the image with one notice; a stale generation or a second answer
    /// drops with none (`docs/tui.md`, "Images").
    fn display_answered(&mut self, key: (String, String), result: Option<&Value>) -> bool {
        let Some(fetch) = self.images.display.remove(&key) else {
            return false;
        };
        // A late or duplicate answer draws nowhere: the record stands, so
        // a redraw still finds it shown or failed and asks nothing again.
        // Only the asked generation shows, once.
        if fetch.state != DisplayState::Asked || fetch.generation != self.images.generation {
            self.images.display.insert(key, fetch);
            return true;
        }
        let data = result
            .and_then(|result| result.get("data"))
            .and_then(Value::as_str)
            .filter(|data| !data.is_empty());
        let mime = result
            .and_then(|result| result.get("mime_type"))
            .and_then(Value::as_str);
        let (Some(data), Some(mime)) = (data, mime) else {
            self.fail_display(&key, "the answer carried no file", fetch);
            return true;
        };
        if mime == "image/png" {
            self.transmit(key, fetch, data);
            return true;
        }
        let converts = matches!(
            self.screen.pages().image_sizing(),
            crate::image::Sizing::Inline { converts: true, .. }
        );
        if converts && matches!(mime, "image/jpeg" | "image/webp") {
            self.images.display.insert(
                key,
                Display {
                    state: DisplayState::Converting,
                    ..fetch
                },
            );
            self.images.convert_queue.push_back(image::Convert {
                generation: fetch.generation,
                id: fetch.id,
                data: data.to_owned(),
            });
            return true;
        }
        self.fail_display(&key, "the answer was not a PNG image", fetch);
        true
    }

    /// Queues the transmit and place of `data` over the placeholders, and
    /// marks `key` shown. When the sizing is text the ask no longer
    /// applies and a later draw asks again (`docs/tui.md`, "Images").
    fn transmit(&mut self, key: (String, String), fetch: Display, data: &str) {
        let crate::image::Sizing::Inline { cell, .. } = self.screen.pages().image_sizing() else {
            return;
        };
        let part = crate::image::Part {
            session: contract::SessionId(key.0.clone()),
            path: key.1.clone(),
            mime_type: "image/png".to_owned(),
            width: fetch.width,
            height: fetch.height,
        };
        let columns = self.screen.pages().wrap_width();
        let (cols, rows) = crate::image::grid(&part, columns, cell);
        // An empty transmit still sends its place: the writer carries the
        // empty chunk list and the place as one item.
        self.images.out.items.push(kitty_writer::Item::Image {
            id: fetch.id,
            chunks: crate::graphics::transmit(fetch.id, data),
            place: crate::graphics::place(fetch.id, cols, rows),
        });
        self.images.transmitted.insert(fetch.id, fetch.generation);
        self.images.display.insert(
            key,
            Display {
                state: DisplayState::Shown,
                ..fetch
            },
        );
    }

    /// Fails the display fetch `fetch` for `key` with one notice: the
    /// image keeps its one clickable line, is not asked for again while
    /// its session's layout lives, and the owning pages recount to the
    /// one-line form (`docs/tui.md`, "Images"). The owner is the
    /// attached screen while no transcript is swapped in, else the
    /// swapped-in transcript for its own session and the stashed screen
    /// for the attached one.
    fn fail_display(&mut self, key: &(String, String), reason: &str, fetch: Display) {
        let session = contract::SessionId(key.0.clone());
        if self.is_item_session(&session) {
            self.screen.pages_mut().fail_image(&session, &key.1);
        } else {
            self.attached_screen_mut()
                .pages_mut()
                .fail_image(&session, &key.1);
        }
        self.images.display.insert(
            key.clone(),
            Display {
                state: DisplayState::Failed,
                ..fetch
            },
        );
        let name = file_name(&key.1);
        let reason = reason.strip_suffix('.').unwrap_or(reason);
        self.notices
            .push("io_failed", format!("Could not show {name}: {reason}."));
    }

    /// A kitty error reply for `id`: the display image that transmitted
    /// under `id` in this generation fails with one notice, and is not
    /// asked for again while its session's layout lives (`docs/tui.md`,
    /// "Images"). An id nothing transmitted in this generation names an
    /// older session's image, so the error drops.
    pub(crate) fn fail_display_image(&mut self, id: u32) {
        if self.images.transmitted.get(&id) != Some(&self.images.generation) {
            return;
        }
        let failed: Vec<((String, String), Display)> = self
            .images
            .display
            .iter()
            .filter(|(_, fetch)| fetch.id == id && fetch.state != DisplayState::Failed)
            .map(|(key, fetch)| (key.clone(), (*fetch).clone()))
            .collect();
        for (key, fetch) in failed {
            self.fail_display(&key, "the terminal reported an error", fetch);
        }
    }

    /// Answers a conversion worker's completion for `id`: frees the one
    /// worker first, then transmits a converted PNG under the matching
    /// converting image or fails it with the worker's reason. A
    /// completion from an earlier session frees the worker but draws
    /// nowhere, even for the same id (`docs/tui.md`, "Images").
    pub(crate) fn image_converted(
        &mut self,
        generation: u64,
        id: u32,
        result: Result<String, String>,
    ) {
        // The one worker is free whatever session the completion belongs
        // to: a session change never clears it, so only this frees it.
        self.images.convert_busy = false;
        if generation != self.images.generation {
            return;
        }
        let found = self
            .images
            .display
            .iter()
            .find(|(_, fetch)| fetch.id == id && fetch.state == DisplayState::Converting)
            .map(|(key, _)| key.clone());
        let Some(key) = found else {
            return;
        };
        let Some(fetch) = self.images.display.remove(&key) else {
            return;
        };
        match result {
            Err(reason) => self.fail_display(&key, &reason, fetch),
            Ok(data) => {
                self.transmit(key, fetch, &data);
            }
        }
    }

    /// The next queued conversion when the one worker is free: marks the
    /// worker busy first, so the loop never starts a second child
    /// (`docs/tui.md`, "Images").
    pub(crate) fn take_next_convert(&mut self) -> Option<image::Convert> {
        if self.images.convert_busy {
            return None;
        }
        let convert = self.images.convert_queue.pop_front()?;
        self.images.convert_busy = true;
        Some(convert)
    }

    pub(in crate::app) fn image_rejected(&mut self, id: &str, message: &str) -> bool {
        let failed = self
            .images
            .display
            .iter()
            .find(|(_, fetch)| fetch.command == id)
            .map(|(key, fetch)| (key.clone(), (*fetch).clone()));
        if let Some((key, fetch)) = failed {
            self.images.display.remove(&key);
            // Only an asked image fails: a converting image's worker
            // decides, so a duplicate or late rejection changes nothing.
            if fetch.state == DisplayState::Asked {
                self.fail_display(&key, message, fetch);
            } else {
                self.images.display.insert(key, fetch);
            }
            return true;
        }
        let Some(fetch) = self.images.fetches.remove(id) else {
            return false;
        };
        let name = file_name(&fetch.path);
        self.images
            .refused
            .insert((fetch.session.clone(), fetch.path));
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
            if let Some(key) = self
                .screen
                .pages()
                .image(id)
                .map(|part| (part.session.0.clone(), part.path.clone()))
            {
                self.images.refused.insert(key);
            }
            self.cant_open(name, &reason);
        }
    }

    /// The queued viewer opens, for the loop's workers. The display
    /// commands and kitty bytes stay queued for the post-draw drain.
    pub(crate) fn take_image_views(&mut self) -> Vec<image::View> {
        std::mem::take(&mut self.images.out.view)
    }

    /// The queued session-change deletes, for the loop's pre-draw drain:
    /// shown images leave the terminal before the next session draws.
    pub(crate) fn take_image_deletes(&mut self) -> Vec<kitty_writer::Delete> {
        std::mem::take(&mut self.images.deletes)
    }

    /// Restores the display queue after handing the terminal to another
    /// program and taking it back: images the writer still holds stay shown
    /// and are sent again whole, while dropped shown images go back to
    /// asked with a fresh `read_file` each, so on-screen and off-screen
    /// images alike are re-fetched and converted without re-decoding
    /// writer-held payloads (`docs/tui.md`, "Images"). The loop drains
    /// these requests after the first post-resume frame draws.
    pub(crate) fn images_after_handover(&mut self, retained: Vec<u32>) {
        for ((session, path), fetch) in self.images.display.iter_mut() {
            if fetch.state != DisplayState::Shown {
                continue;
            }
            if retained.contains(&fetch.id) {
                continue;
            }
            let command_id = mint();
            let line = serde_json::json!({
                "id": command_id.clone(),
                "command": "read_file",
                "args": {"session": session, "path": path},
            })
            .to_string();
            self.images.out.display.push(line);
            fetch.command = command_id;
            fetch.state = DisplayState::Asked;
        }
    }

    /// The queued display commands and kitty transmit and place bytes,
    /// for the post-draw drain. Deletes never ride along: the pre-draw
    /// drain already wrote them.
    pub(crate) fn take_image_out(&mut self) -> image::Out {
        std::mem::take(&mut self.images.out)
    }

    /// Forgets the session's images on every session change: fetches
    /// and queued views go, and the generation moves, so a late
    /// answer or viewer completion is dropped. Shown images leave the
    /// terminal, and the process-lifetime id allocator gives the next
    /// generation different ids so a late error cannot affect it
    /// (`docs/tui.md`, "Images").
    pub(in crate::app) fn forget_images(&mut self) {
        let shown: Vec<u32> = self
            .images
            .display
            .values()
            .filter(|fetch| fetch.state == DisplayState::Shown)
            .map(|fetch| fetch.id)
            .collect();
        self.images.fetches.clear();
        self.images.refused.clear();
        self.images.display.clear();
        // The queued conversions go with the registry, while the running
        // child keeps the one-worker slot until its completion frees it.
        self.images.convert_queue.clear();
        self.images.out = image::Out::default();
        self.images.out.purge = true;
        self.images.deletes.clear();
        for id in shown {
            self.images.deletes.push(kitty_writer::Delete {
                id,
                bytes: crate::graphics::delete(id),
            });
        }
        self.images.generation = self.images.generation.saturating_add(1);
        self.images.id_allocator.advance_generation();
        let ids = self.images.id_allocator.clone();
        self.screen.pages_mut().set_image_ids(ids.clone());
        self.attached_screen_mut().pages_mut().set_image_ids(ids);
    }

    /// Queues the display `read_file` commands for the inline images
    /// the frame drew, once each: the second draw of a redrawn frame
    /// finds them asked and queues nothing more (`docs/tui.md`,
    /// "Images"). The one-line form needs no bytes and asks nothing, as
    /// does a failed image while its session's layout lives.
    pub(crate) fn images_drawn(&mut self, targets: &[crate::mouse::Target]) {
        let crate::image::Sizing::Inline { converts, .. } = self.screen.pages().image_sizing()
        else {
            return;
        };
        // With no session, or the link down, the loop sends nothing:
        // queueing now would mark the image asked while its command is
        // dropped, so a later draw queues once the link is up.
        if self.session().is_none() || !self.outbox.is_up() {
            return;
        }
        for target in targets {
            let crate::mouse::TargetId::Line(Target::Image(id)) = target.id else {
                continue;
            };
            // Viewer-only ids never enter the display path: no fetch
            // asks for their bytes (`docs/tui.md`, "Images").
            if !crate::image::placeholder_eligible(id) {
                continue;
            }
            let Some(part) = self.screen.pages().image(id) else {
                continue;
            };
            // JPEG and WebP fetch only while the converter is
            // available; PNG always fetches (`docs/tui.md`, "Images").
            if !crate::image::inline_type(&part.mime_type, converts) {
                continue;
            }
            let key = (part.session.0.clone(), part.path.clone());
            if self.images.display.contains_key(&key) {
                continue;
            }
            if self.screen.pages().image_failed(&part.session, &part.path) {
                continue;
            }
            let command_id = mint();
            let line = json!({
                "id": command_id,
                "command": "read_file",
                "args": {"session": part.session.0, "path": part.path},
            })
            .to_string();
            self.images.display.insert(
                key,
                Display {
                    command: command_id.clone(),
                    id,
                    generation: self.images.generation,
                    state: DisplayState::Asked,
                    width: part.width,
                    height: part.height,
                },
            );
            self.images.out.display.push(line);
        }
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
            .push("io_failed", format!("Could not open {name}: {reason}."));
    }
}

/// The file name a notice names: the logged path's last component.
fn file_name(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_owned()
}

#[cfg(test)]
#[path = "images_tests.rs"]
mod tests;
