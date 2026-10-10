//! The jigs' entry points: draw and hover frames from an events file (`docs/testing.md`, "Jigs").

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use contract::clock::Clock;
use contract::{Envelope, Seq};
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::app::App;
use crate::keys::Key;
use crate::link::Line;
use crate::screen::Screen;
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
    screen
        .draw(&mut app, None)
        .map_err(|error| error.to_string())?;
    let mut bytes = Vec::with_capacity(pointer.len());
    for at in pointer {
        let before = written.0.get();
        screen
            .draw(&mut app, Some(*at))
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

/// What reopening `events` costs, split into stages: parsing each line,
/// folding it into the app, and drawing frames through the loop's screen.
/// The bench prints these beside the terminal's attach time
/// (`docs/performance.md`, "Measuring").
#[derive(Debug)]
pub struct OpenStages {
    /// Time in `serde_json::from_str`, one line at a time.
    pub parse: Duration,
    /// Time in `App::on_line`, one line at a time.
    pub fold: Duration,
    /// The frames drawn: one after every `frame_every` lines, and one at
    /// the end, which the last periodic frame covers when the count lands
    /// on it.
    pub frames: usize,
    /// Time in `Screen::draw` across those frames.
    pub frame_time: Duration,
}

/// Reopens `events`, one envelope per line as one session's stream, at
/// `width` by `height` through the loop's screen: each line is parsed
/// and folded as [`draw`] folds it, and a frame is drawn after every
/// `frame_every` lines and once at the end. `usize::MAX` draws the single
/// final frame. An unreadable line is an error naming its number, as
/// [`draw`] names it. Only the bench calls this; the shipped event loop
/// never does.
pub fn measure_open(
    events: &str,
    width: u16,
    height: u16,
    frame_every: usize,
    clock: Arc<dyn Clock>,
) -> Result<OpenStages, String> {
    let mut app = App::new(PathBuf::new());
    app.set_size(width, height);
    let mut screen = Screen::new(CrosstermBackend::new(Counter::default()), width, height)
        .map_err(|error| error.to_string())?;
    let mut stages = OpenStages {
        parse: Duration::ZERO,
        fold: Duration::ZERO,
        frames: 0,
        frame_time: Duration::ZERO,
    };
    let mut since_frame = 0usize;
    for (at, line) in events.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let started = clock.now();
        let envelope: Envelope = serde_json::from_str(line)
            .map_err(|error| format!("line {}: {error}", at.saturating_add(1)))?;
        stages.parse = stages
            .parse
            .saturating_add(clock.now().saturating_duration_since(started));
        if app.session().is_none() {
            app.attach(envelope.session_id.clone());
        }
        let started = clock.now();
        app.on_line(Line::Session(envelope));
        stages.fold = stages
            .fold
            .saturating_add(clock.now().saturating_duration_since(started));
        since_frame = since_frame.saturating_add(1);
        if since_frame >= frame_every {
            stages.frame_time =
                stages
                    .frame_time
                    .saturating_add(draw_frame(&mut screen, &mut app, &clock)?);
            stages.frames = stages.frames.saturating_add(1);
            since_frame = 0;
        }
    }
    // The final frame, unless the last periodic draw already drew it; an
    // empty log still draws once.
    if stages.frames == 0 || since_frame > 0 {
        stages.frame_time =
            stages
                .frame_time
                .saturating_add(draw_frame(&mut screen, &mut app, &clock)?);
        stages.frames = stages.frames.saturating_add(1);
    }
    Ok(stages)
}

/// One frame through the loop's screen, and how long it took.
fn draw_frame(
    screen: &mut Screen<CrosstermBackend<Counter>>,
    app: &mut App,
    clock: &Arc<dyn Clock>,
) -> Result<Duration, String> {
    let started = clock.now();
    screen.draw(app, None).map_err(|error| error.to_string())?;
    Ok(clock.now().saturating_duration_since(started))
}

/// Opens `events`, one envelope per line as one session's stream, at
/// `width` by `height` through the terminal's own paging, with `history`
/// answered from the events in memory as the hub answers from the log. The
/// lines after the last `turn_completed` are a turn still running: the
/// rest is opened in one pass, then the jig pages to the top, jumps across
/// the session, changes the width, and appends the running turn one line a
/// frame, and reports what it measured. The `paging` jig prints it
/// (`docs/testing.md`, "Jigs").
pub fn measure_paging(
    events: &str,
    width: u16,
    height: u16,
    clock: Arc<dyn Clock>,
) -> Result<String, String> {
    const JUMPS: usize = 20;
    let started = clock.now();
    // The running turn follows the last `turn_completed`, by non-empty line.
    let running = events
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .filter_map(|(at, line)| {
            line.contains(r#""kind":"turn_completed""#)
                .then_some(at.saturating_add(1))
        })
        .last()
        .unwrap_or(0);
    let mut paging = Paging {
        app: App::new(PathBuf::new()),
        log: Vec::new(),
        area: Rect::new(0, 0, width, height),
        clock: Arc::clone(&clock),
        most: 0,
    };
    paging.app.set_size(width, height);
    let (mut turns, mut calls) = (0usize, 0usize);
    let mut tail = Vec::new();
    for (at, line) in events
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
    {
        let envelope: Envelope = serde_json::from_str(line)
            .map_err(|error| format!("line {}: {error}", at.saturating_add(1)))?;
        if let Some(seq) = envelope.seq {
            paging.log.push((seq, line));
        }
        if at >= running {
            tail.push(envelope);
            continue;
        }
        if paging.app.session().is_none() {
            paging.app.attach(envelope.session_id.clone());
        }
        turns = turns.saturating_add(usize::from(envelope.kind == "turn_started"));
        calls = calls.saturating_add(usize::from(envelope.kind == "tool_call_requested"));
        paging.app.on_line(Line::Session(envelope));
        paging.most = paging.most.max(paging.app.pages().resident());
    }
    paging.frame()?;
    let open = clock.now().saturating_duration_since(started);
    let (mut loads, mut slowest_load) = (0usize, Duration::ZERO);
    while paging.app.scroll().0 > 0 {
        paging.app.on_key(Key::PageUp, clock.now());
        let (took, loaded) = paging.frame()?;
        if loaded {
            loads = loads.saturating_add(1);
            slowest_load = slowest_load.max(took);
        }
    }
    let total = paging.app.scroll().1;
    let mut slowest_jump = Duration::ZERO;
    // The furthest row jumped to: the jump targets reach the report, so
    // the spread across the session is pinned, not just its timing.
    let mut jumped = 0usize;
    for at in 0..JUMPS {
        jumped = total.saturating_mul(at) / JUMPS;
        paging.app.jump(jumped);
        slowest_jump = slowest_jump.max(paging.frame()?.0);
    }
    let (search_took, search_matches) = paging.search("shell")?;
    let mut slowest_width = Duration::ZERO;
    for wide in [width.saturating_sub(1), width] {
        paging.app.set_size(wide, height);
        slowest_width = slowest_width.max(paging.frame()?.0);
    }
    paging.app.on_key(Key::End, clock.now());
    paging.frame()?;
    let pages_before = paging.app.pages().index().pages().len();
    paging.most = 0;
    let mut slowest_append = Duration::ZERO;
    let appended = tail.len();
    for envelope in tail.drain(..) {
        paging.app.on_line(Line::Session(envelope));
        slowest_append = slowest_append.max(paging.frame()?.0);
    }
    let ms = |took: Duration| took.as_secs_f64() * 1000.0;
    Ok(format!(
        "lines: {}\nturns: {turns}\ncalls: {calls}\npages: {pages_before}\nrows: {total}\n\
         open pass and first frame: {:.2} ms\n\
         slowest frame that loaded pages: {:.2} ms, of {loads} paging up\n\
         slowest jump frame: {:.2} ms, of {JUMPS} to row {jumped}\n\
         search of the whole log: {:.2} ms, {search_matches} matches\n\
         slowest re-count at a new width: {:.2} ms\n\
         slowest append frame: {:.2} ms, of {appended}; pages while appending: {} to {}, \
         most resident {}\n",
        paging.log.len(),
        ms(open),
        ms(slowest_load),
        ms(slowest_jump),
        ms(search_took),
        ms(slowest_width),
        ms(slowest_append),
        pages_before,
        paging.app.pages().index().pages().len(),
        paging.most,
    ))
}

#[cfg(test)]
#[path = "jigs_tests.rs"]
mod tests;

/// The paging jig's terminal: the app, and the session's durable lines as
/// the hub's log holds them, by `seq`.
struct Paging<'a> {
    app: App,
    log: Vec<(Seq, &'a str)>,
    area: Rect,
    clock: Arc<dyn Clock>,
    /// The most pages resident after any frame.
    most: usize,
}

impl Paging<'_> {
    /// Types `query` into the search bar and answers its `history`
    /// commands from the log until the count stops scanning. Returns how
    /// long the scan took and how many matches it kept (`docs/tui.md`,
    /// "History and paging": search of the whole log).
    fn search(&mut self, query: &str) -> Result<(Duration, usize), String> {
        // The scan fetches dropped pages with `history`, so the jig
        // connects first: the answers come from the log, as the hub's
        // would.
        self.app.on_line(Line::Hub(contract::HubLine {
            kind: "hub_hello".to_owned(),
            ts: 0,
            schema_version: contract::SCHEMA_VERSION,
            payload: serde_json::Map::new(),
        }));
        let now = self.clock.now();
        self.app.on_key(Key::CtrlF, now);
        for ch in query.chars() {
            self.app.on_key(Key::Char(ch), now);
        }
        let generation = u64::try_from(query.chars().count()).unwrap_or(u64::MAX);
        let started = self.clock.now();
        let mut outgoing = self.app.find_due(generation);
        let bound = self
            .app
            .pages()
            .page_count()
            .saturating_mul(2)
            .saturating_add(2);
        for _ in 0..bound {
            if outgoing.is_empty() {
                break;
            }
            let line = outgoing.remove(0);
            let command: serde_json::Value =
                serde_json::from_str(&line).map_err(|error| error.to_string())?;
            let id = command
                .get("id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let args = command.get("args");
            let from = args
                .and_then(|args| args.get("from_seq"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let to = args
                .and_then(|args| args.get("to_seq"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(u64::MAX);
            let mut held: Vec<Envelope> = Vec::new();
            for (_, line) in self
                .log
                .iter()
                .skip_while(|(seq, _)| seq.0 < from)
                .take_while(|(seq, _)| seq.0 <= to)
            {
                held.push(serde_json::from_str(line).map_err(|error| error.to_string())?);
            }
            let answer = contract::Envelope {
                kind: "command_accepted".to_owned(),
                session_id: self
                    .app
                    .session()
                    .cloned()
                    .unwrap_or(contract::SessionId(String::new())),
                ts: 0,
                schema_version: contract::SCHEMA_VERSION,
                turn_id: None,
                action_id: None,
                seq: None,
                payload: serde_json::json!({"command_id": id, "result": {"lines": held}})
                    .as_object()
                    .cloned()
                    .unwrap_or_default(),
            };
            outgoing = self.app.on_line(Line::Session(answer));
        }
        if outgoing.into_iter().next().is_some() {
            return Err("the search never settled".to_owned());
        }
        Ok((
            self.clock.now().saturating_duration_since(started),
            self.app.find_matches(),
        ))
    }

    /// One frame: loads what it needs from the log and draws. Returns how
    /// long it took and whether it loaded a page.
    fn frame(&mut self) -> Result<(Duration, bool), String> {
        let started = self.clock.now();
        let mut loaded = false;
        while let Some(range) = self.app.needs().into_iter().next() {
            let from = self.log.partition_point(|(seq, _)| seq < range.start());
            let mut lines = Vec::new();
            for (_, line) in self
                .log
                .iter()
                .skip(from)
                .take_while(|(seq, _)| range.contains(seq))
            {
                lines.push(serde_json::from_str(line).map_err(|error| error.to_string())?);
            }
            self.app.load(lines);
            if self.app.needs().first() == Some(&range) {
                return Err(format!(
                    "the log holds no lines {}..={}",
                    range.start().0,
                    range.end().0
                ));
            }
            loaded = true;
        }
        let mut buf = Buffer::empty(self.area);
        view::render(&self.app, self.area, &mut buf, None);
        self.most = self.most.max(self.app.pages().resident());
        Ok((self.clock.now().saturating_duration_since(started), loaded))
    }
}
