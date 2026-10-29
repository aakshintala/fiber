//! One offered item's source, read only through the driver's command.

use std::cell::RefCell;

use contract::events::OfferedItem;
use contract::{ErrorCode, RequestId, SessionId};
use ratatui::text::Line;
use serde_json::json;

use super::{Row, clipped};
use crate::app::session_command;
use crate::keys::Key;

#[derive(Debug)]
enum Shown {
    Loading,
    Text(String),
    Refused(String),
}

#[derive(Debug)]
struct Body {
    width: u16,
    rows: Vec<Row>,
}

#[derive(Debug)]
pub(crate) struct Source {
    pub(super) item: usize,
    file: usize,
    top: usize,
    asked: Option<String>,
    shown: Shown,
    body: RefCell<Option<Body>>,
}

impl Source {
    pub(crate) fn new(item: usize) -> Self {
        Self {
            item,
            file: 0,
            top: 0,
            asked: None,
            shown: Shown::Loading,
            body: RefCell::new(None),
        }
    }

    fn show(&mut self, shown: Shown) {
        self.shown = shown;
        *self.body.get_mut() = None;
    }

    pub(crate) fn ask(
        &mut self,
        id: &str,
        session: &SessionId,
        request: &RequestId,
        path: &str,
    ) -> String {
        self.asked = Some(id.to_owned());
        self.show(Shown::Loading);
        session_command(
            id,
            "offered_file",
            session,
            Some(json!({"request_id": request, "path": path})),
        )
        .to_string()
    }

    pub(crate) fn answered(&mut self, id: &str, text: String) -> bool {
        if self.asked.as_deref() != Some(id) {
            return false;
        }
        self.show(Shown::Text(text));
        true
    }

    pub(crate) fn refused(&mut self, id: &str, code: &ErrorCode, message: &str) -> bool {
        if self.asked.as_deref() != Some(id) || *code == ErrorCode::DuplicateCommand {
            return false;
        }
        self.show(Shown::Refused(message.to_owned()));
        true
    }

    pub(crate) fn step(&mut self, right: bool, count: usize) -> Option<usize> {
        let next = if right {
            self.file.saturating_add(1).min(count.saturating_sub(1))
        } else {
            self.file.saturating_sub(1)
        };
        if next == self.file {
            return None;
        }
        self.file = next;
        self.top = 0;
        self.asked = None;
        self.show(Shown::Loading);
        Some(next)
    }

    pub(crate) fn on_key(&mut self, key: &Key, total: usize, height: usize) {
        let page = height.saturating_sub(1).max(1);
        self.top = match key {
            Key::Up => self.top.saturating_sub(1),
            Key::Down => self.top.saturating_add(1),
            Key::PageUp => self.top.saturating_sub(page),
            Key::PageDown => self.top.saturating_add(page),
            Key::Char(_)
            | Key::Backspace
            | Key::Enter
            | Key::Esc
            | Key::CtrlC
            | Key::CtrlO
            | Key::End
            | Key::AltA
            | Key::Tab
            | Key::BackTab
            | Key::F1
            | Key::CtrlG
            | Key::CtrlR
            | Key::CtrlF
            | Key::AltUp
            | Key::AltDown
            | Key::AltX
            | Key::AltP
            | Key::AltR
            | Key::AltDigit(_)
            | Key::CtrlV
            | Key::CtrlL => self.top,
        }
        .min(total.saturating_sub(height));
    }

    pub(crate) fn rows(&self, item: &OfferedItem, width: u16) -> Vec<Row> {
        let kind = super::kind_label(item.kind);
        let path = item.files.get(self.file).map_or("", String::as_str);
        let header = format!(
            "{kind} {} · {path} · {}/{}",
            item.name,
            self.file.saturating_add(1),
            item.files.len()
        );
        let mut rows = vec![Row::text(Line::raw(header)), Row::text(Line::raw(""))];
        // Paging and height changes reuse the wrapped body. Keep only the
        // current width; a resize must not retain another full file's rows.
        let mut body = self.body.borrow_mut();
        if body.as_ref().is_some_and(|body| body.width != width) {
            *body = None;
        }
        let body = body.get_or_insert_with(|| {
            let text = match &self.shown {
                Shown::Loading => "Loading…",
                Shown::Refused(message) => message,
                Shown::Text(text) if text.is_empty() => "(empty file)",
                Shown::Text(text) => text,
            };
            Body {
                width,
                rows: text
                    .lines()
                    .flat_map(|line| {
                        wrap(line, width)
                            .into_iter()
                            .map(|line| clipped(Line::raw(line), vec![], width))
                    })
                    .collect(),
            }
        });
        rows.extend_from_slice(&body.rows);
        rows
    }

    /// Shifts the source rows three lines for a wheel step, clamped at
    /// either end. Never a file step: ← and → keep their own path.
    pub(crate) fn wheel(&mut self, up: bool, total: usize, height: usize) {
        let end = total.saturating_sub(height);
        self.top = if up {
            self.top.saturating_sub(3)
        } else {
            self.top.saturating_add(3).min(end)
        };
    }

    pub(crate) fn top(&self) -> usize {
        self.top
    }
}

/// Keeps indentation and makes control bytes harmless before wrapping by
/// display cells. Every emitted row consumes a character, even at width zero.
fn wrap(line: &str, width: u16) -> Vec<String> {
    let mut expanded = String::new();
    let mut column = 0usize;
    for ch in line.chars() {
        if ch == '\t' {
            let spaces = 4 - column % 4;
            expanded.push_str(&" ".repeat(spaces));
            column = column.saturating_add(spaces);
        } else {
            let ch = if crate::text::printable(ch) { ch } else { ' ' };
            expanded.push(ch);
            column = column.saturating_add(crate::format::width(&ch.to_string()));
        }
    }
    let mut rows = Vec::new();
    let mut row = String::new();
    let mut cells = 0usize;
    for ch in expanded.chars() {
        let wide = crate::format::width(&ch.to_string());
        if !row.is_empty() && cells.saturating_add(wide) > usize::from(width) {
            rows.push(std::mem::take(&mut row));
            cells = 0;
        }
        row.push(ch);
        cells = cells.saturating_add(wide);
    }
    rows.push(row);
    rows
}

#[cfg(test)]
#[path = "source_tests.rs"]
mod tests;
