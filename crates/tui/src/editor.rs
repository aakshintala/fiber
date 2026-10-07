//! `open_in_editor`, Ctrl+G: the draft, or a paste token, in `$VISUAL` or
//! `$EDITOR` (`docs/tui.md`, "The input box", "Bindings").

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

/// The notice when neither variable names an editor.
pub(crate) const NO_EDITOR: &str = "Set $VISUAL or $EDITOR to edit the draft.";

/// How many names [`TempFile::create`] tries before giving up.
const ATTEMPTS: usize = 16;

/// The next temporary file's number in this process.
static NEXT: AtomicU64 = AtomicU64::new(0);

/// What the editor opens, and where its text goes back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Target {
    /// The whole draft, every token expanded.
    Draft,
    /// The paste token with this number.
    Token(usize),
    /// A focused conversation item, read only: what the editor returns
    /// is dropped.
    Item,
}

/// The editor command: `$VISUAL` when set and not empty, else `$EDITOR`
/// when set and not empty, read through `var`.
pub(crate) fn command(var: impl Fn(&str) -> Option<String>) -> Option<String> {
    ["VISUAL", "EDITOR"]
        .into_iter()
        .filter_map(var)
        .find(|value| !value.trim().is_empty())
}

/// Runs `command` on a new temporary file holding `text`, in the
/// foreground with this process's stdio, and returns the file's text
/// afterwards. The file holds the text and one line break, and one
/// trailing line break is dropped from what is read back, so a file left
/// as it was returns the same text. An error is the notice naming the
/// cause; the file is removed on every path.
pub(crate) fn run(command: &str, text: &str) -> Result<String, String> {
    run_in(&std::env::temp_dir(), command, text)
}

/// [`run`] with the temporary file in `dir`.
fn run_in(dir: &Path, command: &str, text: &str) -> Result<String, String> {
    let created = |err: io::Error| format!("Could not write a file for the editor: {err}");
    let (temp, mut file) = TempFile::create(dir).map_err(created)?;
    file.write_all(text.as_bytes())
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.flush())
        .map_err(created)?;
    drop(file);
    // `sh` splits the command, so a value with arguments such as `code -w`
    // works; the path goes in as `$1`, never through the split.
    let status = Command::new("/bin/sh")
        .arg("-c")
        .arg(format!("{command} \"$1\""))
        .arg("sh")
        .arg(&temp.0)
        .status()
        .map_err(|err| format!("Could not start the editor: {err}"))?;
    if let Some(signal) = status.signal() {
        return Err(format!(
            "The editor was ended by signal {signal}; the draft is unchanged."
        ));
    }
    if !status.success() {
        let code = status.code().unwrap_or_default();
        return Err(format!(
            "The editor exited with status {code}; the draft is unchanged."
        ));
    }
    let mut edited = fs::read_to_string(&temp.0)
        .map_err(|err| format!("Could not read the edited file: {err}"))?;
    if edited.ends_with('\n') {
        edited.pop();
    }
    Ok(edited)
}

/// The temporary file the editor opens, removed when dropped.
struct TempFile(PathBuf);

impl TempFile {
    /// Creates `fiber-draft-<pid>-<n>.md` in `dir`, new and readable by
    /// this user alone, trying the next `n` when the name is taken.
    fn create(dir: &Path) -> io::Result<(Self, File)> {
        let mut taken = None;
        for _ in 0..ATTEMPTS {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = dir.join(format!("fiber-draft-{}-{n}.md", std::process::id()));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
            {
                Ok(file) => return Ok((Self(path), file)),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => taken = Some(err),
                Err(err) => return Err(err),
            }
        }
        Err(taken.unwrap_or_else(|| io::Error::other("no free name")))
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        fs::remove_file(&self.0).unwrap_or(());
    }
}

#[cfg(test)]
#[path = "editor_tests.rs"]
mod tests;
