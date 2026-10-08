//! Reading an image from the system clipboard (`docs/tui.md`, "The input
//! box"): the command for this machine, and the read under a deadline on
//! the injected clock.

use std::ffi::OsString;

/// How the command's standard output reads: raw PNG bytes, or
/// `osascript`'s `«data PNGf<hex>»` frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code, reason = "the loop wires the reader in a later task")]
pub(crate) enum Decode {
    /// `wl-paste` and `xclip` print the PNG byte for byte.
    Raw,
    /// `osascript` prints the bytes as hex inside a frame.
    AppleScript,
}

/// The clipboard command for this machine: its arguments and how its
/// output decodes.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code, reason = "the loop wires the reader in a later task")]
pub(crate) struct Reader {
    /// The program and its arguments.
    pub(crate) argv: Vec<String>,
    /// How standard output decodes.
    pub(crate) decode: Decode,
}

/// Whether an environment variable counts as set: present and not empty.
#[allow(dead_code, reason = "the loop wires the reader in a later task")]
fn is_set(env: &dyn Fn(&str) -> Option<OsString>, name: &str) -> bool {
    env(name).is_some_and(|value| !value.is_empty())
}

/// The clipboard command for this machine: over SSH there is none; on
/// macOS `osascript` reading `«class PNGf»` when it is on `PATH`, else
/// none; elsewhere `wl-paste --type image/png` under Wayland, else
/// `xclip -selection clipboard -t image/png -o`, each when its display
/// variable is set and its program is on `PATH`.
#[allow(dead_code, reason = "the loop wires the reader in a later task")]
pub(crate) fn command(
    macos: bool,
    env: impl Fn(&str) -> Option<OsString>,
    exists: impl Fn(&str) -> bool,
) -> Option<Reader> {
    if crate::clipboard::over_ssh(&env) {
        return None;
    }
    if macos {
        return exists("osascript").then(|| Reader {
            argv: vec![
                "osascript".to_owned(),
                "-e".to_owned(),
                "the clipboard as «class PNGf»".to_owned(),
            ],
            decode: Decode::AppleScript,
        });
    }
    let env = &env;
    let is_set = |name: &str| is_set(env, name);
    if is_set("WAYLAND_DISPLAY") && exists("wl-paste") {
        return Some(Reader {
            argv: vec![
                "wl-paste".to_owned(),
                "--type".to_owned(),
                "image/png".to_owned(),
            ],
            decode: Decode::Raw,
        });
    }
    if is_set("DISPLAY") && exists("xclip") {
        return Some(Reader {
            argv: vec![
                "xclip".to_owned(),
                "-selection".to_owned(),
                "clipboard".to_owned(),
                "-t".to_owned(),
                "image/png".to_owned(),
                "-o".to_owned(),
            ],
            decode: Decode::Raw,
        });
    }
    None
}

#[cfg(test)]
#[path = "paste_image_tests.rs"]
mod tests;
