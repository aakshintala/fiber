//! One image part's line: its label, its click target and the registry
//! of ids paging hands out (`docs/tui.md`, "Images"). An image is one
//! clickable line; inline rows arrive with kitty's graphics protocol.

use std::collections::BTreeMap;

use contract::shapes::ContentPart;

use crate::app::Target;
use crate::format;
use crate::rows::Rows;

/// One image part of the log: its file in the session's `artifacts/`
/// and its size in pixels, so paging counts its rows without decoding
/// it (`docs/tui.md`, "Images"). The log never holds its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Part {
    pub(crate) path: String,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

impl Part {
    /// The image part of `part`; `None` for any other part. A PDF part
    /// is not an image part: it changes nothing here.
    pub(crate) fn from_content(part: &ContentPart) -> Option<Part> {
        match part {
            ContentPart::Image {
                path,
                width,
                height,
                ..
            } => Some(Part {
                path: path.clone(),
                width: *width,
                height: *height,
            }),
            ContentPart::Text { .. } | ContentPart::Pdf(_) | ContentPart::Unknown => None,
        }
    }

    /// The path's last `/` component: what the line names and the
    /// viewer writes.
    pub(crate) fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    /// The whole line, uncut: `y` and Ctrl+G copy it whole.
    pub(crate) fn label(&self) -> String {
        format!("▣ {} · {}×{}", self.name(), self.width, self.height)
    }
}

/// Every image part of `content`, in order.
pub(crate) fn parts(content: &[ContentPart]) -> Vec<Part> {
    content.iter().filter_map(Part::from_content).collect()
}

/// How an image sizes: text, its one line, until kitty's graphics
/// protocol is known.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum Sizing {
    #[default]
    Text,
}

/// The session's image registry: one id per path, in the order first
/// seen, for the whole session (`docs/tui.md`, "Images"). Two paths
/// never share an id, so clicks name one image.
#[derive(Clone, Debug, Default)]
pub(crate) struct Layout {
    pub(crate) ids: BTreeMap<String, u32>,
    pub(crate) parts: BTreeMap<u32, Part>,
    next: u32,
    pub(crate) sizing: Sizing,
}

impl Layout {
    /// The id `path` drew under, when it has drawn.
    pub(crate) fn id(&self, path: &str) -> Option<u32> {
        self.ids.get(path).copied()
    }

    /// Notes `part`, handing its path the next id when unseen; a path
    /// already seen keeps its id, so a page dropped and folded again
    /// draws the same id.
    pub(crate) fn note(&mut self, part: &Part) -> u32 {
        if let Some(id) = self.ids.get(&part.path) {
            return *id;
        }
        self.next = self.next.saturating_add(1);
        let id = self.next;
        self.ids.insert(part.path.clone(), id);
        self.parts.insert(id, part.clone());
        id
    }
}

/// Where an image's line sits.
pub(crate) enum Align {
    /// At the ledger's indent, under a call's row.
    Left { indent: u16 },
    /// Right-aligned with the prompt bubble's text.
    Right,
}

/// Draws `part` as its one line, cut to the columns left after its
/// indent so it is always one row, carrying its click target.
pub(crate) fn rows(part: &Part, layout: &Layout, columns: u16, align: Align, out: &mut Rows) {
    // Text sizing only: the line. Inline rows arrive with kitty's
    // graphics protocol.
    let Sizing::Text = layout.sizing;
    let target = layout.id(&part.path).map(Target::Image);
    match align {
        Align::Left { indent } => {
            let max = usize::from(columns).saturating_sub(usize::from(indent));
            let pad = " ".repeat(usize::from(indent));
            out.push((
                format::dim(format!("{pad}{}", cut(&part.label(), max))),
                target,
            ));
        }
        Align::Right => {
            let line = format::dim(cut(&part.label(), usize::from(columns)));
            out.push((line.right_aligned(), target));
        }
    }
}

/// `label` cut to at most `max` columns, ending in `…` when cut, so the
/// drawn line is always one row.
fn cut(label: &str, max: usize) -> String {
    if format::width(label) <= max {
        return label.to_owned();
    }
    if max == 0 {
        return String::new();
    }
    format!("{}…", format::cut(label, max - 1))
}

/// One image the viewer opens: its id, its file name, the session it
/// came from, its bytes in base64, and the session generation that
/// asked, so a stale completion after a session change is dropped.
pub(crate) struct View {
    pub(crate) id: u32,
    pub(crate) name: String,
    pub(crate) session: String,
    pub(crate) data: String,
    pub(crate) generation: u64,
}

/// What the app queues for the loop: the terminal never writes to the
/// tty itself.
#[derive(Default)]
pub(crate) struct Out {
    pub(crate) view: Vec<View>,
}

#[cfg(test)]
#[path = "image_tests.rs"]
mod tests;
