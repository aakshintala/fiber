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

pub(crate) use file_tools::{EditError, ReadError, WriteError};
pub(crate) use media::{ImageReadError, PdfError};
pub(crate) use misc::{AskUserError, SkillError, WebSearchError};
pub(crate) use session::{SessionListError, SessionMessageError};

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
    /// The file does not exist.
    #[error("`{}` does not exist.", path.display())]
    NotFound {
        /// The file as resolved.
        path: std::path::PathBuf,
    },
    /// Inspecting the file failed. The text is made in `files.rs`; part 5
    /// of #923 types it.
    // debt: part 5 of #923 types `ResolveError`/`InspectError`
    #[error("{message}")]
    Inspect {
        /// `InspectError::Tool`'s text.
        message: String,
    },
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
            Self::CouldNotWrite { .. } | Self::Inspect { .. } => ErrorCode::ToolError,
            Self::NotFound { .. } => ErrorCode::NotFound,
            Self::Unsupported { .. } | Self::PdfOverCap { .. } => ErrorCode::UnsupportedFile,
        }
    }
}

#[cfg(test)]
#[path = "error_tests.rs"]
mod tests;
