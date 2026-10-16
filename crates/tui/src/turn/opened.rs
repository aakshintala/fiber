//! An opened call's rows under its ledger row, and the diff renderer the
//! changed-files view and the offer share (`docs/tui.md`, "Tool groups and
//! the ledger").

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::{Map, Value};

use super::group::{Call, dim_span, fit};
use crate::app::Target;
use crate::format::{count, dim};
use crate::markdown::style;
use crate::rows::{RowText, Rows};
use crate::theme::Role;

/// An opened call shows at most this many logical lines, then a dim
/// "… N more lines (Enter to view)" row (`docs/tui.md`, "Tool groups
/// and the ledger").
pub(crate) const OPENED_LINES: usize = 20;

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
            numbers = numbers.map(|(old, new)| (old.saturating_add(1), new.saturating_add(1)));
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
            && let (Some(removed), Some(added)) = (head.get_mut(removed_at), tail.first_mut())
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
    while let (Some(left), Some(right)) = (removed_words.get(leading), added_words.get(leading))
        && word_text(&removed_rest, left) == word_text(&added_rest, right)
    {
        leading += 1;
    }
    let mut trailing = 0;
    while let (Some(left_at), Some(right_at)) = (
        removed_words.len().checked_sub(1 + trailing),
        added_words.len().checked_sub(1 + trailing),
    ) {
        if left_at < leading || right_at < leading {
            break;
        }
        let (Some(left), Some(right)) = (removed_words.get(left_at), added_words.get(right_at))
        else {
            break;
        };
        if word_text(&removed_rest, left) != word_text(&added_rest, right) {
            break;
        }
        trailing += 1;
    }
    if removed_words.len() <= leading + trailing && added_words.len() <= leading + trailing {
        return;
    }
    mark_range(removed, &removed_rest, &removed_words, leading, trailing);
    mark_range(added, &added_rest, &added_words, leading, trailing);
}

/// A paired line's text after its `-` or `+` marker.
fn rest_of(segments: &[(String, bool)]) -> Option<String> {
    let line: String = segments.iter().map(|(text, _)| text.as_str()).collect();
    line.strip_prefix('-')
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

/// What an opened call shows, chosen from the shape of its `details`,
/// never the tool name: an edit's diff, a read's lines, a shell `grep`'s
/// matches, a shell's command and output, a write's `content` argument,
/// or the content as plain text (`docs/tui.md`, "Tool groups and the
/// ledger").
enum View<'a> {
    Diff(&'a str),
    Read {
        start: u64,
        text: &'a str,
    },
    Matches {
        groups: &'a [Value],
        end: End<'a>,
    },
    Shell {
        command: &'a str,
        end: End<'a>,
        output: &'a str,
    },
    Written(&'a str),
    Plain(&'a str),
}

/// How a shell call or a search ended, from its `details`.
enum End<'a> {
    Exit(i64),
    Signal(&'a str),
    None,
}

/// One logical line of the opened view: its number when a gutter names
/// it, whether it stands inside a numbered view, and its text.
struct Body {
    number: Option<u64>,
    gutter: bool,
    line: Line<'static>,
}

/// The call's opened rows under its ledger row: its headline first,
/// then the view its `details` choose, at most [`OPENED_LINES`] logical
/// lines, then at most one dim more row. Every row begins with the
/// eight-column indent and is at most `width` cells wide, whatever
/// `width`; a `width` of zero draws nothing. Rows wrap with the indent
/// on every piece, keeping their text metadata, so copy and search see
/// the unwrapped text. It draws whenever called: the caller's
/// `open_scope` decides, and a search draw reaches it for a closed
/// call. It never panics on any `details`, content or span.
pub(in crate::turn) fn rows(call: &Call, width: u16, out: &mut Rows) {
    let max = usize::from(width);
    if max == 0 {
        return;
    }
    let room = width.saturating_sub(8);
    let mut bodies: Vec<Body> = Vec::new();
    if !call.headline.is_empty() {
        bodies.push(Body {
            number: None,
            gutter: false,
            line: dim(call.headline.clone()),
        });
    }
    push_view(&view_of(call), &mut bodies);
    let shown = bodies.len().min(OPENED_LINES);
    let left = bodies.len().saturating_sub(shown);
    // The gutter fits the largest number among the rows drawn, after
    // the cap: numbers past it do not widen it.
    let gutter = gutter_width(
        bodies
            .iter()
            .take(shown)
            .filter(|body| body.gutter)
            .filter_map(|body| body.number),
    );
    // The gutter is decoration, not text: copy and search skip it, as
    // they skip the indent the wrap adds.
    let mut logical: Vec<(Line<'static>, u16, Option<Target>)> = Vec::with_capacity(shown + 1);
    for body in bodies.into_iter().take(shown) {
        let skip = if body.gutter && gutter > 0 {
            u16::try_from(gutter + 1).unwrap_or(u16::MAX)
        } else {
            0
        };
        logical.push((with_gutter(body, gutter), skip, None));
    }
    if left > 0 {
        let more = u64::try_from(left).unwrap_or(u64::MAX);
        logical.push((
            dim(format!(
                "… {} (Enter to view)",
                count(more, "more line", "more lines")
            )),
            0,
            Some(Target::CallView(call.id)),
        ));
    }
    if shown == 0 {
        logical.push((dim("no output".to_owned()), 0, None));
    }
    for (line, skip, target) in logical {
        let mut pieces = wrap(line, usize::from(room));
        if skip > 0
            && let Some((_, first)) = pieces.first_mut()
        {
            first.skip = first.skip.saturating_add(skip);
        }
        let mut first_piece = true;
        for (mut piece, text) in pieces {
            piece.spans = fit(piece.spans, max);
            // The more row opens the call's whole view; every other row
            // opens nothing here.
            let piece_target = first_piece.then_some(target).flatten();
            first_piece = false;
            out.push_text((piece, piece_target), text);
        }
    }
}

/// The view for a call: the first of `diff`, `start_line`, `matches`,
/// `command` present in its `details` decides, and a malformed one
/// falls to plain, never to a later key. `matches` wins over `command`
/// on a shell `grep`, which carries both. Only a missing or null
/// `details` reads the `content` argument; any other value that is not
/// recognised shows the content.
fn view_of(call: &Call) -> View<'_> {
    let Some(details) = call.details.as_ref().filter(|details| !details.is_null()) else {
        return write_or_plain(call);
    };
    let Some(object) = details.as_object() else {
        return View::Plain(&call.content);
    };
    if let Some(diff) = object.get("diff") {
        if let Some(diff) = diff.as_str() {
            return View::Diff(diff);
        }
        return View::Plain(&call.content);
    }
    if let Some(start) = object.get("start_line") {
        if let Some(start) = start.as_u64() {
            return View::Read {
                start,
                text: &call.content,
            };
        }
        return View::Plain(&call.content);
    }
    if let Some(matches) = object.get("matches") {
        if let Some(groups) = matches.as_array() {
            return View::Matches {
                groups,
                end: end_of(object).unwrap_or(End::None),
            };
        }
        return View::Plain(&call.content);
    }
    if let Some(command) = object.get("command") {
        if let (Some(command), Some(end)) = (command.as_str(), end_of(object)) {
            return View::Shell {
                command,
                end,
                output: &call.content,
            };
        }
        return View::Plain(&call.content);
    }
    View::Plain(&call.content)
}

/// A call with no `details` whose arguments hold a string `content`
/// shows that text, whatever the tool is called; else its content.
fn write_or_plain(call: &Call) -> View<'_> {
    if let Some(text) = call.arguments.get("content").and_then(Value::as_str) {
        View::Written(text)
    } else {
        View::Plain(&call.content)
    }
}

/// The closing from `exit_code` or `signal`, or none when neither is
/// there. `exit_code` is an integer on the wire.
fn end_of(details: &Map<String, Value>) -> Option<End<'_>> {
    if let Some(code) = details.get("exit_code").and_then(Value::as_i64) {
        return Some(End::Exit(code));
    }
    if let Some(signal) = details.get("signal").and_then(Value::as_str) {
        return Some(End::Signal(signal));
    }
    None
}

/// The view's logical lines after the headline.
fn push_view(view: &View, bodies: &mut Vec<Body>) {
    match view {
        View::Diff(diff) => push_diff(diff, bodies),
        View::Read { start, text } => push_numbered(*start, text.lines(), bodies),
        View::Matches { groups, end } => {
            push_matches(groups, bodies);
            push_closing(end, bodies);
        }
        View::Shell {
            command,
            end,
            output,
        } => {
            push_command(command, bodies);
            for line in output_lines(output, end) {
                bodies.push(plain(line));
            }
            push_closing(end, bodies);
        }
        View::Written(text) => push_numbered(1, text.lines(), bodies),
        View::Plain(text) => push_plain(text, bodies),
    }
}

/// A plain body line with no gutter.
fn plain(text: &str) -> Body {
    Body {
        number: None,
        gutter: false,
        line: Line::from(Span::raw(safe_line(text))),
    }
}

/// The diff's lines with their numbers, inside the numbered view.
fn push_diff(diff: &str, bodies: &mut Vec<Body>) {
    for drawn in diff_lines(diff) {
        let line = match drawn.kind {
            DiffKind::Hunk | DiffKind::Note => Line::from(dim_span(joined(&drawn.segments))),
            DiffKind::Removed => Line::from(marked_spans(&drawn.segments, Role::Removed)),
            DiffKind::Added => Line::from(marked_spans(&drawn.segments, Role::Added)),
            DiffKind::Context => Line::from(joined(&drawn.segments)),
        };
        bodies.push(Body {
            number: drawn.number,
            gutter: true,
            line,
        });
    }
}

/// The segments' text joined back into its line.
fn joined(segments: &[(String, bool)]) -> String {
    segments.iter().map(|(text, _)| text.as_str()).collect()
}

/// Diff segments under their colour, the differing words in reverse
/// video.
fn marked_spans(segments: &[(String, bool)], role: Role) -> Vec<Span<'static>> {
    segments
        .iter()
        .map(|(text, marked)| {
            if *marked {
                Span::styled(text.clone(), marked_style(role))
            } else {
                Span::styled(text.clone(), style(role))
            }
        })
        .collect()
}

/// A colour's reverse-video mark for its differing words.
fn marked_style(role: Role) -> Style {
    Style::new()
        .fg(role.color())
        .add_modifier(Modifier::REVERSED)
}

/// Lines numbered from `start`, saturating, inside the numbered view.
fn push_numbered<'a>(start: u64, lines: impl Iterator<Item = &'a str>, bodies: &mut Vec<Body>) {
    let mut number = start;
    for line in lines {
        bodies.push(Body {
            number: Some(number),
            gutter: true,
            line: Line::from(Span::raw(safe_line(line))),
        });
        number = number.saturating_add(1);
    }
}

/// The matches grouped under each file's path, the path bold, in
/// printed order, inside the numbered view.
fn push_matches(groups: &[Value], bodies: &mut Vec<Body>) {
    for group in groups {
        let path = group.get("path").and_then(Value::as_str).unwrap_or("");
        bodies.push(Body {
            number: None,
            gutter: false,
            line: Line::from(Span::styled(
                safe_line(path),
                Style::new().add_modifier(Modifier::BOLD),
            )),
        });
        let lines = group
            .get("lines")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        for line in lines {
            let number = line.get("line").and_then(Value::as_u64).unwrap_or(0);
            let text = line.get("text").and_then(Value::as_str).unwrap_or("");
            let spans = line
                .get("spans")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            bodies.push(Body {
                number: Some(number),
                gutter: true,
                line: Line::from(match_pieces(text, spans)),
            });
        }
    }
}

/// A matched line cut at its merged span edges, the matches on the
/// match tint.
fn match_pieces(text: &str, spans: &[Value]) -> Vec<Span<'static>> {
    let tint = Style::new().bg(Role::Match.color());
    let mut pieces = Vec::new();
    let mut at = 0;
    for (start, end) in merge_spans(text, spans) {
        if let (Some(before), Some(hit)) = (text.get(at..start), text.get(start..end)) {
            if !before.is_empty() {
                pieces.push(Span::raw(safe_line(before)));
            }
            pieces.push(Span::styled(safe_line(hit), tint));
        }
        at = end;
    }
    if let Some(rest) = text.get(at..)
        && !rest.is_empty()
    {
        pieces.push(Span::raw(safe_line(rest)));
    }
    if pieces.is_empty() {
        pieces.push(Span::raw(safe_line(text)));
    }
    pieces
}

/// Span ranges normalised for `text`: reversed, out-of-bounds and
/// off-boundary ranges drop; the rest sort by start and merge where
/// they overlap or touch.
fn merge_spans(text: &str, spans: &[Value]) -> Vec<(usize, usize)> {
    let mut ranges: Vec<(usize, usize)> = spans
        .iter()
        .filter_map(|span| {
            let pair = span.as_array()?;
            let start = usize::try_from(pair.first()?.as_u64()?).ok()?;
            let end = usize::try_from(pair.get(1)?.as_u64()?).ok()?;
            text.get(start..end)?;
            (start < end).then_some((start, end))
        })
        .collect();
    ranges.sort();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (start, end) in ranges {
        if let Some(last) = merged.last_mut()
            && start <= last.1
        {
            last.1 = last.1.max(end);
        } else {
            merged.push((start, end));
        }
    }
    merged
}

/// The full command: "$ " and the command, its later lines bare.
/// Each line wraps on its own.
fn push_command(command: &str, bodies: &mut Vec<Body>) {
    for (index, line) in command.lines().enumerate() {
        let line = if index == 0 {
            format!("$ {line}")
        } else {
            line.to_owned()
        };
        bodies.push(Body {
            number: None,
            gutter: false,
            line: Line::from(Span::raw(safe_line(&line))),
        });
    }
    if command.is_empty() {
        bodies.push(Body {
            number: None,
            gutter: false,
            line: Line::from(Span::raw("$ ".to_owned())),
        });
    }
}

/// The tool's closing text as output rows, dropping the closing line
/// the content already ends with: "Exit code N." or "Killed by
/// <name>." for the values in `details`. When the last line says the
/// output was still held open and the line before it is the tool's
/// closing line, the closing line drops and the notice stays as an
/// output row. A line earlier in the content that merely resembles a
/// closing line stays.
fn output_lines<'a>(output: &'a str, end: &End) -> Vec<&'a str> {
    let mut lines: Vec<&str> = output.lines().collect();
    let closing = match end {
        End::Exit(code) => format!("Exit code {code}."),
        End::Signal(signal) => format!("Killed by {signal}."),
        End::None => return lines,
    };
    if lines.last() == Some(&closing.as_str()) {
        lines.pop();
    } else if lines.last() == Some(&HELD_OPEN)
        && lines.get(lines.len().saturating_sub(2)) == Some(&closing.as_str())
    {
        lines.remove(lines.len().saturating_sub(2));
    }
    lines
}

/// The line the shell keeps when output was still arriving: it stays as
/// an output row while the view draws its own closing line. The view
/// builds this text itself rather than importing the tool.
const HELD_OPEN: &str = "Output was still held open.";

/// One closing line from `details`: "exit N", in `removed` when N is
/// not 0 and dim when it is, or "killed by <signal>" in `removed`.
fn push_closing(end: &End, bodies: &mut Vec<Body>) {
    let line = match end {
        End::Exit(0) => dim("exit 0".to_owned()),
        End::Exit(code) => Line::from(Span::styled(format!("exit {code}"), style(Role::Removed))),
        End::Signal(signal) => Line::from(Span::styled(
            format!("killed by {signal}"),
            style(Role::Removed),
        )),
        End::None => return,
    };
    bodies.push(Body {
        number: None,
        gutter: false,
        line,
    });
}

/// Text that parses to an object or array draws indented two columns
/// per level, in the serialiser's key order; any other text draws
/// verbatim.
fn push_plain(text: &str, bodies: &mut Vec<Body>) {
    let pretty = serde_json::from_str::<Value>(text)
        .ok()
        .filter(|value| value.is_object() || value.is_array())
        .and_then(|value| serde_json::to_string_pretty(&value).ok());
    let text = pretty.as_deref().unwrap_or(text);
    for line in text.lines() {
        bodies.push(plain(line));
    }
}

/// `body` with its gutter: the number right-aligned in the gutter
/// width and one space, dim, or blanks where it has no number. No
/// gutter at all fits no number.
fn with_gutter(body: Body, gutter: usize) -> Line<'static> {
    if !body.gutter || gutter == 0 {
        return body.line;
    }
    let prefix = match body.number {
        Some(number) => format!("{number:>gutter$} "),
        None => " ".repeat(gutter + 1),
    };
    let mut spans = vec![dim_span(prefix)];
    spans.extend(body.line.spans);
    Line::from(spans)
}

/// Every logical line wrapped to `room` with the eight-column indent
/// on every row: the wrap breaks after the last space in the window,
/// dropping it, else cuts hard, never cuts a glyph, and draws a glyph
/// wider than `room` as one blank cell, the same rule `Rows::pad`
/// uses. The pairs keep their text metadata, so copy and search see
/// the unwrapped text.
fn wrap(line: Line<'static>, room: usize) -> Vec<(Line<'static>, RowText)> {
    let mut scratch = Rows::default();
    scratch.push((line, None));
    scratch.pad(0, 8, u16::try_from(room).unwrap_or(u16::MAX));
    let (rows, texts) = scratch.into_parts();
    rows.into_iter()
        .zip(texts)
        .map(|((line, _), text)| (line, text))
        .collect()
}

#[cfg(test)]
#[path = "opened_tests.rs"]
mod tests;
