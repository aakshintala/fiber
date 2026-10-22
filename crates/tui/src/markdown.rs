//! A reply's markdown as styled lines (`docs/tui.md`, "Look"): CommonMark
//! with GFM tables and strikethrough. Every line the renderer returns fits
//! the width it was given, so each draws as exactly one row.

mod reply;
mod table;
mod text;
mod wrap;

use std::ops::Range;

pub(crate) use reply::Reply;

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

pub(crate) use crate::theme::Role;

use crate::highlight;
use crate::rows::{Join, RowText};
use crate::surface::{self, edge_row};
use wrap::{Cell, link_cols, spans_of, truncate, wrap, wrap_joined_indices};

/// A reply rendered at one width.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Rendered {
    /// One row each.
    pub(crate) lines: Vec<Line<'static>>,
    /// Each code block's `copy` target.
    pub(crate) targets: Vec<CopyTarget>,
    /// What each line adds to its logical line, one per line.
    pub(crate) text: Vec<RowText>,
}

/// A code block's click-to-copy target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CopyTarget {
    /// The block's header line, an index into [`Rendered::lines`].
    pub(crate) line: usize,
    /// The `copy` label's cells on that line.
    pub(crate) cols: Range<u16>,
    /// The block's code as fenced, its trailing newline trimmed.
    pub(crate) code: String,
}

impl Rendered {
    /// The `block`th code block's copy target.
    pub(crate) fn target(&self, block: usize) -> Option<CopyTarget> {
        self.targets.get(block).cloned()
    }
}

/// The copy target's label.
const COPY: &str = "click to copy";
/// Cells per tab stop in code.
const TAB: usize = 4;

/// The foreground of `role`.
pub(crate) fn style(role: Role) -> Style {
    Style::new().fg(role.color())
}

/// The foreground of `role` on the code tint.
fn tinted(role: Role) -> Style {
    style(role).bg(Role::Code.color())
}

/// Renders `text` at `width` columns. Pure: the same input gives the same
/// output. An unclosed fence is a code block to the end of the text.
pub(crate) fn render(text: &str, width: u16) -> Rendered {
    let mut writer = Writer {
        width,
        out: Rendered::default(),
        inline: Vec::new(),
        link_stack: Vec::new(),
        inline_links: Vec::new(),
        heading: false,
        strong: 0,
        emphasis: 0,
        strike: 0,
        link: 0,
        quote: 0,
        lists: Vec::new(),
        marker: None,
        code: None,
        table: None,
        separator: None,
        track: text::Track::default(),
    };
    let options = Options::ENABLE_TABLES.union(Options::ENABLE_STRIKETHROUGH);
    for event in Parser::new_ext(text, options) {
        writer.event(event);
    }
    let mut out = writer.out;
    out.text = writer.track.finish();
    out
}

/// One open list: its next number, when ordered, and its item's marker
/// width.
struct List {
    next: Option<u64>,
    marker: usize,
}

/// The renderer's state while it reads events.
struct Writer {
    width: u16,
    out: Rendered,
    /// The block of inline text being gathered.
    inline: Vec<Cell>,
    /// Open links: each one's start in `inline`, or nothing when its
    /// destination does not open (`docs/tui.md`, "Links").
    link_stack: Vec<Option<(usize, String)>>,
    /// The valid links closed in the gathered inline text, as indices
    /// into it.
    inline_links: Vec<(Range<usize>, String)>,
    heading: bool,
    strong: usize,
    emphasis: usize,
    strike: usize,
    link: usize,
    quote: usize,
    lists: Vec<List>,
    /// An item's marker, waiting for its first row.
    marker: Option<String>,
    /// An open code block: its info string and its code so far.
    code: Option<(String, String)>,
    table: Option<table::Table>,
    /// The line count when a separator was last added.
    separator: Option<usize>,
    /// What each line adds to its logical line.
    track: text::Track,
}

impl Writer {
    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => match &mut self.code {
                Some((_, code)) => code.push_str(&text),
                None => self.push(&text, self.style()),
            },
            Event::Code(text) => self.push(&text, style(Role::Info)),
            Event::Html(text)
            | Event::InlineHtml(text)
            | Event::InlineMath(text)
            | Event::DisplayMath(text)
            | Event::FootnoteReference(text) => self.push(&text, self.style()),
            Event::SoftBreak => self.push(" ", self.style()),
            Event::HardBreak => self.push("\n", self.style()),
            Event::Rule => {
                self.flush();
                self.block();
                let rule = "─".repeat(usize::from(self.inner_width()));
                self.put(Line::from(Span::styled(rule, style(Role::Muted))), false);
            }
            Event::TaskListMarker(_) => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph | Tag::HtmlBlock => self.block(),
            Tag::Heading { .. } => {
                self.block();
                self.heading = true;
            }
            Tag::BlockQuote(_) => {
                self.flush();
                self.block();
                self.quote = self.quote.saturating_add(1);
            }
            Tag::CodeBlock(kind) => {
                self.flush();
                self.block();
                let info = match kind {
                    CodeBlockKind::Fenced(info) => info.to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                self.code = Some((info, String::new()));
            }
            Tag::List(start) => {
                self.flush();
                if self.lists.is_empty() {
                    self.block();
                }
                self.lists.push(List {
                    next: start,
                    marker: 0,
                });
            }
            Tag::Item => {
                if let Some(list) = self.lists.last_mut() {
                    let marker = match &mut list.next {
                        Some(next) => {
                            let marker = format!("{next}. ");
                            *next = next.saturating_add(1);
                            marker
                        }
                        None => "• ".to_owned(),
                    };
                    list.marker = marker.chars().count();
                    self.marker = Some(marker);
                }
            }
            Tag::Table(aligns) => {
                self.flush();
                self.block();
                self.table = Some(table::Table::new(aligns));
            }
            Tag::Emphasis => self.emphasis = self.emphasis.saturating_add(1),
            Tag::Strong => self.strong = self.strong.saturating_add(1),
            Tag::Strikethrough => self.strike = self.strike.saturating_add(1),
            Tag::Link { dest_url, .. } => {
                let dest = dest_url.into_string();
                let open = crate::opener::valid(&dest).then_some((self.inline.len(), dest));
                self.link_stack.push(open);
                self.link = self.link.saturating_add(1);
            }
            Tag::TableHead
            | Tag::TableRow
            | Tag::TableCell
            | Tag::Image { .. }
            | Tag::FootnoteDefinition(_)
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
            | Tag::Superscript
            | Tag::Subscript
            | Tag::MetadataBlock(_) => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::HtmlBlock | TagEnd::Item => self.flush(),
            TagEnd::Heading(_) => {
                self.flush();
                self.heading = false;
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.quote = self.quote.saturating_sub(1);
            }
            TagEnd::CodeBlock => {
                if let Some((info, code)) = self.code.take() {
                    self.code_block(&info, &code);
                }
            }
            TagEnd::List(_) => {
                self.flush();
                self.lists.pop();
            }
            TagEnd::TableCell => {
                let cell = std::mem::take(&mut self.inline);
                let links = std::mem::take(&mut self.inline_links);
                if let Some(table) = &mut self.table {
                    table.push_cell(cell, links);
                }
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                if let Some(table) = &mut self.table {
                    table.end_row();
                }
            }
            TagEnd::Table => {
                if let Some(table) = self.table.take() {
                    for (line, links) in table.layout(usize::from(self.inner_width())) {
                        self.put_row(line, Join::Break, 0, 0, false, links);
                    }
                }
            }
            TagEnd::Emphasis => self.emphasis = self.emphasis.saturating_sub(1),
            TagEnd::Strong => self.strong = self.strong.saturating_sub(1),
            TagEnd::Strikethrough => self.strike = self.strike.saturating_sub(1),
            TagEnd::Link => {
                if let Some(open) = self.link_stack.pop().flatten() {
                    let (start, dest) = open;
                    let end = self.inline.len();
                    self.inline_links.push((start..end, dest));
                }
                self.link = self.link.saturating_sub(1);
            }
            TagEnd::Image
            | TagEnd::FootnoteDefinition
            | TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition
            | TagEnd::Superscript
            | TagEnd::Subscript
            | TagEnd::MetadataBlock(_) => {}
        }
    }

    /// The style inline text takes here: a link in the accent colour,
    /// any other text in the full text colour, or the heading's, with
    /// the open emphasis (`docs/tui.md`, "Links").
    fn style(&self) -> Style {
        let mut style = if self.heading {
            style(Role::Heading).add_modifier(Modifier::BOLD)
        } else {
            style(Role::Text)
        };
        if self.link > 0 {
            style = style.fg(Role::Accent.color());
        }
        for (open, modifier) in [
            (self.strong, Modifier::BOLD),
            (self.emphasis, Modifier::ITALIC),
            (self.strike, Modifier::CROSSED_OUT),
            (self.link, Modifier::UNDERLINED),
        ] {
            if open > 0 {
                style = style.add_modifier(modifier);
            }
        }
        style
    }

    /// Adds inline text. A newline is a hard break; a tab is a space.
    fn push(&mut self, text: &str, style: Style) {
        self.inline.extend(
            text.chars()
                .map(|ch| (if ch == '\t' { ' ' } else { ch }, style)),
        );
    }

    /// A blank line before a block, unless one is there already, the block
    /// is the first, or it sits inside a list.
    fn block(&mut self) {
        let len = self.out.lines.len();
        if len > 0 && self.lists.is_empty() && self.separator != Some(len) {
            self.track
                .row(Join::Break, self.quote_width(), 0, false, Vec::new());
            self.out.lines.push(Line::from(self.quote_prefix()));
            self.separator = Some(self.out.lines.len());
        }
    }

    /// `│ ` once per open block quote.
    fn quote_prefix(&self) -> Vec<Span<'static>> {
        (0..self.quote)
            .map(|_| Span::styled("│ ", style(Role::Muted)))
            .collect()
    }

    /// The cells the quote bars take.
    fn quote_width(&self) -> u16 {
        let bars: usize = self.quote_prefix().iter().map(Span::width).sum();
        u16::try_from(bars).unwrap_or(u16::MAX)
    }

    /// The width a block lays out in, inside the quote bars.
    fn inner_width(&self) -> u16 {
        self.width.saturating_sub(self.quote_width())
    }

    /// Adds a block's row after the quote bars, which are not text; with
    /// `decoration` none of it is.
    fn put(&mut self, line: Line<'static>, decoration: bool) {
        self.put_row(line, Join::Break, 0, 0, decoration, Vec::new());
    }

    /// Adds a row after the quote bars that joins the row before by `join`,
    /// whose first `gutter` cells after the bars and whose last `tail`
    /// cells are not text. `links` are the link cells in `line`, offset
    /// past the bars.
    fn put_row(
        &mut self,
        line: Line<'static>,
        join: Join,
        gutter: u16,
        tail: u16,
        decoration: bool,
        links: Vec<(Range<u16>, String)>,
    ) {
        let bars = self.quote_width();
        let skip = bars.saturating_add(gutter);
        let links = links
            .into_iter()
            .map(|(range, url)| {
                (
                    range.start.saturating_add(bars)..range.end.saturating_add(bars),
                    url,
                )
            })
            .collect();
        self.track.row(join, skip, tail, decoration, links);
        let mut spans = self.quote_prefix();
        spans.extend(line.spans);
        self.out.lines.push(Line::from(spans));
    }

    /// The first row's prefix and the other rows': the quote bars, the
    /// list's indent, and the item's marker or its width in spaces.
    fn prefixes(&mut self) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
        let mut first = self.quote_prefix();
        let mut rest = first.clone();
        if let Some(list) = self.lists.last() {
            let indent = " ".repeat(self.lists.len().saturating_sub(1).saturating_mul(2));
            let hang = format!("{indent}{}", " ".repeat(list.marker));
            match self.marker.take() {
                Some(marker) => {
                    first.push(Span::raw(indent));
                    first.push(Span::styled(marker, style(Role::Accent)));
                }
                None => first.push(Span::raw(hang.clone())),
            }
            rest.push(Span::raw(hang));
        }
        (first, rest)
    }

    /// Wraps the gathered inline text into lines. An item's marker shows
    /// even when its item has no text. The first row's text starts after
    /// the quote bars, so a list's indent and marker are text; the other
    /// rows' start after their whole prefix, the hang indent included.
    fn flush(&mut self) {
        if self.inline.is_empty() && self.marker.is_none() {
            self.inline_links.clear();
            return;
        }
        let cells = std::mem::take(&mut self.inline);
        let links = std::mem::take(&mut self.inline_links);
        let (first, rest) = self.prefixes();
        let bars = self.quote_width();
        let hang = u16::try_from(rest.iter().map(Span::width).sum::<usize>()).unwrap_or(u16::MAX);
        let first_width: usize = first.iter().map(Span::width).sum();
        let rest_width: usize = rest.iter().map(Span::width).sum();
        let first_room = usize::from(self.width).saturating_sub(first_width).max(1);
        let rest_room = usize::from(self.width).saturating_sub(rest_width).max(1);
        let rows = wrap_joined_indices(&cells, first_room, rest_room, true);
        if rows.is_empty() {
            self.track.row(Join::Break, bars, 0, false, Vec::new());
            self.out.lines.push(Line::from(first));
            return;
        }
        for (at, (row, join, indices)) in rows.into_iter().enumerate() {
            let skip = if at == 0 { bars } else { hang };
            let mut spans = if at == 0 { first.clone() } else { rest.clone() };
            let prefix: usize = spans.iter().map(Span::width).sum();
            let row_links = link_cols(&row, &indices, prefix, &links);
            self.track.row(join, skip, 0, false, row_links);
            spans.extend(spans_of(&row));
            self.out.lines.push(Line::from(spans));
        }
    }

    /// A code block: the edge rows, the header row with its label and
    /// `copy`, then each line numbered, all on the tint across the width.
    /// Without colour the slab reserves a column on each side: the edge
    /// rows already box through `edge_row`, and the slab's own sides
    /// close the reserved columns, so the code and the header sit one
    /// column in, two narrower. The quote bars, the number gutters, the
    /// joins, the styles and the copied source bytes stay
    /// (`docs/tui.md`, "Look").
    fn code_block(&mut self, info: &str, code: &str) {
        let code = code.strip_suffix('\n').unwrap_or(code);
        let inner = self.inner_width();
        let width = usize::from(inner);
        let label = info
            .split(|ch: char| ch.is_whitespace() || ch == ',')
            .next()
            .unwrap_or_default();
        let boxed = surface::box_fit(inner);
        let content = if boxed {
            width.saturating_sub(2)
        } else {
            width
        };
        // One reserved column past the bars and the gutter is not text.
        let side = u16::from(boxed);
        if let Some(cols) = copy_cols(inner) {
            let bars = self.quote_width();
            self.out.targets.push(CopyTarget {
                line: self.out.lines.len().saturating_add(1),
                cols: cols.start.saturating_add(bars)..cols.end.saturating_add(bars),
                code: code.to_owned(),
            });
        }
        self.put(edge_row(width, Role::Code, true), true);
        self.put(sided(header(label, content), boxed), true);
        let runs = highlight::spans(info, code).unwrap_or_else(|| {
            code.split('\n')
                .map(|line| vec![(Role::CodeText, line.to_owned())])
                .collect()
        });
        // The number's columns fit every line, four at minimum.
        let digits = runs.len().to_string().len().max(NUMBER_WIDTH);
        for (at, line) in runs.iter().enumerate() {
            let mut cells = Vec::new();
            for (role, text) in line {
                // A comment is italic as well as its colour.
                let base = if *role == Role::Comment {
                    tinted(*role).add_modifier(Modifier::ITALIC)
                } else {
                    tinted(*role)
                };
                for ch in text.chars() {
                    if ch == '\t' {
                        let stop = TAB.saturating_sub(cells.len() % TAB);
                        cells.extend(std::iter::repeat_n((' ', base), stop));
                    } else {
                        cells.push((ch, base));
                    }
                }
            }
            // The number right-aligned in its columns, then two spaces;
            // each row skips its own prefix.
            let number = format!("{:>width$}  ", at.saturating_add(1), width = digits);
            let first = [Span::styled(number.clone(), tinted(Role::Rule))];
            let gutter_cells = crate::text::width(&number);
            let rest = [Span::styled(" ".repeat(gutter_cells), tinted(Role::Rule))];
            let gutter = u16::try_from(gutter_cells).unwrap_or(u16::MAX);
            let mut rows = wrap(&cells, content, &first, &rest, false);
            if rows.is_empty() {
                rows.push((Line::from(first.to_vec()), Join::Break));
            }
            for (mut row, join) in rows {
                let pad = content.saturating_sub(row.width());
                row.spans
                    .push(Span::styled(" ".repeat(pad), tinted(Role::CodeText)));
                let row = sided(row, boxed);
                // The trailing side is decoration, not text: the logical
                // copy keeps the source bytes without the border.
                self.put_row(
                    row,
                    join,
                    gutter.saturating_add(side),
                    side,
                    false,
                    Vec::new(),
                );
            }
        }
        self.put(edge_row(width, Role::Code, false), true);
    }
}

/// The line number's minimum columns, without its two spaces.
const NUMBER_WIDTH: usize = 4;

/// The `copy` label's cells on a header row `width` wide: its last
/// thirteen, or none on a narrower row. Without colour the label sits in
/// the interior the reserved sides leave, never on a side
/// (`docs/tui.md`, "Look").
fn copy_cols(width: u16) -> Option<Range<u16>> {
    if surface::box_fit(width) {
        let interior = width.saturating_sub(2);
        let start = interior.checked_sub(u16::try_from(COPY.len()).unwrap_or(u16::MAX))?;
        Some(start.saturating_add(1)..interior.saturating_add(1))
    } else {
        let start = width.checked_sub(u16::try_from(COPY.len()).unwrap_or(u16::MAX))?;
        Some(start..width)
    }
}

/// A code block's header row: one column in, the label left, truncated
/// with `…` to fit, and `copy` right. Too narrow for the label it has
/// none, and too narrow for `copy` it has neither.
fn header(label: &str, width: usize) -> Line<'static> {
    let copy = if width >= COPY.len() { COPY } else { "" };
    let room = width.saturating_sub(COPY.len().saturating_add(2));
    let shown = truncate(label, room);
    let lead = if width > COPY.len() { " " } else { "" };
    let pad = width.saturating_sub(
        crate::text::width(lead)
            .saturating_add(crate::text::width(&shown))
            .saturating_add(copy.len()),
    );
    Line::from(vec![
        Span::styled(lead, tinted(Role::Muted)),
        Span::styled(shown, tinted(Role::Muted)),
        Span::styled(" ".repeat(pad), tinted(Role::Muted)),
        Span::styled(copy, tinted(Role::Muted)),
    ])
}

/// `line` with the slab's own dim sides when `boxed`: `│` in the
/// columns the slab reserves on each side, so the text keeps its cells
/// and a nested slab boxes itself inside the card's sides
/// (`docs/tui.md`, "Look").
fn sided(mut line: Line<'static>, boxed: bool) -> Line<'static> {
    if boxed {
        let side = Style::new().add_modifier(Modifier::DIM);
        line.spans.insert(0, Span::styled("│", side));
        line.spans.push(Span::styled("│", side));
    }
    line
}

#[cfg(test)]
#[path = "markdown_tests.rs"]
mod tests;
