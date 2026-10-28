//! The span rules a renderer's lines draw through (`docs/tui.md`, "What a
//! renderer returns").
//!
//! The terminal builds the cells and the plain text from the same spans,
//! so what is drawn and what is selected, copied or searched cannot
//! differ. It measures width by grapheme cluster and cuts or pads each
//! line to the width, so an extension cannot overflow its slot or move
//! its neighbours. Pad cells are decoration, not text: [`FittedLine::pad`]
//! counts them, and the row text holds the count in its tail.

use std::fmt::Display;
use std::ops::Range;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::RawSpan;
use crate::theme::Role;

/// One fitted line: the cells, each valid link's cells, and how many
/// trailing cells are pad.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FittedLine {
    /// The line's cells, padded with spaces to exactly the width.
    pub(crate) line: Line<'static>,
    /// Each valid link's cells in the line and its destination.
    pub(crate) links: Vec<(Range<u16>, String)>,
    /// The trailing cells that are pad, not text.
    pub(crate) pad: u16,
    /// The animated item that drew the line, if any: what the paint pass
    /// marks as visible (`docs/tui.md`, "Animation").
    pub(crate) anim: Option<super::anim::AnimKey>,
}

/// What a renderer's spans got wrong: the field that names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Shape {
    /// The span's text holds a control character.
    Text,
    /// The `fg` names no colour role.
    Fg(String),
    /// The `bg` names no colour role.
    Bg(String),
    /// The `link` is one [`crate::opener::valid`] refuses.
    Link(String),
}

impl Display for Shape {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text => write!(out, "text holds a control character"),
            Self::Fg(name) => write!(out, "fg is not a colour role: {name:?}"),
            Self::Bg(name) => write!(out, "bg is not a colour role: {name:?}"),
            Self::Link(to) => write!(out, "link is not opened on click: {to:?}"),
        }
    }
}

/// Every line fitted to `width`: each span validated, cut by grapheme
/// with [`crate::format::cut`], padded with spaces to exactly `width`.
/// A bad span rejects the whole result, so a failure draws no partial
/// lines. With `flat` every style is dropped and the text stays.
pub(crate) fn fit(
    lines: &[Vec<RawSpan>],
    width: u16,
    flat: bool,
) -> Result<Vec<FittedLine>, Shape> {
    let max = usize::from(width);
    lines
        .iter()
        .map(|spans| fit_line(spans, max, flat))
        .collect()
}

/// One line fitted to `max` cells.
fn fit_line(spans: &[RawSpan], max: usize, flat: bool) -> Result<FittedLine, Shape> {
    let mut styled = Vec::with_capacity(spans.len());
    for span in spans {
        styled.push(style_span(span, flat)?);
    }
    let mut out: Vec<Span<'static>> = Vec::with_capacity(spans.len() + 1);
    let mut links: Vec<(Range<u16>, String)> = Vec::new();
    let mut taken = 0usize;
    for (span, (style, link)) in spans.iter().zip(styled) {
        let room = max.saturating_sub(taken);
        if room == 0 {
            break;
        }
        let cells = crate::format::width(&span.text);
        if cells <= room {
            let end = taken.saturating_add(cells);
            if let Some(to) = link {
                links.push((cell_range(taken, end), to));
            }
            out.push(Span::styled(span.text.clone(), style));
            taken = end;
            continue;
        }
        let kept = crate::format::cut(&span.text, room);
        let kept_cells = crate::format::width(&kept);
        if !kept.is_empty() {
            let end = taken.saturating_add(kept_cells);
            if let Some(to) = link {
                links.push((cell_range(taken, end), to));
            }
            out.push(Span::styled(kept, style));
            taken = end;
        }
        break;
    }
    let pad = max.saturating_sub(taken);
    if pad > 0 {
        out.push(Span::raw(" ".repeat(pad)));
    }
    Ok(FittedLine {
        line: Line::from(out),
        links,
        pad: u16::try_from(pad).unwrap_or(u16::MAX),
        // The cache tags the animated item that stored the line; a
        // fitted line is still until then.
        anim: None,
    })
}

/// The cells `start..end` as a link range.
fn cell_range(start: usize, end: usize) -> Range<u16> {
    u16::try_from(start).unwrap_or(u16::MAX)..u16::try_from(end).unwrap_or(u16::MAX)
}

/// One span's style and link: the text holds no control character, `fg`
/// and `bg` name colour roles, and the link opens on click.
fn style_span(span: &RawSpan, flat: bool) -> Result<(Style, Option<String>), Shape> {
    if span.text.chars().any(|cell| cell.is_control()) {
        return Err(Shape::Text);
    }
    let fg = match &span.fg {
        None => None,
        Some(name) => Some(
            Role::from_name(name)
                .ok_or_else(|| Shape::Fg(name.clone()))?
                .color(),
        ),
    };
    let bg = match &span.bg {
        None => None,
        Some(name) => Some(
            Role::from_name(name)
                .ok_or_else(|| Shape::Bg(name.clone()))?
                .color(),
        ),
    };
    if let Some(to) = &span.link
        && !crate::opener::valid(to)
    {
        return Err(Shape::Link(to.clone()));
    }
    if flat {
        return Ok((Style::default(), span.link.clone()));
    }
    let mut style = Style::default();
    if let Some(marker) = fg {
        style = style.fg(marker);
    }
    if let Some(marker) = bg {
        style = style.bg(marker);
    }
    if span.bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    if span.dim {
        style = style.add_modifier(Modifier::DIM);
    }
    if span.italic {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if span.underline {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    Ok((style, span.link.clone()))
}

/// The raw spans a built-in row's spans convert to: role markers to
/// names, the four modifiers to flags. Built-in ledger rows carry no
/// links, and clicks need the handlers a later ticket builds, so both
/// stay none; any other colour is not a role and stays out too.
pub(crate) fn from_ratatui(spans: &[Span<'_>]) -> Vec<RawSpan> {
    spans
        .iter()
        .map(|span| {
            let style = span.style;
            RawSpan {
                text: span.content.to_string(),
                fg: role_name(&style, false),
                bg: role_name(&style, true),
                bold: style.add_modifier.contains(Modifier::BOLD),
                dim: style.add_modifier.contains(Modifier::DIM),
                italic: style.add_modifier.contains(Modifier::ITALIC),
                underline: style.add_modifier.contains(Modifier::UNDERLINED),
                link: None,
                click: None,
            }
        })
        .collect()
}

/// The role `style` names on its foreground or background, when it is
/// one. Only the theme names colours (`docs/tui.md`, "Themes"): the
/// marker's type stays inferred here, and the theme reads it.
fn role_name(style: &Style, background: bool) -> Option<String> {
    let marker = if background { style.bg } else { style.fg };
    Role::of_marker(marker).map(|role| role.name().to_owned())
}

#[cfg(test)]
#[path = "spans_tests.rs"]
mod tests;
