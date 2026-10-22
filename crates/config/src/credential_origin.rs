//! Where a provider's labels come from, without running or reading a key
//! (`docs/model-routing.md`, "Credentials"): the stored credential of the
//! label first, then the source the person configured for that label, then,
//! for the label `default`, the one the provider's data declares. Listing a
//! label's origin reads no key; only [`Credentials::key_presence`] reads one,
//! and only for the label a session would use, to test whether it is blank.
//! Every byte it reads comes from a descriptor that a non-blocking open
//! returned and `fstat` proved regular first, so a path swapped for a FIFO
//! after the listing is never waited on and never read.

use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

use crate::credential::{Credentials, DEFAULT_LABEL};
use crate::credential_file::credential_path;
use crate::error::ConfigError;
use crate::extension::ProviderData;
use crate::home::plain;
use crate::secret::CredentialSource;

/// Where a label's key comes from, decided without running or reading it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// A stored credential owns the label.
    Stored,
    /// An environment variable holding the key.
    Env(String),
    /// A file holding the key.
    File(PathBuf),
    /// A program that would print the key, by its program alone, since its
    /// arguments may hold a key. Doctor never runs it.
    Command(String),
}

/// Whether a label's key is there, without running a command or reading a
/// non-regular descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    /// A key is there and not blank.
    Present,
    /// Nothing is there, or what is there holds no key.
    Absent,
    /// A stored credential is there but blank: a session's
    /// `credential_failed`.
    Empty,
    /// A command source, which doctor never runs.
    Unchecked,
}

/// Which source [`Credentials::locate`] chose: the one precedence, so
/// [`Credentials::credential_with`], [`Credentials::credential_origins`]
/// and [`Credentials::key_presence`] never disagree about it.
#[derive(Debug, Clone)]
pub(crate) enum Located {
    /// A regular stored file exists; it owns the label even when one is
    /// configured too.
    Stored,
    /// The configured source, or the provider's own for `default`.
    Source(CredentialSource),
    /// Nothing stored or configured.
    Nothing,
}

impl Credentials {
    /// Every label of `provider`, sorted, each with its origin under
    /// [`Credentials::credential_with`]'s precedence. Reads no key: a
    /// stored credential is found by its regular file existing, never by
    /// reading it.
    pub fn credential_origins(
        &self,
        provider: &ProviderData,
    ) -> Vec<(String, Result<Origin, ConfigError>)> {
        self.labels(provider)
            .into_iter()
            .map(|label| {
                let origin = match self.locate(provider, &label) {
                    Ok(Located::Stored) => Ok(Origin::Stored),
                    Ok(Located::Source(source)) => Ok(match source {
                        CredentialSource::Env(var) => Origin::Env(var),
                        CredentialSource::File(file) => Origin::File(file),
                        CredentialSource::Command { argv, .. } => {
                            Origin::Command(argv.first().cloned().unwrap_or_default())
                        }
                    }),
                    // The label was listed but nothing answers for it: its
                    // stored file vanished between the listing and the
                    // check. Reads no key either way.
                    Ok(Located::Nothing) => Err(vanished(self.home(), provider, &label)),
                    Err(error) => Err(error),
                };
                (label, origin)
            })
            .collect()
    }

    /// Whether the key under `label` is there, as `credential_with` would
    /// look for it: stored, configured, own (`default` only). `env` answers
    /// for environment variables, so tests control it. A command is
    /// [`Presence::Unchecked`], never run. Only this function reads a key,
    /// and only to test whether it is blank; the key is never returned.
    pub fn key_presence(
        &self,
        provider: &ProviderData,
        label: &str,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Presence, ConfigError> {
        let stored_name = provider
            .credential_name
            .as_deref()
            .unwrap_or(&provider.name);
        match self.locate(provider, label)? {
            Located::Stored => {
                let path = credential_path(self.home(), stored_name, label)?;
                match read_validated(&path, true) {
                    Ok(text) => Ok(if text.trim().is_empty() {
                        Presence::Empty
                    } else {
                        Presence::Present
                    }),
                    // A stored file owns its label even when it vanishes
                    // before its check: that is its failure, never a
                    // silent fall back to another source.
                    Err(OpenFail::NotFound(_)) => Err(ConfigError::Io {
                        file: path,
                        source: io::Error::from(ErrorKind::NotFound),
                    }),
                    // A link, FIFO, directory or any other non-regular
                    // descriptor is refused as stored, as `plain` refuses
                    // it at listing time.
                    Err(OpenFail::IsDir) | Err(OpenFail::NotRegular) => {
                        Err(ConfigError::NotPlain { file: path })
                    }
                    Err(OpenFail::Other(source)) => Err(ConfigError::Io { file: path, source }),
                }
            }
            Located::Source(source) => match source {
                CredentialSource::Env(var) => Ok(match env(&var) {
                    Some(value) if !value.trim().is_empty() => Presence::Present,
                    _ => Presence::Absent,
                }),
                CredentialSource::File(file) => {
                    match read_validated(&file, false) {
                        Ok(text) => Ok(if text.trim().is_empty() {
                            Presence::Absent
                        } else {
                            Presence::Present
                        }),
                        // A missing file holds no key, as a session's
                        // `NotFound`-to-missing arm says.
                        Err(OpenFail::NotFound(_)) => Ok(Presence::Absent),
                        // A directory where the key file should be is the
                        // session's non-`NotFound` `Io` arm, without
                        // opening or reading it.
                        Err(OpenFail::IsDir) => Err(ConfigError::Io {
                            file: file.clone(),
                            source: is_directory_error(),
                        }),
                        // A FIFO or any other non-regular file is never
                        // opened for reading: unlike a session, doctor
                        // never waits for a FIFO writer.
                        Err(OpenFail::NotRegular) => Ok(Presence::Absent),
                        Err(OpenFail::Other(source)) => Err(ConfigError::Io {
                            file: file.clone(),
                            source,
                        }),
                    }
                }
                CredentialSource::Command { .. } => Ok(Presence::Unchecked),
            },
            Located::Nothing => Ok(Presence::Absent),
        }
    }

    /// The one precedence: stored, else configured, else the provider's own
    /// for `default`. Reads no key. [`Credentials::credential_with`],
    /// [`Credentials::credential_origins`] and
    /// [`Credentials::key_presence`] all call it, so the three never
    /// disagree about which source answers for a label.
    pub(crate) fn locate(
        &self,
        provider: &ProviderData,
        label: &str,
    ) -> Result<Located, ConfigError> {
        let stored = provider
            .credential_name
            .as_deref()
            .unwrap_or(&provider.name);
        // The path checks first: an invalid label name, a link on the way
        // there, or a link, FIFO or directory at the label itself refuses
        // the stored path, as `read_credential` refuses it.
        let path = credential_path(self.home(), stored, label)?;
        if plain(&path, false)? {
            return Ok(Located::Stored);
        }
        if let Some(source) = self.configured_source(&provider.name, label) {
            return Ok(Located::Source(source));
        }
        if label == DEFAULT_LABEL
            && let Some(own) = provider.credential_from.credential().cloned()
        {
            return Ok(Located::Source(own));
        }
        Ok(Located::Nothing)
    }
}

/// A label that was listed but answers nothing: its stored file vanished
/// between the listing and the check.
fn vanished(home: &Path, provider: &ProviderData, label: &str) -> ConfigError {
    let stored = provider
        .credential_name
        .as_deref()
        .unwrap_or(&provider.name);
    match credential_path(home, stored, label) {
        Ok(file) => ConfigError::Io {
            file,
            source: io::Error::from(ErrorKind::NotFound),
        },
        Err(error) => error,
    }
}

/// An I/O error of kind `IsADirectory`, without opening or reading
/// anything: the session's `fs::read` fails the same way on a directory.
pub(crate) fn is_directory_error() -> io::Error {
    io::Error::from_raw_os_error(rustix::io::Errno::ISDIR.raw_os_error())
}

/// Why a validated read gave no text.
#[derive(Debug)]
pub(crate) enum OpenFail {
    /// Nothing is there, with the operating system's error, so a reader
    /// that reports it names the same failure `fs::read` would.
    NotFound(io::Error),
    /// A directory is there, unread.
    IsDir,
    /// A symbolic link, FIFO or any other non-regular file is there: never
    /// opened for reading.
    NotRegular,
    /// Any other I/O failure.
    Other(io::Error),
}

/// A descriptor for the regular file at `path`, opened for a
/// non-blocking read: the shared half of a validated read, so the script
/// read and the credential read prove the same thing. With `nofollow`
/// the open refuses symbolic links, as stored credentials do; without it
/// the open follows them, as a session's configured file source does. No
/// bytes are read before the descriptor check, and a FIFO is never opened
/// for reading, so no call waits for a writer however the path changes
/// under it.
pub(crate) fn open_validated(path: &Path, nofollow: bool) -> Result<std::fs::File, OpenFail> {
    // The metadata check first, so a directory or FIFO seen there is never
    // opened; the descriptor check after the open decides, so a swap in
    // between cannot move the read.
    let meta = if nofollow {
        fs::symlink_metadata(path)
    } else {
        fs::metadata(path)
    };
    match meta {
        Err(error) if error.kind() == ErrorKind::NotFound => return Err(OpenFail::NotFound(error)),
        Err(error) => return Err(OpenFail::Other(error)),
        Ok(meta) => {
            if meta.is_dir() {
                return Err(OpenFail::IsDir);
            }
            if !meta.is_file() {
                return Err(OpenFail::NotRegular);
            }
        }
    }
    seam::fire_pre_open(path);
    let mut flags = rustix::fs::OFlags::RDONLY
        .union(rustix::fs::OFlags::NONBLOCK)
        .union(rustix::fs::OFlags::CLOEXEC);
    if nofollow {
        flags = flags.union(rustix::fs::OFlags::NOFOLLOW);
    }
    let fd = match rustix::fs::open(path, flags, rustix::fs::Mode::empty()) {
        Ok(fd) => fd,
        Err(error) if error == rustix::io::Errno::NOENT => {
            return Err(OpenFail::NotFound(io::Error::from(error)));
        }
        Err(error) if nofollow && error == rustix::io::Errno::LOOP => {
            return Err(OpenFail::NotRegular);
        }
        Err(error) => return Err(OpenFail::Other(io::Error::from(error))),
    };
    let stat = rustix::fs::fstat(&fd).map_err(|error| OpenFail::Other(io::Error::from(error)))?;
    match rustix::fs::FileType::from_raw_mode(stat.st_mode) {
        rustix::fs::FileType::Directory => return Err(OpenFail::IsDir),
        rustix::fs::FileType::RegularFile => {}
        rustix::fs::FileType::Symlink
        | rustix::fs::FileType::Fifo
        | rustix::fs::FileType::Socket
        | rustix::fs::FileType::CharacterDevice
        | rustix::fs::FileType::BlockDevice
        | rustix::fs::FileType::Unknown => return Err(OpenFail::NotRegular),
    }
    Ok(std::fs::File::from(fd))
}

/// The text of the regular file at `path`, read only after a descriptor
/// for it proved regular. With `nofollow` the open refuses symbolic links,
/// as stored credentials do; without it the open follows them, as a
/// session's configured file source does. No bytes are read before the
/// descriptor check, and a FIFO is never opened for reading, so no call
/// waits for a writer however the path changes under it.
pub(crate) fn read_validated(path: &Path, nofollow: bool) -> Result<String, OpenFail> {
    let mut file = open_validated(path, nofollow)?;
    seam::note_read(path);
    let mut text = String::new();
    std::io::Read::read_to_string(&mut file, &mut text).map_err(OpenFail::Other)?;
    Ok(text)
}

/// The test-only seams: a pause between the metadata check and the open,
/// and an observer that marks every descriptor the reader actually reads.
/// Production builds both to nothing.
#[cfg(test)]
pub(crate) mod seam {

    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, mpsc};

    /// One armed pause: the path whose open waits, and the channels that
    /// tell the test it is waiting and release it.
    struct Gate {
        /// The path whose open waits.
        target: PathBuf,
        /// Tells the test the open is waiting.
        arrived: mpsc::Sender<()>,
        /// The test releases the open through it.
        release: mpsc::Receiver<()>,
    }

    /// The armed pauses, in arm order.
    static GATES: Mutex<Vec<Gate>> = Mutex::new(Vec::new());

    /// Every path the reader read from, in read order.
    static READS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

    /// Removes every gate for `target`.
    fn disarm(target: &Path) {
        GATES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retain(|gate| gate.target != target);
    }

    /// Holds an armed pause; dropping it disarms the path, so a test that
    /// fails before releasing leaves no gate behind.
    pub(crate) struct Pause {
        /// The path the pause was armed for.
        target: PathBuf,
    }

    impl Drop for Pause {
        fn drop(&mut self) {
            disarm(&self.target);
        }
    }

    /// Pauses the next validated open of `target` between its metadata
    /// check and its open. Returns the arrival the open signals through,
    /// the release the test drops or sends through, and the guard that
    /// disarms the path when it drops.
    pub(crate) fn arm_pre_open(target: &Path) -> (mpsc::Receiver<()>, mpsc::Sender<()>, Pause) {
        let (arrived_tx, arrived_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        GATES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(Gate {
                target: target.to_path_buf(),
                arrived: arrived_tx,
                release: release_rx,
            });
        (
            arrived_rx,
            release_tx,
            Pause {
                target: target.to_path_buf(),
            },
        )
    }

    /// Runs after the metadata check accepted a regular file and before
    /// the non-blocking open. An armed path signals its arrival and waits
    /// for the test's release; any other path returns at once.
    pub(crate) fn fire_pre_open(path: &Path) {
        let gate = {
            let mut gates = GATES
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            gates
                .iter()
                .position(|gate| gate.target == path)
                .map(|at| gates.remove(at))
        };
        let Some(gate) = gate else { return };
        gate.arrived.send(()).unwrap_or(());
        gate.release.recv().unwrap_or(());
    }

    /// Marks `path` as read: called only after the descriptor proved
    /// regular, just before the first byte is read.
    pub(crate) fn note_read(path: &Path) {
        READS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(path.to_path_buf());
    }

    /// Takes every path marked read since the last call.
    pub(crate) fn take_reads() -> Vec<PathBuf> {
        std::mem::take(
            &mut *READS
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }
}

#[cfg(not(test))]
pub(crate) mod seam {
    //! Production seams: both compile to nothing.

    use std::path::Path;

    /// No pause outside tests.
    pub(crate) fn fire_pre_open(_path: &Path) {}

    /// No read mark outside tests.
    pub(crate) fn note_read(_path: &Path) {}
}

#[cfg(test)]
#[path = "credential_origin_tests.rs"]
mod tests;
