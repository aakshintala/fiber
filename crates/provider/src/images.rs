//! The images of a tool result, shared by all four protocols: one function
//! turns (text, images, media, text_only) into the final text plus the
//! encoded images, and each protocol only wraps them in its own shape; one
//! function measures a request body's size and whether it carried an image.
//! The provider does no file I/O: the loop reads and encodes each stored
//! file and hands over `media`, a map from the conversation's path to the
//! file's base64. The bytes are the stored file's, never re-encoded or
//! resized (`docs/model-routing.md`, "Image limits"), so a resume sends the
//! same bytes.

use contract::provider::{ImageRef, Input, InputSize, Media, ModelRequest, PdfRef};
use serde_json::{Value, json};

/// The image line when the model does not take images
/// (`docs/system-prompt.md`, "The texts").
const IMAGE_LEFT_OUT: &str = include_str!("../prompt/image-left-out.md").trim_ascii_end();

/// The image line when the stored file cannot be read
/// (`docs/system-prompt.md`, "The texts"). A PDF page that cannot be read
/// uses this text too.
const IMAGE_UNREADABLE: &str = include_str!("../prompt/image-unreadable.md").trim_ascii_end();

/// The PDF line when the model does not take images
/// (`docs/system-prompt.md`, "The texts").
const PDF_LEFT_OUT: &str = include_str!("../prompt/pdf-left-out.md").trim_ascii_end();

/// The PDF line when the stored file cannot be read
/// (`docs/system-prompt.md`, "The texts").
const PDF_UNREADABLE: &str = include_str!("../prompt/pdf-unreadable.md").trim_ascii_end();

/// Fills `{path}` in a prompt template with the stored file's path.
fn fill(template: &str, path: &str) -> String {
    template.replace("{path}", path)
}

/// One stored image, base64-encoded for the wire.
pub(crate) struct Encoded {
    /// Its type, such as `image/png`.
    pub mime_type: String,
    /// The stored file's bytes as standard base64.
    pub data: String,
}

/// A tool result's text, images and PDFs, ready for a protocol to wrap in
/// its own shape.
pub(crate) struct Prepared {
    /// The text with one line appended per image or PDF that is not sent.
    pub text: String,
    /// The images that can be sent, in conversation order.
    pub images: Vec<Encoded>,
    /// The PDFs that can be sent natively, in conversation order.
    pub documents: Vec<Document>,
}

/// How a protocol carries a PDF: natively as a document, or as its pages
/// rendered as images.
pub(crate) enum PdfForm {
    /// The protocol accepts a PDF in a tool result; each readable PDF is
    /// sent as a document.
    Native,
    /// The protocol rejects a file part in a tool message; each PDF's
    /// pages go through the image rule, and a PDF without pages adds
    /// nothing.
    Pages,
}

/// One stored PDF, base64-encoded for the wire.
pub(crate) struct Document {
    /// The stored file's base name, so a resent request carries the same
    /// bytes.
    pub filename: String,
    /// The stored file's bytes as standard base64.
    pub data: String,
}

/// The final text plus the encoded images: the plain text when no image can
/// be sent. A file `media` lacks (it could not be read, or must not be) is
/// not sent; the text gets one line saying so, so the model is never left to
/// believe it saw the image. For a model that cannot take images
/// (`text_only`), `media` is ignored; each image gets one line saying it was
/// left out instead.
pub(crate) fn prepare(
    text: &str,
    images: &[ImageRef],
    pdfs: &[PdfRef],
    media: &Media,
    text_only: bool,
    form: PdfForm,
) -> Prepared {
    let mut text = text.to_owned();
    let mut encoded = Vec::new();
    for image in images {
        if text_only {
            append_line(&mut text, &fill(IMAGE_LEFT_OUT, &image.path));
            continue;
        }
        match encoded_image(image, media) {
            Some(data) => encoded.push(Encoded {
                mime_type: image.mime_type.clone(),
                data,
            }),
            None => append_line(&mut text, &fill(IMAGE_UNREADABLE, &image.path)),
        }
    }
    let mut documents = Vec::new();
    for pdf in pdfs {
        if text_only {
            append_line(&mut text, &fill(PDF_LEFT_OUT, &pdf.path));
            continue;
        }
        match form {
            PdfForm::Native => match encoded_pdf(pdf, media) {
                Some((filename, data)) => documents.push(Document { filename, data }),
                None => append_line(&mut text, &fill(PDF_UNREADABLE, &pdf.path)),
            },
            PdfForm::Pages => {
                for page in pdf.pages.iter().flatten() {
                    match encoded_image(page, media) {
                        Some(data) => encoded.push(Encoded {
                            mime_type: page.mime_type.clone(),
                            data,
                        }),
                        None => append_line(&mut text, &fill(IMAGE_UNREADABLE, &page.path)),
                    }
                }
            }
        }
    }
    Prepared {
        text,
        images: encoded,
        documents,
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
            Input::User { images, .. } => !images.is_empty(),
            Input::ToolResult { images, pdfs, .. } => !images.is_empty() || !pdfs.is_empty(),
            Input::Assistant { .. }
            | Input::Reasoning { .. }
            | Input::ToolCall { .. }
            | Input::ToolSearchResult { .. } => false,
        });
    InputSize {
        bytes: u64::try_from(body.len()).unwrap_or(u64::MAX),
        media,
    }
}

/// A `tool_result`'s `content` as `anthropic-messages` carries it: the plain
/// text when no image or document can be sent, otherwise a text block, when
/// there is text, then one `image` block per encoded image and one
/// `document` block per encoded PDF.
pub(crate) fn anthropic_content(prepared: Prepared) -> Value {
    if prepared.images.is_empty() && prepared.documents.is_empty() {
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
    for document in &prepared.documents {
        blocks.push(json!({
            "type": "document",
            "source": {"type": "base64", "media_type": "application/pdf", "data": document.data},
        }));
    }
    Value::Array(blocks)
}

/// The file's base64, or `None` when `media` lacks its path.
fn encoded_image(image: &ImageRef, media: &Media) -> Option<String> {
    media.get(&image.path).map(|data| data.to_string())
}

/// The stored PDF's base name and bytes as base64, or `None` when `media`
/// lacks its path. The bytes are never transformed.
fn encoded_pdf(pdf: &PdfRef, media: &Media) -> Option<(String, String)> {
    let data = media.get(&pdf.path)?;
    let filename = std::path::Path::new(&pdf.path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| pdf.path.clone());
    Some((filename, data.to_string()))
}

#[cfg(test)]
#[path = "images_tests.rs"]
mod tests;
