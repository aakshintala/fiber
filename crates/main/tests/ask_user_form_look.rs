//! Binary-level tests of the `ask_user` question form (`docs/tui.md`,
//! "A question form"): the real binary in a pseudo-terminal draws one
//! three-question form as the doc draws it, answers it into the turn's
//! card, and declines it with Esc.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test helpers; a failure is the test's; a live test prints its outcome"
)]

mod support;

use serde_json::json;
use support::pty::{Colour, Grid, Run};
use support::{Setup, TRUECOLOUR, write_json};

/// The question-form tint in truecolour.
const APPROVAL_BG: Colour = Colour::Rgb(26, 26, 34);
/// `secondary` in the dark theme: the stripe, the title, the marks.
const SECONDARY: Colour = Colour::Rgb(179, 157, 219);
/// `accent` in the dark theme: answered tabs, answer headers.
const ACCENT: Colour = Colour::Rgb(110, 170, 254);
/// `rule` in the dark theme: the "Chat about this" button, the card rule.
const RULE: Colour = Colour::Rgb(58, 58, 74);

/// The scripted turn: one `ask_user` call with three questions, then the
/// closing text.
fn form_script() -> serde_json::Value {
    json!({"steps": [
        {"tool_calls": [{"name": "ask_user", "arguments": {"questions": [
            {"header": "Scope", "question": "Which crates should the change touch?",
             "multiSelect": true, "options": [
                {"label": "hub", "description": "the session host"},
                {"label": "tui", "description": "the terminal"}]},
            {"header": "Tests", "question": "Which tests cover it?",
             "options": [{"label": "unit"}, {"label": "pty"}]},
            {"header": "Docs", "question": "Which docs change?"},
        ]}}]},
        {"text": ["Thanks."]},
    ]})
}

/// A setup whose scripted model asks the three-question form.
fn form_setup() -> Setup {
    let setup = Setup::new();
    write_json(&setup.workspace().join("s.json"), &form_script());
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 1000}}),
    );
    setup
}

/// Spawns `fiber` on a 160 by 48 terminal in truecolour.
fn terminal(setup: &Setup) -> Run {
    Run::spawn(setup, 160, 48, &[], &TRUECOLOUR)
}

/// The first row holding `needle`, or a panic naming it.
fn row_with(grid: &Grid, needle: &str) -> u16 {
    u16::try_from(
        grid.rows
            .iter()
            .position(|row| row.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} in\n{}", grid.contents)),
    )
    .unwrap_or(u16::MAX)
}

/// The column where `needle` starts on `y`, matching the cells one by
/// one, so a wide cell before it changes nothing.
fn col_at(grid: &Grid, y: u16, needle: &str) -> u16 {
    let want: Vec<char> = needle.chars().collect();
    for x in 0..160u16 {
        let mut ok = !want.is_empty();
        for (dx, want) in want.iter().enumerate() {
            let xi = x.saturating_add(u16::try_from(dx).unwrap_or(u16::MAX));
            if xi >= 160 || grid.cell(xi, y).symbol != want.to_string() {
                ok = false;
                break;
            }
        }
        if ok {
            return x;
        }
    }
    panic!("{needle:?} on row {y}")
}

/// The output replayed through a `vt100` screen of its own, keeping the
/// inverse and underline the support grid drops.
fn parsed(output: &[u8]) -> vt100::Parser {
    let mut parser = vt100::Parser::new(48, 160, 0);
    parser.process(output);
    parser
}

/// Every cell of `needle` at `(x, y)` is reversed on `screen`.
#[track_caller]
fn assert_inverse(screen: &vt100::Screen, y: u16, x: u16, needle: &str) {
    for (dx, ch) in needle.chars().enumerate() {
        let dx = u16::try_from(dx).unwrap_or(u16::MAX);
        let cell = screen
            .cell(y, x.saturating_add(dx))
            .unwrap_or_else(|| panic!("cell ({}, {y}) for {needle:?}", x + dx));
        assert_eq!(cell.contents(), ch.to_string(), "cell ({}, {y})", x + dx);
        assert!(cell.inverse(), "cell ({}, {y}) not reversed", x + dx);
    }
}

/// No cell of `needle` at `(x, y)` is reversed on `screen`.
#[track_caller]
fn assert_not_inverse(screen: &vt100::Screen, y: u16, x: u16, needle: &str) {
    for (dx, ch) in needle.chars().enumerate() {
        let dx = u16::try_from(dx).unwrap_or(u16::MAX);
        let cell = screen
            .cell(y, x.saturating_add(dx))
            .unwrap_or_else(|| panic!("cell ({}, {y}) for {needle:?}", x + dx));
        assert_eq!(cell.contents(), ch.to_string(), "cell ({}, {y})", x + dx);
        assert!(!cell.inverse(), "cell ({}, {y}) reversed", x + dx);
    }
}

/// Every cell of `needle` at `(x, y)` is underlined on `screen`.
#[track_caller]
fn assert_underlined(screen: &vt100::Screen, y: u16, x: u16, needle: &str) {
    for (dx, ch) in needle.chars().enumerate() {
        let dx = u16::try_from(dx).unwrap_or(u16::MAX);
        let cell = screen
            .cell(y, x.saturating_add(dx))
            .unwrap_or_else(|| panic!("cell ({}, {y}) for {needle:?}", x + dx));
        assert_eq!(cell.contents(), ch.to_string(), "cell ({}, {y})", x + dx);
        assert!(cell.underline(), "cell ({}, {y}) not underlined", x + dx);
    }
}

/// Quits the run: two interrupts and Enter, as the journeys quit.
fn quit(mut run: Run) {
    run.write(b"\x03\x03\r");
    run.wait_screen("the primary screen with the resume line", |grid| {
        !grid.alternate_screen && !grid.hide_cursor && grid.contents.contains("fiber resume")
    });
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}

/// Opens the form and waits for its named title, returning the run.
fn opened() -> (Setup, Run) {
    let setup = form_setup();
    let mut run = terminal(&setup);
    run.ready();
    run.write(b"run it\r");
    // The session shows its name, the first prompt: the first frame can
    // still carry the session id, so the wait holds until the named
    // header draws.
    run.wait_screen("the question form", |grid| {
        grid.contents
            .contains("Question from run it · 3 questions, any can be skipped")
    });
    (setup, run)
}

#[test]
fn an_ask_user_form_draws_the_doc_look_and_answers_into_the_card() {
    let (_setup, mut run) = opened();
    let grid = run.wait_screen("the first question", |grid| {
        grid.contents
            .contains("Which crates should the change touch?")
    });
    // The title row, then a blank row, the tab row, a blank row, the
    // question, "choose any", a blank row, the options, the words row,
    // "Next →", a blank row and the hint.
    let title = row_with(&grid, "Question from run it");
    let left = "Question from run it · 3 questions, any can be skipped";
    let right = "Esc: chat about this";
    assert!(
        !grid.rows[usize::from(title)].contains(" · 1 of"),
        "{}",
        grid.contents
    );
    // The tint's last cell is the hint's last one: the title is padded to
    // it with spaces.
    let end = (0..160)
        .rev()
        .find(|x| grid.cell(*x, title).bg == APPROVAL_BG)
        .expect("the tinted run");
    let pad = end + 1 - 2 - 54 - 20;
    let want = format!("▌ {left}{}{right}", " ".repeat(usize::from(pad)));
    assert_eq!(
        grid.rows[usize::from(title)].trim_end(),
        want,
        "{}",
        grid.contents
    );
    let rows = [
        "▌",
        "←  ☐ Scope    ☐ Tests    ☐ Docs    ✔ Submit  →",
        "▌",
        "Which crates should the change touch?",
        "choose any",
        "▌",
        "▸ 1 [ ] hub  the session host",
        "  2 [ ] tui  the terminal",
        "  3 ✎ Type an answer, alone or with the options",
        "  Next →",
        "▌",
        "←→ question · ↑↓ choose · Enter chooses and moves on · Space toggles · type to answer in words",
    ];
    for (dy, want) in rows.iter().enumerate() {
        let y = title + 1 + u16::try_from(dy).unwrap_or(u16::MAX);
        let row = grid.rows[usize::from(y)].trim_end();
        let row = row.strip_prefix("▌ ").unwrap_or(row);
        assert_eq!(row, *want, "row {y} in\n{}", grid.contents);
    }
    // Nothing is chosen yet: every option mark reads "[ ]" and none "[x]".
    assert!(grid.contents.contains("[ ]"), "{}", grid.contents);
    assert!(!grid.contents.contains("[x]"), "{}", grid.contents);
    // Every row's first cell is the stripe in `secondary`, every cell up
    // to the tint end has the form's tint, and the edges draw above and
    // below the thirteen rows.
    for y in title..title + 13 {
        for x in 0..end + 1 {
            assert_eq!(grid.cell(x, y).bg, APPROVAL_BG, "({x}, {y})");
        }
        assert_eq!(grid.cell(0, y).symbol.as_str(), "▌", "row {y}");
        assert_eq!(grid.cell(0, y).fg, SECONDARY, "row {y}");
    }
    for x in 0..end + 1 {
        assert_eq!(grid.cell(x, title - 1).symbol.as_str(), "▄", "top {x}");
        assert_eq!(grid.cell(x, title + 13).symbol.as_str(), "▀", "bottom {x}");
    }
    // The title: "Question" bold in `secondary`, the count in
    // `secondary`, the hint dim at the tint's end.
    let head = col_at(&grid, title, "Question");
    for dx in 0..8u16 {
        let cell = grid.cell(head + dx, title);
        assert_eq!(cell.fg, SECONDARY, "col {dx}");
        assert!(cell.bold, "col {dx}");
    }
    let count = col_at(&grid, title, "3 questions");
    for dx in [0, 5, 11] {
        let cell = grid.cell(count + dx, title);
        assert_eq!(cell.fg, SECONDARY, "col {dx}");
        assert!(!cell.bold, "col {dx}");
    }
    let esc = col_at(&grid, title, "Esc: chat about this");
    assert_eq!(esc + 20, end + 1);
    for dx in 0..20u16 {
        assert!(grid.cell(esc + dx, title).dim, "col {dx}");
    }
    // The tab row: "← " dim, the current tab reversed, answered tabs in
    // `accent` (none is answered, so none is), " →" dim.
    let tabs = title + 2;
    let left_arrow = col_at(&grid, tabs, "← ");
    for dx in 0..2u16 {
        assert!(grid.cell(left_arrow + dx, tabs).dim, "col {dx}");
    }
    let right_arrow = col_at(&grid, tabs, " →");
    for dx in 0..2u16 {
        assert!(grid.cell(right_arrow + dx, tabs).dim, "col {dx}");
    }
    let screen = parsed(&run.output());
    let screen = screen.screen();
    let current = col_at(&grid, tabs, " ☐ Scope ");
    assert_inverse(screen, tabs, current, " ☐ Scope ");
    assert_not_inverse(screen, tabs, col_at(&grid, tabs, " ☐ Tests "), " ☐ Tests ");
    assert_not_inverse(
        screen,
        tabs,
        col_at(&grid, tabs, " ✔ Submit "),
        " ✔ Submit ",
    );
    for needle in [" ☐ Tests ", " ☐ Docs ", " ✔ Submit "] {
        let x = col_at(&grid, tabs, needle);
        for (dx, _) in needle.chars().enumerate() {
            let cell = grid.cell(x + dx as u16, tabs);
            assert_eq!(cell.fg, Colour::Default, "{needle} col {dx}");
        }
    }
    // The cursor's option: "▸ " in `secondary", its number dim, "[ ]"
    // dim, the label underlined under the cursor, the description dim.
    let option = title + 7;
    let pointed = col_at(&grid, option, "▸");
    assert_eq!(grid.cell(pointed, option).fg, SECONDARY);
    let number = col_at(&grid, option, "1 [ ] hub");
    assert!(grid.cell(number, option).dim);
    let mark = col_at(&grid, option, "[ ] hub");
    for dx in [0, 2] {
        let cell = grid.cell(mark + dx, option);
        assert_eq!(cell.fg, Colour::Default, "col {dx}");
    }
    let label = col_at(&grid, option, "hub");
    assert_underlined(screen, option, label, "hub");
    assert!(!grid.cell(label, option).bold);
    let about = col_at(&grid, option, "the session host");
    for dx in [0, 10] {
        assert!(grid.cell(about + dx, option).dim, "col {dx}");
    }
    // "Next →" in `secondary`, the hint dim.
    let next = title + 10;
    let finishing = col_at(&grid, next, "Next →");
    for dx in [0, 5] {
        assert_eq!(grid.cell(finishing + dx, next).fg, SECONDARY, "col {dx}");
    }
    let hint = title + 12;
    let hint_at = col_at(&grid, hint, "←→ question");
    for dx in [0, 20, 60] {
        assert!(grid.cell(hint_at + dx, hint).dim, "col {dx}");
    }
    // A digit chooses that option: "2" toggles the terminal's option and
    // the label goes bold with "[x]" in `secondary`.
    run.write(b"2");
    let grid = run.wait_screen("the toggled option", |grid| {
        grid.contents.contains("2 [x] tui")
    });
    let toggled = row_with(&grid, "2 [x] tui");
    let marked = col_at(&grid, toggled, "[x] tui");
    for dx in [0, 1, 2] {
        assert_eq!(grid.cell(marked + dx, toggled).fg, SECONDARY, "col {dx}");
    }
    let picked = col_at(&grid, toggled, "tui");
    for dx in [0, 1, 2] {
        assert!(grid.cell(picked + dx, toggled).bold, "col {dx}");
    }
    // → moves to the Tests tab; Tab moves to the Docs words row, empty:
    // the placeholder is dim with its first character reversed.
    run.write(b"\x1b[C");
    run.wait_screen("the Tests tab", |grid| {
        grid.contents.contains("Which tests cover it?")
    });
    run.write(b"\t");
    let grid = run.wait_screen("the Docs words row", |grid| {
        grid.contents.contains("Which docs change?") && grid.contents.contains("Type an answer")
    });
    let words = row_with(&grid, "Type an answer");
    let placeholder = col_at(&grid, words, "Type an answer");
    for dx in 0..14u16 {
        assert!(grid.cell(placeholder + dx, words).dim, "col {dx}");
    }
    let screen = parsed(&run.output());
    let screen = screen.screen();
    assert_inverse(screen, words, placeholder, "T");
    assert_not_inverse(screen, words, placeholder + 1, "y");
    // Typing replaces the whole placeholder: "2" draws bold, dim gone.
    run.write(b"2");
    let grid = run.wait_screen("the typed answer", |grid| {
        grid.rows.iter().any(|row| row.contains("✎ 2"))
    });
    let typed = row_with(&grid, "✎ 2");
    assert!(
        !grid.rows[usize::from(typed)].contains("Type an answer"),
        "{}",
        grid.contents
    );
    let two = col_at(&grid, typed, "2");
    assert!(grid.cell(two, typed).bold);
    assert!(!grid.cell(two, typed).dim);
    // Tab moves to Submit: " Submit " reversed, " Chat about this " on
    // the `rule` tint, "Review" bold, each answer on its padded header in
    // `accent` with "skipped" dim for the unanswered Tests.
    run.write(b"\t");
    let grid = run.wait_screen("the Submit tab", |grid| grid.contents.contains("Review"));
    let submit = row_with(&grid, " Submit ");
    let screen = parsed(&run.output());
    let screen = screen.screen();
    assert_inverse(
        screen,
        submit,
        col_at(&grid, submit, " Submit "),
        " Submit ",
    );
    let chatting = col_at(&grid, submit, " Chat about this ");
    for dx in 0..17u16 {
        assert_eq!(grid.cell(chatting + dx, submit).bg, RULE, "col {dx}");
    }
    let review = row_with(&grid, "Review");
    let bolded = col_at(&grid, review, "Review");
    for dx in 0..6u16 {
        assert!(grid.cell(bolded + dx, review).bold, "col {dx}");
    }
    let scope = row_with(&grid, "Scope");
    let header = col_at(&grid, scope, "Scope");
    for dx in 0..13u16 {
        assert_eq!(grid.cell(header + dx, scope).fg, ACCENT, "col {dx}");
    }
    let answered = col_at(&grid, scope, "tui");
    assert!(grid.cell(answered, scope).bold);
    let tests = row_with(&grid, "Tests");
    let skipped = col_at(&grid, tests, "skipped");
    assert!(grid.cell(skipped, tests).dim);
    // Enter answers: the card reads "you answered" bold in `accent",
    // the clock shape dim, the rule in `rule` to the card's inner edge,
    // each header padded to 13 cells in `accent` with the answer bold or
    // "skipped" dim.
    run.write(b"\r");
    let grid = run.wait_screen("the answered card", |grid| {
        grid.contents.contains("you answered")
    });
    let rule = row_with(&grid, "you answered");
    let you = col_at(&grid, rule, "you answered");
    for dx in 0..12u16 {
        let cell = grid.cell(you + dx, rule);
        assert_eq!(cell.fg, ACCENT, "col {dx}");
        assert!(cell.bold, "col {dx}");
    }
    // " · HH:MM": the shape, never the clock.
    for (dx, want) in [(12, " "), (13, "·"), (14, " "), (17, ":"), (20, " ")] {
        assert_eq!(grid.cell(you + dx, rule).symbol.as_str(), want, "col {dx}");
        assert!(grid.cell(you + dx, rule).dim, "col {dx}");
    }
    for dx in [15, 16, 18, 19] {
        assert!(
            grid.cell(you + dx, rule)
                .symbol
                .chars()
                .all(|ch| ch.is_ascii_digit()),
            "col {dx}"
        );
        assert!(grid.cell(you + dx, rule).dim, "col {dx}");
    }
    let dashes = (21u16..160)
        .take_while(|dx| grid.cell(you + *dx, rule).symbol.as_str() == "─")
        .count();
    assert!(dashes > 10, "the rule fills the card");
    for dx in 21..21 + dashes as u16 {
        assert_eq!(grid.cell(you + dx, rule).fg, RULE, "col {dx}");
    }
    let scope = row_with(&grid, "Scope");
    let header = col_at(&grid, scope, "Scope");
    for dx in 0..13u16 {
        assert_eq!(grid.cell(header + dx, scope).fg, ACCENT, "col {dx}");
    }
    assert_eq!(
        &grid.rows[usize::from(scope)][usize::from(header)..usize::from(header) + 13],
        "Scope        "
    );
    let answered = col_at(&grid, scope, "tui");
    assert!(grid.cell(answered, scope).bold);
    let tests = row_with(&grid, "Tests");
    assert_eq!(
        &grid.rows[usize::from(tests)][usize::from(header)..usize::from(header) + 13],
        "Tests        "
    );
    let skipped = col_at(&grid, tests, "skipped");
    assert!(grid.cell(skipped, tests).dim);
    let docs = row_with(&grid, "Docs");
    assert_eq!(
        &grid.rows[usize::from(docs)][usize::from(header)..usize::from(header) + 13],
        "Docs         "
    );
    let quoted = col_at(&grid, docs, "\"2\"");
    for dx in 0..3u16 {
        assert!(grid.cell(quoted + dx, docs).bold, "col {dx}");
    }
    quit(run);
}

#[test]
fn esc_declines_the_form_and_ends_the_turn() {
    let (_setup, mut run) = opened();
    run.write(b"\x1b[27u");
    // The ledger reads "declined" and the panel is gone.
    let grid = run.wait_screen("the declined turn", |grid| {
        grid.contents.contains("declined") && !grid.contents.contains("Question from")
    });
    assert!(grid.contents.contains("declined"), "{}", grid.contents);
    quit(run);
}
