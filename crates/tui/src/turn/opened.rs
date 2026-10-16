//! An opened call's rows under its ledger row, and the diff renderer the
//! changed-files view and the offer share (`docs/tui.md`, "Tool groups and
//! the ledger").

/// One drawn diff line: its line number when a hunk header numbers it,
/// its kind, and its text cut into plain and marked pieces. The pieces'
/// concatenation is the printed line, hunk headers dropped from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiffLine {
    pub(crate) number: Option<u64>,
    pub(crate) kind: DiffKind,
    pub(crate) segments: Vec<(String, bool)>,
}

/// What a diff line is: its hunk header, a removed or added line, a
/// context line, or the "no newline" marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiffKind {
    Hunk,
    Removed,
    Added,
    Context,
    Note,
}

/// The diff as drawn lines (`docs/tui.md`, "Tool groups and the ledger").
/// Lines before the first `@@` are the diff's own headers and are
/// dropped; a diff with no `@@` draws all its lines plain with no gutter.
/// Numbers come from `@@ -a[,b] +c[,d] @@`: `-` shows old and advances
/// old, `+` shows new and advances new, context shows new and advances
/// both. A header whose numbers do not parse as `u64` draws as a hunk
/// row, and the lines after it draw unnumbered until the next parseable
/// header. `\ No newline at end of file` draws unnumbered and advances
/// nothing. Only a run of exactly one removed line directly followed by
/// exactly one added line marks the words that differ. Every step on a
/// number saturates, so no header overflows.
pub(crate) fn diff_lines(diff: &str) -> Vec<DiffLine> {
    let mut lines = Vec::new();
    let mut numbers: Option<(u64, u64)> = None;
    let mut seen_hunk = false;
    for line in diff.lines() {
        if line.starts_with("@@") {
            seen_hunk = true;
            numbers = parse_hunk(line);
            lines.push(DiffLine {
                number: None,
                kind: DiffKind::Hunk,
                segments: vec![(safe_line(line), false)],
            });
        } else if !seen_hunk {
            continue;
        } else if line.starts_with('\\') {
            lines.push(DiffLine {
                number: None,
                kind: DiffKind::Note,
                segments: vec![(safe_line(line), false)],
            });
        } else if line.starts_with('-') {
            let number = numbers.map(|(old, _)| old);
            numbers = numbers.map(|(old, new)| (old.saturating_add(1), new));
            lines.push(DiffLine {
                number,
                kind: DiffKind::Removed,
                segments: vec![(safe_line(line), false)],
            });
        } else if line.starts_with('+') {
            let number = numbers.map(|(_, new)| new);
            numbers = numbers.map(|(old, new)| (old, new.saturating_add(1)));
            lines.push(DiffLine {
                number,
                kind: DiffKind::Added,
                segments: vec![(safe_line(line), false)],
            });
        } else {
            let number = numbers.map(|(_, new)| new);
            numbers = numbers.map(|(old, new)| {
                (old.saturating_add(1), new.saturating_add(1))
            });
            lines.push(DiffLine {
                number,
                kind: DiffKind::Context,
                segments: vec![(safe_line(line), false)],
            });
        }
    }
    if !seen_hunk {
        return diff
            .lines()
            .map(|line| DiffLine {
                number: None,
                kind: DiffKind::Context,
                segments: vec![(safe_line(line), false)],
            })
            .collect();
    }
    pair_runs(&mut lines);
    lines
}

/// The old and new start lines of `@@ -a[,b] +c[,d] @@`, or none when
/// either does not parse as a `u64`.
fn parse_hunk(line: &str) -> Option<(u64, u64)> {
    let rest = line.strip_prefix("@@")?.trim_start();
    let (old, rest) = rest.split_once(' ')?;
    let (new, _) = rest.split_once(' ')?;
    let old = old
        .strip_prefix('-')?
        .split(',')
        .next()?
        .parse::<u64>()
        .ok()?;
    let new = new
        .strip_prefix('+')?
        .split(',')
        .next()?
        .parse::<u64>()
        .ok()?;
    Some((old, new))
}

/// Marks the differing words in every run of exactly one removed line
/// directly followed by exactly one added line. A run is the maximal
/// block of consecutive removed lines then consecutive added lines, so a
/// removed line after an added one starts a new run.
fn pair_runs(lines: &mut [DiffLine]) {
    let mut block: Vec<usize> = Vec::new();
    let mut saw_added = false;
    for index in 0..lines.len() {
        let kind = lines
            .get(index)
            .map(|line| line.kind)
            .unwrap_or(DiffKind::Note);
        match kind {
            DiffKind::Removed => {
                if saw_added {
                    flush_block(&mut block, lines);
                    saw_added = false;
                }
                block.push(index);
            }
            DiffKind::Added => {
                saw_added = true;
                block.push(index);
            }
            DiffKind::Hunk | DiffKind::Context | DiffKind::Note => {
                flush_block(&mut block, lines);
                saw_added = false;
            }
        }
    }
    flush_block(&mut block, lines);
}

/// Marks one finished block when it holds exactly one removed line then
/// one added line. The two indices arrive in order, the removed first:
///
/// a `-` after a `+` flushes before it is pushed, so a block longer than
/// two never holds the 1-and-1 shape.
fn flush_block(block: &mut Vec<usize>, lines: &mut [DiffLine]) {
    if let [removed_at, added_at] = block.as_slice() {
        let (removed_at, added_at) = (*removed_at, *added_at);
        if removed_at < added_at
            && let (head, tail) = lines.split_at_mut(added_at)
            && let (Some(removed), Some(added)) =
                (head.get_mut(removed_at), tail.first_mut())
        {
            pair_words(removed, added);
        }
    }
    block.clear();
}

/// A word's text, or none when its range left the line.
fn word_text<'a>(line: &'a str, word: &(usize, usize)) -> Option<&'a str> {
    line.get(word.0..word.1)
}

/// Marks the words between the common leading and trailing words of a
/// removed and an added line, both lines keeping their spacing. Lines
/// whose words match, however the spacing differs, mark nothing.
fn pair_words(removed: &mut DiffLine, added: &mut DiffLine) {
    let Some(removed_rest) = rest_of(&removed.segments) else {
        return;
    };
    let Some(added_rest) = rest_of(&added.segments) else {
        return;
    };
    let removed_words = word_ranges(&removed_rest);
    let added_words = word_ranges(&added_rest);
    let mut leading = 0;
    while let (Some(left), Some(right)) =
        (removed_words.get(leading), added_words.get(leading))
        && word_text(&removed_rest, left) == word_text(&added_rest, right)
    {
        leading += 1;
    }
    let mut trailing = 0;
    loop {
        let (Some(left_at), Some(right_at)) = (
            removed_words.len().checked_sub(1 + trailing),
            added_words.len().checked_sub(1 + trailing),
        ) else {
            break;
        };
        if left_at < leading || right_at < leading {
            break;
        }
        let (Some(left), Some(right)) = (
            removed_words.get(left_at),
            added_words.get(right_at),
        ) else {
            break;
        };
        if word_text(&removed_rest, left) != word_text(&added_rest, right) {
            break;
        }
        trailing += 1;
    }
    if removed_words.len() <= leading + trailing
        && added_words.len() <= leading + trailing
    {
        return;
    }
    mark_range(removed, &removed_rest, &removed_words, leading, trailing);
    mark_range(added, &added_rest, &added_words, leading, trailing);
}

/// A paired line's text after its `-` or `+` marker.
fn rest_of(segments: &[(String, bool)]) -> Option<String> {
    let line: String = segments.iter().map(|(text, _)| text.as_str()).collect();
    line
        .strip_prefix('-')
        .or_else(|| line.strip_prefix('+'))
        .map(str::to_owned)
}

/// Cuts a paired line into its marker plus its words, each word carrying
/// the gap after it, flagged between the common ends.
fn mark_range(
    line: &mut DiffLine,
    rest: &str,
    words: &[(usize, usize)],
    leading: usize,
    trailing: usize,
) {
    let mut pieces: Vec<(String, bool)> = Vec::new();
    if let Some(first) = words.first()
        && first.0 > 0
        && let Some(gap) = rest.get(..first.0)
    {
        pieces.push((gap.to_owned(), false));
    }
    for (index, word) in words.iter().enumerate() {
        let end = words
            .get(index + 1)
            .map(|next| next.0)
            .unwrap_or_else(|| rest.len());
        let marked = index >= leading && index + trailing < words.len();
        if let (Some(text), Some(gap)) = (rest.get(word.0..word.1), rest.get(word.1..end)) {
            pieces.push((format!("{text}{gap}"), marked));
        }
    }
    if pieces.is_empty() {
        return;
    }
    let marker = line
        .segments
        .iter()
        .map(|(text, _)| text.as_str())
        .collect::<String>()
        .chars()
        .next()
        .unwrap_or(' ');
    if let Some(first) = pieces.first_mut() {
        first.0.insert(0, marker);
    }
    line.segments = pieces;
}

/// The byte ranges of the words in `text`: runs of non-whitespace.
fn word_ranges(text: &str) -> Vec<(usize, usize)> {
    let mut words = Vec::new();
    let mut start: Option<usize> = None;
    for (index, ch) in text.char_indices() {
        if ch.is_whitespace() {
            if let Some(from) = start.take() {
                words.push((from, index));
            }
        } else if start.is_none() {
            start = Some(index);
        }
    }
    if let Some(from) = start {
        words.push((from, text.len()));
    }
    words
}

/// How wide a gutter holding `numbers` runs: the digit count of the
/// largest, right-aligned; none at all when there is no number. Zero has
/// one digit; the count is computed, never added to the number.
pub(crate) fn gutter_width(numbers: impl Iterator<Item = u64>) -> usize {
    numbers
        .map(|n| {
            n.checked_ilog10()
                .map(|digits| digits as usize + 1)
                .unwrap_or(1)
        })
        .max()
        .unwrap_or(0)
}

/// Removes terminal controls from a line; tabs use four spaces so diff
/// and file content cannot send terminal escapes through the view.
pub(crate) fn safe_line(line: &str) -> String {
    let mut safe = String::with_capacity(line.len());
    for ch in line.chars() {
        if ch == '\t' {
            safe.push_str("    ");
        } else if !ch.is_control() {
            safe.push(ch);
        }
    }
    safe
}

#[cfg(test)]
#[path = "opened_tests.rs"]
mod tests;
