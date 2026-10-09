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
        tail: 0,
        decoration: true,
        links: Vec::new(),
        scopes: Vec::new(),
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

#[test]
fn open_scope_records_the_target_on_rows_inside_it() {
    use crate::app::Target;
    let mut rows = Rows::default();
    rows.push(row("top"));
    assert!(rows.open_scope(Target::Group(1), true));
    rows.push(row("ledger"));
    assert!(!rows.open_scope(Target::Call(2), false));
    rows.push(row("detail"));
    rows.end_scope();
    rows.push(row("more"));
    rows.end_scope();
    rows.push(row("bottom"));
    let (_, texts) = rows.into_parts();
    let scopes: Vec<Vec<Target>> = texts.into_iter().map(|text| text.scopes).collect();
    assert_eq!(
        scopes,
        [
            Vec::new(),
            vec![Target::Group(1)],
            vec![Target::Group(1), Target::Call(2)],
            vec![Target::Group(1)],
            Vec::new(),
        ]
    );
}

#[test]
fn open_scope_returns_open_on_a_normal_draw_and_true_on_all_open() {
    use crate::app::Target;
    let mut shown = Rows::default();
    assert!(shown.open_scope(Target::Group(1), true));
    assert!(!shown.open_scope(Target::Call(2), false));
    let mut all = Rows::all_open();
    assert!(all.open_scope(Target::Group(1), true));
    assert!(all.open_scope(Target::Call(2), false));
}

#[test]
fn end_scope_without_a_scope_does_nothing() {
    let mut rows = Rows::default();
    rows.end_scope();
    rows.push(row("a"));
    let (_, texts) = rows.into_parts();
    assert_eq!(texts, vec![RowText::plain()]);
}

#[test]
fn tail_cells_are_not_text() {
    let plain = RowText::plain();
    assert_eq!(plain.tail, 0);
}
