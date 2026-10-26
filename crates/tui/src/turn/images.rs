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

/// The prompt's bubble, then each image's line under its text,
/// right-aligned with the bubble. An image-only prompt draws only
/// its lines: no empty bubble row.
pub(crate) fn bubble_rows(prompt: &Prompt, width: u16, layout: &image::Layout, out: &mut Rows) {
    crate::bubble::rows(&prompt.text, width, out);
    for part in &prompt.images {
        image::rows(part, layout, width, image::Align::Right, out);
    }
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
        image::rows(part, layout, width, image::Align::Left { indent: 0 }, out);
    }
}

#[cfg(test)]
#[path = "images_tests.rs"]
mod tests;
