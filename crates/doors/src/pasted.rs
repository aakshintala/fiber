//! An image pasted in `prompt` or `steer` (`docs/invocation.md`,
//! "Commands"): decoded, processed once through the image child as it
//! enters, and logged by path (`docs/model-routing.md`, "Image limits").

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use contract::ErrorCode;
use contract::commands::SentPart;
use contract::images::{ImageError, Images};
use contract::shapes::ContentPart;
use contract::tool::Cancel;

/// Turns the client's sent parts into the logged content parts, processing
/// image parts in order through `images` and stopping at the first error.
/// The log and the inbox receive the processed parts, never the bytes.
pub(crate) fn content(
    parts: Vec<SentPart>,
    images: Option<&dyn Images>,
    cancel: &dyn Cancel,
) -> Result<Vec<ContentPart>, (ErrorCode, String)> {
    let mut out = Vec::new();
    // `N` counts image parts only, from 1, in the order sent.
    let mut seen = 0u32;
    for part in parts {
        match part {
            SentPart::Text { text } => out.push(ContentPart::Text { text }),
            SentPart::Image { data, .. } => {
                seen += 1;
                let number = seen;
                let bytes = match STANDARD.decode(&data) {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        return Err((
                            ErrorCode::InvalidArguments,
                            format!("Image {number} cannot be read: its data is not base64."),
                        ));
                    }
                };
                let Some(images) = images else {
                    return Err((
                        ErrorCode::InvalidArguments,
                        format!(
                            "Image {number} cannot be read: this Fiber processes no images yet."
                        ),
                    ));
                };
                match images.process(&bytes, cancel) {
                    Ok(stored) => out.push(ContentPart::Image {
                        path: stored.path,
                        mime_type: stored.mime_type,
                        width: stored.width,
                        height: stored.height,
                    }),
                    Err(ImageError::Unreadable(message)) => {
                        return Err((
                            ErrorCode::InvalidArguments,
                            format!("Image {number} cannot be read: {message}"),
                        ));
                    }
                    Err(ImageError::Failed(message)) => {
                        return Err((
                            ErrorCode::IoFailed,
                            format!("Image {number} could not be processed: {message}"),
                        ));
                    }
                    Err(ImageError::Cancelled) => {
                        return Err((
                            ErrorCode::Closing,
                            format!("Image {number} was not processed: the session is closing."),
                        ));
                    }
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
#[path = "pasted_tests.rs"]
mod tests;
