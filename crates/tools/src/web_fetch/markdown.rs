//! HTML to markdown for `web_fetch` (`docs/tools.md`, "web_fetch"). One pass
//! over the page, no tree: the converter holds the page and its output, and
//! no nesting depth in the input becomes recursion or an indent without a cap.

/// Levels of list indentation and block quote prefix kept; a hostile page
/// that nests deeper than this gets no more indentation.
const MAX_LEVELS: usize = 8;

/// The longest entity, `&` and `;` included, that is looked at.
const MAX_ENTITY: usize = 32;

/// Converts `html` to markdown: headings, paragraphs, lists, links, images,
/// emphasis, code, quotes and tables, with scripts, styles and the head
/// dropped. Text that is not markup, and markup it does not know, passes
/// through. The result ends with one newline, or is empty.
pub(crate) fn to_markdown(html: &str) -> String {
    let mut converter = Converter::default();
    converter.run(html);
    converter.finish()
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
    /// Open `svg`, `noscript` and `template` elements, whose content is
    /// dropped.
    hidden: usize,
    in_head: bool,
    /// Open `pre` elements; the text of one is verbatim.
    pre: usize,
    /// Where the content of the outermost `pre` starts in `out`.
    pre_start: usize,
    link: Option<Link>,
    lists: Vec<List>,
    /// Open lists past [`MAX_LEVELS`]: kept as a count, not entries, so a
    /// hostile page of opens cannot grow the stack. Closing tags pop this
    /// first. `quote`, `hidden` and `pre` are already counts, not stacks.
    over: usize,
    quote: usize,
    cells: usize,
    /// Whether the last thing written was whitespace, so another is dropped.
    last_space: bool,
    /// Set when a `<title>` had no end tag, so no later one is searched for.
    title_unclosed: bool,
}

impl Default for Converter {
    fn default() -> Self {
        Self {
            out: String::new(),
            title: None,
            hidden: 0,
            in_head: false,
            pre: 0,
            pre_start: 0,
            link: None,
            lists: Vec::new(),
            over: 0,
            quote: 0,
            cells: 0,
            last_space: true,
            title_unclosed: false,
        }
    }
}

impl Converter {
    fn run(&mut self, html: &str) {
        // Slices, not indexes: every step hands back a shorter suffix, so a
        // broken step can only misread, never spin.
        let mut rest = html;
        while let Some(lt) = rest.find('<') {
            self.text(rest.get(..lt).unwrap_or_default());
            rest = rest.get(lt..).unwrap_or_default();
            rest = self.markup(rest);
        }
        self.text(rest);
    }

    /// Handles the `<` that starts `rest` and returns the text after it.
    /// Every path consumes at least one byte, so the caller always moves on.
    fn markup<'a>(&mut self, rest: &'a str) -> &'a str {
        if let Some(body) = rest.strip_prefix("<!--") {
            return match body.find("-->") {
                Some(end) => body.get(end + "-->".len()..).unwrap_or_default(),
                None => "",
            };
        }
        let next = rest.as_bytes().get(1).copied();
        match next {
            Some(b'!' | b'?') => {
                return match rest.find('>') {
                    Some(end) => rest.get(end + 1..).unwrap_or_default(),
                    None => "",
                };
            }
            Some(byte) if byte.is_ascii_alphabetic() => {}
            Some(b'/') if rest.as_bytes().get(2).is_some_and(u8::is_ascii_alphabetic) => {}
            _ => {
                self.text("<");
                return rest.get(1..).unwrap_or_default();
            }
        }
        // debt: a `>` inside a quoted attribute value ends the tag, because
        // a search that respects quotes is quadratic on a page of unclosed
        // quotes; read quotes if a page shows the damage.
        let Some(end) = rest.find('>') else {
            self.text(rest);
            return "";
        };
        let tag = rest.get(1..end).unwrap_or_default();
        self.tag(tag, rest.get(end + 1..).unwrap_or_default())
    }

    /// Handles one tag, `<` and `>` removed. Returns the text after it,
    /// which is past `after` for an element whose content is skipped.
    fn tag<'a>(&mut self, raw: &str, after: &'a str) -> &'a str {
        let (closing, raw) = match raw.strip_prefix('/') {
            Some(raw) => (true, raw),
            None => (false, raw),
        };
        let name_end = raw
            .find(|c: char| !(c.is_ascii_alphanumeric() || "-:".contains(c)))
            .unwrap_or(raw.len());
        let name = raw.get(..name_end).unwrap_or_default().to_ascii_lowercase();
        let attrs = raw.get(name_end..).unwrap_or_default();
        let self_closing = attrs.trim_end().ends_with('/');
        let opens = !closing && !self_closing;

        match name.as_str() {
            "script" | "style" if !closing => {
                // Past its end tag, or the end of the page when it has none.
                return match split_end_tag(after, &name) {
                    Some((_, resume)) => resume,
                    None => "",
                };
            }
            "title" if !closing => return self.title(after),
            "head" => self.in_head = !closing,
            "body" if !closing => self.in_head = false,
            "svg" | "noscript" | "template" => {
                if closing {
                    self.hidden = self.hidden.saturating_sub(1);
                } else if opens {
                    self.hidden += 1;
                }
            }
            _ => {
                if self.hidden == 0 && !self.in_head {
                    self.visible_tag(&name, closing, attrs);
                }
            }
        }
        after
    }

    /// Reads a title's text up to its end tag. Only the first title counts.
    fn title<'a>(&mut self, after: &'a str) -> &'a str {
        if self.title_unclosed {
            return after;
        }
        if self.hidden > 0 {
            return after;
        }
        let Some((text, resume)) = split_end_tag(after, "title") else {
            self.title_unclosed = true;
            return after;
        };
        if self.title.is_none() {
            let decoded = decode(text);
            let title = decoded
                .split_ascii_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            self.title = Some(title);
        }
        resume
    }

    fn visible_tag(&mut self, name: &str, closing: bool, attrs: &str) {
        if self.pre > 0 && !matches!(name, "pre" | "br") {
            return;
        }
        match name {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.block_break();
                if !closing {
                    let level = name.as_bytes().get(1).map_or(1, |digit| digit - b'0');
                    for _ in 0..level {
                        self.push('#');
                    }
                    self.push(' ');
                }
            }
            "p" | "div" | "section" | "article" | "main" | "header" | "footer" | "nav"
            | "aside" => self.block_break(),
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
                    self.start_link(attrs);
                }
            }
            "strong" | "b" => self.push_str("**"),
            "em" | "i" => self.push_str("_"),
            "code" => self.push_str("`"),
            "img" if !closing => self.image(attrs),
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

    fn start_link(&mut self, attrs: &str) {
        // No `pre` check: `visible_tag` returns before every tag but `pre`
        // and `br` inside `pre`, so a link never opens there.
        if self.link.is_some() {
            return;
        }
        if let Some(href) = attribute(attrs, "href") {
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

    fn image(&mut self, attrs: &str) {
        let alt = attribute(attrs, "alt").unwrap_or_default();
        match attribute(attrs, "src") {
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

    fn text(&mut self, raw: &str) {
        if self.hidden > 0 || self.in_head {
            return;
        }
        let mut rest = raw;
        while let Some(amp) = rest.find('&') {
            self.plain(rest.get(..amp).unwrap_or_default());
            rest = rest.get(amp..).unwrap_or_default();
            match entity(rest) {
                Some((Entity::Char(c), used)) => {
                    self.character(c);
                    rest = rest.get(used..).unwrap_or_default();
                }
                Some((Entity::Written, used)) => {
                    self.push_str(rest.get(..used).unwrap_or_default());
                    rest = rest.get(used..).unwrap_or_default();
                }
                None => {
                    self.push('&');
                    rest = rest.get(1..).unwrap_or_default();
                }
            }
        }
        self.plain(rest);
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

/// The text between `after` and its end tag `</name>`, matched without
/// regard to case, and the text after that tag. The scan walks the `</`
/// occurrences with an iterator, so a broken offset can only misread: the
/// walk still ends when the occurrences run out.
fn split_end_tag<'a>(after: &'a str, name: &str) -> Option<(&'a str, &'a str)> {
    let bytes = after.as_bytes();
    for (lt, _) in after.match_indices("</") {
        let name_at = lt + 2;
        let candidate = bytes.get(name_at..name_at + name.len())?;
        let boundary = bytes.get(name_at + name.len()).copied();
        if candidate.eq_ignore_ascii_case(name.as_bytes())
            && boundary.is_some_and(|b| b == b'>' || b == b'/' || b.is_ascii_whitespace())
        {
            let close = after.get(name_at..)?.find('>')?;
            return Some((
                after.get(..lt).unwrap_or_default(),
                after.get(name_at + close + 1..).unwrap_or_default(),
            ));
        }
    }
    None
}

enum Entity {
    /// A character reference that names one.
    Char(char),
    /// A well-formed reference that names nothing, kept as written.
    Written,
}

/// The entity at the `&` that starts `rest`, and how many bytes it takes.
fn entity(rest: &str) -> Option<(Entity, usize)> {
    let window = rest.as_bytes().get(..MAX_ENTITY.min(rest.len()))?;
    let semicolon = window.iter().position(|&b| b == b';')?;
    let body = rest.get(1..semicolon)?;
    if body.is_empty() {
        return None;
    }
    if !body.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'#') {
        return None;
    }
    let used = semicolon + 1;
    let named = match body {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some(' '),
        _ => None,
    };
    let numeric = body.strip_prefix('#').and_then(|digits| {
        let (digits, radix) = match digits.strip_prefix(['x', 'X']) {
            Some(hex) => (hex, 16),
            None => (digits, 10),
        };
        u32::from_str_radix(digits, radix).ok()
    });
    let decoded = named.or_else(|| numeric.filter(|&n| n != 0).and_then(char::from_u32));
    Some((decoded.map_or(Entity::Written, Entity::Char), used))
}

/// `text` with its entities decoded.
fn decode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(rest.get(..amp).unwrap_or_default());
        rest = rest.get(amp..).unwrap_or_default();
        match entity(rest) {
            Some((Entity::Char(c), used)) => {
                out.push(c);
                rest = rest.get(used..).unwrap_or_default();
            }
            Some((Entity::Written, used)) => {
                out.push_str(rest.get(..used).unwrap_or_default());
                rest = rest.get(used..).unwrap_or_default();
            }
            None => {
                out.push('&');
                rest = rest.get(1..).unwrap_or_default();
            }
        }
    }
    out.push_str(rest);
    out
}

/// The value of attribute `wanted` in `attrs`, entities decoded, or `None`
/// when it is absent or has no value. The scan hands back a shorter suffix
/// on every pass, so a broken step can only misread, never spin.
fn attribute(attrs: &str, wanted: &str) -> Option<String> {
    let mut rest = attrs;
    while !rest.is_empty() {
        let tail = rest.trim_start_matches(|c: char| c.is_ascii_whitespace());
        let name_end = tail
            .bytes()
            .position(|b| !(b.is_ascii_alphanumeric() || b"-_:".contains(&b)))
            .unwrap_or(tail.len());
        let (name, after_name) = tail.split_at(name_end);
        if name.is_empty() {
            // Not a name: past one character, so the scan always moves on.
            let first = after_name.chars().next()?;
            rest = after_name.get(first.len_utf8()..).unwrap_or_default();
            continue;
        }
        let tail = after_name.trim_start_matches(|c: char| c.is_ascii_whitespace());
        let Some(value_rest) = tail.strip_prefix('=') else {
            // A name with no value: the next attribute starts after it.
            rest = after_name;
            continue;
        };
        let value_rest = value_rest.trim_start_matches(|c: char| c.is_ascii_whitespace());
        let first = value_rest.as_bytes().first()?;
        if *first == b'"' || *first == b'\'' {
            let body = value_rest.get(1..).unwrap_or_default();
            let Some(quote_end) = body.bytes().position(|b| b == *first) else {
                // An unclosed quote runs to the end.
                if name.eq_ignore_ascii_case(wanted) {
                    return Some(decode(body));
                }
                return None;
            };
            let value = body.get(..quote_end).unwrap_or_default();
            rest = body
                .get(quote_end..)
                .unwrap_or_default()
                .get(1..)
                .unwrap_or_default();
            if name.eq_ignore_ascii_case(wanted) {
                return Some(decode(value));
            }
        } else {
            let end = value_rest
                .find(|c: char| c.is_ascii_whitespace())
                .unwrap_or(value_rest.len());
            let value = value_rest.get(..end).unwrap_or_default();
            rest = value_rest.get(end..).unwrap_or_default();
            if name.eq_ignore_ascii_case(wanted) {
                return Some(decode(value));
            }
        }
    }
    None
}

#[cfg(test)]
#[path = "markdown_tests.rs"]
mod tests;
