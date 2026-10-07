//! HTML to markdown for `web_fetch` (`docs/tools.md`, "web_fetch"). One pass
//! over the page, no tree: html5ever's tokenizer, with no document tree,
//! feeds the single-pass writer below in slices, so the converter holds the
//! page and its output, and no nesting depth in the input becomes recursion
//! or an indent without a cap. Every character reference is decoded per the
//! HTML standard by the tokenizer, in text and attributes.

use std::cell::RefCell;

use html5ever::tokenizer::states::RawKind;
use html5ever::tokenizer::{
    BufferQueue, Tag, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
};

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
    cell.into_inner().finish()
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

/// The start tags that can be in the head, beside `script`, `style` and
/// `title`, which switch to raw text first. Any other ends it.
const IN_HEAD: [&str; 10] = [
    "base", "basefont", "bgsound", "head", "html", "link", "meta", "noframes", "noscript",
    "template",
];

/// The start tags that close every open `svg`, beside a `font` with a
/// `color`, `face` or `size` attribute.
const OUT_OF_SVG: [&str; 44] = [
    "b",
    "big",
    "blockquote",
    "body",
    "br",
    "center",
    "code",
    "dd",
    "div",
    "dl",
    "dt",
    "em",
    "embed",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "head",
    "hr",
    "i",
    "img",
    "li",
    "listing",
    "menu",
    "meta",
    "nobr",
    "ol",
    "p",
    "pre",
    "ruby",
    "s",
    "small",
    "span",
    "strong",
    "strike",
    "sub",
    "sup",
    "table",
    "tt",
    "u",
    "ul",
    "var",
];

/// An open element whose content is dropped.
#[derive(Clone, Copy, PartialEq)]
enum Hidden {
    Svg,
    Noscript,
    Template,
}

impl Hidden {
    fn of(name: &str) -> Option<Self> {
        match name {
            "svg" => Some(Hidden::Svg),
            "noscript" => Some(Hidden::Noscript),
            "template" => Some(Hidden::Template),
            _ => None,
        }
    }
}

/// What a list item is numbered by.
struct List {
    ordered: bool,
    count: u64,
}

/// A link whose closing tag has not arrived.
struct Link {
    href: String,
    text: String,
}

struct Converter {
    out: String,
    title: Option<String>,
    /// The text of the title being collected, when one is.
    title_text: String,
    /// Whether a title has been seen: only the first counts, as browsers
    /// render only the first.
    title_done: bool,
    /// The raw-text element whose text is arriving, if any.
    raw: Option<Raw>,
    /// Open `svg`, `noscript` and `template` elements, whose content is
    /// dropped, innermost last: one byte each, so a page of opens grows it
    /// by less than the page itself.
    hidden: Vec<Hidden>,
    /// Where in `hidden` the outermost open `svg` is. Everything after it
    /// is foreign content, inside the `svg`: there nothing switches state,
    /// so a `title` is neither collected nor switched, and a `noscript` or
    /// `template` is an element of the `svg`, closed with it.
    svg: Option<usize>,
    in_head: bool,
    /// Open `pre` elements; the text of one is verbatim.
    pre: usize,
    /// Where the content of the outermost `pre` starts in `out`.
    pre_start: usize,
    link: Option<Link>,
    lists: Vec<List>,
    /// Open lists past [`MAX_LEVELS`]: kept as a count, not entries, so a
    /// hostile page of opens cannot grow the stack. Closing tags pop this
    /// first. `quote` and `pre` are already counts, not stacks.
    over: usize,
    quote: usize,
    cells: usize,
    /// Whether the last thing written was whitespace, so another is dropped.
    last_space: bool,
}

impl Default for Converter {
    fn default() -> Self {
        Self {
            out: String::new(),
            title: None,
            title_text: String::new(),
            title_done: false,
            raw: None,
            hidden: Vec::new(),
            svg: None,
            in_head: false,
            pre: 0,
            pre_start: 0,
            link: None,
            lists: Vec::new(),
            over: 0,
            quote: 0,
            cells: 0,
            last_space: true,
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
        if self.svg.is_none() {
            if name == "script" {
                self.raw = Some(Raw::Script);
                return TokenSinkResult::RawData(RawKind::ScriptData);
            }
            if name == "style" {
                self.raw = Some(Raw::Style);
                return TokenSinkResult::RawData(RawKind::Rawtext);
            }
            if name == "title" {
                if !self.title_done && self.hidden.is_empty() {
                    self.raw = Some(Raw::Title);
                    self.title_text.clear();
                } else {
                    self.raw = Some(Raw::TitleDrop);
                }
                return TokenSinkResult::RawData(RawKind::Rcdata);
            }
        }
        // The HTML standard ends the head at the first start tag that cannot
        // be in it, and every open `svg` at the first that cannot be in one.
        if !IN_HEAD.contains(&name) {
            self.in_head = false;
        }
        if breaks_out_of_svg(name, tag) {
            self.close_svg();
        }
        if name == "head" {
            self.in_head = true;
        } else if let Some(hidden) = Hidden::of(name) {
            if !tag.self_closing {
                if hidden == Hidden::Svg && self.svg.is_none() {
                    self.svg = Some(self.hidden.len());
                }
                self.hidden.push(hidden);
            }
        } else if self.hidden.is_empty() && !self.in_head {
            self.visible_tag(name, false, tag);
        }
        TokenSinkResult::Continue
    }

    fn end_tag(&mut self, name: &str, tag: &Tag) {
        if self.raw.is_some_and(|raw| name == raw.name()) {
            self.end_raw();
            return;
        }
        // The HTML standard also ends the head at these end tags, and every
        // open `svg` at `</p>` and `</br>`; `</br>` is then a `br`.
        if matches!(name, "head" | "body" | "html" | "br") {
            self.in_head = false;
        }
        if matches!(name, "p" | "br") {
            self.close_svg();
        }
        if let Some(hidden) = Hidden::of(name) {
            self.end_hidden(hidden);
        } else if self.hidden.is_empty() && !self.in_head {
            self.visible_tag(name, true, tag);
        }
    }

    /// An end tag of a hidden element, as the HTML standard closes one.
    /// Inside an `svg` it closes the innermost element of its name there.
    /// Failing that, `</template>` closes the innermost `template`, and a
    /// `</noscript>` the innermost hidden element when it is a `noscript`,
    /// as a `template` or `noscript` around one stops it. Anything open
    /// inside the closed element closes with it; any other end tag is
    /// ignored, so an `svg` already closed stays closed.
    fn end_hidden(&mut self, hidden: Hidden) {
        let foreign = self.svg.unwrap_or(self.hidden.len());
        let open = self.hidden.iter().copied().enumerate().rev();
        let at = open
            .clone()
            .take_while(|&(at, _)| at >= foreign)
            .find(|&(_, open)| open == hidden)
            .or_else(|| match hidden {
                Hidden::Template => open.clone().find(|&(_, open)| open == hidden),
                Hidden::Svg | Hidden::Noscript => open
                    .clone()
                    .find(|&(at, _)| at < foreign)
                    .filter(|&(_, open)| open == hidden),
            })
            .map(|(at, _)| at);
        if let Some(at) = at {
            self.truncate_hidden(at);
        }
    }

    /// Closes every open `svg`, and with them what they hide.
    fn close_svg(&mut self) {
        if let Some(svg) = self.svg {
            self.truncate_hidden(svg);
        }
    }

    /// Closes the hidden element at `at` and everything open inside it.
    fn truncate_hidden(&mut self, at: usize) {
        self.hidden.truncate(at);
        if self.svg.is_some_and(|svg| svg >= at) {
            self.svg = None;
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
        if self.pre > 0 && !matches!(name, "pre" | "br") {
            return;
        }
        if matches!(name, "h1" | "h2" | "h3" | "h4" | "h5" | "h6") {
            self.block_break();
            if !closing {
                let level = name.as_bytes().get(1).map_or(1, |digit| digit - b'0');
                for _ in 0..level {
                    self.push('#');
                }
                self.push(' ');
            }
            return;
        }
        if matches!(
            name,
            "p" | "div" | "section" | "article" | "main" | "header" | "footer" | "nav" | "aside"
        ) {
            self.block_break();
            return;
        }
        match name {
            "br" => self.soft_break(),
            "hr" => {
                self.block_break();
                self.push_str("---");
                self.block_break();
            }
            "blockquote" => {
                self.block_break();
                self.quote = if closing {
                    self.quote.saturating_sub(1)
                } else {
                    self.quote + 1
                };
            }
            "ul" | "ol" => self.list(closing, name == "ol"),
            "li" => {
                if !closing {
                    self.item();
                } else {
                    self.soft_break();
                }
            }
            "a" => {
                if closing {
                    self.end_link();
                } else {
                    self.start_link(tag);
                }
            }
            "strong" | "b" => self.push_str("**"),
            "em" | "i" => self.push_str("_"),
            "code" => self.push_str("`"),
            "img" if !closing => self.image(tag),
            "pre" => self.pre_tag(closing),
            "table" => self.block_break(),
            "tr" => {
                self.soft_break();
                self.cells = 0;
            }
            "td" | "th" if !closing => {
                if self.cells > 0 {
                    self.trim_inline();
                    self.push_str(" | ");
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
                self.soft_break();
                return;
            }
            self.lists.pop();
            if self.lists.is_empty() {
                self.block_break();
            } else {
                self.soft_break();
            }
            return;
        }
        if self.lists.is_empty() {
            self.block_break();
        } else {
            self.soft_break();
        }
        if self.lists.len() >= MAX_LEVELS {
            self.over += 1;
        } else {
            self.lists.push(List { ordered, count: 0 });
        }
    }

    fn item(&mut self) {
        self.soft_break();
        let indent = self.lists.len().saturating_sub(1).min(MAX_LEVELS) * 2;
        for _ in 0..indent {
            self.push(' ');
        }
        let marker = match self.lists.last_mut() {
            Some(list) if list.ordered => {
                list.count = list.count.saturating_add(1);
                format!("{}. ", list.count)
            }
            Some(_) | None => "- ".to_owned(),
        };
        self.push_str(&marker);
    }

    fn start_link(&mut self, tag: &Tag) {
        // No `pre` check: `visible_tag` returns before every tag but `pre`
        // and `br` inside `pre`, so a link never opens there.
        if self.link.is_some() {
            return;
        }
        if let Some(href) = attribute(tag, "href") {
            self.link = Some(Link {
                href,
                text: String::new(),
            });
            self.last_space = true;
        }
    }

    fn end_link(&mut self) {
        let Some(link) = self.link.take() else {
            return;
        };
        let text = link
            .text
            .split_ascii_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if text.is_empty() {
            return;
        }
        self.push('[');
        self.push_str(&text);
        self.push_str("](");
        self.push_str(&link.href);
        self.push(')');
    }

    fn image(&mut self, tag: &Tag) {
        let alt = attribute(tag, "alt").unwrap_or_default();
        match attribute(tag, "src") {
            Some(src) => {
                self.push_str("![");
                self.push_str(&alt);
                self.push_str("](");
                self.push_str(&src);
                self.push(')');
            }
            None => self.plain(&alt),
        }
    }

    fn pre_tag(&mut self, closing: bool) {
        if !closing {
            if self.pre == 0 {
                self.block_break();
                self.push_str("```\n");
                self.pre_start = self.out.len();
            }
            self.pre += 1;
        } else if self.pre > 0 {
            self.pre -= 1;
            if self.pre == 0 {
                self.close_fence();
            }
        }
    }

    fn close_fence(&mut self) {
        while self.out.len() > self.pre_start && self.out.ends_with(['\n', '\r']) {
            self.out.pop();
        }
        if self.out.len() > self.pre_start {
            self.out.push('\n');
        }
        self.out.push_str("```");
        self.last_space = false;
        self.block_break();
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
        if !self.hidden.is_empty() {
            return;
        }
        self.in_head &= text.chars().all(|c| c.is_ascii_whitespace());
        if self.in_head {
            return;
        }
        self.plain(text);
    }

    fn plain(&mut self, text: &str) {
        for c in text.chars() {
            self.character(c);
        }
    }

    fn character(&mut self, c: char) {
        if self.pre == 0 && c.is_ascii_whitespace() {
            if !self.last_space {
                self.push(' ');
            }
        } else {
            self.push(c);
        }
    }

    /// Writes one character where the converter is writing: the open link's
    /// text, or the output, which starts a quoted line with its prefix.
    fn push(&mut self, c: char) {
        self.last_space = c.is_ascii_whitespace();
        if self.pre == 0
            && let Some(link) = &mut self.link
        {
            link.text.push(c);
            return;
        }
        // Without a quote the loop below runs zero times, so no guard is
        // needed: one less comparison a mutant could flip for nothing.
        if c != '\n' && (self.out.is_empty() || self.out.ends_with('\n')) {
            for _ in 0..self.quote.min(MAX_LEVELS) {
                self.out.push_str("> ");
            }
        }
        self.out.push(c);
    }

    fn push_str(&mut self, text: &str) {
        for c in text.chars() {
            self.push(c);
        }
    }

    /// Whether a break is inside `pre`, where it does nothing. `visible_tag`
    /// returns before every tag that breaks, and `close_fence` runs after
    /// `pre` hits zero, so a mutant of this check changes nothing.
    #[cfg_attr(false, mutants::skip)]
    fn break_in_pre(&self) -> bool {
        self.pre > 0
    }

    /// Ends the line, and leaves a blank one after it. Inside a link, a
    /// space: a link's text is one line. Inside `pre`, nothing.
    fn block_break(&mut self) {
        if self.break_in_pre() {
            return;
        }
        if self.link.is_some() {
            self.space();
            return;
        }
        self.trim_inline();
        if self.out.is_empty() || self.out.ends_with("\n\n") {
            return;
        }
        self.out.push_str(if self.out.ends_with('\n') {
            "\n"
        } else {
            "\n\n"
        });
        self.last_space = true;
    }

    /// Ends the line.
    fn soft_break(&mut self) {
        if self.pre > 0 {
            self.out.push('\n');
            return;
        }
        if self.link.is_some() {
            self.space();
            return;
        }
        self.trim_inline();
        if self.out.is_empty() || self.out.ends_with('\n') {
            return;
        }
        self.out.push('\n');
        self.last_space = true;
    }

    fn space(&mut self) {
        if self.last_space {
            return;
        }
        self.push(' ');
    }

    fn trim_inline(&mut self) {
        while self.out.ends_with([' ', '\t']) {
            self.out.pop();
        }
        self.last_space = self
            .out
            .chars()
            .next_back()
            .is_none_or(|c| c.is_ascii_whitespace());
    }

    fn finish(mut self) -> String {
        if self.pre > 0 {
            self.pre = 0;
            self.close_fence();
        }
        self.end_link();
        self.out.truncate(self.out.trim_end().len());
        if let Some(title) = self.title.take().filter(|title| !title.is_empty()) {
            let head = if self.out.is_empty() {
                format!("# {title}")
            } else {
                format!("# {title}\n\n")
            };
            self.out.insert_str(0, &head);
        }
        if !self.out.is_empty() {
            self.out.push('\n');
        }
        self.out
    }
}

/// Whether a start tag closes every open `svg`, per the HTML standard's
/// rules for foreign content: an HTML element that cannot be inside one.
fn breaks_out_of_svg(name: &str, tag: &Tag) -> bool {
    OUT_OF_SVG.contains(&name)
        || name == "font"
            && ["color", "face", "size"]
                .iter()
                .any(|wanted| attribute(tag, wanted).is_some())
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
