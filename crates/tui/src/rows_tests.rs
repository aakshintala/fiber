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

#[test]
fn on_surface_tints_and_edges_the_rows_from_its_start() {
    // Rows from `from` sit on the surface tint with half-block edges
    // (`docs/tui.md`, "Look").
    let mut rows = Rows::default();
    rows.push(row("a"));
    rows.push(row("b"));
    rows.push(row("c"));
    rows.on_surface(
        1,
        20,
        crate::theme::Role::Surface,
        crate::surface::Edges::BOTH,
    );
    let (lines, texts) = rows.into_parts();
    assert_eq!(lines.len(), 5);
    assert_eq!(lines[0].0.to_string(), "a");
    assert_eq!(lines[0].0.style.bg, None);
    assert_eq!(lines[1].0.to_string(), "▄".repeat(20));
    assert!(texts[1].decoration);
    assert_eq!(lines[2].0.to_string(), "b");
    assert_eq!(
        lines[2].0.style.bg,
        Some(crate::theme::Role::Surface.color())
    );
    assert_eq!(lines[3].0.to_string(), "c");
    assert_eq!(
        lines[3].0.style.bg,
        Some(crate::theme::Role::Surface.color())
    );
    assert_eq!(lines[4].0.to_string(), "▀".repeat(20));
    assert!(texts[4].decoration);
    assert!(lines[1].1.is_none() && lines[4].1.is_none());
    assert_eq!(texts[2], RowText::plain());
    assert_eq!(texts[3], RowText::plain());
}

#[test]
fn on_surface_with_no_rows_pushes_nothing() {
    // No rows from `from`: nothing (`docs/tui.md`, "Look").
    for from in [1, 3] {
        let mut rows = Rows::default();
        rows.push(row("a"));
        rows.on_surface(
            from,
            20,
            crate::theme::Role::Surface,
            crate::surface::Edges::BOTH,
        );
        let (lines, _) = rows.into_parts();
        assert_eq!(lines.len(), 1, "from {from}");
        assert_eq!(lines[0].0.to_string(), "a");
    }
}

#[test]
fn on_surface_with_one_row_has_both_edges() {
    let mut rows = Rows::default();
    rows.push(row("a"));
    rows.push(row("b"));
    rows.on_surface(
        1,
        20,
        crate::theme::Role::Surface,
        crate::surface::Edges::BOTH,
    );
    let (lines, _) = rows.into_parts();
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[1].0.to_string(), "▄".repeat(20));
    assert_eq!(lines[3].0.to_string(), "▀".repeat(20));
}

#[test]
fn on_surface_draws_only_the_edges_asked() {
    use crate::surface::Edges;
    for (edges, top, bottom) in [
        (
            Edges {
                top: false,
                bottom: true,
            },
            false,
            true,
        ),
        (
            Edges {
                top: true,
                bottom: false,
            },
            true,
            false,
        ),
        (
            Edges {
                top: false,
                bottom: false,
            },
            false,
            false,
        ),
    ] {
        let mut rows = Rows::default();
        rows.push(row("a"));
        rows.push(row("b"));
        rows.on_surface(0, 10, crate::theme::Role::Surface, edges);
        let (lines, _) = rows.into_parts();
        let want = 2 + usize::from(top) + usize::from(bottom);
        assert_eq!(lines.len(), want);
        let mut at = 0;
        if top {
            assert_eq!(lines[at].0.to_string(), "▄".repeat(10));
            at += 1;
        }
        assert_eq!(lines[at].0.to_string(), "a");
        assert_eq!(lines[at + 1].0.to_string(), "b");
        if bottom {
            assert_eq!(lines[at + 2].0.to_string(), "▀".repeat(10));
        }
    }
}

#[test]
fn on_surface_keeps_each_rows_text() {
    use crate::app::Target;
    // Skip, tail, join, links and scopes stay; both edges carry the scope
    // stack (`docs/tui.md`, "Look").
    let mut rows = Rows::default();
    rows.push(row("a"));
    assert!(rows.open_scope(Target::Group(1), true));
    rows.push_text(
        (Line::raw("b".to_owned()), None),
        RowText {
            join: Join::WrapSpace,
            skip: 2,
            tail: 1,
            decoration: false,
            links: vec![(0..1, "https://x".to_owned())],
            scopes: Vec::new(),
        },
    );
    rows.on_surface(
        1,
        10,
        crate::theme::Role::Surface,
        crate::surface::Edges::BOTH,
    );
    let (lines, texts) = rows.into_parts();
    assert_eq!(lines.len(), 4);
    assert_eq!(texts[2].join, Join::WrapSpace);
    assert_eq!(texts[2].skip, 2);
    assert_eq!(texts[2].tail, 1);
    assert_eq!(texts[2].links, vec![(0..1, "https://x".to_owned())]);
    assert_eq!(texts[2].scopes, vec![Target::Group(1)]);
    assert_eq!(texts[1].scopes, vec![Target::Group(1)]);
    assert_eq!(texts[3].scopes, vec![Target::Group(1)]);
}
