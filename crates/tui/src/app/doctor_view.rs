//! The `/doctor` view (`docs/tui.md`, "Swapped views"): the report
//! `fiber doctor` prints, captured when the view opens and shown one
//! plain row per snapshot line, from the same code that prints it, so
//! the two never differ. It reads a local snapshot through no seam, so
//! its key, edit and click routes never reach the configure seam or a
//! workspace.

use crate::keys::Key;
use crate::settings_view::Act;
use crate::swapped::{Frame, Ink, List};

/// `/doctor`'s state: the scrolled list and the snapshot lines.
#[derive(Debug)]
pub(crate) struct Doctor {
    /// The scrolled list over the snapshot rows.
    list: List,
    /// The captured report, one entry per line.
    lines: Vec<String>,
}

impl Doctor {
    /// Shows the captured `report`: the scrolled list and the snapshot
    /// lines, one plain row per line, so the view renders the capture
    /// verbatim.
    pub(crate) fn open(report: &str) -> Self {
        Self {
            list: List::default(),
            lines: report.lines().map(str::to_owned).collect(),
        }
    }

    /// Hands `key` to the view: Esc closes, and scrolling never reaches
    /// the configure seam or a workspace.
    pub(crate) fn key(&mut self, key: &Key, height: usize) -> Act {
        if *key == Key::Esc {
            return Act::Close;
        }
        let frame = self.frame();
        let shown = crate::swapped::rows_height(&frame, height);
        self.list.key(key, frame.rows.len(), shown);
        Act::Stay
    }

    /// The view's frame: the title and the command that opened it, one
    /// plain row per snapshot line, no lines below, no field, and no
    /// footer keys, like the status view. Each row's target comes from
    /// the shared renderer, and a click on one stays.
    pub(crate) fn frame(&self) -> Frame {
        Frame {
            title: "Doctor".to_owned(),
            command: "/doctor".to_owned(),
            rows: self
                .lines
                .iter()
                .map(|line| vec![(line.clone(), None, Ink::Plain)])
                .collect(),
            list: self.list,
            below: Vec::new(),
            field: None,
            footer: String::new(),
        }
    }
}
