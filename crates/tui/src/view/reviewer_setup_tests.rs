//! The reviewer setup panel: the frames of the reviewed prototype, the
//! window that follows the focus, and how it clips, scrolls and layers.

use std::path::PathBuf;
use std::sync::Arc;

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use serde_json::{Value, json};

use contract::clock::Clock;
use contract::notes::Topic;

use super::{draw, frame};
use crate::app::{App, Effect, Surface};
use crate::configure::{
    ConfigureError, FileOutcome, FileSaved, NotesFile, NotesSaved, ReviewerNotes,
};
use crate::configure_fake::Fake;
use crate::format::width as cells;
use crate::home::Launch;
use crate::keys::Key;
use crate::link::Line;
use crate::reviewer_setup::Screen;
use crate::theme::Role;
use crate::view::overlay;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";
const GLOBAL_FILE: &str = "~/.fiber/config.json";
const PROJECT_FILE: &str = "~/.fiber/projects/fiber/config.json";

// the lines the reviewed frames hold
const SQUASH: &str = "squash-merging my own pull requests once CI passes";
const SQUASH_EV: &str = "14 escalations answered allow for `gh pr merge … --squash`";
const HOSTS: &str = "corp.example hosts and registries count as inside";
const HOSTS_EV: &str = "answer: only corp.example hosts and registries count as inside";
const OURS: &str = "github.com/acme/* are ours";
const MINE: &str = "my account amogh-dev is mine";
const LABS: &str = "github.com/acme-labs/* are ours";
const LABS_EV: &str = "its git remotes: upstream github.com/acme-labs/fiber";
const OLD: &str = "github.com/acme-old/* are ours";
const ANSWER: &str = "github.com/acme, and my account amogh-dev";
const SYNC_ERROR: &str = "could not sync the directory: the new file is in place";

// -------------------------------------------------------------- fixtures

fn now() -> std::time::Instant {
    fakes::clock::FakeClock::new().now()
}

fn envelope(kind: &str, payload: Value) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

fn notes(global: &str, project: &str) -> ReviewerNotes {
    ReviewerNotes {
        global: NotesFile {
            file: PathBuf::from(GLOBAL_FILE),
            text: global.to_owned(),
            version: "global-v1".to_owned(),
        },
        project: NotesFile {
            file: PathBuf::from(PROJECT_FILE),
            text: project.to_owned(),
            version: "project-v1".to_owned(),
        },
    }
}

/// A session app at `width` by `height` with the setup open on topic 1
/// over `notes`.
struct Fixture {
    app: App,
    fake: Arc<Fake>,
}

impl Fixture {
    /// The session on screen with the seam installed and no setup open.
    fn attached(global: &str, project: &str, width: u16, height: u16) -> Self {
        let mut app = App::new(PathBuf::from("/w"));
        app.set_home(Launch {
            location: crate::LaunchLocation::Local {
                workspace: PathBuf::from("/w"),
                project: "-w".to_owned(),
                git: false,
            },
            hover: true,
            version: "0.0.1".to_owned(),
            model: Some("test/model".to_owned()),
            ..Default::default()
        });
        app.set_size(width, height);
        app.on_line(Line::Hub(contract::HubLine {
            kind: "hub_hello".to_owned(),
            ts: 0,
            schema_version: contract::SCHEMA_VERSION,
            payload: serde_json::Map::new(),
        }));
        app.on_line(envelope(
            "session_status",
            json!({
                "name": "work", "workspace": "/w", "project": "-w",
                "state": "idle", "since": 0, "started": 0, "calls": 0,
                "delegates": 0, "jobs": 0, "clients": 0, "model": "test/model",
                "spend": {"tokens": {"input": 1, "output": 2, "cache_read": 0,
                    "cache_write": {}}, "cost": 0.0, "subscription_cost": 0.0},
            }),
        ));
        app.attach(contract::SessionId(SESSION.to_owned()));
        let fake = Arc::new(Fake::new(Vec::new()));
        *fake.notes.lock().expect("notes lock") = Ok(notes(global, project));
        app.set_configure(Some(fake.clone() as Arc<dyn crate::Configure>));
        Self { app, fake }
    }

    /// The setup open on topic 1 over `notes`.
    fn new(global: &str, project: &str, width: u16, height: u16) -> Self {
        let mut fx = Self::attached(global, project, width, height);
        for ch in "/reviewer-setup".chars() {
            fx.app.on_key(Key::Char(ch), now());
        }
        assert_eq!(fx.app.on_key(Key::Enter, now()), Effect::None);
        assert!(fx.app.reviewer_setup_open(), "the setup did not open");
        fx
    }

    fn empty() -> Self {
        Self::new("", "", 120, 40)
    }

    fn press(&mut self, key: Key) -> Effect {
        self.app.on_key(key, now())
    }

    fn type_text(&mut self, text: &str) {
        for ch in text.chars() {
            self.press(Key::Char(ch));
        }
    }

    /// Enter on the question, then the hub's answer.
    fn ask(&mut self, evidence: Value, proposed: Value) {
        let Effect::Send(lines) = self.press(Key::Enter) else {
            panic!("Enter on the question sent nothing");
        };
        let sent: Value = serde_json::from_str(&lines[0]).expect("a json line");
        assert_eq!(sent["command"], "propose_notes");
        let answer = envelope(
            "command_accepted",
            json!({"command_id": sent["id"],
                "result": {"evidence": evidence, "proposed": proposed}}),
        );
        assert!(self.app.on_line(answer).is_empty());
    }

    /// Leaves a topic with no lines for it.
    fn skip(&mut self) {
        self.ask(json!([]), json!([]));
        self.press(Key::Enter);
    }

    /// Skips topics until the question on show is `topic`'s.
    fn skip_to(&mut self, topic: Topic) {
        while self.setup().topic() != topic {
            self.skip();
        }
    }

    /// The session starts working, so quitting asks first.
    fn work(&mut self) {
        self.app.on_line(envelope(
            "session_status",
            json!({
                "name": "work", "workspace": "/w", "project": "-w",
                "state": "streaming", "since": 0, "started": 0, "calls": 0,
                "delegates": 0, "jobs": 0, "clients": 0, "model": "test/model",
                "spend": {"tokens": {"input": 1, "output": 2, "cache_read": 0,
                    "cache_write": {}}, "cost": 0.0, "subscription_cost": 0.0},
            }),
        ));
    }

    fn setup(&self) -> &crate::reviewer_setup::Setup {
        self.app.reviewer_setup().expect("a setup is open")
    }

    /// The screen drawn whole.
    fn screen(&mut self) -> String {
        let (width, height) = self.app.screen_size();
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        crate::view::render(&self.app, area, &mut buf, None);
        crate::view::text(&buf)
    }
}

fn session_evidence() -> Value {
    json!([{"kind": "session", "prefix": "gh pr merge … --squash", "count": 14,
        "here": 14, "text": SQUASH_EV}])
}

fn cite_first(text: &str) -> Value {
    json!([{"text": text, "cites": [0]}])
}

/// Topic 4 with the session's squash proposal on the lines screen.
fn first() -> Fixture {
    let mut fx = Fixture::empty();
    fx.skip_to(Topic::RoutineActions);
    fx.ask(session_evidence(), cite_first(SQUASH));
    assert_eq!(fx.setup().screen(), Screen::Lines);
    fx
}

fn outside() -> Fixture {
    let mut fx = Fixture::empty();
    fx.skip_to(Topic::InternalHosts);
    fx.type_text("only corp.example hosts and registries count as inside");
    fx.ask(
        json!([{"kind": "answer", "text": HOSTS_EV}]),
        cite_first(HOSTS),
    );
    fx
}

fn rerun() -> Fixture {
    let mut fx = Fixture::new(
        &format!("### Source control\n- {MINE}\n"),
        &format!("### Source control\n- {OURS}\n"),
        120,
        40,
    );
    fx.ask(
        json!([{"kind": "files", "text": LABS_EV}]),
        cite_first(LABS),
    );
    fx
}

fn struck() -> Fixture {
    let mut fx = first();
    fx.press(Key::Char('x'));
    // The next topic's question, then back to this topic's lines.
    assert_eq!(fx.setup().screen(), Screen::Question);
    fx.press(Key::Esc);
    assert_eq!(fx.setup().screen(), Screen::Lines);
    fx
}

fn flipped() -> Fixture {
    let mut fx = first();
    fx.press(Key::Tab);
    fx
}

/// The diff of the reviewed frame: the old line struck, the labs line and
/// the squash line added to the project notes.
fn diff() -> Fixture {
    let mut fx = Fixture::new(
        "",
        &format!("### Source control\n- {OURS}\n- {OLD}\n"),
        120,
        40,
    );
    fx.ask(
        json!([{"kind": "files", "text": LABS_EV}]),
        cite_first(LABS),
    );
    // Strike the old earlier line, then confirm the proposal.
    fx.press(Key::Up);
    fx.press(Key::Char('x'));
    assert_eq!(fx.setup().topic(), Topic::SourceControl);
    fx.press(Key::Enter);
    fx.skip_to(Topic::RoutineActions);
    fx.ask(session_evidence(), cite_first(SQUASH));
    fx.press(Key::Enter);
    fx.skip_to_diff();
    fx
}

impl Fixture {
    fn skip_to_diff(&mut self) {
        while self.setup().screen() != Screen::Diff {
            self.skip();
        }
    }
}

fn question() -> Fixture {
    let mut fx = Fixture::empty();
    fx.type_text(ANSWER);
    fx
}

fn waiting() -> Fixture {
    let mut fx = Fixture::empty();
    assert!(matches!(fx.press(Key::Enter), Effect::Send(_)));
    assert_eq!(fx.setup().screen(), Screen::Waiting);
    fx
}

/// The notice a reviewer's deny opens over empty notes.
fn notice() -> Fixture {
    let mut fx = Fixture::attached("", "", 120, 40);
    fx.app.on_line(envelope(
        "permission_resolved",
        json!({"request_id": "r_9", "decision": "deny", "decided_by": "reviewer"}),
    ));
    assert_eq!(fx.setup().screen(), Screen::Notice);
    fx
}

fn saved() -> Fixture {
    let mut fx = Fixture::empty();
    *fx.fake.notes_saved.lock().expect("saved lock") = Ok(NotesSaved::Written(vec![
        FileSaved {
            file: PathBuf::from(GLOBAL_FILE),
            outcome: FileOutcome::SavedUnsynced(ConfigureError {
                code: contract::ErrorCode::IoFailed,
                message: SYNC_ERROR.to_owned(),
            }),
        },
        FileSaved {
            file: PathBuf::from(PROJECT_FILE),
            outcome: FileOutcome::Saved,
        },
    ]));
    fx.ask(
        json!([{"kind": "files", "text": LABS_EV}]),
        cite_first(LABS),
    );
    fx.press(Key::Enter);
    fx.skip_to_diff();
    fx.press(Key::Enter);
    assert_eq!(fx.setup().screen(), Screen::Saved);
    fx
}

// ------------------------------------------------------------- the checks

/// The overlay the setup builds at `columns` wide and `height` high.
fn overlay_of(fx: &Fixture, columns: u16, height: u16) -> overlay::Overlay {
    frame(fx.setup(), overlay::question_inner(columns), height)
}

fn row_width(row: &overlay::Row) -> usize {
    row.spans.iter().map(|span| cells(&span.content)).sum()
}

/// No row is wider than the panel's inner width at 80 columns, and the
/// legend fits whole.
#[track_caller]
fn fits_80_columns(fx: &Fixture) {
    let inner = overlay::question_inner(80);
    let framed = overlay_of(fx, 80, 40);
    for row in &framed.body {
        assert!(row_width(row) <= inner, "{} wide", row_width(row));
    }
    let footer = framed.footer.as_ref().expect("a legend");
    assert!(row_width(footer) <= inner, "the legend is cut");
}

/// The rows the frame bars.
fn barred(fx: &Fixture) -> usize {
    overlay_of(fx, 120, 40)
        .body
        .iter()
        .filter(|row| row.barred)
        .count()
}

#[track_caller]
fn checked(mut fx: Fixture, name: &str, bars: usize) -> String {
    fits_80_columns(&fx);
    assert_eq!(barred(&fx), bars, "{name}: barred rows");
    let screen = fx.screen();
    // The buffer agrees: the bar paints its row on the accent.
    let barred_rows = barred_in_buffer(&mut fx);
    assert_eq!(barred_rows, bars, "{name}: barred rows drawn");
    screen
}

/// Screen rows whose first panel cell carries the bar's background.
fn barred_in_buffer(fx: &mut Fixture) -> usize {
    let (width, height) = fx.app.screen_size();
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    crate::view::render(&fx.app, area, &mut buf, None);
    let slab = fx
        .app
        .frames()
        .slab_of(Surface::ReviewerSetup)
        .expect("the setup drew");
    (slab.top()..slab.bottom())
        .filter(|y| buf[(slab.x + 3, *y)].bg == Role::Accent.color())
        .count()
}

#[test]
fn first_frame() {
    insta::assert_snapshot!("first", checked(first(), "first", 1));
}

#[test]
fn outside_frame() {
    insta::assert_snapshot!("outside", checked(outside(), "outside", 1));
}

#[test]
fn rerun_frame() {
    insta::assert_snapshot!("rerun", checked(rerun(), "rerun", 1));
}

#[test]
fn struck_frame() {
    insta::assert_snapshot!("struck", checked(struck(), "struck", 1));
}

#[test]
fn flipped_frame() {
    insta::assert_snapshot!("flipped", checked(flipped(), "flipped", 1));
}

#[test]
fn diff_frame() {
    insta::assert_snapshot!("diff", checked(diff(), "diff", 0));
}

#[test]
fn question_frame() {
    insta::assert_snapshot!("question", checked(question(), "question", 0));
}

#[test]
fn waiting_frame() {
    insta::assert_snapshot!("waiting", checked(waiting(), "waiting", 0));
}

#[test]
fn saved_frame() {
    insta::assert_snapshot!("saved", checked(saved(), "saved", 0));
}

#[test]
fn notice_frame() {
    insta::assert_snapshot!("notice", checked(notice(), "notice", 0));
}

#[test]
fn only_the_rerun_frame_labels_earlier_lines_and_new_proposals() {
    for (name, fx) in [
        ("first", first()),
        ("outside", outside()),
        ("flipped", flipped()),
        ("struck", struck()),
    ] {
        let mut fx = fx;
        let screen = fx.screen();
        assert!(!screen.contains("Earlier lines"), "{name}");
        assert!(!screen.contains("New proposals"), "{name}");
        assert!(screen.contains("Topic "), "{name} has no topic line");
    }
    let mut fx = rerun();
    let screen = fx.screen();
    assert!(screen.contains("Earlier lines"));
    assert!(screen.contains("New proposals"));
    assert!(!screen.contains("Which accounts"), "the question stays out");
}

#[test]
fn the_struck_frame_has_no_question_and_no_evidence() {
    let mut fx = struck();
    let screen = fx.screen();
    assert!(screen.contains("struck — kept out of the notes"));
    assert!(!screen.contains("What routine actions"));
    assert!(!screen.contains("14 escalations"));
}

#[test]
fn the_flipped_frame_says_what_the_line_was() {
    let mut fx = flipped();
    let screen = fx.screen();
    assert!(screen.contains("[global]"));
    assert!(screen.contains("was per-project · Tab flipped it"));
    // Flipped back, the line is the rule's again and the note goes.
    fx.press(Key::Tab);
    assert!(!fx.screen().contains("Tab flipped it"));
}

// ------------------------------------------------------- the lines window

fn thirty() -> Fixture {
    let mut fx = Fixture::new("", "", 120, 24);
    let proposed: Vec<Value> = (0..30)
        .map(|at| json!({"text": format!("line number {at:02} of the thirty"), "cites": [0]}))
        .collect();
    fx.ask(
        json!([{"kind": "files", "text": "its git remotes: origin github.com/acme/fiber"}]),
        Value::Array(proposed),
    );
    fx
}

#[test]
fn a_thirty_line_screen_keeps_the_focused_row_in_view_with_one_barred_row() {
    let mut fx = thirty();
    let capacity = overlay::question_capacity(24);
    for at in 0..30 {
        let screen = fx.screen();
        assert!(
            screen.contains(&format!("line number {at:02} of the thirty")),
            "line {at} is out of view:\n{screen}"
        );
        let framed = overlay_of(&fx, 120, 24);
        assert_eq!(
            framed.body.iter().filter(|row| row.barred).count(),
            1,
            "line {at}"
        );
        assert!(framed.body.len() <= capacity, "line {at}");
        assert_eq!(barred_in_buffer(&mut fx), 1, "line {at}");
        fx.press(Key::Down);
    }
    // And back up to the first.
    for at in (0..29).rev() {
        fx.press(Key::Up);
        let screen = fx.screen();
        assert!(
            screen.contains(&format!("line number {at:02} of the thirty")),
            "line {at} is out of view going up"
        );
    }
}

// ----------------------------------------------------- the height budget

fn long_proposal() -> Fixture {
    let mut fx = Fixture::empty();
    let text = "word ".repeat(80);
    fx.ask(
        json!([{"kind": "files", "text": "its git remotes: origin github.com/acme/fiber"}]),
        cite_first(text.trim()),
    );
    fx
}

#[test]
fn a_long_focused_line_stays_within_the_capacity_and_ends_in_an_ellipsis() {
    let fx = long_proposal();
    for height in [9u16, 12] {
        let framed = frame(fx.setup(), overlay::question_inner(120), height);
        let capacity = overlay::question_capacity(height);
        assert!(
            framed.body.len() <= capacity,
            "height {height}: {} rows",
            framed.body.len()
        );
        let bars = framed.body.iter().filter(|row| row.barred).count();
        assert!(bars >= 1, "height {height}: a barred row");
        if height == 9 {
            assert_eq!(bars, 1, "one row fits at height 9");
        }
        let last = framed.body.last().expect("a body row");
        let text: String = last
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(text.ends_with('…'), "height {height}: {text:?}");
        assert!(row_width(last) <= overlay::question_inner(120));
    }
}

#[test]
fn a_line_that_fits_is_not_clipped() {
    let fx = first();
    let framed = frame(fx.setup(), overlay::question_inner(120), 40);
    for row in &framed.body {
        let text: String = row.spans.iter().map(|span| span.content.as_ref()).collect();
        assert!(!text.ends_with('…'), "{text:?}");
    }
}

#[test]
fn the_draw_stays_inside_the_area_at_every_small_height_and_width() {
    let fx = long_proposal();
    for height in 1..=12u16 {
        for width in [1u16, 7, 20, 40, 80, 120] {
            let area = Rect::new(0, 0, width, height);
            let mut buf = Buffer::empty(area);
            let mut targets = Vec::new();
            let slab = draw(&fx.app, area, &mut buf, &mut targets);
            assert!(
                slab.is_empty() || area.contains(Position::new(slab.x, slab.y)),
                "{width}x{height}: {slab:?}"
            );
            assert!(slab.right() <= area.right() && slab.bottom() <= area.bottom());
        }
    }
}

// ------------------------------------------------------------ U+200B

#[test]
fn a_zero_width_space_is_neither_drawn_nor_saved() {
    let mut fx = Fixture::empty();
    fx.ask(
        json!([{"kind": "files", "text": "its git remotes: origin github.com/acme/fiber"}]),
        cite_first("ab\u{200B}cd are ours"),
    );
    let screen = fx.screen();
    assert!(screen.contains("abcd are ours"));
    assert!(!screen.contains('\u{200B}'));
    fx.press(Key::Enter);
    fx.skip_to_diff();
    fx.press(Key::Enter);
    let saves = fx.fake.notes_saves();
    let write = saves[0].2.clone().expect("a project write");
    assert_eq!(write.text, "### Source control\n- abcd are ours\n");
}

// ------------------------------------------------------------ the diff

/// A diff of thirty added lines, longer than the panel.
fn long_diff() -> Fixture {
    let mut fx = Fixture::new("", "", 120, 20);
    let proposed: Vec<Value> = (0..30)
        .map(|at| json!({"text": format!("added line {at:02}"), "cites": [0]}))
        .collect();
    fx.ask(
        json!([{"kind": "files", "text": LABS_EV}]),
        Value::Array(proposed),
    );
    for _ in 0..30 {
        fx.press(Key::Enter);
    }
    fx.skip_to_diff();
    fx
}

#[test]
fn a_long_diff_scrolls_by_line_and_page_and_clamps_at_both_edges() {
    let mut fx = long_diff();
    let total = frame(fx.setup(), overlay::question_inner(120), 400)
        .body
        .len();
    // The draw notes the viewport the keys clamp against.
    let first = fx.screen();
    let slab = fx
        .app
        .frames()
        .slab_of(Surface::ReviewerSetup)
        .expect("the setup drew");
    let capacity = usize::from(slab.height) - 8;
    let page = capacity - 1;
    assert!(total > capacity + 2 * page, "{total} rows, {capacity} fit");
    assert!(first.contains("Global notes"));
    assert!(!first.contains("from the next session."));
    fx.press(Key::Up);
    assert_eq!(fx.setup().scroll(), 0, "no scrolling above the top");
    fx.press(Key::Down);
    assert_eq!(fx.setup().scroll(), 1);
    fx.press(Key::PageDown);
    assert_eq!(fx.setup().scroll(), 1 + page);
    for _ in 0..200 {
        fx.press(Key::Down);
    }
    let farthest = total - capacity;
    assert_eq!(fx.setup().scroll(), farthest, "Down stops at the last row");
    assert!(fx.screen().contains("from the next session."));
    fx.press(Key::PageDown);
    assert_eq!(fx.setup().scroll(), farthest, "PgDn stops at the last row");
    fx.press(Key::PageUp);
    assert_eq!(fx.setup().scroll(), farthest - page);
    for _ in 0..200 {
        fx.press(Key::PageUp);
    }
    assert_eq!(fx.setup().scroll(), 0, "PgUp stops at the first row");
    assert!(fx.screen().contains("Global notes"));
}

// ------------------------------------------------------------- layering

#[test]
fn the_slab_is_noted_when_open_and_absent_when_closed() {
    let fx = first();
    let (width, height) = fx.app.screen_size();
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    crate::view::render(&fx.app, area, &mut buf, None);
    let slab = fx
        .app
        .frames()
        .slab_of(Surface::ReviewerSetup)
        .expect("the setup drew");
    assert!(!slab.is_empty());
    // The noted slab is the one drawn: its top edge is the ▄ run and its
    // bottom edge the ▀ run.
    for x in slab.left()..slab.right() {
        assert_eq!(buf[(x, slab.top())].symbol(), "▄", "top edge at {x}");
        assert_eq!(
            buf[(x, slab.bottom() - 1)].symbol(),
            "▀",
            "bottom edge at {x}"
        );
    }
    // Close it from topic 1.
    let mut closed = Fixture::empty();
    closed.press(Key::Esc);
    assert!(closed.app.reviewer_setup().is_none());
    let mut buf = Buffer::empty(area);
    crate::view::render(&closed.app, area, &mut buf, None);
    assert_eq!(closed.app.frames().slab_of(Surface::ReviewerSetup), None);
}

#[test]
fn the_setup_drops_the_conversations_click_targets() {
    let mut fx = Fixture::empty();
    // Close it, seed a conversation, draw, and open it again over it.
    fx.press(Key::Esc);
    for at in 0..3 {
        fx.app.on_line(envelope(
            "turn_started",
            json!({"input": [{"type": "message", "source": "driver",
                "content": [{"type": "text", "text": format!("turn {at}")}]}]}),
        ));
        fx.app
            .on_line(envelope("turn_completed", json!({"outcome": "completed"})));
    }
    let (width, height) = fx.app.screen_size();
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let before = crate::view::render(&fx.app, area, &mut buf, None);
    let conversation = |target: &crate::mouse::Target| {
        matches!(
            target.id,
            crate::mouse::TargetId::Line(_)
                | crate::mouse::TargetId::Link { .. }
                | crate::mouse::TargetId::FindResult(_)
                | crate::mouse::TargetId::NewBelow
                | crate::mouse::TargetId::Turn(_)
                | crate::mouse::TargetId::Offer(_)
        )
    };
    assert!(
        before.iter().any(conversation),
        "the conversation has targets to drop"
    );
    for ch in "/reviewer-setup".chars() {
        fx.press(Key::Char(ch));
    }
    fx.press(Key::Enter);
    assert!(fx.app.reviewer_setup_open());
    let mut buf = Buffer::empty(area);
    let after = crate::view::render(&fx.app, area, &mut buf, None);
    assert!(!after.iter().any(conversation));
    assert!(
        after
            .iter()
            .any(|target| target.id == crate::mouse::TargetId::CloseOverlay)
    );
}

/// The cells of `buf` inside `slab`, as rows.
fn cells_in(buf: &Buffer, slab: Rect) -> Vec<String> {
    (slab.top()..slab.bottom())
        .map(|y| {
            (slab.left()..slab.right())
                .map(|x| buf[(x, y)].symbol().to_owned())
                .collect::<String>()
        })
        .collect()
}

#[test]
fn the_quit_question_draws_over_the_setup() {
    let mut fx = first();
    fx.work();
    let (width, height) = fx.app.screen_size();
    let area = Rect::new(0, 0, width, height);
    fx.app.quit();
    assert_eq!(fx.app.top_surface(), Surface::Quit);
    let mut buf = Buffer::empty(area);
    crate::view::render(&fx.app, area, &mut buf, None);
    let quit = fx
        .app
        .frames()
        .slab_of(Surface::Quit)
        .expect("the quit question drew");
    // A twin with no setup draws the same quit question: nothing of the
    // setup shows through inside its slab.
    let mut twin = Fixture::empty();
    twin.press(Key::Esc);
    twin.work();
    twin.app.quit();
    let mut twin_buf = Buffer::empty(area);
    crate::view::render(&twin.app, area, &mut twin_buf, None);
    assert_eq!(cells_in(&buf, quit), cells_in(&twin_buf, quit));
    // The setup is still drawn under it: the two screens differ outside the
    // quit question's slab.
    assert_ne!(
        crate::view::text(&buf),
        crate::view::text(&twin_buf),
        "the setup is under the quit question"
    );
}

#[test]
fn the_import_report_draws_over_the_setup() {
    let mut fx = first();
    let (width, height) = fx.app.screen_size();
    let area = Rect::new(0, 0, width, height);
    let names: contract::events::ImportedNames = serde_json::from_value(json!({
        "mcp_servers": ["server0"], "skills": ["skill0"],
        "commands": ["command0"], "instructions": ["AGENTS"],
    }))
    .expect("imported names");
    fx.app.open_import_report(None, names, Vec::new());
    let mut buf = Buffer::empty(area);
    crate::view::render(&fx.app, area, &mut buf, None);
    let report = fx
        .app
        .frames()
        .slab_of(Surface::Report)
        .expect("the report drew");
    let mut twin = Fixture::empty();
    twin.press(Key::Esc);
    let names: contract::events::ImportedNames = serde_json::from_value(json!({
        "mcp_servers": ["server0"], "skills": ["skill0"],
        "commands": ["command0"], "instructions": ["AGENTS"],
    }))
    .expect("imported names");
    twin.app.open_import_report(None, names, Vec::new());
    let mut twin_buf = Buffer::empty(area);
    crate::view::render(&twin.app, area, &mut twin_buf, None);
    assert_eq!(cells_in(&buf, report), cells_in(&twin_buf, report));
}
