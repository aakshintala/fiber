//! Failures of the media readers: PDFs (`pdf`) and images (`image`).

use std::path::PathBuf;

use contract::ErrorCode;

/// Every way reading a PDF fails.
#[derive(Debug, thiserror::Error)]
pub(crate) enum PdfError {
    /// `pages` names more than one call takes.
    #[error(
        "`pages` {text} names {count} pages; a request takes at most {max}.",
        max = crate::pdf::RANGE_MAX
    )]
    PagesTooMany {
        /// The `pages` argument as the call named it.
        text: String,
        /// How many pages it names.
        count: u32,
    },
    /// `pages` is no page or range.
    #[error("`pages` must be a page or a range such as `3` or `1-5`, counted from 1.")]
    PagesMalformed,
    /// PDF reading is not configured.
    #[error("PDF reading is not configured.")]
    NotConfigured,
    /// The image child could not start.
    #[error("the image child could not start: {source}.")]
    ChildStart {
        /// The spawn failure.
        #[source]
        source: std::io::Error,
    },
    /// The image child answered without a usable count or cut.
    #[error("the image child {why}.")]
    ChildAnswer {
        /// What the child printed instead.
        why: &'static str,
    },
    /// The image child named no page count for a file past the end.
    #[error("the image child named no page count.")]
    NoPageCount,
    /// The file is no PDF the child reads. The text is made by the image
    /// child.
    // debt: the text is made by the image child, until the image child names its PDF
    // refusal by kind
    #[error("`{}` cannot be read as a PDF: {message}", path.display())]
    Unreadable {
        /// The file as resolved.
        path: PathBuf,
        /// The child's refusal.
        message: String,
    },
    /// The image child exited with a status.
    #[error("the image child exited with status {status}: {message}")]
    ChildExited {
        /// The child's exit status.
        status: i32,
        /// The child's capped standard error.
        message: String,
    },
    /// The image child was killed by a signal.
    #[error("the image child was killed by a signal: {message}")]
    ChildSignalled {
        /// The child's capped standard error.
        message: String,
    },
    /// The range asks past the file's last page.
    #[error(
        "`pages` {first}-{last} is past the end: `{}` has {total} pages.",
        path.display()
    )]
    PastTheEnd {
        /// The file as resolved.
        path: PathBuf,
        /// The range's first page.
        first: u32,
        /// The range's last page.
        last: u32,
        /// The file's page count.
        total: u32,
    },
    /// The whole file holds more pages than one call reads without `pages`.
    #[error(
        "`{}` has {total} pages. A PDF of more than {whole} pages needs `pages`, such as `1-5`; a request takes at most {max} pages.",
        path.display(),
        whole = crate::pdf::WHOLE_MAX,
        max = crate::pdf::RANGE_MAX
    )]
    NeedsPages {
        /// The file as resolved.
        path: PathBuf,
        /// The file's page count.
        total: u32,
    },
}

impl PdfError {
    /// The code from `docs/errors.md`: one fixed code per case, with no
    /// wildcard arm.
    pub(crate) fn code(&self) -> ErrorCode {
        match self {
            Self::PagesTooMany { .. } | Self::PagesMalformed { .. } => ErrorCode::InvalidArguments,
            Self::NotConfigured
            | Self::ChildStart { .. }
            | Self::ChildAnswer { .. }
            | Self::NoPageCount
            | Self::ChildExited { .. }
            | Self::ChildSignalled { .. } => ErrorCode::ToolError,
            Self::Unreadable { .. } => ErrorCode::UnsupportedFile,
            Self::PastTheEnd { .. } | Self::NeedsPages { .. } => ErrorCode::InvalidArguments,
        }
    }
}

/// Every way reading an image fails. The last two texts are made in
/// `contract::images`.
// debt: the last two texts are made in contract::images, until
// `contract::images::ImageError` carries typed cases
#[derive(Debug, thiserror::Error)]
pub(crate) enum ImageReadError {
    /// Image reading is not configured.
    #[error("image reading is not configured.")]
    NotConfigured,
    /// The file is no image the child reads.
    #[error("`{}` cannot be read as an image: {message}", path.display())]
    Unreadable {
        /// The file as resolved.
        path: PathBuf,
        /// The child's refusal.
        message: String,
    },
    /// The child could not run, take the bytes or answer.
    #[error("{message}")]
    ChildFailed {
        /// The child's sentence.
        message: String,
    },
}

impl ImageReadError {
    /// The code from `docs/errors.md`: one fixed code per case, with no
    /// wildcard arm.
    pub(crate) fn code(&self) -> ErrorCode {
        match self {
            Self::NotConfigured | Self::ChildFailed { .. } => ErrorCode::ToolError,
            Self::Unreadable { .. } => ErrorCode::UnsupportedFile,
        }
    }
}
