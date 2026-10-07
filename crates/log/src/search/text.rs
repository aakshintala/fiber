//! The query's matchers and the snippet (`docs/tools.md`, "Searching past
//! sessions"). Every matcher matches literals with the regex crate's Unicode
//! simple case folding, so the raw pass, the decoded strings and the
//! artifacts agree on what matches.

use grep_matcher::Matcher as _;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};

/// How many characters a snippet shows before the match's start.
const BEFORE: usize = 100;
/// How many characters a snippet shows in all.
const WINDOW: usize = 200;
/// What marks a snippet's end that was cut.
const CUT: char = '…';

/// The query in the forms the scan matches.
pub(super) struct Text {
    /// The query as the log escapes it inside a JSON string.
    escaped: String,
    /// The query, matched against decoded strings.
    decoded: RegexMatcher,
    /// The query, matched against an artifact's bytes: line by line, or
    /// across lines when the query holds a newline.
    artifact: RegexMatcher,
    /// Whether the query holds a newline.
    multi_line: bool,
}

impl Text {
    /// The matchers for `text`, or why they could not be built.
    pub(super) fn new(text: &str) -> Result<Self, String> {
        let multi_line = text.contains('\n');
        let decoded = literals(None).build_literals(&[text]).map_err(reason)?;
        // A matcher with a line terminator refuses a pattern holding it.
        let terminator = if multi_line { None } else { Some(b'\n') };
        let artifact = literals(terminator)
            .build_literals(&[text])
            .map_err(reason)?;
        Ok(Self {
            escaped: escaped(text),
            decoded,
            artifact,
            multi_line,
        })
    }

    /// The raw pass's matcher for one log: the escaped query, the starts of
    /// the lines that name the session, and `extra`, each already as the log
    /// escapes it. No literal holds a raw newline, so the matcher keeps the
    /// line terminator.
    pub(super) fn raw(&self, extra: &[String]) -> Result<RegexMatcher, String> {
        let mut all: Vec<&str> = vec![
            &self.escaped,
            r#"{"kind":"session_named""#,
            r#"{"kind":"turn_started""#,
        ];
        all.extend(extra.iter().map(String::as_str));
        literals(Some(b'\n')).build_literals(&all).map_err(reason)
    }

    /// Where the query first matches in `decoded`, as byte offsets.
    pub(super) fn find(&self, decoded: &str) -> Option<(usize, usize)> {
        let found = self.decoded.find(decoded.as_bytes()).ok()??;
        Some((found.start(), found.end()))
    }

    /// The matcher for an artifact's bytes.
    pub(super) fn artifact(&self) -> &RegexMatcher {
        &self.artifact
    }

    /// Whether an artifact is searched across lines.
    pub(super) fn multi_line(&self) -> bool {
        self.multi_line
    }
}

/// A builder for literals matched ignoring case, with `terminator` as the
/// line terminator.
fn literals(terminator: Option<u8>) -> RegexMatcherBuilder {
    let mut builder = RegexMatcherBuilder::new();
    builder
        .fixed_strings(true)
        .case_insensitive(true)
        .unicode(true)
        .line_terminator(terminator);
    builder
}

/// A matcher's build failure as a sentence.
fn reason(error: grep_regex::Error) -> String {
    format!("cannot search for this text: {error}")
}

/// `text` as the log writes it inside a JSON string: the log's own encoder
/// with the quotes taken off, so every escape it writes (`\"`, `\\`, `\n`,
/// `\t`, `\u001b`, ...) is the one matched.
pub(super) fn escaped(text: &str) -> String {
    let quoted = serde_json::to_string(text).unwrap_or_default();
    quoted
        .strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .unwrap_or_default()
        .to_owned()
}

/// About [`WINDOW`] characters of `text` around the match that starts at
/// byte offset `start`: up to [`BEFORE`] characters before the match's start, then
/// up to [`WINDOW`] characters in all from there. Each end that was cut is
/// marked with `…`. Counted in characters, never splitting one.
pub(super) fn snippet(text: &str, start: usize) -> String {
    let before = text.get(..start).unwrap_or_default().chars().count();
    let skip = before.saturating_sub(BEFORE);
    let from = text
        .char_indices()
        .nth(skip)
        .map_or(text.len(), |(index, _)| index);
    let to = text
        .char_indices()
        .nth(skip + WINDOW)
        .map_or(text.len(), |(index, _)| index);
    let mut out = String::new();
    if from > 0 {
        out.push(CUT);
    }
    out.push_str(text.get(from..to).unwrap_or_default());
    if to < text.len() {
        out.push(CUT);
    }
    out
}

#[cfg(test)]
#[path = "text_tests.rs"]
mod tests;
