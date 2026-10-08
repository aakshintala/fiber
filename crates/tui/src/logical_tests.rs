use ratatui::text::Line;

use super::*;

/// A row of `text` joining by `join`, skipping `skip` cells.
fn row(text: &str, join: Join, skip: u16) -> (Row, RowText) {
    (
        (Line::raw(text.to_owned()), None),
        RowText {
            join,
            skip,
            ..RowText::plain()
        },
    )
}

/// The logical lines' texts of `rows`.
fn texts(rows: Vec<(Row, RowText)>) -> Vec<String> {
    let (rows, texts): (Vec<Row>, Vec<RowText>) = rows.into_iter().unzip();
    logical(&rows, &texts)
        .into_iter()
        .map(|line| line.text)
        .collect()
}

#[test]
fn soft_wraps_join_with_their_space() {
    let got = texts(vec![
        row("hello", Join::Break, 0),
        row("world", Join::WrapSpace, 0),
    ]);
    assert_eq!(got, ["hello world"]);
}

#[test]
fn character_breaks_join_with_nothing() {
    let got = texts(vec![
        row("abcde", Join::Break, 0),
        row("fgh", Join::Wrap, 0),
    ]);
    assert_eq!(got, ["abcdefgh"]);
}

#[test]
fn a_breaking_row_starts_a_line() {
    let got = texts(vec![
        row("one", Join::Break, 0),
        row("two", Join::Break, 0),
        row("", Join::Break, 0),
        row("three", Join::Break, 0),
    ]);
    assert_eq!(got, ["one", "two", "", "three"]);
}

#[test]
fn decoration_rows_add_no_text() {
    let (header, mut text) = row("rust        copy", Join::Break, 0);
    text.decoration = true;
    let got = texts(vec![
        row("before", Join::Break, 0),
        (header, text),
        row("1 │ code", Join::Break, 4),
    ]);
    assert_eq!(got, ["before", "code"]);
}

#[test]
fn skip_cells_are_not_text() {
    let got = texts(vec![
        row("│ • item one", Join::Break, 2),
        row("│   two", Join::WrapSpace, 4),
        row(" 世 x", Join::Break, 3),
    ]);
    assert_eq!(got, ["• item one two", " x"]);
}

#[test]
fn trailing_spaces_are_trimmed() {
    let got = texts(vec![
        row(" hello   ", Join::Break, 1),
        row(" world   ", Join::WrapSpace, 1),
    ]);
    assert_eq!(got, ["hello world"]);
    assert_eq!(
        line_text(&Line::raw("  ab  "), &RowText::plain()),
        (0, "  ab".to_owned())
    );
    let skip = RowText {
        skip: 2,
        ..RowText::plain()
    };
    assert_eq!(line_text(&Line::raw("│ ab  "), &skip), (4, "ab".to_owned()));
}

#[test]
fn every_char_maps_back_to_its_row_or_to_a_joining_space() {
    let rows = vec![
        row("│ ab", Join::Break, 2),
        row("│ cd", Join::WrapSpace, 2),
        row("│ e", Join::Wrap, 2),
    ];
    let (rows, texts): (Vec<Row>, Vec<RowText>) = rows.into_iter().unzip();
    let lines = logical(&rows, &texts);
    let [line] = lines.as_slice() else {
        panic!("one logical line: {lines:?}");
    };
    assert_eq!(line.text, "ab cde");
    assert_eq!(line.from.len(), line.text.chars().count());
    for (ch, from) in line.text.chars().zip(&line.from) {
        match from {
            Some((at, byte)) => {
                let (row, _) = rows.get(*at).expect("a row");
                let drawn = row.to_string();
                assert_eq!(
                    drawn.get(*byte..).and_then(|rest| rest.chars().next()),
                    Some(ch)
                );
            }
            None => assert_eq!(ch, ' ', "only a joining space has no row"),
        }
    }
    assert_eq!(
        line.from,
        [
            Some((0, 4)),
            Some((0, 5)),
            None,
            Some((1, 4)),
            Some((1, 5)),
            Some((2, 4))
        ]
    );
}
