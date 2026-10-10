//! Tests for `/login`: the rows, the hidden key field and its store
//! steps, and that the key is never drawn (`docs/tui.md`, "Logging in").

use std::path::Path;

use contract::ErrorCode;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::{Act, Ctx, Login};
use crate::ConfigureError;
use crate::configure::{LoginKind, LoginTarget, Stored};
use crate::configure_fake::Fake;
use crate::keys::{Edit, Key};
use crate::login_worker::LoginTicket;
use crate::swapped::{Spot, render};

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

/// A call's context over `fake`, 24 rows tall, in `/w`.
fn ctx(fake: &Fake) -> Ctx<'_> {
    Ctx {
        seam: fake,
        workspace: Path::new("/w"),
        height: 24,
        width: 80,
        usage: None,
    }
}

/// The view over `targets()` with the row at `index` selected.
fn at(fake: &Fake, index: usize) -> Login {
    let mut login = Login::open(&ctx(fake));
    login.list.select(index, login.items.len(), 20);
    login
}

/// Presses each key in turn.
fn press(login: &mut Login, fake: &Fake, keys: &[Key]) -> Vec<Act> {
    keys.iter().map(|key| login.key(key, &ctx(fake))).collect()
}

/// Types `text` into the open panel.
fn typed(login: &mut Login, fake: &Fake, text: &str) {
    let keys: Vec<Key> = text.chars().map(Key::Char).collect();
    press(login, fake, &keys);
}

/// Opens the key panel over `acme`: the second row.
fn open_key_panel(login: &mut Login, fake: &Fake) {
    press(login, fake, &[Key::Down, Key::Enter]);
}

/// Opens the value panel over the secret: the last row.
fn open_secret_panel(login: &mut Login, fake: &Fake) {
    press(
        login,
        fake,
        &[Key::Down, Key::Down, Key::Down, Key::Down, Key::Enter],
    );
}

/// `login` drawn at `width` x `height`, one string per row.
fn drawn(login: &Login, width: u16, height: u16) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    render(
        &login.frame(usize::from(width)),
        area,
        &mut buf,
        &mut Vec::new(),
    );
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
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
    assert!(screen.contains("  acme  key"), "{screen}");
    assert!(screen.contains("  codex  browser"), "{screen}");
    assert!(screen.contains("Secrets"), "{screen}");
    assert!(screen.contains("  acme.api_key"), "{screen}");
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
fn enter_on_a_heading_or_the_note_does_nothing() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    assert!(matches!(login.key(&Key::Enter, &ctx(&fake)), Act::Stay));
    assert!(!panel_open(&login));
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
    assert_eq!(login.frame(80).field, None);
    assert!(fake.stores().is_empty());
}

#[test]
fn esc_on_the_list_closes() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    assert!(matches!(login.key(&Key::Esc, &ctx(&fake)), Act::Close));
}

#[test]
fn a_click_selects_a_row_and_the_x_closes() {
    let fake = fake_with(targets());
    let mut login = Login::open(&ctx(&fake));
    assert!(matches!(login.click(Spot::Row(4), &ctx(&fake)), Act::Stay));
    assert_eq!(login.list.selected(), 4);
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
    let held = std::sync::Arc::new(crate::configure_fake::FakeLogin::new());
    login.started(crate::login_worker::LoginWorker::new(
        LoginTicket(1),
        held.clone() as std::sync::Arc<dyn crate::configure::BrowserLogin>,
    ));
    assert!(matches!(login.key(&Key::Esc, &ctx(&fake)), Act::Stay));
    assert_eq!(login.frame(80).below, Vec::<String>::new());
    assert_eq!(held.cancels(), 1);
}

#[test]
fn dropping_a_waiting_view_cancels_once() {
    let fake = fake_with(targets());
    let held = std::sync::Arc::new(crate::configure_fake::FakeLogin::new());
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
    let held = std::sync::Arc::new(crate::configure_fake::FakeLogin::new());
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
