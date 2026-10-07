//! A downloaded page on its way to its result (`docs/tools.md`,
//! "web_fetch"): converted to markdown piece by piece as it arrives, so no
//! whole copy of the page is held beside its markdown.

use super::charset::{self, Decoding, PRESCAN};
use super::markdown::Stream;

/// The size of the pieces a download is handled in.
pub(super) const PIECE: usize = 64 * 1024;

/// An HTML page converting to markdown as its bytes arrive. Its character
/// set is chosen from its first 1024 bytes, as a whole page's would be
/// (`docs/tools.md`, "HTML to markdown"), so the markdown is the same
/// however the bytes are cut into pieces.
pub(super) struct Html {
    content_type: Option<String>,
    /// The first bytes, held until 1024 have arrived or the page ends.
    held: Vec<u8>,
    /// Set once the character set is chosen.
    decoding: Option<Decoding>,
    stream: Stream,
}

impl Html {
    /// A page served with `content_type`.
    pub(super) fn new(content_type: Option<&str>) -> Self {
        Self {
            content_type: content_type.map(str::to_owned),
            held: Vec::new(),
            decoding: None,
            stream: Stream::default(),
        }
    }

    /// Converts the next bytes of the page.
    pub(super) fn push(&mut self, bytes: &[u8]) {
        let rest = if self.decoding.is_some() {
            bytes
        } else {
            let wanted = PRESCAN.saturating_sub(self.held.len()).min(bytes.len());
            let (head, rest) = bytes.split_at(wanted);
            self.held.extend_from_slice(head);
            if self.held.len() < PRESCAN {
                return;
            }
            self.start();
            rest
        };
        self.feed(rest, false);
    }

    /// Ends the page: its markdown.
    pub(super) fn finish(mut self) -> String {
        if self.decoding.is_none() {
            self.start();
        }
        self.feed(&[], true);
        self.stream.finish()
    }

    /// Chooses the character set from the held bytes, then converts them.
    fn start(&mut self) {
        let encoding = charset::encoding(self.content_type.as_deref(), &self.held);
        self.decoding = Some(Decoding::new(encoding));
        let held = std::mem::take(&mut self.held);
        self.feed(&held, false);
    }

    fn feed(&mut self, bytes: &[u8], last: bool) {
        let Some(decoding) = &mut self.decoding else {
            return;
        };
        let stream = &mut self.stream;
        decoding.push(bytes, last, &mut |text| stream.push(text));
    }
}
