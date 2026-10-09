//! Operating system commands the terminal writes beside its frames: the
//! window title and the desktop notification (`docs/tui.md`, "State
//! glyphs", "Naming the session", "Getting the person's attention").

/// The title as last written, so an unchanged one writes nothing.
#[derive(Debug, Default)]
pub(crate) struct Title {
    last: Option<String>,
}

impl Title {
    /// The bytes setting the title to `text`, or `None` when it is the
    /// title last written.
    pub(crate) fn next(&mut self, text: String) -> Option<Vec<u8>> {
        if self.last.as_ref() == Some(&text) {
            return None;
        }
        let bytes = title(&text);
        self.last = Some(text);
        Some(bytes)
    }

    /// Forgets the title last written, so the next one is written whatever
    /// it is: after another program had the terminal.
    pub(crate) fn forget(&mut self) {
        self.last = None;
    }
}

/// Every C0 and C1 control character and DEL dropped from `text`, so a
/// session's name cannot end the sequence or start another.
pub(crate) fn clean(text: &str) -> String {
    text.chars().filter(|ch| !ch.is_control()).collect()
}

/// OSC 2 setting the window title to `text`, with every C0 and C1 control
/// character and DEL dropped so a session's name cannot end the sequence
/// or start another.
pub(crate) fn title(text: &str) -> Vec<u8> {
    let mut out = b"\x1b]2;".to_vec();
    out.extend_from_slice(clean(text).as_bytes());
    out.push(0x07);
    out
}

/// OSC 9 sending the desktop notification `text` (`docs/tui.md`,
/// "Getting the person's attention"), with every C0 and C1 control
/// character and DEL dropped so a session's name cannot end the sequence
/// or start another.
pub(crate) fn notify(text: &str) -> Vec<u8> {
    let mut out = b"\x1b]9;".to_vec();
    out.extend_from_slice(clean(text).as_bytes());
    out.push(0x07);
    out
}

/// OSC 22 setting the pointer to the resize arrow, or back to the
/// default (`docs/tui.md`, "Layout").
pub(crate) fn pointer(resize: bool) -> &'static [u8] {
    if resize {
        b"\x1b]22;col-resize\x1b\\"
    } else {
        b"\x1b]22;default\x1b\\"
    }
}

/// The pointer shape as last written; it starts as the default.
#[derive(Debug, Default)]
pub(crate) struct Shape {
    resize: bool,
}

impl Shape {
    /// The bytes setting the shape, or `None` when it is the shape last
    /// written.
    pub(crate) fn next(&mut self, resize: bool) -> Option<&'static [u8]> {
        if resize == self.resize {
            return None;
        }
        self.resize = resize;
        Some(pointer(resize))
    }

    /// The terminal shows the default shape again: after the restore.
    pub(crate) fn reset(&mut self) {
        self.resize = false;
    }
}

#[cfg(test)]
#[path = "osc_tests.rs"]
mod tests;
