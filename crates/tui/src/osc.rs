//! Operating system commands the terminal writes beside its frames: the
//! window title (`docs/tui.md`, "State glyphs", "Naming the session").

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

/// OSC 2 setting the window title to `text`, with every C0 and C1 control
/// character and DEL dropped so a session's name cannot end the sequence
/// or start another.
pub(crate) fn title(text: &str) -> Vec<u8> {
    let clean: String = text.chars().filter(|ch| !ch.is_control()).collect();
    let mut out = b"\x1b]2;".to_vec();
    out.extend_from_slice(clean.as_bytes());
    out.push(0x07);
    out
}

#[cfg(test)]
#[path = "osc_tests.rs"]
mod tests;
