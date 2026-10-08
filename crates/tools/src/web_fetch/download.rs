//! A downloaded page on its way to its result (`docs/tools.md`,
//! "web_fetch"): saved to `artifacts/` and converted to markdown piece by
//! piece as it arrives, so no whole copy of the page is held beside its
//! markdown.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::charset::{self, Decoding, PRESCAN};
use super::markdown::Stream;

/// Wraps an artifact's file in a test's own writer.
pub(super) type Wrap = Arc<dyn Fn(File) -> Box<dyn Write + Send> + Send + Sync>;

/// The size of the pieces a download is handled in.
pub(super) const PIECE: usize = 65_536;

/// An HTML page converting to markdown as its bytes arrive. Its character
/// set is chosen from its first 1024 bytes, as a whole page's would be
/// (`docs/tools.md`, "HTML to markdown"), so the markdown is the same
/// however the bytes are cut into pieces.
pub(super) struct Html {
    content_type: Option<String>,
    /// The first bytes, held until 1024 have arrived or the page ends.
    held: Vec<u8>,
    /// Set once the character set is chosen.
    decoding: Option<Decoding>,
    stream: Stream,
}

impl Html {
    /// A page served with `content_type`.
    pub(super) fn new(content_type: Option<&str>) -> Self {
        Self {
            content_type: content_type.map(str::to_owned),
            held: Vec::new(),
            decoding: None,
            stream: Stream::default(),
        }
    }

    /// Converts the next bytes of the page.
    pub(super) fn push(&mut self, bytes: &[u8]) {
        let rest = if self.decoding.is_some() {
            bytes
        } else {
            let wanted = PRESCAN.saturating_sub(self.held.len()).min(bytes.len());
            let (head, rest) = bytes.split_at(wanted);
            self.held.extend_from_slice(head);
            if self.held.len() < PRESCAN {
                return;
            }
            self.start();
            rest
        };
        self.feed(rest, false);
    }

    /// Ends the page: its markdown.
    pub(super) fn finish(mut self) -> String {
        if self.decoding.is_none() {
            self.start();
        }
        self.feed(&[], true);
        self.stream.finish()
    }

    /// Chooses the character set from the held bytes, then converts them.
    fn start(&mut self) {
        let encoding = charset::encoding(self.content_type.as_deref(), &self.held);
        self.decoding = Some(Decoding::new(encoding));
        let held = std::mem::take(&mut self.held);
        self.feed(&held, false);
    }

    fn feed(&mut self, bytes: &[u8], last: bool) {
        let Some(decoding) = &mut self.decoding else {
            return;
        };
        let stream = &mut self.stream;
        decoding.push(bytes, last, &mut |text| stream.push(text));
    }
}

/// A download being saved to `artifacts/`. Its file is removed when it is
/// dropped unless [`Artifact::keep`] gave its path, so a call that fails,
/// times out or is stopped leaves nothing behind. Only a file this artifact
/// created is ever written or removed.
pub(super) struct Artifact {
    path: PathBuf,
    /// The open file, until a failure closes it.
    file: Option<Box<dyn Write + Send>>,
    /// Whether the file is this artifact's own and still on disk.
    created: bool,
    /// Why the download could not be saved, once it could not.
    failed: Option<String>,
}

impl Artifact {
    /// Creates `<stem>.<extension>` in `dir`, and `dir` if it is missing.
    /// A failure is recorded for [`Artifact::keep`], never returned.
    pub(super) fn create(dir: &Path, stem: &str, extension: &str, wrap: Option<&Wrap>) -> Self {
        let path = dir.join(format!("{stem}.{extension}"));
        let opened = fs::create_dir_all(dir)
            .and_then(|()| OpenOptions::new().write(true).create_new(true).open(&path));
        match opened {
            Ok(file) => Self {
                file: Some(match wrap {
                    Some(wrap) => wrap(file),
                    None => Box::new(file),
                }),
                created: true,
                failed: None,
                path,
            },
            Err(error) => Self {
                failed: Some(unsaved(&path, &error)),
                file: None,
                created: false,
                path,
            },
        }
    }

    /// Appends `bytes`. After a failure it writes nothing: the failure is
    /// recorded and the partial file removed.
    pub(super) fn write(&mut self, bytes: &[u8]) {
        let Some(file) = &mut self.file else {
            return;
        };
        if let Err(error) = file.write_all(bytes) {
            self.failed = Some(unsaved(&self.path, &error));
            self.file = None;
            self.remove();
        }
    }

    /// Whether every write so far reached the file.
    fn saving(&self) -> bool {
        self.failed.is_none()
    }

    /// The saved file's path, kept on disk, or why it could not be saved.
    pub(super) fn keep(mut self) -> Result<String, String> {
        match self.failed.take() {
            Some(message) => Err(message),
            None => {
                self.created = false;
                Ok(self.path.display().to_string())
            }
        }
    }

    /// Removes the file if it is this artifact's own. A removal that fails
    /// leaves the file: nothing is left to report it to.
    fn remove(&mut self) {
        if self.created {
            self.created = false;
            let _removed = fs::remove_file(&self.path);
        }
    }
}

impl Drop for Artifact {
    fn drop(&mut self) {
        self.file = None;
        self.remove();
    }
}

/// Where a download's pieces go as they are read: its artifact, if it is
/// saved, and its converter, if it is HTML. A body that is neither is only
/// counted.
pub(super) struct Sink<'a> {
    pub(super) artifact: Option<Artifact>,
    pub(super) html: Option<Html>,
    /// Whether the fetch was stopped: checked before each piece.
    pub(super) stopped: &'a dyn Fn() -> bool,
}

impl Write for Sink<'_> {
    /// Takes one piece. Once the fetch is stopped it takes nothing more and
    /// fails, ending the read. A save failure never fails the read: the
    /// body is still counted to the limit, so a page too large is reported
    /// as too large, and from then on nothing is saved or converted.
    fn write(&mut self, piece: &[u8]) -> io::Result<usize> {
        if (self.stopped)() {
            return Err(io::Error::other("the fetch was stopped"));
        }
        let saving = match &mut self.artifact {
            Some(artifact) => {
                artifact.write(piece);
                artifact.saving()
            }
            None => false,
        };
        if saving && let Some(html) = &mut self.html {
            html.push(piece);
        }
        Ok(piece.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Reads `body` into `sink` in pieces of at most 64 KiB, at most `limit`
/// bytes: how many were read.
pub(super) fn copy(body: &mut dyn Read, limit: u64, sink: &mut Sink<'_>) -> io::Result<u64> {
    let mut reader = BufReader::with_capacity(PIECE, body.take(limit));
    io::copy(&mut reader, sink)
}

/// The message for a download that could not be saved to `path`.
fn unsaved(path: &Path, error: &io::Error) -> String {
    format!(
        "could not save the download to {}: {error}.",
        path.display()
    )
}

#[cfg(test)]
#[path = "download_tests.rs"]
mod tests;
