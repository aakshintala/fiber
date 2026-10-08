//! HTML to markdown for `web_fetch` (`docs/tools.md`, "web_fetch"). One pass
//! over the page, no tree: html5ever's tokenizer, with no document tree,
//! feeds the single-pass writer in a child module as the page's text
//! arrives, so the converter holds its output and never the page, and no
//! nesting depth in the input becomes recursion or an indent without a cap.
//! Every character reference is decoded per the HTML standard by the
//! tokenizer, in text and attributes.

use std::cell::RefCell;

use html5ever::tendril::StrTendril;
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

/// The largest slice of a whole page [`to_markdown`] feeds at once.
#[cfg(test)]
const SLICE: usize = 64 * 1024;

/// Converts a page whose text arrives in pieces to markdown: headings,
/// paragraphs, lists, links, images, emphasis, code, quotes and tables,
/// with scripts, styles and the head dropped. Text that is not markup, and
/// markup it does not know, passes through. However the page is cut into
/// pieces, the markdown is the same: a tag, entity, character or
/// `</script>` cut across pieces changes nothing.
pub(crate) struct Stream {
    tokenizer: Tokenizer<Sink>,
    queue: BufferQueue,
}

impl Default for Stream {
    fn default() -> Self {
        let sink = Sink {
            cell: RefCell::default(),
        };
        Self {
            tokenizer: Tokenizer::new(sink, TokenizerOpts::default()),
            queue: BufferQueue::default(),
        }
    }
}

impl Stream {
    /// Converts the next piece of the page.
    pub(crate) fn push(&mut self, text: &str) {
        self.queue.push_back(text.into());
        let _feed = self.tokenizer.feed(&self.queue);
    }

    /// Ends the page: the markdown, ending with one newline, or empty.
    pub(crate) fn finish(self) -> String {
        self.tokenizer.end();
        let converter = self.tokenizer.sink.cell.into_inner();
        converter.writer.finish(converter.title)
    }
}

/// Converts `html` whole to markdown, as [`Stream`] does.
#[cfg(test)]
pub(crate) fn to_markdown(html: &str) -> String {
    convert(html, SLICE)
}

/// Converts `html` pushing slices of at most `slice` bytes, each cut on a
/// char boundary.
#[cfg(test)]
fn convert(html: &str, slice: usize) -> String {
    let slice = slice.max(1);
    let mut stream = Stream::default();
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
        stream.push(html.get(start..end).unwrap_or_default());
        start = end;
    }
    stream.finish()
}

/// The tokenizer's sink: tags drive the writer, character tokens become
/// text, and everything else (comments, doctypes, parse errors, NUL) writes
/// nothing.
struct Sink {
    cell: RefCell<Converter>,
}

impl TokenSink for Sink {
    type Handle = ();

    fn process_token(&self, token: Token, _line: u64) -> TokenSinkResult<Self::Handle> {
        let mut converter = self.cell.borrow_mut();
        match token {
            Token::TagToken(tag) => converter.tag_token(tag),
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

#[derive(Default)]
struct Converter {
    title: Option<String>,
    /// A whitespace run in the title being collected, collapsed to one
    /// space when another visible character follows it.
    title_space: bool,
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

impl Converter {
    /// Handles one tokenized tag. A `script`, `style` or first `title`
    /// start tag switches the tokenizer to raw text; anything else is
    /// handled and the tokenizer continues as usual. The tag is owned, so
    /// a link's `href` moves out of it without a copy.
    fn tag_token(&mut self, tag: Tag) -> TokenSinkResult<()> {
        match tag.kind {
            TagKind::StartTag => self.start_tag(tag),
            TagKind::EndTag => {
                self.end_tag(tag);
                TokenSinkResult::Continue
            }
        }
    }

    fn start_tag(&mut self, mut tag: Tag) -> TokenSinkResult<()> {
        let name = tag.name.clone();
        let name = name.as_ref();
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
                    // The collected title is output storage: it always
                    // becomes the heading unless empty.
                    self.title = Some(String::new());
                    self.title_space = false;
                } else {
                    self.raw = Some(Raw::TitleDrop);
                }
                return TokenSinkResult::RawData(RawKind::Rcdata);
            }
        }
        self.hidden.open(name, &tag);
        if !self.hidden.is_hidden() && !self.hidden.in_head() {
            self.visible_tag(name, false, &mut tag);
        }
        TokenSinkResult::Continue
    }

    fn end_tag(&mut self, mut tag: Tag) {
        let name = tag.name.clone();
        let name = name.as_ref();
        if self.raw.is_some_and(|raw| name == raw.name()) {
            self.end_raw();
            return;
        }
        if !self.hidden.end(name) && !self.hidden.is_hidden() && !self.hidden.in_head() {
            self.visible_tag(name, true, &mut tag);
        }
    }

    /// The end tag of the raw-text element: a collected title becomes the
    /// pending heading, everything else was already dropped. The title
    /// arrived collapsed, so nothing is copied here. A dropped title
    /// leaves `title_done` alone, so a later title still counts.
    fn end_raw(&mut self) {
        if self.raw == Some(Raw::Title) {
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

    fn visible_tag(&mut self, name: &str, closing: bool, tag: &mut Tag) {
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
                } else if let Some(href) = take_href(tag) {
                    self.writer.start_link(href);
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
                self.writer.push_str(alt);
                self.writer.push_str("](");
                self.writer.push_str(src);
                self.writer.push(')');
            }
            None => self.writer.plain(alt),
        }
    }

    /// Character tokens: already entity-decoded per the HTML standard by
    /// the tokenizer. Raw `script` and `style` text is dropped, a counted
    /// title's is collapsed as it arrives into the title string, and
    /// anything hidden or in the head is dropped. Text that is not all
    /// whitespace ends the head, as the HTML standard ends it.
    fn chars(&mut self, text: &str) {
        match self.raw {
            Some(Raw::Title) => {
                let Some(title) = self.title.as_mut() else {
                    return;
                };
                for c in text.chars() {
                    if c.is_ascii_whitespace() {
                        self.title_space = true;
                    } else {
                        // Leading whitespace drops out with the pending
                        // space, as in the writer's link text.
                        if self.title_space && !title.is_empty() {
                            title.push(' ');
                        }
                        self.title_space = false;
                        title.push(c);
                    }
                }
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

/// The value of attribute `wanted` on a tokenized tag, borrowed from the
/// token, or `None` when it is absent. Names are compared ASCII
/// case-insensitively; values arrive entity-decoded per the HTML standard
/// from the tokenizer.
fn attribute<'a>(tag: &'a Tag, wanted: &str) -> Option<&'a str> {
    tag.attrs.iter().find_map(|attr| {
        let name: &str = &attr.name.local;
        if name == wanted {
            let value: &str = &attr.value;
            Some(value)
        } else {
            None
        }
    })
}

/// Moves the `href` out of a tokenized open tag without a copy, for the
/// open link to hold until its closing tag.
fn take_href(tag: &mut Tag) -> Option<StrTendril> {
    tag.attrs.iter_mut().find_map(|attr| {
        let name: &str = &attr.name.local;
        if name == "href" {
            Some(std::mem::take(&mut attr.value))
        } else {
            None
        }
    })
}

#[cfg(test)]
#[path = "markdown_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "markdown_bounds_tests.rs"]
mod bounds_tests;

#[cfg(test)]
#[path = "markdown_props_tests.rs"]
mod props_tests;

#[cfg(test)]
#[path = "pages_tests.rs"]
mod pages_tests;
