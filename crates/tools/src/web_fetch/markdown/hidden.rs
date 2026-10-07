//! Tracks hidden HTML elements and foreign-content state for markdown conversion.

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

/// Hidden elements and the HTML head state, independent of markdown output.
pub(super) struct Hidden {
    /// Open `svg`, `noscript` and `template` elements, whose content is
    /// dropped. Each has a depth, its place among them counted from the
    /// outermost; the three stacks below hold the depths of each kind,
    /// innermost last, so every open, close and lookup is amortised O(1).
    hidden: usize,
    /// Depths of the open `svg` elements. Everything inside the outermost
    /// is foreign content: there nothing switches state, so a `title` is
    /// neither collected nor switched, and a `noscript` or `template` is an
    /// element of the `svg`, closed with it.
    svgs: Vec<usize>,
    noscripts: Vec<usize>,
    templates: Vec<usize>,
    in_head: bool,
}

impl Default for Hidden {
    fn default() -> Self {
        Self {
            hidden: 0,
            svgs: Vec::new(),
            noscripts: Vec::new(),
            templates: Vec::new(),
            in_head: false,
        }
    }
}

impl Hidden {
    pub(super) fn is_hidden(&self) -> bool {
        self.hidden > 0
    }

    pub(super) fn in_svg(&self) -> bool {
        !self.svgs.is_empty()
    }

    pub(super) fn in_head(&self) -> bool {
        self.in_head
    }

    /// Handles the hidden-element state changes for a start tag.
    pub(super) fn open(&mut self, name: &str, tag: &Tag) {
        // The HTML standard ends the head at the first start tag that cannot
        // be in it, and every open `svg` at the first that cannot be in one.
        if !IN_HEAD.contains(&name) {
            self.in_head = false;
        }
        if breaks_out_of_svg(name, tag) {
            self.close_svg();
        }
        let depth = self.hidden;
        if name == "head" {
            self.in_head = true;
        } else if let Some(depths) = self.depths(name) {
            if !tag.self_closing {
                depths.push(depth);
                self.hidden += 1;
            }
        }
    }

    /// Handles the hidden-element state changes for an end tag, returning
    /// whether it names a hidden element.
    pub(super) fn end(&mut self, name: &str) -> bool {
        // The HTML standard also ends the head at these end tags, and every
        // open `svg` at `</p>` and `</br>`; `</br>` is then a `br`.
        if matches!(name, "head" | "body" | "html" | "br") {
            self.in_head = false;
        }
        if matches!(name, "p" | "br") {
            self.close_svg();
        }
        if matches!(name, "svg" | "noscript" | "template") {
            self.end_hidden(name);
            true
        } else {
            false
        }
    }

    /// Whether the head is still open after this text token.
    pub(super) fn text_is_in_head(&mut self, text: &str) -> bool {
        self.in_head &= text.chars().all(|c| c.is_ascii_whitespace());
        self.in_head
    }

    /// An end tag of a hidden element, as the HTML standard closes one.
    /// Inside an `svg` it closes the innermost element of its name there.
    /// Failing that, `</template>` closes the innermost `template`, and a
    /// `</noscript>` the innermost hidden element when it is a `noscript`,
    /// as a `template` or `noscript` around one stops it. Anything open
    /// inside the closed element closes with it; any other end tag is
    /// ignored, so an `svg` already closed stays closed.
    fn end_hidden(&mut self, name: &str) {
        let foreign = self.svgs.first().copied().unwrap_or(self.hidden);
        let innermost = self.depths(name).and_then(|depths| depths.last().copied());
        let at = innermost.filter(|&at| at >= foreign).or_else(|| {
            if name == "template" {
                innermost
            } else {
                foreign
                    .checked_sub(1)
                    .filter(|&below| innermost == Some(below))
            }
        });
        if let Some(at) = at {
            self.close_hidden(at);
        }
    }

    /// Closes every open `svg`, and with them what they hide.
    fn close_svg(&mut self) {
        if let Some(&svg) = self.svgs.first() {
            self.close_hidden(svg);
        }
    }

    /// Closes the hidden element at depth `at` and everything open inside
    /// it: each depth is popped once, so this is amortised O(1).
    fn close_hidden(&mut self, at: usize) {
        self.hidden = at;
        for depths in [&mut self.svgs, &mut self.noscripts, &mut self.templates] {
            while depths.last().is_some_and(|&depth| depth >= at) {
                depths.pop();
            }
        }
    }

    /// The depths of the open hidden elements named `name`, if it names
    /// a hidden element.
    fn depths(&mut self, name: &str) -> Option<&mut Vec<usize>> {
        match name {
            "svg" => Some(&mut self.svgs),
            "noscript" => Some(&mut self.noscripts),
            "template" => Some(&mut self.templates),
            _ => None,
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
