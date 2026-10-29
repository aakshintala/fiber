//! Binary-level tests of the model picker's keys (`docs/tui.md`, "Swapped
//! views"): the real binary on a 120x32 pseudo-terminal opens the picker,
//! filters as it types, and Ctrl+S chooses for this session only, saving
//! nothing to the global config (`docs/testing.md`, "Levels").
//!
//! The waits read the shared driver's grid, and every assertion reads the
//! run's grid after its wait returns: the grid's text and rows.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;

use fakes::ProviderServer;
use serde_json::{Value, json};
use support::Setup;
use support::pty::{Grid, Run};

/// Installs a provider `fake` with the models `alpha`, `beta` and `gamma`
/// on `openai-responses` at the fake server, reading its key from
/// `FIBER_TEST_FAKE_KEY`, and makes `fake/alpha` the configured model.
/// `alpha` declares the thinking levels `low` and `high`, so the
/// thinking chip opens the picker on its level chips (`docs/tui.md`,
/// "Home").
fn install_picker_provider(setup: &Setup, server: &ProviderServer) {
    let source = setup.root.path().join("src");
    support::write_json(
        &source.join("extension.json"),
        &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "providers": {"fake": [format!("{}/v1", server.url())]}}),
    );
    let models: Vec<Value> = ["alpha", "beta", "gamma"]
        .iter()
        .map(|id| {
            let mut model = json!({"id": id, "protocol": "openai-responses",
                   "base_url": format!("{}/v1", server.url()),
                   "context_window": 100000});
            if *id == "alpha" {
                model["thinking_levels"] = json!(["low", "high"]);
                model["thinking_default"] = json!("low");
            }
            model
        })
        .collect();
    support::write_json(
        &source.join("providers/fake.json"),
        &json!({
            "name": "fake",
            "credential": {"env": "FIBER_TEST_FAKE_KEY"},
            "models": models,
        }),
    );
    extensions::plan(
        &setup.home(),
        &extensions::Request::Path(source),
        "0.0.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    support::write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/alpha", "hub": {"idle_exit_ms": 200}}),
    );
}

/// Whether any grid row holds `needle`.
fn shows(screen: &Grid, needle: &str) -> bool {
    screen.rows.iter().any(|row| row.contains(needle))
}

/// The session log's `model_changed` lines for a stall report, so a
/// catalogue stall says whether a switch already landed. Reads under
/// construction are absent, never failures: a missing session dir or
/// log reads as empty.
fn session_model_changed(setup: &Setup) -> String {
    let sessions = log::sessions_dir(&setup.home(), &worktree::project(&setup.workspace()));
    let mut switches = Vec::new();
    if let Ok(dirs) = fs::read_dir(&sessions) {
        for dir in dirs.filter_map(Result::ok) {
            let log = fs::read_to_string(dir.path().join("events.jsonl")).unwrap_or_default();
            switches.extend(
                support::complete_lines(&log)
                    .into_iter()
                    .filter(|line| line["kind"] == "model_changed"),
            );
        }
    }
    format!("session model_changed: {switches:?}")
}

/// Waits for the open picker to show its answered catalogue with
/// `gamma`: `gamma` present and the `Reading the model lists` stall
/// text gone. It reports through the shared `wait_screen_reporting`
/// helper: on expiry the panic names the catalogue stage and carries
/// the grid plus the session log, so the next sighting of the stall
/// says whether the picker opened and whether a switch already
/// landed.
#[track_caller]
fn wait_picker_catalogue(run: &mut Run, setup: &Setup) {
    run.wait_screen_reporting(
        "the picker catalogue with gamma",
        |screen| shows(screen, "gamma") && !shows(screen, "Reading the model lists"),
        || session_model_changed(setup),
    );
}

#[test]
fn ctrl_s_chooses_the_filtered_model_for_this_session_only() {
    let setup = Setup::new();
    let server = ProviderServer::start([support::hello()]).unwrap();
    install_picker_provider(&setup, &server);
    let mut run = Run::spawn(
        &setup,
        120,
        32,
        &[],
        &[
            ("TERM", "xterm-256color"),
            ("FIBER_TEST_FAKE_KEY", "sk-test"),
        ],
    );
    // The end of the first frame proves the input reader runs before
    // the prompt goes out; the reply stays drawn, so the grid check
    // before the finished title is order-free.
    run.ready();
    let from = run.output().len();
    run.write(b"say hi\r");
    run.wait_screen("the reply", |screen| shows(screen, "Hel"));
    run.turn_finished(from);
    let saved = fs::read(setup.home().join("config.json")).unwrap();
    // Ctrl+L opens the picker: the open wait names the picker frame
    // and the catalogue wait names its answered models, so a stall
    // says whether the picker never opened or the catalogue never
    // answered. The open picker draws from the terminal's own view
    // once the turn closed, so the finished turn above proves the key
    // is taken.
    run.write(b"\x0c");
    run.wait_screen_reporting(
        "the open picker",
        |screen| shows(screen, "Type to search"),
        || session_model_changed(&setup),
    );
    wait_picker_catalogue(&mut run, &setup);
    let opened = run.screen().rows.clone();
    assert!(
        opened.iter().any(|row| row.contains("Type to search")),
        "the picker shows its filter: {opened:?}"
    );
    // Typing narrows the list to `beta`: the count names one row of the
    // three installed models, and `gamma` drops out. The open picker's
    // catalogue above proves the filter is taken.
    run.write(b"bet");
    run.wait_screen("the filtered list", |screen| {
        shows(screen, "1 of 3 models") && !shows(screen, "gamma")
    });
    let filtered = run.screen().rows.clone();
    assert!(
        filtered.iter().any(|row| row.contains("1 of 3 models")),
        "the count names one of three: {filtered:?}"
    );
    assert!(
        filtered.iter().all(|row| !row.contains("gamma")),
        "gamma dropped out: {filtered:?}"
    );
    // Ctrl+S chooses at once for this session only: the picker closes and
    // the panel names the session's new model. The filtered list above
    // proves the choice is taken.
    run.write(b"\x13");
    run.wait_screen("the session on fake/beta", |screen| {
        shows(screen, "fake/beta") && !shows(screen, "Type to search")
    });
    let switched = run.screen().rows.clone();
    assert!(
        switched.iter().any(|row| row.contains("fake/beta")),
        "the panel names the session model: {switched:?}"
    );
    // The session's log holds the switch, from the driver.
    let sessions = log::sessions_dir(&setup.home(), &worktree::project(&setup.workspace()));
    let dirs: Vec<_> = fs::read_dir(&sessions)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(dirs.len(), 1, "one session ran: {dirs:?}");
    let log = fs::read_to_string(dirs[0].join("events.jsonl")).unwrap();
    let changed: Vec<Value> = support::complete_lines(&log)
        .into_iter()
        .filter(|line| line["kind"] == "model_changed")
        .collect();
    assert_eq!(changed.len(), 1, "one switch: {changed:?}");
    assert_eq!(changed[0]["payload"]["after"]["model"], "fake/beta");
    assert_eq!(changed[0]["payload"]["source"], "driver");
    // Nothing reached the global config: byte for byte, and no `models`.
    assert_eq!(fs::read(setup.home().join("config.json")).unwrap(), saved);
    let config: Value =
        serde_json::from_slice(&fs::read(setup.home().join("config.json")).unwrap()).unwrap();
    assert!(config.get("models").is_none(), "no level saved: {config}");
    // Quitting is taken in any state; the session-on-beta grid above
    // proves the terminal drew before the quit goes out.
    run.write(b"\x03\x03\r");
    run.wait_screen("the restored terminal", |screen| {
        !screen.alternate_screen && !screen.hide_cursor
    });
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

/// Spawns home at 120x32 with the picker provider installed and `env`
/// in the child's environment: the fake server answers no turn, since
/// no prompt goes out. Returns the run once the first frame drew and
/// the chip row names the configured model, so a click never lands
/// before its target draws.
#[track_caller]
fn home_run(setup: &Setup, env: &[(&str, &str)]) -> Run {
    let mut full = vec![("FIBER_TEST_FAKE_KEY", "sk-test")];
    full.extend(env.iter().copied());
    let mut run = Run::spawn(setup, 120, 32, &[], &full);
    run.ready();
    run.wait_screen("home with its chips", |screen| {
        shows(screen, "[fake/alpha]") && shows(screen, "[thinking:")
    });
    run
}

/// The chip row's cell just inside `needle`'s opening bracket on
/// `screen`: the coordinates come from the drawn grid, never from a
/// hard-coded column, so the centred box may sit anywhere.
#[track_caller]
fn chip_cell(screen: &Grid, needle: &str) -> (u16, u16) {
    let y = screen
        .rows
        .iter()
        .position(|row| row.contains(needle))
        .unwrap_or_else(|| panic!("no chip {needle} in {:?}", screen.rows));
    let y = u16::try_from(y).expect("the chip fits the screen");
    let symbols: Vec<String> = needle.chars().map(|ch| ch.to_string()).collect();
    let len = u16::try_from(symbols.len()).expect("the chip fits its row");
    let (cols, _) = screen.size();
    let x = (0..=cols.saturating_sub(len))
        .find(|x| {
            symbols.iter().enumerate().all(|(dx, symbol)| {
                screen
                    .cell(
                        x.saturating_add(u16::try_from(dx).expect("the chip fits its row")),
                        y,
                    )
                    .symbol
                    == *symbol
            })
        })
        .expect("the chip's cells on its row");
    (x + 1, y)
}

/// Clicks the cell at `(x, y)`: an SGR press then its release on the
/// same cell, the pair the rail's click test sends.
fn click(run: &mut Run, x: u16, y: u16) {
    run.write(format!("\x1b[<0;{};{}M\x1b[<0;{};{}m", x + 1, y + 1, x + 1, y + 1).as_bytes());
}

/// Parks the pointer on the cell at `(x, y)`: an SGR motion report
/// with no button, as the look hover test sends.
fn hover(run: &mut Run, x: u16, y: u16) {
    run.write(format!("\x1b[<35;{};{}M", x + 1, y + 1).as_bytes());
}

/// Whether the picker draws its open catalogue: the filter row and a
/// model row from the installed list.
fn picker_open(screen: &Grid) -> bool {
    shows(screen, "Type to search") && shows(screen, "gamma")
}

/// Whether the picker's focused row is the current model's level
/// chips: the `alpha` row carries the focus gutter and the current
/// mark, and the row under it names the `thinking` levels only a
/// level row draws.
fn thinking_focused(screen: &Grid) -> bool {
    let Some(y) = screen.rows.iter().position(|row| {
        row.contains("alpha") && row.contains("\u{203a}") && row.contains("current")
    }) else {
        return false;
    };
    screen
        .rows
        .get(y + 1)
        .is_some_and(|row| row.contains("thinking") && row.contains("low") && row.contains("high"))
}

/// Closes the open picker with Esc and quits home: the Esc wait proves
/// the picker closed before the quit goes out, and the primary screen
/// proves the quit.
#[track_caller]
fn close_and_quit(mut run: Run) {
    run.write(b"\x1b[27u");
    run.wait_screen("home again", |screen| {
        !shows(screen, "Type to search") && shows(screen, "[fake/alpha]")
    });
    run.write(b"\x03\x03");
    run.wait_screen("the restored terminal", |screen| {
        !screen.alternate_screen && !screen.hide_cursor
    });
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

#[test]
fn clicking_the_model_chip_opens_the_model_picker() {
    let setup = Setup::new();
    let server = ProviderServer::start([support::hello()]).unwrap();
    install_picker_provider(&setup, &server);
    let mut run = home_run(&setup, &[("TERM", "xterm-256color")]);
    // The home grid above proves the chip drew before the click.
    let (x, y) = chip_cell(&run.screen(), "[fake/alpha]");
    click(&mut run, x, y);
    run.wait_screen("the open picker with its catalogue", picker_open);
    close_and_quit(run);
}

#[test]
fn clicking_the_thinking_chip_opens_the_picker_on_its_level_chips() {
    let setup = Setup::new();
    let server = ProviderServer::start([support::hello()]).unwrap();
    install_picker_provider(&setup, &server);
    let mut run = home_run(&setup, &[("TERM", "xterm-256color")]);
    // The home grid above proves the chip drew before the click.
    // A thinking click falls back to an ordinary choose while the
    // cached model lists have not answered (`open_thinking`), and the
    // fallback draws exactly like the thinking open, so the first
    // open warms the lists: it waits for the catalogue, then closes.
    // Esc closes at once with no query typed.
    let (x0, y0) = chip_cell(&run.screen(), "[fake/alpha]");
    click(&mut run, x0, y0);
    run.wait_screen("the open picker with its catalogue", picker_open);
    run.write(b"\x1b[27u");
    let home = run.wait_screen("home again", |screen| {
        !shows(screen, "Type to search") && shows(screen, "[fake/alpha]")
    });
    // The lists now name alpha, so this click opens on its level
    // chips in Thinking mode, not the fallback choose. The focused
    // row alone cannot tell the two apart: both land on the current
    // model with its level chips drawn, so Enter chooses at once
    // with no chip move. A thinking open starts the current row
    // touched (`model_picker::open`), saving only the default
    // level; an ordinary choose would save only the model and write
    // no level, failing the config read below.
    let (x, y) = chip_cell(&home, "[thinking:");
    click(&mut run, x, y);
    run.wait_screen("the picker on alpha's level chips", |screen| {
        picker_open(screen) && thinking_focused(screen)
    });
    run.write(b"\r");
    // The closed picker and the renamed chip prove the choice landed
    // before the config read below.
    run.wait_screen("home with the saved level", |screen| {
        !shows(screen, "Type to search")
            && shows(screen, "[fake/alpha]")
            && shows(screen, "[thinking: low]")
    });
    let config: Value =
        serde_json::from_slice(&fs::read(setup.home().join("config.json")).unwrap()).unwrap();
    assert_eq!(
        config["model"], "fake/alpha",
        "the model is untouched: {config}"
    );
    assert_eq!(
        config["models"]["fake/alpha"]["thinking"], "low",
        "only the level saved: {config}"
    );
    // The picker already closed on Enter, so the quit goes out at once.
    run.write(b"\x03\x03");
    run.wait_screen("the restored terminal", |screen| {
        !screen.alternate_screen && !screen.hide_cursor
    });
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

#[test]
fn an_unset_host_notice_shows_model_unconfigured_not_model_invalid() {
    let setup = Setup::new();
    // A provider whose model's `base_url` names the `workspace`
    // placeholder, which declares no environment variable and whose
    // setting value is not a host, so the terminal's own read leaves
    // the model out with a per-model `model_unconfigured` notice
    // naming the model and the setting: the positive control for the
    // valueless placeholder's launch silence.
    let source = setup.root.path().join("src");
    support::write_json(
        &source.join("extension.json"),
        &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "providers": {"fake": ["https://{workspace}/v1"]}}),
    );
    support::write_json(
        &source.join("providers/fake.json"),
        &json!({
            "name": "fake",
            "credential": {"env": "FIBER_TEST_FAKE_KEY"},
            "placeholders": {"workspace": {}},
            "models": [{"id": "alpha", "protocol": "openai-responses",
                        "base_url": "https://{workspace}/v1", "context_window": 100000}],
        }),
    );
    extensions::plan(
        &setup.home(),
        &extensions::Request::Path(source),
        "0.0.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    support::write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/alpha", "hub": {"idle_exit_ms": 200}}),
    );
    // The setting value is not a host, so the fill leaves the model
    // out per model rather than grouping it.
    support::write_json(
        &setup.home().join("config/fake.json"),
        &json!({"workspace": "x/y"}),
    );
    let mut run = Run::spawn(
        &setup,
        120,
        32,
        &[],
        &[
            ("TERM", "xterm-256color"),
            ("FIBER_TEST_FAKE_KEY", "sk-test"),
        ],
    );
    // The end of the first frame proves the input reader runs before
    // the background catalogue read answers.
    run.ready();
    // The band shows the notice's code beside its message. The wait
    // names the code and the model, which survive the 120-column
    // truncation that cuts the message's tail; notices fade after 10
    // seconds, so the wait catches the row rather than sleeping past
    // it.
    let screen = run.wait_screen("the model_unconfigured notice", |screen| {
        shows(screen, "model_unconfigured") && shows(screen, "fake/alpha")
    });
    assert!(
        shows(&screen, "model_unconfigured · The model `fake/alpha`"),
        "the band shows the code with the message: {}",
        screen.contents
    );
    assert!(
        screen
            .rows
            .iter()
            .all(|row| !(row.contains("model_invalid") && row.contains("fake/alpha"))),
        "no model_invalid row carries it: {:?}",
        screen.rows
    );
    // Ctrl+L opens the picker: the open wait names the picker frame,
    // so a stall says whether the picker never opened.
    run.write(b"\x0c");
    run.wait_screen("the open picker", |screen| shows(screen, "Type to search"));
    // The empty catalogue proves the read answered with nothing
    // listed and nothing grouped: the value stayed per-model instead
    // of becoming a count row.
    run.wait_screen("the answered empty catalogue", |screen| {
        shows(screen, "No models. Install")
    });
    let counted = run.screen().rows.clone();
    assert!(
        counted.iter().all(|row| !row.contains("models need")),
        "no count row groups the not-a-host model: {counted:?}"
    );
    // Esc closes the picker at once with no query typed, and the quit
    // goes out at home.
    run.write(b"\x1b[27u");
    run.wait_screen("home again", |screen| !shows(screen, "Type to search"));
    run.write(b"\x03\x03");
    run.wait_screen("the restored terminal", |screen| {
        !screen.alternate_screen && !screen.hide_cursor
    });
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

/// The chip's cells on `screen`: `len` cells from `x` on row `y`.
fn chip_cells(screen: &Grid, x: u16, y: u16, len: u16) -> Vec<support::pty::Cell> {
    (0..len).map(|dx| screen.cell(x + dx, y)).collect()
}

#[test]
fn hovering_a_home_chip_tints_it_and_moving_off_restores_it() {
    let setup = Setup::new();
    let server = ProviderServer::start([support::hello()]).unwrap();
    install_picker_provider(&setup, &server);
    // Truecolour, as the link hover test runs: at 256 colours the
    // hover tint and the chips' surface tint share one palette entry,
    // so a hovered chip reads back unchanged there.
    let mut run = home_run(&setup, &support::TRUECOLOUR);
    // The motion enable proves the terminal takes pointer reports
    // before the first hover goes out.
    run.wait_bytes(0, support::pty::MOTION, "mouse motion enabled");
    // Both chips unfocused: nothing sent a key, so focus sits on the
    // entry bar and no focused chip keeps its highlight by spec.
    for needle in ["[fake/alpha]", "[thinking:"] {
        let before = run.screen();
        let (x, y) = chip_cell(&before, needle);
        let len = u16::try_from(needle.len()).expect("the chip fits its row");
        let plain = chip_cells(&before, x, y, len);
        hover(&mut run, x, y);
        run.wait_screen(&format!("the hovered chip {needle}"), |screen| {
            chip_cells(screen, x, y, len) != plain
        });
        // The top-left corner holds no target, so parking there
        // leaves every chip.
        hover(&mut run, 0, 0);
        run.wait_screen(&format!("the restored chip {needle}"), |screen| {
            chip_cells(screen, x, y, len) == plain
        });
    }
    run.write(b"\x03\x03");
    run.wait_screen("the restored terminal", |screen| {
        !screen.alternate_screen && !screen.hide_cursor
    });
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}
