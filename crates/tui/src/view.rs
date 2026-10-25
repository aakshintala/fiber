//! Drawing the terminal: the conversation's styled lines, the notices over
//! it, the quit hint, the approval badge, the steering queue, and the input
//! box or the approval panel in its place (`docs/tui.md`, "Turns", "The
//! input box", "Steering", "Notices", "Approvals and questions"), with the
//! click target under the pointer tinted ("Mouse and hover").

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget, Wrap};

use crate::app::{App, Surface};
use crate::completion_rows::Rows;
use crate::markdown::{Role, style};
use crate::mouse::{self, Target, TargetId};
use crate::text::to_u16;

pub(crate) mod banner;
pub(crate) mod chrome;
mod clear;
mod completions;
mod drag;
mod history;
#[path = "home_view.rs"]
mod home;
mod input_box;
pub(crate) mod item;
mod key_map;
mod logo;
pub(crate) mod marks;
pub(crate) mod model_picker;
mod notice_rows;
mod offer;
pub(crate) mod overlay;
pub(crate) mod panel;
mod quit;
pub(crate) mod rail;
pub(crate) mod request;
mod results;
mod reviewer_setup;
pub(crate) mod scroll_bar;
pub(crate) mod status_rows;
mod steering_queue;
mod working_line;

pub(crate) use home::list_window;
pub(crate) use home::max_question_scroll;
use home::notice_rows_area;

/// The overlay shown while scrolled up once new output arrives.
const NEW_BELOW: &str = " ↓ New messages below · End ";

/// Shown in the conversation's top-right corner after a copy, below
/// the search box when it is open.
const COPIED: &str = " Copied ";

/// The approval panel's tint when a standing rule or review asked.
pub(crate) const APPROVAL_TINT: Style = Style::new().bg(Role::Approval.color());

/// The approval panel's tint when the reviewer escalated.
pub(crate) const ALERT_TINT: Style = Style::new().bg(Role::Alert.color());

/// Paints `rect` as the click target under the pointer: each cell takes
/// its own background 0x14 lighter on each channel where it sits on a
/// tint, and the hover tint where it has none (`docs/tui.md`, palette,
/// `hover`).
pub(crate) fn lift_hover(buf: &mut Buffer, rect: Rect) {
    let area = buf.area.intersection(rect);
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let bg = buf[(x, y)].bg;
            buf[(x, y)].set_bg(crate::theme::hover_bg(bg));
        }
    }
}

/// The focused click target's style in navigate mode: reversed, so it
/// shows on every theme and with no colour.
pub(crate) const FOCUS_STYLE: Style = Style::new().add_modifier(Modifier::REVERSED);

/// One line, wrapped the way it draws.
pub(crate) fn paragraph(line: Line<'_>) -> Paragraph<'_> {
    Paragraph::new(line).wrap(Wrap { trim: false })
}

/// How many rows `line` takes at `width`: one when it fits, without
/// wrapping it.
pub(crate) fn rows(line: Line<'_>, width: u16) -> usize {
    if line.width() <= usize::from(width) {
        return 1;
    }
    paragraph(line).line_count(width).max(1)
}

/// The slab the quit question or the notice overlay draws now, or an
/// empty rect when `frame` is not open: it draws that frame alone over the
/// area the real frame gives it, into a scratch buffer, and takes the slab
/// the draw returns. The wheel over the slab is the frame's, whatever lies
/// beneath.
pub(crate) fn frame_of(app: &App, frame: Surface) -> Rect {
    let (width, height) = app.screen_size();
    let screen = Rect::new(0, 0, width, height);
    let swapped = app.config_view_open() || app.model_picker_open() || app.keys_screen_open();
    let column = app.chrome().layout().map_or(screen, |layout| layout.column);
    let area = match (app.home_screen(), frame) {
        (Some(home), Surface::Notice) if !swapped => notice_rows_area(app, &home, screen),
        (Some(_), _) => screen,
        (None, Surface::Notice) => Rect::new(
            column.x,
            column.y,
            column.width,
            u16::try_from(app.conversation_height()).unwrap_or(u16::MAX),
        ),
        (None, _) => column,
    };
    let mut buf = Buffer::empty(screen);
    let mut targets = Vec::new();
    if frame == Surface::Quit {
        quit::draw(app, area, &mut buf, &mut targets)
    } else if frame == Surface::Clear {
        clear::draw(app, area, &mut buf, &mut targets)
    } else if frame == Surface::Notice {
        notice_rows::overlay(app, area, &mut buf, &mut targets)
    } else {
        Rect::default()
    }
}

/// Draws the open framed surfaces, bottom first, so the one on top draws
/// last: `report` draws the import report where the caller's view puts
/// it, the quit question over `area`. Each notes the slab it drew, so a
/// click outside the overlay on top closes it (`docs/tui.md`, "Bindings").
fn draw_framed(
    app: &App,
    area: Rect,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
    mut report: impl FnMut(&mut Vec<Target>, &mut Buffer) -> Rect,
) {
    for surface in app.framed_surfaces() {
        if surface == Surface::Report {
            let slab = report(targets, buf);
            app.frames().note(Surface::Report, slab);
        } else if surface == Surface::Quit {
            let slab = quit::draw(app, area, buf, targets);
            app.frames().note(Surface::Quit, slab);
        } else if surface == Surface::Clear {
            let slab = clear::draw(app, area, buf, targets);
            app.frames().note(Surface::Clear, slab);
        }
    }
}

/// Drops the click targets of what an overlay covers: the conversation's
/// lines, links, results, offer and turns.
fn drop_conversation_targets(targets: &mut Vec<Target>) {
    targets.retain(|target| {
        !matches!(
            target.id,
            TargetId::Line(_)
                | TargetId::Link { .. }
                | TargetId::FindResult(_)
                | TargetId::NewBelow
                | TargetId::Turn(_)
                | TargetId::Offer(_)
        )
    });
}

/// Notes the slab `swapped::draw` drew: the model picker's under the
/// picker, else the `/login` or `/logout` view's under its configuration
/// view. Every other view draws full-area with no frame of its own.
fn note_swapped(app: &App, slab: Option<Rect>) {
    let Some(slab) = slab else {
        return;
    };
    let surface = if app.model_picker_open() {
        Surface::Picker
    } else {
        Surface::ConfigView
    };
    app.frames().note(surface, slab);
}

/// Draws `app` into `area` of `buf`, from the bottom up: the narrow
/// layout's status rows on the last rows, then the input box with a
/// completion panel above it, or the approval panel in their place, then
/// the steering queue, the reconnect banner, the working line, the
/// notice rows above it, and the badge and the quit hint when shown, and
/// the conversation in the rows left, or the key map over them while it
/// is open. A screen too short for them all drops the hint first,
/// then the badge, then the notice rows. A panel taller than the screen keeps
/// its top, except that a question form scrolls to keep its cursor's row
/// shown.
///
/// Returns the click targets drawn, in draw order. Last, the target under
/// `pointer`, if any, gets [`lift_hover`] as its background.
pub(crate) fn render(
    app: &App,
    area: Rect,
    buf: &mut Buffer,
    pointer: Option<(u16, u16)>,
) -> Vec<Target> {
    // The frame's asks start here: `Screen::draw_with` may render twice,
    // and the loop takes what the drawn frame asked.
    app.motion().clear_wake();
    // The framed slabs start here too: each framed surface notes the slab
    // it draws, so a click outside the overlay on top closes it.
    app.frames().clear();
    // Below the floor one line shows, home included; home draws while no
    // session is on screen; the conversation draws in its column once one
    // attaches.
    if let Some(line) = app.chrome().floor_line() {
        chrome::floor(line, area, buf);
        return Vec::new();
    }
    // A swapped view takes home's place (`docs/tui.md`, "Swapped
    // views"): the `/keys` screen, the configuration view or the model
    // picker. The notice rows draw over its bottom rows, unreserved, and
    // the overlay over the whole area (`docs/tui.md`, "Notices").
    if (app.config_view_open() || app.model_picker_open() || app.keys_screen_open())
        && app.on_home()
    {
        let mut targets = Vec::new();
        let slab = crate::swapped::draw(app, area, buf, &mut targets);
        note_swapped(app, slab);
        let slab = notice_rows::overlay(app, area, buf, &mut targets);
        // debt: five note sites stay inline; extract a shared note helper if a sixth framed surface appears.
        app.frames().note(Surface::Notice, slab);
        let mut rows_bottom = area.bottom();
        notice_rows::draw(app, area, buf, &mut rows_bottom, &mut targets);
        // The import report and the quit question take every key over
        // these views, so they draw over them too, the quit question
        // last: without it Enter would quit while the choices never show.
        draw_framed(app, area, buf, &mut targets, |targets, buf| {
            crate::import_report::draw_report(app, area, buf, targets)
        });
        if let Some(id) = pointer.and_then(|(col, row)| mouse::hit(&targets, col, row)) {
            for target in targets.iter().filter(|target| target.id == id) {
                lift_hover(buf, target.rect);
            }
        }
        if let Some(id) = app.focused() {
            for target in targets.iter().filter(|target| target.id == id) {
                buf.set_style(target.rect, FOCUS_STYLE);
            }
        }
        return targets;
    }
    if let Some(screen) = app.home_screen() {
        return home::render(app, &screen, area, buf, pointer);
    }
    let area = match app.chrome().layout() {
        Some(layout) => {
            chrome::draw(&layout, buf);
            layout.column
        }
        None => area,
    };
    let mut targets = Vec::new();
    if let Some(rect) = app.chrome().layout().and_then(|layout| layout.panel) {
        panel::draw(app, rect, buf, &mut targets);
    }
    if let Some(rect) = app.chrome().layout().and_then(|layout| layout.rail) {
        rail::draw(app, rect, buf, pointer, &mut targets);
    }
    let mut bottom = area.bottom();
    // The input box's top-edge row, once it draws; the `/` and `@`
    // overlay sits above it.
    let mut box_top = bottom;
    status_rows::draw_status(app, area, buf, &mut bottom, &mut targets);
    if let Some(panel) = app.panel() {
        bottom = request::draw(&panel, area, bottom, buf, &mut targets);
    }
    if app.panel().is_none() {
        box_top = input_box::draw(app, area, &mut bottom, buf, &mut targets);
    }
    status_rows::draw_widget(app, area, buf, &mut bottom, &mut targets);
    // The steering queue sits above the input box, its newest row lowest;
    // a row a `steer` sent ends in a ✕ that drops it.
    steering_queue::draw(app, area, &mut bottom, buf, &mut targets);
    status_rows::draw_delegates(app, area, buf, &mut bottom, &mut targets);
    banner::draw(app, area, buf, &mut bottom);
    working_line::draw(app, area, buf, &mut bottom, &mut targets);
    // The notice rows sit directly above the working line, oldest first
    // (`docs/tui.md`, "Notices").
    notice_rows::draw(app, area, buf, &mut bottom, &mut targets);
    if let Some(rect) = app
        .badge()
        .and_then(|badge| put_spans(buf, area, &mut bottom, badge_spans(&badge)))
    {
        targets.push(Target {
            id: TargetId::Badge,
            rect,
        });
    }
    if app.hint() {
        put(buf, area, &mut bottom, &app.hint_text(), Style::default());
    }
    let rows = bottom.saturating_sub(area.y);
    let conversation = Rect::new(area.x, area.y, area.width, rows);
    match app.keymap_top() {
        Some(_) => {
            let slab = key_map::draw(app, conversation, buf, &mut targets);
            // With a session attached the key map notes its frame; on
            // home it is the full-screen drawing with no frame of its
            // own.
            if app.session().is_some() {
                app.frames().note(Surface::Keymap, slab);
            }
        }
        // A swapped view takes the conversation's place: the `/keys`
        // screen, the configuration view, model picker or session view.
        // The notice rows sit below it in the reserved rows, and the
        // overlay draws over it (`docs/tui.md`, "Notices").
        None if app.config_view_open()
            || app.model_picker_open()
            || app.keys_screen_open()
            || app.session_view_open() =>
        {
            let slab = crate::swapped::draw(app, conversation, buf, &mut targets);
            note_swapped(app, slab);
            let slab = notice_rows::overlay(app, conversation, buf, &mut targets);
            app.frames().note(Surface::Notice, slab);
        }
        // An item view swaps into the conversation area and takes all of
        // it: its header on the top rows, then the delegate's transcript
        // or the item's body (`docs/tui.md`, "Swapped views").
        None if app.item_open() => {
            let height = usize::from(conversation.height);
            let header_rows = item::ITEM_HEADER_ROWS.min(height);
            let header_height = to_u16(header_rows);
            let header = Rect::new(
                conversation.x,
                conversation.y,
                conversation.width,
                header_height,
            );
            item::draw_header(app, header, buf, &mut targets);
            let body_y = conversation.y.saturating_add(header_height);
            let body_height = conversation.height.saturating_sub(header_height);
            let body = Rect::new(conversation.x, body_y, conversation.width, body_height);
            if app.item_view().is_some_and(|view| view.has_transcript) {
                let inset = crate::layout::past_gutter(body, app.chrome().gutter());
                let _ = conversation_rows(app, inset, buf, &mut targets);
                marks::draw(app, inset, buf, &mut targets, app.hint_layout(inset).gap);
            } else {
                item::draw_body(app, body, buf);
            }
            let slab = notice_rows::overlay(app, conversation, buf, &mut targets);
            app.frames().note(Surface::Notice, slab);
        }
        None => {
            // The repository offer swaps in for the conversation.
            match app.offer_rows(conversation.width) {
                Some((rows, top)) => offer::render(&rows, top, conversation, buf, &mut targets),
                // The search results swap in for the conversation while
                // open (`docs/tui.md`, "Search", "Swapped views").
                None => match app.find_results() {
                    Some(view) => {
                        results::render(&view, conversation, buf, &mut targets);
                        marks::draw(app, conversation, buf, &mut targets, None);
                    }
                    None => {
                        // The rows draw in the scroll bar's rows area, one
                        // column narrower than the conversation, leaving
                        // the last column to the bar, which draws before
                        // the marks over it. The rows keep one blank column
                        // on the left while the layout applies.
                        let inset = crate::layout::past_gutter(conversation, app.chrome().gutter());
                        let (rows, bar) = scroll_bar::split(inset);
                        let pin = conversation_rows(app, rows, buf, &mut targets);
                        scroll_bar::draw(app, bar, buf);
                        marks::draw(app, inset, buf, &mut targets, app.hint_layout(inset).gap);
                        // The pinned header over row 0, drawn as the row
                        // it stands for, after the marks pass: never in an
                        // item transcript, and never while following.
                        if let Some(pin) = pin {
                            let rect = Rect::new(rows.x, rows.y, rows.width, 1);
                            draw_row(pin.line, rect, buf);
                            targets.push(Target {
                                id: TargetId::Line(pin.target),
                                rect,
                            });
                        }
                    }
                },
            }
            let slab = notice_rows::overlay(app, conversation, buf, &mut targets);
            app.frames().note(Surface::Notice, slab);
        }
    }
    drag::draw(app, buf, pointer);
    // The completion panels draw over the conversation's bottom rows,
    // above the input box's top edge, after everything under them, so
    // their cells win. The Ctrl+R panel draws centred over the
    // conversation instead of in reserved rows.
    if let Some(completions) = app.completions() {
        let search = matches!(completions.rows, Rows::Search(_));
        let slab = completions::draw(&completions, conversation, box_top, buf, &mut targets);
        app.frames().note(
            if search {
                Surface::Search
            } else {
                Surface::Completion
            },
            slab,
        );
    }
    // The surfaces that draw a frame of their own go up bottom first, so
    // the one on top draws last. The import report draws over the
    // conversation: one overlay with both counts, no notices
    // (`docs/tui.md`, "Overlays"). The conversation's own targets go
    // away under it, as under the notices overlay. The quit question
    // draws above the report, as it takes every key while open.
    // The reviewer setup draws first among them: the report and the quit
    // question draw over it, as they take every key before it does.
    let setup = reviewer_setup::draw(app, conversation, buf, &mut targets);
    if !setup.is_empty() {
        drop_conversation_targets(&mut targets);
    }
    app.frames().note(Surface::ReviewerSetup, setup);
    draw_framed(app, area, buf, &mut targets, |targets, buf| {
        drop_conversation_targets(targets);
        crate::import_report::draw_report(app, conversation, buf, targets)
    });
    if let Some(id) = pointer.and_then(|(col, row)| mouse::hit(&targets, col, row)) {
        for target in targets.iter().filter(|target| target.id == id) {
            lift_hover(buf, target.rect);
        }
    }
    if let Some(id) = app.focused()
        && !matches!(id, TargetId::Link { .. })
    {
        // Links draw their own focused look in the marks, on the
        // `surface_raised` tint with the hint under them
        // (`docs/tui.md`, "Links").
        for target in targets.iter().filter(|target| target.id == id) {
            buf.set_style(target.rect, FOCUS_STYLE);
        }
    }
    targets
}

/// The badge's spans: `⚠` in `attention`, the rest bold in `attention`
/// (`docs/tui.md`, "An approval").
fn badge_spans(badge: &str) -> Vec<Span<'_>> {
    let rest = Style::new()
        .fg(Role::Attention.color())
        .add_modifier(Modifier::BOLD);
    let mark = Style::new().fg(Role::Attention.color());
    let end = badge
        .find('⚠')
        .map(|at| at.saturating_add('⚠'.len_utf8()))
        .unwrap_or_default();
    match (badge.get(..end), badge.get(end..)) {
        (Some(head), Some(tail)) => {
            vec![Span::styled(head, mark), Span::styled(tail, rest)]
        }
        _ => vec![Span::styled(badge, rest)],
    }
}

/// Puts `spans` on the row above `bottom` and moves `bottom` up to it,
/// returning the cells their text took; nothing once `bottom` reaches the
/// top of `area`.
fn put_spans(buf: &mut Buffer, area: Rect, bottom: &mut u16, spans: Vec<Span<'_>>) -> Option<Rect> {
    let row = bottom.checked_sub(1).filter(|row| *row >= area.y)?;
    let mut x = area.x;
    for span in spans {
        let room = usize::from(area.width).saturating_sub(usize::from(x.saturating_sub(area.x)));
        let (end, _) = buf.set_stringn(x, row, &span.content, room, span.style);
        x = end;
    }
    *bottom = row;
    Some(Rect::new(area.x, row, x.saturating_sub(area.x), 1))
}

/// Puts `text` on the row above `bottom` and moves `bottom` up to it,
/// returning the cells its text took; nothing once `bottom` reaches the top
/// of `area`.
fn put(buf: &mut Buffer, area: Rect, bottom: &mut u16, text: &str, style: Style) -> Option<Rect> {
    let row = bottom.checked_sub(1).filter(|row| *row >= area.y)?;
    let (end, _) = buf.set_stringn(area.x, row, text, usize::from(area.width), style);
    *bottom = row;
    Some(Rect::new(area.x, row, end.saturating_sub(area.x), 1))
}

/// Where the terminal cursor shows: at the draft's cursor while the input
/// box has focus, at a question form's text cursor while its words row has
/// the cursor, `None` while navigating, any other panel or the repository
/// offer is open, or the cursor's row is off a screen too short for it. A
/// shown form's caret wins over an open repository offer, which already
/// gives the panel its keys.
pub(crate) fn cursor(app: &App, area: Rect) -> Option<Position> {
    // A swapped view draws any field caret in its frame; it has no
    // input-box caret.
    if app.chrome().floor_line().is_some()
        || app.config_view_open()
        || app.model_picker_open()
        || app.keys_screen_open()
        || app.session_view_open()
    {
        return None;
    }
    if let Some(screen) = app.home_screen() {
        return home::cursor(app, &screen, area);
    }
    let area = app.chrome().layout().map_or(area, |layout| layout.column);
    if app.focused().is_some() {
        return None;
    }
    // A shown panel wins over the offer: the panel takes the keys, so a
    // form's caret takes the cursor.
    if let Some(panel) = app.panel() {
        return request::caret(&panel, area, status_rows::input_bottom(app, area));
    }
    // The offer hides the draft's cursor while it swaps in for the
    // conversation.
    if app.offer_open() {
        return None;
    }
    // The search bar's cursor at its query's end, while it is open
    // (`docs/tui.md`, "Search"). While the results view is open the bar
    // is hidden, so its cursor hides too; the bar state is untouched.
    if let Some(bar) = app.find_bar() {
        return if app.results_open() {
            None
        } else {
            marks::bar_cursor(&bar, area)
        };
    }
    let (below, col) = input_box::cursor_row(app, area.width, usize::from(area.height));
    let y = status_rows::input_bottom(app, area)
        .checked_sub(below)
        .filter(|y| *y >= area.y)?;
    let x = area.x.saturating_add(col.min(area.width.saturating_sub(1)));
    Some(Position::new(x, y))
}

/// Whether the line draws on a card tint: a turn, surface or handoff
/// card, whose sides draw as a thin box without colour, and which marks
/// the pinned row's tint (`docs/tui.md`, "Look").
fn card_tint(line: &Line<'_>) -> bool {
    line.style.bg == Some(Role::Turn.color())
        || line.style.bg == Some(Role::Surface.color())
        || line.style.bg == Some(Role::Handoff.color())
}

/// Draws one conversation row into its rect: the line with its own
/// background filling the column, and, without colour on a card tint,
/// the thin-box sides (`docs/tui.md`, "Look").
fn draw_row(line: Line<'_>, rect: Rect, buf: &mut Buffer) {
    let turn_tint = card_tint(&line);
    let fill = Style {
        bg: line.style.bg,
        ..Style::default()
    };
    paragraph(line).style(fill).render(rect, buf);
    if crate::surface::box_fit(rect.width) && turn_tint {
        crate::surface::draw_sides(buf, rect);
    }
}

/// The row of the " Copied " confirmation in `area`, when drawn: on the
/// row under the bar when the bar draws, else on the top row
/// (`docs/tui.md`, "Selection and copy").
fn copied_row(app: &App, area: Rect) -> Option<u16> {
    if !app.copied() {
        return None;
    }
    let offset = if app.find_bar().is_some() && !app.results_open() {
        // The bar draws in the conversation's area with the scroll
        // bar's column; this area drops that column on the main path.
        let room = Rect::new(area.x, area.y, area.width.saturating_add(1), area.height);
        match marks::bar_rect(room).map(|rect| rect.height) {
            Some(3) => 5,
            Some(_) => 1,
            None => 0,
        }
    } else {
        0
    };
    Some(area.y.saturating_add(offset))
}

/// Blanks the hint's reserved row with its card's tint, so no previous
/// frame's cells show past the hint (`docs/tui.md`, "Links").
fn blank_row(buf: &mut Buffer, area: Rect, y: u16, style: Style) {
    let width = usize::from(area.width);
    buf.set_stringn(area.x, y, " ".repeat(width), width, style);
}

/// Draws the conversation's visible rows, bottom-aligned while it is
/// shorter than its area. Only the pages that draw a visible row are read;
/// a page not loaded draws blank rows. Pushes a target over each shown line
/// that opens something, and over "↓ New messages below" when drawn.
/// Returns the pinned header for the caller to draw after the marks pass,
/// so covered-row selection, find and link looks never land on the pin
/// (`docs/tui.md`, "Pinned call").
fn conversation_rows(
    app: &App,
    area: Rect,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
) -> Option<crate::window::Pin> {
    let height = usize::from(area.height);
    // The focused link's hint reserves the row under its line, laid out
    // with the targets: rows on and below it draw one lower, so hint
    // text and click targets agree (`docs/tui.md`, "Links").
    let placed = app.hint_layout(area);
    let top = placed.top;
    let gap = placed.gap;
    let mut y = placed.y0;
    let end = top.saturating_add(height);
    let crate::window::Shown {
        first,
        lines,
        turns,
        spins,
        pin,
    } = app.shown(top, height);
    // Where this frame's turn targets splice in: after the targets
    // drawn before the conversation, before its line targets.
    let base = targets.len();
    let mut start = first;
    let overlay = app.has_new() && area.height > 0;
    let last = area.bottom().saturating_sub(u16::from(overlay));
    // The card tint above the hint's row: the row the hint reserves
    // ends its card, so the blank row keeps it.
    let mut under = Style::default();
    // Each line's cells on screen: its y and how many of its rows show,
    // laid out with the gap in the loop below. The turns loop after it
    // reads them.
    let mut layout: Vec<(u16, u16)> = Vec::with_capacity(lines.len());
    // The pinned header covers the top row's own line: that line keeps
    // no hit, so the pin is the click and focus target on row 0 and the
    // covered row steps or reveals out from under it (`docs/tui.md`,
    // "Pinned call").
    let mut pin = if !app.item_open() && app.top().is_some() && area.height > 0 {
        pin
    } else {
        None
    };
    // On the top row the " Copied " confirmation wins over the pin,
    // which yields (`docs/tui.md`, "Pinned call").
    if copied_row(app, area) == Some(area.y) {
        pin = None;
    }
    let pinned = pin.is_some();
    // A line wholly above `top` or below `end` shows no rows.
    for (at, (line, rows, open)) in lines.into_iter().enumerate() {
        let next = start.saturating_add(rows);
        let skip = top.saturating_sub(start);
        let count = next.min(end).saturating_sub(start.max(top));
        if gap.is_some_and(|gap| gap == y) {
            blank_row(buf, area, y, under);
            y = y.saturating_add(1);
        }
        if y >= area.bottom() {
            break;
        }
        layout.push((y, to_u16(count)));
        let room = area.bottom().saturating_sub(y);
        let take = count.min(usize::from(room));
        let rect = Rect::new(area.x, y, area.width, to_u16(take));
        // A short row and the wrapped rows of a long one fill the column
        // with the row's own background, so a card's surface tint reaches
        // the column's edge (`docs/tui.md`, "Look").
        let turn_tint = card_tint(&line);
        let fill = Style {
            bg: line.style.bg,
            ..Style::default()
        };
        paragraph(line)
            .style(fill)
            .scroll((to_u16(skip), 0))
            .render(rect, buf);
        under = fill;
        // Without colour a turn's card is a thin box: its edge rows
        // already draw ┌─┐ and └─┘, and its body rows take │ on the
        // card's columns (`docs/tui.md`, "Look"). The markers still
        // name the surface until the paint pass, so the pads the card
        // keeps are decorated without moving text; the prompt bubble
        // stays edge-free, and a cut card draws no corner it withheld.
        if crate::surface::box_fit(area.width) && turn_tint {
            crate::surface::draw_sides(buf, rect);
        }
        // A marked line spins its cell on the working line's tick; the
        // mark is the builders' promise that the line moves.
        if let Some(col) = spins
            .iter()
            .find_map(|(marked, col)| (*marked == at).then_some(*col))
        {
            working_line::spin(
                app,
                buf,
                area,
                working_line::Drawn {
                    y,
                    col,
                    first_row_shown: skip == 0,
                    rows,
                    count,
                },
            );
        }
        if let Some(open) = open {
            let height = rect.height.min(last.saturating_sub(rect.y));
            // A code block's copy target is its `copy` cells, not the line.
            let cells = app.copy_cells(open).map_or(rect, |cols| Rect {
                x: area.x.saturating_add(cols.start),
                width: cols.end.saturating_sub(cols.start),
                ..rect
            });
            if height > 0 && !(pinned && y == area.y) {
                targets.push(Target {
                    id: TargetId::Line(open),
                    rect: Rect { height, ..cells },
                });
            }
        }
        y = y.saturating_add(to_u16(count));
        start = next;
    }
    // Turns are focus stops over the rows their page cards draw; they
    // are not click targets, so the line targets drawn before them win
    // clicks: their targets splice in front, in draw order.
    let mut turned: Vec<Target> = Vec::new();
    for (at, range) in turns {
        let mut rect: Option<Rect> = None;
        for (line, (line_y, count)) in layout.iter().enumerate() {
            if range.contains(&line) {
                rect = Some(match rect {
                    Some(before) => Rect {
                        height: before.height.saturating_add(*count),
                        ..before
                    },
                    None => Rect::new(area.x, *line_y, area.width, *count),
                });
            }
        }
        if let Some(rect) = rect {
            let height = rect.height.min(last.saturating_sub(rect.y));
            if height > 0 && !(pinned && y == area.y) {
                turned.push(Target {
                    id: TargetId::Turn(at),
                    rect: Rect { height, ..rect },
                });
            }
        }
    }
    targets.splice(base..base, turned);
    // The hint's row past the last line, when the scroll freed it
    // there: blank it with the card tint, as in the loop.
    if gap.is_some_and(|gap| gap == y) {
        blank_row(buf, area, y, under);
    }
    if overlay {
        // The pill over the conversation's bottom row: its ends and its
        // label reset the cells they cover, keeping the background under
        // them, so no modifier from the row below survives
        // (`docs/tui.md`, "Turns"). The row is never blanked.
        let row = area.bottom().saturating_sub(1);
        let mut cells: Vec<(String, bool)> = Vec::with_capacity(30);
        cells.push(("▐".to_owned(), false));
        cells.extend(
            Line::raw(NEW_BELOW)
                .styled_graphemes(Style::default())
                .map(|grapheme| (grapheme.symbol.to_owned(), true)),
        );
        cells.push(("▌".to_owned(), false));
        let start = area
            .x
            .saturating_add(area.width.saturating_sub(to_u16(cells.len())) / 2);
        let end = start.saturating_add(to_u16(cells.len())).min(area.right());
        for (x, (symbol, is_label)) in (start..end).zip(cells) {
            let cell = &mut buf[(x, row)];
            let bg = cell.bg;
            cell.reset();
            cell.bg = bg;
            cell.set_symbol(&symbol);
            if is_label {
                cell.set_fg(Role::Attention.color());
                cell.set_bg(Role::Rule.color());
            } else {
                cell.set_fg(Role::Rule.color());
            }
        }
        targets.push(Target {
            id: TargetId::NewBelow,
            rect: Rect::new(start, row, end.saturating_sub(start), 1),
        });
    }
    if let Some(y) = copied_row(app, area) {
        // " Copied " in `info` on the `surface` tint
        // (`docs/tui.md`, "Selection and copy").
        // The row under a five-row bar in a five-row area is the next
        // region's: omit the confirmation rather than cover it, and clip
        // its width to the area (`docs/tui.md`, "Selection and copy").
        if y < area.bottom() {
            let width = to_u16(crate::format::width(COPIED)).min(area.width);
            let x = area.x.saturating_add(area.width.saturating_sub(width));
            buf.set_style(
                Rect::new(x, y, width, 1),
                Style::new().bg(Role::Surface.color()),
            );
            buf.set_stringn(x, y, COPIED, usize::from(width), style(Role::Info));
        }
    }
    pin
}

/// Where the conversation is scrolled to.
#[derive(Debug, Default)]
pub(crate) struct Scroll {
    /// The top wrapped row while scrolled up; `None` follows new output.
    pub(crate) top: Option<usize>,
    /// New output arrived while scrolled up.
    pub(crate) has_new: bool,
}

impl Scroll {
    /// New output while scrolled up shows the overlay; the view stays put.
    pub(crate) fn changed(&mut self) {
        if self.top.is_some() {
            self.has_new = true;
        }
    }

    /// PageUp: up by `step` from the top row, or from `bottom` when
    /// following.
    pub(crate) fn up(&mut self, step: usize, bottom: usize) {
        let top = self.top.unwrap_or(bottom);
        self.top = Some(top.saturating_sub(step));
    }

    /// PageDown: down by `step`, following again on reaching `bottom`.
    pub(crate) fn down(&mut self, step: usize, bottom: usize) {
        let Some(top) = self.top else {
            return;
        };
        let next = top.saturating_add(step);
        if next >= bottom {
            self.follow();
        } else {
            self.top = Some(next);
        }
    }

    /// End jumps to the bottom and resumes following.
    pub(crate) fn follow(&mut self) {
        self.top = None;
        self.has_new = false;
    }
}

/// The screen as text, one row per line, trailing spaces trimmed.
pub(crate) fn text(buf: &Buffer) -> String {
    let area = buf.area;
    let mut out = String::new();
    for y in area.top()..area.bottom() {
        let mut row = String::new();
        for x in area.left()..area.right() {
            if let Some(cell) = buf.cell((x, y)) {
                row.push_str(cell.symbol());
            }
        }
        out.push_str(row.trim_end());
        out.push('\n');
    }
    out
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod tests;
