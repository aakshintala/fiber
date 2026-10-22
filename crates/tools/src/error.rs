//! Every tool failure as its own case (`docs/errors.md`, "Where a code
//! comes from"): one fixed code per case, mapped in `code()` with no
//! wildcard arm, so a new case does not compile until it has one. Each
//! case's message is the sentence shown for it, and `Output.error` is built
//! from a case at the edge through `From<&ToolError> for Failure`.

use contract::ErrorCode;
use contract::shapes::Failure;

mod file_tools;
mod media;
mod misc;
mod session;
mod web_fetch;

pub(crate) use file_tools::{EditError, ReadError, WriteError};
pub(crate) use media::{ImageReadError, PdfError};
pub(crate) use misc::{AskUserError, SkillError, WebSearchError};
pub(crate) use session::{SessionListError, SessionMessageError};
pub(crate) use web_fetch::{UrlError, WebFetchError};

/// Every way a built-in tool call fails. The root is the one type that
/// reaches `Output`. Cases arrive with their callers, so no case names a
/// failure nothing builds yet.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ToolError {
    /// The call's arguments do not fit the schema.
    #[error(transparent)]
    Arguments(#[from] ArgumentsError),
    /// A file tool's path moved between the permission check and the run.
    #[error(transparent)]
    File(#[from] FileError),
    /// `session_list` could not list the running sessions.
    #[error(transparent)]
    SessionList(#[from] SessionListError),
    /// `session_message` was not delivered.
    #[error(transparent)]
    SessionMessage(#[from] SessionMessageError),
    /// The `ask_user` call or answer does not parse.
    #[error(transparent)]
    AskUser(#[from] AskUserError),
    /// `skill` names no loadable skill.
    #[error(transparent)]
    Skill(#[from] SkillError),
    /// Reading a PDF failed.
    #[error(transparent)]
    Pdf(#[from] PdfError),
    /// Reading an image failed.
    #[error(transparent)]
    Image(#[from] ImageReadError),
    /// `read` failed.
    #[error(transparent)]
    Read(#[from] ReadError),
    /// `write` failed.
    #[error(transparent)]
    Write(#[from] WriteError),
    /// `edit` failed.
    #[error(transparent)]
    Edit(#[from] EditError),
    /// `web_search` has nothing to run or the backend failed.
    #[error(transparent)]
    WebSearch(#[from] WebSearchError),
    /// `web_fetch` refused the URL, or the fetch failed.
    #[error(transparent)]
    WebFetch(#[from] WebFetchError),
}

impl ToolError {
    /// The code from `docs/errors.md`: one fixed code per case, with no
    /// wildcard arm.
    pub(crate) fn code(&self) -> ErrorCode {
        match self {
            Self::Arguments(error) => error.code(),
            Self::File(error) => error.code(),
            Self::SessionList(error) => error.code(),
            Self::SessionMessage(error) => error.code(),
            Self::AskUser(error) => error.code(),
            Self::Skill(error) => error.code(),
            Self::Pdf(error) => error.code(),
            Self::Image(error) => error.code(),
            Self::Read(error) => error.code(),
            Self::Write(error) => error.code(),
            Self::Edit(error) => error.code(),
            Self::WebSearch(error) => error.code(),
            Self::WebFetch(error) => error.code(),
        }
    }
}

impl From<&ToolError> for Failure {
    /// The contract record of a case: its code and its sentence, with no
    /// retry or provider carried over.
    fn from(error: &ToolError) -> Self {
        Failure {
            code: error.code(),
            message: error.to_string(),
            retry_after_ms: None,
            provider: None,
        }
    }
}

impl From<ToolError> for Failure {
    /// The contract record of a case, owned.
    fn from(error: ToolError) -> Self {
        Self::from(&error)
    }
}

/// A call whose arguments do not fit the tool's schema.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ArgumentsError {
    /// The arguments do not fit the schema.
    #[error("The arguments do not fit the schema: {source}.")]
    DoNotFit {
        /// The schema mismatch.
        #[source]
        source: serde_json::Error,
    },
}

impl ArgumentsError {
    /// The code from `docs/errors.md`: one fixed code per case, with no
    /// wildcard arm.
    pub(crate) fn code(&self) -> ErrorCode {
        match self {
            Self::DoNotFit { .. } => ErrorCode::InvalidArguments,
        }
    }
}

/// A file tool's shared failures (`read`, `edit`, `write`).
#[derive(Debug, thiserror::Error)]
pub(crate) enum FileError {
    /// The path moved between the permission check and a read.
    #[error("`{raw}` changed between the permission check and the read. Nothing was read.")]
    ChangedBeforeRead {
        /// The path as the call named it.
        raw: String,
    },
    /// The path moved between the permission check and a write.
    #[error("`{raw}` changed between the permission check and the write. Nothing was written.")]
    ChangedBeforeWrite {
        /// The path as the call named it.
        raw: String,
    },
    /// The path does not resolve.
    #[error(transparent)]
    Resolve(#[from] ResolveError),
    /// The file cannot be inspected.
    #[error(transparent)]
    Inspect(#[from] InspectError),
    /// The file could not be written.
    #[error("`{}` could not be written: {source}.", path.display())]
    CouldNotWrite {
        /// The file as resolved.
        path: std::path::PathBuf,
        /// The filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// A directory, device or file the tool will not write.
    #[error("{}", crate::files::unsupported_message(path, kind, *size, hint))]
    Unsupported {
        /// The file as resolved.
        path: std::path::PathBuf,
        /// Detected type, such as `a directory` or `binary data`.
        kind: String,
        /// Size in bytes.
        size: u64,
        /// Extra sentence, or empty.
        hint: &'static str,
    },
    /// A PDF over the 100 MiB cap, never readable and never writable.
    #[error("{}", crate::pdf::over_cap_message(path, *size))]
    PdfOverCap {
        /// The file as resolved.
        path: std::path::PathBuf,
        /// Size in bytes, from the file's metadata.
        size: u64,
    },
}

impl FileError {
    /// The code from `docs/errors.md`: one fixed code per case, with no
    /// wildcard arm.
    pub(crate) fn code(&self) -> ErrorCode {
        match self {
            Self::ChangedBeforeRead { .. } | Self::ChangedBeforeWrite { .. } => {
                ErrorCode::PathChanged
            }
            Self::CouldNotWrite { .. } => ErrorCode::ToolError,
            Self::Resolve(error) => error.code(),
            Self::Inspect(error) => error.code(),
            Self::Unsupported { .. } | Self::PdfOverCap { .. } => ErrorCode::UnsupportedFile,
        }
    }
}

/// Why a path could not be resolved (`files::resolve`).
#[derive(Debug, thiserror::Error)]
pub(crate) enum ResolveError {
    /// `..` in the part of the path that does not exist yet.
    #[error("`{raw}` cannot be resolved: `..` points through a directory that does not exist.")]
    DotDotThroughMissing {
        /// The path as the call named it.
        raw: String,
    },
    /// A symbolic link chain past the link cap, or a loop.
    #[error("`{raw}` could not be resolved: too many levels of symbolic links.")]
    TooManyLinks {
        /// The path as the call named it.
        raw: String,
    },
    /// A component could not be read.
    #[error("`{}` could not be resolved: {source}.", path.display())]
    Unreadable {
        /// The component that failed.
        path: std::path::PathBuf,
        /// The filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// A symbolic link whose target is empty.
    #[error("`{}` could not be resolved: the symbolic link is empty.", path.display())]
    EmptyLink {
        /// The link.
        path: std::path::PathBuf,
    },
    /// A file sits where a directory is needed.
    #[error("`{raw}` could not be resolved: `{}` is not a directory.", path.display())]
    NotADirectory {
        /// The path as the call named it.
        raw: String,
        /// The component that is a file.
        path: std::path::PathBuf,
    },
}

impl ResolveError {
    /// The code from `docs/errors.md`: one fixed code per case, with no
    /// wildcard arm.
    pub(crate) fn code(&self) -> ErrorCode {
        match self {
            Self::DotDotThroughMissing { .. } => ErrorCode::InvalidArguments,
            Self::TooManyLinks { .. }
            | Self::Unreadable { .. }
            | Self::EmptyLink { .. }
            | Self::NotADirectory { .. } => ErrorCode::ToolError,
        }
    }
}

/// Why a file could not be inspected (`files::inspect`).
#[derive(Debug, thiserror::Error)]
pub(crate) enum InspectError {
    /// The path does not exist.
    #[error("`{}` does not exist.", path.display())]
    NotFound {
        /// The file as resolved.
        path: std::path::PathBuf,
    },
    /// A filesystem failure.
    #[error("`{}` could not be read: {source}.", path.display())]
    Unreadable {
        /// The file as resolved.
        path: std::path::PathBuf,
        /// The filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// The file's size does not fit in 64 bits.
    #[error("`{}` could not be read: its size does not fit in 64 bits.", path.display())]
    SizeOverflow {
        /// The file as resolved.
        path: std::path::PathBuf,
    },
}

impl InspectError {
    /// The code from `docs/errors.md`: one fixed code per case, with no
    /// wildcard arm.
    pub(crate) fn code(&self) -> ErrorCode {
        match self {
            Self::NotFound { .. } => ErrorCode::NotFound,
            Self::Unreadable { .. } | Self::SizeOverflow { .. } => ErrorCode::ToolError,
        }
    }
}

#[cfg(test)]
#[path = "error_tests.rs"]
mod tests;
