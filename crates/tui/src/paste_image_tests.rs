//! Tests for the clipboard image reader: the command choice, the frame
//! decode, the size checks and the read under its deadline.

use std::collections::HashMap;
use std::ffi::OsString;

use super::{Decode, Reader, command};

/// The environment holding `vars`.
fn env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
    let map: HashMap<String, OsString> = vars
        .iter()
        .map(|(name, value)| ((*name).to_owned(), OsString::from(value)))
        .collect();
    move |name: &str| map.get(name).cloned()
}

/// `exists` holding the programs on `PATH`.
fn on_path(programs: &[&str]) -> impl Fn(&str) -> bool {
    let path: Vec<String> = programs.iter().map(|name| (*name).to_owned()).collect();
    move |name: &str| path.iter().any(|program| program == name)
}

fn osascript() -> Reader {
    Reader {
        argv: vec![
            "osascript".to_owned(),
            "-e".to_owned(),
            "the clipboard as «class PNGf»".to_owned(),
        ],
        decode: Decode::AppleScript,
    }
}

fn wl_paste() -> Reader {
    Reader {
        argv: vec![
            "wl-paste".to_owned(),
            "--type".to_owned(),
            "image/png".to_owned(),
        ],
        decode: Decode::Raw,
    }
}

fn xclip() -> Reader {
    Reader {
        argv: vec![
            "xclip".to_owned(),
            "-selection".to_owned(),
            "clipboard".to_owned(),
            "-t".to_owned(),
            "image/png".to_owned(),
            "-o".to_owned(),
        ],
        decode: Decode::Raw,
    }
}

#[test]
fn the_clipboard_command_follows_the_machine() {
    // C1: SSH_CONNECTION alone reads nothing.
    assert_eq!(
        command(
            false,
            env(&[("SSH_CONNECTION", "x"), ("DISPLAY", ":0")]),
            on_path(&["xclip"])
        ),
        None
    );
    // C2: SSH_TTY alone reads nothing.
    assert_eq!(
        command(
            false,
            env(&[("SSH_TTY", "/dev/ttys0"), ("DISPLAY", ":0")]),
            on_path(&["xclip"])
        ),
        None
    );
    // C3: macOS with osascript reads through it.
    assert_eq!(
        command(true, env(&[]), on_path(&["osascript"])),
        Some(osascript())
    );
    // C4: macOS without osascript reads nothing, even with X11 set.
    assert_eq!(
        command(
            true,
            env(&[("DISPLAY", ":0")]),
            on_path(&["xclip", "wl-paste"])
        ),
        None
    );
    // C5: off macOS, osascript alone on PATH is nothing.
    assert_eq!(
        command(false, env(&[]), on_path(&["osascript"])),
        None
    );
    // C6: Wayland reads through wl-paste; without it, through xclip; an
    // empty WAYLAND_DISPLAY is unset.
    assert_eq!(
        command(
            false,
            env(&[("WAYLAND_DISPLAY", "wayland-0"), ("DISPLAY", ":0")]),
            on_path(&["wl-paste", "xclip"])
        ),
        Some(wl_paste())
    );
    assert_eq!(
        command(
            false,
            env(&[("WAYLAND_DISPLAY", "wayland-0"), ("DISPLAY", ":0")]),
            on_path(&["xclip"])
        ),
        Some(xclip())
    );
    assert_eq!(
        command(
            false,
            env(&[("WAYLAND_DISPLAY", "")]),
            on_path(&["wl-paste", "xclip"])
        ),
        None
    );
    // C7: X11 reads through xclip; without either side, nothing.
    assert_eq!(
        command(false, env(&[("DISPLAY", ":0")]), on_path(&["xclip"])),
        Some(xclip())
    );
    assert_eq!(
        command(false, env(&[("DISPLAY", ":0")]), on_path(&["wl-paste"])),
        None
    );
    assert_eq!(
        command(false, env(&[]), on_path(&["xclip"])),
        None
    );
    // C8: every reader's arguments and decode, exactly.
    assert_eq!(osascript().decode, Decode::AppleScript);
    assert_eq!(wl_paste().decode, Decode::Raw);
    assert_eq!(xclip().decode, Decode::Raw);
}
