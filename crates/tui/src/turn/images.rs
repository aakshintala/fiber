//! A prompt's and a steering message's images draw as one clickable line
//! each; a call's images draw inline where the terminal speaks kitty's
//! graphics protocol (`docs/tui.md`, "Images").

use contract::shapes::ContentPart;

use crate::app::text_of;
use crate::image;
use crate::rows::Rows;

/// The ledger's indent, as `format.rs` draws it: a call's images sit
/// under its row.
const LEDGER_INDENT: u16 = 4;

/// A prompt that started a turn: its text and its images, in order.
#[derive(Debug, Clone)]
pub(crate) struct Prompt {
    pub(crate) text: String,
    pub(crate) images: Vec<image::Part>,
}

/// The prompt `content` started in `session`: its text, and every image
/// part it carried, in order.
pub(crate) fn prompt_of(session: &contract::SessionId, content: &[ContentPart]) -> Prompt {
    Prompt {
        text: text_of(content),
        images: image::parts(session, content),
    }
}

/// The prompt's bubble, then each image's rows under its text,
/// right-aligned with the bubble: the inline grid with its caption
/// where the terminal speaks kitty's graphics protocol, else each
/// image's one line. An image-only prompt draws only its lines: no
/// empty bubble row, pending the surveyor ruling on that shape.
pub(crate) fn bubble_rows(prompt: &Prompt, width: u16, layout: &image::Layout, out: &mut Rows) {
    crate::bubble::rows(&prompt.text, width, out);
    let block = bubble_block_width(&prompt.text, width);
    for part in &prompt.images {
        image::rows(part, layout, width, image::Align::Right, block, out);
    }
}

/// The prompt text's actual wrapped bubble block width, as
/// `bubble::rows` computes it: `None` for an empty or missing bubble,
/// `Some(columns)` below four columns where the bubble cap's
/// `clamp(4, columns)` cannot run, else the widest wrapped row plus
/// pads capped at 72% of the columns.
fn bubble_block_width(text: &str, columns: u16) -> Option<u16> {
    if text.trim().is_empty() || columns == 0 {
        return None;
    }
    if columns < 4 {
        return Some(columns);
    }
    let cap = usize::from(columns).saturating_mul(72) / 100;
    let cap = cap.clamp(4, usize::from(columns));
    let wrapped = crate::format::wrap_joined(text, cap.saturating_sub(3));
    let widest = wrapped
        .iter()
        .map(|(row, _)| crate::format::width(row))
        .max()
        .unwrap_or(0);
    u16::try_from(widest.saturating_add(3).min(cap)).ok()
}

/// Each image's rows at the ledger's indent, under a call's row: the
/// placeholder grid with its caption where the sizing is inline, its
/// one line otherwise.
pub(crate) fn call_rows(
    images: &[image::Part],
    width: u16,
    layout: &image::Layout,
    out: &mut Rows,
) {
    for part in images {
        image::ledger_rows(
            part,
            layout,
            width,
            image::Align::Left {
                indent: LEDGER_INDENT,
            },
            out,
        );
    }
}

/// Each image's line under the steering message's text.
pub(crate) fn steer_rows(
    images: &[image::Part],
    width: u16,
    layout: &image::Layout,
    out: &mut Rows,
) {
    for part in images {
        image::rows(
            part,
            layout,
            width,
            image::Align::Left { indent: 0 },
            None,
            out,
        );
    }
}

#[cfg(test)]
#[path = "images_tests.rs"]
mod tests;
