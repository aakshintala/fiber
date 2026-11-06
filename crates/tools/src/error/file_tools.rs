//! Failures of the file tools: `read` here; `edit` and `write` join
//! them in task 5.

/// Every way `read` fails.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ReadError {
    /// Reaching the file failed.
    #[error(transparent)]
    File(#[from] super::FileError),
    /// Reading the PDF failed.
    #[error(transparent)]
    Pdf(#[from] super::PdfError),
    /// The call's arguments do not fit the schema.
    #[error(transparent)]
    Arguments(#[from] super::ArgumentsError),
    /// `offset` or `limit` is below 1.
    #[error("`{key}` must be 1 or greater.")]
    LineBelowOne {
        /// The argument at fault: `offset` or `limit`.
        key: &'static str,
    },
    /// `offset` or `limit` does not fit `usize`.
    #[error("`{key}` is too large.")]
    LineTooLarge {
        /// The argument at fault: `offset` or `limit`.
        key: &'static str,
    },
    /// `offset` is past the file's last line.
    #[error("`offset` is past the end of the file, which has {total} lines.")]
    OffsetPastEnd {
        /// How many lines the file holds.
        total: usize,
    },
    /// `pages` names a file that is not a PDF.
    #[error("`pages` applies only to a PDF, and `{}` is not one.", path.display())]
    PagesOnNonPdf {
        /// The file as resolved.
        path: std::path::PathBuf,
    },
    /// The path does not exist, but a sibling name matches apart from
    /// Unicode spaces (`docs/tools.md`, "read"). Nothing was opened.
    #[error("{}", space_lookalike_message(requested, shown, *omitted))]
    SpaceLookalike {
        /// The file as resolved.
        requested: std::path::PathBuf,
        /// At most the first 10 sorted match paths; only basenames show.
        shown: Vec<std::path::PathBuf>,
        /// How many further matches are not shown.
        omitted: usize,
    },
}

/// The `not_found` message for [`ReadError::SpaceLookalike`]: one match is
/// named with how it differs, several are listed with at most the first 10
/// sorted basenames plus the count left out. Only basenames ever show.
fn space_lookalike_message(
    requested: &std::path::Path,
    shown: &[std::path::PathBuf],
    omitted: usize,
) -> String {
    let head = format!("`{}` does not exist.", requested.display());
    match (shown, omitted) {
        ([only], 0) => format!(
            "{head} An entry in the same directory matches it apart from Unicode spaces, so none was opened: {}.",
            quoted_base(only)
        ),
        _ => {
            let list = shown
                .iter()
                .map(|path| quoted_base(path))
                .collect::<Vec<_>>()
                .join(", ");
            if omitted == 0 {
                format!(
                    "{head} These names match it apart from Unicode spaces, so none was opened: {list}."
                )
            } else {
                format!(
                    "{head} These names match it apart from Unicode spaces, so none was opened (showing {} of {}): {list}, and {omitted} more are not listed.",
                    shown.len(),
                    shown.len() + omitted,
                )
            }
        }
    }
}

/// One match path's entry file name in backticks for the lookalike message.
fn quoted_base(path: &std::path::Path) -> String {
    let base = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    format!("`{base}`")
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
            Self::File(error) => error.code(),
            Self::Pdf(error) => error.code(),
            Self::Arguments(error) => error.code(),
            Self::SpaceLookalike { .. } => contract::ErrorCode::NotFound,
            Self::LineBelowOne { .. }
            | Self::LineTooLarge { .. }
            | Self::OffsetPastEnd { .. }
            | Self::PagesOnNonPdf { .. } => contract::ErrorCode::InvalidArguments,
        }
    }
}
