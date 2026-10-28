//! A prompt's and a steering message's images draw as one clickable line
//! each; a call's images draw inline where the terminal speaks kitty's
//! graphics protocol (`docs/tui.md`, "Images").

use contract::shapes::ContentPart;

use crate::app::text_of;
use crate::ext::Input;
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
/// image's one line. An image-only prompt's bubble holds only its image
/// rows and caption, with no empty text row. A bound `prompt` renderer
/// replaces the bubble and image lines: one image keeps its row-level
/// target on each replacement line; several images have placement-specific
/// targets, which replacement lines do not carry.
pub(crate) fn bubble_rows(prompt: &Prompt, width: u16, layout: &image::Layout, out: &mut Rows) {
    let flat = out.seam.as_ref().is_some_and(|seam| seam.flat);
    let input = crate::ext::Prompt {
        text: &prompt.text,
        images: prompt
            .images
            .iter()
            .map(|part| crate::ext::PromptImage {
                path: &part.path,
                width: part.width,
                height: part.height,
            })
            .collect(),
        flat,
    };
    let wrapped = Input::Prompt(input);
    let builtin = |input: &Input<'_>, w: u16| {
        if !matches!(input, Input::Prompt(_)) {
            return Vec::new();
        }
        let mut scratch = Rows::default();
        crate::bubble::rows(&prompt.text, w, &mut scratch);
        let block = bubble_block_width(&prompt.text, w);
        for part in &prompt.images {
            image::rows(part, layout, w, image::Align::Right, block, &mut scratch);
        }
        let (rows, _) = scratch.into_parts();
        rows.into_iter()
            .map(|(line, _)| crate::ext::spans::from_ratatui(&line.spans))
            .collect()
    };
    let rendered = match out.fills_for("prompt") {
        crate::ext::SlotSelection::Draw { extension } => {
            out.ext_lines("prompt", &extension, &wrapped, width, &builtin)
        }
        crate::ext::SlotSelection::Absent | crate::ext::SlotSelection::Suppressed => None,
    };
    match rendered {
        Some(fitted) => {
            let target = single_target(prompt, layout);
            for line in fitted.iter() {
                out.push_fitted(line, target);
            }
        }
        None => {
            crate::bubble::rows(&prompt.text, width, out);
            let block = bubble_block_width(&prompt.text, width);
            for part in &prompt.images {
                image::rows(part, layout, width, image::Align::Right, block, out);
            }
        }
    }
}

/// The one image target when `prompt` holds a single noted image, else
/// none: a single image's target is row-level, while several images
/// place one target per image line.
fn single_target(prompt: &Prompt, layout: &image::Layout) -> Option<crate::app::Target> {
    let [part] = prompt.images.as_slice() else {
        return None;
    };
    layout
        .id(&part.session, &part.path)
        .map(crate::app::Target::Image)
}

/// The prompt bubble's width for image rows: `Some(columns)` when its
/// text is empty, the actual wrapped block width when text is present,
/// and `None` only at zero columns. Below four columns it uses the
/// columns directly, where the bubble cap's `clamp(4, columns)` cannot
/// run; otherwise it uses the widest wrapped row plus pads capped at
/// 72% of the columns.
fn bubble_block_width(text: &str, columns: u16) -> Option<u16> {
    if columns == 0 {
        return None;
    }
    if text.trim().is_empty() {
        return Some(columns);
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
