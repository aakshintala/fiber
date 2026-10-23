use super::*;
use contract::events::OfferedKind;
use fakes::MUST_SUCCEED_WITHIN;

fn item() -> OfferedItem {
    serde_json::from_value(serde_json::json!({"kind": "hook", "name": "db", "hash": "h", "required": false, "summary": "", "files": ["tools/db.sh", "tools/other.sh"]})).unwrap()
}

fn text(source: &Source, width: u16) -> Vec<String> {
    source
        .rows(&item(), width)
        .into_iter()
        .skip(2)
        .map(|row| {
            assert!(row.clip);
            assert!(row.spots.is_empty());
            assert_eq!(row.height(width), 1);
            let mut buffer =
                ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, width, 1));
            buffer.set_line(0, 0, &row.line, width);
            for (column, cell) in buffer.content.iter().enumerate() {
                assert!(column + crate::format::width(cell.symbol()) <= usize::from(width));
            }
            row.line.to_string()
        })
        .collect()
}

fn asked() -> Source {
    let mut source = Source::new(0);
    source.ask(
        "c1",
        &SessionId("s".into()),
        &RequestId("r".into()),
        "tools/db.sh",
    );
    source
}

#[test]
fn the_request_line_names_the_session_offer_and_exact_path() {
    let mut source = Source::new(0);
    let line = source.ask(
        "c_5d20",
        &SessionId("s_4c1d".into()),
        &RequestId("r_2c01".into()),
        "tools/db.sh",
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&line).unwrap(),
        serde_json::json!({"id":"c_5d20","command":"offered_file","session_id":"s_4c1d","args":{"request_id":"r_2c01","path":"tools/db.sh"}})
    );
    assert_eq!(text(&source, 80), ["Loading…"]);
    let rows = source.rows(&item(), 80);
    assert_eq!(rows[0].line.to_string(), "Hook db · tools/db.sh · 1/2");
    assert_eq!(item().kind, OfferedKind::Hook);
}

#[test]
fn answers_and_refusals_match_only_the_asked_id() {
    let mut source = asked();
    assert!(!source.answered("old", "old".into()));
    assert!(!source.refused("old", &ErrorCode::IoFailed, "old"));
    assert_eq!(text(&source, 80), ["Loading…"]);
    assert!(source.answered("c1", "source".into()));
    assert_eq!(text(&source, 80), ["source"]);
    assert!(source.refused("c1", &ErrorCode::InvalidArguments, "refused"));
    assert_eq!(text(&source, 80), ["refused"]);
}

#[test]
fn duplicate_command_changes_nothing() {
    let mut source = asked();
    assert!(!source.refused("c1", &ErrorCode::DuplicateCommand, "duplicate"));
    assert_eq!(text(&source, 80), ["Loading…"]);
}

#[test]
fn steps_clamp_reset_scroll_and_forget_the_previous_answer() {
    let mut source = asked();
    source.on_key(&Key::PageDown, 100, 10);
    assert_eq!(source.top(), 9);
    assert_eq!(source.step(false, 2), None);
    assert_eq!(source.step(true, 2), Some(1));
    assert_eq!(source.top(), 0);
    assert_eq!(text(&source, 80), ["Loading…"]);
    assert!(!source.answered("c1", "late".into()));
    assert_eq!(source.step(true, 2), None);
    assert_eq!(source.step(false, 2), Some(0));
    assert_eq!(source.step(false, 0), None);
}

#[test]
fn scroll_keys_clamp_at_both_edges() {
    let mut source = asked();
    source.on_key(&Key::Up, 20, 5);
    assert_eq!(source.top(), 0);
    source.on_key(&Key::Down, 20, 5);
    assert_eq!(source.top(), 1);
    source.on_key(&Key::PageUp, 20, 5);
    assert_eq!(source.top(), 0);
    for _ in 0..4 {
        source.on_key(&Key::PageDown, 20, 5);
    }
    assert_eq!(source.top(), 15);
    source.on_key(&Key::Down, 20, 5);
    assert_eq!(source.top(), 15);
    source.on_key(&Key::Up, 20, 5);
    assert_eq!(source.top(), 14);
    source.on_key(&Key::Down, 20, 5);
    assert_eq!(source.top(), 15);
    source.on_key(&Key::PageUp, 20, 5);
    assert_eq!(source.top(), 11);
    source.on_key(&Key::Down, 1, 5);
    assert_eq!(source.top(), 0);
    source.on_key(&Key::PageDown, 3, 0);
    assert_eq!(source.top(), 1);
}

#[test]
fn empty_tabs_controls_crlf_and_wide_text_keep_their_cells() {
    let mut source = asked();
    for (given, expected) in [
        ("", vec!["(empty file)"]),
        (
            "\tX\na\tX\nabc\tX\nabcd\tX",
            vec!["    X", "a   X", "abc X", "abcd    X"],
        ),
        ("a\u{1b}b\u{7}c", vec!["a b c"]),
        ("one\r\ntwo\r\n", vec!["one", "two"]),
    ] {
        source.answered("c1", given.into());
        assert_eq!(text(&source, 80), expected);
    }
    source.answered("c1", "123界x".into());
    assert_eq!(text(&source, 4), ["123", "界x"]);
    source.answered("c1", "界x".into());
    assert_eq!(text(&source, 1), ["界", "x"]);
}

#[test]
fn wrapping_keeps_characters_at_and_around_the_right_edge() {
    assert_eq!(wrap("abc", 4), ["abc"]);
    assert_eq!(wrap("abcd", 4), ["abcd"]);
    assert_eq!(wrap("abcde", 4), ["abcd", "e"]);
    assert_eq!(wrap("ab界", 4), ["ab界"]);
    assert_eq!(wrap("界", 2), ["界"]);
}

#[test]
fn width_zero_and_one_consume_each_character_without_hanging() {
    let source = asked();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut source = source;
        source.answered("c1", "界x".into());
        tx.send((text(&source, 0), text(&source, 1))).unwrap();
    });
    let rows =
        fakes::Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&rx, "wrap source at zero width");
    assert_eq!(rows.0, ["界", "x"]);
    assert_eq!(rows.1, ["界", "x"]);
}

#[test]
fn every_other_key_is_swallowed_without_scrolling() {
    let mut source = asked();
    for key in [
        Key::Char('x'),
        Key::Backspace,
        Key::Enter,
        Key::Esc,
        Key::CtrlC,
        Key::CtrlO,
        Key::End,
        Key::AltA,
        Key::Tab,
        Key::BackTab,
        Key::F1,
        Key::CtrlG,
        Key::CtrlR,
        Key::CtrlF,
        Key::AltUp,
        Key::AltDown,
        Key::AltX,
        Key::AltP,
        Key::AltR,
        Key::AltDigit(1),
        Key::CtrlV,
        Key::CtrlL,
    ] {
        source.on_key(&key, 20, 5);
        assert_eq!(source.top(), 0, "{key:?}");
    }
}
