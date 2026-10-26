//! An opened call's rows under its ledger row, diffs drawn through the
//! shared renderer in [`crate::diff`] (`docs/tui.md`, "Tool groups and
//! the ledger").

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::{Map, Value};

use super::group::{Call, dim_span, fit};
use crate::app::Target;
use crate::diff::{diff_lines, gutter_prefix, gutter_width, safe_line, styled_spans};
use crate::format::{count, dim};
use crate::rows::{RowText, Rows};
use crate::swapped::Ink;
use crate::theme::Role;
use crate::theme::style;

/// An opened call shows at most this many logical lines, then a dim
/// "… N more lines (Enter to view)" row (`docs/tui.md`, "Tool groups
/// and the ledger").
pub(crate) const OPENED_LINES: usize = 20;

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
#[derive(Debug, Clone)]
pub(crate) struct Body {
    pub(crate) number: Option<u64>,
    pub(crate) gutter: bool,
    line: Line<'static>,
}

impl Body {
    /// The line's cells for a swapped-view frame: each span's text with
    /// the ink its style draws as. Bold heads and dim hints keep their
    /// ink, role colours and marked words keep theirs, and the match
    /// tint keeps its mark; anything else draws plain.
    pub(crate) fn cells(&self) -> Vec<(String, Ink)> {
        self.line
            .spans
            .iter()
            .map(|span| (span.content.to_string(), ink_of(&span.style)))
            .collect()
    }
}

/// A span style back to its ink: bold is a heading, dim is muted, a
/// reversed role colour is that role marked, a role colour is that
/// role, and the match tint keeps its mark.
fn ink_of(style: &Style) -> Ink {
    if style.add_modifier.contains(Modifier::BOLD) {
        return Ink::Heading;
    }
    if style.add_modifier.contains(Modifier::DIM) {
        return Ink::Muted;
    }
    if style.add_modifier.contains(Modifier::REVERSED) {
        return role_of(style).map_or(Ink::Plain, Ink::Marked);
    }
    if style.bg == Some(crate::theme::Role::Match.color()) {
        return Ink::Marked(crate::theme::Role::Match);
    }
    role_of(style).map_or(Ink::Plain, Ink::Role)
}

/// The role a span style draws in, if it draws one.
fn role_of(style: &Style) -> Option<crate::theme::Role> {
    crate::theme::Role::ALL
        .into_iter()
        .find(|role| Some(role.color()) == style.fg)
}

/// The call's opened rows with no line cap and no more row: its
/// headline first, then the view its `details` choose. The inline
/// ledger draws the first [`OPENED_LINES`] of these; the call's swapped
/// view draws them all, and draws paged output between [`prefix`] and
/// [`closing`] instead when the result has an artifact.
pub(crate) fn uncapped(call: &Call) -> Vec<Body> {
    let mut bodies: Vec<Body> = Vec::new();
    if !call.headline().is_empty() {
        bodies.push(Body {
            number: None,
            gutter: false,
            line: dim(call.headline().to_owned()),
        });
    }
    let parsed: Option<Value> = parsed_details(call);
    push_view(&parsed_view(call, &parsed), &mut bodies);
    bodies
}

/// The rows a swapped view draws around paged artifact output: the
/// headline and a shell call's command rows, never the output itself.
pub(crate) fn prefix(call: &Call) -> Vec<Body> {
    let mut bodies: Vec<Body> = Vec::new();
    if !call.headline().is_empty() {
        bodies.push(Body {
            number: None,
            gutter: false,
            line: dim(call.headline().to_owned()),
        });
    }
    let parsed: Option<Value> = parsed_details(call);
    if let View::Shell { command, .. } = parsed_view(call, &parsed) {
        push_command(command, &mut bodies);
    }
    bodies
}

/// The closing row a swapped view draws after paged artifact output:
/// the shell's or search's closing, if its `details` name one.
pub(crate) fn closing(call: &Call) -> Vec<Body> {
    let parsed: Option<Value> = parsed_details(call);
    let view = parsed_view(call, &parsed);
    let mut bodies: Vec<Body> = Vec::new();
    match view {
        View::Matches { end, .. } | View::Shell { end, .. } => push_closing(&end, &mut bodies),
        View::Diff(_) | View::Read { .. } | View::Written(_) | View::Plain(_) => {}
    }
    bodies
}

/// The swapped view's inputs for one call: its identity, its kind and
/// target, its uncapped rows for a result with no artifact, the rows a
/// paged artifact draws around, and the paging inputs from its result.
#[derive(Debug, Clone)]
pub(crate) struct CallSnapshot {
    /// The call's id: Esc returns with its row still focused.
    pub(crate) id: usize,
    /// Whether the call is open inline behind the view.
    pub(crate) open: bool,
    /// The capitalised kind word, drawn bold on the top row.
    pub(crate) title: String,
    /// The call's target, drawn dim on the top row.
    pub(crate) command: String,
    /// The whole call's uncapped rows, drawn when no artifact pages it.
    pub(crate) plain: Vec<Body>,
    /// The call's output text, as the tool returned it: what `y` copies
    /// for a result with no artifact, with no gutter, line numbers,
    /// headline, shell command or closing row.
    pub(crate) copy: String,
    /// The headline and shell command rows, drawn around paged output.
    pub(crate) prefix: Vec<Body>,
    /// The shell or search closing row, drawn after paged output.
    pub(crate) closing: Vec<Body>,
    /// The first paged output line's number, for numbered output.
    pub(crate) numbering: Option<u64>,
    /// The full output's path, when the result was cut or a hook
    /// returned text for it.
    pub(crate) artifact: Option<String>,
    /// On a cut result, the byte ranges of the full output held in
    /// `content`: the model's-view edges.
    pub(crate) kept: Option<Vec<[u64; 2]>>,
}

/// Snapshots the call `id` names among `calls` for its swapped view:
/// `None` when it drew no call by that id.
pub(crate) fn snapshot<'a>(
    mut calls: impl Iterator<Item = &'a Call>,
    id: usize,
) -> Option<CallSnapshot> {
    let call = calls.find(|call| call.id == id)?;
    let kind = crate::format::kind(&call.name, &call.arguments);
    let mut title = kind.word().to_owned();
    if let Some(first) = title.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    let copy = call.content().to_owned();
    Some(CallSnapshot {
        id: call.id,
        open: call.open,
        title,
        command: crate::format::summary(&call.name, &call.arguments),
        plain: uncapped(call),
        prefix: prefix(call),
        closing: closing(call),
        numbering: numbering(call),
        copy,
        artifact: call
            .result
            .as_deref()
            .and_then(|result| result.artifact.clone()),
        kept: call
            .result
            .as_deref()
            .and_then(|result| result.kept.clone()),
    })
}

/// The swapped view's inputs rebuilt from the call's completion: a
/// view opened on a running call shows its empty rows until the result
/// arrives, so the completion rebuilds the same per-kind rows the
/// snapshot holds. The caller keeps its identity, target and scroll.
pub(crate) fn snapshot_completed(
    id: usize,
    name: &str,
    arguments: &Value,
    _action: &str,
    done: &contract::events::ToolCallCompleted,
) -> CallSnapshot {
    let content = crate::format::content_text(done);
    let headline = crate::format::headline(done);
    let details = done
        .details
        .clone()
        .filter(|details| !details.is_null())
        .map(|details| details.to_string());
    // No `action` field: the temporary call's action is never read
    // (the snapshot exposes no action; rendering uses name/arguments),
    // so the default stands.
    let call = Call {
        id,
        name: name.to_owned(),
        arguments: arguments.clone(),
        result: Some(Box::new(super::group::CallResult {
            details,
            artifact: done.artifact.clone(),
            kept: done.kept.clone(),
            signal: done
                .process
                .as_ref()
                .and_then(|process| process.signal.clone()),
            headline,
            content,
        })),
        ..Call::default()
    };
    let kind = crate::format::kind(&call.name, &call.arguments);
    let mut title = kind.word().to_owned();
    if let Some(first) = title.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    let copy = call.content().to_owned();
    CallSnapshot {
        id: call.id,
        open: call.open,
        title,
        command: crate::format::summary(&call.name, &call.arguments),
        plain: uncapped(&call),
        prefix: prefix(&call),
        closing: closing(&call),
        numbering: numbering(&call),
        copy,
        artifact: call
            .result
            .as_deref()
            .and_then(|result| result.artifact.clone()),
        kept: call
            .result
            .as_deref()
            .and_then(|result| result.kept.clone()),
    }
}

/// The first paged output line's number: a read's `start_line`, 1 for
/// a written file, none for unnumbered output.
pub(crate) fn numbering(call: &Call) -> Option<u64> {
    let parsed: Option<Value> = parsed_details(call);
    match parsed_view(call, &parsed) {
        View::Read { start, .. } => Some(start),
        View::Written(_) => Some(1),
        View::Diff(_) | View::Matches { .. } | View::Shell { .. } | View::Plain(_) => None,
    }
}

/// The call's opened rows under its ledger row: the first
/// [`OPENED_LINES`] of [`uncapped`], then at most one dim more row.
/// Every row begins with the
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
    let bodies = uncapped(call);
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
        // Pieces are never empty and adding zero is a no-op, so no
        // guard is needed.
        if let Some((_, first)) = pieces.first_mut() {
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

/// The call's parsed details: `None` when it carries none. Details ride
/// as JSON text and parse when the call opens, never on load.
fn parsed_details(call: &Call) -> Option<Value> {
    call.details_text()
        .and_then(|text| serde_json::from_str(text).ok())
}

/// The call's view from its parsed details: a string that fails to
/// parse falls back to the plain-content view for unrecognised details.
fn parsed_view<'a>(call: &'a Call, parsed: &'a Option<Value>) -> View<'a> {
    if call.details_text().is_some() && parsed.is_none() {
        View::Plain(call.content())
    } else {
        view_of(call, parsed.as_ref())
    }
}

/// The view for a call: the first of `diff`, `start_line`, `matches`,
/// `command` present in its `details` decides, and a malformed one
/// falls to plain, never to a later key. `matches` wins over `command`
/// on a shell `grep`, which carries both. Only a missing or null
/// `details` reads the `content` argument; any other value that is not
/// recognised shows the content.
fn view_of<'a>(call: &'a Call, details: Option<&'a Value>) -> View<'a> {
    let Some(details) = details.filter(|details| !details.is_null()) else {
        return write_or_plain(call);
    };
    let Some(object) = details.as_object() else {
        return View::Plain(call.content());
    };
    if let Some(diff) = object.get("diff") {
        if let Some(diff) = diff.as_str() {
            return View::Diff(diff);
        }
        return View::Plain(call.content());
    }
    if let Some(start) = object.get("start_line") {
        if let Some(start) = start.as_u64() {
            return View::Read {
                start,
                text: call.content(),
            };
        }
        return View::Plain(call.content());
    }
    if let Some(matches) = object.get("matches") {
        if let Some(groups) = matches.as_array() {
            return View::Matches {
                groups,
                end: end_of(object).unwrap_or(End::None),
            };
        }
        return View::Plain(call.content());
    }
    if let Some(command) = object.get("command") {
        if let (Some(command), Some(end)) = (command.as_str(), end_of(object)) {
            return View::Shell {
                command,
                end,
                output: call.content(),
            };
        }
        return View::Plain(call.content());
    }
    View::Plain(call.content())
}

/// A call with no `details` whose arguments hold a string `content`
/// shows that text, whatever the tool is called; else its content.
fn write_or_plain(call: &Call) -> View<'_> {
    if let Some(text) = call.arguments.get("content").and_then(Value::as_str) {
        View::Written(text)
    } else {
        View::Plain(call.content())
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
        bodies.push(Body {
            number: drawn.number,
            gutter: true,
            line: Line::from(styled_spans(&drawn)),
        });
    }
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
    if !body.gutter {
        return body.line;
    }
    let Some(prefix) = gutter_prefix(body.number, gutter) else {
        return body.line;
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
