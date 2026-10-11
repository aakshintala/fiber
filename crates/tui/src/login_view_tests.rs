//! Tests for `/login`: the rows, the hidden key field and its store
//! steps, and that the key is never drawn (`docs/tui.md`, "Logging in").

use std::path::Path;

use contract::ErrorCode;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use ratatui::style::Modifier;
use ratatui::text::Span;

use super::{Act, Ctx, Login};
use crate::ConfigureError;
use crate::configure::{LoginKind, LoginTarget, Stored};
use crate::configure_fake::Fake;
use crate::keys::{Edit, Key};
use crate::login_worker::LoginTicket;
use crate::mouse::{Target, TargetId};
use crate::swapped::{Spot, render};
use crate::theme::Role;
use crate::view::overlay;

/// A key provider, a browser provider and a secret.
fn targets() -> Vec<LoginTarget> {
    vec![
        LoginTarget {
            name: "acme".to_owned(),
            kind: LoginKind::Key,
        },
        LoginTarget {
            name: "codex".to_owned(),
            kind: LoginKind::Browser,
        },
        LoginTarget {
            name: "acme.api_key".to_owned(),
            kind: LoginKind::Secret,
        },
    ]
}

/// A seam answering `targets`.
fn fake_with(targets: Vec<LoginTarget>) -> Fake {
    let fake = Fake::new(Vec::new());
    if let Ok(mut held) = fake.targets.lock() {
        *held = Ok(targets);
    }
    fake
}

/// A seam whose read fails with `message`.
fn failed_fake(message: &str) -> Fake {
    let fake = Fake::new(Vec::new());
    if let Ok(mut held) = fake.targets.lock() {
        *held = Err(ConfigureError {
            code: ErrorCode::ConfigInvalid,
            message: message.to_owned(),
        });
    }
    fake
}

/// A seam refusing every store with `message`.
fn refusing_fake(message: &str) -> Fake {
    let fake = fake_with(targets());
    if let Ok(mut held) = fake.stored.lock() {
        *held = Err(ConfigureError {
            code: ErrorCode::Usage,
            message: message.to_owned(),
        });
    }
    fake
}

/// A seam answering stores with `stored`.
fn answering_fake(stored: Stored) -> Fake {
    let fake = fake_with(targets());
    if let Ok(mut held) = fake.stored.lock() {
        *held = Ok(stored);
    }
    fake
}

/// A call's context over `fake`, `height` rows tall, in `/w`.
fn ctx_h(fake: &Fake, height: usize) -> Ctx<'_> {
    Ctx {
        seam: fake,
        workspace: Path::new("/w"),
        height,
        width: 80,
        usage: None,
    }
}

/// A call's context over `fake`, 24 rows tall, in `/w`.
fn ctx(fake: &Fake) -> Ctx<'_> {
    ctx_h(fake, 24)
}

/// The view over `targets()` with the item at `index` selected.
fn at(fake: &Fake, index: usize) -> Login {
    let context = ctx(fake);
    let mut login = Login::open(&context);
    login.selected = index;
    login.sync_list(&context);
    login
}

/// Presses each key in turn.
fn press(login: &mut Login, fake: &Fake, keys: &[Key]) -> Vec<Act> {
    keys.iter().map(|key| login.key(key, &ctx(fake))).collect()
}

/// Presses each key in turn with a `height`-row context.
fn press_h(login: &mut Login, fake: &Fake, height: usize, keys: &[Key]) -> Vec<Act> {
    let context = ctx_h(fake, height);
    keys.iter().map(|key| login.key(key, &context)).collect()
}

/// Types `text` into the open panel.
fn typed(login: &mut Login, fake: &Fake, text: &str) {
    let keys: Vec<Key> = text.chars().map(Key::Char).collect();
    press(login, fake, &keys);
}

/// Opens the key panel over `acme`: open focuses the first target, so
/// Enter opens it with no Down. Edited by hand: the two panel helpers
/// are different edits, one dropping a key and one shortening a run
/// that skips the gap (`docs/workflow.md`, "Tools, not handwork").
fn open_key_panel(login: &mut Login, fake: &Fake) {
    press(login, fake, &[Key::Enter]);
}

/// Opens the value panel over the secret: two Downs, skipping the gap
/// and the Secrets heading. Edited by hand, see `open_key_panel`.
fn open_secret_panel(login: &mut Login, fake: &Fake) {
    press(login, fake, &[Key::Down, Key::Down, Key::Enter]);
}

/// `login` drawn at `width` x `height`, one string per row: the list
/// overlay while the list is open, else the legacy frame, as
/// `swapped::draw` routes.
fn drawn(login: &Login, width: u16, height: u16) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    if login.list_open() {
        login.draw(area, &mut buf, &mut Vec::new());
    } else {
        render(
            &login.frame(usize::from(width)),
            area,
            &mut buf,
            &mut Vec::new(),
        );
    }
    text_of(&buf, width, height).join("\n")
}

/// `login`'s list overlay at `width` x `height`: the buffer and the
/// click targets it pushed.
fn overlaid(login: &Login, width: u16, height: u16) -> (Buffer, Vec<Target>) {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    login.draw(area, &mut buf, &mut targets);
    (buf, targets)
}

/// The buffer's rows as text, trailing spaces trimmed.
fn text_of(buf: &Buffer, width: u16, height: u16) -> Vec<String> {
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

/// The rows whose cells ride the accent bar.
fn barred_rows(buf: &Buffer, width: u16, height: u16) -> Vec<u16> {
    let accent = Role::Accent.color();
    (0..height)
        .filter(|y| (0..width).any(|x| buf[(x, *y)].bg == accent))
        .collect()
}

/// No cell rides the accent bar.
fn assert_no_bar(buf: &Buffer, width: u16, height: u16) {
    assert!(barred_rows(buf, width, height).is_empty());
}

/// A blank overlay body row.
fn blank() -> overlay::Row {
    overlay::Row {
        spans: Vec::new(),
        right: Vec::new(),
        targets: Vec::new(),
        barred: false,
    }
}

/// The list overlay's height over a `rows`-row body: the chrome never
/// depends on the body's text, only on its row count.
fn probe_height(rows: usize) -> u16 {
    let framed = overlay::Overlay {
        title: Some(("Log in".to_owned(), Some(Span::raw("✕")))),
        close: Some(TargetId::View(Spot::Close)),
        body: (0..rows).map(|_| blank()).collect(),
        footer: Some(overlay::legend(&[
            ("↑↓", "move"),
            ("Enter", "log in"),
            ("Esc", "close"),
        ])),
        prefer: 96,
    };
    overlay::height(&framed)
}

/// The slab's height in `buf`: the ▄ edge row through the ▀
/// edge row.
fn slab_height(buf: &Buffer, width: u16, height: u16) -> u16 {
    let top = (0..height)
        .find(|y| (0..width).any(|x| buf[(x, *y)].symbol() == "▄"))
        .expect("no top edge");
    let bottom = (0..height)
        .rev()
        .find(|y| (0..width).any(|x| buf[(x, *y)].symbol() == "▀"))
        .expect("no bottom edge");
    bottom - top + 1
}

/// The ▄ edge's run of edge cells in `buf`.
fn edge_run(buf: &Buffer, width: u16, height: u16) -> usize {
    for y in 0..height {
        let run = (0..width).filter(|x| buf[(*x, y)].symbol() == "▄").count();
        if run > 0 {
            return run;
        }
    }
    panic!("no top edge");
}

/// Whether the panel is open.
fn panel_open(login: &Login) -> bool {
    matches!(login.mode, super::Mode::Panel(_))
}

#[test]
fn the_list_marks_key_and_browser() {
    let fake = fake_with(targets());
    let screen = drawn(&Login::open(&ctx(&fake)), 80, 24);
    assert!(screen.contains("Providers"), "{screen}");
    // Each row holds its name and its tag; the key column spans the
    // providers and the secrets, so `acme` no longer sits two columns
    // from `key`.
    for (name, tag) in [("acme", "key"), ("codex", "browser"), ("acme.api_key", "")] {
        assert!(
            screen
                .lines()
                .any(|row| row.contains(name) && (tag.is_empty() || row.contains(tag))),
            "{screen}"
        );
    }
    assert!(!screen.contains("  acme  key"), "{screen}");
    assert!(screen.contains("Secrets"), "{screen}");
    // A heading only for a group that has rows.
    let screen = drawn(
        &Login::open(&ctx(&fake_with(vec![targets()[0].clone()]))),
        80,
        24,
    );
    assert!(screen.contains("Providers"), "{screen}");
    assert!(!screen.contains("Secrets"), "{screen}");
    let screen = drawn(
        &Login::open(&ctx(&fake_with(vec![targets()[2].clone()]))),
        80,
        24,
    );
    assert!(!screen.contains("Providers"), "{screen}");
    assert!(screen.contains("Secrets"), "{screen}");
}

#[test]
fn exactly_one_row_is_barred_on_the_focused_target() {
    let fake = fake_with(targets());
    let (buf, _) = overlaid(&Login::open(&ctx(&fake)), 80, 24);
    let barred = barred_rows(&buf, 80, 24);
    assert_eq!(barred.len(), 1);
    let row: String = (0..80).map(|x| buf[(x, barred[0])].symbol()).collect();
    assert!(row.contains("acme"), "{row}");
}

#[test]
fn the_title_holds_log_in_and_the_close() {
    let fake = fake_with(targets());
    let (buf, targets) = overlaid(&Login::open(&ctx(&fake)), 80, 24);
    let rows = text_of(&buf, 80, 24);
    let title = rows
        .iter()
        .find(|row| row.contains("Log in"))
        .expect("no title");
    assert!(title.contains("✕"), "{title}");
    let close = targets
        .iter()
        .find(|target| target.id == TargetId::View(Spot::Close))
        .expect("no close target");
    assert_eq!(buf[(close.rect.x, close.rect.y)].symbol(), "✕");
}

#[test]
fn the_headings_are_dim_with_a_blank_row_between_the_groups() {
    let fake = fake_with(targets());
    let (buf, _) = overlaid(&Login::open(&ctx(&fake)), 80, 24);
    let rows = text_of(&buf, 80, 24);
    let providers = rows
        .iter()
        .position(|row| row.contains("Providers"))
        .expect("no heading");
    let x = rows[providers].find("Providers").expect("no heading");
    assert!(
        buf[(
            u16::try_from(x).unwrap_or(u16::MAX),
            u16::try_from(providers).unwrap_or(u16::MAX)
        )]
            .modifier
            .contains(Modifier::DIM)
    );
    let codex = rows
        .iter()
        .position(|row| row.contains("codex"))
        .expect("no codex");
    let secrets = rows
        .iter()
        .position(|row| row.contains("Secrets"))
        .expect("no heading");
    assert_eq!(secrets, codex + 2);
    assert!(rows[codex + 1].trim().is_empty());
}

#[test]
fn with_no_targets_the_note_pads_to_three_rows_with_no_bar() {
    let fake = fake_with(Vec::new());
    let (buf, _) = overlaid(&Login::open(&ctx(&fake)), 80, 24);
    let rows = text_of(&buf, 80, 24);
    assert!(
        rows.iter()
            .any(|row| row.contains("No provider is installed")),
        "{}",
        rows.join("\n")
    );
    assert_eq!(slab_height(&buf, 80, 24), probe_height(3));
    assert_no_bar(&buf, 80, 24);
}

#[test]
fn a_failed_read_pads_to_three_rows_with_no_bar() {
    let fake = failed_fake("config.json: not JSON");
    let (buf, _) = overlaid(&Login::open(&ctx(&fake)), 80, 24);
    let rows = text_of(&buf, 80, 24);
    assert!(
        rows.iter().any(|row| row.contains("config.json: not JSON")),
        "{}",
        rows.join("\n")
    );
    assert_eq!(slab_height(&buf, 80, 24), probe_height(3));
    assert_no_bar(&buf, 80, 24);
}

#[test]
fn one_browser_provider_pads_the_body_to_three_rows() {
    let fake = fake_with(vec![LoginTarget {
        name: "codex".to_owned(),
        kind: LoginKind::Browser,
    }]);
    let (buf, _) = overlaid(&Login::open(&ctx(&fake)), 80, 24);
    assert_eq!(slab_height(&buf, 80, 24), probe_height(3));
    assert_eq!(barred_rows(&buf, 80, 24).len(), 1);
}

#[test]
fn after_a_store_the_window_clips_the_said_from_below() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    open_key_panel(&mut login, &fake);
    typed(&mut login, &fake, "sk-a");
    assert!(matches!(login.key(&Key::Enter, &ctx(&fake)), Act::Stay));
    // Nine rows tall the window holds only the focused target.
    let narrow = drawn(&login, 80, 9);
    assert!(
        !narrow.contains("Stored credentials/acme/default."),
        "{narrow}"
    );
    assert!(narrow.lines().any(|row| row.contains("acme")), "{narrow}");
    let wide = drawn(&login, 80, 24);
    assert!(wide.contains("Stored credentials/acme/default."), "{wide}");
}

#[test]
fn a_long_refusal_wraps_and_is_never_cut_sideways() {
    let message = "w".repeat(100);
    let fake = fake_with(targets());
    let mut login = waiting(&fake, LoginTicket(1));
    login.step(
        LoginTicket(1),
        crate::login_worker::LoginStep::Done(Err(ConfigureError {
            code: contract::ErrorCode::Usage,
            message: message.clone(),
        })),
    );
    let screen = drawn(&login, 80, 24);
    let wrapped: Vec<String> = screen
        .lines()
        .filter(|row| !row.trim().is_empty() && row.trim().chars().all(|ch| ch == 'w'))
        .map(|row| row.trim().to_owned())
        .collect();
    assert_eq!(wrapped.len(), 2, "{screen}");
    assert_eq!(wrapped.join(""), message);
    for row in &wrapped {
        assert!(crate::format::width(row) <= 76, "{row:?}");
    }
}

#[test]
fn a_very_long_browser_name_draws_one_barred_row() {
    let fake = fake_with(vec![LoginTarget {
        name: "b".repeat(65_530),
        kind: LoginKind::Browser,
    }]);
    let (buf, _) = overlaid(&Login::open(&ctx(&fake)), 80, 24);
    assert_eq!(barred_rows(&buf, 80, 24).len(), 1);
    assert_eq!(slab_height(&buf, 80, 24), probe_height(3));
}

#[test]
fn names_cut_at_the_budget_keep_the_tag_on_screen() {
    // At 80 columns with a browser target the name budget is 65: the
    // inner 76 less the gutter, the spacing and `browser`.
    for (len, whole) in [(65, true), (66, false)] {
        let name = "n".repeat(len);
        let fake = fake_with(vec![LoginTarget {
            name: name.clone(),
            kind: LoginKind::Browser,
        }]);
        let screen = drawn(&Login::open(&ctx(&fake)), 80, 24);
        let row = screen
            .lines()
            .find(|row| row.contains("browser"))
            .unwrap_or_else(|| panic!("no tag in {screen}"))
            .to_owned();
        if whole {
            assert!(row.contains(&name), "{row}");
        } else {
            assert!(!row.contains(&name), "{row}");
            assert!(row.contains(&"n".repeat(65)), "{row}");
        }
    }
}

#[test]
fn at_one_row_only_the_title_draws_with_no_bar() {
    for fake in [
        fake_with(targets()),
        fake_with(Vec::new()),
        failed_fake("config.json: not JSON"),
    ] {
        let (buf, _) = overlaid(&Login::open(&ctx(&fake)), 80, 1);
        let row: String = (0..80).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(row.contains("Log in"), "{row}");
        assert_no_bar(&buf, 80, 1);
    }
}

#[test]
fn at_eight_rows_no_choice_draws_and_at_nine_only_the_focus_does() {
    // The chrome is eight rows: the saturation count draws no body
    // row, one row taller draws exactly the focused target, barred.
    let fake = fake_with(targets());
    let login = Login::open(&ctx(&fake));
    let short = drawn(&login, 80, 8);
    assert!(!short.contains("acme"), "{short}");
    let (buf, _) = overlaid(&login, 80, 9);
    let barred = barred_rows(&buf, 80, 9);
    assert_eq!(barred.len(), 1);
    let row: String = (0..80).map(|x| buf[(x, barred[0])].symbol()).collect();
    assert!(row.contains("acme"), "{row}");
}

#[test]
fn a_resize_keeps_the_focused_row_drawn() {
    let fake = fake_with(targets());
    let login = at(&fake, 5);
    let (buf, _) = overlaid(&login, 80, 10);
    let barred = barred_rows(&buf, 80, 10);
    assert_eq!(barred.len(), 1);
    let row: String = (0..80).map(|x| buf[(x, barred[0])].symbol()).collect();
    assert!(row.contains("acme.api_key"), "{row}");
}

#[test]
fn page_down_lands_on_a_drawn_target() {
    let fake = fake_with(targets());
    let mut login = at(&fake, 1);
    press_h(&mut login, &fake, 10, &[Key::PageDown]);
    assert_eq!(login.selected, 2);
    let (buf, _) = overlaid(&login, 80, 10);
    let barred = barred_rows(&buf, 80, 10);
    assert_eq!(barred.len(), 1);
    let row: String = (0..80).map(|x| buf[(x, barred[0])].symbol()).collect();
    assert!(row.contains("codex"), "{row}");
}

#[test]
fn scrolled_click_targets_name_the_rows_under_them() {
    let fake = fake_with(targets());
    let login = at(&fake, 5);
    // Tall enough every target pushes a target; scrolled to the last
    // secret only its target is pushed, on the row that draws it.
    for (height, pushed) in [(24, 3), (10, 1)] {
        let (buf, targets) = overlaid(&login, 80, height);
        let mut rows = 0;
        for target in &targets {
            let TargetId::View(Spot::Row(at)) = target.id else {
                continue;
            };
            let super::Item::Target(t) = login.items[at] else {
                continue;
            };
            let name = login.targets[t].name.clone();
            let row: String = (0..80).map(|x| buf[(x, target.rect.y)].symbol()).collect();
            assert!(row.contains(&name), "row {at} misses {name}: {row}");
            rows += 1;
        }
        assert_eq!(rows, pushed, "height {height}");
    }
}

#[test]
fn the_footer_padding_holds_the_width_while_scrolled() {
    // A 60-column secret name last: at 80x9 the window holds only
    // `acme`, but the ▄ run equals the 80x24 one.
    let mut made = vec![targets()[0].clone(), targets()[1].clone()];
    made.push(LoginTarget {
        name: "s".repeat(60),
        kind: LoginKind::Secret,
    });
    let fake = fake_with(made);
    let login = Login::open(&ctx(&fake));
    let (narrow, _) = overlaid(&login, 80, 9);
    let (wide, _) = overlaid(&login, 80, 24);
    assert_eq!(edge_run(&narrow, 80, 9), edge_run(&wide, 80, 24));
}

#[test]
fn no_targets_says_so() {
    let fake = fake_with(Vec::new());
    let screen = drawn(&Login::open(&ctx(&fake)), 80, 24);
    assert!(
        screen.contains("No provider is installed, and no installed extension declares a secret."),
        "{screen}"
    );
    assert!(!screen.contains("Providers"), "{screen}");
    assert!(!screen.contains("Secrets"), "{screen}");
}

#[test]
fn a_failed_read_says_why_and_enter_does_nothing() {
    let fake = failed_fake("config.json: not JSON");
    let mut login = Login::open(&ctx(&fake));
    assert!(login.items.is_empty());
    assert!(drawn(&login, 80, 24).contains("config.json: not JSON"));
    assert!(matches!(login.key(&Key::Enter, &ctx(&fake)), Act::Stay));
    assert!(!panel_open(&login));
    assert!(fake.stores().is_empty());
}

#[test]
fn open_focuses_the_first_target() {
    let fake = fake_with(targets());
    let login = Login::open(&ctx(&fake));
    assert_eq!(login.selected, 1);
}

#[test]
fn enter_right_after_open_opens_key_entry() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    assert!(matches!(login.key(&Key::Enter, &ctx(&fake)), Act::Stay));
    assert!(panel_open(&login));
}

#[test]
fn down_and_up_step_over_targets_only() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    press(&mut login, &fake, &[Key::Down]);
    assert_eq!(login.selected, 2);
    // Down from codex skips the gap and the Secrets heading.
    press(&mut login, &fake, &[Key::Down]);
    assert_eq!(login.selected, 5);
    // Down on the last target stays.
    press(&mut login, &fake, &[Key::Down]);
    assert_eq!(login.selected, 5);
    // Up from the secret skips back past the gap.
    press(&mut login, &fake, &[Key::Up]);
    assert_eq!(login.selected, 2);
    press(&mut login, &fake, &[Key::Up]);
    assert_eq!(login.selected, 1);
    // Up on the first target stays.
    press(&mut login, &fake, &[Key::Up]);
    assert_eq!(login.selected, 1);
}

#[test]
fn page_up_and_page_down_land_on_targets() {
    let fake = fake_with(targets());
    // (height, from, key, to): the page is the shown body rows less
    // one, at least one, so height 10 pages by 1, 11 by 2, 24 by 15
    // and 9 by 1 at the floor.
    for (height, from, key, to) in [
        (10, 1, Key::PageDown, 2),
        (10, 2, Key::PageDown, 5),
        (10, 5, Key::PageDown, 5),
        (10, 5, Key::PageUp, 2),
        (10, 2, Key::PageUp, 1),
        (10, 1, Key::PageUp, 1),
        (11, 1, Key::PageDown, 5),
        (11, 5, Key::PageUp, 2),
        (12, 5, Key::PageUp, 2),
        (24, 1, Key::PageDown, 5),
        (24, 5, Key::PageUp, 1),
        (9, 1, Key::PageDown, 2),
    ] {
        let mut login = at(&fake, from);
        press_h(&mut login, &fake, height, &[key]);
        assert_eq!(login.selected, to, "height {height} from {from}");
    }
}

#[test]
fn paging_with_one_target_stays() {
    let fake = fake_with(vec![targets()[0].clone()]);
    let mut login = Login::open(&ctx(&fake));
    assert_eq!(login.selected, 1);
    press(
        &mut login,
        &fake,
        &[Key::PageDown, Key::PageUp, Key::Down, Key::Up],
    );
    assert_eq!(login.selected, 1);
}

#[test]
fn keys_with_no_targets_do_nothing() {
    let fake = fake_with(Vec::new());
    let mut login = Login::open(&ctx(&fake));
    for key in [Key::Up, Key::Down, Key::PageUp, Key::PageDown, Key::Enter] {
        assert!(matches!(login.key(&key, &ctx(&fake)), Act::Stay));
    }
    assert_eq!(login.selected, 0);
    assert!(!panel_open(&login));
    assert!(fake.stores().is_empty());
}

#[test]
fn keys_on_a_failed_read_do_nothing() {
    let fake = failed_fake("config.json: not JSON");
    let mut login = Login::open(&ctx(&fake));
    for key in [Key::Up, Key::Down, Key::PageUp, Key::PageDown, Key::Enter] {
        assert!(matches!(login.key(&key, &ctx(&fake)), Act::Stay));
    }
    assert_eq!(login.selected, 0);
    assert!(!panel_open(&login));
    assert!(fake.stores().is_empty());
}

#[test]
fn the_window_maths_saturate() {
    // The list overlay's chrome is eight rows: shorter than it no body
    // row shows, at nine rows tall exactly one does.
    assert_eq!(super::shown_body_rows(7), 0);
    assert_eq!(super::shown_body_rows(8), 0);
    assert_eq!(super::shown_body_rows(9), 1);
    // An empty body or window starts at the top without underflowing.
    assert_eq!(super::window_start(0, 0, 0), 0);
    assert_eq!(super::window_start(0, 0, 1), 0);
    // PageUp and PageDown with no row shown do not panic and stay put.
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    press_h(&mut login, &fake, 1, &[Key::PageDown, Key::PageUp]);
    assert_eq!(login.selected, 1);
}

#[test]
fn with_no_row_shown_down_and_up_leave_both_selections() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    let legacy = login.list.selected();
    // Each key on its own: a Down followed by an Up would hide a
    // broken freeze by cancelling out.
    press_h(&mut login, &fake, 1, &[Key::Down]);
    assert_eq!(login.selected, 1);
    assert_eq!(login.list.selected(), legacy);
    press_h(&mut login, &fake, 1, &[Key::Up]);
    assert_eq!(login.selected, 1);
    assert_eq!(login.list.selected(), legacy);
}

#[test]
fn the_step_viewport_follows_the_selection() {
    // Six key providers and two secrets: ten frame rows, the gap left
    // out, the last secret at frame row 9. Eight rows tall six rows
    // show, so the last secret scrolls the viewport to today's top.
    let mut made = Vec::new();
    for n in 0..6 {
        made.push(LoginTarget {
            name: format!("p{n}"),
            kind: LoginKind::Key,
        });
    }
    for name in ["s0.api_key", "s1.api_key"] {
        made.push(LoginTarget {
            name: name.to_owned(),
            kind: LoginKind::Secret,
        });
    }
    let fake = fake_with(made);
    let context = ctx_h(&fake, 8);
    let mut login = Login::open(&context);
    assert_eq!(login.selected, 1);
    for _ in 0..7 {
        login.key(&Key::Down, &context);
    }
    assert_eq!(login.selected, 10);
    assert_eq!(login.list.selected(), 9);
    assert_eq!(login.list.top(), 4);
    // Its secret panel opens over the same frame rows as today.
    assert!(matches!(login.key(&Key::Enter, &context), Act::Stay));
    assert!(panel_open(&login));
    let screen = drawn(&login, 80, 8);
    assert!(screen.contains("Value for s1.api_key:"), "{screen}");
}

#[test]
fn enter_on_a_heading_or_the_note_does_nothing() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    // The headings and the gap never take focus: Enter on each stays.
    for selected in [0, 3, 4] {
        login.selected = selected;
        assert!(matches!(login.key(&Key::Enter, &ctx(&fake)), Act::Stay));
        assert!(!panel_open(&login));
    }
    assert!(fake.stores().is_empty());
    let fake = fake_with(Vec::new());
    let mut login = Login::open(&ctx(&fake));
    assert!(matches!(login.key(&Key::Enter, &ctx(&fake)), Act::Stay));
    assert!(!panel_open(&login));
}

#[test]
fn enter_on_a_browser_row_asks_for_a_browser_login_and_writes_nothing() {
    let fake = fake_with(targets());
    let mut login = at(&fake, 2);
    assert!(matches!(
        login.key(&Key::Enter, &ctx(&fake)),
        Act::Login(name) if name == "codex"
    ));
    assert!(!panel_open(&login));
    assert!(fake.stores().is_empty());
}

#[test]
fn the_key_is_dots_one_per_character() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    open_key_panel(&mut login, &fake);
    typed(&mut login, &fake, "sk-é9");
    // Five characters, one of them two bytes: five dots.
    let field = login.frame(80).field;
    assert_eq!(field, Some(("•••••".to_owned(), 5)));
    assert_eq!(
        login.frame(80).below,
        [
            "Label (--as): default.".to_owned(),
            "Key for acme:".to_owned(),
        ]
    );
    assert!(drawn(&login, 80, 24).contains("> •••••"));
}

#[test]
fn the_key_never_appears_in_a_frame() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    open_key_panel(&mut login, &fake);
    typed(&mut login, &fake, "sk-SECRET-1");
    for screen in [drawn(&login, 80, 24), {
        press(&mut login, &fake, &[Key::Tab]);
        typed(&mut login, &fake, "work");
        drawn(&login, 80, 24)
    }] {
        for row in screen.lines() {
            assert!(!row.contains("SECRET"), "{row}");
            assert!(!row.contains("sk-"), "{row}");
        }
    }
}

#[test]
fn the_key_never_appears_in_debug() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    open_key_panel(&mut login, &fake);
    typed(&mut login, &fake, "sk-SECRET-1");
    for debug in [
        format!("{:?}", login),
        format!("{:?}", login.frame(80)),
        {
            press(&mut login, &fake, &[Key::Tab]);
            typed(&mut login, &fake, "work");
            format!("{:?}", login)
        },
        format!("{:?}", login.frame(80)),
    ] {
        assert!(!debug.contains("SECRET"), "{debug}");
    }
    assert!(format!("{:?}", login).contains("SecretField(11 chars)"));
}

#[test]
fn a_typed_key_reaches_the_seam_as_typed() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    open_key_panel(&mut login, &fake);
    typed(&mut login, &fake, "sk-é9x");
    press(&mut login, &fake, &[Key::Backspace]);
    typed(&mut login, &fake, "Z");
    assert!(matches!(login.key(&Key::Enter, &ctx(&fake)), Act::Stay));
    // Backspace deletes a character, not a byte; nothing is trimmed but the
    // ends.
    assert_eq!(fake.secret_of(0), "sk-é9Z");
    assert_eq!(fake.stores(), [("acme".to_owned(), None)]);
}

#[test]
fn a_pasted_key_is_one_secret_with_no_token() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    open_key_panel(&mut login, &fake);
    // Twelve lines, two past the ten where a draft would tokenize: the key
    // field takes the whole text, with no token.
    let pasted: String = (1..=12)
        .map(|n| format!("a{n}"))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    login.edit_key(&Edit::Paste(pasted.clone()));
    let screen = drawn(&login, 80, 24);
    assert!(!screen.contains("[Pasted"), "{screen}");
    assert!(!screen.contains("a1"), "{screen}");
    let field = screen
        .lines()
        .find(|row| row.starts_with("> "))
        .unwrap_or_else(|| panic!("no field in {screen}"))
        .to_owned();
    assert!(field.chars().skip(2).all(|ch| ch == '•'), "{field}");
    assert!(matches!(login.key(&Key::Enter, &ctx(&fake)), Act::Stay));
    assert_eq!(fake.secret_of(0), pasted.trim());
}

#[test]
fn typing_and_backspace_edit_the_focused_field() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    open_key_panel(&mut login, &fake);
    typed(&mut login, &fake, "abc");
    press(&mut login, &fake, &[Key::Backspace]);
    assert_eq!(login.frame(80).field, Some(("••".to_owned(), 2)));
    press(&mut login, &fake, &[Key::Tab]);
    typed(&mut login, &fake, "xy");
    press(&mut login, &fake, &[Key::Backspace]);
    assert_eq!(login.frame(80).field, Some(("x".to_owned(), 1)));
    // The key's dots are untouched by the label's edits.
    assert!(
        drawn(&login, 80, 24).contains("Key for acme: ••"),
        "{}",
        drawn(&login, 80, 24)
    );
}

#[test]
fn delete_word_clears_the_key() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    open_key_panel(&mut login, &fake);
    typed(&mut login, &fake, "abc");
    login.edit_key(&Edit::DeleteWord);
    assert_eq!(login.frame(80).field, Some((String::new(), 0)));
}

#[test]
fn other_edits_leave_the_key() {
    for edit in [
        Edit::Left,
        Edit::Right,
        Edit::ShiftEnter,
        Edit::CtrlJ,
        Edit::WordLeft,
        Edit::WordRight,
        Edit::LineStart,
        Edit::LineEnd,
        Edit::Delete,
    ] {
        let fake = fake_with(targets());
        let mut login = Login::open(&ctx(&fake));
        open_key_panel(&mut login, &fake);
        typed(&mut login, &fake, "abc");
        login.edit_key(&edit);
        assert_eq!(
            login.frame(80).field,
            Some(("•••".to_owned(), 3)),
            "{edit:?}"
        );
        assert!(matches!(login.key(&Key::Enter, &ctx(&fake)), Act::Stay));
        assert_eq!(fake.secret_of(0), "abc", "{edit:?}");
    }
}

#[test]
fn ctrl_v_does_nothing_in_the_panel() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    open_key_panel(&mut login, &fake);
    typed(&mut login, &fake, "ab");
    assert!(matches!(login.key(&Key::CtrlV, &ctx(&fake)), Act::Stay));
    assert_eq!(login.frame(80).field, Some(("••".to_owned(), 2)));
    assert!(fake.stores().is_empty());
}

#[test]
fn tab_moves_to_the_label_and_backtab_back() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    open_key_panel(&mut login, &fake);
    typed(&mut login, &fake, "ab");
    press(&mut login, &fake, &[Key::Tab]);
    assert_eq!(login.frame(80).field, Some((String::new(), 0)));
    typed(&mut login, &fake, "w");
    assert_eq!(login.frame(80).field, Some(("w".to_owned(), 1)));
    press(&mut login, &fake, &[Key::BackTab]);
    assert_eq!(login.frame(80).field, Some(("••".to_owned(), 2)));
}

#[test]
fn a_secret_has_no_label_and_tab_stays_on_the_value() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    open_secret_panel(&mut login, &fake);
    assert!(matches!(login.key(&Key::Tab, &ctx(&fake)), Act::Stay));
    typed(&mut login, &fake, "v1");
    assert_eq!(login.frame(80).field, Some(("••".to_owned(), 2)));
    assert!(matches!(login.key(&Key::Enter, &ctx(&fake)), Act::Stay));
    assert_eq!(fake.stores(), [("acme.api_key".to_owned(), None)]);
    assert_eq!(fake.secret_of(0), "v1");
}

#[test]
fn the_label_is_passed_trimmed_and_empty_is_none() {
    for (label, stored) in [
        ("", None),
        ("  ", None),
        (" work ", Some("work".to_owned())),
    ] {
        let fake = fake_with(targets());
        let mut login = Login::open(&ctx(&fake));
        open_key_panel(&mut login, &fake);
        typed(&mut login, &fake, "sk-a");
        press(&mut login, &fake, &[Key::Tab]);
        typed(&mut login, &fake, label);
        assert!(matches!(login.key(&Key::Enter, &ctx(&fake)), Act::Stay));
        assert_eq!(fake.stores(), [("acme".to_owned(), stored)], "{label:?}");
    }
}

#[test]
fn success_says_the_stored_path_and_closes_the_panel() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    open_key_panel(&mut login, &fake);
    typed(&mut login, &fake, "sk-a");
    assert!(matches!(login.key(&Key::Enter, &ctx(&fake)), Act::Stay));
    assert!(!panel_open(&login));
    assert_eq!(login.frame(80).field, None);
    assert_eq!(login.frame(80).below, ["Stored credentials/acme/default."]);
}

#[test]
fn a_replaced_secret_says_replaced() {
    let fake = answering_fake(Stored {
        path: "credentials/acme.api_key".to_owned(),
        replaced: true,
    });
    let mut login = Login::open(&ctx(&fake));
    open_secret_panel(&mut login, &fake);
    typed(&mut login, &fake, "v2");
    assert!(matches!(login.key(&Key::Enter, &ctx(&fake)), Act::Stay));
    assert!(!panel_open(&login));
    assert_eq!(
        login.frame(80).below,
        ["Replaced credentials/acme.api_key."]
    );
}

#[test]
fn a_refusal_clears_the_key_and_keeps_the_label() {
    let fake = refusing_fake("credentials/acme/default is already stored");
    let mut login = Login::open(&ctx(&fake));
    open_key_panel(&mut login, &fake);
    typed(&mut login, &fake, "sk-x");
    press(&mut login, &fake, &[Key::Tab]);
    typed(&mut login, &fake, "work");
    assert!(matches!(login.key(&Key::Enter, &ctx(&fake)), Act::Stay));
    assert!(panel_open(&login));
    assert_eq!(
        login.frame(80).below,
        [
            "credentials/acme/default is already stored".to_owned(),
            "Label (--as): work.".to_owned(),
            "Key for acme:".to_owned(),
        ]
    );
    assert_eq!(login.frame(80).field, Some((String::new(), 0)));
    // The refusal stays drawn with the panel open: the key field is empty
    // and focused, the label is still there, and the key never appears.
    let screen = drawn(&login, 80, 24);
    assert!(
        screen.contains("credentials/acme/default is already stored"),
        "{screen}"
    );
    assert!(!screen.contains("sk-x"), "{screen}");
    // The key field is empty and focused: typing dots again, and the label
    // is still there.
    typed(&mut login, &fake, "z");
    assert_eq!(login.frame(80).field, Some(("•".to_owned(), 1)));
    assert!(
        drawn(&login, 80, 24).contains("credentials/acme/default is already stored"),
        "{}",
        drawn(&login, 80, 24)
    );
    press(&mut login, &fake, &[Key::Tab]);
    assert_eq!(login.frame(80).field, Some(("work".to_owned(), 4)));
    assert!(
        login
            .frame(80)
            .below
            .contains(&"credentials/acme/default is already stored".to_owned())
    );
    assert_eq!(
        fake.stores(),
        [("acme".to_owned(), Some("work".to_owned()))]
    );
}

#[test]
fn a_secret_refusal_shows_the_message_and_clears_the_value() {
    let fake = refusing_fake("No value was given; nothing was stored.");
    let mut login = Login::open(&ctx(&fake));
    open_secret_panel(&mut login, &fake);
    assert!(matches!(login.key(&Key::Enter, &ctx(&fake)), Act::Stay));
    assert!(panel_open(&login));
    assert_eq!(
        login.frame(80).below,
        [
            "No value was given; nothing was stored.".to_owned(),
            "Value for acme.api_key:".to_owned(),
        ]
    );
    assert_eq!(login.frame(80).field, Some((String::new(), 0)));
    let screen = drawn(&login, 80, 24);
    assert!(
        screen.contains("No value was given; nothing was stored."),
        "{screen}"
    );
}

#[test]
fn esc_in_the_panel_stores_nothing_and_returns_to_the_list() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    open_key_panel(&mut login, &fake);
    typed(&mut login, &fake, "sk-a");
    assert!(matches!(login.key(&Key::Esc, &ctx(&fake)), Act::Stay));
    assert!(!panel_open(&login));
    assert!(login.list_open());
    assert_eq!(login.frame(80).field, None);
    assert!(fake.stores().is_empty());
}

#[test]
fn esc_on_the_list_closes() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    assert!(login.list_open());
    assert!(matches!(login.key(&Key::Esc, &ctx(&fake)), Act::Close));
}

#[test]
fn a_click_selects_a_row_and_the_x_closes() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    assert!(matches!(login.click(Spot::Row(5), &ctx(&fake)), Act::Stay));
    assert_eq!(login.selected, 5);
    // A click on the Secrets heading, the gap or past the last item
    // leaves the selection.
    for spot in [Spot::Row(4), Spot::Row(3), Spot::Row(99)] {
        assert!(matches!(login.click(spot, &ctx(&fake)), Act::Stay));
        assert_eq!(login.selected, 5);
    }
    assert!(matches!(login.click(Spot::Close, &ctx(&fake)), Act::Close));
}

#[test]
fn a_click_on_a_row_while_the_panel_is_open_keeps_the_panel() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    open_key_panel(&mut login, &fake);
    assert!(matches!(login.click(Spot::Row(4), &ctx(&fake)), Act::Stay));
    assert!(panel_open(&login));
    assert!(login.frame(80).field.is_some());
}

#[test]
fn a_long_key_draws_only_dots() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    open_key_panel(&mut login, &fake);
    // Twenty-nine characters, two past the field's room of 27 at width 30:
    // the row scrolls to the caret and shows dots only.
    typed(&mut login, &fake, &"x".repeat(29));
    let screen = drawn(&login, 30, 24);
    let field = screen
        .lines()
        .find(|row| row.starts_with("> "))
        .unwrap_or_else(|| panic!("no field in {screen}"))
        .to_owned();
    assert_eq!(field, format!("> {}", "•".repeat(27)));
}

#[test]
fn login_80x24() {
    let fake = fake_with(targets());
    let login = Login::open(&ctx(&fake));
    insta::assert_snapshot!("login_80x24", drawn(&login, 80, 24));
}

#[test]
fn login_key_panel_80x24() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    open_key_panel(&mut login, &fake);
    typed(&mut login, &fake, "sk-abc");
    press(&mut login, &fake, &[Key::Tab]);
    typed(&mut login, &fake, "work");
    insta::assert_snapshot!("login_key_panel_80x24", drawn(&login, 80, 24));
}

/// A waiting view over `codex`, on `ticket`.
fn waiting(fake: &Fake, ticket: LoginTicket) -> Login {
    let mut login = at(fake, 2);
    assert!(matches!(
        login.key(&Key::Enter, &ctx(fake)),
        Act::Login(name) if name == "codex"
    ));
    login.wait(ticket);
    login
}

#[test]
fn enter_on_a_browser_row_waits_and_says_starting() {
    let fake = fake_with(targets());
    let login = waiting(&fake, LoginTicket(1));
    let frame = login.frame(80);
    assert_eq!(frame.field, None);
    assert_eq!(frame.below, ["Starting the login to codex…"]);
    assert_eq!(frame.footer, "Esc cancel");
    let screen = drawn(&login, 80, 24);
    assert!(screen.contains("Starting the login to codex…"), "{screen}");
    assert!(screen.contains("Esc cancel"), "{screen}");
}

#[test]
fn open_on_the_waiting_ticket_shows_the_url_and_returns_it() {
    let fake = fake_with(targets());
    let mut login = waiting(&fake, LoginTicket(1));
    let url = "https://auth.example/authorize?state=1";
    assert_eq!(
        login.step(
            LoginTicket(1),
            crate::login_worker::LoginStep::Open(url.to_owned())
        ),
        Some(url.to_owned())
    );
    let frame = login.frame(80);
    assert_eq!(
        frame.below,
        [
            "Open this URL to log in to codex:".to_owned(),
            url.to_owned()
        ]
    );
    assert_eq!(frame.footer, "y copy URL · Esc cancel");
}

#[test]
fn steps_for_other_tickets_change_nothing_and_open_nothing() {
    let fake = fake_with(targets());
    let mut login = waiting(&fake, LoginTicket(5));
    let url = "https://auth.example/authorize?state=1";
    assert_eq!(
        login.step(
            LoginTicket(4),
            crate::login_worker::LoginStep::Open(url.to_owned())
        ),
        None
    );
    assert_eq!(
        login.step(
            LoginTicket(6),
            crate::login_worker::LoginStep::Open(url.to_owned())
        ),
        None
    );
    assert_eq!(
        login.step(
            LoginTicket(4),
            crate::login_worker::LoginStep::Code {
                url: url.to_owned(),
                code: "ABCD-1234".to_owned(),
            }
        ),
        None
    );
    let frame = login.frame(80);
    assert_eq!(frame.below, ["Starting the login to codex…"]);
    assert_eq!(frame.footer, "Esc cancel");
}

#[test]
fn a_code_step_shows_the_code_and_opens_nothing() {
    let fake = fake_with(targets());
    let mut login = waiting(&fake, LoginTicket(1));
    let url = "https://auth.example/device";
    assert_eq!(
        login.step(
            LoginTicket(1),
            crate::login_worker::LoginStep::Code {
                url: url.to_owned(),
                code: "ABCD-1234".to_owned(),
            }
        ),
        None
    );
    let frame = login.frame(80);
    assert_eq!(
        frame.below,
        [
            "Enter the code ABCD-1234 at this URL to log in to codex:".to_owned(),
            url.to_owned()
        ]
    );
    assert_eq!(frame.footer, "y copy URL · Esc cancel");
}

#[test]
fn y_copies_the_shown_url_whole_and_nothing_before_it_arrives() {
    let fake = fake_with(targets());
    let mut login = waiting(&fake, LoginTicket(1));
    assert!(matches!(login.key(&Key::Char('y'), &ctx(&fake)), Act::Stay));
    let url = "https://auth.example/authorize?state=1";
    login.step(
        LoginTicket(1),
        crate::login_worker::LoginStep::Open(url.to_owned()),
    );
    assert!(matches!(
        login.key(&Key::Char('y'), &ctx(&fake)),
        Act::Copy(shown) if shown == url
    ));
}

#[test]
fn the_end_returns_to_the_rows_saying_what_it_stored() {
    let fake = fake_with(targets());
    let mut login = waiting(&fake, LoginTicket(1));
    login.step(
        LoginTicket(1),
        crate::login_worker::LoginStep::Open("https://auth.example/authorize".to_owned()),
    );
    login.step(
        LoginTicket(1),
        crate::login_worker::LoginStep::Done(Ok(Stored {
            path: "credentials/codex/alice@example.com".to_owned(),
            replaced: false,
        })),
    );
    assert_eq!(
        login.frame(80).below,
        ["Stored credentials/codex/alice@example.com."]
    );
    let screen = drawn(&login, 80, 24);
    assert!(
        screen.contains("Stored credentials/codex/alice@example.com."),
        "{screen}"
    );
}

#[test]
fn a_failed_end_returns_to_the_rows_saying_why() {
    let fake = fake_with(targets());
    let mut login = waiting(&fake, LoginTicket(1));
    login.step(LoginTicket(1),
        crate::login_worker::LoginStep::Done(Err(ConfigureError {
            code: ErrorCode::Usage,
            message: "credentials/codex/alice@example.com is already stored; log in under another label with --as <label>, or run `fiber logout codex --as alice@example.com` first.".to_owned(),
        })),
    );
    assert!(
        login.frame(80).below[0].contains("--as"),
        "{:?}",
        login.frame(80).below
    );
}

#[test]
fn esc_while_waiting_returns_to_the_rows_and_cancels_once() {
    let fake = fake_with(targets());
    let mut login = waiting(&fake, LoginTicket(1));
    let (fake_login, _answer) = crate::configure_fake::FakeLogin::new();
    let held = std::sync::Arc::new(fake_login);
    login.started(crate::login_worker::LoginWorker::new(
        LoginTicket(1),
        held.clone() as std::sync::Arc<dyn crate::configure::BrowserLogin>,
    ));
    assert!(matches!(login.key(&Key::Esc, &ctx(&fake)), Act::Stay));
    assert!(login.list_open());
    assert_eq!(login.frame(80).below, Vec::<String>::new());
    assert_eq!(held.cancels(), 1);
}

#[test]
fn dropping_a_waiting_view_cancels_once() {
    let fake = fake_with(targets());
    let (fake_login, _answer) = crate::configure_fake::FakeLogin::new();
    let held = std::sync::Arc::new(fake_login);
    {
        let mut login = waiting(&fake, LoginTicket(1));
        login.started(crate::login_worker::LoginWorker::new(
            LoginTicket(1),
            held.clone() as std::sync::Arc<dyn crate::configure::BrowserLogin>,
        ));
    }
    assert_eq!(held.cancels(), 1);
}

#[test]
fn a_started_worker_for_another_ticket_is_cancelled_at_once() {
    let fake = fake_with(targets());
    let mut login = waiting(&fake, LoginTicket(1));
    let (fake_login, _answer) = crate::configure_fake::FakeLogin::new();
    let held = std::sync::Arc::new(fake_login);
    login.started(crate::login_worker::LoginWorker::new(
        LoginTicket(2),
        held.clone() as std::sync::Arc<dyn crate::configure::BrowserLogin>,
    ));
    assert_eq!(held.cancels(), 1);
    // The view still waits on its own ticket.
    let url = "https://auth.example/authorize?state=1";
    assert_eq!(
        login.step(
            LoginTicket(1),
            crate::login_worker::LoginStep::Open(url.to_owned())
        ),
        Some(url.to_owned())
    );
}

#[test]
fn a_started_worker_for_the_waiting_ticket_is_kept_until_esc() {
    let fake = fake_with(targets());
    let mut login = waiting(&fake, LoginTicket(1));
    let (fake_login, _answer) = crate::configure_fake::FakeLogin::new();
    let held = std::sync::Arc::new(fake_login);
    login.started(crate::login_worker::LoginWorker::new(
        LoginTicket(1),
        held.clone() as std::sync::Arc<dyn crate::configure::BrowserLogin>,
    ));
    assert_eq!(held.cancels(), 0);
    assert!(matches!(login.key(&Key::Esc, &ctx(&fake)), Act::Stay));
    assert_eq!(held.cancels(), 1);
}

#[test]
fn a_url_at_the_width_is_one_row_and_past_it_is_two() {
    let fake = fake_with(targets());
    let mut login = waiting(&fake, LoginTicket(1));
    let one = "u".repeat(80);
    login.step(
        LoginTicket(1),
        crate::login_worker::LoginStep::Open(one.clone()),
    );
    assert_eq!(login.frame(80).below.len(), 2);
    let two = "u".repeat(81);
    login.step(LoginTicket(1), crate::login_worker::LoginStep::Open(two));
    assert_eq!(login.frame(80).below.len(), 3);
    // Width 0 wraps at 1: every character takes its own row.
    login.step(
        LoginTicket(1),
        crate::login_worker::LoginStep::Open("ab".to_owned()),
    );
    assert_eq!(login.frame(0).below.len(), 3);
}

#[test]
fn a_wide_character_wraps_by_columns_not_chars() {
    let fake = fake_with(targets());
    let mut login = waiting(&fake, LoginTicket(1));
    let url = "https://例え.jp/authorize";
    login.step(
        LoginTicket(1),
        crate::login_worker::LoginStep::Open(url.to_owned()),
    );
    let below = login.frame(20).below;
    for row in below.iter().skip(1) {
        assert!(
            crate::format::width(row) <= 20,
            "{row:?} is wider than 20 columns"
        );
    }
    assert_eq!(below[1..].join(""), url);
}

#[test]
fn a_view_too_short_for_the_url_cuts_rows_and_keeps_copy_whole() {
    let fake = fake_with(targets());
    let mut login = waiting(&fake, LoginTicket(1));
    let url = format!("https://auth.example/{}", "u".repeat(300));
    login.step(
        LoginTicket(1),
        crate::login_worker::LoginStep::Open(url.clone()),
    );
    let area = Rect::new(0, 0, 20, 8);
    let mut buf = Buffer::empty(area);
    render(&login.frame(20), area, &mut buf, &mut Vec::new());
    let rows: Vec<String> = (0..8)
        .map(|y| {
            (0..20)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect();
    assert!(rows[0].starts_with("Log in"), "{rows:?}");
    assert!(
        rows.iter().any(|row| row.contains("Open this URL")),
        "{rows:?}"
    );
    // The frame carries the whole footer; the 20-column render shows what
    // fits of it.
    assert_eq!(login.frame(20).footer, "y copy URL · Esc cancel");
    assert!(
        rows.iter().any(|row| row.contains("y copy URL")),
        "{rows:?}"
    );
    // `y` still copies the whole URL.
    assert!(matches!(
        login.key(&Key::Char('y'), &ctx(&fake)),
        Act::Copy(shown) if shown == url
    ));
    // Tall enough, the URL rows read whole again.
    let tall = drawn(&login, 80, 24);
    let lines: Vec<&str> = tall.lines().collect();
    let at = lines
        .iter()
        .position(|line| line.contains("Open this URL"))
        .unwrap_or_else(|| panic!("no prompt in {tall}"));
    let end = lines
        .iter()
        .position(|line| line.contains("y copy URL"))
        .unwrap_or_else(|| panic!("no footer in {tall}"));
    assert_eq!(lines[at + 1..end].join(""), url);
}

#[test]
fn login_browser_waiting_80x24() {
    let fake = fake_with(targets());
    let mut login = waiting(&fake, LoginTicket(1));
    let url = format!("https://auth.example/authorize?state={}", "s".repeat(100));
    login.step(LoginTicket(1), crate::login_worker::LoginStep::Open(url));
    insta::assert_snapshot!("login_browser_waiting_80x24", drawn(&login, 80, 24));
}

#[test]
fn a_row_click_while_waiting_keeps_the_waiting_mode() {
    let fake = fake_with(targets());
    let mut login = waiting(&fake, LoginTicket(1));
    let url = "https://auth.example/authorize?state=1";
    login.step(
        LoginTicket(1),
        crate::login_worker::LoginStep::Open(url.to_owned()),
    );
    assert!(matches!(login.click(Spot::Row(1), &ctx(&fake)), Act::Stay));
    let frame = login.frame(80);
    assert_eq!(
        frame.below,
        [
            "Open this URL to log in to codex:".to_owned(),
            url.to_owned()
        ]
    );
}
