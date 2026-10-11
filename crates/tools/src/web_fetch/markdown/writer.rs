//! Owns markdown output state and writes text, breaks, fences and the title.
//! A link's text is written straight into the output as it arrives: `[`
//! goes before its first visible character, and a run of whitespace
//! becomes one pending space, written only when another visible character
//! follows. A link with no text writes nothing at all.

use html5ever::tendril::StrTendril;

use super::MAX_LEVELS;

/// A link whose closing tag has not arrived: its `href`, held without a
/// copy, and whether `[` is already written.
struct Link {
    href: StrTendril,
    bracket: bool,
}

/// The markdown output, at most fourteen times the decoded page. Every
/// output byte is one of five kinds: text, markers (`#`, `-`, `N. `,
/// `**`, `_`, backticks, ` | `, `---`, fences, `[`, `](`, `)`, `![`),
/// newlines, indentation (at most 14 bytes, written only by an `<li>`),
/// and quote prefixes (at most 16 bytes, written only before the first
/// byte of a line that is not a newline). Text is never longer than its
/// source, apart from character references, whose output stays under
/// twice their source. Each prefixed line is charged to two distinct
/// pieces of input: the one that wrote the newline before it and the one
/// that wrote its first byte, each charged at most one line in each role.
/// Per tag, the most it writes over its shortest source is `<li>` with 53
/// bytes for 4 (a newline, 16 prefix, 14 indent and a `N. ` marker of at
/// most 22), 13.25 times; `<h6>` writes 25 for 4, `<hr>` 23 for 4,
/// `</pre>` 22 for 6, `<b>` 18 for 3, and a `pre` text line 18 for 2. The
/// trailing newline adds at most 1 byte on a page of at least 1. So 13.25
/// is the worst a page reaches, and 14 leaves margin; ordinary pages stay
/// below 2. Charset expansion is counted apart: decoding hands over at
/// most 3 bytes per downloaded byte.
pub(super) struct Writer {
    out: String,
    /// Whether the last thing written was whitespace, so another is dropped.
    last_space: bool,
    /// A whitespace run inside a link, written as one space only when a
    /// visible character follows it.
    link_space: bool,
    /// Open `pre` elements; the text of one is verbatim.
    pre: usize,
    /// Where the content of the outermost `pre` starts in `out`.
    pre_start: usize,
    link: Option<Link>,
    quote: usize,
}

impl Default for Writer {
    fn default() -> Self {
        Self {
            out: String::new(),
            last_space: true,
            link_space: false,
            pre: 0,
            pre_start: 0,
            link: None,
            quote: 0,
        }
    }
}

impl Writer {
    pub(super) fn in_pre(&self) -> bool {
        self.pre > 0
    }

    pub(super) fn block_quote(&mut self, closing: bool) {
        self.block_break();
        self.quote = if closing {
            self.quote.saturating_sub(1)
        } else {
            self.quote + 1
        };
    }

    pub(super) fn start_link(&mut self, href: StrTendril) {
        // No `pre` check: `visible_tag` returns before every tag but `pre`
        // and `br` inside `pre`, so a link never opens there.
        if self.link.is_some() {
            return;
        }
        self.link = Some(Link {
            href,
            bracket: false,
        });
        self.link_space = false;
        self.last_space = true;
    }

    pub(super) fn end_link(&mut self) {
        let Some(link) = self.link.take() else {
            return;
        };
        // A trailing whitespace run drops out with the pending space, so
        // a link with no visible text writes nothing.
        self.link_space = false;
        if !link.bracket {
            return;
        }
        self.push_str("](");
        self.push_str(&link.href);
        self.push(')');
    }

    pub(super) fn pre_tag(&mut self, closing: bool) {
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

    pub(super) fn push_str(&mut self, text: &str) {
        for c in text.chars() {
            self.push(c);
        }
    }

    pub(super) fn plain(&mut self, text: &str) {
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

    pub(super) fn push(&mut self, c: char) {
        self.last_space = c.is_ascii_whitespace();
        if self.pre == 0 && self.link.is_some() {
            // Whitespace inside a link waits as one pending space; the
            // link's text is already in the output, so nothing is held.
            if c.is_ascii_whitespace() {
                self.link_space = true;
                return;
            }
            let bracketed = self.link.as_ref().is_some_and(|link| link.bracket);
            if !bracketed {
                if let Some(link) = &mut self.link {
                    link.bracket = true;
                }
                // Leading whitespace drops out with the pending space.
                self.link_space = false;
                if self.out.is_empty() || self.out.ends_with('\n') {
                    for _ in 0..self.quote.min(MAX_LEVELS) {
                        self.out.push_str("> ");
                    }
                }
                self.out.push('[');
            } else if self.link_space {
                self.link_space = false;
                self.out.push(' ');
            }
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

    /// Whether a break is inside `pre`, where it does nothing. `visible_tag`
    /// returns before every tag that breaks, and `close_fence` runs after
    /// `pre` hits zero, so a mutant of this check changes nothing.
    #[cfg_attr(false, mutants::skip)]
    fn break_in_pre(&self) -> bool {
        self.pre > 0
    }

    /// Ends the line, and leaves a blank one after it. Inside a link, a
    /// space: a link's text is one line. Inside `pre`, nothing.
    pub(super) fn block_break(&mut self) {
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
    pub(super) fn soft_break(&mut self) {
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

    pub(super) fn trim_inline(&mut self) {
        while self.out.ends_with([' ', '\t']) {
            self.out.pop();
        }
        self.last_space = self
            .out
            .chars()
            .next_back()
            .is_none_or(|c| c.is_ascii_whitespace());
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

    pub(super) fn finish(mut self, title: Option<String>) -> String {
        if self.pre > 0 {
            self.pre = 0;
            self.close_fence();
        }
        self.end_link();
        self.out.truncate(self.out.trim_end().len());
        let Some(title) = title.filter(|title| !title.is_empty()) else {
            if !self.out.is_empty() {
                self.out.push('\n');
            }
            return self.out;
        };
        if self.out.is_empty() {
            // No markdown: the title string itself becomes the result.
            let mut out = title;
            out.insert_str(0, "# ");
            out.push('\n');
            return out;
        }
        // Both parts are non-empty: the result is built in the longer of
        // the two buffers, copying the shorter once, so finishing holds
        // no second copy of either. Each insert copies only what it adds.
        // Equal lengths take this branch: either copy costs the same and
        // the output is identical, so no measurement can tell them apart.
        if title.len() <= self.out.len() {
            self.out.insert_str(0, "\n\n");
            self.out.insert_str(0, &title);
            self.out.insert_str(0, "# ");
        } else {
            let mut out = title;
            out.insert_str(0, "# ");
            out.push_str("\n\n");
            out.push_str(&self.out);
            self.out = out;
        }
        self.out.push('\n');
        self.out
    }
}
