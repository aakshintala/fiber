//! `fiber update`: one command that moves the Fiber binary and every
//! installed extension to the same release channel (`docs/releasing.md`,
//! "Updating"). The `--channel` flag beats `FIBER_CHANNEL`; an empty or
//! unset variable means stable. `upgrade` runs the same command.

use std::fs;
use std::io::{BufRead, Write};
use std::path::Path;

use contract::ErrorCode;
use contract::clock::Clock;
use contract::shapes::Failure;

use crate::error::CliError;

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
pub fn update(
    home: &Path,
    fiber_version: &str,
    target: &str,
    channel: Channel,
    bases: &ReleaseBases,
    clock: &dyn Clock,
    io: Io<'_>,
) -> i32 {
    let binary = match std::env::current_exe() {
        Ok(binary) => binary,
        Err(e) => {
            return crate::fail(crate::failed(
                ErrorCode::IoFailed,
                format!("the running binary's path: {e}"),
            ));
        }
    };
    let origin = extensions::Origin::github();
    crate::finish_code(run(
        home,
        fiber_version,
        target,
        channel,
        bases,
        clock,
        io,
        &binary,
        &origin,
        &crate::hub_service::hub_restart,
    ))
}

/// A release version ordered against another: the base triple, whether
/// the release is a stable one (stable sorts above a nightly on the
/// same base), and the nightly date, which orders two nightlies on one
/// base.
type VersionKey = ([u64; 3], bool, String);

/// The running version as an orderable key: a nightly-form version
/// keeps its date, anything else parses as a stable triple.
fn running_key(fiber_version: &str) -> Result<VersionKey, extensions::Error> {
    match extensions::nightly_parts(fiber_version) {
        Ok((base, date)) => Ok((base, false, date)),
        Err(_) => extensions::version(fiber_version).map(|base| (base, true, String::new())),
    }
}

/// The target tag as an orderable key plus the bare version staging
/// uses: a stable tag parses as a triple, a nightly tag as its base
/// and date.
fn target_key(tag: &str, channel: Channel) -> Result<(VersionKey, String), extensions::Error> {
    let key = match channel {
        Channel::Stable => extensions::version(tag).map(|base| (base, true, String::new())),
        Channel::Nightly => extensions::nightly_parts(tag).map(|(base, date)| (base, false, date)),
    }?;
    let bare = tag.strip_prefix('v').unwrap_or(tag);
    Ok((key, bare.to_owned()))
}

/// Proves the binary directory takes a write before the commit renames
/// anything: a probe file created and removed, naming the directory on
/// failure.
fn check_writable(directory: &Path) -> Result<(), CliError> {
    let probe = directory.join(".fiber-update-writable");
    if let Err(source) = fs::write(&probe, b"") {
        return Err(CliError::UpdateBinaryDirectory {
            dir: directory.to_path_buf(),
            source,
        });
    }
    fs::remove_file(&probe).unwrap_or(());
    Ok(())
}

/// The run `update` calls: resolves the channel's tag, stages that
/// release (or the running one when already newest), asks once over the
/// combined member summaries, commits extensions then docs, renames the
/// staged binary last, and restarts the hub with the sessions kept.
#[allow(
    clippy::too_many_arguments,
    reason = "the run takes what main parsed plus the seams tests replace"
)]
fn run(
    home: &Path,
    fiber_version: &str,
    triple: &str,
    channel: Channel,
    bases: &ReleaseBases,
    clock: &dyn Clock,
    io: Io<'_>,
    binary: &Path,
    origin: &extensions::Origin,
    restart: &dyn Fn(&Path) -> Result<bool, Failure>,
) -> Result<i32, CliError> {
    let tag = match channel {
        Channel::Stable => extensions::latest_stable_tag(&bases.web)?,
        Channel::Nightly => extensions::fetch_nightly_tag(&bases.api)?,
    };
    let (target, bare) = target_key(&tag, channel)?;
    let running = running_key(fiber_version)?;
    let already = target <= running;
    let version = if already {
        fiber_version.strip_prefix('v').unwrap_or(fiber_version)
    } else {
        &bare
    };
    let base = format!("{}/releases", bases.web.trim_end_matches('/'));
    if already {
        extensions::install_docs_if_missing(
            home,
            &extensions::Release {
                base: base.clone(),
                version: version.to_owned(),
                commit: String::new(),
            },
            clock,
        )?;
    }
    let staged = extensions::stage_update(
        home,
        &extensions::Target {
            base,
            version: version.to_owned(),
            triple: triple.to_owned(),
        },
        origin,
        clock,
    )?;
    let names: Vec<String> = staged
        .plan()
        .items()
        .map(|item| item.name.clone())
        .collect();
    let summaries = crate::extension::summaries(staged.plan());
    if !crate::extension::approve::install_approved(&summaries, io.terminal, io.input, io.err)? {
        writeln!(io.err, "fiber: nothing was installed.").unwrap_or(());
        return Ok(1);
    }
    let directory = binary.parent().unwrap_or(Path::new("/"));
    check_writable(directory)?;
    let staged_binary = staged.commit()?;
    if !staged_binary.is_file() {
        return Err(CliError::Extensions(extensions::Error::BadArchive {
            archive: format!("fiber-{triple}.tar.gz"),
            why: "holds no `fiber` binary".into(),
        }));
    }
    for name in &names {
        writeln!(io.err, "fiber: updated {name}").unwrap_or(());
    }
    writeln!(io.err, "fiber: updated docs").unwrap_or(());
    fs::rename(&staged_binary, binary).map_err(|source| CliError::UpdateBinaryRename {
        staged: staged_binary.clone(),
        binary: binary.to_path_buf(),
        source,
    })?;
    match restart(home) {
        Ok(true) => {}
        Ok(false) => crate::hub_service::stop_running_hub(home, clock),
        Err(restart) => {
            writeln!(
                io.err,
                "fiber: warning: the hub was not restarted: {}",
                restart.message
            )
            .unwrap_or(());
        }
    }
    Ok(0)
}

#[cfg(test)]
#[path = "update_tests.rs"]
mod tests;
