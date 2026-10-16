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
        spin: None,
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
            spin: None,
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

#[test]
fn pad_prefixes_blank_cells_and_shifts_text_marks() {
    // One pad column: the row starts with a blank, skip grows by one,
    // and the link and spin move past it; decoration stays
    // (`docs/tui.md`, "Turns").
    use crate::app::Target;
    let mut rows = Rows::default();
    rows.push_text(
        row("hello world"),
        RowText {
            skip: 0,
            tail: 0,
            decoration: true,
            links: vec![(3..7, "https://x".to_owned())],
            scopes: Vec::new(),
            spin: Some(0),
            join: Join::Break,
        },
    );
    rows.pad(0, 1, 20);
    let (lines, texts) = rows.into_parts();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].0.to_string(), " hello world");
    assert_eq!(lines[0].0.width(), 12);
    assert_eq!(texts[0].skip, 1);
    assert_eq!(texts[0].links, vec![(4..8, "https://x".to_owned())]);
    assert_eq!(texts[0].spin, Some(1));
    assert!(texts[0].decoration);
    let _ = Target::Group(1);
}

#[test]
fn pad_splits_an_over_wide_row_at_a_space() {
    // A dim line of 29 cells at inner 10 breaks after its spaces into 3
    // pieces, each padded to at most 11 cells, keeping the dim style and
    // joining with the dropped space; the target stays on the first
    // piece (`docs/tui.md`, "Turns", "Selection and copy").
    use ratatui::style::{Modifier, Style};
    use ratatui::text::Span;
    let dim = Style::default().add_modifier(Modifier::DIM);
    let mut rows = Rows::default();
    rows.push_text(
        (
            Line::styled("aaaa bbbb cccc dddd eeee ffff", dim),
            Some(crate::app::Target::Group(3)),
        ),
        RowText {
            join: Join::WrapSpace,
            ..RowText::plain()
        },
    );
    rows.pad(0, 1, 10);
    let (lines, texts) = rows.into_parts();
    assert_eq!(lines.len(), 3);
    let shown: Vec<String> = lines.iter().map(|(line, _)| line.to_string()).collect();
    assert_eq!(shown, [" aaaa bbbb", " cccc dddd", " eeee ffff"]);
    for (line, _) in &lines {
        assert!(line.width() <= 11, "{line}");
        assert!(line.to_string().starts_with(' '));
        assert!(
            line.spans
                .iter()
                .all(|span: &Span| span.style.add_modifier.contains(Modifier::DIM))
                || line.spans.first().is_some_and(|span| span.content == " "),
            "{line:?}"
        );
    }
    assert_eq!(
        texts.iter().map(|text| text.join).collect::<Vec<_>>(),
        [Join::WrapSpace, Join::WrapSpace, Join::WrapSpace]
    );
    assert_eq!(lines[0].1, Some(crate::app::Target::Group(3)));
    assert!(lines[1].1.is_none() && lines[2].1.is_none());
    let logical: Vec<String> = crate::logical::logical(&lines, &texts)
        .iter()
        .map(|line| line.text.clone())
        .collect();
    assert_eq!(logical, ["aaaa bbbb cccc dddd eeee ffff"]);
}

#[test]
fn pad_splits_a_multi_span_row_keeping_each_style() {
    // An edit-ledger-shaped row (a 40-cell path, " +3" in added, " −1"
    // in removed, all bold) at inner 12: every piece fits the room with
    // its pad, and the colours and the bold stay on their text
    // (`docs/tui.md`, "Turns").
    use ratatui::style::{Modifier, Style};
    use ratatui::text::Span;
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let line = Line::from(vec![
        Span::styled("a".repeat(40), bold),
        Span::styled(" +3", bold.fg(crate::theme::Role::Added.color())),
        Span::styled(" −1", bold.fg(crate::theme::Role::Removed.color())),
    ]);
    let mut rows = Rows::default();
    rows.push((line, None));
    rows.pad(0, 1, 12);
    let (lines, texts) = rows.into_parts();
    assert_eq!(lines.len(), 4);
    for (line, _) in &lines {
        assert!(line.width() <= 13, "{line}");
    }
    for (line, _) in &lines {
        for span in line.spans.iter().skip(1) {
            assert!(span.style.add_modifier.contains(Modifier::BOLD), "{span:?}");
        }
    }
    let last = lines
        .last()
        .map(|(line, _)| line.to_string())
        .expect("a last piece");
    assert!(last.contains("+3"), "{last:?}");
    assert!(last.contains("−1"), "{last:?}");
    let added = lines
        .iter()
        .flat_map(|(line, _)| &line.spans)
        .find(|span| span.content.contains("+3"))
        .expect("the added count keeps its span");
    assert_eq!(added.style.fg, Some(crate::theme::Role::Added.color()));
    let removed = lines
        .iter()
        .flat_map(|(line, _)| &line.spans)
        .find(|span| span.content.contains("−1"))
        .expect("the removed count keeps its span");
    assert_eq!(removed.style.fg, Some(crate::theme::Role::Removed.color()));
    let logical: Vec<String> = crate::logical::logical(&lines, &texts)
        .iter()
        .map(|line| line.text.clone())
        .collect();
    assert_eq!(logical, [format!("{} +3 −1", "a".repeat(40))]);
}

#[test]
fn pad_redistributes_skip_tail_links_and_spin() {
    // A 20-cell row with skip 2, tail 3, a link on 8..14 and a spin at
    // 15, at inner 6 with pad 1: each piece's marks cover exactly the
    // original cells it drew, shifted by the pad, and the pieces copy
    // back to the row's text (`docs/tui.md`, "Selection and copy").
    let mut rows = Rows::default();
    rows.push_text(
        row("0123456789abcdefghij"),
        RowText {
            skip: 2,
            tail: 3,
            links: vec![
                (8..14, "https://x".to_owned()),
                (0..6, "https://boundary".to_owned()),
                (5..7, "https://adjacent".to_owned()),
            ],
            spin: Some(15),
            ..RowText::plain()
        },
    );
    rows.pad(0, 1, 6);
    let (lines, texts) = rows.into_parts();
    assert_eq!(lines.len(), 4);
    let shown: Vec<String> = lines.iter().map(|(line, _)| line.to_string()).collect();
    assert_eq!(shown, [" 012345", " 6789ab", " cdefgh", " ij"]);
    assert_eq!(
        texts.iter().map(|text| text.skip).collect::<Vec<_>>(),
        [3, 1, 1, 1]
    );
    assert_eq!(
        texts.iter().map(|text| text.tail).collect::<Vec<_>>(),
        [0, 0, 1, 2]
    );
    assert_eq!(
        texts
            .iter()
            .map(|text| text.links.clone())
            .collect::<Vec<_>>(),
        [
            vec![
                (1..7, "https://boundary".to_owned()),
                (6..7, "https://adjacent".to_owned()),
            ],
            vec![
                (3..7, "https://x".to_owned()),
                (1..2, "https://adjacent".to_owned()),
            ],
            vec![(1..3, "https://x".to_owned())],
            Vec::new(),
        ]
    );
    assert_eq!(
        texts.iter().map(|text| text.spin).collect::<Vec<_>>(),
        [None, None, Some(4), None]
    );
    assert_eq!(
        texts.iter().map(|text| text.join).collect::<Vec<_>>(),
        [Join::Break, Join::Wrap, Join::Wrap, Join::Wrap]
    );
    let logical: Vec<String> = crate::logical::logical(&lines, &texts)
        .iter()
        .map(|line| line.text.clone())
        .collect();
    assert_eq!(logical, ["23456789abcdefg"]);
}

#[test]
fn pad_hard_cuts_a_word_longer_than_the_inner_width() {
    // No space in the window: the pieces cut hard and join with nothing
    // (`docs/tui.md`, "Selection and copy").
    let mut rows = Rows::default();
    rows.push(row("abcdefghij"));
    rows.pad(0, 1, 4);
    let (lines, texts) = rows.into_parts();
    let shown: Vec<String> = lines.iter().map(|(line, _)| line.to_string()).collect();
    assert_eq!(shown, [" abcd", " efgh", " ij"]);
    assert_eq!(
        texts.iter().map(|text| text.join).collect::<Vec<_>>(),
        [Join::Break, Join::Wrap, Join::Wrap]
    );
    let logical: Vec<String> = crate::logical::logical(&lines, &texts)
        .iter()
        .map(|line| line.text.clone())
        .collect();
    assert_eq!(logical, ["abcdefghij"]);
}

#[test]
fn pad_blanks_a_glyph_wider_than_the_inner_width() {
    // "界" is two cells at inner 1: it draws as one blank cell keeping
    // its style, plus the pad (`docs/tui.md`, "Turns").
    use ratatui::style::{Modifier, Style};
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let mut rows = Rows::default();
    rows.push((Line::styled("界", bold), None));
    rows.pad(0, 1, 1);
    let (lines, texts) = rows.into_parts();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].0.to_string(), "  ");
    assert_eq!(lines[0].0.width(), 2);
    assert!(
        lines[0].0.style.add_modifier.contains(Modifier::BOLD),
        "{:#?}",
        lines[0].0
    );
    assert_eq!(texts[0].skip, 1);
}

#[test]
fn pad_never_splits_a_zwj_emoji() {
    let line = Line::raw("x👩‍💻");
    assert_eq!(super::fitted(&line, 3), 1);

    let mut rows = Rows::default();
    rows.push((line.clone(), None));
    rows.pad(0, 1, 4);
    let (lines, _) = rows.into_parts();
    assert_eq!(
        lines
            .iter()
            .map(|(line, _)| line.to_string())
            .collect::<Vec<_>>(),
        [" x👩‍💻"]
    );

    let mut rows = Rows::default();
    rows.push((line, None));
    rows.pad(0, 0, 2);
    let (lines, _) = rows.into_parts();
    assert_eq!(
        lines
            .iter()
            .map(|(line, _)| line.to_string())
            .collect::<Vec<_>>(),
        ["x", "👩‍💻"]
    );
}

#[test]
fn pad_respects_the_row_start_and_width_boundaries() {
    let expected = ["first", "second", " third"];
    for (from, want) in [
        (2, expected.to_vec()),
        (3, vec!["first", "second", "third"]),
        (4, vec!["first", "second", "third"]),
    ] {
        let mut rows = Rows::default();
        rows.extend([row("first"), row("second"), row("third")]);
        rows.pad(from, 1, 20);
        let (lines, _) = rows.into_parts();
        let shown: Vec<String> = lines.iter().map(|(line, _)| line.to_string()).collect();
        assert_eq!(shown, want, "from {from}");
    }

    for (inner, want) in [(2, vec!["ab", "c"]), (3, vec!["abc"]), (4, vec!["abc"])] {
        let mut rows = Rows::default();
        rows.push(row("abc"));
        rows.pad(0, 0, inner);
        let (lines, _) = rows.into_parts();
        let shown: Vec<String> = lines.iter().map(|(line, _)| line.to_string()).collect();
        assert_eq!(shown, want, "inner {inner}");
    }

    let mut rows = Rows::default();
    rows.push(row("abc"));
    rows.pad(0, 1, 0);
    let (lines, _) = rows.into_parts();
    assert_eq!(lines[0].0.to_string(), " abc");
}

#[test]
fn pad_keeps_an_exact_fit_line_intact() {
    let empty = ratatui::text::Span::styled(
        "",
        ratatui::style::Style::default().add_modifier(ratatui::style::Modifier::BOLD),
    );
    let mut rows = Rows::default();
    rows.push((
        Line::from(vec![empty, ratatui::text::Span::raw("abc")]),
        None,
    ));
    rows.pad(0, 0, 3);
    let (lines, _) = rows.into_parts();
    assert_eq!(lines[0].0.spans.len(), 2);
    assert_eq!(lines[0].0.spans[0].content, "");
    assert_eq!(lines[0].0.spans[1].content, "abc");
}

#[test]
fn pad_keeps_exact_width_glyphs_and_spaces_at_split_boundaries() {
    for (inner, expected) in [(1, " "), (2, "界"), (3, "界")] {
        let mut rows = Rows::default();
        rows.push(row("界"));
        rows.pad(0, 0, inner);
        let (lines, _) = rows.into_parts();
        assert_eq!(
            lines
                .iter()
                .map(|(line, _)| line.to_string())
                .collect::<Vec<_>>(),
            [expected],
            "inner {inner}"
        );
    }

    let mut rows = Rows::default();
    rows.push(row("ab"));
    rows.pad(0, 0, 1);
    let (lines, _) = rows.into_parts();
    assert_eq!(
        lines
            .iter()
            .map(|(line, _)| line.to_string())
            .collect::<Vec<_>>(),
        ["a", "b"]
    );

    let mut rows = Rows::default();
    rows.push(row(" abcde"));
    rows.pad(0, 0, 3);
    let (lines, _) = rows.into_parts();
    assert_eq!(
        lines
            .iter()
            .map(|(line, _)| line.to_string())
            .collect::<Vec<_>>(),
        [" ab", "cde"]
    );

    let mut rows = Rows::default();
    rows.push(row("ab cd "));
    rows.pad(0, 0, 4);
    let (lines, _) = rows.into_parts();
    assert_eq!(
        lines
            .iter()
            .map(|(line, _)| line.to_string())
            .collect::<Vec<_>>(),
        ["ab", "cd "]
    );
}

#[test]
fn a_spin_on_a_piece_boundary_belongs_to_the_next_piece_only() {
    for (spin, expected) in [
        (5, [Some(6), None, None]),
        (6, [None, Some(1), None]),
        (7, [None, Some(2), None]),
    ] {
        let mut rows = Rows::default();
        rows.push_text(
            row("0123456789abcdefgh"),
            RowText {
                spin: Some(spin),
                ..RowText::plain()
            },
        );
        rows.pad(0, 1, 6);
        let (_, texts) = rows.into_parts();
        assert_eq!(
            texts.iter().map(|text| text.spin).collect::<Vec<_>>(),
            expected,
            "spin at {spin}"
        );
    }
}

#[test]
fn plan_returns_no_pieces_for_an_empty_row() {
    assert!(super::plan(&[], 6).is_empty());
}

#[test]
fn fitted_counts_the_pieces_pad_draws() {
    // The count and the drawing share one split rule (`docs/tui.md`,
    // "Turns").
    for (text, inner) in [
        ("hello world", 3),
        ("hello world", 5),
        ("hello world", 30),
        ("abcdefghij", 4),
        ("aaaa bbbb cccc dddd eeee ffff", 10),
        ("", 6),
    ] {
        let line = Line::raw(text.to_owned());
        let mut rows = Rows::default();
        rows.push((line.clone(), None));
        rows.pad(0, 0, inner);
        let (lines, _) = rows.into_parts();
        assert_eq!(
            super::fitted(&line, inner),
            lines.len(),
            "{text:?} at {inner}"
        );
    }
    assert_eq!(super::fitted(&Line::raw("hi"), 0), 1);
}

#[test]
fn gap_inserts_one_plain_blank_row_at_the_index() {
    // One empty plain row between the rows around it, with the scopes
    // current at the call (`docs/tui.md`, "Turns").
    use crate::app::Target;
    let mut rows = Rows::default();
    rows.push(row("a"));
    assert!(rows.open_scope(Target::Group(1), true));
    rows.push(row("b"));
    rows.gap(1);
    rows.end_scope();
    rows.push(row("c"));
    let (lines, texts) = rows.into_parts();
    let shown: Vec<String> = lines.iter().map(|(line, _)| line.to_string()).collect();
    assert_eq!(shown, ["a", "", "b", "c"]);
    assert_eq!(
        texts[1],
        RowText {
            scopes: vec![Target::Group(1)],
            ..RowText::plain()
        }
    );
}

/// A texts vector shorter than its rows ends the pad: the rows keep
/// their shape instead of drawing half-moved.
#[test]
fn pad_with_a_missing_text_row_leaves_the_rows() {
    let mut out = Rows {
        rows: vec![(Line::raw("abc"), None)],
        texts: Vec::new(),
        scopes: Vec::new(),
        all_open: false,
    };
    out.pad(0, 1, 10);
    assert_eq!(out.rows.len(), 1);
    assert_eq!(out.rows[0].0.to_string(), "abc");
}
