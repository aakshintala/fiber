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

/// Every way `edit` fails.
#[derive(Debug, thiserror::Error)]
pub(crate) enum EditError {
    /// Reaching the file failed.
    #[error(transparent)]
    File(#[from] super::FileError),
    /// The call's arguments do not fit the schema.
    #[error(transparent)]
    Arguments(#[from] super::ArgumentsError),
    /// The call names no edit block.
    #[error("`edits` must contain at least one block.")]
    NoBlocks,
    /// `old_text` does not occur.
    #[error("{message}")]
    NoMatch {
        /// The matcher's sentence.
        message: String,
    },
    /// `old_text` occurs more than once.
    #[error("{message}")]
    Ambiguous {
        /// The matcher's sentence.
        message: String,
    },
    /// An edit block does not parse.
    #[error("{message}")]
    InvalidEdit {
        /// The matcher's sentence.
        message: String,
    },
    /// A match offset is not a character boundary.
    #[error("{message}")]
    Boundary {
        /// The matcher's sentence.
        message: String,
    },
}

impl EditError {
    /// The code from `docs/errors.md`: one fixed code per case, with no
    /// wildcard arm.
    pub(crate) fn code(&self) -> contract::ErrorCode {
        match self {
            Self::File(error) => error.code(),
            Self::Arguments(error) => error.code(),
            Self::NoBlocks | Self::InvalidEdit { .. } => contract::ErrorCode::InvalidArguments,
            Self::NoMatch { .. } => contract::ErrorCode::NoMatch,
            Self::Ambiguous { .. } => contract::ErrorCode::AmbiguousMatch,
            Self::Boundary { .. } => contract::ErrorCode::ToolError,
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
