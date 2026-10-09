//! The images of a tool result, shared by all four protocols: one function
//! turns (text, images, session_dir, text_only) into the final text plus the
//! encoded images, and each protocol only wraps them in its own shape; one
//! function measures a request body's size and whether it carried an image.
//! The bytes are the stored file's, never re-encoded or resized
//! (`docs/model-routing.md`, "Image limits"), so a resume sends the same
//! bytes.

use std::path::{Component, Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use contract::provider::{ImageRef, Input, InputSize, ModelRequest};
use serde_json::{Value, json};

/// One stored image, base64-encoded for the wire.
pub(crate) struct Encoded {
    /// Its type, such as `image/png`.
    pub mime_type: String,
    /// The stored file's bytes as standard base64.
    pub data: String,
}

/// A tool result's text and images, ready for a protocol to wrap in its own
/// shape.
pub(crate) struct Prepared {
    /// The text with one line appended per image that is not sent.
    pub text: String,
    /// The images that can be sent, in conversation order.
    pub images: Vec<Encoded>,
}

/// The final text plus the encoded images: the plain text when no image can
/// be sent. A file that cannot be read is not sent; the text gets one line
/// saying so, so the model is never left to believe it saw the image. For a
/// model that cannot take images (`text_only`), no file is read at all;
/// each image gets one line saying it was left out instead.
pub(crate) fn prepare(
    text: &str,
    images: &[ImageRef],
    session_dir: &Path,
    text_only: bool,
) -> Prepared {
    let mut text = text.to_owned();
    let mut encoded = Vec::new();
    for image in images {
        if text_only {
            append_line(
                &mut text,
                &format!(
                    "[Image {} left out: this model does not take images.]",
                    image.path
                ),
            );
            continue;
        }
        match encoded_image(image, session_dir) {
            Some(data) => encoded.push(Encoded {
                mime_type: image.mime_type.clone(),
                data,
            }),
            None => append_line(
                &mut text,
                &format!("[Image {} could not be read.]", image.path),
            ),
        }
    }
    Prepared {
        text,
        images: encoded,
    }
}

/// Appends `line` to `text`, starting a new line unless `text` is empty or
/// already ends with one.
fn append_line(text: &mut String, line: &str) {
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(line);
}

/// `data:<mime>;base64,<data>`, as both OpenAI protocols carry an image.
pub(crate) fn data_url(image: &Encoded) -> String {
    format!("data:{};base64,{}", image.mime_type, image.data)
}

/// The input a protocol sent for one call: the request body's length in
/// bytes, exactly as posted, and whether it carried an image. A text-only
/// endpoint sends no image part, so it reports no media. An image whose file
/// cannot be read is not sent but still counts as media: the rate waits
/// rather than being skewed.
pub(crate) fn input_size(body: &[u8], request: &ModelRequest, text_only: bool) -> InputSize {
    let media = !text_only
        && request.conversation.iter().any(|input| match input {
            Input::User { images, .. } | Input::ToolResult { images, .. } => !images.is_empty(),
            Input::Assistant { .. } | Input::Reasoning { .. } | Input::ToolCall { .. } => false,
        });
    InputSize {
        bytes: u64::try_from(body.len()).unwrap_or(u64::MAX),
        media,
    }
}

/// A `tool_result`'s `content` as `anthropic-messages` carries it: the plain
/// text when no image can be sent, otherwise a text block, when there is
/// text, then one `image` block per encoded image.
pub(crate) fn anthropic_content(prepared: Prepared) -> Value {
    if prepared.images.is_empty() {
        return json!(prepared.text);
    }
    let mut blocks = Vec::new();
    if !prepared.text.is_empty() {
        blocks.push(json!({"type": "text", "text": prepared.text}));
    }
    for image in &prepared.images {
        blocks.push(json!({
            "type": "image",
            "source": {"type": "base64", "media_type": image.mime_type, "data": image.data},
        }));
    }
    Value::Array(blocks)
}

/// The file `path` names on disk, or `None` when it must not be read. A
/// relative path stays under `session_dir`; an absolute path is read only
/// when it names a session's stored artifacts, `<sessions>/<one>/artifacts/`
/// plus the file, which is how a rewound child names its parent's files:
/// the parent lives beside the child in the same `sessions/` directory.
pub(crate) fn media_path(path: &str, session_dir: &Path) -> Option<PathBuf> {
    let candidate = Path::new(path);
    if !candidate.is_absolute() {
        if !candidate
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
        {
            return None;
        }
        return Some(session_dir.join(candidate));
    }
    if !candidate
        .components()
        .all(|part| matches!(part, Component::Normal(_) | Component::RootDir))
    {
        return None;
    }
    let rest = candidate.strip_prefix(session_dir.parent()?).ok()?;
    let mut parts = rest.components();
    if !matches!(parts.next(), Some(Component::Normal(_))) {
        return None;
    }
    if !matches!(parts.next(), Some(Component::Normal(name)) if name == "artifacts") {
        return None;
    }
    parts.next()?;
    Some(candidate.to_path_buf())
}

/// The file's bytes as base64, or `None` when its path must not be read
/// or the file cannot be read.
fn encoded_image(image: &ImageRef, session_dir: &Path) -> Option<String> {
    let resolved = media_path(&image.path, session_dir)?;
    let bytes = std::fs::read(resolved).ok()?;
    Some(STANDARD.encode(bytes))
}

#[cfg(test)]
#[path = "images_tests.rs"]
mod tests;
