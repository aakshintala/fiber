//! Tests for bare URLs (`docs/tui.md`, "Links").

use super::urls;

/// The URLs `text` holds, as text.
fn found(text: &str) -> Vec<String> {
    urls(text)
        .into_iter()
        .map(|range| text.get(range).unwrap_or_default().to_owned())
        .collect()
}

#[test]
fn urls_finds_bare_urls_and_cuts_trailing_punctuation() {
    type Case<'a> = (&'a str, &'a str, Vec<&'a str>);
    let cases: Vec<Case<'_>> = vec![
        (
            "a bare URL",
            "see http://example.com/a here",
            vec!["http://example.com/a"],
        ),
        (
            "an https URL",
            "see https://example.com",
            vec!["https://example.com"],
        ),
        (
            "an uppercase scheme",
            "see HTTP://example.com/a here",
            vec!["HTTP://example.com/a"],
        ),
        (
            "a trailing period",
            "see http://example.com.",
            vec!["http://example.com"],
        ),
        (
            "trailing punctuation",
            "see http://example.com,;:",
            vec!["http://example.com"],
        ),
        (
            "a quoted URL",
            "\"http://example.com/a\"",
            vec!["http://example.com/a"],
        ),
        (
            "a parenthesised URL",
            "(http://example.com/a)",
            vec!["http://example.com/a"],
        ),
        (
            "a URL with a balanced paren",
            "http://example.com/a(b)",
            vec!["http://example.com/a(b)"],
        ),
        (
            "a URL followed by a close paren it did not open",
            "see (http://example.com/a),",
            vec!["http://example.com/a"],
        ),
        (
            "a bracketed URL",
            "[http://example.com/a]",
            vec!["http://example.com/a"],
        ),
        ("http alone is none", "see http:// here", vec![]),
        (
            "a URL inside a word",
            "abhttp://example.com/cd",
            vec!["http://example.com/cd"],
        ),
        (
            "two URLs",
            "http://a.example and https://b.example/c",
            vec!["http://a.example", "https://b.example/c"],
        ),
        (
            "a long URL that wraps on screen",
            "see http://example.com/aaaa-bbbb-cccc-dddd-eeee-ffff-gggg-hhhh for more",
            vec!["http://example.com/aaaa-bbbb-cccc-dddd-eeee-ffff-gggg-hhhh"],
        ),
    ];
    for (name, text, expected) in cases {
        let expected: Vec<String> = expected.into_iter().map(str::to_owned).collect();
        assert_eq!(found(text), expected, "{name}");
    }
}

// Click, focus and stale targets for links below.

use std::path::PathBuf;

use contract::clock::Clock;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::{Value, json};

use super::link_hash;
use crate::app::{App, Effect};
use crate::keys::{Button, Key, Mouse, MouseKind};
use crate::link::Line;
use crate::mouse::{TargetId, hit};

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

/// The injected clock's now.
fn now() -> std::time::Instant {
    fakes::clock::FakeClock::new().now()
}

/// A session line.
fn line(kind: &str, payload: Value, action: Option<&str>) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|id| contract::ActionId(id.to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// The hub's `hub_hello`.
fn hello() -> Line {
    Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    })
}

/// An app attached at `width` by `height`, showing one done turn with
/// `text`.
fn replied(width: u16, height: u16, text: &str) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(width, height);
    assert!(app.on_line(hello()).is_empty());
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.on_line(line(
        "turn_started",
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": " "}]}]}),
        None,
    ));
    app.on_line(line("text_completed", json!({ "text": text }), Some("a_m")));
    app.on_line(line(
        "turn_completed",
        json!({"outcome": "completed"}),
        None,
    ));
    app
}

/// The app drawn at its size, with its targets.
fn draw(app: &App) -> (Buffer, Vec<crate::mouse::Target>) {
    let (width, height) = (app.screen.width(), app.screen.height());
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let targets = crate::view::render(app, area, &mut buf, None);
    (buf, targets)
}

/// The link targets drawn now.
fn links(app: &App) -> Vec<(TargetId, Rect)> {
    let (_, targets) = draw(app);
    targets
        .into_iter()
        .filter(|target| matches!(target.id, TargetId::Link { .. }))
        .map(|target| (target.id, target.rect))
        .collect()
}

/// The first link target whose URL is `url`.
fn link_for(app: &App, url: &str) -> (TargetId, Rect) {
    let area = app.conversation_area();
    app.visible_links(area)
        .into_iter()
        .find(|link| link.url == url)
        .map(|link| (link.id, link.rects.into_iter().next().unwrap_or_default()))
        .unwrap_or_else(|| panic!("no link to {url}"))
}

#[test]
fn a_bare_url_inside_a_markdown_label_keeps_the_markdown_destination() {
    let mut app = replied(
        60,
        12,
        "[https://label.example](https://destination.example)",
    );
    app.set_opener(true);
    let area = app.conversation_area();
    let found = app.visible_links(area);
    // One target, not two: the label's cells keep the markdown
    // destination, so the target under the pointer opens it.
    assert_eq!(found.len(), 1, "{found:?}");
    let rect = found[0].rects.first().expect("a link rect");
    let (_, targets) = draw(&app);
    let id = hit(&targets, rect.x, rect.y).expect("a link under the label");
    assert_eq!(id, found[0].id);
    assert_eq!(
        app.on_click(id),
        Effect::OpenLink("https://destination.example".to_owned())
    );
}

#[test]
fn a_bare_url_wrapped_over_three_rows_opens_whole_from_each_row() {
    let url = "http://example.com/aaaa-bbbb-cccc-dddd-eeee-ffff-gggg-hhhh";
    let mut app = replied(24, 16, &format!("see {url} for more"));
    app.set_opener(true);
    let area = app.conversation_area();
    let found = app.visible_links(area);
    let entry = found
        .iter()
        .find(|link| link.url == url)
        .unwrap_or_else(|| panic!("no wrapped bare link in {found:?}"));
    // One link, one rect per row it covers: a click on any row opens
    // the whole URL, not the row's fragment.
    assert_eq!(entry.rects.len(), 3, "{entry:?}");
    for rect in &entry.rects {
        let (_, targets) = draw(&app);
        let id = hit(&targets, rect.x, rect.y).expect("a target on the row");
        assert_eq!(id, entry.id, "{rect:?}");
        assert_eq!(
            app.on_click(id),
            Effect::OpenLink(url.to_owned()),
            "{rect:?}"
        );
    }
}

#[test]
fn a_click_on_a_markdown_link_opens_its_destination() {
    let mut app = replied(60, 12, "[docs](https://example.com/a)");
    app.set_opener(true);
    let (id, _) = link_for(&app, "https://example.com/a");
    assert_eq!(
        app.on_click(id),
        Effect::OpenLink("https://example.com/a".to_owned())
    );
    assert!(!app.copied());
}

#[test]
fn a_click_on_a_bare_url_opens_it() {
    let mut app = replied(60, 12, "see http://example.com/a here");
    app.set_opener(true);
    let (id, _) = link_for(&app, "http://example.com/a");
    assert_eq!(
        app.on_click(id),
        Effect::OpenLink("http://example.com/a".to_owned())
    );
}

#[test]
fn a_link_wrapped_over_two_rows_is_one_target_on_both() {
    let mut app = replied(16, 12, "[a long docs link here](http://example.com/a) tail");
    app.set_opener(true);
    let area = app.conversation_area();
    let found = app.visible_links(area);
    let entry = found
        .iter()
        .find(|link| link.url == "http://example.com/a")
        .unwrap_or_else(|| panic!("no wrapped link in {found:?}"));
    assert_eq!(entry.rects.len(), 2, "{entry:?}");
    assert_eq!(entry.rects[0].y + 1, entry.rects[1].y);
    // Either row's target opens the same destination.
    for rect in &entry.rects {
        assert_eq!(
            app.on_click(entry.id),
            Effect::OpenLink("http://example.com/a".to_owned()),
            "{rect:?}"
        );
    }
}

#[test]
fn with_no_opener_a_click_copies_and_shows_copied() {
    let mut app = replied(60, 12, "[docs](https://example.com/a)");
    app.set_opener(false);
    let (id, _) = link_for(&app, "https://example.com/a");
    assert_eq!(
        app.on_click(id),
        Effect::Copy("https://example.com/a".to_owned())
    );
    assert!(app.copied());
}

#[test]
fn a_click_on_a_stale_link_target_does_nothing() {
    let text: Vec<String> = (0..30).map(|n| format!("paragraph {n}")).collect();
    let mut app = replied(
        40,
        12,
        &format!("{}\n\n[docs](https://example.com/a)", text.join("\n\n")),
    );
    app.set_opener(true);
    let (id, rect) = link_for(&app, "https://example.com/a");
    // PageUp in the same read as the click: the frame the click was drawn
    // from is stale, so the old id opens nothing.
    app.on_key(Key::PageUp, now());
    assert_eq!(app.on_click(id), Effect::None, "stale {rect:?}");
}

#[test]
fn a_stale_id_names_its_row_column_and_hash() {
    let mut app = replied(60, 12, "[docs](https://example.com/a)");
    app.set_opener(true);
    let (id, _) = link_for(&app, "https://example.com/a");
    let TargetId::Link { row, col, url } = id else {
        panic!("a link id");
    };
    // Same cell, another URL.
    let other = TargetId::Link {
        row,
        col,
        url: link_hash("https://other.example"),
    };
    assert_eq!(app.on_click(other), Effect::None);
    // Same URL, another cell.
    let moved = TargetId::Link {
        row: row.saturating_add(1),
        col,
        url,
    };
    assert_eq!(app.on_click(moved), Effect::None);
    // The drawn id still opens.
    assert_eq!(
        app.on_click(id),
        Effect::OpenLink("https://example.com/a".to_owned())
    );
}

#[test]
fn y_on_a_focused_link_copies_its_url() {
    let mut app = replied(60, 12, "[docs](https://example.com/a)");
    app.set_opener(true);
    let (id, _) = link_for(&app, "https://example.com/a");
    app.focus = Some(id);
    assert_eq!(
        app.on_key(Key::Char('y'), now()),
        Effect::Copy("https://example.com/a".to_owned())
    );
    assert!(app.copied());
}

#[test]
fn enter_on_a_focused_link_opens_it() {
    let mut app = replied(60, 12, "[docs](https://example.com/a)");
    app.set_opener(true);
    let (id, _) = link_for(&app, "https://example.com/a");
    app.focus = Some(id);
    assert_eq!(
        app.on_key(Key::Enter, now()),
        Effect::OpenLink("https://example.com/a".to_owned())
    );
}

#[test]
fn a_focused_link_sits_on_surface_raised_with_its_hint_under_it() {
    use crate::theme::Role;
    use ratatui::style::Modifier;
    let mut app = replied(
        60,
        16,
        "[docs](https://example.com/a) and [other](https://example.com/b)\n\nmore text here",
    );
    app.set_opener(true);
    let (id, first) = link_for(&app, "https://example.com/a");
    app.focus = Some(id);
    let (buf, _) = draw(&app);
    // Every cell of the focused link: accent, underlined, on
    // `surface_raised`, and not reversed by the generic focus style.
    for dx in 0..first.width {
        let cell = &buf[(first.x.saturating_add(dx), first.y)];
        assert_eq!(cell.fg, Role::Accent.color(), "link cell {dx}");
        assert_eq!(cell.bg, Role::SurfaceRaised.color(), "link cell {dx}");
        assert!(
            cell.modifier.contains(Modifier::UNDERLINED),
            "link cell {dx}"
        );
        assert!(
            !cell.modifier.contains(Modifier::REVERSED),
            "link cell {dx}"
        );
    }
    // The link without focus keeps the card's background.
    let (_, second) = link_for(&app, "https://example.com/b");
    let plain = &buf[(second.x, second.y)];
    assert_eq!(plain.fg, Role::Accent.color());
    assert_ne!(plain.bg, Role::SurfaceRaised.color());
    // The hint sits on the row under the link, dim.
    let hint = "Enter opens · y copies";
    let row: String = (first.x..first.x.saturating_add(22))
        .map(|x| buf[(x, first.y.saturating_add(1))].symbol().to_owned())
        .collect();
    assert_eq!(row, hint);
    for dx in 0..22 {
        assert!(
            buf[(first.x.saturating_add(dx), first.y.saturating_add(1))]
                .modifier
                .contains(Modifier::DIM),
            "hint cell {dx}"
        );
    }
    // Focus gone, the hint goes with it.
    app.focus = None;
    let (buf, _) = draw(&app);
    let text = crate::view::text(&buf);
    assert!(!text.contains(hint), "{text}");
}

#[test]
fn a_focused_link_on_the_last_row_scrolls_its_hint_into_view() {
    use crate::theme::Role;
    use ratatui::style::Modifier;
    // Paragraphs enough to fill the conversation, then the link: one
    // wheel step up moves the rows three rows down the screen, putting
    // the link on the conversation's last row. The hint stays visible:
    // the rows scroll up one row to free its row under the link
    // (`docs/tui.md`, "Links").
    let paragraphs: Vec<String> = (0..30).map(|n| format!("paragraph {n}")).collect();
    let text = format!(
        "{}\n\n[docs](https://example.com/a)",
        paragraphs.join("\n\n")
    );
    let mut app = replied(60, 16, &text);
    app.set_opener(true);
    app.on_wheel(&Mouse {
        kind: MouseKind::WheelUp,
        col: 30,
        row: 6,
    });
    let (id, first) = link_for(&app, "https://example.com/a");
    assert_eq!(
        first.y.saturating_add(1),
        app.conversation_area().bottom(),
        "the link is the last conversation row"
    );
    app.focus = Some(id);
    let bottom = app.conversation_area().bottom();
    let (buf, targets) = draw(&app);
    // The link moves up one row and keeps its focused look; the hint
    // draws on the freed last row, dim.
    let moved = bottom.saturating_sub(2);
    let cell = &buf[(first.x, moved)];
    assert_eq!(cell.fg, Role::Accent.color(), "the moved link's colour");
    assert_eq!(
        cell.bg,
        Role::SurfaceRaised.color(),
        "the moved link's tint"
    );
    let hint = "Enter opens · y copies";
    let row: String = (first.x..first.x.saturating_add(22))
        .map(|x| buf[(x, bottom.saturating_sub(1))].symbol().to_owned())
        .collect();
    assert_eq!(row, hint);
    for dx in 0..22 {
        assert!(
            buf[(first.x.saturating_add(dx), bottom.saturating_sub(1))]
                .modifier
                .contains(Modifier::DIM),
            "hint cell {dx}"
        );
    }
    // The hint's cells open the focused link, and Enter and `y` keep
    // working (`docs/tui.md`, "Links").
    let under = hit(&targets, first.x, bottom.saturating_sub(1)).expect("a target on the hint row");
    assert_eq!(under, id);
    assert_eq!(
        app.on_click(under),
        Effect::OpenLink("https://example.com/a".to_owned())
    );
    assert_eq!(
        app.on_key(Key::Enter, now()),
        Effect::OpenLink("https://example.com/a".to_owned())
    );
    app.focus = Some(id);
    assert_eq!(
        app.on_key(Key::Char('y'), now()),
        Effect::Copy("https://example.com/a".to_owned())
    );
    // Focus gone, the hint goes with it.
    app.focus = None;
    let (buf, _) = draw(&app);
    let text = crate::view::text(&buf);
    assert!(!text.contains(hint), "{text}");
}

#[test]
fn a_hint_past_the_last_row_blanks_stale_cells() {
    // A shell row with a bare URL, focused: shell output is a plain,
    // unpadded row, so with the link on the last content row the hint row
    // sits past the last line, and only the trailing blank in
    // `conversation_rows` clears it (`docs/tui.md`, "Links"). The buffer
    // starts full of stale cells, as a previous frame's rows: past the
    // hint text the row must be blank, not stale.
    let url = "http://example.com/a";
    let mut app = replied(60, 16, "a reply");
    app.set_opener(true);
    app.screen.pages_mut().add_shell(Some(url.to_owned()));
    app.settle();
    let area = app.conversation_area();
    let (id, first) = link_for(&app, url);
    app.focus = Some(id);
    let gap = app.hint_layout(area).gap.expect("the hint reserves a row");
    let (width, height) = (app.screen.width(), app.screen.height());
    let full = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(full);
    for y in full.top()..full.bottom() {
        for x in full.left()..full.right() {
            buf[(x, y)].set_symbol("X");
        }
    }
    let _targets = crate::view::render(&app, full, &mut buf, None);
    // The hint draws on the reserved row, dim.
    let hint = crate::view::marks::HINT;
    let row: String = (first.x..first.x.saturating_add(22))
        .map(|x| buf[(x, gap)].symbol().to_owned())
        .collect();
    assert_eq!(row, hint);
    // Every cell past the hint is blank: no stale cell shows. The
    // scroll bar paints the last column after the rows.
    let last_col = full.right().saturating_sub(1);
    for x in first.x.saturating_add(22)..last_col {
        assert_eq!(buf[(x, gap)].symbol(), " ", "stale cell at {x}");
    }
    // The focused link keeps its row above the hint, and Enter still
    // opens it.
    let moved = gap.saturating_sub(1);
    let cell = &buf[(first.x, moved)];
    assert_eq!(cell.fg, crate::theme::Role::Accent.color());
    assert_eq!(
        app.on_key(Key::Enter, now()),
        Effect::OpenLink(url.to_owned())
    );
}

#[test]
fn a_focused_table_link_reserves_its_hint_row() {
    use crate::theme::Role;
    use ratatui::style::{Color, Modifier};
    // Two table link rows back to back: the hint for the upper link
    // reserves its own row under it, so the lower row keeps its text
    // and its target keeps its URL (`docs/tui.md`, "Links").
    let mut app = replied(
        60,
        16,
        "| name | link |\n| --- | --- |\n| [spec](https://example.com/spec) | tail |\n| [site](https://example.com/site) | end |\n\nmore text here",
    );
    app.set_opener(true);
    let (id, first) = link_for(&app, "https://example.com/spec");
    let (_, second) = link_for(&app, "https://example.com/site");
    assert_eq!(second.y, first.y.saturating_add(1), "adjacent table rows");
    app.focus = Some(id);
    let (buf, targets) = draw(&app);
    // The hint sits on the reserved row under the focused link, dim,
    // keeping the cells' colours so the card shows through.
    let hint = "Enter opens · y copies";
    let gap = first.y.saturating_add(1);
    let row: String = (first.x..first.x.saturating_add(22))
        .map(|x| buf[(x, gap)].symbol().to_owned())
        .collect();
    assert_eq!(row, hint);
    for dx in 0..22 {
        let cell = &buf[(first.x.saturating_add(dx), gap)];
        assert!(cell.modifier.contains(Modifier::DIM), "hint cell {dx}");
        assert_eq!(cell.fg, Color::Reset, "hint cell {dx}");
    }
    // The reserved row is one blank card row: every cell shares its
    // background, so no previous frame's cells show past the hint. The
    // scroll bar paints the last column after the rows.
    let bg = buf[(0, gap)].bg;
    let last_col = buf.area.width.saturating_sub(1);
    for x in 0..last_col {
        assert_eq!(buf[(x, gap)].bg, bg, "gap cell {x}");
    }
    // The focused link stays where it was, on its tint.
    let (moved, at) = link_for(&app, "https://example.com/spec");
    assert_eq!((moved, at), (id, first));
    assert_eq!(
        buf[(first.x, first.y)].bg,
        Role::SurfaceRaised.color(),
        "the focused link's tint"
    );
    // The lower row moved down one with its text: no target sits on
    // the hint row, and its own target still opens its URL.
    assert!(
        hit(&targets, first.x, gap).is_none(),
        "no target under the hint"
    );
    let (site_id, site) = link_for(&app, "https://example.com/site");
    assert_eq!(
        site.y,
        second.y.saturating_add(1),
        "the lower row moved down one"
    );
    let screen = crate::view::text(&buf);
    assert!(
        screen
            .lines()
            .nth(usize::from(site.y))
            .unwrap_or_default()
            .contains("site"),
        "the lower row keeps its text\n{screen}"
    );
    assert_eq!(
        app.on_click(site_id),
        Effect::OpenLink("https://example.com/site".to_owned())
    );
    // Enter opens and `y` copies the focused link.
    assert_eq!(
        app.on_key(Key::Enter, now()),
        Effect::OpenLink("https://example.com/spec".to_owned())
    );
    app.focus = Some(id);
    assert_eq!(
        app.on_key(Key::Char('y'), now()),
        Effect::Copy("https://example.com/spec".to_owned())
    );
}

#[test]
fn a_focused_link_moves_down_to_the_next_link() {
    use crate::theme::Role;
    // From the table above with the upper link focused, moving focus
    // to the lower link moves the reserved row under it: the hint
    // follows focus and every target keeps its URL (`docs/tui.md`,
    // "Links").
    let mut app = replied(
        60,
        16,
        "| name | link |\n| --- | --- |\n| [spec](https://example.com/spec) | tail |\n| [site](https://example.com/site) | end |\n\nmore text here",
    );
    app.set_opener(true);
    let (id, _) = link_for(&app, "https://example.com/spec");
    app.focus = Some(id);
    assert!(
        targets_text(&app).contains("Enter opens"),
        "the first hint shows"
    );
    let (site_id, _) = link_for(&app, "https://example.com/site");
    app.focus = Some(site_id);
    let (buf, _) = draw(&app);
    // The hint sits under the newly focused link's line, and it keeps
    // the focused tint while the old one keeps the card's background.
    // The refetch reads the newly focused layout's rows.
    let (_, site) = link_for(&app, "https://example.com/site");
    let gap = site.y.saturating_add(1);
    let row: String = (site.x..site.x.saturating_add(22))
        .map(|x| buf[(x, gap)].symbol().to_owned())
        .collect();
    assert_eq!(row, "Enter opens · y copies");
    assert_eq!(
        buf[(site.x, site.y)].bg,
        Role::SurfaceRaised.color(),
        "the newly focused link's tint"
    );
    let (spec_id, spec) = link_for(&app, "https://example.com/spec");
    assert_eq!(spec_id, id);
    assert_ne!(
        buf[(spec.x, spec.y)].bg,
        Role::SurfaceRaised.color(),
        "the old link lost its tint"
    );
    assert_eq!(
        app.on_key(Key::Enter, now()),
        Effect::OpenLink("https://example.com/site".to_owned())
    );
}

/// The screen's text for `app` as drawn now.
fn targets_text(app: &App) -> String {
    let (buf, _) = draw(app);
    crate::view::text(&buf)
}

/// Whether any link drawn now opens `url`.
fn targets_text_has(app: &App, url: &str) -> bool {
    let area = app.conversation_area();
    app.visible_links(area)
        .into_iter()
        .any(|link| link.url == url)
}

#[test]
fn a_focused_link_pushes_the_last_row_off_the_screen() {
    // Paragraphs, a mid link, then a last link on the conversation's
    // last row: focusing the mid link reserves its hint row by moving
    // the rows below down one, so the last row drops with its target
    // instead of opening under the hint (`docs/tui.md`, "Links").
    let paragraphs: Vec<String> = (0..30).map(|n| format!("paragraph {n}")).collect();
    let text = format!(
        "{}\n\n[mid](https://example.com/mid)\n\n[last](https://example.com/last)",
        paragraphs.join("\n\n")
    );
    let mut app = replied(60, 16, &text);
    app.set_opener(true);
    app.on_wheel(&Mouse {
        kind: MouseKind::WheelUp,
        col: 30,
        row: 6,
    });
    let bottom = app.conversation_area().bottom();
    let (_, last) = link_for(&app, "https://example.com/last");
    assert_eq!(
        last.y.saturating_add(1),
        bottom,
        "the last link is the last conversation row"
    );
    let (id, _) = link_for(&app, "https://example.com/mid");
    app.focus = Some(id);
    let (buf, targets) = draw(&app);
    // The hint shows under the focused link while the dropped row's
    // text and target are gone together.
    let text = crate::view::text(&buf);
    assert!(text.contains("Enter opens · y copies"), "{text}");
    assert!(!text.contains("https://example.com/last"), "{text}");
    assert!(
        targets
            .iter()
            .filter(|target| matches!(target.id, TargetId::Link { .. }))
            .all(|target| target.rect.y < bottom),
        "no target hangs past the screen"
    );
    assert!(
        !targets_text_has(&app, "https://example.com/last"),
        "the dropped link has no target"
    );
}

#[test]
fn a_focused_link_on_a_partially_visible_wrapped_line_keeps_its_hint_in_bounds() {
    use crate::format;

    let url = "http://example.com/aaaa-bbbb-cccc-dddd-eeee-ffff-gggg-hhhh-iiii-jjjj-kkkk";
    let paragraphs: Vec<String> = (0..20).map(|n| format!("paragraph {n}")).collect();
    let mut app = replied(24, 16, &paragraphs.join("\n\n"));
    // Shell output is a plain, unpadded row. The long bare URL wraps only
    // when drawn, so its final wrapped rows can fall beyond the viewport.
    app.screen.pages_mut().add_shell(Some(url.to_owned()));
    app.settle();
    app.set_opener(true);
    let area = app.conversation_area();
    app.on_wheel(&Mouse {
        kind: MouseKind::WheelUp,
        col: area.x.saturating_add(area.width / 2),
        row: area.y.saturating_add(area.height / 2),
    });
    let area = app.conversation_area();
    let (top, y0, shown) = app.view_rows(area);
    let link = app
        .visible_links(area)
        .into_iter()
        .find(|link| link.url == url)
        .expect("the partly shown shell URL");
    let last = link.rects.last().expect("a visible URL row");
    let row = top.saturating_add(usize::from(last.y.saturating_sub(y0)));
    let (_, end) = app.line_span(row).expect("the URL's wrapped line");
    assert!(
        end > top.saturating_add(shown),
        "the line is cut by the viewport"
    );
    app.focus = Some(link.id);

    // Before the fix, the line's full wrapped height put the hint past the
    // conversation. Rendering then indexed beyond the buffer.
    let (buf, targets) = draw(&app);
    let layout = app.hint_layout(area);
    let gap = layout.gap.expect("the focused link reserves its hint");
    assert!(
        area.y <= gap && gap < area.bottom(),
        "gap {gap} in {area:?}"
    );
    let screen = crate::view::text(&buf);
    assert!(
        screen.contains(format::cut(crate::view::marks::HINT, usize::from(area.width)).as_str()),
        "{screen}"
    );
    assert!(
        targets
            .iter()
            .filter(|target| matches!(target.id, TargetId::Link { .. }))
            .all(|target| { target.rect.y >= area.y && target.rect.bottom() <= area.bottom() }),
        "link target outside the conversation: {targets:?}"
    );
    let hint_target = targets
        .iter()
        .find(|target| target.id == link.id && target.rect.y == gap)
        .expect("the hint row targets its focused link");
    assert_eq!(
        app.on_click(hint_target.id),
        Effect::OpenLink(url.to_owned())
    );
}

#[test]
fn a_focused_link_reserves_its_hint_above_the_new_below_pill() {
    use crate::theme::Role;
    use ratatui::style::Modifier;

    let text = "paragraph one\n\nparagraph two paragraph two paragraph two paragraph two\n\n[docs](https://example.com/a)\n\nafter";
    let mut app = App::new(PathBuf::from("/w"));
    assert!(app.on_line(hello()).is_empty());
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.set_size(40, 13);
    app.on_line(line(
        "turn_started",
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": " "}]}]}),
        None,
    ));
    app.on_line(line("text_completed", json!({ "text": text }), Some("a_m")));
    app.on_key(Key::PageUp, now());
    app.on_line(line("text_completed", json!({"text": "more"}), Some("a_n")));
    assert!(app.has_new());
    app.set_opener(true);
    let area = app.conversation_area();
    let link = app
        .visible_links(area)
        .into_iter()
        .find(|link| link.url == "https://example.com/a")
        .expect("the link is visible above the pill");
    let first = link.rects.first().expect("a link row");
    assert_eq!(first.y, area.bottom().saturating_sub(2), "penultimate row");
    app.focus = Some(link.id);

    let (buf, targets) = draw(&app);
    // Focus moves the link up one row, reserves its old row for the hint,
    // and leaves the new-below pill and its target on the last row.
    let gap = first.y;
    let hint: String = (first.x..first.x.saturating_add(22))
        .map(|x| buf[(x, gap)].symbol().to_owned())
        .collect();
    assert_eq!(hint, crate::view::marks::HINT);
    for x in first.x..first.x.saturating_add(22) {
        assert!(
            buf[(x, gap)].modifier.contains(Modifier::DIM),
            "hint at {x}"
        );
    }
    assert_eq!(
        buf[(first.x, first.y.saturating_sub(1))].bg,
        Role::SurfaceRaised.color(),
        "the focused link moves up"
    );
    let pill = targets
        .iter()
        .find(|target| target.id == crate::mouse::TargetId::NewBelow)
        .expect("the new-below pill keeps its target");
    assert_eq!(pill.rect.y, area.bottom().saturating_sub(1));
    let pill_row: String = (0..area.width)
        .map(|x| buf[(x, pill.rect.y)].symbol().to_owned())
        .collect();
    assert!(pill_row.contains("New messages below · End"), "{pill_row}");
    let hint_target = targets
        .iter()
        .find(|target| target.id == link.id && target.rect.y == gap)
        .expect("the hint row still targets the focused link");
    assert_eq!(hint_target.rect.height, 1);
    assert_ne!(pill.rect.y, hint_target.rect.y);
    assert_eq!(
        app.on_click(hint_target.id),
        Effect::OpenLink("https://example.com/a".to_owned())
    );
}

#[test]
fn a_focused_link_can_scroll_to_the_conversations_first_row_and_keep_its_hint() {
    use crate::theme::Role;

    let url = "http://example.com/a";
    let mut app = replied(60, 7, "a reply");
    app.set_opener(true);
    app.screen.pages_mut().add_shell(Some(url.to_owned()));
    app.settle();
    let area = app.conversation_area();
    assert_eq!(area.height, 2, "the conversation has two rows");
    let link = app
        .visible_links(area)
        .into_iter()
        .find(|link| link.url == url)
        .expect("the final shell row is a link");
    assert_eq!(link.rects[0].y, area.bottom().saturating_sub(1));
    app.focus = Some(link.id);

    let (buf, targets) = draw(&app);
    let gap = area.bottom().saturating_sub(1);
    assert_eq!(app.hint_layout(area).gap, Some(gap));
    assert_eq!(
        buf[(link.rects[0].x, area.y)].bg,
        Role::SurfaceRaised.color()
    );
    let hint: String = (link.rects[0].x..link.rects[0].x.saturating_add(22))
        .map(|x| buf[(x, gap)].symbol().to_owned())
        .collect();
    assert_eq!(hint, crate::view::marks::HINT);
    assert!(
        targets
            .iter()
            .any(|target| target.id == link.id && target.rect.y == gap)
    );
    assert_eq!(
        app.on_key(Key::Enter, now()),
        Effect::OpenLink(url.to_owned())
    );
}

#[test]
fn a_focused_link_filling_its_window_draws_no_hint() {
    // A one-row conversation window scrolled so the link's row is its
    // only row: no row below can free the hint's row and no row above
    // can give way, so only the focused look paints (`docs/tui.md`,
    // "Links"). PageUp steps one row in a one-row window.
    let mut app = replied(60, 6, "[docs](https://example.com/a)");
    app.set_opener(true);
    let mut found = None;
    for _ in 0..10 {
        let area = app.conversation_area();
        if let Some(entry) = app
            .visible_links(area)
            .into_iter()
            .find(|link| link.url == "https://example.com/a")
        {
            found = Some((entry.id, entry.rects.first().copied()))
        }
        if found.is_some() {
            break;
        }
        app.on_key(Key::PageUp, now());
    }
    let (id, rect) = found.expect("the link scrolls into the one-row window");
    let area = app.conversation_area();
    assert_eq!(area.height, 1);
    assert_eq!(rect.expect("a link row").y, area.y);
    app.focus = Some(id);
    assert!(app.hint_layout(area).gap.is_none(), "no room for the hint");
    assert_eq!(
        app.on_key(Key::Enter, now()),
        Effect::OpenLink("https://example.com/a".to_owned())
    );
}

#[test]
fn a_link_under_the_notice_overlay_is_not_a_target() {
    let mut app = replied(60, 12, "[docs](https://example.com/a)");
    app.on_line(line(
        "notice",
        json!({"code": "extension_failed", "message": "careful"}),
        None,
    ));
    app.open_more_notices();
    assert!(app.notice_overlay().is_some());
    assert!(links(&app).is_empty());
}

#[test]
fn a_press_on_a_link_still_starts_a_selection() {
    let mut app = replied(60, 12, "[docs](https://example.com/a) tail");
    let (_, targets) = draw(&app);
    let link = targets
        .iter()
        .find(|target| matches!(target.id, TargetId::Link { .. }))
        .expect("a link target");
    let at = (link.rect.x, link.rect.y);
    let report = |app: &mut App, kind: MouseKind, (col, row): (u16, u16)| {
        let (_, targets) = draw(app);
        app.on_select(&Mouse { kind, col, row }, &targets)
    };
    assert_eq!(
        report(&mut app, MouseKind::Press(Button::Left), at),
        Effect::None
    );
    let to = (at.0.saturating_add(6), at.1);
    assert_eq!(
        report(&mut app, MouseKind::Drag(Button::Left), to),
        Effect::None
    );
    assert!(app.select.span().is_some());
    assert!(matches!(
        report(&mut app, MouseKind::Release, to),
        Effect::Copy(_)
    ));
}

#[test]
fn a_bare_url_is_drawn_on_exactly_its_own_cells() {
    // Each URL starts or ends at a line edge or beside a space, so a
    // cell off at either end moves the rect's column or width.
    let url = "http://example.com/a";
    let cases = [
        ("see http://example.com/a here", 4, 20),
        ("http://example.com/a here", 0, 20),
        ("see http://example.com/a", 4, 20),
    ];
    for (text, col, width) in cases {
        let mut app = replied(60, 12, text);
        app.set_opener(true);
        let area = app.conversation_area();
        let found = app.visible_links(area);
        let link = found
            .iter()
            .find(|link| link.url == url)
            .unwrap_or_else(|| panic!("no bare link in {text:?}: {found:?}"));
        assert_eq!(link.rects.len(), 1, "{text:?}: {link:?}");
        let rect = link.rects[0];
        // The card's padding shifts the cells one column right.
        assert_eq!(
            (rect.x, rect.width),
            (area.x.saturating_add(col).saturating_add(1), width),
            "{text:?}"
        );
    }
}

#[test]
fn the_same_url_on_two_separated_rows_is_two_links() {
    let mut app = replied(60, 16, "http://a.example\n\nbetween\n\nhttp://a.example");
    app.set_opener(true);
    let found = app.visible_links(app.conversation_area());
    let hits: Vec<_> = found
        .iter()
        .filter(|link| link.url == "http://a.example")
        .collect();
    assert_eq!(hits.len(), 2, "{found:?}");
    assert!(hits.iter().all(|link| link.rects.len() == 1), "{hits:?}");
}

#[test]
fn two_different_urls_on_adjacent_rows_stay_two_links() {
    let mut app = replied(60, 12, "http://a.example  \nhttp://b.example");
    app.set_opener(true);
    let found = app.visible_links(app.conversation_area());
    let urls: Vec<&str> = found.iter().map(|link| link.url.as_str()).collect();
    assert_eq!(urls, ["http://a.example", "http://b.example"], "{found:?}");
}

#[test]
fn a_bare_url_in_a_prompt_bubble_is_on_its_drawn_cells() {
    // The bubble is right-aligned, so the link starts where the drawn
    // `http` starts, not where it would sit in a reply row.
    let prompt = "see http://example.com/a";
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(60, 12);
    assert!(app.on_line(hello()).is_empty());
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.on_line(line(
        "turn_started",
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": prompt}]}]}),
        None,
    ));
    app.on_line(line(
        "turn_completed",
        json!({"outcome": "completed"}),
        None,
    ));
    let area = app.conversation_area();
    let (buf, _) = draw(&app);
    let mut col = None;
    for y in area.top()..area.bottom() {
        let row: String = (area.left()..area.right())
            .map(|x| buf[(x, y)].symbol().to_owned())
            .collect();
        if let Some(at) = row.find("http") {
            col = Some(area.x.saturating_add(u16::try_from(at).unwrap_or(u16::MAX)));
            break;
        }
    }
    let col = col.expect("the prompt draws its URL");
    let found = app.visible_links(area);
    let link = found
        .iter()
        .find(|link| link.url == "http://example.com/a")
        .unwrap_or_else(|| panic!("no bare link in {prompt:?}: {found:?}"));
    assert_eq!(link.rects.len(), 1, "{link:?}");
    assert_eq!(
        (link.rects[0].x, link.rects[0].width),
        (col, 20),
        "{link:?}"
    );
}

#[test]
fn a_swapped_view_on_home_tints_the_focused_row_and_lifts_the_hovered_row() {
    use crate::home::Launch;
    use crate::swapped::Spot;
    use crate::theme::hover_bg;
    use ratatui::style::Modifier;
    // The `/keys` screen on home draws through the swapped-view early
    // return, with one target per display line and every action wrapped
    // to its tallest column count (`docs/tui.md:2100`: every
    // display line of a wrapped action takes a mouse target — the first
    // action wraps to two lines, so at least three display lines exist
    // to bar two and hover another).
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        ..Default::default()
    });
    app.set_size(80, 24);
    for ch in "/keys".chars() {
        assert_eq!(app.on_key(Key::Char(ch), now()), Effect::None);
    }
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(app.keys_screen_open());
    assert!(app.on_home() && app.swapped_view_open());
    let area = Rect::new(0, 0, 80, 24);
    let rows: Vec<(TargetId, Rect)> = {
        let mut buf = Buffer::empty(area);
        let targets = crate::view::render(&app, area, &mut buf, None);
        targets
            .into_iter()
            .filter(|target| matches!(target.id, TargetId::View(Spot::Row(_))))
            .map(|target| (target.id, target.rect))
            .collect()
    };
    assert!(
        rows.len() >= 3,
        "three lines to bar two and hover one: {rows:?}"
    );
    let (focus_id, focus_rect) = rows[0];
    let (_, cont_rect) = rows[1];
    let (hover_id, hover_rect) = rows[2];
    assert_ne!(focus_rect.y, hover_rect.y);
    app.focus = Some(focus_id);
    // The unhovered frame: the focused lines' base for the hover math.
    let mut base = Buffer::empty(area);
    crate::view::render(&app, area, &mut base, None);
    // Hover the first line outside the focused action.
    let pointer = (hover_rect.x, hover_rect.y);
    let mut buf = Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, Some(pointer));
    assert_eq!(hit(&targets, pointer.0, pointer.1), Some(hover_id));
    // The focused action keeps the focus style on every display line,
    // with the gutter mark on the first only and no hover lift: the
    // pointer sits elsewhere.
    for rect in [focus_rect, cont_rect] {
        for dx in 0..rect.width {
            let at = (rect.x.saturating_add(dx), rect.y);
            let cell = &buf[at];
            assert!(
                cell.modifier.contains(Modifier::REVERSED),
                "focused cell {dx}"
            );
            assert_eq!(cell.bg, base[at].bg, "focused cell {dx} unlifted");
        }
    }
    let first: String = (0..area.width)
        .map(|x| buf[(x, focus_rect.y)].symbol().to_owned())
        .collect();
    assert!(
        first.starts_with("› "),
        "gutter mark on the focused first line: {first:?}"
    );
    let second: String = (0..area.width)
        .map(|x| buf[(x, cont_rect.y)].symbol().to_owned())
        .collect();
    assert!(
        second.starts_with("  "),
        "no gutter mark on the continuation: {second:?}"
    );
    // The hovered row lifts each cell over its unhovered background,
    // with no focus style: focus sits elsewhere.
    for dx in 0..hover_rect.width {
        let at = (hover_rect.x.saturating_add(dx), hover_rect.y);
        let cell = &buf[at];
        assert_eq!(cell.bg, hover_bg(base[at].bg), "hovered cell {dx}");
        assert!(
            !cell.modifier.contains(Modifier::REVERSED),
            "hovered cell {dx}"
        );
    }
}
