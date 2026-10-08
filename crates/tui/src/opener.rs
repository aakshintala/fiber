//! Opening links (`docs/tui.md`, "Links"): which destinations a click may
//! open, and which program opens them. A destination must name `http`,
//! `https` or `mailto`, carry no whitespace or control character, and fit
//! in 2,048 bytes; anything else draws underlined with no target. A click
//! runs `open` or `xdg-open` on the terminal's machine; over SSH or with
//! no opener it copies instead.

use std::ffi::OsString;

/// The longest destination a click opens.
const MAX_URL: usize = 2_048;

/// Whether `url` may open on click: scheme `http`, `https` or `mailto` in
/// any case, no whitespace or control character, at most 2,048 bytes.
pub(crate) fn valid(url: &str) -> bool {
    if url.is_empty() || url.len() > MAX_URL {
        return false;
    }
    if url.chars().any(|ch| ch.is_whitespace() || ch.is_control()) {
        return false;
    }
    let Some((scheme, _)) = url.split_once(':') else {
        return false;
    };
    matches!(
        scheme.to_ascii_lowercase().as_str(),
        "http" | "https" | "mailto"
    )
}

/// The program that opens a URL: none over SSH, else `open`, else
/// `xdg-open`, else none. It runs as [`crate::clipboard::pipe`] runs a
/// copy: its own thread and process group, never a shell, never waited on.
pub(crate) fn command(
    env: impl Fn(&str) -> Option<OsString>,
    exists: impl Fn(&str) -> bool,
) -> Option<Vec<&'static str>> {
    if env("SSH_CONNECTION").is_some() || env("SSH_TTY").is_some() {
        return None;
    }
    if exists("open") {
        return Some(vec!["open"]);
    }
    if exists("xdg-open") {
        return Some(vec!["xdg-open"]);
    }
    None
}

#[cfg(test)]
#[path = "opener_tests.rs"]
mod tests;
