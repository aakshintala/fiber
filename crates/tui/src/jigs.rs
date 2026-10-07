//! The jigs' entry points: draw and hover frames from an events file (`docs/testing.md`, "Jigs").

use std::io;
use std::path::PathBuf;

use contract::Envelope;
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::Screen;
use crate::app::App;
use crate::link::Line;
use crate::view;

/// Folds `events`, one envelope per line as one session's stream, and
/// draws them at `width` by `height`. Returns the screen as text, each row
/// trimmed of trailing spaces. An unreadable line is an error naming its
/// number. The `draw` jig prints it (`docs/testing.md`, "Jigs").
pub fn draw(events: &str, width: u16, height: u16) -> Result<String, String> {
    let app = fold(events, width, height)?;
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    view::render(&app, area, &mut buf, None);
    Ok(view::text(&buf))
}

/// Folds `events` as [`draw`] does and puts the request the panel shows
/// aside, so it waits on the badge, a click target. Then draws them at
/// `width` by `height` through the loop's screen, and moves the pointer to
/// each of `pointer` in turn, drawing after each as the loop does for a
/// motion report. Returns the bytes each report wrote. The `hover` jig
/// times it (`docs/tui.md`, "Mouse and hover").
pub fn hover_frames(
    events: &str,
    width: u16,
    height: u16,
    pointer: &[(u16, u16)],
) -> Result<Vec<usize>, String> {
    let mut app = fold(events, width, height)?;
    app.put_aside();
    let written = Counter::default();
    let mut screen = Screen::new(CrosstermBackend::new(written.clone()), width, height)
        .map_err(|error| error.to_string())?;
    screen.draw(&app, None).map_err(|error| error.to_string())?;
    let mut bytes = Vec::with_capacity(pointer.len());
    for at in pointer {
        let before = written.0.get();
        screen
            .draw(&app, Some(*at))
            .map_err(|error| error.to_string())?;
        bytes.push(written.0.get().saturating_sub(before));
    }
    Ok(bytes)
}

/// Counts the bytes written through it.
#[derive(Clone, Default)]
struct Counter(std::rc::Rc<std::cell::Cell<usize>>);

impl io::Write for Counter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.set(self.0.get().saturating_add(bytes.len()));
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// An app at `width` by `height` with `events` folded, one envelope per
/// line as one session's stream. An unreadable line is an error naming
/// its number.
fn fold(events: &str, width: u16, height: u16) -> Result<App, String> {
    let mut app = App::new(PathBuf::new());
    app.set_size(width, height);
    for (at, line) in events.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let envelope: Envelope = serde_json::from_str(line)
            .map_err(|error| format!("line {}: {error}", at.saturating_add(1)))?;
        if app.session().is_none() {
            app.attach(envelope.session_id.clone());
        }
        app.on_line(Line::Session(envelope));
    }
    Ok(app)
}
