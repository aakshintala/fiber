//! One image part's line: its label, its click target and the registry
//! of ids paging hands out (`docs/tui.md`, "Images"). An image is one
//! clickable line; inline rows arrive with kitty's graphics protocol.

use std::collections::BTreeMap;

use contract::SessionId;
use contract::shapes::ContentPart;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::Target;
use crate::rows::Rows;
use crate::theme::Role;

/// One image part of the log: its file in the session's `artifacts/`
/// and its size in pixels, so paging counts its rows without decoding
/// it (`docs/tui.md`, "Images"). The log never holds its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Part {
    /// The segment whose log carried it: the same path in two segments
    /// draws two images, resolved under different `artifacts/` dirs.
    pub(crate) session: SessionId,
    pub(crate) path: String,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

impl Part {
    /// The image part of `part` in `session`; `None` for any other
    /// part. A PDF part is not an image part: it changes nothing here.
    pub(crate) fn from_content(session: &SessionId, part: &ContentPart) -> Option<Part> {
        match part {
            ContentPart::Image {
                path,
                width,
                height,
                ..
            } => Some(Part {
                session: session.clone(),
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

/// Every image part of `session`'s `content`, in order.
pub(crate) fn parts(session: &SessionId, content: &[ContentPart]) -> Vec<Part> {
    content
        .iter()
        .filter_map(|part| Part::from_content(session, part))
        .collect()
}

/// How an image sizes: text, its one line, until kitty's graphics
/// protocol is known.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum Sizing {
    #[default]
    Text,
}

/// The chain's image registry: one id per `(session, path)`, in the
/// order first seen (`docs/tui.md`, "Images"). Two paths never share
/// an id, and neither does one path in two segments, so clicks name
/// one image.
#[derive(Clone, Debug, Default)]
pub(crate) struct Layout {
    pub(crate) ids: BTreeMap<(SessionId, String), u32>,
    pub(crate) parts: BTreeMap<u32, Part>,
    next: u32,
    pub(crate) sizing: Sizing,
}

impl Layout {
    /// The id `path` drew under in `session`, when it has drawn.
    pub(crate) fn id(&self, session: &SessionId, path: &str) -> Option<u32> {
        self.ids.get(&(session.clone(), path.to_owned())).copied()
    }

    /// Notes `part`, handing its `(session, path)` the next id when
    /// unseen; a pair already seen keeps its id, so a page dropped and
    /// folded again draws the same id.
    pub(crate) fn note(&mut self, part: &Part) -> u32 {
        let key = (part.session.clone(), part.path.clone());
        if let Some(id) = self.ids.get(&key) {
            return *id;
        }
        self.next = self.next.saturating_add(1);
        let id = self.next;
        self.ids.insert(key, id);
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
/// indent so it is always one row, carrying its click target. The line
/// reads `▣ <name>` in the link look and ` · <dims>` dim
/// (`docs/tui.md`, "Images", "Links").
pub(crate) fn rows(part: &Part, layout: &Layout, columns: u16, align: Align, out: &mut Rows) {
    // Text sizing only: the line. Inline rows arrive with kitty's
    // graphics protocol.
    let Sizing::Text = layout.sizing;
    let target = layout.id(&part.session, &part.path).map(Target::Image);
    let name = format!("▣ {}", part.name());
    match align {
        Align::Left { indent } => {
            let max = usize::from(columns).saturating_sub(usize::from(indent));
            let mut spans = Vec::new();
            if indent > 0 {
                spans.push(Span::raw(" ".repeat(usize::from(indent))));
            }
            spans.extend(spans_of(&crate::text::cut(&part.label(), max, "…"), &name));
            out.push((Line::from(spans), target));
        }
        Align::Right => {
            let line = Line::from(spans_of(
                &crate::text::cut(&part.label(), usize::from(columns), "…"),
                &name,
            ));
            out.push((line.right_aligned(), target));
        }
    }
}

/// The label's spans in the link look: `▣ <name>` in the accent colour,
/// underlined, and ` · <dims>` dim. A label cut inside its dims keeps
/// the split; one cut inside its name draws wholly as the link, since
/// no dims show.
fn spans_of(label: &str, name: &str) -> Vec<Span<'static>> {
    let link = Style::new()
        .fg(Role::Accent.color())
        .add_modifier(Modifier::UNDERLINED);
    if let Some(rest) = label.strip_prefix(name) {
        vec![
            Span::styled(name.to_owned(), link),
            Span::styled(
                rest.to_owned(),
                Style::default().add_modifier(Modifier::DIM),
            ),
        ]
    } else {
        vec![Span::styled(label.to_owned(), link)]
    }
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
