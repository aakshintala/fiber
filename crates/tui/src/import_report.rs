//! The accepted-import report overlay (`docs/tui.md`, "Notices",
//! "Overlays"): one scrollable overlay per accepted import, no notices.
//!
//! The report names its source in the title, groups what the import
//! copied by kind in hub order, and lists what it left behind in hub
//! order with each reason dim. The frame shows a window of the body
//! rows at the scrolled-to offset, so the keys and the wheel clamp to
//! what the draw shows.

#[cfg(test)]
#[path = "import_report_tests.rs"]
mod tests;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use contract::events::{ImportedNames, LeftBehindEntry};

use crate::app::{App, Effect};
use crate::keys::Key;
use crate::mouse::{Target, TargetId};
use crate::view::overlay::{self, Place, Row};

/// The overlay's preferred content width: the question-or-notice class
/// (`docs/tui.md`, "Overlays").
pub(crate) const PREFER: u16 = 71;

/// The report's title for `source`: the harness the import copied, or
/// the generic title for a bare `/import` covering both.
pub(crate) fn title_for(source: Option<&str>) -> String {
    match source {
        Some("claude-code") => "Imported from Claude Code".to_owned(),
        Some("codex") => "Imported from Codex".to_owned(),
        _ => "Imported".to_owned(),
    }
}

/// One accepted import's report: the title naming its source, what the
/// import copied by kind in hub order, what it left behind in hub
/// order with its reasons, and the scrolled-to first body row.
#[derive(Debug, Clone, Default)]
pub(crate) struct ImportReport {
    /// The title naming the import's source.
    title: String,
    /// What the import copied, by final name in hub order.
    imported: ImportedNames,
    /// What the import did not copy, in hub order.
    left_behind: Vec<LeftBehindEntry>,
    /// The first body row the frame shows.
    top: usize,
}

impl ImportReport {
    /// The report for an accepted import from `source` (`None` for a
    /// bare `/import` covering both sources), scrolled to its first
    /// row.
    pub(crate) fn open(
        source: Option<&str>,
        imported: ImportedNames,
        left_behind: Vec<LeftBehindEntry>,
    ) -> Self {
        Self {
            title: title_for(source),
            imported,
            left_behind,
            top: 0,
        }
    }

    /// The title naming the report's source, for the tests.
    #[cfg(test)]
    pub(crate) fn title(&self) -> &str {
        &self.title
    }

    /// The copied items' count: every section count is an item count.
    pub(crate) fn copied_count(&self) -> usize {
        self.imported.mcp_servers.len()
            + self.imported.skills.len()
            + self.imported.commands.len()
            + self.imported.instructions.len()
    }

    /// The left-behind items' count.
    pub(crate) fn left_count(&self) -> usize {
        self.left_behind.len()
    }

    /// The first body row the frame shows, for the tests.
    #[cfg(test)]
    pub(crate) fn top(&self) -> usize {
        self.top
    }

    /// The copied groups in hub order: empty groups draw no header, so
    /// they are skipped where the rows build.
    fn groups(&self) -> [(&str, &[String]); 4] {
        [
            ("MCP servers", &self.imported.mcp_servers),
            ("Skills", &self.imported.skills),
            ("Commands", &self.imported.commands),
            ("Instructions", &self.imported.instructions),
        ]
    }

    /// One body row with `spans`: no right end, no targets, no bar.
    fn row(spans: Vec<Span<'static>>) -> Row {
        Row {
            spans,
            right: Vec::new(),
            targets: Vec::new(),
            barred: false,
        }
    }

    /// One blank body row, standing the sections off each other.
    fn blank() -> Row {
        Self::row(Vec::new())
    }

    /// The full body rows at `inner` columns: the copied section
    /// grouped by kind, then the left-behind section in hub order.
    /// Section headers draw even at zero counts; empty groups draw no
    /// header.
    pub(crate) fn rows(&self, inner: usize) -> Vec<Row> {
        let section = |text: String| {
            Self::row(vec![Span::styled(
                text,
                Style::new().add_modifier(Modifier::BOLD),
            )])
        };
        let mut rows = vec![section(format!("Copied · {}", self.copied_count()))];
        for (header, names) in self.groups() {
            if names.is_empty() {
                continue;
            }
            rows.push(Self::row(vec![Span::raw(header.to_owned())]));
            rows.extend(
                names
                    .iter()
                    .map(|name| Self::row(vec![Span::raw(name.clone())])),
            );
        }
        rows.push(Self::blank());
        rows.push(section(format!("Left behind · {}", self.left_count())));
        rows.extend(
            self.left_behind
                .iter()
                .flat_map(|left| left_rows(&left.item, &left.reason, inner)),
        );
        rows
    }

    /// The highest first row the frame shows at `inner` columns in
    /// `height` rows: the rows past the frame's capacity.
    pub(crate) fn max_top(&self, inner: usize, height: u16) -> usize {
        self.rows(inner)
            .len()
            .saturating_sub(overlay::question_capacity(height))
    }

    /// Scrolls by `n` rows, holding at both edges.
    pub(crate) fn scroll_by(&mut self, down: bool, n: usize, max: usize) {
        let top = self.top.min(max);
        self.top = if down {
            top.saturating_add(n).min(max)
        } else {
            top.saturating_sub(n)
        };
    }

    /// Scrolls by one page (`height` rows' worth), holding at both
    /// edges.
    pub(crate) fn scroll_page(&mut self, down: bool, height: u16, max: usize) {
        self.scroll_by(down, overlay::question_capacity(height), max);
    }

    /// The shared frame for the report at `inner` columns in `height`
    /// rows: the title with its ✕, the window of body rows at the
    /// scrolled-to offset, and the dim legend footer. The title, body
    /// and footer keep one blank row between them through the frame.
    pub(crate) fn frame(&self, inner: usize, height: u16) -> overlay::Overlay {
        let max = self.max_top(inner, height);
        let body = self
            .rows(inner)
            .into_iter()
            .skip(self.top.min(max))
            .take(overlay::question_capacity(height))
            .collect();
        overlay::Overlay {
            title: Some((self.title.clone(), Some(Span::raw("✕")))),
            close: Some(TargetId::CloseOverlay),
            body,
            footer: Some(footer()),
            prefer: PREFER,
        }
    }
}

/// The report's footer: the reload reminder with the Esc legend, each
/// key bold and its label dim, joined by " · ".
fn footer() -> Row {
    let mut footer = overlay::legend(&[("Esc", "closes")]);
    footer.spans.insert(
        0,
        Span::styled(
            "Imported items apply on the next /reload · ".to_owned(),
            Style::new().add_modifier(Modifier::DIM),
        ),
    );
    footer
}

/// One left-behind entry's rows at `inner` columns: `item: reason`
/// wrapped as one paragraph, the item in the normal colour and the
/// reason dim. The walk is the shared `overlay::head_tail_rows` one,
/// so the report and the login failure wrap alike.
fn left_rows(item: &str, reason: &str, inner: usize) -> Vec<Row> {
    let head = format!("{item}: ");
    let dim = Style::new().add_modifier(Modifier::DIM);
    let mut rows = overlay::head_tail_rows(&head, Style::new(), reason, dim, "", inner);
    if rows.is_empty() {
        rows.push(ImportReport::row(Vec::new()));
    }
    rows
}

impl App {
    /// Opens the accepted-import report, replacing any open report,
    /// and adds no notice. Opening while one is open replaces that
    /// report exactly once; closing leaves no notice behind.
    pub(crate) fn open_import_report(
        &mut self,
        source: Option<String>,
        imported: ImportedNames,
        left_behind: Vec<LeftBehindEntry>,
    ) {
        self.import_report = Some(ImportReport::open(source.as_deref(), imported, left_behind));
    }

    /// Whether the import report is open, on home or over a session.
    pub(crate) fn import_report_open(&self) -> bool {
        self.import_report.is_some()
    }

    /// Closes the import report, leaving no notice behind.
    pub(crate) fn close_import_report(&mut self) {
        self.import_report = None;
    }

    /// The open report, for the tests.
    #[cfg(test)]
    pub(crate) fn import_report(&self) -> Option<&ImportReport> {
        self.import_report.as_ref()
    }

    /// The full-screen area the report keys and the wheel clamp
    /// against, as the delete question's keys do.
    fn report_area(&self) -> Rect {
        let (width, height) = self.screen_size();
        Rect::new(0, 0, width, self.report_height(height))
    }

    /// The rows the drawn report frames against: the full screen on
    /// home, the conversation's rows over a session — the same height
    /// the frame windows against, so paging never strands rows past
    /// the bottom (`docs/tui.md`, "Layout").
    fn report_height(&self, screen: u16) -> u16 {
        if self.on_home() {
            screen
        } else {
            u16::try_from(self.conversation_height().min(usize::from(screen))).unwrap_or(screen)
        }
    }

    /// Scrolls the open report by `n` rows, holding at both edges.
    pub(crate) fn scroll_report_rows(&mut self, down: bool, n: usize) {
        let area = self.report_area();
        let inner = overlay::question_inner(area.width);
        if let Some(report) = self.import_report.as_mut() {
            let max = report.max_top(inner, area.height);
            report.scroll_by(down, n, max);
        }
    }

    /// Scrolls the open report by one page, holding at both edges.
    pub(crate) fn scroll_report_page(&mut self, down: bool) {
        let area = self.report_area();
        let inner = overlay::question_inner(area.width);
        if let Some(report) = self.import_report.as_mut() {
            let max = report.max_top(inner, area.height);
            report.scroll_page(down, area.height, max);
        }
    }

    /// A key while the report is open: Esc closes it, ↑/↓ scroll by
    /// rows and PgUp/PgDn by pages, Ctrl+C passes through to quit,
    /// and anything else is swallowed so nothing reaches the draft,
    /// the list or the conversation behind it. `None` while closed.
    pub(crate) fn report_key(&mut self, key: &Key) -> Option<Effect> {
        self.import_report.as_ref()?;
        match key {
            Key::Esc => {
                self.close_import_report();
                Some(Effect::None)
            }
            Key::Up => {
                self.scroll_report_rows(false, 1);
                Some(Effect::None)
            }
            Key::Down => {
                self.scroll_report_rows(true, 1);
                Some(Effect::None)
            }
            Key::PageUp => {
                self.scroll_report_page(false);
                Some(Effect::None)
            }
            Key::PageDown => {
                self.scroll_report_page(true);
                Some(Effect::None)
            }
            // Ctrl+C passes through to quit, as the import offer and
            // the key map do; everything else is swallowed.
            Key::CtrlC => None,
            Key::Char(_)
            | Key::Backspace
            | Key::Enter
            | Key::End
            | Key::AltA
            | Key::Tab
            | Key::BackTab
            | Key::F1
            | Key::CtrlO
            | Key::CtrlG
            | Key::CtrlR
            | Key::CtrlV
            | Key::CtrlL
            | Key::CtrlF
            | Key::AltUp
            | Key::AltDown
            | Key::AltX
            | Key::AltP
            | Key::AltR
            | Key::AltDigit(_) => Some(Effect::None),
        }
    }
}

/// Draws the open report over `area` with the shared overlay
/// frame: the title with its ✕ (`docs/tui.md`, "Overlays"), the
/// window of body rows at the scrolled-to offset, and the dim legend
/// footer. Returns the drawn slab, so the caller can swallow the
/// clicks landing on it, as the delete question does. Nothing to
/// draw leaves the targets alone.
pub(crate) fn draw_report(
    app: &App,
    area: Rect,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
) -> Rect {
    let Some(report) = app.import_report.as_ref() else {
        return Rect::new(area.x, area.y, 0, 0);
    };
    let framed = report.frame(overlay::question_inner(area.width), area.height);
    overlay::draw(buf, area, &framed, Place::Centre, targets)
}
