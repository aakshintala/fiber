//! `fiber update`: one command that moves the Fiber binary and every
//! installed extension to the same release channel (`docs/releasing.md`,
//! "Updating"). The `--channel` flag beats `FIBER_CHANNEL`; an empty or
//! unset variable means stable. `upgrade` runs the same command.

use std::io::{BufRead, Write};
use std::path::Path;

use contract::clock::Clock;

/// The release channel `fiber update` installs from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// The default: the newest `v<major>.<minor>.<patch>` release.
    Stable,
    /// The newest nightly pre-release.
    Nightly,
}

/// Why the channel did not resolve: the invalid non-empty value. Clap
/// rejects an invalid `--channel` flag itself, so only `FIBER_CHANNEL`
/// reaches this; the message names the value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("`{value}` is not `stable` or `nightly`: check `FIBER_CHANNEL`.")]
pub struct ChannelError {
    /// The invalid non-empty value `FIBER_CHANNEL` held.
    value: String,
}

/// The release channel: the flag wins, then `FIBER_CHANNEL`; an empty or
/// unset variable means stable.
pub fn resolve_channel(flag: Option<Channel>, env: Option<&str>) -> Result<Channel, ChannelError> {
    match flag {
        Some(channel) => Ok(channel),
        None => match env {
            None | Some("") | Some("stable") => Ok(Channel::Stable),
            Some("nightly") => Ok(Channel::Nightly),
            Some(value) => Err(ChannelError {
                value: value.to_owned(),
            }),
        },
    }
}

/// The two roots `fiber update` downloads from: release assets hang under
/// `{web}/releases/download`, and the nightly list comes from
/// `{api}/releases`. Tests point both at their fixture server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseBases {
    /// The repository root, such as `https://github.com/aakshintala/fiber`.
    pub web: String,
    /// The API repository root, such as
    /// `https://api.github.com/repos/aakshintala/fiber`.
    pub api: String,
}

impl ReleaseBases {
    /// The production roots.
    pub fn production() -> Self {
        Self {
            web: "https://github.com/aakshintala/fiber".to_owned(),
            api: "https://api.github.com/repos/aakshintala/fiber".to_owned(),
        }
    }
}

/// The terminal question `fiber update` asks through, the same seam
/// `extension` asks through: `terminal` from `stdin.is_terminal()` at the
/// call site, `input` its lock, `err` standard error.
pub struct Io<'a> {
    /// Whether stdin is a terminal: without one there is no question.
    pub terminal: bool,
    /// Standard input, to read the answer from.
    pub input: &'a mut dyn BufRead,
    /// Standard error, to print the question on.
    pub err: &'a mut dyn Write,
}

/// `fiber update`: move the running binary and every installed extension
/// to the channel's newest release, together.
#[allow(
    clippy::too_many_arguments,
    reason = "one entry point takes its plain values, as the other commands do"
)]
#[allow(
    clippy::todo,
    reason = "the run is not built yet; until it is, calling update fails loudly instead of silently doing nothing"
)]
pub fn update(
    home: &Path,
    fiber_version: &str,
    target: &str,
    channel: Channel,
    bases: &ReleaseBases,
    clock: &dyn Clock,
    io: Io<'_>,
) -> i32 {
    let _ = (home, fiber_version, target, channel, bases, clock);
    let _ = io;
    todo!("`fiber update` runs once the run itself lands")
}

#[cfg(test)]
#[path = "update_tests.rs"]
mod tests;
