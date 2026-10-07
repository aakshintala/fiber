//! The image child's driver (`docs/architecture.md`, "The call rules"):
//! processes one image's bytes once, writes the processed file to the
//! session's `artifacts/`, and names it (`docs/model-routing.md`, "Image limits").

/// The image child's driver: processes one image's bytes once, writes the
/// processed file to the session's `artifacts/`, and names it.
pub trait Images: Send + Sync {
    /// Processes one image's bytes, writing the processed file to the
    /// session's `artifacts/` and returning its reference, or the refusal.
    fn process(
        &self,
        bytes: &[u8],
        cancel: &dyn crate::tool::Cancel,
    ) -> Result<crate::provider::ImageRef, ImageError>;
}

/// Why an image was not processed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageError {
    /// Not an image the child reads, or over 50 MP: the child's message.
    Unreadable(String),
    /// The child could not run, take the bytes or answer: a sentence.
    Failed(String),
    /// Cancelled; the child was killed and reaped.
    Cancelled,
}
