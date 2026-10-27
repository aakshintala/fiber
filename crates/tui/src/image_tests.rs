//! Tests for one image part's line: what is kept, what the line
//! names, and that it always draws one row. Ledger rows draw the
//! inline grid with its caption; the prompt and steering forms stay
//! the one line.

use contract::shapes::ContentPart;
use ratatui::layout::Alignment;
use ratatui::style::Modifier;
use ratatui::text::Line;

use super::{Align, IdAllocator, Layout, parts};
use crate::app::Target;
use crate::format;
use crate::rows::Rows;
use crate::theme::Role;

fn session() -> contract::SessionId {
    contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned())
}

fn other() -> contract::SessionId {
    contract::SessionId("s_bbbbbbbbbbbbbbbb".to_owned())
}

fn image(path: &str, width: u32, height: u32) -> ContentPart {
    ContentPart::Image {
        path: path.to_owned(),
        mime_type: "image/png".to_owned(),
        width,
        height,
    }
}

#[test]
fn part_from_content_keeps_path_and_size() {
    let part = super::Part::from_content(&session(), &image("artifacts/i_a.png", 1280, 800));
    assert_eq!(
        part,
        Some(super::Part {
            session: session(),
            path: "artifacts/i_a.png".to_owned(),
            mime_type: "image/png".to_owned(),
            width: 1280,
            height: 800,
        })
    );
}

#[test]
fn parts_skip_text_and_unknown() {
    // A PDF part is not an image part either: it changes nothing here.
    let content = vec![
        ContentPart::Text {
            text: "hi".to_owned(),
        },
        image("artifacts/a.png", 10, 20),
        serde_json::from_value(serde_json::json!(
            {"type": "pdf", "path": "artifacts/a.pdf", "page_count": 1}
        ))
        .unwrap_or(ContentPart::Unknown),
        ContentPart::Unknown,
        image("artifacts/b.png", 30, 40),
    ];
    let kept = parts(&session(), &content);
    assert_eq!(
        kept.iter()
            .map(|part| part.path.clone())
            .collect::<Vec<_>>(),
        ["artifacts/a.png", "artifacts/b.png"]
    );
}

#[test]
fn label_names_the_file_and_size() {
    for (path, width, height, label) in [
        ("artifacts/i_a.png", 1280, 800, "▣ i_a.png · 1280×800"),
        ("shot.png", 1, 1, "▣ shot.png · 1×1"),
        ("", 0, 0, "▣  · 0×0"),
    ] {
        let Some(part) = super::Part::from_content(&session(), &image(path, width, height)) else {
            panic!("kept {path}");
        };
        assert_eq!(part.label(), label, "{path}");
    }
}

/// Draws `label`'s part left-aligned at `columns`, returning its rows.
fn drawn(label: &str, columns: u16) -> Vec<(String, Option<Target>)> {
    let mut layout = Layout::default();
    let part = super::Part {
        session: session(),
        path: "artifacts/a.png".to_owned(),
        mime_type: "image/png".to_owned(),
        width: 1280,
        height: 800,
    };
    assert!(label.is_empty() || part.label() == label);
    layout.note(&part);
    let mut out = Rows::default();
    super::rows(
        &part,
        &layout,
        columns,
        Align::Left { indent: 0 },
        None,
        &mut out,
    );
    let (rows, _) = out.into_parts();
    rows.into_iter()
        .map(|(line, target)| (line.to_string(), target))
        .collect()
}

#[test]
fn a_long_label_is_cut_to_its_width_and_draws_one_row() {
    // A 200-character file name at width 40: one row, ending in `…`.
    let name = "a".repeat(200) + ".png";
    let path = format!("artifacts/{name}");
    let Some(part) = super::Part::from_content(&session(), &image(&path, 1280, 800)) else {
        panic!("kept");
    };
    let mut layout = Layout::default();
    let id = layout.note(&part);
    let mut out = Rows::default();
    super::rows(
        &part,
        &layout,
        40,
        Align::Left { indent: 0 },
        None,
        &mut out,
    );
    let (rows, _) = out.into_parts();
    assert_eq!(rows.len(), 1);
    let (line, target) = &rows[0];
    let text = line.to_string();
    assert_eq!(format::width(&text), 40);
    assert!(text.ends_with('…'), "{text}");
    assert_eq!(*target, Some(Target::Image(id)));
    // `y` gives the whole label, never the cut line.
    assert!(!part.label().ends_with('…'));
    assert!(format::width(&part.label()) > 40);
}

#[test]
fn an_image_line_draws_its_name_as_a_link_and_its_dims_dim() {
    let lines = lines_at(60);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].to_string(), "▣ a.png · 1280×800");
    let spans = &lines[0].spans;
    assert_eq!(spans.len(), 2);
    // `▣ <name>` in the accent colour, underlined.
    assert_eq!(spans[0].content.as_ref(), "▣ a.png");
    assert_eq!(spans[0].style.fg, Some(Role::Accent.color()));
    assert!(spans[0].style.add_modifier.contains(Modifier::UNDERLINED));
    // ` · <dims>` dim.
    assert_eq!(spans[1].content.as_ref(), " · 1280×800");
    assert!(spans[1].style.add_modifier.contains(Modifier::DIM));
    assert_eq!(spans[1].style.fg, None);
}

#[test]
fn a_label_cut_inside_its_dims_keeps_the_split() {
    // One column short: the dims lose their end to `…`, the name is
    // whole.
    let lines = lines_at(17);
    assert_eq!(lines.len(), 1);
    let spans = &lines[0].spans;
    assert_eq!(spans.len(), 2);
    assert_eq!(spans[0].content.as_ref(), "▣ a.png");
    assert_eq!(spans[0].style.fg, Some(Role::Accent.color()));
    assert_eq!(lines[0].to_string(), "▣ a.png · 1280×8…");
    assert!(spans[1].style.add_modifier.contains(Modifier::DIM));
}

#[test]
fn a_label_cut_inside_its_name_draws_wholly_as_the_link() {
    let lines = lines_at(4);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].to_string(), "▣ a…");
    let spans = &lines[0].spans;
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].style.fg, Some(Role::Accent.color()));
    assert!(spans[0].style.add_modifier.contains(Modifier::UNDERLINED));
}

/// The part's line left-aligned at `columns` with `indent` blank
/// columns before it.
fn lines_at_indented(columns: u16, indent: u16) -> Vec<Line<'static>> {
    let mut layout = Layout::default();
    let part = super::Part {
        session: session(),
        path: "artifacts/a.png".to_owned(),
        mime_type: "image/png".to_owned(),
        width: 1280,
        height: 800,
    };
    layout.note(&part);
    let mut out = Rows::default();
    super::rows(
        &part,
        &layout,
        columns,
        Align::Left { indent },
        None,
        &mut out,
    );
    let (rows, _) = out.into_parts();
    rows.into_iter().map(|(line, _)| line).collect()
}

/// The part's line left-aligned at `columns`.
fn lines_at(columns: u16) -> Vec<Line<'static>> {
    lines_at_indented(columns, 0)
}

#[test]
fn an_indented_line_keeps_plain_padding_before_the_link() {
    let lines = lines_at_indented(60, 4);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].to_string(), "    ▣ a.png · 1280×800");
    let spans = &lines[0].spans;
    assert_eq!(spans.len(), 3);
    assert_eq!(spans[0].content.as_ref(), "    ");
    assert_eq!(spans[1].content.as_ref(), "▣ a.png");
    assert_eq!(spans[1].style.fg, Some(Role::Accent.color()));
}

#[test]
fn an_image_line_counts_one_row_and_names_its_target() {
    let rows = drawn("▣ a.png · 1280×800", 60);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "▣ a.png · 1280×800");
    assert!(matches!(rows[0].1, Some(Target::Image(_))));
}

#[test]
fn a_right_aligned_line_stays_right_aligned() {
    let mut layout = Layout::default();
    let part = super::Part {
        session: session(),
        path: "artifacts/a.png".to_owned(),
        mime_type: "image/png".to_owned(),
        width: 10,
        height: 20,
    };
    layout.note(&part);
    let mut out = Rows::default();
    super::rows(&part, &layout, 60, Align::Right, None, &mut out);
    let (rows, _) = out.into_parts();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0.alignment, Some(Alignment::Right));
}

#[test]
fn a_line_names_its_own_paths_id() {
    let mut layout = Layout::default();
    let first = super::Part {
        session: session(),
        path: "artifacts/a.png".to_owned(),
        mime_type: "image/png".to_owned(),
        width: 10,
        height: 20,
    };
    let second = super::Part {
        path: "artifacts/b.png".to_owned(),
        ..first.clone()
    };
    layout.note(&first);
    layout.note(&second);
    let mut out = Rows::default();
    super::rows(
        &second,
        &layout,
        60,
        Align::Left { indent: 0 },
        None,
        &mut out,
    );
    let (rows, _) = out.into_parts();
    assert!(matches!(rows[0].1, Some(Target::Image(2))));
    assert_eq!(layout.id(&session(), "artifacts/c.png"), None);
}

#[test]
fn one_path_in_two_segments_draws_two_ids() {
    let mut layout = Layout::default();
    let first = super::Part::from_content(&session(), &image("artifacts/a.png", 10, 20))
        .expect("the own image is kept");
    let second = super::Part::from_content(&other(), &image("artifacts/a.png", 10, 20))
        .expect("the ancestor image is kept");
    let one = layout.note(&first);
    let two = layout.note(&second);
    assert_ne!(one, two, "one path in two segments draws two ids");
    assert_eq!(layout.id(&session(), "artifacts/a.png"), Some(one));
    assert_eq!(layout.id(&other(), "artifacts/a.png"), Some(two));
    assert_eq!(layout.note(&first), one, "refolding keeps the id");
}

/// A layout sizing inline with `cell`, having noted `part`.
fn inline_layout(part: &super::Part, cell: (u16, u16)) -> Layout {
    let mut layout = Layout::default();
    layout.note(part);
    layout.sizing = super::Sizing::Inline { cell };
    layout
}

#[test]
fn an_inline_png_draws_placeholders_then_its_caption() {
    use ratatui::style::Color;
    // 1280 by 800 at 80 columns with a 10 by 20 cell: 32 by 10
    // placeholders, then the caption line.
    let part = sized(1280, 800);
    let mut out = Rows::default();
    super::ledger_rows(
        &part,
        &inline_layout(&part, (10, 20)),
        80,
        Align::Left { indent: 4 },
        &mut out,
    );
    let (rows, _) = out.into_parts();
    assert_eq!(rows.len(), 11);
    for (at, (line, target)) in rows.iter().enumerate().take(10) {
        assert_eq!(
            line.spans.len(),
            33,
            "row {at}: the indent and one cell per column"
        );
        assert_eq!(line.spans[0].content.as_ref(), "    ");
        for span in line.spans.iter().skip(1) {
            assert!(
                span.content.as_ref().starts_with('\u{10EEEE}'),
                "row {at}: every grid cell is a placeholder"
            );
            assert_eq!(span.style.fg, Some(Color::Indexed(1)));
        }
        assert!(matches!(target, Some(Target::Image(1))), "row {at}");
    }
    let (caption, target) = &rows[10];
    assert_eq!(caption.to_string(), "    ▣ a.png · 1280×800");
    assert!(matches!(target, Some(Target::Image(1))));
}

#[test]
fn a_non_png_stays_its_one_line_when_sized_inline() {
    let jpeg = ContentPart::Image {
        path: "artifacts/a.jpg".to_owned(),
        mime_type: "image/jpeg".to_owned(),
        width: 1280,
        height: 800,
    };
    let part = super::Part::from_content(&session(), &jpeg).expect("the jpeg is kept");
    let mut out = Rows::default();
    super::ledger_rows(
        &part,
        &inline_layout(&part, (10, 20)),
        80,
        Align::Left { indent: 4 },
        &mut out,
    );
    let (rows, _) = out.into_parts();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0.to_string(), "    ▣ a.jpg · 1280×800");
}

#[test]
fn steering_stays_one_line_when_sized_inline() {
    // Steering draws through `rows` with no bubble width: even sized
    // inline, a noted PNG stays its one clickable line.
    let part = sized(1280, 800);
    let layout = inline_layout(&part, (10, 20));
    let mut out = Rows::default();
    super::rows(
        &part,
        &layout,
        80,
        Align::Left { indent: 0 },
        None,
        &mut out,
    );
    let (rows, _) = out.into_parts();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0.to_string(), "▣ a.png · 1280×800");
    assert!(matches!(rows[0].1, Some(Target::Image(1))));
    assert!(
        rows[0]
            .0
            .spans
            .iter()
            .all(|span| !span.content.as_ref().starts_with('\u{10EEEE}')),
        "no placeholders outside a ledger row"
    );
}

#[test]
fn ledger_indent_zero_stays_one_line_when_sized_inline() {
    // Kills `replace > with >= in ledger_rows`: a noted,
    // placeholder-eligible PNG at `Align::Left { indent: 0 }` is no
    // ledger row, so even sized inline it stays its one clickable
    // viewer-target line instead of drawing the inline grid
    // (`docs/tui.md`, "Images").
    let part = sized(1280, 800);
    let layout = inline_layout(&part, (10, 20));
    assert!(super::placeholder_eligible(1), "id 1 names a colour");
    let mut out = Rows::default();
    super::ledger_rows(&part, &layout, 80, Align::Left { indent: 0 }, &mut out);
    let (rows, _) = out.into_parts();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0.to_string(), "▣ a.png · 1280×800");
    assert!(matches!(rows[0].1, Some(Target::Image(1))));
    assert!(
        rows[0]
            .0
            .spans
            .iter()
            .all(|span| !span.content.as_ref().starts_with('\u{10EEEE}')),
        "no placeholders without a ledger indent"
    );
}

#[test]
fn a_bubble_without_a_text_width_stays_one_line_when_sized_inline() {
    // No text bubble means no block width: even sized inline, a noted
    // PNG stays its one clickable line, right-aligned.
    let part = sized(1280, 800);
    let layout = inline_layout(&part, (10, 20));
    let mut out = Rows::default();
    super::rows(&part, &layout, 80, Align::Right, None, &mut out);
    let (rows, _) = out.into_parts();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0.to_string(), "▣ a.png · 1280×800");
    assert_eq!(rows[0].0.alignment, Some(Alignment::Right));
    assert!(matches!(rows[0].1, Some(Target::Image(1))));
    assert!(
        rows[0]
            .0
            .spans
            .iter()
            .all(|span| !span.content.as_ref().starts_with('\u{10EEEE}')),
        "no placeholders without a text bubble"
    );
}

#[test]
fn a_bubble_png_draws_the_shared_grid_right_aligned() {
    use ratatui::style::Color;
    // 1280 by 800 at 80 columns with a 10 by 20 cell: 32 by 10
    // placeholders, then the caption cut to the bubble block width
    // (`docs/tui.md`, "Images": at most 40% of the conversation's
    // width and 12 rows, keeping proportions; a click opens the
    // viewer, so every grid row and the caption carry its target).
    let part = sized(1280, 800);
    let mut out = Rows::default();
    super::rows(
        &part,
        &inline_layout(&part, (10, 20)),
        80,
        Align::Right,
        Some(15),
        &mut out,
    );
    let (rows, _) = out.into_parts();
    assert_eq!(rows.len(), 11);
    for (at, (line, target)) in rows.iter().enumerate().take(10) {
        assert_eq!(line.spans.len(), 32, "row {at}: one cell per column");
        assert_eq!(line.alignment, Some(Alignment::Right), "row {at}");
        for span in line.spans.iter() {
            assert!(
                span.content.as_ref().starts_with('\u{10EEEE}'),
                "row {at}: every grid cell is a placeholder"
            );
            assert_eq!(span.style.fg, Some(Color::Indexed(1)));
        }
        assert!(matches!(target, Some(Target::Image(1))), "row {at}");
    }
    let (caption, target) = &rows[10];
    assert_eq!(caption.alignment, Some(Alignment::Right));
    assert_eq!(caption.to_string(), "▣ a… · 1280×800");
    assert_eq!(format::width(&caption.to_string()), 15);
    assert!(matches!(target, Some(Target::Image(1))));
    // The bubble grid matches the ledger grid at the same conversation
    // width: the shared 40%/12/proportion sizing (`docs/tui.md`,
    // "Images").
    let mut ledger_out = Rows::default();
    super::ledger_rows(
        &part,
        &inline_layout(&part, (10, 20)),
        80,
        Align::Left { indent: 4 },
        &mut ledger_out,
    );
    let (ledger_rows, _) = ledger_out.into_parts();
    assert_eq!(ledger_rows.len(), rows.len());
    for (bubble, ledger) in rows.iter().take(10).zip(ledger_rows.iter()) {
        assert_eq!(
            bubble.0.spans.len() + 1,
            ledger.0.spans.len(),
            "the same grid, without the ledger indent"
        );
    }
}

#[test]
fn a_bubble_caption_shortens_its_name_first() {
    // The label is 18 columns wide: at the edge it draws whole, a
    // column under it shortens the name with the dimensions kept, and
    // wider blocks change nothing. Past the suffix the line is cut
    // like any other text.
    let part = sized(1280, 800);
    let layout = Layout::default();
    for (block, text) in [
        (18u16, "▣ a.png · 1280×800"),
        (17u16, "▣ a.p… · 1280×800"),
        (19u16, "▣ a.png · 1280×800"),
        (7u16, "▣ a.pn…"),
    ] {
        let mut out = Rows::default();
        super::rows(&part, &layout, 80, Align::Right, Some(block), &mut out);
        let (rows, _) = out.into_parts();
        assert_eq!(rows.len(), 1, "block {block}");
        assert_eq!(rows[0].0.to_string(), text, "block {block}");
        assert_eq!(rows[0].0.alignment, Some(Alignment::Right), "block {block}");
        assert_eq!(
            format::width(&rows[0].0.to_string()),
            usize::from(block.min(18)),
            "block {block}"
        );
    }
}

#[test]
fn a_bubble_caption_keeps_its_dimensions() {
    // The dimensions survive a long filename inside a short bubble,
    // and the line never exceeds its block (`docs/tui.md`, "Images").
    let long =
        super::Part::from_content(&session(), &image("artifacts/i_0123456789abcdef.png", 1, 1))
            .expect("the image is kept");
    for (part, max, text, link) in [
        (&sized(1280, 800), 18, "▣ a.png · 1280×800", "▣ a.png"),
        (&sized(1280, 800), 17, "▣ a.p… · 1280×800", "▣ a.p…"),
        (&sized(1280, 800), 14, "▣ … · 1280×800", "▣ …"),
        (&sized(1280, 800), 7, "▣ a.pn…", "▣ a.png"),
        (&sized(1280, 800), 3, "▣ …", "▣ a.png"),
        (&long, 15, "▣ i_0123… · 1×1", "▣ i_0123…"),
    ] {
        let (caption, name) = super::bubble_caption(part, max);
        assert_eq!(caption, text, "max {max}");
        assert_eq!(name, link, "max {max}");
        assert!(
            format::width(&caption) <= max,
            "max {max}: the line stays inside its block"
        );
    }
    let (caption, _) = super::bubble_caption(&long, 15);
    assert!(
        caption.contains("· 1×1"),
        "the dimensions survive a long filename: {caption}"
    );
}

#[test]
fn a_viewer_only_bubble_image_keeps_its_one_line() {
    // 255 distinct images fill the placeholder colours, so the last
    // notes a viewer-only id past the range.
    let allocator = IdAllocator::default();
    let mut layout = Layout::default();
    layout.set_allocator(allocator);
    for at in 0..255 {
        let part = super::Part::from_content(
            &session(),
            &image(&format!("artifacts/b_{at}.png"), 1280, 800),
        )
        .expect("the image is kept");
        layout.note(&part);
    }
    let part = super::Part::from_content(&session(), &image("artifacts/b_254.png", 1280, 800))
        .expect("the image is kept");
    let id = layout.id(&session(), "artifacts/b_254.png");
    assert!(matches!(id, Some(id) if id > 255));
    layout.sizing = super::Sizing::Inline { cell: (10, 20) };
    let mut out = Rows::default();
    super::rows(&part, &layout, 80, Align::Right, Some(20), &mut out);
    let (rows, _) = out.into_parts();
    assert_eq!(rows.len(), 1, "no grid past the range");
    assert_eq!(rows[0].0.to_string(), "▣ b_254.… · 1280×800");
    assert_eq!(rows[0].0.alignment, Some(Alignment::Right));
    assert_eq!(rows[0].1, id.map(Target::Image));
    assert!(
        rows[0]
            .0
            .spans
            .iter()
            .all(|span| !span.content.as_ref().starts_with('\u{10EEEE}')),
        "no placeholders past the range"
    );
}

#[test]
fn a_non_png_bubble_stays_its_one_line() {
    // Only PNG draws inline (ticket #687 Part 2b): a JPEG in a bubble
    // stays its one clickable line, cut to the block width.
    let jpeg = ContentPart::Image {
        path: "artifacts/a.jpg".to_owned(),
        mime_type: "image/jpeg".to_owned(),
        width: 1280,
        height: 800,
    };
    let part = super::Part::from_content(&session(), &jpeg).expect("the jpeg is kept");
    let mut out = Rows::default();
    super::rows(
        &part,
        &inline_layout(&part, (10, 20)),
        80,
        Align::Right,
        Some(20),
        &mut out,
    );
    let (rows, _) = out.into_parts();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0.to_string(), "▣ a.jpg · 1280×800");
    assert_eq!(rows[0].0.alignment, Some(Alignment::Right));
    assert!(matches!(rows[0].1, Some(Target::Image(1))));
}

#[test]
fn a_failed_bubble_stays_its_one_line() {
    // A failed display falls back to the clickable line: no grid draws
    // for it, and the line is cut to the block width.
    let part = sized(1280, 800);
    let mut layout = inline_layout(&part, (10, 20));
    layout.fail(&session(), "artifacts/a.png");
    let mut out = Rows::default();
    super::rows(&part, &layout, 80, Align::Right, Some(20), &mut out);
    let (rows, _) = out.into_parts();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0.to_string(), "▣ a.png · 1280×800");
    assert!(matches!(rows[0].1, Some(Target::Image(1))));
    assert!(
        rows[0]
            .0
            .spans
            .iter()
            .all(|span| !span.content.as_ref().starts_with('\u{10EEEE}')),
        "no placeholders for a failed image"
    );
}

#[test]
fn a_flat_bubble_stays_its_one_line() {
    // A flat page draws every image as its one line, even sized inline
    // with a block width (`docs/tui.md`, "Screen readers").
    let part = sized(1280, 800);
    let ext = crate::ext::Ext {
        flat: true,
        ..Default::default()
    };
    let mut out = Rows::default().with_seam(ext.bind(
        crate::ext::cache::PageKey {
            session: contract::SessionId("s".into()),
            index: 0,
        },
        crate::ext::SessionCtx {
            session_id: contract::SessionId("s".into()),
            halves: None,
        },
    ));
    super::rows(
        &part,
        &inline_layout(&part, (10, 20)),
        80,
        Align::Right,
        Some(20),
        &mut out,
    );
    let (rows, _) = out.into_parts();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0.to_string(), "▣ a.png · 1280×800");
    assert!(matches!(rows[0].1, Some(Target::Image(1))));
}

#[test]
fn an_unnoted_bubble_stays_its_one_line_without_a_target() {
    // No id, no target: the line still draws, cut to the block width.
    let part = sized(1280, 800);
    let layout = Layout {
        sizing: super::Sizing::Inline { cell: (10, 20) },
        ..Default::default()
    };
    let mut out = Rows::default();
    super::rows(&part, &layout, 80, Align::Right, Some(7), &mut out);
    let (rows, _) = out.into_parts();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0.to_string(), "▣ a.pn…");
    assert_eq!(rows[0].1, None);
}

#[test]
fn an_unnoted_part_falls_back_to_its_one_line() {
    let part = sized(1280, 800);
    let layout = Layout {
        sizing: super::Sizing::Inline { cell: (10, 20) },
        ..Default::default()
    };
    let mut out = Rows::default();
    super::ledger_rows(&part, &layout, 80, Align::Left { indent: 4 }, &mut out);
    let (rows, _) = out.into_parts();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0.to_string(), "    ▣ a.png · 1280×800");
}

#[test]
fn a_zero_grid_draws_only_its_caption() {
    let part = sized(0, 800);
    let mut out = Rows::default();
    super::ledger_rows(
        &part,
        &inline_layout(&part, (10, 20)),
        80,
        Align::Left { indent: 4 },
        &mut out,
    );
    let (rows, _) = out.into_parts();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0.to_string(), "    ▣ a.png · 0×800");
}

/// A part with `width` by `height` logged pixels.
fn sized(width: u32, height: u32) -> super::Part {
    super::Part::from_content(&session(), &image("artifacts/a.png", width, height))
        .expect("the image is kept")
}

#[test]
fn part_keeps_its_mime_type() {
    assert_eq!(sized(10, 20).mime_type, "image/png");
    let jpeg = ContentPart::Image {
        path: "artifacts/a.jpg".to_owned(),
        mime_type: "image/jpeg".to_owned(),
        width: 10,
        height: 20,
    };
    assert_eq!(
        super::Part::from_content(&session(), &jpeg)
            .expect("the jpeg is kept")
            .mime_type,
        "image/jpeg"
    );
}

#[test]
fn the_registry_never_hands_out_31() {
    let mut layout = Layout::default();
    let mut ids = Vec::new();
    for at in 0..35 {
        let part =
            super::Part::from_content(&session(), &image(&format!("artifacts/i_{at}.png"), 10, 20))
                .expect("the image is kept");
        ids.push(layout.note(&part));
    }
    // Id 31 is the graphics probe's: the 31st image takes 32, and every
    // id stays unique.
    assert!(
        !ids.contains(&31),
        "the probe's id is never an image: {ids:?}"
    );
    assert_eq!(ids[30], 32);
    assert_eq!(ids[31], 33);
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), 35);
}

#[test]
fn ids_stop_at_255_and_later_images_keep_their_one_line() {
    let allocator = IdAllocator::default();
    let mut layout = Layout::default();
    layout.set_allocator(allocator.clone());
    let mut ids = Vec::new();
    for at in 0..300 {
        let part =
            super::Part::from_content(&session(), &image(&format!("artifacts/i_{at}.png"), 10, 20))
                .expect("the image is kept");
        ids.push(layout.note(&part));
    }
    // Every image keeps its own nonzero id. Which ids may style
    // placeholder cells is `placeholder_eligible`: ids 1 through 255
    // but never the graphics probe's 31 (`docs/tui.md`, "Images"). A
    // past-range id keeps its unique viewer target without drawing
    // placeholders or entering the display-fetch path: the caption
    // click opens it (`docs/tui.md`, "Images").
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), 300, "no id repeats");
    assert!(!sorted.contains(&0), "every image is noted");
    assert!(!sorted.contains(&31), "the probe's id is never an image");
    let eligible: Vec<u32> = sorted
        .iter()
        .copied()
        .filter(|id| super::placeholder_eligible(*id))
        .collect();
    assert!(!eligible.is_empty(), "some ids name placeholder colours");
    assert!(
        eligible.iter().all(|id| *id <= 255),
        "eligible ids stay in the placeholder range"
    );
    assert_eq!(eligible.first(), Some(&1));
    assert_eq!(eligible.last(), Some(&255));
    let viewer: Vec<u32> = sorted
        .iter()
        .copied()
        .filter(|id| !super::placeholder_eligible(*id))
        .collect();
    assert!(!viewer.is_empty(), "past the range ids stay unique");
    assert!(
        viewer.iter().all(|id| *id > 255),
        "viewer-only ids never style placeholder cells"
    );
    assert_eq!(viewer.first(), Some(&256));
    // A past-range part draws its one line with its own target, never
    // a placeholder grid.
    let last = super::Part::from_content(&session(), &image("artifacts/i_299.png", 10, 20))
        .expect("the image is kept");
    let last_id = layout.id(&session(), "artifacts/i_299.png");
    assert!(matches!(last_id, Some(id) if id > 255));
    layout.sizing = super::Sizing::Inline { cell: (10, 20) };
    let mut out = Rows::default();
    super::ledger_rows(&last, &layout, 80, Align::Left { indent: 4 }, &mut out);
    let (rows, _) = out.into_parts();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0.to_string(), "    ▣ i_299.png · 10×20");
    assert_eq!(rows[0].1, last_id.map(Target::Image));
    assert!(
        rows[0]
            .0
            .spans
            .iter()
            .all(|span| !span.content.as_ref().starts_with('\u{10EEEE}')),
        "no placeholders past the range"
    );

    // New page layouts share the allocator: a later image cannot
    // receive a previous image's id after a refold.
    let later = super::Part::from_content(&other(), &image("artifacts/after.png", 10, 20))
        .expect("the later image is kept");
    let mut next_layout = Layout::default();
    next_layout.set_allocator(allocator);
    let later_id = next_layout.note(&later);
    assert_ne!(later_id, 0, "a later image is noted");
    assert!(
        !sorted.contains(&later_id),
        "a later image never reuses an id"
    );
    next_layout.sizing = super::Sizing::Inline { cell: (10, 20) };
    let mut out = Rows::default();
    super::ledger_rows(
        &later,
        &next_layout,
        80,
        Align::Left { indent: 4 },
        &mut out,
    );
    let (rows, _) = out.into_parts();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0.to_string(), "    ▣ after.png · 10×20");
    assert_eq!(rows[0].1, Some(Target::Image(later_id)));
}

#[test]
fn only_ids_1_through_255_but_not_31_style_placeholders() {
    // At, just below and just above each edge: 0 names no image, 31 is
    // the graphics probe's, and past 255 ids stay viewer-only.
    for id in [1, 2, 30, 32, 254, 255] {
        assert!(super::placeholder_eligible(id), "{id} styles placeholders");
    }
    for id in [0, 31, 256, 257, u32::MAX] {
        assert!(!super::placeholder_eligible(id), "{id} never styles cells");
    }
}

#[test]
fn u32_max_is_handed_out_once_then_exhaustion_returns_zero() {
    let allocator = IdAllocator::default();
    allocator.lock().next = u32::MAX - 1;
    let mut layout = Layout::default();
    layout.set_allocator(allocator);
    let max = super::Part::from_content(&session(), &image("artifacts/max.png", 10, 20))
        .expect("the image is kept");
    assert_eq!(layout.note(&max), u32::MAX);
    assert_eq!(layout.id(&session(), "artifacts/max.png"), Some(u32::MAX));
    // After `u32::MAX` allocation notes nothing, returning the no-id
    // sentinel without repeating an id.
    for path in ["artifacts/after.png", "artifacts/again.png"] {
        let part =
            super::Part::from_content(&session(), &image(path, 10, 20)).expect("the image is kept");
        assert_eq!(layout.note(&part), 0, "{path} is past exhaustion");
        assert_eq!(layout.id(&session(), path), None, "{path} stays unnoted");
    }
    let handed: Vec<u32> = layout.ids.values().copied().collect();
    assert_eq!(
        handed.iter().filter(|id| **id == u32::MAX).count(),
        1,
        "`u32::MAX` is returned at most once"
    );
}

#[test]
fn ids_follow_image_identity_across_layouts_and_change_by_generation() {
    let allocator = IdAllocator::default();
    let first = super::Part::from_content(&session(), &image("artifacts/a.png", 10, 20))
        .expect("the first image is kept");
    let other_session = super::Part::from_content(&other(), &image("artifacts/a.png", 10, 20))
        .expect("the other session's image is kept");

    let mut first_layout = Layout::default();
    first_layout.set_allocator(allocator.clone());
    let first_id = first_layout.note(&first);
    let mut refolded = Layout::default();
    refolded.set_allocator(allocator.clone());
    assert_eq!(refolded.note(&first), first_id, "a refold keeps its id");
    assert_ne!(
        refolded.note(&other_session),
        first_id,
        "distinct sessions never share an id"
    );

    allocator.advance_generation();
    let mut next_generation = Layout::default();
    next_generation.set_allocator(allocator.clone());
    assert_ne!(
        next_generation.note(&first),
        first_id,
        "an attached generation never reuses an id"
    );
    allocator.advance_generation();
    assert_ne!(
        first_layout.note(&first),
        first_id,
        "an existing layout moves to its new generation's id"
    );
}

#[test]
fn grid_keeps_a_small_image_at_its_natural_cells() {
    // A small image draws as it is: no upscale past its natural cells.
    assert_eq!(super::grid(&sized(20, 40), 60, (10, 20)), (2, 2));
    assert_eq!(super::grid(&sized(25, 45), 60, (10, 20)), (2, 2));
    assert_eq!(super::grid(&sized(100, 100), 80, (10, 20)), (10, 5));
    assert_eq!(super::grid(&sized(1, 1), 60, (10, 20)), (1, 1));
}

#[test]
fn grid_caps_at_forty_percent_and_twelve_rows() {
    // 1280 by 800 at 80 columns: the width binds at 32 columns, and the
    // height follows at 10 rows.
    assert_eq!(super::grid(&sized(1280, 800), 80, (10, 20)), (32, 10));
    // A tall image binds the rows instead: 1 column by 12 rows.
    assert_eq!(super::grid(&sized(100, 2000), 80, (10, 20)), (1, 12));
    // A wide image binds the columns, holding at least one row.
    assert_eq!(super::grid(&sized(4000, 100), 80, (10, 20)), (32, 1));
}

#[test]
fn grid_binds_on_the_narrower_box() {
    // At equality the width binds: 32 columns by 12 rows.
    assert_eq!(super::grid(&sized(1280, 960), 80, (10, 20)), (32, 12));
    // A pixel wider binds the width with fewer rows.
    assert_eq!(super::grid(&sized(1281, 960), 80, (10, 20)), (32, 11));
    // A pixel narrower binds the height with fewer columns.
    assert_eq!(super::grid(&sized(1279, 960), 80, (10, 20)), (31, 12));
}

#[test]
fn grid_clamps_to_297_placeholders() {
    // 1920 by 1080 at 84 columns: 40% is 33 columns, the height follows
    // at 9 rows, 33 by 9 placeholders.
    let (cols, rows) = super::grid(&sized(1920, 1080), 84, (10, 20));
    assert_eq!((cols, rows), (33, 9));
    assert_eq!(u32::from(cols) * u32::from(rows), 297);
}

#[test]
fn grid_stays_inside_the_diacritic_table() {
    // Past 160 columns 40% tops 64: the diacritic table's edge.
    assert_eq!(super::grid(&sized(2000, 100), 160, (10, 20)), (64, 1));
    assert_eq!(super::grid(&sized(2000, 100), 159, (10, 20)), (63, 1));
}

#[test]
fn grid_draws_nothing_without_size_columns_or_cells() {
    for (width, height, columns, cell) in [
        (0, 800, 80, (10, 20)),
        (1280, 0, 80, (10, 20)),
        (0, 0, 80, (10, 20)),
        (1280, 800, 0, (10, 20)),
        (1280, 800, 80, (0, 20)),
        (1280, 800, 80, (10, 0)),
    ] {
        assert_eq!(
            super::grid(&sized(width, height), columns, cell),
            (0, 0),
            "{width} by {height} at {columns} with {cell:?}"
        );
    }
}

#[test]
fn rows_ignores_the_inline_sizing() {
    // Left rows never go inline through `rows`: even sized inline, a
    // noted PNG stays its one line.
    let part = sized(1280, 800);
    let mut out = Rows::default();
    super::rows(
        &part,
        &inline_layout(&part, (10, 20)),
        80,
        Align::Left { indent: 4 },
        None,
        &mut out,
    );
    let (rows, _) = out.into_parts();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0.to_string(), "    ▣ a.png · 1280×800");
}

#[test]
fn a_failed_image_draws_its_one_line() {
    // A failed display falls back to the clickable line: the grid never
    // draws for it, and the target stays the image's.
    let part = sized(1280, 800);
    let mut layout = inline_layout(&part, (10, 20));
    layout.fail(&session(), "artifacts/a.png");
    let mut out = Rows::default();
    super::ledger_rows(&part, &layout, 80, Align::Left { indent: 4 }, &mut out);
    let (rows, _) = out.into_parts();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0.to_string(), "    ▣ a.png · 1280×800");
    assert!(matches!(rows[0].1, Some(Target::Image(1))));
    // Another path still draws inline: the failure names one image.
    let other = super::Part::from_content(&session(), &image("artifacts/b.png", 1280, 800))
        .expect("the image is kept");
    layout.note(&other);
    let mut out = Rows::default();
    super::ledger_rows(&other, &layout, 80, Align::Left { indent: 4 }, &mut out);
    let (rows, _) = out.into_parts();
    assert_eq!(rows.len(), 11, "the other image still draws its grid");
}

#[test]
fn a_flat_seam_draws_the_one_line() {
    // A flat page draws every image as its one line: the rows' seam
    // says flat even where the sizing is inline.
    let part = sized(1280, 800);
    let ext = crate::ext::Ext {
        flat: true,
        ..Default::default()
    };
    let mut out = Rows::default().with_seam(ext.bind(
        crate::ext::cache::PageKey {
            session: contract::SessionId("s".into()),
            index: 0,
        },
        crate::ext::SessionCtx {
            session_id: contract::SessionId("s".into()),
            halves: None,
        },
    ));
    super::ledger_rows(
        &part,
        &inline_layout(&part, (10, 20)),
        80,
        Align::Left { indent: 4 },
        &mut out,
    );
    let (rows, _) = out.into_parts();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0.to_string(), "    ▣ a.png · 1280×800");
    assert!(matches!(rows[0].1, Some(Target::Image(1))));
}

#[test]
fn grid_handles_the_largest_logged_size() {
    // The widest logged size at the widest columns: the products stay
    // inside 64 bits, and both caps still bind. At 65535 columns 40%
    // clamps to the diacritic table's 64, and the row cap binds below
    // the column cap.
    assert_eq!(
        super::grid(&sized(u32::MAX, u32::MAX), u16::MAX, (10, 20)),
        (24, 12)
    );
    assert_eq!(
        super::grid(&sized(u32::MAX, u32::MAX), u16::MAX, (1, 1)),
        (12, 12)
    );
    // A maximum width with a small height still draws: the row cap
    // binds, and neither side runs past the diacritic table.
    assert_eq!(
        super::grid(&sized(u32::MAX, 1), u16::MAX, (10, 20)),
        (64, 1)
    );
}
