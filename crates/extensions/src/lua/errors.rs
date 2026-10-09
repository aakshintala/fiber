//! Turning a stored stop or a missed deadline into the error a caller gets.

use std::io;
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
        | Target::Tool(_)
        | Target::Effects(_) => Error::UnknownCallback {
            extension: name.to_owned(),
            callback: target.to_string(),
        },
    }
}

/// A copy of the error that stopped the extension, for the next caller.
/// These are the errors a stopped extension can hold.
pub(super) fn again(name: &str, e: &Error) -> Error {
    match e {
        Error::Io { path, source } => Error::Io {
            path: path.clone(),
            source: io::Error::new(source.kind(), source.to_string()),
        },
        Error::Lua { extension, message } => Error::Lua {
            extension: extension.clone(),
            message: message.clone(),
        },
        Error::Unattended { extension, call } => Error::Unattended {
            extension: extension.clone(),
            call: call.clone(),
        },
        Error::Damaged(inner) => Error::Damaged(inner.clone()),
        Error::Timeout {
            extension,
            callback,
            timeout_ms,
        } => Error::Timeout {
            extension: extension.clone(),
            callback: callback.clone(),
            timeout_ms: *timeout_ms,
        },
        Error::Abandoned {
            extension,
            callback,
        } => Error::Abandoned {
            extension: extension.clone(),
            callback: callback.clone(),
        },
        Error::Stopped { .. }
        | Error::Config(_)
        | Error::Overlaps { .. }
        | Error::NeedsNewerFiber { .. }
        | Error::ApiVersion { .. }
        | Error::BadVersion { .. }
        | Error::BadName { .. }
        | Error::GitMissing
        | Error::Git { .. }
        | Error::NoRepository { .. }
        | Error::MajorConflict { .. }
        | Error::NoVersion { .. }
        | Error::Unresolved
        | Error::NotInstalled { .. }
        | Error::WrongName { .. }
        | Error::Busy
        | Error::SlugTaken { .. }
        | Error::NoTag { .. }
        | Error::BadRecord { .. }
        | Error::InstallStep { .. }
        | Error::InstallExited { .. }
        | Error::Download { .. }
        | Error::BinaryChecksum { .. }
        | Error::ArchiveChecksum { .. }
        | Error::BadArchive { .. }
        | Error::Rollback { .. }
        | Error::ProviderMissing { .. }
        | Error::ModelMissing { .. }
        | Error::UnknownModel { .. }
        | Error::Unconfigured { .. }
        | Error::Ambiguous { .. }
        | Error::UnknownCommand { .. }
        | Error::UnknownCallback { .. }
        | Error::BadReturn { .. }
        | Error::Credential(_)
        | Error::RefreshRejected { .. }
        | Error::RefreshUnreachable { .. }
        | Error::BadRepositoryPath { .. }
        | Error::Pin { .. }
        | Error::ChangedWhileCopying { .. }
        | Error::NoModel => stopped(name),
    }
}
