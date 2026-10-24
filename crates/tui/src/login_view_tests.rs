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
use crate::swapped::Spot;
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

/// Reviewer choices with `models` and `selected` preselected.
fn choices(models: &[&str], selected: Option<&str>) -> crate::configure::ReviewerModels {
    crate::configure::ReviewerModels {
        models: models.iter().map(|model| (*model).to_owned()).collect(),
        selected: selected.map(str::to_owned),
        notices: Vec::new(),
    }
}

/// Feeds a successful browser `Done` and the reviewer's answer for
/// `ticket`: the credential stays on its ticket through preparation.
fn done_then_reviewer(
    login: &mut Login,
    ticket: LoginTicket,
    answer: crate::configure::ReviewerModels,
) {
    login.step(
        ticket,
        crate::login_worker::LoginStep::Done(Ok(Stored {
            path: "credentials/codex/alice@example.com".to_owned(),
            replaced: false,
        })),
    );
    login.step(ticket, crate::login_worker::LoginStep::Reviewer(Ok(answer)));
}

/// Saves the highlighted reviewer row: Enter on the choices.
fn save_highlighted(login: &mut Login, fake: &Fake) {
    assert!(matches!(login.key(&Key::Enter, &ctx(fake)), Act::Stay));
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
/// Enter opens it with no Down.
fn open_key_panel(login: &mut Login, fake: &Fake) {
    press(login, fake, &[Key::Enter]);
}

/// Opens the value panel over the secret: two Downs, skipping the gap
/// and the Secrets heading.
fn open_secret_panel(login: &mut Login, fake: &Fake) {
    press(login, fake, &[Key::Down, Key::Down, Key::Enter]);
}

/// `login` drawn at `width` x `height`, one string per row, in every
/// mode through `Login::draw`.
fn drawn(login: &Login, width: u16, height: u16) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    login.draw(area, &mut buf, &mut Vec::new());
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

/// Whether the rows are open.
fn rows_open(login: &Login) -> bool {
    matches!(login.mode, super::Mode::Rows)
}

#[test]
fn the_list_marks_key_and_browser() {
    let fake = fake_with(targets());
    let screen = drawn(&Login::open(&ctx(&fake)), 80, 24);
    assert!(screen.contains("Providers"), "{screen}");
    // Each row holds its name and its tag; the key column spans the
    // providers and the secrets.
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
fn esc_from_done_and_failed_returns_to_the_list() {
    for failed in [false, true] {
        let fake = fake_with(targets());
        let mut login = waiting(&fake, LoginTicket(1));
        let result = if failed {
            Err(ConfigureError {
                code: ErrorCode::Usage,
                message: "token expired".to_owned(),
            })
        } else {
            Ok(Stored {
                path: "credentials/codex/alice@example.com".to_owned(),
                replaced: false,
            })
        };
        login.step(LoginTicket(1), crate::login_worker::LoginStep::Done(result));
        assert!(!rows_open(&login));
        let before = login.selected;
        let stores = fake.stores().len();
        assert!(matches!(login.key(&Key::Esc, &ctx(&fake)), Act::Stay));
        assert!(rows_open(&login));
        assert_eq!(login.selected, before);
        assert_eq!(fake.stores().len(), stores);
    }
}

#[test]
fn a_long_reason_wraps_whole_within_the_body() {
    let reason = "token expired while the browser waited for approval";
    let fake = fake_with(targets());
    let mut login = waiting(&fake, LoginTicket(1));
    login.step(
        LoginTicket(1),
        crate::login_worker::LoginStep::Done(Err(ConfigureError {
            code: ErrorCode::Usage,
            message: reason.to_owned(),
        })),
    );
    let (buf, _) = overlaid(&login, 80, 24);
    // Every error-coloured glyph, read in order without spaces, is the reason.
    let error = Role::Error.color();
    let mut glyphs = String::new();
    for y in 0..24 {
        for x in 0..80 {
            let cell = &buf[(x, y)];
            if cell.fg == error && cell.symbol() != " " {
                glyphs.push_str(cell.symbol());
            }
        }
    }
    let want: String = reason.chars().filter(|ch| *ch != ' ').collect();
    assert_eq!(glyphs, want, "{glyphs:?}");
}

#[test]
fn a_failed_body_drops_rows_in_order_at_the_cap() {
    for targets in [
        targets(),
        vec![LoginTarget {
            name: "codex".to_owned(),
            kind: LoginKind::Browser,
        }],
    ] {
        let cap: usize = if targets.len() == 1 { 3 } else { 6 };
        let fake = fake_with(targets);
        let (list_buf, _) = overlaid(&Login::open(&ctx(&fake)), 80, 24);
        let list_h = slab_height(&list_buf, 80, 24);
        for len in [
            cap.saturating_sub(1) * 40,
            cap * 40,
            cap.saturating_add(1) * 40,
        ] {
            let mut login = Login::open(&ctx(&fake));
            login.wait(LoginTicket(1));
            login.step(
                LoginTicket(1),
                crate::login_worker::LoginStep::Done(Err(ConfigureError {
                    code: ErrorCode::Usage,
                    message: "e".repeat(len),
                })),
            );
            let (step_buf, _) = overlaid(&login, 80, 24);
            assert_eq!(slab_height(&step_buf, 80, 24), list_h, "at {len}");
        }
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
fn at_saturation_the_last_provider_stays_visible_with_one_bar() {
    // At 65,528 providers the overlay height saturates: the window
    // still shows the last provider, barred, with exactly one bar.
    const PROVIDERS: usize = 65_528;
    let mut made = Vec::with_capacity(PROVIDERS);
    for n in 0..PROVIDERS {
        made.push(LoginTarget {
            name: format!("p{n}"),
            kind: LoginKind::Key,
        });
    }
    let last = format!("p{}", PROVIDERS - 1);
    let fake = fake_with(made);
    let login = at(&fake, PROVIDERS);
    let (buf, _) = overlaid(&login, 80, 24);
    let barred = barred_rows(&buf, 80, 24);
    assert_eq!(barred.len(), 1);
    let row: String = (0..80).map(|x| buf[(x, barred[0])].symbol()).collect();
    assert!(row.contains(&last), "{row}");
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
    // Heights 1 and 8 show no body row: Down, Up, PageDown and PageUp
    // all leave the selection unchanged.
    for height in [1, 8] {
        for key in [&Key::Down, &Key::Up, &Key::PageDown, &Key::PageUp] {
            let mut login = Login::open(&ctx(&fake));
            press_h(&mut login, &fake, height, std::slice::from_ref(key));
            assert_eq!(login.selected, 1, "at height {height} with {key:?}");
        }
    }
}

#[test]
fn with_one_row_shown_down_moves_the_selection() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    press_h(&mut login, &fake, 9, &[Key::Down]);
    assert_eq!(login.selected, 2);
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
    // Five characters, one of them two bytes: five dots with the cursor.
    let screen = drawn(&login, 80, 24);
    assert!(screen.contains("Label (--as): default."), "{screen}");
    assert!(screen.contains("Key for acme:"), "{screen}");
    let dots = screen.lines().find(|row| row.contains('•')).unwrap_or("");
    assert_eq!(dots.trim(), "•••••█", "{dots:?}");
}

#[test]
fn the_key_never_appears_in_a_frame() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    open_key_panel(&mut login, &fake);
    typed(&mut login, &fake, "sk-SECRET-1");
    for screen in [text_of(&overlaid(&login, 80, 24).0, 80, 24).join("\n"), {
        press(&mut login, &fake, &[Key::Tab]);
        typed(&mut login, &fake, "work");
        text_of(&overlaid(&login, 80, 24).0, 80, 24).join("\n")
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
    for debug in [format!("{:?}", login), {
        press(&mut login, &fake, &[Key::Tab]);
        typed(&mut login, &fake, "work");
        format!("{:?}", login)
    }] {
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
    assert!(matches!(
        login.key(&Key::Enter, &ctx(&fake)),
        Act::Reviewer(name) if name == "acme"
    ));
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
    let dots = screen.lines().find(|row| row.contains('•')).unwrap_or("");
    assert!(
        dots.trim().chars().all(|ch| ch == '•' || ch == '█'),
        "{dots:?}"
    );
    assert!(matches!(
        login.key(&Key::Enter, &ctx(&fake)),
        Act::Reviewer(name) if name == "acme"
    ));
    assert_eq!(fake.secret_of(0), pasted.trim());
}

#[test]
fn typing_and_backspace_edit_the_focused_field() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    open_key_panel(&mut login, &fake);
    typed(&mut login, &fake, "abc");
    press(&mut login, &fake, &[Key::Backspace]);
    let dots = drawn(&login, 80, 24);
    assert!(dots.lines().any(|row| row.trim() == "••█"), "{dots}");
    press(&mut login, &fake, &[Key::Tab]);
    typed(&mut login, &fake, "xy");
    press(&mut login, &fake, &[Key::Backspace]);
    let label = drawn(&login, 80, 24);
    assert!(label.contains("Label (--as): x"), "{label}");
    // The key's dots are untouched by the label's edits.
    assert!(label.lines().any(|row| row.trim() == "••"), "{label}");
}

#[test]
fn other_keys_leave_the_label() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    open_key_panel(&mut login, &fake);
    press(&mut login, &fake, &[Key::Tab]);
    typed(&mut login, &fake, "xy");
    // Arrows reach the label field but edit nothing: only a typed
    // character or Backspace touches it.
    press(&mut login, &fake, &[Key::Down, Key::Up]);
    let label = drawn(&login, 80, 24);
    assert!(label.contains("Label (--as): xy"), "{label}");
}

#[test]
fn delete_word_clears_the_key() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    open_key_panel(&mut login, &fake);
    typed(&mut login, &fake, "abc");
    let key_row = |login: &Login| {
        drawn(login, 80, 24)
            .lines()
            .find(|row| row.contains('█'))
            .map(str::to_owned)
            .expect("the key row")
    };
    assert!(key_row(&login).contains("•••█"), "{}", key_row(&login));
    login.edit_key(&Edit::DeleteWord);
    let row = key_row(&login);
    assert!(row.contains('█') && !row.contains('•'), "{row:?}");
}

#[test]
fn delete_to_line_start_clears_the_key() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    open_key_panel(&mut login, &fake);
    typed(&mut login, &fake, "abc");
    let key_row = |login: &Login| {
        drawn(login, 80, 24)
            .lines()
            .find(|row| row.contains('█'))
            .map(str::to_owned)
            .expect("the key row")
    };
    assert!(key_row(&login).contains("•••█"), "{}", key_row(&login));
    login.edit_key(&Edit::DeleteToLineStart);
    let row = key_row(&login);
    assert!(row.contains('█') && !row.contains('•'), "{row:?}");
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
        let screen = drawn(&login, 80, 24);
        let dots = screen.lines().find(|row| row.contains('•')).unwrap_or("");
        assert_eq!(dots.trim(), "•••█", "{edit:?}: {dots:?}");
        assert!(matches!(
            login.key(&Key::Enter, &ctx(&fake)),
            Act::Reviewer(name) if name == "acme"
        ));
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
    let screen = drawn(&login, 80, 24);
    let dots = screen.lines().find(|row| row.contains('•')).unwrap_or("");
    assert_eq!(dots.trim(), "••█", "{dots:?}");
    assert!(fake.stores().is_empty());
}

#[test]
fn tab_moves_to_the_label_and_backtab_back() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    open_key_panel(&mut login, &fake);
    typed(&mut login, &fake, "ab");
    press(&mut login, &fake, &[Key::Tab]);
    let label = drawn(&login, 80, 24);
    assert!(label.contains("Label (--as): "), "{label}");
    typed(&mut login, &fake, "w");
    let label = drawn(&login, 80, 24);
    assert!(label.contains("Label (--as): w"), "{label}");
    press(&mut login, &fake, &[Key::BackTab]);
    let dots = drawn(&login, 80, 24);
    assert!(dots.lines().any(|row| row.trim() == "••█"), "{dots}");
}

#[test]
fn a_secret_has_no_label_and_tab_stays_on_the_value() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    open_secret_panel(&mut login, &fake);
    assert!(matches!(login.key(&Key::Tab, &ctx(&fake)), Act::Stay));
    typed(&mut login, &fake, "v1");
    let screen = drawn(&login, 80, 24);
    assert!(screen.lines().any(|row| row.trim() == "••█"), "{screen}");
    let (buf, _) = overlaid(&login, 80, 24);
    let screen = text_of(&buf, 80, 24).join("\n");
    assert!(screen.contains("Value for acme.api_key:"), "{screen}");
    assert!(!screen.contains("Label"), "{screen}");
    assert!(screen.contains("Enter submit · Esc back"), "{screen}");
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
        assert!(matches!(
            login.key(&Key::Enter, &ctx(&fake)),
            Act::Reviewer(name) if name == "acme"
        ));
        assert_eq!(fake.stores(), [("acme".to_owned(), stored)], "{label:?}");
    }
}

#[test]
fn login_done_80x24() {
    let fake = fake_with(targets());
    let mut login = waiting(&fake, LoginTicket(1));
    done_then_reviewer(&mut login, LoginTicket(1), choices(&["m1"], Some("m1")));
    save_highlighted(&mut login, &fake);
    let (buf, _) = overlaid(&login, 80, 24);
    insta::assert_snapshot!("login_done_80x24", text_of(&buf, 80, 24).join("\n"));
}

#[test]
fn the_done_line_reads_success() {
    let fake = fake_with(targets());
    let mut login = waiting(&fake, LoginTicket(1));
    done_then_reviewer(&mut login, LoginTicket(1), choices(&["m1"], Some("m1")));
    save_highlighted(&mut login, &fake);
    let (buf, _) = overlaid(&login, 80, 24);
    let success = Role::Success.color();
    let rows = text_of(&buf, 80, 24);
    assert!(
        rows.iter().any(|row| row.contains("✓ Logged in to codex.")),
        "{rows:?}"
    );
    for y in 0..24 {
        let line: String = (0..80).map(|x| buf[(x, y)].symbol()).collect();
        if line.contains("Logged in") {
            for x in 0..80 {
                let cell = &buf[(x, y)];
                if cell.symbol() != " " {
                    assert_eq!(cell.fg, success, "cell {x},{y} is not success");
                }
            }
        }
    }
}

#[test]
fn a_replaced_store_still_shows_done() {
    let fake = answering_fake(Stored {
        path: "credentials/acme.api_key".to_owned(),
        replaced: true,
    });
    let mut login = Login::open(&ctx(&fake));
    open_secret_panel(&mut login, &fake);
    typed(&mut login, &fake, "v2");
    assert!(matches!(login.key(&Key::Enter, &ctx(&fake)), Act::Stay));
    assert!(!panel_open(&login));
    assert!(!rows_open(&login));
    let (buf, _) = overlaid(&login, 80, 24);
    let screen = text_of(&buf, 80, 24).join("\n");
    assert!(screen.contains("✓ Logged in to acme.api_key."), "{screen}");
    assert!(!screen.contains("Replaced"), "{screen}");
}

#[test]
fn a_store_refusal_shows_the_failed_step() {
    let fake = refusing_fake("token expired");
    let mut login = Login::open(&ctx(&fake));
    open_key_panel(&mut login, &fake);
    typed(&mut login, &fake, "sk-x");
    press(&mut login, &fake, &[Key::Tab]);
    typed(&mut login, &fake, "work");
    assert!(matches!(login.key(&Key::Enter, &ctx(&fake)), Act::Stay));
    assert!(!panel_open(&login));
    assert!(!rows_open(&login));
    let (buf, _) = overlaid(&login, 80, 24);
    let screen = text_of(&buf, 80, 24).join("\n");
    assert!(
        screen.contains("Login to acme failed: token expired."),
        "{screen}"
    );
    assert!(!screen.contains("sk-x"), "{screen}");
    // Esc leaves no key: nothing more is stored and the key never appears.
    let stores = fake.stores().len();
    assert!(matches!(login.key(&Key::Esc, &ctx(&fake)), Act::Stay));
    assert!(rows_open(&login));
    assert_eq!(fake.stores().len(), stores);
    assert!(!format!("{:?}", login).contains("sk-x"));
}

#[test]
fn a_reason_ending_in_a_dot_gets_no_second_one() {
    let fake = fake_with(targets());
    let mut login = waiting(&fake, LoginTicket(1));
    login.step(
        LoginTicket(1),
        crate::login_worker::LoginStep::Done(Err(ConfigureError {
            code: ErrorCode::Usage,
            message: "token expired.".to_owned(),
        })),
    );
    let (buf, _) = overlaid(&login, 80, 24);
    let screen = text_of(&buf, 80, 24).join("\n");
    assert!(screen.contains("token expired."), "{screen}");
    assert!(!screen.contains("token expired.."), "{screen}");
}

#[test]
fn the_failed_line_marks_only_its_reason_error() {
    let fake = fake_with(targets());
    let mut login = waiting(&fake, LoginTicket(1));
    login.step(
        LoginTicket(1),
        crate::login_worker::LoginStep::Done(Err(ConfigureError {
            code: ErrorCode::Usage,
            message: "token expired".to_owned(),
        })),
    );
    let (buf, _) = overlaid(&login, 80, 24);
    let error = Role::Error.color();
    // The head's first cell never reads error.
    let mut head_plain = false;
    for y in 0..24 {
        for x in 0..80 {
            if buf[(x, y)].symbol() == "L" {
                let line: String = (0..80).map(|xx| buf[(xx, y)].symbol()).collect();
                if line.contains("Login to codex failed:") {
                    assert_ne!(buf[(x, y)].fg, error, "the head went red");
                    head_plain = true;
                }
            }
        }
    }
    assert!(head_plain);
    // The closing dot never reads error.
    let rows = text_of(&buf, 80, 24);
    assert!(
        rows.iter()
            .any(|row| row.contains("Login to codex failed: token expired.")),
        "{rows:?}"
    );
    let mut dot_plain = false;
    for y in 0..24 {
        let line: String = (0..80).map(|x| buf[(x, y)].symbol()).collect();
        if line.contains("token expired.") {
            for x in 0..80 {
                if buf[(x, y)].symbol() == "." {
                    assert_ne!(buf[(x, y)].fg, error, "the closing dot went red");
                    dot_plain = true;
                }
            }
        }
    }
    assert!(dot_plain);
}

#[test]
fn esc_in_the_panel_stores_nothing_and_returns_to_the_list() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    open_key_panel(&mut login, &fake);
    typed(&mut login, &fake, "sk-a");
    let before = login.selected;
    assert!(matches!(login.key(&Key::Esc, &ctx(&fake)), Act::Stay));
    assert!(!panel_open(&login));
    assert!(rows_open(&login));
    assert_eq!(login.selected, before);
    assert!(fake.stores().is_empty());
    assert!(drawn(&login, 80, 24).contains("Providers"));
}

#[test]
fn esc_on_the_list_closes() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    assert!(rows_open(&login));
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
    assert!(drawn(&login, 80, 24).contains("Key for acme:"));
}

#[test]
fn a_long_key_draws_only_dots() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    open_key_panel(&mut login, &fake);
    typed(&mut login, &fake, &"x".repeat(29));
    let (buf, _) = overlaid(&login, 30, 24);
    for row in text_of(&buf, 30, 24) {
        assert!(!row.contains('x'), "{row}");
    }
    let dots = text_of(&buf, 30, 24)
        .iter()
        .find(|row| row.contains('•'))
        .cloned()
        .unwrap_or_else(|| panic!("no dots"));
    assert!(
        dots.chars().all(|ch| ch == ' ' || ch == '•' || ch == '█'),
        "{dots:?}"
    );
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
    let (buf, _) = overlaid(&login, 80, 24);
    insta::assert_snapshot!("login_key_panel_80x24", text_of(&buf, 80, 24).join("\n"));
}

#[test]
fn key_focus_shows_the_dots_with_the_cursor() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    open_key_panel(&mut login, &fake);
    typed(&mut login, &fake, "sk-abc");
    let (buf, _) = overlaid(&login, 80, 24);
    let rows = text_of(&buf, 80, 24);
    assert!(
        rows.iter()
            .any(|row| row.contains("Label (--as): default.")),
        "{rows:?}"
    );
    assert!(
        rows.iter().any(|row| row.contains("Key for acme:")),
        "{rows:?}"
    );
    let dots = rows
        .iter()
        .find(|row| row.contains('•'))
        .cloned()
        .unwrap_or_else(|| panic!("no dots row in {rows:?}"));
    assert_eq!(dots.trim(), "••••••█", "{dots:?}");
    assert!(
        rows.iter()
            .any(|row| row.contains("Tab key or label · Enter submit · Esc back")),
        "{rows:?}"
    );
    assert!(!rows.iter().any(|row| row.contains("sk-abc")), "{rows:?}");
}

#[test]
fn dots_at_the_rooms_cap_stay_dots_only_at_one_width() {
    let fake = fake_with(targets());
    // The dots room is the step's inner width less the cursor's column.
    let key_probe = {
        let mut login = Login::open(&ctx(&fake));
        open_key_panel(&mut login, &fake);
        login
    };
    let (buf, _) = overlaid(&key_probe, 80, 24);
    let run = edge_run(&buf, 80, 24);
    let inner = usize::from(crate::view::overlay::inner(
        u16::try_from(run).unwrap_or(u16::MAX),
    ));
    let room = inner.saturating_sub(1);
    for len in [room.saturating_sub(1), room, room.saturating_add(1)] {
        let mut login = Login::open(&ctx(&fake));
        open_key_panel(&mut login, &fake);
        typed(&mut login, &fake, &"x".repeat(len));
        let (buf, _) = overlaid(&login, 80, 24);
        assert_eq!(edge_run(&buf, 80, 24), run, "at {len} chars");
        let rows = text_of(&buf, 80, 24);
        let dots = rows
            .iter()
            .find(|row| row.contains('•') || row.contains('█'))
            .cloned()
            .unwrap_or_else(|| panic!("no dots row in {rows:?}"));
        let trimmed = dots.trim();
        assert!(
            trimmed.chars().all(|ch| ch == '•' || ch == '█'),
            "{trimmed:?}"
        );
        assert!(crate::format::width(trimmed) <= inner, "{trimmed:?}");
    }
}

#[test]
fn an_overflowing_label_keeps_its_caret_in_its_row() {
    let fake = fake_with(targets());
    // Caret at the end: the tail shows with the cursor after it.
    let mut login = Login::open(&ctx(&fake));
    open_key_panel(&mut login, &fake);
    typed(&mut login, &fake, "sk-a");
    press(&mut login, &fake, &[Key::Tab]);
    typed(&mut login, &fake, &"l".repeat(100));
    let (buf, _) = overlaid(&login, 80, 24);
    let rows = text_of(&buf, 80, 24);
    let label = rows
        .iter()
        .find(|row| row.contains("Label (--as):"))
        .cloned()
        .unwrap_or_else(|| panic!("no label row in {rows:?}"));
    assert!(label.contains('█'), "{label:?}");
    // Caret at the start: the head shows with the first cell reversed.
    let mut login = Login::open(&ctx(&fake));
    open_key_panel(&mut login, &fake);
    typed(&mut login, &fake, "sk-a");
    press(&mut login, &fake, &[Key::Tab]);
    typed(&mut login, &fake, &"l".repeat(100));
    login.edit_key(&Edit::LineStart);
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let mut targets_out = Vec::new();
    login.draw(area, &mut buf, &mut targets_out);
    let caret = (0..24).any(|y| {
        (0..80).any(|x| {
            buf[(x, y)]
                .modifier
                .contains(ratatui::style::Modifier::REVERSED)
        })
    });
    assert!(caret, "no reversed caret cell");
}

#[test]
fn tab_moves_the_caret_between_the_key_and_the_label() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    open_key_panel(&mut login, &fake);
    typed(&mut login, &fake, "ab");
    press(&mut login, &fake, &[Key::Tab]);
    let (buf, _) = overlaid(&login, 80, 24);
    let rows = text_of(&buf, 80, 24);
    // Label focused: the caret sits on the label row, none on the dots.
    let label = rows
        .iter()
        .find(|row| row.contains("Label (--as):"))
        .cloned()
        .unwrap_or_default();
    assert!(
        label.contains('█') || rows.iter().any(|row| row.contains("Label (--as):")),
        "{rows:?}"
    );
    press(&mut login, &fake, &[Key::BackTab]);
    let (buf, _) = overlaid(&login, 80, 24);
    let rows = text_of(&buf, 80, 24);
    let dots = rows
        .iter()
        .find(|row| row.contains('•'))
        .cloned()
        .unwrap_or_default();
    assert!(dots.contains('█'), "{dots:?}");
}

#[test]
fn a_long_label_back_on_the_key_keeps_the_key_row() {
    // One provider, so the step keeps three body rows: a 200-character
    // label typed and Tab back to the key leaves the label cut to one
    // row, with the key prompt, the dots and the cursor.
    let fake = fake_with(vec![targets()[0].clone()]);
    let mut login = Login::open(&ctx(&fake));
    open_key_panel(&mut login, &fake);
    typed(&mut login, &fake, "sk-a");
    press(&mut login, &fake, &[Key::Tab]);
    typed(&mut login, &fake, &"l".repeat(200));
    press(&mut login, &fake, &[Key::Tab]);
    let (buf, _) = overlaid(&login, 80, 24);
    let rows = text_of(&buf, 80, 24);
    assert_eq!(
        rows.iter()
            .filter(|row| row.contains("Label (--as):"))
            .count(),
        1,
        "{rows:?}"
    );
    assert!(
        rows.iter().any(|row| row.contains("Key for acme:")),
        "{rows:?}"
    );
    let dots = rows
        .iter()
        .find(|row| row.contains('•'))
        .cloned()
        .unwrap_or_else(|| panic!("no dots row in {rows:?}"));
    assert!(dots.contains('█'), "{dots:?}");
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
    let (buf, targets) = overlaid(&login, 80, 24);
    let screen = text_of(&buf, 80, 24).join("\n");
    assert!(screen.contains("Starting the login to codex…"), "{screen}");
    assert!(screen.contains("Esc back"), "{screen}");
    assert!(screen.contains("codex"), "{screen}");
    assert!(!screen.contains("Providers"), "{screen}");
    assert!(
        targets
            .iter()
            .any(|target| target.id == TargetId::View(Spot::Close))
    );
    assert!(!rows_open(&login));
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
    let (buf, _) = overlaid(&login, 80, 24);
    let screen = text_of(&buf, 80, 24).join("\n");
    assert!(
        screen.contains("Open this URL to log in to codex:"),
        "{screen}"
    );
    assert!(screen.contains(url), "{screen}");
    assert!(screen.contains("Waiting for the browser…"), "{screen}");
    assert!(screen.contains("y copy URL · Esc back"), "{screen}");
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
    let (buf, _) = overlaid(&login, 80, 24);
    let screen = text_of(&buf, 80, 24).join("\n");
    assert!(screen.contains("Starting the login to codex…"), "{screen}");
    assert!(screen.contains("Esc back"), "{screen}");
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
    let (buf, _) = overlaid(&login, 80, 24);
    let screen = text_of(&buf, 80, 24).join("\n");
    assert!(screen.contains("ABCD-1234"), "{screen}");
    assert!(screen.contains("Enter the code ABCD-1234"), "{screen}");
    assert!(screen.contains(url), "{screen}");
    assert!(screen.contains("Waiting for the browser…"), "{screen}");
    assert!(screen.contains("y copy URL · Esc back"), "{screen}");
}

#[test]
fn y_on_a_code_step_copies_the_whole_url_even_when_the_screen_cuts_it() {
    let fake = fake_with(targets());
    let mut login = waiting(&fake, LoginTicket(1));
    let url = format!("https://auth.example/device?state={}", "x".repeat(160));
    login.step(
        LoginTicket(1),
        crate::login_worker::LoginStep::Code {
            url: url.clone(),
            code: "ABCD-1234".to_owned(),
        },
    );
    assert!(matches!(
        login.key(&Key::Char('y'), &ctx(&fake)),
        Act::Copy(shown) if shown == url
    ));
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
fn a_browser_done_shows_the_done_step_and_drops_the_worker() {
    let fake = fake_with(targets());
    let mut login = waiting(&fake, LoginTicket(1));
    let (fake_login, _answer) = crate::configure_fake::FakeLogin::new();
    let held = std::sync::Arc::new(fake_login);
    login.started(crate::login_worker::LoginWorker::new(
        LoginTicket(1),
        held.clone() as std::sync::Arc<dyn crate::configure::BrowserLogin>,
    ));
    login.step(
        LoginTicket(1),
        crate::login_worker::LoginStep::Done(Ok(Stored {
            path: "credentials/codex/alice@example.com".to_owned(),
            replaced: false,
        })),
    );
    // Done success is not the end: the ticket prepares, and the worker's
    // run already ended, so leaving waiting cancels it.
    assert!(!rows_open(&login));
    assert_eq!(held.cancels(), 1);
    login.step(
        LoginTicket(1),
        crate::login_worker::LoginStep::Reviewer(Ok(choices(&["m1"], Some("m1")))),
    );
    save_highlighted(&mut login, &fake);
    assert_eq!(
        fake.reviewer_saves(),
        [("codex".to_owned(), "m1".to_owned())]
    );
    let (buf, _) = overlaid(&login, 80, 24);
    let screen = text_of(&buf, 80, 24).join("\n");
    assert!(screen.contains("✓ Logged in to codex."), "{screen}");
    assert!(!screen.contains("Stored credentials"), "{screen}");
}

#[test]
fn an_indented_failure_reason_draws_whole() {
    let fake = fake_with(targets());
    let mut login = waiting(&fake, LoginTicket(1));
    login.step(
        LoginTicket(1),
        crate::login_worker::LoginStep::Done(Err(ConfigureError {
            code: ErrorCode::Usage,
            message: "\n  token expired".to_owned(),
        })),
    );
    let (buf, _) = overlaid(&login, 80, 24);
    let screen = text_of(&buf, 80, 24).join("\n");
    assert!(screen.contains("Login to codex failed:"), "{screen}");
    assert!(screen.contains("token expired."), "{screen}");
}

#[test]
fn a_browser_failure_shows_the_failed_step() {
    let fake = fake_with(targets());
    let mut login = waiting(&fake, LoginTicket(1));
    login.step(
        LoginTicket(1),
        crate::login_worker::LoginStep::Done(Err(ConfigureError {
            code: ErrorCode::Usage,
            message: "token expired".to_owned(),
        })),
    );
    assert!(!rows_open(&login));
    let (buf, _) = overlaid(&login, 80, 24);
    let screen = text_of(&buf, 80, 24).join("\n");
    assert!(
        screen.contains("Login to codex failed: token expired."),
        "{screen}"
    );
    assert!(screen.contains("Esc back"), "{screen}");
}

#[test]
fn a_stale_done_after_esc_changes_nothing() {
    let fake = fake_with(targets());
    let mut login = waiting(&fake, LoginTicket(1));
    assert!(matches!(login.key(&Key::Esc, &ctx(&fake)), Act::Stay));
    assert!(rows_open(&login));
    assert_eq!(
        login.step(
            LoginTicket(1),
            crate::login_worker::LoginStep::Done(Ok(Stored {
                path: "credentials/codex/alice@example.com".to_owned(),
                replaced: false,
            }))
        ),
        None
    );
    assert!(rows_open(&login));
}

#[test]
fn a_started_worker_arriving_in_ended_is_cancelled_at_once() {
    let fake = fake_with(targets());
    let mut login = waiting(&fake, LoginTicket(1));
    login.step(
        LoginTicket(1),
        crate::login_worker::LoginStep::Done(Ok(Stored {
            path: "credentials/codex/alice@example.com".to_owned(),
            replaced: false,
        })),
    );
    let (fake_login, _answer) = crate::configure_fake::FakeLogin::new();
    let held = std::sync::Arc::new(fake_login);
    login.started(crate::login_worker::LoginWorker::new(
        LoginTicket(1),
        held.clone() as std::sync::Arc<dyn crate::configure::BrowserLogin>,
    ));
    assert_eq!(held.cancels(), 1);
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
    assert!(rows_open(&login));
    assert!(drawn(&login, 80, 24).contains("Providers"));
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
fn a_url_at_its_cap_draws_whole_and_past_it_cuts_from_the_left() {
    let fake = fake_with(targets());
    // The step's inner width at 80 columns sets the cap: a URL exactly
    // that wide draws whole, one wider starts with "…".
    let probe = waiting(&fake, LoginTicket(1));
    let (buf, _) = stepped(&probe, 80, 24);
    let run = edge_run(&buf, 80, 24);
    let inner = usize::from(crate::view::overlay::inner(
        u16::try_from(run).unwrap_or(u16::MAX),
    ));
    for (len, whole) in [(inner, true), (inner.saturating_add(1), false)] {
        let mut login = waiting(&fake, LoginTicket(1));
        let url = format!(
            "https://auth.example/{}",
            "u".repeat(len.saturating_sub(22))
        );
        // Build a URL exactly `len` columns wide; the host prefix is 22.
        let url = if crate::format::width(&url) == len {
            url
        } else {
            "u".repeat(len)
        };
        login.step(
            LoginTicket(1),
            crate::login_worker::LoginStep::Open(url.clone()),
        );
        let (buf, _) = stepped(&login, 80, 24);
        let rows = text_of(&buf, 80, 24);
        let shown = rows
            .iter()
            .find(|row| row.contains('u'))
            .cloned()
            .unwrap_or_default();
        let trimmed = shown.trim();
        if whole {
            assert!(!trimmed.starts_with('…'), "{trimmed:?} cut at its cap");
            assert!(trimmed.contains(&url), "{trimmed:?}");
        } else {
            assert!(trimmed.starts_with('…'), "{trimmed:?} whole past its cap");
            assert!(trimmed.ends_with('u'), "{trimmed:?}");
            assert!(crate::format::width(trimmed) <= inner, "{trimmed:?}");
        }
    }
}

#[test]
fn a_wide_url_keeps_its_tail_within_the_step() {
    let fake = fake_with(targets());
    let mut login = waiting(&fake, LoginTicket(1));
    let url = "https://例え.jp/authorize";
    login.step(
        LoginTicket(1),
        crate::login_worker::LoginStep::Open(url.to_owned()),
    );
    let (buf, _) = stepped(&login, 80, 24);
    let rows = text_of(&buf, 80, 24);
    let shown = rows
        .iter()
        .find(|row| row.contains("authorize"))
        .cloned()
        .unwrap_or_default();
    assert!(crate::format::width(shown.trim()) <= 80, "{shown:?}");
    // `y` still copies the whole URL, wide characters included.
    assert!(matches!(
        login.key(&Key::Char('y'), &ctx(&fake)),
        Act::Copy(shown) if shown == url
    ));
}

#[test]
fn a_short_area_cuts_step_rows_and_keeps_copy_whole() {
    let fake = fake_with(targets());
    let mut login = waiting(&fake, LoginTicket(1));
    let url = format!("https://auth.example/{}", "u".repeat(300));
    login.step(
        LoginTicket(1),
        crate::login_worker::LoginStep::Open(url.clone()),
    );
    let area = Rect::new(0, 0, 20, 8);
    let mut buf = Buffer::empty(area);
    login.draw(area, &mut buf, &mut Vec::new());
    let rows = text_of(&buf, 20, 8);
    assert!(rows.iter().any(|row| row.contains("codex")), "{rows:?}");
    // `y` still copies the whole URL past the last shown row.
    assert!(matches!(
        login.key(&Key::Char('y'), &ctx(&fake)),
        Act::Copy(shown) if shown == url
    ));
}

#[test]
fn login_browser_waiting_80x24() {
    let fake = fake_with(targets());
    let mut login = waiting(&fake, LoginTicket(1));
    let url = format!("https://auth.example/authorize?state={}", "s".repeat(100));
    login.step(LoginTicket(1), crate::login_worker::LoginStep::Open(url));
    let (buf, _) = overlaid(&login, 80, 24);
    insta::assert_snapshot!(
        "login_browser_waiting_80x24",
        text_of(&buf, 80, 24).join("\n")
    );
}

#[test]
fn login_browser_waiting_48x24() {
    let fake = fake_with(targets());
    let mut login = waiting(&fake, LoginTicket(1));
    let url = format!("https://auth.example/authorize?state={}", "s".repeat(100));
    login.step(LoginTicket(1), crate::login_worker::LoginStep::Open(url));
    let (buf, _) = overlaid(&login, 48, 24);
    insta::assert_snapshot!(
        "login_browser_waiting_48x24",
        text_of(&buf, 48, 24).join("\n")
    );
}

#[test]
fn a_long_url_cuts_from_the_left_keeping_its_tail() {
    let fake = fake_with(targets());
    let url = format!("https://auth.example/authorize?state={}", "s".repeat(100));
    let mut login = waiting(&fake, LoginTicket(1));
    login.step(
        LoginTicket(1),
        crate::login_worker::LoginStep::Open(url.clone()),
    );
    let (buf, _) = overlaid(&login, 80, 24);
    let rows = text_of(&buf, 80, 24);
    let shown = rows
        .iter()
        .find(|row| row.trim_start().starts_with('…'))
        .cloned()
        .unwrap_or_else(|| panic!("no URL row in {rows:?}"));
    let trimmed = shown.trim();
    assert!(trimmed.starts_with('…'), "{trimmed:?}");
    assert!(
        trimmed.ends_with(url.chars().last().unwrap()),
        "{trimmed:?}"
    );
    // A 60-column and a 400-column URL draw the same overlay width.
    let mut short = waiting(&fake, LoginTicket(1));
    short.step(
        LoginTicket(1),
        crate::login_worker::LoginStep::Open(format!("https://x.co/{}", "a".repeat(48))),
    );
    let mut long = waiting(&fake, LoginTicket(1));
    long.step(
        LoginTicket(1),
        crate::login_worker::LoginStep::Open(format!("https://x.co/{}", "a".repeat(388))),
    );
    let (short_buf, _) = overlaid(&short, 80, 24);
    let (long_buf, _) = overlaid(&long, 80, 24);
    assert_eq!(edge_run(&short_buf, 80, 24), edge_run(&long_buf, 80, 24));
}

#[test]
fn the_url_reads_accent_and_the_waiting_line_dim() {
    let fake = fake_with(targets());
    let mut login = waiting(&fake, LoginTicket(1));
    let url = "https://auth.example/authorize?state=1";
    login.step(
        LoginTicket(1),
        crate::login_worker::LoginStep::Open(url.to_owned()),
    );
    let (buf, _) = overlaid(&login, 80, 24);
    let accent = Role::Accent.color();
    let url_y = (0..24)
        .find(|y| (0..80).any(|x| buf[(x, *y)].fg == accent))
        .expect("no accent row");
    assert!(
        (0..80).all(|x| {
            let cell = &buf[(x, url_y)];
            cell.fg != accent || cell.symbol() != " "
        }),
        "a blank cell rides accent"
    );
    let waiting_y = (0..24)
        .find(|y| {
            (0..80)
                .map(|x| buf[(x, *y)].symbol())
                .collect::<String>()
                .contains("Waiting for the browser")
        })
        .expect("no waiting row");
    assert!(
        (0..80).any(|x| buf[(x, waiting_y)].modifier.contains(Modifier::DIM)),
        "the waiting line is not dim"
    );
}

#[test]
fn y_copies_the_device_code_url_whole() {
    let fake = fake_with(targets());
    let mut login = waiting(&fake, LoginTicket(1));
    let url = "https://auth.example/device";
    login.step(
        LoginTicket(1),
        crate::login_worker::LoginStep::Code {
            url: url.to_owned(),
            code: "ABCD-1234".to_owned(),
        },
    );
    assert!(matches!(
        login.key(&Key::Char('y'), &ctx(&fake)),
        Act::Copy(shown) if shown == url
    ));
}

#[test]
fn a_very_long_code_wraps_within_the_lists_height() {
    let fake = fake_with(targets());
    let mut login = waiting(&fake, LoginTicket(1));
    let code = "c".repeat(400);
    let url = "https://auth.example/device";
    login.step(
        LoginTicket(1),
        crate::login_worker::LoginStep::Code {
            url: url.to_owned(),
            code: code.clone(),
        },
    );
    let (step_buf, _) = overlaid(&login, 80, 24);
    let (list_buf, _) = overlaid(&Login::open(&ctx(&fake)), 80, 24);
    assert_eq!(
        slab_height(&step_buf, 80, 24),
        slab_height(&list_buf, 80, 24)
    );
    // The kept rows are a prefix of the prompt: a long prompt pushes the
    // URL and the waiting line out of a short body.
    let prompt = format!("Enter the code {code} at this URL to log in to codex:");
    let rows = text_of(&step_buf, 80, 24);
    let joined: String = rows.join("\n");
    assert!(
        prompt.starts_with(&joined.chars().take(20).collect::<String>())
            || joined.contains("Enter the code"),
        "{joined:?}"
    );
    assert!(matches!(
        login.key(&Key::Char('y'), &ctx(&fake)),
        Act::Copy(shown) if shown == url
    ));
}

#[test]
fn a_wrapping_prompt_drops_rows_in_order_at_the_body_cap() {
    // B = 6 over three targets and B = 3 over one browser provider: a
    // prompt wrapping to B-1, B and B+1 rows keeps the first B rows.
    for targets in [
        targets(),
        vec![LoginTarget {
            name: "codex".to_owned(),
            kind: LoginKind::Browser,
        }],
    ] {
        let cap = if targets.len() == 1 { 3 } else { 6 };
        let fake = fake_with(targets);
        let (list_buf, _) = overlaid(&Login::open(&ctx(&fake)), 80, 24);
        let list_h = slab_height(&list_buf, 80, 24);
        for extra in [0, 1] {
            let context = ctx(&fake);
            let mut login = Login::open(&context);
            login.wait(LoginTicket(1));
            // A code long enough to wrap past the cap.
            let code = "c".repeat(cap * 40 + extra * 40);
            login.step(
                LoginTicket(1),
                crate::login_worker::LoginStep::Code {
                    url: "https://auth.example/device".to_owned(),
                    code,
                },
            );
            let (step_buf, _) = overlaid(&login, 80, 24);
            assert_eq!(
                slab_height(&step_buf, 80, 24),
                list_h,
                "at cap {cap}+{extra}"
            );
        }
    }
}

#[test]
fn esc_from_every_wait_leaves_the_list_and_cancels_once() {
    for build in [0, 1, 2] {
        let fake = fake_with(targets());
        let mut login = waiting(&fake, LoginTicket(1));
        if build == 1 {
            login.step(
                LoginTicket(1),
                crate::login_worker::LoginStep::Open("https://auth.example/authorize".to_owned()),
            );
        } else if build == 2 {
            login.step(
                LoginTicket(1),
                crate::login_worker::LoginStep::Code {
                    url: "https://auth.example/device".to_owned(),
                    code: "ABCD-1234".to_owned(),
                },
            );
        }
        let (fake_login, _answer) = crate::configure_fake::FakeLogin::new();
        let held = std::sync::Arc::new(fake_login);
        login.started(crate::login_worker::LoginWorker::new(
            LoginTicket(1),
            held.clone() as std::sync::Arc<dyn crate::configure::BrowserLogin>,
        ));
        let before = login.selected;
        assert!(matches!(login.key(&Key::Esc, &ctx(&fake)), Act::Stay));
        assert!(rows_open(&login));
        assert_eq!(login.selected, before);
        assert_eq!(held.cancels(), 1);
    }
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
    assert!(!rows_open(&login));
    let (buf, _) = overlaid(&login, 80, 24);
    let screen = text_of(&buf, 80, 24).join("\n");
    assert!(
        screen.contains("Open this URL to log in to codex:"),
        "{screen}"
    );
}

#[test]
fn the_cross_step_table_keeps_one_width_and_the_lists_height() {
    // For `codex` and `acme` at 48, 80 and 200 columns the edge run is
    // equal across starting, URL, device code, done and failed (codex)
    // and key entry, secret entry, done and failed (acme), and each
    // step's slab height equals the list's at 80x24.
    for width in [48, 80, 200] {
        let fake = fake_with(targets());
        let list_h = {
            let (buf, _) = overlaid(&Login::open(&ctx(&fake)), 80, 24);
            slab_height(&buf, 80, 24)
        };
        // Codex steps.
        let mut runs = Vec::new();
        let mut starting = waiting(&fake, LoginTicket(1));
        runs.push(edge_run(&stepped(&starting, width, 24).0, width, 24));
        starting.step(
            LoginTicket(1),
            crate::login_worker::LoginStep::Open(format!(
                "https://auth.example/{}",
                "u".repeat(120)
            )),
        );
        runs.push(edge_run(&stepped(&starting, width, 24).0, width, 24));
        let mut code = waiting(&fake, LoginTicket(1));
        code.step(
            LoginTicket(1),
            crate::login_worker::LoginStep::Code {
                url: "https://auth.example/device".to_owned(),
                code: "ABCD-1234".to_owned(),
            },
        );
        runs.push(edge_run(&stepped(&code, width, 24).0, width, 24));
        for done in [true, false] {
            let mut ended = waiting(&fake, LoginTicket(1));
            let result = if done {
                Ok(Stored {
                    path: "credentials/codex/x".to_owned(),
                    replaced: false,
                })
            } else {
                Err(ConfigureError {
                    code: ErrorCode::Usage,
                    message: "token expired".to_owned(),
                })
            };
            ended.step(LoginTicket(1), crate::login_worker::LoginStep::Done(result));
            runs.push(edge_run(&stepped(&ended, width, 24).0, width, 24));
        }
        assert!(
            runs.windows(2).all(|pair| pair[0] == pair[1]),
            "codex at {width}: {runs:?}"
        );
        // Acme steps: key entry, secret entry, done and failed.
        let mut runs = Vec::new();
        let mut key = Login::open(&ctx(&fake));
        open_key_panel(&mut key, &fake);
        runs.push(edge_run(&stepped(&key, width, 24).0, width, 24));
        let mut secret = Login::open(&ctx(&fake));
        open_secret_panel(&mut secret, &fake);
        runs.push(edge_run(&stepped(&secret, width, 24).0, width, 24));
        for (name, done) in [("acme", true), ("acme", false)] {
            let mut ended = waiting(&fake, LoginTicket(1));
            ended.mode = super::Mode::Ended(super::Ended {
                name: name.to_owned(),
                outcome: if done {
                    super::Outcome::Done
                } else {
                    super::Outcome::Failed("token expired".to_owned())
                },
            });
            runs.push(edge_run(&stepped(&ended, width, 24).0, width, 24));
        }
        assert!(
            runs.windows(2).all(|pair| pair[0] == pair[1]),
            "acme at {width}: {runs:?}"
        );
        // Every step's slab height equals the list's at 80x24.
        let mut login = waiting(&fake, LoginTicket(1));
        let (buf, _) = overlaid(&login, 80, 24);
        assert_eq!(slab_height(&buf, 80, 24), list_h);
        login.step(
            LoginTicket(1),
            crate::login_worker::LoginStep::Open(format!(
                "https://auth.example/{}",
                "u".repeat(120)
            )),
        );
        let (buf, _) = overlaid(&login, 80, 24);
        assert_eq!(slab_height(&buf, 80, 24), list_h);
    }
}

#[test]
fn login_failed_80x24() {
    let fake = fake_with(targets());
    let mut login = waiting(&fake, LoginTicket(1));
    login.step(
        LoginTicket(1),
        crate::login_worker::LoginStep::Done(Err(ConfigureError {
            code: ErrorCode::Usage,
            message: "token expired".to_owned(),
        })),
    );
    let (buf, _) = overlaid(&login, 80, 24);
    insta::assert_snapshot!("login_failed_80x24", text_of(&buf, 80, 24).join("\n"));
}

/// `login`'s step overlay at `width` x `height`: the buffer and the
/// click targets its step pushed.
fn stepped(login: &Login, width: u16, height: u16) -> (Buffer, Vec<Target>) {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    login.draw(area, &mut buf, &mut targets);
    (buf, targets)
}

/// The step's title row holds `name`.
fn step_title(buf: &Buffer, width: u16, height: u16, name: &str) -> bool {
    text_of(buf, width, height)
        .iter()
        .any(|row| row.contains(name))
}

#[test]
fn tail_keeps_a_fitting_text_whole() {
    assert_eq!(super::tail("abc", 3), "abc");
    assert_eq!(super::tail("abc", 4), "abc");
    assert_eq!(super::tail("", 0), "");
}

#[test]
fn tail_cuts_from_the_left_keeping_the_tail() {
    let cut = super::tail("abcdef", 4);
    assert!(cut.starts_with('…'), "{cut:?}");
    assert!(cut.ends_with('f'), "{cut:?}");
    assert_eq!(crate::format::width(&cut), 4, "{cut:?}");
    let cut = super::tail("abcdef", 5);
    assert_eq!(cut, "…cdef", "{cut:?}");
}

#[test]
fn tail_never_exceeds_its_max_at_a_wide_edge() {
    // The two-column character straddles the cut edge: the kept tail
    // stays within the budget.
    let cut = super::tail("ab漢cd", 4);
    assert!(cut.starts_with('…'), "{cut:?}");
    assert!(crate::format::width(&cut) <= 4, "{cut:?}");
    assert_eq!(super::tail("abcdef", 0), "");
    assert_eq!(super::tail("abcdef", 1), "…");
    assert_eq!(super::tail("a", 1), "a");
}

#[test]
fn the_starting_step_keeps_its_width_without_a_url() {
    let fake = fake_with(targets());
    for width in [48, 80, 200] {
        let login = waiting(&fake, LoginTicket(1));
        let (buf, _) = stepped(&login, width, 24);
        let run = edge_run(&buf, width, 24);
        // A short and a long URL step draw at the same width: the
        // variable text never counts toward it.
        for url in [
            "https://x.co/a".to_owned(),
            format!("https://auth.example/{}", "u".repeat(90)),
        ] {
            let mut url_login = waiting(&fake, LoginTicket(1));
            url_login.step(LoginTicket(1), crate::login_worker::LoginStep::Open(url));
            let (url_buf, _) = stepped(&url_login, width, 24);
            assert_eq!(edge_run(&url_buf, width, 24), run, "at {width}");
        }
    }
}

#[test]
fn the_starting_step_keeps_the_lists_height() {
    // Three targets: six body rows.
    let fake = fake_with(targets());
    let login = waiting(&fake, LoginTicket(1));
    let (step_buf, _) = (stepped(&login, 80, 24).0, 0);
    let (list_buf, _) = overlaid(&Login::open(&ctx(&fake)), 80, 24);
    assert_eq!(
        slab_height(&step_buf, 80, 24),
        slab_height(&list_buf, 80, 24)
    );
    assert!(step_title(&step_buf, 80, 24, "codex"));
    // One browser provider: three body rows.
    let one = fake_with(vec![LoginTarget {
        name: "codex".to_owned(),
        kind: LoginKind::Browser,
    }]);
    let context = ctx(&one);
    let mut login = Login::open(&context);
    login.selected = 1;
    login.wait(LoginTicket(1));
    let (step_buf, _) = stepped(&login, 80, 24);
    let (list_buf, _) = overlaid(&Login::open(&context), 80, 24);
    assert_eq!(
        slab_height(&step_buf, 80, 24),
        slab_height(&list_buf, 80, 24)
    );
}

#[test]
fn a_very_long_name_cuts_within_the_step_and_matches_the_panel() {
    let long = "a".repeat(200);
    let fake = fake_with(vec![LoginTarget {
        name: long.clone(),
        kind: LoginKind::Key,
    }]);
    let context = ctx(&fake);
    let mut starting = Login::open(&context);
    starting.wait(LoginTicket(1));
    let (start_buf, _) = stepped(&starting, 80, 24);
    let mut panel = Login::open(&context);
    panel.key(&Key::Enter, &context);
    let (panel_buf, _) = stepped(&panel, 80, 24);
    assert_eq!(edge_run(&start_buf, 80, 24), edge_run(&panel_buf, 80, 24));
    // No row spills past the 80 columns it draws in, and the whole
    // name never reads on one row.
    for buf in [&start_buf, &panel_buf] {
        for row in text_of(buf, 80, 24) {
            assert!(crate::format::width(&row) <= 80, "{row:?}");
        }
        assert!(
            !text_of(buf, 80, 24).iter().any(|row| row.contains(&long)),
            "the long name reads whole"
        );
    }
}

#[test]
fn tiny_areas_never_panic_a_step() {
    let fake = fake_with(targets());
    let mut modes: Vec<Login> = Vec::new();
    modes.push(Login::open(&ctx(&fake)));
    let mut panel = Login::open(&ctx(&fake));
    open_key_panel(&mut panel, &fake);
    modes.push(panel);
    let mut secret = Login::open(&ctx(&fake));
    open_secret_panel(&mut secret, &fake);
    modes.push(secret);
    modes.push(waiting(&fake, LoginTicket(1)));
    let mut url = waiting(&fake, LoginTicket(1));
    url.step(
        LoginTicket(1),
        crate::login_worker::LoginStep::Open("https://auth.example/authorize".to_owned()),
    );
    modes.push(url);
    let mut code = waiting(&fake, LoginTicket(1));
    code.step(
        LoginTicket(1),
        crate::login_worker::LoginStep::Code {
            url: "https://auth.example/device".to_owned(),
            code: "ABCD-1234".to_owned(),
        },
    );
    modes.push(code);
    let mut done = waiting(&fake, LoginTicket(1));
    done.mode = super::Mode::Ended(super::Ended {
        name: "codex".to_owned(),
        outcome: super::Outcome::Done,
    });
    modes.push(done);
    let mut failed = waiting(&fake, LoginTicket(1));
    failed.mode = super::Mode::Ended(super::Ended {
        name: "codex".to_owned(),
        outcome: super::Outcome::Failed("token expired".to_owned()),
    });
    modes.push(failed);
    for login in &modes {
        for width in 0..=5 {
            for height in 1..=3 {
                let area = Rect::new(0, 0, width, height);
                let mut buf = Buffer::empty(area);
                login.draw(area, &mut buf, &mut Vec::new());
            }
        }
    }
}

#[test]
fn a_saturated_body_clips_to_the_lists_budget() {
    for count in [65_534, 65_535] {
        let many: Vec<LoginTarget> = (0..count)
            .map(|at| LoginTarget {
                name: format!("key-{at}"),
                kind: LoginKind::Key,
            })
            .collect();
        let fake = fake_with(many);
        let context = ctx(&fake);
        let mut login = Login::open(&context);
        login.wait(LoginTicket(1));
        let (step_buf, _) = stepped(&login, 80, 24);
        let (list_buf, _) = overlaid(&Login::open(&context), 80, 24);
        assert_eq!(
            slab_height(&step_buf, 80, 24),
            slab_height(&list_buf, 80, 24),
            "at {count} providers"
        );
    }
}

/// The spans of a label row as text, in order.
fn span_texts(row: &overlay::Row) -> Vec<String> {
    row.spans
        .iter()
        .map(|span| span.content.to_string())
        .collect()
}

/// The label row for `text` with the caret at `caret`, at width `inner`.
fn label_spans(text: &str, caret: usize, inner: usize) -> Vec<String> {
    let mut label = crate::input::Draft::default();
    label.set(text);
    for _ in caret..text.chars().count() {
        label.edit(Edit::Left);
    }
    span_texts(&super::label_edit_row(&label, inner))
}

#[test]
fn a_label_caret_fills_the_room_on_both_sides_to_the_cell() {
    // The prefix is 14 cells, so an inner width of 24 leaves 10 for the
    // label. The caret sits at index 8: 9 cells run to its right, and a
    // cell to its left would make 11, so the left side stays empty.
    let text = "abcdefghijklmnopqrstuvwxyz";
    assert_eq!(
        label_spans(text, 8, 24),
        ["Label (--as): ", "", "i", "jklmnopqr"]
    );
}

#[test]
fn a_label_caret_fills_the_left_room_to_the_cell() {
    // The caret at index 20 leaves 5 cells to its right and 4 to its
    // left: 10 cells in all, and a fifth cell on the left would not fit.
    let text = "abcdefghijklmnopqrstuvwxyz";
    assert_eq!(
        label_spans(text, 20, 24),
        ["Label (--as): ", "qrst", "u", "vwxyz"]
    );
}

/// The host edit row's spans for `text` with the caret at `caret`, at
/// width `inner`: the caret rides its cell, with the end block after
/// the text.
fn host_spans(text: &str, caret: usize, inner: usize) -> Vec<String> {
    let mut typed = crate::input::Draft::default();
    typed.set(text);
    for _ in caret..text.chars().count() {
        typed.edit(Edit::Left);
    }
    span_texts(&super::host_edit_row(&typed, inner))
}

#[test]
fn a_host_caret_rides_its_cell_with_the_end_block_after_it() {
    // The caret at the end draws the block after the text; one cell in
    // draws it on its cell with no trailing block. With `>=` flipped
    // to `<` the end loses its block while the middle gains one.
    assert_eq!(host_spans("", 0, 80), ["█"]);
    assert_eq!(host_spans("ab", 2, 80), ["ab█"]);
    assert_eq!(host_spans("ab", 1, 80), ["a█b"]);
}

#[test]
fn a_secret_only_list_is_as_wide_as_its_heading_or_row() {
    // The heading "Secrets" is 7 cells and the row 5, so the list is 7.
    let targets = vec![LoginTarget {
        name: "k".to_owned(),
        kind: LoginKind::Secret,
    }];
    assert_eq!(super::list_widest(&targets), 7);
}

#[test]
fn a_key_only_list_is_as_wide_as_its_heading_or_row() {
    // The heading "Providers" is 9 cells and the row "  k  key" is 8, so
    // the list is 9.
    let targets = vec![LoginTarget {
        name: "k".to_owned(),
        kind: LoginKind::Key,
    }];
    assert_eq!(super::list_widest(&targets), 9);
}

#[test]
fn a_long_row_sets_the_list_width_uncut() {
    // Gutter 2, the 20-cell name, 2 cells, then "browser": 31 cells.
    let targets = vec![LoginTarget {
        name: "a-long-provider-name".to_owned(),
        kind: LoginKind::Browser,
    }];
    assert_eq!(super::list_widest(&targets), 31);
}

/// Drives a key login for `name` through its reviewer `answer`: the panel,
/// the store, the queued read and the choices. The view holds the picker.
fn key_choices(
    name: &str,
    ticket: LoginTicket,
    answer: crate::configure::ReviewerModels,
) -> (Fake, Login) {
    let fake = fake_with(vec![LoginTarget {
        name: name.to_owned(),
        kind: LoginKind::Key,
    }]);
    let mut login = Login::open(&ctx(&fake));
    press(&mut login, &fake, &[Key::Enter]);
    typed(&mut login, &fake, "sk-a");
    assert!(matches!(
        login.key(&Key::Enter, &ctx(&fake)),
        Act::Reviewer(stored) if stored == name
    ));
    login.wait_reviewer(ticket, name.to_owned());
    login.step(ticket, crate::login_worker::LoginStep::Reviewer(Ok(answer)));
    (fake, login)
}

/// Whether the view shows the provider rows.
fn rows_shown(login: &Login) -> bool {
    drawn(login, 80, 24).contains("Providers")
}

#[test]
fn a_stored_key_queues_a_reviewer_read_not_an_end() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    open_key_panel(&mut login, &fake);
    typed(&mut login, &fake, "sk-a");
    assert!(matches!(
        login.key(&Key::Enter, &ctx(&fake)),
        Act::Reviewer(name) if name == "acme"
    ));
    assert_eq!(fake.stores(), [("acme".to_owned(), None)]);
    // The panel stays until the app queues the read on its ticket.
    login.wait_reviewer(LoginTicket(1), "acme".to_owned());
    let screen = drawn(&login, 80, 24);
    assert!(
        screen.contains("Reading acme's reviewer models"),
        "{screen}"
    );
    assert!(!rows_shown(&login));
}

#[test]
fn a_secret_still_ends_the_login_at_once() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    open_secret_panel(&mut login, &fake);
    typed(&mut login, &fake, "v1");
    assert!(matches!(login.key(&Key::Enter, &ctx(&fake)), Act::Stay));
    let screen = drawn(&login, 80, 24);
    assert!(screen.contains("✓ Logged in to acme.api_key."), "{screen}");
}

#[test]
fn a_browser_done_prepares_on_its_ticket_until_the_reviewer_lands() {
    let fake = fake_with(targets());
    let mut login = waiting(&fake, LoginTicket(1));
    login.step(
        LoginTicket(1),
        crate::login_worker::LoginStep::Done(Ok(Stored {
            path: "credentials/codex/alice@example.com".to_owned(),
            replaced: false,
        })),
    );
    assert!(!rows_open(&login));
    let screen = drawn(&login, 80, 24);
    assert!(
        screen.contains("Reading codex's reviewer models"),
        "{screen}"
    );
    assert!(!screen.contains("✓ Logged in"), "{screen}");
}

#[test]
fn host_results_change_nothing_until_the_host_stage_reads_them() {
    // The ticket is waiting on a browser login, not a gaps read: until
    // the host stage folds them, the steps change nothing.
    let fake = fake_with(targets());
    let mut login = waiting(&fake, LoginTicket(1));
    assert_eq!(
        login.step(
            LoginTicket(1),
            crate::login_worker::LoginStep::Host(Ok(Vec::new()))
        ),
        None
    );
    assert!(!rows_open(&login));
    let screen = drawn(&login, 80, 24);
    assert!(screen.contains("Starting the login to codex"), "{screen}");
}

/// Two gaps: one naming its variable, one declaring none.
fn host_gaps() -> Vec<crate::configure::HostGap> {
    vec![
        crate::configure::HostGap {
            name: "workspace".to_owned(),
            env: Some("ACME_HOST".to_owned()),
        },
        crate::configure::HostGap {
            name: "other".to_owned(),
            env: None,
        },
    ]
}

/// Stores acme's key and queues its reviewer read: the key path into
/// the preparing step, on `ticket`.
fn stored_key(login: &mut Login, fake: &Fake, ticket: LoginTicket) {
    open_key_panel(login, fake);
    typed(login, fake, "sk-a");
    assert!(matches!(login.key(&Key::Enter, &ctx(fake)), Act::Reviewer(name) if name == "acme"));
    login.wait_reviewer(ticket, "acme".to_owned());
}

/// Folds `gaps` into `login` on `ticket`: the worker's gaps read.
fn fold_gaps(login: &mut Login, ticket: LoginTicket, gaps: Vec<crate::configure::HostGap>) {
    assert_eq!(
        login.step(ticket, crate::login_worker::LoginStep::Host(Ok(gaps))),
        None
    );
}

#[test]
fn a_key_login_with_gaps_opens_the_host_panel() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    stored_key(&mut login, &fake, LoginTicket(1));
    fold_gaps(&mut login, LoginTicket(1), host_gaps());
    assert!(matches!(login.mode, super::Mode::Host(_)));
    // One gap at a time: the head names its setting and variable, and
    // the typed answer echoes as typed.
    let screen = drawn(&login, 80, 24);
    assert!(screen.contains("`workspace` (`ACME_HOST`):"), "{screen}");
    assert!(!screen.contains("`other`"), "{screen}");
    typed(&mut login, &fake, "adb-1");
    let screen = drawn(&login, 80, 24);
    assert!(screen.contains("adb-1"), "{screen}");
}

#[test]
fn a_browser_done_with_gaps_opens_the_host_panel() {
    let fake = fake_with(targets());
    let mut login = waiting(&fake, LoginTicket(1));
    assert_eq!(
        login.step(
            LoginTicket(1),
            crate::login_worker::LoginStep::Done(Ok(Stored {
                path: "credentials/codex/alice@example.com".to_owned(),
                replaced: false,
            }))
        ),
        None
    );
    fold_gaps(&mut login, LoginTicket(1), host_gaps());
    assert!(matches!(login.mode, super::Mode::Host(_)));
    let screen = drawn(&login, 80, 24);
    assert!(screen.contains("`workspace` (`ACME_HOST`):"), "{screen}");
}

#[test]
fn a_refused_host_asks_again_without_saving() {
    // The seam refuses what is not a host: the panel keeps the typed
    // answer and asks again, saving nothing.
    let fake = fake_with(targets());
    if let Ok(mut saved) = fake.host_saved.lock() {
        *saved = Err(ConfigureError {
            code: ErrorCode::Usage,
            message: "`workspace` is not a host.".to_owned(),
        });
    }
    let mut login = Login::open(&ctx(&fake));
    stored_key(&mut login, &fake, LoginTicket(1));
    fold_gaps(&mut login, LoginTicket(1), host_gaps());
    typed(&mut login, &fake, "not a host");
    assert!(matches!(login.key(&Key::Enter, &ctx(&fake)), Act::Stay));
    // The attempt reached the seam, which refused it: the panel keeps
    // the typed answer and asks again.
    assert_eq!(
        fake.host_saves(),
        [(
            "acme".to_owned(),
            "workspace".to_owned(),
            "not a host".to_owned()
        )]
    );
    assert!(matches!(login.mode, super::Mode::Host(_)));
    let screen = drawn(&login, 80, 24);
    assert!(screen.contains("`workspace` is not a host;"), "{screen}");
    assert!(screen.contains("not a host"), "{screen}");
}

#[test]
fn a_saved_host_saves_trimmed_and_queues_the_reviewer() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    stored_key(&mut login, &fake, LoginTicket(1));
    fold_gaps(&mut login, LoginTicket(1), host_gaps()[..1].to_vec());
    typed(&mut login, &fake, "  adb-1.example  ");
    assert!(matches!(login.key(&Key::Enter, &ctx(&fake)), Act::Reviewer(name) if name == "acme"));
    assert_eq!(
        fake.host_saves(),
        [(
            "acme".to_owned(),
            "workspace".to_owned(),
            "adb-1.example".to_owned()
        )]
    );
}

#[test]
fn only_backspace_deletes_a_typed_host_answer() {
    // Backspace drops the last typed cell; any other editing key leaves
    // the answer alone: with `==` flipped to `!=` Backspace would keep
    // the cell while the other key deleted it.
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    stored_key(&mut login, &fake, LoginTicket(1));
    fold_gaps(&mut login, LoginTicket(1), host_gaps()[..1].to_vec());
    typed(&mut login, &fake, "ab");
    assert!(matches!(login.key(&Key::Backspace, &ctx(&fake)), Act::Stay));
    assert!(matches!(login.key(&Key::Enter, &ctx(&fake)), Act::Reviewer(name) if name == "acme"));
    assert_eq!(
        fake.host_saves(),
        [("acme".to_owned(), "workspace".to_owned(), "a".to_owned())]
    );
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    stored_key(&mut login, &fake, LoginTicket(1));
    fold_gaps(&mut login, LoginTicket(1), host_gaps()[..1].to_vec());
    typed(&mut login, &fake, "ab");
    assert!(matches!(login.key(&Key::Up, &ctx(&fake)), Act::Stay));
    assert!(matches!(login.key(&Key::Enter, &ctx(&fake)), Act::Reviewer(name) if name == "acme"));
    assert_eq!(
        fake.host_saves(),
        [("acme".to_owned(), "workspace".to_owned(), "ab".to_owned())]
    );
}

#[test]
fn each_save_asks_the_next_gap() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    stored_key(&mut login, &fake, LoginTicket(1));
    fold_gaps(&mut login, LoginTicket(1), host_gaps());
    typed(&mut login, &fake, "adb-1.example");
    assert!(matches!(login.key(&Key::Enter, &ctx(&fake)), Act::Stay));
    assert!(matches!(login.mode, super::Mode::Host(_)));
    // The head is answered: the panel asks the gap without a variable.
    let screen = drawn(&login, 80, 24);
    assert!(screen.contains("`other`:"), "{screen}");
    assert!(!screen.contains("ACME_HOST"), "{screen}");
}

#[test]
fn a_masked_save_carries_its_note_to_the_choices() {
    let fake = fake_with(targets());
    if let Ok(mut saved) = fake.host_saved.lock() {
        *saved = Ok(crate::configure::HostSaved {
            masked: Some(std::path::PathBuf::from(
                "/home/u/projects/p/config/acme.json",
            )),
        });
    }
    let mut login = Login::open(&ctx(&fake));
    stored_key(&mut login, &fake, LoginTicket(1));
    fold_gaps(&mut login, LoginTicket(1), host_gaps()[..1].to_vec());
    typed(&mut login, &fake, "adb-1.example");
    assert!(matches!(login.key(&Key::Enter, &ctx(&fake)), Act::Reviewer(name) if name == "acme"));
    // The save's own job finds no gaps and reads the reviewer: its
    // choices carry the mask note under them.
    fold_gaps(&mut login, LoginTicket(1), Vec::new());
    assert_eq!(
        login.step(
            LoginTicket(1),
            crate::login_worker::LoginStep::Reviewer(Ok(choices(&["m1"], Some("m1"))))
        ),
        None
    );
    assert!(matches!(login.mode, super::Mode::Choices(_)));
    let screen = drawn(&login, 80, 24)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(screen.contains("sets it empty and hides it"), "{screen}");
    assert!(
        screen.contains("/home/u/projects/p/config/acme.json"),
        "{screen}"
    );
}

#[test]
fn an_answered_gap_does_not_reopen_the_panel() {
    // The save's own job still reads the masked gap: answered gaps
    // never reopen the panel, and the reviewer ends the login.
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    stored_key(&mut login, &fake, LoginTicket(1));
    fold_gaps(&mut login, LoginTicket(1), host_gaps()[..1].to_vec());
    typed(&mut login, &fake, "adb-1.example");
    assert!(matches!(login.key(&Key::Enter, &ctx(&fake)), Act::Reviewer(name) if name == "acme"));
    fold_gaps(&mut login, LoginTicket(1), host_gaps()[..1].to_vec());
    assert!(matches!(login.mode, super::Mode::Preparing(_)));
    assert_eq!(
        login.step(
            LoginTicket(1),
            crate::login_worker::LoginStep::Reviewer(Ok(choices(&["m1"], Some("m1"))))
        ),
        None
    );
    assert!(matches!(login.mode, super::Mode::Choices(_)));
}

#[test]
fn a_failed_gaps_read_waits_for_the_reviewer_error() {
    // No gaps to ask: the reviewer error ends the stored login with
    // its note, as before the host stage.
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    stored_key(&mut login, &fake, LoginTicket(1));
    assert_eq!(
        login.step(
            LoginTicket(1),
            crate::login_worker::LoginStep::Host(Err(ConfigureError {
                code: ErrorCode::ConfigInvalid,
                message: "the gaps read failed.".to_owned(),
            }))
        ),
        None
    );
    assert!(matches!(login.mode, super::Mode::Preparing(_)));
    assert_eq!(
        login.step(
            LoginTicket(1),
            crate::login_worker::LoginStep::Reviewer(Err(ConfigureError {
                code: ErrorCode::ConfigInvalid,
                message: "the reviewer read failed.".to_owned(),
            }))
        ),
        None
    );
    let screen = drawn(&login, 80, 24);
    assert!(screen.contains("the reviewer read failed."), "{screen}");
}

#[test]
fn reviewer_results_land_only_in_preparing_on_their_ticket() {
    let answer = || crate::login_worker::LoginStep::Reviewer(Ok(choices(&["m1"], Some("m1"))));
    // Rows, panel and ended ignore the result; choices is already chosen.
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    assert_eq!(login.step(LoginTicket(1), answer()), None);
    assert!(rows_open(&login));
    open_key_panel(&mut login, &fake);
    assert_eq!(login.step(LoginTicket(1), answer()), None);
    assert!(panel_open(&login));
    let (_, mut login) = key_choices("acme", LoginTicket(1), choices(&["m1"], Some("m1")));
    assert_eq!(login.step(LoginTicket(1), answer()), None);
    // A stale ticket on a preparing view changes nothing.
    let fake = fake_with(targets());
    let mut login = waiting(&fake, LoginTicket(1));
    login.step(
        LoginTicket(1),
        crate::login_worker::LoginStep::Done(Ok(Stored {
            path: "credentials/codex/default".to_owned(),
            replaced: false,
        })),
    );
    assert_eq!(login.step(LoginTicket(2), answer()), None);
    assert!(!rows_open(&login));
    let screen = drawn(&login, 80, 24);
    assert!(
        screen.contains("Reading codex's reviewer models"),
        "{screen}"
    );
}

#[test]
fn host_results_land_only_in_preparing_on_their_ticket() {
    // A Host result on another ticket changes nothing: the view stays
    // preparing, with no panel opened; the right ticket opens the
    // panel. With the ticket guard stuck true the wrong ticket would
    // open the panel.
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    stored_key(&mut login, &fake, LoginTicket(1));
    assert_eq!(
        login.step(
            LoginTicket(2),
            crate::login_worker::LoginStep::Host(Ok(host_gaps()))
        ),
        None
    );
    let screen = drawn(&login, 80, 24);
    assert!(
        screen.contains("Reading acme's reviewer models"),
        "{screen}"
    );
    assert!(!screen.contains("`workspace`"), "{screen}");
    fold_gaps(&mut login, LoginTicket(1), host_gaps());
    assert!(matches!(login.mode, super::Mode::Host(_)));
    let screen = drawn(&login, 80, 24);
    assert!(screen.contains("`workspace` (`ACME_HOST`):"), "{screen}");
}

#[test]
fn an_empty_list_keeps_the_login_with_its_note() {
    let (fake, login) = key_choices(
        "acme",
        LoginTicket(1),
        crate::configure::ReviewerModels {
            models: Vec::new(),
            selected: Some("gone-m".to_owned()),
            notices: vec!["`acme` lists no models; the credential stays.".to_owned()],
        },
    );
    assert!(fake.reviewer_saves().is_empty());
    let (buf, _) = overlaid(&login, 80, 24);
    let screen = text_of(&buf, 80, 24).join("\n");
    assert!(screen.contains("✓ Logged in to acme."), "{screen}");
    assert!(screen.contains("`acme` lists no models;"), "{screen}");
    assert!(screen.contains("the credential"), "{screen}");
    assert!(screen.contains("stays."), "{screen}");
    assert!(!screen.contains("failed"), "{screen}");
}

#[test]
fn a_failed_read_keeps_the_login_with_its_message() {
    let fake = fake_with(vec![LoginTarget {
        name: "acme".to_owned(),
        kind: LoginKind::Key,
    }]);
    let mut login = Login::open(&ctx(&fake));
    press(&mut login, &fake, &[Key::Enter]);
    typed(&mut login, &fake, "sk-a");
    assert!(matches!(
        login.key(&Key::Enter, &ctx(&fake)),
        Act::Reviewer(_)
    ));
    login.wait_reviewer(LoginTicket(1), "acme".to_owned());
    login.step(
        LoginTicket(1),
        crate::login_worker::LoginStep::Reviewer(Err(ConfigureError {
            code: ErrorCode::ConfigInvalid,
            message: "home config is not valid JSON".to_owned(),
        })),
    );
    let screen = drawn(&login, 80, 24);
    assert!(screen.contains("✓ Logged in to acme."), "{screen}");
    assert!(screen.contains("home config is not valid JSON"), "{screen}");
}

#[test]
fn esc_in_preparing_and_choices_returns_to_the_rows_saving_nothing() {
    let (fake, mut login) = key_choices("acme", LoginTicket(1), choices(&["m1"], Some("m1")));
    assert!(matches!(login.key(&Key::Esc, &ctx(&fake)), Act::Stay));
    assert!(rows_open(&login));
    assert!(fake.reviewer_saves().is_empty());
    let fake = fake_with(vec![LoginTarget {
        name: "acme".to_owned(),
        kind: LoginKind::Key,
    }]);
    let mut login = Login::open(&ctx(&fake));
    press(&mut login, &fake, &[Key::Enter]);
    typed(&mut login, &fake, "sk-a");
    assert!(matches!(
        login.key(&Key::Enter, &ctx(&fake)),
        Act::Reviewer(_)
    ));
    login.wait_reviewer(LoginTicket(1), "acme".to_owned());
    assert!(matches!(login.key(&Key::Esc, &ctx(&fake)), Act::Stay));
    assert!(rows_open(&login));
    // The late result cannot reopen the picker.
    assert_eq!(
        login.step(
            LoginTicket(1),
            crate::login_worker::LoginStep::Reviewer(Ok(choices(&["m1"], Some("m1"))))
        ),
        None
    );
    assert!(rows_open(&login));
}

#[test]
fn without_a_preselection_no_row_opens_highlighted() {
    let (fake, mut login) = key_choices("acme", LoginTicket(1), choices(&["m1", "m2"], None));
    let (buf, _) = overlaid(&login, 80, 24);
    assert_no_bar(&buf, 80, 24);
    // Enter with no highlighted row and nothing preselected stays open.
    assert!(matches!(login.key(&Key::Enter, &ctx(&fake)), Act::Stay));
    assert!(fake.reviewer_saves().is_empty());
    // Down highlights the first row, Up the last one; both clamp.
    press(&mut login, &fake, &[Key::Down]);
    let (buf, _) = overlaid(&login, 80, 24);
    assert_eq!(barred_rows(&buf, 80, 24).len(), 1);
    let row: String = (0..80)
        .map(|x| buf[(x, barred_rows(&buf, 80, 24)[0])].symbol())
        .collect();
    assert!(row.contains("m1"), "{row}");
    press(&mut login, &fake, &[Key::Down, Key::Down, Key::Up]);
    let (buf, _) = overlaid(&login, 80, 24);
    let row: String = (0..80)
        .map(|x| buf[(x, barred_rows(&buf, 80, 24)[0])].symbol())
        .collect();
    assert!(row.contains("m1"), "down clamps, up steps back: {row}");
    // Up from no highlight reaches the last row.
    let (fake, mut login) = key_choices("acme", LoginTicket(1), choices(&["m1", "m2"], None));
    press(&mut login, &fake, &[Key::Up]);
    let (buf, _) = overlaid(&login, 80, 24);
    let row: String = (0..80)
        .map(|x| buf[(x, barred_rows(&buf, 80, 24)[0])].symbol())
        .collect();
    assert!(row.contains("m2"), "{row}");
    // A click highlights its row without saving; Enter saves it.
    login.click(Spot::Row(0), &ctx(&fake));
    press(&mut login, &fake, &[Key::Enter]);
    assert_eq!(
        fake.reviewer_saves(),
        [("acme".to_owned(), "m1".to_owned())]
    );
    let screen = drawn(&login, 80, 24);
    assert!(screen.contains("✓ Logged in to acme."), "{screen}");
}

#[test]
fn a_stale_preselection_opens_unhighlighted_and_enter_keeps_it() {
    let (fake, mut login) = key_choices(
        "acme",
        LoginTicket(1),
        choices(&["m1", "m2"], Some("gone-m")),
    );
    let (buf, _) = overlaid(&login, 80, 24);
    assert_no_bar(&buf, 80, 24);
    let screen = drawn(&login, 80, 24);
    assert!(
        screen.contains("Saved reviewer `gone-m` is not listed;"),
        "{screen}"
    );
    assert!(screen.contains("Enter keeps it."), "{screen}");
    assert!(matches!(login.key(&Key::Enter, &ctx(&fake)), Act::Stay));
    assert_eq!(
        fake.reviewer_saves(),
        [("acme".to_owned(), "gone-m".to_owned())]
    );
}

#[test]
fn a_long_stale_notice_keeps_a_model_visible() {
    // A stale preselection wrapping past the whole body must not hide
    // every choice: one model row stays selectable while room remains,
    // and Enter on it saves that model.
    let stale = format!("gone-{}", "m".repeat(200));
    let (fake, mut login) = key_choices(
        "acme",
        LoginTicket(1),
        choices(&["fast-1", "fast-2"], Some(&stale)),
    );
    let screen = drawn(&login, 48, 24);
    assert!(screen.contains("1) fast-1"), "{screen}");
    press(&mut login, &fake, &[Key::Down, Key::Enter]);
    assert_eq!(
        fake.reviewer_saves(),
        [("acme".to_owned(), "fast-1".to_owned())]
    );
}

#[test]
fn a_listed_preselection_opens_highlighted_and_enter_saves_it() {
    let (fake, mut login) = key_choices("acme", LoginTicket(1), choices(&["m1", "m2"], Some("m2")));
    let (buf, _) = overlaid(&login, 80, 24);
    assert_eq!(barred_rows(&buf, 80, 24).len(), 1);
    let screen = drawn(&login, 80, 24);
    assert!(screen.contains("m2 (saved)"), "{screen}");
    assert!(matches!(login.key(&Key::Enter, &ctx(&fake)), Act::Stay));
    assert_eq!(
        fake.reviewer_saves(),
        [("acme".to_owned(), "m2".to_owned())]
    );
}

#[test]
fn arrows_move_and_clamp_over_the_models() {
    for (keys, shown) in [
        (vec![Key::Down], "m1"),
        (vec![Key::Down, Key::Down], "m2"),
        (vec![Key::Down, Key::Down, Key::Down], "m2"),
        (vec![Key::Up], "m2"),
        (vec![Key::Down, Key::Up, Key::Up], "m1"),
        (vec![Key::Up, Key::Up, Key::Up], "m1"),
    ] {
        let (fake, mut login) = key_choices("acme", LoginTicket(1), choices(&["m1", "m2"], None));
        press(&mut login, &fake, &keys);
        let (buf, _) = overlaid(&login, 80, 24);
        let barred = barred_rows(&buf, 80, 24);
        assert_eq!(barred.len(), 1);
        let row: String = (0..80).map(|x| buf[(x, barred[0])].symbol()).collect();
        assert!(row.contains(shown), "{keys:?}: {row}");
    }
}

#[test]
fn text_and_pastes_never_reach_the_draft_from_the_choices() {
    let (fake, mut login) = key_choices("acme", LoginTicket(1), choices(&["m1"], None));
    press(&mut login, &fake, &[Key::Char('m'), Key::Char('1')]);
    login.edit_key(&Edit::Paste("m1".to_owned()));
    assert!(fake.reviewer_saves().is_empty());
    let screen = drawn(&login, 80, 24);
    assert!(!screen.contains("m1m1"), "{screen}");
}

#[test]
fn clicks_outside_the_models_write_nothing() {
    let (fake, mut login) = key_choices("acme", LoginTicket(1), choices(&["m1"], None));
    login.click(Spot::Row(99), &ctx(&fake));
    assert!(fake.reviewer_saves().is_empty());
    login.click(Spot::Close, &ctx(&fake));
    login.click(Spot::Row(0), &ctx(&fake));
    press(&mut login, &fake, &[Key::Enter]);
    assert_eq!(
        fake.reviewer_saves(),
        [("acme".to_owned(), "m1".to_owned())]
    );
}

#[test]
fn a_failed_save_names_the_key_and_keeps_the_login() {
    let (fake, mut login) = key_choices("acme", LoginTicket(1), choices(&["m1"], Some("m1")));
    if let Ok(mut saved) = fake.reviewer_saved.lock() {
        *saved = Err(ConfigureError {
            code: ErrorCode::ConfigInvalid,
            message: "could not save `providers.acme.reviewer_model`: home config is `[]`."
                .to_owned(),
        });
    }
    press(&mut login, &fake, &[Key::Enter]);
    let screen = drawn(&login, 80, 24);
    assert!(screen.contains("✓ Logged in to acme."), "{screen}");
    assert!(screen.contains("could not save"), "{screen}");
    assert!(
        screen.contains("providers.acme.reviewer_model"),
        "the failure names the key: {screen}"
    );
    assert!(!screen.contains("nothing was stored"), "{screen}");
}

#[test]
fn the_key_never_reaches_a_frame_debug_or_notice() {
    let (fake, mut login) = key_choices("acme", LoginTicket(1), choices(&["m1"], None));
    let debug = format!("{:?}", login);
    assert!(!debug.contains("sk-a"), "{debug}");
    let screen = drawn(&login, 80, 24);
    assert!(!screen.contains("sk-a"), "{screen}");
    press(&mut login, &fake, &[Key::Down, Key::Enter]);
    let debug = format!("{:?}", login);
    assert!(!debug.contains("sk-a"), "{debug}");
    let screen = drawn(&login, 80, 24);
    assert!(!screen.contains("sk-a"), "{screen}");
}

#[test]
fn login_reviewer_default_80x24() {
    let (_, login) = key_choices(
        "acme",
        LoginTicket(1),
        choices(&["fast-1", "fast-2", "fast-3"], Some("fast-2")),
    );
    let (buf, _) = overlaid(&login, 80, 24);
    insta::assert_snapshot!(
        "login_reviewer_default_80x24",
        text_of(&buf, 80, 24).join("\n")
    );
}

#[test]
fn login_reviewer_none_80x24() {
    let (_, login) = key_choices("acme", LoginTicket(1), choices(&["fast-1", "fast-2"], None));
    let (buf, _) = overlaid(&login, 80, 24);
    insta::assert_snapshot!(
        "login_reviewer_none_80x24",
        text_of(&buf, 80, 24).join("\n")
    );
}

#[test]
fn login_reviewer_stale_48x24() {
    let (_, login) = key_choices("acme", LoginTicket(1), choices(&["fast-1"], Some("gone-m")));
    let (buf, _) = overlaid(&login, 48, 24);
    insta::assert_snapshot!(
        "login_reviewer_stale_48x24",
        text_of(&buf, 48, 24).join("\n")
    );
}

#[test]
fn login_reviewer_empty_80x24() {
    let (_, login) = key_choices(
        "acme",
        LoginTicket(1),
        crate::configure::ReviewerModels {
            models: Vec::new(),
            selected: Some("gone-m".to_owned()),
            notices: vec!["`acme` lists no models; the credential stays.".to_owned()],
        },
    );
    let (buf, _) = overlaid(&login, 80, 24);
    insta::assert_snapshot!(
        "login_reviewer_empty_80x24",
        text_of(&buf, 80, 24).join("\n")
    );
}

#[test]
fn login_reviewer_write_failed_80x24() {
    let (fake, mut login) = key_choices("acme", LoginTicket(1), choices(&["m1"], Some("m1")));
    if let Ok(mut saved) = fake.reviewer_saved.lock() {
        *saved = Err(ConfigureError {
            code: ErrorCode::ConfigInvalid,
            message: "could not save `providers.acme.reviewer_model`: home config is `[]`."
                .to_owned(),
        });
    }
    press(&mut login, &fake, &[Key::Enter]);
    let (buf, _) = overlaid(&login, 80, 24);
    insta::assert_snapshot!(
        "login_reviewer_write_failed_80x24",
        text_of(&buf, 80, 24).join("\n")
    );
}

/// Every drawn row fits `width` columns.
fn assert_fits(buf: &Buffer, width: u16, height: u16) {
    for y in 0..height {
        let row: String = (0..width).map(|x| buf[(x, y)].symbol()).collect();
        assert!(
            crate::format::width(row.trim_end()) <= usize::from(width),
            "{row:?}"
        );
    }
}

/// A preparing view over `acme`.
fn preparing_view() -> (Fake, Login) {
    let fake = fake_with(vec![LoginTarget {
        name: "acme".to_owned(),
        kind: LoginKind::Key,
    }]);
    let mut login = Login::open(&ctx(&fake));
    login.wait_reviewer(LoginTicket(1), "acme".to_owned());
    (fake, login)
}

#[test]
fn preparing_and_choices_draw_in_tiny_areas() {
    for width in 0..6u16 {
        for height in 1..4u16 {
            let (_, preparing) = preparing_view();
            let (buf, _) = overlaid(&preparing, width, height);
            assert_fits(&buf, width, height);
            let (_, choices) = key_choices("acme", LoginTicket(1), choices(&["m1"], None));
            let (buf, _) = overlaid(&choices, width, height);
            assert_fits(&buf, width, height);
            let (_, ended) = key_choices(
                "acme",
                LoginTicket(1),
                crate::configure::ReviewerModels {
                    models: Vec::new(),
                    selected: None,
                    notices: vec!["note".to_owned()],
                },
            );
            let (buf, _) = overlaid(&ended, width, height);
            assert_fits(&buf, width, height);
        }
    }
}

#[test]
fn choices_around_the_visible_capacity_keep_the_focus_drawn() {
    // One target keeps three body rows: two, three and four models
    // exercise below, at and above capacity.
    for count in [2usize, 3, 4] {
        let models: Vec<String> = (0..count).map(|n| format!("m{n}")).collect();
        let answer = crate::configure::ReviewerModels {
            models,
            selected: None,
            notices: Vec::new(),
        };
        let (fake, mut login) = key_choices("acme", LoginTicket(1), answer);
        // Down to the last row: the final row is reachable, not merely drawn.
        for _ in 0..count {
            press(&mut login, &fake, &[Key::Down]);
        }
        let (buf, _) = overlaid(&login, 80, 24);
        let last = format!("m{}", count.saturating_sub(1));
        let screen = text_of(&buf, 80, 24).join("\n");
        assert!(screen.contains(&last), "{count}: {screen}");
        assert_eq!(barred_rows(&buf, 80, 24).len(), 1);
        // A shorter overlay keeps the focused row visible.
        let (short, _) = overlaid(&login, 80, 10);
        let screen = text_of(&short, 80, 10).join("\n");
        assert!(screen.contains(&last), "{count}: {screen}");
    }
}

#[test]
fn a_very_large_model_count_stays_inside_its_rows() {
    let (_, mut login) = key_choices("acme", LoginTicket(1), choices(&["m1"], None));
    if let super::Mode::Choices(choices) = &mut login.mode {
        choices.models = (0..70_000).map(|n| format!("m{n}")).collect();
        choices.cursor = Some(69_999);
    }
    let (buf, targets) = overlaid(&login, 80, 24);
    assert_fits(&buf, 80, 24);
    let screen = text_of(&buf, 80, 24).join("\n");
    assert!(screen.contains("m69999"), "{screen}");
    assert_eq!(barred_rows(&buf, 80, 24).len(), 1);
    assert!(!targets.is_empty());
}

#[test]
fn long_and_wide_ids_stay_inside_while_the_save_keeps_them_whole() {
    let wide = "あ".repeat(60);
    let long = "m".repeat(200);
    let (fake, mut login) = key_choices(
        "acme",
        LoginTicket(1),
        choices(&[&wide, &long, "org/fast", "v:2"], None),
    );
    let (buf, _) = overlaid(&login, 80, 24);
    assert_fits(&buf, 80, 24);
    for _ in 0..4 {
        press(&mut login, &fake, &[Key::Down]);
    }
    press(&mut login, &fake, &[Key::Enter]);
    assert_eq!(
        fake.reviewer_saves(),
        [("acme".to_owned(), "v:2".to_owned())]
    );
    let (fake, mut login) = key_choices("acme", LoginTicket(1), choices(&[&wide, &long], None));
    press(&mut login, &fake, &[Key::Down, Key::Down, Key::Enter]);
    assert_eq!(fake.reviewer_saves(), [("acme".to_owned(), long)]);
}

#[test]
fn every_step_shares_one_height_at_each_width() {
    for width in [48u16, 80, 200] {
        let fake = fake_with(targets());
        let mut panel = Login::open(&ctx(&fake));
        open_key_panel(&mut panel, &fake);
        let browser_wait = waiting(&fake, LoginTicket(1));
        let mut preparing = waiting(&fake, LoginTicket(1));
        preparing.wait_reviewer(LoginTicket(1), "codex".to_owned());
        let mut picker = waiting(&fake, LoginTicket(1));
        picker.wait_reviewer(LoginTicket(1), "codex".to_owned());
        picker.step(
            LoginTicket(1),
            crate::login_worker::LoginStep::Reviewer(Ok(choices(&["m1", "m2"], Some("m1")))),
        );
        let mut done = waiting(&fake, LoginTicket(1));
        done.wait_reviewer(LoginTicket(1), "codex".to_owned());
        done.step(
            LoginTicket(1),
            crate::login_worker::LoginStep::Reviewer(Ok(choices(&["m1"], Some("m1")))),
        );
        press(&mut done, &fake, &[Key::Enter]);
        let refusing = refusing_fake("token expired");
        let mut failed = Login::open(&ctx(&refusing));
        open_key_panel(&mut failed, &refusing);
        typed(&mut failed, &refusing, "sk-x");
        failed.key(&Key::Enter, &ctx(&refusing));
        let heights: Vec<u16> = [&panel, &browser_wait]
            .into_iter()
            .map(|login| {
                let (buf, _) = overlaid(login, width, 24);
                slab_height(&buf, width, 24)
            })
            .collect();
        let (preparing_buf, _) = overlaid(&preparing, width, 24);
        let (picker_buf, _) = overlaid(&picker, width, 24);
        let (done_buf, _) = overlaid(&done, width, 24);
        let (failed_buf, _) = overlaid(&failed, width, 24);
        let mut all = heights;
        all.extend([
            slab_height(&preparing_buf, width, 24),
            slab_height(&picker_buf, width, 24),
            slab_height(&done_buf, width, 24),
            slab_height(&failed_buf, width, 24),
        ]);
        assert!(
            all.windows(2).all(|pair| pair[0] == pair[1]),
            "{width}: {all:?}"
        );
        let runs: Vec<usize> = [&preparing_buf, &picker_buf, &done_buf, &failed_buf]
            .into_iter()
            .map(|buf| edge_run(buf, width, 24))
            .collect();
        assert!(
            runs.windows(2).all(|pair| pair[0] == pair[1]),
            "{width}: {runs:?}"
        );
    }
}

#[test]
fn no_step_shows_the_provider_list_rows() {
    let (_, preparing) = preparing_view();
    let (_, picker) = key_choices("acme", LoginTicket(1), choices(&["m1"], Some("m1")));
    for login in [&preparing, &picker] {
        let screen = drawn(login, 80, 24);
        assert!(!screen.contains("Providers"), "{screen}");
        assert!(!screen.contains("Secrets"), "{screen}");
    }
}

#[test]
fn the_failed_reason_reads_error() {
    let refusing = refusing_fake("token expired");
    let mut login = Login::open(&ctx(&refusing));
    open_key_panel(&mut login, &refusing);
    typed(&mut login, &refusing, "sk-x");
    login.key(&Key::Enter, &ctx(&refusing));
    let (buf, _) = overlaid(&login, 80, 24);
    let error = Role::Error.color();
    let rows = text_of(&buf, 80, 24);
    assert!(
        rows.iter().any(|row| row.contains("Login to acme failed:")),
        "{rows:?}"
    );
    for y in 0..24 {
        let line: String = (0..80).map(|x| buf[(x, y)].symbol()).collect();
        if let Some(start) = line.find("token expired") {
            let start = u16::try_from(start).unwrap_or(u16::MAX);
            let end =
                start.saturating_add(u16::try_from("token expired".len()).unwrap_or(u16::MAX));
            for x in start..end {
                let cell = &buf[(x, y)];
                if cell.symbol() != " " {
                    assert_eq!(cell.fg, error, "cell {x},{y} is not error");
                }
            }
        }
    }
}

#[test]
fn notices_fill_only_the_remaining_budget() {
    // Two models with one notice: at a budget of three the notice draws,
    // and at exactly the models' two rows it is clipped away.
    fn texts(rows: &[(overlay::Row, crate::rows::Join)]) -> Vec<String> {
        rows.iter()
            .map(|(row, _)| {
                row.spans
                    .iter()
                    .map(|span| span.content.to_string())
                    .collect()
            })
            .collect()
    }
    let models = crate::configure::ReviewerModels {
        models: ["m1".to_owned(), "m2".to_owned()].to_vec(),
        selected: None,
        notices: ["note-one".to_owned()].to_vec(),
    };
    let choices = super::Choices::open("acme".to_owned(), models);
    let (rows, _) = super::choice_rows(&choices, 60, 3, 24);
    assert_eq!(texts(&rows), ["  1) m1", "  2) m2", "note-one"]);
    let (rows, _) = super::choice_rows(&choices, 60, 2, 24);
    assert_eq!(
        texts(&rows),
        ["  1) m1", "  2) m2"],
        "at exactly the budget no notice is added"
    );
}

/// The failure head starts its own line: the first failed row breaks,
/// whatever the later rows join with (`docs/tui.md`, "Selection and
/// copy").
#[test]
fn failed_rows_break_first() {
    let rows = super::failed_rows(
        "Login to acme failed: ",
        "token expired while the browser waited",
        ".",
        20,
    );
    assert!(rows.len() > 1, "the reason wraps: {}", rows.len());
    assert_eq!(rows[0].1, crate::rows::Join::Break);
}

/// With no items the failure message is the whole body: no blank row
/// leads it (`docs/tui.md`, "Logging in").
#[test]
fn a_failed_read_starts_the_body_with_the_message() {
    let fake = failed_fake("config.json: not JSON");
    let login = Login::open(&ctx(&fake));
    let (framed, _) = login.list_framed(Rect::new(0, 0, 80, 24));
    let first = framed.body.first().expect("a body row");
    let text: String = first.spans.iter().map(|span| span.content.as_ref()).collect();
    assert!(
        text.contains("config.json: not JSON"),
        "the message leads the body: {text:?}"
    );
}
