//! Tests for one image part's line: what is kept, what the line
//! names, and that it always draws one row. Ledger rows draw the
//! inline grid with its caption; the prompt and steering forms stay
//! the one line.

use contract::shapes::ContentPart;
use ratatui::layout::Alignment;
use ratatui::style::Modifier;
use ratatui::text::Line;

use super::{Align, Layout, parts};
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
    super::rows(&part, &layout, columns, Align::Left { indent: 0 }, &mut out);
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
    super::rows(&part, &layout, 40, Align::Left { indent: 0 }, &mut out);
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
    super::rows(&part, &layout, columns, Align::Left { indent }, &mut out);
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
    super::rows(&part, &layout, 60, Align::Right, &mut out);
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
    super::rows(&second, &layout, 60, Align::Left { indent: 0 }, &mut out);
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
fn the_bubble_and_steering_stay_one_line_when_sized_inline() {
    let part = sized(1280, 800);
    let layout = inline_layout(&part, (10, 20));
    for align in [Align::Right, Align::Left { indent: 0 }] {
        let mut out = Rows::default();
        super::ledger_rows(&part, &layout, 80, align, &mut out);
        let (rows, _) = out.into_parts();
        assert_eq!(rows.len(), 1, "the bubble and steering stay one line");
        assert!(
            rows[0]
                .0
                .spans
                .iter()
                .all(|span| !span.content.as_ref().starts_with('\u{10EEEE}')),
            "no placeholders outside a ledger row"
        );
    }
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
fn grid_keeps_a_small_image_at_its_natural_cells() {
    // A small image draws as it is: no upscale past its natural cells.
    assert_eq!(super::grid(&sized(20, 40), 60, (10, 20)), (2, 2));
    assert_eq!(super::grid(&sized(25, 45), 60, (10, 20)), (2, 2));
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
    // The prompt and steering forms call `rows`: even sized inline, a
    // noted PNG stays its one line.
    let part = sized(1280, 800);
    let mut out = Rows::default();
    super::rows(
        &part,
        &inline_layout(&part, (10, 20)),
        80,
        Align::Left { indent: 4 },
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
    let mut out = Rows::default().with_seam(ext.bind(0));
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
