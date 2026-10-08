use ratatui::text::Line;

use super::*;

fn row(text: &str) -> Row {
    (Line::raw(text.to_owned()), None)
}

#[test]
fn a_plain_push_is_a_break_with_no_skip() {
    let mut rows = Rows::default();
    rows.push(row("a"));
    let (_, texts) = rows.into_parts();
    assert_eq!(texts, vec![RowText::plain()]);
    let plain = RowText::plain();
    assert_eq!(plain.join, Join::Break);
    assert_eq!(plain.skip, 0);
    assert!(!plain.decoration);
}

#[test]
fn push_text_keeps_its_text() {
    let mut rows = Rows::default();
    let text = RowText {
        join: Join::WrapSpace,
        skip: 3,
        decoration: true,
    };
    rows.push_text(row("a"), text.clone());
    assert_eq!(rows.len(), 1);
    let (lines, texts) = rows.into_parts();
    assert_eq!(lines.len(), 1);
    assert_eq!(texts, vec![text]);
}

#[test]
fn rows_and_texts_stay_aligned_through_extend() {
    let mut rows = Rows::default();
    let wrapped = RowText {
        join: Join::Wrap,
        ..RowText::plain()
    };
    rows.push_text(row("a"), wrapped.clone());
    rows.extend([row("b"), row("c")]);
    rows.push(row("d"));
    let (lines, texts) = rows.into_parts();
    assert_eq!(lines.len(), texts.len());
    assert_eq!(
        texts,
        vec![
            wrapped,
            RowText::plain(),
            RowText::plain(),
            RowText::plain()
        ]
    );
    let shown: Vec<String> = lines.iter().map(|(line, _)| line.to_string()).collect();
    assert_eq!(shown, ["a", "b", "c", "d"]);
}
