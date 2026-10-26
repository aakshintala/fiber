//! Provider quota figures, shared by the card and the narrow Q segment
//! (`docs/tui.md`, "The panel" and "The narrow layout").

use ratatui::text::{Line, Span};

use super::{Row, sides, title_row};
use crate::app::App;
use crate::app::panel::Spot;
use crate::theme::{Role, style};
use contract::events::{QuotaCredit, QuotaWindow};

/// One window's figure, its remaining share and whether little is left,
/// as the card draws them (shared with `/quota`): `None` for a window
/// reporting neither usage nor reset.
pub(crate) fn window_figure(window: &QuotaWindow, notice_at: f64) -> Option<(String, f64, bool)> {
    if window.reset.is_some() {
        Some(("reset".to_owned(), 100.0, false))
    } else if let Some(used) = window.percent_used {
        let share = (100.0 - used).clamp(0.0, 100.0);
        Some((format!("{share:.0}%"), share, used >= notice_at))
    } else {
        None
    }
}

/// One credit's figure, its remaining share and whether little is left,
/// as the card draws them (shared with `/quota`).
pub(crate) fn credit_figure(credit: &QuotaCredit, notice_at: f64) -> (String, Option<f64>, bool) {
    let share = credit
        .limit
        .filter(|limit| *limit > 0.0)
        .map(|limit| (credit.remaining / limit * 100.0).clamp(0.0, 100.0));
    let figure = share.map_or_else(
        || format!("{:.2}", credit.remaining),
        |share| format!("{share:.0}%"),
    );
    let low = share.is_some_and(|share| 100.0 - share >= notice_at);
    (figure, share, low)
}

struct Limit {
    name: String,
    short: String,
    figure: String,
    share: Option<f64>,
    low: bool,
}

/// Each reported window or credit, named by provider and, when needed,
/// credential label. Unreachable labels without a last value draw nothing.
fn limits(app: &App) -> Vec<Limit> {
    let query = &app.panel_state().quota;
    let mut out = Vec::new();
    for info in &query.rows {
        let labels = query
            .rows
            .iter()
            .filter(|row| row.provider == info.provider)
            .count();
        let provider = if labels > 1 {
            format!("{} {}", info.provider, info.credential)
        } else {
            info.provider.clone()
        };
        if let Some(windows) = &info.windows {
            for window in windows {
                let Some((figure, share, low)) = window_figure(window, query.notice_at) else {
                    continue;
                };
                out.push(Limit {
                    name: format!("{provider} {}", window.name),
                    short: if windows.len() > 1 {
                        format!("{provider} {}", window.name)
                    } else {
                        provider.clone()
                    },
                    figure,
                    share: Some(share),
                    low,
                });
            }
        }
        if let Some(credit) = &info.credit {
            let (figure, share, low) = credit_figure(credit, query.notice_at);
            out.push(Limit {
                name: format!("{provider} credit"),
                short: provider,
                figure,
                low,
                share,
            });
        }
    }
    out
}

/// The Quota card's title, figure rows and bars; no rows means no slab.
pub(super) fn rows(app: &App, text: usize) -> Vec<Row> {
    let limits = limits(app);
    if limits.is_empty() {
        return Vec::new();
    }
    // The title and each data row open the quota view, which fetches
    // every label's quota; the bars open nothing.
    let mut title = title_row(
        "Quota",
        vec![Span::styled("extension", style(Role::Muted))],
        text,
    );
    title.spot = Some(Spot::Quota);
    let mut out = vec![title];
    for limit in limits {
        out.push(Row {
            line: sides(
                vec![Span::styled(limit.name, style(Role::Muted))],
                Span::styled(
                    limit.figure,
                    style(if limit.low { Role::Error } else { Role::Text }),
                ),
                text,
            ),
            spot: Some(Spot::Quota),
            tint: None,
            edge: false,
        });
        if let Some(share) = limit.share {
            out.push(Row {
                line: Line::from(bar(share, text.saturating_sub(2), limit.low)),
                spot: None,
                tint: None,
                edge: false,
            });
        }
    }
    out
}

/// Filled cells round to nearest, with one visible cell for any positive
/// share. The share is clamped before this conversion, and the result is
/// bounded by the track width.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "share is clamped to 0..=100 and fill is bounded by cells"
)]
/// Shared with the `/quota` view, whose bars match the card's.
pub(crate) fn bar(share: f64, cells: usize, low: bool) -> Vec<Span<'static>> {
    let fill = if share > 0.0 {
        ((share / 100.0 * cells as f64).round() as usize)
            .max(1)
            .min(cells)
    } else {
        0
    };
    vec![
        Span::styled(
            "▆".repeat(fill),
            style(if low { Role::Error } else { Role::Accent }),
        ),
        Span::styled("▆".repeat(cells.saturating_sub(fill)), style(Role::Rule)),
    ]
}

/// The narrow layout's Q segment, opening the quota view.
pub(crate) fn segment_spans(app: &App) -> Option<Vec<Span<'static>>> {
    let limits = limits(app);
    if limits.is_empty() {
        return None;
    }
    let mut out = vec![Span::styled("Q ", style(Role::Muted))];
    for (at, limit) in limits.into_iter().enumerate() {
        if at > 0 {
            out.push(Span::styled(" · ", style(Role::Muted)));
        }
        out.push(Span::styled(
            format!("{} {}", limit.short, limit.figure),
            style(if limit.low { Role::Error } else { Role::Text }),
        ));
    }
    Some(out)
}

#[cfg(test)]
#[path = "quota_tests.rs"]
mod tests;
