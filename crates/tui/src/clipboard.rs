//! Copying to the clipboard (`docs/tui.md`, "Selection and copy"): OSC 52
//! to the terminal, and on a local session the system clipboard command as
//! well.

use std::ffi::OsString;
use std::io::{self, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitStatus, Stdio};
use std::thread::JoinHandle;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

/// The system clipboard commands, in the order tried: macOS, then Wayland,
/// then the two X11 tools.
const COMMANDS: &[&[&str]] = &[
    &["pbcopy"],
    &["wl-copy"],
    &["xclip", "-selection", "clipboard"],
    &["xsel", "--clipboard", "--input"],
];

/// The OSC 52 sequence that sets the clipboard to `text`.
pub(crate) fn osc52(text: &str) -> Vec<u8> {
    let mut out = b"\x1b]52;c;".to_vec();
    out.extend_from_slice(STANDARD.encode(text).as_bytes());
    out.push(0x07);
    out
}

/// The system clipboard command to pipe a copy to: the first of
/// [`COMMANDS`] whose program `exists`, or none on a session over SSH,
/// which `env` shows by `SSH_CONNECTION` or `SSH_TTY`.
pub(crate) fn command(
    env: impl Fn(&str) -> Option<OsString>,
    exists: impl Fn(&str) -> bool,
) -> Option<Vec<&'static str>> {
    if env("SSH_CONNECTION").is_some() || env("SSH_TTY").is_some() {
        return None;
    }
    COMMANDS
        .iter()
        .find(|argv| argv.first().is_some_and(|program| exists(program)))
        .map(|argv| argv.to_vec())
}

/// Whether `program` is a file in a directory on `PATH`.
pub(crate) fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

/// Pipes `text` to the command `argv` on a thread of its own: spawn with
/// standard input piped, write, close it, wait. The caller never waits on
/// it, so a command that hangs holds only that thread. The command runs in
/// its own process group, away from the terminal's signals.
pub(crate) fn pipe(
    argv: Vec<String>,
    text: String,
) -> io::Result<JoinHandle<io::Result<ExitStatus>>> {
    crate::sources::builder("tui-clipboard").spawn(move || {
        let (program, args) = argv
            .split_first()
            .ok_or_else(|| io::Error::other("no clipboard command"))?;
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()?;
        let written = child
            .stdin
            .take()
            .map_or(Ok(()), |mut stdin| stdin.write_all(text.as_bytes()));
        let status = child.wait()?;
        written.map(|()| status)
    })
}

/// Copies `text`: OSC 52 to `tty`, then `command` on its own thread. A
/// failed write or a missing or failing command is dropped: the other route
/// may still have copied.
pub(crate) fn copy(tty: Option<&std::fs::File>, command: Option<&[String]>, text: String) {
    if let Some(mut out) = tty {
        out.write_all(&osc52(&text))
            .and_then(|()| out.flush())
            .unwrap_or(());
    }
    if let Some(argv) = command {
        drop(pipe(argv.to_vec(), text));
    }
}

#[cfg(test)]
#[path = "clipboard_tests.rs"]
mod tests;
