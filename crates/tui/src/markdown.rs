//! A reply's markdown as styled lines (`docs/tui.md`, "Look"): CommonMark
//! with GFM tables and strikethrough. Every line the renderer returns fits
//! the width it was given, so each draws as exactly one row.

mod roles;
mod table;

use std::cell::RefCell;
use std::ops::Range;

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

pub(crate) use roles::Role;

use crate::app::Target;
use crate::highlight;
use crate::turn::Row;

/// A reply rendered at one width.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Rendered {
    /// One row each.
    pub(crate) lines: Vec<Line<'static>>,
    /// Each code block's `copy` target.
    pub(crate) targets: Vec<CopyTarget>,
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

/// One reply's text and its render, kept until the text changes or the
/// render is wanted at another width.
#[derive(Debug)]
pub(crate) struct Reply {
    text: String,
    id: usize,
    cache: RefCell<Option<(u16, Rendered)>>,
}

impl Reply {
    /// A reply holding `text`, its copy targets named by `id`.
    pub(crate) fn new(text: String, id: usize) -> Self {
        Self {
            text,
            id,
            cache: RefCell::new(None),
        }
    }

    /// The id its copy targets carry.
    pub(crate) fn id(&self) -> usize {
        self.id
    }

    /// Appends a delta.
    pub(crate) fn push(&mut self, text: &str) {
        self.text.push_str(text);
        *self.cache.get_mut() = None;
    }

    /// Replaces the text.
    pub(crate) fn set(&mut self, text: String) {
        self.text = text;
        *self.cache.get_mut() = None;
    }

    /// The text rendered at `width`, from the cache when it was rendered at
    /// that width.
    pub(crate) fn rendered(&self, width: u16) -> Rendered {
        let Ok(mut cached) = self.cache.try_borrow_mut() else {
            return render(&self.text, width);
        };
        match &*cached {
            Some((at, rendered)) if *at == width => rendered.clone(),
            Some(_) | None => {
                let rendered = render(&self.text, width);
                *cached = Some((width, rendered.clone()));
                rendered
            }
        }
    }

    /// The rendered lines at `width`, each code block's header carrying
    /// its copy target.
    pub(crate) fn rows(&self, width: u16, out: &mut Vec<Row>) {
        let rendered = self.rendered(width);
        for (at, line) in rendered.lines.into_iter().enumerate() {
            let block = rendered.targets.iter().position(|target| target.line == at);
            let target = block.map(|block| Target::Copy {
                reply: self.id,
                block,
            });
            out.push((line, target));
        }
    }
}

/// The copy target's label.
const COPY: &str = "copy";
/// Cells per tab stop in code.
const TAB: usize = 4;

/// One character and its style, before wrapping.
type Cell = (char, Style);

/// The foreground of `role`.
pub(crate) fn style(role: Role) -> Style {
    Style::new().fg(role.color())
}

/// The foreground of `role` on the code tint.
fn tinted(role: Role) -> Style {
    style(role).bg(Role::CodeTint.color())
}

/// Renders `text` at `width` columns. Pure: the same input gives the same
/// output. An unclosed fence is a code block to the end of the text.
pub(crate) fn render(text: &str, width: u16) -> Rendered {
    let mut writer = Writer {
        width,
        out: Rendered::default(),
        inline: Vec::new(),
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
    };
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    for event in Parser::new_ext(text, options) {
        writer.event(event);
    }
    writer.out
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
            Event::Code(text) => self.push(&text, tinted(Role::CodeText)),
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
                let rule = "─".repeat(usize::from(self.width));
                self.out
                    .lines
                    .push(Line::from(Span::styled(rule, style(Role::Dim))));
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
            Tag::Link { .. } => self.link = self.link.saturating_add(1),
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
                if let Some(table) = &mut self.table {
                    table.push_cell(cell);
                }
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                if let Some(table) = &mut self.table {
                    table.end_row();
                }
            }
            TagEnd::Table => {
                if let Some(table) = self.table.take() {
                    self.out.lines.extend(table.layout(usize::from(self.width)));
                }
            }
            TagEnd::Emphasis => self.emphasis = self.emphasis.saturating_sub(1),
            TagEnd::Strong => self.strong = self.strong.saturating_sub(1),
            TagEnd::Strikethrough => self.strike = self.strike.saturating_sub(1),
            TagEnd::Link => self.link = self.link.saturating_sub(1),
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

    /// The style inline text takes here: the full text colour, or the
    /// heading's, with the open emphasis.
    fn style(&self) -> Style {
        let mut style = if self.heading {
            style(Role::Heading).add_modifier(Modifier::BOLD)
        } else {
            style(Role::Text)
        };
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
            self.out.lines.push(Line::from(self.quote_prefix()));
            self.separator = Some(self.out.lines.len());
        }
    }

    /// `│ ` once per open block quote.
    fn quote_prefix(&self) -> Vec<Span<'static>> {
        (0..self.quote)
            .map(|_| Span::styled("│ ", style(Role::Dim)))
            .collect()
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
    /// even when its item has no text.
    fn flush(&mut self) {
        if self.inline.is_empty() && self.marker.is_none() {
            return;
        }
        let cells = std::mem::take(&mut self.inline);
        let (first, rest) = self.prefixes();
        let lines = wrap(&cells, usize::from(self.width), &first, &rest, true);
        if lines.is_empty() {
            self.out.lines.push(Line::from(first));
        } else {
            self.out.lines.extend(lines);
        }
    }

    /// A code block: the header row with its label and `copy`, then each
    /// line numbered, all on the tint across the width.
    fn code_block(&mut self, info: &str, code: &str) {
        let code = code.strip_suffix('\n').unwrap_or(code);
        let width = usize::from(self.width);
        let label = info
            .split(|ch: char| ch.is_whitespace() || ch == ',')
            .next()
            .unwrap_or_default();
        if let Some(cols) = copy_cols(self.width) {
            self.out.targets.push(CopyTarget {
                line: self.out.lines.len(),
                cols,
                code: code.to_owned(),
            });
        }
        self.out.lines.push(header(label, width));
        let runs = highlight::spans(info, code).unwrap_or_else(|| {
            code.split('\n')
                .map(|line| vec![(Role::CodeText, line.to_owned())])
                .collect()
        });
        let digits = runs.len().to_string().len();
        for (at, line) in runs.iter().enumerate() {
            let mut cells = Vec::new();
            for (role, text) in line {
                for ch in text.chars() {
                    if ch == '\t' {
                        let stop = TAB.saturating_sub(cells.len() % TAB);
                        cells.extend(std::iter::repeat_n((' ', tinted(*role)), stop));
                    } else {
                        cells.push((ch, tinted(*role)));
                    }
                }
            }
            let number = format!("{:>digits$} │ ", at.saturating_add(1));
            let first = [Span::styled(number, tinted(Role::Dim))];
            let rest = [Span::styled(
                format!("{:digits$} │ ", ""),
                tinted(Role::Dim),
            )];
            let mut rows = wrap(&cells, width, &first, &rest, false);
            if rows.is_empty() {
                rows.push(Line::from(first.to_vec()));
            }
            for mut row in rows {
                let pad = width.saturating_sub(row.width());
                row.spans
                    .push(Span::styled(" ".repeat(pad), tinted(Role::CodeText)));
                self.out.lines.push(row);
            }
        }
    }
}

/// The `copy` label's cells on a header row `width` wide: its last four,
/// or none on a row narrower than four.
fn copy_cols(width: u16) -> Option<Range<u16>> {
    let start = width.checked_sub(4)?;
    Some(start..width)
}

/// A code block's header row: the label left, truncated with `…` to fit,
/// and `copy` right. Under 6 cells it has no label, and under 4 no `copy`.
fn header(label: &str, width: usize) -> Line<'static> {
    let copy = if width >= COPY.len() { COPY } else { "" };
    let room = if width >= 6 {
        width.saturating_sub(COPY.len().saturating_add(1))
    } else {
        0
    };
    let shown = truncate(label, room);
    let pad = width.saturating_sub(cells_width(&shown).saturating_add(copy.len()));
    Line::from(vec![
        Span::styled(shown, tinted(Role::Dim)),
        Span::styled(" ".repeat(pad), tinted(Role::Dim)),
        Span::styled(copy, tinted(Role::Accent)),
    ])
}

/// `text` cut to `room` cells, ending in `…` when cut.
fn truncate(text: &str, room: usize) -> String {
    if cells_width(text) <= room {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let width = char_width(ch);
        if used.saturating_add(width).saturating_add(1) > room {
            break;
        }
        out.push(ch);
        used = used.saturating_add(width);
    }
    if room > 0 {
        out.push('…');
    }
    out
}

/// The cells `ch` takes on screen.
fn char_width(ch: char) -> usize {
    let mut buf = [0u8; 4];
    Span::raw(&*ch.encode_utf8(&mut buf)).width()
}

/// The cells `text` takes on screen.
fn cells_width(text: &str) -> usize {
    Span::raw(text).width()
}

/// Wraps `cells` into lines `width` wide, the first after `first` and the
/// rest after `rest`. With `words` a line breaks between words where it
/// can, and spaces at a break are dropped; without, it breaks at the
/// width. A newline ends a line.
fn wrap(
    cells: &[Cell],
    width: usize,
    first: &[Span<'static>],
    rest: &[Span<'static>],
    words: bool,
) -> Vec<Line<'static>> {
    let prefix = |spans: &[Span<'static>]| spans.iter().map(Span::width).sum::<usize>();
    let first_room = width.saturating_sub(prefix(first)).max(1);
    let rest_room = width.saturating_sub(prefix(rest)).max(1);
    wrap_cells(cells, first_room, rest_room, words)
        .into_iter()
        .enumerate()
        .map(|(at, row)| {
            let mut spans = if at == 0 {
                first.to_vec()
            } else {
                rest.to_vec()
            };
            spans.extend(spans_of(&row));
            Line::from(spans)
        })
        .collect()
}

/// Wraps `cells` into rows: the first `first` cells wide, the others
/// `rest`. See [`wrap`].
fn wrap_cells(cells: &[Cell], first: usize, rest: usize, words: bool) -> Vec<Vec<Cell>> {
    let mut rows: Vec<Vec<Cell>> = Vec::new();
    let mut row: Vec<Cell> = Vec::new();
    let mut used = 0usize;
    let mut at = 0usize;
    while let Some(&(ch, _)) = cells.get(at) {
        let room = if rows.is_empty() { first } else { rest };
        if ch == '\n' {
            rows.push(std::mem::take(&mut row));
            used = 0;
            at = at.saturating_add(1);
            continue;
        }
        if words && ch == ' ' && row.is_empty() && !rows.is_empty() {
            at = at.saturating_add(1);
            continue;
        }
        let end = if words && ch != ' ' {
            cells
                .iter()
                .skip(at)
                .position(|(ch, _)| *ch == ' ' || *ch == '\n')
                .map_or(cells.len(), |len| at.saturating_add(len))
        } else {
            at.saturating_add(1)
        };
        let piece = cells.get(at..end).unwrap_or_default();
        let width: usize = piece.iter().map(|(ch, _)| char_width(*ch)).sum();
        if used.saturating_add(width) <= room {
            row.extend_from_slice(piece);
            used = used.saturating_add(width);
            at = end;
            continue;
        }
        if words && !row.is_empty() && (ch == ' ' || width <= rest) {
            while row.last().is_some_and(|(ch, _)| *ch == ' ') {
                row.pop();
            }
            rows.push(std::mem::take(&mut row));
            used = 0;
            continue;
        }
        for &cell in piece {
            let room = if rows.is_empty() { first } else { rest };
            let width = char_width(cell.0);
            if used.saturating_add(width) > room && !row.is_empty() {
                rows.push(std::mem::take(&mut row));
                used = 0;
            }
            row.push(cell);
            used = used.saturating_add(width);
        }
        at = end;
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows
}

/// Cells as spans, one per run of the same style.
fn spans_of(cells: &[Cell]) -> Vec<Span<'static>> {
    let mut spans: Vec<(Style, String)> = Vec::new();
    for &(ch, style) in cells {
        match spans.last_mut() {
            Some((last, text)) if *last == style => text.push(ch),
            _ => spans.push((style, ch.to_string())),
        }
    }
    spans
        .into_iter()
        .map(|(style, text)| Span::styled(text, style))
        .collect()
}

#[cfg(test)]
#[path = "markdown_tests.rs"]
mod tests;
