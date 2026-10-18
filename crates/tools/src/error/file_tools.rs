//! Failures of the file tools: `read` here; `edit` and `write` join
//! them in task 5.

/// Every way `read` fails.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ReadError {
    /// Reading the PDF failed.
    #[error(transparent)]
    Pdf(#[from] super::PdfError),
}

impl ReadError {
    /// The code from `docs/errors.md`: one fixed code per case, with no
    /// wildcard arm.
    pub(crate) fn code(&self) -> contract::ErrorCode {
        match self {
            Self::Pdf(error) => error.code(),
        }
    }
}
