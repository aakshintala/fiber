//! The images of a tool result, as `anthropic-messages` carries them: inside
//! the `tool_result`, as `image` blocks after the text. The bytes are the
//! stored file's, never re-encoded or resized (`docs/model-routing.md`,
//! "Image limits"), so a resume sends the same bytes.

use std::path::{Component, Path};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use contract::provider::ImageRef;
use serde_json::{Value, json};

/// A `tool_result`'s `content`: the plain text when no image can be sent,
/// otherwise a text block, when there is text, then one `image` block for each
/// stored file read from `session_dir`. A file that cannot be read is not
/// sent; the text gets one line saying so, so the model is never left to
/// believe it saw the image.
//
// debt: an image is sent to a model that cannot take one, a model's declared `input` kinds reach the request, #603's design follow-up
pub(crate) fn content(text: &str, images: &[ImageRef], session_dir: &Path) -> Value {
    if images.is_empty() {
        return json!(text);
    }
    let mut text = text.to_owned();
    let mut blocks = Vec::new();
    for image in images {
        match encoded(image, session_dir) {
            Some(data) => blocks.push(json!({
                "type": "image",
                "source": {"type": "base64", "media_type": image.mime_type, "data": data},
            })),
            None => {
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(&format!("[Image {} could not be read.]", image.path));
            }
        }
    }
    if blocks.is_empty() {
        return json!(text);
    }
    if !text.is_empty() {
        blocks.insert(0, json!({"type": "text", "text": text}));
    }
    Value::Array(blocks)
}

/// The file's bytes as base64, or `None` when its path leaves the session
/// directory or the file cannot be read.
fn encoded(image: &ImageRef, session_dir: &Path) -> Option<String> {
    let relative = Path::new(&image.path);
    if !relative
        .components()
        .all(|part| matches!(part, Component::Normal(_)))
    {
        return None;
    }
    let bytes = std::fs::read(session_dir.join(relative)).ok()?;
    Some(STANDARD.encode(bytes))
}

#[cfg(test)]
#[path = "anthropic_images_tests.rs"]
mod tests;
