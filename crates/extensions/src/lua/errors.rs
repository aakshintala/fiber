//! Turning a stored stop or a missed deadline into the error a caller gets.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use crate::Error;

use super::{Target, timeout_ms};

pub(super) fn stopped(name: &str) -> Error {
    Error::Stopped {
        extension: name.to_owned(),
    }
}

pub(super) fn timed_out(name: &str, target: &Target, timeout: Duration) -> Error {
    Error::Timeout {
        extension: name.to_owned(),
        callback: target.to_string(),
        timeout_ms: timeout_ms(timeout),
    }
}

pub(super) fn not_registered(name: &str, target: &Target) -> Error {
    match target {
        Target::Command(command) => Error::UnknownCommand {
            extension: name.to_owned(),
            command: command.clone(),
        },
        Target::Provider { .. }
        | Target::Hook { .. }
        | Target::Timer { .. }
        | Target::Search(_)
        | Target::Tool(_)
        | Target::Effects(_) => Error::UnknownCallback {
            extension: name.to_owned(),
            callback: target.to_string(),
        },
    }
}

/// What a stopped extension re-raises: the six errors a stop can hold,
/// each rebuilt per caller, and `Stopped` for every other. A closed enum
/// so a new re-raised shape fails to compile until every `match` handles
/// it (`docs/code-quality.md`, "Lints").
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum StopReason {
    Io {
        path: PathBuf,
        kind: io::ErrorKind,
        message: String,
    },
    Lua {
        extension: String,
        message: String,
    },
    Unattended {
        extension: String,
        call: String,
    },
    Damaged(crate::Damaged),
    Timeout {
        extension: String,
        callback: String,
        timeout_ms: u64,
    },
    Abandoned {
        extension: String,
        callback: String,
    },
    Stopped,
}

impl StopReason {
    /// The reason `e` stops with: the six re-raised shapes move out, every
    /// other error stops as `Stopped`.
    pub(super) fn of(e: Error) -> Self {
        if let Error::Io { path, source } = e {
            return Self::Io {
                path,
                kind: source.kind(),
                message: source.to_string(),
            };
        }
        if let Error::Lua { extension, message } = e {
            return Self::Lua { extension, message };
        }
        if let Error::Unattended { extension, call } = e {
            return Self::Unattended { extension, call };
        }
        if let Error::Damaged(inner) = e {
            return Self::Damaged(inner);
        }
        if let Error::Timeout {
            extension,
            callback,
            timeout_ms,
        } = e
        {
            return Self::Timeout {
                extension,
                callback,
                timeout_ms,
            };
        }
        if let Error::Abandoned {
            extension,
            callback,
        } = e
        {
            return Self::Abandoned {
                extension,
                callback,
            };
        }
        Self::Stopped
    }

    /// Each caller's copy of the stored error: the six shapes rebuild with
    /// their stored text, so `of(e).error(n).to_string() == e.to_string()`
    /// for them, and every other is `Stopped` for `name`.
    pub(super) fn error(&self, name: &str) -> Error {
        match self {
            Self::Io { path, kind, message } => Error::Io {
                path: path.clone(),
                source: io::Error::new(*kind, message.clone()),
            },
            Self::Lua { extension, message } => Error::Lua {
                extension: extension.clone(),
                message: message.clone(),
            },
            Self::Unattended { extension, call } => Error::Unattended {
                extension: extension.clone(),
                call: call.clone(),
            },
            Self::Damaged(inner) => Error::Damaged(inner.clone()),
            Self::Timeout {
                extension,
                callback,
                timeout_ms,
            } => Error::Timeout {
                extension: extension.clone(),
                callback: callback.clone(),
                timeout_ms: *timeout_ms,
            },
            Self::Abandoned {
                extension,
                callback,
            } => Error::Abandoned {
                extension: extension.clone(),
                callback: callback.clone(),
            },
            Self::Stopped => stopped(name),
        }
    }
}
