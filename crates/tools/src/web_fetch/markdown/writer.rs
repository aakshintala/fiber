//! Owns markdown output state and writes text, breaks, fences and the title.

use html5ever::tokenizer::Tag;

use super::{MAX_LEVELS, attribute};

/// A link whose closing tag has not arrived.
struct Link {
    href: String,
    text: String,
}

pub(super) struct Writer {
    out: String,
    /// Whether the last thing written was whitespace, so another is dropped.
    last_space: bool,
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

    pub(super) fn start_link(&mut self, tag: &Tag) {
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

    pub(super) fn end_link(&mut self) {
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
        if let Some(title) = title.filter(|title| !title.is_empty()) {
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
