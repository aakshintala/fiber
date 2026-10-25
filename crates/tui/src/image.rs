//! One image part's line: its label, its click target and the registry
//! of ids paging hands out (`docs/tui.md`, "Images"). An image is one
//! clickable line; inline rows arrive with kitty's graphics protocol.

use std::collections::{BTreeMap, BTreeSet};
#[cfg(not(test))]
use std::sync::OnceLock;
use std::sync::{Arc, Mutex, MutexGuard};

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
    /// Its type, such as `image/png`: only PNG draws inline
    /// (`docs/tui.md`, "Images").
    pub(crate) mime_type: String,
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
                mime_type,
                width,
                height,
            } => Some(Part {
                session: session.clone(),
                path: path.clone(),
                mime_type: mime_type.clone(),
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
    /// The placeholder grid with this cell size in pixels, then the
    /// caption line (`docs/tui.md`, "Images").
    Inline { cell: (u16, u16) },
}

/// The placeholder grid's caps: at most 40% of the conversation's width
/// and 12 rows, keeping the image's proportions (`docs/tui.md`,
/// "Images"). A placeholder names its cell through the diacritics after
/// it, so neither side runs past the diacritic table.
const MAX_ROWS: u16 = 12;
const MAX_DIACRITIC: u64 = 64;

/// The placeholder grid for `part` in a `columns`-wide conversation
/// with `cell` pixels per cell: the logged size in pixels, fit into the
/// caps without decoding the image (`docs/tui.md`, "Images"). No upscale
/// past the image's natural cells. Zero logged dimensions or no columns
/// draw no grid: the caption line still counts the image's row. The
/// products run in 64 bits: the logged size is unbounded, so 32-bit
/// cells times 32-bit pixels overflow.
pub(crate) fn grid(part: &Part, columns: u16, cell: (u16, u16)) -> (u16, u16) {
    if part.width == 0 || part.height == 0 || columns == 0 {
        return (0, 0);
    }
    let (cell_w, cell_h) = (u64::from(cell.0), u64::from(cell.1));
    if cell_w == 0 || cell_h == 0 {
        return (0, 0);
    }
    // The width cap in cells, inside the diacritic table.
    let max_cols = (u64::from(columns) * 40 / 100).clamp(1, MAX_DIACRITIC);
    let (width, height) = (u64::from(part.width), u64::from(part.height));
    // Natural cells, at least one: a small image draws as it is.
    if width <= max_cols * cell_w && height <= u64::from(MAX_ROWS) * cell_h {
        let cols = (width / cell_w).max(1);
        let rows = (height / cell_h).max(1);
        return (
            u16::try_from(cols.min(MAX_DIACRITIC)).unwrap_or(u16::MAX),
            u16::try_from(rows.min(u64::from(MAX_ROWS))).unwrap_or(u16::MAX),
        );
    }
    // The width binds when the width-capped box is narrower, by aspect,
    // than the row-capped box.
    if max_cols * cell_w * height <= u64::from(MAX_ROWS) * cell_h * width {
        let rows = (height * max_cols * cell_w / (width * cell_h)).max(1);
        (
            u16::try_from(max_cols).unwrap_or(u16::MAX),
            u16::try_from(rows.min(u64::from(MAX_ROWS))).unwrap_or(u16::MAX),
        )
    } else {
        let cols = (width * u64::from(MAX_ROWS) * cell_h / (height * cell_w)).max(1);
        (
            u16::try_from(cols.min(max_cols.min(MAX_DIACRITIC))).unwrap_or(u16::MAX),
            MAX_ROWS,
        )
    }
}

/// Image ids shared by every page layout during the TUI's lifetime.
/// One `(generation, session, path)` keeps its id across refolds, while
/// a new generation or image gets a never-reused id. The terminal's
/// placeholder colour carries one byte, so ids stop at 255.
#[derive(Clone, Debug)]
pub(crate) struct IdAllocator(Arc<Mutex<IdState>>);

#[derive(Debug, Default)]
struct IdState {
    generation: u64,
    next: u32,
    ids: BTreeMap<(u64, SessionId, String), u32>,
}

impl Default for IdAllocator {
    fn default() -> Self {
        #[cfg(test)]
        {
            Self::new()
        }
        #[cfg(not(test))]
        {
            static PROCESS: OnceLock<IdAllocator> = OnceLock::new();
            PROCESS.get_or_init(Self::new).clone()
        }
    }
}

impl IdAllocator {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(IdState::default())))
    }

    /// Uses a fresh identity scope after a session attachment.
    pub(crate) fn advance_generation(&self) {
        let mut state = self.lock();
        state.generation = state.generation.saturating_add(1);
    }

    /// The current identity scope.
    fn generation(&self) -> u64 {
        self.lock().generation
    }

    /// Returns the id for this image in the current scope, allocating
    /// the next unused placeholder colour or 0 after exhaustion.
    fn id(&self, part: &Part) -> u32 {
        let mut state = self.lock();
        let key = (state.generation, part.session.clone(), part.path.clone());
        if let Some(id) = state.ids.get(&key) {
            return *id;
        }
        let mut id = state.next.saturating_add(1);
        if id == crate::term::PROBE_ID {
            id = id.saturating_add(1);
        }
        if id > u32::from(u8::MAX) {
            return 0;
        }
        state.next = id;
        state.ids.insert(key, id);
        id
    }

    /// A poisoned lock still holds allocated ids, so keep them monotonic.
    fn lock(&self) -> MutexGuard<'_, IdState> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// The chain's image registry: one id per `(session, path)`, in the
/// order first seen (`docs/tui.md`, "Images"). Two paths never share
/// an id, and neither does one path in two segments, so clicks name
/// one image.
#[derive(Clone, Debug, Default)]
pub(crate) struct Layout {
    pub(crate) ids: BTreeMap<(SessionId, String), u32>,
    pub(crate) parts: BTreeMap<u32, Part>,
    allocator: IdAllocator,
    generation: u64,
    pub(crate) sizing: Sizing,
    /// The images whose display failed, by `(session, path)`: each keeps
    /// its one clickable line, and no fetch asks for it again while this
    /// layout lives (`docs/tui.md`, "Images").
    failed: BTreeSet<(SessionId, String)>,
}

impl Layout {
    /// Shares the TUI's process-lifetime id source with this layout.
    pub(crate) fn set_allocator(&mut self, allocator: IdAllocator) {
        self.allocator = allocator;
        self.sync_generation();
    }

    /// Assigns fresh ids when the attached session generation changes.
    fn sync_generation(&mut self) {
        let generation = self.allocator.generation();
        if generation == self.generation {
            return;
        }
        let parts = std::mem::take(&mut self.parts);
        self.ids.clear();
        self.generation = generation;
        for part in parts.into_values() {
            self.note(&part);
        }
    }

    /// The id `path` drew under in `session`, when it has drawn.
    pub(crate) fn id(&self, session: &SessionId, path: &str) -> Option<u32> {
        self.ids.get(&(session.clone(), path.to_owned())).copied()
    }

    /// Notes `part`, handing its `(session, path)` the next id when
    /// unseen; a pair already seen keeps its id, so a page dropped and
    /// folded again draws the same id. Id 31 is never handed out: it is
    /// the graphics probe's (`docs/tui.md`, "Images"). Ids stop at 255:
    /// the placeholder colour carries only one byte, so past 255 images
    /// a part stays unnoted and draws its one line, and 0 names no
    /// image.
    pub(crate) fn note(&mut self, part: &Part) -> u32 {
        self.sync_generation();
        let key = (part.session.clone(), part.path.clone());
        if let Some(id) = self.ids.get(&key) {
            return *id;
        }
        let id = self.allocator.id(part);
        if id == 0 {
            return 0;
        }
        self.ids.insert(key, id);
        self.parts.insert(id, part.clone());
        id
    }

    /// Marks the image at `path` in `session` failed: it draws its one
    /// line from now on, and no fetch asks for it again while this
    /// layout lives (`docs/tui.md`, "Images").
    pub(crate) fn fail(&mut self, session: &SessionId, path: &str) {
        self.failed.insert((session.clone(), path.to_owned()));
    }

    /// Whether the image at `path` in `session` failed: one line, no
    /// refetch (`docs/tui.md`, "Images").
    pub(crate) fn failed(&self, session: &SessionId, path: &str) -> bool {
        self.failed.contains(&(session.clone(), path.to_owned()))
    }
}

/// Where an image's line sits.
#[derive(Clone, Copy)]
pub(crate) enum Align {
    /// At the ledger's indent, under a call's row.
    Left { indent: u16 },
    /// Right-aligned with the prompt bubble's text.
    Right,
}

/// Draws `part` as its one line, cut to the columns left after its
/// indent so it is always one row, carrying its click target. The line
/// reads `▣ <name>` in the link look and ` · <dims>` dim
/// (`docs/tui.md`, "Images", "Links"). The prompt bubble and steering
/// images always draw through here, never inline.
pub(crate) fn rows(part: &Part, layout: &Layout, columns: u16, align: Align, out: &mut Rows) {
    text_rows(part, layout, columns, align, out);
}

/// Draws `part` under a call's ledger row: its one line when the sizing
/// is text, the flat seam shows, or its display failed; otherwise the
/// placeholder grid with its caption for a noted PNG, and the one line
/// for anything else (`docs/tui.md`, "Images").
pub(crate) fn ledger_rows(
    part: &Part,
    layout: &Layout,
    columns: u16,
    align: Align,
    out: &mut Rows,
) {
    let Sizing::Inline { cell } = layout.sizing else {
        text_rows(part, layout, columns, align, out);
        return;
    };
    // A failed image draws its one clickable line, and a flat page draws
    // every image as its one line: neither fetches (`docs/tui.md`,
    // "Images", "Screen readers").
    if layout.failed(&part.session, &part.path) || out.seam.as_ref().is_some_and(|seam| seam.flat) {
        text_rows(part, layout, columns, align, out);
        return;
    }
    match align {
        // Only a noted PNG under a call's ledger row draws inline.
        Align::Left { indent } if indent > 0 && part.mime_type == "image/png" => {
            if let Some(id) = layout.id(&part.session, &part.path) {
                inline_rows(part, layout, id, columns, indent, cell, out);
            } else {
                text_rows(part, layout, columns, align, out);
            }
        }
        Align::Left { .. } | Align::Right => text_rows(part, layout, columns, align, out),
    }
}

/// Draws `part`'s placeholder grid with `id`, then its caption line: the
/// grid moves with the text through kitty's Unicode placeholders, and a
/// click on either opens the viewer (`docs/tui.md`, "Images").
fn inline_rows(
    part: &Part,
    layout: &Layout,
    id: u32,
    columns: u16,
    indent: u16,
    cell: (u16, u16),
    out: &mut Rows,
) {
    let (cols, rows) = grid(part, columns, cell);
    let target = Some(Target::Image(id));
    for row in 0..rows {
        let mut spans = Vec::with_capacity(usize::from(cols).saturating_add(1));
        if indent > 0 {
            spans.push(Span::raw(" ".repeat(usize::from(indent))));
        }
        for col in 0..cols {
            spans.push(crate::graphics::cell(
                usize::from(row),
                usize::from(col),
                crate::look::placeholder_style(id),
            ));
        }
        out.push((Line::from(spans), target));
    }
    // The caption reads as the one-line form, so the line stays the same
    // with inline off.
    text_rows(part, layout, columns, Align::Left { indent }, out);
}

/// Draws `part` as its one line, cut to the columns left after its
/// indent so it is always one row, carrying its click target.
fn text_rows(part: &Part, layout: &Layout, columns: u16, align: Align, out: &mut Rows) {
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
    /// Display `read_file` command lines the loop sends for the inline
    /// images the frame drew (`docs/tui.md`, "Images").
    pub(crate) display: Vec<String>,
    /// Kitty transmit and place bytes the loop writes after the screen
    /// draw: each answer's transmit and place contiguously, never
    /// deletes (`docs/tui.md`, "Images").
    pub(crate) kitty: Vec<u8>,
}

#[cfg(test)]
#[path = "image_tests.rs"]
mod tests;
