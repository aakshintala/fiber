//! `/reviewer-setup` on the app (`docs/permissions.md`, "Setting up the
//! notes"): the command, its binding to one session, the requests and the
//! save it sends, and what interrupts it.

use std::path::PathBuf;
use std::sync::Arc;

use ratatui::layout::Position;
use serde_json::{Value, json};

use contract::SessionId;
use contract::notes::Topic;

use crate::app::NoticeLine;
use crate::app::commands::commands_testkit::{
    enter, hub_line, painted, sent, session_line, turn_starts, type_text,
};
use crate::app::top::tests::{
    SESSION, drawn, home_app, now, open, outside_corner, press_at, session_app,
};
use crate::app::{App, Effect, Link, Surface};
use crate::configure::{
    ConfigureError, FileOutcome, FileSaved, NotesFile, NotesSaved, NotesWrite, ReviewerNotes,
};
use crate::configure_fake::Fake;
use crate::keys::{Button, Edit, Key, Mouse, MouseKind};
use crate::link::Line;
use crate::mouse::{Pointer, Target, TargetId};
use crate::reviewer_setup::{LineState, Screen};

const GLOBAL_FILE: &str = "/home/fiber/config.json";
const PROJECT_FILE: &str = "/home/fiber/projects/-w/config.json";
const OTHER: &str = "s_cccccccccccccccc";

// -------------------------------------------------------------- fixtures

/// The session app with a fake seam answering empty notes.
fn app() -> (App, Arc<Fake>) {
    let mut app = session_app();
    let fake = Arc::new(Fake::new(Vec::new()));
    set_notes(&fake, notes("", ""));
    app.set_configure(Some(fake.clone() as Arc<dyn crate::Configure>));
    (app, fake)
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

fn set_notes(fake: &Fake, notes: ReviewerNotes) {
    *fake.notes.lock().expect("notes lock") = Ok(notes);
}

fn set_saved(fake: &Fake, saved: Result<NotesSaved, ConfigureError>) {
    *fake.notes_saved.lock().expect("saved lock") = saved;
}

fn press(app: &mut App, key: Key) -> Effect {
    app.on_key(key, now())
}

/// Types `/reviewer-setup` and Enter; the setup opens on topic 1.
fn start(app: &mut App) {
    assert_eq!(enter(app, "/reviewer-setup"), Effect::None);
    assert!(app.reviewer_setup_open(), "the setup did not open");
}

fn screen(app: &App) -> Screen {
    app.reviewer_setup().expect("a setup is open").screen()
}

fn topic(app: &App) -> Topic {
    app.reviewer_setup().expect("a setup is open").topic()
}

fn answer(app: &App) -> String {
    app.reviewer_setup()
        .expect("a setup is open")
        .answer()
        .to_owned()
}

fn error(app: &App) -> Option<String> {
    app.reviewer_setup()
        .expect("a setup is open")
        .error()
        .map(str::to_owned)
}

fn notices(app: &App) -> Vec<(String, String)> {
    app.notices()
        .into_iter()
        .filter_map(|line| match line {
            NoticeLine::Notice { code, text, .. } => Some((code, text)),
            NoticeLine::More(_) => None,
        })
        .collect()
}

fn with_session(session: &str, mut line: Line) -> Line {
    if let Line::Session(envelope) = &mut line {
        envelope.session_id = SessionId(session.to_owned());
    }
    line
}

fn with_action(mut line: Line) -> Line {
    if let Line::Session(envelope) = &mut line {
        envelope.action_id = Some(contract::ActionId("a_1".to_owned()));
    }
    line
}

fn accepted(id: &str, result: Value) -> Line {
    session_line(
        "command_accepted",
        json!({"command_id": id, "result": result}),
    )
}

fn rejected(id: &str, code: &str, message: &str) -> Line {
    session_line(
        "command_rejected",
        json!({"command_id": id, "code": code, "message": message}),
    )
}

fn hub(kind: &str, session: &str) -> Line {
    hub_line(kind, json!({"session_id": session}))
}

fn status(session: &str, workspace: &str, project: &str, state: &str) -> Line {
    with_session(
        session,
        session_line(
            "session_status",
            json!({
                "name": "work", "workspace": workspace, "project": project,
                "state": state, "since": 0, "started": 0, "calls": 0,
                "delegates": 0, "jobs": 0, "clients": 0, "model": "test/model",
                "spend": {"tokens": {"input": 1, "output": 2, "cache_read": 0,
                    "cache_write": {}}, "cost": 0.0, "subscription_cost": 0.0},
            }),
        ),
    )
}

fn proposals(evidence: Value, proposed: Value) -> Value {
    json!({"evidence": evidence, "proposed": proposed})
}

fn one_line() -> Value {
    proposals(
        json!([{"kind": "files", "text": "its git remotes: origin github.com/acme/fiber"}]),
        json!([{"text": "github.com/acme/* are ours", "cites": [0]}]),
    )
}

fn nothing() -> Value {
    proposals(json!([]), json!([]))
}

/// Enter on the question: the one `propose_notes` line it sends.
fn propose(app: &mut App) -> Value {
    let lines = sent(press(app, Key::Enter));
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["command"], "propose_notes");
    lines[0].clone()
}

/// The id of a sent line.
fn id_of(line: &Value) -> String {
    line["id"].as_str().expect("an id").to_owned()
}

/// Proposes the topic on show and answers with `result`; the id it used.
fn ask(app: &mut App, result: Value) -> String {
    let id = id_of(&propose(app));
    assert!(app.on_line(accepted(&id, result)).is_empty());
    id
}

/// Walks every topic: topic 1 gives one line the person confirms, the other
/// four give nothing. Stops on the diff.
fn to_the_diff_with_a_line(app: &mut App) {
    ask(app, one_line());
    assert_eq!(screen(app), Screen::Lines);
    press(app, Key::Enter);
    while screen(app) != Screen::Diff {
        assert_eq!(screen(app), Screen::Question);
        ask(app, nothing());
        press(app, Key::Enter);
    }
}

fn saved_both() -> Result<NotesSaved, ConfigureError> {
    Ok(NotesSaved::Written(vec![
        FileSaved {
            file: PathBuf::from(GLOBAL_FILE),
            outcome: FileOutcome::Saved,
        },
        FileSaved {
            file: PathBuf::from(PROJECT_FILE),
            outcome: FileOutcome::Saved,
        },
    ]))
}

fn failure(message: &str) -> ConfigureError {
    ConfigureError {
        code: contract::ErrorCode::IoFailed,
        message: message.to_owned(),
    }
}

/// The app drawn at its size: the text on screen.
fn on_screen(app: &mut App) -> String {
    let (width, height) = app.screen_size();
    painted(app, width, height).0
}

// ------------------------------------------------------------ the command

#[test]
fn the_command_clears_the_draft_reads_the_notes_and_shows_topic_ones_question() {
    let (mut app, fake) = app();
    set_notes(&fake, notes("", "### Source control\n- earlier\n"));
    type_text(&mut app, "/reviewer-setup");
    assert_eq!(press(&mut app, Key::Enter), Effect::None);
    assert!(app.reviewer_setup_open());
    assert_eq!(app.top_surface(), Surface::ReviewerSetup);
    assert_eq!(screen(&app), Screen::Question);
    assert_eq!(topic(&app), Topic::SourceControl);
    // The draft was cleared: nothing comes back when the setup closes.
    assert_eq!(press(&mut app, Key::Esc), Effect::None);
    assert!(!app.reviewer_setup_open());
    assert!(!on_screen(&mut app).contains("/reviewer-setup"));
}

#[test]
fn with_no_session_a_notice_says_so_and_nothing_opens() {
    let mut app = crate::app::commands::commands_testkit::connected();
    app.set_configure(Some(
        Arc::new(Fake::new(Vec::new())) as Arc<dyn crate::Configure>
    ));
    assert_eq!(enter(&mut app, "/reviewer-setup"), Effect::None);
    assert!(app.reviewer_setup().is_none());
    assert_eq!(
        notices(&app),
        [(
            "session_not_found".to_owned(),
            "No session on screen.".to_owned()
        )]
    );
}

#[test]
fn with_the_link_down_nothing_opens_and_the_draft_stays() {
    let (mut app, _) = app();
    app.outbox.link = Link::Down;
    type_text(&mut app, "/reviewer-setup");
    assert_eq!(press(&mut app, Key::Enter), Effect::None);
    assert!(app.reviewer_setup().is_none());
    assert!(notices(&app).iter().all(|(code, _)| code != "io_failed"));
    assert!(on_screen(&mut app).contains("/reviewer-setup"));
}

#[test]
fn with_no_seam_the_not_available_notice_shows() {
    let mut app = session_app();
    assert_eq!(enter(&mut app, "/reviewer-setup"), Effect::None);
    assert!(app.reviewer_setup().is_none());
    assert_eq!(
        notices(&app),
        [(
            "io_failed".to_owned(),
            "Not available in this terminal.".to_owned()
        )]
    );
}

#[test]
fn a_failed_read_shows_one_notice_and_opens_nothing() {
    let (mut app, fake) = app();
    *fake.notes.lock().expect("notes lock") = Err(failure("config.json: unreadable"));
    assert_eq!(enter(&mut app, "/reviewer-setup"), Effect::None);
    assert!(app.reviewer_setup().is_none());
    assert_eq!(
        notices(&app),
        [(
            "io_failed".to_owned(),
            "Could not read the reviewer notes: config.json: unreadable".to_owned()
        )]
    );
}

// ----------------------------------------------------------- the binding

const NOT_LOADED: &str = "Could not read the reviewer notes: the session has not loaded yet.";
const NO_PROJECT: &str = "Could not read the reviewer notes: this session has no project.";

fn opens_nothing_with(app: &mut App, expect: &str) {
    assert_eq!(enter(app, "/reviewer-setup"), Effect::None);
    assert!(app.reviewer_setup().is_none());
    assert_eq!(notices(app), [("io_failed".to_owned(), expect.to_owned())]);
    // The draft was cleared with the notice.
    assert!(!on_screen(app).contains("/reviewer-setup"));
}

#[test]
fn with_no_home_the_session_has_not_loaded() {
    let mut app = crate::app::commands::commands_testkit::attached();
    app.set_configure(Some(
        Arc::new(Fake::new(Vec::new())) as Arc<dyn crate::Configure>
    ));
    opens_nothing_with(&mut app, NOT_LOADED);
}

#[test]
fn with_no_row_for_the_session_it_has_not_loaded() {
    let mut app = home_app();
    app.set_configure(Some(
        Arc::new(Fake::new(Vec::new())) as Arc<dyn crate::Configure>
    ));
    app.attach(SessionId(OTHER.to_owned()));
    opens_nothing_with(&mut app, NOT_LOADED);
}

#[test]
fn a_row_with_no_project_has_no_project() {
    let (mut app, _) = app();
    app.on_line(status(SESSION, "/w", "", "idle"));
    opens_nothing_with(&mut app, NO_PROJECT);
}

#[test]
fn a_row_with_no_workspace_has_no_project() {
    let (mut app, _) = app();
    app.on_line(status(SESSION, "", "-w", "idle"));
    opens_nothing_with(&mut app, NO_PROJECT);
}

#[test]
fn the_save_names_the_rows_workspace_not_the_launch_directory() {
    let (mut app, fake) = app();
    set_saved(&fake, saved_both());
    // The launch directory is `/w`; the session's own row says otherwise.
    app.on_line(status(
        SESSION,
        "/elsewhere/project",
        "-elsewhere-project",
        "idle",
    ));
    start(&mut app);
    to_the_diff_with_a_line(&mut app);
    press(&mut app, Key::Enter);
    let made = fake.notes_saves();
    assert_eq!(made.len(), 1);
    assert_eq!(made[0].0, PathBuf::from("/elsewhere/project"));
}

#[test]
fn the_binding_is_fixed_when_the_row_changes_after_the_open() {
    let (mut app, fake) = app();
    set_saved(&fake, saved_both());
    start(&mut app);
    app.on_line(status(SESSION, "/moved", "-moved", "idle"));
    to_the_diff_with_a_line(&mut app);
    press(&mut app, Key::Enter);
    assert_eq!(fake.notes_saves()[0].0, PathBuf::from("/w"));
}

// -------------------------------------------------------------- requests

#[test]
fn enter_sends_one_propose_notes_for_the_bound_session_and_waits() {
    let (mut app, fake) = app();
    set_notes(
        &fake,
        notes("### Source control\n- g\n", "### Source control\n- p\n"),
    );
    start(&mut app);
    type_text(&mut app, "github.com/acme");
    let line = propose(&mut app);
    assert_eq!(line["session_id"], SESSION);
    assert_eq!(line["args"]["topic"], "source_control");
    assert_eq!(line["args"]["answer"], "github.com/acme");
    assert_eq!(line["args"]["earlier"], json!(["g", "p"]));
    assert!(app.outbox.pending.contains_key(&id_of(&line)));
    assert_eq!(screen(&app), Screen::Waiting);
}

#[test]
fn a_blank_answer_sends_no_answer() {
    let (mut app, _) = app();
    start(&mut app);
    type_text(&mut app, "  ");
    let line = propose(&mut app);
    assert!(line["args"].get("answer").is_none_or(Value::is_null));
}

#[test]
fn the_matching_answer_opens_the_lines_screen_and_settles_the_entry() {
    let (mut app, _) = app();
    start(&mut app);
    ask(&mut app, one_line());
    assert_eq!(screen(&app), Screen::Lines);
    assert!(app.outbox.pending.is_empty());
}

#[test]
fn an_answer_for_another_session_changes_nothing() {
    let (mut app, _) = app();
    start(&mut app);
    let id = id_of(&propose(&mut app));
    app.on_line(with_session(OTHER, accepted(&id, one_line())));
    assert_eq!(screen(&app), Screen::Waiting);
}

#[test]
fn an_answer_for_another_command_changes_nothing() {
    let (mut app, _) = app();
    start(&mut app);
    propose(&mut app);
    app.on_line(accepted("other-id", one_line()));
    assert_eq!(screen(&app), Screen::Waiting);
}

#[test]
fn an_answer_with_no_propose_notes_result_changes_nothing() {
    let (mut app, _) = app();
    start(&mut app);
    let id = id_of(&propose(&mut app));
    app.on_line(accepted(&id, json!({"commands": []})));
    assert_eq!(screen(&app), Screen::Waiting);
}

#[test]
fn a_rejection_of_the_pending_id_reopens_the_question_with_its_message_and_no_notice() {
    let (mut app, _) = app();
    start(&mut app);
    type_text(&mut app, "acme");
    let id = id_of(&propose(&mut app));
    let before = notices(&app);
    app.on_line(rejected(&id, "busy", "An approval is waiting."));
    assert_eq!(screen(&app), Screen::Question);
    assert_eq!(answer(&app), "acme");
    assert_eq!(error(&app).as_deref(), Some("An approval is waiting."));
    assert_eq!(notices(&app), before);
    assert!(app.outbox.pending.is_empty());
}

#[test]
fn a_rejection_of_another_command_changes_nothing() {
    let (mut app, _) = app();
    start(&mut app);
    propose(&mut app);
    app.on_line(rejected("other-id", "busy", "An approval is waiting."));
    assert_eq!(screen(&app), Screen::Waiting);
}

#[test]
fn esc_while_waiting_drops_an_accepted_answer_when_it_arrives() {
    let (mut app, _) = app();
    start(&mut app);
    type_text(&mut app, "acme");
    let id = id_of(&propose(&mut app));
    assert_eq!(press(&mut app, Key::Esc), Effect::None);
    assert_eq!(screen(&app), Screen::Question);
    assert_eq!(answer(&app), "acme");
    assert!(app.on_line(accepted(&id, one_line())).is_empty());
    assert_eq!(screen(&app), Screen::Question);
    assert!(app.outbox.pending.is_empty());
}

#[test]
fn esc_while_waiting_drops_a_rejection_when_it_arrives() {
    let (mut app, _) = app();
    start(&mut app);
    let id = id_of(&propose(&mut app));
    press(&mut app, Key::Esc);
    let before = notices(&app);
    app.on_line(rejected(&id, "busy", "An approval is waiting."));
    assert_eq!(screen(&app), Screen::Question);
    assert_eq!(error(&app), None);
    assert_eq!(notices(&app), before);
}

#[test]
fn closing_with_a_request_pending_drops_its_answer_silently() {
    let (mut app, _) = app();
    start(&mut app);
    let id = id_of(&propose(&mut app));
    app.reviewer_setup_close();
    assert!(!app.reviewer_setup_open());
    let before = notices(&app);
    assert!(app.on_line(accepted(&id, one_line())).is_empty());
    assert!(app.reviewer_setup().is_none());
    assert_eq!(notices(&app), before);
    assert!(app.outbox.pending.is_empty());
}

#[test]
fn an_answer_settles_only_once() {
    let (mut app, _) = app();
    start(&mut app);
    let id = id_of(&propose(&mut app));
    app.on_line(accepted(&id, one_line()));
    assert_eq!(screen(&app), Screen::Lines);
    // The same line again is no longer the pending one.
    press(&mut app, Key::Esc);
    assert_eq!(screen(&app), Screen::Question);
    app.on_line(accepted(&id, one_line()));
    assert_eq!(screen(&app), Screen::Question);
}

// ----------------------------------------------------------------- busy

#[test]
fn a_busy_session_refuses_enter_on_a_question_with_a_dim_row_and_no_notice() {
    let (mut app, _) = app();
    start(&mut app);
    turn_starts(&mut app);
    type_text(&mut app, "acme");
    let before = notices(&app);
    let pending = app.outbox.pending.len();
    assert_eq!(press(&mut app, Key::Enter), Effect::None);
    assert_eq!(screen(&app), Screen::Question);
    assert_eq!(answer(&app), "acme");
    assert_eq!(
        error(&app).as_deref(),
        Some("A turn is running. Propose again when it ends.")
    );
    assert_eq!(app.outbox.pending.len(), pending);
    assert_eq!(notices(&app), before);
    assert!(app.reviewer_setup_open());
}

#[test]
fn a_busy_session_still_takes_every_other_key() {
    let (mut app, _) = app();
    start(&mut app);
    turn_starts(&mut app);
    type_text(&mut app, "ab");
    press(&mut app, Key::Backspace);
    assert_eq!(answer(&app), "a");
}

#[test]
fn the_turn_ending_lets_enter_propose_again() {
    let (mut app, _) = app();
    start(&mut app);
    turn_starts(&mut app);
    press(&mut app, Key::Enter);
    assert!(error(&app).is_some());
    app.on_line(session_line(
        "turn_completed",
        json!({"outcome": "completed"}),
    ));
    propose(&mut app);
    assert_eq!(screen(&app), Screen::Waiting);
}

// ----------------------------------------------------------------- save

#[test]
fn the_save_sends_only_the_changed_layer_with_the_version_read() {
    let (mut app, fake) = app();
    set_saved(&fake, saved_both());
    start(&mut app);
    to_the_diff_with_a_line(&mut app);
    assert_eq!(press(&mut app, Key::Enter), Effect::None);
    let made = fake.notes_saves();
    assert_eq!(made.len(), 1);
    assert_eq!(made[0].0, PathBuf::from("/w"));
    assert_eq!(made[0].1, None);
    assert_eq!(
        made[0].2,
        Some(NotesWrite {
            version: "project-v1".to_owned(),
            text: "### Source control\n- github.com/acme/* are ours\n".to_owned(),
        })
    );
    assert_eq!(screen(&app), Screen::Saved);
}

#[test]
fn a_stale_save_reads_both_layers_again_and_shows_the_diff_on_the_fresh_text() {
    let (mut app, fake) = app();
    set_saved(&fake, Ok(NotesSaved::Stale));
    start(&mut app);
    to_the_diff_with_a_line(&mut app);
    // Someone else wrote the project file meanwhile.
    let mut fresh = notes("", "### Source control\n- theirs\n");
    fresh.project.version = "project-v2".to_owned();
    set_notes(&fake, fresh);
    press(&mut app, Key::Enter);
    assert_eq!(screen(&app), Screen::Diff);
    set_saved(&fake, saved_both());
    press(&mut app, Key::Enter);
    let made = fake.notes_saves();
    assert_eq!(made.len(), 2);
    let write = made[1].2.clone().expect("a project write");
    assert_eq!(write.version, "project-v2");
    assert!(write.text.contains("- theirs"), "{}", write.text);
    assert!(write.text.contains("- github.com/acme/* are ours"));
}

#[test]
fn a_stale_save_whose_reread_fails_stays_on_the_diff_with_the_error() {
    let (mut app, fake) = app();
    set_saved(&fake, Ok(NotesSaved::Stale));
    start(&mut app);
    to_the_diff_with_a_line(&mut app);
    *fake.notes.lock().expect("notes lock") = Err(failure("config.json: unreadable"));
    press(&mut app, Key::Enter);
    assert_eq!(screen(&app), Screen::Diff);
    assert_eq!(error(&app).as_deref(), Some("config.json: unreadable"));
}

#[test]
fn a_failed_save_stays_on_the_diff_with_the_error() {
    let (mut app, fake) = app();
    set_saved(&fake, Err(failure("disk full")));
    start(&mut app);
    to_the_diff_with_a_line(&mut app);
    assert_eq!(press(&mut app, Key::Enter), Effect::None);
    assert_eq!(screen(&app), Screen::Diff);
    assert_eq!(error(&app).as_deref(), Some("disk full"));
}

fn to_saved(app: &mut App, fake: &Fake) {
    set_saved(fake, saved_both());
    start(app);
    to_the_diff_with_a_line(app);
    press(app, Key::Enter);
    assert_eq!(screen(app), Screen::Saved);
}

#[test]
fn r_on_the_saved_screen_reloads_the_bound_session_and_closes() {
    let (mut app, fake) = app();
    to_saved(&mut app, &fake);
    let lines = sent(press(&mut app, Key::Char('r')));
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["command"], "reload");
    assert_eq!(lines[0]["session_id"], SESSION);
    assert!(app.reviewer_setup().is_none());
}

#[test]
fn n_on_the_saved_screen_closes_and_sends_nothing() {
    let (mut app, fake) = app();
    to_saved(&mut app, &fake);
    assert_eq!(press(&mut app, Key::Char('n')), Effect::None);
    assert!(app.reviewer_setup().is_none());
}

#[test]
fn esc_on_the_saved_screen_closes_and_sends_nothing() {
    let (mut app, fake) = app();
    to_saved(&mut app, &fake);
    assert_eq!(press(&mut app, Key::Esc), Effect::None);
    assert!(app.reviewer_setup().is_none());
}

#[test]
fn r_and_n_do_nothing_when_no_file_was_written() {
    let (mut app, fake) = app();
    set_saved(
        &fake,
        Ok(NotesSaved::Written(vec![FileSaved {
            file: PathBuf::from(PROJECT_FILE),
            outcome: FileOutcome::NotSaved(failure("read-only")),
        }])),
    );
    start(&mut app);
    to_the_diff_with_a_line(&mut app);
    press(&mut app, Key::Enter);
    assert_eq!(screen(&app), Screen::Saved);
    assert_eq!(press(&mut app, Key::Char('r')), Effect::None);
    assert_eq!(press(&mut app, Key::Char('n')), Effect::None);
    assert!(app.reviewer_setup_open());
}

#[test]
fn esc_on_the_diff_sends_no_save_and_closes() {
    let (mut app, fake) = app();
    start(&mut app);
    to_the_diff_with_a_line(&mut app);
    assert_eq!(press(&mut app, Key::Esc), Effect::None);
    assert!(app.reviewer_setup().is_none());
    assert!(fake.notes_saves().is_empty());
}

#[test]
fn enter_on_a_diff_with_no_changed_layer_sends_no_save_and_says_nothing_changed() {
    let (mut app, fake) = app();
    start(&mut app);
    while screen(&app) != Screen::Diff {
        ask(&mut app, nothing());
        press(&mut app, Key::Enter);
    }
    assert_eq!(press(&mut app, Key::Enter), Effect::None);
    assert!(app.reviewer_setup().is_none());
    assert!(fake.notes_saves().is_empty());
    assert_eq!(
        notices(&app),
        [(
            "notes_unchanged".to_owned(),
            "Nothing changed. No file was written.".to_owned()
        )]
    );
}

// --------------------------------------------------------- interruptions

#[test]
fn attaching_to_another_session_closes_with_a_notice_and_saves_nothing() {
    let (mut app, fake) = app();
    start(&mut app);
    to_the_diff_with_a_line(&mut app);
    app.attach(SessionId(OTHER.to_owned()));
    assert!(app.reviewer_setup().is_none());
    assert!(fake.notes_saves().is_empty());
    assert_eq!(
        notices(&app),
        [(
            "session_not_found".to_owned(),
            "The reviewer setup closed: another session is on screen. Nothing was saved."
                .to_owned()
        )]
    );
}

#[test]
fn going_home_closes_with_the_notice_and_opening_another_session_adds_none() {
    let (mut app, _) = app();
    start(&mut app);
    app.go_home();
    assert!(app.reviewer_setup().is_none());
    assert_eq!(notices(&app).len(), 1);
    app.attach(SessionId(OTHER.to_owned()));
    assert!(app.reviewer_setup().is_none());
    assert_eq!(notices(&app).len(), 1);
}

#[test]
fn going_home_with_no_setup_pushes_no_notice() {
    let (mut app, _) = app();
    app.go_home();
    assert!(notices(&app).is_empty());
}

#[test]
fn a_hub_reconnect_keeps_the_setup_reopens_the_question_and_drops_the_resent_answer() {
    let (mut app, _) = app();
    start(&mut app);
    type_text(&mut app, "acme");
    let line = propose(&mut app);
    let id = id_of(&line);
    // The loop's write keeps the line for a reconnect.
    app.outbox.wrote(&line.to_string());
    app.disconnected();
    let resent: Vec<Value> = app
        .on_line(hub_line("hub_hello", json!({})))
        .iter()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    assert!(
        resent
            .iter()
            .any(|line| line["command"] == "propose_notes" && line["id"] == id),
        "{resent:?}"
    );
    assert!(app.reviewer_setup_open());
    assert_eq!(screen(&app), Screen::Question);
    assert_eq!(answer(&app), "acme");
    assert_eq!(
        error(&app).as_deref(),
        Some("The session restarted. Enter proposes again.")
    );
    // The resent command's answer carries the forgotten id.
    let before = notices(&app);
    assert!(app.on_line(accepted(&id, one_line())).is_empty());
    assert_eq!(screen(&app), Screen::Question);
    assert_eq!(notices(&app), before);
}

#[test]
fn a_reconnect_with_nothing_waiting_changes_nothing() {
    let (mut app, _) = app();
    start(&mut app);
    type_text(&mut app, "acme");
    app.disconnected();
    app.on_line(hub_line("hub_hello", json!({})));
    assert!(app.reviewer_setup_open());
    assert_eq!(screen(&app), Screen::Question);
    assert_eq!(error(&app), None);
    assert_eq!(answer(&app), "acme");
}

#[test]
fn decisions_made_survive_a_restart_of_a_waiting_request() {
    let (mut app, _) = app();
    start(&mut app);
    ask(&mut app, one_line());
    press(&mut app, Key::Enter);
    assert_eq!(topic(&app), Topic::InternalHosts);
    propose(&mut app);
    app.on_line(hub("stream_closed", SESSION));
    assert_eq!(screen(&app), Screen::Question);
    press(&mut app, Key::Esc);
    assert_eq!(screen(&app), Screen::Lines);
    assert_eq!(topic(&app), Topic::SourceControl);
    let rows = app.reviewer_setup().expect("open").line_rows();
    assert!(rows.iter().any(|row| row.state == LineState::Kept));
}

#[test]
fn a_stream_line_naming_the_bound_session_ends_a_waiting_proposal() {
    for kind in ["stream_reopened", "stream_closed", "session_left"] {
        let (mut app, _) = app();
        start(&mut app);
        type_text(&mut app, "acme");
        let id = id_of(&propose(&mut app));
        app.on_line(hub(kind, SESSION));
        assert_eq!(screen(&app), Screen::Question, "{kind}");
        assert_eq!(answer(&app), "acme", "{kind}");
        assert_eq!(
            error(&app).as_deref(),
            Some("The session restarted. Enter proposes again."),
            "{kind}"
        );
        app.on_line(accepted(&id, one_line()));
        assert_eq!(screen(&app), Screen::Question, "{kind}: answer dropped");
    }
}

#[test]
fn a_stream_line_with_nothing_waiting_changes_nothing() {
    for kind in ["stream_reopened", "stream_closed", "session_left"] {
        let (mut app, _) = app();
        start(&mut app);
        app.on_line(hub(kind, SESSION));
        assert_eq!(screen(&app), Screen::Question, "{kind}");
        assert_eq!(error(&app), None, "{kind}");
    }
}

#[test]
fn a_stream_line_naming_another_session_changes_nothing() {
    for kind in ["stream_reopened", "stream_closed", "session_left"] {
        let (mut app, _) = app();
        start(&mut app);
        propose(&mut app);
        app.on_line(hub(kind, OTHER));
        assert_eq!(screen(&app), Screen::Waiting, "{kind}");
    }
}

fn approval() -> Line {
    with_action(session_line(
        "permission_requested",
        json!({"request_id": "r_2", "effects": ["executes"], "reversible": true,
            "step": "standing_ask",
            "standing_rule": {"scope": "global", "prefix": "p"}}),
    ))
}

fn approval_resolved() -> Line {
    with_action(session_line(
        "permission_resolved",
        json!({"request_id": "r_2", "decision": "deny", "decided_by": "cancel"}),
    ))
}

#[test]
fn an_approval_hides_the_setup_and_leaving_brings_it_back_unchanged() {
    let (mut app, _) = app();
    start(&mut app);
    type_text(&mut app, "acme");
    app.on_line(approval());
    assert!(!app.top_surfaces().contains(&Surface::ReviewerSetup));
    assert!(!app.reviewer_setup_open());
    // No key reaches it while the approval is up.
    press(&mut app, Key::Char('z'));
    assert_eq!(answer(&app), "acme");
    app.on_line(approval_resolved());
    assert!(app.top_surfaces().contains(&Surface::ReviewerSetup));
    assert_eq!(screen(&app), Screen::Question);
    assert_eq!(answer(&app), "acme");
}

// -------------------------------------------------------------- Ctrl+C

#[test]
fn two_ctrl_c_presses_quit_when_no_session_works() {
    let (mut app, _) = app();
    app.on_line(status(SESSION, "/w", "-w", "idle"));
    start(&mut app);
    type_text(&mut app, "acme");
    assert_eq!(app.on_key(Key::CtrlC, now()), Effect::None);
    assert_eq!(app.on_key(Key::CtrlC, now()), Effect::Quit);
}

#[test]
fn one_ctrl_c_changes_nothing_in_the_setup() {
    let (mut app, _) = app();
    start(&mut app);
    type_text(&mut app, "acme");
    assert_eq!(app.on_key(Key::CtrlC, now()), Effect::None);
    assert_eq!(screen(&app), Screen::Question);
    assert_eq!(answer(&app), "acme");
    assert!(app.reviewer_setup_open());
}

#[test]
fn ctrl_c_from_the_waiting_screen_leaves_the_request_waiting() {
    let (mut app, _) = app();
    app.on_line(status(SESSION, "/w", "-w", "idle"));
    start(&mut app);
    propose(&mut app);
    assert_eq!(app.on_key(Key::CtrlC, now()), Effect::None);
    assert_eq!(screen(&app), Screen::Waiting);
    assert_eq!(app.on_key(Key::CtrlC, now()), Effect::Quit);
}

#[test]
fn ctrl_c_from_the_diff_leaves_the_diff_and_quits_on_the_second_press() {
    let (mut app, fake) = app();
    app.on_line(status(SESSION, "/w", "-w", "idle"));
    start(&mut app);
    to_the_diff_with_a_line(&mut app);
    assert_eq!(app.on_key(Key::CtrlC, now()), Effect::None);
    assert_eq!(screen(&app), Screen::Diff);
    assert_eq!(app.on_key(Key::CtrlC, now()), Effect::Quit);
    assert!(fake.notes_saves().is_empty());
}

#[test]
fn with_a_session_working_the_second_ctrl_c_asks_and_esc_returns_to_the_same_screen() {
    let (mut app, _) = app();
    start(&mut app);
    type_text(&mut app, "acme");
    assert_eq!(app.on_key(Key::CtrlC, now()), Effect::None);
    assert_eq!(app.on_key(Key::CtrlC, now()), Effect::None);
    assert_eq!(app.top_surface(), Surface::Quit);
    // The question takes the next key: the setup's field does not.
    press(&mut app, Key::Char('z'));
    assert_eq!(answer(&app), "acme");
    assert_eq!(press(&mut app, Key::Esc), Effect::None);
    assert_eq!(app.top_surface(), Surface::ReviewerSetup);
    assert_eq!(screen(&app), Screen::Question);
    assert_eq!(answer(&app), "acme");
}

// -------------------------------------------------------------- surfaces

#[test]
fn the_setup_sits_between_the_key_map_and_the_picker() {
    let (mut app, _) = app();
    start(&mut app);
    open(&mut app, Surface::Keymap);
    open(&mut app, Surface::Picker);
    let ours: Vec<Surface> = app
        .top_surfaces()
        .into_iter()
        .filter(|surface| {
            matches!(
                surface,
                Surface::Keymap | Surface::ReviewerSetup | Surface::Picker
            )
        })
        .collect();
    assert_eq!(
        ours,
        [Surface::Keymap, Surface::ReviewerSetup, Surface::Picker]
    );
    assert_eq!(app.top_surface(), Surface::Keymap);
}

#[test]
fn the_setup_is_absent_once_another_session_is_on_screen() {
    let (mut app, _) = app();
    start(&mut app);
    app.attach(SessionId(OTHER.to_owned()));
    assert!(!app.top_surfaces().contains(&Surface::ReviewerSetup));
}

#[test]
fn a_paste_goes_into_the_answer_only_while_the_setup_is_on_top() {
    let (mut app, _) = app();
    start(&mut app);
    type_text(&mut app, "a");
    assert_eq!(app.on_edit(Edit::Paste("b\nc".to_owned())), Effect::None);
    assert_eq!(answer(&app), "ab c");
    // Another surface on top takes the edit instead.
    open(&mut app, Surface::Keymap);
    app.on_edit(Edit::Paste("zzz".to_owned()));
    assert_eq!(answer(&app), "ab c");
}

#[test]
fn every_other_edit_does_nothing_behind_the_setup() {
    let (mut app, _) = app();
    start(&mut app);
    type_text(&mut app, "ab");
    for edit in [
        Edit::Left,
        Edit::Right,
        Edit::ShiftEnter,
        Edit::CtrlJ,
        Edit::WordLeft,
        Edit::WordRight,
    ] {
        assert_eq!(app.on_edit(edit), Effect::None);
    }
    assert_eq!(answer(&app), "ab");
    assert!(app.input().expand().is_empty(), "the composer stayed empty");
}

fn wheel(app: &mut App, kind: MouseKind, col: u16, row: u16) {
    let (_, targets) = drawn(app);
    let mut pointer = Pointer::default();
    app.on_mouse(
        &Mouse { kind, col, row },
        &targets,
        &mut pointer,
        true,
        now(),
    );
}

#[test]
fn the_wheel_does_nothing_while_the_setup_is_open() {
    let (mut app, _) = app();
    start(&mut app);
    let before = on_screen(&mut app);
    let (width, height) = app.screen_size();
    let (_, targets) = drawn(&app);
    let slab = app
        .frames()
        .slab_of(Surface::ReviewerSetup)
        .expect("the setup drew");
    let outside = outside_corner("wheel", &targets, slab, width, height);
    for kind in [MouseKind::WheelUp, MouseKind::WheelDown] {
        wheel(&mut app, kind, slab.x + 1, slab.y + 1);
        assert_eq!(on_screen(&mut app), before);
        wheel(&mut app, kind, outside.0, outside.1);
        assert_eq!(on_screen(&mut app), before);
    }
}

#[test]
fn the_cross_closes_the_setup_and_saves_nothing() {
    let (mut app, fake) = app();
    start(&mut app);
    to_the_diff_with_a_line(&mut app);
    let (_, targets) = drawn(&app);
    let cross = targets
        .iter()
        .find(|target| target.id == TargetId::CloseOverlay)
        .expect("the setup draws its cross");
    assert_eq!(app.on_click(cross.id), Effect::None);
    assert!(app.reviewer_setup().is_none());
    assert!(fake.notes_saves().is_empty());
}

// ------------------------------------------------- a press outside (R51)

/// The first screen corner outside the setup's slab, with the frame's
/// targets.
fn outside(app: &App) -> (u16, u16, Vec<Target>) {
    let (width, height) = app.screen_size();
    let (_, targets) = drawn(app);
    let slab = app
        .frames()
        .slab_of(Surface::ReviewerSetup)
        .expect("the setup drew");
    let (col, row) = outside_corner("reviewer setup", &targets, slab, width, height);
    (col, row, targets)
}

/// What a screen shows of the setup: where it is and what it holds.
fn state(app: &App) -> Option<(Screen, Topic, String)> {
    app.reviewer_setup()
        .map(|setup| (setup.screen(), setup.topic(), setup.answer().to_owned()))
}

/// Brings an app to a screen.
type Reach = fn(&mut App, &Fake);

/// Builds two apps in the same state with `reach`.
fn twins(reach: Reach) -> (App, App) {
    let (mut app, fake) = app();
    reach(&mut app, &fake);
    let (mut twin, twin_fake) = self::app();
    reach(&mut twin, &twin_fake);
    (app, twin)
}

fn at_topic_one_question(app: &mut App, _: &Fake) {
    start(app);
    type_text(app, "typed");
}

fn at_topic_two_question(app: &mut App, _: &Fake) {
    start(app);
    ask(app, one_line());
    press(app, Key::Enter);
    assert_eq!(topic(app), Topic::InternalHosts);
}

fn at_waiting(app: &mut App, _: &Fake) {
    start(app);
    type_text(app, "typed");
    propose(app);
}

fn at_lines(app: &mut App, _: &Fake) {
    start(app);
    ask(app, one_line());
}

fn at_diff(app: &mut App, _: &Fake) {
    start(app);
    to_the_diff_with_a_line(app);
}

fn at_saved(app: &mut App, fake: &Fake) {
    to_saved(app, fake);
}

#[test]
fn a_press_outside_equals_esc_on_each_screen() {
    let cases: [(&str, Reach); 6] = [
        ("topic one question", at_topic_one_question),
        ("topic two question", at_topic_two_question),
        ("waiting", at_waiting),
        ("lines", at_lines),
        ("diff", at_diff),
        ("saved", at_saved),
    ];
    for (label, reach) in cases {
        let (mut app, mut twin) = twins(reach);
        let (col, row, targets) = outside(&app);
        let esc = twin.on_key(Key::Esc, now());
        let effect = press_at(&mut app, &targets, col, row);
        assert_eq!(effect, esc, "{label}");
        assert_eq!(state(&app), state(&twin), "{label}");
        assert_eq!(
            app.outbox.pending.len(),
            twin.outbox.pending.len(),
            "{label}"
        );
    }
}

#[test]
fn a_press_outside_on_topic_one_closes_and_drops_the_answer() {
    let (mut app, _) = twins(at_topic_one_question);
    let (col, row, targets) = outside(&app);
    press_at(&mut app, &targets, col, row);
    assert!(app.reviewer_setup().is_none());
}

#[test]
fn a_press_outside_on_topic_two_shows_topic_ones_lines_with_decisions_kept() {
    let (mut app, _) = twins(at_topic_two_question);
    let (col, row, targets) = outside(&app);
    press_at(&mut app, &targets, col, row);
    assert_eq!(screen(&app), Screen::Lines);
    assert_eq!(topic(&app), Topic::SourceControl);
    let rows = app.reviewer_setup().expect("open").line_rows();
    assert!(rows.iter().any(|row| row.state == LineState::Kept));
}

#[test]
fn a_press_outside_while_waiting_drops_the_answer_when_it_arrives() {
    let (mut app, _) = twins(at_waiting);
    let id = app
        .outbox
        .pending
        .keys()
        .next()
        .cloned()
        .expect("a request");
    let (col, row, targets) = outside(&app);
    press_at(&mut app, &targets, col, row);
    assert_eq!(screen(&app), Screen::Question);
    app.on_line(accepted(&id, one_line()));
    assert_eq!(screen(&app), Screen::Question);
}

#[test]
fn a_press_outside_on_the_diff_sends_no_save() {
    let (mut app, fake) = app();
    at_diff(&mut app, &fake);
    let (col, row, targets) = outside(&app);
    press_at(&mut app, &targets, col, row);
    assert!(app.reviewer_setup().is_none());
    assert!(fake.notes_saves().is_empty());
}

#[test]
fn a_press_on_the_slab_edge_leaves_the_setup_open_and_unchanged() {
    let (mut app, _) = app();
    start(&mut app);
    type_text(&mut app, "typed");
    let (_, targets) = drawn(&app);
    let slab = app
        .frames()
        .slab_of(Surface::ReviewerSetup)
        .expect("the setup drew");
    let corners = [
        (slab.x, slab.y),
        (slab.right() - 1, slab.y),
        (slab.x, slab.bottom() - 1),
        (slab.right() - 1, slab.bottom() - 1),
    ];
    for (col, row) in corners {
        let on_cross = targets
            .iter()
            .any(|t| t.id == TargetId::CloseOverlay && t.rect.contains(Position::new(col, row)));
        if on_cross {
            continue;
        }
        assert_eq!(press_at(&mut app, &targets, col, row), Effect::None);
        assert!(app.reviewer_setup_open(), "({col}, {row}) is inside");
        assert_eq!(answer(&app), "typed");
    }
}

#[test]
fn a_press_on_the_cross_closes_outright_with_nothing_sent() {
    let (mut app, fake) = app();
    at_diff(&mut app, &fake);
    let (_, targets) = drawn(&app);
    let cross = targets
        .iter()
        .find(|target| target.id == TargetId::CloseOverlay)
        .expect("the setup draws its cross");
    let (col, row) = (cross.rect.x, cross.rect.y);
    // A click is a press and a release on the same target.
    let mut pointer = Pointer::default();
    for kind in [MouseKind::Press(Button::Left), MouseKind::Release] {
        let mouse = Mouse { kind, col, row };
        assert_eq!(
            app.on_mouse(&mouse, &targets, &mut pointer, true, now()),
            Effect::None
        );
    }
    assert!(app.reviewer_setup().is_none());
    assert!(fake.notes_saves().is_empty());
}

#[test]
fn a_press_outside_the_quit_question_closes_only_the_quit_question() {
    let (mut app, _) = app();
    start(&mut app);
    type_text(&mut app, "typed");
    app.quit();
    assert_eq!(app.top_surface(), Surface::Quit);
    let (width, height) = app.screen_size();
    let (_, targets) = drawn(&app);
    let slab = app.frames().slab_of(Surface::Quit).expect("the quit drew");
    let (col, row) = outside_corner("quit over setup", &targets, slab, width, height);
    press_at(&mut app, &targets, col, row);
    assert_ne!(app.top_surface(), Surface::Quit);
    assert!(app.reviewer_setup_open());
    assert_eq!(answer(&app), "typed");
}
