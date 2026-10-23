//! Tests for choosing a not-installed provider in the picker: the job it
//! starts, the rows it draws, and what the result does (`docs/tui.md`,
//! "Swapped views").

use std::path::PathBuf;
use std::sync::Arc;

use contract::ErrorCode;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::json;

use super::super::{App, ConfigView, Effect, Link, NoticeLine};
use crate::catalogue::Refresh;
use crate::configure::{Configure, ConfigureError, LoginNeed, ProviderInstall};
use crate::configure_fake::Fake;
use crate::home::Launch;
use crate::keys::{Edit, Key};
use crate::model_picker::fixtures::catalogue;
use crate::model_picker::{InstallStart, Mode};
use crate::swapped::Spot;

fn now() -> std::time::Instant {
    use contract::clock::Clock;
    fakes::clock::FakeClock::new().now()
}

/// A local home at "/w", 100x30, with `seam` and a catalogue of one model
/// and `openai` and `zeta` not installed.
fn home(seam: Option<Arc<Fake>>) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        ..Default::default()
    });
    app.set_size(100, 30);
    app.set_configure(seam.map(|seam| seam as Arc<dyn Configure>));
    app.on_models(Ok(catalogue(&["acme/m1"], &["openai", "zeta"])));
    app
}

fn fake() -> Arc<Fake> {
    Arc::new(Fake::new(Vec::new()))
}

fn open(app: &mut App) {
    assert_eq!(app.on_key(Key::CtrlL, now()), Effect::None);
    assert!(app.model_picker_open());
}

/// Opens the picker and moves onto the first not-installed row.
fn on_first_row(app: &mut App) {
    open(app);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.model_picker.install_target().as_deref(), Some("openai"));
}

fn screen(app: &App) -> String {
    let area = Rect::new(0, 0, 100, 30);
    let mut buf = Buffer::empty(area);
    let _ = crate::view::render(app, area, &mut buf, None);
    crate::view::text(&buf)
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

fn installed(summary: &[&str], login: Vec<LoginNeed>) -> Result<ProviderInstall, ConfigureError> {
    Ok(ProviderInstall {
        installed: Vec::new(),
        summary: summary.iter().map(|line| (*line).to_owned()).collect(),
        login,
    })
}

fn failed(code: ErrorCode, message: &str) -> Result<ProviderInstall, ConfigureError> {
    Err(ConfigureError {
        code,
        message: message.to_owned(),
    })
}

fn missing(provider: &str) -> LoginNeed {
    LoginNeed {
        provider: provider.to_owned(),
        error: None,
    }
}

/// The layout row of the `n`th not-installed row.
fn row_at(app: &App, n: usize) -> usize {
    app.model_picker_view().unwrap().installs[n].at
}

fn job(app: &mut App) -> InstallStart {
    app.take_install_start().expect("an install job")
}

#[test]
fn enter_on_a_focused_row_starts_one_job_and_the_row_reads_installing() {
    let fake = fake();
    let mut app = home(Some(Arc::clone(&fake)));
    on_first_row(&mut app);
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(app.model_picker_open(), "the picker stays open");
    let start = job(&mut app);
    assert_eq!(start.provider, "openai");
    assert_eq!(start.workspace, PathBuf::from("/w"));
    assert!(std::ptr::addr_eq(
        Arc::as_ptr(&start.seam),
        Arc::as_ptr(&fake)
    ));
    assert!(app.take_install_start().is_none());
    assert!(screen(&app).contains(" ⟳ installing"), "{}", screen(&app));
}

#[test]
fn a_second_choice_while_one_installs_starts_nothing() {
    let mut app = home(Some(fake()));
    on_first_row(&mut app);
    let _ = app.on_key(Key::Enter, now());
    let _ = job(&mut app);
    let _ = app.on_key(Key::Enter, now());
    assert!(app.take_install_start().is_none());
    let zeta = row_at(&app, 1);
    let _ = app.model_picker_click(Spot::Cell(zeta, 0));
    assert!(app.take_install_start().is_none());
    // The choice is dropped before any check can fail the other row.
    app.set_configure(None);
    let _ = app.model_picker_click(Spot::Cell(zeta, 0));
    assert!(!screen(&app).contains("✗"), "{}", screen(&app));
}

#[test]
fn a_click_on_the_name_starts_the_job_and_a_click_elsewhere_only_focuses() {
    let mut app = home(Some(fake()));
    open(&mut app);
    let zeta = row_at(&app, 1);
    let _ = app.model_picker_click(Spot::Row(zeta));
    assert!(app.take_install_start().is_none());
    assert_eq!(app.model_picker.install_target().as_deref(), Some("zeta"));
    let openai = row_at(&app, 0);
    let _ = app.model_picker_click(Spot::Cell(openai, 0));
    let start = job(&mut app);
    assert_eq!(start.provider, "openai");
    assert_eq!(app.model_picker.install_target().as_deref(), Some("openai"));
}

#[test]
fn the_session_only_key_and_the_arrows_do_nothing_on_a_row() {
    let mut app = home(Some(fake()));
    on_first_row(&mut app);
    assert_eq!(app.model_picker_session_only(), Effect::None);
    assert_eq!(app.model_picker_edit(&Edit::Left), Some(Effect::None));
    assert_eq!(app.model_picker_edit(&Edit::Right), Some(Effect::None));
    assert!(app.model_picker_open());
    assert!(app.outbox.pending.is_empty());
    assert!(app.take_install_start().is_none());
    assert_eq!(app.model_picker.install_target().as_deref(), Some("openai"));
}

#[test]
fn enter_with_nothing_focused_installs_nothing() {
    // No model shown and an empty query: nothing is focused.
    let mut app = home(Some(fake()));
    app.on_models(Ok(catalogue(&[], &["openai"])));
    open(&mut app);
    assert_eq!(app.model_picker.focus(), None);
    let _ = app.on_key(Key::Enter, now());
    assert!(app.take_install_start().is_none());
    assert!(app.model_picker_open());
    // A query that hides every model but matches a row focuses nothing.
    let mut app = home(Some(fake()));
    open(&mut app);
    let _ = app.on_key(Key::Char('z'), now());
    let _ = app.on_key(Key::Enter, now());
    assert!(app.take_install_start().is_none());
    assert!(app.model_picker_open());
}

#[test]
fn a_success_drops_the_row_draws_the_outcome_asks_one_refresh_and_lists_the_models() {
    let mut app = home(Some(fake()));
    on_first_row(&mut app);
    let _ = app.on_key(Key::Enter, now());
    let _ = job(&mut app);
    assert_eq!(app.take_reads(), Some(Refresh::Stale), "the open's read");
    assert_eq!(app.take_reads(), None);

    app.on_install(
        "openai".to_owned(),
        installed(&["Install openai from x"], vec![missing("openai")]),
    );

    assert!(app.model_picker_open());
    let text = screen(&app);
    assert!(!text.contains("openai · not installed"), "{text}");
    assert!(!text.contains("installing"), "{text}");
    assert!(text.contains("✓ installed openai"), "{text}");
    assert!(text.contains("Install openai from x"), "{text}");
    assert!(
        text.contains("openai needs a credential: run `fiber login openai`"),
        "{text}"
    );
    assert_eq!(app.take_reads(), Some(Refresh::Stale));
    assert_eq!(app.take_reads(), None);
    assert!(notices(&app).is_empty());

    app.on_models(Ok(catalogue(&["acme/m1", "openai/gpt"], &["zeta"])));
    let text = screen(&app);
    assert!(text.contains("gpt"), "{text}");
    assert!(text.contains("2 models"), "{text}");
    assert!(text.contains("✓ installed openai"), "{text}");

    app.model_picker.close();
    open(&mut app);
    let text = screen(&app);
    assert!(!text.contains("✓ installed"), "{text}");
    assert!(text.contains("gpt"), "{text}");
}

#[test]
fn a_failure_keeps_the_row_with_its_error_and_a_retry_runs_a_fresh_job() {
    let mut app = home(Some(fake()));
    on_first_row(&mut app);
    let _ = app.on_key(Key::Enter, now());
    let _ = job(&mut app);
    app.on_install(
        "openai".to_owned(),
        failed(ErrorCode::IoFailed, "no network"),
    );
    assert!(app.model_picker_open());
    let text = screen(&app);
    assert!(text.contains("openai · not installed"), "{text}");
    assert!(text.contains("✗ no network"), "{text}");
    assert!(
        notices(&app).is_empty(),
        "the open picker shows it on the row"
    );
    assert_eq!(
        app.take_reads(),
        Some(Refresh::Stale),
        "only the open's read"
    );
    assert_eq!(app.take_reads(), None);

    let _ = app.on_key(Key::Enter, now());
    assert_eq!(job(&mut app).provider, "openai");
    assert!(screen(&app).contains(" ⟳ installing"));
    app.on_install("openai".to_owned(), installed(&["Install"], Vec::new()));
    let text = screen(&app);
    assert!(!text.contains("no network"), "{text}");
    assert!(text.contains("✓ installed openai"), "{text}");
}

#[test]
fn a_result_that_lands_with_the_picker_closed_is_a_notice_and_still_asks_the_refresh() {
    let mut app = home(Some(fake()));
    on_first_row(&mut app);
    let _ = app.on_key(Key::Enter, now());
    let _ = job(&mut app);
    let _ = app.take_reads();
    let _ = app.on_key(Key::Esc, now());
    assert!(!app.model_picker_open());

    app.on_install(
        "openai".to_owned(),
        installed(
            &["Install openai"],
            vec![
                missing("openai"),
                LoginNeed {
                    provider: "zeta".to_owned(),
                    error: Some("no keyring".to_owned()),
                },
            ],
        ),
    );

    assert_eq!(app.take_reads(), Some(Refresh::Stale));
    // `docs/tui.md` promises each notice, not their order: compare sorted.
    let mut shown = notices(&app);
    shown.sort();
    assert_eq!(
        shown,
        [
            (
                "credential_failed".to_owned(),
                "zeta's credential could not be read: no keyring".to_owned()
            ),
            (
                "credential_missing".to_owned(),
                "openai needs a credential: run `fiber login openai`".to_owned()
            ),
        ]
    );
    open(&mut app);
    let text = screen(&app);
    assert!(!text.contains("✓ installed"), "{text}");
    assert!(
        !text.contains("openai · not installed"),
        "no row returns: {text}"
    );
    assert!(text.contains("zeta · not installed"), "{text}");
}

#[test]
fn a_failure_that_lands_with_the_picker_closed_is_a_notice_under_its_own_code() {
    let mut app = home(Some(fake()));
    on_first_row(&mut app);
    let _ = app.on_key(Key::Enter, now());
    let _ = job(&mut app);
    let _ = app.on_key(Key::Esc, now());
    app.on_install(
        "openai".to_owned(),
        failed(ErrorCode::ExtensionFailed, "no network\nsecond"),
    );
    assert_eq!(
        notices(&app),
        [(
            "extension_failed".to_owned(),
            "Installing openai failed: no network".to_owned()
        )]
    );
    open(&mut app);
    assert!(screen(&app).contains("openai · not installed"));
}

#[test]
fn an_install_still_runs_when_the_picker_closes_and_reopens() {
    let mut app = home(Some(fake()));
    on_first_row(&mut app);
    let _ = app.on_key(Key::Enter, now());
    let _ = job(&mut app);
    let _ = app.on_key(Key::Esc, now());
    open(&mut app);
    assert!(screen(&app).contains(" ⟳ installing"), "{}", screen(&app));
    // The running install blocks a second choice after the reopen too.
    let _ = app.on_key(Key::Down, now());
    let _ = app.on_key(Key::Down, now());
    let _ = app.on_key(Key::Enter, now());
    assert!(app.take_install_start().is_none());
}

#[test]
fn the_other_modes_list_no_rows() {
    for mode in [Mode::Scope, Mode::Thinking] {
        let mut app = home(Some(fake()));
        let _ = app.open_model_picker(mode);
        let view = app.model_picker_view().unwrap();
        assert!(view.installs.is_empty(), "{mode:?}");
        assert!(!screen(&app).contains("not installed"), "{mode:?}");
    }
}

#[test]
fn a_scope_hides_the_new_providers_models_until_show_all() {
    let after = catalogue(&["acme/other", "openai/gpt"], &[]);
    let listed = |app: &App| -> Vec<String> {
        app.model_picker_view()
            .unwrap()
            .sections
            .iter()
            .flat_map(|section| section.models.iter().map(|model| model.id.clone()))
            .collect()
    };
    let mut app = home(Some(fake()));
    app.on_models(Ok(catalogue(&["acme/other"], &["openai"])));
    app.model_picker.scoped = vec!["acme/other".to_owned()];
    on_first_row(&mut app);
    let _ = app.on_key(Key::Enter, now());
    let _ = job(&mut app);
    app.on_install("openai".to_owned(), installed(&[], Vec::new()));
    app.on_models(Ok(after.clone()));
    assert_eq!(listed(&app), ["other"]);
    let _ = app.on_key(Key::Tab, now());
    assert_eq!(listed(&app), ["other", "gpt"]);

    let mut app = home(Some(fake()));
    app.on_models(Ok(catalogue(&["acme/other"], &["openai"])));
    on_first_row(&mut app);
    let _ = app.on_key(Key::Enter, now());
    let _ = job(&mut app);
    app.on_install("openai".to_owned(), installed(&[], Vec::new()));
    app.on_models(Ok(after));
    assert_eq!(listed(&app), ["other", "gpt"]);
}

#[test]
fn with_no_seam_the_row_fails_at_once() {
    let mut app = home(None);
    on_first_row(&mut app);
    let _ = app.on_key(Key::Enter, now());
    assert!(app.take_install_start().is_none());
    let text = screen(&app);
    assert!(
        text.contains("✗ Installing is not available in this terminal."),
        "{text}"
    );
    assert!(text.contains("openai · not installed"), "{text}");
}

#[test]
fn with_no_workspace_on_a_local_terminal_the_row_fails_at_once() {
    let mut app = App::new(None);
    app.set_home(Launch::default());
    app.set_size(100, 30);
    app.set_configure(Some(fake() as Arc<dyn Configure>));
    app.on_models(Ok(catalogue(&["acme/m1"], &["openai"])));
    on_first_row(&mut app);
    let _ = app.on_key(Key::Enter, now());
    assert!(app.take_install_start().is_none());
    assert!(
        screen(&app).contains("✗ No workspace to install into."),
        "{}",
        screen(&app)
    );
}

/// A remote home with no local workspace over a router the scripted hub
/// serves, as the config views' remote cases build one.
fn remote_case(
    script: Vec<(serde_json::Value, serde_json::Value)>,
    test: impl FnOnce(App) + Send + 'static,
) {
    remote_case_in(script, None, test);
}

/// As [`remote_case`], with `workspace` as the app's local workspace.
fn remote_case_in(
    script: Vec<(serde_json::Value, serde_json::Value)>,
    workspace: Option<&'static str>,
    test: impl FnOnce(App) + Send + 'static,
) {
    use std::io::{BufRead, BufReader};
    use std::sync::mpsc;
    let deadline = fakes::Deadline::start();
    let router = crate::remote::Router::new();
    let (client, hub) = std::os::unix::net::UnixStream::pair().unwrap();
    let reader = client.try_clone().unwrap();
    router.attach(client);
    let (tx, _rx) = mpsc::channel();
    let feed =
        crate::link::Feed::new(tx, crate::link::Credits::new(64)).routed(Arc::clone(&router));
    let (server, served) = mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(hub.try_clone().unwrap());
        for (expected, result) in script {
            let mut text = String::new();
            reader.read_line(&mut text).unwrap();
            let line: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(line["command"], expected["command"]);
            assert_eq!(line["args"], expected["args"]);
            let payload = if let Some(message) = result.get("reject") {
                json!({"command_id":line["id"],"code":"stale_request","message":message})
            } else {
                json!({"command_id":line["id"],"result":result})
            };
            let kind = if payload.get("code").is_some() {
                "command_rejected"
            } else {
                "command_accepted"
            };
            crate::link::write_line(
                &hub,
                &json!({"kind":kind,"ts":1,"schema_version":1,"payload":payload}).to_string(),
            )
            .unwrap();
        }
        server.send(()).unwrap();
    });
    std::thread::spawn(move || crate::link::read_lines(reader, &feed));
    let (done, finished) = mpsc::channel();
    std::thread::spawn(move || {
        let seam = Arc::new(Fake::new(Vec::new()));
        let remote = Arc::new(crate::remote::Remote::new(seam, "/client".into(), router));
        let mut app = App::new(workspace.map(PathBuf::from));
        if workspace.is_none() {
            app.set_home(Launch {
                location: crate::LaunchLocation::Remote {
                    hub: "box".to_owned(),
                },
                ..Default::default()
            });
        }
        app.set_size(100, 30);
        app.set_configure(Some(Arc::clone(&remote) as Arc<dyn Configure>));
        app.set_remote(remote);
        app.on_models(Ok(catalogue(&["acme/m1"], &["openai"])));
        test(app);
        done.send(()).unwrap();
    });
    deadline.recv_or_fail(&finished, "remote install behaviour");
    deadline.recv_or_fail(&served, "remote install wire script");
}

fn global_file() -> (serde_json::Value, serde_json::Value) {
    (
        json!({"command":"configure","args":{"operation":"global_file"}}),
        json!({"file":"/hub/config.json"}),
    )
}

#[test]
fn on_a_remote_terminal_with_the_link_down_the_row_fails_and_the_hub_is_never_asked() {
    remote_case(Vec::new(), |mut app| {
        assert!(!app.outbox.is_up());
        on_first_row(&mut app);
        let _ = app.on_key(Key::Enter, now());
        assert!(app.take_install_start().is_none());
        assert!(
            screen(&app).contains("✗ The hub is not connected."),
            "{}",
            screen(&app)
        );
    });
}

#[test]
fn a_remote_home_with_no_workspace_installs_at_the_hub_global_parent() {
    remote_case(vec![global_file()], |mut app| {
        app.outbox.link = Link::Up;
        on_first_row(&mut app);
        let _ = app.on_key(Key::Enter, now());
        let start = job(&mut app);
        assert_eq!(start.provider, "openai");
        assert_eq!(start.workspace, PathBuf::from("/hub"));
    });
}

#[test]
fn a_remote_terminal_with_a_workspace_installs_there_and_asks_the_hub_nothing() {
    // The script is empty: a hub-global read would wait for ever.
    remote_case_in(Vec::new(), Some("/w"), |mut app| {
        app.outbox.link = Link::Up;
        let _ = app.open_model_picker(Mode::Choose);
        let _ = app.on_key(Key::Down, now());
        let _ = app.on_key(Key::Enter, now());
        let start = job(&mut app);
        assert_eq!(start.workspace, PathBuf::from("/w"));
    });
}

#[test]
fn a_failed_hub_global_read_fails_the_row_with_its_message() {
    remote_case(
        vec![(global_file().0, json!({"reject":"the hub is busy"}))],
        |mut app| {
            app.outbox.link = Link::Up;
            on_first_row(&mut app);
            let _ = app.on_key(Key::Enter, now());
            assert!(app.take_install_start().is_none());
            assert!(
                screen(&app).contains("✗ the hub is busy"),
                "{}",
                screen(&app)
            );
        },
    );
}

#[test]
fn while_a_remote_install_runs_the_config_views_are_refused_and_ask_nothing() {
    const REFUSED: &str = "a provider is installing; try again when it finishes";
    // The script holds only the install's own global-file read: any other
    // ask would wait for an answer that never comes.
    remote_case(
        vec![
            global_file(),
            (
                json!({"command":"configure","args":{"operation":"set","workspace":"/hub","layer":"global","key":"model","text":"\"hub/m\""}}),
                json!({"file":"/hub/config.json","warnings":[]}),
            ),
        ],
        |mut app| {
            app.outbox.link = Link::Up;
            on_first_row(&mut app);
            let _ = app.on_key(Key::Enter, now());
            let _ = job(&mut app);

            for view in [ConfigView::Settings, ConfigView::Tools, ConfigView::Login] {
                assert_eq!(app.open_config_view(view), Effect::None);
                assert!(!app.config_view_open());
            }
            assert_eq!(app.theme_command("light"), Effect::None);
            app.save_setting("model", "\"hub/m\"");
            let all = notices(&app);
            // Every notice is the refusal.
            assert!(
                all.iter().all(|(_, text)| text.contains(REFUSED)),
                "{all:?}"
            );

            // Esc closes the picker; the install still shows running.
            let _ = app.on_key(Key::Esc, now());
            assert!(!app.model_picker_open());
            open(&mut app);
            assert!(screen(&app).contains(" ⟳ installing"));

            // Once the result lands the write goes to the hub again.
            app.on_install("openai".to_owned(), installed(&[], Vec::new()));
            let before = notices(&app).len();
            app.save_setting("model", "\"hub/m\"");
            assert_eq!(notices(&app).len(), before);
        },
    );
}

#[test]
fn a_local_install_leaves_the_config_views_open() {
    let fake = fake();
    let mut app = home(Some(Arc::clone(&fake)));
    on_first_row(&mut app);
    let _ = app.on_key(Key::Enter, now());
    let _ = job(&mut app);
    app.model_picker.close();
    let _ = app.open_config_view(ConfigView::Settings);
    assert!(app.config_view_open());
    assert_eq!(fake.reads.lock().unwrap().len(), 1);
}

#[test]
fn the_spinner_asks_for_a_frame_only_while_an_install_shows() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home(Some(fake()));
    app.set_now(contract::clock::Clock::now(&*clock), 0);
    open(&mut app);
    let _ = screen(&app);
    assert_eq!(app.take_wake(), None, "no install: the picker asks nothing");

    let _ = app.on_key(Key::Down, now());
    let _ = app.on_key(Key::Enter, now());
    let _ = job(&mut app);
    let _ = screen(&app);
    assert!(app.take_wake().is_some(), "a running install animates");

    app.set_reduced_motion(true);
    let _ = screen(&app);
    assert_eq!(app.take_wake(), None, "reduced motion keeps it still");

    app.set_reduced_motion(false);
    app.on_install("openai".to_owned(), installed(&[], Vec::new()));
    let _ = screen(&app);
    assert_eq!(app.take_wake(), None, "the install ended");

    // A running install under a closed picker asks nothing either.
    app.model_picker.close();
    let _ = screen(&app);
    assert_eq!(app.take_wake(), None);
}
