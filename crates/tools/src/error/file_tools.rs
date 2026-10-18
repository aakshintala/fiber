//! Failures of the file tools: `read` here; `edit` and `write` join
//! them in task 5.

/// Every way `read` fails.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ReadError {
    /// Reading the PDF failed.
    #[error(transparent)]
    Pdf(#[from] super::PdfError),
}

/// Every way `write` fails.
#[derive(Debug, thiserror::Error)]
pub(crate) enum WriteError {
    /// Reaching the file failed.
    #[error(transparent)]
    File(#[from] super::FileError),
    /// The call's arguments do not fit the schema.
    #[error(transparent)]
    Arguments(#[from] super::ArgumentsError),
    /// The file was never read in this context.
    #[error("`{}` was not read in this context. Read it first.", path.display())]
    StaleNotRead {
        /// The file as resolved.
        path: std::path::PathBuf,
    },
    /// The file changed since it was read.
    #[error("`{}` changed since it was read. Read it first.", path.display())]
    StaleChanged {
        /// The file as resolved.
        path: std::path::PathBuf,
    },
}

impl WriteError {
    /// The code from `docs/errors.md`: one fixed code per case, with no
    /// wildcard arm.
    pub(crate) fn code(&self) -> contract::ErrorCode {
        match self {
            Self::File(error) => error.code(),
            Self::Arguments(error) => error.code(),
            Self::StaleNotRead { .. } | Self::StaleChanged { .. } => contract::ErrorCode::StaleFile,
        }
    }
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
