//! One reply's text and its render, kept until the text changes or the
//! render is wanted at another width. A reply and its clones share one
//! render slot until either changes its text, so drawing a clone fills
//! the stored reply's slot; changing a reply gives it a fresh, empty
//! slot, leaving the others' text and render alone.

use std::sync::{Arc, Mutex};

use super::{Rendered, render};
use crate::app::Target;
use crate::rows::{RowText, Rows};

/// The render at the width last asked, shared by a reply and its clones.
type Shared = Arc<Mutex<Option<(u16, Arc<Rendered>)>>>;

/// One reply's text, its copy target ids, and the render slot it shares
/// with its clones. Every reply sharing a slot holds the same text, so
/// the slot holds their common render when it holds one.
#[derive(Debug, Clone)]
pub(crate) struct Reply {
    text: String,
    id: usize,
    cache: Shared,
}

impl Reply {
    /// A reply holding `text`, its copy targets named by `id`.
    pub(crate) fn new(text: String, id: usize) -> Self {
        Self {
            text,
            id,
            cache: Arc::new(Mutex::new(None)),
        }
    }

    /// The id its copy targets carry.
    pub(crate) fn id(&self) -> usize {
        self.id
    }

    /// Appends a delta, leaving the shared slot for the replies still
    /// holding the old text.
    pub(crate) fn push(&mut self, text: &str) {
        self.text.push_str(text);
        self.cache = Arc::new(Mutex::new(None));
    }

    /// Replaces the text, leaving the shared slot for the replies still
    /// holding the old text.
    pub(crate) fn set(&mut self, text: String) {
        self.text = text;
        self.cache = Arc::new(Mutex::new(None));
    }

    /// The text rendered at `width`, from the shared slot when it was
    /// rendered at that width.
    pub(crate) fn rendered(&self, width: u16) -> Arc<Rendered> {
        crate::work::add(|work| work.reply_renders += 1);
        let mut cached = self.cache.lock().unwrap_or_else(|error| error.into_inner());
        if let Some((at, rendered)) = &*cached
            && *at == width
        {
            return Arc::clone(rendered);
        }
        let rendered = Arc::new(render(&self.text, width));
        *cached = Some((width, Arc::clone(&rendered)));
        rendered
    }

    /// The width the shared slot holds a render for, if any. Tests only:
    /// reads the slot without filling it.
    #[cfg(test)]
    pub(crate) fn cached_at(&self) -> Option<u16> {
        let cached = self.cache.lock().unwrap_or_else(|error| error.into_inner());
        cached.as_ref().map(|(width, _)| *width)
    }

    /// The rendered lines at `width`, each code block's header carrying
    /// its copy target, and each with what it adds to its logical line.
    pub(crate) fn rows(&self, width: u16, out: &mut Rows) {
        let rendered = self.rendered(width);
        for (at, line) in rendered.lines.iter().cloned().enumerate() {
            let block = rendered.targets.iter().position(|target| target.line == at);
            let target = block.map(|block| Target::Copy {
                reply: self.id,
                block,
            });
            let text = rendered
                .text
                .get(at)
                .cloned()
                .unwrap_or_else(RowText::plain);
            out.push_text((line, target), text);
        }
    }
}

#[cfg(test)]
#[path = "reply_tests.rs"]
mod tests;
