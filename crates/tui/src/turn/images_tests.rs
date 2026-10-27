//! Tests for a prompt's, a call's and a steering message's image
//! rows: kept in order, under the text at their indent.

use contract::shapes::ContentPart;

use super::{bubble_rows, call_rows, prompt_of, steer_rows};
use crate::app::Target;
use crate::image::Layout;
use crate::rows::Rows;

fn session() -> contract::SessionId {
    contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned())
}

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
            session: session(),
            path: (*path).to_owned(),
            mime_type: "image/png".to_owned(),
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
    let prompt = prompt_of(
        &session(),
        &[
            text("look"),
            image("artifacts/b.png"),
            text("closer"),
            image("artifacts/a.png"),
        ],
    );
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
        crate::image::parts(&session(), &content),
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
    assert!(shown.contains(&" ▣ s.png · 1280×800".to_owned()));
}

#[test]
fn bubble_rows_put_each_image_line_under_the_text_right_aligned() {
    use ratatui::layout::Alignment;
    let prompt = prompt_of(&session(), &[text("look"), image("artifacts/b.png")]);
    let layout = noted(&["artifacts/b.png"]);
    let mut out = Rows::default();
    bubble_rows(&prompt, 60, &layout, &mut out);
    let (rows, _) = out.into_parts();
    assert!(!rows.is_empty());
    let (line, target) = rows.last().unwrap_or_else(|| panic!("an image row"));
    // "look" wraps to a block 7 wide, so the 19-wide caption is cut.
    assert_eq!(line.to_string(), "▣ b.pn…");
    assert_eq!(line.alignment, Some(Alignment::Right));
    assert!(matches!(target, Some(Target::Image(_))));
}

#[test]
fn call_rows_sit_at_the_ledger_indent() {
    let layout = noted(&["artifacts/a.png"]);
    let mut out = Rows::default();
    call_rows(
        &[crate::image::Part {
            session: session(),
            path: "artifacts/a.png".to_owned(),
            mime_type: "image/png".to_owned(),
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

/// A layout sizing its images inline with `cell`.
fn inline_layout(paths: &[&str], cell: (u16, u16)) -> Layout {
    let mut layout = noted(paths);
    layout.sizing = crate::image::Sizing::Inline { cell };
    layout
}

#[test]
fn call_rows_draw_the_inline_grid() {
    let layout = inline_layout(&["artifacts/a.png"], (10, 20));
    let mut out = Rows::default();
    call_rows(
        &[crate::image::Part {
            session: session(),
            path: "artifacts/a.png".to_owned(),
            mime_type: "image/png".to_owned(),
            width: 1280,
            height: 800,
        }],
        60,
        &layout,
        &mut out,
    );
    let (rows, _) = out.into_parts();
    // 1280 by 800 at 60 columns with a 10 by 20 cell: 24 by 7
    // placeholders, then the caption line.
    assert_eq!(rows.len(), 8);
    assert!(
        rows[0]
            .0
            .spans
            .iter()
            .any(|span| span.content.as_ref().starts_with('\u{10EEEE}')),
        "the grid draws first"
    );
    assert_eq!(rows[7].0.to_string(), "    ▣ a.png · 1280×800");
    assert!(matches!(rows[7].1, Some(Target::Image(_))));
}

#[test]
fn bubble_rows_draw_the_shared_grid_right_aligned() {
    use ratatui::layout::Alignment;
    // The short prompt supplies a text bubble. The PNG uses the shared
    // conversation-width grid; its caption stays within the bubble text
    // width. `docs/tui.md`, "Images", sets the 40%-width and 12-row
    // caps and says a click opens the viewer.
    let prompt = prompt_of(&session(), &[text("look"), image("artifacts/b.png")]);
    let layout = inline_layout(&["artifacts/b.png"], (10, 20));
    let mut out = Rows::default();
    bubble_rows(&prompt, 60, &layout, &mut out);
    let (rows, _) = out.into_parts();
    let grid_rows: Vec<_> = rows
        .iter()
        .filter(|(line, _)| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().starts_with('\u{10EEEE}'))
        })
        .collect();
    assert!(!grid_rows.is_empty(), "the inline grid is visible");
    assert!(
        grid_rows.len() <= 12,
        "the grid stays within the documented cap"
    );
    for (line, target) in &grid_rows {
        assert!(
            line.spans.len() <= 60 * 40 / 100,
            "the grid stays within 40% width"
        );
        assert_eq!(line.alignment, Some(Alignment::Right));
        assert!(
            line.spans
                .iter()
                .all(|span| span.content.as_ref().starts_with('\u{10EEEE}')),
            "every grid cell is a placeholder"
        );
        assert!(matches!(target, Some(Target::Image(_))));
    }
    let (caption, target) = rows
        .iter()
        .find(|(line, _)| line.to_string().contains('▣'))
        .unwrap_or_else(|| panic!("a caption row"));
    assert_eq!(caption.alignment, Some(Alignment::Right));
    assert!(crate::format::width(&caption.to_string()) <= 7);
    assert!(matches!(target, Some(Target::Image(_))));
    // The bubble grid matches the call grid at the same conversation
    // width: the shared 40%/12/proportion sizing (`docs/tui.md`,
    // "Images").
    let mut calls = Rows::default();
    call_rows(
        &[crate::image::Part {
            session: session(),
            path: "artifacts/b.png".to_owned(),
            mime_type: "image/png".to_owned(),
            width: 1280,
            height: 800,
        }],
        60,
        &layout,
        &mut calls,
    );
    let (call_rows, _) = calls.into_parts();
    let call_grid: Vec<_> = call_rows
        .iter()
        .filter(|(line, _)| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().starts_with('\u{10EEEE}'))
        })
        .collect();
    assert_eq!(
        grid_rows.len(),
        call_grid.len(),
        "the shared grid has the same height"
    );
    for (bubble, call) in grid_rows.iter().zip(call_grid.iter()) {
        assert_eq!(
            bubble.0.spans.len() + 1,
            call.0.spans.len(),
            "the same grid, without the ledger indent"
        );
    }
}

#[test]
fn a_short_prompt_caps_its_caption_at_its_bubble_block() {
    use ratatui::layout::Alignment;
    // "hi" wraps to a block 5 wide, so the caption ends there even
    // though the conversation is 60 columns wide.
    let prompt = prompt_of(&session(), &[text("hi"), image("artifacts/b.png")]);
    let layout = inline_layout(&["artifacts/b.png"], (10, 20));
    let mut out = Rows::default();
    bubble_rows(&prompt, 60, &layout, &mut out);
    let (rows, _) = out.into_parts();
    let (caption, target) = rows.last().unwrap_or_else(|| panic!("a caption row"));
    assert_eq!(caption.alignment, Some(Alignment::Right));
    assert!(
        crate::format::width(&caption.to_string()) <= 5,
        "the caption stays inside the short bubble: {caption:?}"
    );
    assert!(matches!(target, Some(Target::Image(_))));
}

#[test]
fn bubble_block_width_follows_the_bubble_rows_formula() {
    // At, just below and just above the bubble cap's edge: a short
    // prompt takes its widest row plus pads, a 40-wide prompt takes
    // the cap exactly, and longer prompts stay capped. Below four
    // columns the width is the columns, and an empty prompt takes no
    // width at all.
    assert_eq!(super::bubble_block_width("", 60), None);
    assert_eq!(super::bubble_block_width("   ", 60), None);
    assert_eq!(super::bubble_block_width("hi", 0), None);
    assert_eq!(super::bubble_block_width("hi", 1), Some(1));
    assert_eq!(super::bubble_block_width("hi", 3), Some(3));
    assert_eq!(super::bubble_block_width("hi", 60), Some(5));
    assert_eq!(super::bubble_block_width("look at this", 60), Some(15));
    assert_eq!(super::bubble_block_width(&"x".repeat(40), 60), Some(43));
    assert_eq!(super::bubble_block_width(&"x".repeat(41), 60), Some(43));
    assert_eq!(super::bubble_block_width(&"x".repeat(100), 60), Some(43));
    assert_eq!(super::bubble_block_width("look", 4), Some(4));
}

#[test]
fn bubble_block_width_at_four_columns_measures_a_narrow_bubble() {
    // Kills `replace < with <= in bubble_block_width`: at exactly
    // four columns the width still runs the wrap, it does not take
    // the narrow shortcut. A combining mark is non-empty but takes
    // no cell, so the wrapped block is narrower than the columns.
    assert_eq!(crate::format::width("\u{301}"), 0);
    assert_eq!(super::bubble_block_width("\u{301}", 4), Some(3));
}

#[test]
fn bubble_rows_below_four_columns_draws_without_panicking() {
    // Below four columns the bubble cap's `clamp(4, columns)` cannot
    // run, so the block width is the columns: the rows still draw.
    let prompt = prompt_of(&session(), &[text("look"), image("artifacts/b.png")]);
    let layout = inline_layout(&["artifacts/b.png"], (10, 20));
    let mut out = Rows::default();
    bubble_rows(&prompt, 3, &layout, &mut out);
    let (rows, _) = out.into_parts();
    assert!(rows.len() > 1, "the bubble and its image draw");
    let (caption, target) = rows.last().unwrap_or_else(|| panic!("a caption row"));
    assert!(
        crate::format::width(&caption.to_string()) <= 3,
        "the caption stays inside three columns: {caption:?}"
    );
    assert!(matches!(target, Some(Target::Image(_))));
}

#[test]
fn steer_rows_stay_one_line_when_sized_inline() {
    let layout = inline_layout(&["artifacts/s.png"], (10, 20));
    let mut out = Rows::default();
    steer_rows(
        &[crate::image::Part {
            session: session(),
            path: "artifacts/s.png".to_owned(),
            mime_type: "image/png".to_owned(),
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

#[test]
fn an_image_only_prompt_draws_its_line() {
    // No empty bubble row: the image line is the only row.
    let prompt = prompt_of(&session(), &[image("artifacts/a.png")]);
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
            session: session(),
            path: "artifacts/s.png".to_owned(),
            mime_type: "image/png".to_owned(),
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
