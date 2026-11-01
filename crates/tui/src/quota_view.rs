//! The `/quota` view: one row per entry of the `quota` answer, in its
//! order (`docs/tui.md`, "Swapped views").
//!
//! The rows reuse the panel card's figures and bars
//! (`crate::view::panel::quota`): each window a dim name, its percent
//! and its bar, then its reset time dim on the row after it. An
//! unreachable entry shows its last value and `unreachable` in `error`,
//! or `unreachable` alone when it has no last value. While the fetch is
//! in flight the header's command reads `/quota · fetching`, over the
//! cached answer.

use crate::view::panel::quota::{credit_figure, window_figure};
use contract::events::{QuotaInfo, QuotaWindow};

use crate::swapped::{Frame, Ink, List};
use crate::theme::Role;

/// The quota bars cap at this width, so a wide screen keeps them
/// readable: a full-width bar would drown the figures.
const BAR_W: usize = 40;

/// One cell of a row: its text, never a click target of its own, and how
/// it draws.
fn cell(text: String, ink: Ink) -> (String, Option<crate::swapped::Spot>, Ink) {
    (text, None, ink)
}

/// A reset time as the view draws it: `resets in` then the duration,
/// minutes rounded down, `3h12m`, and days above a day, `2d4h`.
fn resets_in(mins: u64) -> String {
    if mins >= 1440 {
        format!("{}d{}h", mins / 1440, mins % 1440 / 60)
    } else if mins >= 60 {
        format!("{}h{}m", mins / 60, mins % 60)
    } else {
        format!("{mins}m")
    }
}

/// The figure ink: `error` where little is left, plain otherwise.
fn figure_ink(low: bool) -> Ink {
    if low {
        Ink::Role(Role::Error)
    } else {
        Ink::Plain
    }
}

/// The bar ink: `error` where little is left, accent otherwise.
fn bar_ink(low: bool) -> Ink {
    if low {
        Ink::Role(Role::Error)
    } else {
        Ink::Role(Role::Accent)
    }
}

type Row = Vec<(String, Option<crate::swapped::Spot>, Ink)>;

/// One window's rows: the dim name and its percent, its bar, then its
/// reset time dim on the row after it.
fn window_rows(
    rows: &mut Vec<Row>,
    window: &QuotaWindow,
    notice_at: f64,
    width: usize,
    now_ms: u64,
) {
    let Some((figure, share, low)) = window_figure(window, notice_at) else {
        return;
    };
    rows.push(vec![
        cell(window.name.clone(), Ink::Muted),
        cell(format!("  {figure}"), figure_ink(low)),
    ]);
    rows.push(bar_row(share, low, width));
    if let Some(resets_at) = window.resets_at {
        let mins = resets_at.saturating_sub(now_ms) / 60_000;
        rows.push(vec![cell(
            format!("resets in {}", resets_in(mins)),
            Ink::Muted,
        )]);
    }
}

/// A share bar in the view's capped width: the fill, then the track,
/// as the panel card draws them.
fn bar_row(share: f64, low: bool, width: usize) -> Row {
    let cells = width.min(BAR_W);
    let mut spans = crate::view::panel::quota::bar(share, cells, low).into_iter();
    let fill = spans
        .next()
        .map(|span| span.content.into_owned())
        .unwrap_or_default();
    let track = spans
        .next()
        .map(|span| span.content.into_owned())
        .unwrap_or_default();
    vec![cell(fill, bar_ink(low)), cell(track, Ink::Role(Role::Rule))]
}

/// One entry's header: the provider bold, its credential label dim.
fn entry_header(rows: &mut Vec<Row>, info: &QuotaInfo) {
    rows.push(vec![
        cell(info.provider.clone(), Ink::Heading),
        cell(format!(" · {}", info.credential), Ink::Muted),
    ]);
}

/// An unreachable entry's rows: each last value and `unreachable` in
/// `error`, or `unreachable` alone when it has no last value.
fn unreachable_rows(rows: &mut Vec<Row>, info: &QuotaInfo, notice_at: f64) {
    let mut figures = Vec::new();
    for window in info.windows.iter().flatten() {
        if let Some((figure, _, _)) = window_figure(window, notice_at) {
            figures.push(figure);
        }
    }
    if let Some(credit) = &info.credit {
        figures.push(credit_figure(credit, notice_at).0);
    }
    if figures.is_empty() {
        rows.push(vec![cell("unreachable".to_owned(), Ink::Role(Role::Error))]);
        return;
    }
    for figure in figures {
        rows.push(vec![
            cell(figure, Ink::Plain),
            cell(" · ".to_owned(), Ink::Muted),
            cell("unreachable".to_owned(), Ink::Role(Role::Error)),
        ]);
    }
}

/// One entry's rows: its header, then its windows in vendor order and its
/// credit, or its unreachable rows.
fn entry_rows(rows: &mut Vec<Row>, info: &QuotaInfo, notice_at: f64, width: usize, now_ms: u64) {
    entry_header(rows, info);
    if info.unreachable.is_some() {
        unreachable_rows(rows, info, notice_at);
        return;
    }
    for window in info.windows.iter().flatten() {
        window_rows(rows, window, notice_at, width, now_ms);
    }
    if let Some(credit) = &info.credit {
        let (figure, share, low) = credit_figure(credit, notice_at);
        rows.push(vec![
            cell("credit".to_owned(), Ink::Muted),
            cell(format!("  {figure}"), figure_ink(low)),
        ]);
        if let Some(share) = share {
            rows.push(bar_row(share, low, width));
        }
    }
}

/// The quota rows for the swapped view: while the fetch is in flight the
/// header's command carries the `fetching` mark; one dim line stands in
/// place of the rows with no entries.
pub(crate) fn frame(
    rows: &[QuotaInfo],
    notice_at: f64,
    fetching: bool,
    list: List,
    width: u16,
    now_ms: u64,
) -> Frame {
    let mut out = Vec::new();
    if rows.is_empty() {
        out.push(vec![cell(
            "No provider reports quota".to_owned(),
            Ink::Muted,
        )]);
    }
    let width = usize::from(width);
    for (at, info) in rows.iter().enumerate() {
        if at > 0 {
            out.push(vec![cell(String::new(), Ink::Plain)]);
        }
        entry_rows(&mut out, info, notice_at, width, now_ms);
    }
    Frame {
        joins: Vec::new(),
        title: "Quota".to_owned(),
        command: if fetching {
            "/quota · fetching".to_owned()
        } else {
            "/quota".to_owned()
        },
        rows: out,
        list,
        below: Vec::new(),
        field: None,
        footer: String::new(),
    }
}

#[cfg(test)]
#[path = "quota_view_tests.rs"]
mod tests;
