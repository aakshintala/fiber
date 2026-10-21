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
use crate::rows::Join;
use crate::view::overlay::{self, Place, Row};

/// The overlay's preferred content width: the question-or-notice class
/// (`docs/tui.md`, "Overlays").
pub(crate) const PREFER: u16 = 71;
/// The widest body the rows wrap at: 67 content columns at the
/// 71-column preference.
const MAX_INNER: usize = 67;

/// The body's inner width for `area_w` columns: the frame's four blank
/// columns off, kept where wrapping stays readable.
pub(crate) fn inner_width(area_w: u16) -> usize {
    usize::from(area_w.saturating_sub(4)).clamp(1, MAX_INNER)
}

/// The body rows the frame shows in `height` rows: the frame's chrome
/// off (two edges, two pads, the title and its gap, the footer and its
/// gap), at least one row.
pub(crate) fn capacity(height: u16) -> usize {
    usize::from(height.saturating_sub(8)).max(1)
}

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
        self.rows(inner).len().saturating_sub(capacity(height))
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
        self.scroll_by(down, capacity(height), max);
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
            .take(capacity(height))
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
    let dim = Style::new().add_modifier(Modifier::DIM);
    Row {
        spans: vec![
            Span::styled("Imported items apply on the next /reload".to_owned(), dim),
            Span::styled(" · ".to_owned(), dim),
            Span::styled("Esc".to_owned(), Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(" ".to_owned()),
            Span::styled("closes".to_owned(), dim),
        ],
        right: Vec::new(),
        targets: Vec::new(),
        barred: false,
    }
}

/// One left-behind entry's rows at `inner` columns: `item: reason`
/// wrapped as one paragraph, the item in the normal colour and the
/// reason dim. The wrap drops the breaking space, so each wrapped row
/// after the first starts one cell past the plain text it shows.
fn left_rows(item: &str, reason: &str, inner: usize) -> Vec<Row> {
    let head = format!("{item}: ");
    let plain = format!("{head}{reason}");
    let dim = Style::new().add_modifier(Modifier::DIM);
    let mut rows = Vec::new();
    // The plain text's offset where this wrapped row starts.
    let mut at = 0usize;
    for (line, join) in crate::format::wrap_joined(&plain, inner) {
        let start = at;
        at = at.saturating_add(line.chars().count());
        if join == Join::WrapSpace {
            at = at.saturating_add(1);
        }
        let end = start.saturating_add(line.chars().count());
        let cut = head.chars().count().clamp(start, end).saturating_sub(start);
        let at = line
            .char_indices()
            .nth(cut)
            .map(|(at, _)| at)
            .unwrap_or(line.len());
        let (normal, dimmed) = line.split_at(at);
        let mut spans = Vec::new();
        if !normal.is_empty() {
            spans.push(Span::raw(normal.to_owned()));
        }
        if !dimmed.is_empty() {
            spans.push(Span::styled(dimmed.to_owned(), dim));
        }
        rows.push(ImportReport::row(spans));
    }
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
        let inner = inner_width(area.width);
        if let Some(report) = self.import_report.as_mut() {
            let max = report.max_top(inner, area.height);
            report.scroll_by(down, n, max);
        }
    }

    /// Scrolls the open report by one page, holding at both edges.
    pub(crate) fn scroll_report_page(&mut self, down: bool) {
        let area = self.report_area();
        let inner = inner_width(area.width);
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
    let framed = report.frame(inner_width(area.width), area.height);
    overlay::draw(buf, area, &framed, Place::Centre, targets)
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    /// A report with every kind copied and two items left behind.
    fn full() -> ImportReport {
        ImportReport::open(
            Some("codex"),
            ImportedNames {
                mcp_servers: vec!["zeta".to_owned(), "alpha".to_owned()],
                skills: vec!["lint".to_owned()],
                commands: vec!["plan".to_owned()],
                instructions: vec!["AGENTS.md".to_owned()],
            },
            vec![
                LeftBehindEntry {
                    item: "auth.json".to_owned(),
                    reason: "credentials are never imported".to_owned(),
                },
                LeftBehindEntry {
                    item: "other.json".to_owned(),
                    reason: "unknown".to_owned(),
                },
            ],
        )
    }

    #[test]
    fn the_title_names_the_source() {
        assert_eq!(title_for(Some("claude-code")), "Imported from Claude Code");
        assert_eq!(title_for(Some("codex")), "Imported from Codex");
    }

    #[test]
    fn a_bare_import_reports_the_generic_title() {
        assert_eq!(title_for(None), "Imported");
        assert_eq!(title_for(Some("prose")), "Imported");
    }

    /// The row texts, one per body row: styles assert separately.
    fn texts(report: &ImportReport, inner: usize) -> Vec<String> {
        report
            .rows(inner)
            .iter()
            .map(|row| {
                row.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect()
    }

    #[test]
    fn sections_group_the_copied_kinds_in_hub_order() {
        let texts = texts(&full(), 67);
        assert_eq!(
            texts,
            [
                "Copied · 5",
                "MCP servers",
                "zeta",
                "alpha",
                "Skills",
                "lint",
                "Commands",
                "plan",
                "Instructions",
                "AGENTS.md",
                "",
                "Left behind · 2",
                "auth.json: credentials are never imported",
                "other.json: unknown",
            ]
        );
    }

    #[test]
    fn empty_groups_draw_no_header_but_kept_groups_keep_hub_order() {
        let report = ImportReport::open(
            Some("claude-code"),
            ImportedNames {
                skills: vec!["lint".to_owned()],
                ..ImportedNames::default()
            },
            Vec::new(),
        );
        assert_eq!(
            texts(&report, 67),
            ["Copied · 1", "Skills", "lint", "", "Left behind · 0"]
        );
    }

    #[test]
    fn an_empty_report_draws_both_sections_with_zero_counts() {
        let report = ImportReport::open(None, ImportedNames::default(), Vec::new());
        assert_eq!(report.title(), "Imported");
        assert_eq!(texts(&report, 67), ["Copied · 0", "", "Left behind · 0"]);
    }

    #[test]
    fn section_counts_are_item_counts() {
        let report = full();
        assert_eq!(report.copied_count(), 5);
        assert_eq!(report.left_count(), 2);
    }

    #[test]
    fn a_left_behind_row_carries_its_reason_dim() {
        let rows = full().rows(67);
        let row = &rows[12];
        assert_eq!(row.spans.len(), 2);
        assert_eq!(row.spans[0].content, "auth.json: ");
        assert_eq!(row.spans[0].style, Style::new());
        assert_eq!(row.spans[1].content, "credentials are never imported");
        assert_eq!(row.spans[1].style, Style::new().add_modifier(Modifier::DIM));
    }

    #[test]
    fn a_wrapped_left_behind_row_keeps_the_reason_dim_past_the_break() {
        let report = ImportReport::open(
            None,
            ImportedNames::default(),
            vec![LeftBehindEntry {
                item: "auth.json".to_owned(),
                reason: "credentials are never imported anywhere".to_owned(),
            }],
        );
        let rows = report.rows(30);
        let plain: Vec<String> = rows
            .iter()
            .skip(3)
            .map(|row| {
                row.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect();
        assert_eq!(
            plain,
            ["auth.json: credentials are", "never imported anywhere",]
        );
        // The break falls inside the reason, so the first row's tail
        // and the whole second row read dim.
        assert_eq!(rows[3].spans.len(), 2);
        assert_eq!(
            rows[3].spans[1].style,
            Style::new().add_modifier(Modifier::DIM)
        );
        assert_eq!(rows[4].spans.len(), 1);
        assert_eq!(
            rows[4].spans[0].style,
            Style::new().add_modifier(Modifier::DIM)
        );
    }

    #[test]
    fn scrolling_by_rows_holds_at_both_edges() {
        let report = full();
        // Fourteen body rows in a two-row frame scroll twelve.
        let max = report.rows(67).len().saturating_sub(capacity(10));
        assert_eq!(max, 12);
        let mut scrolled = report.clone();
        scrolled.scroll_by(true, 1, max);
        assert_eq!(scrolled.top(), 1);
        scrolled.scroll_by(false, 1, max);
        assert_eq!(scrolled.top(), 0);
        // Up at the top holds.
        scrolled.scroll_by(false, 5, max);
        assert_eq!(scrolled.top(), 0);
        // Down past the end holds at the last row the frame shows.
        scrolled.scroll_by(true, max.saturating_add(10), max);
        assert_eq!(scrolled.top(), max);
        scrolled.scroll_by(true, 1, max);
        assert_eq!(scrolled.top(), max);
        // One step back from the end moves again.
        scrolled.scroll_by(false, 1, max);
        assert_eq!(scrolled.top(), max.saturating_sub(1));
    }

    #[test]
    fn scrolling_clamps_a_stale_offset_before_it_moves() {
        let mut report = full();
        let max = report.rows(67).len().saturating_sub(capacity(10));
        report.top = max.saturating_add(4);
        report.scroll_by(true, 1, max);
        assert_eq!(report.top(), max);
        report.top = max.saturating_add(4);
        report.scroll_by(false, 1, max);
        assert_eq!(report.top(), max.saturating_sub(1));
    }

    #[test]
    fn paging_moves_a_page_and_holds_at_the_edges() {
        let report = full();
        // Fourteen body rows in a two-row frame: a page is two rows.
        let max = report.rows(67).len().saturating_sub(capacity(10));
        let page = capacity(10);
        assert_eq!((max, page), (12, 2));
        let mut scrolled = report.clone();
        scrolled.scroll_page(true, 10, max);
        assert_eq!(scrolled.top(), page.min(max));
        scrolled.scroll_page(true, 10, max);
        scrolled.scroll_page(true, 10, max);
        scrolled.scroll_page(true, 10, max);
        scrolled.scroll_page(true, 10, max);
        scrolled.scroll_page(true, 10, max);
        assert_eq!(scrolled.top(), max);
        scrolled.scroll_page(true, 10, max);
        assert_eq!(scrolled.top(), max);
        scrolled.scroll_page(false, 10, max);
        assert_eq!(scrolled.top(), max.saturating_sub(page));
        let mut top = report.clone();
        top.scroll_page(false, 10, max);
        assert_eq!(top.top(), 0);
    }

    #[test]
    fn the_frame_windows_the_body_at_the_scrolled_offset() {
        let mut report = full();
        let inner = 67;
        let height = 12;
        let cap = capacity(height);
        let all = texts(&report, inner);
        assert!(all.len() > cap);
        let framed = report.frame(inner, height);
        assert_eq!(
            framed.title.as_ref().map(|title| title.0.clone()),
            Some("Imported from Codex".to_owned())
        );
        assert_eq!(framed.prefer, PREFER);
        assert!(framed.footer.is_some());
        let shown: Vec<String> = framed.body.iter().map(text_of).collect();
        assert_eq!(shown, all[..cap]);
        // Scrolled to the bottom the frame shows the last rows.
        let max = report.max_top(inner, height);
        report.top = max;
        let scrolled = report.frame(inner, height);
        let shown: Vec<String> = scrolled.body.iter().map(text_of).collect();
        assert_eq!(shown, all[max..]);
        assert!(shown.contains(&"other.json: unknown".to_owned()));
    }

    /// One body row's text.
    fn text_of(row: &Row) -> String {
        row.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<Vec<_>>()
            .join("")
    }
}
