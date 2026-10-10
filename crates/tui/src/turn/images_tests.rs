//! Tests for a prompt's, a call's and a steering message's image
//! rows: kept in order, under the text at their indent.

use contract::shapes::ContentPart;

use super::{bubble_rows, call_rows, prompt_of, steer_rows};
use crate::app::Target;
use crate::image::Layout;
use crate::rows::Rows;

fn text(text: &str) -> ContentPart {
    ContentPart::Text {
        text: text.to_owned(),
    }
}

fn image(path: &str) -> ContentPart {
    ContentPart::Image {
        path: path.to_owned(),
        mime_type: "image/png".to_owned(),
        width: 1280,
        height: 800,
    }
}

/// Notes every image part's path in `layout`, so rows carry targets.
fn noted(paths: &[&str]) -> Layout {
    let mut layout = Layout::default();
    for path in paths {
        layout.note(&crate::image::Part {
            path: (*path).to_owned(),
            width: 1280,
            height: 800,
        });
    }
    layout
}

fn texts(out: Rows) -> Vec<String> {
    let (rows, _) = out.into_parts();
    rows.into_iter().map(|(line, _)| line.to_string()).collect()
}

#[test]
fn a_prompt_keeps_its_images_in_order() {
    let prompt = prompt_of(&[
        text("look"),
        image("artifacts/b.png"),
        text("closer"),
        image("artifacts/a.png"),
    ]);
    assert_eq!(prompt.text, "lookcloser");
    assert_eq!(
        prompt
            .images
            .iter()
            .map(|part| part.path.clone())
            .collect::<Vec<_>>(),
        ["artifacts/b.png", "artifacts/a.png"]
    );
}

#[test]
fn a_steer_keeps_its_images() {
    use jiff::tz::TimeZone;
    // Through the card: the steering text draws, then each image's
    // line under it.
    let mut turn = crate::turn::Turn::new(Vec::new(), 0);
    let content = vec![text("wait"), image("artifacts/s.png")];
    turn.steer(
        crate::app::text_of(&content),
        crate::image::parts(&content),
        0,
    );
    let layout = noted(&["artifacts/s.png"]);
    let mut out = Rows::default();
    turn.rows(
        60,
        &TimeZone::UTC,
        crate::surface::Edges::BOTH,
        &layout,
        &mut out,
    );
    let (rows, _) = out.into_parts();
    let shown: Vec<String> = rows.into_iter().map(|(line, _)| line.to_string()).collect();
    assert!(shown.iter().any(|line| line.contains("wait")));
    assert!(shown.contains(&"▣ s.png · 1280×800".to_owned()));
}

#[test]
fn bubble_rows_put_each_image_line_under_the_text_right_aligned() {
    use ratatui::layout::Alignment;
    let prompt = prompt_of(&[text("look"), image("artifacts/b.png")]);
    let layout = noted(&["artifacts/b.png"]);
    let mut out = Rows::default();
    bubble_rows(&prompt, 60, &layout, &mut out);
    let (rows, _) = out.into_parts();
    assert!(!rows.is_empty());
    let (line, target) = rows.last().unwrap_or_else(|| panic!("an image row"));
    assert_eq!(line.to_string(), "▣ b.png · 1280×800");
    assert_eq!(line.alignment, Some(Alignment::Right));
    assert!(matches!(target, Some(Target::Image(_))));
}

#[test]
fn call_rows_sit_at_the_ledger_indent() {
    let layout = noted(&["artifacts/a.png"]);
    let mut out = Rows::default();
    call_rows(
        &[crate::image::Part {
            path: "artifacts/a.png".to_owned(),
            width: 1280,
            height: 800,
        }],
        60,
        &layout,
        &mut out,
    );
    let (rows, _) = out.into_parts();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0.to_string(), "    ▣ a.png · 1280×800");
    assert!(matches!(rows[0].1, Some(Target::Image(_))));
}

#[test]
fn an_image_only_prompt_draws_its_line() {
    // No empty bubble row: the image line is the only row.
    let prompt = prompt_of(&[image("artifacts/a.png")]);
    assert!(prompt.text.trim().is_empty());
    let layout = noted(&["artifacts/a.png"]);
    let mut out = Rows::default();
    bubble_rows(&prompt, 60, &layout, &mut out);
    let shown = texts(out);
    assert_eq!(shown, ["▣ a.png · 1280×800"]);
}

#[test]
fn steer_rows_draw_each_line_with_no_indent() {
    let layout = noted(&["artifacts/s.png"]);
    let mut out = Rows::default();
    steer_rows(
        &[crate::image::Part {
            path: "artifacts/s.png".to_owned(),
            width: 640,
            height: 480,
        }],
        60,
        &layout,
        &mut out,
    );
    let (rows, _) = out.into_parts();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0.to_string(), "▣ s.png · 640×480");
}
