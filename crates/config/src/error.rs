//! What can go wrong reading or writing configuration, and each case's code
//! (`docs/errors.md`). No message ever holds a value from a file, so a secret
//! pasted into the wrong place does not reach an error.

use std::io;
use std::path::PathBuf;

use contract::ErrorCode;

/// A failure of the `config` crate.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// A file is not valid JSON.
    #[error("{} is not valid JSON (line {line}, column {column}). Fix the file and try again.", file.display())]
    Json {
        /// The file.
        file: PathBuf,
        /// Where the parser stopped.
        line: usize,
        /// Where the parser stopped.
        column: usize,
    },
    /// A key holds a value of the wrong type.
    #[error("{source_name}: `{key}` must be {expected}.")]
    WrongType {
        /// The file, or `-c` for the command line.
        source_name: String,
        /// The dotted key.
        key: String,
        /// What it must be.
        expected: String,
    },
    /// A file could not be read or written.
    #[error("{}: {source}", file.display())]
    Io {
        /// The file.
        file: PathBuf,
        /// Why.
        source: io::Error,
    },
    /// `FIBER_HOME` is empty or relative, or no home directory is known.
    #[error("{0}")]
    FiberHome(&'static str),
    /// A `-c` argument that is not `key=value`, or a key that is not a dotted
    /// path.
    #[error("`{arg}` is not a dotted key and a value, as in `-c handoff.tokens=200000`.")]
    Override {
        /// The argument as given, up to any `=`.
        arg: String,
    },
    /// A `fiber config set` key that may not be written to the layer's file:
    /// unknown, or a layer the key may not be set in.
    #[error("`{key}` cannot be set in {}: {why}.", file.display())]
    Refused {
        /// The key as typed.
        key: String,
        /// The file it would have written.
        file: PathBuf,
        /// Why, never a value.
        why: &'static str,
    },
    /// A repository file, or a credential, that is a symbolic link or not a
    /// regular file or directory.
    #[error("{} is a symbolic link or not a regular file, so Fiber does not read it.", file.display())]
    NotPlain {
        /// The path.
        file: PathBuf,
    },
    /// A project key that is not one file name.
    #[error("`{key}` is not a project key: it must be one file name in projects/.")]
    ProjectKey {
        /// The key.
        key: String,
    },
    /// A file that is JSON but does not have the shape its kind of file needs.
    #[error("{} does not fit {expected} (line {line}, column {column}). Fix the file and try again.", file.display())]
    Shape {
        /// The file.
        file: PathBuf,
        /// Where the value that does not fit ends.
        line: usize,
        /// Where the value that does not fit ends.
        column: usize,
        /// What kind of file it should be.
        expected: &'static str,
    },
    /// No key for a provider.
    #[error("No credential for `{provider}`: {why}. Run `fiber login {provider}`.")]
    CredentialMissing {
        /// The provider.
        provider: String,
        /// Where Fiber looked, never a value.
        why: String,
    },
    /// A stored credential exists but cannot be used.
    #[error(
        "The stored credential for `{provider}` cannot be used: {why}. Run `fiber login {provider}`."
    )]
    CredentialFailed {
        /// The provider.
        provider: String,
        /// Why it cannot be used, never a value.
        why: String,
    },
    /// A secret's name that is not one file name.
    #[error("`{name}` is not a secret's name: it must be one file name in credentials/.")]
    SecretName {
        /// The name.
        name: String,
    },
}

impl ConfigError {
    /// The stable code a caller switches on.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Json { .. }
            | Self::WrongType { .. }
            | Self::NotPlain { .. }
            | Self::Shape { .. } => ErrorCode::ConfigInvalid,
            Self::CredentialMissing { .. } => ErrorCode::CredentialMissing,
            Self::CredentialFailed { .. } => ErrorCode::CredentialFailed,
            Self::Io { .. } => ErrorCode::IoFailed,
            Self::FiberHome(_) | Self::Override { .. } | Self::Refused { .. } => ErrorCode::Usage,
            Self::ProjectKey { .. } | Self::SecretName { .. } => ErrorCode::InvalidArguments,
        }
    }
}
