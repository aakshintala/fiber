//! HTML to markdown for `web_fetch` (`docs/tools.md`, "web_fetch"). One pass
//! over the page, no tree: html5ever's tokenizer, with no document tree,
//! feeds the single-pass writer in a child module in slices, so the converter
//! holds the page and its output, and no nesting depth in the input becomes recursion
//! or an indent without a cap. Every character reference is decoded per the
//! HTML standard by the tokenizer, in text and attributes.

use std::cell::RefCell;

use html5ever::tokenizer::states::RawKind;
use html5ever::tokenizer::{
    BufferQueue, Tag, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
};

mod hidden;
mod writer;

use hidden::Hidden;
use writer::Writer;

/// Levels of list indentation and block quote prefix kept; a hostile page
/// that nests deeper than this gets no more indentation.
const MAX_LEVELS: usize = 8;

/// The largest slice of the page fed to the tokenizer at once: the
/// converter never holds a second whole copy as tendrils.
const SLICE: usize = 64 * 1024;

/// Converts `html` to markdown: headings, paragraphs, lists, links, images,
/// emphasis, code, quotes and tables, with scripts, styles and the head
/// dropped. Text that is not markup, and markup it does not know, passes
/// through. The result ends with one newline, or is empty.
pub(crate) fn to_markdown(html: &str) -> String {
    convert(html, SLICE)
}

/// Converts `html` feeding the tokenizer in slices of at most `slice`
/// bytes, each cut on a char boundary. Every slice size converts the same:
/// a tag, entity, multibyte char or `</script>` cut across slices changes
/// nothing.
fn convert(html: &str, slice: usize) -> String {
    let slice = slice.max(1);
    let cell = RefCell::new(Converter::default());
    let sink = Sink { cell: &cell };
    let tokenizer = Tokenizer::new(sink, TokenizerOpts::default());
    let queue = BufferQueue::default();
    let bytes = html.as_bytes();
    let mut start = 0;
    // `end` never passes the length, so the cuts land on it exactly.
    while start != bytes.len() {
        let mut end = bytes.len().min(start.saturating_add(slice));
        // Cutting on a boundary cuts nothing (`floor` returns it), so the
        // cut runs unconditionally: one less check a mutant could flip.
        let floor = html.floor_char_boundary(end);
        end = if floor == start {
            html.ceil_char_boundary(end)
        } else {
            floor
        };
        queue.push_back(html.get(start..end).unwrap_or_default().into());
        let _feed = tokenizer.feed(&queue);
        start = end;
    }
    tokenizer.end();
    let converter = cell.into_inner();
    converter.writer.finish(converter.title)
}

/// The tokenizer's sink: tags drive the writer, character tokens become
/// text, and everything else (comments, doctypes, parse errors, NUL) writes
/// nothing.
struct Sink<'a> {
    cell: &'a RefCell<Converter>,
}

impl TokenSink for Sink<'_> {
    type Handle = ();

    fn process_token(&self, token: Token, _line: u64) -> TokenSinkResult<Self::Handle> {
        let mut converter = self.cell.borrow_mut();
        match token {
            Token::TagToken(tag) => converter.tag_token(&tag),
            Token::CharacterTokens(text) => {
                converter.chars(&text);
                TokenSinkResult::Continue
            }
            Token::DoctypeToken(_)
            | Token::CommentToken(_)
            | Token::NullCharacterToken
            | Token::EOFToken
            | Token::ParseError(_) => TokenSinkResult::Continue,
        }
    }

    fn end(&self) {
        self.cell.borrow_mut().input_ended();
    }
}

/// The raw-text element whose text is arriving, if any. Each variant
/// knows its end-tag name, so the element and its handling cannot disagree.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Raw {
    /// `script`: dropped.
    Script,
    /// `style`: dropped.
    Style,
    /// A `title` that counts: collected for the leading heading.
    Title,
    /// A `title` that does not count: read as raw text and dropped.
    TitleDrop,
}

impl Raw {
    /// The end tag that closes the element.
    fn name(self) -> &'static str {
        match self {
            Raw::Script => "script",
            Raw::Style => "style",
            Raw::Title | Raw::TitleDrop => "title",
        }
    }
}

/// What a list item is numbered by.
struct List {
    ordered: bool,
    count: u64,
}

struct Converter {
    title: Option<String>,
    /// The text of the title being collected, when one is.
    title_text: String,
    /// Whether a title has been seen: only the first counts, as browsers
    /// render only the first.
    title_done: bool,
    /// The raw-text element whose text is arriving, if any.
    raw: Option<Raw>,
    hidden: Hidden,
    writer: Writer,
    lists: Vec<List>,
    /// Open lists past [`MAX_LEVELS`]: kept as a count, not entries, so a
    /// hostile page of opens cannot grow the stack. Closing tags pop this
    /// first. The writer's `quote` and `pre` are already counts, not stacks.
    over: usize,
    cells: usize,
}

impl Default for Converter {
    fn default() -> Self {
        Self {
            title: None,
            title_text: String::new(),
            title_done: false,
            raw: None,
            hidden: Hidden::default(),
            writer: Writer::default(),
            lists: Vec::new(),
            over: 0,
            cells: 0,
        }
    }
}

impl Converter {
    /// Handles one tokenized tag. A `script`, `style` or first `title`
    /// start tag switches the tokenizer to raw text; anything else is
    /// handled and the tokenizer continues as usual.
    fn tag_token(&mut self, tag: &Tag) -> TokenSinkResult<()> {
        match tag.kind {
            TagKind::StartTag => self.start_tag(&tag.name, tag),
            TagKind::EndTag => {
                self.end_tag(&tag.name, tag);
                TokenSinkResult::Continue
            }
        }
    }

    fn start_tag(&mut self, name: &str, tag: &Tag) -> TokenSinkResult<()> {
        // Raw-text switches apply everywhere but inside `svg`, whose
        // content is foreign content: tokenized as markup and dropped.
        // Inside `noscript` and `template` the switches apply.
        if !self.hidden.in_svg() {
            if name == "script" {
                self.raw = Some(Raw::Script);
                return TokenSinkResult::RawData(RawKind::ScriptData);
            }
            if name == "style" {
                self.raw = Some(Raw::Style);
                return TokenSinkResult::RawData(RawKind::Rawtext);
            }
            if name == "title" {
                if !self.title_done && !self.hidden.is_hidden() {
                    self.raw = Some(Raw::Title);
                    self.title_text.clear();
                } else {
                    self.raw = Some(Raw::TitleDrop);
                }
                return TokenSinkResult::RawData(RawKind::Rcdata);
            }
        }
        self.hidden.open(name, tag);
        if !self.hidden.is_hidden() && !self.hidden.in_head() {
            self.visible_tag(name, false, tag);
        }
        TokenSinkResult::Continue
    }

    fn end_tag(&mut self, name: &str, tag: &Tag) {
        if self.raw.is_some_and(|raw| name == raw.name()) {
            self.end_raw();
            return;
        }
        if !self.hidden.end(name) && !self.hidden.is_hidden() && !self.hidden.in_head() {
            self.visible_tag(name, true, tag);
        }
    }

    /// The end tag of the raw-text element: a collected title becomes the
    /// pending heading, everything else was already dropped. A dropped
    /// title leaves `title_done` alone, so a later title still counts.
    fn end_raw(&mut self) {
        if self.raw == Some(Raw::Title) {
            let title = self
                .title_text
                .split_ascii_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            self.title = Some(title);
            self.title_done = true;
        }
        self.raw = None;
    }

    /// The input ended: a title never closed swallows the rest of the page
    /// in Rcdata, so its collected text, whitespace collapsed, becomes the
    /// leading heading, and no text is lost.
    fn input_ended(&mut self) {
        if self.raw == Some(Raw::Title) {
            self.end_raw();
        } else {
            self.raw = None;
        }
    }

    fn visible_tag(&mut self, name: &str, closing: bool, tag: &Tag) {
        if self.writer.in_pre() && !matches!(name, "pre" | "br") {
            return;
        }
        if matches!(name, "h1" | "h2" | "h3" | "h4" | "h5" | "h6") {
            self.writer.block_break();
            if !closing {
                let level = name.as_bytes().get(1).map_or(1, |digit| digit - b'0');
                for _ in 0..level {
                    self.writer.push('#');
                }
                self.writer.push(' ');
            }
            return;
        }
        if matches!(
            name,
            "p" | "div" | "section" | "article" | "main" | "header" | "footer" | "nav" | "aside"
        ) {
            self.writer.block_break();
            return;
        }
        match name {
            "br" => self.writer.soft_break(),
            "hr" => {
                self.writer.block_break();
                self.writer.push_str("---");
                self.writer.block_break();
            }
            "blockquote" => self.writer.block_quote(closing),
            "ul" | "ol" => self.list(closing, name == "ol"),
            "li" => {
                if !closing {
                    self.item();
                } else {
                    self.writer.soft_break();
                }
            }
            "a" => {
                if closing {
                    self.writer.end_link();
                } else {
                    self.writer.start_link(tag);
                }
            }
            "strong" | "b" => self.writer.push_str("**"),
            "em" | "i" => self.writer.push_str("_"),
            "code" => self.writer.push_str("`"),
            "img" if !closing => self.image(tag),
            "pre" => self.writer.pre_tag(closing),
            "table" => self.writer.block_break(),
            "tr" => {
                self.writer.soft_break();
                self.cells = 0;
            }
            "td" | "th" if !closing => {
                if self.cells > 0 {
                    self.writer.trim_inline();
                    self.writer.push_str(" | ");
                }
                self.cells += 1;
            }
            _ => {}
        }
        if name == "table" && closing {
            self.cells = 0;
        }
    }

    fn list(&mut self, closing: bool, ordered: bool) {
        if closing {
            if self.over > 0 {
                self.over -= 1;
                self.writer.soft_break();
                return;
            }
            self.lists.pop();
            if self.lists.is_empty() {
                self.writer.block_break();
            } else {
                self.writer.soft_break();
            }
            return;
        }
        if self.lists.is_empty() {
            self.writer.block_break();
        } else {
            self.writer.soft_break();
        }
        if self.lists.len() >= MAX_LEVELS {
            self.over += 1;
        } else {
            self.lists.push(List { ordered, count: 0 });
        }
    }

    fn item(&mut self) {
        self.writer.soft_break();
        let indent = self.lists.len().saturating_sub(1).min(MAX_LEVELS) * 2;
        for _ in 0..indent {
            self.writer.push(' ');
        }
        let marker = match self.lists.last_mut() {
            Some(list) if list.ordered => {
                list.count = list.count.saturating_add(1);
                format!("{}. ", list.count)
            }
            Some(_) | None => "- ".to_owned(),
        };
        self.writer.push_str(&marker);
    }

    fn image(&mut self, tag: &Tag) {
        let alt = attribute(tag, "alt").unwrap_or_default();
        match attribute(tag, "src") {
            Some(src) => {
                self.writer.push_str("![");
                self.writer.push_str(&alt);
                self.writer.push_str("](");
                self.writer.push_str(&src);
                self.writer.push(')');
            }
            None => self.writer.plain(&alt),
        }
    }

    /// Character tokens: already entity-decoded per the HTML standard by
    /// the tokenizer. Raw `script` and `style` text is dropped, a counted
    /// title's is collected, and anything hidden or in the head is dropped.
    /// Text that is not all whitespace ends the head, as the HTML standard
    /// ends it.
    fn chars(&mut self, text: &str) {
        match self.raw {
            Some(Raw::Title) => {
                self.title_text.push_str(text);
                return;
            }
            Some(_) => return,
            None => {}
        }
        if self.hidden.is_hidden() {
            return;
        }
        if self.hidden.text_is_in_head(text) {
            return;
        }
        self.writer.plain(text);
    }
}

/// The value of attribute `wanted` on a tokenized tag, or `None` when it is
/// absent. Names are compared ASCII case-insensitively; values arrive
/// entity-decoded per the HTML standard from the tokenizer.
fn attribute(tag: &Tag, wanted: &str) -> Option<String> {
    tag.attrs.iter().find_map(|attr| {
        let name: &str = &attr.name.local;
        if name == wanted {
            Some(attr.value.to_string())
        } else {
            None
        }
    })
}

#[cfg(test)]
#[path = "markdown_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "markdown_props_tests.rs"]
mod props_tests;

#[cfg(test)]
#[path = "pages_tests.rs"]
mod pages_tests;
