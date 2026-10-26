//! Tests for the span rules: validation, cut and pad, links, flat, and
//! the built-in span conversion.

use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use super::{Shape, fit, from_ratatui};
use crate::ext::RawSpan;
use crate::theme::Role;

fn span(text: &str) -> RawSpan {
    RawSpan {
        text: text.to_owned(),
        ..RawSpan::default()
    }
}

fn styled_span(text: &str) -> RawSpan {
    RawSpan {
        text: text.to_owned(),
        fg: Some("success".to_owned()),
        bg: Some("surface".to_owned()),
        bold: true,
        dim: true,
        italic: true,
        underline: true,
        link: Some("https://example.com".to_owned()),
        click: Some("badge".to_owned()),
    }
}

fn first(lines: &[super::FittedLine]) -> &super::FittedLine {
    lines.first().expect("one fitted line")
}

#[test]
fn a_full_span_maps_every_field() {
    let fitted = fit(&[vec![styled_span(" exit 0 ")]], 20, false).expect("a valid line");
    let line = &first(&fitted).line;
    let drawn = line.spans.first().expect("one span");
    assert_eq!(drawn.content.as_ref(), " exit 0 ");
    assert_eq!(
        drawn.style,
        Style::default()
            .fg(Role::Success.color())
            .bg(Role::Surface.color())
            .add_modifier(Modifier::BOLD | Modifier::DIM | Modifier::ITALIC | Modifier::UNDERLINED)
    );
    assert_eq!(
        first(&fitted).links,
        vec![(0..8, "https://example.com".to_owned())]
    );
    assert_eq!(first(&fitted).pad, 12);
    assert_eq!(line.to_string(), format!(" exit 0 {}", " ".repeat(12)));
}

#[test]
fn a_plain_span_keeps_the_default_style() {
    let fitted = fit(&[vec![span("hi")]], 10, false).expect("a valid line");
    let drawn = first(&fitted).line.spans.first().expect("one span");
    assert_eq!(drawn.style, Style::default());
    assert_eq!(first(&fitted).links, vec![]);
    assert_eq!(first(&fitted).pad, 8);
}

#[test]
fn an_unknown_fg_rejects_the_line_naming_fg() {
    for name in ["nope", "#ff0000", "Success", ""] {
        let bad = RawSpan {
            fg: Some(name.to_owned()),
            ..RawSpan::default()
        };
        let error = fit(&[vec![bad]], 10, false).expect_err("an unknown fg");
        assert_eq!(error, Shape::Fg(name.to_owned()));
        assert!(format!("{error}").contains("fg"), "{error}");
    }
}

#[test]
fn an_unknown_bg_rejects_the_line_naming_bg() {
    for name in ["nope", "#ff0000", "Success", ""] {
        let bad = RawSpan {
            bg: Some(name.to_owned()),
            ..RawSpan::default()
        };
        let error = fit(&[vec![bad]], 10, false).expect_err("an unknown bg");
        assert_eq!(error, Shape::Bg(name.to_owned()));
        assert!(format!("{error}").contains("bg"), "{error}");
    }
}

#[test]
fn a_control_character_rejects_the_line_naming_text() {
    for text in ["a\nb", "a\tb", "a\u{1b}b", "a\u{7f}b", "a\u{9b}b"] {
        let error = fit(&[vec![span(text)]], 10, false).expect_err("a control character");
        assert_eq!(error, Shape::Text);
        assert!(format!("{error}").contains("text"), "{error}");
    }
    fit(&[vec![span("clean text ✓")]], 20, false).expect("no control character");
}

#[test]
fn a_bad_span_rejects_its_line_and_its_result() {
    let bad = RawSpan {
        fg: Some("nope".to_owned()),
        ..RawSpan::default()
    };
    let error = fit(&[vec![bad, span("good")]], 20, false).expect_err("no partial lines");
    assert_eq!(error, Shape::Fg("nope".to_owned()));
    let error = fit(&[vec![span("good")], vec![span("bad\u{7}")]], 20, false)
        .expect_err("no partial lines");
    assert_eq!(error, Shape::Text);
}

#[test]
fn width_boundaries_cut_without_ellipsis_and_pad_with_spaces() {
    let exact = fit(&[vec![span("0123456789")]], 10, false).expect("exactly the width");
    assert_eq!(first(&exact).pad, 0);
    assert_eq!(first(&exact).line.to_string(), "0123456789");
    let over = fit(&[vec![span("0123456789X")]], 10, false).expect("one cell over");
    assert_eq!(first(&over).pad, 0);
    assert_eq!(first(&over).line.to_string(), "0123456789");
    assert!(!first(&over).line.to_string().contains('…'));
    let under = fit(&[vec![span("012345678")]], 10, false).expect("one cell under");
    assert_eq!(first(&under).pad, 1);
    assert_eq!(first(&under).line.to_string(), "012345678 ");
}

#[test]
fn a_wide_character_at_the_edge_is_dropped_whole_and_padded() {
    let fitted = fit(&[vec![span("あいう")]], 5, false).expect("a valid line");
    assert_eq!(first(&fitted).line.to_string(), "あい ");
    assert_eq!(first(&fitted).pad, 1);
}

#[test]
fn combining_marks_and_zwj_sequences_are_never_split() {
    let combo = "e\u{301}";
    let text = format!("abcd{combo}efgh");
    let fitted = fit(&[vec![span(&text)]], 5, false).expect("a valid line");
    let kept = format!("abcd{combo}");
    assert_eq!(first(&fitted).line.to_string(), kept);
    assert_eq!(first(&fitted).pad, 0);
    let family = "👨‍👩‍👧‍👦";
    let cells = crate::format::width(family);
    let text = format!("a{family}");
    let widest = u16::try_from(cells).unwrap_or(u16::MAX);
    for width in 1..=widest {
        let fitted = fit(&[vec![span(&text)]], width, false).expect("a valid line");
        let plain = fitted.first().expect("one fitted line").line.to_string();
        let tail = " ".repeat(usize::from(width).saturating_sub(1));
        assert_eq!(plain, format!("a{tail}"), "width {width}");
        assert!(
            !plain.contains('👨') || plain.contains(family),
            "width {width}"
        );
    }
    let wide = u16::try_from(1usize.saturating_add(cells)).unwrap_or(u16::MAX);
    let fitted = fit(&[vec![span(&text)]], wide, false).expect("a valid line");
    assert!(first(&fitted).line.to_string().contains(family));
    assert_eq!(first(&fitted).pad, 0);
}

#[test]
fn width_zero_draws_an_empty_line() {
    let fitted = fit(&[vec![span("abc")]], 0, false).expect("a valid line");
    assert_eq!(first(&fitted).line.to_string(), "");
    assert_eq!(first(&fitted).pad, 0);
    assert_eq!(first(&fitted).links, vec![]);
}

#[test]
fn no_lines_draws_nothing() {
    let fitted = fit(&[], 20, false).expect("no lines");
    assert_eq!(fitted, vec![]);
}

#[test]
fn the_drawn_text_is_the_span_texts_plus_the_pad() {
    let fitted = fit(&[vec![span("ab"), span("cd")]], 10, false).expect("a valid line");
    let plain = first(&fitted).line.to_string();
    let pad = usize::from(first(&fitted).pad);
    let kept = plain.len().saturating_sub(pad);
    assert_eq!(plain.get(..kept), Some("abcd"));
}

#[test]
fn a_valid_link_marks_its_cells() {
    let second = RawSpan {
        text: "hello".to_owned(),
        link: Some("https://example.com".to_owned()),
        ..RawSpan::default()
    };
    let fitted = fit(&[vec![span("ab"), second]], 20, false).expect("a valid line");
    assert_eq!(
        first(&fitted).links,
        vec![(2..7, "https://example.com".to_owned())]
    );
}

#[test]
fn a_refused_link_rejects_the_line_naming_link() {
    for to in [
        "file:///etc/passwd",
        "javascript:alert(1)",
        "https://exa mple.com",
        "https://example.com/\u{7f}",
    ] {
        let bad = RawSpan {
            text: "go".to_owned(),
            link: Some(to.to_owned()),
            ..RawSpan::default()
        };
        let error = fit(&[vec![bad]], 20, false).expect_err("a refused link");
        assert_eq!(error, Shape::Link(to.to_owned()));
        assert!(format!("{error}").contains("link"), "{error}");
    }
}

#[test]
fn a_cut_link_keeps_only_its_visible_cells() {
    let first_span = RawSpan {
        text: "ab".to_owned(),
        link: Some("https://a.example".to_owned()),
        ..RawSpan::default()
    };
    let second_span = RawSpan {
        text: "cdef".to_owned(),
        link: Some("https://b.example".to_owned()),
        ..RawSpan::default()
    };
    let fitted = fit(&[vec![first_span, second_span]], 5, false).expect("a valid line");
    assert_eq!(first(&fitted).line.to_string(), "abcde");
    assert_eq!(
        first(&fitted).links,
        vec![
            (0..2, "https://a.example".to_owned()),
            (2..5, "https://b.example".to_owned()),
        ]
    );
    let gone = RawSpan {
        text: "cdef".to_owned(),
        link: Some("https://b.example".to_owned()),
        ..RawSpan::default()
    };
    let fitted = fit(&[vec![span("ab"), gone]], 2, false).expect("a valid line");
    assert_eq!(first(&fitted).links, vec![]);
    assert_eq!(first(&fitted).line.to_string(), "ab");
}

#[test]
fn a_span_past_a_full_width_is_dropped_whole() {
    let fitted = fit(&[vec![span("ab"), span("cd")]], 2, false).expect("a valid line");
    assert_eq!(first(&fitted).line.to_string(), "ab");
    assert_eq!(first(&fitted).pad, 0);
}

#[test]
fn flat_drops_every_style_and_keeps_text_and_links() {
    let fitted = fit(&[vec![styled_span("hi")]], 10, true).expect("a valid line");
    let drawn = first(&fitted).line.spans.first().expect("one span");
    assert_eq!(drawn.style, Style::default());
    assert_eq!(
        first(&fitted).line.to_string(),
        format!("hi{}", " ".repeat(8))
    );
    assert_eq!(
        first(&fitted).links,
        vec![(0..2, "https://example.com".to_owned())]
    );
    let kept = fit(&[vec![styled_span("hi")]], 10, false).expect("a valid line");
    assert_ne!(
        kept.first()
            .expect("one span")
            .line
            .spans
            .first()
            .expect("one span")
            .style,
        Style::default()
    );
}

#[test]
fn from_ratatui_names_roles_and_flags() {
    use crate::theme::style;
    let role = from_ratatui(&[Span::styled("x", style(Role::Success))]);
    assert_eq!(
        role.first().expect("one span").fg,
        Some("success".to_owned())
    );
    assert_eq!(role.first().expect("one span").bg, None);
    let dimmed = from_ratatui(&[Span::styled(
        "y",
        Style::default().add_modifier(Modifier::DIM),
    )]);
    assert!(dimmed.first().expect("one span").dim);
    assert_eq!(dimmed.first().expect("one span").fg, None);
    let both = from_ratatui(&[Span::styled(
        "z",
        Style::default().add_modifier(Modifier::DIM | Modifier::ITALIC),
    )]);
    let flags = both.first().expect("one span");
    assert_eq!(
        (flags.dim, flags.italic, flags.bold, flags.underline),
        (true, true, false, false)
    );
    let plain = from_ratatui(&[Span::raw("w")]);
    assert_eq!(
        plain.first().expect("one span"),
        &RawSpan {
            text: "w".to_owned(),
            ..RawSpan::default()
        }
    );
}

#[test]
fn from_ratatui_then_fit_reproduces_the_ledger_styles() {
    use crate::theme::style;
    let built_in = vec![
        Span::styled("info ", style(Role::Info)),
        Span::styled("ok ", style(Role::Success)),
        Span::styled("wait ", style(Role::Attention)),
        Span::styled("+1 ", style(Role::Added)),
        Span::styled("−2", style(Role::Removed)),
        Span::styled("dim", Style::default().add_modifier(Modifier::DIM)),
    ];
    let width: usize = built_in
        .iter()
        .map(|span| crate::format::width(&span.content))
        .sum();
    let width = u16::try_from(width).unwrap_or(u16::MAX);
    let raw = from_ratatui(&built_in);
    let fitted = fit(&[raw], width, false).expect("a valid line");
    let again = &first(&fitted).line.spans;
    assert_eq!(again.len(), built_in.len());
    for (drawn, expected) in again.iter().zip(built_in.iter()) {
        assert_eq!(drawn.content, expected.content);
        assert_eq!(drawn.style, expected.style);
    }
    assert_eq!(first(&fitted).pad, 0);
}
