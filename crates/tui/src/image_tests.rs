//! Tests for one image part's line: what is kept, what the line
//! names, and that it always draws one row.

use contract::shapes::ContentPart;
use ratatui::layout::Alignment;

use super::{Align, Layout, cut, parts};
use crate::app::Target;
use crate::format;
use crate::rows::Rows;

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
    let part = super::Part::from_content(&image("artifacts/i_a.png", 1280, 800));
    assert_eq!(
        part,
        Some(super::Part {
            path: "artifacts/i_a.png".to_owned(),
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
    let kept = parts(&content);
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
        let Some(part) = super::Part::from_content(&image(path, width, height)) else {
            panic!("kept {path}");
        };
        assert_eq!(part.label(), label, "{path}");
    }
}

/// Draws `label`'s part left-aligned at `columns`, returning its rows.
fn drawn(label: &str, columns: u16) -> Vec<(String, Option<Target>)> {
    let mut layout = Layout::default();
    let part = super::Part {
        path: "artifacts/a.png".to_owned(),
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
    let Some(part) = super::Part::from_content(&image(&path, 1280, 800)) else {
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
fn cut_keeps_a_label_that_fits() {
    assert_eq!(cut("▣ a.png · 1×1", 40), "▣ a.png · 1×1");
}

#[test]
fn a_zero_width_line_draws_empty() {
    assert_eq!(cut("▣ a.png · 1×1", 0), "");
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
        path: "artifacts/a.png".to_owned(),
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
