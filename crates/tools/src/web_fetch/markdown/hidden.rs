//! Tracks hidden HTML elements and foreign-content state for markdown conversion.
//!
//! The open hidden elements stand in one byte each on a single stack, with
//! fixed per-kind counts beside it: for the whole stack, and for the part at
//! and above the outermost `svg`. Closing never scans: it pops the stack's
//! top, which is amortised O(1), and the counts say whether anything of a
//! kind is open there at all.

use html5ever::tokenizer::Tag;

use super::attribute;

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

/// Where the document is relative to its head.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Head {
    /// A `head` start tag can still open the head.
    #[default]
    Before,
    /// The head is open.
    In,
    /// The head has ended, or other content began before one opened: the
    /// HTML standard ignores a `head` start tag from here on.
    After,
}

/// A hidden element's kind: one byte on the stack.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum Kind {
    Svg,
    Noscript,
    Template,
}

impl Kind {
    /// The hidden kind named `name`, if it names one.
    fn of(name: &str) -> Option<Self> {
        match name {
            "svg" => Some(Self::Svg),
            "noscript" => Some(Self::Noscript),
            "template" => Some(Self::Template),
            _ => None,
        }
    }

    /// The count of `kind` in either table.
    fn whole(counts: &mut [usize; 3], kind: Self) -> &mut usize {
        match kind {
            Self::Svg => &mut counts[0],
            Self::Noscript => &mut counts[1],
            Self::Template => &mut counts[2],
        }
    }
}

/// Hidden elements and the HTML head state, independent of markdown output.
#[derive(Default)]
pub(super) struct Hidden {
    /// The open `svg`, `noscript` and `template` elements, innermost last,
    /// one byte each. Everything inside the outermost `svg` is foreign
    /// content: there nothing switches state, so a `title` is neither
    /// collected nor switched, and a `noscript` or `template` is an element
    /// of the `svg`, closed with it.
    stack: Vec<Kind>,
    /// How many of each kind are open: `svg`, `noscript`, `template`.
    counts: [usize; 3],
    /// How many of each kind stand at and above the outermost `svg`;
    /// the whole counts while none is open.
    above: [usize; 3],
    /// Where the outermost `svg` stands, while one is open.
    svg_base: usize,
    head: Head,
}

impl Hidden {
    pub(super) fn is_hidden(&self) -> bool {
        !self.stack.is_empty()
    }

    pub(super) fn in_svg(&self) -> bool {
        self.svg_open()
    }

    pub(super) fn in_head(&self) -> bool {
        self.head == Head::In
    }

    /// Whether any `svg` is open.
    fn svg_open(&self) -> bool {
        match self.counts {
            [svg, _, _] => svg > 0,
        }
    }

    /// Whether any `template` is open.
    pub(super) fn templates_open(&self) -> bool {
        match self.counts {
            [_, _, templates] => templates > 0,
        }
    }

    /// Whether a `noscript` stands at or above the outermost `svg`.
    pub(super) fn noscripts_above(&self) -> bool {
        match self.above {
            [_, noscripts, _] => noscripts > 0,
        }
    }

    /// The at-and-above-the-outermost-`svg` counts, for tests.
    #[cfg(test)]
    pub(super) fn above_counts(&self) -> [usize; 3] {
        self.above
    }

    /// The whole-stack count of `kind`.
    fn whole(&mut self, kind: Kind) -> &mut usize {
        Kind::whole(&mut self.counts, kind)
    }

    /// The at-and-above-the-outermost-`svg` count of `kind`.
    fn over(&mut self, kind: Kind) -> &mut usize {
        Kind::whole(&mut self.above, kind)
    }

    /// The bytes the open hidden elements retain.
    #[cfg(test)]
    pub(super) fn open_capacity(&self) -> usize {
        self.stack.capacity()
    }

    /// Handles the hidden-element state changes for a start tag.
    pub(super) fn open(&mut self, name: &str, tag: &Tag) {
        // The HTML standard ends the head at the first start tag that cannot
        // be in it, and every open `svg` at the first that cannot be in one.
        if name == "head" {
            if self.head == Head::Before {
                self.head = Head::In;
            }
        } else if name != "html" && (self.head == Head::Before || !IN_HEAD.contains(&name)) {
            self.head = Head::After;
        }
        if breaks_out_of_svg(name, tag) {
            self.close_svg();
        }
        if let Some(kind) = Kind::of(name)
            && !tag.self_closing
        {
            self.push(kind);
        }
    }

    /// Handles the hidden-element state changes for an end tag, returning
    /// whether it names a hidden element.
    pub(super) fn end(&mut self, name: &str) -> bool {
        // The HTML standard also ends the head at these end tags, and every
        // open `svg` at `</p>` and `</br>`; `</br>` is then a `br`.
        if matches!(name, "head" | "body" | "html" | "br") {
            self.head = Head::After;
        }
        if matches!(name, "p" | "br") {
            self.close_svg();
        }
        if Kind::of(name).is_some() {
            self.end_hidden(name);
            true
        } else {
            false
        }
    }

    /// Whether the head is still open after this text token.
    pub(super) fn text_is_in_head(&mut self, text: &str) -> bool {
        if !text.chars().all(|c| c.is_ascii_whitespace()) {
            self.head = Head::After;
        }
        self.head == Head::In
    }

    /// Pushes one open hidden element: one byte, with its counts. Opening
    /// the outermost `svg` starts the above-`svg` counts over, since what
    /// stands below it is no longer above one.
    fn push(&mut self, kind: Kind) {
        if kind == Kind::Svg && !self.in_svg() {
            self.svg_base = self.stack.len();
            self.above = [0; 3];
        }
        self.stack.push(kind);
        *self.whole(kind) += 1;
        *self.over(kind) += 1;
    }

    /// Pops the innermost open hidden element, keeping its counts. With
    /// no `svg` open the above counts are the whole counts, restored when
    /// the last one closes. With one still open the popped element stood
    /// at or above the outermost one, so its above count falls too: the
    /// outermost `svg` still stands at `svg_base`, and the stack still
    /// holds it, so the length left is past `svg_base` without checking.
    fn pop(&mut self) {
        if let Some(kind) = self.stack.pop() {
            *self.whole(kind) -= 1;
            if !self.in_svg() {
                self.above = self.counts;
            } else {
                *self.over(kind) -= 1;
            }
        }
    }

    /// An end tag of a hidden element, as the HTML standard closes one.
    /// Inside an `svg` it closes the innermost element of its name there.
    /// Failing that, `</template>` closes the innermost `template`, and a
    /// `</noscript>` the innermost hidden element when it is a `noscript`,
    /// as a `template` or `noscript` around one stops it. Anything open
    /// inside the closed element closes with it; any other end tag is
    /// ignored, so an `svg` already closed stays closed. Each element is
    /// pushed once and popped once, so every close is amortised O(1).
    fn end_hidden(&mut self, name: &str) {
        match name {
            "template" => {
                if self.templates_open() {
                    self.pop_until(Kind::Template);
                }
            }
            "noscript" => {
                if !self.in_svg() {
                    // With no `svg` open only a `noscript` on top closes:
                    // anything above one stops its end tag.
                    if self.stack.last() == Some(&Kind::Noscript) {
                        self.pop();
                    }
                } else if self.noscripts_above() {
                    self.pop_until(Kind::Noscript);
                } else if self.svg_base > 0
                    && self.stack.get(self.svg_base - 1) == Some(&Kind::Noscript)
                {
                    // Every `noscript` stands below the outermost `svg`:
                    // the one just below it closes with what it hides. The
                    // count is fixed before popping, so the loop ends even
                    // when the stack is empty: no condition flips to forever.
                    let extra = self.stack.len() - (self.svg_base - 1);
                    for _ in 0..extra {
                        self.pop();
                    }
                }
            }
            _ => {
                if self.in_svg() {
                    self.pop_until(Kind::Svg);
                }
            }
        }
        self.shrink();
    }

    /// Closes every open `svg`, and with them what they hide.
    fn close_svg(&mut self) {
        if self.in_svg() {
            while self.stack.len() > self.svg_base {
                self.pop();
            }
            self.shrink();
        }
    }

    /// Pops the innermost element of `kind` and everything open inside it.
    /// The kind is open, so the loop always finds it.
    fn pop_until(&mut self, kind: Kind) {
        while self.stack.last() != Some(&kind) {
            self.pop();
        }
        self.pop();
    }

    /// Halves the stack's capacity while its length stays below a quarter
    /// of it, floor 64 bytes: the capacity is at most `max(64, 4 × open)`
    /// however elements opened and closed, and every close stays amortised
    /// O(1).
    fn shrink(&mut self) {
        while self.stack.capacity() > 64 && self.stack.len() * 4 < self.stack.capacity() {
            let mut smaller = Vec::with_capacity(self.stack.capacity() / 2);
            smaller.extend(self.stack.iter().copied());
            self.stack = smaller;
        }
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
