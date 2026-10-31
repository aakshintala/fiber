//! Drawing the terminal: the conversation's styled lines, the notices over
//! it, the quit hint, the approval badge, the steering queue, and the input
//! box or the approval panel in its place (`docs/tui.md`, "Turns", "The
//! input box", "Steering", "Notices", "Approvals and questions"), with the
//! click target under the pointer tinted ("Mouse and hover").

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use crate::app::{App, Frame, Surface};
use crate::completion_rows::Rows;
use crate::mouse::{self, Target, TargetId};
use crate::text::to_u16;
use crate::theme::Role;

pub(crate) mod banner;
pub(crate) mod chrome;
mod clear;
mod completions;
mod conversation;
pub(crate) mod cursor;
mod drag;
mod history;
mod home;
mod hooks_offer;
mod input_box;
pub(crate) mod item;
mod key_map;
mod logo;
pub(crate) mod marks;
pub(crate) mod model_picker;
pub(crate) mod notice_rows;
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
pub(crate) mod steering_queue;
pub(crate) mod working_line;

pub(crate) use home::list_window;
use home::notice_rows_area;
pub(crate) use home::question::max_question_scroll;

/// The approval panel's tint when a standing rule or review asked.
pub(crate) const APPROVAL_TINT: Style = Style::new().bg(Role::Approval.color());

/// The approval panel's tint when the reviewer escalated.
pub(crate) const ALERT_TINT: Style = Style::new().bg(Role::Alert.color());

/// Paints `rect` as the click target under the pointer: each cell takes
/// its own background 0x14 lighter on each channel where it sits on a
/// tint, and the hover tint where it has none (`docs/tui.md`, palette,
/// `hover`). Placeholder cells keep their colours: tinting one would
/// destroy the image id they carry (`docs/tui.md`, "Images").
pub(crate) fn lift_hover(buf: &mut Buffer, rect: Rect) {
    let area = buf.area.intersection(rect);
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if crate::graphics::is_placeholder(buf[(x, y)].symbol()) {
                continue;
            }
            let bg = buf[(x, y)].bg;
            buf[(x, y)].set_bg(crate::theme::hover_bg(bg));
        }
    }
}

/// Paints `rect` with the focus style, leaving placeholder cells alone:
/// reversing one would destroy the image id its colours carry
/// (`docs/tui.md`, "Images").
fn focus_style(buf: &mut Buffer, rect: Rect) {
    let area = buf.area.intersection(rect);
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if crate::graphics::is_placeholder(buf[(x, y)].symbol()) {
                continue;
            }
            buf[(x, y)].set_style(FOCUS_STYLE);
        }
    }
}

/// The focused click target's style in navigate mode: reversed, so it
/// shows on every theme and with no colour.
pub(crate) const FOCUS_STYLE: Style = Style::new().add_modifier(Modifier::REVERSED);

/// Paints the hover tint under `hover`'s target, then the focus style on
/// the focused target, so focus wins the cells they share. A focused link
/// keeps the `surface_raised` look the marks pass paints, with its hint
/// under it, so the generic focus style skips links
/// (`docs/tui.md`, "Links").
fn paint_tints(
    buf: &mut Buffer,
    targets: &[Target],
    hover: Option<TargetId>,
    focused: Option<TargetId>,
) {
    if let Some(id) = hover {
        for target in targets.iter().filter(|target| target.id == id) {
            lift_hover(buf, target.rect);
        }
    }
    if let Some(id) = focused
        && !matches!(id, TargetId::Link { .. })
    {
        for target in targets.iter().filter(|target| target.id == id) {
            focus_style(buf, target.rect);
        }
    }
}

/// One line, wrapped the way it draws.
pub(crate) fn paragraph(line: Line<'_>) -> Paragraph<'_> {
    Paragraph::new(line).wrap(Wrap { trim: false })
}

/// How many rows `line` takes at `width`: one when it fits, without
/// wrapping it.
pub(crate) fn rows(line: &Line<'_>, width: u16) -> usize {
    if line.width() <= usize::from(width) {
        return 1;
    }
    paragraph(line.clone()).line_count(width).max(1)
}

/// The slab a framed surface or the notice overlay draws now, or an
/// empty rect when `surface` is not open: it draws that frame alone over
/// the area the real frame gives it, into a scratch buffer, and takes the
/// slab the draw returns. The wheel over the slab is the frame's, whatever
/// lies beneath.
pub(crate) fn frame_of(app: &App, surface: Surface) -> Rect {
    let (width, height) = app.screen_size();
    let screen = Rect::new(0, 0, width, height);
    let swapped = app.swapped_view_open();
    let column = app.chrome().layout().map_or(screen, |layout| layout.column);
    let area = match (app.home_screen(), surface) {
        (_, Surface::DeleteQuestion) => column,
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
    if let Surface::Notice = surface {
        let mut buf = Buffer::empty(screen);
        let mut targets = Vec::new();
        return notice_rows::overlay(
            app,
            area,
            &mut buf,
            &mut targets,
            &notice_rows::frame_bound(app),
        );
    }
    let Some(frame) = Frame::from_surface(surface) else {
        return Rect::default();
    };
    let mut buf = Buffer::empty(screen);
    let mut targets = Vec::new();
    let mut no_report = |_: &mut Vec<Target>, _: &mut Buffer| Rect::default();
    draw_frame(app, frame, area, &mut buf, &mut targets, &mut no_report)
}

/// Draws one framed surface. The exhaustive match owns the drawing rule
/// for each frame; the import report stays at its caller's chosen location.
fn draw_frame(
    app: &App,
    frame: Frame,
    area: Rect,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
    report: &mut impl FnMut(&mut Vec<Target>, &mut Buffer) -> Rect,
) -> Rect {
    match frame {
        Frame::Report => report(targets, buf),
        Frame::Quit => quit::draw(app, area, buf, targets),
        Frame::Clear => clear::draw(app, area, buf, targets),
        Frame::HooksOffer => hooks_offer::draw(app, area, buf, targets),
        Frame::DeleteQuestion => {
            // This frame exists only while a command delete question is open.
            let Some((question, scroll)) = app.delete_command_frame() else {
                return Rect::default();
            };
            home::question::draw_command_delete_question(app, &question, scroll, area, buf, targets)
        }
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
    for frame in app.framed_surfaces() {
        let slab = draw_frame(app, frame, area, buf, targets, &mut report);
        app.frames().note(frame.surface(), slab);
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

/// The logo image's 32-by-4 cells, while home's image form draws, else
/// none: the event loop positions the image write here (`docs/tui.md`,
/// "The logo").
pub(crate) fn logo_rect(app: &App, area: Rect) -> Option<Rect> {
    if app.chrome().floor_line().is_some() {
        return None;
    }
    if app.swapped_view_open() && app.on_home() {
        return None;
    }
    let screen = app.home_screen()?;
    home::logo_rect(app, &screen, area)
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
/// Returns the click targets drawn, in draw order, with the one mark
/// scheduling the animated rows the frame painted. Last, the target under
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
    // The animation frame opens here too, on the one resident cache the
    // conversation and notice paints draw through: open paint advances
    // due items, and reading the dues closes the frame again. Faded and
    // dismissed notices leave the stack first, so the prune drops their
    // animation state before any paint can reuse it.
    app.pages()
        .ext
        .cache
        .retain_notices(&app.notices.live_ids());
    app.pages()
        .ext
        .cache
        .begin_anim_frame(app.motion().now_instant(), app.motion().reduced());
    // Every path hands its painted animated keys to this one mark, so a
    // painted item or notice schedules exactly one due per frame.
    let (targets, painted) = render_frame(app, area, buf, pointer);
    app.pages().ext.cache.mark_painted(&painted);
    targets
}

/// Draws `app` into `area` of `buf`, from the bottom up, like [`render`]
/// documents, and returns the click targets in draw order with the
/// animated item and notice keys the frame painted.
fn render_frame(
    app: &App,
    area: Rect,
    buf: &mut Buffer,
    pointer: Option<(u16, u16)>,
) -> (Vec<Target>, Vec<crate::ext::anim::AnimKey>) {
    let mut painted = Vec::new();
    app.clear_input_note();
    // The framed slabs start here too: each framed surface notes the slab
    // it draws, so a click outside the overlay on top closes it.
    app.frames().clear();
    // Below the floor one line shows, home included; home draws while no
    // session is on screen; the conversation draws in its column once one
    // attaches.
    if let Some(line) = app.chrome().floor_line() {
        chrome::floor(line, area, buf);
        return (Vec::new(), painted);
    }
    // A swapped view takes home's place (`docs/tui.md`, "Swapped
    // views"): the `/keys` screen, the configuration view, the model
    // picker or a session view. The notice rows draw over its bottom rows,
    // unreserved, and the overlay over the whole area (`docs/tui.md`,
    // "Notices").
    if app.swapped_view_open() && app.on_home() {
        let mut targets = Vec::new();
        let slab = crate::swapped::draw(app, area, buf, &mut targets);
        note_swapped(app, slab);
        let slab =
            notice_rows::overlay(app, area, buf, &mut targets, &notice_rows::frame_bound(app));
        // debt: five note sites stay inline; extract a shared note helper if a sixth framed surface appears.
        app.frames().note(Surface::Notice, slab);
        let mut rows_bottom = area.bottom();
        painted.extend(notice_rows::draw(
            app,
            area,
            buf,
            &mut rows_bottom,
            &mut targets,
            &notice_rows::frame_bound(app),
        ));
        // The import report and the quit question take every key over
        // these views, so they draw over them too, the quit question
        // last: without it Enter would quit while the choices never show.
        draw_framed(app, area, buf, &mut targets, |targets, buf| {
            crate::import_report::draw_report(app, area, buf, targets)
        });
        paint_tints(
            buf,
            &targets,
            pointer.and_then(|(col, row)| mouse::hit(&targets, col, row)),
            app.focused(),
        );
        return (targets, painted);
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
    // overlay sits above it. The input box draws the rule, the row and
    // the lines, owning the bottom accounting below them.
    let mut box_top = bottom;
    if let Some(panel) = app.panel() {
        bottom = request::draw_noted(app, &panel, area, bottom, buf, &mut targets);
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
    painted.extend(notice_rows::draw(
        app,
        area,
        buf,
        &mut bottom,
        &mut targets,
        &notice_rows::frame_bound(app),
    ));
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
        None if app.swapped_view_open() => {
            let slab = crate::swapped::draw(app, conversation, buf, &mut targets);
            note_swapped(app, slab);
            // The call's view keeps the find bar over its rows: the
            // bar's state opens through Ctrl+F while the view is open
            // and its count names the view's matches. Conversation
            // marks stay off (see `find_marks`): the view's frame
            // paints its own scoped marks, so mapping the hidden rows
            // here would spray wrong cells over the view.
            if app.call_view_open() {
                marks::draw(app, conversation, buf, &mut targets, None);
            }
            let slab = notice_rows::overlay(
                app,
                conversation,
                buf,
                &mut targets,
                &notice_rows::frame_bound(app),
            );
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
                let keys = conversation::draw(app, inset, buf, &mut targets);
                painted.extend(keys);
            } else {
                item::draw_body(app, body, buf);
            }
            let slab = notice_rows::overlay(
                app,
                conversation,
                buf,
                &mut targets,
                &notice_rows::frame_bound(app),
            );
            app.frames().note(Surface::Notice, slab);
        }
        None => {
            // The repository offer swaps in for the conversation.
            match app.offer_rows(conversation.width) {
                Some((rows, top)) => {
                    offer::render(&rows, top, conversation, buf, &mut targets);
                    // The offer reads as its own surface over the
                    // conversation's rows: the body's drawn rows, the
                    // title never copying
                    // (`docs/tui.md`, "Selection and copy").
                    app.note_surface(crate::app::NotedSurface {
                        surface: crate::app::SurfaceId::Swapped,
                        rows: offer::selection_rows(&rows, top, conversation),
                    });
                    marks::paint_selection(app, buf, crate::app::SurfaceId::Swapped, conversation);
                }
                // The search results swap in for the conversation while
                // open (`docs/tui.md`, "Search", "Swapped views").
                None => match app.find_results() {
                    Some(view) => {
                        results::render(&view, conversation, buf, &mut targets);
                        // The results read as their own surface over the
                        // conversation's rows: the path and match rows
                        // each entry draws
                        // (`docs/tui.md`, "Selection and copy").
                        let mut noted = Vec::new();
                        for (row, (path, text)) in
                            results::selection_rows(&view, conversation.width)
                                .into_iter()
                                .enumerate()
                        {
                            let y = conversation
                                .y
                                .saturating_add(3)
                                .saturating_add(to_u16(row.saturating_mul(3)));
                            if y.saturating_add(1) >= conversation.bottom() {
                                break;
                            }
                            for (dy, line) in [(0, path), (1, text)] {
                                noted.push(crate::app::SpanRow {
                                    y: y.saturating_add(dy),
                                    x: conversation.x,
                                    end: conversation.right(),
                                    text: line,
                                    join: crate::rows::Join::Break,
                                });
                            }
                        }
                        app.note_surface(crate::app::NotedSurface {
                            surface: crate::app::SurfaceId::Swapped,
                            rows: noted,
                        });
                        marks::paint_selection(
                            app,
                            buf,
                            crate::app::SurfaceId::Swapped,
                            conversation,
                        );
                        marks::draw(app, conversation, buf, &mut targets, None);
                    }
                    None => {
                        painted.extend(conversation::draw(app, conversation, buf, &mut targets));
                    }
                },
            }
            let slab = notice_rows::overlay(
                app,
                conversation,
                buf,
                &mut targets,
                &notice_rows::frame_bound(app),
            );
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
    paint_tints(
        buf,
        &targets,
        pointer.and_then(|(col, row)| mouse::hit(&targets, col, row)),
        app.focused(),
    );
    (targets, painted)
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
    if app.chrome().floor_line().is_some() || app.swapped_view_open() {
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
        // The approval draws from the column's bottom: no status line
        // draws under it, so the caret shares that bottom.
        return request::caret(&panel, area, area.bottom());
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
    // The box sits on the column's bottom with the kept status lines
    // inside it, so the caret counts up from there.
    let y = area.bottom().checked_sub(below).filter(|y| *y >= area.y)?;
    let x = area.x.saturating_add(col.min(area.width.saturating_sub(1)));
    Some(Position::new(x, y))
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
