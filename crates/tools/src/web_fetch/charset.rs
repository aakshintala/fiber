//! Charset decoding for `web_fetch` (`docs/tools.md`, "HTML to markdown").
//! WHATWG precedence: a BOM, then the `Content-Type` header's `charset`,
//! then a `<meta>` in the first 1024 bytes, then UTF-8.

#[cfg(test)]
use std::borrow::Cow;
use std::cell::RefCell;

use encoding_rs::{CoderResult, Decoder, Encoding};
use html5ever::tokenizer::{
    BufferQueue, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
};

/// How many leading bytes the `<meta>` prescan reads, lossily as UTF-8.
pub(super) const PRESCAN: usize = 1024;

/// The room the decoder writes into before it hands its text over.
const OUT: usize = 65_536;

/// The declared character set of a page whose first bytes are `head`: the
/// `Content-Type` header's `charset`, then a `<meta>` in the first 1024
/// bytes, then UTF-8. A label `encoding_rs` does not know is skipped for
/// the next source. A BOM outranks all three; the decoder sniffs it.
pub(crate) fn encoding(content_type: Option<&str>, head: &[u8]) -> &'static Encoding {
    content_type
        .and_then(header_encoding)
        .or_else(|| meta_encoding(head))
        .unwrap_or(encoding_rs::UTF_8)
}

/// Decodes `bytes` whole by its declared character set ([`encoding`]),
/// the BOM first. Never fails: unknown bytes become `�`. A BOM is removed.
/// Borrows when UTF-8 is chosen and the bytes after any BOM are valid
/// UTF-8. [`Decoding`] gives the same text piece by piece.
#[cfg(test)]
pub(crate) fn decode<'a>(content_type: Option<&str>, bytes: &'a [u8]) -> Cow<'a, str> {
    encoding(content_type, bytes).decode(bytes).0
}

/// Decodes a page arriving in pieces. Whatever the pieces, the text handed
/// over, joined, is the whole page decoded at once: a BOM still wins and is
/// removed, a character cut across pieces is decoded once whole, and bytes
/// that are not valid become `�`. Each handed-over piece is at most 64 KiB.
pub(crate) struct Decoding {
    decoder: Decoder,
    out: String,
}

impl Decoding {
    pub(crate) fn new(encoding: &'static Encoding) -> Self {
        Self {
            decoder: encoding.new_decoder(),
            out: String::with_capacity(OUT),
        }
    }

    /// Decodes `bytes`, handing the text to `out` as the room fills. `last`
    /// is true once, for the final piece, which may be empty: it flushes a
    /// character the page ends inside of as `�`.
    pub(crate) fn push(&mut self, mut bytes: &[u8], last: bool, out: &mut dyn FnMut(&str)) {
        loop {
            let (result, read, _) = self.decoder.decode_to_string(bytes, &mut self.out, last);
            bytes = bytes.get(read..).unwrap_or_default();
            out(&self.out);
            self.out.clear();
            match result {
                CoderResult::InputEmpty => return,
                // The room is full: hand it over and go on.
                CoderResult::OutputFull => {}
            }
        }
    }
}

/// The `charset` parameter of a `Content-Type` value: split on `;`, the name
/// matched ASCII case-insensitively after trimming, the value trimmed with
/// one pair of surrounding double quotes removed. Unknown labels are `None`,
/// so the next source is tried.
fn header_encoding(content_type: &str) -> Option<&'static Encoding> {
    let mut params = content_type.split(';');
    // The media type itself carries no charset.
    params.next()?;
    for param in params {
        let Some((name, value)) = param.split_once('=') else {
            continue;
        };
        if !name.trim().eq_ignore_ascii_case("charset") {
            continue;
        }
        let mut value = value.trim();
        if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
            value = value.get(1..value.len() - 1).unwrap_or_default();
        }
        let value = value.trim();
        if let Some(encoding) = Encoding::for_label(value.as_bytes()) {
            return Some(encoding);
        }
    }
    None
}

/// The first known `<meta>` charset in the first 1024 bytes, read lossily
/// as UTF-8 (the labels are ASCII). A `<meta>` naming UTF-16LE/BE means
/// UTF-8, and `x-user-defined` means windows-1252 (the WHATWG prescan
/// rule); the header's UTF-16 is honoured, so only this path maps.
fn meta_encoding(bytes: &[u8]) -> Option<&'static Encoding> {
    let end = PRESCAN.min(bytes.len());
    let head = String::from_utf8_lossy(bytes.get(..end).unwrap_or_default());
    let labels = meta_labels(&head);
    for label in &labels {
        if label.eq_ignore_ascii_case("x-user-defined") {
            return Some(encoding_rs::WINDOWS_1252);
        }
        if let Some(encoding) = Encoding::for_label(label.as_bytes()) {
            if encoding == encoding_rs::UTF_16LE || encoding == encoding_rs::UTF_16BE {
                return Some(encoding_rs::UTF_8);
            }
            return Some(encoding);
        }
    }
    None
}

/// Every candidate `<meta>` charset label in the head, in order: the first
/// `meta` start tag carrying a `charset` attribute, or an `http-equiv` of
/// `content-type` (ASCII case-insensitive) whose `content` holds
/// `charset=<label>`. A `<meta>` inside a comment is a comment token, so it
/// is never taken.
fn meta_labels(head: &str) -> Vec<String> {
    let found = RefCell::new(Vec::new());
    struct Sink<'a> {
        labels: &'a RefCell<Vec<String>>,
    }
    impl TokenSink for Sink<'_> {
        type Handle = ();
        fn process_token(&self, token: Token, _line: u64) -> TokenSinkResult<Self::Handle> {
            let Token::TagToken(tag) = token else {
                return TokenSinkResult::Continue;
            };
            if tag.kind != TagKind::StartTag || &*tag.name != "meta" {
                return TokenSinkResult::Continue;
            }
            let mut charset: Option<String> = None;
            let mut http_equiv: Option<String> = None;
            let mut content: Option<String> = None;
            for attr in &tag.attrs {
                if &*attr.name.local == "charset" {
                    charset = Some(attr.value.to_string());
                } else if &*attr.name.local == "http-equiv" {
                    http_equiv = Some(attr.value.to_string());
                } else if &*attr.name.local == "content" {
                    content = Some(attr.value.to_string());
                }
            }
            if let Some(label) = charset {
                self.labels.borrow_mut().push(label);
            } else if http_equiv.is_some_and(|h| h.eq_ignore_ascii_case("content-type"))
                && let Some(label) = content_charset(content.as_deref().unwrap_or_default())
            {
                self.labels.borrow_mut().push(label);
            }
            TokenSinkResult::Continue
        }
    }
    let sink = Sink { labels: &found };
    let tokenizer = Tokenizer::new(sink, TokenizerOpts::default());
    let queue = BufferQueue::default();
    queue.push_back(head.into());
    let _feed = tokenizer.feed(&queue);
    tokenizer.end();
    found.into_inner()
}

/// The `charset=<label>` inside a `content` attribute value: optionally
/// quoted, ending at `;`, a quote or whitespace.
fn content_charset(content: &str) -> Option<String> {
    let lower = content.to_ascii_lowercase();
    let mut rest = lower.as_str();
    loop {
        let (_, after) = rest.split_once("charset")?;
        // The name must end here: `charsetx=` names nothing.
        let boundary = after.chars().next();
        if boundary.is_some_and(|c| matches!(c, 'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_')) {
            rest = after;
            continue;
        }
        let value = after.trim_start();
        // The `=` is required: `charset shift_jis` names nothing.
        let Some(value) = value.strip_prefix('=') else {
            rest = after;
            continue;
        };
        let value = value.trim_start();
        if value.is_empty() {
            return None;
        }
        // Strip one optional quote, then one end: `;`, a quote or
        // whitespace ends the label either way.
        let value = value.strip_prefix(['"', '\'']).unwrap_or(value);
        let end = value
            .find(|c: char| c == ';' || c == '"' || c == '\'' || c.is_ascii_whitespace())
            .unwrap_or(value.len());
        let label = value.get(..end).unwrap_or_default().trim();
        if label.is_empty() {
            return None;
        }
        // Map back to the original case: labels are ASCII.
        return Some(label.to_owned());
    }
}

#[cfg(test)]
#[path = "charset_tests.rs"]
mod tests;
