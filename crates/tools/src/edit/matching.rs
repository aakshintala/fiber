//! How `edit` finds each block and splices it in (`docs/tools.md`, "edit").
//!
//! The search runs on an LF view: the byte order mark is set aside and each
//! `\r\n` is one newline. A lone `\r` is ordinary text. The view keeps an
//! offset for every byte because a folded character can be shorter than the
//! bytes it came from (an em dash is three bytes and folds to one) and a
//! CRLF pair is two bytes seen as one. Untouched lines are copied from the
//! original bytes, so a mixed-ending file keeps those endings.

use crate::files::land::shape_lines;
use crate::write::line_count;

const BOM: &[u8] = b"\xEF\xBB\xBF";

/// One `{ old_text, new_text }` block, in the order the model sent it.
pub(crate) struct Block {
    pub old_text: String,
    pub new_text: String,
}

/// Where one block landed. Line numbers are 1-based.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Report {
    /// Index in the `edits` list.
    pub index: usize,
    /// First original line the match touched.
    pub old_start: u64,
    /// Last original line the match touched.
    pub old_end: u64,
    /// Lines the new text occupies in the written file. `None` when it
    /// occupies none, which is a deletion that leaves no line.
    pub new_span: Option<(u64, u64)>,
    /// The block matched only after folding quotes, dashes and spaces.
    pub normalised: bool,
}

/// The file after every block, plus one report per block in the order sent.
#[derive(Debug)]
pub(crate) struct Applied {
    pub bytes: Vec<u8>,
    pub reports: Vec<Report>,
}

/// Why the blocks were not applied. Nothing is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MatchError {
    /// `old_text` does not occur.
    NoMatch {
        /// Index in the `edits` list.
        index: usize,
    },
    /// `old_text` occurs more than once in the form it was matched in.
    Ambiguous {
        /// Index in the `edits` list.
        index: usize,
        /// How many non-overlapping matches.
        count: usize,
    },
    /// Empty input, an overlap, or a no-op.
    Invalid(String),
    /// A match offset was not a character boundary. Nothing is written.
    Boundary,
}

impl MatchError {
    pub(crate) fn message(&self) -> String {
        match self {
            Self::NoMatch { index } => format!(
                "edits[{index}]: old_text was not found. Read the file again and copy the text exactly."
            ),
            Self::Ambiguous { index, count } => format!(
                "edits[{index}]: old_text matched {count} times. Read the file again and include more surrounding text."
            ),
            Self::Invalid(message) => message.clone(),
            Self::Boundary => {
                "The edit could not be applied: a match offset was not a character boundary."
                    .to_owned()
            }
        }
    }
}

struct Located {
    index: usize,
    lf_start: usize,
    lf_end: usize,
    body_start: usize,
    body_end: usize,
    replacement: String,
    normalised: bool,
    newline_delta: i64,
}

/// Applies every block to `text` or applies none.
///
/// `text` is the file as read, byte order mark included. Each block is
/// matched against that text, not against the output of an earlier block.
pub(crate) fn apply(text: &str, blocks: &[Block]) -> Result<Applied, MatchError> {
    if blocks.is_empty() {
        return Err(MatchError::Invalid(
            "`edits` must contain at least one block.".to_owned(),
        ));
    }
    let (had_bom, body) = split_bom(text);
    let (view, origin) = lf_view(body);
    let mut located = Vec::with_capacity(blocks.len());
    for (index, block) in blocks.iter().enumerate() {
        let old = lf_normalise(&block.old_text);
        let new = lf_normalise(&block.new_text);
        if old.is_empty() {
            return Err(MatchError::Invalid(format!(
                "edits[{index}]: old_text is empty."
            )));
        }
        located.push(place(index, &view, &origin, &old, &new)?);
    }
    reject_overlap(&located)?;
    let spliced = splice(body.as_bytes(), &located);
    let mut bytes = Vec::with_capacity(BOM.len() + spliced.len());
    if had_bom {
        bytes.extend_from_slice(BOM);
    }
    bytes.extend_from_slice(&spliced);
    if bytes == text.as_bytes() {
        return Err(MatchError::Invalid(
            "The edits leave the file unchanged.".to_owned(),
        ));
    }
    Ok(Applied {
        reports: reports(&view, &located)?,
        bytes,
    })
}

fn split_bom(text: &str) -> (bool, &str) {
    match text.strip_prefix('\u{feff}') {
        Some(rest) => (true, rest),
        None => (false, text),
    }
}

fn lf_normalise(text: &str) -> String {
    text.replace("\r\n", "\n")
}

/// LF view of `body` and, for each view byte plus a final sentinel, the body
/// byte where it begins. A `\n` that stands for `\r\n` begins at the `\r`.
fn lf_view(body: &str) -> (String, Vec<usize>) {
    let mut text = String::with_capacity(body.len());
    let mut origin = Vec::with_capacity(body.len() + 1);
    let mut chars = body.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        if ch == '\r' && chars.next_if(|(_, next)| *next == '\n').is_some() {
            origin.push(index);
            text.push('\n');
            continue;
        }
        let len = ch.len_utf8();
        for offset in 0..len {
            origin.push(index + offset);
        }
        text.push(ch);
    }
    origin.push(body.len());
    (text, origin)
}

fn place(
    index: usize,
    view: &str,
    origin: &[usize],
    old: &str,
    new: &str,
) -> Result<Located, MatchError> {
    let (count, first) = hits(view, old);
    if count > 1 {
        return Err(MatchError::Ambiguous { index, count });
    }
    if count == 1 {
        let end = first + old.len();
        return located(index, view, origin, first, end, new.to_owned(), false);
    }
    let (folded, fold_origin) = fold_view(view);
    let needle = fold_view(old).0;
    if needle.is_empty() {
        return Err(MatchError::NoMatch { index });
    }
    let (count, first) = hits(&folded, &needle);
    if count > 1 {
        return Err(MatchError::Ambiguous { index, count });
    }
    if count == 0 {
        return Err(MatchError::NoMatch { index });
    }
    let match_end = first + needle.len();
    let (line_start, line_end) = line_bounds(&folded, first, match_end)?;
    let prefix = slice(&folded, line_start, first)?;
    let suffix = slice(&folded, match_end, line_end)?;
    let mut replacement = String::with_capacity(prefix.len() + new.len() + suffix.len());
    replacement.push_str(prefix);
    replacement.push_str(new);
    replacement.push_str(suffix);
    let (start, end) = map_span(&fold_origin, line_start, line_end)?;
    located(index, view, origin, start, end, replacement, true)
}

fn located(
    index: usize,
    view: &str,
    origin: &[usize],
    lf_start: usize,
    lf_end: usize,
    replacement: String,
    normalised: bool,
) -> Result<Located, MatchError> {
    let (body_start, body_end) = map_span(origin, lf_start, lf_end)?;
    let spanned = slice(view, lf_start, lf_end)?;
    Ok(Located {
        index,
        lf_start,
        lf_end,
        body_start,
        body_end,
        newline_delta: newline_delta(spanned, &replacement),
        replacement,
        normalised,
    })
}

/// Every occurrence of `needle`, overlapping ones included: the search
/// resumes one character after each match start. `(count, first byte)`.
/// `needle` is never empty.
fn hits(haystack: &str, needle: &str) -> (usize, usize) {
    let mut count = 0;
    let mut first = 0;
    let mut from = 0;
    while let Some(at) = haystack[from..].find(needle) {
        let start = from + at;
        if count == 0 {
            first = start;
        }
        count += 1;
        from = start + haystack[start..].chars().next().map_or(1, char::len_utf8);
    }
    (count, first)
}

/// Fold used by the second pass. Trailing spaces are dropped after Unicode
/// spaces have become ASCII spaces, so a trailing NBSP is ignored too. The
/// origin vector maps each folded byte back to the LF view.
fn fold_view(text: &str) -> (String, Vec<usize>) {
    let mut out = String::with_capacity(text.len());
    let mut origin = Vec::with_capacity(text.len() + 1);
    let mut input = 0usize;
    for piece in text.split_inclusive('\n') {
        let has_newline = piece.ends_with('\n');
        let line = match piece.strip_suffix('\n') {
            Some(line) => line,
            None => piece,
        };
        let mut folded = String::with_capacity(line.len());
        let mut folded_origin = Vec::with_capacity(line.len());
        for (byte, ch) in line.char_indices() {
            let start = input + byte;
            let mapped = fold_char(ch);
            for _ in 0..mapped.len_utf8() {
                folded_origin.push(start);
            }
            folded.push(mapped);
        }
        let kept = folded.trim_end_matches(' ');
        out.push_str(kept);
        origin.extend(folded_origin.into_iter().take(kept.len()));
        input += line.len();
        if has_newline {
            origin.push(input);
            out.push('\n');
            input += 1;
        }
    }
    origin.push(text.len());
    (out, origin)
}

fn fold_char(ch: char) -> char {
    match ch {
        '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
        '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
        '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
        '\u{00A0}' | '\u{2002}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => ' ',
        other => other,
    }
}

/// The line containing `[start, end)`. `end` is exclusive and the match is
/// non-empty, so the line runs through the newline that contains byte
/// `end - 1`, or to the end of `text`. That byte can sit inside a character,
/// so the search is over bytes.
fn line_bounds(text: &str, start: usize, end: usize) -> Result<(usize, usize), MatchError> {
    let bytes = text.as_bytes();
    // `start < end` already rules out `end == 0` and `start > len` once `end <= len`.
    if end > bytes.len() || start >= end {
        return Err(MatchError::Boundary);
    }
    let line_start = match bytes.get(..start) {
        Some(prefix) => match prefix.iter().rposition(|&byte| byte == b'\n') {
            Some(index) => index + 1,
            None => 0,
        },
        None => return Err(MatchError::Boundary),
    };
    let last = end - 1;
    let line_end = match bytes.get(last..) {
        Some(rest) => match rest.iter().position(|&byte| byte == b'\n') {
            Some(offset) => last + offset + 1,
            None => bytes.len(),
        },
        None => return Err(MatchError::Boundary),
    };
    Ok((line_start, line_end))
}

fn reject_overlap(located: &[Located]) -> Result<(), MatchError> {
    let mut ordered: Vec<&Located> = located.iter().collect();
    ordered.sort_by_key(|item| (item.body_start, item.index));
    let mut previous: Option<&Located> = None;
    for item in ordered {
        if let Some(before) = previous
            && item.body_start < before.body_end
        {
            let left = before.index.min(item.index);
            let right = before.index.max(item.index);
            return Err(MatchError::Invalid(format!(
                "edits[{left}] and edits[{right}] overlap."
            )));
        }
        previous = Some(item);
    }
    Ok(())
}

fn splice(body: &[u8], located: &[Located]) -> Vec<u8> {
    let mut ordered: Vec<&Located> = located.iter().collect();
    ordered.sort_by_key(|item| item.body_start);
    let mut out = Vec::with_capacity(body.len());
    let mut cursor = 0usize;
    for item in ordered {
        // Overlap was rejected, so `cursor` is not past `body_start`.
        if let Some(gap) = body.get(cursor..item.body_start) {
            out.extend_from_slice(gap);
        }
        out.extend(shape_lines(body, &item.replacement));
        cursor = item.body_end;
    }
    if let Some(tail) = body.get(cursor..) {
        out.extend_from_slice(tail);
    }
    out
}

fn reports(view: &str, located: &[Located]) -> Result<Vec<Report>, MatchError> {
    let mut out = Vec::with_capacity(located.len());
    for item in located {
        let shift: i64 = located
            .iter()
            .filter(|other| other.body_end <= item.body_start)
            .map(|other| other.newline_delta)
            .sum();
        let old_start = line_at(view, item.lf_start)?;
        let old_end = line_at(view, item.lf_end - 1)?;
        let start = old_start.saturating_add_signed(shift);
        let occupied = line_count(item.replacement.as_bytes());
        let new_span = if occupied == 0 {
            None
        } else {
            Some((start, start + occupied - 1))
        };
        out.push(Report {
            index: item.index,
            old_start,
            old_end,
            new_span,
            normalised: item.normalised,
        });
    }
    Ok(out)
}

/// 1-based line of `byte`. The offset may sit inside a character, so the cut
/// moves back to a boundary first: a newline is one byte and cannot hide in
/// the tail of a character. A cut that does not land is an error.
fn line_at(text: &str, byte: usize) -> Result<u64, MatchError> {
    let end = text.floor_char_boundary(byte);
    let prefix = text.get(..end).ok_or(MatchError::Boundary)?;
    let newlines = prefix.matches('\n').count();
    Ok(u64::try_from(newlines)
        .unwrap_or(u64::MAX)
        .saturating_add(1))
}

fn newline_delta(old: &str, new: &str) -> i64 {
    newlines(new) - newlines(old)
}

fn newlines(text: &str) -> i64 {
    i64::try_from(text.matches('\n').count()).unwrap_or(i64::MAX)
}

fn map_span(origin: &[usize], start: usize, end: usize) -> Result<(usize, usize), MatchError> {
    Ok((at(origin, start)?, at(origin, end)?))
}

fn at(origin: &[usize], index: usize) -> Result<usize, MatchError> {
    origin.get(index).copied().ok_or(MatchError::Boundary)
}

fn slice(text: &str, start: usize, end: usize) -> Result<&str, MatchError> {
    text.get(start..end).ok_or(MatchError::Boundary)
}

#[cfg(test)]
#[path = "matching_tests.rs"]
mod tests;
