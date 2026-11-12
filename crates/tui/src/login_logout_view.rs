//! List primitives shared by the login and logout views.

use ratatui::style::Style;
use ratatui::text::Span;

use crate::view::overlay;

/// A heading's group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Group {
    /// Provider rows.
    Providers,
    /// Secret rows.
    Secrets,
}

/// One row in a login or logout list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Item {
    /// A group's heading; Enter on it does nothing.
    Heading(Group),
    /// One provider or secret: its index in the source rows.
    Target(usize),
    /// The blank row between groups.
    Gap,
    /// The note shown when both groups are empty.
    Note,
}

/// Builds the common grouped list from a per-row provider flag.
pub(super) fn build(provider_rows: &[bool]) -> Vec<Item> {
    let mut items = Vec::new();
    if provider_rows.iter().any(|provider| *provider) {
        items.push(Item::Heading(Group::Providers));
        for (at, provider) in provider_rows.iter().enumerate() {
            if *provider {
                items.push(Item::Target(at));
            }
        }
    }
    if provider_rows.iter().any(|provider| !provider) {
        if !items.is_empty() {
            items.push(Item::Gap);
        }
        items.push(Item::Heading(Group::Secrets));
        for (at, provider) in provider_rows.iter().enumerate() {
            if !provider {
                items.push(Item::Target(at));
            }
        }
    }
    if items.is_empty() {
        items.push(Item::Note);
    }
    items
}

/// The list overlay's chrome in rows, measured with a one-row probe.
pub(super) fn shown_body_rows(height: usize, login: bool) -> usize {
    let (title, action) = if login {
        ("Log in", "log in")
    } else {
        ("Log out", "delete")
    };
    height.saturating_sub(list_chrome(title, action))
}

fn list_chrome(title: &'static str, action: &'static str) -> usize {
    let probe = overlay::Overlay {
        title: Some((title.to_owned(), Some(Span::raw("✕")))),
        close: None,
        body: vec![overlay::Row {
            spans: Vec::new(),
            right: Vec::new(),
            targets: Vec::new(),
            barred: false,
        }],
        footer: Some(overlay::legend(&[
            ("↑↓", "move"),
            ("Enter", action),
            ("Esc", "close"),
        ])),
        prefer: 96,
    };
    usize::from(overlay::height(&probe)).saturating_sub(1)
}

/// One overlay body row holding `text` in `style`.
pub(super) fn text_row(text: impl Into<String>, style: Style) -> overlay::Row {
    overlay::Row {
        spans: vec![Span::styled(text.into(), style)],
        right: Vec::new(),
        targets: Vec::new(),
        barred: false,
    }
}

/// One blank overlay body row.
pub(super) fn blank_row() -> overlay::Row {
    overlay::Row {
        spans: Vec::new(),
        right: Vec::new(),
        targets: Vec::new(),
        barred: false,
    }
}
